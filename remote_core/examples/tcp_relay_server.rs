use remote_core::relay::{BoundTcpRelayServer, TcpRelayServerConfig};
use std::error::Error;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let bind_addr = env_socket_addr(
        "REMOTE_PLAY_RELAY_BIND_ADDR",
        SocketAddr::from(([127, 0, 0, 1], 39491)),
    )?;
    let config = TcpRelayServerConfig::new(bind_addr)
        .with_event_logging(env_flag_or("REMOTE_PLAY_RELAY_LOG", false))
        .with_limits(
            env_usize_or("REMOTE_PLAY_RELAY_MAX_CONNECTIONS", 128)?,
            env_usize_or("REMOTE_PLAY_RELAY_MAX_PEERS_PER_GROUP", 8)?,
            env_usize_or("REMOTE_PLAY_RELAY_WRITER_QUEUE_CAPACITY", 16)?,
        )
        .with_idle_timeout(Duration::from_secs(env_u64_or(
            "REMOTE_PLAY_RELAY_IDLE_TIMEOUT_SECONDS",
            30,
        )?));
    let server = BoundTcpRelayServer::bind(config).await?;
    let local_addr = server.local_addr()?;
    let websocket = env_flag_or("REMOTE_PLAY_RELAY_WEBSOCKET", false);
    println!(
        "RemotePlay {} relay listening on {local_addr}",
        if websocket { "WebSocket" } else { "TCP" }
    );

    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let server_task = tokio::spawn(async move {
        if websocket {
            server.run_websocket(cancel_rx).await
        } else {
            server.run(cancel_rx).await
        }
    });
    tokio::signal::ctrl_c().await?;
    let _ = cancel_tx.send(());
    server_task.await??;
    Ok(())
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

fn env_u64_or(name: &'static str, default: u64) -> Result<u64, Box<dyn Error + Send + Sync>> {
    let parsed = env_usize_or(name, usize::try_from(default).unwrap_or(usize::MAX))?;
    u64::try_from(parsed).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is too large"),
        )
        .into()
    })
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
