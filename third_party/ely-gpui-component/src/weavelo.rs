mod assets;
pub mod theme;
pub use assets::{Assets, Before};

pub mod motion {
    #[path = "curve.rs"] mod curve;
    #[path = "spinner.rs"] mod spinner;
    pub use curve::*;
    pub use spinner::{Spinner, SpinnerStyle};
}
pub mod typography {
    #[path = "keys.rs"] pub mod keys;
}
pub mod primitives {
    #[path = "focus.rs"] mod focus;
    #[path = "icon.rs"] mod icon;
    pub use focus::{FocusNext, FocusPrev, FocusRing, FocusScope};
    pub use icon::{Icon, IconName};
}
pub mod buttons {
    #[path = "button.rs"] mod button;
    pub use button::{Button, ButtonVariant};
}

pub fn init(cx: &mut gpui::App) -> anyhow::Result<()> {
    let missing = assets::missing(cx.asset_source().as_ref());
    if !missing.is_empty() { anyhow::bail!("Ely assets missing: {}", missing.join(", ")); }
    assets::load_fonts(cx)?;
    init_for_tests(cx);
    Ok(())
}
pub fn init_for_tests(cx: &mut gpui::App) {
    theme::Theme::init(cx);
    cx.bind_keys([
        gpui::KeyBinding::new("tab", primitives::FocusNext, Some("ElyFocus")),
        gpui::KeyBinding::new("shift-tab", primitives::FocusPrev, Some("ElyFocus")),
    ]);
}
