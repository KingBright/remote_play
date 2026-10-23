//! Device labels are host/user data, independent of the application brand.
/// Validate a user label against the discovery protocol's UTF-8 byte limit.
pub fn valid_name(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) { return None; }
    let value = value.trim();
    (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}
pub fn host_name() -> String {
    platform_host_name().and_then(|value| valid_name(&value))
        .unwrap_or_else(|| "Unnamed computer".to_owned())
}
#[cfg(unix)]
fn platform_host_name() -> Option<String> {
    let mut bytes = [0u8; 256];
    // gethostname reads only the local kernel hostname; it needs no device permissions.
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return None;
    }
    let end = bytes.iter().position(|byte| *byte == 0)?;
    String::from_utf8(bytes[..end].to_vec()).ok()
}
#[cfg(windows)]
fn platform_host_name() -> Option<String> { std::env::var("COMPUTERNAME").ok() }
#[cfg(not(any(unix, windows)))]
fn platform_host_name() -> Option<String> { std::env::var("HOSTNAME").ok() }

/// A local edit is only a draft until the owner confirms its version-checked save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEditor {
    expected: String,
    draft: String,
}
impl NameEditor {
    pub fn new(saved: impl Into<String>) -> Self {
        let saved = saved.into();
        Self { expected: saved.clone(), draft: saved }
    }
    pub fn draft(&self) -> &str { &self.draft }
    pub fn expected(&self) -> &str { &self.expected }
    pub fn edit(&mut self, value: impl Into<String>) { self.draft = value.into(); }
    pub fn use_default(&mut self, host: &str) { self.edit(host); }
    pub fn pending_name(&self) -> Option<String> {
        valid_name(&self.draft).filter(|name| name != &self.expected)
    }
    pub fn validation_error(&self) -> Option<&'static str> {
        valid_name(&self.draft).is_none().then_some(
            "Use 1–128 UTF-8 bytes, without line breaks or control characters.")
    }
    /// Follow owner updates only while the user has no unsaved edit. Keep a
    /// dirty edit's original CAS baseline so an external rename cannot be overwritten.
    pub fn observe_saved(&mut self, saved: &str) {
        if self.draft == self.expected && self.expected != saved {
            self.accept(saved);
        }
    }
    pub fn accept(&mut self, saved: impl Into<String>) { *self = Self::new(saved); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_observations_refresh_pristine_names_including_restore_default() {
        let mut editor = NameEditor::new("Old test name");
        for observed in ["MacStudio·外部改名", "MacStudio.local"] {
            editor.observe_saved(observed);
            assert_eq!(editor.expected(), observed);
            assert_eq!(editor.draft(), observed);
            assert!(editor.pending_name().is_none());
        }
    }

    #[test]
    fn owner_observations_preserve_unsaved_input_and_its_cas_baseline() {
        let invalid = "界".repeat(43);
        for draft in ["My unsaved name", "  ", invalid.as_str()] {
            let mut editor = NameEditor::new("Studio");
            editor.edit(draft);
            editor.observe_saved("Externally renamed Studio");
            assert_eq!(editor.expected(), "Studio");
            assert_eq!(editor.draft(), draft);
        }
    }

    #[test]
    fn abandoning_a_draft_allows_the_next_owner_observation_to_refresh() {
        let mut editor = NameEditor::new("Studio");
        editor.edit("Unsaved");
        editor.observe_saved("Current owner name");
        editor.edit("Studio");
        editor.observe_saved("Current owner name");
        assert_eq!(editor.draft(), "Current owner name");
        assert!(editor.pending_name().is_none());
    }

    #[test]
    fn editor_keeps_drafts_local_and_trims_only_when_saving() {
        let mut editor = NameEditor::new("MacStudio");
        assert!(editor.pending_name().is_none());
        editor.edit("  梁的工作站  ");
        assert_eq!(editor.expected(), "MacStudio");
        assert_eq!(editor.draft(), "  梁的工作站  ");
        assert_eq!(editor.pending_name().as_deref(), Some("梁的工作站"));
        editor.accept("梁的工作站");
        assert_eq!(editor.expected(), "梁的工作站");
        assert!(editor.pending_name().is_none());
    }
    #[test]
    fn editor_rejects_invalid_names_without_losing_the_saved_baseline() {
        let mut editor = NameEditor::new("cube");
        for invalid in ["", "  ", "\nOffice\n", "\tOffice", "a\0b", &"界".repeat(43)] {
            editor.edit(invalid);
            assert!(editor.validation_error().is_some(), "{invalid:?}");
            assert!(editor.pending_name().is_none());
            assert_eq!(editor.expected(), "cube");
        }
        editor.edit("界".repeat(42));
        assert!(editor.pending_name().is_some());
        editor.edit("a".repeat(128));
        assert!(editor.pending_name().is_some());
        editor.edit("a".repeat(129));
        assert!(editor.pending_name().is_none());
    }
    #[test]
    fn restore_default_is_an_explicit_edit_and_accept_is_separate() {
        let mut editor = NameEditor::new("My custom name");
        editor.use_default("HO5");
        assert_eq!(editor.expected(), "My custom name");
        assert_eq!(editor.pending_name().as_deref(), Some("HO5"));
        editor.accept("HO5");
        assert!(editor.pending_name().is_none());
        let reopened = NameEditor::new(editor.expected());
        assert_eq!(reopened.draft(), "HO5");
    }
    #[test]
    fn cancelling_or_failed_save_never_replaces_the_saved_name() {
        let mut editor = NameEditor::new("Studio");
        editor.edit("New name");
        // A refused backend operation must not call accept.
        assert_eq!(editor.expected(), "Studio");
        assert_eq!(editor.pending_name().as_deref(), Some("New name"));
        editor.accept("Current owner name");
        assert_eq!(editor.draft(), "Current owner name");
        assert!(editor.pending_name().is_none());
    }

    #[test]
    fn labels_preserve_custom_names_and_reject_invalid_protocol_text() {
        assert_eq!(valid_name("  梁的 Studio  "), Some("梁的 Studio".into()));
        assert_eq!(valid_name("Weavelo"), Some("Weavelo".into()));
        for invalid in ["", "  ", "line\nfeed", "nul\0name", &"界".repeat(43)] {
            assert!(valid_name(invalid).is_none());
        }
    }
    #[test]
    fn local_default_is_a_readable_host_label() {
        let name = host_name();
        assert!(valid_name(&name).is_some());
        assert_ne!(name, "Weavelo");
    }
}
