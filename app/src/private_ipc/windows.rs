//! Local named pipes restricted to this token user and interactive session.
//! No impersonation, privilege enabling, ACL repair, TCP fallback or OS service.
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
    time::{Duration, Instant},
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{
        Pipes::*,
        RemoteDesktop::ProcessIdToSessionId,
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::*,
    },
};
fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Local(*mut core::ffi::c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
struct Identity {
    sid: String,
    session: u32,
}
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _owned = Local(text.cast());
    let mut len = 0;
    while unsafe { *text.add(len) } != 0 {
        len += 1;
        if len > 256 {
            return Err(denied("Invalid user SID"));
        }
    }
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(text, len)
    }))
}
fn identity(pid: u32) -> io::Result<Identity> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = Handle(process);
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
    }
    if bytes == 0 || bytes > 65536 {
        return Err(denied("Invalid process token"));
    }
    let mut storage = vec![
        0usize;
        (bytes as usize + std::mem::size_of::<usize>() - 1)
            / std::mem::size_of::<usize>()
    ];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let sid = sid_string(user.User.Sid)?;
    let mut session = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if session == 0 {
        return Err(denied("GUI IPC requires an interactive user session"));
    }
    Ok(Identity { sid, session })
}
fn current() -> io::Result<Identity> {
    identity(std::process::id())
}
fn peer(handle: HANDLE, server: bool) -> io::Result<()> {
    let mut pid = 0;
    let ok = unsafe {
        if server {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if identity(pid)? != current()? {
        return Err(denied("Private pipe peer is not this user/session"));
    }
    Ok(())
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn validate_acl(path: &Path) -> io::Result<()> {
    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let result = unsafe {
        GetNamedSecurityInfoW(
            wide(path).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _owned = Local(descriptor);
    let own = current()?.sid;
    if owner.is_null() || sid_string(owner)? != own || dacl.is_null() {
        return Err(denied(
            "Profile ownership or DACL is unsafe; nothing changed",
        ));
    }
    let mut info: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe {
        GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of_val(&info) as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    for index in 0..info.AceCount {
        let mut ace = std::ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType == ACCESS_DENIED_ACE_TYPE as u8 {
            continue;
        }
        if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 {
            return Err(denied("Unsupported profile ACL; nothing changed"));
        }
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = sid_string((&allowed.SidStart as *const u32).cast_mut().cast())?;
        if sid != own && sid != "S-1-5-18" && sid != "S-1-5-32-544" {
            return Err(denied(&format!(
                "Profile grants another principal access; nothing changed (path={}, sid={}, mask=0x{:08x}, ace_flags=0x{:02x}; principal name not established)",
                path.display(),
                sid,
                allowed.Mask,
                header.AceFlags,
            )));
        }
    }
    Ok(())
}
fn ordinary(path: &Path, directory: bool) -> io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || meta.is_dir() != directory
        || (!directory && !meta.is_file())
    {
        return Err(denied("Process state must be an ordinary file/directory"));
    }
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        if ancestor.symlink_metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(denied("Process-state path crosses a reparse point"));
        }
    }
    validate_acl(path)
}
pub fn private_directory(path: &Path) -> io::Result<()> {
    ordinary(path, true)
}
pub fn private_file(path: &Path) -> io::Result<()> {
    ordinary(path, false)
}
fn pipe_name(directory: &Path, name: &str) -> io::Result<String> {
    private_directory(directory)?;
    let identity = current()?;
    let profile = std::fs::canonicalize(directory)?;
    let mut digest = Sha256::new();
    digest.update(identity.sid.as_bytes());
    digest.update(identity.session.to_le_bytes());
    for unit in profile.as_os_str().encode_wide() {
        digest.update(unit.to_le_bytes());
    }
    digest.update([0]);
    digest.update(name.as_bytes());
    Ok(format!(r"\\.\pipe\Weavelo-{:x}", digest.finalize()))
}
fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sid = current()?.sid;
    let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let _owned = Local(descriptor);
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}
pub struct Listener {
    name: String,
    next: Option<NamedPipeServer>,
}
impl Listener {
    pub fn bind(directory: &Path, name: &str) -> io::Result<Self> {
        let name = pipe_name(directory, name)?;
        let next = Some(create(&name, true)?);
        Ok(Self { name, next })
    }
    pub async fn accept(&mut self) -> io::Result<NamedPipeServer> {
        loop {
            let connected = self
                .next
                .as_ref()
                .ok_or_else(|| io::Error::other("Private pipe listener ended"))?;
            connected.connect().await?;
            // Preserve namespace ownership before handing out/dropping this instance.
            let next = create(&self.name, false)?;
            let connected = self.next.replace(next).unwrap();
            if peer(connected.as_raw_handle(), true).is_ok() {
                return Ok(connected);
            }
        }
    }
}
pub fn exists(directory: &Path, name: &str) -> io::Result<bool> {
    match directory.symlink_metadata() {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
        _ => {}
    }
    let name = pipe_name(directory, name)?
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe { WaitNamedPipeW(name.as_ptr(), 1) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(2) => Ok(false),
        Some(121 | 231) => Ok(true),
        _ => Err(error),
    }
}
pub fn absent(directory: &Path, name: &str) -> io::Result<bool> {
    Ok(!exists(directory, name)?)
}
pub struct Client {
    file: File,
    deadline: Instant,
}
pub fn connect(directory: &Path, name: &str, timeout: Duration) -> io::Result<Client> {
    let name = pipe_name(directory, name)?;
    let deadline = Instant::now() + timeout;
    let file = loop {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(&name)
        {
            Ok(file) => break file,
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e),
        }
    };
    peer(file.as_raw_handle(), false)?; // Before any framed command or secret is written.
    let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
    if unsafe {
        SetNamedPipeHandleState(
            file.as_raw_handle(),
            &mode,
            std::ptr::null(),
            std::ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Client { file, deadline })
}
impl Client {
    fn wait(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Private pipe exchange timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
        Ok(())
    }
}
impl Read for Client {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.file.read(buf) {
                // std::fs::File normalizes an empty PIPE_NOWAIT read to Ok(0).
                // A connected empty pipe is not EOF; preserve the exchange deadline.
                Ok(0) if !buf.is_empty() => {
                    let mut available = 0;
                    if unsafe {
                        PeekNamedPipe(
                            self.file.as_raw_handle(),
                            std::ptr::null_mut(),
                            0,
                            std::ptr::null_mut(),
                            &mut available,
                            std::ptr::null_mut(),
                        )
                    } == 0
                    {
                        let error = io::Error::last_os_error();
                        if matches!(error.raw_os_error(), Some(109 | 232 | 233)) {
                            return Ok(0); // Broken/closing/disconnected pipe.
                        }
                        return Err(error);
                    }
                    self.wait()?;
                }
                Err(e) if e.raw_os_error() == Some(232) => self.wait()?,
                other => return other,
            }
        }
    }
}
impl Write for Client {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match self.file.write(buf) {
                Ok(0) if !buf.is_empty() => self.wait()?,
                Err(e) if e.raw_os_error() == Some(232) => self.wait()?,
                other => return other,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn remove_endpoint(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct PrivateTestDirectory {
        _root: tempfile::TempDir,
        path: std::path::PathBuf,
    }
    impl PrivateTestDirectory {
        fn path(&self) -> &Path {
            &self.path
        }
    }
    fn private_test_directory() -> PrivateTestDirectory {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private");
        let sid = current().unwrap().sid;
        let sddl = format!("O:{sid}D:P(A;OICI;FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _owned = Local(descriptor);
        let attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // This new fixture is private at creation. Existing ACLs stay intact.
        assert_ne!(unsafe { CreateDirectoryW(wide(&path).as_ptr(), &attrs) }, 0);
        private_directory(&path).unwrap();
        PrivateTestDirectory { _root: root, path }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn named_pipe_keeps_exclusive_name_and_authenticates_both_peers() {
        let directory = private_test_directory();
        let mut listener = Listener::bind(directory.path(), "test.sock").unwrap();
        assert!(Listener::bind(directory.path(), "test.sock").is_err());
        for _ in 0..2 {
            let path = directory.path().to_owned();
            let client = tokio::task::spawn_blocking(move || {
                let mut pipe = connect(&path, "test.sock", Duration::from_secs(2)).unwrap();
                std::io::Write::write_all(&mut pipe, b"ping").unwrap();
                let mut bytes = [0; 4];
                std::io::Read::read_exact(&mut pipe, &mut bytes).unwrap();
                assert_eq!(&bytes, b"pong");
            });
            let mut pipe = listener.accept().await.unwrap();
            assert!(Listener::bind(directory.path(), "test.sock").is_err());
            let mut bytes = [0; 4];
            pipe.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"ping");
            pipe.write_all(b"pong").await.unwrap();
            client.await.unwrap();
        }
        drop(listener);
        assert!(Listener::bind(directory.path(), "test.sock").is_ok());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn empty_read_waits_for_delayed_reply_and_peer_close_is_eof() {
        let directory = private_test_directory();
        let mut listener = Listener::bind(directory.path(), "delayed.sock").unwrap();
        let path = directory.path().to_owned();
        let (consumed, received) = tokio::sync::oneshot::channel();
        let client = tokio::task::spawn_blocking(move || {
            let mut pipe = connect(&path, "delayed.sock", Duration::from_secs(2)).unwrap();
            assert_eq!(std::io::Read::read(&mut pipe, &mut []).unwrap(), 0);
            let mut bytes = [0; 4];
            std::io::Read::read_exact(&mut pipe, &mut bytes).unwrap();
            assert_eq!(&bytes, b"pong");
            consumed.send(()).unwrap();
            assert_eq!(std::io::Read::read(&mut pipe, &mut [0]).unwrap(), 0);
        });
        let mut pipe = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        pipe.write_all(b"pong").await.unwrap();
        received.await.unwrap();
        drop(pipe);
        client.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn occupied_pipe_never_means_absent_and_silent_peer_is_bounded() {
        let directory = private_test_directory();
        let mut listener = Listener::bind(directory.path(), "silent.sock").unwrap();
        assert!(!absent(directory.path(), "silent.sock").unwrap());
        let path = directory.path().to_owned();
        let client = tokio::task::spawn_blocking(move || {
            let mut pipe = connect(&path, "silent.sock", Duration::from_millis(100)).unwrap();
            let error = std::io::Read::read_exact(&mut pipe, &mut [0]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        });
        let _pipe = listener.accept().await.unwrap();
        client.await.unwrap();
    }
}
