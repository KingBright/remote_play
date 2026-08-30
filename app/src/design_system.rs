use gpui::{Hsla, Rgba, rgb, rgba};
use yororen_ui::theme::{Theme, ThemeSet};

pub fn remote_play_themes() -> ThemeSet {
    ThemeSet::new(light_theme()).dark(dark_theme())
}

#[allow(dead_code)]
#[inline]
pub fn color_accent_cyan() -> Rgba {
    rgb(0x00d1ff)
}

#[allow(dead_code)]
#[inline]
pub fn color_accent_emerald() -> Rgba {
    rgb(0x10b981)
}

#[allow(dead_code)]
#[inline]
pub fn color_accent_purple() -> Rgba {
    rgb(0xa855f7)
}

#[allow(dead_code)]
#[inline]
pub fn color_accent_amber() -> Rgba {
    rgb(0xf59e0b)
}

#[allow(dead_code)]
#[inline]
pub fn color_accent_rose() -> Rgba {
    rgb(0xf43f5e)
}

#[allow(dead_code)]
#[inline]
pub fn color_glass_bg() -> Hsla {
    rgba(0x121316e6).into()
}

#[allow(dead_code)]
#[inline]
pub fn color_glass_card() -> Hsla {
    rgba(0x1c1e22d9).into()
}

#[allow(dead_code)]
#[inline]
pub fn color_border_fine() -> Rgba {
    rgb(0x2d2f34)
}

fn dark_theme() -> Theme {
    let mut theme = Theme::default_dark();

    theme.surface.canvas = rgb(0x121316).into();
    theme.surface.base = rgb(0x1a1c20).into();
    theme.surface.raised = rgb(0x22252b).into();
    theme.surface.sunken = rgb(0x0d0e11).into();
    theme.surface.hover = rgb(0x282b32).into();

    theme.content.primary = rgb(0xf8fafc).into();
    theme.content.secondary = rgb(0xc5c6cc).into();
    theme.content.tertiary = rgb(0x859399).into();
    theme.content.disabled = rgb(0x47494e).into();
    theme.content.on_primary = rgb(0x02080a).into();
    theme.content.on_status = rgb(0x02080a).into();

    theme.border.default = rgb(0x2d2f34).into();
    theme.border.muted = rgb(0x23252a).into();
    theme.border.focus = rgb(0x00d1ff).into();
    theme.border.divider = rgb(0x202227).into();

    theme.action.neutral.bg = rgb(0x22252b).into();
    theme.action.neutral.hover_bg = rgb(0x2d3139).into();
    theme.action.neutral.active_bg = rgb(0x383d47).into();
    theme.action.neutral.fg = theme.content.primary;
    theme.action.neutral.disabled_bg = rgb(0x17191d).into();
    theme.action.neutral.disabled_fg = theme.content.disabled;

    theme.action.primary.bg = rgb(0x8af0c3).into();
    theme.action.primary.hover_bg = rgb(0xa4f6d2).into();
    theme.action.primary.active_bg = rgb(0x70ddae).into();
    theme.action.primary.fg = theme.content.on_primary;
    theme.action.primary.disabled_bg = rgb(0x325345).into();
    theme.action.primary.disabled_fg = rgb(0x84958d).into();

    theme.action.danger.bg = rgb(0xff8f87).into();
    theme.action.danger.hover_bg = rgb(0xffa69f).into();
    theme.action.danger.active_bg = rgb(0xf37970).into();
    theme.action.danger.fg = rgb(0x1e0908).into();
    theme.action.danger.disabled_bg = rgb(0x54302e).into();
    theme.action.danger.disabled_fg = theme.content.disabled;

    theme.status.success.bg = rgb(0x95edbd).into();
    theme.status.success.fg = rgb(0x092d1d).into();
    theme.status.warning.bg = rgb(0xffd37a).into();
    theme.status.warning.fg = rgb(0x352305).into();
    theme.status.error.bg = rgb(0xff928a).into();
    theme.status.error.fg = rgb(0x3b0d0a).into();
    theme.status.info.bg = rgb(0x8dd7ff).into();
    theme.status.info.fg = rgb(0x08263a).into();

    theme
}

fn light_theme() -> Theme {
    let mut theme = Theme::default_light();

    theme.surface.canvas = rgb(0xeef2f0).into();
    theme.surface.base = rgb(0xffffff).into();
    theme.surface.raised = rgb(0xf8faf9).into();
    theme.surface.sunken = rgb(0xe8edeb).into();
    theme.surface.hover = rgb(0xe3e9e6).into();

    theme.content.primary = rgb(0x121816).into();
    theme.content.secondary = rgb(0x3f4b46).into();
    theme.content.tertiary = rgb(0x68756f).into();
    theme.content.disabled = rgb(0x96a19c).into();
    theme.content.on_primary = rgb(0xffffff).into();
    theme.content.on_status = rgb(0x0b1712).into();

    theme.border.default = rgb(0xcbd4d0).into();
    theme.border.muted = rgb(0xdde4e1).into();
    theme.border.focus = rgb(0x087a55).into();
    theme.border.divider = rgb(0xd7dfdb).into();

    theme.action.neutral.bg = rgb(0xe8eeeb).into();
    theme.action.neutral.hover_bg = rgb(0xdde6e1).into();
    theme.action.neutral.active_bg = rgb(0xd0dcd6).into();
    theme.action.neutral.fg = theme.content.primary;
    theme.action.neutral.disabled_bg = rgb(0xedf1ef).into();
    theme.action.neutral.disabled_fg = theme.content.disabled;

    theme.action.primary.bg = rgb(0x153c2f).into();
    theme.action.primary.hover_bg = rgb(0x0e3025).into();
    theme.action.primary.active_bg = rgb(0x08241b).into();
    theme.action.primary.fg = theme.content.on_primary;
    theme.action.primary.disabled_bg = rgb(0x9aada5).into();
    theme.action.primary.disabled_fg = rgb(0xe9efec).into();

    theme.action.danger.bg = rgb(0xc94843).into();
    theme.action.danger.hover_bg = rgb(0xb83d38).into();
    theme.action.danger.active_bg = rgb(0xa93430).into();
    theme.action.danger.fg = rgb(0xffffff).into();
    theme.action.danger.disabled_bg = rgb(0xd9aaa7).into();
    theme.action.danger.disabled_fg = rgb(0xf7e9e8).into();

    theme.status.success.bg = rgb(0xd7f5e4).into();
    theme.status.success.fg = rgb(0x145f3c).into();
    theme.status.warning.bg = rgb(0xffedc1).into();
    theme.status.warning.fg = rgb(0x704a00).into();
    theme.status.error.bg = rgb(0xffd8d4).into();
    theme.status.error.fg = rgb(0x8e2924).into();
    theme.status.info.bg = rgb(0xd5efff).into();
    theme.status.info.fg = rgb(0x165b82).into();

    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Hsla) -> f32 {
        let color = Rgba::from(color);
        let linear = |value: f32| {
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        let a = relative_luminance(a);
        let b = relative_luminance(b);
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn themes_keep_text_and_actions_readable() {
        let themes = [dark_theme(), light_theme()];
        for theme in themes {
            assert!(
                contrast_ratio(theme.content.primary, theme.surface.canvas) >= 7.0,
                "primary text must meet enhanced contrast"
            );
            assert!(
                contrast_ratio(theme.action.primary.fg, theme.action.primary.bg) >= 4.5,
                "primary actions must meet normal text contrast"
            );
            assert!(
                contrast_ratio(theme.action.danger.fg, theme.action.danger.bg) >= 4.5,
                "danger actions must meet normal text contrast"
            );
        }
    }
}
