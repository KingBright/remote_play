//! Bounded decoder-array-slice -> shared-texture bridge on the decoder's GPU.
//! One GPU-only copy is counted explicitly; no decoder surface is CPU-mapped.
//! Decoder sample leases are retained until the copy event completes. Shutdown
//! transfers unfinished leases to a bounded process-wide retirement service.
use super::{D3dVideoBuffer, D3dVideoFrame, VideoColor, VideoGeometry, VideoSurfaceStats};
use ::windows::{
    core::Interface,
    Win32::Graphics::{
        Direct3D10::ID3D10Multithread,
        Direct3D11::*,
        Dxgi::{Common::*, IDXGIKeyedMutex},
    },
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    any::Any,
    marker::PhantomData,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, OnceLock,
    },
    time::Instant,
};
const MAX_SLOTS: usize = 4;
const MAX_PROCESS_PENDING: usize = 32;
static ACTIVE_COPIES: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn reserve() -> Option<Self> {
        ACTIVE_COPIES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_PROCESS_PENDING).then_some(n + 1)
            })
            .ok()
            .map(|_| Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE_COPIES.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Pending {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    query: ID3D11Query,
    frame: D3dVideoFrame,
    tag: u64,
    started: Instant,
    _source_lease: Arc<dyn Any + Send + Sync>,
    _permit: Permit,
}
fn complete(context: &ID3D11DeviceContext, query: &ID3D11Query) -> Result<bool> {
    let mut done = 0u32;
    let result = unsafe {
        (Interface::vtable(context).GetData)(
            Interface::as_raw(context),
            Interface::as_raw(query),
            (&mut done as *mut u32).cast(),
            4,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    match result.0 {
        0 => Ok(done != 0),
        1 => Ok(false),
        _ => bail!("decoder GPU copy event failed: {result:?}"),
    }
}
fn retire_service() -> Result<mpsc::Sender<Pending>> {
    static SERVICE: OnceLock<Result<mpsc::Sender<Pending>, String>> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Pending>();
            std::thread::Builder::new()
                .name("rp-gpu-lease-retirement".into())
                .spawn(move || {
                    let mut pending = Vec::<Pending>::with_capacity(MAX_PROCESS_PENDING);
                    loop {
                        if pending.is_empty() {
                            match rx.recv() {
                                Ok(item) => pending.push(item),
                                Err(_) => return,
                            }
                        }
                        while let Ok(item) = rx.try_recv() {
                            pending.push(item);
                        }
                        let mut i = 0;
                        while i < pending.len() {
                            let item = &pending[i];
                            let done = complete(&item.context, &item.query).unwrap_or(false);
                            let removed = unsafe { item.device.GetDeviceRemovedReason() }.is_err();
                            if done || removed {
                                pending.swap_remove(i);
                            } else {
                                i += 1;
                            }
                        }
                        if !pending.is_empty() {
                            std::thread::sleep(std::time::Duration::from_millis(4));
                        }
                    }
                })
                .map(|_| tx)
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .cloned()
        .map_err(|e| anyhow::anyhow!(e.clone()))
}
struct Slot {
    buffer: Arc<D3dVideoBuffer>,
    texture: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
}
/// A decoder frame copied entirely on GPU and ready for native scene composition.
/// Readiness is GPU copy completion, not monitor scan-out or permission to inject input.
pub struct ReadyD3dVideoFrame {
    /// Resource plus pool lease; clones must remain alive while submitted to the GPU.
    pub frame: D3dVideoFrame,
    /// Opaque caller identity, typically the original timestamp or source-frame ID.
    pub tag: u64,
    /// CPU-observed submission instant, not a hardware timestamp.
    pub copy_started: Instant,
    /// CPU observation of GPU completion; includes polling/scheduling delay.
    pub copy_ready: Instant,
}
/// Measured events in the GPU bridge, excluding decoder internals and scan-out.
#[derive(Debug, Default, Clone, Copy)]
pub struct D3dVideoCopyStats {
    /// Actual shared texture allocations, reused across frames.
    pub allocations: u64,
    /// Explicit GPU copies submitted from decoder textures.
    pub gpu_copies: u64,
    /// GPU copy events observed complete.
    pub gpu_completed: u64,
    /// Output frames skipped before copying because all bounded slots were leased.
    pub pool_busy: u64,
    /// Maximum simultaneously pending copy events in this bridge.
    pub pending_high_water: usize,
}
/// Per-decoder-worker GPU bridge. The application must keep it on its worker;
/// no UI thread waits on a decoder or reads video bytes through this API.
pub struct D3dVideoCopyPool {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    slots: Vec<Slot>,
    pending: Vec<Pending>,
    queries: Vec<ID3D11Query>,
    retire: mpsc::Sender<Pending>,
    stats: D3dVideoCopyStats,
    surface_stats: Arc<VideoSurfaceStats>,
    _worker: PhantomData<Rc<()>>,
}
impl D3dVideoCopyPool {
    /// Use the exact decoder device so the copy is ordered with its decode commands.
    pub fn new(device: ID3D11Device) -> Result<Self> {
        let context = unsafe { device.GetImmediateContext()? };
        let protect: ID3D10Multithread = device.cast().or_else(|_| context.cast())?;
        unsafe {
            protect.SetMultithreadProtected(true);
        }
        Ok(Self {
            device,
            context,
            slots: Vec::with_capacity(MAX_SLOTS),
            pending: Vec::with_capacity(MAX_SLOTS),
            queries: Vec::with_capacity(MAX_SLOTS),
            retire: retire_service()?,
            stats: Default::default(),
            surface_stats: Arc::new(Default::default()),
            _worker: PhantomData,
        })
    }
    /// Snapshot of copy/allocation events; it never reports this bridge as zero-copy.
    pub fn stats(&self) -> D3dVideoCopyStats {
        self.stats
    }
    /// Native compositor counters for all buffers produced by this bridge.
    pub fn surface_stats(&self) -> Arc<VideoSurfaceStats> {
        self.surface_stats.clone()
    }
    /// Number of submitted copies awaiting GPU completion.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    /// Number of allocated pool slots, including retired layouts still leased by a view.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
    /// Submit a complete decoder array slice. Returns false on bounded backpressure.
    ///
    /// # Safety
    /// `lease` must own the real decoder sample for this exact texture/subresource,
    /// preventing reuse until released. The sample's decoder must use this same
    /// device/context ordering. No foreign writer may mutate the source outside it.
    pub unsafe fn submit(
        &mut self,
        source: &ID3D11Texture2D,
        subresource: u32,
        geometry: VideoGeometry,
        color: VideoColor,
        lease: Arc<dyn Any + Send + Sync>,
        tag: u64,
    ) -> Result<bool> {
        let source_device = unsafe { source.GetDevice()? };
        ensure!(
            Interface::as_raw(&source_device) == Interface::as_raw(&self.device),
            "native copy source is on another D3D11 device"
        );
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            source.GetDesc(&mut desc);
        }
        ensure!(
            desc.MipLevels == 1
                && subresource < desc.ArraySize
                && desc.SampleDesc.Count == 1
                && desc.Usage == D3D11_USAGE_DEFAULT
                && desc.CPUAccessFlags == 0,
            "invalid decoder texture slice"
        );
        let format = match desc.Format {
            DXGI_FORMAT_NV12 => super::VideoFormat::Nv12,
            DXGI_FORMAT_P010 => super::VideoFormat::P010,
            _ => bail!("decoder copy requires NV12 or P010"),
        };
        geometry.validate(format).map_err(anyhow::Error::msg)?;
        ensure!(
            geometry.coded == [desc.Width, desc.Height],
            "decoder copy geometry does not match allocation"
        );
        let matches = |slot: &Slot| {
            slot.buffer.desc.Width == desc.Width
                && slot.buffer.desc.Height == desc.Height
                && slot.buffer.desc.Format == desc.Format
        };
        // Remove unused previous layouts, but never reclaim a leased frame.
        self.slots
            .retain(|s| matches(s) || Arc::strong_count(&s.buffer) != 1);
        let index = if let Some(i) = self
            .slots
            .iter()
            .position(|s| matches(s) && Arc::strong_count(&s.buffer) == 1)
        {
            i
        } else {
            if self.slots.len() >= MAX_SLOTS || self.pending.len() >= MAX_SLOTS {
                self.stats.pool_busy += 1;
                return Ok(false);
            }
            let copy_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE
                    | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX)
                    .0 as u32,
            };
            let mut texture = None;
            unsafe {
                self.device
                    .CreateTexture2D(&copy_desc, None, Some(&mut texture))?;
            }
            let texture = texture.context("native copy allocation missing")?;
            let mutex = texture.cast()?;
            let buffer = D3dVideoBuffer::from_texture(texture.clone())?;
            self.slots.push(Slot {
                buffer,
                texture,
                mutex,
            });
            self.stats.allocations += 1;
            self.slots.len() - 1
        };
        let Some(permit) = Permit::reserve() else {
            self.stats.pool_busy += 1;
            return Ok(false);
        };
        let query = if let Some(q) = self.queries.pop() {
            q
        } else {
            let mut q = None;
            unsafe {
                self.device.CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut q),
                )?;
            }
            q.context("copy completion query missing")?
        };
        let slot = &self.slots[index];
        let acquired = unsafe {
            (Interface::vtable(&slot.mutex).AcquireSync)(Interface::as_raw(&slot.mutex), 0, 0)
        };
        if acquired.0 == 258 {
            self.queries.push(query);
            self.stats.pool_busy += 1;
            return Ok(false);
        }
        ensure!(
            acquired.0 == 0,
            "native copy resource lock failed: {acquired:?}"
        );
        // Construct all fallible metadata before native commands start. From
        // submission onward the source lease must reach a pending record even
        // if releasing the cross-device synchronization object reports failure.
        let frame = unsafe {
            D3dVideoFrame::from_ready_buffer(
                slot.buffer.clone(),
                geometry,
                color,
                Arc::new(()),
                self.surface_stats.clone(),
            )?
        };
        let started = Instant::now();
        unsafe {
            self.context.CopySubresourceRegion(
                &slot.texture,
                0,
                0,
                0,
                0,
                source,
                subresource,
                None,
            );
            self.context.End(&query);
        }
        let release = unsafe { slot.mutex.ReleaseSync(0) };
        unsafe {
            self.context.Flush();
        }
        // The pending record pins both the source MF sample and target allocation
        // before returning, even if the subsequent release check reports failure.
        self.pending.push(Pending {
            device: self.device.clone(),
            context: self.context.clone(),
            query,
            frame,
            tag,
            started,
            _source_lease: lease,
            _permit: permit,
        });
        self.stats.gpu_copies += 1;
        self.stats.pending_high_water = self.stats.pending_high_water.max(self.pending.len());
        release?;
        Ok(true)
    }
    /// Poll once without blocking; only ready resources leave the pending list.
    pub fn take_ready(&mut self) -> Result<Vec<ReadyD3dVideoFrame>> {
        let mut ready = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if complete(&self.context, &self.pending[i].query)? {
                // Preserve output order, including decode timestamps across frame reuse.
                let item = self.pending.remove(i);
                self.queries.push(item.query.clone());
                self.stats.gpu_completed += 1;
                ready.push(ReadyD3dVideoFrame {
                    frame: item.frame.clone(),
                    tag: item.tag,
                    copy_started: item.started,
                    copy_ready: Instant::now(),
                });
            } else {
                i += 1;
            }
        }
        Ok(ready)
    }
}
impl Drop for D3dVideoCopyPool {
    fn drop(&mut self) {
        // No GUI/worker shutdown blocks for a GPU. The service's global permits
        // cap all outstanding retained copies at 32 even across device loss.
        for item in self.pending.drain(..) {
            if let Err(error) = self.retire.send(item) {
                // The service has no normal shutdown path. If the process is already
                // unwinding, avoid recycling a driver-owned frame unsafely.
                std::mem::forget(error.0);
            }
        }
    }
}
