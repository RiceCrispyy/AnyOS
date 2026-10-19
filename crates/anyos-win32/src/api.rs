//! Rust implementations of Windows APIs, looked up by DLL and name when the
//! guest reaches a call-gate stub.
//!
//! Each entry records its argument count, which the dispatcher needs to read
//! arguments and, for 32-bit stdcall, to pop them on return.

use std::io::Write;

use anyos_pe::ImportName;

use crate::module::*;
use crate::thread::{Halt, Thread, PROCESS_ID, THREAD_ID};

type ApiFn = fn(&mut Thread<'_, '_>, &[u64]) -> Result<u64, Halt>;

pub(crate) struct Api {
    pub dll: &'static str,
    pub name: &'static str,
    pub args: u8,
    pub f: ApiFn,
}

const fn api(dll: &'static str, name: &'static str, args: u8, f: ApiFn) -> Api {
    Api { dll, name, args, f }
}

static APIS: &[Api] = &[
    api("kernel32", "ExitProcess", 1, exit_process),
    api("kernel32", "GetStdHandle", 1, get_std_handle),
    api("kernel32", "WriteFile", 5, write_file),
    api("kernel32", "WriteConsoleA", 5, write_console_a),
    api("kernel32", "WriteConsoleW", 5, write_console_w),
    api("kernel32", "GetLastError", 0, get_last_error),
    api("kernel32", "SetLastError", 1, set_last_error),
    api("kernel32", "GetCurrentProcess", 0, |_, _| {
        Ok(PSEUDO_PROCESS)
    }),
    api("kernel32", "GetCurrentThread", 0, |_, _| Ok(PSEUDO_THREAD)),
    api("kernel32", "GetCurrentProcessId", 0, |_, _| Ok(PROCESS_ID)),
    api("kernel32", "GetCurrentThreadId", 0, |_, _| Ok(THREAD_ID)),
    api("kernel32", "GetCommandLineA", 0, |t, _| Ok(t.cmdline_a)),
    api("kernel32", "GetCommandLineW", 0, |t, _| Ok(t.cmdline_w)),
    api("kernel32", "GetModuleHandleA", 1, get_module_handle_a),
    api("kernel32", "GetModuleHandleW", 1, get_module_handle_w),
    api("kernel32", "GetModuleHandleExA", 3, get_module_handle_ex_a),
    api("kernel32", "GetModuleHandleExW", 3, get_module_handle_ex_w),
    api("kernel32", "GetModuleFileNameA", 3, get_module_file_name_a),
    api("kernel32", "GetModuleFileNameW", 3, get_module_file_name_w),
    api("kernel32", "LoadLibraryA", 1, load_library_a),
    api("kernel32", "LoadLibraryW", 1, load_library_w),
    api("kernel32", "LoadLibraryExA", 3, load_library_ex_a),
    api("kernel32", "LoadLibraryExW", 3, load_library_ex_w),
    api("kernel32", "FreeLibrary", 1, free_library),
    api("kernel32", "GetProcAddress", 2, get_proc_address),
    api("kernel32", "SetErrorMode", 1, set_error_mode),
    api(
        "kernel32",
        "GetErrorMode",
        0,
        |t, _| Ok(t.error_mode as u64),
    ),
    api("kernel32", "GetVersion", 0, |_, _| Ok(get_version())),
    api("kernel32", "GetVersionExA", 1, get_version_ex_a),
    api("kernel32", "GetVersionExW", 1, get_version_ex_w),
    // DLLs come from the program's directory or AnyOS itself, so DLL search
    // settings are accepted and have nothing to change.
    api("kernel32", "SetDefaultDllDirectories", 1, |_, _| Ok(1)),
    api("kernel32", "SetDllDirectoryA", 1, |_, _| Ok(1)),
    api("kernel32", "SetDllDirectoryW", 1, |_, _| Ok(1)),
    api("kernel32", "GetSystemDirectoryA", 2, |t, a| {
        Ok(copy_str_out(t, a[0], a[1], SYSTEM_DIR, false))
    }),
    api("kernel32", "GetSystemDirectoryW", 2, |t, a| {
        Ok(copy_str_out(t, a[0], a[1], SYSTEM_DIR, true))
    }),
    api("kernel32", "GetWindowsDirectoryA", 2, |t, a| {
        Ok(copy_str_out(t, a[0], a[1], WINDOWS_DIR, false))
    }),
    api("kernel32", "GetWindowsDirectoryW", 2, |t, a| {
        Ok(copy_str_out(t, a[0], a[1], WINDOWS_DIR, true))
    }),
];

const WINDOWS_DIR: &str = r"C:\Windows";
const SYSTEM_DIR: &str = r"C:\Windows\System32";

/// Copy `s` into a guest buffer of `size` characters the way most string
/// APIs do: on success return its length, if it does not fit return the
/// size needed including the terminator and write nothing.
fn copy_str_out(t: &mut Thread<'_, '_>, buf: u64, size: u64, s: &str, wide: bool) -> u64 {
    let units: Vec<u16> = if wide {
        s.encode_utf16().collect()
    } else {
        s.bytes().map(u16::from).collect()
    };
    let len = units.len() as u64;
    if (size as u32 as u64) <= len {
        return len + 1;
    }
    let mut out = Vec::new();
    for &u in units.iter().chain([&0]) {
        if wide {
            out.extend_from_slice(&u.to_le_bytes());
        } else {
            out.push(u as u8);
        }
    }
    if t.process.memory.write(buf, &out).is_err() {
        t.set_last_error(ERROR_NOACCESS);
        return 0;
    }
    len
}

/// The implementation of `dll!symbol`, if there is one.
pub(crate) fn find(dll: &str, symbol: &ImportName) -> Option<&'static Api> {
    let ImportName::Name { name, .. } = symbol else {
        return None;
    };
    let dll = dll.to_ascii_lowercase();
    let dll = dll.strip_suffix(".dll").unwrap_or(&dll);
    APIS.iter().find(|a| a.dll == dll && a.name == name)
}

// Handle values. Real Windows hands out multiples of 4; these never collide
// with the pseudo handles (-1, -2) or INVALID_HANDLE_VALUE.
const STDIN_HANDLE: u64 = 0x10;
const STDOUT_HANDLE: u64 = 0x14;
const STDERR_HANDLE: u64 = 0x18;
const PSEUDO_PROCESS: u64 = u64::MAX;
const PSEUDO_THREAD: u64 = u64::MAX - 1;

pub(crate) const ERROR_INVALID_HANDLE: u32 = 6;
pub(crate) const ERROR_MOD_NOT_FOUND: u32 = 126;
pub(crate) const ERROR_NOACCESS: u32 = 998;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

fn invalid_handle(t: &Thread<'_, '_>) -> u64 {
    if t.long_mode() {
        u64::MAX
    } else {
        0xFFFF_FFFF
    }
}

fn exit_process(_: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Err(Halt::Exit(a[0] as u32))
}

fn get_std_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Ok(match a[0] as u32 as i32 {
        -10 => STDIN_HANDLE,
        -11 => STDOUT_HANDLE,
        -12 => STDERR_HANDLE,
        _ => {
            t.set_last_error(ERROR_INVALID_HANDLE);
            invalid_handle(t)
        }
    })
}

fn get_last_error(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    Ok(t.last_error() as u64)
}

fn set_last_error(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    t.set_last_error(a[0] as u32);
    Ok(0)
}

/// Read `len` guest bytes with the guest's own access rights.
fn guest_bytes(t: &Thread<'_, '_>, addr: u64, len: u64) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; usize::try_from(len).ok()?];
    t.process.memory.read(addr, &mut buf).ok()?;
    Some(buf)
}

/// Store a DWORD result through a guest pointer, if the pointer is non-null.
fn store_dword(t: &mut Thread<'_, '_>, ptr: u64, v: u32) -> bool {
    ptr == 0 || t.process.memory.write(ptr, &v.to_le_bytes()).is_ok()
}

fn sink<'t, 'a>(t: &'t mut Thread<'a, '_>, handle: u64) -> Option<&'t mut dyn Write> {
    match handle & 0xFFFF_FFFF {
        STDOUT_HANDLE => Some(&mut *t.stdio.stdout),
        STDERR_HANDLE => Some(&mut *t.stdio.stderr),
        _ => None,
    }
}

/// Shared by WriteFile and WriteConsoleA/W: write bytes to a console handle.
fn write_out(t: &mut Thread<'_, '_>, handle: u64, data: &[u8], written: u64, units: u32) -> u64 {
    let Some(out) = sink(t, handle) else {
        t.set_last_error(ERROR_INVALID_HANDLE);
        return 0;
    };
    let ok = out.write_all(data).and_then(|_| out.flush()).is_ok();
    if !ok || !store_dword(t, written, units) {
        t.set_last_error(ERROR_NOACCESS);
        return 0;
    }
    1
}

fn write_file(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (handle, buf, len, written) = (a[0], a[1], a[2] as u32, a[3]);
    let Some(data) = guest_bytes(t, buf, len as u64) else {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    };
    Ok(write_out(t, handle, &data, written, len))
}

fn write_console_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    write_file(t, a)
}

fn write_console_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (handle, buf, chars, written) = (a[0], a[1], a[2] as u32, a[3]);
    let Some(raw) = guest_bytes(t, buf, chars as u64 * 2) else {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    };
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    let text = String::from_utf16_lossy(&units);
    Ok(write_out(t, handle, text.as_bytes(), written, chars))
}

pub(crate) fn read_ansi(t: &Thread<'_, '_>, addr: u64) -> Option<String> {
    let b = t.process.memory.read_cstr(addr, 32768).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

pub(crate) fn read_wide(t: &Thread<'_, '_>, addr: u64) -> Option<String> {
    let mut units = Vec::new();
    let mut b = [0u8; 2];
    for k in 0..32768u64 {
        t.process.memory.read(addr + k * 2, &mut b).ok()?;
        match u16::from_le_bytes(b) {
            0 => return Some(String::from_utf16_lossy(&units)),
            u => units.push(u),
        }
    }
    None
}

fn set_error_mode(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let old = t.error_mode;
    t.error_mode = a[0] as u32;
    Ok(old as u64)
}

/// The Windows version AnyOS reports: Windows 10 22H2.
const VERSION: (u32, u32, u32) = (10, 0, 19045);
const VER_PLATFORM_WIN32_NT: u32 = 2;
const VER_NT_WORKSTATION: u8 = 1;

/// Major in the low byte, minor in the next, build in the high word.
fn get_version() -> u64 {
    let (major, minor, build) = VERSION;
    (build << 16 | minor << 8 | major) as u64
}

/// Fill an OSVERSIONINFO(EX)A/W, whose first DWORD says which one it is.
fn get_version_ex(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<u64, Halt> {
    let csd = if wide { 256 } else { 128 };
    let base = 20 + csd;
    let Some(size) = guest_bytes(t, ptr, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    else {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    };
    let ex = if size as u64 == base {
        false
    } else if size as u64 == base + 8 {
        true
    } else {
        t.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    };
    let (major, minor, build) = VERSION;
    let mut info = Vec::with_capacity(size as usize);
    for v in [size, major, minor, build, VER_PLATFORM_WIN32_NT] {
        info.extend_from_slice(&v.to_le_bytes());
    }
    info.resize(base as usize, 0); // empty service pack string
    if ex {
        // Service pack 0.0, no suite flags, a workstation.
        info.extend_from_slice(&[0, 0, 0, 0, 0, 0, VER_NT_WORKSTATION, 0]);
    }
    if t.process.memory.write(ptr, &info).is_err() {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    }
    Ok(1)
}

fn get_version_ex_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_version_ex(t, a[0], false)
}

fn get_version_ex_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_version_ex(t, a[0], true)
}
