//! Per-profile single instance and local-only window activation, shared by all desktops.
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    net::{SocketAddr, UdpSocket},
    path::{Path, PathBuf},
};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RendererIdentity {
    schema: u32,
    renderer: String,
}

pub struct Instance {
    pub visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _lock: std::sync::Arc<File>,
    socket: Option<UdpSocket>,
    info: PathBuf,
    renderer_info: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
    frontend: bool,
}
impl Instance {
    pub fn acquire(directory: &Path) -> Result<Option<Self>, String> {
        Self::acquire_for_renderer(directory, crate::gui_backend::ORIGINAL_RENDERER)
    }
    pub(crate) fn acquire_for_renderer(
        directory: &Path,
        renderer: &str,
    ) -> Result<Option<Self>, String> {
        if renderer != crate::gui_backend::ORIGINAL_RENDERER {
            return Err("Unknown GUI renderer identity".into());
        }
        Self::acquire_mode(directory, Some(renderer))
    }
    /// Window-only ownership is separate from the service's network/profile lock.
    /// The caller must authenticate a running background service before using it.
    pub(crate) fn acquire_frontend(directory: &Path) -> Result<Option<Self>, String> {
        let directory = directory.join("gui-state");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            if let Err(error) = builder.create(&directory) {
                if error.kind() != io::ErrorKind::AlreadyExists { return Err(error.to_string()); }
            }
            let meta = directory.symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
                return Err("Frontend process-state directory is unsafe".into());
            }
        }
        #[cfg(windows)] {std::fs::create_dir_all(&directory).map_err(|e|e.to_string())?;crate::private_ipc::private_directory(&directory).map_err(|e|e.to_string())?;}
        Ok(Self::acquire_for_renderer(&directory, crate::gui_backend::ORIGINAL_RENDERER)?
            .map(|mut instance| {
                instance.frontend = true;
                instance
            }))
    }
    pub fn acquire_headless(directory: &Path) -> Result<Option<Self>, String> {
        Self::acquire_mode(directory, None)
    }
    fn acquire_mode(directory: &Path, renderer: Option<&str>) -> Result<Option<Self>, String> {
        let graphical = renderer.is_some();
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        #[cfg(windows)] crate::private_ipc::private_directory(directory).map_err(|e|e.to_string())?;
        let lock_path = directory.join("desktop-instance.lock");
        let info = directory.join("desktop-instance.addr");
        let renderer_info = directory.join("desktop-instance.renderer.json");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)] for path in [&lock_path,&info,&renderer_info] {
            match path.symlink_metadata(){Ok(_)=>crate::private_ipc::private_file(path).map_err(|e|e.to_string())?,Err(e)if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.to_string())}
        }
        let file = options.open(lock_path).map_err(|e| e.to_string())?;
        if file.try_lock().is_err() {
            let text = std::fs::read_to_string(&info)
                .map_err(|_| "Weavelo is already starting; try opening it again")?;
            if !graphical {
                return Ok(None);
            }
            if text.trim() == "headless" {
                return Err("Weavelo is already running without a GUI. Switch the existing user service to graphical mode instead of starting a second network instance.".into());
            }
            // An installed alpha.7 instance can own this same profile while a
            // newly built original UI starts. Never report that as success by
            // simply waking the wrong renderer via the old "show" protocol.
            let running = std::fs::read(&renderer_info)
                .ok()
                .filter(|b| b.len() <= 1024)
                .and_then(|bytes| serde_json::from_slice::<RendererIdentity>(&bytes).ok());
            let matches = running
                .as_ref()
                .is_some_and(|r| r.schema == 1 && Some(r.renderer.as_str()) == renderer);
            if !matches {
                let name = running
                    .map(|r| r.renderer)
                    .unwrap_or_else(|| "older/unidentified UI".into());
                return Err(format!(
                    "Another Weavelo interface ({name}) already owns this device profile. The requested {} was NOT opened. Close or upgrade the existing GUI through its launcher; no second network instance was started.",
                    renderer.unwrap()
                ));
            }
            let endpoint: SocketAddr = text
                .trim()
                .parse()
                .map_err(|_| "Invalid local activation address")?;
            if !endpoint.ip().is_loopback() {
                return Err("Refusing a nonlocal activation address".into());
            }
            UdpSocket::bind("127.0.0.1:0")
                .and_then(|s| s.send_to(b"show", endpoint))
                .map_err(|e| e.to_string())?;
            return Ok(None);
        }
        let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut metadata = options
            .clone()
            .truncate(true)
            .open(&info)
            .map_err(|e| e.to_string())?;
        if graphical {
            writeln!(
                metadata,
                "{}",
                socket.local_addr().map_err(|e| e.to_string())?
            )
        } else {
            writeln!(metadata, "headless")
        }
        .map_err(|e| e.to_string())?;
        let identity = RendererIdentity {
            schema: 1,
            renderer: renderer.unwrap_or("headless").to_owned(),
        };
        let mut identity_file = options
            .clone()
            .truncate(true)
            .open(&renderer_info)
            .map_err(|e| e.to_string())?;
        identity_file
            .write_all(&serde_json::to_vec(&identity).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(Some(Self {
            visible: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            _lock: std::sync::Arc::new(file),
            socket: Some(socket),
            info,
            renderer_info,
            task: None,
            frontend: false,
        }))
    }
    /// A verified standalone service may only activate its existing window.
    /// If its owner vanished, do not acquire the profile or start a runtime.
    pub(crate) fn activate_existing(directory: &Path) -> Result<(), String> {
        crate::private_ipc::private_directory(directory).map_err(|e|e.to_string())?;
        let info=directory.join("desktop-instance.addr");
        let renderer=directory.join("desktop-instance.renderer.json");
        crate::private_ipc::private_file(&info).map_err(|e|e.to_string())?;
        crate::private_ipc::private_file(&renderer).map_err(|e|e.to_string())?;
        let identity:RendererIdentity=serde_json::from_slice(&std::fs::read(renderer).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if identity.schema!=1||identity.renderer!=crate::gui_backend::ORIGINAL_RENDERER{return Err("Existing owner has no compatible GUI".into());}
        let endpoint:SocketAddr=std::fs::read_to_string(info).map_err(|e|e.to_string())?.trim().parse().map_err(|_|"Existing GUI activation address is unavailable")?;
        if !endpoint.ip().is_loopback(){return Err("Refusing a nonlocal activation address".into());}
        UdpSocket::bind("127.0.0.1:0").and_then(|s|s.send_to(b"show",endpoint)).map_err(|e|e.to_string())?;
        Ok(())
    }
    pub(crate) fn service_lock(&self)->Result<std::sync::Arc<File>,String>{
        if self.is_frontend(){return Err("A frontend cannot own another background runtime".into());}
        Ok(self._lock.clone())
    }
    pub(crate) fn state_directory(&self) -> &Path { self.info.parent().expect("instance state directory") }

    pub(crate) fn is_frontend(&self) -> bool { self.frontend }

    /// Same lock/activation protocol for every renderer. The callback only signals
    /// its own GUI; it must not start another network or capture runtime.
    pub(crate) fn attach_callback(&mut self, wake: impl Fn() + Send + 'static) -> io::Result<()> {
        let Some(socket) = self.socket.take() else {
            return Ok(());
        };
        let socket = tokio::net::UdpSocket::from_std(socket)?;
        let visible = self.visible.clone();
        self.task = Some(tokio::spawn(async move {
            let mut bytes = [0u8; 8];
            while let Ok((size, addr)) = socket.recv_from(&mut bytes).await {
                if addr.ip().is_loopback() && &bytes[..size] == b"show" {
                    visible.store(true, std::sync::atomic::Ordering::Release);
                    wake();
                }
            }
        }));
        Ok(())
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.info);
        let _ = std::fs::remove_file(&self.renderer_info);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_and_reopening_frontend_preserves_same_background_profile_lock() {
        let profile = tempfile::tempdir().unwrap();
        let host = Instance::acquire_headless(profile.path()).unwrap().unwrap();
        let frontend = Instance::acquire_frontend(profile.path()).unwrap().unwrap();
        assert!(frontend.frontend);
        drop(frontend);
        assert!(Instance::acquire_headless(profile.path()).unwrap().is_none());
        let reopened = Instance::acquire_frontend(profile.path()).unwrap().unwrap();
        drop(reopened);
        assert!(Instance::acquire_headless(profile.path()).unwrap().is_none());
        drop(host);
        assert!(Instance::acquire_headless(profile.path()).unwrap().is_some());
        assert!(!profile.path().join("mesh.conf").exists());
        assert!(!profile.path().join("mesh.secret").exists());
    }

    #[test]
    fn profile_named_gui_state_still_owns_runtime_and_its_actual_lock() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("gui-state");
        std::fs::create_dir(&profile).unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owner = Instance::acquire(&profile).unwrap().unwrap();
        assert!(!owner.is_frontend());
        let service_lock = owner.service_lock().unwrap();
        assert!(Instance::acquire(&profile).unwrap().is_none());
        let frontend = Instance::acquire_frontend(&profile).unwrap().unwrap();
        assert!(frontend.is_frontend());
        assert_eq!(frontend.state_directory(), profile.join("gui-state"));
        assert!(frontend.service_lock().is_err());
        assert!(Instance::acquire_frontend(&profile).unwrap().is_none());

        // Service ownership keeps the original lock alive after its window
        // instance drops; the separate frontend lock cannot replace it.
        let contender = OpenOptions::new().read(true).write(true)
            .open(profile.join("desktop-instance.lock")).unwrap();
        drop(owner);
        assert!(contender.try_lock().is_err());
        drop(service_lock);
        assert!(contender.try_lock().is_ok());
    }
    #[test]
    fn headless_runtime_also_owns_the_profile_and_rejects_a_second_gui() {
        let dir = tempfile::tempdir().unwrap();
        let first = Instance::acquire_headless(dir.path()).unwrap().unwrap();
        assert!(Instance::acquire_headless(dir.path()).unwrap().is_none());
        assert!(Instance::acquire(dir.path()).is_err());
        drop(first);
        assert!(Instance::acquire(dir.path()).unwrap().is_some());
    }
    #[test]
    fn headless_start_never_duplicates_a_graphical_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let _first = Instance::acquire(dir.path()).unwrap().unwrap();
        assert!(Instance::acquire_headless(dir.path()).unwrap().is_none());
    }
    #[test]
    fn duplicate_launch_only_activates_existing_instance() {
        let dir = tempfile::tempdir().unwrap();
        let first = Instance::acquire(dir.path()).unwrap().unwrap();
        assert!(Instance::acquire(dir.path()).unwrap().is_none());
        let mut bytes = [0u8; 8];
        assert_eq!(test_receive_activation(first.socket.as_ref().unwrap(), &mut bytes), 4);
        drop(first);
        assert!(Instance::acquire(dir.path()).unwrap().is_some());
    }
}

#[cfg(test)]
mod renderer_identity_tests {
    use super::*;
    use crate::gui_backend::ORIGINAL_RENDERER;
    #[test]
    fn current_gpui_launch_never_activates_a_legacy_diagnostic_owner() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = Instance::acquire_mode(dir.path(), Some("egui-diagnostic"))
            .unwrap()
            .unwrap();
        let error = Instance::acquire_for_renderer(dir.path(), ORIGINAL_RENDERER)
            .err()
            .unwrap();
        assert!(error.contains("NOT opened"));
        assert!(error.contains("egui-diagnostic"));
        assert_eq!(
            legacy
                .socket
                .as_ref()
                .unwrap()
                .recv(&mut [0u8; 8])
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn unidentified_old_instance_is_not_mistaken_for_original() {
        let dir = tempfile::tempdir().unwrap();
        let first = Instance::acquire(dir.path()).unwrap().unwrap();
        std::fs::remove_file(&first.renderer_info).unwrap();
        assert!(
            Instance::acquire_for_renderer(dir.path(), ORIGINAL_RENDERER)
                .err()
                .unwrap()
                .contains("older/unidentified")
        );
    }
    #[test]
    fn same_original_interface_only_activates_existing_window() {
        let dir = tempfile::tempdir().unwrap();
        let first = Instance::acquire_for_renderer(dir.path(), ORIGINAL_RENDERER)
            .unwrap()
            .unwrap();
        assert!(
            Instance::acquire_for_renderer(dir.path(), ORIGINAL_RENDERER)
                .unwrap()
                .is_none()
        );
        let mut bytes = [0u8; 8];
        assert_eq!(test_receive_activation(first.socket.as_ref().unwrap(), &mut bytes), 4);
        assert_eq!(&bytes[..4], b"show");
        drop(first);
        assert!(!dir.path().join("desktop-instance.renderer.json").exists());
        assert!(
            Instance::acquire_for_renderer(dir.path(), ORIGINAL_RENDERER)
                .unwrap()
                .is_some()
        );
    }
}

#[cfg(test)]
fn test_receive_activation(socket:&UdpSocket,bytes:&mut [u8])->usize {
    // Sending a local UDP packet does not promise it is readable in the same
    // instruction slice. Wait for this exact test socket, with a fixed deadline.
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(std::time::Duration::from_secs(1))).unwrap();
    socket.recv(bytes).expect("same-renderer activation must reach the existing owner")
}
