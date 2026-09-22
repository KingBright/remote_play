//! Manual device acceptance host. Keep authentication enabled when testing across
//! networks: REMOTE_PLAY_SESSION_PSK or REMOTE_PLAY_TEST_GROUP_DIR selects the key.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(directory) = std::env::var_os("REMOTE_PLAY_TEST_GROUP_DIR") {
        let identity =
            remote_core::mesh::AppPrivateMeshConfigStore::new(std::path::PathBuf::from(directory))
                .load_or_generate("Acceptance host")?;
        remote_core::session_crypto::use_paired_session_secret(
            identity.network_secret.expose_secret(),
        );
        if let Some(path) = std::env::var_os("REMOTE_PLAY_TEST_INVITE_FILE") {
            std::fs::write(
                path,
                remote_core::encode_pairing_qr(&identity.invite_code(), 5154)?,
            )?;
        }
    }
    host::run_host_service(host::HostServiceConfig {
        bind_addr: std::env::var("REMOTE_PLAY_TEST_BIND")
            .unwrap_or_else(|_| "127.0.0.1:5154".into())
            .parse()?,
        stats: remote_core::Statistics::new(),
        enable_clipboard_sync: true,
        enable_file_transfer: true,
        enable_talkback: false,
    })
    .await
}
