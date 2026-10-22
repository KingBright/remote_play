//! Native file commands reuse the original GUI connection and transfer service.
//! A private durable request reservation prevents retrying uncertain sends.
use super::original_owner::OriginalOwner;
use client::{TransferDirection, TransferStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum Action {
    Send {
        target: String,
        path: PathBuf,
        sha256: String,
    },
    Status,
    Peers,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    request_id: String,
    generation: String,
    target: String,
    path: PathBuf,
    sha256: String,
    bytes: u64,
    state: String,
    terminal: bool,
    connection_id: Option<u32>,
    transfer_id: Option<u64>,
    transferred_bytes: u64,
    detail: String,
    remote_content_verified: bool,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn digest(path: &Path) -> Result<(String, u64), Error> {
    if !path.is_absolute()
        || path
            .ancestors()
            .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    {
        return Err("Choose an absolute non-linked source file".into());
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err("Source must be a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
            return Err("Source must be an owned, unaliased regular file".into());
        }
    }
    if meta.len() > remote_core::file_transfer::FileTransferPolicy::default().max_file_bytes {
        return Err("Source exceeds the existing file transfer limit".into());
    }
    let mut input = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if input.metadata()?.len() != meta.len() {
        return Err("Source changed during verification".into());
    }
    Ok((format!("{:x}", hash.finalize()), meta.len()))
}
fn read(path: &Path) -> Result<Record, Error> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 16384 {
        return Err("Invalid file request receipt".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.nlink() != 1
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err("Private file request receipt required".into());
        }
    }
    let record: Record = serde_json::from_slice(&fs::read(path)?)?;
    if record.schema != 1 || !valid_id(&record.request_id) {
        return Err("Unsupported file request receipt".into());
    }
    Ok(record)
}
fn reserve(path: &Path, record: &Record) -> Result<bool, Error> {
    use std::fs::OpenOptions;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut out) => {
            out.write_all(&serde_json::to_vec(record)?)?;
            out.sync_all()?;
            #[cfg(unix)]
            fs::File::open(path.parent().ok_or("Missing receipt directory")?)?.sync_all()?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let old = read(path)?;
            if old.target != record.target
                || old.path != record.path
                || old.sha256 != record.sha256
                || old.bytes != record.bytes
            {
                return Err("Request ID is already bound to a different transfer".into());
            }
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}
fn save(path: &Path, record: &Record) -> Result<(), Error> {
    let previous = read(path)?;
    if previous.request_id != record.request_id
        || previous.generation != record.generation
        || previous.target != record.target
        || previous.path != record.path
        || previous.sha256 != record.sha256
    {
        return Err("File request receipt changed".into());
    }
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing receipt directory")?)?;
    temp.write_all(&serde_json::to_vec(record)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().ok_or("Missing receipt directory")?)?.sync_all()?;
    Ok(())
}
fn directory(gui_directory: &Path) -> Result<PathBuf, Error> {
    crate::private_ipc::private_directory(gui_directory)?;
    let dir = gui_directory.join("file-requests");
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    // Existing paths are validated, never chmodded or followed through a link.
    crate::private_ipc::private_directory(&dir)?;
    Ok(dir)
}
fn response(record: &Record) -> Result<serde_json::Value, Error> {
    Ok(serde_json::json!({"ok":true,"file_request":record}))
}
pub(crate) fn dispatch(
    owner: Arc<OriginalOwner>,
    profile: &Path,
    generation: &str,
    id: &str,
    action: Action,
) -> Result<serde_json::Value, Error> {
    if !valid_id(id) {
        return Err("Invalid file request ID".into());
    }
    if matches!(&action, Action::Peers) {
        let configuration = crate::UnifiedRuntimeConfig::from_env()?;
        return Ok(serde_json::json!({"ok":true,"generation":generation,
            "file_transfer_preference_enabled":owner.native_file_transfer_enabled(&configuration),
            "paired_file_scope_present":remote_core::session_crypto::paired_file_session_key().is_some(),
            "devices":owner.snapshot().devices.iter().map(|d|serde_json::json!({"device_id":d.device_id,"display_name":d.display_name,"online":d.online})).collect::<Vec<_>>(),
            "receive_dir":owner.receive_dir()}));
    }
    let path = directory(profile)?.join(format!("{id}.json"));
    match action {
        Action::Status => {
            let record = read(&path)?;
            let mut result = response(&record)?;
            result["owner_generation_current"] = serde_json::json!(record.generation == generation);
            Ok(result)
        }
        Action::Send {
            target,
            path: source,
            sha256,
        } => {
            if target.is_empty()
                || target.len() > 256
                || target.chars().any(char::is_control)
                || sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err("Target device and lowercase SHA-256 are required".into());
            }
            let (actual, bytes) = digest(&source)?;
            if actual != sha256 {
                return Err("Selected source differs from expected SHA-256".into());
            }
            let record = Record {
                schema: 1,
                request_id: id.into(),
                generation: generation.into(),
                target,
                path: source,
                sha256,
                bytes,
                state: "accepted".into(),
                terminal: false,
                connection_id: None,
                transfer_id: None,
                transferred_bytes: 0,
                detail:
                    "Reserved before any connection or send; query this ID if the reply is lost"
                        .into(),
                remote_content_verified: false,
            };
            if !reserve(&path, &record)? {
                return response(&read(&path)?);
            }
            // The reservation survives a disconnected caller or GUI restart. It
            // is never deleted or automatically replayed, including after failure.
            let reply = response(&record)?;
            let scope = remote_core::shared_files::current_share_scope();
            tokio::spawn(async move {
                let mut record = record;
                if let Err(error) = send(&owner, &path, &mut record, scope).await {
                    record.state = if record.connection_id.is_some() {
                        "not_confirmed"
                    } else {
                        "failed_before_send"
                    }
                    .into();
                    record.terminal = true;
                    record.detail = error.to_string();
                    let _ = save(&path, &record);
                }
            });
            Ok(reply)
        }
        Action::Peers => unreachable!(),
    }
}
async fn send(
    owner: &OriginalOwner,
    path: &Path,
    record: &mut Record,
    scope: remote_core::shared_files::ShareScope,
) -> Result<(), Error> {
    static SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _serial = SERIAL
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if !owner.native_file_transfer_enabled(&crate::UnifiedRuntimeConfig::from_env()?) {
        return Err("File transfers are disabled in the existing product settings".into());
    }
    if scope != remote_core::shared_files::current_share_scope() {
        return Err("Device network changed after reservation".into());
    }
    owner.connect_silent_files(&record.target).await?;
    let (connection, transfers) = owner.cli_file_connection(&record.target, scope)?;
    let baseline = transfers
        .lock()
        .unwrap()
        .identified_snapshots()
        .into_iter()
        .filter_map(|(direction, id, _)| (direction == TransferDirection::Outgoing).then_some(id))
        .collect::<Vec<_>>();
    if digest(&record.path)? != (record.sha256.clone(), record.bytes) {
        return Err("Source changed before queuing".into());
    }
    record.connection_id = connection.upgrade().map(|c| c.id);
    record.state = "queued".into();
    record.detail.clear();
    save(path, record)?;
    owner.send_files_to(&connection, scope, vec![record.path.clone()]);
    let label = record
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Source name is not UTF-8")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        if scope != remote_core::shared_files::current_share_scope()
            || connection.upgrade().is_none()
        {
            return Err("Network or original connection changed; do not retry this request".into());
        }
        let values = transfers.lock().unwrap().identified_snapshots();
        let matches = values
            .iter()
            .filter(|(direction, id, s)| {
                *direction == TransferDirection::Outgoing
                    && record.transfer_id.map_or(
                        !baseline.contains(id) && s.label == label && s.total_bytes == record.bytes,
                        |selected| selected == *id,
                    )
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err("Ambiguous native transfer identity; no success inferred".into());
        }
        if let Some((_, id, snapshot)) = matches.first() {
            record.transfer_id = Some(*id);
            record.transferred_bytes = snapshot.transferred_bytes;
            record.state = match snapshot.status {
                TransferStatus::Running => "sending",
                TransferStatus::Completed => "completed",
                TransferStatus::Cancelled => "cancelled",
                TransferStatus::Error => "error",
            }
            .into();
            record.terminal = snapshot.status != TransferStatus::Running;
            record.detail = snapshot.detail.clone();
            save(path, record)?;
            if record.terminal {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Native transfer completion not observed within 120 seconds; query this request before any new send".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
pub(crate) fn command(profile: &Path, args: &[String]) -> Result<serde_json::Value, Error> {
    use super::directed_receive::{Request, exchange};
    let directory = crate::private_ipc::gui_directory(profile)?;
    let status = exchange(&directory, &Request::Status)?;
    if status["executable_sha256"] != crate::background_service::executable_hash()? {
        return Err("CLI and original GUI executable identity differ".into());
    }
    let generation = status["generation"]
        .as_str()
        .ok_or("GUI generation unavailable")?
        .to_owned();
    let (id, action) = parse(args)?;
    let result = exchange(
        &directory,
        &Request::File {
            generation,
            request_id: id,
            deadline_ms: crate::unix_now_ms().saturating_add(5000),
            action,
        },
    )?;
    if result["ok"] != true {
        return Err(result["error"]
            .as_str()
            .unwrap_or("Original GUI refused file request")
            .to_owned()
            .into());
    }
    Ok(result)
}
fn parse(args: &[String]) -> Result<(String, Action), Error> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice(){
        ["--file-peers-json"]=>Ok((format!("peers-{}",crate::unix_now_ms()),Action::Peers)),
        ["--file-status",id] if valid_id(id)=>Ok(((*id).into(),Action::Status)),
        ["--file-send",target,path,"--sha256",sha,"--request-id",id,"--confirm-send"] if valid_id(id)=>Ok(((*id).into(),Action::Send{target:(*target).into(),path:PathBuf::from(path),sha256:(*sha).into()})),
        _=>Err("Usage: --file-peers-json | --file-status REQUEST_ID | --file-send TARGET_DEVICE_ID ABSOLUTE_SOURCE --sha256 SHA256 --request-id REQUEST_ID --confirm-send".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(path: PathBuf) -> Record {
        Record {
            schema: 1,
            request_id: "send-1".into(),
            generation: "gui-1".into(),
            target: "paired-peer".into(),
            path,
            sha256: "a".repeat(64),
            bytes: 4,
            state: "accepted".into(),
            terminal: false,
            connection_id: None,
            transfer_id: None,
            transferred_bytes: 0,
            detail: String::new(),
            remote_content_verified: false,
        }
    }
    #[test]
    fn requires_explicit_send_confirmation_and_safe_query_id() {
        assert!(parse(&["--file-send".into(), "peer".into(), "/tmp/file".into()]).is_err());
        assert!(parse(&["--file-status".into(), "../receipt".into()]).is_err());
        assert!(parse(&["--file-status".into(), "send-1".into()]).is_ok());
    }
    #[test]
    fn request_reservation_survives_restart_and_rejects_rebinding() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("send-1.json");
        let mut r = sample(d.path().join("payload"));
        assert!(reserve(&path, &r).unwrap());
        assert!(!reserve(&path, &r).unwrap());
        r.state = "completed".into();
        r.terminal = true;
        r.transfer_id = Some(17);
        save(&path, &r).unwrap();
        assert!(!reserve(&path, &r).unwrap());
        assert_eq!(read(&path).unwrap(), r);
        r.target = "different-peer".into();
        assert!(reserve(&path, &r).is_err());
    }
    #[test]
    fn source_digest_rejects_symlink_and_directory() {
        let d = tempfile::tempdir().unwrap();
        let p = fs::canonicalize(d.path()).unwrap().join("payload");
        fs::write(&p, b"test").unwrap();
        assert_eq!(
            digest(&p).unwrap(),
            (format!("{:x}", Sha256::digest(b"test")), 4)
        );
        assert!(digest(d.path()).is_err());
        #[cfg(unix)]
        {
            let link = d.path().join("alias");
            std::os::unix::fs::symlink(&p, &link).unwrap();
            assert!(digest(&link).is_err());
        }
    }
    #[test]
    fn receipt_directory_initializes_once_and_rejects_an_existing_link() {
        let fixture = tempfile::tempdir().unwrap();
        let gui = fs::canonicalize(fixture.path()).unwrap();
        let selected = directory(&gui).unwrap();
        assert_eq!(directory(&gui).unwrap(), selected);
        crate::private_ipc::private_directory(&selected).unwrap();
        fs::remove_dir(&selected).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&gui, &selected).unwrap();
            assert!(directory(&gui).is_err());
            assert!(
                selected
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[tokio::test]
    async fn attached_frontend_uses_effective_preference_without_a_legacy_file_owner() {
        let (_tx, rx) = tokio::sync::watch::channel(Default::default());
        let backend = Arc::new(
            crate::UnifiedServiceOwner::from_background_snapshot(rx)
                .await
                .unwrap(),
        );
        assert!(!backend.file_transfer_enabled());
        let owner = OriginalOwner::new(backend);
        let mut configuration = crate::UnifiedRuntimeConfig::app_defaults();
        assert!(owner.native_file_transfer_enabled(&configuration));
        configuration.enable_file_transfer = false;
        assert!(!owner.native_file_transfer_enabled(&configuration));
        let standalone = OriginalOwner::new(Arc::new(
            crate::UnifiedServiceOwner::start(crate::UnifiedServiceOwnerConfig::default())
                .await
                .unwrap(),
        ));
        configuration.enable_file_transfer = true;
        assert!(!standalone.native_file_transfer_enabled(&configuration));
    }

    #[test]
    fn incomplete_reservation_is_not_silently_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("send-1.json");
        fs::write(&p, b"partial").unwrap();
        assert!(reserve(&p, &sample(d.path().join("payload"))).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"partial");
    }
}
