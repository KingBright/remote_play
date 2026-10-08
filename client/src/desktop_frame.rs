//! Renderer-neutral frame adapter. Only this boundary knows about native buffers.
use std::sync::Arc;
#[derive(Clone)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
    pub decoded_at: std::time::Instant,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn rgba(frame: &crate::MacDecodedVideoFrame) -> Result<RgbaFrame, String> {
    #[cfg(all(target_os = "windows", feature = "native-windows-video"))]
    if frame.native.is_some() {
        return Err("Native video requires its GPU presenter; no CPU readback fallback".into());
    }
    Ok(RgbaFrame {
        width: frame.width,
        height: frame.height,
        pixels: frame.rgba.clone(),
        decoded_at: frame.decoded_at,
    })
}

#[cfg(target_os = "macos")]
pub fn rgba(frame: &crate::MacDecodedVideoFrame) -> Result<RgbaFrame, String> {
    use remote_core::VideoFrame;
    use std::ffi::c_void;
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
        fn CVPixelBufferGetBaseAddressOfPlane(buffer: *mut c_void, plane: usize) -> *const u8;
        fn CVPixelBufferGetBytesPerRowOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetWidthOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetHeightOfPlane(buffer: *mut c_void, plane: usize) -> usize;
        fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
    }
    struct Unlock(*mut c_void);
    impl Drop for Unlock {
        fn drop(&mut self) {
            unsafe {
                CVPixelBufferUnlockBaseAddress(self.0, 1);
            }
        }
    }
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|n| n > 4096 * 2160) {
        return Err("frame dimensions exceed desktop renderer budget".into());
    }
    unsafe {
        let b = frame.cv_pixel_buffer;
        if b.is_null()
            || CVPixelBufferGetPixelFormatType(b) != 875704422
            || CVPixelBufferLockBaseAddress(b, 1) != 0
        {
            return Err("VideoToolbox NV12 frame is unavailable".into());
        }
        let _unlock = Unlock(b);
        let yp = CVPixelBufferGetBaseAddressOfPlane(b, 0);
        let uvp = CVPixelBufferGetBaseAddressOfPlane(b, 1);
        let ys = CVPixelBufferGetBytesPerRowOfPlane(b, 0);
        let uvs = CVPixelBufferGetBytesPerRowOfPlane(b, 1);
        if yp.is_null()
            || uvp.is_null()
            || ys < w
            || uvs < w
            || CVPixelBufferGetWidthOfPlane(b, 0) < w
            || CVPixelBufferGetHeightOfPlane(b, 0) < h
            || CVPixelBufferGetHeightOfPlane(b, 1) < h.div_ceil(2)
        {
            return Err("invalid native plane geometry".into());
        }
        let mut pixels = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let l = *yp.add(y * ys + x) as f32;
                let u = *uvp.add((y / 2) * uvs + (x / 2) * 2) as f32 - 128.;
                let v = *uvp.add((y / 2) * uvs + (x / 2) * 2 + 1) as f32 - 128.;
                let i = (y * w + x) * 4;
                pixels[i] = (l + 1.5748 * v).clamp(0., 255.) as u8;
                pixels[i + 1] = (l - 0.1873 * u - 0.4681 * v).clamp(0., 255.) as u8;
                pixels[i + 2] = (l + 1.8556 * u).clamp(0., 255.) as u8;
                pixels[i + 3] = 255;
            }
        }
        Ok(RgbaFrame {
            width: w as u32,
            height: h as u32,
            pixels: Arc::new(pixels),
            decoded_at: frame.decoded_at,
        })
    }
}
