//! Compiled product identity for the supported desktop GUI.
use serde::Serialize;

pub const ORIGINAL_RENDERER: &str = "restored-original-gpui";

// Capability of the compiled role entries, not native deployment acceptance.
// Keep Windows conservative until its own lifecycle validation is complete.
pub const SEPARATED_LIFECYCLE_COMPILED: bool = cfg!(all(
    any(target_os = "macos", target_os = "linux"), feature = "gpui-restoration"
));

#[derive(Serialize)]
pub struct ProductInfo {
    pub schema: u32,
    pub product: &'static str,
    pub version: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub default_gui: &'static str,
    pub original_gui_compiled: bool,
    pub native_video_compiled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<serde_json::Value>,
    pub separated_lifecycle: bool,
    pub background_entry: Option<&'static str>,
    pub frontend_entry: Option<&'static str>,
}

pub fn product_info() -> ProductInfo {
    ProductInfo {
        schema: 1,
        product: "Weavelo",
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        default_gui: ORIGINAL_RENDERER,
        original_gui_compiled: cfg!(feature = "gpui-restoration"),
        native_video_compiled: cfg!(all(target_os = "macos", feature = "gpui-restoration"))
            || cfg!(all(target_os = "linux", feature = "native-linux-video"))
            || cfg!(all(target_os = "windows", feature = "native-windows-video")),
        build_identity: crate::build_identity::compiled(),
        separated_lifecycle: SEPARATED_LIFECYCLE_COMPILED,
        background_entry: SEPARATED_LIFECYCLE_COMPILED.then_some("--background-service"),
        frontend_entry: SEPARATED_LIFECYCLE_COMPILED.then_some("--gui"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_claim_matches_both_compiled_role_entries() {
        let info = product_info();
        assert_eq!(info.separated_lifecycle, SEPARATED_LIFECYCLE_COMPILED);
        assert_eq!(info.background_entry, SEPARATED_LIFECYCLE_COMPILED.then_some("--background-service"));
        assert_eq!(info.frontend_entry, SEPARATED_LIFECYCLE_COMPILED.then_some("--gui"));
        #[cfg(all(any(target_os = "macos", target_os = "linux"), feature = "gpui-restoration"))]
        {
            // These production implementations must exist in the same build;
            // this test starts neither a runtime nor a credential store.
            let _background = crate::background_service::run;
            let _frontend = crate::restored_ui::run_attached_gui;
        }
    }

    #[test]
    fn product_contract_names_the_compiled_gpui_gui() {
        let info = product_info();
        assert_eq!(info.default_gui, ORIGINAL_RENDERER);
        assert_eq!(
            info.original_gui_compiled,
            cfg!(feature = "gpui-restoration")
        );
        let value = serde_json::to_value(info).unwrap();
        assert!(
            !value
                .as_object()
                .unwrap()
                .contains_key("diagnostic_gui_requires_opt_in")
        );
    }
}
