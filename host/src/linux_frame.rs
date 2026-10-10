//! Owned CPU frames at the restricted PipeWire/encoder boundary.
//!
//! SPA mappings and buffers stay on the PipeWire thread. A borrowed plane begins
//! at SPA data.data (the mapping offset has already been applied by PipeWire).
//! Only checked, tightly packed copies and metadata cross this boundary.

use std::sync::Mutex;
use tokio::sync::Notify;

pub const MAX_DIMENSION: u32 = 8192;
pub const MAX_PIXELS: u64 = 33_554_432;
pub const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra,
    Bgrx,
    Rgba,
    Rgbx,
    Nv12,
    I420,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceColor {
    // Preserve the exact SPA enums, including zero (unknown). Conversion to
    // codec values belongs to the encoder, never to a resolution heuristic.
    pub range: u32,
    pub matrix: u32,
    pub transfer: u32,
    pub primaries: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    Identity,
    Unsupported(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameFormat {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub color: SourceColor,
    pub crop: Option<Crop>,
    pub transform: Transform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStamp {
    pub generation: u64,
    pub sequence: Option<u64>,
    /// PipeWire's nanosecond PTS. Its epoch is not the host timing epoch.
    pub pipewire_pts_ns: Option<i64>,
    /// Host quanta arrival time; no inferred capture time is claimed.
    pub arrival_ts_us: u64,
}

pub struct BorrowedPlane<'a> {
    pub data: &'a [u8],
    /// Original mapping offset is metadata, not another index into data.data.
    pub mapping_offset: u32,
    pub chunk_offset: u32,
    pub chunk_size: u32,
    pub stride: i32,
}

#[derive(Debug)]
pub struct OwnedFrame {
    pub source_format: FrameFormat,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<Vec<u8>>,
    pub mapping_offsets: Vec<u32>,
    pub stamp: FrameStamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    Dimensions,
    UnsupportedTransform,
    Crop,
    PlaneCount,
    Stride,
    Bounds,
    Budget,
    Generation,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid native Linux frame: {self:?}")
    }
}
impl std::error::Error for FrameError {}

struct PlaneLayout {
    row_bytes: usize,
    rows: usize,
    crop_x_bytes: usize,
    crop_y: usize,
    copied_row_bytes: usize,
    copied_rows: usize,
}

impl FrameFormat {
    fn crop_rect(self) -> Result<Crop, FrameError> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_DIMENSION
            || self.height > MAX_DIMENSION
            || u64::from(self.width) * u64::from(self.height) > MAX_PIXELS
        {
            return Err(FrameError::Dimensions);
        }
        if self.transform != Transform::Identity {
            return Err(FrameError::UnsupportedTransform);
        }
        let crop = self.crop.unwrap_or(Crop {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        });
        if crop.width == 0
            || crop.height == 0
            || crop
                .x
                .checked_add(crop.width)
                .is_none_or(|x| x > self.width)
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|y| y > self.height)
        {
            return Err(FrameError::Crop);
        }
        if matches!(self.pixel_format, PixelFormat::Nv12 | PixelFormat::I420)
            && [
                self.width,
                self.height,
                crop.x,
                crop.y,
                crop.width,
                crop.height,
            ]
            .into_iter()
            .any(|value| value % 2 != 0)
        {
            return Err(FrameError::Crop);
        }
        Ok(crop)
    }

    fn plane_layouts(self, crop: Crop) -> Vec<PlaneLayout> {
        let layout = |subsample: u32, bytes: u32| PlaneLayout {
            row_bytes: (self.width / subsample * bytes) as usize,
            rows: (self.height / subsample) as usize,
            crop_x_bytes: (crop.x / subsample * bytes) as usize,
            crop_y: (crop.y / subsample) as usize,
            copied_row_bytes: (crop.width / subsample * bytes) as usize,
            copied_rows: (crop.height / subsample) as usize,
        };
        match self.pixel_format {
            PixelFormat::Bgra | PixelFormat::Bgrx | PixelFormat::Rgba | PixelFormat::Rgbx => {
                vec![layout(1, 4)]
            }
            PixelFormat::Nv12 => vec![layout(1, 1), layout(2, 2)],
            PixelFormat::I420 => vec![layout(1, 1), layout(2, 1), layout(2, 1)],
        }
    }
}

impl OwnedFrame {
    pub fn copy_from(
        format: FrameFormat,
        planes: &[BorrowedPlane<'_>],
        stamp: FrameStamp,
    ) -> Result<Self, FrameError> {
        if stamp.generation == 0 {
            return Err(FrameError::Generation);
        }
        let crop = format.crop_rect()?;
        let layouts = format.plane_layouts(crop);
        if layouts.len() != planes.len() {
            return Err(FrameError::PlaneCount);
        }
        // Validate every plane before allocating or copying any of them.
        let mut total = 0usize;
        for (plane, layout) in planes.iter().zip(&layouts) {
            let stride = usize::try_from(plane.stride).map_err(|_| FrameError::Stride)?;
            if stride < layout.row_bytes {
                return Err(FrameError::Stride);
            }
            let begin = plane.chunk_offset as usize;
            let end = begin
                .checked_add(plane.chunk_size as usize)
                .ok_or(FrameError::Bounds)?;
            let required = stride
                .checked_mul(layout.rows - 1)
                .and_then(|value| value.checked_add(layout.row_bytes))
                .ok_or(FrameError::Bounds)?;
            if end > plane.data.len() || required > plane.chunk_size as usize {
                return Err(FrameError::Bounds);
            }
            let bytes = layout
                .copied_row_bytes
                .checked_mul(layout.copied_rows)
                .ok_or(FrameError::Budget)?;
            total = total.checked_add(bytes).ok_or(FrameError::Budget)?;
            if total > MAX_FRAME_BYTES {
                return Err(FrameError::Budget);
            }
        }
        let mut owned = Vec::with_capacity(planes.len());
        for (plane, layout) in planes.iter().zip(&layouts) {
            let stride = plane.stride as usize;
            let mut copy = Vec::with_capacity(layout.copied_row_bytes * layout.copied_rows);
            for row in 0..layout.copied_rows {
                let begin = plane.chunk_offset as usize
                    + (layout.crop_y + row) * stride
                    + layout.crop_x_bytes;
                copy.extend_from_slice(&plane.data[begin..begin + layout.copied_row_bytes]);
            }
            owned.push(copy);
        }
        Ok(Self {
            source_format: format,
            width: crop.width,
            height: crop.height,
            planes: owned,
            mapping_offsets: planes.iter().map(|plane| plane.mapping_offset).collect(),
            stamp,
        })
    }
}

struct MailboxState {
    latest: Option<OwnedFrame>,
    paused: bool,
    closed: bool,
}

/// Only unsubmitted raw frames may replace one another. No compressed AU type
/// is accepted here. Closing revokes the generation and wakes blocked readers.
pub struct FrameMailbox {
    generation: u64,
    state: Mutex<MailboxState>,
    changed: Notify,
}

impl FrameMailbox {
    pub fn new(generation: u64) -> Result<Self, FrameError> {
        if generation == 0 {
            return Err(FrameError::Generation);
        }
        Ok(Self {
            generation,
            state: Mutex::new(MailboxState {
                latest: None,
                paused: false,
                closed: false,
            }),
            changed: Notify::new(),
        })
    }

    pub fn publish(&self, frame: OwnedFrame) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || state.paused || frame.stamp.generation != self.generation {
            return false;
        }
        state.latest = Some(frame);
        drop(state);
        self.changed.notify_one();
        true
    }

    pub fn set_paused(&self, paused: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.paused = paused;
        state.latest = None;
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        state.latest = None;
        drop(state);
        self.changed.notify_waiters();
    }

    pub async fn receive(&self) -> Result<OwnedFrame, FrameError> {
        loop {
            let notification = self.changed.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                if state.closed {
                    return Err(FrameError::Generation);
                }
                if let Some(frame) = state.latest.take() {
                    return Ok(frame);
                }
            }
            notification.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(pixel_format: PixelFormat) -> FrameFormat {
        FrameFormat {
            width: 4,
            height: 2,
            pixel_format,
            color: SourceColor::default(),
            crop: None,
            transform: Transform::Identity,
        }
    }
    fn stamp(sequence: u64) -> FrameStamp {
        FrameStamp {
            generation: 1,
            sequence: Some(sequence),
            pipewire_pts_ns: None,
            arrival_ts_us: 4321,
        }
    }
    fn packed(sequence: u64) -> OwnedFrame {
        let data = [sequence as u8; 32];
        OwnedFrame::copy_from(
            format(PixelFormat::Rgba),
            &[BorrowedPlane {
                data: &data,
                mapping_offset: 0,
                chunk_offset: 0,
                chunk_size: 32,
                stride: 16,
            }],
            stamp(sequence),
        )
        .unwrap()
    }

    #[test]
    fn packed_padding_offset_and_mapping_origin_are_independent() {
        for pixel_format in [
            PixelFormat::Rgba,
            PixelFormat::Rgbx,
            PixelFormat::Bgra,
            PixelFormat::Bgrx,
        ] {
            let mut bytes = vec![99; 46];
            bytes[3..19].fill(1);
            bytes[23..39].fill(2);
            let frame = OwnedFrame::copy_from(
                format(pixel_format),
                &[BorrowedPlane {
                    data: &bytes,
                    mapping_offset: 4096,
                    chunk_offset: 3,
                    chunk_size: 36,
                    stride: 20,
                }],
                stamp(7),
            )
            .unwrap();
            assert_eq!(frame.planes[0], [vec![1; 16], vec![2; 16]].concat());
            assert_eq!(frame.mapping_offsets, vec![4096]);
            bytes.fill(0);
            assert_eq!(frame.planes[0][16], 2); // No mapping pointer survived.
        }
    }

    #[test]
    fn nv12_and_i420_preserve_separate_chroma_planes() {
        let y = [1, 2, 3, 4, 99, 99, 5, 6, 7, 8];
        let uv = [9, 10, 11, 12];
        let u = [9, 11];
        let v = [10, 12];
        let plane = |data, size, stride| BorrowedPlane {
            data,
            mapping_offset: 0,
            chunk_offset: 0,
            chunk_size: size,
            stride,
        };
        let nv12 = OwnedFrame::copy_from(
            format(PixelFormat::Nv12),
            &[plane(&y, 10, 6), plane(&uv, 4, 4)],
            stamp(1),
        )
        .unwrap();
        assert_eq!(nv12.planes, vec![vec![1, 2, 3, 4, 5, 6, 7, 8], uv.to_vec()]);
        let i420 = OwnedFrame::copy_from(
            format(PixelFormat::I420),
            &[plane(&y, 10, 6), plane(&u, 2, 2), plane(&v, 2, 2)],
            stamp(1),
        )
        .unwrap();
        assert_eq!(i420.planes[1], u);
        assert_eq!(i420.planes[2], v);
    }

    #[test]
    fn crop_copies_rows_and_preserves_original_format_and_metadata() {
        let mut source = format(PixelFormat::Rgba);
        source.crop = Some(Crop {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        });
        source.color = SourceColor {
            range: 1,
            matrix: 0,
            transfer: 4,
            primaries: 7,
        };
        let bytes: Vec<_> = (0..32).collect();
        let mut metadata = stamp(9);
        metadata.pipewire_pts_ns = Some(8_000_000_000);
        let frame = OwnedFrame::copy_from(
            source,
            &[BorrowedPlane {
                data: &bytes,
                mapping_offset: 0,
                chunk_offset: 0,
                chunk_size: 32,
                stride: 16,
            }],
            metadata,
        )
        .unwrap();
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.planes[0], bytes[20..28]);
        assert_eq!(frame.source_format, source);
        assert_eq!(frame.stamp, metadata);
        assert_ne!(
            frame.stamp.arrival_ts_us,
            frame.stamp.pipewire_pts_ns.unwrap() as u64 / 1000
        );
    }

    #[test]
    fn malformed_layouts_never_allocate_a_frame() {
        let data = [0; 32];
        for (offset, size, stride, expected) in [
            (0, 32, -16, FrameError::Stride),
            (0, 32, 15, FrameError::Stride),
            (0, 31, 16, FrameError::Bounds),
            (1, 32, 16, FrameError::Bounds),
            (u32::MAX, u32::MAX, 16, FrameError::Bounds),
            (0, 32, i32::MAX, FrameError::Bounds),
        ] {
            assert_eq!(
                OwnedFrame::copy_from(
                    format(PixelFormat::Bgra),
                    &[BorrowedPlane {
                        data: &data,
                        mapping_offset: 0,
                        chunk_offset: offset,
                        chunk_size: size,
                        stride
                    }],
                    stamp(1)
                )
                .unwrap_err(),
                expected
            );
        }
        assert_eq!(
            OwnedFrame::copy_from(format(PixelFormat::I420), &[], stamp(1)).unwrap_err(),
            FrameError::PlaneCount
        );
    }

    #[test]
    fn unknown_color_is_retained_and_unsupported_geometry_fails_closed() {
        assert_eq!(packed(1).source_format.color, SourceColor::default());
        for (source, expected) in [
            (
                FrameFormat {
                    width: u32::MAX,
                    ..format(PixelFormat::Rgba)
                },
                FrameError::Dimensions,
            ),
            (
                FrameFormat {
                    transform: Transform::Unsupported(1),
                    ..format(PixelFormat::Rgba)
                },
                FrameError::UnsupportedTransform,
            ),
            (
                FrameFormat {
                    crop: Some(Crop {
                        x: u32::MAX,
                        y: 0,
                        width: 2,
                        height: 2,
                    }),
                    ..format(PixelFormat::Nv12)
                },
                FrameError::Crop,
            ),
            (
                FrameFormat {
                    crop: Some(Crop {
                        x: 1,
                        y: 0,
                        width: 2,
                        height: 2,
                    }),
                    ..format(PixelFormat::Nv12)
                },
                FrameError::Crop,
            ),
        ] {
            assert_eq!(
                OwnedFrame::copy_from(source, &[], stamp(1)).unwrap_err(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn mailbox_retains_newest_unsubmitted_frame_and_rejects_old_generation() {
        let mailbox = FrameMailbox::new(1).unwrap();
        assert!(mailbox.publish(packed(1)));
        assert!(mailbox.publish(packed(2)));
        let mut stale = packed(3);
        stale.stamp.generation = 2;
        assert!(!mailbox.publish(stale));
        assert_eq!(mailbox.receive().await.unwrap().stamp.sequence, Some(2));
        mailbox.set_paused(true);
        assert!(!mailbox.publish(packed(4)));
        mailbox.set_paused(false);
        assert!(mailbox.publish(packed(5)));
        assert_eq!(mailbox.receive().await.unwrap().stamp.sequence, Some(5));
        mailbox.close();
        assert!(!mailbox.publish(packed(6)));
        assert_eq!(mailbox.receive().await.unwrap_err(), FrameError::Generation);
    }

    #[tokio::test]
    async fn closing_a_mailbox_wakes_a_waiting_consumer() {
        let mailbox = std::sync::Arc::new(FrameMailbox::new(1).unwrap());
        let consumer = mailbox.clone();
        let task = tokio::spawn(async move { consumer.receive().await });
        tokio::task::yield_now().await;
        mailbox.close();
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err(), FrameError::Generation);
    }
}
