//! Actual VAAPI decode and synchronized DIRECT DRM export behind a small C ABI.
//! The caller must keep this decoder on a dedicated native worker. All exported
//! frames retain their FFmpeg references; no CPU pixel copy is exposed here.
use anyhow::{Context, Result, bail, ensure};
use gpui::native_video::*;
use std::{
    ffi::{CStr, CString, c_void},
    marker::PhantomData,
    os::{fd::BorrowedFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Object {
    fd: i32,
    bytes: u64,
    modifier: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Plane {
    object_index: u32,
    offset: u64,
    pitch: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Layer {
    fourcc: u32,
    plane_count: u32,
    planes: [Plane; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct FrameInfo {
    abi_version: u32,
    width: u32,
    height: u32,
    allocation_width: u32,
    allocation_height: u32,
    crop_left: u64,
    crop_top: u64,
    crop_right: u64,
    crop_bottom: u64,
    pixel_format: i32,
    range: i32,
    primaries: i32,
    transfer: i32,
    matrix: i32,
    chroma: i32,
    aspect_num: i32,
    aspect_den: i32,
    pts: i64,
    object_count: u32,
    layer_count: u32,
    objects: [Object; 4],
    layers: [Layer; 4],
}
unsafe extern "C" {
    fn rp_linux_decoder_create(node: *const i8, out: *mut *mut c_void) -> i32;
    fn rp_linux_decoder_destroy(decoder: *mut *mut c_void);
    fn rp_linux_decoder_submit(decoder: *mut c_void, data: *const u8, len: usize, pts: i64) -> i32;
    fn rp_linux_decoder_receive(decoder: *mut c_void, out: *mut *mut c_void) -> i32;
    fn rp_linux_frame_info(frame: *const c_void) -> *const FrameInfo;
    fn rp_linux_frame_release(frame: *mut *mut c_void);
    fn rp_linux_frame_info_size() -> usize;
    fn rp_linux_error_string(error: i32, text: *mut i8, len: usize);
}
static FRAME_LEASES: AtomicU64 = AtomicU64::new(0);
pub fn active_native_frame_leases() -> u64 {
    FRAME_LEASES.load(Ordering::Acquire)
}
fn check(code: i32) -> Result<i32> {
    if code < 0 {
        let mut text = [0i8; 256];
        unsafe {
            rp_linux_error_string(code, text.as_mut_ptr(), text.len());
        }
        bail!(
            "native HEVC {code}: {}",
            unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy()
        );
    }
    Ok(code)
}
struct Lease(*mut c_void);
unsafe impl Send for Lease {}
unsafe impl Sync for Lease {}
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            rp_linux_frame_release(&mut self.0);
        }
        FRAME_LEASES.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct LinuxHevcDecoder {
    raw: *mut c_void,
    node: (i64, i64),
    stats: Arc<VideoSurfaceStats>,
    _thread: PhantomData<Rc<()>>,
}
impl Drop for LinuxHevcDecoder {
    fn drop(&mut self) {
        unsafe {
            rp_linux_decoder_destroy(&mut self.raw);
        }
    }
}
pub struct DecodedNative {
    pub tag: u64,
    pub frame: DmaVideoFrame,
}
impl LinuxHevcDecoder {
    pub fn new(node: &Path, stats: Arc<VideoSurfaceStats>) -> Result<Self> {
        ensure!(
            unsafe { rp_linux_frame_info_size() } == std::mem::size_of::<FrameInfo>(),
            "native frame ABI layout mismatch"
        );
        use std::os::unix::ffi::OsStrExt;
        let metadata = std::fs::metadata(node).context("native decode render node unavailable")?;
        let rdev = metadata.rdev();
        ensure!(rdev != 0, "native decode path is not a GPU device");
        let name = CString::new(node.as_os_str().as_bytes())?;
        let mut raw = std::ptr::null_mut();
        check(unsafe { rp_linux_decoder_create(name.as_ptr(), &mut raw) })?;
        Ok(Self {
            raw,
            node: (i64::from(libc::major(rdev)), i64::from(libc::minor(rdev))),
            stats,
            _thread: PhantomData,
        })
    }
    /// false is decoder input backpressure; caller drains output before retrying.
    pub fn submit(&mut self, bytes: &[u8], tag: u64) -> Result<bool> {
        ensure!(tag <= i64::MAX as u64, "native frame tag overflow");
        let result =
            unsafe { rp_linux_decoder_submit(self.raw, bytes.as_ptr(), bytes.len(), tag as i64) };
        if result == -libc::EAGAIN {
            return Ok(false);
        }
        check(result)?;
        Ok(true)
    }
    pub fn receive(&mut self) -> Result<Option<DecodedNative>> {
        let mut raw = std::ptr::null_mut();
        if check(unsafe { rp_linux_decoder_receive(self.raw, &mut raw) })? == 0 {
            return Ok(None);
        }
        FRAME_LEASES.fetch_add(1, Ordering::AcqRel);
        let lease = Arc::new(Lease(raw));
        let i = unsafe { *rp_linux_frame_info(raw) };
        ensure!(
            i.abi_version == 1
                && i.layer_count == 2
                && i.object_count > 0
                && i.object_count <= 4
                && i.pts >= 0,
            "invalid native decoded frame metadata"
        );
        let format = match i.pixel_format {
            8 => VideoFormat::Nv12,
            10 => VideoFormat::P010,
            _ => bail!("unsupported native HEVC pixel format"),
        };
        // No automatic HDR/gamut reduction or inferred color matrix. These are actual
        // decoded stream fields; a unsupported/missing description remains visible.
        ensure!(
            i.primaries == 1 && matches!(i.transfer, 1 | 13),
            "native video requires declared SDR BT.709/sRGB; source color is unspecified or unsupported"
        );
        let matrix = match i.matrix {
            1 => Matrix::Bt709,
            5 | 6 => Matrix::Bt601,
            _ => bail!("source did not declare a supported YUV matrix"),
        };
        let range = match i.range {
            1 => Range::Limited,
            2 => Range::Full,
            _ => bail!("source did not declare its video range"),
        };
        let chroma = match i.chroma {
            0 | 1 => ChromaLocation::Left,
            2 => ChromaLocation::Center,
            _ => bail!("unsupported progressive chroma siting"),
        };
        let make = |plane: usize| -> Result<DmaVideoPlane> {
            let layer = i.layers[plane];
            ensure!(
                layer.plane_count == 1,
                "native auxiliary modifier planes are not supported on this decoder/render pair"
            );
            let p = layer.planes[0];
            ensure!(
                p.object_index < i.object_count,
                "native plane refers to missing object"
            );
            let expected = match (i.pixel_format, plane) {
                (8, 0) => *b"R8  ",
                (8, 1) => *b"GR88",
                (10, 0) => *b"R16 ",
                _ => *b"GR32",
            };
            ensure!(
                layer.fourcc.to_le_bytes() == expected,
                "native component layout differs from declared video format"
            );
            let obj = i.objects[p.object_index as usize];
            Ok(DmaVideoPlane::duplicate(
                unsafe { BorrowedFd::borrow_raw(obj.fd) },
                obj.bytes,
                obj.modifier,
                p.offset,
                p.pitch,
            )?)
        };
        let geometry = VideoGeometry {
            coded: [i.allocation_width, i.allocation_height],
            visible: [
                i.crop_left as u32,
                i.crop_top as u32,
                (u64::from(i.width) - i.crop_left - i.crop_right) as u32,
                (u64::from(i.height) - i.crop_top - i.crop_bottom) as u32,
            ],
            pixel_aspect: if i.aspect_num > 0 && i.aspect_den > 0 {
                [i.aspect_num as u32, i.aspect_den as u32]
            } else {
                [1, 1]
            },
            rotation: Rotation::R0,
        };
        let frame = unsafe {
            DmaVideoFrame::from_ready_planes(
                [make(0)?, make(1)?],
                geometry,
                format,
                VideoColor {
                    matrix,
                    range,
                    chroma,
                },
                self.node,
                lease,
                self.stats.clone(),
            )
        }
        .map_err(anyhow::Error::msg)?;
        Ok(Some(DecodedNative {
            tag: i.pts as u64,
            frame,
        }))
    }
}
/// Never silently choose another adapter on a hybrid-GPU machine. The importer
/// also checks the actual render node. Single-GPU systems need no configuration.
pub fn render_node() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("REMOTE_PLAY_LINUX_RENDER_NODE") {
        let path = PathBuf::from(path);
        ensure!(
            path.is_absolute()
                && path.starts_with("/dev/dri")
                && path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("renderD")),
            "invalid Linux render-node override"
        );
        return Ok(path);
    }
    let mut nodes = std::fs::read_dir("/dev/dri")
        .context("No native GPU render node available")?
        .filter_map(|entry|entry.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .map(|e| e.path())
        .collect::<Vec<_>>();
    nodes.sort();
    ensure!(
        nodes.len() == 1,
        "native decoder needs an unambiguous render node on this multi-GPU host"
    );
    Ok(nodes.remove(0))
}
