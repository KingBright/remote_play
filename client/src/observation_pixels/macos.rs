//! Snapshot-only Core Image bridge. The caller retains the decoded frame.
use super::{ColorMetadata, CpuRgba, PixelError, RgbaLayout};
use std::{ffi::c_void, sync::Arc};

unsafe extern "C" {
    fn weavelo_observation_rgba(
        buffer: *mut c_void,
        width: usize,
        height: usize,
        output: *mut u8,
        length: usize,
    ) -> i32;
}

/// # Safety
/// `buffer` must be null or a live immutable CVPixelBuffer owned by the caller
/// throughout this call. Native completion is synchronous, including GPU work.
pub(crate) unsafe fn copy_pixel_buffer(
    buffer: *mut c_void,
    width: u32,
    height: u32,
    maximum: usize,
) -> Result<CpuRgba, PixelError> {
    let row = (width as usize)
        .checked_mul(4)
        .ok_or(PixelError::ArithmeticOverflow)?;
    let bytes = row
        .checked_mul(height as usize)
        .ok_or(PixelError::ArithmeticOverflow)?;
    let layout = RgbaLayout::validate(width, height, row, bytes, maximum)?;
    let mut pixels = vec![0; layout.bytes()];
    let status = unsafe {
        weavelo_observation_rgba(
            buffer,
            width as usize,
            height as usize,
            pixels.as_mut_ptr(),
            pixels.len(),
        )
    };
    match status {
        0 => Ok(CpuRgba {
            layout,
            pixels: Arc::new(pixels),
            color: ColorMetadata::Srgb,
        }),
        1 => Err(PixelError::NativeReadbackUnsupported),
        2 => Err(PixelError::ColorMetadataUnavailable),
        _ => Err(PixelError::NativeReadbackFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_foundation::{base::TCFType, dictionary::CFDictionary, string::CFString};
    use std::{ffi::c_void, ptr};
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVPixelBufferCreate(
            allocator: *const c_void,
            width: usize,
            height: usize,
            format: u32,
            attributes: *const c_void,
            output: *mut *mut c_void,
        ) -> i32;
        fn CVPixelBufferRelease(buffer: *mut c_void);
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *const u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;
        fn CVBufferSetAttachment(
            buffer: *mut c_void,
            key: *const c_void,
            value: *const c_void,
            mode: u32,
        );
        fn CVBufferCopyAttachment(
            buffer: *mut c_void,
            key: *const c_void,
            mode: *mut u32,
        ) -> *const c_void;
        static kCVImageBufferYCbCrMatrixKey: *const c_void;
        static kCVImageBufferYCbCrMatrix_ITU_R_709_2: *const c_void;
        static kCVImageBufferColorPrimariesKey: *const c_void;
        static kCVImageBufferColorPrimaries_ITU_R_709_2: *const c_void;
        static kCVImageBufferTransferFunctionKey: *const c_void;
        static kCVImageBufferTransferFunction_ITU_R_709_2: *const c_void;
        static kCVPixelBufferIOSurfacePropertiesKey: *const c_void;
    }
    struct Buffer(*mut c_void);
    impl Drop for Buffer {
        fn drop(&mut self) {
            unsafe { CVPixelBufferRelease(self.0) }
        }
    }
    // Actual Core Video buffers with synthetic pixels, never screen capture.
    fn fixture(video_range: bool, metadata: bool) -> Buffer {
        unsafe {
            let mut b = ptr::null_mut();
            let format = if video_range {
                u32::from_be_bytes(*b"420v")
            } else {
                u32::from_be_bytes(*b"420f")
            };
            let key = CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey as _);
            let empty = CFDictionary::<CFString, CFString>::from_CFType_pairs(&[]);
            let attrs = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), empty.as_CFType())]);
            assert_eq!(
                CVPixelBufferCreate(
                    ptr::null(),
                    4,
                    4,
                    format,
                    attrs.as_concrete_TypeRef() as _,
                    &mut b
                ),
                0
            );
            let buffer = Buffer(b);
            assert!(
                !CVPixelBufferGetIOSurface(b).is_null(),
                "fixture must use real IOSurface backing"
            );
            assert_eq!(CVPixelBufferLockBaseAddress(b, 0), 0);
            let y = CVPixelBufferGetBaseAddressOfPlane(b, 0).cast_mut();
            let uv = CVPixelBufferGetBaseAddressOfPlane(b, 1).cast_mut();
            let ys = CVPixelBufferGetBytesPerRowOfPlane(b, 0);
            let uvs = CVPixelBufferGetBytesPerRowOfPlane(b, 1);
            // Distinct top and bottom test the returned row orientation.
            for row in 0..4 {
                for col in 0..4 {
                    *y.add(row * ys + col) = if row < 2 {
                        if video_range { 16 } else { 0 }
                    } else {
                        if video_range { 235 } else { 255 }
                    };
                }
            }
            for row in 0..2 {
                for col in 0..4 {
                    *uv.add(row * uvs + col) = 128;
                }
            }
            assert_eq!(CVPixelBufferUnlockBaseAddress(b, 0), 0);
            if metadata {
                for (key, value) in [
                    (
                        kCVImageBufferYCbCrMatrixKey,
                        kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                    ),
                    (
                        kCVImageBufferColorPrimariesKey,
                        kCVImageBufferColorPrimaries_ITU_R_709_2,
                    ),
                    (
                        kCVImageBufferTransferFunctionKey,
                        kCVImageBufferTransferFunction_ITU_R_709_2,
                    ),
                ] {
                    CVBufferSetAttachment(b, key, value, 1);
                }
            }
            buffer
        }
    }
    #[test]
    fn native_full_and_video_range_readback_has_top_left_rows_and_opaque_srgb() {
        for video_range in [false, true] {
            let b = fixture(video_range, true);
            let image = unsafe { copy_pixel_buffer(b.0, 4, 4, 64) }.unwrap();
            assert_eq!(image.color(), ColorMetadata::Srgb);
            assert_eq!(image.layout().bytes(), 64);
            for (index, pixel) in image.pixels().chunks_exact(4).enumerate() {
                if index < 8 {
                    assert!(pixel[..3].iter().all(|v| *v <= 2), "top pixel {pixel:?}");
                } else {
                    assert!(
                        pixel[..3].iter().all(|v| *v >= 253),
                        "bottom pixel {pixel:?}"
                    );
                }
                assert_eq!(pixel[3], 255);
            }
        }
    }
    #[test]
    fn missing_color_is_an_error_without_guessed_matrix() {
        let b = fixture(false, false);
        assert_eq!(
            unsafe { copy_pixel_buffer(b.0, 4, 4, 64) }.err(),
            Some(PixelError::ColorMetadataUnavailable)
        );
    }
    #[test]
    fn invalid_matrix_is_rejected_instead_of_native_defaulting() {
        let b = fixture(false, false);
        let unknown = CFString::new("weavelo-test-unknown-matrix");
        unsafe {
            CVBufferSetAttachment(
                b.0,
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
                1,
            );
            CVBufferSetAttachment(
                b.0,
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_ITU_R_709_2,
                1,
            );
            CVBufferSetAttachment(
                b.0,
                kCVImageBufferYCbCrMatrixKey,
                unknown.as_concrete_TypeRef() as _,
                1,
            )
        };
        let raw =
            unsafe { CVBufferCopyAttachment(b.0, kCVImageBufferYCbCrMatrixKey, ptr::null_mut()) };
        // Core Video may reject an invalid attachment at insertion. It must
        // never silently supply a valid matrix in this incomplete-color fixture.
        if !raw.is_null() {
            let attached = unsafe { CFString::wrap_under_create_rule(raw as _) };
            assert_eq!(attached.to_string(), unknown.to_string());
        }
        assert_eq!(
            unsafe { copy_pixel_buffer(b.0, 4, 4, 64) }.err(),
            Some(PixelError::ColorMetadataUnavailable)
        );
    }
    #[test]
    fn iosurface_chroma_is_rgba_not_bgra() {
        let b = fixture(false, true);
        unsafe {
            assert_eq!(CVPixelBufferLockBaseAddress(b.0, 0), 0);
            let y = CVPixelBufferGetBaseAddressOfPlane(b.0, 0).cast_mut();
            let uv = CVPixelBufferGetBaseAddressOfPlane(b.0, 1).cast_mut();
            let ys = CVPixelBufferGetBytesPerRowOfPlane(b.0, 0);
            let uvs = CVPixelBufferGetBytesPerRowOfPlane(b.0, 1);
            for row in 0..4 {
                for col in 0..4 {
                    *y.add(row * ys + col) = 128;
                }
            }
            for row in 0..2 {
                for col in 0..2 {
                    *uv.add(row * uvs + col * 2) = 64;
                    *uv.add(row * uvs + col * 2 + 1) = 192;
                }
            }
            assert_eq!(CVPixelBufferUnlockBaseAddress(b.0, 0), 0);
        }
        let image = unsafe { copy_pixel_buffer(b.0, 4, 4, 64) }.unwrap();
        for p in image.pixels().chunks_exact(4) {
            assert!(
                u16::from(p[0]) > u16::from(p[1]) + 40 && u16::from(p[1]) > u16::from(p[2]) + 40,
                "RGBA pixel {p:?}"
            );
            assert_eq!(p[3], 255);
        }
    }
    #[test]
    fn native_buffer_geometry_null_and_caller_budget_are_rejected() {
        let b = fixture(false, true);
        assert_eq!(
            unsafe { copy_pixel_buffer(b.0, 4, 4, 63) }.err(),
            Some(PixelError::SizeLimit)
        );
        assert_eq!(
            unsafe { copy_pixel_buffer(b.0, 2, 4, 64) }.err(),
            Some(PixelError::NativeReadbackUnsupported)
        );
        assert_eq!(
            unsafe { copy_pixel_buffer(ptr::null_mut(), 4, 4, 64) }.err(),
            Some(PixelError::NativeReadbackUnsupported)
        );
    }
}
