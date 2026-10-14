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
use std::time::Instant;

pub struct MacInputInjector {
    source: CGEventSource,
    #[cfg(test)]
    post_observer: Option<std::sync::Arc<dyn Fn(&CGEvent) + Send + Sync>>,
    display_id: Option<u32>,
    window: Option<crate::window_input::WindowTarget>,
    target_pointer: Mutex<Option<(u16, u16)>>,
    pressed_buttons: AtomicU8,
    active_modifiers: AtomicU8,
    pressed_keys: Mutex<BTreeSet<u16>>,
}

unsafe impl Send for MacInputInjector {}
unsafe impl Sync for MacInputInjector {}

impl MacInputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::for_display(CGDisplay::main().id)
    }

    /// Bind normalized input to the actual selected display, including its
    /// global logical origin. The shared host must pass the captured display ID.
    pub fn for_display(id: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        require_post_event_access(unsafe { CGPreflightPostEventAccess() })?;
        display_bounds(id)?;
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Failed to create CGEventSource")?;
        Ok(Self {
            source,
            #[cfg(test)]
            post_observer: None,
            display_id: Some(id),
            window: None,
            target_pointer: Mutex::new(None),
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn for_window(id: u32, pid: i32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        require_post_event_access(unsafe { CGPreflightPostEventAccess() })?;
        let window = crate::window_input::WindowTarget::new(id, pid)?;
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Cannot create window input source")?;
        Ok(Self {
            source,
            #[cfg(test)]
            post_observer: None,
            display_id: None,
            window: Some(window),
            target_pointer: Mutex::new(None),
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }
    fn post(&self, event: &CGEvent, deadline: Option<Instant>) -> bool {
        if let Some(window) = &self.window {
            if !window.alive() {
                // A destroyed target has no safe global release destination.
                // Consume ownership, drop the post, never redirect to a new app.
                return false;
            }
            event.set_integer_value_field(
                EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
                i64::from(window.id),
            );
            event.set_integer_value_field(
                EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT,
                i64::from(window.id),
            );
            // This is explicitly foreground-window control, not background posting.
        }
        // Last application check, after target validation and event construction.
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return false;
        }
        #[cfg(test)]
        if let Some(record) = &self.post_observer {
            record(event);
            return true;
        }
        event.post(CGEventTapLocation::HID);
        true
    }
    fn pointer_position(&self) -> CGPoint {
        if let Some(window) = &self.window {
            if let (Ok(bounds), Some((x, y))) = (
                window.bounds(),
                *self.target_pointer.lock().expect("source pointer lock"),
            ) {
                return normalized_pointer_location(x, y, bounds);
            }
            return CGPoint::new(0., 0.);
        }
        if let Some(id) = self.display_id {
            if let Ok(bounds) = display_bounds(id) {
                if let Some((x, y)) = *self.target_pointer.lock().expect("source pointer lock") {
                    return normalized_pointer_location(x, y, bounds);
                }
                return clamp_display_pointer(current_pointer_location(&self.source), bounds);
            }
        }
        current_pointer_location(&self.source)
    }
    fn prepare_window_event(&self, event: &InputEvent, deadline: Option<Instant>) -> Result<(), Box<dyn Error + Send + Sync>> {
        let Some(window) = &self.window else {
            display_bounds(self.display_id.ok_or("Input display is not bound")?)?;
            if matches!(
                event,
                InputEvent::MouseDown(_) | InputEvent::MouseScroll { .. }
            ) && self
                .target_pointer
                .lock()
                .map_err(|_| "Source pointer lock poisoned")?
                .is_none()
            {
                return Err("Pointer location for the selected display is unconfirmed. Click inside its remote picture before control".into());
            }
            if let InputEvent::Touch {
                normalized_x,
                normalized_y,
                ..
            } = event
                && (!normalized_x.is_finite() || !normalized_y.is_finite())
            {
                return Err("Invalid touch coordinates".into());
            }
            return Ok(());
        };
        window.bounds()?;
        if matches!(event, InputEvent::MouseMove { .. }) {
            return Err("Window control requires normalized absolute coordinates".into());
        }
        if matches!(event, InputEvent::MouseMoveAbsolute { .. }) {
            return Ok(());
        }
        if matches!(
            event,
            InputEvent::MouseDown(_) | InputEvent::MouseScroll { .. }
        ) && self
            .target_pointer
            .lock()
            .expect("source pointer lock")
            .is_none()
        {
            return Err("Click inside the selected remote window before using its pointer".into());
        }
        if matches!(
            event,
            InputEvent::MouseDown(_)
                | InputEvent::Touch {
                    action: protocol::TouchAction::Down,
                    ..
                }
        ) {
            window.focus_for_click(deadline)?;
            let point = match event {
                InputEvent::Touch {
                    normalized_x,
                    normalized_y,
                    ..
                } => {
                    if !normalized_x.is_finite() || !normalized_y.is_finite() {
                        return Err("Invalid touch coordinates".into());
                    }
                    (
                        (normalized_x.clamp(0., 1.) * 65535.) as u16,
                        (normalized_y.clamp(0., 1.) * 65535.) as u16,
                    )
                }
                _ => self
                    .target_pointer
                    .lock()
                    .expect("source pointer lock")
                    .ok_or("Pointer position missing")?,
            };
            window.check_pointer(point.0, point.1)?;
            return Ok(());
        }
        // Release only previously tracked presses. Source changes also release held state.
        if matches!(
            event,
            InputEvent::MouseUp(_)
                | InputEvent::KeyUp(_)
                | InputEvent::Key { pressed: false, .. }
                | InputEvent::Touch {
                    action: protocol::TouchAction::Up | protocol::TouchAction::Cancel,
                    ..
                }
        ) {
            return Ok(());
        }
        if matches!(event, InputEvent::MouseScroll { .. }) {
            let (x, y) = self
                .target_pointer
                .lock()
                .expect("source pointer lock")
                .ok_or("Pointer position missing")?;
            window.check_pointer(x, y)?;
        }
        if !window.focused() {
            return Err("The selected application window does not have keyboard focus. Click that window's remote picture before typing".into());
        }
        Ok(())
    }

    fn inject(&self, event: InputEvent, deadline: Option<Instant>) {
        match event {
            InputEvent::KeyDown(scancode) => {
                self.post_tracked_key(scancode as u16, true, self.current_modifiers(), deadline);
            }
            InputEvent::KeyUp(scancode) => {
                self.post_tracked_key(scancode as u16, false, self.current_modifiers(), deadline);
            }
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                let modifiers = if pressed {
                    modifiers
                } else {
                    modifiers & self.current_modifiers()
                };
                if !self.set_modifiers(modifiers, deadline) {
                    return;
                }
                self.post_tracked_key(key_code, pressed, modifiers, deadline);
            }
            InputEvent::ModifiersChanged(modifiers) => {
                if !self.set_modifiers(modifiers, deadline) {
                    return;
                }
            }
            InputEvent::MouseMove { dx, dy } => {
                let current_pos = self.pointer_position();
                let mut new_pos =
                    CGPoint::new(current_pos.x + dx as f64, current_pos.y + dy as f64);
                if let Some(id) = self.display_id {
                    let Ok(bounds) = display_bounds(id) else {
                        return;
                    };
                    new_pos = clamp_display_pointer(new_pos, bounds);
                    *self.target_pointer.lock().expect("source pointer lock") =
                        Some(pointer_coordinates(new_pos, bounds));
                }
                if let Ok(cg_event) = CGEvent::new_mouse_event(
                    self.source.clone(),
                    pointer_event_type(self.pressed_buttons.load(Ordering::Relaxed)),
                    new_pos,
                    pointer_mouse_button(self.pressed_buttons.load(Ordering::Relaxed)),
                ) {
                    self.post(&cg_event, deadline);
                }
            }
            InputEvent::MouseMoveAbsolute { x, y } => {
                let bounds = if let Some(window) = &self.window {
                    let Ok(bounds) = window.bounds() else {
                        return;
                    };
                    *self.target_pointer.lock().expect("source pointer lock") = Some((x, y));
                    if !window.focused() || window.check_pointer(x, y).is_err() {
                        return;
                    }
                    bounds
                } else {
                    let Some(id) = self.display_id else {
                        return;
                    };
                    let Ok(bounds) = display_bounds(id) else {
                        return;
                    };
                    *self.target_pointer.lock().expect("source pointer lock") = Some((x, y));
                    bounds
                };
                self.post_pointer_move(normalized_pointer_location(x, y, bounds), deadline);
            }
            InputEvent::MouseDown(button) => {
                if self.window.is_some() || self.display_id.is_some() {
                    self.post_pointer_move(self.pointer_position(), deadline);
                }
                self.post_mouse_button(button, true, deadline);
            }
            InputEvent::MouseUp(button) => {
                self.post_mouse_button(button, false, deadline);
            }
            InputEvent::MouseScroll { delta_x, delta_y } => {
                if self.window.is_some() || self.display_id.is_some() {
                    self.post_pointer_move(self.pointer_position(), deadline);
                }
                if let Ok(cg_event) = CGEvent::new_scroll_event(
                    self.source.clone(),
                    ScrollEventUnit::PIXEL,
                    2,
                    delta_y,
                    delta_x,
                    0,
                ) {
                    self.post(&cg_event, deadline);
                }
            }
            InputEvent::Touch {
                action,
                pointer_id: _,
                normalized_x,
                normalized_y,
                pressure: _,
            } => {
                let bounds = if let Some(window) = &self.window {
                    let Ok(bounds) = window.bounds() else {
                        return;
                    };
                    bounds
                } else {
                    let Some(id) = self.display_id else {
                        return;
                    };
                    let Ok(bounds) = display_bounds(id) else {
                        return;
                    };
                    bounds
                };
                let x = (normalized_x.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
                let y = (normalized_y.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
                if self.window.is_some() || self.display_id.is_some() {
                    *self.target_pointer.lock().expect("source pointer lock") = Some((x, y));
                }
                let pos = normalized_pointer_location(x, y, bounds);
                self.post_pointer_move(pos, deadline);

                match action {
                    protocol::TouchAction::Down => {
                        self.post_mouse_button(0, true, deadline);
                    }
                    protocol::TouchAction::Move => {}
                    protocol::TouchAction::Up | protocol::TouchAction::Cancel => {
                        self.post_mouse_button(0, false, deadline);
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
            self.post_key(key_code, false, 0, None);
        }
        let pressed_buttons = self.pressed_buttons.load(Ordering::Relaxed);
        for button in 0..=2 {
            if pressed_buttons & (1 << button) != 0 {
                self.post_mouse_button(button, false, None);
            }
        }
        self.pressed_buttons.store(0, Ordering::Relaxed);
        self.set_modifiers(0, None);
        *self.target_pointer.lock().expect("source pointer lock") = None;
    }

    fn current_modifiers(&self) -> u8 {
        self.active_modifiers.load(Ordering::Relaxed)
    }

    fn post_key(
        &self,
        key_code: u16,
        pressed: bool,
        modifiers: u8,
        deadline: Option<Instant>,
    ) -> bool {
        if let Ok(cg_event) = CGEvent::new_keyboard_event(self.source.clone(), key_code, pressed) {
            cg_event.set_flags(modifier_event_flags(modifiers));
            return self.post(&cg_event, deadline);
        }
        false
    }

    fn post_tracked_key(
        &self,
        key_code: u16,
        pressed: bool,
        modifiers: u8,
        deadline: Option<Instant>,
    ) {
        let mut keys = self.pressed_keys.lock().expect("pressed keys lock");
        if keys.contains(&key_code) == pressed {
            return;
        }
        if self.post_key(key_code, pressed, modifiers, deadline) {
            if pressed {
                keys.insert(key_code);
            } else {
                keys.remove(&key_code);
            }
        }
    }

    fn set_modifiers(&self, modifiers: u8, deadline: Option<Instant>) -> bool {
        for (flag, key_code) in modifier_key_codes() {
            if (self.current_modifiers() ^ modifiers) & flag != 0 {
                let down = modifiers & flag != 0;
                let accepted_flags = if down {
                    self.current_modifiers() | flag
                } else {
                    self.current_modifiers() & !flag
                };
                if self.post_key(key_code, down, accepted_flags, deadline) {
                    if down {
                        self.active_modifiers.fetch_or(flag, Ordering::Relaxed);
                    } else {
                        self.active_modifiers.fetch_and(!flag, Ordering::Relaxed);
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn post_pointer_move(&self, position: CGPoint, deadline: Option<Instant>) {
        let pressed_buttons = self.pressed_buttons.load(Ordering::Relaxed);
        if let Ok(cg_event) = CGEvent::new_mouse_event(
            self.source.clone(),
            pointer_event_type(pressed_buttons),
            position,
            pointer_mouse_button(pressed_buttons),
        ) {
            self.post(&cg_event, deadline);
        }
    }

    fn post_mouse_button(&self, button: u8, pressed: bool, deadline: Option<Instant>) {
        if !pressed
            && (button > 2 || self.pressed_buttons.load(Ordering::Relaxed) & (1 << button) == 0)
        {
            return;
        }
        let (Some(cg_button), Some(event_type)) =
            (mouse_button(button), mouse_event_type(button, pressed))
        else {
            return;
        };
        let Ok(cg_event) = CGEvent::new_mouse_event(
            self.source.clone(),
            event_type,
            self.pointer_position(),
            cg_button,
        ) else {
            return;
        };
        cg_event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, button as i64);
        cg_event.set_flags(modifier_event_flags(self.current_modifiers()));
        let mask = 1_u8 << button;
        if (self.pressed_buttons.load(Ordering::Relaxed) & mask != 0) == pressed {
            return;
        }
        if self.post(&cg_event, deadline) {
            if pressed {
                self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
            } else {
                self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
            }
        }
    }
}

fn require_post_event_access(allowed: bool) -> Result<(), Box<dyn Error + Send + Sync>> {
    if allowed {
        Ok(())
    } else {
        Err("The running signed RemotePlay host lacks input-posting permission. Verify its App identity and enable RemotePlay in System Settings > Privacy & Security > Accessibility".into())
    }
}

fn display_bounds(
    id: u32,
) -> Result<core_graphics::geometry::CGRect, Box<dyn Error + Send + Sync>> {
    let display = CGDisplay::new(id);
    validate_display_bounds(display.bounds(), display.is_online(), display.is_active())
}

pub(crate) fn display_input_status(id: u32) -> Result<(), Box<dyn Error + Send + Sync>> {
    display_bounds(id).map(|_| ())
}

pub(crate) fn post_event_access_status() -> Result<(), Box<dyn Error + Send + Sync>> {
    require_post_event_access(unsafe { CGPreflightPostEventAccess() })
}

/// Geometry capability only. Permission is checked by the constructor and
/// every injected event so an OS denial uses the existing INPUT_UNAVAILABLE
/// error path instead of being mistaken for an unimplemented display target.
pub(crate) fn display_input_capable(id: u32) -> bool {
    display_bounds(id).is_ok()
}

fn validate_display_bounds(
    bounds: core_graphics::geometry::CGRect,
    online: bool,
    active: bool,
) -> Result<core_graphics::geometry::CGRect, Box<dyn Error + Send + Sync>> {
    if !online || !active {
        return Err(
            "Selected display is no longer active. Reselect an available source on the host".into(),
        );
    }
    if ![
        bounds.origin.x,
        bounds.origin.y,
        bounds.size.width,
        bounds.size.height,
    ]
    .iter()
    .all(|n| n.is_finite())
        || bounds.size.width < 1.
        || bounds.size.height < 1.
    {
        return Err(
            "Selected display geometry is unavailable. Reselect its source before enabling control"
                .into(),
        );
    }
    Ok(bounds)
}

fn clamp_display_pointer(point: CGPoint, bounds: core_graphics::geometry::CGRect) -> CGPoint {
    CGPoint::new(
        point
            .x
            .clamp(bounds.origin.x, bounds.origin.x + bounds.size.width - 0.5),
        point
            .y
            .clamp(bounds.origin.y, bounds.origin.y + bounds.size.height - 0.5),
    )
}

fn pointer_coordinates(point: CGPoint, bounds: core_graphics::geometry::CGRect) -> (u16, u16) {
    let point = clamp_display_pointer(point, bounds);
    (
        (((point.x - bounds.origin.x) / bounds.size.width) * f64::from(u16::MAX)).round() as u16,
        (((point.y - bounds.origin.y) / bounds.size.height) * f64::from(u16::MAX)).round() as u16,
    )
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
}

fn current_pointer_location(source: &CGEventSource) -> CGPoint {
    CGEvent::new(source.clone())
        .map(|event| event.location())
        .unwrap_or_else(|_| CGPoint::new(0.0, 0.0))
}

pub(crate) fn normalized_pointer_location(
    x: u16,
    y: u16,
    display_bounds: core_graphics::geometry::CGRect,
) -> CGPoint {
    let normalized_x = f64::from(x) / f64::from(u16::MAX);
    let normalized_y = f64::from(y) / f64::from(u16::MAX);
    CGPoint::new(
        display_bounds.origin.x
            + (display_bounds.size.width * normalized_x)
                .min((display_bounds.size.width - 0.5).max(0.)),
        display_bounds.origin.y
            + (display_bounds.size.height * normalized_y)
                .min((display_bounds.size.height - 0.5).max(0.)),
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
        // Consent can be revoked after the injector was created. Propagate a
        // real error through INPUT_UNAVAILABLE before changing held state or
        // posting events; CGEvent::post itself has no success return value.
        require_post_event_access(unsafe { CGPreflightPostEventAccess() })?;
        self.prepare_window_event(&event, None)?;
        self.inject(event, None);
        Ok(())
    }

    fn inject_input_before(
        &self,
        event: InputEvent,
        deadline: std::time::Instant,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if Instant::now() >= deadline {
            return Err("input expired before target validation".into());
        }
        require_post_event_access(unsafe { CGPreflightPostEventAccess() })?;
        self.prepare_window_event(&event, Some(deadline))?;
        // AX focus/identity validation may have waited; check after it as well.
        if std::time::Instant::now() >= deadline {
            return Err("input expired after target validation".into());
        }
        self.inject(event, Some(deadline));
        if Instant::now() >= deadline {
            return Err("input expired during native dispatch".into());
        }
        Ok(())
    }
    fn release_all_input(&self) {
        self.release_all_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_revoked_post_access_is_an_error_not_injection_success() {
        assert!(require_post_event_access(true).is_ok());
        let error = require_post_event_access(false).unwrap_err().to_string();
        assert!(error.contains("running signed RemotePlay host"));
        assert!(error.contains("Accessibility"));
    }

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
        assert_eq!((bottom_right.x, bottom_right.y), (1699.5, 1099.5));
    }

    #[test]
    fn selected_display_mapping_retains_negative_origins_and_logical_dimensions() {
        use core_graphics::geometry::{CGRect, CGSize};
        // A Retina source is normalized before transmission; injection uses
        // Quartz logical coordinates, not the encoded pixel dimensions.
        for bounds in [
            CGRect::new(&CGPoint::new(-1728., 0.), &CGSize::new(1728., 1117.)),
            CGRect::new(&CGPoint::new(0., -1080.), &CGSize::new(1920., 1080.)),
            CGRect::new(&CGPoint::new(1920., 120.), &CGSize::new(1080., 1920.)),
        ] {
            validate_display_bounds(bounds, true, true).unwrap();
            let first = normalized_pointer_location(0, 0, bounds);
            let last = normalized_pointer_location(u16::MAX, u16::MAX, bounds);
            assert_eq!((first.x, first.y), (bounds.origin.x, bounds.origin.y));
            assert_eq!(
                (last.x, last.y),
                (
                    bounds.origin.x + bounds.size.width - 0.5,
                    bounds.origin.y + bounds.size.height - 0.5
                )
            );
            let clamped = clamp_display_pointer(CGPoint::new(-10000., 10000.), bounds);
            assert_eq!(
                (clamped.x, clamped.y),
                (bounds.origin.x, bounds.origin.y + bounds.size.height - 0.5)
            );
        }
    }

    #[test]
    fn inactive_or_invalid_display_never_falls_back_to_main_display() {
        use core_graphics::geometry::{CGRect, CGSize};
        let valid = CGRect::new(&CGPoint::new(-1920., 0.), &CGSize::new(1920., 1080.));
        assert!(validate_display_bounds(valid, false, true).is_err());
        assert!(validate_display_bounds(valid, true, false).is_err());
        for size in [
            CGSize::new(0., 1080.),
            CGSize::new(f64::NAN, 900.),
            CGSize::new(900., f64::INFINITY),
        ] {
            assert!(
                validate_display_bounds(CGRect::new(&CGPoint::new(0., 0.), &size), true, true)
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    fn fixture(until: Option<Instant>) -> (Arc<MacInputInjector>, Arc<Mutex<Vec<(u32, i64)>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        let input = MacInputInjector {
            source: CGEventSource::new(CGEventSourceStateID::Private).unwrap(),
            display_id: None,
            window: None,
            target_pointer: Mutex::new(None),
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
            post_observer: Some(Arc::new(move |event| {
                let mut calls = sink.lock().unwrap();
                calls.push((
                    event.get_type() as u32,
                    event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
                ));
                if calls.len() == 1 {
                    if let Some(until) = until {
                        std::thread::sleep(until.saturating_duration_since(Instant::now()));
                    }
                }
            })),
        };
        (Arc::new(input), calls)
    }
    #[test]
    fn fresh_deadline_posts_and_cleans_only_the_accepted_key() {
        let (input, calls) = fixture(None);
        input.inject(
            InputEvent::KeyDown(0),
            Some(Instant::now() + std::time::Duration::from_secs(1)),
        );
        input.release_all_input();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (CGEventType::KeyDown as u32, 0),
                (CGEventType::KeyUp as u32, 0)
            ]
        );
    }
    #[test]
    fn key_lock_wait_expiry_does_not_post_or_create_cleanup_up() {
        let (input, calls) = fixture(None);
        let held = input.pressed_keys.lock().unwrap();
        let until = Instant::now() + std::time::Duration::from_millis(40);
        let (tx, rx) = std::sync::mpsc::channel();
        let other = input.clone();
        let worker = std::thread::spawn(move || {
            tx.send(()).unwrap();
            other.inject(InputEvent::KeyDown(0), Some(until));
        });
        rx.recv().unwrap();
        std::thread::sleep(
            until.saturating_duration_since(Instant::now()) + std::time::Duration::from_millis(2),
        );
        drop(held);
        worker.join().unwrap();
        input.release_all_input();
        assert!(calls.lock().unwrap().is_empty());
    }
    #[test]
    fn batch_expiry_does_not_post_unaccepted_modifiers_or_key_cleanup() {
        let (mut input, calls) = fixture(None);
        // Finish the native fixture setup before starting the event budget.
        let _prepared = CGEvent::new_keyboard_event(input.source.clone(), 56, true).unwrap();
        let until = Instant::now() + std::time::Duration::from_millis(40);
        let sink = calls.clone();
        Arc::get_mut(&mut input).unwrap().post_observer = Some(Arc::new(move |event| {
            let mut calls = sink.lock().unwrap();
            calls.push((
                event.get_type() as u32,
                event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE),
            ));
            if calls.len() == 1 {
                std::thread::sleep(
                    until.saturating_duration_since(Instant::now())
                        + std::time::Duration::from_millis(2),
                );
            }
        }));
        input.inject(
            InputEvent::Key {
                key_code: 0,
                pressed: true,
                modifiers: protocol::input_modifiers::SHIFT | protocol::input_modifiers::CONTROL,
            },
            Some(until),
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(CGEventType::FlagsChanged as u32, 56)]
        );
        input.release_all_input();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (CGEventType::FlagsChanged as u32, 56),
                (CGEventType::FlagsChanged as u32, 56)
            ]
        );
    }
}
