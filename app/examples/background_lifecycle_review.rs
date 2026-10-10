//! Disposable lifecycle executable using the exact production entrypoint.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::args().nth(1).as_deref() == Some("--check-loopback-host") {
        let endpoint: std::net::SocketAddr = std::env::args().nth(2).ok_or("Missing test endpoint")?.parse()?;
        if !endpoint.ip().is_loopback() { return Err("Review connections must stay on loopback".into()); }
        remote_core::init_crypto_provider();
        let config = remote_play_app::UnifiedRuntimeConfig::from_env()?;
        let identity = remote_core::mesh::AppPrivateMeshConfigStore::new(config.mesh_dir).load()?.ok_or("Missing fixture identity")?;
        remote_core::session_crypto::use_paired_session_secret(identity.network_secret.expose_secret());
        let (connection, _events) = tokio::time::timeout(std::time::Duration::from_secs(10),
            remote_core::workspace_session::WorkspaceConnection::connect(endpoint, std::env::current_dir()?)).await??;
        println!("{{\"authenticated_loopback_connection\":true,\"connection_id\":{}}}", connection.id);
        return Ok(());
    }
    remote_play_app::entrypoint::run().await
}
