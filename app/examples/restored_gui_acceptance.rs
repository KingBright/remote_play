//! Bounded native GUI acceptance with a separate authorized test identity.
//! Never enables local capture, clipboard, talkback or remote input.
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let parent = std::path::PathBuf::from(std::env::var("REMOTE_PLAY_RESTORED_TEST_ROOT")?);
    let output = std::path::PathBuf::from(std::env::var("REMOTE_PLAY_RESTORED_TEST_OUTPUT")?);
    if !parent.is_absolute() || !output.is_absolute() || output.exists() {
        return Err("Use a fresh absolute acceptance directory and output file".into());
    }
    std::fs::create_dir_all(&parent)?;
    let original =
        AppPrivateMeshConfigStore::new(std::env::var("REMOTE_PLAY_RESTORED_AUTH_PROFILE")?)
            .load()?
            .ok_or("authorized source profile missing")?;
    let mesh = MeshConfig::from_invite_code(
        &original.invite_code(),
        "RemotePlay restored GUI acceptance",
    )?;
    let dir = parent.join("test-profile");
    AppPrivateMeshConfigStore::new(&dir).save(&mesh)?;
    remote_core::session_crypto::use_paired_session_secret(mesh.network_secret.expose_secret());
    let mut config = remote_play_app::UnifiedRuntimeConfig::app_defaults();
    config.mesh_dir = dir;
    config.display_name = "RemotePlay restored GUI acceptance".into();
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
    if let Ok(route)=std::env::var("REMOTE_PLAY_RESTORED_TEST_ROUTE") {
        match route.as_str() {
            "Relay" => {config.enable_p2p=false;if config.relay_endpoint.is_none(){return Err("Relay endpoint is unavailable in test configuration".into());}},
            "P2P" => {config.enable_p2p=true;config.relay_endpoint=None;},
            "LAN" => {config.enable_p2p=false;config.relay_endpoint=None;},
            _ => return Err("test route must be Relay, P2P or LAN".into()),
        }
    }

    remote_play_app::restored_ui::run_restored_gui(config).await
}
