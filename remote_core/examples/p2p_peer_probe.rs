use remote_core::p2p::{BoundP2pTunnel, P2pTunnelConfig, P2pTunnelSnapshot};
use std::error::Error;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, watch};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let rendezvous_name = std::env::var("REMOTE_PLAY_P2P_RENDEZVOUS_ADDR")
        .unwrap_or_else(|_| "p.hackerlife.fun:3478".to_string());
    let rendezvous = tokio::net::lookup_host(rendezvous_name.as_str())
        .await?
        .next()
        .ok_or("rendezvous did not resolve")?;
    let group_id = required_env("REMOTE_PLAY_P2P_GROUP_ID")?;
    let peer_id = required_env("REMOTE_PLAY_P2P_PEER_ID")?;
    let initiator = env_flag("REMOTE_PLAY_P2P_PROBE_INITIATOR");
    let timeout = Duration::from_secs(20);

    let echo_socket = UdpSocket::bind("127.0.0.1:0").await?;
    let echo_addr = echo_socket.local_addr()?;
    let tunnel = BoundP2pTunnel::bind(
        P2pTunnelConfig::new(
            "0.0.0.0:0".parse::<SocketAddr>()?,
            rendezvous,
            group_id,
            peer_id.clone(),
            Vec::new(),
        )?
        .with_local_target_addr(echo_addr),
    )
    .await?;
    let public_local = tunnel.local_addr()?;
    let (cancel_tx, _) = broadcast::channel(1);
    let (snapshot_tx, mut snapshot_rx) = watch::channel(P2pTunnelSnapshot::default());
    let tunnel_task = tokio::spawn(tunnel.run(cancel_tx.subscribe(), snapshot_tx));

    let echo_task = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let (len, source) = echo_socket.recv_from(&mut buf).await?;
            if buf[..len].starts_with(b"RPPROBE-PING:") {
                let mut reply = b"RPPROBE-PONG:".to_vec();
                reply.extend_from_slice(&buf[..len]);
                echo_socket.send_to(&reply, source).await?;
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), std::io::Error>(())
    });

    let peer = tokio::time::timeout(timeout, async {
        let peer = loop {
            if let Some(peer) = snapshot_rx
                .borrow()
                .peers
                .iter()
                .find(|peer| peer.direct_ready)
                .cloned()
            {
                break peer;
            }
            snapshot_rx
                .changed()
                .await
                .map_err(|_| "P2P snapshot closed")?;
        };
        Ok::<_, Box<dyn Error + Send + Sync>>(peer)
    })
    .await
    .map_err(|_| "P2P direct-ready timeout")??;

    println!(
        "p2p_direct_ready=PASS local_udp={} peer={} candidate={} route={}",
        public_local, peer.peer_id, peer.candidate, peer.local_endpoint
    );

    if initiator {
        let probe = UdpSocket::bind("127.0.0.1:0").await?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let payload = format!("RPPROBE-PING:{peer_id}:{nonce}").into_bytes();
        probe.send_to(&payload, peer.local_endpoint).await?;
        let mut buf = [0u8; 2048];
        let (len, _) = tokio::time::timeout(Duration::from_secs(5), probe.recv_from(&mut buf))
            .await
            .map_err(|_| "P2P payload echo timeout")??;
        let mut expected = b"RPPROBE-PONG:".to_vec();
        expected.extend_from_slice(&payload);
        if buf[..len] != expected {
            return Err("P2P payload echo mismatch".into());
        }
        println!("p2p_payload_roundtrip=PASS bytes={}", payload.len());
    } else {
        tokio::time::sleep(Duration::from_secs(8)).await;
        println!("p2p_responder_window=PASS");
    }

    let _ = cancel_tx.send(());
    echo_task.abort();
    tunnel_task.await??;
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let value = std::env::var(name).map_err(|_| format!("missing required env {name}"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(value)
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}
