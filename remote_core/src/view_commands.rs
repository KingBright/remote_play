//! Process-local view binding. Never a protocol ID, resource or network runtime.
use std::sync::{Arc, Weak};
use protocol::session::{CaptureSource, CaptureSourceInfo};

/// A choice must come from the current catalog, with exactly one typed source ID.
/// MainDisplay is an explicit ID here, never a fallback for a missing window.
pub fn enumerated_source(catalog: &[CaptureSourceInfo], id: CaptureSource) -> Option<&CaptureSourceInfo> {
    let mut matches = catalog.iter().filter(|source| source.source == id);
    let source = matches.next()?;
    matches.next().is_none().then_some(source)
}

/// Revalidate the menu's captured identity/metadata before a queued action runs.
pub fn apply_enumerated_source<T, Scope: Copy + PartialEq, R>(
    binding: &ViewConnectionBinding<T, Scope>, active: &Arc<T>, scope: Scope,
    expected: &CaptureSourceInfo, catalog: &[CaptureSourceInfo],
    action: impl FnOnce(CaptureSource) -> R,
) -> Option<R> {
    if enumerated_source(catalog, expected.source) != Some(expected) { return None; }
    binding.apply(active, scope, || action(expected.source))
}

pub struct ViewConnectionBinding<T, Scope> {
    connection: Weak<T>,
    scope: Scope,
}
impl<T, Scope: Copy> Clone for ViewConnectionBinding<T, Scope> {
    fn clone(&self) -> Self {
        Self {
            connection: self.connection.clone(),
            scope: self.scope,
        }
    }
}
impl<T, Scope: Copy + PartialEq> ViewConnectionBinding<T, Scope> {
    pub fn new(connection: &Arc<T>, scope: Scope) -> Self {
        Self {
            connection: Arc::downgrade(connection),
            scope,
        }
    }
    pub fn matches(&self, active: &Arc<T>, scope: Scope) -> bool {
        self.scope == scope
            && self
                .connection
                .upgrade()
                .is_some_and(|previous| Arc::ptr_eq(&previous, active))
    }
    /// The adapter must keep its session lock across this check and mutation.
    /// A rejected binding does not call the supplied action.
    pub fn apply<R>(&self, active: &Arc<T>, scope: Scope, action: impl FnOnce() -> R) -> Option<R> {
        self.matches(active, scope).then(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_tabs::SessionCommandState;
    fn source(id: CaptureSource, title: &str, pid: Option<i32>) -> CaptureSourceInfo {
        CaptureSourceInfo { source: id, title: title.into(), application: String::new(),
            process_id: pid, width: 640, height: 480, supports_input: false }
    }
    #[test]
    fn missing_display_or_window_never_dispatches_a_fallback() {
        let a = Arc::new("a"); let binding = ViewConnectionBinding::new(&a, ());
        let display = source(CaptureSource::MainDisplay, "Desktop", None);
        let window = source(CaptureSource::Window(7), "Window", Some(8));
        for (expected, catalog) in [(&display, vec![]), (&display, vec![window.clone()]),
            (&window, vec![display.clone()])] {
            assert!(apply_enumerated_source(&binding, &a, (), expected, &catalog,
                |_| panic!("absent source must not be sent")).is_none());
        }
    }
    #[test]
    fn real_catalog_supports_main_display_display_and_window_without_id_aliasing() {
        let a = Arc::new("a"); let binding = ViewConnectionBinding::new(&a, ());
        let catalog = vec![source(CaptureSource::MainDisplay, "Desktop", None),
            source(CaptureSource::Display(7), "External", None),
            source(CaptureSource::Window(7), "Window", Some(8))];
        let mut sent = Vec::new();
        for selected in &catalog {
            assert_eq!(apply_enumerated_source(&binding, &a, (), selected, &catalog,
                |id| sent.push(id)), Some(()));
        }
        assert_eq!(sent, [CaptureSource::MainDisplay, CaptureSource::Display(7), CaptureSource::Window(7)]);
        let mut duplicated = catalog.clone(); duplicated.push(catalog[0].clone());
        assert!(enumerated_source(&duplicated, CaptureSource::MainDisplay).is_none());
    }
    #[test]
    fn queued_selection_rejects_removed_reused_or_changed_source() {
        let a = Arc::new("a"); let binding = ViewConnectionBinding::new(&a, ());
        let expected = source(CaptureSource::Window(7), "Window", Some(8));
        let mut changed_pid = expected.clone(); changed_pid.process_id = Some(9);
        let mut changed_title = expected.clone(); changed_title.title = "Another window".into();
        let mut changed_geometry = expected.clone(); changed_geometry.width += 1;
        for catalog in [vec![], vec![changed_pid], vec![changed_title], vec![changed_geometry]] {
            assert!(apply_enumerated_source(&binding, &a, (), &expected, &catalog,
                |_| panic!("stale catalog must not be sent")).is_none());
        }
    }
    #[test]
    fn cancel_or_disconnect_invalidates_queued_selection_even_after_selecting_back() {
        let a = Arc::new("a"); let binding = ViewConnectionBinding::new(&a, ());
        let expected = source(CaptureSource::MainDisplay, "Desktop", None);
        let mut commands = SessionCommandState::default(); let old = commands.begin();
        commands.begin(); // User cancels/disconnects, or selects another session.
        let mut sent = Vec::new();
        if commands.is_current(old) {
            apply_enumerated_source(&binding, &a, (), &expected, &[expected.clone()], |id| sent.push(id));
        }
        assert!(sent.is_empty());
        drop(a);
        let replacement = Arc::new("a");
        assert!(apply_enumerated_source(&binding, &replacement, (), &expected, &[expected.clone()],
            |_| panic!("disconnected allocation must not be reused")).is_none());
    }
    #[test]
    fn two_sessions_with_identical_source_ids_do_not_receive_each_others_selection() {
        let a = Arc::new("a"); let b = Arc::new("b");
        let binding = ViewConnectionBinding::new(&a, Some([7; 32]));
        let expected = source(CaptureSource::MainDisplay, "Desktop", None);
        let mut a_sent = Vec::new(); let mut b_sent = Vec::new();
        assert!(apply_enumerated_source(&binding, &b, Some([7; 32]), &expected, &[expected.clone()],
            |id| b_sent.push(id)).is_none());
        assert_eq!(apply_enumerated_source(&binding, &a, Some([7; 32]), &expected, &[expected.clone()],
            |id| a_sent.push(id)), Some(()));
        assert!(apply_enumerated_source(&binding, &a, Some([8; 32]), &expected, &[expected.clone()],
            |_| panic!("changed network must not receive selection")).is_none());
        assert_eq!(a_sent, [CaptureSource::MainDisplay]); assert!(b_sent.is_empty());
    }
    #[test]
    fn queued_source_work_rejects_another_allocation_even_with_same_device_and_source_ids() {
        let a = Arc::new(("device", 42));
        let b = Arc::new(("device", 42));
        let binding = ViewConnectionBinding::new(&a, Some([7; 32]));
        let mut sent = Vec::new();
        assert_eq!(binding.apply(&b, Some([7; 32]), || sent.push(42)), None);
        assert!(sent.is_empty());
        assert_eq!(binding.apply(&a, Some([7; 32]), || sent.push(42)), Some(()));
        assert_eq!(sent, [42]);
    }
    #[test]
    fn source_binding_expires_on_network_change_or_connection_drop_without_retaining_resource() {
        let a = Arc::new("a");
        let binding = ViewConnectionBinding::new(&a, Some([7; 32]));
        assert_eq!(Arc::strong_count(&a), 1);
        assert!(
            binding
                .apply(&a, Some([8; 32]), || panic!("wrong network mutation"))
                .is_none()
        );
        drop(a);
        assert!(!binding.matches(&Arc::new("a"), Some([7; 32])));
    }
    #[test]
    fn latest_source_command_fences_queued_work_and_old_errors_while_discovery_is_independent() {
        let a = Arc::new("a");
        let binding = ViewConnectionBinding::new(&a, ());
        let mut switches = SessionCommandState::default();
        let mut discovery = SessionCommandState::default();
        let old = switches.begin();
        let new = switches.begin();
        let list = discovery.begin();
        let mut sent = Vec::new();
        if switches.is_current(old) {
            binding.apply(&a, (), || sent.push("old"));
        }
        if switches.is_current(new) {
            binding.apply(&a, (), || sent.push("new"));
        }
        assert_eq!(sent, ["new"]);
        assert!(!switches.is_current(old));
        assert!(switches.is_current(new) && discovery.is_current(list));
        let b = Arc::new("b");
        assert!(!(switches.is_current(new) && binding.matches(&b, ())));
    }
    #[test]
    fn selection_round_trip_and_cancel_do_not_revive_old_source_command_receipts() {
        let a = Arc::new("a");
        let binding = ViewConnectionBinding::new(&a, ());
        let mut commands = SessionCommandState::default();
        let old = commands.begin();
        commands.begin(); // Choose B.
        commands.begin(); // Choose A again; same allocation, different user intent.
        assert!(binding.matches(&a, ()));
        assert!(!commands.is_current(old));
        let next = commands.begin();
        commands.begin(); // Disconnect/cancel before dispatch.
        assert!(!commands.is_current(next));
    }
}
