use async_trait::async_trait;
use protocol::{ClipboardBundle, ClipboardImage, ClipboardItem, ClipboardText};
use remote_core::clipboard_plane::{ClipboardSyncPolicy, validate_clipboard_bundle};
use remote_core::{
    ClipboardBackendCapabilities, ClipboardFileReference, ClipboardFileReferenceProvider,
    ClipboardProvider, PlatformKind,
};
use std::{borrow::Cow, error::Error, io::Cursor};

#[derive(Default)]
pub struct WindowsClipboardProvider;

impl WindowsClipboardProvider {
    pub fn new() -> Self {
        Self
    }
}

fn available<T>(result: Result<T, arboard::Error>) -> Result<Option<T>, arboard::Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(arboard::Error::ContentNotAvailable | arboard::Error::ClipboardOccupied) => Ok(None),
        Err(error) => Err(error),
    }
}

#[async_trait]
impl ClipboardProvider for WindowsClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Windows
    }
    fn capabilities(&self) -> ClipboardBackendCapabilities {
        ClipboardBackendCapabilities {
            text: true,
            image: true,
            file_references: true,
            file_bytes: false,
        }
    }

    async fn read_clipboard(
        &mut self,
        policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
        let mut clipboard = arboard::Clipboard::new()?;
        let mut items = Vec::new();
        // Prefer the image over auxiliary text; each native write is atomic.
        if policy.allow_images
            && let Some(image) = available(clipboard.get_image())?
        {
            if image.bytes.len() > policy.max_image_bytes {
                return Err("clipboard image exceeds decoded byte limit".into());
            }
            let rgba = image::RgbaImage::from_raw(
                image.width.try_into()?,
                image.height.try_into()?,
                image.bytes.into_owned(),
            )
            .ok_or("invalid clipboard image dimensions")?;
            let mut encoded = Cursor::new(Vec::new());
            rgba.write_to(&mut encoded, image::ImageFormat::Png)?;
            items.push(ClipboardItem::Image(ClipboardImage {
                mime_type: "image/png".into(),
                width: Some(rgba.width()),
                height: Some(rgba.height()),
                bytes: encoded.into_inner(),
            }));
        } else if policy.allow_text
            && let Some(text) = available(clipboard.get_text())?
        {
            items.push(ClipboardItem::Text(ClipboardText { text }));
        }
        if items.is_empty() {
            return Ok(None);
        }
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;
        let bundle = ClipboardBundle::new(id, items);
        validate_clipboard_bundle(&bundle)?;
        policy.validate_bundle(&bundle)?;
        Ok(Some(bundle))
    }

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        validate_clipboard_bundle(bundle)?;
        policy.validate_bundle(bundle)?;
        let mut clipboard = arboard::Clipboard::new()?;
        if let Some(image) = bundle.items.iter().find_map(|item| {
            if let ClipboardItem::Image(image) = item {
                Some(image)
            } else {
                None
            }
        }) {
            let format = match image.mime_type.as_str() {
                "image/png" => image::ImageFormat::Png,
                "image/tiff" => image::ImageFormat::Tiff,
                _ => return Err("unsupported clipboard image format".into()),
            };
            let mut reader = image::ImageReader::with_format(Cursor::new(&image.bytes), format);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(16_384);
            limits.max_image_height = Some(16_384);
            limits.max_alloc = Some(policy.max_image_bytes as u64);
            reader.limits(limits);
            let rgba = reader.decode()?.into_rgba8();
            if rgba.len() > policy.max_image_bytes {
                return Err("clipboard image exceeds decoded byte limit".into());
            }
            clipboard.set_image(arboard::ImageData {
                width: rgba.width() as usize,
                height: rgba.height() as usize,
                bytes: Cow::Owned(rgba.into_raw()),
            })?;
        } else if let Some(text) = bundle.items.iter().find_map(|item| {
            if let ClipboardItem::Text(text) = item {
                Some(&text.text)
            } else {
                None
            }
        }) {
            clipboard.set_text(text)?;
        } else {
            return Err("clipboard bundle has no supported representation".into());
        }
        Ok(())
    }
}

#[async_trait]
impl ClipboardFileReferenceProvider for WindowsClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Windows
    }
    async fn read_clipboard_file_references(
        &mut self,
    ) -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>> {
        Ok(available(arboard::Clipboard::new()?.get().file_list())?
            .unwrap_or_default()
            .into_iter()
            .map(ClipboardFileReference::new)
            .collect())
    }
    async fn write_clipboard_file_references(
        &mut self,
        references: &[ClipboardFileReference],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let paths: Vec<_> = references.iter().map(|r| &r.path).collect();
        arboard::Clipboard::new()?.set().file_list(&paths)?;
        Ok(())
    }
}
