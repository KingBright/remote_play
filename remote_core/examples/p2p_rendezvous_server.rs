use remote_core::p2p::{BoundP2pRendezvousServer, P2pRendezvousServerConfig};
use std::error::Error;
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let bind_addr = env_socket_addr(
        "REMOTE_PLAY_P2P_RENDEZVOUS_BIND_ADDR",
        SocketAddr::from(([0, 0, 0, 0], 3478)),
    )?;
    let config = P2pRendezvousServerConfig::new(bind_addr)
        .with_event_logging(env_flag_or("REMOTE_PLAY_P2P_RENDEZVOUS_LOG", false))
        .with_limits(
            env_usize_or("REMOTE_PLAY_P2P_MAX_PEERS_PER_GROUP", 16)?,
            env_usize_or("REMOTE_PLAY_P2P_MAX_TOTAL_PEERS", 4096)?,
        );
    let server = BoundP2pRendezvousServer::bind(config).await?;
    println!(
        "RemotePlay P2P rendezvous listening on {}",
        server.local_addr()?
    );

    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let task = tokio::spawn(server.run(cancel_rx));
    tokio::signal::ctrl_c().await?;
    let _ = cancel_tx.send(());
    task.await??;
    Ok(())
}

fn env_flag_or(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => match value.as_str() {
            "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON" => true,
            "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

fn env_usize_or(name: &'static str, default: usize) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(default);
    };
    let value = value.to_string_lossy();
    let parsed = value.parse::<usize>().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name}={value:?} is not a positive integer: {err}"),
        )
    })?;
    if parsed == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must be greater than zero"),
        )
        .into());
    }
    Ok(parsed)
}

fn env_socket_addr(
    name: &'static str,
    fallback: SocketAddr,
) -> Result<SocketAddr, Box<dyn Error + Send + Sync>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(fallback);
    };
    let value = value.to_string_lossy();
    value.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name}={value:?} is not a valid socket address: {err}"),
        )
        .into()
    })
}
