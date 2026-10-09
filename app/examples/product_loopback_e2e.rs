//! Full production RestoredDashboard over authenticated loopback transport.
//! The explicit peer offers generated HEVC sources; it never captures a screen,
//! injects OS input, reads an installed identity, or advertises beyond loopback.
#[cfg(target_os = "macos")]
#[path = "support/product_loopback_peer.rs"]
mod peer;
#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use remote_core::{discovery::*, mesh::MeshConfig};
    use remote_play_app::{UnifiedRuntimeHandle, UnifiedServiceOwner, UnifiedServiceOwnerConfig};
    use std::{path::PathBuf, sync::Arc, time::Duration};
    let root = PathBuf::from(format!(
        "/tmp/remoteplay-product-loopback-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let profile = root.join("mesh");
    // Set only task-specific configuration before creating any runtime threads.
    unsafe {
        std::env::set_var("REMOTE_PLAY_DEVICE_GROUP_DIR", &profile);
        std::env::set_var("REMOTE_PLAY_RESTORED_TEST_OUTPUT", root.join("gui.json"));
        std::env::set_var("REMOTE_PLAY_RESTORED_TEST_SECONDS", "180");
        std::env::set_var("RP_LOOPBACK_PRODUCT_OBSERVATIONS", "1");
    }
    let mut preferences = remote_play_app::preferences::UserPreferences::default();
    preferences.ui.pause_when_inactive = false;
    preferences.stream = remote_play_app::preferences::StreamPreferences {
        width: 640,
        height: 360,
        fps: 20,
        bitrate_kbps: 2500,
    };
    preferences.side_services = remote_play_app::preferences::SideServicePreferences {
        clipboard_sync: false,
        file_transfer: false,
        talkback: false,
    };
    preferences.extra.insert(
        "receive_dir".into(),
        serde_json::json!(root.join("received")),
    );
    let preferences_path = remote_play_app::preferences::UserPreferences::default_path();
    if preferences_path != root.join("preferences.json") {
        return Err("loopback preferences escaped the isolated fixture directory".into());
    }
    preferences.save_to_path(&preferences_path)?;
    let identity = MeshConfig::generate("RemotePlay product loopback acceptance");
    remote_core::init_crypto_provider();
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    // Only this newly generated in-memory test identity is used. It is not
    // printed, exported, derived from, or installed over an existing profile.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(3)
        .enable_all()
        .build()?;
    let (owner, controls) = runtime.block_on(async {
        let mut fixture = peer::start(&root, &identity.network_name).await?;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let discovery_address = socket.local_addr()?;
        drop(socket);
        let discovery = DiscoveryRuntimeConfig {
            bind_addr: discovery_address,
            announce_targets: vec![],
            route_overrides: vec![],
            relay_routes: vec![],
            announcement: DiscoveryAnnouncement {
                network_name: identity.network_name.clone(),
                device_id: identity.node_id.clone(),
                display_name: identity.display_name.clone(),
                control_port: 0,
                virtual_ip: None,
                capabilities: DiscoveryCapabilities {
                    can_view: true,
                    ..Default::default()
                },
                scope: DiscoveryScope::Lan,
                ttl: Duration::from_secs(4),
            },
            announce_interval: Duration::from_secs(1),
            prune_interval: Duration::from_secs(1),
            accept_any_network: false,
        };
        let owner = Arc::new(
            UnifiedServiceOwner::start(UnifiedServiceOwnerConfig {
                discovery: Some(discovery),
                ..Default::default()
            })
            .await?,
        );
        fixture.announce(discovery_address).await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((owner, fixture))
    })?;
    let handle = Arc::new(UnifiedRuntimeHandle::from_workspace_owner(owner));
    let _entered = runtime.enter();
    println!(
        "PRODUCT_LOOPBACK_READY root={} renderer=restored-original-gpui",
        root.display()
    );
    std::fs::write(
        root.join("provenance.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "pid":std::process::id(),"renderer":"restored-original-gpui","full_product_tree":true,
            "real_workspace_protocol":true,"peer":"explicit generated HEVC loopback fixture",
            "listeners":"loopback","installed_profile_read":false,"os_capture":false,"os_input_injection":false,
            "version":env!("CARGO_PKG_VERSION"),"executable":std::env::current_exe()?.display().to_string()
        }))?,
    )?;
    let result = remote_play_app::restored_ui::run_restored_workspace_runtime(handle, &profile);
    drop(controls);
    drop(_entered);
    drop(runtime);
    result
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This explicit product/loopback acceptance currently runs on macOS.");
    std::process::exit(1);
}
