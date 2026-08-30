use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use std::ffi::c_void;

const IO_SUCCESS: i32 = 0;
const ASSERTION_LEVEL_ON: u32 = 255;
const USER_ACTIVE_REMOTE: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionDeclareUserActivity(
        assertion_name: *const c_void,
        user_type: u32,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

pub(crate) struct DisplayPowerGuard {
    assertion_ids: Vec<u32>,
}

impl DisplayPowerGuard {
    pub(crate) fn activate() -> Self {
        let assertion_name = CFString::new("RemotePlay active streaming session");
        let assertion_name_ref = assertion_name.as_concrete_TypeRef() as *const c_void;
        let mut assertion_ids = Vec::with_capacity(2);

        let mut user_activity_id = 0;
        let user_activity_status = unsafe {
            IOPMAssertionDeclareUserActivity(
                assertion_name_ref,
                USER_ACTIVE_REMOTE,
                &mut user_activity_id,
            )
        };
        if user_activity_status == IO_SUCCESS {
            assertion_ids.push(user_activity_id);
        } else {
            eprintln!(
                "Unable to wake the remote display for streaming: IOKit status {user_activity_status}"
            );
        }

        let assertion_type = CFString::new("PreventUserIdleDisplaySleep");
        let mut display_sleep_id = 0;
        let display_sleep_status = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef() as *const c_void,
                ASSERTION_LEVEL_ON,
                assertion_name_ref,
                &mut display_sleep_id,
            )
        };
        if display_sleep_status == IO_SUCCESS {
            if !assertion_ids.contains(&display_sleep_id) {
                assertion_ids.push(display_sleep_id);
            }
        } else {
            eprintln!(
                "Unable to keep the remote display awake while streaming: IOKit status {display_sleep_status}"
            );
        }

        Self { assertion_ids }
    }
}

impl Drop for DisplayPowerGuard {
    fn drop(&mut self) {
        for assertion_id in self.assertion_ids.drain(..).rev() {
            let status = unsafe { IOPMAssertionRelease(assertion_id) };
            if status != IO_SUCCESS {
                eprintln!(
                    "Unable to release RemotePlay display assertion {assertion_id}: IOKit status {status}"
                );
            }
        }
    }
}
