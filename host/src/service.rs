use crate::{StreamingRunConfig, env_flag_enabled, run_streaming};
use protocol::{AudioControlTarget, ContentKind, ControlMessage, DataEnvelope};
use remote_core::net::UdpMultiplexer;
use remote_core::session_crypto::{
    SessionCrypto, load_session_psk, mac_session_accept, mac_session_hello, now_unix_ms,
    random_bytes_16, require_session_auth, verify_session_mac,
};
use remote_core::stats::Statistics;
use std::collections::HashSet;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

#[derive(Clone)]
pub struct HostServiceConfig {
    pub bind_addr: SocketAddr,
    pub stats: Arc<Statistics>,
}

fn is_active_client(active_client_addr: Option<SocketAddr>, source: SocketAddr) -> bool {
    active_client_addr == Some(source)
}

pub async fn run_host_service(
    config: HostServiceConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let HostServiceConfig { bind_addr, stats } = config;
    let multiplexer = UdpMultiplexer::bind(&bind_addr.to_string()).await?;
    let (udp_sender, udp_receiver) = multiplexer.split();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        println!("Host implementation is currently macOS, Linux, and Windows only.");
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        use remote_core::InputInjector;
        use remote_core::net::MultiplexedPacket;

        #[cfg(target_os = "macos")]
        let input_injector: Arc<dyn InputInjector + Send + Sync> =
            Arc::new(crate::input_injector::MacInputInjector::new()?);

        #[cfg(target_os = "linux")]
        let input_injector: Arc<dyn InputInjector + Send + Sync> =
            Arc::new(crate::linux_input::LinuxUinputInjector::new()?);

        #[cfg(target_os = "windows")]
        let input_injector: Arc<dyn InputInjector + Send + Sync> =
            Arc::new(crate::windows_input::WindowsInputInjector::new()?);

        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;
        let mut active_clipboard_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        let mut active_file_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        let mut active_talkback_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        #[cfg(target_os = "macos")]
        let mut active_talkback_settings_tx: Option<
            watch::Sender<crate::talkback_player::TalkbackPlaybackSettings>,
        > = None;
        #[cfg(not(target_os = "macos"))]
        let mut active_talkback_settings_tx: Option<watch::Sender<()>> = None;

        let mut active_session_id: Option<u32> = None;
        let mut active_client_addr: Option<SocketAddr> = None;
        let mut active_settings_tx: Option<watch::Sender<crate::StreamSettings>> = None;
        let mut active_keyframe_requested: Option<Arc<AtomicBool>> = None;
        let mut authenticated_peers: HashSet<SocketAddr> = HashSet::new();
        let session_psk = load_session_psk();
        let require_auth = require_session_auth();
        let mut last_heartbeat = Instant::now();

        println!("Listening for ControlMessages on {}...", bind_addr);

        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if active_cancel_tx.is_some() && last_heartbeat.elapsed() > Duration::from_secs(15) {
                        println!("Heartbeat timeout (>15s)! Stopping stream...");
                        if let Some(tx) = active_cancel_tx.take() {
                            let _ = tx.send(());
                        }
                        active_clipboard_tx = None;
                        active_file_tx = None;
                        active_talkback_tx = None;
                        active_talkback_settings_tx = None;
                        active_settings_tx = None;
                        active_session_id = None;
                        active_client_addr = None;
                        input_injector.release_all_input();
                    }
                }
                recv_res = udp_receiver.recv() => {
                    match recv_res {
                        Ok(MultiplexedPacket::Control(msg, client_addr)) => {
                            if is_active_client(active_client_addr, client_addr) {
                                last_heartbeat = Instant::now();
                            }
                            match msg {
                                ControlMessage::Input(input_event) => {
                                    if is_active_client(active_client_addr, client_addr) {
                                        let _ = input_injector.inject_input(input_event);
                                    }
                                }
                                ControlMessage::Heartbeat => {
                                    // already refreshed last_heartbeat
                                }
                                ControlMessage::Ping { client_send_ts } => {
                                    if is_active_client(active_client_addr, client_addr) {
                                        let now_ms = (std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            & 0xFFFFFFFFFFFFFFFF) as u64;
                                        let pong = ControlMessage::Pong {
                                            client_send_ts,
                                            host_recv_ts: now_ms,
                                            host_send_ts: now_ms,
                                        };
                                        let _ = udp_sender.send_control(&pong, client_addr).await;
                                    }
                                }
                                ControlMessage::UpdateStreamSettings { width, height, fps, bitrate_kbps, session_id } => {
                                    if is_active_client(active_client_addr, client_addr)
                                        && active_session_id == Some(session_id)
                                        && let Some(tx) = &active_settings_tx
                                    {
                                        let _ = tx.send(crate::StreamSettings {
                                            width,
                                            height,
                                            fps,
                                            bitrate_kbps,
                                        });
                                    }
                                }
                                ControlMessage::RequestKeyframe { session_id } => {
                                    if is_active_client(active_client_addr, client_addr)
                                        && active_session_id == Some(session_id)
                                        && let Some(flag) = &active_keyframe_requested
                                    {
                                        flag.store(true, Relaxed);
                                    }
                                }
                                ControlMessage::SessionHello {
                                    nonce,
                                    timestamp_ms,
                                    mac,
                                } => {
                                    let Some(psk) = session_psk.as_ref() else {
                                        let reject = ControlMessage::SessionReject {
                                            reason: "host has no session PSK configured".into(),
                                        };
                                        let _ = udp_sender.send_control(&reject, client_addr).await;
                                        continue;
                                    };
                                    let expected = mac_session_hello(psk, &nonce, timestamp_ms);
                                    if let Err(err) =
                                        verify_session_mac(&expected, &mac, timestamp_ms, now_unix_ms())
                                    {
                                        let reject = ControlMessage::SessionReject {
                                            reason: err.to_string(),
                                        };
                                        let _ = udp_sender.send_control(&reject, client_addr).await;
                                        continue;
                                    }
                                    let salt = random_bytes_16();
                                    let ts = now_unix_ms();
                                    let accept_mac = mac_session_accept(psk, &salt, ts);
                                    match (
                                        SessionCrypto::from_psk(psk, &salt),
                                        SessionCrypto::from_psk(psk, &salt),
                                    ) {
                                        (Ok(send_crypto), Ok(recv_crypto)) => {
                                            let _ = udp_sender.install_crypto(send_crypto);
                                            let _ = udp_receiver.install_crypto(recv_crypto);
                                            authenticated_peers.insert(client_addr);
                                            let accept = ControlMessage::SessionAccept {
                                                salt,
                                                timestamp_ms: ts,
                                                mac: accept_mac,
                                            };
                                            let _ = udp_sender.send_control(&accept, client_addr).await;
                                        }
                                        (Err(err), _) | (_, Err(err)) => {
                                            let reject = ControlMessage::SessionReject {
                                                reason: err.to_string(),
                                            };
                                            let _ = udp_sender.send_control(&reject, client_addr).await;
                                        }
                                    }
                                }
                                ControlMessage::StopStream => {
                                    if !is_active_client(active_client_addr, client_addr) {
                                        continue;
                                    }
                                    println!("Received StopStream from client. Stopping...");
                                    if let Some(tx) = active_cancel_tx.take() {
                                        let _ = tx.send(());
                                    }
                                    active_clipboard_tx = None;
                                    active_file_tx = None;
                                    active_talkback_tx = None;
                                    active_talkback_settings_tx = None;
                                    active_settings_tx = None;
                                    active_session_id = None;
                                    active_client_addr = None;
                                    input_injector.release_all_input();
                                }
                                ControlMessage::AudioControl { session_id, target, muted, volume_percent } => {
                                    if is_active_client(active_client_addr, client_addr)
                                        && active_session_id == Some(session_id) {
                                        #[cfg(target_os = "macos")]
                                        if target == AudioControlTarget::ViewerTalkbackPlayback
                                            && let Some(settings_tx) = &active_talkback_settings_tx
                                        {
                                            let _ = settings_tx.send(
                                                crate::talkback_player::TalkbackPlaybackSettings {
                                                    muted,
                                                    volume_percent,
                                                },
                                            );
                                        }
                                    }
                                }
                                ControlMessage::StartStream { width, height, fps, bitrate_kbps, session_id } => {
                                    if require_auth && !authenticated_peers.contains(&client_addr) {
                                        let reject = ControlMessage::SessionReject {
                                            reason: "StartStream requires SessionHello".into(),
                                        };
                                        let _ = udp_sender.send_control(&reject, client_addr).await;
                                        continue;
                                    }
                                    println!("Received StartStream from {} with {}x{}@{}fps ({} kbps)", client_addr, width, height, fps, bitrate_kbps);

                                    if let Some(tx) = active_cancel_tx.take() {
                                        let _ = tx.send(());
                                    }
                                    active_clipboard_tx = None;
                                    active_file_tx = None;
                                    active_talkback_tx = None;
                                    active_talkback_settings_tx = None;
                                    active_session_id = Some(session_id);
                                    active_client_addr = Some(client_addr);
                                    input_injector.release_all_input();

                                    last_heartbeat = Instant::now();

                                    let (cancel_tx, cancel_rx) = broadcast::channel(1);
                                    active_cancel_tx = Some(cancel_tx);
                                    let (settings_tx, settings_rx) = watch::channel(crate::StreamSettings {
                                        width,
                                        height,
                                        fps,
                                        bitrate_kbps,
                                    });
                                    active_settings_tx = Some(settings_tx);
                                    let clipboard_inbound_rx = if env_flag_enabled("REMOTE_PLAY_CLIPBOARD_SYNC") {
                                        let (tx, rx) = mpsc::channel(1024);
                                        active_clipboard_tx = Some(tx);
                                        Some(rx)
                                    } else {
                                        None
                                    };
                                    let file_transfer_enabled = env_flag_enabled("REMOTE_PLAY_FILE_TRANSFER")
                                        || env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD")
                                        || std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").is_some();
                                    let file_inbound_rx = if file_transfer_enabled {
                                        let (tx, rx) = mpsc::channel(1024);
                                        active_file_tx = Some(tx);
                                        Some(rx)
                                    } else {
                                        None
                                    };
                                    #[cfg(target_os = "macos")]
                                    let (talkback_inbound_rx, talkback_settings_rx) = if env_flag_enabled("REMOTE_PLAY_TALKBACK") {
                                        let (tx, rx) = mpsc::channel(1024);
                                        let (settings_tx, settings_rx) = watch::channel(crate::talkback_player::TalkbackPlaybackSettings::default());
                                        active_talkback_tx = Some(tx);
                                        active_talkback_settings_tx = Some(settings_tx);
                                        (Some(rx), Some(settings_rx))
                                    } else {
                                        (None, None)
                                    };
                                    #[cfg(not(target_os = "macos"))]
                                    let (talkback_inbound_rx, talkback_settings_rx) = (None, None);
                                    let host_send_file = std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").map(PathBuf::from);
                                    let keyframe_requested = Arc::new(AtomicBool::new(true));
                                    active_keyframe_requested = Some(keyframe_requested.clone());

                                    let sender_clone = udp_sender.clone();
                                    let stats_clone = stats.clone();

                                    tokio::spawn(async move {
                                        if let Err(e) = run_streaming(StreamingRunConfig {
                                            session_id,
                                            client_addr,
                                            width,
                                            height,
                                            fps,
                                            bitrate_kbps,
                                            udp_sender: sender_clone,
                                            stats: stats_clone,
                                            cancel_rx,
                                            clipboard_inbound_rx,
                                            file_inbound_rx,
                                            talkback_inbound_rx,
                                            talkback_settings_rx,
                                            stream_settings_rx: Some(settings_rx),
                                            host_send_file,
                                            keyframe_requested,
                                        }).await {
                                            eprintln!("Streaming task error: {}", e);
                                        }
                                        println!("Streaming task stopped.");
                                    });
                                }
                                _ => {}
                            }
                        }
                        Ok(
                            MultiplexedPacket::Data(envelope, addr)
                            | MultiplexedPacket::DataWithTiming(envelope, _, addr),
                        ) => {
                            if !is_active_client(active_client_addr, addr) {
                                continue;
                            }
                            match envelope.header.kind {
                                ContentKind::ClipboardBundle => {
                                    if let Some(tx) = &active_clipboard_tx
                                        && let Err(err) = tx.try_send(envelope)
                                    {
                                        eprintln!("Clipboard inbound queue rejected packet: {}", err);
                                    }
                                }
                                ContentKind::FileManifest
                                | ContentKind::FileChunk
                                | ContentKind::FileControl => {
                                    if let Some(tx) = &active_file_tx
                                        && let Err(err) = tx.try_send(envelope)
                                    {
                                        eprintln!("File transfer inbound queue rejected packet: {}", err);
                                    }
                                }
                                ContentKind::AudioStreamConfig | ContentKind::AudioOpus => {
                                    if let Some(tx) = &active_talkback_tx
                                        && let Err(err) = tx.try_send(envelope)
                                    {
                                        eprintln!("Talkback inbound queue rejected packet: {}", err);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("UDP Receive Error: {}", e);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_active_client;

    #[test]
    fn session_packets_are_accepted_only_from_the_active_peer() {
        let active = "127.0.0.1:41000".parse().unwrap();
        let other = "127.0.0.1:41001".parse().unwrap();

        assert!(is_active_client(Some(active), active));
        assert!(!is_active_client(Some(active), other));
        assert!(!is_active_client(None, active));
    }
}
