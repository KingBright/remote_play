use remote_core::mesh::default_app_private_mesh_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const PREFERENCES_FILE_NAME: &str = "preferences.json";

fn default_width() -> u32 {
    1920
}

fn default_height() -> u32 {
    1080
}

fn default_fps() -> u32 {
    60
}

fn default_bitrate_kbps() -> u32 {
    20_000
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_scale_mode() -> String {
    "aspect_fit".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPreferences {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_bitrate_kbps")]
    pub bitrate_kbps: u32,
}

impl Default for StreamPreferences {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            fps: default_fps(),
            bitrate_kbps: default_bitrate_kbps(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideServicePreferences {
    #[serde(default = "default_true")]
    pub clipboard_sync: bool,
    #[serde(default = "default_true")]
    pub file_transfer: bool,
    #[serde(default = "default_true")]
    pub talkback: bool,
}

impl Default for SideServicePreferences {
    fn default() -> Self {
        Self {
            clipboard_sync: default_true(),
            file_transfer: default_true(),
            talkback: default_true(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiPreferences {
    #[serde(default = "default_true")]
    pub pause_when_inactive: bool,
    #[serde(default = "default_scale_mode")]
    pub scale_mode: String,
    #[serde(default = "default_false")]
    pub telemetry_hud_collapsed: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            pause_when_inactive: true,
            scale_mode: default_scale_mode(),
            telemetry_hud_collapsed: default_false(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct UserPreferences {
    #[serde(default)]
    pub stream: StreamPreferences,
    #[serde(default)]
    pub side_services: SideServicePreferences,
    #[serde(default)]
    pub ui: UiPreferences,
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl UserPreferences {
    pub fn default_preferences_dir() -> PathBuf {
        default_app_private_mesh_dir()
    }
    pub fn default_path() -> PathBuf {
        #[cfg(test)]
        {
            static TEST_DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
            TEST_DIR
                .get_or_init(|| tempfile::tempdir().expect("test preferences directory"))
                .path()
                .join(PREFERENCES_FILE_NAME)
        }
        #[cfg(not(test))]
        {
            Self::default_preferences_dir().join(PREFERENCES_FILE_NAME)
        }
    }
    fn ensure_default() -> io::Result<PathBuf> {
        #[cfg(test)]
        {
            Ok(Self::default_path())
        }
        #[cfg(not(test))]
        {
            Self::initialize_profile(&Self::default_preferences_dir())
        }
    }
    /// Snapshot the old document once, under the new instance's lock. Never
    /// open a legacy lock for writing, rewrite its document, or follow links.
    pub(crate) fn initialize_profile(profile: &Path) -> io::Result<PathBuf> {
        remote_core::desktop_storage::prepare_profile(profile)?;
        let path = profile.join(PREFERENCES_FILE_NAME);
        let _lock = Self::lock_file(&path)?;
        if let Some(mut current) = remote_core::desktop_storage::open_regular(&path, true)? {
            let mut bytes = Vec::new();
            current.read_to_end(&mut bytes)?;
            Self::parse(&bytes)?;
            return Ok(path);
        }
        let legacy = profile.parent().map(|p| p.join(PREFERENCES_FILE_NAME));
        let bytes = if let Some(mut source) = legacy
            .as_ref()
            .map(|p| remote_core::desktop_storage::open_regular(p, false))
            .transpose()?
            .flatten()
        {
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes)?;
            Self::parse(&bytes)?;
            bytes
        } else {
            // A fresh independent instance never enables receiving by accident.
            let mut prefs = Self::default();
            prefs.side_services.file_transfer = false;
            serde_json::to_vec_pretty(&prefs).map_err(io::Error::other)?
        };
        Self::write_document(&path, &bytes, true)?;
        Ok(path)
    }
    pub fn load_or_default() -> Self {
        match Self::ensure_default().and_then(|p| Self::read_or_default(&p)) {
            Ok(prefs) => prefs,
            Err(error) => {
                eprintln!("Preferences unavailable; file transfers remain disabled: {error}");
                let mut prefs = Self::default();
                prefs.side_services.file_transfer = false;
                prefs
            }
        }
    }
    pub fn load_from_path(path: &Path) -> Option<Self> {
        Self::read_document(path).ok().flatten().map(|(_, p)| p)
    }
    pub fn save(&self) -> io::Result<()> {
        self.save_to_path(&Self::ensure_default()?)
    }
    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        let _lock = Self::lock_file(path)?;
        let existing = Self::read_document(path)?;
        let (mut raw, before) = existing
            .clone()
            .unwrap_or((serde_json::json!({}), Self::default()));
        let before = serde_json::to_value(before).map_err(io::Error::other)?;
        let after = serde_json::to_value(self).map_err(io::Error::other)?;
        if existing.is_none() {
            raw = after.clone();
        } else {
            Self::merge_changes(&mut raw, &before, &after);
        }
        if existing.is_none() || before != after {
            Self::write_document(
                path,
                &serde_json::to_vec_pretty(&raw).map_err(io::Error::other)?,
                false,
            )?;
        }
        Ok(())
    }
    /// The stable sibling lock covers read, change and atomic replacement.
    /// Merge only changed typed fields into the original document so unknown
    /// nested fields survive, including fields older readers do not model.
    pub fn update(change: impl FnOnce(&mut Self)) -> io::Result<()> {
        Self::update_at_path(&Self::ensure_default()?, change)
    }
    fn update_at_path(path: &Path, change: impl FnOnce(&mut Self)) -> io::Result<()> {
        let _lock = Self::lock_file(path)?;
        let (mut raw, mut prefs) =
            Self::read_document(path)?.unwrap_or_else(|| (serde_json::json!({}), Self::default()));
        let before = serde_json::to_value(&prefs).map_err(io::Error::other)?;
        change(&mut prefs);
        let after = serde_json::to_value(&prefs).map_err(io::Error::other)?;
        if before != after {
            Self::merge_changes(&mut raw, &before, &after);
            Self::write_document(
                path,
                &serde_json::to_vec_pretty(&raw).map_err(io::Error::other)?,
                false,
            )?;
        }
        Ok(())
    }
    fn merge_changes(
        raw: &mut serde_json::Value,
        before: &serde_json::Value,
        after: &serde_json::Value,
    ) {
        if before == after {
            return;
        }
        if let (Some(old), Some(new)) = (before.as_object(), after.as_object()) {
            if !raw.is_object() {
                *raw = serde_json::json!({});
            }
            let target = raw.as_object_mut().unwrap();
            for (key, previous) in old {
                match new.get(key) {
                    Some(next) if next != previous => Self::merge_changes(
                        target.entry(key.clone()).or_insert(serde_json::Value::Null),
                        previous,
                        next,
                    ),
                    None => {
                        target.remove(key);
                    }
                    _ => {}
                }
            }
            for (key, value) in new {
                if !old.contains_key(key) {
                    target.insert(key.clone(), value.clone());
                }
            }
        } else {
            *raw = after.clone();
        }
    }
    fn parse(bytes: &[u8]) -> io::Result<(serde_json::Value, Self)> {
        Ok((
            serde_json::from_slice(bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            serde_json::from_slice(bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        ))
    }
    fn read_document(path: &Path) -> io::Result<Option<(serde_json::Value, Self)>> {
        let Some(mut file) = remote_core::desktop_storage::open_regular(path, false)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Self::parse(&bytes).map(Some)
    }
    fn read_or_default(path: &Path) -> io::Result<Self> {
        Ok(Self::read_document(path)?
            .map(|(_, p)| p)
            .unwrap_or_default())
    }
    fn parent_dir(path: &Path) -> &Path {
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }
    fn lock_file(path: &Path) -> io::Result<fs::File> {
        let mut lock = path.as_os_str().to_os_string();
        lock.push(".lock");
        remote_core::desktop_storage::lock_file(&PathBuf::from(lock))
    }
    fn write_document(path: &Path, bytes: &[u8], new_only: bool) -> io::Result<()> {
        let before = remote_core::desktop_storage::regular_file(path, false)?;
        if new_only && before.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Scoped preferences appeared; existing file retained",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(Self::parent_dir(path))?;
        if let Some(metadata) = before {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.write_all(bytes)?;
        temporary.as_file().sync_all()?;
        remote_core::desktop_storage::regular_file(path, false)?;
        if new_only {
            temporary.persist_noclobber(path).map_err(|e| e.error)?;
        } else {
            temporary.persist(path).map_err(|e| e.error)?;
        }
        #[cfg(unix)]
        fs::File::open(Self::parent_dir(path))?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_default_values_are_valid() {
        let prefs = UserPreferences::default();
        assert_eq!(prefs.stream.width, 1920);
        assert_eq!(prefs.stream.height, 1080);
        assert_eq!(prefs.stream.fps, 60);
        assert_eq!(prefs.stream.bitrate_kbps, 20_000);
        assert!(prefs.side_services.clipboard_sync);
        assert!(prefs.side_services.file_transfer);
        assert!(prefs.side_services.talkback);
        assert_eq!(prefs.ui.scale_mode, "aspect_fit");
        assert!(!prefs.ui.telemetry_hud_collapsed);
    }

    #[test]
    fn preferences_round_trip_serialization() {
        let mut prefs = UserPreferences::default();
        prefs.stream.width = 3840;
        prefs.stream.height = 2160;
        prefs.stream.fps = 120;
        prefs.stream.bitrate_kbps = 80_000;
        prefs.ui.scale_mode = "fill".to_string();
        prefs.ui.telemetry_hud_collapsed = true;

        let json = serde_json::to_string_pretty(&prefs).expect("serialize");
        let deserialized: UserPreferences = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(prefs, deserialized);
    }

    #[test]
    fn preferences_atomic_disk_save_and_load() {
        let temp_dir =
            std::env::temp_dir().join(format!("weavelo-prefs-test-{}", std::process::id()));
        let file_path = temp_dir.join("test_prefs.json");

        let mut prefs = UserPreferences::default();
        prefs.stream.width = 2560;
        prefs.stream.height = 1440;
        prefs.stream.fps = 60;
        prefs.stream.bitrate_kbps = 40_000;

        prefs.save_to_path(&file_path).expect("save");
        let loaded = UserPreferences::load_from_path(&file_path).expect("load");
        assert_eq!(prefs, loaded);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn preferences_backward_forward_compatibility() {
        // Old or missing fields should fall back to default cleanly
        let partial_json = r#"{
            "stream": {
                "width": 1280,
                "height": 720
            },
            "future_feature_flag": true,
            "future_nested_setting": { "mode": "ultra" }
        }"#;

        let prefs: UserPreferences = serde_json::from_str(partial_json).expect("parse partial");
        assert_eq!(prefs.stream.width, 1280);
        assert_eq!(prefs.stream.height, 720);
        assert_eq!(prefs.stream.fps, 60); // Default applied
        assert_eq!(prefs.stream.bitrate_kbps, 20_000); // Default applied
        assert!(prefs.extra.contains_key("future_feature_flag"));
        assert!(prefs.extra.contains_key("future_nested_setting"));
    }

    #[test]
    fn concurrent_updates_preserve_each_field_and_increment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..10 {
                        UserPreferences::update_at_path(path, |prefs| {
                            prefs.stream.bitrate_kbps += 1;
                            prefs.extra.insert(format!("worker-{worker}"), true.into());
                        })
                        .unwrap();
                    }
                });
            }
        });
        let prefs = UserPreferences::load_from_path(&path).unwrap();
        assert_eq!(prefs.stream.bitrate_kbps, 20_080);
        assert_eq!(prefs.extra.len(), 8);
    }

    #[test]
    fn invalid_preferences_are_preserved_on_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        fs::write(&path, b"{ invalid configuration").unwrap();
        let err =
            UserPreferences::update_at_path(&path, |prefs| prefs.stream.fps = 120).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), b"{ invalid configuration");
    }

    #[test]
    fn failed_replace_leaves_destination_and_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "original").unwrap();
        assert!(UserPreferences::default().save_to_path(&path).is_err());
        assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2); // destination and lock
    }

    #[test]
    fn unchanged_update_does_not_replace_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        let json = "{\"stream\":{\"fps\":60},\"future_feature\":true}";
        fs::write(&path, json).unwrap();
        UserPreferences::update_at_path(&path, |prefs| prefs.stream.fps = 60).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), json);
    }

    fn legacy_fixture(base: &Path) -> PathBuf {
        let parent = base.join("Weavelo");
        remote_core::desktop_storage::prepare_profile(&parent).unwrap();
        let bytes=br#"{"stream":{"bitrate_kbps":50000,"future_codec":{"keep":true}},"side_services":{"file_transfer":false,"future_rule":{"scope":"keep"}},"receive_dir":"/old/retained/directory","user_note":{"text":"preserve all fields"}}"#;
        fs::write(parent.join(PREFERENCES_FILE_NAME), bytes).unwrap();
        parent
    }
    #[test]
    fn profile_snapshot_preserves_disabled_setting_and_all_nested_user_fields() {
        let f = tempfile::tempdir().unwrap();
        let parent = legacy_fixture(f.path());
        let legacy = parent.join(PREFERENCES_FILE_NAME);
        let original = fs::read(&legacy).unwrap();
        let metadata = legacy.metadata().unwrap();
        let path = UserPreferences::initialize_profile(&parent.join("Desktop")).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(
            !UserPreferences::load_from_path(&path)
                .unwrap()
                .side_services
                .file_transfer
        );
        UserPreferences::update_at_path(&path, |p| p.stream.fps = 90).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["stream"]["future_codec"]["keep"], true);
        assert_eq!(value["side_services"]["future_rule"]["scope"], "keep");
        assert_eq!(value["receive_dir"], "/old/retained/directory");
        assert_eq!(value["user_note"]["text"], "preserve all fields");
        assert_eq!(fs::read(&legacy).unwrap(), original);
        assert_eq!(
            legacy.metadata().unwrap().permissions(),
            metadata.permissions()
        );
    }
    #[test]
    fn independent_profiles_snapshot_once_and_do_not_follow_parent_changes() {
        let f = tempfile::tempdir().unwrap();
        let parent = legacy_fixture(f.path());
        let a = UserPreferences::initialize_profile(&parent.join("Desktop-a")).unwrap();
        let b = UserPreferences::initialize_profile(&parent.join("Desktop-b")).unwrap();
        let old_b = fs::read(&b).unwrap();
        fs::write(
            parent.join(PREFERENCES_FILE_NAME),
            br#"{"side_services":{"file_transfer":true},"user_note":"new parent value"}"#,
        )
        .unwrap();
        UserPreferences::update_at_path(&a, |p| p.ui.scale_mode = "fill".into()).unwrap();
        UserPreferences::initialize_profile(a.parent().unwrap()).unwrap();
        assert!(
            !UserPreferences::load_from_path(&a)
                .unwrap()
                .side_services
                .file_transfer
        );
        assert_eq!(fs::read(b).unwrap(), old_b);
        assert!(
            UserPreferences::load_from_path(&parent.join(PREFERENCES_FILE_NAME))
                .unwrap()
                .side_services
                .file_transfer
        );
    }
    #[test]
    fn missing_or_invalid_migration_cannot_enable_receiving_or_replace_user_data() {
        let f = tempfile::tempdir().unwrap();
        let profile = f.path().join("Desktop");
        let path = UserPreferences::initialize_profile(&profile).unwrap();
        assert!(
            !UserPreferences::load_from_path(&path)
                .unwrap()
                .side_services
                .file_transfer
        );
        fs::write(&path, b"invalid scoped document").unwrap();
        assert!(UserPreferences::initialize_profile(&profile).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid scoped document");
        let other = f.path().join("Other");
        fs::write(
            f.path().join(PREFERENCES_FILE_NAME),
            b"invalid legacy document",
        )
        .unwrap();
        assert!(UserPreferences::initialize_profile(&other).is_err());
        assert!(!other.join(PREFERENCES_FILE_NAME).exists());
        assert_eq!(
            fs::read(f.path().join(PREFERENCES_FILE_NAME)).unwrap(),
            b"invalid legacy document"
        );
    }
    #[cfg(unix)]
    #[test]
    fn scoped_migration_refuses_linked_entries_without_touching_targets() {
        use std::os::unix::fs::symlink;
        let f = tempfile::tempdir().unwrap();
        let parent = legacy_fixture(f.path());
        let profile = parent.join("Desktop");
        let original = fs::read(parent.join(PREFERENCES_FILE_NAME)).unwrap();
        remote_core::desktop_storage::prepare_profile(&profile).unwrap();
        symlink(
            parent.join(PREFERENCES_FILE_NAME),
            profile.join(PREFERENCES_FILE_NAME),
        )
        .unwrap();
        assert!(UserPreferences::initialize_profile(&profile).is_err());
        assert!(profile.join(PREFERENCES_FILE_NAME).is_symlink());
        assert_eq!(
            fs::read(parent.join(PREFERENCES_FILE_NAME)).unwrap(),
            original
        );
    }
    #[test]
    #[ignore = "subprocess worker; invoked only with an owned temporary fixture path"]
    fn scoped_preference_update_worker() {
        let Some(path) = std::env::var_os("WEAVELO_PREFS_TEST_CHILD") else {
            return;
        };
        let path = PathBuf::from(path);
        for _ in 0..20 {
            UserPreferences::update_at_path(&path, |p| p.stream.bitrate_kbps += 1).unwrap();
        }
    }
    #[test]
    fn two_process_updates_are_serialized_and_leave_other_instances_and_legacy_unchanged() {
        let f = tempfile::tempdir().unwrap();
        let parent = legacy_fixture(f.path());
        let a = UserPreferences::initialize_profile(&parent.join("Desktop-a")).unwrap();
        let b = UserPreferences::initialize_profile(&parent.join("Desktop-b")).unwrap();
        let old_b = fs::read(&b).unwrap();
        let old_parent = fs::read(parent.join(PREFERENCES_FILE_NAME)).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for _ in 0..2 {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "preferences::tests::scoped_preference_update_worker",
                        "--ignored",
                    ])
                    .env("WEAVELO_PREFS_TEST_CHILD", &a)
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(
            UserPreferences::load_from_path(&a)
                .unwrap()
                .stream
                .bitrate_kbps,
            50040
        );
        assert_eq!(fs::read(b).unwrap(), old_b);
        assert_eq!(
            fs::read(parent.join(PREFERENCES_FILE_NAME)).unwrap(),
            old_parent
        );
        let value: serde_json::Value = serde_json::from_slice(&fs::read(a).unwrap()).unwrap();
        assert_eq!(value["stream"]["future_codec"]["keep"], true);
        assert_eq!(value["side_services"]["file_transfer"], false);
    }
}
