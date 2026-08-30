use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use protocol::InputEvent;
use remote_core::InputInjector;
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

pub struct MacInputInjector {
    source: CGEventSource,
    pressed_buttons: AtomicU8,
    active_modifiers: AtomicU8,
    pressed_keys: Mutex<BTreeSet<u16>>,
}

unsafe impl Send for MacInputInjector {}
unsafe impl Sync for MacInputInjector {}

impl MacInputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let has_post_event_access = request_post_event_access_if_needed();
        if !has_post_event_access {
            eprintln!(
                "[Input] macOS Accessibility permission is required for remote keyboard and pointer control"
            );
        }
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Failed to create CGEventSource")?;
        Ok(Self {
            source,
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn inject(&self, event: InputEvent) {
        match event {
            InputEvent::KeyDown(scancode) => {
                self.post_tracked_key(scancode as u16, true, self.current_modifiers());
            }
            InputEvent::KeyUp(scancode) => {
                self.post_tracked_key(scancode as u16, false, self.current_modifiers());
            }
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                self.set_modifiers(modifiers);
                self.post_tracked_key(key_code, pressed, modifiers);
            }
            InputEvent::ModifiersChanged(modifiers) => {
                self.set_modifiers(modifiers);
            }
            InputEvent::MouseMove { dx, dy } => {
                let current_pos = current_pointer_location(&self.source);
                let new_pos = CGPoint::new(current_pos.x + dx as f64, current_pos.y + dy as f64);
                if let Ok(cg_event) = CGEvent::new_mouse_event(
                    self.source.clone(),
                    pointer_event_type(self.pressed_buttons.load(Ordering::Relaxed)),
                    new_pos,
                    pointer_mouse_button(self.pressed_buttons.load(Ordering::Relaxed)),
                ) {
                    cg_event.post(CGEventTapLocation::HID);
                }
            }
            InputEvent::MouseMoveAbsolute { x, y } => {
                self.post_pointer_move(normalized_pointer_location(
                    x,
                    y,
                    CGDisplay::main().bounds(),
                ));
            }
            InputEvent::MouseDown(button) => {
                self.post_mouse_button(button, true);
            }
            InputEvent::MouseUp(button) => {
                self.post_mouse_button(button, false);
            }
            InputEvent::MouseScroll { delta_x, delta_y } => {
                if let Ok(cg_event) = CGEvent::new_scroll_event(
                    self.source.clone(),
                    ScrollEventUnit::PIXEL,
                    2,
                    delta_y,
                    delta_x,
                    0,
                ) {
                    cg_event.post(CGEventTapLocation::HID);
                }
            }
            InputEvent::Touch {
                action,
                pointer_id: _,
                normalized_x,
                normalized_y,
                pressure: _,
            } => {
                let bounds = CGDisplay::main().bounds();
                let x = (normalized_x.clamp(0.0, 1.0) * bounds.size.width as f32) as u16;
                let y = (normalized_y.clamp(0.0, 1.0) * bounds.size.height as f32) as u16;
                let pos = normalized_pointer_location(x, y, bounds);
                self.post_pointer_move(pos);

                match action {
                    protocol::TouchAction::Down => {
                        self.post_mouse_button(1, true);
                    }
                    protocol::TouchAction::Move => {}
                    protocol::TouchAction::Up | protocol::TouchAction::Cancel => {
                        self.post_mouse_button(1, false);
                    }
                }
            }
        }
    }

    pub fn release_all_input(&self) {
        let pressed_keys = {
            let mut keys = self.pressed_keys.lock().expect("pressed keys lock");
            std::mem::take(&mut *keys)
        };
        for key_code in pressed_keys {
            self.post_key(key_code, false, 0);
        }
        let pressed_buttons = self.pressed_buttons.swap(0, Ordering::Relaxed);
        for button in 0..=2 {
            if pressed_buttons & (1 << button) != 0 {
                self.post_mouse_button(button, false);
            }
        }
        self.set_modifiers(0);
    }

    fn current_modifiers(&self) -> u8 {
        self.active_modifiers.load(Ordering::Relaxed)
    }

    fn post_key(&self, key_code: u16, pressed: bool, modifiers: u8) {
        if let Ok(cg_event) = CGEvent::new_keyboard_event(self.source.clone(), key_code, pressed) {
            cg_event.set_flags(modifier_event_flags(modifiers));
            cg_event.post(CGEventTapLocation::HID);
        }
    }

    fn post_tracked_key(&self, key_code: u16, pressed: bool, modifiers: u8) {
        self.post_key(key_code, pressed, modifiers);
        let mut keys = self.pressed_keys.lock().expect("pressed keys lock");
        if pressed {
            keys.insert(key_code);
        } else {
            keys.remove(&key_code);
        }
    }

    fn set_modifiers(&self, modifiers: u8) {
        let previous = self.active_modifiers.swap(modifiers, Ordering::Relaxed);
        for (flag, key_code) in modifier_key_codes() {
            if (previous & flag != 0) != (modifiers & flag != 0) {
                self.post_key(key_code, modifiers & flag != 0, modifiers);
            }
        }
    }

    fn post_pointer_move(&self, position: CGPoint) {
        let pressed_buttons = self.pressed_buttons.load(Ordering::Relaxed);
        if let Ok(cg_event) = CGEvent::new_mouse_event(
            self.source.clone(),
            pointer_event_type(pressed_buttons),
            position,
            pointer_mouse_button(pressed_buttons),
        ) {
            cg_event.post(CGEventTapLocation::HID);
        }
    }

    fn post_mouse_button(&self, button: u8, pressed: bool) {
        let (Some(cg_button), Some(event_type)) =
            (mouse_button(button), mouse_event_type(button, pressed))
        else {
            return;
        };
        let Ok(cg_event) = CGEvent::new_mouse_event(
            self.source.clone(),
            event_type,
            current_pointer_location(&self.source),
            cg_button,
        ) else {
            return;
        };
        cg_event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, button as i64);
        cg_event.post(CGEventTapLocation::HID);

        let mask = 1_u8 << button;
        if pressed {
            self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
        } else {
            self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
        }
    }
}

fn request_post_event_access_if_needed() -> bool {
    unsafe {
        if CGPreflightPostEventAccess() {
            true
        } else {
            CGRequestPostEventAccess() || CGPreflightPostEventAccess()
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

fn current_pointer_location(source: &CGEventSource) -> CGPoint {
    CGEvent::new(source.clone())
        .map(|event| event.location())
        .unwrap_or_else(|_| CGPoint::new(0.0, 0.0))
}

fn normalized_pointer_location(
    x: u16,
    y: u16,
    display_bounds: core_graphics::geometry::CGRect,
) -> CGPoint {
    let normalized_x = f64::from(x) / f64::from(u16::MAX);
    let normalized_y = f64::from(y) / f64::from(u16::MAX);
    CGPoint::new(
        display_bounds.origin.x + display_bounds.size.width * normalized_x,
        display_bounds.origin.y + display_bounds.size.height * normalized_y,
    )
}

fn modifier_key_codes() -> [(u8, u16); 6] {
    use protocol::input_modifiers;
    [
        (input_modifiers::SHIFT, 0x38),
        (input_modifiers::CONTROL, 0x3b),
        (input_modifiers::ALT, 0x3a),
        (input_modifiers::META, 0x37),
        (input_modifiers::FUNCTION, 0x3f),
        (input_modifiers::CAPS_LOCK, 0x39),
    ]
}

fn modifier_event_flags(modifiers: u8) -> CGEventFlags {
    use protocol::input_modifiers;
    let mut flags = CGEventFlags::empty();
    if modifiers & input_modifiers::SHIFT != 0 {
        flags.insert(CGEventFlags::CGEventFlagShift);
    }
    if modifiers & input_modifiers::CONTROL != 0 {
        flags.insert(CGEventFlags::CGEventFlagControl);
    }
    if modifiers & input_modifiers::ALT != 0 {
        flags.insert(CGEventFlags::CGEventFlagAlternate);
    }
    if modifiers & input_modifiers::META != 0 {
        flags.insert(CGEventFlags::CGEventFlagCommand);
    }
    if modifiers & input_modifiers::FUNCTION != 0 {
        flags.insert(CGEventFlags::CGEventFlagSecondaryFn);
    }
    if modifiers & input_modifiers::CAPS_LOCK != 0 {
        flags.insert(CGEventFlags::CGEventFlagAlphaShift);
    }
    flags
}

fn mouse_button(button: u8) -> Option<CGMouseButton> {
    match button {
        0 => Some(CGMouseButton::Left),
        1 => Some(CGMouseButton::Right),
        2 => Some(CGMouseButton::Center),
        _ => None,
    }
}

fn mouse_event_type(button: u8, pressed: bool) -> Option<CGEventType> {
    match (button, pressed) {
        (0, true) => Some(CGEventType::LeftMouseDown),
        (0, false) => Some(CGEventType::LeftMouseUp),
        (1, true) => Some(CGEventType::RightMouseDown),
        (1, false) => Some(CGEventType::RightMouseUp),
        (2, true) => Some(CGEventType::OtherMouseDown),
        (2, false) => Some(CGEventType::OtherMouseUp),
        _ => None,
    }
}

fn pointer_event_type(pressed_buttons: u8) -> CGEventType {
    if pressed_buttons & (1 << 0) != 0 {
        CGEventType::LeftMouseDragged
    } else if pressed_buttons & (1 << 1) != 0 {
        CGEventType::RightMouseDragged
    } else if pressed_buttons & (1 << 2) != 0 {
        CGEventType::OtherMouseDragged
    } else {
        CGEventType::MouseMoved
    }
}

fn pointer_mouse_button(pressed_buttons: u8) -> CGMouseButton {
    if pressed_buttons & (1 << 1) != 0 {
        CGMouseButton::Right
    } else if pressed_buttons & (1 << 2) != 0 {
        CGMouseButton::Center
    } else {
        CGMouseButton::Left
    }
}

impl InputInjector for MacInputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.inject(event);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_buttons_map_to_core_graphics_buttons_and_event_types() {
        assert_eq!(mouse_button(0).map(|button| button as u32), Some(0));
        assert_eq!(mouse_button(1).map(|button| button as u32), Some(1));
        assert_eq!(mouse_button(2).map(|button| button as u32), Some(2));
        assert!(mouse_button(3).is_none());

        assert_eq!(mouse_event_type(0, true).map(|kind| kind as u32), Some(1));
        assert_eq!(mouse_event_type(0, false).map(|kind| kind as u32), Some(2));
        assert_eq!(mouse_event_type(1, true).map(|kind| kind as u32), Some(3));
        assert_eq!(mouse_event_type(1, false).map(|kind| kind as u32), Some(4));
        assert_eq!(mouse_event_type(2, true).map(|kind| kind as u32), Some(25));
        assert_eq!(mouse_event_type(2, false).map(|kind| kind as u32), Some(26));
    }

    #[test]
    fn drag_event_type_follows_the_pressed_button() {
        assert_eq!(pointer_event_type(0) as u32, 5);
        assert_eq!(pointer_event_type(1 << 0) as u32, 6);
        assert_eq!(pointer_event_type(1 << 1) as u32, 7);
        assert_eq!(pointer_event_type(1 << 2) as u32, 27);
        assert_eq!(pointer_mouse_button(0) as u32, 0);
        assert_eq!(pointer_mouse_button(1 << 0) as u32, 0);
        assert_eq!(pointer_mouse_button(1 << 1) as u32, 1);
        assert_eq!(pointer_mouse_button(1 << 2) as u32, 2);
    }

    #[test]
    fn normalized_pointer_coordinates_cover_the_display_bounds() {
        let bounds = core_graphics::geometry::CGRect::new(
            &CGPoint::new(100.0, 200.0),
            &core_graphics::geometry::CGSize::new(1600.0, 900.0),
        );

        let top_left = normalized_pointer_location(0, 0, bounds);
        assert_eq!((top_left.x, top_left.y), (100.0, 200.0));

        let bottom_right = normalized_pointer_location(u16::MAX, u16::MAX, bounds);
        assert_eq!((bottom_right.x, bottom_right.y), (1700.0, 1100.0));
    }
}
