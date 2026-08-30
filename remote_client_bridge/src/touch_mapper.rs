use protocol::{InputEvent, TouchAction, input_modifiers};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchMode {
    DirectTouch,
    VirtualTrackpad,
    GamepadOverlay,
}

#[derive(Debug, Clone)]
pub struct RemoteScreenBounds {
    pub width: u16,
    pub height: u16,
}

impl Default for RemoteScreenBounds {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}

pub struct TouchStateTracker {
    pub mode: TouchMode,
    pub bounds: RemoteScreenBounds,
    last_touch_pos: Option<(f32, f32)>,
    touch_down_time: Option<Instant>,
    active_pointers_count: usize,
    long_press_threshold: Duration,
    drag_threshold_px: f32,
    has_dragged: bool,
}

impl Default for TouchStateTracker {
    fn default() -> Self {
        Self::new(TouchMode::DirectTouch, RemoteScreenBounds::default())
    }
}

impl TouchStateTracker {
    pub fn new(mode: TouchMode, bounds: RemoteScreenBounds) -> Self {
        Self {
            mode,
            bounds,
            last_touch_pos: None,
            touch_down_time: None,
            active_pointers_count: 0,
            long_press_threshold: Duration::from_millis(380),
            drag_threshold_px: 6.0,
            has_dragged: false,
        }
    }

    pub fn set_mode(&mut self, mode: TouchMode) {
        self.mode = mode;
    }

    pub fn set_bounds(&mut self, width: u16, height: u16) {
        self.bounds = RemoteScreenBounds { width, height };
    }

    /// 将移动端/Web端多点触控转换为标准 RemotePlay InputEvents
    pub fn process_touch(
        &mut self,
        action: TouchAction,
        pointer_id: u32,
        norm_x: f32,
        norm_y: f32,
        _pressure: f32,
        now: Instant,
    ) -> Vec<InputEvent> {
        let mut events = Vec::new();
        let clamped_x = norm_x.clamp(0.0, 1.0);
        let clamped_y = norm_y.clamp(0.0, 1.0);

        let abs_x = (clamped_x * (self.bounds.width.saturating_sub(1) as f32)).round() as u16;
        let abs_y = (clamped_y * (self.bounds.height.saturating_sub(1) as f32)).round() as u16;

        match self.mode {
            TouchMode::DirectTouch => {
                match action {
                    TouchAction::Down => {
                        self.active_pointers_count = self.active_pointers_count.saturating_add(1);
                        self.last_touch_pos = Some((clamped_x, clamped_y));
                        self.touch_down_time = Some(now);
                        self.has_dragged = false;

                        if self.active_pointers_count == 1 {
                            // 单指按下：先定位鼠标绝对坐标，再按下鼠标左键
                            events.push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                            events.push(InputEvent::MouseDown(1));
                        }
                    }
                    TouchAction::Move => {
                        if let Some((lx, ly)) = self.last_touch_pos {
                            let dx_px = (clamped_x - lx) * (self.bounds.width as f32);
                            let dy_px = (clamped_y - ly) * (self.bounds.height as f32);

                            if dx_px.abs() > self.drag_threshold_px || dy_px.abs() > self.drag_threshold_px {
                                self.has_dragged = true;
                            }

                            if self.active_pointers_count == 1 {
                                // 单指滑动：拖拽或移动
                                events.push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                            } else if self.active_pointers_count == 2 {
                                // 双指滑动：映射为页面滚动
                                events.push(InputEvent::MouseScroll {
                                    delta_x: -(dx_px as i32),
                                    delta_y: -(dy_px as i32),
                                });
                            }
                        }
                        self.last_touch_pos = Some((clamped_x, clamped_y));
                    }
                    TouchAction::Up | TouchAction::Cancel => {
                        if self.active_pointers_count == 1 {
                            if let Some(down_t) = self.touch_down_time {
                                if !self.has_dragged && now.duration_since(down_t) >= self.long_press_threshold {
                                    // 长按触发鼠标右键模拟
                                    events.push(InputEvent::MouseUp(1));
                                    events.push(InputEvent::MouseDown(2));
                                    events.push(InputEvent::MouseUp(2));
                                } else {
                                    events.push(InputEvent::MouseUp(1));
                                }
                            } else {
                                events.push(InputEvent::MouseUp(1));
                            }
                        } else if self.active_pointers_count == 2 && !self.has_dragged {
                            // 双指轻点：触发鼠标右键
                            events.push(InputEvent::MouseDown(2));
                            events.push(InputEvent::MouseUp(2));
                        }

                        self.active_pointers_count = self.active_pointers_count.saturating_sub(1);
                        if self.active_pointers_count == 0 {
                            self.last_touch_pos = None;
                            self.touch_down_time = None;
                            self.has_dragged = false;
                        }
                    }
                }
            }
            TouchMode::VirtualTrackpad => {
                match action {
                    TouchAction::Down => {
                        self.active_pointers_count = self.active_pointers_count.saturating_add(1);
                        self.last_touch_pos = Some((clamped_x, clamped_y));
                        self.touch_down_time = Some(now);
                        self.has_dragged = false;
                    }
                    TouchAction::Move => {
                        if let Some((lx, ly)) = self.last_touch_pos {
                            let raw_dx = (clamped_x - lx) * (self.bounds.width as f32) * 1.5;
                            let raw_dy = (clamped_y - ly) * (self.bounds.height as f32) * 1.5;

                            if raw_dx.abs() > self.drag_threshold_px || raw_dy.abs() > self.drag_threshold_px {
                                self.has_dragged = true;
                            }

                            if self.active_pointers_count == 1 {
                                // 触控板相对位移
                                events.push(InputEvent::MouseMove {
                                    dx: raw_dx.round() as i32,
                                    dy: raw_dy.round() as i32,
                                });
                            } else if self.active_pointers_count == 2 {
                                // 触控板双指滚动
                                events.push(InputEvent::MouseScroll {
                                    delta_x: -(raw_dx as i32),
                                    delta_y: -(raw_dy as i32),
                                });
                            }
                        }
                        self.last_touch_pos = Some((clamped_x, clamped_y));
                    }
                    TouchAction::Up | TouchAction::Cancel => {
                        if !self.has_dragged {
                            if self.active_pointers_count == 1 {
                                // 单指轻触点击：鼠标左键单击
                                events.push(InputEvent::MouseDown(1));
                                events.push(InputEvent::MouseUp(1));
                            } else if self.active_pointers_count == 2 {
                                // 双指轻触点击：鼠标右键单击
                                events.push(InputEvent::MouseDown(2));
                                events.push(InputEvent::MouseUp(2));
                            }
                        }

                        self.active_pointers_count = self.active_pointers_count.saturating_sub(1);
                        if self.active_pointers_count == 0 {
                            self.last_touch_pos = None;
                            self.touch_down_time = None;
                            self.has_dragged = false;
                        }
                    }
                }
            }
            TouchMode::GamepadOverlay => {
                // 转发触控原生事件，由上层手柄按键分发器直接产生按键事件
                events.push(InputEvent::Touch {
                    action,
                    pointer_id,
                    normalized_x: clamped_x,
                    normalized_y: clamped_y,
                    pressure: _pressure,
                });
            }
        }

        events
    }
}

/// 移动端虚拟修饰键辅助转换
pub fn create_virtual_key_event(key_name: &str, pressed: bool) -> Option<InputEvent> {
    match key_name.to_lowercase().as_str() {
        "esc" => Some(InputEvent::Key { key_code: 53, pressed, modifiers: 0 }),
        "tab" => Some(InputEvent::Key { key_code: 48, pressed, modifiers: 0 }),
        "ctrl" | "control" => Some(InputEvent::Key { key_code: 59, pressed, modifiers: input_modifiers::CONTROL }),
        "alt" | "opt" | "option" => Some(InputEvent::Key { key_code: 58, pressed, modifiers: input_modifiers::ALT }),
        "win" | "cmd" | "meta" => Some(InputEvent::Key { key_code: 55, pressed, modifiers: input_modifiers::META }),
        "shift" => Some(InputEvent::Key { key_code: 56, pressed, modifiers: input_modifiers::SHIFT }),
        "f5" => Some(InputEvent::Key { key_code: 96, pressed, modifiers: 0 }),
        "f11" => Some(InputEvent::Key { key_code: 103, pressed, modifiers: 0 }),
        "up" | "arrowup" => Some(InputEvent::Key { key_code: 126, pressed, modifiers: 0 }),
        "down" | "arrowdown" => Some(InputEvent::Key { key_code: 125, pressed, modifiers: 0 }),
        "left" | "arrowleft" => Some(InputEvent::Key { key_code: 123, pressed, modifiers: 0 }),
        "right" | "arrowright" => Some(InputEvent::Key { key_code: 124, pressed, modifiers: 0 }),
        "enter" | "return" => Some(InputEvent::Key { key_code: 36, pressed, modifiers: 0 }),
        "backspace" | "delete" => Some(InputEvent::Key { key_code: 51, pressed, modifiers: 0 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_touch_maps_to_absolute_mouse_and_click() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds { width: 1920, height: 1080 },
        );
        let now = Instant::now();

        // Touch Down at center (0.5, 0.5)
        let events_down = tracker.process_touch(TouchAction::Down, 0, 0.5, 0.5, 1.0, now);
        assert_eq!(events_down.len(), 2);
        assert_eq!(events_down[0], InputEvent::MouseMoveAbsolute { x: 960, y: 540 });
        assert_eq!(events_down[1], InputEvent::MouseDown(1));

        // Touch Up
        let events_up = tracker.process_touch(TouchAction::Up, 0, 0.5, 0.5, 0.0, now + Duration::from_millis(50));
        assert_eq!(events_up.len(), 1);
        assert_eq!(events_up[0], InputEvent::MouseUp(1));
    }

    #[test]
    fn virtual_trackpad_moves_and_taps() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::VirtualTrackpad,
            RemoteScreenBounds { width: 1920, height: 1080 },
        );
        let now = Instant::now();

        let _ = tracker.process_touch(TouchAction::Down, 0, 0.1, 0.1, 1.0, now);
        let move_events = tracker.process_touch(TouchAction::Move, 0, 0.12, 0.13, 1.0, now + Duration::from_millis(16));
        assert!(!move_events.is_empty());
        match move_events[0] {
            InputEvent::MouseMove { dx, dy } => {
                assert!(dx > 0);
                assert!(dy > 0);
            }
            _ => panic!("Expected MouseMove relative event"),
        }
    }

    #[test]
    fn virtual_key_events_support_modifiers() {
        let ctrl = create_virtual_key_event("ctrl", true).unwrap();
        match ctrl {
            InputEvent::Key { key_code, pressed, modifiers } => {
                assert_eq!(key_code, 59);
                assert!(pressed);
                assert_eq!(modifiers, input_modifiers::CONTROL);
            }
            _ => panic!("Expected Key event"),
        }
    }
}
