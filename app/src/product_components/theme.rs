// Derived from Yororen UI 0.2.0, commit 31005a92286067b56dcc45e7833b91e428533274.
// Copyright 2026 MeowLynxSea; Apache-2.0. See LICENSE-APACHE and NOTICE.
// Modified by RemotePlay: semantic snapshot backed by Ely; original theme manager removed.
use gpui::{App, Hsla, WindowAppearance, hsla, rgb};

#[derive(Clone, Debug, Default)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}
impl TextDirection {
    fn is_rtl(&self) -> bool {
        matches!(self, Self::Rtl)
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub surface: SurfaceTheme,
    pub content: ContentTheme,
    pub border: BorderTheme,
    pub action: ActionTheme,
    pub status: StatusTheme,
    pub shadow: ShadowTheme,
    /// Text direction (LTR or RTL)
    pub text_direction: TextDirection,
}

#[derive(Clone, Debug)]
pub struct SurfaceTheme {
    pub canvas: Hsla,
    pub base: Hsla,
    pub raised: Hsla,
    pub sunken: Hsla,
    pub hover: Hsla,
}

#[derive(Clone, Debug)]
pub struct ContentTheme {
    pub primary: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub disabled: Hsla,
    pub on_primary: Hsla,
    pub on_status: Hsla,
}

#[derive(Clone, Debug)]
pub struct BorderTheme {
    pub default: Hsla,
    pub muted: Hsla,
    pub focus: Hsla,
    pub divider: Hsla,
}

#[derive(Clone, Debug)]
pub struct ActionTheme {
    pub neutral: ActionVariant,
    pub primary: ActionVariant,
    pub danger: ActionVariant,
}

#[derive(Clone, Debug)]
pub struct ActionVariant {
    pub bg: Hsla,
    pub hover_bg: Hsla,
    pub active_bg: Hsla,
    pub fg: Hsla,
    pub disabled_bg: Hsla,
    pub disabled_fg: Hsla,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionVariantKind {
    Neutral,
    Primary,
    Danger,
}

#[derive(Clone, Debug)]
pub struct StatusTheme {
    pub success: StatusVariant,
    pub warning: StatusVariant,
    pub error: StatusVariant,
    pub info: StatusVariant,
}

#[derive(Clone, Debug)]
pub struct StatusVariant {
    pub bg: Hsla,
    pub fg: Hsla,
}

#[derive(Clone, Debug)]
pub struct ShadowTheme {
    pub elevation_1: Hsla,
    pub elevation_2: Hsla,
}

impl Theme {
    pub fn default_dark() -> Self {
        let content = ContentTheme {
            primary: rgb(0xF2F2F3).into(),
            secondary: rgb(0xC8C8CC).into(),
            tertiary: rgb(0x9B9BA1).into(),
            disabled: rgb(0x6F6F76).into(),
            on_primary: rgb(0x0B0B0D).into(),
            on_status: rgb(0x0B0B0D).into(),
        };

        Self {
            surface: SurfaceTheme {
                canvas: rgb(0x0F0F11).into(),
                base: rgb(0x151518).into(),
                raised: rgb(0x1D1D21).into(),
                sunken: rgb(0x111113).into(),
                hover: rgb(0x232327).into(),
            },
            content: content.clone(),
            border: BorderTheme {
                default: rgb(0x2A2A2F).into(),
                muted: rgb(0x1E1E22).into(),
                focus: rgb(0x8BB0FF).into(),
                divider: rgb(0x1E1E22).into(),
            },
            action: ActionTheme {
                neutral: ActionVariant {
                    bg: rgb(0x1D1D21).into(),
                    hover_bg: rgb(0x24242A).into(),
                    active_bg: rgb(0x2A2A31).into(),
                    fg: content.primary,
                    disabled_bg: rgb(0x1A1A1D).into(),
                    disabled_fg: content.disabled,
                },
                primary: ActionVariant {
                    bg: rgb(0xF4F4F6).into(),
                    hover_bg: rgb(0xFFFFFF).into(),
                    active_bg: rgb(0xE9E9EC).into(),
                    fg: content.on_primary,
                    disabled_bg: rgb(0xE0E0E4).into(),
                    disabled_fg: rgb(0x5B5B61).into(),
                },
                danger: ActionVariant {
                    bg: rgb(0xFFB4AE).into(),
                    hover_bg: rgb(0xFFA099).into(),
                    active_bg: rgb(0xFF8A82).into(),
                    fg: content.on_status,
                    disabled_bg: rgb(0xE0B3AF).into(),
                    disabled_fg: rgb(0x5B5B61).into(),
                },
            },
            status: StatusTheme {
                success: StatusVariant {
                    bg: rgb(0xB9F5C9).into(),
                    fg: content.on_status,
                },
                warning: StatusVariant {
                    bg: rgb(0xFFE1A6).into(),
                    fg: content.on_status,
                },
                error: StatusVariant {
                    bg: rgb(0xFFB4AE).into(),
                    fg: content.on_status,
                },
                info: StatusVariant {
                    bg: rgb(0xB6D9FF).into(),
                    fg: content.on_status,
                },
            },
            shadow: ShadowTheme {
                elevation_1: hsla(0.0, 0.0, 0.0, 0.3),
                elevation_2: hsla(0.0, 0.0, 0.0, 0.45),
            },
            text_direction: TextDirection::Ltr,
        }
    }

    pub fn default_light() -> Self {
        let content = ContentTheme {
            primary: rgb(0x141416).into(),
            secondary: rgb(0x3E3E45).into(),
            tertiary: rgb(0x6B6B73).into(),
            disabled: rgb(0x9A9AA2).into(),
            on_primary: rgb(0xFFFFFF).into(),
            on_status: rgb(0x0B0B0D).into(),
        };

        Self {
            surface: SurfaceTheme {
                canvas: rgb(0xF4F4F6).into(),
                base: rgb(0xFFFFFF).into(),
                raised: rgb(0xFBFBFD).into(),
                sunken: rgb(0xEFEFF2).into(),
                hover: rgb(0xE6E6EA).into(),
            },
            content: content.clone(),
            border: BorderTheme {
                default: rgb(0xD8D8DD).into(),
                muted: rgb(0xE3E3E8).into(),
                focus: rgb(0x2F63FF).into(),
                divider: rgb(0xE3E3E8).into(),
            },
            action: ActionTheme {
                neutral: ActionVariant {
                    bg: rgb(0xF1F1F3).into(),
                    hover_bg: rgb(0xE6E6EA).into(),
                    active_bg: rgb(0xDADADF).into(),
                    fg: content.primary,
                    disabled_bg: rgb(0xE7E7EA).into(),
                    disabled_fg: content.disabled,
                },
                primary: ActionVariant {
                    bg: rgb(0x121214).into(),
                    hover_bg: rgb(0x0C0C0D).into(),
                    active_bg: rgb(0x000000).into(),
                    fg: content.on_primary,
                    disabled_bg: rgb(0x2A2A2E).into(),
                    disabled_fg: rgb(0xD0D0D6).into(),
                },
                danger: ActionVariant {
                    bg: rgb(0xFFB4AE).into(),
                    hover_bg: rgb(0xFFA099).into(),
                    active_bg: rgb(0xFF8A82).into(),
                    fg: content.on_status,
                    disabled_bg: rgb(0xF0CBC7).into(),
                    disabled_fg: content.disabled,
                },
            },
            status: StatusTheme {
                success: StatusVariant {
                    bg: rgb(0xB9F5C9).into(),
                    fg: content.on_status,
                },
                warning: StatusVariant {
                    bg: rgb(0xFFE1A6).into(),
                    fg: content.on_status,
                },
                error: StatusVariant {
                    bg: rgb(0xFFB4AE).into(),
                    fg: content.on_status,
                },
                info: StatusVariant {
                    bg: rgb(0xB6D9FF).into(),
                    fg: content.on_status,
                },
            },
            shadow: ShadowTheme {
                elevation_1: hsla(0.0, 0.0, 0.0, 0.18),
                elevation_2: hsla(0.0, 0.0, 0.0, 0.3),
            },
            text_direction: TextDirection::Ltr,
        }
    }

    /// Check if RTL mode is enabled.
    pub fn is_rtl(&self) -> bool {
        self.text_direction.is_rtl()
    }

    /// Get the default text direction.
    pub fn default_text_direction() -> TextDirection {
        TextDirection::Ltr
    }

    pub fn action_variant(&self, variant: ActionVariantKind) -> &ActionVariant {
        match variant {
            ActionVariantKind::Neutral => &self.action.neutral,
            ActionVariantKind::Primary => &self.action.primary,
            ActionVariantKind::Danger => &self.action.danger,
        }
    }
}

pub struct ThemeSet {
    pub light: Theme,
    pub dark: Option<Theme>,
}
impl ThemeSet {
    pub fn new(light: Theme) -> Self {
        Self { light, dark: None }
    }
    pub fn dark(mut self, dark: Theme) -> Self {
        self.dark = Some(dark);
        self
    }
}
pub trait ActiveTheme {
    fn theme(&self) -> Theme;
}
impl ActiveTheme for App {
    fn theme(&self) -> Theme {
        let ely = ely_gpui_component::theme::ActiveTheme::theme(self);
        let mut t = if ely.mode() == ely_gpui_component::theme::Mode::Dark {
            crate::design_system::dark_theme()
        } else {
            crate::design_system::light_theme()
        };
        let c = &ely.colors;
        t.surface.canvas = c.bg;
        t.surface.base = c.surface;
        t.surface.raised = c.overlay;
        t.surface.sunken = c.sunken;
        t.surface.hover = c.hover;
        t.content.primary = c.fg;
        t.content.secondary = c.fg_muted;
        t.content.tertiary = c.fg_subtle;
        t.border.default = c.border;
        t.border.focus = c.focus;
        t
    }
}
pub fn install(appearance: WindowAppearance, themes: ThemeSet, cx: &mut App) {
    use ely_gpui_component::theme::{Mode, Palette, Theme as ElyTheme};
    fn palette(t: &Theme, mut p: Palette) -> Palette {
        p.bg = t.surface.canvas;
        p.surface = t.surface.base;
        p.overlay = t.surface.raised;
        p.sunken = t.surface.sunken;
        p.hover = t.surface.hover;
        p.fg = t.content.primary;
        p.fg_muted = t.content.secondary;
        p.fg_subtle = t.content.tertiary;
        p.border = t.border.default;
        p.focus = t.border.focus;
        p.accent = t.action.primary.bg;
        p.accent_hover = t.action.primary.hover_bg;
        p.on_accent = t.action.primary.fg;
        p.active = t.action.neutral.active_bg;
        p.success = t.status.success.bg;
        p.warning = t.status.warning.bg;
        p.danger = t.status.error.bg;
        p.info = t.status.info.bg;
        p
    }
    ElyTheme::update(cx, |theme| {
        theme.font_family = crate::design_system::product_ui_font().into();
        theme.mono_family = crate::design_system::product_mono_font().into();
    });
    ElyTheme::set_palette(
        Mode::Light,
        Some(palette(&themes.light, Palette::light(false))),
        cx,
    );
    if let Some(dark) = themes.dark {
        ElyTheme::set_palette(Mode::Dark, Some(palette(&dark, Palette::dark(false))), cx);
    }
    ElyTheme::set_mode_now(Mode::from(appearance), cx);
}
