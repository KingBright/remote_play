use protocol::InputEvent;
use remote_core::InputInjector;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::raw::{c_char, c_int, c_short, c_uint, c_ulong};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

// Linux Input subsystem constants from <linux/input.h> and <linux/uinput.h>
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_WHEEL: u16 = 0x08;
const REL_HWHEEL: u16 = 0x06;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MAX_RANGE: i32 = 65535;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;

// uinput ioctl codes
const UI_SET_EVBIT: c_ulong = 0x40045564;
const UI_SET_KEYBIT: c_ulong = 0x40045565;
const UI_SET_RELBIT: c_ulong = 0x40045566;
const UI_SET_ABSBIT: c_ulong = 0x40045567;
const UI_DEV_CREATE: c_ulong = 0x5501;
const UI_DEV_DESTROY: c_ulong = 0x5502;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputUserDev {
    name: [c_char; 80],
    id: InputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

#[repr(C)]
struct TimeVal {
    tv_sec: c_ulong,
    tv_usec: c_ulong,
}

#[repr(C)]
struct InputEventRaw {
    time: TimeVal,
    r#type: u16,
    code: u16,
    value: i32,
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn write(fd: c_int, buf: *const std::ffi::c_void, count: usize) -> isize;
}

pub struct LinuxUinputInjector {
    file: Mutex<Option<File>>,
    pressed_buttons: AtomicU8,
    active_modifiers: AtomicU8,
    pressed_keys: Mutex<BTreeSet<u16>>,
}

unsafe impl Send for LinuxUinputInjector {}
unsafe impl Sync for LinuxUinputInjector {}

impl LinuxUinputInjector {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
        {
            Ok(f) => f,
            Err(err) => {
                eprintln!(
                    "[LinuxInput] Failed to open /dev/uinput: {err}. Checking /dev/input/uinput..."
                );
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/input/uinput")
                    .map_err(|e| format!("Cannot open /dev/uinput: {e}"))?
            }
        };

        let fd = file.as_raw_fd();
        unsafe {
            // Enable event types
            ioctl(fd, UI_SET_EVBIT, EV_SYN as c_ulong);
            ioctl(fd, UI_SET_EVBIT, EV_KEY as c_ulong);
            ioctl(fd, UI_SET_EVBIT, EV_REL as c_ulong);
            ioctl(fd, UI_SET_EVBIT, EV_ABS as c_ulong);

            // Enable mouse buttons
            for btn in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
                ioctl(fd, UI_SET_KEYBIT, btn as c_ulong);
            }

            // Enable keyboard keys (1 to 255)
            for k in 1..=255 {
                ioctl(fd, UI_SET_KEYBIT, k as c_ulong);
            }

            // Enable relative movement & wheel
            ioctl(fd, UI_SET_RELBIT, REL_X as c_ulong);
            ioctl(fd, UI_SET_RELBIT, REL_Y as c_ulong);
            ioctl(fd, UI_SET_RELBIT, REL_WHEEL as c_ulong);
            ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as c_ulong);

            // Enable absolute movement (for absolute mouse coordinates)
            ioctl(fd, UI_SET_ABSBIT, ABS_X as c_ulong);
            ioctl(fd, UI_SET_ABSBIT, ABS_Y as c_ulong);

            // Setup uinput device setup
            let mut uidev: UinputUserDev = std::mem::zeroed();
            let dev_name = b"RemotePlay Virtual Controller\0";
            std::ptr::copy_nonoverlapping(
                dev_name.as_ptr() as *const c_char,
                uidev.name.as_mut_ptr(),
                dev_name.len(),
            );
            uidev.id.bustype = 0x03; // BUS_USB
            uidev.id.vendor = 0x1234;
            uidev.id.product = 0x5678;
            uidev.id.version = 1;

            uidev.absmin[ABS_X as usize] = 0;
            uidev.absmax[ABS_X as usize] = ABS_MAX_RANGE;
            uidev.absmin[ABS_Y as usize] = 0;
            uidev.absmax[ABS_Y as usize] = ABS_MAX_RANGE;

            let dev_bytes = std::slice::from_raw_parts(
                &uidev as *const _ as *const u8,
                std::mem::size_of::<UinputUserDev>(),
            );
            write(fd, dev_bytes.as_ptr() as *const _, dev_bytes.len());

            ioctl(fd, UI_DEV_CREATE);
        }

        Ok(Self {
            file: Mutex::new(Some(file)),
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }

    fn write_event(&self, ev_type: u16, code: u16, value: i32) {
        let lock = self.file.lock().unwrap();
        if let Some(ref file) = *lock {
            let ev = InputEventRaw {
                time: TimeVal {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                r#type: ev_type,
                code,
                value,
            };
            unsafe {
                write(
                    file.as_raw_fd(),
                    &ev as *const _ as *const _,
                    std::mem::size_of::<InputEventRaw>(),
                );
            }
        }
    }

    fn syn_report(&self) {
        self.write_event(EV_SYN, SYN_REPORT, 0);
    }
}

impl Drop for LinuxUinputInjector {
    fn drop(&mut self) {
        let mut lock = self.file.lock().unwrap();
        if let Some(file) = lock.take() {
            unsafe {
                ioctl(file.as_raw_fd(), UI_DEV_DESTROY);
            }
        }
    }
}

impl InputInjector for LinuxUinputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>> {
        match event {
            InputEvent::MouseMove { dx, dy } => {
                if dx != 0 {
                    self.write_event(EV_REL, REL_X, dx as i32);
                }
                if dy != 0 {
                    self.write_event(EV_REL, REL_Y, dy as i32);
                }
                self.syn_report();
            }
            InputEvent::MouseMoveAbsolute { x, y } => {
                let abs_x = ((x as i64 * ABS_MAX_RANGE as i64) / 65535) as i32;
                let abs_y = ((y as i64 * ABS_MAX_RANGE as i64) / 65535) as i32;
                self.write_event(EV_ABS, ABS_X, abs_x);
                self.write_event(EV_ABS, ABS_Y, abs_y);
                self.syn_report();
            }
            InputEvent::MouseDown(button) => {
                let btn_code = match button {
                    0 => BTN_LEFT,
                    1 => BTN_RIGHT,
                    2 => BTN_MIDDLE,
                    3 => BTN_SIDE,
                    4 => BTN_EXTRA,
                    _ => BTN_LEFT,
                };
                self.pressed_buttons
                    .fetch_or(1 << (button.min(7)), Ordering::Relaxed);
                self.write_event(EV_KEY, btn_code, 1);
                self.syn_report();
            }
            InputEvent::MouseUp(button) => {
                let btn_code = match button {
                    0 => BTN_LEFT,
                    1 => BTN_RIGHT,
                    2 => BTN_MIDDLE,
                    3 => BTN_SIDE,
                    4 => BTN_EXTRA,
                    _ => BTN_LEFT,
                };
                self.pressed_buttons
                    .fetch_and(!(1 << (button.min(7))), Ordering::Relaxed);
                self.write_event(EV_KEY, btn_code, 0);
                self.syn_report();
            }
            InputEvent::MouseScroll { delta_x, delta_y } => {
                if delta_y != 0 {
                    let steps = if delta_y > 0 { 1 } else { -1 };
                    self.write_event(EV_REL, REL_WHEEL, steps);
                }
                if delta_x != 0 {
                    let steps = if delta_x > 0 { 1 } else { -1 };
                    self.write_event(EV_REL, REL_HWHEEL, steps);
                }
                self.syn_report();
            }
            InputEvent::KeyDown(scancode) => {
                let linux_key = map_gpui_or_mac_to_linux_key(scancode as u16);
                self.pressed_keys.lock().unwrap().insert(linux_key);
                self.write_event(EV_KEY, linux_key, 1);
                self.syn_report();
            }
            InputEvent::KeyUp(scancode) => {
                let linux_key = map_gpui_or_mac_to_linux_key(scancode as u16);
                self.pressed_keys.lock().unwrap().remove(&linux_key);
                self.write_event(EV_KEY, linux_key, 0);
                self.syn_report();
            }
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                self.active_modifiers.store(modifiers, Ordering::Relaxed);
                let linux_key = map_gpui_or_mac_to_linux_key(key_code);
                if pressed {
                    self.pressed_keys.lock().unwrap().insert(linux_key);
                    self.write_event(EV_KEY, linux_key, 1);
                } else {
                    self.pressed_keys.lock().unwrap().remove(&linux_key);
                    self.write_event(EV_KEY, linux_key, 0);
                }
                self.syn_report();
            }
            InputEvent::ModifiersChanged(modifiers) => {
                self.active_modifiers.store(modifiers, Ordering::Relaxed);
            }
            InputEvent::Touch { .. } => {}
        }
        Ok(())
    }
}

/// Translate cross-platform virtual key code to standard Linux evdev key code (linux/input-event-codes.h)
fn map_gpui_or_mac_to_linux_key(code: u16) -> u16 {
    // If it's already an evdev keycode (< 255 and standard range), or macOS standard:
    match code {
        0x00 => 30,  // KEY_A
        0x01 => 31,  // KEY_S
        0x02 => 32,  // KEY_D
        0x03 => 33,  // KEY_F
        0x04 => 35,  // KEY_H
        0x05 => 34,  // KEY_G
        0x06 => 44,  // KEY_Z
        0x07 => 45,  // KEY_X
        0x08 => 46,  // KEY_C
        0x09 => 47,  // KEY_V
        0x0B => 48,  // KEY_B
        0x0C => 16,  // KEY_Q
        0x0D => 17,  // KEY_W
        0x0E => 18,  // KEY_E
        0x0F => 19,  // KEY_R
        0x10 => 21,  // KEY_Y
        0x11 => 20,  // KEY_T
        0x12 => 2,   // KEY_1
        0x13 => 3,   // KEY_2
        0x14 => 4,   // KEY_3
        0x15 => 5,   // KEY_4
        0x16 => 7,   // KEY_6
        0x17 => 6,   // KEY_5
        0x18 => 13,  // KEY_EQUAL
        0x19 => 10,  // KEY_9
        0x1A => 8,   // KEY_7
        0x1B => 12,  // KEY_MINUS
        0x1C => 9,   // KEY_8
        0x1D => 11,  // KEY_0
        0x1E => 27,  // KEY_RIGHTBRACE
        0x1F => 24,  // KEY_O
        0x20 => 22,  // KEY_U
        0x21 => 26,  // KEY_LEFTBRACE
        0x22 => 23,  // KEY_I
        0x23 => 25,  // KEY_P
        0x24 => 28,  // KEY_ENTER
        0x25 => 38,  // KEY_L
        0x26 => 36,  // KEY_J
        0x27 => 40,  // KEY_APOSTROPHE
        0x28 => 37,  // KEY_K
        0x29 => 39,  // KEY_SEMICOLON
        0x2A => 43,  // KEY_BACKSLASH
        0x2B => 51,  // KEY_COMMA
        0x2C => 53,  // KEY_SLASH
        0x2D => 49,  // KEY_N
        0x2E => 50,  // KEY_M
        0x2F => 52,  // KEY_DOT
        0x30 => 15,  // KEY_TAB
        0x31 => 57,  // KEY_SPACE
        0x32 => 41,  // KEY_GRAVE
        0x33 => 14,  // KEY_BACKSPACE
        0x35 => 1,   // KEY_ESC
        0x37 => 125, // KEY_LEFTMETA (Cmd/Super)
        0x38 => 42,  // KEY_LEFTSHIFT
        0x39 => 58,  // KEY_CAPSLOCK
        0x3A => 56,  // KEY_LEFTALT
        0x3B => 29,  // KEY_LEFTCTRL
        0x3C => 54,  // KEY_RIGHTSHIFT
        0x3D => 100, // KEY_RIGHTALT
        0x3E => 97,  // KEY_RIGHTCTRL
        0x7B => 105, // KEY_LEFT
        0x7C => 106, // KEY_RIGHT
        0x7D => 108, // KEY_DOWN
        0x7E => 103, // KEY_UP
        other if other < 255 => other,
        _ => 30,
    }
}
