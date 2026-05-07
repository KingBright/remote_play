use core_graphics::event::{CGEvent, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use protocol::InputEvent;
use std::error::Error;

pub struct MacInputInjector {
    source: CGEventSource,
}

unsafe impl Send for MacInputInjector {}
unsafe impl Sync for MacInputInjector {}

impl MacInputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "Failed to create CGEventSource")?;
        Ok(Self { source })
    }

    pub fn inject(&self, event: InputEvent) {
        match event {
            InputEvent::KeyDown(scancode) => {
                if let Ok(cg_event) =
                    CGEvent::new_keyboard_event(self.source.clone(), scancode as u16, true)
                {
                    cg_event.post(core_graphics::event::CGEventTapLocation::HID);
                }
            }
            InputEvent::KeyUp(scancode) => {
                if let Ok(cg_event) =
                    CGEvent::new_keyboard_event(self.source.clone(), scancode as u16, false)
                {
                    cg_event.post(core_graphics::event::CGEventTapLocation::HID);
                }
            }
            InputEvent::MouseMove { dx, dy } => {
                // To do relative movement we fetch current cursor position and add dx, dy
                // For simplicity assuming we just emit mouse move.
                let current_pos = CGPoint::new(0.0, 0.0); // Would fetch real pos
                let new_pos = CGPoint::new(current_pos.x + dx as f64, current_pos.y + dy as f64);
                if let Ok(cg_event) = CGEvent::new_mouse_event(
                    self.source.clone(),
                    CGEventType::MouseMoved,
                    new_pos,
                    CGMouseButton::Left,
                ) {
                    cg_event.post(core_graphics::event::CGEventTapLocation::HID);
                }
            }
            InputEvent::MouseDown(_btn) => {
                println!("Mouse down not implemented");
            }
            InputEvent::MouseUp(_btn) => {
                println!("Mouse up not implemented");
            }
        }
    }
}
