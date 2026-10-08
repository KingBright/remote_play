//! Native-video contract shared by backend adapters. Geometry is defined once for
//! sampling and inverse input mapping; unsupported resources fail before GPU use.
//! This module owns no network/session state and never reads frame pixels on CPU.
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{D3dVideoBuffer, D3dVideoFrame};
#[cfg(target_os = "windows")]
mod copy_windows;
#[cfg(target_os = "windows")]
pub use copy_windows::{D3dVideoCopyPool, D3dVideoCopyStats, ReadyD3dVideoFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// GPU plane representation accepted by the native-video adapters.
pub enum VideoFormat {
    /// 8-bit luma with an interleaved 4:2:0 chroma plane.
    Nv12,
    /// 10-bit 4:2:0 samples stored in the high bits of 16-bit components.
    P010,
    /// Opaque 8-bit blue/green/red/alpha resource; no YUV conversion.
    Bgra8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// SDR YCbCr-to-RGB matrix identified by the producer.
pub enum Matrix {
    /// ITU-R BT.601 luma/chroma coefficients.
    Bt601,
    /// ITU-R BT.709 luma/chroma coefficients.
    Bt709,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Quantization range of the coded signal, not its memory type.
pub enum Range {
    /// Video-range values, including headroom/footroom outside nominal limits.
    Limited,
    /// Full available signal range at the declared bit depth.
    Full,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Horizontal location of subsampled chroma relative to luma.
pub enum ChromaLocation {
    /// Chroma is centered between its corresponding luma samples.
    Center,
    /// Chroma is aligned with the left corresponding luma sample.
    Left,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Explicit SDR signal interpretation; does not imply HDR tone mapping.
pub struct VideoColor {
    /// Producer-declared YCbCr matrix.
    pub matrix: Matrix,
    /// Producer-declared code-value range.
    pub range: Range,
    /// Chroma sampling position used by the adapter.
    pub chroma: ChromaLocation,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Clockwise rotation from source pixels to the displayed rectangle.
pub enum Rotation {
    /// No rotation.
    R0,
    /// Rotate clockwise by 90 degrees.
    R90,
    /// Rotate by 180 degrees.
    R180,
    /// Rotate clockwise by 270 degrees.
    R270,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Validated relation between resource pixels and displayed content.
pub struct VideoGeometry {
    /// Backing resource width and height including padding.
    pub coded: [u32; 2],
    /// x, y, width, height, in coded pixels. Padding is outside this rectangle.
    pub visible: [u32; 4],
    /// Positive sample width-to-height rational ratio.
    pub pixel_aspect: [u32; 2],
    /// Source-to-display rotation, shared by sampling and pointer mapping.
    pub rotation: Rotation,
}
#[derive(Clone, Copy, Debug, PartialEq)]
/// Finite rectangle in a caller-selected common coordinate space.
pub struct VideoRect {
    /// Left coordinate, possibly negative for offset desktops.
    pub x: f32,
    /// Top coordinate, possibly negative for offset desktops.
    pub y: f32,
    /// Positive horizontal extent in the same units as the origin.
    pub width: f32,
    /// Positive vertical extent in the same units as the origin.
    pub height: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// How video display dimensions fit inside the viewport.
pub enum VideoFit {
    /// Preserve aspect ratio and leave bars when needed.
    Contain,
    /// Preserve aspect ratio and crop beyond the viewport.
    Cover,
    /// Fill the viewport, permitting aspect-ratio changes.
    Stretch,
    /// Preserve aspect ratio without enlarging the native display extent.
    ScaleDown,
    /// Keep native display size at the viewport origin.
    Native,
}

impl VideoRect {
    /// Reject empty, nonfinite, overflowing or unrepresentable rectangles.
    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.
            && self.height > 0.
            && (self.x + self.width).is_finite()
            && (self.y + self.height).is_finite()
            && self.x + self.width > self.x
            && self.y + self.height > self.y
    }
    /// Test a point with inclusive left/top and exclusive right/bottom edges.
    pub fn contains(self, p: [f32; 2]) -> bool {
        self.valid()
            && p[0].is_finite()
            && p[1].is_finite()
            && p[0] >= self.x
            && p[1] >= self.y
            && p[0] < self.x + self.width
            && p[1] < self.y + self.height
    }
}
impl VideoGeometry {
    /// Validate crop, backing size and subsampled format constraints.
    pub fn validate(self, format: VideoFormat) -> Result<(), &'static str> {
        let [w, h] = self.coded;
        let [x, y, cw, ch] = self.visible;
        if w == 0 || h == 0 || w > 16384 || h > 16384 || cw == 0 || ch == 0 {
            return Err("invalid video extent");
        }
        if x.checked_add(cw).is_none_or(|end| end > w)
            || y.checked_add(ch).is_none_or(|end| end > h)
        {
            return Err("visible video crop is outside its resource");
        }
        if format != VideoFormat::Bgra8 && (w % 2 != 0 || h % 2 != 0) {
            return Err("4:2:0 resource dimensions must be even");
        }
        if self.pixel_aspect.iter().any(|v| *v == 0 || *v > 10000) {
            return Err("invalid sample aspect ratio");
        }
        Ok(())
    }
    /// Return visible display size after sample aspect and rotation.
    pub fn display_size(self) -> [f32; 2] {
        let width =
            self.visible[2] as f32 * self.pixel_aspect[0] as f32 / self.pixel_aspect[1] as f32;
        let height = self.visible[3] as f32;
        match self.rotation {
            Rotation::R0 | Rotation::R180 => [width, height],
            _ => [height, width],
        }
    }
    /// Compute the display rectangle without mixing device pixels and UI units.
    pub fn destination(self, viewport: VideoRect, fit: VideoFit) -> Option<VideoRect> {
        // Geometry can be constructed by callers before a backend validates its
        // pixel format. Reject malformed bounds even on this metadata-only path.
        if !viewport.valid() || self.validate(VideoFormat::Bgra8).is_err() {
            return None;
        }
        let [w, h] = self.display_size();
        if !w.is_finite() || !h.is_finite() || w <= 0. || h <= 0. {
            return None;
        }
        if fit == VideoFit::Stretch {
            return Some(viewport);
        }
        if fit == VideoFit::Native {
            let rect = VideoRect {
                x: viewport.x,
                y: viewport.y,
                width: w,
                height: h,
            };
            return rect.valid().then_some(rect);
        }
        // This is a once-per-layout calculation, not pixel processing. The wider
        // intermediate avoids overflow before a legitimate final f32 rectangle.
        let (w, h) = (f64::from(w), f64::from(h));
        let a = f64::from(viewport.width) / w;
        let b = f64::from(viewport.height) / h;
        let scale = match fit {
            VideoFit::Contain => a.min(b),
            VideoFit::ScaleDown => a.min(b).min(1.),
            _ => a.max(b),
        };
        let rect = VideoRect {
            x: (f64::from(viewport.x) + (f64::from(viewport.width) - w * scale) * 0.5) as f32,
            y: (f64::from(viewport.y) + (f64::from(viewport.height) - h * scale) * 0.5) as f32,
            width: (w * scale) as f32,
            height: (h * scale) as f32,
        };
        rect.valid().then_some(rect)
    }
    /// The renderer and pointer mapper use the identical orientation. No DPI is
    /// silently applied: viewport and point must be in the same coordinate units.
    pub fn inverse_unit(self, u: f32, v: f32) -> [f32; 2] {
        match self.rotation {
            Rotation::R0 => [u, v],
            Rotation::R90 => [v, 1. - u],
            Rotation::R180 => [1. - u, 1. - v],
            Rotation::R270 => [1. - v, u],
        }
    }
    /// Map a viewport point into normalized source-visible coordinates; reject bars.
    pub fn map_input(
        self,
        viewport: VideoRect,
        fit: VideoFit,
        point: [f32; 2],
    ) -> Option<[f32; 2]> {
        if !viewport.contains(point) {
            return None;
        }
        let rect = self.destination(viewport, fit)?;
        if !rect.contains(point) {
            return None;
        }
        let p = self.inverse_unit(
            (point[0] - rect.x) / rect.width,
            (point[1] - rect.y) / rect.height,
        );
        Some([p[0].clamp(0., 1.), p[1].clamp(0., 1.)])
    }
    /// Map display-unit coordinates to normalized backing-resource coordinates.
    pub fn texture_uv(self, u: f32, v: f32) -> [f32; 2] {
        let p = self.inverse_unit(u, v);
        [
            (self.visible[0] as f32 + p[0] * self.visible[2] as f32) / self.coded[0] as f32,
            (self.visible[1] as f32 + p[1] * self.visible[3] as f32) / self.coded[1] as f32,
        ]
    }
    /// Return affine texture-coordinate rows used by native shaders.
    pub fn uv_rows(self) -> [[f32; 4]; 2] {
        let a = self.texture_uv(0., 0.);
        let b = self.texture_uv(1., 0.);
        let c = self.texture_uv(0., 1.);
        [
            [b[0] - a[0], c[0] - a[0], a[0], 0.],
            [b[1] - a[1], c[1] - a[1], a[1], 0.],
        ]
    }
    /// Return half-texel visible bounds to avoid sampling padded luma pixels.
    pub fn uv_clamp(self) -> [f32; 4] {
        let [x, y, w, h] = self.visible;
        let [cw, ch] = self.coded;
        [
            (x as f32 + 0.5) / cw as f32,
            (y as f32 + 0.5) / ch as f32,
            ((x + w) as f32 - 0.5) / cw as f32,
            ((y + h) as f32 - 0.5) / ch as f32,
        ]
    }
}
impl VideoColor {
    /// Converts normalized R8/RG8 or high-bit-aligned R16/RG16 samples to signal
    /// RGB. No HDR tone mapping or gamut conversion is implied by this matrix.
    pub fn rows(self, format: VideoFormat) -> [[f32; 4]; 3] {
        if format == VideoFormat::Bgra8 {
            return [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.]];
        }
        let (max, shift, bits) = if format == VideoFormat::P010 {
            (65535f64, 64f64, 4f64)
        } else {
            (255., 1., 1.)
        };
        let (ys, yoff, cs, coff) = match self.range {
            Range::Limited => (
                max / (219. * bits * shift),
                16. * bits * shift / max,
                max / (224. * bits * shift),
                128. * bits * shift / max,
            ),
            Range::Full => (
                max / ((256. * bits - 1.) * shift),
                0.,
                max / ((256. * bits - 1.) * shift),
                128. * bits * shift / max,
            ),
        };
        let (kr, kb) = match self.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1. - kr - kb;
        let coefficients = [
            [ys, 0., 2. * (1. - kr) * cs],
            [
                ys,
                -2. * kb * (1. - kb) / kg * cs,
                -2. * kr * (1. - kr) / kg * cs,
            ],
            [ys, 2. * (1. - kb) * cs, 0.],
        ];
        coefficients.map(|r| {
            [
                r[0] as f32,
                r[1] as f32,
                r[2] as f32,
                (-r[0] * yoff - (r[1] + r[2]) * coff) as f32,
            ]
        })
    }
}

/// Facts about this surface renderer only. GPU completion is not screen scan-out.
#[derive(Default, Debug)]
pub struct VideoSurfaceStats {
    /// Most recent adapter error, retained for application diagnostics.
    pub last_error: std::sync::Mutex<Option<String>>,
    /// Number of GPU resource imports, not frames received.
    pub imports: AtomicU64,
    /// Number of GPU draws submitted; not monitor presentation.
    pub draws_submitted: AtomicU64,
    /// Number of draw-completion queries observed as complete.
    pub gpu_completed: AtomicU64,
    /// Draws skipped because an external owner held the resource.
    pub sync_busy: AtomicU64,
    /// Draws rejected by the bounded in-flight resource budget.
    pub queue_full: AtomicU64,
    /// Resources or draws rejected by metadata/device validation.
    pub rejected: AtomicU64,
}
impl VideoSurfaceStats {
    /// Snapshot imports, submitted, completed, busy, full and rejected counters.
    pub fn snapshot(&self) -> [u64; 6] {
        [
            &self.imports,
            &self.draws_submitted,
            &self.gpu_completed,
            &self.sync_busy,
            &self.queue_full,
            &self.rejected,
        ]
        .map(|v| v.load(Ordering::Relaxed))
    }
}

/// Descriptive validation only: a DMA-BUF handle is not enough to safely import
/// a video plane. Modifier/capability and completion checks are backend duties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaPlane {
    /// Index of the backing DMA object in the exporter list.
    pub object_index: usize,
    /// Byte offset into the backing object.
    pub offset: u64,
    /// Byte distance between successive rows.
    pub pitch: u32,
}
/// Validate linear NV12 plane spans; does not validate tiled DRM modifiers.
pub fn validate_linear_nv12_layout(
    coded: [u32; 2],
    object_sizes: &[u64],
    planes: [DmaPlane; 2],
) -> Result<(), &'static str> {
    let [w, h] = coded;
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || w > 16384 || h > 16384 {
        return Err("invalid NV12 extent");
    }
    if object_sizes.is_empty() || object_sizes.len() > 4 {
        return Err("invalid DMA object count");
    }
    let mut ranges = [(0usize, 0u64, 0u64); 2];
    for (index, p) in planes.into_iter().enumerate() {
        let size = *object_sizes
            .get(p.object_index)
            .ok_or("DMA plane references missing object")?;
        if p.pitch < w {
            return Err("DMA plane row pitch is shorter than its pixels");
        }
        let rows = if index == 0 { h } else { h / 2 };
        let len = u64::from(rows - 1)
            .checked_mul(u64::from(p.pitch))
            .and_then(|n| n.checked_add(u64::from(w)))
            .ok_or("DMA plane span overflow")?;
        let end = p
            .offset
            .checked_add(len)
            .ok_or("DMA plane offset overflow")?;
        if end > size {
            return Err("DMA plane exceeds backing allocation");
        }
        ranges[index] = (p.object_index, p.offset, end);
    }
    if ranges[0].0 == ranges[1].0 && ranges[0].1 < ranges[1].2 && ranges[1].1 < ranges[0].2 {
        return Err("DMA video planes overlap");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn geo() -> VideoGeometry {
        VideoGeometry {
            coded: [1920, 1088],
            visible: [0, 0, 1920, 1080],
            pixel_aspect: [1, 1],
            rotation: Rotation::R0,
        }
    }
    fn near(a: f32, b: f32) {
        assert!((a - b).abs() < 0.00002, "{a} != {b}");
    }
    #[test]
    fn padding_never_becomes_visible() {
        let g = geo();
        assert!(g.validate(VideoFormat::Nv12).is_ok());
        near(g.texture_uv(1., 1.)[1], 1080. / 1088.);
        assert!(g.uv_clamp()[3] < 1080. / 1088.);
    }
    #[test]
    fn overflow_and_odd_coded_sizes_rejected() {
        let mut g = geo();
        g.visible[0] = u32::MAX;
        assert!(g.validate(VideoFormat::Nv12).is_err());
        g = geo();
        g.coded[0] = 1919;
        assert!(g.validate(VideoFormat::Nv12).is_err());
    }
    #[test]
    fn bars_and_outside_never_map_to_remote_input() {
        let g = geo();
        let v = VideoRect {
            x: 10.,
            y: 20.,
            width: 1000.,
            height: 1000.,
        };
        assert_eq!(g.map_input(v, VideoFit::Contain, [20., 30.]), None);
        assert_eq!(g.map_input(v, VideoFit::Contain, [f32::NAN, 500.]), None);
        assert_eq!(g.map_input(v, VideoFit::Contain, [1010., 520.]), None);
        assert_eq!(
            g.map_input(v, VideoFit::Contain, [510., 520.]),
            Some([0.5, 0.5])
        );
    }
    #[test]
    fn pointer_and_sample_align_at_every_rotation_and_dpi() {
        for rotation in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            let mut g = geo();
            g.rotation = rotation;
            for dpi in [1., 1.25, 1.5, 2., 3.] {
                let [w, h] = g.display_size();
                let v = VideoRect {
                    x: -100. * dpi,
                    y: 35. * dpi,
                    width: w * dpi,
                    height: h * dpi,
                };
                for [u, vv] in [[0.01, 0.01], [0.25, 0.75], [0.99, 0.99]] {
                    let p = g
                        .map_input(
                            v,
                            VideoFit::Contain,
                            [v.x + u * v.width, v.y + vv * v.height],
                        )
                        .unwrap();
                    let uv = g.texture_uv(u, vv);
                    near(uv[0], p[0]);
                    near(uv[1], p[1] * 1080. / 1088.);
                }
            }
        }
    }
    #[test]
    fn cover_and_anamorphic_geometry_stay_invertible() {
        let mut g = geo();
        g.pixel_aspect = [2, 1];
        let v = VideoRect {
            x: 0.,
            y: 0.,
            width: 800.,
            height: 600.,
        };
        near(g.destination(v, VideoFit::Cover).unwrap().height, 600.);
        let p = g.map_input(v, VideoFit::Cover, [400., 300.]).unwrap();
        near(p[0], 0.5);
        near(p[1], 0.5);
    }
    #[test]
    fn invalid_viewport_does_not_create_nan_coordinates() {
        let g = geo();
        for n in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(g
                .destination(
                    VideoRect {
                        x: 0.,
                        y: 0.,
                        width: n,
                        height: 20.
                    },
                    VideoFit::Contain
                )
                .is_none());
        }
    }
    #[test]
    fn uv_rows_match_direct_mapping() {
        for rot in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            let mut g = geo();
            g.rotation = rot;
            let rows = g.uv_rows();
            for [u, v] in [[0., 0.], [1., 1.], [0.2, 0.7]] {
                let direct = g.texture_uv(u, v);
                for i in 0..2 {
                    near(rows[i][0] * u + rows[i][1] * v + rows[i][2], direct[i]);
                }
            }
        }
    }
    #[test]
    fn video_black_white_neutral_are_correct_in_8_and_10_bit() {
        for format in [VideoFormat::Nv12, VideoFormat::P010] {
            let mul = if format == VideoFormat::P010 {
                256. / 65535.
            } else {
                1. / 255.
            };
            for matrix in [Matrix::Bt601, Matrix::Bt709] {
                let rows = VideoColor {
                    matrix,
                    range: Range::Limited,
                    chroma: ChromaLocation::Center,
                }
                .rows(format);
                for (y, wanted) in [(16., 0.), (235., 1.)] {
                    let input = [y * mul, 128. * mul, 128. * mul];
                    for r in rows {
                        near(
                            r[0] * input[0] + r[1] * input[1] + r[2] * input[2] + r[3],
                            wanted,
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn full_range_10bit_is_not_misread_as_16bit() {
        let c = VideoColor {
            matrix: Matrix::Bt709,
            range: Range::Full,
            chroma: ChromaLocation::Center,
        };
        for r in c.rows(VideoFormat::P010) {
            let input = [
                1023. * 64. / 65535.,
                512. * 64. / 65535.,
                512. * 64. / 65535.,
            ];
            near(
                r[0] * input[0] + r[1] * input[1] + r[2] * input[2] + r[3],
                1.,
            );
        }
    }
    #[test]
    fn linear_dma_planes_validate_stride_offset_and_overlap() {
        let p = [
            DmaPlane {
                object_index: 0,
                offset: 0,
                pitch: 2048,
            },
            DmaPlane {
                object_index: 0,
                offset: 2048 * 1088,
                pitch: 2048,
            },
        ];
        assert!(validate_linear_nv12_layout([1920, 1088], &[2048 * 1632], p).is_ok());
        let mut bad = p;
        bad[1].offset = 100;
        assert!(validate_linear_nv12_layout([1920, 1088], &[2048 * 1632], bad).is_err());
        bad = p;
        bad[1].offset = u64::MAX;
        assert!(validate_linear_nv12_layout([1920, 1088], &[u64::MAX], bad).is_err());
        bad = p;
        bad[1].object_index = 2;
        assert!(validate_linear_nv12_layout([1920, 1088], &[2048 * 1632], bad).is_err());
    }
}

#[cfg(all(target_os = "windows", feature = "test-support"))]
pub use windows::synthetic_frame;

#[cfg(test)]
mod boundary_tests {
    use super::*;
    fn g() -> VideoGeometry {
        VideoGeometry {
            coded: [2048, 1152],
            visible: [32, 16, 1920, 1080],
            pixel_aspect: [1, 1],
            rotation: Rotation::R0,
        }
    }
    #[test]
    fn finite_components_with_overflowed_edges_are_rejected() {
        let v = VideoRect {
            x: f32::MAX,
            y: 0.,
            width: f32::MAX,
            height: 1.,
        };
        assert!(!v.valid());
        assert!(g().destination(v, VideoFit::Contain).is_none());
        let lost = VideoRect {
            x: 1.0e30,
            y: 0.,
            width: 1.,
            height: 1.,
        };
        assert!(
            !lost.valid(),
            "unrepresentable input extent must not accept clicks"
        );
    }
    #[test]
    fn failed_geometry_does_not_create_input_coordinates() {
        let view = VideoRect {
            x: 0.,
            y: 0.,
            width: 800.,
            height: 600.,
        };
        for corrupted in [
            VideoGeometry {
                coded: [0, 1080],
                ..g()
            },
            VideoGeometry {
                visible: [u32::MAX, 0, 1, 1],
                ..g()
            },
            VideoGeometry {
                pixel_aspect: [0, 1],
                ..g()
            },
        ] {
            for fit in [VideoFit::Contain, VideoFit::Cover, VideoFit::Stretch] {
                assert!(corrupted.map_input(view, fit, [400., 300.]).is_none());
            }
        }
    }
    #[test]
    fn cover_result_that_cannot_be_represented_is_rejected() {
        let very_wide = VideoGeometry {
            coded: [16384, 2],
            visible: [0, 0, 16384, 2],
            pixel_aspect: [10000, 1],
            rotation: Rotation::R0,
        };
        let view = VideoRect {
            x: 0.,
            y: 0.,
            width: 1.,
            height: f32::MAX / 4.,
        };
        assert!(view.valid());
        assert!(very_wide.destination(view, VideoFit::Cover).is_none());
    }
    #[test]
    fn rotated_cropped_point_has_independent_golden_answers() {
        for (rotation, expected) in [
            (Rotation::R0, [0.2, 0.7]),
            (Rotation::R90, [0.7, 0.8]),
            (Rotation::R180, [0.8, 0.3]),
            (Rotation::R270, [0.3, 0.2]),
        ] {
            let geometry = VideoGeometry { rotation, ..g() };
            let view = VideoRect {
                x: -150.,
                y: 72.,
                width: 1000.,
                height: 800.,
            };
            let point = [view.x + view.width * 0.2, view.y + view.height * 0.7];
            let actual = geometry.map_input(view, VideoFit::Stretch, point).unwrap();
            for i in 0..2 {
                assert!((actual[i] - expected[i]).abs() < 0.00001);
            }
            let uv = geometry.texture_uv(0.2, 0.7);
            assert!((uv[0] - (32. + 1920. * expected[0]) / 2048.).abs() < 0.00001);
            assert!((uv[1] - (16. + 1080. * expected[1]) / 1152.).abs() < 0.00001);
        }
    }
    #[test]
    fn sampled_layouts_keep_render_and_input_in_the_same_coordinate_space() {
        let mut seed = 0x517c_e105u32;
        let mut sample_count = 0;
        for _ in 0..4096 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let width = 64 + 2 * (seed % 1024);
            let height = 64 + 2 * ((seed >> 10) % 768);
            let visible = [2, 4, width - 8, height - 10];
            let geometry = VideoGeometry {
                coded: [width, height],
                visible,
                pixel_aspect: if seed & 1 == 0 { [1, 1] } else { [4, 3] },
                rotation: match seed % 4 {
                    0 => Rotation::R0,
                    1 => Rotation::R90,
                    2 => Rotation::R180,
                    _ => Rotation::R270,
                },
            };
            geometry.validate(VideoFormat::Nv12).unwrap();
            let dpi = [1., 1.25, 1.5, 2., 3.][(seed as usize >> 20) % 5];
            let view = VideoRect {
                x: -73. * dpi,
                y: 51. * dpi,
                width: (320 + (seed % 1500)) as f32 * dpi,
                height: (240 + ((seed >> 6) % 900)) as f32 * dpi,
            };
            let fit = match (seed >> 4) % 3 {
                0 => VideoFit::Contain,
                1 => VideoFit::Cover,
                _ => VideoFit::Stretch,
            };
            let rect = geometry.destination(view, fit).unwrap();
            let point = [
                view.x + view.width * (0.2 + (seed % 60) as f32 / 100.),
                view.y + view.height * (0.2 + ((seed >> 8) % 60) as f32 / 100.),
            ];
            match geometry.map_input(view, fit, point) {
                None => assert!(!rect.contains(point)),
                Some(input) => {
                    let unit = [
                        (point[0] - rect.x) / rect.width,
                        (point[1] - rect.y) / rect.height,
                    ];
                    let rows = geometry.uv_rows();
                    for i in 0..2 {
                        let actual = rows[i][0] * unit[0] + rows[i][1] * unit[1] + rows[i][2];
                        let expected = (visible[i] as f32 + input[i] * visible[i + 2] as f32)
                            / geometry.coded[i] as f32;
                        assert!((actual - expected).abs() < 0.00002);
                    }
                    sample_count += 1;
                }
            }
        }
        assert!(sample_count > 2000);
        println!(
            "ALIGNED_LAYOUT_SAMPLES 4096 layouts checked; {sample_count} valid input positions"
        );
    }
    #[test]
    fn linear_planes_require_backing_bytes_not_only_a_valid_handle() {
        let p = [
            DmaPlane {
                object_index: 0,
                offset: 0,
                pitch: 1920,
            },
            DmaPlane {
                object_index: 1,
                offset: 0,
                pitch: 1920,
            },
        ];
        assert!(validate_linear_nv12_layout([1920, 1080], &[1920 * 1080, 1920 * 540], p).is_ok());
        assert!(
            validate_linear_nv12_layout([1920, 1080], &[1920 * 1080, 1920 * 540 - 1], p).is_err()
        );
        assert!(validate_linear_nv12_layout([1920, 1080], &[0, 1920 * 540], p).is_err());
        let short = [
            p[0],
            DmaPlane {
                pitch: 1919,
                ..p[1]
            },
        ];
        assert!(
            validate_linear_nv12_layout([1920, 1080], &[1920 * 1080, 1920 * 540], short).is_err()
        );
    }
}

/// Run a bounded synthetic physical-GPU validation in test-support builds only.
/// It creates no application window, remote session, screen capture or input.
#[cfg(all(target_os = "windows", feature = "test-support"))]
pub fn validate_windows_gpu() -> anyhow::Result<()> {
    crate::platform::validate_native_windows_gpu()
}

#[cfg(test)]
mod fit_parity_tests {
    use super::*;
    #[test]
    fn original_fit_modes_do_not_round_sample_aspect() {
        let g = VideoGeometry {
            coded: [1920, 1088],
            visible: [0, 0, 1920, 1080],
            pixel_aspect: [1, 1],
            rotation: Rotation::R0,
        };
        let large = VideoRect {
            x: 12.,
            y: 30.,
            width: 3840.,
            height: 2160.,
        };
        assert_eq!(
            g.destination(large, VideoFit::ScaleDown),
            Some(VideoRect {
                x: 972.,
                y: 570.,
                width: 1920.,
                height: 1080.
            })
        );
        assert_eq!(
            g.destination(large, VideoFit::Native),
            Some(VideoRect {
                x: 12.,
                y: 30.,
                width: 1920.,
                height: 1080.
            })
        );
        let small = VideoRect {
            x: -20.,
            y: 10.,
            width: 480.,
            height: 270.,
        };
        assert_eq!(g.destination(small, VideoFit::ScaleDown), Some(small));
        assert_eq!(g.map_input(large, VideoFit::ScaleDown, [13., 31.]), None);
        assert_eq!(
            g.map_input(large, VideoFit::Native, [972., 570.]),
            Some([0.5, 0.5])
        );
    }
    #[test]
    fn full_range_eight_bit_neutral_black_white_is_unchanged() {
        for matrix in [Matrix::Bt601, Matrix::Bt709] {
            let rows = VideoColor {
                matrix,
                range: Range::Full,
                chroma: ChromaLocation::Center,
            }
            .rows(VideoFormat::Nv12);
            for y in [0., 1.] {
                for row in rows {
                    let actual = row[0] * y + (row[1] + row[2]) * (128. / 255.) + row[3];
                    assert!((actual - y).abs() < 0.00001);
                }
            }
        }
    }
}

#[cfg(target_os="linux")]
pub(crate) mod linux;
#[cfg(target_os="linux")]
pub use linux::{DmaVideoPlane,DmaVideoFrame,linux_video_imports_live};
