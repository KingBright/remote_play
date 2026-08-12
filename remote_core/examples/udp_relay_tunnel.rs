use remote_core::relay::{BoundUdpRelayTunnel, UdpRelayTunnelConfig};
use std::error::Error;
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let relay_addr = required_socket_addr("REMOTE_PLAY_RELAY_SERVER_ADDR")?;
    let group_id = required_env("REMOTE_PLAY_RELAY_GROUP")?;
    let peer_id = required_env("REMOTE_PLAY_RELAY_PEER_ID")?;
    let bind_addr = env_socket_addr(
        "REMOTE_PLAY_RELAY_BIND_ADDR",
        SocketAddr::from(([127, 0, 0, 1], 0)),
    )?;

    let mut config = UdpRelayTunnelConfig::new(bind_addr, relay_addr, group_id, peer_id)?;
    if let Some(target) = optional_socket_addr("REMOTE_PLAY_RELAY_LOCAL_TARGET_ADDR")? {
        config = config.with_local_target_addr(target);
    }

    let tunnel = BoundUdpRelayTunnel::bind(config).await?;
    println!(
        "RemotePlay UDP relay tunnel listening on {}",
        tunnel.local_addr()?
    );
    println!("Relay server: {relay_addr}");

    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let tunnel_task = tokio::spawn(tunnel.run(cancel_rx));
    tokio::signal::ctrl_c().await?;
    let _ = cancel_tx.send(());
    tunnel_task.await??;
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let value = std::env::var(name).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is required"),
        )
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is empty"),
        )
        .into());
    }
    Ok(value.to_string())
}

fn required_socket_addr(name: &'static str) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    let value = required_env(name)?;
    parse_socket_addr(name, &value)
}

fn optional_socket_addr(
    name: &'static str,
) -> Result<Option<SocketAddr>, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = value.to_string_lossy();
    if value.trim().is_empty() {
        return Ok(None);
    }
    parse_socket_addr(name, &value).map(Some)
}

fn env_socket_addr(
    name: &'static str,
    fallback: SocketAddr,
) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(fallback);
    };
    let value = value.to_string_lossy();
    if value.trim().is_empty() {
        return Ok(fallback);
    }
    parse_socket_addr(name, &value)
}

fn parse_socket_addr(
    name: &'static str,
    value: &str,
) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    value.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name}={value:?} is not a valid socket address: {err}"),
        )
        .into()
    })
}
