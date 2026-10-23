//! Device labels are host/user data, independent of the application brand.
/// Validate a user label against the discovery protocol's UTF-8 byte limit.
pub fn valid_name(value: &str) -> Option<String> {
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
#[cfg(test)]
mod tests {
    use super::*;
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
