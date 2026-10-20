//! Bounded native GUI acceptance with a separate authorized test identity.
//! Never enables local capture, clipboard, talkback or remote input.
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let parent = std::path::PathBuf::from(std::env::var("WEAVELO_RESTORED_TEST_ROOT")?);
    let output = std::path::PathBuf::from(std::env::var("WEAVELO_RESTORED_TEST_OUTPUT")?);
    if !parent.is_absolute() || !output.is_absolute() || output.exists() {
        return Err("Use a fresh absolute acceptance directory and output file".into());
    }
    if std::env::var("WEAVELO_MEDIA_ACCEPTANCE").as_deref() == Ok("1") {
        if std::env::var("WEAVELO_RESTORED_TEST_SECONDS").ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|seconds| (5..=180).contains(seconds)).is_none() {
            return Err("Media acceptance requires TEST_SECONDS in 5..=180".into());
        }
        if !cfg!(target_os = "macos") || parent.join("test-profile").exists()
            || !output.starts_with(&parent) || std::env::var_os("WEAVELO_RESTORED_TEST_SEQUENCE").is_some() {
            return Err("Media acceptance requires a fresh isolated macOS single-device fixture".into());
        }
        let device = std::env::var("WEAVELO_RESTORED_TEST_DEVICE")?;
        let source: serde_json::Value = serde_json::from_str(&std::env::var("WEAVELO_RESTORED_TEST_SOURCE")?)?;
        if device.len()!=32 || !device.bytes().all(|c|c.is_ascii_hexdigit())
            || source["source"]["Window"].as_u64().is_none_or(|id|id==0)
            || source["process_id"].as_i64().is_none_or(|pid|pid<=0)
            || !source["title"].as_str().is_some_and(|title|title.starts_with("Weavelo Media Fixture ")) {
            return Err("Media acceptance requires the exact owned fixture device/window/PID/title".into());
        }
    }
    std::fs::create_dir_all(&parent)?;
    let original =
        AppPrivateMeshConfigStore::new(std::env::var("WEAVELO_RESTORED_AUTH_PROFILE")?)
            .load()?
            .ok_or("authorized source profile missing")?;
    let mesh = MeshConfig::from_invite_code(
        &original.invite_code(),
        "Weavelo restored GUI acceptance",
    )?;
    let dir = parent.join("test-profile");
    AppPrivateMeshConfigStore::new(&dir).save(&mesh)?;
    remote_core::session_crypto::use_paired_session_secret(mesh.network_secret.expose_secret());
    let mut config = weavelo_app::UnifiedRuntimeConfig::app_defaults();
    // An isolated acceptance runtime must be able to avoid the installed owner's
    // discovery port; Relay/P2P forward discovery to this same nonzero port.
    config.discovery_port = remote_core::discovery::discovery_port_from_env()?;
    config.mesh_dir = dir;
    config.display_name = "Weavelo restored GUI acceptance".into();
    config.enable_passive_host = false;
    config.enable_client_receiver = false;
    config.enable_viewer_media = false;
    config.enable_talkback = false;
    config.enable_clipboard_sync = false;
    config.enable_file_transfer = true;
    config.host_bind_addr = "127.0.0.1:0".parse()?;
    // Route-specific acceptance is opt-in and never changes the product defaults.
    // Disabling other tunnel producers narrows discovery; it does not suppress
    // LAN discovery. The receipt's actual route must still match the requested
    // route before a caller can count a transport-specific test as passed.
    if let Ok(route)=std::env::var("WEAVELO_RESTORED_TEST_ROUTE") {
        match route.as_str() {
            "Relay" => {config.enable_p2p=false;if config.relay_endpoint.is_none(){return Err("Relay endpoint is unavailable in test configuration".into());}},
            "P2P" => {config.enable_p2p=true;config.relay_endpoint=None;},
            "LAN" => {config.enable_p2p=false;config.relay_endpoint=None;},
            _ => return Err("test route must be Relay, P2P or LAN".into()),
        }
    }

    weavelo_app::restored_ui::run_restored_gui(config).await
}
