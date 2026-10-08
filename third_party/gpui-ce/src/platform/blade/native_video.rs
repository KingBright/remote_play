//! Real Blade-context import/composition. Scene order remains GPUI-owned. The
//! frame lease and imported image live until GPU completion, not element paint.
use super::*;
use crate::{
    native_video::{linux::AllocationKey, ChromaLocation, DmaVideoFrame, VideoFormat},
    PaintSurface,
};
use gpu::native_drm::{DmaBufImage, DmaBufPlaneDesc, PlaneFormat};
use std::{collections::VecDeque, os::fd::AsFd, sync::atomic::Ordering};
const MAX_CACHED: usize = 12;
const MAX_BATCHES: usize = 3;
const MAX_FRAME_SURFACES: usize = 32;
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct NativeParams {
    target_rect: [f32; 4],
    clip: [f32; 4],
    uv0: [f32; 4],
    uv1: [f32; 4],
    visible_uv: [f32; 4],
    red: [f32; 4],
    green: [f32; 4],
    blue: [f32; 4],
    view: [f32; 4],
}
#[derive(blade_macros::ShaderData)]
struct NativeData {
    native_params: NativeParams,
    native_y: gpu::TextureView,
    native_uv: gpu::TextureView,
    native_sampler: gpu::Sampler,
}
struct Imported {
    planes: [DmaBufImage; 2],
    _gpu: Arc<gpu::Context>,
}
impl Drop for Imported {
    fn drop(&mut self){crate::native_video::linux::LIVE_IMPORTED_ALLOCATIONS.fetch_sub(1,Ordering::AcqRel);}
}
struct Prepared {
    draws:u64,
    frame: DmaVideoFrame,
    images: Arc<Imported>,
}
struct Batch {
    sync: gpu::SyncPoint,
    uses: Vec<Prepared>,
}
pub(super) struct NativeVideo {
    pipeline: Option<gpu::RenderPipeline>,
    sampler: Option<gpu::Sampler>,
    cache: VecDeque<(AllocationKey, Arc<Imported>)>,
    pending: VecDeque<Batch>,
    current: Vec<Prepared>,
    free_uses:Vec<Vec<Prepared>>,
}
impl NativeVideo {
    pub(super) fn new() -> Self {
        Self {
            pipeline: None,
            sampler: None,
            cache: VecDeque::with_capacity(MAX_CACHED),
            pending: VecDeque::with_capacity(MAX_BATCHES),
            current: Vec::with_capacity(MAX_FRAME_SURFACES),
            free_uses:Vec::with_capacity(MAX_BATCHES),
        }
    }
    fn reject(frame: &DmaVideoFrame, error: impl ToString) {
        frame.stats().rejected.fetch_add(1, Ordering::Relaxed);
        *frame
            .stats()
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(error.to_string());
    }
    fn initialize(&mut self, gpu: &gpu::Context, info: gpu::SurfaceInfo) {
        if self.pipeline.is_some() {
            return;
        }
        use gpu::ShaderData;
        let shader = gpu.create_shader(gpu::ShaderDesc {
            source: include_str!("native_video.wgsl"),
        });
        shader.check_struct_size::<NativeParams>();
        self.pipeline = Some(gpu.create_render_pipeline(gpu::RenderPipelineDesc {
            name: "native-video-drm",
            data_layouts: &[&NativeData::layout()],
            vertex: shader.at("vs_native"),
            vertex_fetches: &[],
            primitive: gpu::PrimitiveState {
                topology: gpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            fragment: Some(shader.at("fs_native")),
            color_targets: &[gpu::ColorTargetState {
                format: info.format,
                blend: None,
                write_mask: gpu::ColorWrites::default(),
            }],
            multisample_state: gpu::MultisampleState::default(),
        }));
        self.sampler = Some(gpu.create_sampler(gpu::SamplerDesc {
            name: "native-video-linear",
            mag_filter: gpu::FilterMode::Linear,
            min_filter: gpu::FilterMode::Linear,
            ..Default::default()
        }));
    }
    fn reap(&mut self, gpu: &gpu::Context) {
        while self
            .pending
            .front()
            .is_some_and(|b| gpu.wait_for(&b.sync, 0))
        {
            let mut batch = self.pending.pop_front().unwrap();
            for item in batch.uses.drain(..) {
                item.frame
                    .stats()
                    .gpu_completed
                    .fetch_add(item.draws, Ordering::Relaxed);
            }
            if self.free_uses.len()<MAX_BATCHES{self.free_uses.push(batch.uses);}
        }
    }
    fn import(gpu: &Arc<gpu::Context>, frame: &DmaVideoFrame) -> Result<Arc<Imported>, String> {
        let g = frame.geometry();
        let planes = &frame.0.planes;
        let make = |i: usize| {
            let p = &planes[i];
            let div = if i == 0 { 1 } else { 2 };
            let format = match (frame.0.format, i) {
                (VideoFormat::Nv12, 0) => PlaneFormat::R8,
                (VideoFormat::Nv12, _) => PlaneFormat::Rg8,
                (VideoFormat::P010, 0) => PlaneFormat::R16,
                _ => PlaneFormat::Rg16,
            };
            unsafe {
                gpu.import_dma_buf_plane(&DmaBufPlaneDesc {
                    fd: p.fd.as_fd(),
                    allocation_size: p.key.bytes,
                    modifier: p.key.modifier,
                    offset: p.key.offset,
                    pitch: p.key.pitch,
                    width: g.coded[0] / div,
                    height: g.coded[1] / div,
                    format,
                    render_node: frame.0.key.render_node,
                })
            }
            .map_err(|e| e.to_string())
        };
        let y = make(0)?;
        let uv = make(1)?;
        frame.stats().imports.fetch_add(1, Ordering::Relaxed);
        crate::native_video::linux::LIVE_IMPORTED_ALLOCATIONS.fetch_add(1,Ordering::AcqRel);
        Ok(Arc::new(Imported {
            planes: [y, uv],
            _gpu: gpu.clone(),
        }))
    }
    pub(super) fn prepare(
        &mut self,
        gpu: &Arc<gpu::Context>,
        scene: &Scene,
        info: gpu::SurfaceInfo,
        encoder: &mut gpu::CommandEncoder,
    ) {
        self.reap(gpu);
        self.current.clear();
        // Empty/closed video views release cached allocations instead of keeping
        // their backing memory until the whole application closes. In-flight
        // batches retain their own refs until the GPU has completed.
        if scene.surfaces.is_empty(){self.cache.clear();return;}
        for batch in scene.batches() {
            if let PrimitiveBatch::Surfaces(surfaces) = batch {
                for s in surfaces {
                    let frame = &s.linux_video_frame;
                    if let Some(existing)=self.current.iter_mut().find(|item|item.frame==*frame){
                        // PiP uses the same allocation but adds an actual draw.
                        existing.draws+=1;
                        continue;
                    }
                    if self.pending.len() >= MAX_BATCHES || self.current.len() >= MAX_FRAME_SURFACES {
                        frame.stats().queue_full.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let images = if let Some(index) =
                        self.cache.iter().position(|(key, _)| *key == frame.0.key)
                    {
                        let item = self.cache.remove(index).unwrap();
                        let images = item.1.clone();
                        self.cache.push_back(item);
                        images
                    } else {
                        match Self::import(gpu, frame) {
                            Ok(images) => {
                                if self.cache.len() == MAX_CACHED {
                                    self.cache.pop_front();
                                }
                                self.cache.push_back((frame.0.key.clone(), images.clone()));
                                images
                            }
                            Err(error) => {
                                Self::reject(frame, error);
                                continue;
                            }
                        }
                    };
                    if !self.current.iter().any(|p| Arc::ptr_eq(&p.images, &images)) {
                        unsafe {
                            for plane in &images.planes {
                                encoder.acquire_dma_buf(plane);
                            }
                        }
                    }
                    self.current.push(Prepared {
                        draws:1,
                        frame: frame.clone(),
                        images,
                    });
                }
            }
        }
        if !self.current.is_empty() {
            self.initialize(gpu, info);
        }
    }
    pub(super) fn draw(
        &self,
        pass: &mut gpu::RenderCommandEncoder<'_>,
        surface: &PaintSurface,
        viewport: [f32; 2],
    ) {
        let Some(prepared) = self
            .current
            .iter()
            .find(|p| p.frame == surface.linux_video_frame)
        else {
            return;
        };
        let frame = &prepared.frame;
        let geometry = frame.geometry();
        let rows = geometry.uv_rows();
        let color = frame.0.color.rows(frame.0.format);
        let b = surface.bounds;
        let c = surface.content_mask.bounds;
        let params = NativeParams {
            target_rect: [b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0],
            clip: [c.origin.x.0, c.origin.y.0, c.size.width.0, c.size.height.0],
            uv0: rows[0],
            uv1: rows[1],
            visible_uv: geometry.uv_clamp(),
            red: color[0],
            green: color[1],
            blue: color[2],
            view: [
                viewport[0],
                viewport[1],
                if frame.0.color.chroma == ChromaLocation::Left {
                    0.5 / geometry.coded[0] as f32
                } else {
                    0.
                },
                0.,
            ],
        };
        let mut encoder = pass.with(self.pipeline.as_ref().unwrap());
        encoder.bind(
            0,
            &NativeData {
                native_params: params,
                native_y: prepared.images.planes[0].view(),
                native_uv: prepared.images.planes[1].view(),
                native_sampler: self.sampler.unwrap(),
            },
        );
        encoder.draw(0, 4, 0, 1);
    }
    pub(super) fn release(&self, encoder: &mut gpu::CommandEncoder) {
        for (index, item) in self.current.iter().enumerate() {
            if !self.current[..index]
                .iter()
                .any(|p| Arc::ptr_eq(&p.images, &item.images))
            {
                unsafe {
                    for plane in &item.images.planes {
                        encoder.release_dma_buf(plane);
                    }
                }
            }
        }
    }
    pub(super) fn submitted(&mut self, sync: &gpu::SyncPoint) {
        if self.current.is_empty() {
            return;
        }
        for item in &self.current {
            item.frame
                .stats()
                .draws_submitted
                .fetch_add(item.draws, Ordering::Relaxed);
            item.frame.0.submitted.store(true, Ordering::Release);
            item.frame
                .stats()
                .last_error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        }
        let replacement=self.free_uses.pop().unwrap_or_else(||Vec::with_capacity(MAX_FRAME_SURFACES));
        let uses = std::mem::replace(&mut self.current, replacement);
        self.pending.push_back(Batch {
            sync: sync.clone(),
            uses,
        });
    }
    // Caller has waited for the last renderer sync point before invoking this.
    pub(super) fn destroy(&mut self, gpu: &gpu::Context) {
        self.reap(gpu);
        assert!(
            self.pending.is_empty(),
            "native producer leases cannot be freed before GPU completion"
        );
        self.current.clear();
        self.cache.clear();
        if let Some(mut p) = self.pipeline.take() {
            gpu.destroy_render_pipeline(&mut p);
        }
        if let Some(s) = self.sampler.take() {
            gpu.destroy_sampler(s);
        }
    }
}

// Explicit validation only. It runs the same import/cache/shader/draw code above,
// not a second hand-written Vulkan shader. No call is present in normal playback.
#[cfg(feature = "native-video-validation")]
pub(super) fn validation_pixels(frame: DmaVideoFrame) -> Result<Vec<[u8; 4]>, String> {
    if std::env::var("RP_BLADE_NATIVE_PIXEL_TEST").as_deref() != Ok("1") {
        return Err("explicit compositor readback validation flag required".into());
    }
    let gpu = Arc::new(
        unsafe {
            gpu::Context::init(gpu::ContextDesc {
                presentation: false,
                ..Default::default()
            })
        }
        .map_err(|e| format!("native validation context: {e:?}"))?,
    );
    if gpu.device_information().is_software_emulated {
        return Err("software validation renderer is not allowed".into());
    }
    let format = gpu::TextureFormat::Bgra8Unorm;
    let extent = gpu::Extent {
        width: 96,
        height: 64,
        depth: 1,
    };
    let texture = gpu.create_texture(gpu::TextureDesc {
        name: "native-test-target",
        format,
        size: extent,
        array_layer_count: 1,
        mip_level_count: 1,
        sample_count: 1,
        dimension: gpu::TextureDimension::D2,
        usage: gpu::TextureUsage::TARGET | gpu::TextureUsage::COPY,
        external: None,
    });
    let view = gpu.create_texture_view(
        texture,
        gpu::TextureViewDesc {
            name: "native-test-target",
            format,
            dimension: gpu::ViewDimension::D2,
            subresources: &gpu::TextureSubresources::default(),
        },
    );
    let buffer = gpu.create_buffer(gpu::BufferDesc {
        name: "native-test-readback",
        size: 96 * 64 * 4,
        memory: gpu::Memory::Shared,
    });
    let mut commands = gpu.create_command_encoder(gpu::CommandEncoderDesc {
        name: "native-compositor-validation",
        buffer_count: 1,
    });
    commands.start();
    unsafe {
        commands.init_texture(texture);
    }
    let mut scene = Scene::default();
    scene.insert_primitive(PaintSurface {
        order: 0,
        bounds: crate::bounds(
            crate::point(ScaledPixels(8.), ScaledPixels(8.)),
            crate::size(ScaledPixels(80.), ScaledPixels(48.)),
        ),
        content_mask: crate::ContentMask {
            bounds: crate::bounds(
                crate::point(ScaledPixels(16.), ScaledPixels(12.)),
                crate::size(ScaledPixels(64.), ScaledPixels(36.)),
            ),
        },
        linux_video_frame: frame.clone(),
    });
    scene.finish();
    let mut pipeline = NativeVideo::new();
    pipeline.prepare(
        &gpu,
        &scene,
        gpu::SurfaceInfo {
            format,
            alpha: gpu::AlphaMode::Ignored,
        },
        &mut commands,
    );
    let mut pass = commands.render(
        "native-surface-validation",
        gpu::RenderTargetSet {
            colors: &[gpu::RenderTarget {
                view,
                init_op: gpu::InitOp::Clear(gpu::TextureColor::TransparentBlack),
                finish_op: gpu::FinishOp::Store,
            }],
            depth_stencil: None,
        },
    );
    for batch in scene.batches() {
        if let PrimitiveBatch::Surfaces(surfaces) = batch {
            for surface in surfaces {
                pipeline.draw(&mut pass, surface, [96., 64.]);
            }
        }
    }
    drop(pass);
    pipeline.release(&mut commands);
    {
        let mut transfer = commands.transfer("test-only-readback");
        transfer.copy_texture_to_buffer(texture.into(), buffer.into(), 96 * 4, extent);
    }
    let sync = gpu.submit(&mut commands);
    pipeline.submitted(&sync);
    if !gpu.wait_for(&sync, 3000) {
        // A failed test must not recycle uncertain in-flight external resources.
        // Leave them held for this explicitly bounded validation process to exit.
        std::mem::forget((pipeline, commands, gpu, scene, frame));
        return Err("native compositor validation GPU completion timeout".into());
    }
    let data = unsafe { std::slice::from_raw_parts(buffer.data(), 96 * 64 * 4) };
    let pixels = data
        .chunks_exact(4)
        .map(|p| [p[2], p[1], p[0], p[3]])
        .collect();
    let error = frame.stats().last_error.lock().unwrap().clone();
    let submitted = frame.was_submitted();
    pipeline.destroy(&gpu);
    gpu.destroy_texture_view(view);
    gpu.destroy_texture(texture);
    gpu.destroy_buffer(buffer);
    gpu.destroy_command_encoder(&mut commands);
    if let Some(error) = error {
        return Err(error);
    }
    if !submitted {
        return Err("native shader was not submitted".into());
    }
    Ok(pixels)
}
