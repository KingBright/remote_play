//! Immutable Linux-native video metadata and producer-frame ownership. Public
//! construction retains FDs, never pixel bytes. The renderer owns GPU retirement.
use super::{VideoColor, VideoFormat, VideoGeometry, VideoSurfaceStats};
use std::{
    any::Any,
    fs::File,
    os::{
        fd::{BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PlaneKey {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub modifier: u64,
    pub offset: u64,
    pub pitch: u64,
}
#[derive(Debug)]
/// Owned native plane handle and explicit allocation layout, without CPU pixels.
pub struct DmaVideoPlane {
    pub(crate) fd: OwnedFd,
    pub(crate) key: PlaneKey,
}
impl DmaVideoPlane {
    /// Duplicate a borrowed plane FD; preserve its real allocation identity and layout.
    pub fn duplicate(
        fd: BorrowedFd<'_>,
        bytes: u64,
        modifier: u64,
        offset: u64,
        pitch: u64,
    ) -> std::io::Result<Self> {
        let file = File::from(fd.try_clone_to_owned()?);
        let info = file.metadata()?;
        if bytes == 0 || offset >= bytes || pitch == 0 || modifier == u64::MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid native video plane metadata",
            ));
        }
        Ok(Self {
            key: PlaneKey {
                device: info.dev(),
                inode: info.ino(),
                bytes,
                modifier,
                offset,
                pitch,
            },
            fd: file.into(),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AllocationKey {
    pub planes: [PlaneKey; 2],
    pub coded: [u32; 2],
    pub depth: u8,
    pub render_node: (i64, i64),
}
pub(crate) struct Frame {
    pub planes: [DmaVideoPlane; 2],
    pub key: AllocationKey,
    pub geometry: VideoGeometry,
    pub color: VideoColor,
    pub format: VideoFormat,
    pub stats: Arc<VideoSurfaceStats>,
    pub submitted: AtomicBool,
    pub _lease: Arc<dyn Any + Send + Sync>,
}
#[derive(Clone)]
/// Shared immutable decoded frame, kept alive through the GPU submission lifetime.
pub struct DmaVideoFrame(pub(crate) Arc<Frame>);
impl std::fmt::Debug for DmaVideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DmaVideoFrame")
            .field("geometry", &self.0.geometry)
            .field("format", &self.0.format)
            .finish()
    }
}
impl PartialEq for DmaVideoFrame {
    fn eq(&self, rhs: &Self) -> bool {
        Arc::ptr_eq(&self.0, &rhs.0)
    }
}
impl Eq for DmaVideoFrame {}
impl DmaVideoFrame {
    /// # Safety
    /// Planes must describe the supplied native decoded frame. Producer writes
    /// must have completed before publication (e.g. synchronized DIRECT export).
    /// `lease` must prevent overwrite/recycling until all clones are released.
    /// Only negotiated SDR NV12/P010 is supported; do not label HDR as SDR.
    pub unsafe fn from_ready_planes(
        planes: [DmaVideoPlane; 2],
        geometry: VideoGeometry,
        format: VideoFormat,
        color: VideoColor,
        render_node: (i64, i64),
        lease: Arc<dyn Any + Send + Sync>,
        stats: Arc<VideoSurfaceStats>,
    ) -> Result<Self, String> {
        geometry.validate(format).map_err(str::to_owned)?;
        let depth = match format {
            VideoFormat::Nv12 => 8,
            VideoFormat::P010 => 10,
            _ => return Err("Linux native video currently requires NV12/P010 planes".into()),
        };
        if render_node.0 < 0 || render_node.1 < 0 {
            return Err("missing decoder render-node identity".into());
        }
        let row = u64::from(geometry.coded[0]) * if depth == 8 { 1 } else { 2 };
        for p in &planes {
            if p.key.pitch < row {
                return Err("native plane pitch shorter than coded row".into());
            }
        }
        let key = AllocationKey {
            planes: [planes[0].key.clone(), planes[1].key.clone()],
            coded: geometry.coded,
            depth,
            render_node,
        };
        Ok(Self(Arc::new(Frame {
            planes,
            key,
            geometry,
            color,
            format,
            stats,
            submitted: AtomicBool::new(false),
            _lease: lease,
        })))
    }
    /// Source crop, coded size, orientation and pixel aspect ratio.
    pub fn geometry(&self) -> VideoGeometry {
        self.0.geometry
    }
    /// Surface submission/import counters; none of these imply display scan-out.
    pub fn stats(&self) -> &Arc<VideoSurfaceStats> {
        &self.0.stats
    }
    /// Whether this exact frame has entered a submitted GPU command batch.
    pub fn was_submitted(&self) -> bool {
        self.0.submitted.load(Ordering::Acquire)
    }
}

#[cfg(feature = "native-video-validation")]
impl DmaVideoFrame {
    /// Render a retained synthetic frame with the production shader into a small
    /// offscreen target. Requires RP_BLADE_NATIVE_PIXEL_TEST=1 and is not part of
    /// playback. The returned 96x64 RGBA pixels are validation-only readback.
    pub fn validation_pixels(&self, rotation: super::Rotation) -> Result<Vec<[u8; 4]>, String> {
        use std::os::fd::AsFd;
        let copy = |i: usize| {
            let p = &self.0.planes[i];
            DmaVideoPlane::duplicate(
                p.fd.as_fd(),
                p.key.bytes,
                p.key.modifier,
                p.key.offset,
                p.key.pitch,
            )
            .map_err(|e| e.to_string())
        };
        let mut geometry = self.geometry();
        geometry.rotation = rotation;
        let frame = unsafe {
            Self::from_ready_planes(
                [copy(0)?, copy(1)?],
                geometry,
                self.0.format,
                self.0.color,
                self.0.key.render_node,
                self.0._lease.clone(),
                Arc::new(VideoSurfaceStats::default()),
            )
        }?;
        crate::platform::native_video_validation_pixels(frame)
    }
}

pub(crate) static LIVE_IMPORTED_ALLOCATIONS:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(0);
/// Count this process's live imported video allocations, including in-flight
/// retired frames. Diagnostic only; not a per-session input or readiness signal.
pub fn linux_video_imports_live()->u64{LIVE_IMPORTED_ALLOCATIONS.load(Ordering::Acquire)}
