//! Original product visuals, shared across desktop renderers without redesign.
//! The production GPUI view and platform probe use these same functions.
use crate::design_system::{
    color_accent_cyan, color_accent_emerald, color_border_fine, color_glass_card,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use crate::product_components::{
    component::{IconName, icon},
    theme::{ActiveTheme, Theme},
};

pub(crate) fn full_idle_canvas_stage<T: 'static>(cx: &mut Context<T>) -> Div {
    let theme = cx.theme().clone();
    div()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x0e0f12))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .p_8()
                .rounded(px(8.0))
                .bg(color_glass_card())
                .border_1()
                .border_color(color_border_fine())
                .shadow_lg()
                .child(
                    div()
                        .size(px(60.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(0x12151c))
                        .border_1()
                        .border_color(color_accent_cyan())
                        .child(
                            icon(IconName::Server)
                                .size(px(26.0))
                                .color(color_accent_cyan()),
                        ),
                )
                .child(
                    div()
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .child("RemotePlay"),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(theme.content.tertiary)
                        .child("Ready for incoming and outgoing sessions"),
                ),
        )
}

pub(crate) fn stream_status_capsule_card(
    title: String,
    is_live: bool,
    compact_stats: String,
    compact: bool,
    theme: &Theme,
    action_slot: Option<AnyElement>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(color_glass_card())
        .border_1()
        .border_color(color_border_fine())
        .rounded_full()
        .shadow_lg()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_1()
                .child(
                    div()
                        .size(px(8.0))
                        .flex_none()
                        .rounded_full()
                        .bg(if is_live {
                            Hsla::from(color_accent_emerald())
                        } else {
                            theme.content.disabled
                        }),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.content.primary)
                        .whitespace_nowrap()
                        .max_w(if compact { px(120.0) } else { px(220.0) })
                        .truncate()
                        .child(title),
                ),
        )
        .child(div().w(px(1.0)).h(px(16.0)).bg(theme.border.divider))
        .child(
            div()
                .text_size(px(10.0))
                // Preserve fixed-width telemetry on DirectWrite, which does not
                // interpret the CSS generic name as an installed font family.
                .font_family(crate::design_system::product_mono_font())
                .text_color(theme.content.secondary)
                .whitespace_nowrap()
                .child(compact_stats),
        )
        .when_some(action_slot, |this, slot| this.child(slot))
}
