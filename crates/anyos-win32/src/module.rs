//! Module APIs: GetModuleHandle, LoadLibrary, GetProcAddress and friends.
//!
//! Application DLLs are real guest modules, loaded by `anyos-loader`. System
//! DLLs (kernel32, user32, ...) are provided by AnyOS itself: each gets a
//! one-page placeholder image whose address is its `HMODULE`, and
//! `GetProcAddress` on it hands out call-gate stubs for the APIs AnyOS
//! implements.

use std::path::Path;

use anyos_loader::LoadOptions;
use anyos_memory::Protection;
use anyos_pe::catalog::classify;
use anyos_pe::ImportName;

use crate::api::{read_ansi, read_wide, ERROR_MOD_NOT_FOUND, ERROR_NOACCESS};
use crate::thread::{Halt, Thread};

const ERROR_PROC_NOT_FOUND: u32 = 127;
const ERROR_BAD_EXE_FORMAT: u32 = 193;
const ERROR_DLL_INIT_FAILED: u32 = 1114;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_PARAMETER: u32 = 87;

const DONT_RESOLVE_DLL_REFERENCES: u64 = 0x1;
const LOAD_LIBRARY_AS_DATAFILE: u64 = 0x2;
const LOAD_LIBRARY_AS_IMAGE_RESOURCE: u64 = 0x20;
const LOAD_LIBRARY_AS_DATAFILE_EXCLUSIVE: u64 = 0x40;
const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u64 = 0x4;

/// System DLLs that every Windows process has loaded from the start.
const ALWAYS_LOADED: &[&str] = &["ntdll.dll", "kernel32.dll", "kernelbase.dll"];

/// A system DLL the guest holds a handle to.
#[derive(Debug, Clone)]
pub(crate) struct SystemModule {
    /// Lower-cased file name, e.g. `kernel32.dll`.
    pub name: String,
    pub base: u64,
}

/// What an `HMODULE` refers to.
enum Module {
    /// Index into `Process::modules`.
    Loaded(usize),
    /// Index into `Thread::system_modules`.
    System(usize),
}

/// `C:\dir\KERNEL32` -> `kernel32.dll`; `app.` -> `app` (a trailing dot
/// means "no extension" to LoadLibrary).
fn dll_file_name(name: &str) -> String {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let lower = file.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix('.') {
        stripped.to_string()
    } else if lower.contains('.') {
        lower
    } else {
        format!("{lower}.dll")
    }
}

fn by_handle(t: &Thread<'_, '_>, h: u64) -> Option<Module> {
    if h == 0 {
        return Some(Module::Loaded(0));
    }
    if let Some(i) = t.process.modules.iter().position(|m| m.base == h) {
        return Some(Module::Loaded(i));
    }
    t.system_modules
        .iter()
        .position(|m| m.base == h)
        .map(Module::System)
}

fn by_name(t: &Thread<'_, '_>, file: &str) -> Option<Module> {
    if let Some(i) = t
        .process
        .modules
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(file))
    {
        return Some(Module::Loaded(i));
    }
    t.system_modules
        .iter()
        .position(|m| m.name == file)
        .map(Module::System)
}

fn handle_of(t: &Thread<'_, '_>, m: &Module) -> u64 {
    match *m {
        Module::Loaded(i) => t.process.modules[i].base,
        Module::System(i) => t.system_modules[i].base,
    }
}

/// The handle of system DLL `file`, mapping its placeholder image on first use.
fn system_module(t: &mut Thread<'_, '_>, file: &str) -> Result<u64, Halt> {
    if let Some(m) = t.system_modules.iter().find(|m| m.name == file) {
        return Ok(m.base);
    }
    let mem = &mut t.process.memory;
    let tag = format!("{file} (provided by anyos)");
    let base = mem.map_anywhere(0x1000, Protection::RW, &tag, None)?;
    // Enough of a DOS header for code that checks for "MZ".
    mem.write_raw(base, b"MZ")?;
    mem.protect(base, 0x1000, Protection::R)?;
    t.system_modules.push(SystemModule {
        name: file.to_string(),
        base,
    });
    Ok(base)
}

/// Whether the process already has system DLL `file` loaded on Windows: the
/// core DLLs always are, and so is anything a module imports from.
fn system_dll_present(t: &Thread<'_, '_>, file: &str) -> bool {
    ALWAYS_LOADED.contains(&file)
        || t.system_modules.iter().any(|m| m.name == file)
        || t.process.modules.iter().any(|m| {
            m.image
                .imports
                .iter()
                .any(|i| !i.delay_load && dll_file_name(&i.dll) == file)
        })
}

/// Read a module or procedure name argument, ANSI or UTF-16.
fn read_name(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Option<String> {
    let s = if wide {
        read_wide(t, ptr)
    } else {
        read_ansi(t, ptr)
    };
    if s.is_none() {
        t.set_last_error(ERROR_NOACCESS);
    }
    s
}

fn module_handle(t: &mut Thread<'_, '_>, name: &str) -> Result<u64, Halt> {
    let file = dll_file_name(name);
    if let Some(m) = by_name(t, &file) {
        return Ok(handle_of(t, &m));
    }
    // On real Windows the C/C++ runtime DLLs (msvcrt, msvcr71, msvcp60, …)
    // are always reachable because every system DLL chain pulls them in
    // transitively.  Return a placeholder stub so that apps which probe for
    // the CRT before using it (e.g. Director/Shockwave projectors) do not
    // receive NULL and take an "environment broken" exit path.
    //
    // For all other system DLLs (user32, gdi32, …) we only return a stub when
    // the process is already expected to have them: either they appear in the
    // PE import table or were previously accessed via LoadLibrary.  This
    // matches real Windows behaviour where a pure console app can legitimately
    // see NULL for user32.dll.
    if classify(&file).is_system() {
        let always_present = matches!(classify(&file), anyos_pe::catalog::ApiFamily::Crt);
        if always_present || system_dll_present(t, &file) {
            return system_module(t, &file);
        }
    }
    t.set_last_error(ERROR_MOD_NOT_FOUND);
    Ok(0)
}

fn get_module_handle(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<u64, Halt> {
    if ptr == 0 {
        return Ok(t.process.modules[0].base);
    }
    match read_name(t, ptr, wide) {
        Some(name) => {
            // Emit the resolved string so --trace shows what was actually queried,
            // not just the raw guest pointer.
            t.trace_note(&format!("GetModuleHandle: querying \"{}\"", name));
            module_handle(t, &name)
        }
        None => Ok(0),
    }
}

pub(crate) fn get_module_handle_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_handle(t, a[0], false)
}

pub(crate) fn get_module_handle_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_handle(t, a[0], true)
}

fn get_module_handle_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (flags, name, out) = (a[0], a[1], a[2]);
    if out == 0 {
        t.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let handle = if flags & GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS != 0 {
        let hit = t
            .process
            .modules
            .iter()
            .find(|m| name >= m.base && name < m.base + m.size)
            .map(|m| m.base)
            .or_else(|| {
                t.system_modules
                    .iter()
                    .find(|m| name >= m.base && name < m.base + 0x1000)
                    .map(|m| m.base)
            });
        match hit {
            Some(h) => h,
            None => {
                t.set_last_error(ERROR_MOD_NOT_FOUND);
                0
            }
        }
    } else {
        get_module_handle(t, name, wide)?
    };
    let p = t.ptr_size();
    if t.process
        .memory
        .write(out, &handle.to_le_bytes()[..p])
        .is_err()
    {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    }
    Ok((handle != 0) as u64)
}

pub(crate) fn get_module_handle_ex_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_handle_ex(t, a, false)
}

pub(crate) fn get_module_handle_ex_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_handle_ex(t, a, true)
}

fn load_library(t: &mut Thread<'_, '_>, ptr: u64, flags: u64, wide: bool) -> Result<u64, Halt> {
    let Some(name) = read_name(t, ptr, wide) else {
        return Ok(0);
    };
    let file = dll_file_name(&name);
    if let Some(m) = by_name(t, &file) {
        return Ok(handle_of(t, &m));
    }
    if classify(&file).is_system() || system_dll_present(t, &file) {
        return system_module(t, &file);
    }

    let Some(path) = find_dll(&t.search_dirs, &file) else {
        t.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };
    let first_new = t.process.modules.len();
    let opts = LoadOptions {
        search_dirs: t.search_dirs.clone(),
        load_dependencies: true,
        ..LoadOptions::default()
    };
    let loaded = std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            let image = anyos_pe::parse(&bytes).map_err(|e| e.to_string())?;
            let p = &mut t.process;
            p.map_image(
                &file,
                Some(path.clone()),
                &bytes,
                image,
                anyos_loader::BasePolicy::Preferred,
            )
            .and_then(|_| p.load_dependencies(&opts))
            .and_then(|_| p.bind_all())
            .map_err(|e| e.to_string())
        });
    if let Err(e) = loaded {
        // Undo nothing: whatever got mapped stays, as an unreferenced image.
        t.trace_note(&format!("LoadLibrary({name}) failed: {e}"));
        t.set_last_error(ERROR_BAD_EXE_FORMAT);
        return Ok(0);
    }
    let base = t.process.modules[first_new].base;
    let data_only = DONT_RESOLVE_DLL_REFERENCES
        | LOAD_LIBRARY_AS_DATAFILE
        | LOAD_LIBRARY_AS_IMAGE_RESOURCE
        | LOAD_LIBRARY_AS_DATAFILE_EXCLUSIVE;
    if flags & data_only == 0 {
        // Deepest dependency first, as at start-up.
        for i in (first_new..t.process.modules.len()).rev() {
            if !t.init_dll(i)? {
                let module = t.process.modules[i].name.clone();
                t.trace_note(&format!("{module}: DllMain returned FALSE"));
                t.set_last_error(ERROR_DLL_INIT_FAILED);
                return Ok(0);
            }
        }
    }
    Ok(base)
}

fn find_dll(dirs: &[std::path::PathBuf], file: &str) -> Option<std::path::PathBuf> {
    dirs.iter().find_map(|dir| {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let hit = e.file_name().to_string_lossy().eq_ignore_ascii_case(file);
            (hit && e.path().is_file()).then(|| e.path())
        })
    })
}

pub(crate) fn load_library_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    load_library(t, a[0], 0, false)
}

pub(crate) fn load_library_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    load_library(t, a[0], 0, true)
}

pub(crate) fn load_library_ex_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    load_library(t, a[0], a[2], false)
}

pub(crate) fn load_library_ex_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    load_library(t, a[0], a[2], true)
}

/// Modules are never unloaded, so this only checks the handle.
pub(crate) fn free_library(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if a[0] != 0 && by_handle(t, a[0]).is_some() {
        Ok(1)
    } else {
        t.set_last_error(crate::api::ERROR_INVALID_HANDLE);
        Ok(0)
    }
}

pub(crate) fn get_proc_address(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, proc_ptr) = (a[0], a[1]);
    let Some(module) = by_handle(t, h) else {
        t.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };
    // A pointer whose high bits are all zero is an ordinal (MAKEINTRESOURCE).
    let symbol = if proc_ptr >> 16 == 0 {
        ImportName::Ordinal {
            ordinal: proc_ptr as u16,
        }
    } else {
        match read_ansi(t, proc_ptr) {
            Some(name) => ImportName::Name { name, hint: 0 },
            None => {
                t.set_last_error(ERROR_NOACCESS);
                return Ok(0);
            }
        }
    };
    let found = match module {
        Module::Loaded(i) => {
            let m = &t.process.modules[i];
            if m.find_export(&symbol).is_some() {
                // Forwarders are followed the same way imports are bound.
                let name = m.name.clone();
                Some(t.process.resolve(&name, &symbol, 0).map(|(addr, _)| addr))
            } else {
                None
            }
        }
        Module::System(i) => {
            let dll = t.system_modules[i].name.clone();
            if crate::api::find(&dll, &symbol).is_some() {
                let p = &mut t.process;
                Some(
                    p.call_gate
                        .stub_for(&mut p.memory, p.machine, &dll, &symbol)
                        .map_err(Into::into),
                )
            } else {
                // Report it as missing rather than hand out a stub that would
                // stop the program later: callers that probe for optional
                // APIs check for NULL and fall back.
                t.trace_note(&format!(
                    "GetProcAddress: {dll}!{symbol} is not implemented, returning NULL"
                ));
                None
            }
        }
    };
    match found {
        Some(Ok(addr)) => Ok(addr),
        Some(Err(e)) => Err(crate::RunError::Load(e).into()),
        None => {
            t.set_last_error(ERROR_PROC_NOT_FOUND);
            Ok(0)
        }
    }
}

/// How the guest sees a host path: drive-letter paths stay as they are, and
/// Unix paths appear under `Z:`, the way Wine shows the host root.
pub(crate) fn guest_path(host: &Path) -> String {
    let host = std::fs::canonicalize(host).unwrap_or_else(|_| host.to_path_buf());
    let s = host.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if s.starts_with('/') {
        format!("Z:{}", s.replace('/', "\\"))
    } else {
        s.to_string()
    }
}

/// The program's own path as the guest sees it.
pub(crate) fn main_module_path(t: &Thread<'_, '_>) -> String {
    module_file_name(t, &Module::Loaded(0))
}

fn module_file_name(t: &Thread<'_, '_>, m: &Module) -> String {
    match *m {
        Module::Loaded(i) => {
            let m = &t.process.modules[i];
            match &m.path {
                Some(p) => crate::file::guest_path(t, p),
                None => format!(r"C:\{}", m.name),
            }
        }
        Module::System(i) => format!(r"C:\Windows\System32\{}", t.system_modules[i].name),
    }
}

fn get_module_file_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (h, buf, size) = (a[0], a[1], a[2] as u32 as usize);
    let Some(module) = by_handle(t, h) else {
        t.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(0);
    };
    if size == 0 {
        t.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    let path = module_file_name(t, &module);
    let units: Vec<u16> = if wide {
        path.encode_utf16().collect()
    } else {
        path.bytes().map(u16::from).collect()
    };
    // Since Windows Vista: truncate, still terminate, and return the buffer size.
    let n = units.len().min(size - 1);
    let mut out: Vec<u8> = Vec::new();
    for &u in units[..n].iter().chain([&0]) {
        if wide {
            out.extend_from_slice(&u.to_le_bytes());
        } else {
            out.push(u as u8);
        }
    }
    if t.process.memory.write(buf, &out).is_err() {
        t.set_last_error(ERROR_NOACCESS);
        return Ok(0);
    }
    if n < units.len() {
        t.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(size as u64);
    }
    Ok(n as u64)
}

pub(crate) fn get_module_file_name_a(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_file_name(t, a, false)
}

pub(crate) fn get_module_file_name_w(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    get_module_file_name(t, a, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dll_names_normalise_like_load_library() {
        assert_eq!(dll_file_name("KERNEL32"), "kernel32.dll");
        assert_eq!(
            dll_file_name(r"C:\Windows\System32\User32.DLL"),
            "user32.dll"
        );
        assert_eq!(dll_file_name("plugin."), "plugin");
        assert_eq!(dll_file_name("ctl.ocx"), "ctl.ocx");
    }

    #[test]
    fn unix_paths_appear_under_z() {
        if cfg!(unix) {
            assert_eq!(
                guest_path(Path::new("/no/such/dir/setup.exe")),
                r"Z:\no\such\dir\setup.exe"
            );
        }
    }
}
