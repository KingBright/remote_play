use protocol::InputEvent;
use remote_core::InputInjector;
use std::error::Error;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    SendInput, VIRTUAL_KEY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

pub struct WindowsInputInjector {
    pressed: AtomicU8,
}

impl WindowsInputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            pressed: AtomicU8::new(0),
        })
    }
}

fn send_mouse(flags: u32, dx: i32, dy: i32, data: u32) {
    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_key(vk: VIRTUAL_KEY, pressed: bool) {
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if pressed { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mac_or_gpui_to_vk(code: u16) -> VIRTUAL_KEY {
    match code {
        53 => 0x1B,  // Esc
        48 => 0x09,  // Tab
        36 => 0x0D,  // Return
        51 => 0x08,  // Delete/Backspace
        123 => 0x25, // Left
        124 => 0x27, // Right
        125 => 0x28, // Down
        126 => 0x26, // Up
        59 => 0x11,  // Ctrl
        58 => 0x12,  // Alt
        55 => 0x5B,  // Cmd/Win
        56 => 0x10,  // Shift
        96 => 0x74,  // F5
        103 => 0x7A, // F11
        other if other < 0xFF => other as VIRTUAL_KEY,
        _ => 0,
    }
}

impl InputInjector for WindowsInputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        match event {
            InputEvent::MouseMove { dx, dy } => send_mouse(MOUSEEVENTF_MOVE, dx, dy, 0),
            InputEvent::MouseMoveAbsolute { x, y } => unsafe {
                let screen_w = GetSystemMetrics(SM_CXSCREEN).max(1);
                let screen_h = GetSystemMetrics(SM_CYSCREEN).max(1);
                let abs_x = (i32::from(x) * 65535) / screen_w;
                let abs_y = (i32::from(y) * 65535) / screen_h;
                send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, abs_x, abs_y, 0);
            },
            InputEvent::MouseDown(button) | InputEvent::MouseUp(button) => {
                let down = matches!(event, InputEvent::MouseDown(_));
                let flags = match (button, down) {
                    (0 | 1, true) => MOUSEEVENTF_LEFTDOWN,
                    (0 | 1, false) => MOUSEEVENTF_LEFTUP,
                    (2, true) => MOUSEEVENTF_RIGHTDOWN,
                    (2, false) => MOUSEEVENTF_RIGHTUP,
                    (3, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (3, false) => MOUSEEVENTF_MIDDLEUP,
                    _ => 0,
                };
                if flags != 0 {
                    send_mouse(flags, 0, 0, 0);
                    if down {
                        self.pressed.fetch_or(1 << button.min(7), Ordering::Relaxed);
                    } else {
                        self.pressed
                            .fetch_and(!(1 << button.min(7)), Ordering::Relaxed);
                    }
                }
            }
            InputEvent::MouseScroll {
                delta_x: _,
                delta_y,
            } => {
                send_mouse(MOUSEEVENTF_WHEEL, 0, 0, (delta_y * 120) as u32);
            }
            InputEvent::Key {
                key_code, pressed, ..
            } => {
                let vk = mac_or_gpui_to_vk(key_code);
                if vk != 0 {
                    send_key(vk, pressed);
                }
            }
            InputEvent::KeyDown(code) => send_key(code as VIRTUAL_KEY, true),
            InputEvent::KeyUp(code) => send_key(code as VIRTUAL_KEY, false),
            InputEvent::ModifiersChanged(_) | InputEvent::Touch { .. } => {}
        }
        Ok(())
    }

    fn release_all_input(&self) {
        self.pressed.store(0, Ordering::Relaxed);
        send_mouse(MOUSEEVENTF_LEFTUP, 0, 0, 0);
        send_mouse(MOUSEEVENTF_RIGHTUP, 0, 0, 0);
        send_mouse(MOUSEEVENTF_MIDDLEUP, 0, 0, 0);
    }
}
