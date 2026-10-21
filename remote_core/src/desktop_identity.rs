//! The new desktop namespace. The retained client is never auto-discovered or migrated.
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopIdentity {
    pub schema: u32,
    pub product: String,
    pub application_id: String,
    pub install_directory: String,
    pub profile_directory: String,
    pub service_label: String,
    pub linux_service: String,
    pub control_port: u16,
    pub discovery_port: u16,
}

pub fn identity() -> &'static DesktopIdentity {
    static IDENTITY: LazyLock<DesktopIdentity> = LazyLock::new(|| {
        let value: DesktopIdentity =
            serde_json::from_str(include_str!("../../deploy/desktop/product-identity.json"))
                .expect("reviewed desktop identity");
        assert_eq!(value.schema, 1);
        assert_eq!(value.product, "Weavelo");
        assert_eq!(value.application_id, "fun.hackerlife.weavelo.desktop");
        assert_ne!(value.control_port, 39271);
        assert_ne!(value.discovery_port, 38117);
        value
    });
    &IDENTITY
}

/// Pure path selection, with no filesystem access or fallback to a retained profile.
pub fn profile(platform: &str, home: &Path, config_home: Option<&Path>) -> PathBuf {
    let id = identity();
    match platform {
        "macos" => home
            .join("Library/Application Support")
            .join(&id.product)
            .join(&id.profile_directory),
        "windows" => config_home
            .unwrap_or(home)
            .join(&id.product)
            .join(&id.profile_directory),
        _ => config_home
            .map(Path::to_owned)
            .unwrap_or_else(|| home.join(".config"))
            .join("weavelo")
            .join(id.profile_directory.to_ascii_lowercase()),
    }
}

/// Reject accidental inherited launch settings from the retained client. This
/// only classifies paths; it never resolves or opens another profile.
pub fn retained_profile(path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let text = text.trim_end_matches('/');
    [
        "/weavelo/nativemesh",
        "/weavelo/mesh",
        "/remoteplay/mesh",
        "/remote_play/mesh",
    ]
    .iter()
    .any(|suffix| text.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_identity_profiles_and_ipc_are_disjoint_from_retained_clients() {
        let home = Path::new("/fixture/home");
        for (platform, base, retained) in [
            (
                "macos",
                None,
                home.join("Library/Application Support/Weavelo/NativeMesh"),
            ),
            (
                "windows",
                Some(Path::new("/fixture/appdata")),
                PathBuf::from("/fixture/appdata/Weavelo/Mesh"),
            ),
            ("linux", None, home.join(".config/weavelo/mesh")),
        ] {
            let new = profile(platform, home, base);
            assert_ne!(new, retained);
            for endpoint in [
                "background.sock",
                "gui-receive.sock",
                "instance.lock",
                "preferences.json",
            ] {
                assert_ne!(new.join(endpoint), retained.join(endpoint));
            }
        }
    }
    #[test]
    fn desktop_identity_has_distinct_service_application_and_network_endpoints() {
        let id = identity();
        assert_eq!(id.service_label, format!("{}.host", id.application_id));
        assert_eq!(id.linux_service, "weavelo-desktop.service");
        assert_ne!(id.service_label, "fun.hackerlife.weavelo.host");
        assert_ne!(id.control_port, 39271);
        assert_ne!(id.discovery_port, 38117);
    }

    #[test]
    fn desktop_identity_refuses_inherited_retained_profile_paths() {
        for path in [
            "/fixture/Weavelo/NativeMesh",
            "/fixture/weavelo/mesh",
            r"C:\Users\fixture\RemotePlay\Mesh",
        ] {
            assert!(retained_profile(Path::new(path)));
        }
        assert!(!retained_profile(Path::new("/fixture/Weavelo/Desktop")));
        assert!(!retained_profile(Path::new("/private/test-fixture")));
    }
}
