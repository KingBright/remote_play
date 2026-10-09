//! Process-local view binding. Never a protocol ID, resource or network runtime.
use std::sync::{Arc, Weak};

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
