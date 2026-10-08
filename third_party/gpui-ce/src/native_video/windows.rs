use super::{VideoColor, VideoFormat, VideoGeometry, VideoSurfaceStats};
use anyhow::{bail, ensure, Context, Result};
use std::{
    any::Any,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use windows::{
    core::Interface,
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
};
static NEXT_BUFFER: AtomicU64 = AtomicU64::new(1);

/// A shared, shader-readable GPU allocation. Imports are cached by this stable
/// object ID, never by a recyclable COM pointer or a per-frame timestamp.
pub struct D3dVideoBuffer {
    pub(crate) id: u64,
    pub(crate) handle: HANDLE,
    pub(crate) desc: D3D11_TEXTURE2D_DESC,
    pub(crate) adapter_luid: (u32, i32),
    pub(crate) format: VideoFormat,
    _texture: ID3D11Texture2D,
}
// NT handles and D3D resource references are thread-safe; GPU access is always
// through the keyed mutex. No immediate device context is stored in this type.
unsafe impl Send for D3dVideoBuffer {}
unsafe impl Sync for D3dVideoBuffer {}
impl fmt::Debug for D3dVideoBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3dVideoBuffer")
            .field("id", &self.id)
            .field("format", &self.format)
            .field("extent", &[self.desc.Width, self.desc.Height])
            .finish()
    }
}
impl Drop for D3dVideoBuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}
impl D3dVideoBuffer {
    /// Export one already-created GPU allocation, without copying its pixels.
    /// Unsupported format/layout/device sharing is rejected, never emulated on CPU.
    pub fn from_texture(texture: ID3D11Texture2D) -> Result<Arc<Self>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            texture.GetDesc(&mut desc);
        }
        let format = match desc.Format {
            DXGI_FORMAT_NV12 => VideoFormat::Nv12,
            DXGI_FORMAT_P010 => VideoFormat::P010,
            DXGI_FORMAT_B8G8R8A8_UNORM => VideoFormat::Bgra8,
            _ => bail!("unsupported native-video DXGI format"),
        };
        VideoGeometry {
            coded: [desc.Width, desc.Height],
            visible: [0, 0, desc.Width, desc.Height],
            pixel_aspect: [1, 1],
            rotation: super::Rotation::R0,
        }
        .validate(format)
        .map_err(anyhow::Error::msg)?;
        ensure!(
            desc.ArraySize == 1
                && desc.MipLevels == 1
                && desc.SampleDesc.Count == 1
                && desc.Usage == D3D11_USAGE_DEFAULT,
            "native-video export requires a single GPU texture with no CPU access"
        );
        ensure!(
            desc.CPUAccessFlags == 0 && (desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32) != 0,
            "texture must remain GPU-only and shader-readable"
        );
        let flags =
            (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX).0 as u32;
        ensure!(
            desc.MiscFlags & flags == flags,
            "native-video resource requires NT sharing and keyed synchronization"
        );
        let device = unsafe { texture.GetDevice()? };
        let dxgi: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi.GetAdapter()? };
        let info = unsafe { adapter.GetDesc()? };
        let resource: IDXGIResource1 = texture.cast()?;
        let handle = unsafe {
            resource.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                None,
            )?
        };
        Ok(Arc::new(Self {
            id: NEXT_BUFFER.fetch_add(1, Ordering::Relaxed),
            handle,
            desc,
            adapter_luid: (info.AdapterLuid.LowPart, info.AdapterLuid.HighPart),
            format,
            _texture: texture,
        }))
    }
    /// Stable process-local resource identifier, distinct from per-frame timestamps.
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Validated backing texture format.
    pub fn format(&self) -> VideoFormat {
        self.format
    }
}

pub(crate) struct FrameInner {
    pub buffer: Arc<D3dVideoBuffer>,
    pub geometry: VideoGeometry,
    pub color: VideoColor,
    pub stats: Arc<VideoSurfaceStats>,
    pub submitted: AtomicBool,
    pub _producer_lease: Arc<dyn Any + Send + Sync>,
}
/// Immutable published frame. Keep the producer's native-frame lease, not its bytes.
#[derive(Clone)]
pub struct D3dVideoFrame(pub(crate) Arc<FrameInner>);
impl fmt::Debug for D3dVideoFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3dVideoFrame")
            .field("buffer", &self.0.buffer)
            .field("geometry", &self.0.geometry)
            .finish()
    }
}
impl PartialEq for D3dVideoFrame {
    fn eq(&self, rhs: &Self) -> bool {
        Arc::ptr_eq(&self.0, &rhs.0)
    }
}
impl Eq for D3dVideoFrame {}
impl D3dVideoFrame {
    /// Publish a completed decoder/producer frame.
    ///
    /// # Safety
    /// The lease must prevent this buffer's mutation/recycling while a frame is
    /// alive. Every writer must also use this resource's keyed mutex (key 0),
    /// flush producer commands and release key 0 before publishing. This native
    /// synchronization contract cannot be checked from a COM pointer alone.
    /// HDR must be tone/gamut-mapped by a negotiated GPU path before using this
    /// SDR API; passing PQ/HLG values as ordinary SDR is not supported.
    pub unsafe fn from_ready_buffer(
        buffer: Arc<D3dVideoBuffer>,
        geometry: VideoGeometry,
        color: VideoColor,
        lease: Arc<dyn Any + Send + Sync>,
        stats: Arc<VideoSurfaceStats>,
    ) -> Result<Self> {
        geometry
            .validate(buffer.format)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            geometry.coded == [buffer.desc.Width, buffer.desc.Height],
            "video metadata/resource dimensions disagree"
        );
        Ok(Self(Arc::new(FrameInner {
            buffer,
            geometry,
            color,
            stats,
            submitted: AtomicBool::new(false),
            _producer_lease: lease,
        })))
    }
    /// Create a new view of the same immutable native allocation without copying
    /// pixels or weakening the original producer lease.
    pub fn with_geometry(&self, geometry: VideoGeometry) -> Result<Self> {
        geometry
            .validate(self.0.buffer.format)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            geometry.coded == self.0.geometry.coded,
            "reframing cannot change resource dimensions"
        );
        Ok(Self(Arc::new(FrameInner {
            buffer: self.0.buffer.clone(),
            geometry,
            color: self.0.color,
            stats: self.0.stats.clone(),
            submitted: AtomicBool::new(false),
            _producer_lease: self.0._producer_lease.clone(),
        })))
    }

    /// Producer-declared crop, aspect and orientation of this frame.
    pub fn geometry(&self) -> VideoGeometry {
        self.0.geometry
    }
    /// Whether this exact immutable frame/view reached a native draw command.
    /// A previously submitted frame from the same pool cannot satisfy this check.
    /// This is submission evidence only, not scan-out or presentation latency.
    pub fn was_submitted(&self) -> bool {
        self.0.submitted.load(Ordering::Acquire)
    }

    /// Renderer-side counters for this frame stream; not scan-out evidence.
    pub fn stats(&self) -> &Arc<VideoSurfaceStats> {
        &self.0.stats
    }
}

/// Test-only synthetic frame producer. Uploads a generated pattern once, not a
/// production decode path. No screen, network, microphone or user file is read.
#[cfg(feature = "test-support")]
pub fn synthetic_frame(
    format: VideoFormat,
    width: u32,
    height: u32,
    color: VideoColor,
) -> Result<D3dVideoFrame> {
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
    ensure!(
        width >= 16
            && height >= 16
            && width <= 4096
            && height <= 4096
            && width % 2 == 0
            && height % 2 == 0,
        "invalid synthetic extent"
    );
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            ::windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.context("no hardware test device")?;
    let context = context.context("no test context")?;
    let (mut data, pitch, dxgi) = match format {
        VideoFormat::Nv12 => (
            vec![128u8; (width * height * 3 / 2) as usize],
            width,
            DXGI_FORMAT_NV12,
        ),
        VideoFormat::P010 => (
            vec![0u8; (width * height * 3) as usize],
            width * 2,
            DXGI_FORMAT_P010,
        ),
        VideoFormat::Bgra8 => (
            vec![0u8; (width * height * 4) as usize],
            width * 4,
            DXGI_FORMAT_B8G8R8A8_UNORM,
        ),
    };
    let values = [16u8, 235, 64, 180];
    for y in 0..height {
        for x in 0..width {
            let quadrant = ((x >= width / 2) as usize) + 2 * ((y >= height / 2) as usize);
            let v = values[quadrant];
            let index = (y * width + x) as usize;
            match format {
                VideoFormat::Nv12 => data[index] = v,
                VideoFormat::P010 => {
                    let bytes = (u16::from(v) * 256).to_le_bytes();
                    data[index * 2..index * 2 + 2].copy_from_slice(&bytes);
                }
                VideoFormat::Bgra8 => {
                    data[index * 4..index * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
    }
    if format == VideoFormat::P010 {
        for chunk in data[(width * height * 2) as usize..].chunks_exact_mut(2) {
            chunk.copy_from_slice(&32768u16.to_le_bytes());
        }
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: dxgi,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0
            as u32,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: data.as_ptr().cast(),
        SysMemPitch: pitch,
        SysMemSlicePitch: 0,
    };
    let mut texture = None;
    unsafe {
        device.CreateTexture2D(&desc, Some(&init), Some(&mut texture))?;
        context.Flush();
    }
    let buffer = D3dVideoBuffer::from_texture(texture.context("test texture missing")?)?;
    let geometry = VideoGeometry {
        coded: [width, height],
        visible: [0, 0, width, height],
        pixel_aspect: [1, 1],
        rotation: super::Rotation::R0,
    };
    // The only producer is this immutable one-time initializer; the resource is
    // retained for the entire frame lifetime and no writes occur after return.
    unsafe {
        D3dVideoFrame::from_ready_buffer(
            buffer,
            geometry,
            color,
            Arc::new(()),
            Arc::new(VideoSurfaceStats::default()),
        )
    }
}
