use super::*;

pub(super) struct ConnectionServicesConfig {
    pub(super) clipboard_enabled: Arc<std::sync::atomic::AtomicBool>,
    pub(super) session_id: u32,
    pub(super) scheduled_sender: ScheduledDataSender,
    pub(super) cancel_rx: broadcast::Receiver<()>,
    pub(super) file_inbound_rx: Option<mpsc::Receiver<DataEnvelope>>,
    pub(super) talkback_inbound_rx: Option<mpsc::Receiver<DataEnvelope>>,
    #[cfg(target_os = "macos")]
    pub(super) talkback_settings_rx:
        Option<watch::Receiver<crate::talkback_player::TalkbackPlaybackSettings>>,
    #[cfg(not(target_os = "macos"))]
    pub(super) talkback_settings_rx: Option<watch::Receiver<()>>,
    pub(super) host_send_file: Option<PathBuf>,
}

pub(super) async fn run(config: ConnectionServicesConfig) {
    let ConnectionServicesConfig {
        clipboard_enabled,
        session_id,
        scheduled_sender,
        mut cancel_rx,
        file_inbound_rx,
        talkback_inbound_rx,
        talkback_settings_rx,
        host_send_file,
    } = config;
    #[cfg(not(target_os = "macos"))]
    let _ = (session_id, talkback_inbound_rx, talkback_settings_rx);
    let scheduled_media_sender = Some(scheduled_sender);
    let (_file_command_tx_guard, _file_transfer_task, _file_event_logger) =
        if let (Some(inbound_rx), Some(sender)) = (file_inbound_rx, scheduled_media_sender.clone())
        {
            let (command_tx, command_rx) = mpsc::channel(16);
            let (event_tx, mut event_rx) = mpsc::unbounded_channel();
            let (forward_tx, forward_rx) = mpsc::unbounded_channel();
            #[cfg(target_os = "macos")]
            let file_clipboard_provider = MacClipboardProvider::new();
            #[cfg(target_os = "windows")]
            let file_clipboard_provider = WindowsClipboardProvider::new();
            #[cfg(target_os = "linux")]
            let file_clipboard_provider = LinuxClipboardProvider::new();
            tokio::spawn(
                remote_core::clipboard_file_runtime::run_clipboard_file_sync(
                    file_clipboard_provider,
                    command_tx.clone(),
                    event_rx,
                    Some(forward_tx),
                    cancel_rx.resubscribe(),
                    remote_core::clipboard_file_runtime::ClipboardFileSyncConfig {
                        enabled: Some(clipboard_enabled),
                        ..Default::default()
                    },
                ),
            );
            event_rx = forward_rx;
            let _file_clipboard_enabled = env_flag_enabled("REMOTE_PLAY_FILE_CLIPBOARD");
            let receive_dir = env_path_or_temp(
                "REMOTE_PLAY_FILE_RECEIVE_DIR",
                "remote-play-host-received-files",
            );
            let runtime_cancel_rx = cancel_rx.resubscribe();
            let runtime_task = tokio::spawn(async move {
                if let Err(err) = run_file_transfer_runtime(
                    sender,
                    command_rx,
                    inbound_rx,
                    event_tx,
                    runtime_cancel_rx,
                    FileTransferRuntimeConfig {
                        receive_dir,
                        ..FileTransferRuntimeConfig::default()
                    },
                )
                .await
                {
                    eprintln!("File transfer task error: {}", err);
                }
            });
            let logger_cancel_rx = cancel_rx.resubscribe();
            let event_logger = tokio::spawn(async move {
                let mut cancel_rx = logger_cancel_rx;
                loop {
                    tokio::select! {
                        _ = cancel_rx.recv() => break,
                        event = event_rx.recv() => {
                            let Some(event) = event else { break };
                            log_file_transfer_event("Host", event);
                        }
                    }
                }
            });
            if let Some(send_file) = host_send_file {
                let auto_send_tx = command_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    println!(
                        "Host auto-enqueueing send file on session startup: {}",
                        send_file.display()
                    );
                    if let Err(err) = auto_send_tx
                        .send(FileTransferCommand::SendFile {
                            path: send_file,
                            mime_type: None,
                        })
                        .await
                    {
                        eprintln!("Failed to enqueue initial host send file: {}", err);
                    }
                });
            }
            let command_tx_guard = Some(command_tx);
            (command_tx_guard, Some(runtime_task), Some(event_logger))
        } else {
            (None, None, None)
        };

    #[cfg(target_os = "macos")]
    let _talkback_player = if let (Some(inbound_rx), Some(settings_rx)) =
        (talkback_inbound_rx, talkback_settings_rx)
    {
        match crate::talkback_player::TalkbackPlayer::new(session_id, inbound_rx, settings_rx) {
            Ok(player) => Some(player),
            Err(err) => {
                eprintln!("Failed to initialize viewer talkback playback: {}", err);
                None
            }
        }
    } else {
        None
    };

    let _ = cancel_rx.recv().await;
}

pub(super) fn start_clipboard(
    sender: ScheduledDataSender,
) -> (mpsc::Sender<DataEnvelope>, broadcast::Sender<()>) {
    let (inbound_tx, inbound_rx) = mpsc::channel(256);
    let (cancel, cancel_rx) = broadcast::channel(2);
    tokio::spawn(async move {
        #[cfg(target_os = "macos")]
        let provider = MacClipboardProvider::new();
        #[cfg(target_os = "linux")]
        let provider = LinuxClipboardProvider::new();
        #[cfg(target_os = "windows")]
        let provider = WindowsClipboardProvider::new();
        if let Err(error) = run_clipboard_sync(
            provider,
            sender,
            inbound_rx,
            cancel_rx,
            ClipboardSyncRunnerConfig::default(),
        )
        .await
        {
            eprintln!("Clipboard: {error}");
        }
    });
    (inbound_tx, cancel)
}
