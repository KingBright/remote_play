//! Local encoded test clip -> actual MF decoder -> GPU copy -> original GPUI surface.
//! No capture, network identity, remote input, files transfer, or installed app changes.
#[cfg(target_os = "windows")]
#[path = "../src/design_system.rs"]
mod design_system;
#[cfg(target_os = "windows")]
#[path = "support/hevc_fixture.rs"]
mod fixture;
#[cfg(target_os = "windows")]
#[path = "../src/original_design.rs"]
mod original_design;

#[cfg(target_os = "windows")]
mod check {
    use super::{
        design_system::*, fixture::access_units, original_design::stream_status_capsule_card,
    };
    use client::windows_hevc::{D3dDecodedFrame, WindowsHevcDecoder};
    use gpui::{
        native_video::{
            ChromaLocation, D3dVideoCopyPool, Matrix, Range, ReadyD3dVideoFrame, Rotation,
            VideoColor, VideoGeometry, VideoSurfaceStats,
        },
        *,
    };
    use std::{
        collections::BTreeSet,
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
    #[derive(Default)]
    struct State {
        latest: Mutex<Option<ReadyD3dVideoFrame>>,
        worker_report: Mutex<Option<serde_json::Value>>,
        worker_error: Mutex<Option<String>>,
        surface_stats: Mutex<Option<Arc<VideoSurfaceStats>>>,
        seen: Mutex<BTreeSet<u64>>,
        submitted: Mutex<BTreeSet<u64>>,
        window_ready: (Mutex<bool>, std::sync::Condvar),
        paint_observations: Mutex<Vec<serde_json::Value>>,
        done: AtomicBool,
        stop: AtomicBool,
        renders: AtomicU64,
        overwritten: AtomicU64,
        frame_ready: tokio::sync::Notify,
    }
    struct View {
        state: Arc<State>,
        frame: Option<ReadyD3dVideoFrame>,
    }
    impl Render for View {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.state.renders.fetch_add(1, Ordering::Relaxed) == 0 {
                let gpu = window.gpu_specs().expect("native GPU metadata");
                assert!(!gpu.is_software_emulated, "software GPU not accepted");
                println!("DECODED_WINDOW_GPU {}", gpu.device_name);
                *self.state.window_ready.0.lock().unwrap()=true;
                self.state.window_ready.1.notify_all();
            }
            // Observe the previous frame after the renderer processed its scene,
            // not when Render merely creates an element carrying that frame.
            if let Some(previous)=&self.frame && previous.frame.was_submitted() {
                self.state.submitted.lock().unwrap().insert(previous.tag);
            }
            if let Some(next) = self.state.latest.lock().unwrap().take() {
                self.frame = Some(next);
            }
            let theme = cx.theme().clone();
            let mut body = div()
                .relative()
                .size_full()
                .bg(theme.surface.canvas)
                .text_color(theme.content.primary)
                .font_family(".SystemUIFont");
            if let Some(frame) = &self.frame {
                let first = self.state.seen.lock().unwrap().insert(frame.tag);
                if first {
                    self.state.paint_observations.lock().unwrap().push(serde_json::json!({"pts_100ns":frame.tag,"copy_submit_to_ui_render_us":frame.copy_started.elapsed().as_micros(),"copy_ready_to_ui_render_us":frame.copy_ready.elapsed().as_micros(),"geometry":frame.frame.geometry().visible}));
                }
                body = body.child(
                    div().absolute().inset_0().child(
                        surface(frame.frame.clone())
                            .object_fit(ObjectFit::Contain)
                            .size_full(),
                    ),
                );
            } else {
                body = body.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child("Waiting for actual hardware-decoded video"),
                );
            }
            body.child(
                div()
                    .absolute()
                    .top(px(14.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(stream_status_capsule_card(
                        "RemotePlay".into(),
                        false,
                        "Hardware HEVC · native video · original controls".into(),
                        false,
                        &theme,
                        None,
                    )),
            )
        }
    }
    fn color(frame: &D3dDecodedFrame) -> Result<VideoColor, E> {
        // Preserve the source's VUI even if the MFT output media type omits it.
        // Values here came from bounded bitstream parsing, not filename, resolution
        // inference, a test-only override or hard-coded display defaults.
        let signal = frame
            .source_signal
            .ok_or("source did not signal a color description")?;
        if signal.matrix != Some(1)
            || signal.primaries != Some(1)
            || signal.transfer != Some(1)
            || signal.full_range
        {
            return Err(format!("unexpected test signal {signal:?}").into());
        }
        if frame.matrix.is_some_and(|m| m != 0 && m != 1)
            || frame.nominal_range.is_some_and(|r| r != 0 && r != 2)
        {
            return Err("decoder output color conflicts with source VUI".into());
        }
        let chroma = match frame.chroma_location.unwrap_or(0) {
            0 => ChromaLocation::Left,
            1 => ChromaLocation::Center,
            _ => return Err("unsupported progressive chroma position".into()),
        };
        Ok(VideoColor {
            matrix: Matrix::Bt709,
            range: Range::Limited,
            chroma,
        })
    }
    fn publish(
        pool: &mut D3dVideoCopyPool,
        state: &State,
        copy_times: &mut Vec<u64>,
    ) -> Result<(), E> {
        for ready in pool.take_ready()? {
            copy_times.push(
                ready
                    .copy_ready
                    .duration_since(ready.copy_started)
                    .as_micros() as u64,
            );
            let previous = state.latest.lock().unwrap().replace(ready);
            if previous.is_some() {
                state.overwritten.fetch_add(1, Ordering::Relaxed);
            }
            drop(previous);
            state.frame_ready.notify_one();
        }
        Ok(())
    }
    fn worker(path: &PathBuf, state: &State) -> Result<serde_json::Value, E> {
        let data = std::fs::read(path)?;
        if data.len() > 32 * 1024 * 1024 {
            return Err("encoded fixture exceeds bound".into());
        }
        let sequence = client::hevc_sequence::sequence_info(&data)?.ok_or("fixture SPS missing")?;
        let units = access_units(&data);
        if units.len() != 60 {
            return Err(format!("expected 60 encoded frames, got {}", units.len()).into());
        }
        // Keep cold window/font startup separate from paced video performance.
        // Do not send the finite clip into a viewer that does not exist yet.
        let startup=Instant::now();
        let ready=state.window_ready.0.lock().unwrap();
        let (ready,timeout)=state.window_ready.1.wait_timeout_while(ready,Duration::from_secs(5),|ready|!*ready).unwrap();
        if !*ready || timeout.timed_out() {return Err("native window did not become ready within the startup budget".into());}
        drop(ready);
        let ui_startup_wait_us=startup.elapsed().as_micros();
        let mut decoder = WindowsHevcDecoder::new(sequence, [30, 1])?;
        let mut pool = D3dVideoCopyPool::new(decoder.device().clone())?;
        *state.surface_stats.lock().unwrap() = Some(pool.surface_stats());
        let mut copy_times = Vec::new();
        let mut submit_times = Vec::new();
        let mut decoder_samples = Vec::new();
        let begun = Instant::now();
        let mut accept = |decoded: Arc<D3dDecodedFrame>,
                          pool: &mut D3dVideoCopyPool|
         -> Result<(), E> {
            if decoded.format.0 != if sequence.bit_depth == 10 { 104 } else { 103 } {
                return Err("decoder silently changed bit depth".into());
            }
            let geometry = VideoGeometry {
                coded: decoded.coded,
                visible: decoded.visible,
                pixel_aspect: decoded.sample_aspect,
                rotation: Rotation::R0,
            };
            let metadata = color(&decoded)?;
            let tag = u64::try_from(decoded.pts_100ns)?;
            decoder_samples.push(serde_json::json!({"pts":tag,"subresource":decoded.subresource,"coded":decoded.coded,"visible":decoded.visible,"format":decoded.format.0,"matrix":decoded.matrix,"range":decoded.nominal_range,"transfer":decoded.transfer}));
            let lease = decoded.retention_lease()?;
            // The retained object is the exact IMFSample used to obtain this slice.
            let copied = unsafe {
                pool.submit(
                    decoded.texture(),
                    decoded.subresource,
                    geometry,
                    metadata,
                    lease,
                    tag,
                )?
            };
            if !copied {
                return Err("native pool saturated under matched 30fps validation workload".into());
            }
            Ok(())
        };
        for (i, unit) in units.iter().enumerate() {
            let schedule = begun + Duration::from_nanos(i as u64 * 1_000_000_000 / 30);
            while Instant::now() < schedule {
                if state.stop.load(Ordering::Acquire) {
                    return Err("test window closed before completion".into());
                }
                publish(&mut pool, state, &mut copy_times)?;
                std::thread::sleep(Duration::from_millis(1));
            }
            let start = Instant::now();
            let frames = decoder.push(
                unit,
                i as i64 * 333333,
                remote_core::media_plane::is_hevc_keyframe(unit),
            )?;
            submit_times.push(start.elapsed().as_micros() as u64);
            for frame in frames {
                accept(frame, &mut pool)?;
            }
            publish(&mut pool, state, &mut copy_times)?;
        }
        for frame in decoder.finish()? {
            accept(frame, &mut pool)?;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while pool.pending_count() > 0 {
            publish(&mut pool, state, &mut copy_times)?;
            if Instant::now() > deadline {
                return Err("GPU copy completion timeout".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let stats = pool.stats();
        if decoder.output_frames != 60
            || stats.gpu_copies != 60
            || stats.gpu_completed != 60
            || stats.allocations > 4
            || stats.pool_busy != 0
        {
            return Err(format!(
                "decoder/copy accounting mismatch: decoded={}, {stats:?}",
                decoder.output_frames
            )
            .into());
        }
        Ok(
            serde_json::json!({"passed":true,"decoder":decoder.decoder_name,"adapter":decoder.adapter_name,"input_units":units.len(),"hardware_frames":decoder.output_frames,"bit_depth":sequence.bit_depth,"source_coded":sequence.coded,"source_visible":sequence.visible,
            "ui_startup_wait_us":ui_startup_wait_us,"copies":stats.gpu_copies,"copy_completed":stats.gpu_completed,"shared_texture_allocations":stats.allocations,"max_pending":stats.pending_high_water,"pool_busy":stats.pool_busy,
            "one_explicit_gpu_copy_per_frame":true,"cpu_decoded_pixel_maps":0,"cpu_rgba_conversion":false,"paced_input_fps":30,"elapsed_ms":begun.elapsed().as_millis(),
            "decoder_call_us":submit_times,"copy_submit_to_observed_gpu_ready_us":copy_times,"sample_metadata":decoder_samples}),
        )
    }
    pub fn run() -> Result<(), E> {
        let path = PathBuf::from(std::env::var("RP_NATIVE_HEVC_FIXTURE")?);
        let output = PathBuf::from(std::env::var("RP_NATIVE_HEVC_RECEIPT")?);
        if output.exists() {
            return Err("refusing to overwrite validation receipt".into());
        }
        let state = Arc::new(State::default());
        let observe = state.clone();
        let view_state = state.clone();
        let handle = std::thread::Builder::new()
            .name("native-hevc-validation".into())
            .spawn(move || {
                match worker(&path, &observe) {
                    Ok(report) => *observe.worker_report.lock().unwrap() = Some(report),
                    Err(error) => *observe.worker_error.lock().unwrap() = Some(error.to_string()),
                };
                observe.done.store(true, Ordering::Release);
                observe.frame_ready.notify_one();
            })?;
        let output2 = output.clone();
        let report_state = state.clone();
        let updates = state.clone();
        Application::new().with_assets(UiAsset).run(move|cx:&mut App|{
            component::init(cx);cx.set_global(GlobalTheme::new_with_themes(WindowAppearance::Dark,remote_play_themes()));
            cx.open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(bounds(point(px(100.),px(100.)),size(px(1120.),px(740.))))),titlebar:Some(TitlebarOptions{title:Some("RemotePlay native decoded video validation".into()),..Default::default()}),..Default::default()},move|_,cx|cx.new(|_|View{state:view_state,frame:None})).expect("native window creation");
            cx.spawn(async move|cx|{
                loop{updates.frame_ready.notified().await;if updates.stop.load(Ordering::Acquire){break;}let _=cx.update(|cx|cx.refresh_windows());if updates.done.load(Ordering::Acquire){break;}}
            }).detach();
            cx.spawn(async move|cx|{
                for _ in 0..150 {Timer::after(Duration::from_millis(50)).await;if report_state.done.load(Ordering::Acquire){break;}}
                // Let the final decoded resource enter a compositor frame; still not
                // a monitor scan-out measurement or physical input acceptance.
                for _ in 0..4 {Timer::after(Duration::from_millis(50)).await;let _=cx.update(|cx|cx.refresh_windows());}
                report_state.stop.store(true,Ordering::Release);report_state.frame_ready.notify_one();
                let decoder=report_state.worker_report.lock().unwrap().clone();let error=report_state.worker_error.lock().unwrap().clone();
                let surface=report_state.surface_stats.lock().unwrap().clone();let counters=surface.as_ref().map(|s|s.snapshot());let surface_error=surface.as_ref().and_then(|s|s.last_error.lock().unwrap().clone());
                let seen:Vec<_>=report_state.seen.lock().unwrap().iter().copied().collect();
                let submitted:Vec<_>=report_state.submitted.lock().unwrap().iter().copied().collect();
                let passed=submitted.len()>=55&&error.is_none()&&surface_error.is_none()&&decoder.as_ref().is_some_and(|v|v["passed"]==true)&&seen.len()>=55&&counters.is_some_and(|c|c[0]<=4&&c[1]>=55&&c[2]>0&&c[3]==0&&c[4]==0&&c[5]==0);
                let receipt=serde_json::json!({"passed":passed,"actual_hevc_decoder_to_original_gpui_window":true,"source_is_local_encoded_test_clip":true,"worker_error":error,"decoder":decoder,"unique_pts_rendered":seen,"unique_pts_gpu_submitted":submitted,"startup_excluded_from_paced_clip_but_reported":true,"render_count":report_state.renders.load(Ordering::Relaxed),"latest_slot_overwrites":report_state.overwritten.load(Ordering::Relaxed),"surface_counters":counters,"surface_error":surface_error,"ui_observations":report_state.paint_observations.lock().unwrap().clone(),"no_capture":true,"no_network":true,"no_remote_input":true,"screen_scanout_latency_measured":false});
                std::fs::write(&output2,serde_json::to_vec_pretty(&receipt).unwrap()).expect("write receipt");println!("NATIVE_DECODED_WINDOW passed={passed} unique_frames={}",seen.len());
                let _=cx.update(|cx|cx.quit());
            }).detach();
        });
        state.stop.store(true, Ordering::Release);
        state.frame_ready.notify_one();
        handle.join().map_err(|_| "decoder worker panicked")?;
        let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(output)?)?;
        if receipt["passed"] != true {
            return Err(format!(
                "native decoded window incomplete: {:?}",
                receipt["worker_error"]
            )
            .into());
        }
        Ok(())
    }
}
#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    check::run()
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This native decoded window validation targets Windows.");
}
