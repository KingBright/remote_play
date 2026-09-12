use remote_core::mesh::default_app_private_mesh_dir;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
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
    #[serde(default = "default_scale_mode")]
    pub scale_mode: String,
    #[serde(default = "default_false")]
    pub telemetry_hud_collapsed: bool,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
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
        let mesh_dir = default_app_private_mesh_dir();
        if let Some(parent) = mesh_dir.parent() {
            parent.to_path_buf()
        } else {
            mesh_dir
        }
    }

    pub fn default_path() -> PathBuf {
        #[cfg(test)]
        {
            // Runtime-owner tests use this API too: never write the user's real preferences.
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

    pub fn load_or_default() -> Self {
        match Self::read_or_default(&Self::default_path()) {
            Ok(preferences) => preferences,
            Err(err) => {
                eprintln!("Failed to load preferences; using defaults for this session: {err}");
                Self::default()
            }
        }
    }

    pub fn load_from_path(path: &Path) -> Option<Self> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to_path(&Self::default_path())
    }

    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        let _lock = Self::lock_file(path)?;
        self.write_atomic(path)
    }

    /// Serialize the entire read/modify/replace operation, including other app processes.
    /// Invalid or unreadable existing preferences must never be replaced with defaults.
    pub fn update(change: impl FnOnce(&mut Self)) -> io::Result<()> {
        Self::update_at_path(&Self::default_path(), change)
    }

    fn update_at_path(path: &Path, change: impl FnOnce(&mut Self)) -> io::Result<()> {
        let _lock = Self::lock_file(path)?;
        let mut preferences = Self::read_or_default(path)?;
        let previous = preferences.clone();
        change(&mut preferences);
        if preferences != previous {
            preferences.write_atomic(path)?;
        }
        Ok(())
    }

    fn read_or_default(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(data) => serde_json::from_slice(&data)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    fn parent_dir(path: &Path) -> &Path {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn lock_file(path: &Path) -> io::Result<fs::File> {
        fs::create_dir_all(Self::parent_dir(path))?;
        // A stable sibling lock survives replacement of the preferences file itself.
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(PathBuf::from(lock_path))?;
        lock.lock()?;
        Ok(lock)
    }

    fn write_atomic(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Unique, same-filesystem temporary file; persist replaces atomically on Windows too.
        let mut temp = tempfile::NamedTempFile::new_in(Self::parent_dir(path))?;
        temp.write_all(json.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|err| err.error)?;
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
            std::env::temp_dir().join(format!("remote-play-prefs-test-{}", std::process::id()));
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
}
