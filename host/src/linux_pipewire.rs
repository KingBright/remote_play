//! Platform independent contract for a single restricted PipeWire stream.
//! Native mappings are borrowed only during process(); only checked owned YUV
//! and local generation/negotiation stamps may enter the one-frame mailbox.
use crate::linux_frame::{
    BorrowedPlane, FrameError, FrameFormat, FrameMailbox, FrameStamp, OwnedFrame, PixelFormat,
    Transform,
};
use std::sync::Arc;

#[cfg(target_os = "linux")]
pub(crate) mod native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerError {
    Format,
    Buffer,
    Closed,
    Revision,
}

pub(crate) struct CopyContract {
    generation: u64,
    revision: u64,
    format: Option<FrameFormat>,
    mailbox: Arc<FrameMailbox>,
}

/// Only mapped CPU memory is admitted. A DMA-BUF descriptor is not a byte slice.
pub(crate) struct MappedChunk<'a> {
    pub bytes: &'a [u8],
    pub flags: i32,
    pub mapping_offset: u32,
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
}

/// SPA EMPTY is neutral media, not evidence that the mapped bytes contain
/// pixels. Recycled storage may still contain a previous window's image.
pub(crate) fn validate_chunk_flags(flags: i32) -> Result<(), WorkerError> {
    // SPA_CHUNK_FLAG_CORRUPTED (bit 0) and SPA_CHUNK_FLAG_EMPTY (bit 1).
    // libspa 0.10.1 names only CORRUPTED, but retains the other raw bits.
    if flags & 0b11 != 0 {
        return Err(WorkerError::Buffer);
    }
    Ok(())
}

impl CopyContract {
    pub(crate) fn new(generation: u64, mailbox: Arc<FrameMailbox>) -> Self {
        Self {
            generation,
            revision: 0,
            format: None,
            mailbox,
        }
    }
    pub(crate) fn renegotiate(&mut self, format: Option<FrameFormat>) -> Result<u64, WorkerError> {
        self.revision = self.revision.checked_add(1).ok_or(WorkerError::Revision)?;
        self.format = None;
        if !self.mailbox.set_format_revision(self.revision) {
            return Err(WorkerError::Closed);
        }
        if let Some(format) = format {
            if !matches!(format.pixel_format, PixelFormat::Nv12 | PixelFormat::I420)
                || format.transform != Transform::Identity
                || format.width == 0
                || format.height == 0
                || format.width > crate::linux_frame::MAX_DIMENSION
                || format.height > crate::linux_frame::MAX_DIMENSION
                || format.width % 2 != 0
                || format.height % 2 != 0
                || u64::from(format.width) * u64::from(format.height)
                    > crate::linux_frame::MAX_PIXELS
            {
                return Err(WorkerError::Format);
            }
            self.format = Some(format);
        }
        Ok(self.revision)
    }
    pub(crate) fn is_negotiated(&self) -> bool {
        self.format.is_some()
    }
    pub(crate) fn discontinuity(&mut self) -> Result<(), WorkerError> {
        self.renegotiate(self.format).map(|_| ())
    }
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn copy(
        &self,
        revision: u64,
        chunks: &[MappedChunk<'_>],
        crop: Option<crate::linux_frame::Crop>,
        transform: Transform,
        sequence: Option<u64>,
        pts: Option<i64>,
        arrival: u64,
    ) -> Result<OwnedFrame, WorkerError> {
        if self.mailbox.is_closed() {
            return Err(WorkerError::Closed);
        }
        if revision != self.revision {
            return Err(WorkerError::Revision);
        }
        let mut format = self.format.ok_or(WorkerError::Format)?;
        for chunk in chunks {
            validate_chunk_flags(chunk.flags)?;
        }
        format.crop = crop;
        format.transform = transform;
        let planes = split_planes(format, chunks).map_err(|_| WorkerError::Buffer)?;
        let frame = OwnedFrame::copy_from(
            format,
            &planes,
            FrameStamp {
                generation: self.generation,
                format_revision: revision,
                sequence,
                pipewire_pts_ns: pts,
                arrival_ts_us: arrival,
            },
        )
        .map_err(|_| WorkerError::Buffer)?;
        crate::linux_raw_encode::validate_native_input(&frame).map_err(|_| WorkerError::Format)?;
        Ok(frame)
    }
}

fn split_planes<'a>(
    format: FrameFormat,
    chunks: &[MappedChunk<'a>],
) -> Result<Vec<BorrowedPlane<'a>>, FrameError> {
    let count = match format.pixel_format {
        PixelFormat::Nv12 => 2,
        PixelFormat::I420 => 3,
        _ => return Err(FrameError::PlaneCount),
    };
    let borrow = |chunk: &MappedChunk<'a>| BorrowedPlane {
        data: chunk.bytes,
        mapping_offset: chunk.mapping_offset,
        chunk_offset: chunk.offset,
        chunk_size: chunk.size,
        stride: chunk.stride,
    };
    if chunks.len() == count {
        return Ok(chunks.iter().map(borrow).collect());
    }
    if chunks.len() != 1 {
        return Err(FrameError::PlaneCount);
    }
    // SPA video raw single-block layout: full Y stride, then interleaved UV or
    // two half-stride chroma planes. Reject ambiguous odd I420 strides.
    let chunk = &chunks[0];
    let stride = u32::try_from(chunk.stride).map_err(|_| FrameError::Stride)?;
    if stride < format.width || (count == 3 && stride % 2 != 0) {
        return Err(FrameError::Stride);
    }
    let y_size = stride
        .checked_mul(format.height)
        .ok_or(FrameError::Bounds)?;
    let chroma_stride = if count == 2 { stride } else { stride / 2 };
    let c_size = chroma_stride
        .checked_mul(format.height / 2)
        .ok_or(FrameError::Bounds)?;
    // The final chroma row need not include its unused trailing padding.
    let chroma_width = if count == 2 {
        format.width
    } else {
        format.width / 2
    };
    let last_span = chroma_stride
        .checked_mul(format.height / 2 - 1)
        .and_then(|bytes| bytes.checked_add(chroma_width))
        .ok_or(FrameError::Bounds)?;
    let total = y_size
        .checked_add(
            c_size
                .checked_mul((count - 2) as u32)
                .ok_or(FrameError::Bounds)?,
        )
        .and_then(|bytes| bytes.checked_add(last_span))
        .ok_or(FrameError::Bounds)?;
    if total > chunk.size
        || chunk
            .offset
            .checked_add(chunk.size)
            .is_none_or(|end| end as usize > chunk.bytes.len())
    {
        return Err(FrameError::Bounds);
    }
    let mut result = vec![BorrowedPlane {
        chunk_size: y_size,
        ..borrow(chunk)
    }];
    for index in 0..count - 1 {
        result.push(BorrowedPlane {
            data: chunk.bytes,
            mapping_offset: chunk.mapping_offset,
            chunk_offset: chunk
                .offset
                .checked_add(y_size)
                .and_then(|offset| offset.checked_add(index as u32 * c_size))
                .ok_or(FrameError::Bounds)?,
            chunk_size: chunk
                .size
                .saturating_sub(y_size + index as u32 * c_size)
                .min(c_size),
            stride: chroma_stride as i32,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
