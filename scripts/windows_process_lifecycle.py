"""Stop only a verified Windows process while retaining its native handle.

No privilege enabling, name-based termination, policy changes or retry fallback.
The caller must already hold authorization to stop this exact application.
"""
import ctypes
from ctypes import wintypes
import os


def stop_verified(instance, pid, executable, binary, expected=None, kernel=None):
    """Hold the exact ordinary-user process handle through verify/stop/wait."""
    k = kernel or ctypes.WinDLL('kernel32', use_last_error=True)
    def bind(name, args, result):
        f = getattr(k, name)
        f.argtypes = args
        f.restype = result
        return f
    opened = bind('OpenProcess', [wintypes.DWORD,wintypes.BOOL,wintypes.DWORD], wintypes.HANDLE)
    image = bind('QueryFullProcessImageNameW', [wintypes.HANDLE,wintypes.DWORD,wintypes.LPWSTR,ctypes.POINTER(wintypes.DWORD)], wintypes.BOOL)
    terminate = bind('TerminateProcess', [wintypes.HANDLE,wintypes.UINT], wintypes.BOOL)
    wait = bind('WaitForSingleObject', [wintypes.HANDLE,wintypes.DWORD], wintypes.DWORD)
    close = bind('CloseHandle', [wintypes.HANDLE], wintypes.BOOL)
    h = opened(0x101001, False, pid)  # terminate + synchronize + limited query; no privilege enabling.
    if not h:
        raise RuntimeError('Ordinary process handle unavailable')
    try:
        buffer = ctypes.create_unicode_buffer(32768)
        size = wintypes.DWORD(len(buffer))
        if not image(h,0,buffer,ctypes.byref(size)) or os.path.normcase(buffer.value) != os.path.normcase(str(executable)):
            raise RuntimeError('Held process handle belongs to another image')
        instance.native(pid, executable, binary, expected)
        if not terminate(h,0) or wait(h,15000) != 0:
            raise RuntimeError('Original process termination not confirmed')
    finally:
        close(h)
