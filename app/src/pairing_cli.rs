//! Native pairing commands. The existing owner validates identity and idle state.
//! Invitations cross only a separately inherited anonymous pipe; never argv,
//! environment variables, ordinary stdout/stderr, files, or a second identity store.
use client::{MeshPairingBackend, MeshPairingMessageKind, MeshPairingSnapshot};
use remote_core::mesh::{MeshSecret, public_metadata_fingerprint};
use serde::Serialize;
use std::{
    fmt,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::mpsc,
    time::Duration,
};

const LIMIT: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Status,
    Create,
    Export(usize),
    Join,
    Rename { expected: String, name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingCliError {
    Arguments,
    ConfirmationRequired,
    UnsafePipe,
    PipeIo,
    PipeTimeout,
    InvalidInvitation,
    OwnerUnavailable,
    OperationUnconfirmed,
}
impl fmt::Display for PairingCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Arguments => "Invalid pairing command; invitation text is never accepted in arguments",
            Self::ConfirmationRequired => "Changing this device group requires --confirm-change",
            Self::UnsafePipe => "Invitation transport requires a separate inherited private pipe",
            Self::PipeIo => "Private invitation pipe failed; no invitation was printed",
            Self::PipeTimeout => "Private invitation pipe timed out; inspect current state before retrying",
            Self::InvalidInvitation => "Private invitation input is empty, oversized, or invalid text",
            Self::OwnerUnavailable => "Existing same-user owner is unavailable or its identity is incompatible",
            Self::OperationUnconfirmed => "Pairing change is not confirmed; inspect --pairing-status-json before another change",
        })
    }
}
impl std::error::Error for PairingCliError {}

/// Deliberately not the serializable MeshPairingSnapshot (which contains an invite).
#[derive(Debug, Serialize)]
pub(crate) struct Report {
    schema: u32,
    operation: &'static str,
    accepted: bool,
    network_name_sha256: String,
    device_id_sha256: String,
    display_name: String,
    restart_required: bool,
    last_message_kind: &'static str,
    runtime_registration_verified: bool,
    private_invitation_exported: bool,
}
fn report(snapshot: &MeshPairingSnapshot, operation: &'static str) -> Report {
    Report {
        schema: 1,
        operation,
        accepted: true,
        network_name_sha256: public_metadata_fingerprint(&snapshot.network_name),
        device_id_sha256: public_metadata_fingerprint(&snapshot.device_id),
        display_name: snapshot.display_name.clone(),
        restart_required: snapshot.restart_required,
        last_message_kind: match snapshot.message_kind {
            MeshPairingMessageKind::Neutral => "neutral",
            MeshPairingMessageKind::Success => "success",
            MeshPairingMessageKind::Warning => "warning",
            MeshPairingMessageKind::Error => "error",
        },
        // Persistence/reload admission does not prove live registration or discovery.
        runtime_registration_verified: false,
        private_invitation_exported: false,
    }
}
fn parse(args: &[String]) -> Result<Command, PairingCliError> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--pairing-status-json"] => Ok(Command::Status),
        ["--pairing-set-name", name, "--expected-name", expected, "--confirm-change"]
            if remote_core::device_name::valid_name(name).is_some() =>
            Ok(Command::Rename { expected: expected.to_string(), name: name.trim().to_string() }),
        ["--pairing-create-group", "--confirm-change"] => Ok(Command::Create),
        ["--pairing-join-stdin", "--confirm-change"] => Ok(Command::Join),
        ["--pairing-create-group"] | ["--pairing-join-stdin"] => {
            Err(PairingCliError::ConfirmationRequired)
        }
        ["--pairing-export-invite-pipe", descriptor] => descriptor
            .parse::<usize>()
            .map(Command::Export)
            .map_err(|_| PairingCliError::Arguments),
        _ => Err(PairingCliError::Arguments),
    }
}

fn read_invitation(input: impl Read) -> Result<MeshSecret, PairingCliError> {
    let mut bytes = Vec::new();
    input
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PairingCliError::PipeIo)?;
    if bytes.len() > LIMIT as usize || bytes.contains(&0) {
        return Err(PairingCliError::InvalidInvitation);
    }
    let text = String::from_utf8(bytes).map_err(|_| PairingCliError::InvalidInvitation)?;
    let text = text.trim();
    if text.is_empty() || text.contains(['\r', '\n']) {
        return Err(PairingCliError::InvalidInvitation);
    }
    MeshSecret::new(text).map_err(|_| PairingCliError::InvalidInvitation)
}
fn read_private_input(input: File) -> Result<MeshSecret, PairingCliError> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(read_invitation(input));
    });
    rx.recv_timeout(IO_TIMEOUT)
        .map_err(|_| PairingCliError::PipeTimeout)?
}
fn write_private_invitation(mut output: File, code: String) -> Result<(), PairingCliError> {
    if code.is_empty() || code.len() as u64 + 1 > LIMIT || code.contains(['\r', '\n', '\0']) {
        return Err(PairingCliError::InvalidInvitation);
    }
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            output.write_all(code.as_bytes())?;
            output.write_all(b"\n")?;
            output.flush()
        })()
        .map_err(|_: std::io::Error| PairingCliError::PipeIo);
        let _ = tx.send(result);
    });
    rx.recv_timeout(IO_TIMEOUT)
        .map_err(|_| PairingCliError::PipeTimeout)?
}
fn run_business(
    command: Command,
    backend: &dyn MeshPairingBackend,
    input: Option<File>,
    output: Option<File>,
) -> Result<Report, PairingCliError> {
    let snapshot = match &command {
        Command::Status | Command::Export(_) => backend.snapshot(),
        Command::Create => backend.create_new_group(),
        Command::Rename { expected, name } => backend.rename_device(expected, name),
        Command::Join => {
            let input = input.ok_or(PairingCliError::UnsafePipe)?;
            let invitation = read_private_input(input)?;
            backend.join_from_invite_code(invitation.expose_secret())
        }
    };
    if matches!(command, Command::Create | Command::Join | Command::Rename { .. })
        && snapshot.message_kind != MeshPairingMessageKind::Success
    {
        // Never return backend message text: an input/parser error may quote secrets.
        return Err(PairingCliError::OperationUnconfirmed);
    }
    let operation = match command {
        Command::Status => "status",
        Command::Create => "create-group",
        Command::Join => "join-group",
        Command::Rename { .. } => "rename-device",
        Command::Export(_) => "export-invitation",
    };
    let mut result = report(&snapshot, operation);
    if matches!(command, Command::Export(_)) {
        write_private_invitation(
            output.ok_or(PairingCliError::UnsafePipe)?,
            snapshot.invite_code,
        )?;
        result.private_invitation_exported = true;
    }
    Ok(result)
}

pub(crate) fn command(args: &[String], profile: &Path) -> Result<Report, PairingCliError> {
    let selected = parse(args)?;
    // Validate transport before connecting or attempting any mutation.
    let input = if selected == Command::Join {
        Some(pipe::input()?)
    } else {
        None
    };
    let output = if let Command::Export(handle) = selected {
        Some(pipe::output(handle)?)
    } else {
        None
    };
    // No local store mutation, fallback owner, daemon startup or force mode.
    let backend = crate::background_service::Client::connect(profile)
        .map_err(|_| PairingCliError::OwnerUnavailable)?;
    run_business(selected, &backend, input, output)
}

#[cfg(unix)]
mod pipe {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};
    fn duplicate(fd: i32, writing: bool) -> Result<File, PairingCliError> {
        let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd, info.as_mut_ptr()) } != 0 {
            return Err(PairingCliError::UnsafePipe);
        }
        let info = unsafe { info.assume_init() };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if info.st_mode & libc::S_IFMT != libc::S_IFIFO
            || info.st_uid != unsafe { libc::geteuid() }
            || info.st_nlink != 0
            || flags < 0
            || (writing && flags & libc::O_ACCMODE == libc::O_RDONLY)
            || (!writing && flags & libc::O_ACCMODE == libc::O_WRONLY)
        {
            return Err(PairingCliError::UnsafePipe);
        }
        if writing {
            for standard in 0..=2 {
                let mut other = std::mem::MaybeUninit::<libc::stat>::uninit();
                if unsafe { libc::fstat(standard, other.as_mut_ptr()) } == 0 {
                    let other = unsafe { other.assume_init() };
                    if (info.st_dev, info.st_ino) == (other.st_dev, other.st_ino) {
                        return Err(PairingCliError::UnsafePipe);
                    }
                }
            }
        }
        let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if dup < 0 {
            return Err(PairingCliError::UnsafePipe);
        }
        Ok(unsafe { File::from_raw_fd(dup) })
    }
    pub(super) fn input() -> Result<File, PairingCliError> {
        duplicate(std::io::stdin().as_raw_fd(), false)
    }
    pub(super) fn output(fd: usize) -> Result<File, PairingCliError> {
        if fd <= 2 || fd > i32::MAX as usize {
            return Err(PairingCliError::UnsafePipe);
        }
        duplicate(fd as i32, true)
    }
}

#[cfg(windows)]
mod pipe {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use windows_sys::Win32::{
        Foundation::{CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle},
        Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType},
        System::Threading::GetCurrentProcess,
    };
    fn duplicate(handle: RawHandle, writing: bool) -> Result<File, PairingCliError> {
        if handle.is_null() || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
            return Err(PairingCliError::UnsafePipe);
        }
        if writing {
            for standard in [
                std::io::stdin().as_raw_handle(),
                std::io::stdout().as_raw_handle(),
                std::io::stderr().as_raw_handle(),
            ] {
                if handle == standard || unsafe { CompareObjectHandles(handle, standard) } != 0 {
                    return Err(PairingCliError::UnsafePipe);
                }
            }
        }
        let current = unsafe { GetCurrentProcess() };
        let mut copied = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                current,
                handle,
                current,
                &mut copied,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(PairingCliError::UnsafePipe);
        }
        Ok(unsafe { File::from_raw_handle(copied) })
    }
    pub(super) fn input() -> Result<File, PairingCliError> {
        duplicate(std::io::stdin().as_raw_handle(), false)
    }
    pub(super) fn output(handle: usize) -> Result<File, PairingCliError> {
        duplicate(handle as RawHandle, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn arguments(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| x.to_string()).collect()
    }
    fn fixture() -> MeshPairingSnapshot {
        MeshPairingSnapshot {
            network_name: "synthetic-private-network".into(),
            device_id: "synthetic-device".into(),
            display_name: "fixture".into(),
            invite_code: "DO-NOT-PRINT-SYNTHETIC-INVITE".into(),
            config_dir: "/synthetic/profile".into(),
            message: "DO-NOT-PRINT-BACKEND-MESSAGE".into(),
            message_kind: MeshPairingMessageKind::Success,
            restart_required: false,
        }
    }
    struct Backend {
        calls: AtomicUsize,
        fail: bool,
    }
    impl MeshPairingBackend for Backend {
        fn snapshot(&self) -> MeshPairingSnapshot {
            fixture()
        }
        fn create_new_group(&self) -> MeshPairingSnapshot {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut s = fixture();
            if self.fail {
                s.message_kind = MeshPairingMessageKind::Error;
            }
            s
        }
        fn join_from_invite_code(&self, _: &str) -> MeshPairingSnapshot {
            self.create_new_group()
        }
        fn rename_device(&self, _: &str, _: &str) -> MeshPairingSnapshot { self.create_new_group() }
        fn request_runtime_reload(&self, _: String) -> MeshPairingSnapshot {
            panic!("No alternate reload path");
        }
    }
    #[test]
    fn mutation_requires_explicit_confirmation_and_never_takes_an_invitation_argument() {
        assert_eq!(
            parse(&arguments(&["--pairing-create-group"])),
            Err(PairingCliError::ConfirmationRequired)
        );
        assert_eq!(
            parse(&arguments(&["--pairing-join-stdin"])),
            Err(PairingCliError::ConfirmationRequired)
        );
        assert_eq!(
            parse(&arguments(&[
                "--pairing-join-stdin",
                "--confirm-change",
                "secret"
            ])),
            Err(PairingCliError::Arguments)
        );
        assert_eq!(
            parse(&arguments(&["--pairing-create-group", "--confirm-change"])),
            Ok(Command::Create)
        );
    }
    #[test]
    fn rename_requires_exact_arguments_confirmation_and_owner_business_service() {
        assert!(parse(&arguments(&["--pairing-set-name", "Studio", "--expected-name", "Weavelo"])).is_err());
        assert!(parse(&arguments(&["--pairing-set-name", "bad\nname", "--expected-name", "Weavelo", "--confirm-change"])).is_err());
        let selected = parse(&arguments(&["--pairing-set-name", "Studio", "--expected-name", "Weavelo", "--confirm-change"])).unwrap();
        let backend = Backend { calls: AtomicUsize::new(0), fail: false };
        assert_eq!(run_business(selected, &backend, None, None).unwrap().operation, "rename-device");
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn public_report_and_errors_never_serialize_invitation_or_backend_text() {
        let result = report(&fixture(), "status");
        let text = serde_json::to_string(&result).unwrap();
        assert!(!text.contains("DO-NOT-PRINT"));
        assert!(!text.contains("invite_code"));
        assert!(!text.contains("synthetic-private-network"));
        assert!(!result.runtime_registration_verified);
        for error in [
            PairingCliError::OwnerUnavailable,
            PairingCliError::OperationUnconfirmed,
        ] {
            assert!(!format!("{error:?} {error}").contains("DO-NOT-PRINT"));
        }
    }
    #[test]
    fn source_backend_rejection_is_not_reported_as_an_accepted_change() {
        let backend = Backend {
            calls: AtomicUsize::new(0),
            fail: true,
        };
        assert!(matches!(
            run_business(Command::Create, &backend, None, None),
            Err(PairingCliError::OperationUnconfirmed)
        ));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn invitation_reader_bounds_text_and_rejects_empty_nul_and_multiple_lines() {
        for bytes in [
            Vec::new(),
            vec![b'x'; LIMIT as usize + 1],
            b"x\0y".to_vec(),
            b"x\ny".to_vec(),
            vec![0xff],
        ] {
            assert!(read_invitation(bytes.as_slice()).is_err());
        }
        assert!(read_invitation(b"  synthetic-invitation\n".as_slice()).is_ok());
    }
    #[cfg(unix)]
    fn anonymous_pipe() -> (File, File) {
        use std::os::fd::FromRawFd;
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }
    #[cfg(unix)]
    #[test]
    fn private_export_rejects_standard_descriptors_regular_files_and_stdout_aliases() {
        use std::os::fd::AsRawFd;
        assert!(pipe::output(1).is_err());
        assert!(pipe::output(2).is_err());
        let ordinary = tempfile::tempfile().unwrap();
        assert!(pipe::output(ordinary.as_raw_fd() as usize).is_err());
        let alias = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
        if alias >= 0 {
            assert!(pipe::output(alias as usize).is_err());
            unsafe {
                libc::close(alias);
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn private_export_uses_a_separate_pipe_without_secret_in_public_report() {
        use std::os::fd::AsRawFd;
        let (mut input, output) = anonymous_pipe();
        let backend = Backend {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let handle = output.as_raw_fd() as usize;
        let result = run_business(
            Command::Export(handle),
            &backend,
            None,
            Some(pipe::output(handle).unwrap()),
        )
        .unwrap();
        drop(output);
        let mut received = String::new();
        input.read_to_string(&mut received).unwrap();
        assert!(
            received.trim() == fixture().invite_code,
            "Private export bytes differ"
        );
        assert!(result.private_invitation_exported);
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("DO-NOT-PRINT")
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }
    #[cfg(unix)]
    #[test]
    fn bounded_private_join_delegates_once_and_does_not_call_alternate_reload() {
        let (input, mut output) = anonymous_pipe();
        output.write_all(b"synthetic-invitation\n").unwrap();
        drop(output);
        let backend = Backend {
            calls: AtomicUsize::new(0),
            fail: false,
        };
        let result = run_business(Command::Join, &backend, Some(input), None).unwrap();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.operation, "join-group");
        assert!(!result.runtime_registration_verified);
    }
}
