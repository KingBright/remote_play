use remote_core::mesh::{
    AppPrivateMeshConfigStore, EASYTIER_SIDECAR_LOG_FILE_NAME, EasyTierBinaryLocator,
    EasyTierHealthMonitorConfig, EasyTierHealthMonitorHandle, EasyTierSidecarManager, MeshConfig,
    REMOTE_PLAY_EASYTIER_BIN_ENV, spawn_easytier_health_monitor,
};
use std::error::Error;
use std::path::Path;
use std::time::Duration;

const MESH_ENSURE_CONFIG_ARG: &str = "--mesh-ensure-config";
const MESH_LAUNCHD_PLIST_ARG: &str = "--mesh-launchd-plist";
const MESH_DAEMON_RUN_ARG: &str = "--mesh-daemon-run";
const DEFAULT_MESH_DAEMON_LABEL: &str = "com.remoteplay.mesh";
const MESH_DAEMON_LOG_FILE_NAME: &str = "remoteplay-mesh-daemon.log";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    if handle_maintenance_command().await? {
        return Ok(());
    }

    let mut config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
    let headless = headless_enabled();
    if headless {
        config.enable_viewer_media = false;
    }
    #[cfg(target_os = "macos")]
    if !headless {
        return remote_play_app::run_unified_gui(config).await;
    }

    let runtime = remote_play_app::start_unified_runtime(config).await?;
    remote_core::stats::Statistics::start_reporter(runtime.stats.clone(), "Unified", 1);
    println!(
        "RemotePlay unified runtime started with {} background task(s). Press Ctrl-C to stop.",
        runtime.background_task_count()
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn headless_enabled() -> bool {
    std::env::var("REMOTE_PLAY_HEADLESS")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

async fn handle_maintenance_command() -> Result<bool, Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return Ok(false);
    };

    match command.as_str() {
        MESH_ENSURE_CONFIG_ARG => {
            let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
            let mesh = ensure_mesh_config(&config)?;
            println!("mesh_dir={}", config.mesh_dir.display());
            println!("network_name={}", mesh.network_name);
            println!("node_id={}", mesh.node_id);
            Ok(true)
        }
        MESH_LAUNCHD_PLIST_ARG => {
            let label = args
                .next()
                .unwrap_or_else(|| DEFAULT_MESH_DAEMON_LABEL.to_string());
            let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
            let plist = build_mesh_launchd_plist(&config, &label)?;
            print!("{plist}");
            Ok(true)
        }
        MESH_DAEMON_RUN_ARG => {
            let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
            run_mesh_daemon(config).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn ensure_mesh_config(
    config: &remote_play_app::UnifiedRuntimeConfig,
) -> Result<remote_core::mesh::MeshConfig, Box<dyn Error + Send + Sync>> {
    let store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
    Ok(store.load_or_generate(&config.display_name)?)
}

struct MeshDaemonState {
    config_key: Option<String>,
    health: Option<EasyTierHealthMonitorHandle>,
}

async fn run_mesh_daemon(
    config: remote_play_app::UnifiedRuntimeConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut state = MeshDaemonState {
        config_key: None,
        health: None,
    };

    println!(
        "RemotePlay mesh daemon started. mesh_dir={}",
        config.mesh_dir.display()
    );

    loop {
        if let Err(err) = reload_mesh_daemon_if_config_changed(&config, &mut state).await {
            eprintln!("RemotePlay mesh daemon reload failed: {err}");
        }
        if let Some(health) = &mut state.health
            && health.snapshot_rx.has_changed().unwrap_or(false)
        {
            let snapshot = health.snapshot_rx.borrow_and_update().clone();
            println!(
                "RemotePlay mesh daemon status: state={:?} process={:?} virtual_ip={:?} message={}",
                snapshot.state, snapshot.process_state, snapshot.virtual_ip, snapshot.message
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn reload_mesh_daemon_if_config_changed(
    config: &remote_play_app::UnifiedRuntimeConfig,
    state: &mut MeshDaemonState,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
    let mesh = store.load_or_generate(&config.display_name)?;
    let config_key = mesh_daemon_config_key(&mesh);
    if state.config_key.as_ref() == Some(&config_key) {
        return Ok(());
    }

    drop(state.health.take());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let locator = EasyTierBinaryLocator::from_environment();
    let mut manager = EasyTierSidecarManager::from_locator(mesh.clone(), &locator)?;
    manager.set_log_file_path(store.root_dir().join(EASYTIER_SIDECAR_LOG_FILE_NAME));
    manager.start().await?;
    let expected_virtual_ip = manager.config().virtual_ipv4.map(std::net::IpAddr::V4);
    let health =
        spawn_easytier_health_monitor(manager, mesh_daemon_health_config(), expected_virtual_ip);

    println!(
        "RemotePlay mesh daemon loaded group: network_name={} node_id={}",
        mesh.network_name, mesh.node_id
    );
    state.config_key = Some(config_key);
    state.health = Some(health);
    Ok(())
}

fn mesh_daemon_health_config() -> EasyTierHealthMonitorConfig {
    EasyTierHealthMonitorConfig::default()
}

fn mesh_daemon_config_key(mesh: &MeshConfig) -> String {
    let peers = mesh
        .initial_peers
        .iter()
        .map(|peer| peer.as_str())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|{}|{}|{}|{}",
        mesh.network_name,
        mesh.network_secret.expose_secret(),
        mesh.node_id,
        mesh.display_name,
        peers
    )
}

fn build_mesh_launchd_plist(
    config: &remote_play_app::UnifiedRuntimeConfig,
    label: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
    let _mesh = store.load_or_generate(&config.display_name)?;
    let locator = EasyTierBinaryLocator::from_environment();
    let easytier = locator.locate()?;
    let daemon_log = store.root_dir().join(MESH_DAEMON_LOG_FILE_NAME);
    let program = std::env::current_exe()?;
    let args = vec![MESH_DAEMON_RUN_ARG.to_string()];
    let home = std::env::var("HOME").unwrap_or_default();
    let mesh_dir = config.mesh_dir.display().to_string();
    let easytier_bin = easytier.path.display().to_string();
    let env = [
        ("HOME", home.as_str()),
        ("REMOTE_PLAY_DISPLAY_NAME", config.display_name.as_str()),
        ("REMOTE_PLAY_MESH_DIR", mesh_dir.as_str()),
        (REMOTE_PLAY_EASYTIER_BIN_ENV, easytier_bin.as_str()),
    ];

    Ok(render_launchd_plist(
        label,
        &program,
        &args,
        &env,
        &daemon_log,
    ))
}

fn render_launchd_plist(
    label: &str,
    program: &Path,
    args: &[String],
    env: &[(&str, &str)],
    log_path: &Path,
) -> String {
    let mut plist = String::new();
    plist.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    plist.push_str(
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
    );
    plist.push_str("<plist version=\"1.0\">\n<dict>\n");
    plist.push_str("    <key>Label</key>\n");
    plist.push_str(&format!("    <string>{}</string>\n", xml_escape(label)));
    plist.push_str("    <key>ProgramArguments</key>\n    <array>\n");
    plist.push_str(&format!(
        "        <string>{}</string>\n",
        xml_escape(&program.display().to_string())
    ));
    for arg in args {
        plist.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }
    plist.push_str("    </array>\n");
    plist.push_str("    <key>EnvironmentVariables</key>\n    <dict>\n");
    for (key, value) in env {
        plist.push_str(&format!("        <key>{}</key>\n", xml_escape(key)));
        plist.push_str(&format!("        <string>{}</string>\n", xml_escape(value)));
    }
    plist.push_str("    </dict>\n");
    plist.push_str("    <key>RunAtLoad</key>\n    <true/>\n");
    plist.push_str("    <key>KeepAlive</key>\n    <true/>\n");
    plist.push_str("    <key>StandardOutPath</key>\n");
    plist.push_str(&format!(
        "    <string>{}</string>\n",
        xml_escape(&log_path.display().to_string())
    ));
    plist.push_str("    <key>StandardErrorPath</key>\n");
    plist.push_str(&format!(
        "    <string>{}</string>\n",
        xml_escape(&log_path.display().to_string())
    ));
    plist.push_str("</dict>\n</plist>\n");
    plist
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn launchd_plist_runs_dynamic_mesh_daemon_without_static_secrets() {
        let plist = render_launchd_plist(
            "com.remoteplay.mesh&test",
            &PathBuf::from("/Applications/RemotePlay.app/Contents/MacOS/remote_play"),
            &[MESH_DAEMON_RUN_ARG.to_string()],
            &[
                ("REMOTE_PLAY_DISPLAY_NAME", "Desk \"A\""),
                (
                    "REMOTE_PLAY_MESH_DIR",
                    "/Users/me/Library/Application Support/RemotePlay/Mesh",
                ),
                (
                    REMOTE_PLAY_EASYTIER_BIN_ENV,
                    "/Applications/RemotePlay.app/Contents/Resources/bin/easytier-core",
                ),
            ],
            &PathBuf::from(
                "/Users/me/Library/Application Support/RemotePlay/Mesh/remoteplay-mesh-daemon.log",
            ),
        );

        assert!(plist.contains("com.remoteplay.mesh&amp;test"));
        assert!(plist.contains(MESH_DAEMON_RUN_ARG));
        assert!(plist.contains("Desk &quot;A&quot;"));
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains(REMOTE_PLAY_EASYTIER_BIN_ENV));
        assert!(!plist.contains("--network-secret"));
        assert!(!plist.contains("--network-name"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>StandardErrorPath</key>"));
    }

    #[test]
    fn mesh_daemon_config_key_changes_when_group_changes() {
        let first = MeshConfig::generate("Desk");
        let mut second = first.clone();
        second.network_name = "other-network".to_string();

        assert_ne!(
            mesh_daemon_config_key(&first),
            mesh_daemon_config_key(&second)
        );
    }

    #[test]
    fn mesh_daemon_health_config_does_not_spawn_cli_probe_by_default() {
        assert!(mesh_daemon_health_config().probe.is_none());
    }
}
