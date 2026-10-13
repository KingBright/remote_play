//! Private same-user process transport. The framed application protocol and
//! its generation/lease semantics are shared; this layer grants no media rights.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;
#[cfg(unix)]
mod unix {
    use std::{
        fs, io,
        os::unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::UnixStream,
        },
        path::Path,
        time::Duration,
    };
    pub type Client = UnixStream;
    pub struct Listener {
        inner: tokio::net::UnixListener,
    }
    pub fn private_directory(path: &Path) -> io::Result<()> {
        let meta = path.symlink_metadata()?;
        if !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Profile must be a private, same-user directory",
            ));
        }
        Ok(())
    }
    pub fn private_file(path: &Path) -> io::Result<()> {
        let meta = path.symlink_metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.nlink() != 1
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Process marker must be a private, same-user regular file",
            ));
        }
        Ok(())
    }
    pub fn private_socket(path: &Path) -> io::Result<()> {
        let meta = path.symlink_metadata()?;
        if !meta.file_type().is_socket()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Endpoint must be a private, same-user socket",
            ));
        }
        Ok(())
    }
    pub fn exists(directory: &Path, name: &str) -> io::Result<bool> {
        match directory.symlink_metadata() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
            _ => private_directory(directory)?,
        }
        match directory.join(name).symlink_metadata() {
            Ok(_) => {
                private_socket(&directory.join(name))?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
    pub fn absent(directory: &Path, name: &str) -> io::Result<bool> {
        if !exists(directory, name)? {
            return Ok(true);
        }
        match UnixStream::connect(directory.join(name)) {
            Ok(_) => Ok(false),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => Ok(true),
            Err(e) => Err(e),
        }
    }
    pub fn connect(directory: &Path, name: &str, timeout: Duration) -> io::Result<Client> {
        private_directory(directory)?;
        let path = directory.join(name);
        private_socket(&path)?;
        let socket = UnixStream::connect(&path)?;
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        private_socket(&path)?;
        Ok(socket)
    }
    impl Listener {
        pub fn bind(directory: &Path, name: &str) -> io::Result<Self> {
            private_directory(directory)?;
            let path = directory.join(name);
            if exists(directory, name)? {
                if !absent(directory, name)? {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "A live process owns this private endpoint",
                    ));
                }
                fs::remove_file(&path)?;
            }
            let inner = tokio::net::UnixListener::bind(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            Ok(Self { inner })
        }
        pub async fn accept(&mut self) -> io::Result<tokio::net::UnixStream> {
            loop {
                let (socket, _) = self.inner.accept().await?;
                if socket.peer_cred().map(|c| c.uid()).ok() == Some(unsafe { libc::geteuid() }) {
                    return Ok(socket);
                }
            }
        }
    }
    pub fn remove_endpoint(path: &Path) {
        let _ = fs::remove_file(path);
    }
}
#[cfg(unix)]
pub(crate) use unix::*;

pub(crate) fn gui_directory(profile: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    private_directory(profile)?;
    let frontend = profile.join("gui-state");
    let attached = exists(&frontend, "gui-receive.sock")?;
    let standalone = exists(profile, "gui-receive.sock")?;
    match (attached, standalone) {
        (true, false) => Ok(frontend),
        (false, true) => Ok(profile.to_owned()),
        _ => Err(std::io::Error::other(
            "Expected exactly one running GUI receive adapter",
        )),
    }
}
