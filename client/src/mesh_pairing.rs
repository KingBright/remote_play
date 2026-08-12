use remote_core::mesh::{AppPrivateMeshConfigStore, MeshConfig};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshPairingMessageKind {
    Neutral,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPairingSnapshot {
    pub network_name: String,
    pub device_id: String,
    pub display_name: String,
    pub invite_code: String,
    pub config_dir: PathBuf,
    pub message: String,
    pub message_kind: MeshPairingMessageKind,
    pub restart_required: bool,
}

#[derive(Clone)]
pub struct MeshPairingControl {
    display_name: String,
    config_dir: PathBuf,
    mesh_reload_tx: Option<mpsc::UnboundedSender<()>>,
    snapshot: Arc<Mutex<MeshPairingSnapshot>>,
}

impl MeshPairingControl {
    pub fn load_or_create(
        config_dir: impl Into<PathBuf>,
        display_name: impl Into<String>,
    ) -> Result<Self, String> {
        let config_dir = config_dir.into();
        let display_name = display_name.into();
        let store = AppPrivateMeshConfigStore::new(&config_dir);
        let config = store
            .load_or_generate(display_name.clone())
            .map_err(|err| format!("Mesh setup failed: {err}"))?;
        let snapshot = snapshot_from_config(
            &config,
            config_dir.clone(),
            "Device group is ready.",
            MeshPairingMessageKind::Neutral,
            false,
        );

        Ok(Self {
            display_name,
            config_dir,
            mesh_reload_tx: None,
            snapshot: Arc::new(Mutex::new(snapshot)),
        })
    }

    pub fn with_mesh_reload_tx(mut self, mesh_reload_tx: mpsc::UnboundedSender<()>) -> Self {
        self.mesh_reload_tx = Some(mesh_reload_tx);
        self
    }

    pub fn snapshot(&self) -> MeshPairingSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn create_new_group(&self) -> MeshPairingSnapshot {
        let config = MeshConfig::generate(&self.display_name);
        self.save_and_update(
            config,
            "Created a new device group. Share the new code with the other device.",
            MeshPairingMessageKind::Success,
            true,
        )
    }

    pub fn join_from_invite_code(&self, invite_code: &str) -> MeshPairingSnapshot {
        let invite_code = invite_code.trim();
        if invite_code.is_empty() {
            return self.update_message(
                "Clipboard does not contain a device-group code.",
                MeshPairingMessageKind::Warning,
                false,
            );
        }

        match MeshConfig::from_invite_code(invite_code, &self.display_name) {
            Ok(config) => self.save_and_update(
                config,
                "Joined the device group from clipboard.",
                MeshPairingMessageKind::Success,
                true,
            ),
            Err(err) => self.update_message(
                format!("Could not read that device-group code: {err}"),
                MeshPairingMessageKind::Error,
                false,
            ),
        }
    }

    pub fn request_runtime_reload(&self, message: impl Into<String>) -> MeshPairingSnapshot {
        let message = message.into();
        let Some(tx) = &self.mesh_reload_tx else {
            return self.update_message(
                format!("{message} Restart RemotePlay to use it."),
                MeshPairingMessageKind::Warning,
                true,
            );
        };

        match tx.send(()) {
            Ok(()) => self.update_message(
                format!("{message} Network services are refreshing."),
                MeshPairingMessageKind::Success,
                false,
            ),
            Err(_) => self.update_message(
                format!("{message} Restart RemotePlay to use it."),
                MeshPairingMessageKind::Warning,
                true,
            ),
        }
    }

    fn save_and_update(
        &self,
        config: MeshConfig,
        message: impl Into<String>,
        message_kind: MeshPairingMessageKind,
        restart_required: bool,
    ) -> MeshPairingSnapshot {
        let store = AppPrivateMeshConfigStore::new(&self.config_dir);
        match store.save(&config) {
            Ok(()) => {
                let (message, restart_required) =
                    self.request_mesh_reload_after_save(message.into(), restart_required);
                let snapshot = snapshot_from_config(
                    &config,
                    self.config_dir.clone(),
                    message,
                    message_kind,
                    restart_required,
                );
                *self.snapshot.lock().unwrap() = snapshot.clone();
                snapshot
            }
            Err(err) => self.update_message(
                format!("Could not save device-group config: {err}"),
                MeshPairingMessageKind::Error,
                false,
            ),
        }
    }

    fn request_mesh_reload_after_save(
        &self,
        message: String,
        restart_required: bool,
    ) -> (String, bool) {
        if !restart_required {
            return (message, false);
        }
        let Some(tx) = &self.mesh_reload_tx else {
            return (message, true);
        };
        match tx.send(()) {
            Ok(()) => (format!("{message} Network services are refreshing."), false),
            Err(_) => (message, true),
        }
    }

    fn update_message(
        &self,
        message: impl Into<String>,
        message_kind: MeshPairingMessageKind,
        restart_required: bool,
    ) -> MeshPairingSnapshot {
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.message = message.into();
        snapshot.message_kind = message_kind;
        snapshot.restart_required = restart_required;
        snapshot.clone()
    }
}

fn snapshot_from_config(
    config: &MeshConfig,
    config_dir: PathBuf,
    message: impl Into<String>,
    message_kind: MeshPairingMessageKind,
    restart_required: bool,
) -> MeshPairingSnapshot {
    MeshPairingSnapshot {
        network_name: config.network_name.clone(),
        device_id: config.node_id.clone(),
        display_name: config.display_name.clone(),
        invite_code: config.invite_code(),
        config_dir,
        message: message.into(),
        message_kind,
        restart_required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_core::mesh::MeshConfig;
    use std::fs;

    #[test]
    fn load_or_create_persists_invite_code() {
        let root = temp_dir("load");
        let control = MeshPairingControl::load_or_create(&root, "Desk").expect("pairing control");
        let first = control.snapshot();

        let reloaded = MeshPairingControl::load_or_create(&root, "Desk").expect("reload");
        assert_eq!(reloaded.snapshot().network_name, first.network_name);
        assert_eq!(reloaded.snapshot().invite_code, first.invite_code);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn join_from_invite_code_replaces_group_but_keeps_local_display_name() {
        let root = temp_dir("join");
        let control = MeshPairingControl::load_or_create(&root, "Viewer").expect("pairing control");
        let remote = MeshConfig::generate("Host");
        let joined = control.join_from_invite_code(&remote.invite_code());

        assert_eq!(joined.network_name, remote.network_name);
        assert_eq!(joined.display_name, "Viewer");
        assert_ne!(joined.device_id, remote.node_id);
        assert!(joined.restart_required);
        assert_eq!(joined.message_kind, MeshPairingMessageKind::Success);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_invite_code_updates_status_without_replacing_group() {
        let root = temp_dir("invalid");
        let control = MeshPairingControl::load_or_create(&root, "Viewer").expect("pairing control");
        let before = control.snapshot();
        let after = control.join_from_invite_code("not a code");

        assert_eq!(after.network_name, before.network_name);
        assert_eq!(after.message_kind, MeshPairingMessageKind::Error);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn successful_join_requests_mesh_reload_when_available() {
        let root = temp_dir("reload");
        let (reload_tx, mut reload_rx) = mpsc::unbounded_channel();
        let control = MeshPairingControl::load_or_create(&root, "Viewer")
            .expect("pairing control")
            .with_mesh_reload_tx(reload_tx);
        let remote = MeshConfig::generate("Host");
        let joined = control.join_from_invite_code(&remote.invite_code());

        assert!(!joined.restart_required);
        assert!(joined.message.contains("Network services are refreshing"));
        reload_rx.try_recv().expect("reload should be requested");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_runtime_reload_updates_status_and_uses_reload_channel() {
        let root = temp_dir("explicit-reload");
        let (reload_tx, mut reload_rx) = mpsc::unbounded_channel();
        let control = MeshPairingControl::load_or_create(&root, "Viewer")
            .expect("pairing control")
            .with_mesh_reload_tx(reload_tx);

        let snapshot = control.request_runtime_reload("Mesh setup installed.");

        assert!(!snapshot.restart_required);
        assert_eq!(snapshot.message_kind, MeshPairingMessageKind::Success);
        assert!(snapshot.message.contains("Network services are refreshing"));
        reload_rx.try_recv().expect("reload should be requested");

        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "remote-play-client-mesh-pairing-{name}-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).expect("create temp dir");
        root
    }
}
