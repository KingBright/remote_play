//! Real native-window probe of the existing design, not a replacement app.
//! Reuses the original design system and two original visual functions verbatim.
//! No networking, capture, recording, account state or input injection is started.
use remote_play_app::product_components;
#[path = "../src/design_system.rs"]
mod design_system;
#[path = "../src/original_design.rs"]
mod original_design;
use design_system::*;
use gpui::prelude::FluentBuilder;
use gpui::*;
use original_design::{full_idle_canvas_stage, stream_status_capsule_card};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use remote_play_app::product_components::{
    assets::UiAsset,
    component::{self, IconName, icon},
    theme::{ActiveTheme},
};

struct OriginalDesignProbe {
    renders: Arc<AtomicU64>,
    show_drawer: bool,
}
impl Render for OriginalDesignProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.renders.fetch_add(1, Ordering::Relaxed) == 0 {
            if let Some(gpu) = _window.gpu_specs() {
                println!(
                    "ORIGINAL_GPUI_GPU {}",
                    serde_json::json!({"device":gpu.device_name,"software_emulated":gpu.is_software_emulated})
                );
                assert!(
                    !gpu.is_software_emulated,
                    "hardware GPU rendering required for this acceptance"
                );
            } else {
                println!("ORIGINAL_GPUI_GPU metadata unavailable on this backend");
            }
        }
        let theme = cx.theme().clone();
        let mut root = div()
            .relative()
            .size_full()
            .bg(theme.surface.canvas)
            .text_color(theme.content.primary)
            .font_family("sans-serif")
            .child(full_idle_canvas_stage(cx));
        root = root.child(
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
                    "Original GPUI design · compatibility probe".into(),
                    false,
                    &theme,
                    Some(
                        div()
                            .id("original-probe-toggle")
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(theme.action.primary.bg)
                            .text_color(theme.action.primary.fg)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_drawer = !this.show_drawer;
                                cx.notify();
                            }))
                            .child("Devices")
                            .into_any_element(),
                    ),
                )),
        );
        if self.show_drawer {
            root = root.child(
                div()
                    .absolute()
                    .top(px(78.))
                    .left(px(16.))
                    .w(px(320.))
                    .p_5()
                    .rounded(px(14.))
                    .bg(color_glass_card())
                    .border_1()
                    .border_color(color_border_fine())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Your devices"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.content.secondary)
                            .child(
                                "Platform rendering check only. No device connection is started.",
                            ),
                    ),
            );
        }
        root
    }
}
fn main() {
    let output = std::env::var("RP_GPUI_PROBE_RECEIPT").expect("explicit receipt path required");
    assert!(
        !std::path::Path::new(&output).exists(),
        "receipt already exists"
    );
    let seconds = std::env::var("RP_GPUI_PROBE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(12)
        .clamp(3, 60);
    let began = Instant::now();
    let renders = Arc::new(AtomicU64::new(0));
    let count = renders.clone();
    let saved_renders = renders.clone();
    let saved_output = output.clone();
    Application::new()
        .with_assets(UiAsset)
        .run(move |cx: &mut App| {
            component::init(cx).expect("Ely native acceptance assets");
            remote_play_app::product_components::theme::install(
                WindowAppearance::Dark,
                remote_play_themes(), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds(
                        point(px(100.), px(100.)),
                        size(px(1120.), px(740.)),
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("RemotePlay original design compatibility".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                move |_, cx| {
                    cx.new(|_| OriginalDesignProbe {
                        renders: count,
                        show_drawer: true,
                    })
                },
            )
            .expect("native window creation failed");
            cx.spawn(async move |cx| {
                Timer::after(Duration::from_secs(seconds)).await;
                // Cocoa terminate does not return to main; write observed evidence first.
                let rendered=saved_renders.load(Ordering::Relaxed);
                assert!(rendered>0,"native view never rendered");
                let receipt=serde_json::json!({"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,
                    "gpui":"0.3.3","ely_revision":"f756043853ca93407e2da5d07cfe520c84f90963","native_window_created":true,"render_calls":rendered,
                    "elapsed_ms":began.elapsed().as_millis(),"original_design_system_reused":true,
                    "full_product_restored":false,"video_tested":false,"network_started":false,"input_injected":false});
                std::fs::write(&saved_output,serde_json::to_vec_pretty(&receipt).unwrap()).expect("receipt write failed");
                println!("ORIGINAL_GPUI_NATIVE_WINDOW {receipt}");
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let rendered = renders.load(Ordering::Relaxed);
    assert!(rendered > 0, "native view never rendered");
    let receipt = serde_json::json!({"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"gpui":"0.3.3","ely_revision":"f756043853ca93407e2da5d07cfe520c84f90963","native_window_created":true,"render_calls":rendered,"elapsed_ms":began.elapsed().as_millis(),"original_design_system_reused":true,"full_product_restored":false,"video_tested":false,"network_started":false,"input_injected":false});
    std::fs::write(output, serde_json::to_vec_pretty(&receipt).unwrap())
        .expect("receipt write failed");
    println!("ORIGINAL_GPUI_NATIVE_WINDOW {receipt}");
}
