//! Preserve source color metadata in the compressed stream. This module never
//! converts pixels, guesses a matrix from dimensions, or relabels unknown data.
use core_foundation::{
    base::{CFType, CFTypeRef, TCFType},
    string::CFString,
};
use std::{error::Error, ffi::c_void};
use video_toolbox_sys::{
    compression::{
        VTCompressionSessionRef, kVTCompressionPropertyKey_ColorPrimaries,
        kVTCompressionPropertyKey_TransferFunction, kVTCompressionPropertyKey_YCbCrMatrix,
    },
    session::{VTSessionCopyProperty, VTSessionSetProperty},
};

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVBufferCopyAttachment(buffer: *const c_void, key: CFTypeRef, mode: *mut u32) -> CFTypeRef;
    static kCVImageBufferColorPrimariesKey: CFTypeRef;
    static kCVImageBufferTransferFunctionKey: CFTypeRef;
    static kCVImageBufferYCbCrMatrixKey: CFTypeRef;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SourceColor {
    pub(crate) primaries: Option<CFString>,
    pub(crate) transfer: Option<CFString>,
    pub(crate) matrix: Option<CFString>,
}
impl SourceColor {
    /// The caller owns a valid native buffer for the whole operation. Retain only
    /// three small native metadata objects, not a pixel view or image copy.
    pub(crate) unsafe fn from_pixel_buffer(
        buffer: *const c_void,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        if buffer.is_null() {
            return Err("Null capture buffer in color metadata".into());
        }
        let keys = unsafe {
            [
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferYCbCrMatrixKey,
            ]
        };
        let mut values = [None, None, None];
        for (slot, key) in values.iter_mut().zip(keys) {
            let raw = unsafe { CVBufferCopyAttachment(buffer, key, std::ptr::null_mut()) };
            if raw.is_null() {
                continue;
            }
            let value = unsafe { CFType::wrap_under_create_rule(raw) };
            *slot = Some(
                value
                    .downcast::<CFString>()
                    .ok_or("Capture color attachment has the wrong native type")?,
            );
        }
        let [primaries, transfer, matrix] = values;
        Ok(Self {
            primaries,
            transfer,
            matrix,
        })
    }
    /// A change affects only signaling for the exact source metadata. The caller
    /// requests a key frame after applying it, so new receivers get the new SPS.
    pub(crate) unsafe fn apply(
        &self,
        session: VTCompressionSessionRef,
        previous: Option<&Self>,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        if session.is_null() {
            return Err("Missing encoder during color propagation".into());
        }
        if previous == Some(self) {
            return Ok(false);
        }
        let keys = unsafe {
            [
                kVTCompressionPropertyKey_ColorPrimaries,
                kVTCompressionPropertyKey_TransferFunction,
                kVTCompressionPropertyKey_YCbCrMatrix,
            ]
        };
        let values = [&self.primaries, &self.transfer, &self.matrix];
        let old = previous.map(|v| [&v.primaries, &v.transfer, &v.matrix]);
        let mut changed = false;
        for (i, (key, value)) in keys.into_iter().zip(values).enumerate() {
            if old.is_some_and(|prior| prior[i] == value) || (old.is_none() && value.is_none()) {
                continue;
            }
            let raw = value
                .as_ref()
                .map_or(std::ptr::null(), TCFType::as_CFTypeRef);
            let status = unsafe { VTSessionSetProperty(session, key, raw) };
            if status != 0 {
                // Some encoders fix their output colorimetry. It is acceptable
                // only when the actual encoder property equals the source value.
                let mut reported: CFTypeRef = std::ptr::null();
                let copied = unsafe {
                    VTSessionCopyProperty(
                        session,
                        key,
                        std::ptr::null(),
                        (&mut reported as *mut CFTypeRef).cast(),
                    )
                };
                let observed = if reported.is_null() {
                    None
                } else {
                    Some(unsafe { CFType::wrap_under_create_rule(reported) })
                };
                let same = copied == 0
                    && observed
                        .as_ref()
                        .and_then(|v| v.downcast::<CFString>())
                        .as_ref()
                        == value.as_ref();
                if !same {
                    return Err(format!("Encoder rejected source color property {i} (OSStatus {status}); refusing to mislabel pixels").into());
                }
            }
            changed = true;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_metadata_is_not_a_default_color_space() {
        assert_eq!(
            SourceColor::default(),
            SourceColor {
                primaries: None,
                transfer: None,
                matrix: None
            }
        );
        assert_ne!(
            SourceColor::default(),
            SourceColor {
                matrix: Some(CFString::new("ITU_R_709_2")),
                ..Default::default()
            }
        );
    }
    #[test]
    fn transfer_and_matrix_are_independent_properties() {
        let a = SourceColor {
            primaries: Some(CFString::new("ITU_R_709_2")),
            transfer: Some(CFString::new("sRGB")),
            matrix: Some(CFString::new("ITU_R_709_2")),
        };
        let b = SourceColor {
            matrix: Some(CFString::new("ITU_R_601_4")),
            ..a.clone()
        };
        assert_ne!(a, b);
        assert_eq!(a.transfer, b.transfer);
    }
}

// ScreenCaptureKit must be told which RGB->YCbCr conversion to perform. This
// configures actual pixels at capture, not an assumed matrix attached downstream.
// Color primaries/transfer remain those reported by the source; no gamut narrowing.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGDisplayStreamYCbCrMatrix_ITU_R_709_2: CFTypeRef;
}
pub(crate) fn capture_yuv_matrix() -> String {
    unsafe {
        CFString::wrap_under_get_rule(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2.cast()).to_string()
    }
}
