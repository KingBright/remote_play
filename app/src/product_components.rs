use gpui::{prelude::*, *};
use std::sync::Arc;

mod input_actions;
mod text_edit_state;
mod text_input;
pub mod theme;
pub use ely_gpui_component::buttons::Button;
use text_edit_state::TextEditState;
pub use text_input::text_input;
const CURSOR_BLINK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
type ChangeCallback<T> = Arc<dyn Fn(T, &mut Window, &mut App)>;

pub mod assets {
    use gpui::{AssetSource, SharedString};
    use std::borrow::Cow;
    pub struct UiAsset;
    impl AssetSource for UiAsset {
        fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
            let bytes: Option<&'static [u8]> = match path {
                "remoteplay-icons/server.svg" => {
                    Some(include_bytes!("product_components/icons/server.svg"))
                }
                "remoteplay-icons/maximize-off.svg" => {
                    Some(include_bytes!("product_components/icons/maximize-off.svg"))
                }
                "remoteplay-icons/maximize-on.svg" => {
                    Some(include_bytes!("product_components/icons/maximize-on.svg"))
                }
                "remoteplay-icons/minimize.svg" => {
                    Some(include_bytes!("product_components/icons/minimize.svg"))
                }
                "remoteplay-icons/info.svg" => {
                    Some(include_bytes!("product_components/icons/info.svg"))
                }
                "remoteplay-icons/user.svg" => {
                    Some(include_bytes!("product_components/icons/user.svg"))
                }
                "remoteplay-icons/ping-indicator-0.svg" => Some(include_bytes!(
                    "product_components/icons/ping-indicator-0.svg"
                )),
                "remoteplay-icons/ping-indicator-1.svg" => Some(include_bytes!(
                    "product_components/icons/ping-indicator-1.svg"
                )),
                "remoteplay-icons/ping-indicator-2.svg" => Some(include_bytes!(
                    "product_components/icons/ping-indicator-2.svg"
                )),
                "remoteplay-icons/ping-indicator-3.svg" => Some(include_bytes!(
                    "product_components/icons/ping-indicator-3.svg"
                )),
                _ => None,
            };
            if let Some(bytes) = bytes {
                return Ok(Some(Cow::Borrowed(bytes)));
            }
            ely_gpui_component::Assets.load(path)
        }
        fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
            let mut names = ely_gpui_component::Assets.list(path)?;
            for icon in [
                "server.svg",
                "maximize-off.svg",
                "maximize-on.svg",
                "minimize.svg",
                "info.svg",
                "user.svg",
                "ping-indicator-0.svg",
                "ping-indicator-1.svg",
                "ping-indicator-2.svg",
                "ping-indicator-3.svg",
            ] {
                let icon = format!("remoteplay-icons/{icon}");
                if icon.starts_with(path) {
                    names.push(icon.into());
                }
            }
            Ok(names)
        }
    }
}
pub mod component {
    pub use super::{Button, IconName, button, icon, text_input, tooltip};
    pub fn init(cx: &mut gpui::App) -> gpui::Result<()> {
        #[cfg(test)]
        ely_gpui_component::init_for_tests(cx);
        #[cfg(not(test))]
        ely_gpui_component::init(cx)?;
        super::text_input::init(cx);
        Ok(())
    }
}

impl From<theme::ActionVariantKind> for ely_gpui_component::buttons::ButtonVariant {
    fn from(value: theme::ActionVariantKind) -> Self {
        match value {
            theme::ActionVariantKind::Neutral => Self::Secondary,
            theme::ActionVariantKind::Primary => Self::Primary,
            theme::ActionVariantKind::Danger => Self::Danger,
        }
    }
}
pub fn button(id: impl Into<ElementId>) -> Button {
    Button::new(id, "")
}

pub enum IconName {
    Server,
    Maximize(bool),
    Minimize,
    PingIndicator(u8),
    User,
    Info,
}
#[derive(IntoElement)]
pub struct ProductIcon {
    base: Svg,
}
pub fn icon(name: IconName) -> ProductIcon {
    let file = match name {
        IconName::Server => "server.svg",
        IconName::Maximize(false) => "maximize-off.svg",
        IconName::Maximize(true) => "maximize-on.svg",
        IconName::Minimize => "minimize.svg",
        IconName::PingIndicator(0) => "ping-indicator-0.svg",
        IconName::PingIndicator(1) => "ping-indicator-1.svg",
        IconName::PingIndicator(2) => "ping-indicator-2.svg",
        IconName::PingIndicator(_) => "ping-indicator-3.svg",
        IconName::User => "user.svg",
        IconName::Info => "info.svg",
    };
    ProductIcon {
        base: svg().path(format!("remoteplay-icons/{file}")),
    }
}
impl ProductIcon {
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.base = self.base.text_color(color);
        self
    }
}
impl Styled for ProductIcon {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}
impl RenderOnce for ProductIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.base
    }
}

pub struct TooltipBuilder(SharedString);
struct ProductTooltip(SharedString);
pub fn tooltip(text: impl Into<SharedString>) -> TooltipBuilder {
    TooltipBuilder(text.into())
}
impl TooltipBuilder {
    pub fn build(self) -> impl Fn(&mut Window, &mut App) -> AnyView {
        move |_, cx| cx.new(|_| ProductTooltip(self.0.clone())).into()
    }
}
impl Render for ProductTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use theme::ActiveTheme;
        let theme = cx.theme();
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(theme.surface.raised)
            .border_1()
            .border_color(theme.border.default)
            .text_color(theme.content.primary)
            .text_size(px(11.0))
            .child(self.0.clone())
    }
}

struct InputStyle {
    bg: Hsla,
    border: Hsla,
    focus_border: Hsla,
    text_color: Hsla,
}
fn compute_input_style(
    theme: &theme::Theme,
    disabled: bool,
    bg: Option<Hsla>,
    border: Option<Hsla>,
    focus: Option<Hsla>,
    text: Option<Hsla>,
) -> InputStyle {
    InputStyle {
        bg: if disabled {
            theme.surface.sunken
        } else {
            bg.unwrap_or(theme.surface.base)
        },
        border: if disabled {
            theme.border.muted
        } else {
            border.unwrap_or(theme.border.default)
        },
        focus_border: focus.unwrap_or(theme.border.focus),
        text_color: if disabled {
            theme.content.disabled
        } else {
            text.unwrap_or(theme.content.primary)
        },
    }
}
