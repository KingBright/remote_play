//! Private storage belongs to one explicit desktop profile, never its parent.
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

fn denied(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}
pub fn prepare_profile(profile: &Path) -> io::Result<()> {
    if !profile.is_absolute()
        || profile
            .components()
            .any(|p| matches!(p, Component::ParentDir | Component::CurDir))
        || crate::desktop_identity::retained_profile(profile)
    {
        return Err(denied(
            "An absolute independent desktop profile is required",
        ));
    }
    crate::mesh::create_private_dir(profile).map_err(|e| denied(&e.to_string()))
}
pub fn regular_file(path: &Path, private: bool) -> io::Result<Option<fs::Metadata>> {
    crate::mesh::check_profile_path(path).map_err(|e| denied(&e.to_string()))?;
    let metadata = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(denied("An owned regular file is required"));
    }
    #[cfg(unix)]
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || (private && metadata.mode() & 0o077 != 0)
    {
        return Err(denied(
            "File ownership, link count or private permissions changed; nothing repaired",
        ));
    }
    #[cfg(not(unix))]
    let _ = private;
    Ok(Some(metadata))
}
pub fn open_regular(path: &Path, private: bool) -> io::Result<Option<fs::File>> {
    let Some(before) = regular_file(path, private)? else {
        return Ok(None);
    };
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let Some(after) = regular_file(path, private)? else {
        return Err(denied("File disappeared while opening"));
    };
    #[cfg(unix)]
    if (before.dev(), before.ino()) != (opened.dev(), opened.ino())
        || (after.dev(), after.ino()) != (opened.dev(), opened.ino())
    {
        return Err(denied("File changed while opening"));
    }
    if before.len() != opened.len() || after.len() != opened.len() {
        return Err(denied("File changed while opening"));
    }
    Ok(Some(file))
}
pub fn lock_file(path: &Path) -> io::Result<fs::File> {
    prepare_profile(
        path.parent()
            .ok_or_else(|| denied("Missing profile directory"))?,
    )?;
    regular_file(path, true)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    regular_file(path, true)?;
    #[cfg(unix)]
    {
        let m = file.metadata()?;
        let current = regular_file(path, true)?.ok_or_else(|| denied("Lock disappeared"))?;
        if m.uid() != unsafe { libc::geteuid() }
            || m.nlink() != 1
            || m.mode() & 0o077 != 0
            || (m.dev(), m.ino()) != (current.dev(), current.ino())
        {
            return Err(denied("Private lock changed"));
        }
    }
    file.lock()?;
    Ok(file)
}
/// Configured overrides remain within this instance. Existing unsafe paths are
/// refused rather than followed, chmodded, replaced, or mapped to an old folder.
pub fn receive_directory_at(profile: &Path, requested: Option<&Path>) -> io::Result<PathBuf> {
    prepare_profile(profile)?;
    let target = requested
        .map(Path::to_path_buf)
        .unwrap_or_else(|| profile.join("Received Files"));
    if target == profile
        || !target.is_absolute()
        || !target.starts_with(profile)
        || target
            .components()
            .any(|p| matches!(p, Component::ParentDir | Component::CurDir))
    {
        return Err(denied(
            "Receive directory must remain inside this desktop profile",
        ));
    }
    prepare_profile(&target)?;
    Ok(target)
}
pub fn receive_directory() -> io::Result<PathBuf> {
    let profile = crate::mesh::default_app_private_mesh_dir();
    let requested = std::env::var_os("WEAVELO_FILE_RECEIVE_DIR").map(PathBuf::from);
    receive_directory_at(&profile, requested.as_deref())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "weavelo-storage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            prepare_profile(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn receive_paths_are_instance_scoped_and_existing_bytes_are_preserved() {
        let f = Fixture::new();
        let a = f.0.join("a");
        let b = f.0.join("b");
        let first = receive_directory_at(&a, None).unwrap();
        let second = receive_directory_at(&b, None).unwrap();
        assert_ne!(first, second);
        fs::write(first.join("keep"), b"original").unwrap();
        assert_eq!(receive_directory_at(&a, None).unwrap(), first);
        assert_eq!(fs::read(first.join("keep")).unwrap(), b"original");
        assert!(fs::read_dir(second).unwrap().next().is_none());
    }
    #[test]
    fn receive_path_cannot_escape_profile_or_use_retained_namespace() {
        let f = Fixture::new();
        let profile = f.0.join("Desktop");
        for path in [
            f.0.join("outside"),
            profile.clone(),
            profile.join("../outside"),
        ] {
            assert!(receive_directory_at(&profile, Some(&path)).is_err());
        }
        assert!(!f.0.join("outside").exists());
        assert!(prepare_profile(&f.0.join("Weavelo/NativeMesh")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn linked_or_nonprivate_receive_directories_are_refused_without_repair() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let f = Fixture::new();
        let profile = f.0.join("Desktop");
        prepare_profile(&profile).unwrap();
        let outside = f.0.join("outside");
        prepare_profile(&outside).unwrap();
        symlink(&outside, profile.join("Received Files")).unwrap();
        assert!(receive_directory_at(&profile, None).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        fs::remove_file(profile.join("Received Files")).unwrap();
        let dir = profile.join("Received Files");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(receive_directory_at(&profile, None).is_err());
        assert_eq!(dir.metadata().unwrap().mode() & 0o777, 0o755);
    }
    #[cfg(unix)]
    #[test]
    fn linked_preferences_and_locks_are_not_followed_or_replaced() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = f.0.join("original");
        fs::write(&outside, b"original").unwrap();
        let linked = f.0.join("preferences.json");
        symlink(&outside, &linked).unwrap();
        assert!(open_regular(&linked, false).is_err());
        assert!(lock_file(&linked).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"original");
        assert!(linked.is_symlink());
    }
}
