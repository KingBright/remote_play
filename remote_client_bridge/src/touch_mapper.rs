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

use std::collections::HashMap;

pub struct TouchStateTracker {
    pub mode: TouchMode,
    pub bounds: RemoteScreenBounds,
    touch_down_pos: Option<(f32, f32)>,
    last_touch_pos: Option<(f32, f32)>,
    touch_down_time: Option<Instant>,
    active_pointers: Vec<u32>,
    pointer_positions: HashMap<u32, (f32, f32)>,
    long_press_threshold: Duration,
    drag_threshold_px: f32,
    has_dragged: bool,
    is_left_down: bool,
    session_max_pointers: usize,
    two_finger_right_clicked: bool,
    long_press_triggered: bool,
    last_two_finger_centroid: Option<(f32, f32)>,
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
            touch_down_pos: None,
            last_touch_pos: None,
            touch_down_time: None,
            active_pointers: Vec::new(),
            pointer_positions: HashMap::new(),
            long_press_threshold: Duration::from_millis(380),
            drag_threshold_px: 6.0,
            has_dragged: false,
            is_left_down: false,
            session_max_pointers: 0,
            two_finger_right_clicked: false,
            long_press_triggered: false,
            last_two_finger_centroid: None,
        }
    }

    pub fn set_mode(&mut self, mode: TouchMode) -> Vec<InputEvent> {
        if self.mode == mode {
            return Vec::new();
        }
        let releases = self.cancel_all();
        self.mode = mode;
        releases
    }

    pub fn cancel_all(&mut self) -> Vec<InputEvent> {
        let releases = if self.is_left_down {
            vec![InputEvent::MouseUp(0)]
        } else {
            Vec::new()
        };
        self.active_pointers.clear();
        self.pointer_positions.clear();
        self.reset_session();
        releases
    }

    pub fn set_bounds(&mut self, width: u16, height: u16) {
        self.bounds = RemoteScreenBounds { width, height };
    }

    fn reset_session(&mut self) {
        self.touch_down_pos = None;
        self.last_touch_pos = None;
        self.touch_down_time = None;
        self.has_dragged = false;
        self.is_left_down = false;
        self.session_max_pointers = 0;
        self.two_finger_right_clicked = false;
        self.long_press_triggered = false;
        self.last_two_finger_centroid = None;
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
        if !norm_x.is_finite() || !norm_y.is_finite() {
            return self.cancel_all();
        }
        if self.mode != TouchMode::GamepadOverlay
            && action != TouchAction::Down
            && !self.active_pointers.contains(&pointer_id)
        {
            return events;
        }
        // Absolute coordinates are normalized to the entire u16 range on the wire,
        // not decoded-video pixels. Bounds only determine relative-motion sensitivity.
        let clamped_x = norm_x.clamp(0.0, 1.0);
        let clamped_y = norm_y.clamp(0.0, 1.0);

        let abs_x = (clamped_x * f32::from(u16::MAX)).round() as u16;
        let abs_y = (clamped_y * f32::from(u16::MAX)).round() as u16;

        match self.mode {
            TouchMode::DirectTouch => {
                match action {
                    TouchAction::Down => {
                        if !self.active_pointers.contains(&pointer_id) {
                            self.active_pointers.push(pointer_id);
                        }
                        self.pointer_positions
                            .insert(pointer_id, (clamped_x, clamped_y));
                        self.session_max_pointers =
                            self.session_max_pointers.max(self.active_pointers.len());

                        if self.active_pointers.len() == 1 {
                            // 单指按下：记录初始位置与时间，绝对移动光标定位，不发送任何按键（避免污染后续滑动/长按）
                            self.touch_down_pos = Some((clamped_x, clamped_y));
                            self.touch_down_pos = Some((clamped_x, clamped_y));
                            self.last_touch_pos = Some((clamped_x, clamped_y));
                            self.touch_down_time = Some(now);
                            self.has_dragged = false;
                            self.is_left_down = false;
                            self.long_press_triggered = false;
                            self.two_finger_right_clicked = false;
                            self.last_two_finger_centroid = None;

                            events.push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                        } else if self.active_pointers.len() == 2 {
                            // 多指介入：若先前处于拖拽状态，立即释放左键，防止在双指手势中卡键
                            if self.is_left_down {
                                events.push(InputEvent::MouseUp(0));
                                self.is_left_down = false;
                            }
                            if let (Some(&p0), Some(&p1)) = (
                                self.pointer_positions.get(&self.active_pointers[0]),
                                self.pointer_positions.get(&self.active_pointers[1]),
                            ) {
                                self.last_two_finger_centroid =
                                    Some(((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0));
                            }
                        } else if self.active_pointers.len() > 2 && self.is_left_down {
                            events.push(InputEvent::MouseUp(0));
                            self.is_left_down = false;
                        }
                    }
                    TouchAction::Move => {
                        self.pointer_positions
                            .insert(pointer_id, (clamped_x, clamped_y));
                        self.last_touch_pos = Some((clamped_x, clamped_y));

                        if self.active_pointers.len() == 1 && self.session_max_pointers == 1 {
                            if !self.long_press_triggered {
                                if let Some((start_x, start_y)) = self.touch_down_pos {
                                    let dx_total_px =
                                        (clamped_x - start_x) * (self.bounds.width as f32);
                                    let dy_total_px =
                                        (clamped_y - start_y) * (self.bounds.height as f32);

                                    if dx_total_px.abs() > self.drag_threshold_px
                                        || dy_total_px.abs() > self.drag_threshold_px
                                    {
                                        self.has_dragged = true;
                                    }
                                }

                                if self.has_dragged {
                                    // 单指滑动判定为拖拽：在位移前补发 MouseDown(0)
                                    if !self.is_left_down {
                                        events.push(InputEvent::MouseDown(0));
                                        self.is_left_down = true;
                                    }
                                    events
                                        .push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                                } else {
                                    // 阀值内轻微位移：若按压时长已达长按阈值，触发右键菜单
                                    if let Some(down_t) = self.touch_down_time {
                                        if now.duration_since(down_t) >= self.long_press_threshold {
                                            self.long_press_triggered = true;
                                            events.push(InputEvent::MouseMoveAbsolute {
                                                x: abs_x,
                                                y: abs_y,
                                            });
                                            events.push(InputEvent::MouseDown(1));
                                            events.push(InputEvent::MouseUp(1));
                                        } else {
                                            events.push(InputEvent::MouseMoveAbsolute {
                                                x: abs_x,
                                                y: abs_y,
                                            });
                                        }
                                    } else {
                                        events.push(InputEvent::MouseMoveAbsolute {
                                            x: abs_x,
                                            y: abs_y,
                                        });
                                    }
                                }
                            } else {
                                events.push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                            }
                        } else if self.active_pointers.len() == 2 {
                            // 双指滑动：基于双指质心位移映射为平滑页面滚动（杜绝单触点覆盖与双倍累加抖动）
                            if self.is_left_down {
                                events.push(InputEvent::MouseUp(0));
                                self.is_left_down = false;
                            }
                            if let (Some(&p0), Some(&p1)) = (
                                self.pointer_positions.get(&self.active_pointers[0]),
                                self.pointer_positions.get(&self.active_pointers[1]),
                            ) {
                                let cur_centroid = ((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0);
                                if let Some(prev_centroid) = self.last_two_finger_centroid {
                                    let dx_px = (cur_centroid.0 - prev_centroid.0)
                                        * (self.bounds.width as f32);
                                    let dy_px = (cur_centroid.1 - prev_centroid.1)
                                        * (self.bounds.height as f32);

                                    if dx_px.abs() > self.drag_threshold_px
                                        || dy_px.abs() > self.drag_threshold_px
                                    {
                                        self.has_dragged = true;
                                    }

                                    let delta_x = -(dx_px.round() as i32);
                                    let delta_y = -(dy_px.round() as i32);
                                    if delta_x != 0 || delta_y != 0 {
                                        self.has_dragged = true;
                                        events.push(InputEvent::MouseScroll { delta_x, delta_y });
                                    }
                                }
                                self.last_two_finger_centroid = Some(cur_centroid);
                            }
                        }
                    }
                    TouchAction::Up => {
                        if self.session_max_pointers == 1 {
                            if self.is_left_down {
                                // 拖拽释放
                                events.push(InputEvent::MouseUp(0));
                                self.is_left_down = false;
                            } else if self.long_press_triggered {
                                // 长按右键已在按住过程中触发，抬起时不重复发射
                            } else if let Some(down_t) = self.touch_down_time {
                                if !self.has_dragged
                                    && now.duration_since(down_t) >= self.long_press_threshold
                                {
                                    // 单指长按抬起：触发鼠标右键（无 MouseDown(0) 污染）
                                    events
                                        .push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                                    events.push(InputEvent::MouseDown(1));
                                    events.push(InputEvent::MouseUp(1));
                                    self.long_press_triggered = true;
                                } else if !self.has_dragged {
                                    // 单指短按点击（无拖拽且时长 < 380ms）：触发鼠标左键单击
                                    events
                                        .push(InputEvent::MouseMoveAbsolute { x: abs_x, y: abs_y });
                                    events.push(InputEvent::MouseDown(0));
                                    events.push(InputEvent::MouseUp(0));
                                }
                            }
                        } else if self.session_max_pointers == 2 {
                            if self.is_left_down {
                                events.push(InputEvent::MouseUp(0));
                                self.is_left_down = false;
                            }
                            if !self.has_dragged && !self.two_finger_right_clicked {
                                // 双指轻点：触发鼠标右键，且整轮手势仅触发一次，不污染后续抬指
                                events.push(InputEvent::MouseDown(1));
                                events.push(InputEvent::MouseUp(1));
                                self.two_finger_right_clicked = true;
                            }
                        }

                        self.active_pointers.retain(|&id| id != pointer_id);
                        self.pointer_positions.remove(&pointer_id);
                        if self.active_pointers.len() < 2 {
                            self.last_two_finger_centroid = None;
                        }

                        if self.active_pointers.is_empty() {
                            self.reset_session();
                        }
                    }
                    TouchAction::Cancel => {
                        if self.is_left_down {
                            events.push(InputEvent::MouseUp(0));
                            self.is_left_down = false;
                        }

                        self.active_pointers.retain(|&id| id != pointer_id);
                        self.pointer_positions.remove(&pointer_id);
                        if self.active_pointers.len() < 2 {
                            self.last_two_finger_centroid = None;
                        }

                        if self.active_pointers.is_empty() {
                            self.reset_session();
                        }
                    }
                }
            }
            TouchMode::VirtualTrackpad => {
                match action {
                    TouchAction::Down => {
                        if !self.active_pointers.contains(&pointer_id) {
                            self.active_pointers.push(pointer_id);
                        }
                        self.pointer_positions
                            .insert(pointer_id, (clamped_x, clamped_y));
                        self.session_max_pointers =
                            self.session_max_pointers.max(self.active_pointers.len());

                        if self.active_pointers.len() == 1 {
                            self.touch_down_pos = Some((clamped_x, clamped_y));
                            self.last_touch_pos = Some((clamped_x, clamped_y));
                            self.touch_down_time = Some(now);
                            self.has_dragged = false;
                            self.two_finger_right_clicked = false;
                            self.last_two_finger_centroid = None;
                        } else if self.active_pointers.len() == 2 {
                            let p0 = self
                                .pointer_positions
                                .get(&self.active_pointers[0])
                                .copied();
                            let p1 = self
                                .pointer_positions
                                .get(&self.active_pointers[1])
                                .copied();
                            if let (Some(p0), Some(p1)) = (p0, p1) {
                                self.last_two_finger_centroid =
                                    Some(((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0));
                            }
                        }
                    }
                    TouchAction::Move => {
                        let prev_pos = self
                            .pointer_positions
                            .insert(pointer_id, (clamped_x, clamped_y));
                        if self.active_pointers.len() == 1 && self.session_max_pointers == 1 {
                            if let Some((lx, ly)) = prev_pos {
                                let raw_dx = (clamped_x - lx) * (self.bounds.width as f32) * 1.5;
                                let raw_dy = (clamped_y - ly) * (self.bounds.height as f32) * 1.5;

                                let (start_x, start_y) =
                                    self.touch_down_pos.unwrap_or((clamped_x, clamped_y));
                                if ((clamped_x - start_x) * f32::from(self.bounds.width)).abs()
                                    > self.drag_threshold_px
                                    || ((clamped_y - start_y) * f32::from(self.bounds.height)).abs()
                                        > self.drag_threshold_px
                                {
                                    self.has_dragged = true;
                                }

                                // 触控板相对位移
                                events.push(InputEvent::MouseMove {
                                    dx: raw_dx.round() as i32,
                                    dy: raw_dy.round() as i32,
                                });
                            }
                        } else if self.active_pointers.len() == 2 {
                            // 触控板双指滚动
                            if let (Some(&p0), Some(&p1)) = (
                                self.pointer_positions.get(&self.active_pointers[0]),
                                self.pointer_positions.get(&self.active_pointers[1]),
                            ) {
                                let cur_centroid = ((p0.0 + p1.0) / 2.0, (p0.1 + p1.1) / 2.0);
                                if let Some(prev_centroid) = self.last_two_finger_centroid {
                                    let raw_dx = (cur_centroid.0 - prev_centroid.0)
                                        * (self.bounds.width as f32)
                                        * 1.5;
                                    let raw_dy = (cur_centroid.1 - prev_centroid.1)
                                        * (self.bounds.height as f32)
                                        * 1.5;

                                    if raw_dx.abs() > self.drag_threshold_px
                                        || raw_dy.abs() > self.drag_threshold_px
                                    {
                                        self.has_dragged = true;
                                    }

                                    let delta_x = -(raw_dx.round() as i32);
                                    let delta_y = -(raw_dy.round() as i32);
                                    if delta_x != 0 || delta_y != 0 {
                                        self.has_dragged = true;
                                        events.push(InputEvent::MouseScroll { delta_x, delta_y });
                                    }
                                }
                                self.last_two_finger_centroid = Some(cur_centroid);
                            }
                        }
                        self.last_touch_pos = Some((clamped_x, clamped_y));
                    }
                    TouchAction::Up => {
                        if !self.has_dragged {
                            if self.session_max_pointers == 1 {
                                // 单指轻触点击：鼠标左键单击
                                events.push(InputEvent::MouseDown(0));
                                events.push(InputEvent::MouseUp(0));
                            } else if self.session_max_pointers == 2
                                && !self.two_finger_right_clicked
                            {
                                // 双指轻触点击：鼠标右键单击（仅触发一次）
                                events.push(InputEvent::MouseDown(1));
                                events.push(InputEvent::MouseUp(1));
                                self.two_finger_right_clicked = true;
                            }
                        }

                        self.active_pointers.retain(|&id| id != pointer_id);
                        self.pointer_positions.remove(&pointer_id);
                        if self.active_pointers.len() < 2 {
                            self.last_two_finger_centroid = None;
                        }

                        if self.active_pointers.is_empty() {
                            self.reset_session();
                        }
                    }
                    TouchAction::Cancel => {
                        self.active_pointers.retain(|&id| id != pointer_id);
                        self.pointer_positions.remove(&pointer_id);
                        if self.active_pointers.len() < 2 {
                            self.last_two_finger_centroid = None;
                        }

                        if self.active_pointers.is_empty() {
                            self.reset_session();
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

fn letter_to_mac_code(ch: char) -> Option<u16> {
    match ch {
        'a' => Some(0),
        'b' => Some(11),
        'c' => Some(8),
        'd' => Some(2),
        'e' => Some(14),
        'f' => Some(3),
        'g' => Some(5),
        'h' => Some(4),
        'i' => Some(34),
        'j' => Some(38),
        'k' => Some(40),
        'l' => Some(37),
        'm' => Some(46),
        'n' => Some(45),
        'o' => Some(31),
        'p' => Some(35),
        'q' => Some(12),
        'r' => Some(15),
        's' => Some(1),
        't' => Some(17),
        'u' => Some(32),
        'v' => Some(9),
        'w' => Some(13),
        'x' => Some(7),
        'y' => Some(16),
        'z' => Some(6),
        _ => None,
    }
}

fn digit_to_mac_code(ch: char) -> Option<u16> {
    match ch {
        '0' => Some(29),
        '1' => Some(18),
        '2' => Some(19),
        '3' => Some(20),
        '4' => Some(21),
        '5' => Some(23),
        '6' => Some(22),
        '7' => Some(26),
        '8' => Some(28),
        '9' => Some(25),
        _ => None,
    }
}

fn symbol_to_mac_code_and_modifiers(ch: char) -> Option<(u16, u8)> {
    match ch {
        '-' => Some((27, 0)),
        '=' => Some((24, 0)),
        '[' => Some((33, 0)),
        ']' => Some((30, 0)),
        '\\' => Some((42, 0)),
        ';' => Some((41, 0)),
        '\'' => Some((39, 0)),
        ',' => Some((43, 0)),
        '.' => Some((47, 0)),
        '/' => Some((44, 0)),
        '`' => Some((50, 0)),
        '!' => Some((18, input_modifiers::SHIFT)),
        '@' => Some((19, input_modifiers::SHIFT)),
        '#' => Some((20, input_modifiers::SHIFT)),
        '$' => Some((21, input_modifiers::SHIFT)),
        '%' => Some((23, input_modifiers::SHIFT)),
        '^' => Some((22, input_modifiers::SHIFT)),
        '&' => Some((26, input_modifiers::SHIFT)),
        '*' => Some((28, input_modifiers::SHIFT)),
        '(' => Some((25, input_modifiers::SHIFT)),
        ')' => Some((29, input_modifiers::SHIFT)),
        '_' => Some((27, input_modifiers::SHIFT)),
        '+' => Some((24, input_modifiers::SHIFT)),
        '{' => Some((33, input_modifiers::SHIFT)),
        '}' => Some((30, input_modifiers::SHIFT)),
        '|' => Some((42, input_modifiers::SHIFT)),
        ':' => Some((41, input_modifiers::SHIFT)),
        '"' => Some((39, input_modifiers::SHIFT)),
        '<' => Some((43, input_modifiers::SHIFT)),
        '>' => Some((47, input_modifiers::SHIFT)),
        '?' => Some((44, input_modifiers::SHIFT)),
        '~' => Some((50, input_modifiers::SHIFT)),
        _ => None,
    }
}

/// 移动端虚拟修饰键辅助转换
pub fn create_virtual_key_event(key_name: &str, pressed: bool) -> Option<InputEvent> {
    let trimmed = key_name.trim();
    if trimmed.is_empty() {
        return if key_name == " " {
            Some(InputEvent::Key {
                key_code: 49,
                pressed,
                modifiers: 0,
            })
        } else {
            None
        };
    }

    if trimmed.len() == 1 {
        let ch = trimmed.chars().next().unwrap();
        if ch.is_ascii_uppercase() {
            let lower = ch.to_ascii_lowercase();
            if let Some(code) = letter_to_mac_code(lower) {
                return Some(InputEvent::Key {
                    key_code: code,
                    pressed,
                    modifiers: input_modifiers::SHIFT,
                });
            }
        } else if ch.is_ascii_lowercase() {
            if let Some(code) = letter_to_mac_code(ch) {
                return Some(InputEvent::Key {
                    key_code: code,
                    pressed,
                    modifiers: 0,
                });
            }
        } else if ch.is_ascii_digit() {
            if let Some(code) = digit_to_mac_code(ch) {
                return Some(InputEvent::Key {
                    key_code: code,
                    pressed,
                    modifiers: 0,
                });
            }
        } else if let Some((code, modifiers)) = symbol_to_mac_code_and_modifiers(ch) {
            return Some(InputEvent::Key {
                key_code: code,
                pressed,
                modifiers,
            });
        }
    }

    match trimmed.to_lowercase().as_str() {
        "esc" | "escape" => Some(InputEvent::Key {
            key_code: 53,
            pressed,
            modifiers: 0,
        }),
        "tab" | "\t" => Some(InputEvent::Key {
            key_code: 48,
            pressed,
            modifiers: 0,
        }),
        "ctrl" | "control" => Some(InputEvent::Key {
            key_code: 59,
            pressed,
            modifiers: input_modifiers::CONTROL,
        }),
        "alt" | "opt" | "option" => Some(InputEvent::Key {
            key_code: 58,
            pressed,
            modifiers: input_modifiers::ALT,
        }),
        "win" | "cmd" | "meta" => Some(InputEvent::Key {
            key_code: 55,
            pressed,
            modifiers: input_modifiers::META,
        }),
        "shift" => Some(InputEvent::Key {
            key_code: 56,
            pressed,
            modifiers: input_modifiers::SHIFT,
        }),
        "capslock" | "caps_lock" => Some(InputEvent::Key {
            key_code: 57,
            pressed,
            modifiers: 0,
        }),
        "f1" => Some(InputEvent::Key {
            key_code: 122,
            pressed,
            modifiers: 0,
        }),
        "f2" => Some(InputEvent::Key {
            key_code: 120,
            pressed,
            modifiers: 0,
        }),
        "f3" => Some(InputEvent::Key {
            key_code: 99,
            pressed,
            modifiers: 0,
        }),
        "f4" => Some(InputEvent::Key {
            key_code: 118,
            pressed,
            modifiers: 0,
        }),
        "f5" => Some(InputEvent::Key {
            key_code: 96,
            pressed,
            modifiers: 0,
        }),
        "f6" => Some(InputEvent::Key {
            key_code: 97,
            pressed,
            modifiers: 0,
        }),
        "f7" => Some(InputEvent::Key {
            key_code: 98,
            pressed,
            modifiers: 0,
        }),
        "f8" => Some(InputEvent::Key {
            key_code: 100,
            pressed,
            modifiers: 0,
        }),
        "f9" => Some(InputEvent::Key {
            key_code: 101,
            pressed,
            modifiers: 0,
        }),
        "f10" => Some(InputEvent::Key {
            key_code: 109,
            pressed,
            modifiers: 0,
        }),
        "f11" => Some(InputEvent::Key {
            key_code: 103,
            pressed,
            modifiers: 0,
        }),
        "f12" => Some(InputEvent::Key {
            key_code: 111,
            pressed,
            modifiers: 0,
        }),
        "insert" => Some(InputEvent::Key {
            key_code: 114,
            pressed,
            modifiers: 0,
        }),
        "home" => Some(InputEvent::Key {
            key_code: 115,
            pressed,
            modifiers: 0,
        }),
        "pageup" | "page_up" => Some(InputEvent::Key {
            key_code: 116,
            pressed,
            modifiers: 0,
        }),
        "pagedown" | "page_down" => Some(InputEvent::Key {
            key_code: 121,
            pressed,
            modifiers: 0,
        }),
        "end" => Some(InputEvent::Key {
            key_code: 119,
            pressed,
            modifiers: 0,
        }),
        "up" | "arrowup" | "▲" => Some(InputEvent::Key {
            key_code: 126,
            pressed,
            modifiers: 0,
        }),
        "down" | "arrowdown" | "▼" => Some(InputEvent::Key {
            key_code: 125,
            pressed,
            modifiers: 0,
        }),
        "left" | "arrowleft" | "◄" => Some(InputEvent::Key {
            key_code: 123,
            pressed,
            modifiers: 0,
        }),
        "right" | "arrowright" | "►" => Some(InputEvent::Key {
            key_code: 124,
            pressed,
            modifiers: 0,
        }),
        "enter" | "return" | "\n" => Some(InputEvent::Key {
            key_code: 36,
            pressed,
            modifiers: 0,
        }),
        "backspace" | "delete" | "\u{8}" | "\u{7f}" => Some(InputEvent::Key {
            key_code: 51,
            pressed,
            modifiers: 0,
        }),
        "space" | " " => Some(InputEvent::Key {
            key_code: 49,
            pressed,
            modifiers: 0,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_coordinates_follow_wire_contract_at_every_resolution() {
        for (width, height) in [(640, 480), (1920, 1080), (3840, 2160)] {
            for (n, encoded) in [(0.0, 0), (0.5, 32768), (1.0, 65535)] {
                let mut tracker = TouchStateTracker::new(
                    TouchMode::DirectTouch,
                    RemoteScreenBounds { width, height },
                );
                assert_eq!(
                    tracker.process_touch(TouchAction::Down, 0, n, n, 1.0, Instant::now()),
                    vec![InputEvent::MouseMoveAbsolute {
                        x: encoded,
                        y: encoded
                    }]
                );
            }
        }
    }

    #[test]
    fn switching_touch_mode_releases_drag_and_ignores_cancelled_tail() {
        let mut t = TouchStateTracker::default();
        let now = Instant::now();
        t.process_touch(TouchAction::Down, 1, 0.2, 0.2, 1.0, now);
        t.process_touch(TouchAction::Move, 1, 0.4, 0.2, 1.0, now);
        assert_eq!(
            t.set_mode(TouchMode::VirtualTrackpad),
            vec![InputEvent::MouseUp(0)]
        );
        assert!(
            t.process_touch(TouchAction::Up, 1, 0.4, 0.2, 0.0, now)
                .is_empty()
        );
    }

    #[test]
    fn slow_two_finger_scroll_has_no_click_on_lift() {
        let mut t = TouchStateTracker::default();
        let now = Instant::now();
        t.process_touch(TouchAction::Down, 1, 0.2, 0.2, 1.0, now);
        t.process_touch(TouchAction::Down, 2, 0.4, 0.2, 1.0, now);
        for i in 1..20 {
            let y = 0.2 + i as f32 * 0.001;
            t.process_touch(TouchAction::Move, 1, 0.2, y, 1.0, now);
            t.process_touch(TouchAction::Move, 2, 0.4, y, 1.0, now);
        }
        assert!(
            t.process_touch(TouchAction::Up, 1, 0.2, 0.22, 0.0, now)
                .is_empty()
        );
        assert!(
            t.process_touch(TouchAction::Up, 2, 0.4, 0.22, 0.0, now)
                .is_empty()
        );
    }

    #[test]
    fn direct_touch_tap_emits_click_on_up_without_premature_down() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        // Touch Down at center (0.5, 0.5) - should only position cursor, NO MouseDown(0)
        let events_down = tracker.process_touch(TouchAction::Down, 0, 0.5, 0.5, 1.0, now);
        assert_eq!(events_down.len(), 1);
        assert_eq!(
            events_down[0],
            InputEvent::MouseMoveAbsolute { x: 32768, y: 32768 }
        );

        // Touch Up after 50ms - emits clean atomic click
        let events_up = tracker.process_touch(
            TouchAction::Up,
            0,
            0.5,
            0.5,
            0.0,
            now + Duration::from_millis(50),
        );
        assert_eq!(events_up.len(), 3);
        assert_eq!(
            events_up[0],
            InputEvent::MouseMoveAbsolute { x: 32768, y: 32768 }
        );
        assert_eq!(events_up[1], InputEvent::MouseDown(0));
        assert_eq!(events_up[2], InputEvent::MouseUp(0));
    }

    #[test]
    fn direct_touch_long_press_emits_clean_right_click() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        // Touch Down at (0.2, 0.2)
        let events_down = tracker.process_touch(TouchAction::Down, 0, 0.2, 0.2, 1.0, now);
        assert_eq!(events_down.len(), 1);
        assert_eq!(
            events_down[0],
            InputEvent::MouseMoveAbsolute { x: 13107, y: 13107 }
        );

        // Touch Up after 400ms without dragging - triggers right click, NO left button pollution
        let events_up = tracker.process_touch(
            TouchAction::Up,
            0,
            0.2,
            0.2,
            0.0,
            now + Duration::from_millis(400),
        );
        assert_eq!(events_up.len(), 3);
        assert_eq!(
            events_up[0],
            InputEvent::MouseMoveAbsolute { x: 13107, y: 13107 }
        );
        assert_eq!(events_up[1], InputEvent::MouseDown(1));
        assert_eq!(events_up[2], InputEvent::MouseUp(1));
    }

    #[test]
    fn direct_touch_drag_emits_mouse_down_and_move_and_up() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        // Touch Down at (0.1, 0.1)
        let events_down = tracker.process_touch(TouchAction::Down, 0, 0.1, 0.1, 1.0, now);
        assert_eq!(events_down.len(), 1);

        // Move slightly (within 6px threshold: 0.001 * 1920 = 1.92px)
        let events_move_small = tracker.process_touch(
            TouchAction::Move,
            0,
            0.101,
            0.1,
            1.0,
            now + Duration::from_millis(16),
        );
        assert_eq!(events_move_small.len(), 1); // Only MouseMoveAbsolute, no MouseDown yet

        // Move significantly beyond threshold (0.02 * 1920 = 38.4px)
        let events_drag = tracker.process_touch(
            TouchAction::Move,
            0,
            0.12,
            0.1,
            1.0,
            now + Duration::from_millis(32),
        );
        assert_eq!(events_drag.len(), 2);
        assert_eq!(events_drag[0], InputEvent::MouseDown(0));
        assert!(matches!(
            events_drag[1],
            InputEvent::MouseMoveAbsolute { .. }
        ));

        // Touch Up - releases drag
        let events_up = tracker.process_touch(
            TouchAction::Up,
            0,
            0.12,
            0.1,
            0.0,
            now + Duration::from_millis(100),
        );
        assert_eq!(events_up.len(), 1);
        assert_eq!(events_up[0], InputEvent::MouseUp(0));
    }

    #[test]
    fn direct_touch_two_finger_scroll_does_not_pollute_mouse_button() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        // Finger 0 down
        let ev0 = tracker.process_touch(TouchAction::Down, 0, 0.5, 0.5, 1.0, now);
        assert_eq!(ev0.len(), 1); // MouseMoveAbsolute only

        // Finger 1 down
        let ev1 = tracker.process_touch(
            TouchAction::Down,
            1,
            0.6,
            0.5,
            1.0,
            now + Duration::from_millis(10),
        );
        assert!(ev1.is_empty());

        // Finger 0 moves down (scroll)
        let scroll_ev = tracker.process_touch(
            TouchAction::Move,
            0,
            0.5,
            0.55,
            1.0,
            now + Duration::from_millis(20),
        );
        assert_eq!(scroll_ev.len(), 1);
        match scroll_ev[0] {
            InputEvent::MouseScroll { delta_y, .. } => {
                assert!(delta_y < 0);
            }
            _ => panic!("Expected MouseScroll"),
        }

        // Lift both fingers - no spurious mouse clicks!
        let up0 = tracker.process_touch(
            TouchAction::Up,
            0,
            0.5,
            0.55,
            0.0,
            now + Duration::from_millis(30),
        );
        assert!(up0.is_empty());
        let up1 = tracker.process_touch(
            TouchAction::Up,
            1,
            0.6,
            0.5,
            0.0,
            now + Duration::from_millis(40),
        );
        assert!(up1.is_empty());
    }

    #[test]
    fn direct_touch_two_finger_tap_emits_right_click() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::DirectTouch,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        // Finger 0 down
        tracker.process_touch(TouchAction::Down, 0, 0.5, 0.5, 1.0, now);
        // Finger 1 down
        tracker.process_touch(TouchAction::Down, 1, 0.6, 0.5, 1.0, now);

        // Finger 0 up without dragging -> emits right click
        let up0 = tracker.process_touch(
            TouchAction::Up,
            0,
            0.5,
            0.5,
            0.0,
            now + Duration::from_millis(50),
        );
        assert_eq!(up0.len(), 2);
        assert_eq!(up0[0], InputEvent::MouseDown(1));
        assert_eq!(up0[1], InputEvent::MouseUp(1));

        // Finger 1 up -> MUST be completely empty, no spurious left click!
        let up1 = tracker.process_touch(
            TouchAction::Up,
            1,
            0.6,
            0.5,
            0.0,
            now + Duration::from_millis(60),
        );
        assert!(
            up1.is_empty(),
            "Finger 1 up must not emit spurious left clicks!"
        );
    }

    #[test]
    fn virtual_trackpad_two_finger_tap_emits_right_click_without_spurious_left() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::VirtualTrackpad,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        tracker.process_touch(TouchAction::Down, 0, 0.5, 0.5, 1.0, now);
        tracker.process_touch(TouchAction::Down, 1, 0.6, 0.5, 1.0, now);

        let up0 = tracker.process_touch(
            TouchAction::Up,
            0,
            0.5,
            0.5,
            0.0,
            now + Duration::from_millis(50),
        );
        assert_eq!(up0.len(), 2);
        assert_eq!(up0[0], InputEvent::MouseDown(1));
        assert_eq!(up0[1], InputEvent::MouseUp(1));

        let up1 = tracker.process_touch(
            TouchAction::Up,
            1,
            0.6,
            0.5,
            0.0,
            now + Duration::from_millis(60),
        );
        assert!(
            up1.is_empty(),
            "Virtual trackpad finger 1 up must not emit spurious left clicks!"
        );
    }

    #[test]
    fn virtual_trackpad_moves_and_taps() {
        let mut tracker = TouchStateTracker::new(
            TouchMode::VirtualTrackpad,
            RemoteScreenBounds {
                width: 1920,
                height: 1080,
            },
        );
        let now = Instant::now();

        let _ = tracker.process_touch(TouchAction::Down, 0, 0.1, 0.1, 1.0, now);
        let move_events = tracker.process_touch(
            TouchAction::Move,
            0,
            0.12,
            0.13,
            1.0,
            now + Duration::from_millis(16),
        );
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
    fn virtual_key_events_support_modifiers_and_characters() {
        let ctrl = create_virtual_key_event("ctrl", true).unwrap();
        match ctrl {
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                assert_eq!(key_code, 59);
                assert!(pressed);
                assert_eq!(modifiers, input_modifiers::CONTROL);
            }
            _ => panic!("Expected Key event"),
        }

        let key_a = create_virtual_key_event("a", true).unwrap();
        match key_a {
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                assert_eq!(key_code, 0);
                assert!(pressed);
                assert_eq!(modifiers, 0);
            }
            _ => panic!("Expected Key event for 'a'"),
        }

        let key_upper_a = create_virtual_key_event("A", true).unwrap();
        match key_upper_a {
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                assert_eq!(key_code, 0);
                assert!(pressed);
                assert_eq!(modifiers, input_modifiers::SHIFT);
            }
            _ => panic!("Expected Key event for 'A' with SHIFT"),
        }

        let slash = create_virtual_key_event("/", true).unwrap();
        match slash {
            InputEvent::Key {
                key_code,
                modifiers,
                ..
            } => {
                assert_eq!(key_code, 44);
                assert_eq!(modifiers, 0);
            }
            _ => panic!("Expected Key event for '/'"),
        }

        let question = create_virtual_key_event("?", true).unwrap();
        match question {
            InputEvent::Key {
                key_code,
                modifiers,
                ..
            } => {
                assert_eq!(key_code, 44);
                assert_eq!(modifiers, input_modifiers::SHIFT);
            }
            _ => panic!("Expected Key event for '?' with SHIFT"),
        }

        let space = create_virtual_key_event("space", true).unwrap();
        match space {
            InputEvent::Key { key_code, .. } => assert_eq!(key_code, 49),
            _ => panic!("Expected Space key"),
        }
    }
}

#[derive(Default)]
pub struct VirtualKeyboard {
    held_modifiers: u8,
}

impl VirtualKeyboard {
    pub fn event(&mut self, name: &str, pressed: bool) -> Vec<InputEvent> {
        let Some(InputEvent::Key {
            key_code,
            modifiers,
            ..
        }) = create_virtual_key_event(name, pressed)
        else {
            return Vec::new();
        };
        let modifier = match key_code {
            59 => input_modifiers::CONTROL,
            58 => input_modifiers::ALT,
            55 => input_modifiers::META,
            56 => input_modifiers::SHIFT,
            _ => 0,
        };
        if modifier != 0 {
            if pressed {
                self.held_modifiers |= modifier;
            } else {
                self.held_modifiers &= !modifier;
            }
            return vec![InputEvent::ModifiersChanged(self.held_modifiers)];
        }
        let mut events = vec![InputEvent::Key {
            key_code,
            pressed,
            modifiers: modifiers | self.held_modifiers,
        }];
        if !pressed && modifiers & !self.held_modifiers != 0 {
            events.push(InputEvent::ModifiersChanged(self.held_modifiers));
        }
        events
    }
}

#[cfg(test)]
mod virtual_keyboard_tests {
    use super::*;
    #[test]
    fn held_ctrl_survives_a_key_pair() {
        let mut keys = VirtualKeyboard::default();
        assert_eq!(
            keys.event("ctrl", true),
            vec![InputEvent::ModifiersChanged(input_modifiers::CONTROL)]
        );
        for pressed in [true, false] {
            assert!(
                matches!(keys.event("c", pressed).as_slice(), [InputEvent::Key { modifiers, .. }] if *modifiers == input_modifiers::CONTROL)
            );
        }
        assert_eq!(
            keys.event("ctrl", false),
            vec![InputEvent::ModifiersChanged(0)]
        );
    }
    #[test]
    fn multiple_modifiers_release_independently() {
        let mut keys = VirtualKeyboard::default();
        keys.event("ctrl", true);
        keys.event("shift", true);
        assert_eq!(
            keys.event("ctrl", false),
            vec![InputEvent::ModifiersChanged(input_modifiers::SHIFT)]
        );
        assert_eq!(
            keys.event("shift", false),
            vec![InputEvent::ModifiersChanged(0)]
        );
    }
    #[test]
    fn uppercase_temporary_shift_does_not_stick() {
        let mut keys = VirtualKeyboard::default();
        keys.event("ctrl", true);
        assert_eq!(
            keys.event("A", false).last(),
            Some(&InputEvent::ModifiersChanged(input_modifiers::CONTROL))
        );
        assert!(keys.event("unknown-key", true).is_empty());
    }
}
