//! Verify the existing Octendra authority, not a second credential/grant store.
//! The optional private locator pins an existing desktop-launch.json. IPC clients
//! cannot select that authority file. No locator is created or enabled by this code.
use super::computer_use_ipc::{Actor, Call, Target};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
type Error = &'static str;
#[cfg(all(test, unix))]
#[path = "computer_use_authority_tests.rs"]
mod tests;

pub(crate) struct Authority {
    launch: Option<PathBuf>,
    directory: PathBuf,
    generation: String,
    gui_hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Process {
    pid: i32,
    parent: i32,
    started: [u64; 2],
    path: PathBuf,
    hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caller {
    worker: Process,
    agent: Process,
    authority_digest: String,
    pub(super) expires_at_ms: u64,
}
impl Caller {
    pub(super) fn owns_peer(&self, peer: Option<i32>) -> bool {
        peer.is_some_and(|pid| pid == self.worker.pid || pid == self.agent.pid)
    }
}
fn authority_digest(root: &[u8], worker: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update((root.len() as u64).to_be_bytes());
    hash.update(root);
    hash.update(worker);
    format!("{:x}", hash.finalize())
}
fn owned(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    #[cfg(unix)]
    {
        use std::{
            io::Read,
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let m = path
            .symlink_metadata()
            .map_err(|_| "authority_file_unavailable")?;
        let parent = path
            .parent()
            .ok_or("authority_file_not_private")?
            .symlink_metadata()
            .map_err(|_| "authority_file_not_private")?;
        if !parent.is_dir()
            || parent.file_type().is_symlink()
            || parent.uid() != unsafe { libc::geteuid() }
            || parent.mode() & 0o777 != 0o700
        {
            return Err("authority_directory_not_private");
        }
        if !path.is_absolute()
            || !m.is_file()
            || m.file_type().is_symlink()
            || m.nlink() != 1
            || m.uid() != unsafe { libc::geteuid() }
            || m.mode() & 0o777 != 0o600
            || m.len() > maximum as u64
        {
            return Err("authority_file_not_private");
        }
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| "authority_file_unavailable")?;
        let actual = f.metadata().map_err(|_| "authority_file_unavailable")?;
        if actual.ino() != m.ino() || actual.dev() != m.dev() {
            return Err("authority_file_changed");
        }
        let mut bytes = vec![];
        f.by_ref()
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "authority_file_unavailable")?;
        if bytes.len() > maximum {
            return Err("authority_file_too_large");
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, maximum);
        Err("authority_platform_unavailable")
    }
}
#[cfg(target_os = "macos")]
fn process(pid: i32) -> Result<Process, Error> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    if pid <= 1 {
        return Err("caller_unavailable");
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if n != size as i32 {
        return Err("caller_unavailable");
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32
        || info.pbi_uid != unsafe { libc::geteuid() }
        || info.pbi_ruid != info.pbi_uid
    {
        return Err("caller_user_mismatch");
    }
    // Executable path and fixed PID/UID/start-time fields only. Never argv/env.
    let mut raw = [0u8; 4096];
    if unsafe { libc::proc_pidpath(pid, raw.as_mut_ptr().cast(), raw.len() as u32) } <= 0 {
        return Err("caller_unavailable");
    }
    let end = raw
        .iter()
        .position(|b| *b == 0)
        .ok_or("caller_unavailable")?;
    let path = PathBuf::from(std::str::from_utf8(&raw[..end]).map_err(|_| "caller_unavailable")?);
    let m = path.symlink_metadata().map_err(|_| "caller_unavailable")?;
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.uid() != info.pbi_uid
        || m.mode() & 0o022 != 0
        || m.len() > 256 * 1024 * 1024
    {
        return Err("caller_executable_untrusted");
    }
    let bytes = std::fs::read(&path).map_err(|_| "caller_unavailable")?;
    Ok(Process {
        pid,
        parent: info.pbi_ppid as i32,
        started: [info.pbi_start_tvsec, info.pbi_start_tvusec],
        path,
        hash: format!("{:x}", Sha256::digest(bytes)),
    })
}
#[cfg(not(target_os = "macos"))]
fn process(_pid: i32) -> Result<Process, Error> {
    Err("authority_platform_unavailable")
}
impl Authority {
    pub fn new(directory: &Path, generation: &str, gui_hash: &str) -> Self {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Locator {
            desktop_launch: PathBuf,
        }
        let launch = owned(&directory.join("octendra-authority.json"), 4096)
            .ok()
            .and_then(|b| serde_json::from_slice::<Locator>(&b).ok())
            .map(|l| l.desktop_launch)
            .filter(|p| {
                p.is_absolute() && p.file_name().is_some_and(|n| n == "desktop-launch.json")
            });
        Self {
            launch,
            directory: directory.to_owned(),
            generation: generation.into(),
            gui_hash: gui_hash.into(),
        }
    }
    pub fn configured(&self) -> bool {
        cfg!(target_os = "macos") && self.launch.is_some()
    }
    pub fn verify(
        &self,
        call: &Call,
        peer: Option<i32>,
        saved: Option<&Caller>,
    ) -> Result<Caller, Error> {
        let path = self
            .launch
            .as_ref()
            .ok_or("octendra_authority_not_configured")?;
        let root_bytes = owned(path, 16384)?;
        let root: Value = serde_json::from_slice(&root_bytes).map_err(|_| "authority_invalid")?;
        let (actor, lease_id) = call.identity();
        let worker_path = path
            .parent()
            .ok_or("authority_invalid")?
            .join(format!("desktop-worker-{lease_id}.json"));
        let worker_bytes = owned(&worker_path, 16384)?;
        let worker: Value =
            serde_json::from_slice(&worker_bytes).map_err(|_| "task_authority_invalid")?;
        let authority_digest = authority_digest(&root_bytes, &worker_bytes);
        let expires_at_ms = worker["expires_at_ms"]
            .as_u64()
            .ok_or("task_authority_invalid")?;
        authorize(
            &root,
            &worker,
            actor,
            call,
            &self.directory,
            &self.generation,
            &self.gui_hash,
        )?;
        let peer = process(peer.ok_or("caller_unavailable")?)?;
        let caller = if matches!(call, Call::Validate { .. }) {
            let saved = saved.ok_or("observer_closed")?;
            if peer != saved.agent {
                return Err("caller_not_approved_agent");
            }
            let worker = process(saved.worker.pid)?;
            let agent = process(worker.parent)?;
            if worker != saved.worker || agent != saved.agent {
                return Err("worker_process_changed");
            }
            Caller {
                worker,
                agent,
                authority_digest,
                expires_at_ms,
            }
        } else {
            let agent = process(peer.parent)?;
            let caller = Caller {
                worker: peer,
                agent,
                authority_digest,
                expires_at_ms,
            };
            caller
        };
        if !matches!(call, Call::Open { .. })
            && saved.is_some_and(|s| s.worker != caller.worker || s.agent != caller.agent)
        {
            return Err("caller_process_changed");
        }
        if !matches!(call, Call::Open { .. } | Call::Close { .. })
            && saved.is_some_and(|s| s != &caller)
        {
            return Err("caller_or_authority_changed");
        }
        if root["worker_sha256"] != caller.worker.hash
            || root["agent_sha256"] != caller.agent.hash
            || root["worker_path"].as_str().map(Path::new) != Some(caller.worker.path.as_path())
        {
            return Err("caller_executable_not_approved");
        }
        Ok(caller)
    }
}
fn authorize(
    root: &Value,
    worker: &Value,
    actor: &Actor,
    call: &Call,
    directory: &Path,
    generation: &str,
    gui_hash: &str,
) -> Result<(), Error> {
    let closing = matches!(call, Call::Close { .. });
    let now = crate::unix_now_ms();
    let target: Target = serde_json::from_value(root["weavelo"]["target"].clone())
        .map_err(|_| "authority_target_missing")?;
    let profile = root["weavelo"]["profile"]
        .as_str()
        .map(Path::new)
        .ok_or("authority_profile_missing")?;
    let version = root["capability_version"]
        .as_u64()
        .ok_or("authority_version_missing")?;
    if version == 0
        || root["enabled"] != true
        || root["observe"] != true
        || !root["input"].is_boolean()
        || worker["allow_input"] != root["input"]
        || (root["input"] == true && root["weavelo"]["input_platform"] != "macos")
        || root["identity"]["owner"] != actor.owner
        || root["identity"]["device_id"] != actor.executor_device_id
        || root["identity"]["desktop_id"] != target.desktop_id()
        || root["weavelo"]["generation"] != generation
        || root["weavelo"]["gui_executable_sha256"] != gui_hash
        || (directory != profile && directory != profile.join("gui-state"))
        || worker["weavelo"] != root["weavelo"]
        || worker["enabled"] != true
        || worker["app_id"] != "fun.hackerlife.octendra.desktop-worker"
        || worker["desktop_session"] != "weavelo"
        || root["desktop_session"] != "weavelo"
        || root["desktop_uid"] != worker["desktop_uid"]
        || worker["identity"]["owner"] != actor.owner
        || worker["identity"]["task_id"] != actor.task_id
        || worker["identity"]["device_id"] != actor.executor_device_id
        || worker["identity"]["desktop_id"] != target.desktop_id()
        || worker["capability_version"] != version
        || worker["worker_sha256"] != root["worker_sha256"]
        || worker["agent_sha256"] != root["agent_sha256"]
        || (!closing
            && worker["expires_at_ms"]
                .as_u64()
                .is_none_or(|n| n <= now || n > now + 55_000))
    {
        return Err("octendra_task_authority_mismatch");
    }
    #[cfg(unix)]
    if root["desktop_uid"] != unsafe { libc::geteuid() } {
        return Err("authority_user_mismatch");
    }
    if let Call::Open {
        target: requested,
        max_bytes,
        control,
        ..
    } = call
    {
        if root["input"] != *control
            || requested != &target
            || root["max_frame_bytes"]
                .as_u64()
                .is_none_or(|n| *max_bytes as u64 > n)
            || worker["max_frame_bytes"]
                .as_u64()
                .is_none_or(|n| *max_bytes as u64 > n)
        {
            return Err("authority_target_or_budget_mismatch");
        }
    }
    if matches!(call, Call::Act { .. }) && root["input"] != true {
        return Err("input_not_authorized");
    }
    Ok(())
}
