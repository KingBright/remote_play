//! Shared desktop session state and profile ownership for GPUI and headless mode.
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Weak};

#[cfg(feature = "gpui-restoration")]
pub(crate) mod foreground_runtime;
pub(crate) mod device_list;
#[cfg(feature = "gpui-restoration")]
pub(crate) mod device_drawer;
pub(crate) mod instance;
mod model;
#[cfg(all(test, feature = "gpui-restoration"))]
pub(crate) use model::OriginalGuiSession;
#[cfg(feature = "gpui-restoration")]
pub(crate) mod original_owner;
#[cfg(feature = "gpui-restoration")]
mod original_presenter;
mod requests;

#[derive(Deserialize)]
struct Preferences {
    #[serde(default = "default_receive_dir")]
    receive_dir: PathBuf,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            receive_dir: default_receive_dir(),
        }
    }
}

fn default_receive_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Downloads/RemotePlay")
}

fn prefs_path() -> PathBuf {
    crate::preferences::UserPreferences::default_path()
}

fn same_connection(
    target: &Weak<remote_core::workspace_session::WorkspaceConnection>,
    connection: &Arc<remote_core::workspace_session::WorkspaceConnection>,
) -> bool {
    target
        .upgrade()
        .is_some_and(|target| Arc::ptr_eq(&target, connection))
}

/// Both launch modes must own the same profile lock before joining the network.
pub struct HeadlessInstanceGuard {
    _instance: instance::Instance,
}

pub fn claim_headless_runtime(
    directory: &std::path::Path,
) -> Result<Option<HeadlessInstanceGuard>, String> {
    Ok(
        instance::Instance::acquire_headless(directory)?.map(|instance| HeadlessInstanceGuard {
            _instance: instance,
        }),
    )
}
