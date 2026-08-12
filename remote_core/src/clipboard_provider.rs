use crate::clipboard_plane::{ClipboardPlaneError, ClipboardSyncPolicy, validate_clipboard_bundle};
use crate::traits::{
    ClipboardBackendCapabilities, ClipboardFileReference, ClipboardFileReferenceProvider,
    ClipboardFileStore, ClipboardProvider, PlatformKind,
};
use async_trait::async_trait;
use protocol::{ClipboardBundle, ClipboardFile};
use std::error::Error;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct MemoryClipboardProvider {
    current: Option<ClipboardBundle>,
}

impl MemoryClipboardProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bundle(bundle: ClipboardBundle) -> Self {
        Self {
            current: Some(bundle),
        }
    }

    pub fn clear(&mut self) {
        self.current = None;
    }
}

#[async_trait]
impl ClipboardProvider for MemoryClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Other
    }

    fn capabilities(&self) -> ClipboardBackendCapabilities {
        ClipboardBackendCapabilities {
            text: true,
            image: true,
            file_references: false,
            file_bytes: true,
        }
    }

    async fn read_clipboard(
        &mut self,
        policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
        let Some(bundle) = self.current.clone() else {
            return Ok(None);
        };

        validate_clipboard_bundle_for_policy(&bundle, policy)?;
        Ok(Some(bundle))
    }

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        validate_clipboard_bundle_for_policy(bundle, policy)?;
        self.current = Some(bundle.clone());
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryClipboardFileReferenceProvider {
    current: Vec<ClipboardFileReference>,
}

impl MemoryClipboardFileReferenceProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_references(current: Vec<ClipboardFileReference>) -> Self {
        Self { current }
    }

    pub fn references(&self) -> &[ClipboardFileReference] {
        &self.current
    }
}

#[async_trait]
impl ClipboardFileReferenceProvider for MemoryClipboardFileReferenceProvider {
    async fn read_clipboard_file_references(
        &mut self,
    ) -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>> {
        Ok(self.current.clone())
    }

    async fn write_clipboard_file_references(
        &mut self,
        references: &[ClipboardFileReference],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.current = references.to_vec();
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FilesystemClipboardFileStore;

#[async_trait]
impl ClipboardFileStore for FilesystemClipboardFileStore {
    async fn load_clipboard_file(
        &self,
        path: &Path,
    ) -> Result<ClipboardFile, Box<dyn Error + Send + Sync>> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| is_safe_clipboard_file_name(name))
            .ok_or_else(|| invalid_input("clipboard file path does not have a safe file name"))?
            .to_string();
        let bytes = tokio::fs::read(path).await?;

        Ok(ClipboardFile {
            name,
            mime_type: None,
            bytes,
        })
    }

    async fn materialize_clipboard_file(
        &self,
        file: &ClipboardFile,
        target_dir: &Path,
    ) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
        if !is_safe_clipboard_file_name(&file.name) {
            return Err(Box::new(invalid_input(
                "clipboard file name is empty or contains path components",
            )));
        }

        tokio::fs::create_dir_all(target_dir).await?;
        let target = target_dir.join(&file.name);
        tokio::fs::write(&target, &file.bytes).await?;
        Ok(target)
    }
}

fn validate_clipboard_bundle_for_policy(
    bundle: &ClipboardBundle,
    policy: ClipboardSyncPolicy,
) -> Result<(), ClipboardPlaneError> {
    validate_clipboard_bundle(bundle)?;
    policy.validate_bundle(bundle)
}

fn is_safe_clipboard_file_name(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard_plane::ClipboardItemClass;
    use crate::traits::{ClipboardFileReferenceProvider, ClipboardProvider};
    use protocol::{ClipboardImage, ClipboardItem, ClipboardText};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn text_bundle() -> ClipboardBundle {
        ClipboardBundle::new(
            1,
            vec![ClipboardItem::Text(ClipboardText {
                text: "hello".to_string(),
            })],
        )
    }

    fn file_bundle() -> ClipboardBundle {
        ClipboardBundle::new(
            2,
            vec![ClipboardItem::File(ClipboardFile {
                name: "clip.txt".to_string(),
                mime_type: Some("text/plain".to_string()),
                bytes: b"file bytes".to_vec(),
            })],
        )
    }

    #[tokio::test]
    async fn memory_clipboard_provider_roundtrips_text() {
        let bundle = text_bundle();
        let mut provider = MemoryClipboardProvider::new();

        provider
            .write_clipboard(&bundle, ClipboardSyncPolicy::default())
            .await
            .expect("text clipboard should write");
        let read = provider
            .read_clipboard(ClipboardSyncPolicy::default())
            .await
            .expect("text clipboard should read");

        assert_eq!(read, Some(bundle));
        assert_eq!(provider.platform(), PlatformKind::Other);
        assert!(provider.capabilities().text);
    }

    #[tokio::test]
    async fn memory_clipboard_provider_applies_file_policy() {
        let bundle = file_bundle();
        let mut provider = MemoryClipboardProvider::new();

        let err = provider
            .write_clipboard(&bundle, ClipboardSyncPolicy::default())
            .await
            .expect_err("file clipboard should require explicit policy");

        let plane_err = err
            .downcast_ref::<ClipboardPlaneError>()
            .expect("error should be a clipboard plane error");
        assert!(matches!(
            plane_err,
            ClipboardPlaneError::DisallowedItem {
                item_index: 0,
                item_class: ClipboardItemClass::File
            }
        ));
    }

    #[tokio::test]
    async fn memory_clipboard_provider_roundtrips_image() {
        let bundle = ClipboardBundle::new(
            3,
            vec![ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".to_string(),
                width: Some(1),
                height: Some(1),
                bytes: vec![0x89, b'P', b'N', b'G'],
            })],
        );
        let mut provider = MemoryClipboardProvider::with_bundle(bundle.clone());

        let read = provider
            .read_clipboard(ClipboardSyncPolicy::default())
            .await
            .expect("image clipboard should read");

        assert_eq!(read, Some(bundle));
    }

    #[tokio::test]
    async fn memory_clipboard_file_reference_provider_roundtrips_paths() {
        let refs = vec![ClipboardFileReference::new("/tmp/a.txt")];
        let mut provider = MemoryClipboardFileReferenceProvider::with_references(refs.clone());

        assert_eq!(
            provider
                .read_clipboard_file_references()
                .await
                .expect("file references should read"),
            refs
        );

        let updated = vec![ClipboardFileReference::new("/tmp/b.txt")];
        provider
            .write_clipboard_file_references(&updated)
            .await
            .expect("file references should write");
        assert_eq!(provider.references(), updated.as_slice());
    }

    #[tokio::test]
    async fn filesystem_clipboard_file_store_roundtrips_small_file() {
        let base = unique_temp_dir("remote-play-clipboard-store");
        let source_dir = base.join("source");
        let target_dir = base.join("target");
        tokio::fs::create_dir_all(&source_dir)
            .await
            .expect("source dir should be created");
        let source = source_dir.join("clip.txt");
        tokio::fs::write(&source, b"hello file")
            .await
            .expect("source file should be written");

        let store = FilesystemClipboardFileStore;
        let file = store
            .load_clipboard_file(&source)
            .await
            .expect("file should load");
        assert_eq!(file.name, "clip.txt");
        assert_eq!(file.bytes, b"hello file");

        let materialized = store
            .materialize_clipboard_file(&file, &target_dir)
            .await
            .expect("file should materialize");
        let bytes = tokio::fs::read(&materialized)
            .await
            .expect("materialized file should be readable");
        assert_eq!(bytes, b"hello file");

        let _ = tokio::fs::remove_dir_all(&base).await;
    }

    #[tokio::test]
    async fn filesystem_clipboard_file_store_rejects_path_components() {
        let store = FilesystemClipboardFileStore;
        let file = ClipboardFile {
            name: "../escape.txt".to_string(),
            mime_type: None,
            bytes: b"nope".to_vec(),
        };

        let err = store
            .materialize_clipboard_file(&file, &unique_temp_dir("remote-play-clipboard-store"))
            .await
            .expect_err("unsafe file name should be rejected");

        assert_eq!(
            err.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::InvalidInput)
        );
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }
}
