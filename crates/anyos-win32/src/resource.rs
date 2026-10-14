//! Resources: FindResource, LoadResource, LockResource, SizeofResource and
//! user32's LoadString, read straight from the resource directory of a
//! module mapped in guest memory.
//!
//! An `HRSRC` is the guest address of the resource's
//! `IMAGE_RESOURCE_DATA_ENTRY`; an `HGLOBAL` from LoadResource is the address
//! of the data itself, so LockResource has nothing to do.

use crate::api::*;
use crate::thread::{Halt, Thread};

const ERROR_RESOURCE_DATA_NOT_FOUND: u32 = 1812;
const ERROR_RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
const ERROR_RESOURCE_NAME_NOT_FOUND: u32 = 1814;
const ERROR_RESOURCE_LANG_NOT_FOUND: u32 = 1815;
const RT_STRING: u64 = 6;

/// A resource type, name or language: a number or a string.
#[derive(Debug, Clone, PartialEq)]
enum ResId {
    Id(u16),
    Name(String),
}

fn module_base(t: &Thread<'_, '_>, h: u64) -> u64 {
    if h == 0 {
        t.process.modules[0].base
    } else {
        h
    }
}

/// Address and size of a module's resource directory.
fn resource_dir(t: &Thread<'_, '_>, base: u64) -> Option<(u64, u32)> {
    if read_u16(t, base)? != 0x5A4D {
        return None;
    }
    let pe = base + read_u32(t, base + 0x3C)? as u64;
    if read_u32(t, pe)? != 0x4550 {
        return None;
    }
    let opt = pe + 24;
    let dirs = match read_u16(t, opt)? {
        0x10B => opt + 96,
        0x20B => opt + 112,
        _ => return None,
    };
    let rva = read_u32(t, dirs + 2 * 8)?;
    let size = read_u32(t, dirs + 2 * 8 + 4)?;
    (rva != 0 && size != 0).then_some((base + rva as u64, size))
}

/// A type or name argument: `MAKEINTRESOURCE(n)`, `"#n"` or a string.
fn res_arg(t: &Thread<'_, '_>, p: u64, wide: bool) -> Option<ResId> {
    if p < 0x1_0000 {
        return Some(ResId::Id(p as u16));
    }
    let s = read_str(t, p, wide)?;
    Some(
        match s.strip_prefix('#').and_then(|n| n.parse::<u16>().ok()) {
            Some(n) => ResId::Id(n),
            None => ResId::Name(s),
        },
    )
}

/// The entries of the resource directory table at `table`: (id, target,
/// target-is-a-subdirectory).
fn entries(t: &Thread<'_, '_>, root: u64, table: u64) -> Vec<(ResId, u64, bool)> {
    let named = read_u16(t, table + 12).unwrap_or(0) as u64;
    let ids = read_u16(t, table + 14).unwrap_or(0) as u64;
    let mut out = Vec::new();
    for i in 0..(named + ids).min(4096) {
        let e = table + 16 + i * 8;
        let (Some(name), Some(off)) = (read_u32(t, e), read_u32(t, e + 4)) else {
            break;
        };
        let id = if name & 0x8000_0000 != 0 {
            let s = root + (name & 0x7FFF_FFFF) as u64;
            let len = read_u16(t, s).unwrap_or(0) as u64;
            let units: Vec<u16> = guest_bytes(t, s + 2, len * 2)
                .unwrap_or_default()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect();
            ResId::Name(String::from_utf16_lossy(&units))
        } else {
            ResId::Id(name as u16)
        };
        out.push((
            id,
            root + (off & 0x7FFF_FFFF) as u64,
            off & 0x8000_0000 != 0,
        ));
    }
    out
}

fn same(a: &ResId, b: &ResId) -> bool {
    match (a, b) {
        (ResId::Id(x), ResId::Id(y)) => x == y,
        (ResId::Name(x), ResId::Name(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

/// Find a resource's data entry. With no language, prefer neutral, then
/// English, then whatever comes first.
fn find(
    t: &Thread<'_, '_>,
    module: u64,
    kind: &ResId,
    name: &ResId,
    lang: Option<u16>,
) -> Result<u64, u32> {
    let base = module_base(t, module);
    let Some((root, _)) = resource_dir(t, base) else {
        return Err(ERROR_RESOURCE_DATA_NOT_FOUND);
    };
    let Some((_, types, true)) = entries(t, root, root)
        .into_iter()
        .find(|e| same(&e.0, kind))
    else {
        return Err(ERROR_RESOURCE_TYPE_NOT_FOUND);
    };
    let Some((_, names, true)) = entries(t, root, types)
        .into_iter()
        .find(|e| same(&e.0, name))
    else {
        return Err(ERROR_RESOURCE_NAME_NOT_FOUND);
    };
    let langs: Vec<(u16, u64)> = entries(t, root, names)
        .into_iter()
        .filter(|e| !e.2)
        .filter_map(|(id, data, _)| match id {
            ResId::Id(l) => Some((l, data)),
            ResId::Name(_) => None,
        })
        .collect();
    let pick = |want: u16| langs.iter().find(|(l, _)| *l == want).map(|(_, d)| *d);
    let primary = |want: u16| {
        langs
            .iter()
            .find(|(l, _)| l & 0x3FF == want & 0x3FF)
            .map(|(_, d)| *d)
    };
    let found = match lang {
        Some(l) if l != 0 => pick(l).or_else(|| primary(l)),
        _ => pick(0)
            .or_else(|| pick(0x0409))
            .or_else(|| primary(0x0409))
            .or_else(|| langs.first().map(|(_, d)| *d)),
    };
    found.ok_or(ERROR_RESOURCE_LANG_NOT_FOUND)
}

fn find_resource(
    t: &mut Thread<'_, '_>,
    module: u64,
    kind: u64,
    name: u64,
    lang: Option<u16>,
    wide: bool,
) -> Result<u64, Halt> {
    let (Some(kind), Some(name)) = (res_arg(t, kind, wide), res_arg(t, name, wide)) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    match find(t, module, &kind, &name, lang) {
        Ok(entry) => Ok(entry),
        Err(code) => fail(t, code, 0),
    }
}

/// The data address and size of a found resource.
fn resource_data(t: &Thread<'_, '_>, module: u64, entry: u64) -> Option<(u64, u32)> {
    let rva = read_u32(t, entry)?;
    let size = read_u32(t, entry + 4)?;
    Some((module_base(t, module) + rva as u64, size))
}

/// String `id` of a module's string table, as UTF-16 units with its address.
fn load_string_units(t: &Thread<'_, '_>, module: u64, id: u32) -> Option<(u64, Vec<u16>)> {
    let block = ResId::Id(((id >> 4) + 1) as u16);
    let entry = find(t, module, &ResId::Id(RT_STRING as u16), &block, None).ok()?;
    let (mut p, _) = resource_data(t, module, entry)?;
    for _ in 0..(id & 15) {
        let len = read_u16(t, p)? as u64;
        p += 2 + len * 2;
    }
    let len = read_u16(t, p)? as u64;
    let units = guest_bytes(t, p + 2, len * 2)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    Some((p + 2, units))
}

fn load_string(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (module, id, buf, cch) = (a[0], a[1] as u32, a[2], a[3] as u32 as u64);
    let Some((at, units)) = load_string_units(t, module, id & 0xFFFF) else {
        if buf != 0 && cch > 0 {
            write_bytes(t, buf, if wide { &[0, 0] } else { &[0] });
        }
        return fail(t, ERROR_RESOURCE_NAME_NOT_FOUND, 0);
    };
    if wide && cch == 0 {
        // A read-only pointer to the string itself (not terminated).
        write_ptr(t, buf, at);
        return Ok(units.len() as u64);
    }
    let s = String::from_utf16_lossy(&units);
    let (n, _) = copy_str_truncate(t, buf, cch, &s, wide);
    Ok(n)
}

pub(crate) static APIS: &[Api] = &[
    k32("FindResourceA", 3, |t, a| {
        find_resource(t, a[0], a[2], a[1], None, false)
    }),
    k32("FindResourceW", 3, |t, a| {
        find_resource(t, a[0], a[2], a[1], None, true)
    }),
    k32("FindResourceExA", 4, |t, a| {
        find_resource(t, a[0], a[1], a[2], Some(a[3] as u16), false)
    }),
    k32("FindResourceExW", 4, |t, a| {
        find_resource(t, a[0], a[1], a[2], Some(a[3] as u16), true)
    }),
    k32("LoadResource", 2, |t, a| {
        match resource_data(t, a[0], a[1]) {
            Some((data, _)) if a[1] != 0 => Ok(data),
            _ => fail(t, ERROR_INVALID_HANDLE, 0),
        }
    }),
    k32("LockResource", 1, |_, a| Ok(a[0])),
    k32("UnlockResource", 1, |_, _| Ok(0)),
    k32("FreeResource", 1, |_, _| Ok(0)),
    k32("SizeofResource", 2, |t, a| {
        match resource_data(t, a[0], a[1]) {
            Some((_, size)) if a[1] != 0 => Ok(size as u64),
            _ => fail(t, ERROR_INVALID_HANDLE, 0),
        }
    }),
    api("user32", "LoadStringA", 4, |t, a| load_string(t, a, false)),
    api("user32", "LoadStringW", 4, |t, a| load_string(t, a, true)),
];
