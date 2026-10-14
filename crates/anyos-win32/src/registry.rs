//! The registry functions of advapi32, backed by the environment's
//! `registry.toml` (see `anyos_env::Registry`).
//!
//! The registry is loaded when the program starts and written back when it
//! ends, if anything changed. Without an environment it lives only as long as
//! the run. Registry functions return their error code instead of setting the
//! last error.

use anyos_env::RegValue;

use crate::api::*;
use crate::thread::{Halt, Thread};

const HKEY_CLASSES_ROOT: u32 = 0x8000_0000;
const HKEY_CURRENT_USER: u32 = 0x8000_0001;
const HKEY_LOCAL_MACHINE: u32 = 0x8000_0002;
const HKEY_USERS: u32 = 0x8000_0003;
const HKEY_CURRENT_CONFIG: u32 = 0x8000_0005;

const REG_NONE: u32 = 0;
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;
const REG_BINARY: u32 = 3;
const REG_DWORD: u32 = 4;
const REG_MULTI_SZ: u32 = 7;
const REG_QWORD: u32 = 11;

const REG_CREATED_NEW_KEY: u32 = 1;
const REG_OPENED_EXISTING_KEY: u32 = 2;
const RRF_NOEXPAND: u32 = 0x1000_0000;
const RRF_ZEROONFAILURE: u32 = 0x2000_0000;
const ERROR_UNSUPPORTED_TYPE: u32 = 1630;
const ERROR_BADKEY: u32 = 1010;

/// First handle value for opened keys, well away from kernel handles.
const FIRST_KEY: u32 = 0x0070_0000;

/// Load the environment's registry and add the keys every Windows install has.
pub(crate) fn init(t: &mut Thread<'_, '_>) {
    if let Some(path) = t.host.registry_file.clone() {
        if path.exists() {
            match anyos_env::Registry::load(&path) {
                Ok(r) => t.k.registry = r,
                Err(e) => t.trace_note(&format!("registry not loaded: {e}")),
            }
        }
    }
    let user = t.host.user.clone();
    let long = t.long_mode();
    let reg = &mut t.k.registry;
    let mut seed = |key: &str, name: &str, value: RegValue| {
        if reg.get(key, name).is_none() {
            reg.set(key, name, value);
        }
    };
    let nt = r"HKLM\Software\Microsoft\Windows NT\CurrentVersion";
    seed(nt, "ProductName", RegValue::Sz("Windows 10 Pro".into()));
    seed(nt, "EditionID", RegValue::Sz("Professional".into()));
    seed(nt, "InstallationType", RegValue::Sz("Client".into()));
    seed(nt, "CurrentVersion", RegValue::Sz("6.3".into()));
    seed(nt, "CurrentBuild", RegValue::Sz("19045".into()));
    seed(nt, "CurrentBuildNumber", RegValue::Sz("19045".into()));
    seed(nt, "CurrentMajorVersionNumber", RegValue::Dword(10));
    seed(nt, "CurrentMinorVersionNumber", RegValue::Dword(0));
    seed(nt, "ReleaseId", RegValue::Sz("2009".into()));
    seed(nt, "DisplayVersion", RegValue::Sz("22H2".into()));
    seed(nt, "SystemRoot", RegValue::Sz(r"C:\Windows".into()));
    seed(nt, "RegisteredOwner", RegValue::Sz(user));
    seed(nt, "RegisteredOrganization", RegValue::Sz(String::new()));
    let cv = r"HKLM\Software\Microsoft\Windows\CurrentVersion";
    seed(
        cv,
        "ProgramFilesDir",
        RegValue::Sz(r"C:\Program Files".into()),
    );
    seed(
        cv,
        "CommonFilesDir",
        RegValue::Sz(r"C:\Program Files\Common Files".into()),
    );
    if long {
        seed(
            cv,
            "ProgramFilesDir (x86)",
            RegValue::Sz(r"C:\Program Files (x86)".into()),
        );
    }
    seed(
        cv,
        "ProgramW6432Dir",
        RegValue::Sz(r"C:\Program Files".into()),
    );
    let cpu = r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0";
    seed(
        cpu,
        "ProcessorNameString",
        RegValue::Sz("AnyOS Virtual CPU".into()),
    );
    seed(cpu, "~MHz", RegValue::Dword(3000));
    seed(
        cpu,
        "Identifier",
        RegValue::Sz("Intel64 Family 6 Model 158 Stepping 10".into()),
    );
    seed(cpu, "VendorIdentifier", RegValue::Sz("GenuineIntel".into()));
    for key in [
        r"HKCU\Software",
        r"HKCU\Environment",
        r"HKLM\Software\Classes",
        r"HKU\.DEFAULT",
    ] {
        reg.create_key(key);
    }
}

/// Save the registry if the program changed it.
pub(crate) fn shutdown(t: &mut Thread<'_, '_>) {
    if !t.k.registry_dirty {
        return;
    }
    if let Some(path) = t.host.registry_file.clone() {
        if let Err(e) = t.k.registry.save(&path) {
            t.trace_note(&format!("registry not saved: {e}"));
        }
    }
    t.k.registry_dirty = false;
}

/// The key path an `HKEY` refers to.
fn key_path(t: &Thread<'_, '_>, hkey: u64) -> Option<String> {
    let h = hkey as u32;
    Some(
        match h {
            HKEY_CLASSES_ROOT => r"HKEY_LOCAL_MACHINE\Software\Classes",
            HKEY_CURRENT_USER => "HKEY_CURRENT_USER",
            HKEY_LOCAL_MACHINE => "HKEY_LOCAL_MACHINE",
            HKEY_USERS => "HKEY_USERS",
            HKEY_CURRENT_CONFIG => {
                r"HKEY_LOCAL_MACHINE\System\CurrentControlSet\Hardware Profiles\Current"
            }
            _ => return t.k.reg_keys.get(&h).cloned(),
        }
        .to_string(),
    )
}

fn join(base: &str, sub: &str) -> String {
    let sub = sub.trim_matches('\\');
    if sub.is_empty() {
        base.to_string()
    } else {
        format!(r"{base}\{sub}")
    }
}

fn is_root(path: &str) -> bool {
    !path.contains('\\')
}

fn key_exists(t: &Thread<'_, '_>, path: &str) -> bool {
    is_root(path) || t.k.registry.key_exists(path) || !t.k.registry.subkeys(path).is_empty()
}

fn new_key_handle(t: &mut Thread<'_, '_>, path: String) -> u64 {
    if t.k.next_reg_key < FIRST_KEY {
        t.k.next_reg_key = FIRST_KEY;
    }
    let h = t.k.next_reg_key;
    t.k.next_reg_key += 4;
    t.k.reg_keys.insert(h, path);
    h as u64
}

/// The key `hkey\sub` names, for functions taking an optional subkey.
fn sub_path(t: &mut Thread<'_, '_>, hkey: u64, sub: u64, wide: bool) -> Result<String, u32> {
    let base = key_path(t, hkey).ok_or(ERROR_INVALID_HANDLE)?;
    let sub = opt_str(t, sub, wide).map_err(|()| ERROR_NOACCESS)?;
    Ok(join(&base, sub.as_deref().unwrap_or("")))
}

fn value_name(t: &mut Thread<'_, '_>, p: u64, wide: bool) -> Result<String, u32> {
    opt_str(t, p, wide)
        .map(Option::unwrap_or_default)
        .map_err(|()| ERROR_NOACCESS)
}

/// A value's type and its bytes as an A or W function returns them.
fn value_bytes(v: &RegValue, wide: bool) -> (u32, Vec<u8>) {
    match v {
        RegValue::Sz(s) => (REG_SZ, encode(s, wide)),
        RegValue::ExpandSz(s) => (REG_EXPAND_SZ, encode(s, wide)),
        RegValue::MultiSz(list) => {
            let mut b: Vec<u8> = list.iter().flat_map(|s| encode(s, wide)).collect();
            b.extend(encode("", wide));
            (REG_MULTI_SZ, b)
        }
        RegValue::Dword(d) => (REG_DWORD, d.to_le_bytes().to_vec()),
        RegValue::Qword(q) => (REG_QWORD, q.to_le_bytes().to_vec()),
        RegValue::Binary(b) => (REG_BINARY, b.clone()),
    }
}

/// Decode the strings in registry data (NUL-separated).
fn decode_strings(data: &[u8], wide: bool) -> Vec<String> {
    let text = if wide {
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        crate::strings::ansi_to_string(data)
    };
    let mut parts: Vec<String> = text.split('\0').map(str::to_string).collect();
    while parts.last().is_some_and(String::is_empty) {
        parts.pop();
    }
    parts
}

/// The value a program writes, from its type and raw bytes.
fn value_from(kind: u32, data: &[u8], wide: bool) -> RegValue {
    match kind {
        REG_SZ => RegValue::Sz(
            decode_strings(data, wide)
                .into_iter()
                .next()
                .unwrap_or_default(),
        ),
        REG_EXPAND_SZ => RegValue::ExpandSz(
            decode_strings(data, wide)
                .into_iter()
                .next()
                .unwrap_or_default(),
        ),
        REG_MULTI_SZ => RegValue::MultiSz(decode_strings(data, wide)),
        REG_DWORD if data.len() >= 4 => {
            RegValue::Dword(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
        }
        REG_QWORD if data.len() >= 8 => {
            RegValue::Qword(u64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8])))
        }
        _ => RegValue::Binary(data.to_vec()),
    }
}

/// Hand value data back through `(type, data, size)` pointers.
fn return_value(
    t: &mut Thread<'_, '_>,
    kind: u32,
    bytes: &[u8],
    type_ptr: u64,
    data: u64,
    size_ptr: u64,
) -> u64 {
    store_u32(t, type_ptr, kind);
    if size_ptr == 0 {
        return if data == 0 {
            0
        } else {
            ERROR_INVALID_PARAMETER as u64
        };
    }
    let Some(size) = read_u32(t, size_ptr) else {
        return ERROR_NOACCESS as u64;
    };
    write_u32(t, size_ptr, bytes.len() as u32);
    if data == 0 {
        return 0;
    }
    if (size as usize) < bytes.len() {
        return ERROR_MORE_DATA as u64;
    }
    if !write_bytes(t, data, bytes) {
        return ERROR_NOACCESS as u64;
    }
    0
}

fn changed(t: &mut Thread<'_, '_>) {
    t.k.registry_dirty = true;
}

// ---------------------------------------------------------------------------
// Keys

fn open_key(t: &mut Thread<'_, '_>, hkey: u64, sub: u64, out: u64, wide: bool) -> u64 {
    let path = match sub_path(t, hkey, sub, wide) {
        Ok(p) => p,
        Err(e) => return e as u64,
    };
    if !key_exists(t, &path) {
        return ERROR_FILE_NOT_FOUND as u64;
    }
    let h = new_key_handle(t, path);
    if !write_ptr(t, out, h) {
        return ERROR_NOACCESS as u64;
    }
    0
}

fn create_key(
    t: &mut Thread<'_, '_>,
    hkey: u64,
    sub: u64,
    out: u64,
    disposition: u64,
    wide: bool,
) -> u64 {
    let path = match sub_path(t, hkey, sub, wide) {
        Ok(p) => p,
        Err(e) => return e as u64,
    };
    let existed = key_exists(t, &path);
    if !existed {
        // Like Windows, create every missing key on the way.
        let parts: Vec<&str> = path.split('\\').collect();
        for n in 2..=parts.len() {
            let key = parts[..n].join("\\");
            if !t.k.registry.key_exists(&key) {
                t.k.registry.create_key(&key);
            }
        }
        changed(t);
    }
    store_u32(
        t,
        disposition,
        if existed {
            REG_OPENED_EXISTING_KEY
        } else {
            REG_CREATED_NEW_KEY
        },
    );
    let h = new_key_handle(t, path);
    if !write_ptr(t, out, h) {
        return ERROR_NOACCESS as u64;
    }
    0
}

fn close_key(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let h = a[0] as u32;
    if h >= HKEY_CLASSES_ROOT || t.k.reg_keys.remove(&h).is_some() {
        Ok(0)
    } else {
        Ok(ERROR_INVALID_HANDLE as u64)
    }
}

fn delete_key(t: &mut Thread<'_, '_>, hkey: u64, sub: u64, wide: bool) -> u64 {
    let path = match sub_path(t, hkey, sub, wide) {
        Ok(p) => p,
        Err(e) => return e as u64,
    };
    if is_root(&path) {
        return ERROR_ACCESS_DENIED as u64;
    }
    if !key_exists(t, &path) {
        return ERROR_FILE_NOT_FOUND as u64;
    }
    if !t.k.registry.subkeys(&path).is_empty() {
        return ERROR_ACCESS_DENIED as u64;
    }
    t.k.registry.delete_tree(&path);
    changed(t);
    0
}

fn delete_tree(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    if !key_exists(t, &path) {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    }
    if a[1] == 0 || is_root(&path) {
        // Without a subkey, empty the key itself but keep it.
        for k in t.k.registry.subkeys(&path) {
            t.k.registry.delete_tree(&join(&path, &k));
        }
        for (n, _) in t.k.registry.values(&path) {
            t.k.registry.delete_value(&path, &n);
        }
    } else {
        t.k.registry.delete_tree(&path);
    }
    changed(t);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Values

fn query_value_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    let name = match value_name(t, a[1], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    let Some(v) = t.k.registry.get(&path, &name).cloned() else {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    };
    let (kind, bytes) = value_bytes(&v, wide);
    Ok(return_value(t, kind, &bytes, a[3], a[4], a[5]))
}

/// RRF_RT_* bit for a value type.
fn rrf_bit(kind: u32) -> u32 {
    match kind {
        REG_NONE => 0x1,
        REG_SZ => 0x2,
        REG_EXPAND_SZ => 0x4,
        REG_BINARY => 0x8,
        REG_DWORD => 0x10,
        REG_MULTI_SZ => 0x20,
        REG_QWORD => 0x40,
        _ => 0,
    }
}

fn get_value(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (flags, type_ptr, data, size_ptr) = (a[3] as u32, a[4], a[5], a[6]);
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    if !key_exists(t, &path) {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    }
    let name = match value_name(t, a[2], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    let zero_on_failure = |t: &mut Thread<'_, '_>| {
        if flags & RRF_ZEROONFAILURE != 0 && data != 0 {
            if let Some(n) = read_u32(t, size_ptr) {
                write_bytes(t, data, &vec![0; n.min(1 << 16) as usize]);
            }
        }
    };
    let Some(v) = t.k.registry.get(&path, &name).cloned() else {
        zero_on_failure(t);
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    };
    let v = match v {
        RegValue::ExpandSz(s) if flags & RRF_NOEXPAND == 0 => {
            RegValue::Sz(crate::sysinfo::expand(t, &s))
        }
        v => v,
    };
    let (kind, bytes) = value_bytes(&v, wide);
    // An expanded REG_EXPAND_SZ still passes an RRF_RT_REG_EXPAND_SZ filter.
    let allowed = flags & 0xFFFF;
    let passes = allowed & rrf_bit(kind) != 0
        || (kind == REG_SZ && flags & RRF_NOEXPAND == 0 && allowed & 0x4 != 0);
    if !passes {
        zero_on_failure(t);
        return Ok(ERROR_UNSUPPORTED_TYPE as u64);
    }
    Ok(return_value(t, kind, &bytes, type_ptr, data, size_ptr))
}

fn set_value(
    t: &mut Thread<'_, '_>,
    path: &str,
    name: &str,
    kind: u32,
    data: u64,
    size: u64,
    wide: bool,
) -> u64 {
    let Some(bytes) = guest_bytes(t, data, size.min(1 << 24)) else {
        return ERROR_NOACCESS as u64;
    };
    let v = value_from(kind, &bytes, wide);
    t.k.registry.set(path, name, v);
    changed(t);
    0
}

fn set_value_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    if is_root(&path) {
        return Ok(ERROR_ACCESS_DENIED as u64);
    }
    let name = match value_name(t, a[1], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    Ok(set_value(
        t,
        &path,
        &name,
        a[3] as u32,
        a[4],
        a[5] as u32 as u64,
        wide,
    ))
}

fn set_key_value(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    let name = match value_name(t, a[2], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    Ok(set_value(
        t,
        &path,
        &name,
        a[3] as u32,
        a[4],
        a[5] as u32 as u64,
        wide,
    ))
}

/// RegSetValue: the default value of a subkey, always a string.
fn set_value_old(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    if a[2] as u32 != REG_SZ {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    let Some(s) = read_str(t, a[3], wide) else {
        return Ok(ERROR_NOACCESS as u64);
    };
    t.k.registry.set(&path, "", RegValue::Sz(s));
    changed(t);
    Ok(0)
}

/// RegQueryValue: the default value of a subkey, as a string.
fn query_value_old(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    if !key_exists(t, &path) {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    }
    let s = match t.k.registry.get(&path, "") {
        Some(RegValue::Sz(s) | RegValue::ExpandSz(s)) => s.clone(),
        _ => String::new(),
    };
    Ok(return_value(t, REG_SZ, &encode(&s, wide), 0, a[2], a[3]))
}

fn delete_value(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    let name = match value_name(t, a[1], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    if !t.k.registry.delete_value(&path, &name) {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    }
    changed(t);
    Ok(0)
}

fn delete_key_value(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let path = match sub_path(t, a[0], a[1], wide) {
        Ok(p) => p,
        Err(e) => return Ok(e as u64),
    };
    let name = match value_name(t, a[2], wide) {
        Ok(n) => n,
        Err(e) => return Ok(e as u64),
    };
    if !t.k.registry.delete_value(&path, &name) {
        return Ok(ERROR_FILE_NOT_FOUND as u64);
    }
    changed(t);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Enumeration

/// Copy a name into a buffer whose size (in characters, including the
/// terminator) is passed and returned through `len_ptr`.
fn return_name(t: &mut Thread<'_, '_>, name: &str, buf: u64, len_ptr: u64, wide: bool) -> u64 {
    let Some(size) = read_u32(t, len_ptr) else {
        return ERROR_NOACCESS as u64;
    };
    let len = char_len(name, wide) as u32;
    if size <= len {
        return ERROR_MORE_DATA as u64;
    }
    if !write_bytes(t, buf, &encode(name, wide)) {
        return ERROR_NOACCESS as u64;
    }
    write_u32(t, len_ptr, len);
    0
}

fn enum_key_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    let keys = t.k.registry.subkeys(&path);
    let Some(name) = keys.get(a[1] as u32 as usize) else {
        return Ok(ERROR_NO_MORE_ITEMS as u64);
    };
    let r = return_name(t, name, a[2], a[3], wide);
    if r == 0 {
        if a[5] != 0 && a[6] != 0 {
            return_name(t, "", a[5], a[6], wide);
        }
        if a[7] != 0 {
            write_u64(t, a[7], crate::time::filetime_now());
        }
    }
    Ok(r)
}

fn enum_key(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    let keys = t.k.registry.subkeys(&path);
    let Some(name) = keys.get(a[1] as u32 as usize) else {
        return Ok(ERROR_NO_MORE_ITEMS as u64);
    };
    let (_, fit) = copy_str_truncate(t, a[2], a[3] as u32 as u64, name, wide);
    Ok(if fit { 0 } else { ERROR_MORE_DATA as u64 })
}

fn enum_value(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    let values = t.k.registry.values(&path);
    let Some((name, v)) = values.get(a[1] as u32 as usize) else {
        return Ok(ERROR_NO_MORE_ITEMS as u64);
    };
    let r = return_name(t, name, a[2], a[3], wide);
    if r != 0 {
        return Ok(r);
    }
    let (kind, bytes) = value_bytes(v, wide);
    Ok(return_value(t, kind, &bytes, a[5], a[6], a[7]))
}

fn query_info_key(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(path) = key_path(t, a[0]) else {
        return Ok(ERROR_INVALID_HANDLE as u64);
    };
    if !key_exists(t, &path) {
        return Ok(ERROR_BADKEY as u64);
    }
    let keys = t.k.registry.subkeys(&path);
    let values = t.k.registry.values(&path);
    if a[1] != 0 && a[2] != 0 {
        let r = return_name(t, "", a[1], a[2], wide);
        if r != 0 {
            return Ok(r);
        }
    }
    let max_key = keys.iter().map(|k| char_len(k, wide)).max().unwrap_or(0);
    let max_name = values
        .iter()
        .map(|(n, _)| char_len(n, wide))
        .max()
        .unwrap_or(0);
    let max_data = values
        .iter()
        .map(|(_, v)| value_bytes(v, wide).1.len())
        .max()
        .unwrap_or(0);
    store_u32(t, a[4], keys.len() as u32);
    store_u32(t, a[5], max_key as u32);
    store_u32(t, a[6], 0);
    store_u32(t, a[7], values.len() as u32);
    store_u32(t, a[8], max_name as u32);
    store_u32(t, a[9], max_data as u32);
    store_u32(t, a[10], 0);
    if a[11] != 0 {
        write_u64(t, a[11], crate::time::filetime_now());
    }
    Ok(0)
}

fn flush_key(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    shutdown(t);
    Ok(0)
}

const fn adv(name: &'static str, args: u8, f: crate::api::ApiFn) -> Api {
    api("advapi32", name, args, f)
}

pub(crate) static APIS: &[Api] = &[
    adv("RegOpenKeyA", 3, |t, a| {
        Ok(open_key(t, a[0], a[1], a[2], false))
    }),
    adv("RegOpenKeyW", 3, |t, a| {
        Ok(open_key(t, a[0], a[1], a[2], true))
    }),
    adv("RegOpenKeyExA", 5, |t, a| {
        Ok(open_key(t, a[0], a[1], a[4], false))
    }),
    adv("RegOpenKeyExW", 5, |t, a| {
        Ok(open_key(t, a[0], a[1], a[4], true))
    }),
    adv("RegOpenCurrentUser", 2, |t, a| {
        Ok(open_key(t, HKEY_CURRENT_USER as u64, 0, a[1], true))
    }),
    adv("RegOpenUserClassesRoot", 4, |t, a| {
        Ok(open_key(t, HKEY_CLASSES_ROOT as u64, 0, a[3], true))
    }),
    adv("RegCreateKeyA", 3, |t, a| {
        Ok(create_key(t, a[0], a[1], a[2], 0, false))
    }),
    adv("RegCreateKeyW", 3, |t, a| {
        Ok(create_key(t, a[0], a[1], a[2], 0, true))
    }),
    adv("RegCreateKeyExA", 9, |t, a| {
        Ok(create_key(t, a[0], a[1], a[7], a[8], false))
    }),
    adv("RegCreateKeyExW", 9, |t, a| {
        Ok(create_key(t, a[0], a[1], a[7], a[8], true))
    }),
    adv("RegCloseKey", 1, close_key),
    adv("RegDeleteKeyA", 2, |t, a| {
        Ok(delete_key(t, a[0], a[1], false))
    }),
    adv("RegDeleteKeyW", 2, |t, a| {
        Ok(delete_key(t, a[0], a[1], true))
    }),
    adv("RegDeleteKeyExA", 4, |t, a| {
        Ok(delete_key(t, a[0], a[1], false))
    }),
    adv("RegDeleteKeyExW", 4, |t, a| {
        Ok(delete_key(t, a[0], a[1], true))
    }),
    adv("RegDeleteTreeA", 2, |t, a| delete_tree(t, a, false)),
    adv("RegDeleteTreeW", 2, |t, a| delete_tree(t, a, true)),
    adv("RegQueryValueExA", 6, |t, a| query_value_ex(t, a, false)),
    adv("RegQueryValueExW", 6, |t, a| query_value_ex(t, a, true)),
    adv("RegQueryValueA", 4, |t, a| query_value_old(t, a, false)),
    adv("RegQueryValueW", 4, |t, a| query_value_old(t, a, true)),
    adv("RegGetValueA", 7, |t, a| get_value(t, a, false)),
    adv("RegGetValueW", 7, |t, a| get_value(t, a, true)),
    adv("RegSetValueExA", 6, |t, a| set_value_ex(t, a, false)),
    adv("RegSetValueExW", 6, |t, a| set_value_ex(t, a, true)),
    adv("RegSetValueA", 5, |t, a| set_value_old(t, a, false)),
    adv("RegSetValueW", 5, |t, a| set_value_old(t, a, true)),
    adv("RegSetKeyValueA", 6, |t, a| set_key_value(t, a, false)),
    adv("RegSetKeyValueW", 6, |t, a| set_key_value(t, a, true)),
    adv("RegDeleteValueA", 2, |t, a| delete_value(t, a, false)),
    adv("RegDeleteValueW", 2, |t, a| delete_value(t, a, true)),
    adv("RegDeleteKeyValueA", 3, |t, a| {
        delete_key_value(t, a, false)
    }),
    adv("RegDeleteKeyValueW", 3, |t, a| delete_key_value(t, a, true)),
    adv("RegEnumKeyA", 4, |t, a| enum_key(t, a, false)),
    adv("RegEnumKeyW", 4, |t, a| enum_key(t, a, true)),
    adv("RegEnumKeyExA", 8, |t, a| enum_key_ex(t, a, false)),
    adv("RegEnumKeyExW", 8, |t, a| enum_key_ex(t, a, true)),
    adv("RegEnumValueA", 8, |t, a| enum_value(t, a, false)),
    adv("RegEnumValueW", 8, |t, a| enum_value(t, a, true)),
    adv("RegQueryInfoKeyA", 12, |t, a| query_info_key(t, a, false)),
    adv("RegQueryInfoKeyW", 12, |t, a| query_info_key(t, a, true)),
    adv("RegFlushKey", 1, flush_key),
    adv("RegNotifyChangeKeyValue", 5, |_, _| Ok(0)),
    adv("RegOverridePredefKey", 2, |_, _| Ok(0)),
    adv("RegDisablePredefinedCache", 0, |_, _| Ok(0)),
    adv("RegDisablePredefinedCacheEx", 0, |_, _| Ok(0)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_guest_bytes() {
        for wide in [false, true] {
            for v in [
                RegValue::Sz("hello".into()),
                RegValue::ExpandSz(r"%TEMP%\x".into()),
                RegValue::MultiSz(vec!["a".into(), "bc".into()]),
                RegValue::Dword(0xDEAD_BEEF),
                RegValue::Qword(1 << 40),
                RegValue::Binary(vec![1, 2, 3]),
            ] {
                let (kind, bytes) = value_bytes(&v, wide);
                assert_eq!(value_from(kind, &bytes, wide), v);
            }
        }
        // Strings written without a terminator read back the same.
        assert_eq!(
            value_from(REG_SZ, b"abc", false),
            RegValue::Sz("abc".into())
        );
    }

    #[test]
    fn subkeys_join_cleanly() {
        assert_eq!(
            join("HKEY_CURRENT_USER", r"\Software\X\"),
            r"HKEY_CURRENT_USER\Software\X"
        );
        assert_eq!(join("HKEY_CURRENT_USER", ""), "HKEY_CURRENT_USER");
    }
}
