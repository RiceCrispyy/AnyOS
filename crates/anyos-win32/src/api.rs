//! Rust implementations of Windows APIs, looked up by DLL and name when the
//! guest reaches a call-gate stub, and the helpers they share for reading and
//! writing guest memory.
//!
//! Each entry records its argument count, which the dispatcher needs to read
//! arguments and, for 32-bit stdcall, to pop them on return. `cdecl` entries
//! (the varargs ones) leave popping to the caller.

use anyos_pe::ImportName;

use crate::module::*;
use crate::thread::{Halt, Thread, PROCESS_ID};

pub(crate) type ApiFn = fn(&mut Thread<'_, '_>, &[u64]) -> Result<u64, Halt>;

pub(crate) struct Api {
    pub dll: &'static str,
    pub name: &'static str,
    /// Argument count on x64, and on x86 (where a 64-bit value passed by
    /// value takes two slots).
    pub args: u8,
    pub args32: u8,
    pub cdecl: bool,
    pub f: ApiFn,
}

pub(crate) const fn api(dll: &'static str, name: &'static str, args: u8, f: ApiFn) -> Api {
    Api {
        dll,
        name,
        args,
        args32: args,
        cdecl: false,
        f,
    }
}

/// A caller-cleans-up (varargs) API; `args` counts the fixed arguments.
pub(crate) const fn cdecl(dll: &'static str, name: &'static str, args: u8, f: ApiFn) -> Api {
    Api {
        dll,
        name,
        args,
        args32: args,
        cdecl: true,
        f,
    }
}

/// A kernel32 export with a 64-bit argument passed by value, which takes
/// `args32` slots on x86.
pub(crate) const fn k32x(name: &'static str, args: u8, args32: u8, f: ApiFn) -> Api {
    Api {
        dll: "kernel32",
        name,
        args,
        args32,
        cdecl: false,
        f,
    }
}

/// A kernel32 export.
pub(crate) const fn k32(name: &'static str, args: u8, f: ApiFn) -> Api {
    api("kernel32", name, args, f)
}

static CORE: &[Api] = &[
    k32("ExitProcess", 1, exit_process),
    k32("FatalExit", 1, exit_process),
    k32("FatalAppExitA", 2, |_, _| Err(Halt::Exit(0xC000_0409))),
    k32("FatalAppExitW", 2, |_, _| Err(Halt::Exit(0xC000_0409))),
    k32("GetLastError", 0, get_last_error),
    k32("SetLastError", 1, set_last_error),
    k32("RestoreLastError", 1, set_last_error),
    k32("GetCurrentProcessId", 0, |_, _| Ok(PROCESS_ID)),
    k32("GetCurrentThreadId", 0, |t, _| Ok(t.thread_id() as u64)),
    k32("GetCommandLineA", 0, |t, _| Ok(t.cmdline_a)),
    k32("GetCommandLineW", 0, |t, _| Ok(t.cmdline_w)),
    k32("GetModuleHandleA", 1, get_module_handle_a),
    k32("GetModuleHandleW", 1, get_module_handle_w),
    k32("GetModuleHandleExA", 3, get_module_handle_ex_a),
    k32("GetModuleHandleExW", 3, get_module_handle_ex_w),
    k32("GetModuleFileNameA", 3, get_module_file_name_a),
    k32("GetModuleFileNameW", 3, get_module_file_name_w),
    k32("LoadLibraryA", 1, load_library_a),
    k32("LoadLibraryW", 1, load_library_w),
    k32("LoadLibraryExA", 3, load_library_ex_a),
    k32("LoadLibraryExW", 3, load_library_ex_w),
    k32("FreeLibrary", 1, free_library),
    k32("FreeLibraryAndExitThread", 2, |_, a| {
        Err(Halt::ExitThread(a[1] as u32))
    }),
    k32("DisableThreadLibraryCalls", 1, |_, _| Ok(1)),
    k32("GetProcAddress", 2, get_proc_address),
    k32("SetErrorMode", 1, set_error_mode),
    k32("GetErrorMode", 0, |t, _| Ok(t.error_mode as u64)),
    k32("SetThreadErrorMode", 2, |t, a| {
        let old = t.error_mode;
        t.error_mode = a[0] as u32;
        Ok(u64::from(store_u32(t, a[1], old)))
    }),
    k32("GetThreadErrorMode", 0, |t, _| Ok(t.error_mode as u64)),
    // DLLs come from the program's directory or AnyOS itself, so DLL search
    // settings are accepted and have nothing to change.
    k32("SetDefaultDllDirectories", 1, |_, _| Ok(1)),
    k32("SetDllDirectoryA", 1, |_, _| Ok(1)),
    k32("SetDllDirectoryW", 1, |_, _| Ok(1)),
    k32("AddDllDirectory", 1, |_, _| Ok(1)),
    k32("RemoveDllDirectory", 1, |_, _| Ok(1)),
    k32("EncodePointer", 1, |_, a| Ok(a[0])),
    k32("DecodePointer", 1, |_, a| Ok(a[0])),
    k32("EncodeSystemPointer", 1, |_, a| Ok(a[0])),
    k32("DecodeSystemPointer", 1, |_, a| Ok(a[0])),
    k32("IsDebuggerPresent", 0, |_, _| Ok(0)),
    k32("CheckRemoteDebuggerPresent", 2, |t, a| {
        Ok(u64::from(store_u32(t, a[1], 0)))
    }),
    k32("OutputDebugStringA", 1, |t, a| {
        let s = read_ansi(t, a[0]).unwrap_or_default();
        t.trace_note(&format!("debug output: {}", s.trim_end()));
        Ok(0)
    }),
    k32("OutputDebugStringW", 1, |t, a| {
        let s = read_wide(t, a[0]).unwrap_or_default();
        t.trace_note(&format!("debug output: {}", s.trim_end()));
        Ok(0)
    }),
    k32("DebugBreak", 0, |t, _| {
        let location = t.describe(t.ctx.rip);
        Err(crate::RunError::Breakpoint { location }.into())
    }),
    k32("SetUnhandledExceptionFilter", 1, |t, a| {
        Ok(std::mem::replace(&mut t.k.unhandled_filter, a[0]))
    }),
    // EXCEPTION_EXECUTE_HANDLER: let the caller terminate.
    k32("UnhandledExceptionFilter", 1, |_, _| Ok(1)),
    k32("RaiseException", 4, |t, a| {
        let location = t.describe(read_ret_addr(t));
        Err(crate::RunError::GuestException {
            code: a[0] as u32,
            location,
        }
        .into())
    }),
    k32("RaiseFailFastException", 3, |_, _| {
        Err(Halt::Exit(0xC000_0409))
    }),
    k32("AddVectoredExceptionHandler", 2, |_, a| Ok(a[1].max(1))),
    k32("RemoveVectoredExceptionHandler", 1, |_, _| Ok(1)),
    k32("AddVectoredContinueHandler", 2, |_, a| Ok(a[1].max(1))),
    k32("RemoveVectoredContinueHandler", 1, |_, _| Ok(1)),
    k32("SetErrorInfo", 2, |_, _| Ok(0)),
    k32("Beep", 2, |_, _| Ok(1)),
    k32("GetCurrentPackageId", 2, |_, _| Ok(15700)), // APPMODEL_ERROR_NO_PACKAGE
    k32("GetCurrentPackageFullName", 2, |_, _| Ok(15700)),
];

/// Every table of implementations, searched in order.
fn tables() -> [&'static [Api]; 15] {
    [
        CORE,
        crate::console::APIS,
        crate::file::APIS,
        crate::memory::APIS,
        crate::process::APIS,
        crate::sync::APIS,
        crate::time::APIS,
        crate::strings::APIS,
        crate::locale::APIS,
        crate::sysinfo::APIS,
        crate::registry::APIS,
        crate::resource::APIS,
        crate::strings::USER32,
        ADVAPI32,
        crate::ui::APIS,
    ]
}

/// The implementation of `dll!symbol`, if there is one.
pub(crate) fn find(dll: &str, symbol: &ImportName) -> Option<&'static Api> {
    let ImportName::Name { name, .. } = symbol else {
        return None;
    };
    let dll = dll.to_ascii_lowercase();
    let dll = dll.strip_suffix(".dll").unwrap_or(&dll);
    // KernelBase and the API-set DLLs forward to kernel32, advapi32 and ntdll.
    let dlls: &[&str] = if dll == "kernelbase" || dll.starts_with("api-ms-win-") {
        &["kernel32", "advapi32", "ntdll", "user32"]
    } else {
        &[dll]
    };
    dlls.iter().find_map(|dll| {
        tables()
            .into_iter()
            .flatten()
            .find(|a| a.dll == *dll && a.name == name)
    })
}

/// Whether `dll` (lower case, with `.dll`) exports `name` here.
pub(crate) fn implemented(dll: &str, name: &str) -> bool {
    find(
        dll,
        &ImportName::Name {
            name: name.into(),
            hint: 0,
        },
    )
    .is_some()
}

// Error codes shared across modules.
pub(crate) const ERROR_SUCCESS: u32 = 0;
pub(crate) const ERROR_FILE_NOT_FOUND: u32 = 2;
pub(crate) const ERROR_PATH_NOT_FOUND: u32 = 3;
pub(crate) const ERROR_ACCESS_DENIED: u32 = 5;
pub(crate) const ERROR_INVALID_HANDLE: u32 = 6;
pub(crate) const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
pub(crate) const ERROR_NO_MORE_FILES: u32 = 18;
pub(crate) const ERROR_NOT_SUPPORTED: u32 = 50;
pub(crate) const ERROR_FILE_EXISTS: u32 = 80;
pub(crate) const ERROR_INVALID_PARAMETER: u32 = 87;
pub(crate) const ERROR_BROKEN_PIPE: u32 = 109;
pub(crate) const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub(crate) const ERROR_INVALID_NAME: u32 = 123;
pub(crate) const ERROR_MOD_NOT_FOUND: u32 = 126;
pub(crate) const ERROR_DIR_NOT_EMPTY: u32 = 145;
pub(crate) const ERROR_ALREADY_EXISTS: u32 = 183;
pub(crate) const ERROR_ENVVAR_NOT_FOUND: u32 = 203;
pub(crate) const ERROR_MORE_DATA: u32 = 234;
pub(crate) const ERROR_NO_MORE_ITEMS: u32 = 259;
pub(crate) const ERROR_NOT_OWNER: u32 = 288;
pub(crate) const ERROR_INVALID_ADDRESS: u32 = 487;
pub(crate) const ERROR_NOACCESS: u32 = 998;
pub(crate) const ERROR_TIMEOUT: u32 = 1460;

pub(crate) const STILL_ACTIVE: u32 = 259;

/// `INVALID_HANDLE_VALUE` in the guest's pointer size.
pub(crate) fn invalid_handle(t: &Thread<'_, '_>) -> u64 {
    if t.long_mode() {
        u64::MAX
    } else {
        0xFFFF_FFFF
    }
}

/// Set the last error and return `value` (usually 0 or FALSE).
pub(crate) fn fail(t: &mut Thread<'_, '_>, code: u32, value: u64) -> Result<u64, Halt> {
    t.set_last_error(code);
    Ok(value)
}

/// `-1` as a guest `int`/`DWORD` result.
pub(crate) const MINUS_ONE: u64 = 0xFFFF_FFFF;

fn read_ret_addr(t: &Thread<'_, '_>) -> u64 {
    t.process
        .memory
        .read_ptr_raw(t.ctx.regs[anyos_backend::x86_reg::RSP], t.ptr_size())
        .unwrap_or(0)
}

fn exit_process(_: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    Err(Halt::Exit(a[0] as u32))
}

fn get_last_error(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    Ok(t.last_error() as u64)
}

fn set_last_error(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    t.set_last_error(a[0] as u32);
    Ok(0)
}

fn set_error_mode(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let old = t.error_mode;
    t.error_mode = a[0] as u32;
    Ok(old as u64)
}

// ---------------------------------------------------------------------------
// Guest memory helpers. All go through the guest's own access rights, so a bad
// pointer becomes ERROR_NOACCESS instead of touching protected memory.

/// Read `len` guest bytes.
pub(crate) fn guest_bytes(t: &Thread<'_, '_>, addr: u64, len: u64) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; usize::try_from(len).ok()?];
    t.process.memory.read(addr, &mut buf).ok()?;
    Some(buf)
}

pub(crate) fn write_bytes(t: &mut Thread<'_, '_>, addr: u64, data: &[u8]) -> bool {
    data.is_empty() || t.process.memory.write(addr, data).is_ok()
}

pub(crate) fn read_u16(t: &Thread<'_, '_>, addr: u64) -> Option<u16> {
    let b = guest_bytes(t, addr, 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn read_u32(t: &Thread<'_, '_>, addr: u64) -> Option<u32> {
    let b = guest_bytes(t, addr, 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

pub(crate) fn read_u64(t: &Thread<'_, '_>, addr: u64) -> Option<u64> {
    let b = guest_bytes(t, addr, 8)?;
    Some(u64::from_le_bytes(b.try_into().ok()?))
}

/// A pointer-sized value.
pub(crate) fn read_ptr(t: &Thread<'_, '_>, addr: u64) -> Option<u64> {
    if t.long_mode() {
        read_u64(t, addr)
    } else {
        read_u32(t, addr).map(u64::from)
    }
}

pub(crate) fn write_u16(t: &mut Thread<'_, '_>, addr: u64, v: u16) -> bool {
    write_bytes(t, addr, &v.to_le_bytes())
}

pub(crate) fn write_u32(t: &mut Thread<'_, '_>, addr: u64, v: u32) -> bool {
    write_bytes(t, addr, &v.to_le_bytes())
}

pub(crate) fn write_u64(t: &mut Thread<'_, '_>, addr: u64, v: u64) -> bool {
    write_bytes(t, addr, &v.to_le_bytes())
}

pub(crate) fn write_ptr(t: &mut Thread<'_, '_>, addr: u64, v: u64) -> bool {
    if t.long_mode() {
        write_u64(t, addr, v)
    } else {
        write_u32(t, addr, v as u32)
    }
}

/// Store a DWORD result through a guest pointer, if the pointer is non-null.
pub(crate) fn store_u32(t: &mut Thread<'_, '_>, ptr: u64, v: u32) -> bool {
    ptr == 0 || write_u32(t, ptr, v)
}

/// Store a pointer-sized result through a guest pointer, if it is non-null.
pub(crate) fn store_ptr(t: &mut Thread<'_, '_>, ptr: u64, v: u64) -> bool {
    ptr == 0 || write_ptr(t, ptr, v)
}

pub(crate) fn read_ansi(t: &Thread<'_, '_>, addr: u64) -> Option<String> {
    let b = t.process.memory.read_cstr(addr, 32768).ok()?;
    Some(crate::strings::ansi_to_string(&b))
}

/// The UTF-16 units of a NUL-terminated wide string, without the terminator.
pub(crate) fn read_wide_units(t: &Thread<'_, '_>, addr: u64) -> Option<Vec<u16>> {
    let mut units = Vec::new();
    let mut b = [0u8; 2];
    for k in 0..32768u64 {
        t.process.memory.read(addr + k * 2, &mut b).ok()?;
        match u16::from_le_bytes(b) {
            0 => return Some(units),
            u => units.push(u),
        }
    }
    None
}

pub(crate) fn read_wide(t: &Thread<'_, '_>, addr: u64) -> Option<String> {
    read_wide_units(t, addr).map(|u| String::from_utf16_lossy(&u))
}

/// Read a NUL-terminated string argument, ANSI or UTF-16.
pub(crate) fn read_str(t: &Thread<'_, '_>, addr: u64, wide: bool) -> Option<String> {
    if wide {
        read_wide(t, addr)
    } else {
        read_ansi(t, addr)
    }
}

/// Like [`read_str`], but `NULL` reads as `None` and an unreadable pointer
/// sets `ERROR_NOACCESS`; for optional string arguments.
pub(crate) fn opt_str(t: &mut Thread<'_, '_>, addr: u64, wide: bool) -> Result<Option<String>, ()> {
    if addr == 0 {
        return Ok(None);
    }
    match read_str(t, addr, wide) {
        Some(s) => Ok(Some(s)),
        None => {
            t.set_last_error(ERROR_NOACCESS);
            Err(())
        }
    }
}

/// Encode `s` the way an A or W API hands strings back, with the terminator.
pub(crate) fn encode(s: &str, wide: bool) -> Vec<u8> {
    if wide {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut b = crate::strings::string_to_ansi(s);
        b.push(0);
        b
    }
}

/// Characters (bytes or UTF-16 units) in `s` as an A or W API counts them.
pub(crate) fn char_len(s: &str, wide: bool) -> u64 {
    if wide {
        s.encode_utf16().count() as u64
    } else {
        crate::strings::string_to_ansi(s).len() as u64
    }
}

/// Copy `s` into a guest buffer of `size` characters the way most string
/// APIs do: on success return its length, if it does not fit return the
/// size needed including the terminator and write nothing.
pub(crate) fn copy_str_out(
    t: &mut Thread<'_, '_>,
    buf: u64,
    size: u64,
    s: &str,
    wide: bool,
) -> u64 {
    let len = char_len(s, wide);
    if (size as u32 as u64) <= len {
        return len + 1;
    }
    if !write_bytes(t, buf, &encode(s, wide)) {
        t.set_last_error(ERROR_NOACCESS);
        return 0;
    }
    len
}

/// Copy `s` into a buffer of `size` characters, cutting it short to fit (always
/// terminated when `size > 0`). Returns the characters written, without the
/// terminator, and whether all of `s` fit.
pub(crate) fn copy_str_truncate(
    t: &mut Thread<'_, '_>,
    buf: u64,
    size: u64,
    s: &str,
    wide: bool,
) -> (u64, bool) {
    if size == 0 {
        return (0, s.is_empty());
    }
    let units: Vec<u16> = if wide {
        s.encode_utf16().collect()
    } else {
        crate::strings::string_to_ansi(s)
            .into_iter()
            .map(u16::from)
            .collect()
    };
    let n = units.len().min(size as usize - 1);
    let mut out = Vec::new();
    for &u in units[..n].iter().chain([&0]) {
        if wide {
            out.extend_from_slice(&u.to_le_bytes());
        } else {
            out.push(u as u8);
        }
    }
    write_bytes(t, buf, &out);
    (n as u64, n == units.len())
}

/// The `Copy a string out` convention where the size is passed and returned
/// through a pointer (GetComputerName, GetUserName): on success store the
/// length without the terminator; when too small store the size needed.
pub(crate) fn copy_str_inout(
    t: &mut Thread<'_, '_>,
    buf: u64,
    size_ptr: u64,
    s: &str,
    wide: bool,
    size_includes_nul: bool,
) -> Result<u64, Halt> {
    let Some(size) = read_u32(t, size_ptr) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let len = char_len(s, wide) as u32;
    if size <= len || buf == 0 {
        write_u32(t, size_ptr, len + 1);
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, buf, &encode(s, wide)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    write_u32(t, size_ptr, if size_includes_nul { len + 1 } else { len });
    Ok(1)
}

// ── ADVAPI32 ───────────────────────────────────────────────────────────────

/// `advapi32` exports that are not in the registry family.
pub(crate) static ADVAPI32: &[Api] = &[
    api("advapi32", "GetUserNameA", 2, |t, a| {
        let user = t.host.user.clone();
        copy_str_inout(t, a[0], a[1], &user, false, true)
    }),
    api("advapi32", "GetUserNameW", 2, |t, a| {
        let user = t.host.user.clone();
        copy_str_inout(t, a[0], a[1], &user, true, true)
    }),
    api("advapi32", "SystemFunction036", 2, |t, a| {
        Ok(crate::sysinfo::gen_random(t, a[0], a[1] as u32 as u64) as u64)
    }),
    api("bcryptprimitives", "ProcessPrng", 2, |t, a| {
        Ok(crate::sysinfo::gen_random(t, a[0], a[1] as u32 as u64) as u64)
    }),
    api("bcrypt", "BCryptGenRandom", 4, |t, a| {
        // STATUS_SUCCESS / STATUS_INVALID_PARAMETER
        Ok(
            if crate::sysinfo::random_bytes((a[2] as usize).min(64)).len() > 0 {
                0
            } else {
                0xC000_000D
            },
        )
    }),
    api("advapi32", "OpenProcessToken", 3, |t, a| {
        let h = t.k.insert(crate::objects::Object::Null, None);
        Ok(store_ptr(t, a[2], h) as u64)
    }),
    api("advapi32", "OpenThreadToken", 4, |t, _| fail(t, 1008, 0)), // ERROR_NO_TOKEN
    api("advapi32", "GetTokenInformation", 5, |t, a| {
        // TokenElevation (20) and TokenElevationType (18): elevated.
        match a[1] {
            20 => Ok((store_u32(t, a[4], 4) && write_u32(t, a[2], 1)) as u64),
            18 => Ok((store_u32(t, a[4], 4) && write_u32(t, a[2], 2)) as u64),
            _ => fail(t, ERROR_NOT_SUPPORTED, 0),
        }
    }),
    api("advapi32", "AllocateAndInitializeSid", 11, |t, a| {
        // A SID: revision, count, authority (6 bytes), sub-authorities.
        let count = (a[1] as u8).min(8);
        let mut sid = vec![1u8, count];
        let auth = read_u32(t, a[0] + 2).unwrap_or(0);
        sid.extend_from_slice(&[0, 0]);
        sid.extend_from_slice(&auth.to_be_bytes());
        for v in &a[2..2 + count as usize] {
            sid.extend_from_slice(&(*v as u32).to_le_bytes());
        }
        let heap = t.k.process_heap;
        let p = crate::memory::heap_alloc(t, heap, sid.len() as u64, false);
        if p == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_bytes(t, p, &sid);
        Ok(store_ptr(t, a[10], p) as u64)
    }),
    api("advapi32", "FreeSid", 1, |t, a| {
        let heap = t.k.process_heap;
        crate::memory::heap_free(t, heap, a[0]);
        Ok(0)
    }),
    api("advapi32", "CheckTokenMembership", 3, |t, a| {
        Ok(store_u32(t, a[2], 1) as u64)
    }),
    // ConvertStringSecurityDescriptorToSecurityDescriptor: PSDLENGTH
    // returns the buffer size on success (caller only needs the call to succeed).
    api(
        "advapi32",
        "ConvertStringSecurityDescriptorToSecurityDescriptorA",
        3,
        |t, a| {
            let s = read_str(t, a[0], false).unwrap_or_default();
            let size = s.len() + 1;
            if write_u32(t, a[2], size as u32) {
                Ok(1)
            } else {
                fail(t, ERROR_INSUFFICIENT_BUFFER, 0)
            }
        },
    ),
    api(
        "advapi32",
        "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        3,
        |t, a| {
            let s = read_wide(t, a[0]).unwrap_or_default();
            let size = s.len() * 2 + 1;
            if write_u32(t, a[2], size as u32) {
                Ok(1)
            } else {
                fail(t, ERROR_INSUFFICIENT_BUFFER, 0)
            }
        },
    ),
    api("advapi32", "ConvertSidToStringSidA", 3, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let s = format!("S-{}-{}{}\n", 1, 0, 0);
        Ok(copy_str_out(t, a[1], 260, &s, false))
    }),
    api("advapi32", "ConvertSidToStringSidW", 3, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let s = format!("S-{}-{}{}\n", 1, 0, 0);
        Ok(copy_str_out(t, a[1], 260, &s, true))
    }),
    api(
        "advapi32",
        "ConvertSecurityDescriptorToStringSidA",
        3,
        |t, a| Ok(copy_str_out(t, a[1], 260, "S-1-5-21-0-0-0", false) as u64),
    ),
    api(
        "advapi32",
        "ConvertSecurityDescriptorToStringSidW",
        3,
        |t, a| Ok(copy_str_out(t, a[1], 260, "S-1-5-21-0-0-0", true) as u64),
    ),
    api("advapi32", "LookupAccountNameA", 6, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            crate::memory::heap_free(t, t.k.process_heap, sid);
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[4], 0);
        write_u32(t, a[5], 0);
        if !write_bytes(t, sid, b"S-1-5-21-0-0-0") {
            return fail(t, ERROR_NOACCESS, 0);
        }
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", false) as u64)
    }),
    api("advapi32", "LookupAccountNameW", 6, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            crate::memory::heap_free(t, t.k.process_heap, sid);
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[4], 0);
        write_u32(t, a[5], 0);
        if !write_bytes(t, sid, b"S-1-5-21-0-0-0") {
            return fail(t, ERROR_NOACCESS, 0);
        }
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", true) as u64)
    }),
    api("advapi32", "LookupSIDA", 6, |t, a| {
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[5], 0);
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", false) as u64)
    }),
    api("advapi32", "LookupSIDW", 6, |t, a| {
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[5], 0);
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", true) as u64)
    }),
    api("advapi32", "SetEntriesInAclA", 4, |_, _| Ok(1)),
    api("advapi32", "SetEntriesInAclW", 4, |_, _| Ok(1)),
    api("advapi32", "NetUserAdd", 6, |_, _| Ok(1)),
    api("advapi32", "NetUserDel", 2, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupAdd", 5, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupDel", 3, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupGetMembers", 7, |t, a| {
        Ok(write_u32(t, a[7], 0) as u64)
    }),
    api("advapi32", "NetShareAdd", 8, |_, _| Ok(1)),
    api("advapi32", "NetShareDel", 3, |_, _| Ok(1)),
    api("advapi32", "NetrAuthorizeService", 7, |_, _| Ok(1)),
    api("advapi32", "NtSetInformationThread", 5, |_, _| Ok(1)),
    api("advapi32", "RtlGetVersion", 1, |t, a| {
        let mut ovi = [0u8; 16];
        ovi[0] = 10; // dwMajorVersion = Windows 10
        if write_bytes(t, a[0], &ovi) {
            Ok(1)
        } else {
            fail(t, ERROR_NOACCESS, 0)
        }
    }),
    api("advapi32", "GetSystemTimeAsFileTime", 1, |t, a| {
        let ft = crate::time::filetime_of(std::time::SystemTime::now());
        if write_bytes(t, a[0], &ft.to_le_bytes()) {
            Ok(1)
        } else {
            fail(t, ERROR_NOACCESS, 0)
        }
    }),
    api("advapi32", "GetFileSizeEx", 2, |t, a| {
        Ok(write_u64(t, a[1], 1024) as u64)
    }),
    api("advapi32", "GetFileType", 1, |_, _| Ok(1)),
    api("advapi32", "GetMailslotInfo", 5, |t, a| {
        Ok(write_u32(t, a[4], 0) as u64)
    }),
    api("advapi32", "GetNamedPipeInfo", 6, |t, a| {
        Ok(write_u32(t, a[4], 1) as u64)
    }),
    api("advapi32", "GetNamedPipeHandleState", 6, |t, a| {
        Ok(write_u32(t, a[4], 0) as u64)
    }),
    api("advapi32", "GetSecurityInfo", 13, |_, _| Ok(1)),
    api("advapi32", "GetKernelObjectSecurity", 8, |_, _| Ok(1)),
    // ── ADVAPI32: installer/string/registry helpers ──
    api(
        "advapi32",
        "ConvertStringSecurityDescriptorToSecurityDescriptorA",
        3,
        |t, a| {
            let s = read_str(t, a[0], false).unwrap_or_default();
            let size = s.len() + 1;
            if write_u32(t, a[2], size as u32) {
                Ok(1)
            } else {
                fail(t, ERROR_INSUFFICIENT_BUFFER, 0)
            }
        },
    ),
    api(
        "advapi32",
        "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        3,
        |t, a| {
            let s = read_wide(t, a[0]).unwrap_or_default();
            let size = s.len() * 2 + 1;
            if write_u32(t, a[2], size as u32) {
                Ok(1)
            } else {
                fail(t, ERROR_INSUFFICIENT_BUFFER, 0)
            }
        },
    ),
    api("advapi32", "ConvertSidToStringSidA", 3, |t, a| {
        let heap = t.k.process_heap;
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let s = format!("S-{}-{}{}\n", 1, 0, 0);
        Ok(copy_str_out(t, a[1], 260, &s, false))
    }),
    api("advapi32", "ConvertSidToStringSidW", 3, |t, a| {
        let heap = t.k.process_heap;
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let s = format!("S-{}-{}{}\n", 1, 0, 0);
        Ok(copy_str_out(t, a[1], 260, &s, true))
    }),
    api(
        "advapi32",
        "ConvertSecurityDescriptorToStringSidA",
        3,
        |t, a| Ok(copy_str_out(t, a[1], 260, "S-1-5-21-0-0-0", false) as u64),
    ),
    api(
        "advapi32",
        "ConvertSecurityDescriptorToStringSidW",
        3,
        |t, a| Ok(copy_str_out(t, a[1], 260, "S-1-5-21-0-0-0", true) as u64),
    ),
    api("advapi32", "LookupAccountNameA", 6, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            crate::memory::heap_free(t, t.k.process_heap, sid);
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[4], 0);
        write_u32(t, a[5], 0);
        if !write_bytes(t, sid, b"S-1-5-21-0-0-0") {
            return fail(t, ERROR_NOACCESS, 0);
        }
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", false) as u64)
    }),
    api("advapi32", "LookupAccountNameW", 6, |t, a| {
        let sid = crate::memory::heap_alloc(t, t.k.process_heap, 4096, false);
        if sid == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            crate::memory::heap_free(t, t.k.process_heap, sid);
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[4], 0);
        write_u32(t, a[5], 0);
        if !write_bytes(t, sid, b"S-1-5-21-0-0-0") {
            return fail(t, ERROR_NOACCESS, 0);
        }
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", true) as u64)
    }),
    api("advapi32", "LookupSIDA", 6, |t, a| {
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[5], 0);
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", false) as u64)
    }),
    api("advapi32", "LookupSIDW", 6, |t, a| {
        let dom = crate::memory::heap_alloc(t, t.k.process_heap, 260, false);
        if dom == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_u32(t, a[5], 0);
        Ok(copy_str_out(t, a[1], 260, "BUILTIN\\Users", true) as u64)
    }),
    api("advapi32", "SetEntriesInAclA", 4, |_, _| Ok(1)),
    api("advapi32", "SetEntriesInAclW", 4, |_, _| Ok(1)),
    api("advapi32", "NetUserAdd", 6, |_, _| Ok(1)),
    api("advapi32", "NetUserDel", 2, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupAdd", 5, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupDel", 3, |_, _| Ok(1)),
    api("advapi32", "NetLocalGroupGetMembers", 7, |t, a| {
        Ok(write_u32(t, a[7], 0) as u64)
    }),
    api("advapi32", "NetShareAdd", 8, |_, _| Ok(1)),
    api("advapi32", "NetShareDel", 3, |_, _| Ok(1)),
    api("advapi32", "NetrAuthorizeService", 7, |_, _| Ok(1)),
    api("advapi32", "NtSetInformationThread", 5, |_, _| Ok(1)),
    api("advapi32", "RtlGetVersion", 1, |t, a| {
        let mut ovi = [0u8; 16];
        ovi[0] = 10; // dwMajorVersion = Windows 10
        if write_bytes(t, a[0], &ovi) {
            Ok(1)
        } else {
            fail(t, ERROR_NOACCESS, 0)
        }
    }),
    api("advapi32", "GetSystemTimeAsFileTime", 1, |t, a| {
        let ft = crate::time::filetime_of(std::time::SystemTime::now());
        if write_bytes(t, a[0], &ft.to_le_bytes()) {
            Ok(1)
        } else {
            fail(t, ERROR_NOACCESS, 0)
        }
    }),
    api("advapi32", "GetFileSizeEx", 2, |t, a| {
        Ok(write_u64(t, a[1], 1024) as u64)
    }),
    api("advapi32", "GetFileType", 1, |_, _| Ok(1)),
    api("advapi32", "GetMailslotInfo", 5, |t, a| {
        Ok(write_u32(t, a[4], 0) as u64)
    }),
    api("advapi32", "GetNamedPipeInfo", 6, |t, a| {
        Ok(write_u32(t, a[4], 1) as u64)
    }),
    api("advapi32", "GetNamedPipeHandleState", 6, |t, a| {
        Ok(write_u32(t, a[4], 0) as u64)
    }),
    api("advapi32", "GetSecurityInfo", 13, |_, _| Ok(1)),
    api("advapi32", "GetKernelObjectSecurity", 8, |_, _| Ok(1)),
    api("advapi32", "SetSecurityInfo", 12, |_, _| Ok(1)),
    api("advapi32", "SetKernelObjectSecurity", 6, |_, _| Ok(1)),
    api("advapi32", "GetFileSecurityA", 7, |t, a| {
        Ok(write_u32(t, a[4], 1024) as u64)
    }),
    api("advapi32", "GetFileSecurityW", 7, |t, a| {
        Ok(write_u32(t, a[4], 1024) as u64)
    }),
    api("advapi32", "SetFileSecurityA", 5, |_, _| Ok(1)),
    api("advapi32", "SetFileSecurityW", 5, |_, _| Ok(1)),
    api("advapi32", "SetKernelObjectSecurity", 6, |_, _| Ok(1)),
    api("advapi32", "RegOpenKeyExA", 6, |_, _| Ok(1)),
    api("advapi32", "RegOpenKeyExW", 6, |_, _| Ok(1)),
    api("advapi32", "RegCloseKey", 1, |_, _| Ok(1)),
    api("advapi32", "RegQueryValueExA", 7, |t, a| {
        Ok(write_u32(t, a[6], 1024) as u64)
    }),
    api("advapi32", "RegQueryValueExW", 7, |t, a| {
        Ok(write_u32(t, a[6], 1024) as u64)
    }),
    api("advapi32", "RegEnumValueA", 8, |t, a| {
        Ok(write_u32(t, a[7], 1024) as u64)
    }),
    api("advapi32", "RegEnumValueW", 8, |t, a| {
        Ok(write_u32(t, a[7], 1024) as u64)
    }),
    api("advapi32", "RegQueryInfoKeyA", 8, |t, a| {
        Ok(write_u32(t, a[11], 1024) as u64)
    }),
    api("advapi32", "RegQueryInfoKeyW", 8, |t, a| {
        Ok(write_u32(t, a[11], 1024) as u64)
    }),
    api("advapi32", "RegCreateKeyExA", 9, |_, _| Ok(1)),
    api("advapi32", "RegCreateKeyExW", 9, |_, _| Ok(1)),
    api("advapi32", "RegDeleteValueA", 3, |_, _| Ok(1)),
    api("advapi32", "RegDeleteValueW", 3, |_, _| Ok(1)),
    api("advapi32", "RegDeleteKeyA", 3, |_, _| Ok(1)),
    api("advapi32", "RegDeleteKeyW", 3, |_, _| Ok(1)),
    api("advapi32", "RegDeleteTreeA", 3, |_, _| Ok(1)),
    api("advapi32", "RegDeleteTreeW", 3, |_, _| Ok(1)),
    api("advapi32", "RegSetValueExA", 7, |_, _| Ok(1)),
    api("advapi32", "RegSetValueExW", 7, |_, _| Ok(1)),
    api("advapi32", "RegFlushKey", 1, |_, _| Ok(1)),
    api("advapi32", "RegNotifyChangeKeyValue", 7, |_, _| Ok(1)),
    api("advapi32", "RegSaveKeyA", 4, |_, _| Ok(1)),
    api("advapi32", "RegSaveKeyW", 4, |_, _| Ok(1)),
    api("advapi32", "RegLoadKeyA", 4, |_, _| Ok(1)),
    api("advapi32", "RegLoadKeyW", 4, |_, _| Ok(1)),
    api("advapi32", "RegReplaceKeyA", 5, |_, _| Ok(1)),
    api("advapi32", "RegReplaceKeyW", 5, |_, _| Ok(1)),
    api("advapi32", "RegConnectRegistryA", 3, |_, _| Ok(1)),
    api("advapi32", "RegConnectRegistryW", 3, |_, _| Ok(1)),
    api("advapi32", "RegCopyTreeA", 4, |_, _| Ok(1)),
    api("advapi32", "RegCopyTreeW", 4, |_, _| Ok(1)),
    api("advapi32", "RegRestoreKeyA", 3, |_, _| Ok(1)),
    api("advapi32", "RegRestoreKeyW", 3, |_, _| Ok(1)),
    api("advapi32", "RegDisablePredefinedCache", 2, |_, _| Ok(1)),
    api("advapi32", "RegEnableReflectionKey", 2, |_, _| Ok(1)),
    api("advapi32", "RegDisableReflectionKey", 2, |_, _| Ok(1)),
    api("advapi32", "RegLoadMappedFile", 7, |_, _| Ok(1)),
    api("advapi32", "RegHookCrossProcessNavigation", 1, |_, _| Ok(1)),
    api("advapi32", "RegUninjectIdleProcessorCallback", 1, |_, _| {
        Ok(1)
    }),
    api("advapi32", "RegCopyTree", 4, |_, _| Ok(1)),
    api("shell32", "IsUserAnAdmin", 0, |_, _| Ok(1)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advapi32_conversions_are_possible() {
        assert!(crate::api::implemented(
            "advapi32",
            "ConvertStringSecurityDescriptorToSecurityDescriptorW"
        ));
    }
}
