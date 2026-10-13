//! Bounded, explicit offline GUI acceptance fixtures. Never captures a screen or injects input.
use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("profile") if args.len() == 4 => {
            let source = AppPrivateMeshConfigStore::new(&args[2]).load()?.ok_or("source identity missing")?;
            let target = PathBuf::from(&args[3]);
            if target.exists() { return Err("test profile already exists".into()); }
            let identity = MeshConfig::from_invite_code(&source.invite_code(), "RemotePlay GUI acceptance")?;
            AppPrivateMeshConfigStore::new(&target).save(&identity)?;
            println!("ISOLATED_PROFILE_READY");
        }
        Some("host") if args.len() == 6 => {
            let identity = AppPrivateMeshConfigStore::new(&args[2]).load()?.ok_or("test identity missing")?;
            remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
            let dir = PathBuf::from(&args[3]); std::fs::create_dir_all(&dir)?;
            let path = dir.join("remoteplay-acceptance-512k.bin");
            let data: Vec<_> = (0..524288).map(|n| (n % 251) as u8).collect();
            if path.exists() {
                if std::fs::read(&path)? != data { return Err("fixture path already contains other data".into()); }
            } else {
                use std::io::Write;
                std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?.write_all(&data)?;
            }
            remote_core::shared_files::shared_file_catalog().publish(&path, remote_core::shared_files::current_share_scope()).await?;
            let address: std::net::SocketAddr = args[4].parse()?;
            if !address.ip().is_loopback() { return Err("fixture host must bind loopback".into()); }
            let seconds = args[5].parse::<u64>()?.clamp(10, 180);
            let cfg = host::HostServiceConfig { bind_addr: address, stats: remote_core::Statistics::new(), enable_clipboard_sync: false, enable_file_transfer: true, enable_talkback: false };
            println!("SYNTHETIC_FILE_HOST_READY {address}");
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(seconds), host::run_host_service(cfg)).await { result?; }
            println!("SYNTHETIC_FILE_HOST_STOPPED");
        }
        _ => return Err("usage: desktop_acceptance_fixture profile SOURCE NEW_PROFILE | host PROFILE DIRECTORY LOOPBACK:PORT SECONDS".into()),
    }
    Ok(())
}
