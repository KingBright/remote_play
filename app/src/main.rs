#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    remote_play_app::entrypoint::run().await
}
