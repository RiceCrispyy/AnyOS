//! System information: the environment block, Windows version, system
//! directories, processor and memory information, atoms, error messages and
//! the user and computer names.

use crate::api::*;
use crate::thread::{Halt, Thread};

/// The Windows version AnyOS reports: Windows 10 22H2.
const VERSION: (u32, u32, u32) = (10, 0, 19045);
const VER_PLATFORM_WIN32_NT: u32 = 2;
const VER_NT_WORKSTATION: u8 = 1;
const ERROR_OLD_WIN_VERSION: u32 = 1150;
const ERROR_MR_MID_NOT_FOUND: u32 = 317;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const COMPUTER_NAME: &str = "ANYOS-PC";
const WINDOWS_DIR: &str = r"C:\Windows";
const SYSTEM_DIR: &str = r"C:\Windows\System32";
/// First string atom; smaller values are integer atoms.
const MAXINTATOM: u32 = 0xC000;

// ---------------------------------------------------------------------------
// Environment

/// Build the guest's environment: what a fresh Windows user session has,
/// then `extra` on top.
pub(crate) fn init_env(t: &mut Thread<'_, '_>, extra: &[(String, String)]) {
    let user = t.host.user.clone();
    let long = t.long_mode();
    let (profile, temp) = if t.host.drive_c.is_some() {
        let profile = format!(r"C:\Users\{user}");
        let temp = format!(r"{profile}\AppData\Local\Temp");
        (profile, temp)
    } else {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|h| crate::file::guest_path(t, &h))
            .unwrap_or_else(|| format!(r"C:\Users\{user}"));
        let temp = crate::file::guest_path(t, &std::env::temp_dir());
        (home, temp)
    };
    let nproc = std::thread::available_parallelism().map_or(1, |n| n.get());
    let home_path = profile.get(2..).unwrap_or(r"\").to_string();
    let mut env: Vec<(String, String)> = vec![
        ("ALLUSERSPROFILE".into(), r"C:\ProgramData".into()),
        ("APPDATA".into(), format!(r"{profile}\AppData\Roaming")),
        (
            "CommonProgramFiles".into(),
            r"C:\Program Files\Common Files".into(),
        ),
        ("COMPUTERNAME".into(), COMPUTER_NAME.into()),
        ("ComSpec".into(), format!(r"{SYSTEM_DIR}\cmd.exe")),
        ("HOMEDRIVE".into(), profile.get(..2).unwrap_or("C:").into()),
        ("HOMEPATH".into(), home_path),
        ("LOCALAPPDATA".into(), format!(r"{profile}\AppData\Local")),
        ("NUMBER_OF_PROCESSORS".into(), nproc.to_string()),
        ("OS".into(), "Windows_NT".into()),
        (
            "Path".into(),
            format!(r"{SYSTEM_DIR};{WINDOWS_DIR};{SYSTEM_DIR}\Wbem"),
        ),
        (
            "PATHEXT".into(),
            ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC".into(),
        ),
        (
            "PROCESSOR_ARCHITECTURE".into(),
            if long { "AMD64" } else { "x86" }.into(),
        ),
        ("ProgramData".into(), r"C:\ProgramData".into()),
        ("ProgramFiles".into(), r"C:\Program Files".into()),
        ("PUBLIC".into(), r"C:\Users\Public".into()),
        ("SystemDrive".into(), "C:".into()),
        ("SystemRoot".into(), WINDOWS_DIR.into()),
        ("TEMP".into(), temp.clone()),
        ("TMP".into(), temp),
        ("USERDOMAIN".into(), COMPUTER_NAME.into()),
        ("USERNAME".into(), user),
        ("USERPROFILE".into(), profile),
        ("windir".into(), WINDOWS_DIR.into()),
    ];
    if long {
        env.push(("ProgramFiles(x86)".into(), r"C:\Program Files (x86)".into()));
        env.push((
            "CommonProgramFiles(x86)".into(),
            r"C:\Program Files (x86)\Common Files".into(),
        ));
        env.push(("ProgramW6432".into(), r"C:\Program Files".into()));
    }
    t.k.env = env;
    for (k, v) in extra {
        set_env(t, k, Some(v));
    }
}

/// An environment variable (names ignore case).
pub(crate) fn env_var(t: &Thread<'_, '_>, name: &str) -> Option<String> {
    t.k.env
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn set_env(t: &mut Thread<'_, '_>, name: &str, value: Option<&str>) {
    let pos =
        t.k.env
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(name));
    match (pos, value) {
        (Some(i), Some(v)) => t.k.env[i].1 = v.to_string(),
        (None, Some(v)) => t.k.env.push((name.to_string(), v.to_string())),
        (Some(i), None) => {
            t.k.env.remove(i);
        }
        (None, None) => {}
    }
}

/// Replace `%NAME%` with the variable's value; unknown names stay as written.
pub(crate) fn expand(t: &Thread<'_, '_>, s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match (!name.is_empty()).then(|| env_var(t, name)).flatten() {
                    Some(v) => {
                        out.push_str(&v);
                        rest = &after[end + 1..];
                    }
                    None => {
                        out.push('%');
                        out.push_str(name);
                        rest = &after[end..];
                    }
                }
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn get_environment_variable(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(name) = read_str(t, a[0], wide) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    match env_var(t, &name) {
        Some(v) => {
            t.set_last_error(ERROR_SUCCESS);
            Ok(copy_str_out(t, a[1], a[2], &v, wide))
        }
        None => fail(t, ERROR_ENVVAR_NOT_FOUND, 0),
    }
}

fn set_environment_variable(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(name) = read_str(t, a[0], wide) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    if name.is_empty() || name[1..].contains('=') {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Ok(value) = opt_str(t, a[1], wide) else {
        return Ok(0);
    };
    if value.is_none() && env_var(t, &name).is_none() {
        return fail(t, ERROR_ENVVAR_NOT_FOUND, 0);
    }
    set_env(t, &name, value.as_deref());
    Ok(1)
}

fn expand_environment_strings(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(src) = read_str(t, a[0], wide) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let out = expand(t, &src);
    let needed = char_len(&out, wide) + 1;
    let size = a[2] as u32 as u64;
    if size >= needed && a[1] != 0 && !write_bytes(t, a[1], &encode(&out, wide)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    // The count includes the terminator, whether or not it fit.
    Ok(needed)
}

/// GetEnvironmentStrings: a heap block of `NAME=value\0` entries ending with
/// an extra `\0`, sorted by name as Windows keeps it.
fn get_environment_strings(t: &mut Thread<'_, '_>, wide: bool) -> Result<u64, Halt> {
    let mut vars = t.k.env.clone();
    vars.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(encode(&format!("{k}={v}"), wide));
    }
    block.extend(encode("", wide));
    let heap = t.k.process_heap;
    let p = crate::memory::heap_alloc(t, heap, block.len() as u64, false);
    if p == 0 {
        return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    write_bytes(t, p, &block);
    Ok(p)
}

fn free_environment_strings(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let heap = t.k.process_heap;
    Ok(crate::memory::heap_free(t, heap, a[0]) as u64)
}

fn set_environment_strings(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let mut env = Vec::new();
    let mut p = a[0];
    for _ in 0..8192 {
        let Some(s) = read_str(t, p, wide) else {
            return fail(t, ERROR_NOACCESS, 0);
        };
        if s.is_empty() {
            break;
        }
        p += (char_len(&s, wide) + 1) * if wide { 2 } else { 1 };
        match s[1..].split_once('=') {
            Some((k, v)) => env.push((format!("{}{k}", &s[..1]), v.to_string())),
            None => return fail(t, ERROR_INVALID_PARAMETER, 0),
        }
    }
    t.k.env = env;
    Ok(1)
}

// ---------------------------------------------------------------------------
// Version

fn get_version(_: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    let (major, minor, build) = VERSION;
    Ok(((build as u64) << 16) | ((minor as u64) << 8) | major as u64)
}

/// Fill an OSVERSIONINFO(EX)A/W, whose first DWORD says which one it is.
fn get_version_ex(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<u64, Halt> {
    let csd = if wide { 256 } else { 128 };
    let base = 20 + csd;
    let Some(size) = read_u32(t, ptr) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let ex = if size as u64 == base {
        false
    } else if size as u64 == base + 8 {
        true
    } else {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
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
    if !write_bytes(t, ptr, &info) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

/// VerifyVersionInfo: compare OSVERSIONINFOEX fields against the reported
/// version with the conditions packed in `mask` (3 bits per type bit).
fn verify_version_info(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (info, types) = (a[0], a[1] as u32);
    let mask = if t.long_mode() {
        a[2]
    } else {
        (a[2] & 0xFFFF_FFFF) | (a[3] << 32)
    };
    let csd = if wide { 256 } else { 128 };
    let sp = 20 + csd;
    let (Some(major), Some(minor), Some(build), Some(platform)) = (
        read_u32(t, info + 4),
        read_u32(t, info + 8),
        read_u32(t, info + 12),
        read_u32(t, info + 16),
    ) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let sp_major = read_u16(t, info + sp).unwrap_or(0) as u64;
    let sp_minor = read_u16(t, info + sp + 2).unwrap_or(0) as u64;
    let product = guest_bytes(t, info + sp + 6, 1).map_or(0, |b| b[0]) as u64;
    let cond = |bit: u32| (mask >> (bit * 3)) & 7;
    let check = |c: u64, have: u64, want: u64| match c {
        1 => have == want,
        2 => have > want,
        3 => have >= want,
        4 => have < want,
        5 => have <= want,
        _ => true,
    };
    let (vmajor, vminor, vbuild) = VERSION;
    let mut ok = true;
    // Major, minor and service pack compare as one version number, using the
    // condition of the most significant one given.
    let order = [
        (2u32, 0x2u32, vmajor as u64, major as u64),
        (1, 0x1, vminor as u64, minor as u64),
        (5, 0x20, 0, sp_major),
        (4, 0x10, 0, sp_minor),
    ];
    let given: Vec<_> = order.iter().filter(|(_, b, _, _)| types & b != 0).collect();
    if let Some(first) = given.first() {
        let c = cond(first.0);
        let have: Vec<u64> = given.iter().map(|g| g.2).collect();
        let want: Vec<u64> = given.iter().map(|g| g.3).collect();
        ok &= match c {
            1 => have == want,
            2 => have > want,
            3 => have >= want,
            4 => have < want,
            5 => have <= want,
            _ => true,
        };
    }
    if types & 0x4 != 0 {
        ok &= check(cond(2), vbuild as u64, build as u64);
    }
    if types & 0x8 != 0 {
        ok &= check(cond(3), VER_PLATFORM_WIN32_NT as u64, platform as u64);
    }
    if types & 0x80 != 0 {
        ok &= check(cond(7), VER_NT_WORKSTATION as u64, product);
    }
    if ok {
        Ok(1)
    } else {
        fail(t, ERROR_OLD_WIN_VERSION, 0)
    }
}

/// VerSetConditionMask: store `cond` in the 3-bit field of each type bit.
fn ver_set_condition_mask(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (mut mask, types, cond) = if t.long_mode() {
        (a[0], a[1] as u32, a[2] & 7)
    } else {
        ((a[0] & 0xFFFF_FFFF) | (a[1] << 32), a[2] as u32, a[3] & 7)
    };
    for bit in 0..8 {
        if types & (1 << bit) != 0 {
            mask &= !(7 << (bit * 3));
            mask |= cond << (bit * 3);
        }
    }
    Ok(mask)
}

fn rtl_get_version(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match get_version_ex(t, a[0], true)? {
        1 => Ok(0),
        _ => Ok(0xC000_000D), // STATUS_INVALID_PARAMETER
    }
}

// ---------------------------------------------------------------------------
// Processor and memory

/// SYSTEM_INFO for the guest's architecture.
fn get_system_info(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let long = t.long_mode();
    let nproc = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
    let nproc = nproc.min(if long { 64 } else { 32 });
    let mask = if nproc >= 64 {
        u64::MAX
    } else {
        (1u64 << nproc) - 1
    };
    let mut b = Vec::new();
    let (arch, ptype, max_addr): (u16, u32, u64) = if long {
        (9, 8664, 0x7FFF_FFFE_FFFF)
    } else {
        (0, 586, 0x7FFE_FFFF)
    };
    b.extend_from_slice(&arch.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&4096u32.to_le_bytes());
    let ptr = |b: &mut Vec<u8>, v: u64| {
        if long {
            b.extend_from_slice(&v.to_le_bytes());
        } else {
            b.extend_from_slice(&(v as u32).to_le_bytes());
        }
    };
    ptr(&mut b, 0x1_0000);
    ptr(&mut b, max_addr);
    ptr(&mut b, mask);
    b.extend_from_slice(&nproc.to_le_bytes());
    b.extend_from_slice(&ptype.to_le_bytes());
    b.extend_from_slice(&0x1_0000u32.to_le_bytes()); // allocation granularity
    b.extend_from_slice(&6u16.to_le_bytes()); // processor level
    b.extend_from_slice(&0x9E0Au16.to_le_bytes()); // processor revision
    write_bytes(t, a[0], &b);
    Ok(0)
}

/// Host physical memory: (total, available) in bytes.
fn host_memory() -> (u64, u64) {
    #[cfg(unix)]
    {
        // SAFETY: sysconf only reads system configuration values.
        let (pages, page) = unsafe {
            (
                libc::sysconf(libc::_SC_PHYS_PAGES),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        if pages > 0 && page > 0 {
            let total = pages as u64 * page as u64;
            #[cfg(target_os = "linux")]
            // SAFETY: as above.
            let avail = unsafe { libc::sysconf(libc::_SC_AVPHYS_PAGES) };
            #[cfg(not(target_os = "linux"))]
            let avail = pages / 2;
            let avail = if avail > 0 {
                avail as u64 * page as u64
            } else {
                total / 2
            };
            return (total, avail);
        }
    }
    (8 << 30, 4 << 30)
}

fn global_memory_status_ex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if read_u32(t, a[0]) != Some(64) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let (total, avail) = host_memory();
    let (vtotal, vavail) = if t.long_mode() {
        (0x7FFF_FFFE_0000u64, 0x7FFF_0000_0000u64)
    } else {
        (0x7FFE_0000, 0x7000_0000)
    };
    let load = (100 - avail * 100 / total.max(1)) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(&64u32.to_le_bytes());
    b.extend_from_slice(&load.to_le_bytes());
    for v in [total, avail, total * 2, avail * 2, vtotal, vavail, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    Ok(write_bytes(t, a[0], &b) as u64)
}

/// MEMORYSTATUS: like the Ex form with SIZE_T fields, capped at 2 GB on x86
/// the way old programs expect.
fn global_memory_status(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let long = t.long_mode();
    let (total, avail) = host_memory();
    let cap = |v: u64| if long { v } else { v.min(0x7FFF_FFFF) };
    let (vtotal, vavail) = if long {
        (0x7FFF_FFFE_0000u64, 0x7FFF_0000_0000u64)
    } else {
        (0x7FFE_0000, 0x7000_0000)
    };
    let size: u32 = if long { 56 } else { 32 };
    let load = (100 - avail * 100 / total.max(1)) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(&size.to_le_bytes());
    b.extend_from_slice(&load.to_le_bytes());
    for v in [total, avail, total * 2, avail * 2, vtotal, vavail] {
        let v = cap(v);
        if long {
            b.extend_from_slice(&v.to_le_bytes());
        } else {
            b.extend_from_slice(&(v as u32).to_le_bytes());
        }
    }
    write_bytes(t, a[0], &b);
    Ok(0)
}

fn processor_feature(f: u32) -> bool {
    // CMPXCHG8B, MMX, SSE, RDTSC, SSE2, NX, fast fail.
    matches!(f, 2 | 3 | 6 | 8 | 10 | 12 | 23)
}

// ---------------------------------------------------------------------------
// Names and directories

fn system_dir_api(t: &mut Thread<'_, '_>, a: &[u64], dir: &str, wide: bool) -> Result<u64, Halt> {
    Ok(copy_str_out(t, a[0], a[1], dir, wide))
}

fn get_computer_name_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    // DNS names are lower case; the NetBIOS ones upper case.
    let name = match a[0] {
        1..=3 | 5..=7 => COMPUTER_NAME.to_ascii_lowercase(),
        _ => COMPUTER_NAME.to_string(),
    };
    let name = if matches!(a[0], 2 | 6) {
        String::new()
    } else {
        name
    };
    copy_str_inout(t, a[1], a[2], &name, wide, false)
}

// ---------------------------------------------------------------------------
// Atoms

fn atom_name_arg(t: &mut Thread<'_, '_>, p: u64, wide: bool) -> Result<Result<String, u32>, ()> {
    if p < 0x1_0000 {
        return Ok(Err(p as u32)); // MAKEINTATOM
    }
    let s = read_str(t, p, wide).ok_or(())?;
    if let Some(n) = s.strip_prefix('#').and_then(|n| n.parse::<u32>().ok()) {
        return Ok(Err(n));
    }
    Ok(Ok(s))
}

fn add_atom(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let name = match atom_name_arg(t, a[0], wide) {
        Ok(Ok(s)) => s,
        Ok(Err(n)) if (1..MAXINTATOM).contains(&n) => return Ok(n as u64),
        Ok(Err(_)) => return fail(t, ERROR_INVALID_PARAMETER, 0),
        Err(()) => return fail(t, ERROR_NOACCESS, 0),
    };
    if name.is_empty() || name.len() > 255 {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    if let Some(i) = t.k.atoms.iter().position(|e| {
        e.as_ref()
            .is_some_and(|(n, _)| n.eq_ignore_ascii_case(&name))
    }) {
        if let Some((_, refs)) = t.k.atoms[i].as_mut() {
            *refs += 1;
        }
        return Ok(MAXINTATOM as u64 + i as u64);
    }
    let i = match t.k.atoms.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            t.k.atoms.push(None);
            t.k.atoms.len() - 1
        }
    };
    t.k.atoms[i] = Some((name, 1));
    Ok(MAXINTATOM as u64 + i as u64)
}

fn find_atom(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let name = match atom_name_arg(t, a[0], wide) {
        Ok(Ok(s)) => s,
        Ok(Err(n)) if (1..MAXINTATOM).contains(&n) => return Ok(n as u64),
        Ok(Err(_)) => return fail(t, ERROR_INVALID_PARAMETER, 0),
        Err(()) => return fail(t, ERROR_NOACCESS, 0),
    };
    match t.k.atoms.iter().position(|e| {
        e.as_ref()
            .is_some_and(|(n, _)| n.eq_ignore_ascii_case(&name))
    }) {
        Some(i) => Ok(MAXINTATOM as u64 + i as u64),
        None => fail(t, ERROR_FILE_NOT_FOUND, 0),
    }
}

fn atom_slot(atom: u64) -> Option<usize> {
    let atom = atom as u16 as u32;
    (atom >= MAXINTATOM).then(|| (atom - MAXINTATOM) as usize)
}

fn delete_atom(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if let Some(i) = atom_slot(a[0]) {
        let gone = match t.k.atoms.get_mut(i) {
            Some(Some((_, refs))) => {
                *refs -= 1;
                *refs == 0
            }
            _ => return fail(t, ERROR_INVALID_HANDLE, a[0]),
        };
        if gone {
            t.k.atoms[i] = None;
        }
    }
    Ok(0)
}

fn get_atom_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let name = match atom_slot(a[0]) {
        None if a[0] as u16 != 0 => format!("#{}", a[0] as u16),
        None => return fail(t, ERROR_INVALID_PARAMETER, 0),
        Some(i) => match t.k.atoms.get(i) {
            Some(Some((n, _))) => n.clone(),
            _ => return fail(t, ERROR_INVALID_HANDLE, 0),
        },
    };
    let (n, _) = copy_str_truncate(t, a[1], a[2] as u32 as u64, &name, wide);
    Ok(n)
}

// ---------------------------------------------------------------------------
// FormatMessage

const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x100;
const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x200;
const FORMAT_MESSAGE_FROM_STRING: u32 = 0x400;
const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x1000;
const FORMAT_MESSAGE_ARGUMENT_ARRAY: u32 = 0x2000;
const FORMAT_MESSAGE_MAX_WIDTH_MASK: u32 = 0xFF;

/// The system message for a Win32 error code.
pub(crate) fn system_message(code: u32) -> Option<&'static str> {
    Some(match code {
        0 => "The operation completed successfully.",
        1 => "Incorrect function.",
        2 => "The system cannot find the file specified.",
        3 => "The system cannot find the path specified.",
        4 => "The system cannot open the file.",
        5 => "Access is denied.",
        6 => "The handle is invalid.",
        8 => "Not enough memory resources are available to process this command.",
        13 => "The data is invalid.",
        14 => "Not enough memory resources are available to complete this operation.",
        18 => "There are no more files.",
        32 => "The process cannot access the file because it is being used by another process.",
        50 => "The request is not supported.",
        80 => "The file exists.",
        87 => "The parameter is incorrect.",
        109 => "The pipe has been ended.",
        112 => "There is not enough space on the disk.",
        120 => "This function is not supported on this system.",
        122 => "The data area passed to a system call is too small.",
        123 => "The filename, directory name, or volume label syntax is incorrect.",
        126 => "The specified module could not be found.",
        127 => "The specified procedure could not be found.",
        145 => "The directory is not empty.",
        183 => "Cannot create a file when that file already exists.",
        193 => "%1 is not a valid Win32 application.",
        203 => "The system could not find the environment option that was entered.",
        234 => "More data is available.",
        259 => "No more data is available.",
        487 => "Attempt to access invalid address.",
        998 => "Invalid access to memory location.",
        1150 => "The specified program requires a newer version of Windows.",
        1460 => "This operation returned because the timeout period expired.",
        _ => return None,
    })
}

/// Expand a message template: `%1`..`%99` inserts (with optional `!fmt!`),
/// `%n`, `%t`, `%0` and escapes.
fn format_message_template(
    t: &Thread<'_, '_>,
    template: &str,
    args: Option<(u64, bool)>,
    max_width: u32,
    wide: bool,
) -> String {
    let p = t.ptr_size() as u64;
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\r' && chars.get(i + 1) == Some(&'\n') && max_width != 0 {
            // Line breaks in the template are kept only without a width limit.
            out.push_str(if max_width == FORMAT_MESSAGE_MAX_WIDTH_MASK {
                " "
            } else {
                "\r\n"
            });
            i += 2;
            continue;
        }
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&d) = chars.get(i) else { break };
        match d {
            '0' => return out,
            'n' => out.push_str("\r\n"),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push(' '),
            '1'..='9' => {
                let mut n = d.to_digit(10).unwrap_or(0);
                if let Some(e) = chars.get(i + 1).and_then(|c| c.to_digit(10)) {
                    n = n * 10 + e;
                    i += 1;
                }
                let mut fmt = String::from("s");
                if chars.get(i + 1) == Some(&'!') {
                    if let Some(end) = chars[i + 2..].iter().position(|&c| c == '!') {
                        fmt = chars[i + 2..i + 2 + end].iter().collect();
                        i += end + 2;
                    }
                }
                match args {
                    None => {
                        out.push('%');
                        out.push_str(&n.to_string());
                        if fmt != "s" {
                            out.push_str(&format!("!{fmt}!"));
                        }
                    }
                    Some((base, _)) => {
                        let at = base + (n as u64 - 1) * p;
                        let fmt_units: Vec<u16> = format!("%{fmt}").encode_utf16().collect();
                        let mut va = crate::strings::Args::VaList(at);
                        let s = crate::strings::wsprintf_format(t, &fmt_units, &mut va, wide);
                        out.push_str(&String::from_utf16_lossy(&s));
                    }
                }
            }
            other => out.push(other), // %% %. %! and friends
        }
        i += 1;
    }
    out
}

fn format_message(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (flags, source, id, buf, size, args) = (a[0] as u32, a[1], a[2] as u32, a[4], a[5], a[6]);
    let template = if flags & FORMAT_MESSAGE_FROM_STRING != 0 {
        match read_str(t, source, wide) {
            Some(s) => s,
            None => return fail(t, ERROR_NOACCESS, 0),
        }
    } else if flags & FORMAT_MESSAGE_FROM_SYSTEM != 0 {
        // HRESULT_FROM_WIN32 codes map to the Win32 message.
        let code = if id & 0xFFFF_0000 == 0x8007_0000 {
            id & 0xFFFF
        } else {
            id
        };
        match system_message(code) {
            Some(m) => format!("{m}\r\n"),
            None => return fail(t, ERROR_MR_MID_NOT_FOUND, 0),
        }
    } else {
        // Message tables in modules are not read yet.
        return fail(t, ERROR_MR_MID_NOT_FOUND, 0);
    };
    let inserts = if flags & FORMAT_MESSAGE_IGNORE_INSERTS != 0 || args == 0 {
        None
    } else if flags & FORMAT_MESSAGE_ARGUMENT_ARRAY != 0 {
        Some((args, true))
    } else {
        // A pointer to a va_list, which is itself a pointer to the arguments.
        read_ptr(t, args).map(|va| (va, false))
    };
    let text = format_message_template(
        t,
        &template,
        inserts,
        flags & FORMAT_MESSAGE_MAX_WIDTH_MASK,
        wide,
    );
    let encoded = encode(&text, wide);
    let len = char_len(&text, wide);
    if flags & FORMAT_MESSAGE_ALLOCATE_BUFFER != 0 {
        let heap = t.k.process_heap;
        let n = (encoded.len() as u64).max(size * if wide { 2 } else { 1 });
        let p = crate::memory::heap_alloc(t, heap, n, true);
        if p == 0 {
            return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        write_bytes(t, p, &encoded);
        if !write_ptr(t, buf, p) {
            return fail(t, ERROR_NOACCESS, 0);
        }
        return Ok(len);
    }
    if (size as u32 as u64) <= len {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, buf, &encoded) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(len)
}

// ---------------------------------------------------------------------------
// Random numbers (advapi32!SystemFunction036, a.k.a. RtlGenRandom)

pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok()
    {
        return buf;
    }
    // No urandom (Windows hosts): a hash of the time and a counter.
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let state = RandomState::new();
    for (i, chunk) in buf.chunks_mut(8).enumerate() {
        let mut h = state.build_hasher();
        h.write_usize(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    buf
}

pub(crate) fn gen_random(t: &mut Thread<'_, '_>, buf: u64, len: u64) -> bool {
    let len = len.min(1 << 20) as usize;
    let data = random_bytes(len);
    write_bytes(t, buf, &data)
}

pub(crate) static APIS: &[Api] = &[
    k32("GetEnvironmentVariableA", 3, |t, a| {
        get_environment_variable(t, a, false)
    }),
    k32("GetEnvironmentVariableW", 3, |t, a| {
        get_environment_variable(t, a, true)
    }),
    k32("SetEnvironmentVariableA", 2, |t, a| {
        set_environment_variable(t, a, false)
    }),
    k32("SetEnvironmentVariableW", 2, |t, a| {
        set_environment_variable(t, a, true)
    }),
    k32("ExpandEnvironmentStringsA", 3, |t, a| {
        expand_environment_strings(t, a, false)
    }),
    k32("ExpandEnvironmentStringsW", 3, |t, a| {
        expand_environment_strings(t, a, true)
    }),
    k32("GetEnvironmentStrings", 0, |t, _| {
        get_environment_strings(t, false)
    }),
    k32("GetEnvironmentStringsA", 0, |t, _| {
        get_environment_strings(t, false)
    }),
    k32("GetEnvironmentStringsW", 0, |t, _| {
        get_environment_strings(t, true)
    }),
    k32("FreeEnvironmentStringsA", 1, free_environment_strings),
    k32("FreeEnvironmentStringsW", 1, free_environment_strings),
    k32("SetEnvironmentStringsA", 1, |t, a| {
        set_environment_strings(t, a, false)
    }),
    k32("SetEnvironmentStringsW", 1, |t, a| {
        set_environment_strings(t, a, true)
    }),
    k32("GetVersion", 0, get_version),
    k32("GetVersionExA", 1, |t, a| get_version_ex(t, a[0], false)),
    k32("GetVersionExW", 1, |t, a| get_version_ex(t, a[0], true)),
    k32x("VerifyVersionInfoA", 3, 4, |t, a| {
        verify_version_info(t, a, false)
    }),
    k32x("VerifyVersionInfoW", 3, 4, |t, a| {
        verify_version_info(t, a, true)
    }),
    k32x("VerSetConditionMask", 3, 4, ver_set_condition_mask),
    api("ntdll", "VerSetConditionMask", 3, ver_set_condition_mask),
    api("ntdll", "RtlGetVersion", 1, rtl_get_version),
    api("ntdll", "RtlGetNtVersionNumbers", 3, |t, a| {
        let (major, minor, build) = VERSION;
        store_u32(t, a[0], major);
        store_u32(t, a[1], minor);
        store_u32(t, a[2], 0xF000_0000 | build);
        Ok(0)
    }),
    k32("GetProductInfo", 5, |t, a| {
        Ok(u64::from(store_u32(t, a[4], 0x30)))
    }), // PRODUCT_PROFESSIONAL
    k32("GetSystemInfo", 1, get_system_info),
    k32("GetNativeSystemInfo", 1, get_system_info),
    k32("IsProcessorFeaturePresent", 1, |_, a| {
        Ok(processor_feature(a[0] as u32) as u64)
    }),
    k32("GetActiveProcessorCount", 1, |_, _| {
        Ok(std::thread::available_parallelism().map_or(1, |n| n.get()) as u64)
    }),
    k32("GetMaximumProcessorCount", 1, |_, _| {
        Ok(std::thread::available_parallelism().map_or(1, |n| n.get()) as u64)
    }),
    k32("GetActiveProcessorGroupCount", 0, |_, _| Ok(1)),
    k32("GetMaximumProcessorGroupCount", 0, |_, _| Ok(1)),
    k32("GetNumaHighestNodeNumber", 1, |t, a| {
        Ok(u64::from(store_u32(t, a[0], 0)))
    }),
    k32("GetLogicalProcessorInformation", 2, |t, _| {
        fail(t, ERROR_NOT_SUPPORTED, 0)
    }),
    k32("GetLogicalProcessorInformationEx", 3, |t, _| {
        fail(t, ERROR_NOT_SUPPORTED, 0)
    }),
    k32("GlobalMemoryStatus", 1, global_memory_status),
    k32("GlobalMemoryStatusEx", 1, global_memory_status_ex),
    k32("GetPhysicallyInstalledSystemMemory", 1, |t, a| {
        let kb = host_memory().0 / 1024;
        Ok(write_u64(t, a[0], kb) as u64)
    }),
    k32("GetWindowsDirectoryA", 2, |t, a| {
        system_dir_api(t, a, WINDOWS_DIR, false)
    }),
    k32("GetWindowsDirectoryW", 2, |t, a| {
        system_dir_api(t, a, WINDOWS_DIR, true)
    }),
    k32("GetSystemWindowsDirectoryA", 2, |t, a| {
        system_dir_api(t, a, WINDOWS_DIR, false)
    }),
    k32("GetSystemWindowsDirectoryW", 2, |t, a| {
        system_dir_api(t, a, WINDOWS_DIR, true)
    }),
    k32("GetSystemDirectoryA", 2, |t, a| {
        system_dir_api(t, a, SYSTEM_DIR, false)
    }),
    k32("GetSystemDirectoryW", 2, |t, a| {
        system_dir_api(t, a, SYSTEM_DIR, true)
    }),
    // Not a WOW64 process, so there is no WOW64 system directory to report.
    k32("GetSystemWow64DirectoryA", 2, |t, _| {
        fail(t, ERROR_CALL_NOT_IMPLEMENTED, 0)
    }),
    k32("GetSystemWow64DirectoryW", 2, |t, _| {
        fail(t, ERROR_CALL_NOT_IMPLEMENTED, 0)
    }),
    k32("GetComputerNameA", 2, |t, a| {
        copy_str_inout(t, a[0], a[1], COMPUTER_NAME, false, false)
    }),
    k32("GetComputerNameW", 2, |t, a| {
        copy_str_inout(t, a[0], a[1], COMPUTER_NAME, true, false)
    }),
    k32("GetComputerNameExA", 3, |t, a| {
        get_computer_name_ex(t, a, false)
    }),
    k32("GetComputerNameExW", 3, |t, a| {
        get_computer_name_ex(t, a, true)
    }),
    k32("SetComputerNameA", 1, |t, _| {
        fail(t, ERROR_ACCESS_DENIED, 0)
    }),
    k32("SetComputerNameW", 1, |t, _| {
        fail(t, ERROR_ACCESS_DENIED, 0)
    }),
    k32("AddAtomA", 1, |t, a| add_atom(t, a, false)),
    k32("AddAtomW", 1, |t, a| add_atom(t, a, true)),
    k32("GlobalAddAtomA", 1, |t, a| add_atom(t, a, false)),
    k32("GlobalAddAtomW", 1, |t, a| add_atom(t, a, true)),
    k32("FindAtomA", 1, |t, a| find_atom(t, a, false)),
    k32("FindAtomW", 1, |t, a| find_atom(t, a, true)),
    k32("GlobalFindAtomA", 1, |t, a| find_atom(t, a, false)),
    k32("GlobalFindAtomW", 1, |t, a| find_atom(t, a, true)),
    k32("DeleteAtom", 1, delete_atom),
    k32("GlobalDeleteAtom", 1, delete_atom),
    k32("GetAtomNameA", 3, |t, a| get_atom_name(t, a, false)),
    k32("GetAtomNameW", 3, |t, a| get_atom_name(t, a, true)),
    k32("GlobalGetAtomNameA", 3, |t, a| get_atom_name(t, a, false)),
    k32("GlobalGetAtomNameW", 3, |t, a| get_atom_name(t, a, true)),
    k32("InitAtomTable", 1, |_, _| Ok(1)),
    k32("FormatMessageA", 7, |t, a| format_message(t, a, false)),
    k32("FormatMessageW", 7, |t, a| format_message(t, a, true)),
];

/// advapi32 exports that are not registry functions.
/// Superseded by the ADVAPI32 table in api.rs; kept for reference.
#[allow(dead_code)]
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
        Ok(gen_random(t, a[0], a[1] as u32 as u64) as u64)
    }),
    api("bcryptprimitives", "ProcessPrng", 2, |t, a| {
        Ok(gen_random(t, a[0], a[1]) as u64)
    }),
    api("bcrypt", "BCryptGenRandom", 4, |t, a| {
        // STATUS_SUCCESS / STATUS_INVALID_PARAMETER
        Ok(if gen_random(t, a[1], a[2] as u32 as u64) {
            0
        } else {
            0xC000_000D
        })
    }),
    // A token that says what it can: the user is an administrator (installers
    // check), and other queries are not supported.
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
    api("shell32", "IsUserAnAdmin", 0, |_, _| Ok(1)),
];
