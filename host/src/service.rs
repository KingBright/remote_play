use crate::{StreamingRunConfig, env_flag_enabled, run_streaming};
use protocol::{AudioControlTarget, ContentKind, ControlMessage, DataEnvelope};
use remote_core::net::UdpMultiplexer;
use remote_core::stats::Statistics;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
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

    #[cfg(not(target_os = "macos"))]
    {
        println!("Host implementation is currently macOS only.");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        use crate::input_injector::MacInputInjector;
        use remote_core::net::MultiplexedPacket;

        let input_injector = Arc::new(MacInputInjector::new()?);
        let mut active_cancel_tx: Option<broadcast::Sender<()>> = None;
        let mut active_clipboard_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        let mut active_file_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        let mut active_talkback_tx: Option<mpsc::Sender<DataEnvelope>> = None;
        let mut active_talkback_settings_tx: Option<
            watch::Sender<crate::talkback_player::TalkbackPlaybackSettings>,
        > = None;
        let mut active_session_id: Option<u32> = None;
        let mut active_client_addr: Option<SocketAddr> = None;
        let mut last_heartbeat = Instant::now();

        println!("Listening for ControlMessages on {}...", bind_addr);

        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if active_cancel_tx.is_some() && last_heartbeat.elapsed() > Duration::from_secs(3) {
                        println!("Heartbeat timeout! Stopping stream...");
                        if let Some(tx) = active_cancel_tx.take() {
                            let _ = tx.send(());
                        }
                        active_clipboard_tx = None;
                        active_file_tx = None;
                        active_talkback_tx = None;
                        active_talkback_settings_tx = None;
                        active_session_id = None;
                        active_client_addr = None;
                        input_injector.release_all_input();
                    }
                }
                recv_res = udp_receiver.recv() => {
                    match recv_res {
                        Ok(MultiplexedPacket::Control(msg, client_addr)) => {
                            match msg {
                                ControlMessage::Input(input_event) => {
                                    if is_active_client(active_client_addr, client_addr) {
                                        input_injector.inject(input_event);
                                    }
                                }
                                ControlMessage::Heartbeat => {
                                    if is_active_client(active_client_addr, client_addr) {
                                        last_heartbeat = Instant::now();
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
                                    active_session_id = None;
                                    active_client_addr = None;
                                    input_injector.release_all_input();
                                }
                                ControlMessage::AudioControl { session_id, target, muted, volume_percent } => {
                                    if is_active_client(active_client_addr, client_addr)
                                        && active_session_id == Some(session_id)
                                        && target == AudioControlTarget::ViewerTalkbackPlayback
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
                                ControlMessage::StartStream { width, height, fps, bitrate_kbps, session_id } => {
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
                                    let (talkback_inbound_rx, talkback_settings_rx) = if env_flag_enabled("REMOTE_PLAY_TALKBACK") {
                                        let (tx, rx) = mpsc::channel(1024);
                                        let (settings_tx, settings_rx) = watch::channel(crate::talkback_player::TalkbackPlaybackSettings::default());
                                        active_talkback_tx = Some(tx);
                                        active_talkback_settings_tx = Some(settings_tx);
                                        (Some(rx), Some(settings_rx))
                                    } else {
                                        (None, None)
                                    };
                                    let host_send_file = std::env::var_os("REMOTE_PLAY_HOST_SEND_FILE").map(PathBuf::from);

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
                                            host_send_file,
                                        }).await {
                                            eprintln!("Streaming task error: {}", e);
                                        }
                                        println!("Streaming task stopped.");
                                    });
                                }
                                _ => {}
                            }
                        }
                        Ok(MultiplexedPacket::Data(envelope, addr)) => {
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
