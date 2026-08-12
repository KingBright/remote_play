use async_trait::async_trait;
use objc::runtime::{BOOL, NO, Object};
use objc::{class, msg_send, sel, sel_impl};
use protocol::{ClipboardBundle, ClipboardImage, ClipboardItem, ClipboardText};
use remote_core::clipboard_plane::{ClipboardSyncPolicy, validate_clipboard_bundle};
use remote_core::{
    ClipboardBackendCapabilities, ClipboardFileReference, ClipboardFileReferenceProvider,
    ClipboardProvider, PlatformKind,
};
use std::error::Error;
use std::ffi::CStr;
use std::io;
use std::os::raw::c_void;
use std::path::PathBuf;
use std::slice;
use std::time::{SystemTime, UNIX_EPOCH};

const NS_UTF8_STRING_ENCODING: usize = 4;
const NSPASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";
const NSPASTEBOARD_TYPE_PNG: &str = "public.png";
const NSPASTEBOARD_TYPE_TIFF: &str = "public.tiff";
const NSPASTEBOARD_TYPE_FILE_URL: &str = "public.file-url";
const MIME_PNG: &str = "image/png";
const MIME_TIFF: &str = "image/tiff";

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

pub struct MacClipboardProvider;

impl MacClipboardProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacClipboardProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClipboardProvider for MacClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Macos
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
        with_autorelease_pool(|| read_clipboard_bundle(policy))
    }

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        validate_clipboard_bundle(bundle)?;
        policy.validate_bundle(bundle)?;
        with_autorelease_pool(|| write_clipboard_bundle(bundle))
    }
}

#[async_trait]
impl ClipboardFileReferenceProvider for MacClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::Macos
    }

    async fn read_clipboard_file_references(
        &mut self,
    ) -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>> {
        with_autorelease_pool(read_file_references)
    }

    async fn write_clipboard_file_references(
        &mut self,
        references: &[ClipboardFileReference],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        with_autorelease_pool(|| write_file_references(references))
    }
}

fn read_clipboard_bundle(
    policy: ClipboardSyncPolicy,
) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>> {
    let mut items = Vec::new();

    unsafe {
        let pasteboard = general_pasteboard()?;

        if policy.allow_text
            && let Some(text) = read_string_for_type(pasteboard, NSPASTEBOARD_TYPE_STRING)?
        {
            items.push(ClipboardItem::Text(ClipboardText { text }));
        }

        if policy.allow_images
            && let Some(image) = read_first_image(pasteboard)?
        {
            items.push(ClipboardItem::Image(image));
        }
    }

    if items.is_empty() {
        return Ok(None);
    }

    let bundle = ClipboardBundle::new(next_bundle_id(), items);
    validate_clipboard_bundle(&bundle)?;
    policy.validate_bundle(&bundle)?;
    Ok(Some(bundle))
}

fn write_clipboard_bundle(bundle: &ClipboardBundle) -> Result<(), Box<dyn Error + Send + Sync>> {
    unsafe {
        let pasteboard = general_pasteboard()?;
        let _: isize = msg_send![pasteboard, clearContents];
        let mut wrote_any = false;

        for item in &bundle.items {
            match item {
                ClipboardItem::Text(text) => {
                    write_string_for_type(pasteboard, &text.text, NSPASTEBOARD_TYPE_STRING)?;
                    wrote_any = true;
                }
                ClipboardItem::Image(image) => {
                    write_image(pasteboard, image)?;
                    wrote_any = true;
                }
                ClipboardItem::File(_) => {
                    return Err(Box::new(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "macOS clipboard provider does not write file-byte clipboard items yet",
                    )));
                }
            }
        }

        if !wrote_any {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                "clipboard bundle did not contain writable macOS items",
            )));
        }
    }

    Ok(())
}

fn read_file_references() -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>> {
    let mut references = Vec::new();

    unsafe {
        let pasteboard = general_pasteboard()?;
        let items: *mut Object = msg_send![pasteboard, pasteboardItems];
        if items.is_null() {
            return Ok(references);
        }

        let count: usize = msg_send![items, count];
        for index in 0..count {
            let item: *mut Object = msg_send![items, objectAtIndex: index];
            if item.is_null() {
                continue;
            }

            if let Some(path) = read_file_reference_from_item(item)? {
                references.push(ClipboardFileReference::new(path));
            }
        }
    }

    Ok(references)
}

fn write_file_references(
    references: &[ClipboardFileReference],
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if references.is_empty() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clipboard file references cannot be empty",
        )));
    }

    unsafe {
        let pasteboard = general_pasteboard()?;
        let _: isize = msg_send![pasteboard, clearContents];
        let array: *mut Object =
            msg_send![class!(NSMutableArray), arrayWithCapacity: references.len()];
        if array.is_null() {
            return Err(Box::new(io::Error::other(
                "NSMutableArray arrayWithCapacity returned null",
            )));
        }

        for reference in references {
            let Some(path) = reference.path.to_str() else {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "clipboard file reference path is not valid UTF-8",
                )));
            };
            let ns_path = nsstring(path)?;
            let url: *mut Object = msg_send![class!(NSURL), fileURLWithPath: ns_path];
            if url.is_null() {
                return Err(Box::new(io::Error::other(
                    "NSURL fileURLWithPath returned null",
                )));
            }
            let _: () = msg_send![array, addObject: url];
        }

        let ok: BOOL = msg_send![pasteboard, writeObjects: array];
        if ok == NO {
            return Err(Box::new(io::Error::other(
                "NSPasteboard writeObjects failed for file references",
            )));
        }
    }

    Ok(())
}

unsafe fn general_pasteboard() -> Result<*mut Object, Box<dyn Error + Send + Sync>> {
    let pasteboard: *mut Object = unsafe { msg_send![class!(NSPasteboard), generalPasteboard] };
    if pasteboard.is_null() {
        return Err(Box::new(io::Error::other(
            "NSPasteboard generalPasteboard returned null",
        )));
    }
    Ok(pasteboard)
}

unsafe fn read_file_reference_from_item(
    item: *mut Object,
) -> Result<Option<PathBuf>, Box<dyn Error + Send + Sync>> {
    let ty = unsafe { nsstring(NSPASTEBOARD_TYPE_FILE_URL)? };
    let value: *mut Object = unsafe { msg_send![item, stringForType: ty] };
    if value.is_null() {
        return Ok(None);
    }

    let url_string = unsafe { nsstring_to_string(value)? };
    let Some(path) = file_url_string_to_path(&url_string)? else {
        return Ok(None);
    };
    Ok(Some(path))
}

fn file_url_string_to_path(
    url_string: &str,
) -> Result<Option<PathBuf>, Box<dyn Error + Send + Sync>> {
    unsafe {
        let ns_url_string = nsstring(url_string)?;
        let url: *mut Object = msg_send![class!(NSURL), URLWithString: ns_url_string];
        if url.is_null() {
            return Ok(None);
        }

        let is_file_url: BOOL = msg_send![url, isFileURL];
        if is_file_url == NO {
            return Ok(None);
        }

        let path: *mut Object = msg_send![url, path];
        if path.is_null() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(nsstring_to_string(path)?)))
    }
}

unsafe fn read_string_for_type(
    pasteboard: *mut Object,
    pasteboard_type: &str,
) -> Result<Option<String>, Box<dyn Error + Send + Sync>> {
    let ty = unsafe { nsstring(pasteboard_type)? };
    let value: *mut Object = unsafe { msg_send![pasteboard, stringForType: ty] };
    if value.is_null() {
        return Ok(None);
    }
    unsafe { nsstring_to_string(value) }.map(Some)
}

unsafe fn read_first_image(
    pasteboard: *mut Object,
) -> Result<Option<ClipboardImage>, Box<dyn Error + Send + Sync>> {
    for (pasteboard_type, mime_type) in [
        (NSPASTEBOARD_TYPE_PNG, MIME_PNG),
        (NSPASTEBOARD_TYPE_TIFF, MIME_TIFF),
    ] {
        if let Some(bytes) = unsafe { read_data_for_type(pasteboard, pasteboard_type)? } {
            return Ok(Some(ClipboardImage {
                mime_type: mime_type.to_string(),
                width: None,
                height: None,
                bytes,
            }));
        }
    }

    Ok(None)
}

unsafe fn read_data_for_type(
    pasteboard: *mut Object,
    pasteboard_type: &str,
) -> Result<Option<Vec<u8>>, Box<dyn Error + Send + Sync>> {
    let ty = unsafe { nsstring(pasteboard_type)? };
    let data: *mut Object = unsafe { msg_send![pasteboard, dataForType: ty] };
    if data.is_null() {
        return Ok(None);
    }

    let len: usize = unsafe { msg_send![data, length] };
    let bytes: *const u8 = unsafe { msg_send![data, bytes] };
    if bytes.is_null() && len > 0 {
        return Err(Box::new(io::Error::other("NSData bytes returned null")));
    }

    Ok(Some(unsafe { slice::from_raw_parts(bytes, len) }.to_vec()))
}

unsafe fn write_string_for_type(
    pasteboard: *mut Object,
    value: &str,
    pasteboard_type: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let ns_value = unsafe { nsstring(value)? };
    let ty = unsafe { nsstring(pasteboard_type)? };
    let ok: BOOL = unsafe { msg_send![pasteboard, setString: ns_value forType: ty] };
    if ok == NO {
        return Err(Box::new(io::Error::other(
            "NSPasteboard setString:forType: failed",
        )));
    }

    Ok(())
}

unsafe fn write_image(
    pasteboard: *mut Object,
    image: &ClipboardImage,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let pasteboard_type = image_mime_to_pasteboard_type(&image.mime_type).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "macOS clipboard provider currently writes PNG or TIFF images only",
        )
    })?;
    let data = unsafe { nsdata(&image.bytes)? };
    let ty = unsafe { nsstring(pasteboard_type)? };
    let ok: BOOL = unsafe { msg_send![pasteboard, setData: data forType: ty] };
    if ok == NO {
        return Err(Box::new(io::Error::other(
            "NSPasteboard setData:forType: failed",
        )));
    }

    Ok(())
}

fn image_mime_to_pasteboard_type(mime_type: &str) -> Option<&'static str> {
    match mime_type {
        MIME_PNG => Some(NSPASTEBOARD_TYPE_PNG),
        MIME_TIFF | "image/x-tiff" => Some(NSPASTEBOARD_TYPE_TIFF),
        _ => None,
    }
}

unsafe fn nsstring(value: &str) -> Result<*mut Object, Box<dyn Error + Send + Sync>> {
    let string: *mut Object = unsafe { msg_send![class!(NSString), alloc] };
    let string: *mut Object = unsafe {
        msg_send![
            string,
            initWithBytes: value.as_ptr()
            length: value.len()
            encoding: NS_UTF8_STRING_ENCODING
        ]
    };
    if string.is_null() {
        return Err(Box::new(io::Error::other("NSString allocation failed")));
    }
    let _: *mut Object = unsafe { msg_send![string, autorelease] };
    Ok(string)
}

unsafe fn nsstring_to_string(value: *mut Object) -> Result<String, Box<dyn Error + Send + Sync>> {
    let c_str: *const i8 = unsafe { msg_send![value, UTF8String] };
    if c_str.is_null() {
        return Err(Box::new(io::Error::other("NSString UTF8String was null")));
    }
    Ok(unsafe { CStr::from_ptr(c_str) }
        .to_string_lossy()
        .into_owned())
}

unsafe fn nsdata(bytes: &[u8]) -> Result<*mut Object, Box<dyn Error + Send + Sync>> {
    let data: *mut Object = unsafe {
        msg_send![
            class!(NSData),
            dataWithBytes: bytes.as_ptr()
            length: bytes.len()
        ]
    };
    if data.is_null() {
        return Err(Box::new(io::Error::other("NSData allocation failed")));
    }
    Ok(data)
}

fn with_autorelease_pool<T>(
    f: impl FnOnce() -> Result<T, Box<dyn Error + Send + Sync>>,
) -> Result<T, Box<dyn Error + Send + Sync>> {
    unsafe {
        let pool = objc::runtime::objc_autoreleasePoolPush();
        let result = f();
        objc::runtime::objc_autoreleasePoolPop(pool as *mut c_void);
        result
    }
}

fn next_bundle_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use remote_core::{ClipboardFileReferenceProvider, ClipboardProvider};

    #[test]
    fn maps_supported_image_mime_types_to_pasteboard_types() {
        assert_eq!(
            image_mime_to_pasteboard_type("image/png"),
            Some(NSPASTEBOARD_TYPE_PNG)
        );
        assert_eq!(
            image_mime_to_pasteboard_type("image/tiff"),
            Some(NSPASTEBOARD_TYPE_TIFF)
        );
        assert_eq!(
            image_mime_to_pasteboard_type("image/x-tiff"),
            Some(NSPASTEBOARD_TYPE_TIFF)
        );
        assert_eq!(image_mime_to_pasteboard_type("image/jpeg"), None);
    }

    #[test]
    fn reports_macos_clipboard_capabilities() {
        let provider = MacClipboardProvider::new();
        let capabilities = provider.capabilities();

        assert_eq!(ClipboardProvider::platform(&provider), PlatformKind::Macos);
        assert_eq!(
            ClipboardFileReferenceProvider::platform(&provider),
            PlatformKind::Macos
        );
        assert!(capabilities.text);
        assert!(capabilities.image);
        assert!(capabilities.file_references);
        assert!(!capabilities.file_bytes);
    }
}
