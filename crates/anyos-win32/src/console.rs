//! The console: standard handles, console reads and writes, and the console
//! settings programs query (all of which describe one 80-column window).
//!
//! Console output goes to the [`crate::Stdio`] sinks; console input reads the
//! host process's standard input.

use std::io::{Read, Write};

use crate::api::*;
use crate::thread::{Halt, Thread};

pub(crate) const STDIN_HANDLE: u64 = 0x10;
pub(crate) const STDOUT_HANDLE: u64 = 0x14;
pub(crate) const STDERR_HANDLE: u64 = 0x18;

/// Whether `h` is one of the console handles.
pub(crate) fn is_console(h: u64) -> bool {
    matches!(
        h & 0xFFFF_FFFF,
        STDIN_HANDLE | STDOUT_HANDLE | STDERR_HANDLE
    )
}

fn sink<'t, 'a>(t: &'t mut Thread<'a, '_>, handle: u64) -> Option<&'t mut dyn Write> {
    match handle & 0xFFFF_FFFF {
        STDOUT_HANDLE => Some(&mut *t.stdio.stdout),
        STDERR_HANDLE => Some(&mut *t.stdio.stderr),
        _ => None,
    }
}

/// Write to a console handle. `None` when `handle` is not an output console.
pub(crate) fn write(t: &mut Thread<'_, '_>, handle: u64, data: &[u8]) -> Option<bool> {
    let out = sink(t, handle)?;
    Some(out.write_all(data).and_then(|_| out.flush()).is_ok())
}

/// Read up to `max` bytes of console input (a line at a time, like a cooked
/// console). Returns the bytes read; empty at end of input.
pub(crate) fn read(max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut b = [0u8; 1];
    while out.len() < max {
        match lock.read(&mut b) {
            Ok(1) => {
                out.push(b[0]);
                if b[0] == b'\n' {
                    break;
                }
            }
            _ => break,
        }
    }
    out
}

/// What `GetStdHandle(n)` returns now.
pub(crate) fn std_handle(t: &Thread<'_, '_>, n: u32) -> Option<u64> {
    if let Some(&h) = t.k.std_override.get(&n) {
        return Some(h);
    }
    match n as i32 {
        -10 => Some(STDIN_HANDLE),
        -11 => Some(STDOUT_HANDLE),
        -12 => Some(STDERR_HANDLE),
        _ => None,
    }
}

fn get_std_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match std_handle(t, a[0] as u32) {
        Some(h) => Ok(h),
        None => {
            let v = invalid_handle(t);
            fail(t, ERROR_INVALID_HANDLE, v)
        }
    }
}

fn set_std_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let n = a[0] as u32;
    if !(-12..=-10).contains(&(n as i32)) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    t.k.std_override.insert(n, a[1]);
    Ok(1)
}

fn write_console(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (handle, buf, chars, written) = (a[0], a[1], a[2] as u32, a[3]);
    let unit = if wide { 2 } else { 1 };
    let Some(raw) = guest_bytes(t, buf, chars as u64 * unit) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let data = if wide {
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        String::from_utf16_lossy(&units).into_bytes()
    } else {
        raw
    };
    match write(t, handle, &data) {
        Some(true) => {
            store_u32(t, written, chars);
            Ok(1)
        }
        Some(false) => fail(t, ERROR_NOACCESS, 0),
        None => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn read_console(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (handle, buf, chars, read_ptr) = (a[0], a[1], a[2] as u32, a[3]);
    if handle & 0xFFFF_FFFF != STDIN_HANDLE {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    let data = read(chars as usize);
    let (bytes, n) = if wide {
        let units: Vec<u16> = String::from_utf8_lossy(&data)
            .encode_utf16()
            .take(chars as usize)
            .collect();
        let n = units.len() as u32;
        (
            units
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect::<Vec<u8>>(),
            n,
        )
    } else {
        let n = data.len() as u32;
        (data, n)
    };
    if !write_bytes(t, buf, &bytes) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    store_u32(t, read_ptr, n);
    Ok(1)
}

/// Whether `h` is a console handle (files and pipes are not, which is how
/// the C runtime tells a console from a redirected stream).
fn console_handle(_: &mut Thread<'_, '_>, h: u64) -> bool {
    is_console(h)
}

fn get_console_mode(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, out) = (a[0], a[1]);
    if !console_handle(t, h) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    let default = if h & 0xFFFF_FFFF == STDIN_HANDLE {
        0x7
    } else {
        0x3
    };
    let mode =
        t.k.console_mode
            .get(&(h & 0xFFFF_FFFF))
            .copied()
            .unwrap_or(default);
    if !write_u32(t, out, mode) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn set_console_mode(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !console_handle(t, a[0]) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    t.k.console_mode.insert(a[0] & 0xFFFF_FFFF, a[1] as u32);
    Ok(1)
}

const COLUMNS: u16 = 80;
const ROWS: u16 = 25;

fn screen_buffer_info(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !console_handle(t, a[0]) || a[0] & 0xFFFF_FFFF == STDIN_HANDLE {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    let words: [u16; 11] = [
        COLUMNS,
        300,
        0,
        0,
        7,
        0,
        0,
        COLUMNS - 1,
        ROWS - 1,
        COLUMNS,
        ROWS,
    ];
    let b: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    if !write_bytes(t, a[1], &b) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn get_console_title(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let title = t.k.console_title.clone();
    let (n, _) = copy_str_truncate(t, a[0], a[1], &title, wide);
    Ok(n)
}

fn set_console_title(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    match read_str(t, a[0], wide) {
        Some(s) => {
            t.k.console_title = s;
            Ok(1)
        }
        None => fail(t, ERROR_NOACCESS, 0),
    }
}

/// One key-down INPUT_RECORD per byte of standard input.
fn read_console_input(t: &mut Thread<'_, '_>, a: &[u64], peek: bool) -> Result<u64, Halt> {
    let (buf, len, count) = (a[1], a[2] as u32, a[3]);
    if peek || len == 0 {
        store_u32(t, count, 0);
        return Ok(1);
    }
    let data = read(1);
    let Some(&c) = data.first() else {
        store_u32(t, count, 0);
        return Ok(1);
    };
    let vk = c.to_ascii_uppercase() as u16;
    let mut rec = vec![0u8; 20];
    rec[0..2].copy_from_slice(&1u16.to_le_bytes()); // KEY_EVENT
    rec[4..8].copy_from_slice(&1u32.to_le_bytes()); // bKeyDown
    rec[8..10].copy_from_slice(&1u16.to_le_bytes()); // wRepeatCount
    rec[10..12].copy_from_slice(&vk.to_le_bytes());
    rec[14..16].copy_from_slice(&(c as u16).to_le_bytes());
    if !write_bytes(t, buf, &rec) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    store_u32(t, count, 1);
    Ok(1)
}

fn ok_count(t: &mut Thread<'_, '_>, out: u64, n: u32) -> Result<u64, Halt> {
    store_u32(t, out, n);
    Ok(1)
}

pub(crate) static APIS: &[Api] = &[
    k32("GetStdHandle", 1, get_std_handle),
    k32("SetStdHandle", 2, set_std_handle),
    k32("SetStdHandleEx", 3, set_std_handle),
    k32("WriteConsoleA", 5, |t, a| write_console(t, a, false)),
    k32("WriteConsoleW", 5, |t, a| write_console(t, a, true)),
    k32("ReadConsoleA", 5, |t, a| read_console(t, a, false)),
    k32("ReadConsoleW", 5, |t, a| read_console(t, a, true)),
    k32("ReadConsoleInputA", 4, |t, a| {
        read_console_input(t, a, false)
    }),
    k32("ReadConsoleInputW", 4, |t, a| {
        read_console_input(t, a, false)
    }),
    k32("PeekConsoleInputA", 4, |t, a| {
        read_console_input(t, a, true)
    }),
    k32("PeekConsoleInputW", 4, |t, a| {
        read_console_input(t, a, true)
    }),
    k32("GetNumberOfConsoleInputEvents", 2, |t, a| {
        ok_count(t, a[1], 0)
    }),
    k32("FlushConsoleInputBuffer", 1, |_, _| Ok(1)),
    k32("GetConsoleMode", 2, get_console_mode),
    k32("SetConsoleMode", 2, set_console_mode),
    k32("GetConsoleCP", 0, |_, _| Ok(crate::strings::CP_UTF8 as u64)),
    k32("GetConsoleOutputCP", 0, |_, _| {
        Ok(crate::strings::CP_UTF8 as u64)
    }),
    k32("SetConsoleCP", 1, |_, _| Ok(1)),
    k32("SetConsoleOutputCP", 1, |_, _| Ok(1)),
    k32("GetConsoleScreenBufferInfo", 2, screen_buffer_info),
    k32("SetConsoleScreenBufferSize", 2, |_, _| Ok(1)),
    k32("SetConsoleWindowInfo", 3, |_, _| Ok(1)),
    k32("GetLargestConsoleWindowSize", 1, |_, _| {
        Ok((ROWS as u64) << 16 | COLUMNS as u64)
    }),
    k32("SetConsoleTextAttribute", 2, |_, _| Ok(1)),
    k32("SetConsoleCursorPosition", 2, |_, _| Ok(1)),
    k32("GetConsoleCursorInfo", 2, |t, a| {
        write_u32(t, a[1], 25);
        write_u32(t, a[1] + 4, 1);
        Ok(1)
    }),
    k32("SetConsoleCursorInfo", 2, |_, _| Ok(1)),
    k32("FillConsoleOutputCharacterA", 5, |t, a| {
        ok_count(t, a[4], a[2] as u32)
    }),
    k32("FillConsoleOutputCharacterW", 5, |t, a| {
        ok_count(t, a[4], a[2] as u32)
    }),
    k32("FillConsoleOutputAttribute", 5, |t, a| {
        ok_count(t, a[4], a[2] as u32)
    }),
    k32("ScrollConsoleScreenBufferA", 5, |_, _| Ok(1)),
    k32("ScrollConsoleScreenBufferW", 5, |_, _| Ok(1)),
    k32("GetConsoleTitleA", 2, |t, a| get_console_title(t, a, false)),
    k32("GetConsoleTitleW", 2, |t, a| get_console_title(t, a, true)),
    k32("SetConsoleTitleA", 1, |t, a| set_console_title(t, a, false)),
    k32("SetConsoleTitleW", 1, |t, a| set_console_title(t, a, true)),
    k32("GetConsoleWindow", 0, |_, _| Ok(0)),
    k32("AllocConsole", 0, |_, _| Ok(1)),
    k32("FreeConsole", 0, |_, _| Ok(1)),
    k32("AttachConsole", 1, |_, _| Ok(1)),
    k32("SetConsoleCtrlHandler", 2, |_, _| Ok(1)),
    k32("GenerateConsoleCtrlEvent", 2, |_, _| Ok(1)),
    k32("GetConsoleProcessList", 2, |t, a| {
        if a[1] >= 1 {
            write_u32(t, a[0], crate::thread::PROCESS_ID as u32);
        }
        Ok(1)
    }),
];
