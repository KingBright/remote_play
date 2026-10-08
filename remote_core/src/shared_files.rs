//! Explicitly published open file handles. Remote callers never supply local paths.
//! Publications last for this process and device-group key; clearing prevents new pulls.
use protocol::shared_files::{MAX_SHARED_NAME_BYTES, SHARED_PAGE_SIZE, SharedFileInfo};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::fs::File;
pub type ShareScope = Option<[u8; 32]>;
pub fn current_share_scope() -> ShareScope {
    crate::session_crypto::load_session_psk().map(|key| Sha256::digest(&key).into())
}
#[derive(Debug)]
pub struct SharedFile {
    pub info: SharedFileInfo,
    pub file: Arc<tokio::sync::Mutex<File>>,
    scope: ShareScope,
}
#[derive(Debug, Default)]
pub struct SharedFileCatalog {
    entries: Mutex<BTreeMap<u64, Arc<SharedFile>>>,
}
static CATALOG: OnceLock<Arc<SharedFileCatalog>> = OnceLock::new();
pub fn shared_file_catalog() -> Arc<SharedFileCatalog> {
    CATALOG
        .get_or_init(|| Arc::new(SharedFileCatalog::default()))
        .clone()
}
impl SharedFileCatalog {
    pub async fn publish(&self, path: &Path, scope: ShareScope) -> Result<SharedFileInfo, String> {
        if scope.is_none() {
            return Err("Join a device network before sharing files".into());
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= MAX_SHARED_NAME_BYTES
                    && *s != "."
                    && *s != ".."
                    && !s
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            })
            .ok_or("Unsupported shared file name")?
            .to_string();
        // Open once at explicit local selection. A later path/symlink replacement
        // cannot redirect a remote fetch to another file.
        let file = File::open(path)
            .await
            .map_err(|_| "Cannot open selected file")?;
        let metadata = file
            .metadata()
            .await
            .map_err(|_| "Cannot inspect selected file")?;
        if !metadata.is_file() {
            return Err("Select a regular file, not a directory".into());
        }
        if metadata.len() > crate::file_transfer::FileTransferPolicy::default().max_file_bytes {
            return Err("Selected file exceeds the transfer limit".into());
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, e| e.scope == scope);
        if entries.len() >= 64 {
            return Err("Clear shared files before publishing more than 64 files".into());
        }
        let id = loop {
            let id = rand::random::<u64>() & i64::MAX as u64;
            if id != 0 && !entries.contains_key(&id) {
                break id;
            }
        };
        let info = SharedFileInfo {
            id,
            name,
            size_bytes: metadata.len(),
        };
        entries.insert(
            id,
            Arc::new(SharedFile {
                info: info.clone(),
                file: Arc::new(tokio::sync::Mutex::new(file)),
                scope,
            }),
        );
        Ok(info)
    }
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
    pub fn page(&self, scope: ShareScope, after: u64) -> (Vec<SharedFileInfo>, Option<u64>) {
        let entries = self.entries.lock().unwrap();
        let mut matches = entries
            .values()
            .filter(|f| f.scope == scope && scope.is_some() && f.info.id > after);
        let page: Vec<_> = matches
            .by_ref()
            .take(SHARED_PAGE_SIZE)
            .map(|f| f.info.clone())
            .collect();
        let next = matches.next().and_then(|_| page.last().map(|f| f.id));
        (page, next)
    }
    pub fn get(&self, scope: ShareScope, id: u64) -> Option<Arc<SharedFile>> {
        self.entries
            .lock()
            .unwrap()
            .get(&id)
            .filter(|f| scope.is_some() && f.scope == scope)
            .cloned()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn only_explicit_files_are_listed_and_scopes_are_isolated() {
        let dir = std::env::temp_dir().join(format!("rp-shared-{}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("selected.txt");
        std::fs::write(&path, b"selected").unwrap();
        let catalog = SharedFileCatalog::default();
        let scope = Some([7; 32]);
        assert!(catalog.page(scope, 0).0.is_empty());
        assert!(catalog.publish(&path, None).await.is_err());
        let info = catalog.publish(&path, scope).await.unwrap();
        assert_eq!(catalog.page(scope, 0).0, vec![info.clone()]);
        assert!(catalog.get(Some([8; 32]), info.id).is_none());
        catalog.clear();
        assert!(catalog.get(scope, info.id).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn held_file_does_not_follow_replaced_path() {
        use tokio::io::AsyncReadExt;
        let dir = std::env::temp_dir().join(format!("rp-pinned-{}", rand::random::<u64>()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("selected.txt");
        std::fs::write(&path, b"original").unwrap();
        let catalog = SharedFileCatalog::default();
        let scope = Some([9; 32]);
        let info = catalog.publish(&path, scope).await.unwrap();
        std::fs::rename(&path, dir.join("original.txt")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        let entry = catalog.get(scope, info.id).unwrap();
        let mut file = entry.file.lock().await;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"original");
        drop(file);
        drop(entry);
        catalog.clear();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
