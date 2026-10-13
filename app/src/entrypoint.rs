//! Shared production entry: also used by the isolated lifecycle review executable.
use crate as remote_play_app;
use remote_core::mesh::AppPrivateMeshConfigStore;
use std::error::Error;

const DEVICE_GROUP_ENSURE_ARG: &str = "--device-group-ensure";
const TRACE_ANALYZE_ARG: &str = "--trace-analyze";

pub async fn run() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    if handle_maintenance_command()? {
        return Ok(());
    }

    let mut config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--background-service"] {
        #[cfg(target_os = "macos")]
        return remote_play_app::background_service::run(config).await;
        #[cfg(not(target_os = "macos"))]
        return Err("Separated background/GUI lifecycle is currently validated on macOS only".into());
    }
    if arguments == ["--gui"] {
        #[cfg(all(target_os = "macos", feature = "gpui-restoration"))]
        return remote_play_app::restored_ui::run_attached_gui(config).await;
        #[cfg(not(all(target_os = "macos", feature = "gpui-restoration")))]
        return Err("Attached GPUI frontend is currently available on macOS only".into());
    }
    if !arguments.is_empty() {
        return Err("Unknown RemotePlay command; no window or runtime was started".into());
    }
    // A migrated user installation manually opens the same app. Fail closed if
    // its service endpoint is stale; never fall back to a second standalone host.
    #[cfg(all(target_os = "macos", feature = "gpui-restoration"))]
    if !headless_enabled() && (config.mesh_dir.join("background.sock").symlink_metadata().is_ok()
        || config.mesh_dir.join(remote_play_app::background_service::ROLE_MARKER).symlink_metadata().is_ok()) {
        return remote_play_app::restored_ui::run_attached_gui(config).await;
    }
    let identity =
        AppPrivateMeshConfigStore::new(&config.mesh_dir).load_or_generate(&config.display_name)?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
    let headless = headless_enabled();
    if headless {
        config.enable_viewer_media = false;
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if !headless {
        #[cfg(feature = "gpui-restoration")]
        return remote_play_app::restored_ui::run_restored_gui(config).await;
        #[cfg(not(feature = "gpui-restoration"))]
        return Err("GPUI desktop GUI was not compiled; no alternate interface was started".into());
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    let Some(_runtime_instance) =
        remote_play_app::desktop::claim_headless_runtime(&config.mesh_dir)?
    else {
        return Ok(());
    };
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

fn handle_maintenance_command() -> Result<bool, Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return Ok(false);
    };

    match command.as_str() {
        "--gui-status-json" | "--gui-receive" | "--gui-stop-receive" => {
            #[cfg(all(unix, feature = "gpui-restoration"))]
            {
                let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
                let arguments = std::iter::once(command).chain(args).collect::<Vec<_>>();
                println!("{}", crate::desktop::directed_receive::command(&config.mesh_dir, &arguments)?);
                Ok(true)
            }
            #[cfg(not(all(unix, feature = "gpui-restoration")))]
            { Err("The private GUI receive adapter is available on macOS/Linux".into()) }
        }
        "--background-status-json" => {
            if args.next().is_some() { return Err("--background-status-json takes no extra arguments".into()); }
            #[cfg(target_os = "macos")]
            {
                let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
                let client = remote_play_app::background_service::Client::connect(&config.mesh_dir)?;
                println!("{}", client.status());
                Ok(true)
            }
            #[cfg(not(target_os = "macos"))]
            { Err("Separated lifecycle is currently available on macOS only".into()) }
        }
        "--product-info-json" => {
            if args.next().is_some() {
                return Err("--product-info-json takes no extra arguments".into());
            }
            // Runs before creating/reading a device profile or opening a window.
            println!(
                "{}",
                serde_json::to_string(&remote_play_app::gui_backend::product_info())?
            );
            Ok(true)
        }
        DEVICE_GROUP_ENSURE_ARG => {
            let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
            let store = AppPrivateMeshConfigStore::new(&config.mesh_dir);
            let group = store.load_or_generate(&config.display_name)?;
            println!("device_group_dir={}", config.mesh_dir.display());
            println!("network_name={}", group.network_name);
            println!("node_id={}", group.node_id);
            Ok(true)
        }
        TRACE_ANALYZE_ARG => {
            let report = args
                .next()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|content| {
                    serde_json::from_str::<protocol::PipelineTelemetryReport>(&content).ok()
                })
                .unwrap_or_else(|| {
                    remote_core::PipelineTelemetryEngine::new(1, 100).generate_report()
                });
            let diagnostic = remote_core::BottleneckAnalyzer::analyze(&report);
            println!(
                "{}",
                remote_core::BottleneckAnalyzer::format_diagnostic_report(&diagnostic)
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_commands_are_product_native() {
        assert_eq!(DEVICE_GROUP_ENSURE_ARG, "--device-group-ensure");
        assert_eq!(TRACE_ANALYZE_ARG, "--trace-analyze");
    }
}
