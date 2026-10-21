//! Bounded CPU snapshot bytes for observation, separate from the renderer.
//! Native macOS snapshots use Core Image color conversion, never renderer fallback.
use std::sync::Arc;
#[cfg(target_os = "macos")]
pub(crate) mod macos;

pub const MAX_OBSERVATION_BYTES: usize = 16 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelError {
    NativeReadbackUnsupported,
    InvalidDimensions,
    InvalidStride,
    LengthMismatch,
    SizeLimit,
    ArithmeticOverflow,
    ColorMetadataUnavailable,
    NativeReadbackFailed,
}
impl std::fmt::Display for PixelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PixelError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMetadata {
    /// The software decoder's RGBA buffer carries no range/matrix/transfer/
    /// primaries metadata. Never relabel it sRGB/BT.709 or invent SPA numbers.
    Unspecified,
    /// Core Image converted declared source color into sRGB RGBA8, opaque alpha.
    Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbaLayout {
    width: u32,
    height: u32,
    row_stride: usize,
    bytes: usize,
}
fn byte_length(row_stride: usize, height: usize) -> Result<usize, PixelError> {
    row_stride.checked_mul(height).ok_or(PixelError::ArithmeticOverflow)
}
impl RgbaLayout {
    /// Only tightly packed RGBA8 is supported. Reject padding/trailing bytes
    /// rather than exposing them or silently treating BGRA/YUV as RGBA.
    pub fn validate(width: u32, height: u32, row_stride: usize,
        length: usize, maximum: usize) -> Result<Self, PixelError> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(PixelError::InvalidDimensions);
        }
        let row = usize::try_from(width).ok().and_then(|w| w.checked_mul(4))
            .ok_or(PixelError::ArithmeticOverflow)?;
        let rows = usize::try_from(height).map_err(|_| PixelError::ArithmeticOverflow)?;
        let bytes = byte_length(row_stride, rows)?;
        if row_stride != row { return Err(PixelError::InvalidStride); }
        if bytes > maximum.min(MAX_OBSERVATION_BYTES) { return Err(PixelError::SizeLimit); }
        if length != bytes { return Err(PixelError::LengthMismatch); }
        Ok(Self { width, height, row_stride, bytes })
    }
    pub fn width(self) -> u32 { self.width }
    pub fn height(self) -> u32 { self.height }
    pub fn row_stride(self) -> usize { self.row_stride }
    pub fn bytes(self) -> usize { self.bytes }
}

/// Validated existing bytes, not an encoded image or remote artifact receipt.
/// Cloning the Arc does not allocate/copy a second full-sized frame.
pub struct CpuRgba {
    layout: RgbaLayout,
    pixels: Arc<Vec<u8>>,
    color: ColorMetadata,
}
impl CpuRgba {
    pub(crate) fn from_decoded(width: u32, height: u32, pixels: Arc<Vec<u8>>,
        maximum: usize) -> Result<Self, PixelError> {
        let stride = usize::try_from(width).ok().and_then(|w| w.checked_mul(4))
            .ok_or(PixelError::ArithmeticOverflow)?;
        let layout = RgbaLayout::validate(width, height, stride, pixels.len(), maximum)?;
        Ok(Self { layout, pixels, color: ColorMetadata::Unspecified })
    }
    pub fn layout(&self) -> RgbaLayout { self.layout }
    pub fn pixels(&self) -> &[u8] { &self.pixels }
    pub fn color(&self) -> ColorMetadata { self.color }
    pub fn into_pixels(self) -> Arc<Vec<u8>> { self.pixels }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tightly_packed_cpu_snapshot_retains_exact_bytes_without_copying() {
        let pixels = Arc::new(vec![17, 29, 41, 255, 2, 3, 5, 255]);
        let frame = CpuRgba::from_decoded(2, 1, pixels.clone(), 8).unwrap();
        assert_eq!(Arc::strong_count(&pixels), 2);
        assert_eq!(frame.pixels(), pixels.as_slice());
        assert_eq!(frame.layout().row_stride(), 8);
        assert_eq!(frame.layout().bytes(), 8);
        assert_eq!(frame.color(), ColorMetadata::Unspecified);
    }
    #[test]
    fn empty_native_like_buffer_is_not_a_successful_image() {
        assert_eq!(CpuRgba::from_decoded(2, 2, Arc::new(vec![]), 16).err(),
            Some(PixelError::LengthMismatch));
    }
    #[test]
    fn zero_and_oversized_dimensions_are_rejected() {
        for (w,h) in [(0,1),(1,0),(8193,1),(1,8193),(u32::MAX,u32::MAX)] {
            assert_eq!(RgbaLayout::validate(w,h,4,4,16).unwrap_err(), PixelError::InvalidDimensions);
        }
    }
    #[test]
    fn padded_or_short_rows_are_never_exported() {
        for stride in [0,7,9,12] {
            assert_eq!(RgbaLayout::validate(2,2,stride,stride*2,64).unwrap_err(), PixelError::InvalidStride);
        }
    }
    #[test]
    fn truncated_and_trailing_bytes_are_rejected() {
        for length in [0,15,17,32] {
            assert_eq!(RgbaLayout::validate(2,2,8,length,64).unwrap_err(), PixelError::LengthMismatch);
        }
    }
    #[test]
    fn checked_extent_rejects_machine_word_overflow() {
        assert_eq!(byte_length(usize::MAX,2), Err(PixelError::ArithmeticOverflow));
        assert_eq!(RgbaLayout::validate(2,2,usize::MAX,0,usize::MAX).unwrap_err(), PixelError::ArithmeticOverflow);
    }
    #[test]
    fn caller_budget_cannot_expand_the_fixed_memory_limit() {
        assert_eq!(RgbaLayout::validate(4096,2160,16384,4096*2160*4,usize::MAX).unwrap_err(),
            PixelError::SizeLimit);
        assert_eq!(RgbaLayout::validate(2,2,8,16,15).unwrap_err(), PixelError::SizeLimit);
        assert_eq!(RgbaLayout::validate(1,1,4,4,0).unwrap_err(), PixelError::SizeLimit);
    }
    #[test]
    fn exact_fixed_budget_boundary_is_valid() {
        let layout = RgbaLayout::validate(2048,2048,8192,MAX_OBSERVATION_BYTES,usize::MAX).unwrap();
        assert_eq!(layout.bytes(), MAX_OBSERVATION_BYTES);
    }
}
