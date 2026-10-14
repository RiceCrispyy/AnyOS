//! Rust implementations of Windows APIs, looked up by DLL and name when the
//! guest reaches a call-gate stub.
//!
//! Each entry records its argument count, which the dispatcher needs to read
//! arguments and, for 32-bit stdcall, to pop them on return.

use std::io::Write;

use anyos_pe::ImportName;

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
];

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

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_MOD_NOT_FOUND: u32 = 126;
const ERROR_NOACCESS: u32 = 998;

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

fn read_wide(t: &Thread<'_, '_>, addr: u64) -> Option<String> {
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

fn module_handle(t: &mut Thread<'_, '_>, name: Option<String>) -> u64 {
    let Some(name) = name else {
        return t.process.modules[0].base;
    };
    // Accept a path; Windows matches on the file name.
    let file = name.rsplit(['\\', '/']).next().unwrap_or(&name);
    match t.process.module(file) {
        Some(m) => m.base,
        None => {
            t.set_last_error(ERROR_MOD_NOT_FOUND);
            0
        }
    }
}

fn get_module_handle_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let name = if a[0] == 0 {
        None
    } else {
        match t.process.memory.read_cstr(a[0], 32768) {
            Ok(b) => Some(String::from_utf8_lossy(&b).into_owned()),
            Err(_) => {
                t.set_last_error(ERROR_NOACCESS);
                return Ok(0);
            }
        }
    };
    Ok(module_handle(t, name))
}

fn get_module_handle_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let name = if a[0] == 0 {
        None
    } else {
        match read_wide(t, a[0]) {
            Some(s) => Some(s),
            None => {
                t.set_last_error(ERROR_NOACCESS);
                return Ok(0);
            }
        }
    };
    Ok(module_handle(t, name))
}
