//! Explicit generated HEVC -> VAAPI -> DMA-BUF -> actual GPUI/Blade window.
//! No Weavelo connection, screen capture, keyboard injection or deployment.
#[path = "../../../../app/src/design_system.rs"]
mod design_system;
#[path = "../../../../app/src/original_design.rs"]
mod original_design;
use gpui::{native_video::*, *};
use std::{
    collections::BTreeSet,
    ffi::{CStr, CString, c_void},
    os::{fd::BorrowedFd, unix::fs::MetadataExt},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use yororen_ui::{
    assets::UiAsset,
    component,
    theme::{ActiveTheme, GlobalTheme},
};
type E = Box<dyn std::error::Error + Send + Sync>;
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
    visible_width: u32,
    visible_height: u32,
    allocation_width: u32,
    allocation_height: u32,
    crop_left: u64,
    crop_top: u64,
    crop_right: u64,
    crop_bottom: u64,
    pixel_format: i32,
    color_range: i32,
    color_primaries: i32,
    color_transfer: i32,
    color_matrix: i32,
    chroma_location: i32,
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
    fn rp_linux_decoder_flush(decoder: *mut c_void) -> i32;
    fn rp_linux_decoder_receive(decoder: *mut c_void, out: *mut *mut c_void) -> i32;
    fn rp_linux_frame_info(frame: *const c_void) -> *const FrameInfo;
    fn rp_linux_frame_release(frame: *mut *mut c_void);
    fn rp_linux_frame_info_size() -> usize;
    fn rp_linux_error_string(error: i32, text: *mut i8, len: usize);
}
fn ffcheck(v: i32) -> Result<i32, E> {
    if v < 0 {
        let mut text = [0i8; 256];
        unsafe {
            rp_linux_error_string(v, text.as_mut_ptr(), text.len());
        }
        Err(format!(
            "native decode error {v}: {}",
            unsafe { CStr::from_ptr(text.as_ptr()) }.to_string_lossy()
        )
        .into())
    } else {
        Ok(v)
    }
}
struct Decoder(*mut c_void);
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            rp_linux_decoder_destroy(&mut self.0);
        }
    }
}
struct Lease {
    raw: *mut c_void,
    released: Arc<AtomicU64>,
}
// Ready AVFrames are immutable; FFmpeg's references keep their hardware context
// alive independently of the decoder. Only reference release happens on Drop.
unsafe impl Send for Lease {}
unsafe impl Sync for Lease {}
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            rp_linux_frame_release(&mut self.raw);
        }
        self.released.fetch_add(1, Ordering::Relaxed);
    }
}
struct State {
    latest: Mutex<Option<(u64, DmaVideoFrame)>>,
    seen: Mutex<BTreeSet<u64>>,
    submitted: Mutex<BTreeSet<u64>>,
    stats: Arc<VideoSurfaceStats>,
    released: Arc<AtomicU64>,
    decoded: AtomicU64,
    ready: AtomicBool,
    stop: AtomicBool,
    release_views:AtomicBool,
    retired_while_window_alive:AtomicBool,
    done: AtomicBool,
    error: Mutex<Option<String>>,
    cold_start_us: AtomicU64,
    renders: AtomicU64,
    gpu: Mutex<Option<String>>,
    pixel_reports: Mutex<Vec<serde_json::Value>>,
}
struct WindowView {
    state: Arc<State>,
    frame: Option<(u64, DmaVideoFrame)>,
}
impl Render for WindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.renders.fetch_add(1, Ordering::Relaxed) == 0 {
            if let Some(gpu) = window.gpu_specs() {
                assert!(!gpu.is_software_emulated, "hardware GPU required");
                *self.state.gpu.lock().unwrap() = Some(gpu.device_name);
            }
            self.state.ready.store(true, Ordering::Release);
        }
        if let Some((tag, previous)) = &self.frame {
            if previous.was_submitted() {
                self.state.submitted.lock().unwrap().insert(*tag);
            }
        }
        if let Some(next) = self.state.latest.lock().unwrap().take() {
            self.state.seen.lock().unwrap().insert(next.0);
            self.frame = Some(next);
        }
        if self.state.release_views.load(Ordering::Acquire){self.frame.take();self.state.latest.lock().unwrap().take();}
        let theme = cx.theme().clone();
        let mut view = div()
            .relative()
            .size_full()
            .bg(theme.surface.canvas)
            .text_color(theme.content.primary)
            .font_family(design_system::product_ui_font());
        if let Some((_, frame)) = &self.frame {
            view = view.child(
                div().absolute().inset_0().overflow_hidden().child(
                    surface(frame.clone())
                        .object_fit(ObjectFit::Contain)
                        .size_full(),
                ),
            );
            // Same immutable decoded frame in a second clipped view tests scene ordering
            // and deduplicated acquire/release. This is a validation PiP, not new product UI.
            view = view.child(
                div()
                    .absolute()
                    .bottom(px(18.))
                    .right(px(18.))
                    .w(px(220.))
                    .h(px(130.))
                    .border_2()
                    .border_color(rgb(0x55ccdd))
                    .overflow_hidden()
                    .child(
                        surface(frame.clone())
                            .object_fit(ObjectFit::Cover)
                            .size_full(),
                    ),
            );
        }
        view.child(
            div()
                .absolute()
                .top(px(14.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(original_design::stream_status_capsule_card(
                    "Weavelo".into(),
                    false,
                    "Linux hardware HEVC · native Blade · original controls".into(),
                    false,
                    &theme,
                    None,
                )),
        )
    }
}
fn access_units(bytes: &[u8]) -> Result<Vec<&[u8]>, E> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        let n = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        if (bytes[i + n] >> 1) & 63 == 35 {
            starts.push(i);
        }
        i += n + 2;
    }
    if starts.len() != 60 || starts[0] != 0 {
        return Err("exact generated 60-frame AUD fixture required".into());
    }
    starts.push(bytes.len());
    Ok(starts.windows(2).map(|p| &bytes[p[0]..p[1]]).collect())
}
fn publish(state: &State, raw: *mut c_void) -> Result<DmaVideoFrame, E> {
    let lease = Arc::new(Lease {
        raw,
        released: state.released.clone(),
    });
    let i = unsafe { *rp_linux_frame_info(raw) };
    if i.abi_version != 1
        || i.layer_count != 2
        || i.object_count > 4
        || i.color_matrix != 1
        || i.color_transfer != 1
        || i.color_primaries != 1
        || i.color_range != 1
    {
        return Err(format!("unsupported explicitly signaled test video {i:?}").into());
    }
    let format = match i.pixel_format {
        8 => VideoFormat::Nv12,
        10 => VideoFormat::P010,
        _ => return Err("native video depth unsupported".into()),
    };
    let make = |plane: usize| -> Result<DmaVideoPlane, E> {
        let layer = i.layers[plane];
        if layer.plane_count != 1 {
            return Err("unsupported auxiliary planes".into());
        }
        let p = layer.planes[0];
        if p.object_index >= i.object_count {
            return Err("plane object missing".into());
        }
        let expected = match (i.pixel_format, plane) {
            (8, 0) => *b"R8  ",
            (8, 1) => *b"GR88",
            (10, 0) => *b"R16 ",
            _ => *b"GR32",
        };
        if layer.fourcc.to_le_bytes() != expected {
            return Err("DRM component order disagrees with declared format".into());
        }
        let o = i.objects[p.object_index as usize];
        Ok(DmaVideoPlane::duplicate(
            unsafe { BorrowedFd::borrow_raw(o.fd) },
            o.bytes,
            o.modifier,
            p.offset,
            p.pitch,
        )?)
    };
    let geometry = VideoGeometry {
        coded: [i.allocation_width, i.allocation_height],
        visible: [
            i.crop_left as u32,
            i.crop_top as u32,
            (u64::from(i.visible_width) - i.crop_left - i.crop_right) as u32,
            (u64::from(i.visible_height) - i.crop_top - i.crop_bottom) as u32,
        ],
        pixel_aspect: if i.aspect_num > 0 && i.aspect_den > 0 {
            [i.aspect_num as u32, i.aspect_den as u32]
        } else {
            [1, 1]
        },
        rotation: match (i.pts / 495) % 4 {
            0 => Rotation::R0,
            1 => Rotation::R90,
            2 => Rotation::R180,
            _ => Rotation::R270,
        },
    };
    let rdev = std::fs::metadata("/dev/dri/renderD128")?.rdev();
    let render_node = (i64::from(libc::major(rdev)), i64::from(libc::minor(rdev)));
    let chroma = match i.chroma_location {
        0 | 1 => ChromaLocation::Left,
        2 => ChromaLocation::Center,
        _ => return Err("unsupported chroma siting".into()),
    };
    let frame = unsafe {
        DmaVideoFrame::from_ready_planes(
            [make(0)?, make(1)?],
            geometry,
            format,
            VideoColor {
                matrix: Matrix::Bt709,
                range: Range::Limited,
                chroma,
            },
            render_node,
            lease,
            state.stats.clone(),
        )
    }?;
    if i.pts == 0 && std::env::var("RP_BLADE_NATIVE_PIXEL_TEST").as_deref() == Ok("1") {
        let reference = std::fs::read(std::env::var("RP_BLADE_NATIVE_REFERENCE")?)?;
        // Validate before publishing to the live window, so no other Vulkan
        // queue is simultaneously acquiring this source's foreign ownership.
        let reports = validate_pixels(&frame, &i, &reference)?;
        *state.pixel_reports.lock().unwrap() = reports;
    }
    state.decoded.fetch_add(1, Ordering::Relaxed);
    *state.latest.lock().unwrap() = Some((i.pts as u64, frame.clone()));
    Ok(frame)
}
fn decode(state: Arc<State>, input: PathBuf) -> Result<(), E> {
    assert_eq!(
        unsafe { rp_linux_frame_info_size() },
        std::mem::size_of::<FrameInfo>()
    );
    let started = Instant::now();
    while !state.ready.load(Ordering::Acquire) {
        if started.elapsed() > Duration::from_secs(8) {
            return Err("native GUI startup timed out".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let bytes = std::fs::read(input)?;
    let units = access_units(&bytes)?;
    let node = CString::new("/dev/dri/renderD128")?;
    let mut ptr = std::ptr::null_mut();
    ffcheck(unsafe { rp_linux_decoder_create(node.as_ptr(), &mut ptr) })?;
    let decoder = Decoder(ptr);
    let mut warmed = false;
    let mut drain = || -> Result<(), E> {
        loop {
            let mut raw = std::ptr::null_mut();
            let n = ffcheck(unsafe { rp_linux_decoder_receive(decoder.0, &mut raw) })?;
            if n == 0 {
                return Ok(());
            }
            let frame = publish(&state, raw)?;
            if !warmed {
                let deadline = Instant::now() + Duration::from_secs(8);
                while !frame.was_submitted() {
                    if state.stop.load(Ordering::Acquire) || Instant::now() > deadline {
                        return Err(format!(
                            "first native surface was not submitted: {:?}",
                            *state.stats.last_error.lock().unwrap()
                        )
                        .into());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                state
                    .cold_start_us
                    .store(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                warmed = true;
            }
        }
    };
    for (tag, unit) in units.iter().enumerate() {
        if state.stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let started = Instant::now();
        let mut n = unsafe {
            rp_linux_decoder_submit(decoder.0, unit.as_ptr(), unit.len(), tag as i64 * 33)
        };
        if n == -libc::EAGAIN {
            drain()?;
            n = unsafe {
                rp_linux_decoder_submit(decoder.0, unit.as_ptr(), unit.len(), tag as i64 * 33)
            };
        }
        ffcheck(n)?;
        drain()?;
        if let Some(delay) = Duration::from_millis(33).checked_sub(started.elapsed()) {
            std::thread::sleep(delay);
        }
    }
    ffcheck(unsafe { rp_linux_decoder_flush(decoder.0) })?;
    drain()?;
    Ok(())
}
fn main() -> Result<(), E> {
    if std::env::var("RP_BLADE_NATIVE_WINDOW_TEST").as_deref() != Ok("1") {
        return Err("explicit native window test flag required".into());
    }
    let output = PathBuf::from(std::env::var("RP_BLADE_NATIVE_RECEIPT")?);
    if output.exists() {
        return Err("receipt already exists".into());
    }
    let input = PathBuf::from(std::env::var("RP_BLADE_NATIVE_FIXTURE")?);
    let state = Arc::new(State {
        latest: Mutex::new(None),
        seen: Mutex::new(BTreeSet::new()),
        submitted: Mutex::new(BTreeSet::new()),
        stats: Arc::new(VideoSurfaceStats::default()),
        released: Arc::new(AtomicU64::new(0)),
        decoded: AtomicU64::new(0),
        ready: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        release_views:AtomicBool::new(false),
        retired_while_window_alive:AtomicBool::new(false),
        done: AtomicBool::new(false),
        error: Mutex::new(None),
        cold_start_us: AtomicU64::new(0),
        renders: AtomicU64::new(0),
        gpu: Mutex::new(None),
        pixel_reports: Mutex::new(Vec::new()),
    });
    let s = state.clone();
    let worker = std::thread::Builder::new()
        .name("rp-native-test-decode".into())
        .spawn(move || {
            if let Err(e) = decode(s.clone(), input) {
                *s.error.lock().unwrap() = Some(e.to_string());
            }
            s.done.store(true, Ordering::Release);
        })?;
    let ui = state.clone();
    Application::new()
        .with_assets(UiAsset)
        .run(move |cx: &mut App| {
            component::init(cx);
            cx.set_global(GlobalTheme::new_with_themes(
                WindowAppearance::Dark,
                design_system::weavelo_themes(),
            ));
            let view_state = ui.clone();
            let view = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds(
                            point(px(80.), px(80.)),
                            size(px(1120.), px(740.)),
                        ))),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Weavelo native Linux validation".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|_| WindowView {
                            state: view_state,
                            frame: None,
                        })
                    },
                )
                .unwrap();
            cx.spawn(async move |cx| {
                let started = Instant::now();
                let mut completed_at = None;
                loop {
                    Timer::after(Duration::from_millis(16)).await;
                    let _ = view.update(cx, |_, _, cx| cx.notify());
                    if ui.done.load(Ordering::Acquire) {
                        completed_at.get_or_insert(Instant::now());
                    }
                    if completed_at.is_some_and(|t|t.elapsed()>Duration::from_millis(350)){
                        ui.release_views.store(true,Ordering::Release);
                        if ui.released.load(Ordering::Acquire)==ui.decoded.load(Ordering::Acquire)&&linux_video_imports_live()==0{
                            ui.retired_while_window_alive.store(true,Ordering::Release);break;
                        }
                    }
                    if started.elapsed()>Duration::from_secs(18){break;}
                }
                ui.stop.store(true, Ordering::Release);
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
    state.stop.store(true, Ordering::Release);
    worker.join().map_err(|_| "native decode thread panicked")?;
    state.latest.lock().unwrap().take();
    let stats = state.stats.snapshot();
    let seen = state.seen.lock().unwrap().len();
    let submitted = state.submitted.lock().unwrap().len();
    let decoded = state.decoded.load(Ordering::Relaxed);
    let error = state.error.lock().unwrap().clone();
    let surface_error = state.stats.last_error.lock().unwrap().clone();
    let passed = state.retired_while_window_alive.load(Ordering::Acquire) && decoded == 60
        && submitted >= 55
        && stats[1] > 0
        && stats[5] == 0
        && error.is_none()
        && surface_error.is_none();
    let report = serde_json::json!({"passed":passed,"gpu":*state.gpu.lock().unwrap(),"decoded_native_frames":decoded,"different_frames_seen":seen,"different_frames_gpu_submitted":submitted,"surface_counters":stats,"cold_start_us":state.cold_start_us.load(Ordering::Relaxed),"native_leases_released":state.released.load(Ordering::Relaxed),"native_imports_live_after_stop":linux_video_imports_live(),"native_resources_retired_before_window_exit":state.retired_while_window_alive.load(Ordering::Acquire),"worker_error":error,"surface_error":surface_error,"real_blade_context":true,"pixel_validation":*state.pixel_reports.lock().unwrap(),"original_controls":true,"picture_in_picture_same_frame":true,"rotations":[0,90,180,270],"screen_capture":false,"network":false,"input_injected":false,"production_deployed":false,"scanout_measured":false});
    std::fs::write(output, serde_json::to_vec_pretty(&report)?)?;
    println!("REAL_BLADE_NATIVE_WINDOW {report}");
    if !passed {
        return Err("native original-window validation failed".into());
    }
    Ok(())
}

// Independent limited-range BT.709 oracle on selected software-reference samples.
// This is test-only and never used to turn actual video frames into CPU images.
fn validate_pixels(
    frame: &DmaVideoFrame,
    i: &FrameInfo,
    reference: &[u8],
) -> Result<Vec<serde_json::Value>, E> {
    let (w, h) = (i.visible_width as usize, i.visible_height as usize);
    let word = if i.pixel_format == 8 { 1 } else { 2 };
    if reference.len() != w * h * 3 / 2 * word {
        return Err("unexpected raw reference size".into());
    }
    let sample = |plane: usize, component: usize, x: f64, y: f64| -> f64 {
        let width = if plane == 0 { w } else { w / 2 };
        let height = if plane == 0 { h } else { h / 2 };
        let components = if plane == 0 { 1 } else { 2 };
        let x = x.clamp(0., (width - 1) as f64);
        let y = y.clamp(0., (height - 1) as f64);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(width - 1);
        let y1 = (y0 + 1).min(height - 1);
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let value = |x: usize, y: usize| {
            let offset = if plane == 0 { 0 } else { w * h * word };
            let pos = offset + y * w * word + (x * components + component) * word;
            if word == 1 {
                reference[pos] as f64
            } else {
                u16::from_le_bytes([reference[pos], reference[pos + 1]]) as f64 / 256.
            }
        };
        (value(x0, y0) * (1. - fx) + value(x1, y0) * fx) * (1. - fy)
            + (value(x0, y1) * (1. - fx) + value(x1, y1) * fx) * fy
    };
    let mut reports = Vec::new();
    for (degrees, rotation) in [
        (0, Rotation::R0),
        (90, Rotation::R90),
        (180, Rotation::R180),
        (270, Rotation::R270),
    ] {
        let pixels = frame.validation_pixels(rotation)?;
        if pixels.len() != 96 * 64 {
            return Err("wrong validation target extent".into());
        }
        for (x, y) in [(2usize, 2usize), (12, 20), (84, 32), (48, 52)] {
            if pixels[y * 96 + x] != [0, 0, 0, 0] {
                return Err(format!(
                    "native surface escaped its GPUI content mask at {x},{y}: {:?}",
                    pixels[y * 96 + x]
                )
                .into());
            }
        }
        let mut largest = 0i32;
        let mut samples = Vec::new();
        for (x, y) in [(20usize, 16usize), (48, 30), (76, 44)] {
            let u = (x as f64 + 0.5 - 8.) / 80.;
            let v = (y as f64 + 0.5 - 8.) / 48.;
            let (u, v) = match degrees {
                0 => (u, v),
                90 => (v, 1. - u),
                180 => (1. - u, 1. - v),
                _ => (1. - v, u),
            };
            let luma = (sample(0, 0, u * w as f64 - 0.5, v * h as f64 - 0.5) - 16.) / 219.;
            let cx = u * w as f64 / 2. - if i.chroma_location == 2 { 0.5 } else { 0.25 };
            let cy = v * h as f64 / 2. - 0.5;
            let cb = (sample(1, 0, cx, cy) - 128.) / 224.;
            let cr = (sample(1, 1, cx, cy) - 128.) / 224.;
            let expected = [
                luma + 1.5748 * cr,
                luma - 0.1873242729306488 * cb - 0.46812427293064884 * cr,
                luma + 1.8556 * cb,
            ]
            .map(|v| (v.clamp(0., 1.) * 255.).round() as u8);
            let actual = pixels[y * 96 + x];
            for c in 0..3 {
                largest = largest.max((actual[c] as i32 - expected[c] as i32).abs());
            }
            if actual[3] != 255 {
                return Err("native video alpha is not opaque inside its mask".into());
            }
            samples.push(
                serde_json::json!({"position":[x,y],"expected_rgb":expected,"actual_rgba":actual}),
            );
        }
        if largest > 3 {
            return Err(format!("actual GPUI native shader differs from independent color/rotation reference: rotation={degrees} max_code_difference={largest}, samples={samples:?}").into());
        }
        reports.push(serde_json::json!({"rotation":degrees,"largest_rgb_code_difference":largest,"mask_points":4,"color_points":3,"samples":samples,"readback_scope":"test-only 96x64 output","monitor_scanout":false}));
    }
    Ok(reports)
}
