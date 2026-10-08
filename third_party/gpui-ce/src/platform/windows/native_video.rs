//! D3D11 GPU-only surface compositor. All waiting is zero-timeout. Import and query
//! pools are bounded; only a small uniform buffer is mapped, never a video image.
use crate::{
    native_video::{ChromaLocation, D3dVideoBuffer, D3dVideoFrame, VideoFormat},
    PaintSurface,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::VecDeque,
    sync::atomic::Ordering,
    sync::{Arc, OnceLock, Weak},
};
use windows::{
    core::{Interface, PCSTR},
    Win32::Graphics::{
        Direct3D::Fxc::*,
        Direct3D::*,
        Direct3D11::*,
        Dxgi::{Common::*, *},
    },
};
const MAX_IMPORTS: usize = 12;
const MAX_PENDING: usize = 6;
struct Imported {
    id: u64,
    owner: Weak<D3dVideoBuffer>,
    _texture: ID3D11Texture2D,
    y: ID3D11ShaderResourceView,
    uv: Option<ID3D11ShaderResourceView>,
    mutex: IDXGIKeyedMutex,
}
struct Pending {
    query: ID3D11Query,
    frame: D3dVideoFrame,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
    target: [f32; 4],
    clip: [f32; 4],
    uv0: [f32; 4],
    uv1: [f32; 4],
    clamp: [f32; 4],
    red: [f32; 4],
    green: [f32; 4],
    blue: [f32; 4],
    view: [f32; 4],
}
pub(super) struct VideoPipeline {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    params: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    imports: VecDeque<Imported>,
    pending: Vec<Pending>,
    free_queries: Vec<ID3D11Query>,
}
fn shader_bytes(entry: &'static [u8], target: &'static [u8]) -> Result<Vec<u8>> {
    let source = include_str!("native_video.hlsl");
    let mut blob = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            PCSTR(entry.as_ptr()),
            PCSTR(target.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut blob,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let message = errors
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        bail!("native video shader failed: {error}: {message}");
    }
    let b = blob.context("native shader compiler returned no shader")?;
    Ok(unsafe {
        std::slice::from_raw_parts(b.GetBufferPointer().cast(), b.GetBufferSize()).to_vec()
    })
}
fn keyed_acquire(mutex: &IDXGIKeyedMutex) -> Result<bool> {
    // windows-rs Result<()> discards nonnegative HRESULT values, including
    // WAIT_TIMEOUT / WAIT_ABANDONED. Check the raw status rather than is_ok().
    let status = unsafe { (Interface::vtable(mutex).AcquireSync)(Interface::as_raw(mutex), 0, 0) };
    match status.0 {
        0 => Ok(true),
        258 => Ok(false),
        128 => bail!("shared video resource abandoned; recreate it"),
        _ => Err(anyhow::anyhow!(
            "keyed mutex returned HRESULT 0x{:08x}",
            status.0 as u32
        )),
    }
}
fn query_done(context: &ID3D11DeviceContext, query: &ID3D11Query) -> Result<bool> {
    let mut completed = 0u32;
    let status = unsafe {
        (Interface::vtable(context).GetData)(
            Interface::as_raw(context),
            Interface::as_raw(query),
            (&mut completed as *mut u32).cast(),
            4,
            D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
        )
    };
    match status.0 {
        0 => Ok(completed != 0),
        1 => Ok(false),
        _ => Err(anyhow::anyhow!("native video fence failed: {status:?}")),
    }
}
struct Acquired<'a>(&'a IDXGIKeyedMutex);
impl Drop for Acquired<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.ReleaseSync(0);
        }
    }
}
fn import(device: &ID3D11Device, frame: &D3dVideoFrame) -> Result<Imported> {
    let buffer = &frame.0.buffer;
    let dxgi: IDXGIDevice = device.cast()?;
    let adapter = unsafe { dxgi.GetAdapter()? };
    let desc = unsafe { adapter.GetDesc()? };
    ensure!(
        buffer.adapter_luid == (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
        "decoder and renderer use different GPU adapters; no CPU fallback"
    );
    let device1: ID3D11Device1 = device.cast()?;
    let texture: ID3D11Texture2D = unsafe { device1.OpenSharedResource1(buffer.handle)? };
    let mut actual = D3D11_TEXTURE2D_DESC::default();
    unsafe {
        texture.GetDesc(&mut actual);
    }
    ensure!(
        actual.Width == buffer.desc.Width
            && actual.Height == buffer.desc.Height
            && actual.Format == buffer.desc.Format
            && actual.ArraySize == 1
            && actual.MipLevels == 1,
        "imported video resource layout changed"
    );
    let view = |format: DXGI_FORMAT| -> Result<ID3D11ShaderResourceView> {
        let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                },
            },
        };
        let mut view = None;
        unsafe {
            device.CreateShaderResourceView(&texture, Some(&desc), Some(&mut view))?;
        }
        view.context("missing native video SRV")
    };
    let (y, uv) = match buffer.format {
        VideoFormat::Nv12 => (
            view(DXGI_FORMAT_R8_UNORM)?,
            Some(view(DXGI_FORMAT_R8G8_UNORM)?),
        ),
        VideoFormat::P010 => (
            view(DXGI_FORMAT_R16_UNORM)?,
            Some(view(DXGI_FORMAT_R16G16_UNORM)?),
        ),
        VideoFormat::Bgra8 => (view(DXGI_FORMAT_B8G8R8A8_UNORM)?, None),
    };
    let mutex = texture.cast()?;
    frame.0.stats.imports.fetch_add(1, Ordering::Relaxed);
    Ok(Imported {
        id: buffer.id,
        owner: Arc::downgrade(buffer),
        _texture: texture,
        y,
        uv,
        mutex,
    })
}
impl VideoPipeline {
    pub(super) fn new(device: &ID3D11Device) -> Result<Self> {
        static VERTEX: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
        static PIXEL: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
        let vs = VERTEX
            .get_or_init(|| shader_bytes(b"video_vertex\0", b"vs_4_1\0").map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.clone()))?;
        let ps = PIXEL
            .get_or_init(|| {
                shader_bytes(b"video_fragment\0", b"ps_4_1\0").map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!(e.clone()))?;
        let (mut vertex, mut pixel, mut params, mut sampler, mut blend) =
            (None, None, None, None, None);
        unsafe {
            device.CreateVertexShader(vs, None, Some(&mut vertex))?;
            device.CreatePixelShader(ps, None, Some(&mut pixel))?;
            device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: std::mem::size_of::<Params>() as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut params),
            )?;
            device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: D3D11_FLOAT32_MAX,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8;
            device.CreateBlendState(&desc, Some(&mut blend))?;
        }
        Ok(Self {
            vertex: vertex.context("vertex missing")?,
            pixel: pixel.context("pixel missing")?,
            params: params.context("uniform missing")?,
            sampler: sampler.context("sampler missing")?,
            blend: blend.context("blend missing")?,
            imports: VecDeque::with_capacity(MAX_IMPORTS),
            pending: Vec::with_capacity(MAX_PENDING),
            free_queries: Vec::with_capacity(MAX_PENDING),
        })
    }
    fn reap(&mut self, context: &ID3D11DeviceContext) -> Result<()> {
        let mut i = 0;
        while i < self.pending.len() {
            if query_done(context, &self.pending[i].query)? {
                let finished = self.pending.swap_remove(i);
                finished
                    .frame
                    .0
                    .stats
                    .gpu_completed
                    .fetch_add(1, Ordering::Relaxed);
                self.free_queries.push(finished.query);
            } else {
                i += 1;
            }
        }
        self.imports.retain(|entry| entry.owner.strong_count() != 0);
        Ok(())
    }
    pub(super) fn draw(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        surface: &PaintSurface,
        viewport: [f32; 2],
    ) -> Result<()> {
        let frame = &surface.video_frame;
        self.reap(context)?;
        if self.pending.len() >= MAX_PENDING {
            frame.0.stats.queue_full.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        let index = if let Some(i) = self.imports.iter().position(|v| v.id == frame.0.buffer.id) {
            i
        } else {
            let entry = match import(device, frame) {
                Ok(v) => v,
                Err(e) => {
                    frame.0.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    return Err(e);
                }
            };
            if self.imports.len() >= MAX_IMPORTS {
                self.imports.pop_front();
            }
            self.imports.push_back(entry);
            self.imports.len() - 1
        };
        let entry = &self.imports[index];
        if !keyed_acquire(&entry.mutex)? {
            frame.0.stats.sync_busy.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        let acquired = Acquired(&entry.mutex);
        let geometry = frame.0.geometry;
        let rows = geometry.uv_rows();
        let color = frame.0.color.rows(frame.0.buffer.format);
        let b = surface.bounds;
        let c = surface.content_mask.bounds;
        let data = Params {
            target: [b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0],
            clip: [c.origin.x.0, c.origin.y.0, c.size.width.0, c.size.height.0],
            uv0: rows[0],
            uv1: rows[1],
            clamp: geometry.uv_clamp(),
            red: color[0],
            green: color[1],
            blue: color[2],
            view: [
                viewport[0],
                viewport[1],
                if frame.0.buffer.format == VideoFormat::Bgra8 {
                    1.
                } else {
                    0.
                },
                if frame.0.color.chroma == ChromaLocation::Left {
                    0.5 / geometry.coded[0] as f32
                } else {
                    0.
                },
            ],
        };
        ensure!(
            data.target
                .iter()
                .chain(data.clip.iter())
                .all(|v| v.is_finite())
                && viewport[0] > 0.
                && viewport[1] > 0.,
            "invalid video draw bounds"
        );
        let query = if let Some(q) = self.free_queries.pop() {
            q
        } else {
            let mut q = None;
            unsafe {
                device.CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut q),
                )?;
            }
            q.context("GPU event query missing")?
        };
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(
                &self.params,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(
                (&data as *const Params).cast::<u8>(),
                mapped.pData.cast(),
                std::mem::size_of::<Params>(),
            );
            context.Unmap(&self.params, 0);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: viewport[0],
                Height: viewport[1],
                MinDepth: 0.,
                MaxDepth: 1.,
                ..Default::default()
            }]));
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.VSSetShader(&self.vertex, None);
            context.PSSetShader(&self.pixel, None);
            context.VSSetConstantBuffers(1, Some(&[Some(self.params.clone())]));
            context.PSSetConstantBuffers(1, Some(&[Some(self.params.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            context.PSSetShaderResources(0, Some(&[Some(entry.y.clone()), None, entry.uv.clone()]));
            context.OMSetBlendState(&self.blend, None, u32::MAX);
            context.Draw(4, 0);
            context.PSSetShaderResources(0, Some(&[None, None, None]));
        }
        drop(acquired);
        unsafe {
            context.End(&query);
        }
        self.pending.push(Pending {
            query,
            frame: frame.clone(),
        });
        frame
            .0
            .stats
            .draws_submitted
            .fetch_add(1, Ordering::Relaxed);
        frame.0.submitted.store(true, Ordering::Release);
        Ok(())
    }
}

// This validation code is available only to development/test builds. The app
// example runs it without adding third-party upstream tests to the workspace.
#[cfg(feature = "test-support")]
pub(crate) mod validation {
    use super::*;
    use crate::{
        bounds,
        native_video::{synthetic_frame, Matrix, Range, Rotation, VideoColor},
        point, size, ContentMask, ScaledPixels,
    };
    use std::time::{Duration, Instant};
    fn hardware_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
        let (mut d, mut c) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                ::windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut d),
                None,
                Some(&mut c),
            )?;
        }
        Ok((
            d.context("hardware device missing")?,
            c.context("hardware context missing")?,
        ))
    }
    fn target(
        device: &ID3D11Device,
    ) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView, ID3D11Texture2D)> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: 128,
            Height: 96,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let (mut t, mut v, mut s) = (None, None, None);
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut t))?;
            device.CreateRenderTargetView(t.as_ref().unwrap(), None, Some(&mut v))?;
            let staging = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..desc
            };
            device.CreateTexture2D(&staging, None, Some(&mut s))?;
        }
        Ok((t.unwrap(), v.unwrap(), s.unwrap()))
    }
    fn read_pixel(
        context: &ID3D11DeviceContext,
        texture: &ID3D11Texture2D,
        staging: &ID3D11Texture2D,
        points: &[[u32; 2]],
    ) -> Result<Vec<[u8; 4]>> {
        // Validation-only readback of a small offscreen target, never used by production.
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let mut out = Vec::new();
        unsafe {
            context.CopyResource(staging, texture);
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            for [x, y] in points {
                let p = mapped
                    .pData
                    .cast::<u8>()
                    .add((y * mapped.RowPitch + x * 4) as usize);
                out.push([*p.add(2), *p.add(1), *p, *p.add(3)]);
            }
            context.Unmap(staging, 0);
        }
        Ok(out)
    }
    fn primitive(frame: D3dVideoFrame) -> PaintSurface {
        PaintSurface {
            order: 0,
            bounds: bounds(
                point(ScaledPixels(16.), ScaledPixels(8.)),
                size(ScaledPixels(96.), ScaledPixels(80.)),
            ),
            content_mask: ContentMask {
                bounds: bounds(
                    point(ScaledPixels(24.), ScaledPixels(8.)),
                    size(ScaledPixels(80.), ScaledPixels(80.)),
                ),
            },
            video_frame: frame,
        }
    }
    pub(crate) fn run() -> Result<()> {
        ensure!(
            std::env::var("RP_NATIVE_VIDEO_GPU_TEST").as_deref() == Ok("1"),
            "explicit GPU-test approval flag required"
        );
        let (device, context) = hardware_device()?;
        let mut pipeline = VideoPipeline::new(&device)?;
        super::super::directx_renderer::set_rasterizer_state(&device, &context)?;
        let (texture, view, staging) = target(&device)?;
        let mut records = Vec::new();
        for format in [VideoFormat::Nv12, VideoFormat::P010, VideoFormat::Bgra8] {
            let color = VideoColor {
                matrix: Matrix::Bt709,
                range: Range::Limited,
                chroma: ChromaLocation::Center,
            };
            let frame = synthetic_frame(format, 1920, 1088, color)?;
            let surface = primitive(frame.clone());
            unsafe {
                context.OMSetRenderTargets(Some(&[Some(view.clone())]), None);
                context.ClearRenderTargetView(&view, &[0.4, 0.0, 0.4, 1.0]);
            }
            pipeline.draw(&device, &context, &surface, [128., 96.])?;
            let pixels = read_pixel(
                &context,
                &texture,
                &staging,
                &[[20, 30], [32, 24], [92, 24], [32, 72], [92, 72]],
            )?;
            ensure!(
                pixels[0][0] > 95 && pixels[0][1] < 3 && pixels[0][2] > 95,
                "surface ignored its content mask: {pixels:?}"
            );
            let wanted = if format == VideoFormat::Bgra8 {
                [16u8, 235, 64, 180]
            } else {
                [0u8, 255, 56, 191]
            };
            for (p, expected) in pixels[1..].iter().zip(wanted) {
                for v in &p[..3] {
                    ensure!(
                        (i16::from(*v) - i16::from(expected)).abs() <= 3,
                        "GPU color differs from declared range/depth: {format:?} {pixels:?}"
                    );
                }
            }
            println!("NATIVE_GPU_PIXEL_PASS format={format:?} pixels={pixels:?}");
            // Shader orientation is checked independently from the CPU mapping
            // property tests, using known quadrant permutations.
            for (rotation, indices) in [
                (Rotation::R90, [2, 0, 3, 1]),
                (Rotation::R180, [3, 2, 1, 0]),
                (Rotation::R270, [1, 3, 0, 2]),
            ] {
                let rotated = frame.with_geometry(crate::native_video::VideoGeometry {
                    rotation,
                    ..frame.geometry()
                })?;
                pipeline.draw(&device, &context, &primitive(rotated), [128., 96.])?;
                let observed = read_pixel(
                    &context,
                    &texture,
                    &staging,
                    &[[32, 24], [92, 24], [32, 72], [92, 72]],
                )?;
                for (pixel, index) in observed.iter().zip(indices) {
                    for channel in &pixel[..3] {
                        ensure!(
                            (i16::from(*channel) - i16::from(wanted[index])).abs() <= 3,
                            "rotation {rotation:?} did not match input geometry: {observed:?}"
                        );
                    }
                }
            }
            let geometry = frame.geometry();
            let crop = frame.with_geometry(crate::native_video::VideoGeometry {
                visible: [
                    geometry.coded[0] / 2,
                    0,
                    geometry.coded[0] / 2,
                    geometry.coded[1] / 2,
                ],
                ..geometry
            })?;
            pipeline.draw(&device, &context, &primitive(crop), [128., 96.])?;
            let cropped = read_pixel(
                &context,
                &texture,
                &staging,
                &[[32, 24], [92, 24], [32, 72], [92, 72]],
            )?;
            for pixel in &cropped {
                for channel in &pixel[..3] {
                    ensure!(
                        (i16::from(*channel) - i16::from(wanted[1])).abs() <= 3,
                        "visible crop sampled outside its rectangle: {cropped:?}"
                    );
                }
            }
            println!(
                "NATIVE_GPU_GEOMETRY_PASS format={format:?} rotations=3 cropped_quadrant=verified"
            );
            pipeline.reap(&context)?;
            // Hold the imported mutex to force a zero-timeout read miss. A false
            // HRESULT success here would draw into a resource owned elsewhere.
            // A busy-producer test must use another device. Acquiring the exact
            // same mutex object twice is an invalid recursive call, not timeout.
            let (writer_device, _writer_context) = hardware_device()?;
            let writer_device1: ID3D11Device1 = writer_device.cast()?;
            let writer_texture: ID3D11Texture2D =
                unsafe { writer_device1.OpenSharedResource1(frame.0.buffer.handle)? };
            let mutex: IDXGIKeyedMutex = writer_texture.cast()?;
            ensure!(keyed_acquire(&mutex)?, "test mutex must start available");
            let before = frame.stats().snapshot();
            let begin = Instant::now();
            let busy = pipeline.draw(&device, &context, &surface, [128., 96.]);
            unsafe {
                mutex.ReleaseSync(0)?;
            }
            busy?;
            ensure!(
                begin.elapsed() < Duration::from_millis(100),
                "GUI waited for another GPU owner"
            );
            ensure!(
                frame.stats().snapshot()[1] == before[1]
                    && frame.stats().snapshot()[3] == before[3] + 1,
                "busy surface was incorrectly drawn"
            );
            // Repeat one buffer, ensuring it is not re-opened/re-created per frame.
            for _ in 0..24 {
                pipeline.draw(&device, &context, &surface, [128., 96.])?;
                unsafe {
                    context.Flush();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while !pipeline.pending.is_empty() {
                pipeline.reap(&context)?;
                if Instant::now() > deadline {
                    bail!("GPU completion timeout")
                };
                std::thread::sleep(Duration::from_millis(1));
            }
            let snapshot = frame.stats().snapshot();
            ensure!(snapshot[0] == 1, "per-frame texture import regression");
            ensure!(
                snapshot[1] == snapshot[2],
                "native frame retired before GPU completed"
            );
            ensure!(
                pipeline.imports.len() <= MAX_IMPORTS && pipeline.pending.len() <= MAX_PENDING,
                "unbounded native pool"
            );
            records.push(format!(
                "{format:?}: pixels={pixels:?}, counters={snapshot:?}"
            ));
        }
        println!("NATIVE_VIDEO_GPU_SELFTEST {}", records.join(" | "));
        println!("READBACK_SCOPE test-only 128x96 output; source pattern uploaded once per format; production adapter maps only 144-byte uniforms");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "explicit physical GPU check; use native_video_gpu_check example"]
    fn native_video_gpu_roundtrip_and_nonblocking_ownership() -> anyhow::Result<()> {
        super::validation::run()
    }
}
