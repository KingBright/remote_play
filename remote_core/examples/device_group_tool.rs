use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig, default_app_private_mesh_dir};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or_else(|| usage_error("missing mode"))?;
    let path = PathBuf::from(
        args.next()
            .ok_or_else(|| usage_error("missing credential file path"))?,
    );
    let store = AppPrivateMeshConfigStore::new(default_app_private_mesh_dir());

    match mode.as_str() {
        "export" => {
            if args.next().is_some() {
                return Err(usage_error("export accepts exactly one path"));
            }
            let config = store.load()?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "device group is not configured")
            })?;
            write_private(&path, config.invite_code().as_bytes())?;
            println!("device-group invite exported to {}", path.display());
        }
        "join" => {
            let display_name = args
                .next()
                .unwrap_or_else(|| "RemotePlay Device".to_string());
            if args.next().is_some() {
                return Err(usage_error("join accepts path and optional display name"));
            }
            let invite = fs::read_to_string(&path)?;
            let config = MeshConfig::from_invite_code(invite.trim(), display_name)?;
            store.save(&config)?;
            println!(
                "device-group joined network={} node={}",
                config.network_name, config.node_id
            );
        }
        _ => return Err(usage_error("mode must be export or join")),
    }
    Ok(())
}

fn usage_error(message: &str) -> Box<dyn Error + Send + Sync> {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{message}; usage: device_group_tool <export|join> <file> [display-name]"),
    )
    .into()
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
