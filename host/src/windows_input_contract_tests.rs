//! Compile and exercise the actual Windows injector against a recording API.
//! This checks ownership/failure logic, not Windows ABI, desktop or permissions.
use protocol::{InputEvent, input_modifiers};
use remote_core::InputInjector;
use std::sync::Mutex;

mod actual {
    use super::recording_api as windows_sys;
    include!("windows_input.rs");
    pub(super) fn held_lock(
        input: &WindowsInputInjector,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeSet<u16>> {
        input.pressed_keys.lock().unwrap()
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Posted {
    Mouse(u32),
    Key(u16, bool),
}
#[derive(Default)]
struct Calls {
    attempts: usize,
    fail_next: bool,
    pause_first_until: Option<std::time::Instant>,
    posted: Vec<Posted>,
}
static CALLS: Mutex<Calls> = Mutex::new(Calls {
    attempts: 0,
    fail_next: false,
    pause_first_until: None,
    posted: Vec::new(),
});
static SERIAL: Mutex<()> = Mutex::new(());

#[allow(non_snake_case, non_camel_case_types, dead_code)]
mod recording_api {
    pub type VIRTUAL_KEY = u16;
    #[derive(Clone, Copy)]
    pub struct MOUSEINPUT {
        pub dx: i32,
        pub dy: i32,
        pub mouseData: u32,
        pub dwFlags: u32,
        pub time: u32,
        pub dwExtraInfo: usize,
    }
    #[derive(Clone, Copy)]
    pub struct KEYBDINPUT {
        pub wVk: u16,
        pub wScan: u16,
        pub dwFlags: u32,
        pub time: u32,
        pub dwExtraInfo: usize,
    }
    #[derive(Clone, Copy)]
    pub union INPUT_0 {
        pub mi: MOUSEINPUT,
        pub ki: KEYBDINPUT,
    }
    pub struct INPUT {
        pub r#type: u32,
        pub Anonymous: INPUT_0,
    }
    pub const INPUT_MOUSE: u32 = 0;
    pub const INPUT_KEYBOARD: u32 = 1;
    pub const KEYEVENTF_EXTENDEDKEY: u32 = 1;
    pub const KEYEVENTF_KEYUP: u32 = 2;
    pub const MOUSEEVENTF_MOVE: u32 = 1;
    pub const MOUSEEVENTF_LEFTDOWN: u32 = 2;
    pub const MOUSEEVENTF_LEFTUP: u32 = 4;
    pub const MOUSEEVENTF_RIGHTDOWN: u32 = 8;
    pub const MOUSEEVENTF_RIGHTUP: u32 = 16;
    pub const MOUSEEVENTF_MIDDLEDOWN: u32 = 32;
    pub const MOUSEEVENTF_MIDDLEUP: u32 = 64;
    pub const MOUSEEVENTF_WHEEL: u32 = 0x800;
    pub const MOUSEEVENTF_ABSOLUTE: u32 = 0x8000;
    pub unsafe fn SendInput(count: u32, inputs: *mut INPUT, _size: i32) -> u32 {
        let mut calls = super::CALLS.lock().unwrap();
        calls.attempts += 1;
        if calls.fail_next {
            calls.fail_next = false;
            return 0;
        }
        for i in 0..count {
            let input = unsafe { &*inputs.add(i as usize) };
            let posted = if input.r#type == INPUT_MOUSE {
                super::Posted::Mouse(unsafe { input.Anonymous.mi.dwFlags })
            } else {
                let key = unsafe { input.Anonymous.ki };
                super::Posted::Key(key.wVk, key.dwFlags & KEYEVENTF_KEYUP == 0)
            };
            calls.posted.push(posted);
        }
        if calls.attempts == 1 {
            if let Some(until) = calls.pause_first_until {
                std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
            }
        }
        count
    }
    pub mod Win32 {
        pub mod UI {
            pub mod Input {
                pub mod KeyboardAndMouse {
                    pub use super::super::super::super::*;
                }
            }
        }
    }
}

#[test]
fn cleanup_never_releases_unowned_buttons_or_replays_duplicate_downs() {
    let _serial = SERIAL.lock().unwrap();
    *CALLS.lock().unwrap() = Calls::default();
    let input = actual::WindowsInputInjector::new().unwrap();
    input.inject_input(InputEvent::MouseUp(1)).unwrap();
    input.inject_input(InputEvent::MouseDown(0)).unwrap();
    input.inject_input(InputEvent::MouseDown(0)).unwrap();
    input.release_all_input();
    input.release_all_input();
    let calls = CALLS.lock().unwrap();
    assert_eq!(calls.attempts, 2);
    assert_eq!(calls.posted, vec![Posted::Mouse(2), Posted::Mouse(4)]);
}
#[test]
fn failed_modifier_never_forwards_the_bare_printable_key() {
    let _serial = SERIAL.lock().unwrap();
    *CALLS.lock().unwrap() = Calls {
        fail_next: true,
        ..Default::default()
    };
    let input = actual::WindowsInputInjector::new().unwrap();
    assert!(
        input
            .inject_input(InputEvent::Key {
                key_code: 0,
                pressed: true,
                modifiers: input_modifiers::CONTROL
            })
            .is_err()
    );
    input.release_all_input();
    let calls = CALLS.lock().unwrap();
    assert_eq!(calls.attempts, 1);
    assert!(calls.posted.is_empty());
}
#[test]
fn rejected_cleanup_blocks_new_input_until_known_unaccepted_up_is_cleared() {
    let _serial = SERIAL.lock().unwrap();
    *CALLS.lock().unwrap() = Calls::default();
    let input = actual::WindowsInputInjector::new().unwrap();
    input
        .inject_input(InputEvent::Key {
            key_code: 0,
            pressed: true,
            modifiers: 0,
        })
        .unwrap();
    CALLS.lock().unwrap().fail_next = true;
    input.release_all_input();
    assert!(
        input
            .inject_input(InputEvent::Key {
                key_code: 11,
                pressed: true,
                modifiers: 0
            })
            .is_err()
    );
    assert_eq!(CALLS.lock().unwrap().attempts, 2);
    // Zero accepted events proves the previous up did not execute. This is
    // owner cleanup, distinct from replaying an unknown Portal RPC outcome.
    input.release_all_input();
    input
        .inject_input(InputEvent::Key {
            key_code: 11,
            pressed: true,
            modifiers: 0,
        })
        .unwrap();
    assert_eq!(
        CALLS.lock().unwrap().posted,
        vec![
            Posted::Key(0x41, true),
            Posted::Key(0x41, false),
            Posted::Key(0x42, true)
        ]
    );
}

#[test]
fn fresh_deadline_posts_and_cleans_only_the_accepted_key() {
    let _serial = SERIAL.lock().unwrap();
    *CALLS.lock().unwrap() = Calls::default();
    let input = actual::WindowsInputInjector::new().unwrap();
    input
        .inject_input_before(
            InputEvent::KeyDown(0x41),
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .unwrap();
    input.release_all_input();
    assert_eq!(
        CALLS.lock().unwrap().posted,
        vec![Posted::Key(0x41, true), Posted::Key(0x41, false)]
    );
}
#[test]
fn key_lock_wait_expiry_does_not_post_or_create_cleanup_up() {
    let _serial = SERIAL.lock().unwrap();
    *CALLS.lock().unwrap() = Calls::default();
    let input = std::sync::Arc::new(actual::WindowsInputInjector::new().unwrap());
    let lock = actual::held_lock(&input);
    let until = std::time::Instant::now() + std::time::Duration::from_millis(40);
    let (tx, rx) = std::sync::mpsc::channel();
    let other = input.clone();
    let worker = std::thread::spawn(move || {
        tx.send(()).unwrap();
        other
            .inject_input_before(InputEvent::KeyDown(0x41), until)
            .is_err()
    });
    rx.recv().unwrap();
    std::thread::sleep(
        until.saturating_duration_since(std::time::Instant::now())
            + std::time::Duration::from_millis(2),
    );
    drop(lock);
    assert!(worker.join().unwrap());
    input.release_all_input();
    assert_eq!(CALLS.lock().unwrap().attempts, 0);
}
#[test]
fn batch_expiry_after_first_modifier_does_not_post_remaining_modifier_or_key() {
    let _serial = SERIAL.lock().unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(40);
    *CALLS.lock().unwrap() = Calls {
        pause_first_until: Some(until + std::time::Duration::from_millis(2)),
        ..Default::default()
    };
    let input = actual::WindowsInputInjector::new().unwrap();
    assert!(
        input
            .inject_input_before(
                InputEvent::Key {
                    key_code: 0,
                    pressed: true,
                    modifiers: input_modifiers::SHIFT | input_modifiers::CONTROL
                },
                until
            )
            .is_err()
    );
    assert_eq!(CALLS.lock().unwrap().posted, vec![Posted::Key(0xA0, true)]);
    input.release_all_input();
    assert_eq!(
        CALLS.lock().unwrap().posted,
        vec![Posted::Key(0xA0, true), Posted::Key(0xA0, false)]
    );
}
