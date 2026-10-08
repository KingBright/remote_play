//! Generated GPU patterns in a real GPUI window with the original product capsule.
//! This development probe never creates a device network, media session or input injector.
use remote_play_app::product_components;
#[cfg(target_os = "windows")]
#[path = "../src/design_system.rs"]
mod design_system;
#[cfg(target_os = "windows")]
#[path = "../src/original_design.rs"]
mod original_design;

#[cfg(target_os = "windows")]
mod probe {
    use super::{design_system::*, original_design::stream_status_capsule_card};
    use gpui::{
        native_video::{
            ChromaLocation, D3dVideoFrame, Matrix, Range, Rotation, VideoColor, VideoFormat,
            VideoGeometry, synthetic_frame,
        },
        *,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };
    use remote_play_app::product_components::{
        assets::UiAsset,
        component,
        theme::{ActiveTheme},
    };
    struct NativeWindow {
        frames: Vec<D3dVideoFrame>,
        tick: Arc<AtomicU64>,
        renders: Arc<AtomicU64>,
    }
    impl Render for NativeWindow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.renders.fetch_add(1, Ordering::Relaxed) == 0 {
                let gpu = window
                    .gpu_specs()
                    .expect("Physical GPU metadata is required by this test");
                assert!(
                    !gpu.is_software_emulated,
                    "No software fallback is accepted"
                );
                println!("GPUI_NATIVE_WINDOW_GPU {}", gpu.device_name);
            }
            let tick = self.tick.load(Ordering::Relaxed);
            let theme = cx.theme().clone();
            let mut panels = div().size_full().flex().gap_4().p_4().pt(px(86.));
            for (i, frame) in self.frames.iter().enumerate() {
                let rotation = match (tick / 8 + i as u64) % 4 {
                    0 => Rotation::R0,
                    1 => Rotation::R90,
                    2 => Rotation::R180,
                    _ => Rotation::R270,
                };
                let view = frame
                    .with_geometry(VideoGeometry {
                        rotation,
                        ..frame.geometry()
                    })
                    .expect("fixture geometry");
                panels = panels.child(
                    div()
                        .flex_1()
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme.content.secondary)
                                .child(
                                    [
                                        "NV12 · native GPU",
                                        "P010 · native GPU",
                                        "BGRA · native GPU",
                                    ][i],
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .w_full()
                                .relative()
                                .overflow_hidden()
                                .bg(gpui::rgb(0x101318))
                                .child(surface(view).object_fit(ObjectFit::Contain).size_full()),
                        ),
                );
            }
            div()
                .relative()
                .size_full()
                .bg(theme.surface.canvas)
                .text_color(theme.content.primary)
                // Use GPUI's native system-family selector. CSS aliases are not
                // actual DirectWrite family names and strict test builds reject them.
                .font_family(".SystemUIFont")
                .child(panels)
                .child(
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
                            "Native video validation · original controls".into(),
                            false,
                            &theme,
                            None,
                        )),
                )
        }
    }
    pub fn run() {
        let path = std::path::PathBuf::from(
            std::env::var("RP_NATIVE_WINDOW_RECEIPT").expect("explicit receipt required"),
        );
        assert!(!path.exists(), "do not overwrite evidence");
        assert_eq!(
            std::env::var("RP_NATIVE_VIDEO_GPU_TEST").as_deref(),
            Ok("1")
        );
        let color = VideoColor {
            matrix: Matrix::Bt709,
            range: Range::Limited,
            chroma: ChromaLocation::Center,
        };
        let frames: Vec<_> = [VideoFormat::Nv12, VideoFormat::P010, VideoFormat::Bgra8]
            .into_iter()
            .map(|format| {
                synthetic_frame(format, 1920, 1088, color).expect("physical texture creation")
            })
            .collect();
        let observations: Vec<_> = frames.iter().map(|f| f.stats().clone()).collect();
        let renders = Arc::new(AtomicU64::new(0));
        let tick = Arc::new(AtomicU64::new(0));
        let view_renders = renders.clone();
        let view_tick = tick.clone();
        let out = path.clone();
        Application::new().with_assets(UiAsset).run(move |cx:&mut App| {
            component::init(cx).expect("Ely native acceptance assets");remote_play_app::product_components::theme::install(WindowAppearance::Dark,remote_play_themes(), cx);
            cx.open_window(WindowOptions{window_bounds:Some(WindowBounds::Windowed(bounds(point(px(100.),px(100.)),size(px(1120.),px(740.))))),
                titlebar:Some(TitlebarOptions{title:Some("RemotePlay native-video development check".into()),..Default::default()}),..Default::default()},
                move |_,cx|cx.new(|_|NativeWindow{frames,tick:view_tick,renders:view_renders})).expect("native window creation");
            cx.spawn(async move |cx| {
                for i in 0..40 {
                    Timer::after(Duration::from_millis(200)).await;tick.store(i,Ordering::Relaxed);
                    let _=cx.update(|cx|cx.refresh_windows());
                }
                let counters:Vec<_>=observations.iter().map(|s|s.snapshot()).collect();
                let errors:Vec<_>=observations.iter().map(|s|s.last_error.lock().unwrap().clone()).collect();
                let passed=renders.load(Ordering::Relaxed)>4&&counters.iter().all(|c|c[0]==1&&c[1]>4&&c[2]>0&&c[3]==0&&c[4]==0&&c[5]==0)&&errors.iter().all(Option::is_none);
                let receipt=serde_json::json!({"passed":passed,"real_gpui_window":true,"original_capsule_component":true,"three_native_formats":true,
                    "source":"generated immutable GPU patterns, not decoder output","source_extent":[1920,1088],"render_calls":renders.load(Ordering::Relaxed),
                    "counters_order":["imports","submitted","GPU-completed","busy","queue_full","rejected"],"format_counters":counters,"errors":errors,
                    "rotations_exercised":4,"network_started":false,"capture_started":false,"input_injected":false,"scanout_latency_measured":false});
                std::fs::write(&out,serde_json::to_vec_pretty(&receipt).unwrap()).expect("receipt write");
                println!("GPUI_NATIVE_WINDOW_RECEIPT {receipt}");
                let _=cx.update(|cx|cx.quit());
            }).detach();
        });
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("completed-window evidence"))
                .unwrap();
        assert_eq!(receipt["passed"], true, "native window validation failed");
    }
}
#[cfg(target_os = "windows")]
fn main() {
    probe::run();
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("The D3D window validation must run on its physical Windows device.");
    std::process::exit(2);
}
