//! Compiled product identity for the supported desktop GUI.
use serde::Serialize;

pub const ORIGINAL_RENDERER: &str = "restored-original-gpui";

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
}

pub fn product_info() -> ProductInfo {
    ProductInfo {
        schema: 1,
        product: "RemotePlay",
        version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        default_gui: ORIGINAL_RENDERER,
        original_gui_compiled: cfg!(feature = "gpui-restoration"),
        native_video_compiled: cfg!(all(target_os = "macos", feature = "gpui-restoration"))
            || cfg!(all(target_os = "linux", feature = "native-linux-video"))
            || cfg!(all(target_os = "windows", feature = "native-windows-video")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
