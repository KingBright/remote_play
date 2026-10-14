use protocol::InputEvent;
use remote_core::InputInjector;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::raw::{c_char, c_int, c_ulong};
use std::sync::Mutex;
use std::time::Instant;
type InputResult = Result<(), Box<dyn Error + Send + Sync>>;
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

#[derive(Clone, Copy)]
#[repr(C)]
struct TimeVal {
    tv_sec: c_ulong,
    tv_usec: c_ulong,
}

#[derive(Clone, Copy)]
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
    #[cfg(test)]
    record: Option<std::sync::Arc<dyn Fn(&[InputEventRaw]) -> isize + Send + Sync>>,
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
            #[cfg(test)]
            record: None,
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
        })
    }

    fn write_event(
        &self,
        ev_type: u16,
        code: u16,
        value: i32,
        deadline: Option<Instant>,
    ) -> InputResult {
        self.write_events(&[(ev_type, code, value)], deadline)
    }
    fn write_events(&self, batch: &[(u16, u16, i32)], deadline: Option<Instant>) -> InputResult {
        let mut file = self.file.lock().unwrap();
        let mut events = batch
            .iter()
            .map(|&(r#type, code, value)| InputEventRaw {
                time: TimeVal {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                r#type,
                code,
                value,
            })
            .collect::<Vec<_>>();
        events.push(InputEventRaw {
            time: TimeVal {
                tv_sec: 0,
                tv_usec: 0,
            },
            r#type: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });
        // Keep each logical event (including both pointer axes) and its report
        // in one kernel call. No later expired event can flush a partial batch.
        let size = std::mem::size_of_val(events.as_slice());
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err("input expired before uinput write".into());
        }
        #[cfg(test)]
        if let Some(record) = &self.record {
            return if record(&events) == size as isize {
                Ok(())
            } else {
                Err("recording uinput write failed".into())
            };
        }
        let device = file.as_ref().ok_or("uinput device unavailable")?;
        let written = unsafe { write(device.as_raw_fd(), events.as_ptr() as *const _, size) };
        if written != size as isize {
            // A partial/failed write is not safe to replay or flush later.
            // Destroy only this injector's virtual device and fail closed.
            if let Some(device) = file.take() {
                unsafe {
                    ioctl(device.as_raw_fd(), UI_DEV_DESTROY);
                }
            }
            return Err("uinput write unconfirmed; owned virtual device retired".into());
        }
        Ok(())
    }

    fn key(&self, key: u16, down: bool, deadline: Option<Instant>) -> InputResult {
        let mut held = self.pressed_keys.lock().unwrap();
        if held.contains(&key) == down {
            return Ok(());
        }
        self.write_event(EV_KEY, key, i32::from(down), deadline)?;
        if down {
            held.insert(key);
        } else {
            held.remove(&key);
        }
        // Native ownership is recorded only after the corresponding write.
        let modifiers = held.iter().fold(0, |mask, key| mask | match key {
            42 | 54 => 1, 29 | 97 => 2, 56 | 100 => 4, 125 | 126 => 8, _ => 0,
        });
        self.active_modifiers.store(modifiers, Ordering::Relaxed);
        Ok(())
    }

    fn set_modifiers(&self, modifiers: u8, deadline: Option<Instant>) -> InputResult {
        for (bit, key) in [(1, 42), (2, 29), (4, 56), (8, 125)] {
            let down = modifiers & bit != 0;
            if (self.active_modifiers.load(Ordering::Relaxed) & bit != 0) != down {
                self.key(key, down, deadline)?;
            }
        }
        Ok(())
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

impl LinuxUinputInjector {
    fn inject_before(&self, event: InputEvent, deadline: Option<Instant>) -> InputResult {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err("input expired before uinput dispatch".into());
        }
        match event {
            InputEvent::MouseMove { dx, dy } => {
                let mut batch = Vec::new();
                if dx != 0 {
                    batch.push((EV_REL, REL_X, dx as i32));
                }
                if dy != 0 {
                    batch.push((EV_REL, REL_Y, dy as i32));
                }
                self.write_events(&batch, deadline)?;
            }
            InputEvent::MouseMoveAbsolute { x, y } => {
                self.write_events(
                    &[(EV_ABS, ABS_X, i32::from(x)), (EV_ABS, ABS_Y, i32::from(y))],
                    deadline,
                )?;
            }
            InputEvent::MouseDown(button) | InputEvent::MouseUp(button) => {
                let down = matches!(event, InputEvent::MouseDown(_));
                let key = match button {
                    0 => BTN_LEFT,
                    1 => BTN_RIGHT,
                    2 => BTN_MIDDLE,
                    3 => BTN_SIDE,
                    4 => BTN_EXTRA,
                    _ => return Err("unsupported mouse button".into()),
                };
                let mask = 1 << button;
                if (self.pressed_buttons.load(Ordering::Relaxed) & mask != 0) != down {
                    self.write_event(EV_KEY, key, i32::from(down), deadline)?;
                    if down {
                        self.pressed_buttons.fetch_or(mask, Ordering::Relaxed);
                    } else {
                        self.pressed_buttons.fetch_and(!mask, Ordering::Relaxed);
                    }
                }
            }
            InputEvent::MouseScroll { delta_x, delta_y } => {
                let mut batch = Vec::new();
                if delta_y != 0 {
                    batch.push((EV_REL, REL_WHEEL, delta_y.signum()));
                }
                if delta_x != 0 {
                    batch.push((EV_REL, REL_HWHEEL, delta_x.signum()));
                }
                self.write_events(&batch, deadline)?;
            }
            InputEvent::KeyDown(code) | InputEvent::KeyUp(code) => {
                let down = matches!(event, InputEvent::KeyDown(_));
                let key = map_gpui_or_mac_to_linux_key(
                    u16::try_from(code).map_err(|_| "unsupported key code")?,
                )
                .ok_or("unsupported key code")?;
                self.key(key, down, deadline)?;
            }
            InputEvent::Key {
                key_code,
                pressed,
                modifiers,
            } => {
                let key = map_gpui_or_mac_to_linux_key(key_code).ok_or("unsupported key code")?;
                if pressed {
                    self.set_modifiers(modifiers, deadline)?;
                }
                self.key(key, pressed, deadline)?;
                if !pressed {
                    // A release may remove owned modifiers, never introduce one.
                    self.set_modifiers(modifiers & self.active_modifiers.load(Ordering::Relaxed), deadline)?;
                }
            }
            InputEvent::ModifiersChanged(modifiers) => {
                self.set_modifiers(modifiers, deadline)?;
            }
            InputEvent::Touch { .. } => {}
        }
        Ok(())
    }
}
impl InputInjector for LinuxUinputInjector {
    fn inject_input(&self, event: InputEvent) -> InputResult {
        self.inject_before(event, None)
    }
    fn inject_input_before(&self, event: InputEvent, deadline: Instant) -> InputResult {
        self.inject_before(event, Some(deadline))
    }
    fn release_all_input(&self) {
        let keys = std::mem::take(&mut *self.pressed_keys.lock().unwrap());
        for key in keys {
            let _ = self.write_event(EV_KEY, key, 0, None);
        }
        let buttons = self.pressed_buttons.swap(0, Ordering::Relaxed);
        for (button, key) in [
            (0, BTN_LEFT),
            (1, BTN_RIGHT),
            (2, BTN_MIDDLE),
            (3, BTN_SIDE),
            (4, BTN_EXTRA),
        ] {
            if buttons & (1 << button) != 0 {
                let _ = self.write_event(EV_KEY, key, 0, None);
            }
        }
        self.active_modifiers.store(0, Ordering::Relaxed);
    }
}

/// Translate cross-platform virtual key code to standard Linux evdev key code (linux/input-event-codes.h)
fn map_gpui_or_mac_to_linux_key(code: u16) -> Option<u16> {
    // Unknown codes never become guessed printable keys.
    Some(match code {
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
        _ => return None,
    })
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::sync::Arc;
    fn fixture(
        until: Option<Instant>,
    ) -> (Arc<LinuxUinputInjector>, Arc<Mutex<Vec<(u16, u16, i32)>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = calls.clone();
        let input = LinuxUinputInjector {
            file: Mutex::new(None),
            pressed_buttons: AtomicU8::new(0),
            active_modifiers: AtomicU8::new(0),
            pressed_keys: Mutex::new(BTreeSet::new()),
            record: Some(Arc::new(move |events| {
                let mut calls = sink.lock().unwrap();
                let first = calls.is_empty();
                calls.extend(events.iter().map(|e| (e.r#type, e.code, e.value)));
                if first {
                    if let Some(until) = until {
                        std::thread::sleep(until.saturating_duration_since(Instant::now()));
                    }
                }
                std::mem::size_of_val(events) as isize
            })),
        };
        (Arc::new(input), calls)
    }
    #[test]
    fn modifier_chord_posts_control_before_c_and_cleans_accepted_keys() {
        let (input, calls) = fixture(None);
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        input.inject_input_before(InputEvent::ModifiersChanged(2), deadline).unwrap();
        input.inject_input_before(InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 2,
        }, deadline).unwrap();
        input.release_all_input();
        assert_eq!(*calls.lock().unwrap(), vec![
            (EV_KEY, 29, 1), (EV_SYN, 0, 0), (EV_KEY, 46, 1), (EV_SYN, 0, 0),
            (EV_KEY, 29, 0), (EV_SYN, 0, 0), (EV_KEY, 46, 0), (EV_SYN, 0, 0),
        ]);
    }

    #[test]
    fn modifier_chord_expiry_after_control_never_posts_bare_c() {
        let until = Instant::now() + std::time::Duration::from_millis(40);
        let (input, calls) = fixture(Some(until + std::time::Duration::from_millis(2)));
        assert!(input.inject_input_before(InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 2,
        }, until).is_err());
        input.release_all_input();
        assert_eq!(*calls.lock().unwrap(), vec![
            (EV_KEY, 29, 1), (EV_SYN, 0, 0), (EV_KEY, 29, 0), (EV_SYN, 0, 0),
        ]);
    }

    #[test]
    fn modifier_chord_failed_control_never_posts_c_or_synthetic_cleanup() {
        let (input, calls) = fixture(None);
        let mut input = Arc::try_unwrap(input).ok().unwrap();
        let sink = calls.clone();
        input.record = Some(Arc::new(move |events| {
            sink.lock().unwrap().extend(events.iter().map(|e| (e.r#type, e.code, e.value)));
            -1
        }));
        assert!(input.inject_input_before(InputEvent::Key {
            key_code: 0x08, pressed: true, modifiers: 2,
        }, Instant::now() + std::time::Duration::from_secs(1)).is_err());
        let before = calls.lock().unwrap().clone();
        assert_eq!(before, vec![(EV_KEY, 29, 1), (EV_SYN, 0, 0)]);
        input.release_all_input();
        assert_eq!(*calls.lock().unwrap(), before);
    }

    #[test]
    fn fresh_deadline_posts_and_cleans_only_the_accepted_key() {
        let (input, calls) = fixture(None);
        input
            .inject_input_before(
                InputEvent::KeyDown(0),
                Instant::now() + std::time::Duration::from_secs(1),
            )
            .unwrap();
        input.release_all_input();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (EV_KEY, 30, 1),
                (EV_SYN, 0, 0),
                (EV_KEY, 30, 0),
                (EV_SYN, 0, 0)
            ]
        );
    }
    #[test]
    fn file_lock_wait_expiry_does_not_post_or_create_cleanup_up() {
        let (input, calls) = fixture(None);
        let held = input.file.lock().unwrap();
        let until = Instant::now() + std::time::Duration::from_millis(40);
        let (tx, rx) = std::sync::mpsc::channel();
        let other = input.clone();
        let worker = std::thread::spawn(move || {
            tx.send(()).unwrap();
            other
                .inject_input_before(InputEvent::MouseDown(0), until)
                .is_err()
        });
        rx.recv().unwrap();
        std::thread::sleep(
            until.saturating_duration_since(Instant::now()) + std::time::Duration::from_millis(2),
        );
        drop(held);
        assert!(worker.join().unwrap());
        input.release_all_input();
        assert!(calls.lock().unwrap().is_empty());
    }
    #[test]
    fn batch_expiry_only_cleans_first_accepted_press() {
        let until = Instant::now() + std::time::Duration::from_millis(40);
        let (input, calls) = fixture(Some(until + std::time::Duration::from_millis(2)));
        input
            .inject_input_before(InputEvent::KeyDown(0), until)
            .unwrap();
        assert!(
            input
                .inject_input_before(InputEvent::KeyDown(11), until)
                .is_err()
        );
        input.release_all_input();
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (EV_KEY, 30, 1),
                (EV_SYN, 0, 0),
                (EV_KEY, 30, 0),
                (EV_SYN, 0, 0)
            ]
        );
    }
}
