//! Observation only: never sends a signal, terminates a process or changes ACLs.
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::{io, time::Duration};

pub(super) struct ExitObserver {
    #[cfg(unix)]
    fd: OwnedFd,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}
#[cfg(windows)]
unsafe impl Send for ExitObserver {}
impl ExitObserver {
    pub(super) fn open(pid: u32) -> io::Result<Self> {
        if pid == 0 || pid == std::process::id() {
            return Err(io::Error::other("Refuse self/zero background PID"));
        }
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }
        #[cfg(target_os = "macos")]
        {
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let event = libc::kevent {
                ident: pid as _,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            if unsafe {
                libc::kevent(
                    fd.as_raw_fd(),
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd })
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::*;
            let handle = unsafe {
                OpenProcess(
                    PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                )
            };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Err(io::Error::other(
                "Native process exit observation unavailable",
            ))
        }
    }
    pub(super) fn wait(self, timeout: Duration) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let mut poll = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result =
                unsafe { libc::poll(&mut poll, 1, timeout.as_millis().min(i32::MAX as _) as i32) };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if result == 0 || poll.revents & libc::POLLIN == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Background process exit not confirmed",
                ));
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let duration = libc::timespec {
                tv_sec: timeout.as_secs() as _,
                tv_nsec: timeout.subsec_nanos() as _,
            };
            let result = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &duration,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            if result == 0
                || event.flags & libc::EV_ERROR != 0
                || event.fflags & libc::NOTE_EXIT == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Background process exit not confirmed",
                ));
            }
            // A failed/crashed service exit may trigger its supervisor restart.
            if event.data != 0 {
                return Err(io::Error::other(
                    "Background process did not exit successfully",
                ));
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::{
                Foundation::WAIT_OBJECT_0,
                System::Threading::{GetExitCodeProcess, WaitForSingleObject},
            };
            if unsafe {
                WaitForSingleObject(self.handle, timeout.as_millis().min(u32::MAX as _) as u32)
            } != WAIT_OBJECT_0
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Background process exit not confirmed",
                ));
            }
            let mut status = 0;
            if unsafe { GetExitCodeProcess(self.handle, &mut status) } == 0 || status != 0 {
                return Err(io::Error::other(
                    "Background process did not exit successfully",
                ));
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = timeout;
            Err(io::Error::other(
                "Native process exit observation unavailable",
            ))
        }
    }
}
#[cfg(windows)]
impl Drop for ExitObserver {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}
