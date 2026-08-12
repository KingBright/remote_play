use remote_core::relay::{BoundUdpRelayServer, UdpRelayServerConfig};
use std::error::Error;
use std::net::SocketAddr;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let bind_addr = env_socket_addr(
        "REMOTE_PLAY_RELAY_BIND_ADDR",
        SocketAddr::from(([0, 0, 0, 0], 39490)),
    )?;
    let server = BoundUdpRelayServer::bind(
        UdpRelayServerConfig::new(bind_addr)
            .with_event_logging(env_flag_or("REMOTE_PLAY_RELAY_LOG", false)),
    )
    .await?;
    println!("RemotePlay UDP relay listening on {}", server.local_addr()?);

    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let server_task = tokio::spawn(server.run(cancel_rx));
    tokio::signal::ctrl_c().await?;
    let _ = cancel_tx.send(());
    server_task.await??;
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
