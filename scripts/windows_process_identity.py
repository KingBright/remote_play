"""Read a Windows process's dedicated immutable .rpbuild identity section.

Only the reviewed public build record in the main image is read. No heap,
profile, credentials, privilege enabling, input injection or service management.
Native execution remains a separate Windows acceptance requirement.
"""
from __future__ import annotations
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import struct
import sys


def verify_identity_section(read_at, expected_record: bytes) -> dict:
    """Parse only bounded PE headers and the immutable public identity section."""
    def require(ok):
        if not ok:
            raise RuntimeError('Invalid or missing immutable PE build identity')
    require(type(expected_record) is bytes and 0 < len(expected_record) <= 65536)
    dos = read_at(0, 64); require(len(dos) == 64 and dos[:2] == b'MZ')
    offset = struct.unpack_from('<I', dos, 60)[0]; require(64 <= offset <= 65536 - 24)
    pe = read_at(offset, 24); require(len(pe) == 24 and pe[:4] == b'PE\0\0')
    machine = struct.unpack_from('<H', pe, 4)[0]
    sections = struct.unpack_from('<H', pe, 6)[0]
    optional_size = struct.unpack_from('<H', pe, 20)[0]
    require(machine in (0x8664, 0xaa64) and 0 < sections <= 96 and 64 <= optional_size <= 4096)
    optional = read_at(offset + 24, optional_size); require(len(optional) == optional_size)
    require(struct.unpack_from('<H', optional, 0)[0] == 0x20b)
    image_size, headers_size = struct.unpack_from('<II', optional, 56)
    headers_offset = offset + 24 + optional_size
    require(headers_offset + sections * 40 <= headers_size <= 65536 and headers_size < image_size <= 1024 ** 3)
    headers = read_at(headers_offset, sections * 40); require(len(headers) == sections * 40)
    selected = []
    for n in range(sections):
        record = headers[n * 40:(n + 1) * 40]
        if record[:8] != b'.rpbuild': continue
        size, address = struct.unpack_from('<II', record, 8)
        flags = struct.unpack_from('<I', record, 36)[0]
        require(flags & 0x40000000 and not flags & (0x80000000 | 0x20000000))
        require(len(expected_record) <= size <= 65536 and
                headers_size <= address and address + size <= image_size)
        selected.append((address, size))
    require(len(selected) == 1)
    address, size = selected[0]
    content = read_at(address, size); require(len(content) == size)
    require(content[:len(expected_record)] == expected_record and
            not content[len(expected_record):].strip(b'\0'))
    return {'identity_section_rva': address, 'identity_section_bytes': size}

def observe(pid: int, executable: Path, expected_record: bytes) -> dict:
    if sys.platform != 'win32' or type(pid) is not int or pid <= 0:
        raise RuntimeError('Native Windows process observation unavailable')
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    psapi = ctypes.WinDLL('psapi', use_last_error=True)
    advapi = ctypes.WinDLL('advapi32', use_last_error=True)
    def bind(lib, name, args, result):
        f = getattr(lib, name); f.argtypes = args; f.restype = result
        return f
    open_process = bind(kernel, 'OpenProcess', [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE)
    close = bind(kernel, 'CloseHandle', [wintypes.HANDLE], wintypes.BOOL)
    image_name = bind(kernel, 'QueryFullProcessImageNameW', [wintypes.HANDLE, wintypes.DWORD,
                                                          wintypes.LPWSTR, ctypes.POINTER(wintypes.DWORD)], wintypes.BOOL)
    times = bind(kernel, 'GetProcessTimes', [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4, wintypes.BOOL)
    modules = bind(psapi, 'EnumProcessModules', [wintypes.HANDLE, ctypes.POINTER(wintypes.HMODULE),
                                               wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)], wintypes.BOOL)
    read_memory = bind(kernel, 'ReadProcessMemory', [wintypes.HANDLE, ctypes.c_void_p, ctypes.c_void_p,
                                                    ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)], wintypes.BOOL)
    process_token = bind(advapi, 'OpenProcessToken', [wintypes.HANDLE, wintypes.DWORD,
                                                    ctypes.POINTER(wintypes.HANDLE)], wintypes.BOOL)
    token_info = bind(advapi, 'GetTokenInformation', [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                                    wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)], wintypes.BOOL)
    equal_sid = bind(advapi, 'EqualSid', [ctypes.c_void_p, ctypes.c_void_p], wintypes.BOOL)
    current = bind(kernel, 'GetCurrentProcess', [], wintypes.HANDLE)
    def require(ok):
        if not ok: raise RuntimeError('Required native Windows evidence unavailable; no fallback')
    def owner_sid(handle):
        token = wintypes.HANDLE(); require(process_token(handle, 0x0008, ctypes.byref(token)))
        try:
            needed = wintypes.DWORD()
            token_info(token, 1, None, 0, ctypes.byref(needed))
            require(0 < needed.value <= 65536)
            buffer = ctypes.create_string_buffer(needed.value)
            require(token_info(token, 1, buffer, needed.value, ctypes.byref(needed)))
            return buffer, ctypes.cast(buffer, ctypes.POINTER(ctypes.c_void_p))[0]
        finally:
            close(token)
    def creation(handle):
        values = [wintypes.FILETIME() for _ in range(4)]
        require(times(handle, *[ctypes.byref(v) for v in values]))
        return (values[0].dwHighDateTime << 32) | values[0].dwLowDateTime
    def path(handle):
        buffer = ctypes.create_unicode_buffer(32768); length = wintypes.DWORD(len(buffer))
        require(image_name(handle, 0, buffer, ctypes.byref(length)))
        return os.path.normcase(buffer.value)
    def read(handle, address, count):
        require(0 < count <= 65536)
        buffer = ctypes.create_string_buffer(count); actual = ctypes.c_size_t()
        require(read_memory(handle, address, buffer, count, ctypes.byref(actual)) and actual.value == count)
        return buffer.raw
    # Ordinary read rights only. Never enable SeDebugPrivilege or retry after denial.
    handle = open_process(0x0400 | 0x0010, False, pid)
    require(handle)
    try:
        self_buffer, self_sid = owner_sid(current())
        remote_buffer, remote_sid = owner_sid(handle)
        require(equal_sid(self_sid, remote_sid))
        before = creation(handle)
        expected_path = os.path.normcase(str(executable))
        require(path(handle) == expected_path)
        array = (wintypes.HMODULE * 1024)(); needed = wintypes.DWORD()
        require(modules(handle, array, ctypes.sizeof(array), ctypes.byref(needed)) and
                ctypes.sizeof(wintypes.HMODULE) <= needed.value <= ctypes.sizeof(array))
        base = array[0]
        verify_identity_section(lambda address, count: read(handle, base + address, count), expected_record)
        require(creation(handle) == before and path(handle) == expected_path)
        return {'actual_compiled_identity_section_verified': True, 'pid_reuse_excluded': True,
                'process_path_verified': True, 'release_authorized': False}
    finally:
        close(handle)
