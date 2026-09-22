use remote_core::mesh::AppPrivateMeshConfigStore;
use std::error::Error;

const DEVICE_GROUP_ENSURE_ARG: &str = "--device-group-ensure";
const TRACE_ANALYZE_ARG: &str = "--trace-analyze";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    if handle_maintenance_command()? {
        return Ok(());
    }

    let mut config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
    let identity =
        AppPrivateMeshConfigStore::new(&config.mesh_dir).load_or_generate(&config.display_name)?;
    remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
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

fn handle_maintenance_command() -> Result<bool, Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return Ok(false);
    };

    match command.as_str() {
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
