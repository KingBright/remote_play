use crate::windows_keymap::mac_key_to_vk;
use protocol::{InputEvent, input_modifiers};
use remote_core::InputInjector;
use std::collections::BTreeSet;
use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

pub struct WindowsInputInjector {
    pressed: AtomicU8,
    modifiers: AtomicU8,
    pressed_keys: Mutex<BTreeSet<VIRTUAL_KEY>>,
}

impl WindowsInputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Ok(Self {
            pressed: AtomicU8::new(0),
            modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }

    fn post_key(&self, vk: VIRTUAL_KEY, pressed: bool) {
        send_key(vk, pressed);
        let mut keys = self.pressed_keys.lock().unwrap();
        if pressed {
            keys.insert(vk);
        } else {
            keys.remove(&vk);
        }
    }

    fn set_modifiers(&self, modifiers: u8) {
        let previous = self.modifiers.swap(modifiers, Ordering::Relaxed);
        for (flag, vk) in [
            (input_modifiers::SHIFT, 0xA0),
            (input_modifiers::CONTROL, 0xA2),
            (input_modifiers::ALT, 0xA4),
            (input_modifiers::META, 0x5B),
        ] {
            if (previous ^ modifiers) & flag != 0 {
                self.post_key(vk, modifiers & flag != 0);
            }
        }
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
    let extended = matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5);
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: (if pressed { 0 } else { KEYEVENTF_KEYUP })
                    | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &mut input, std::mem::size_of::<INPUT>() as i32);
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
                    (0, true) => MOUSEEVENTF_LEFTDOWN,
                    (0, false) => MOUSEEVENTF_LEFTUP,
                    (1, true) => MOUSEEVENTF_RIGHTDOWN,
                    (1, false) => MOUSEEVENTF_RIGHTUP,
                    (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (2, false) => MOUSEEVENTF_MIDDLEUP,
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
                key_code,
                pressed,
                modifiers,
            } => {
                self.set_modifiers(modifiers);
                if let Some(vk) = mac_key_to_vk(key_code) {
                    self.post_key(vk, pressed);
                }
            }
            InputEvent::KeyDown(code) => self.post_key(code as VIRTUAL_KEY, true),
            InputEvent::KeyUp(code) => self.post_key(code as VIRTUAL_KEY, false),
            InputEvent::ModifiersChanged(modifiers) => self.set_modifiers(modifiers),
            InputEvent::Touch { .. } => {}
        }
        Ok(())
    }

    fn release_all_input(&self) {
        self.modifiers.store(0, Ordering::Relaxed);
        for vk in std::mem::take(&mut *self.pressed_keys.lock().unwrap()) {
            send_key(vk, false);
        }
        self.pressed.store(0, Ordering::Relaxed);
        send_mouse(MOUSEEVENTF_LEFTUP, 0, 0, 0);
        send_mouse(MOUSEEVENTF_RIGHTUP, 0, 0, 0);
        send_mouse(MOUSEEVENTF_MIDDLEUP, 0, 0, 0);
    }
}
