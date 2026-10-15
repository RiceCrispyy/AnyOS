//! Files and paths: the guest's drives, `CreateFile` and file I/O, directory
//! and search APIs, file mappings, anonymous pipes, handle management and INI
//! files.
//!
//! The guest sees Windows paths. `C:\` is the AnyOS environment's drive when
//! the run has one ([`crate::RunOptions::drive_c`]); on Unix hosts the host
//! file system appears as `Z:\`, the way Wine shows it. Path components match
//! case-insensitively, as on Windows, even on case-sensitive hosts.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyos_memory::{round_up, Protection, PAGE_SIZE};

use crate::api::*;
use crate::console;
use crate::objects::Object;
use crate::thread::{Halt, Thread};
use crate::time::{filetime_of, system_time_of};

pub(crate) const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub(crate) const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const INVALID_FILE_ATTRIBUTES: u64 = 0xFFFF_FFFF;
const ERROR_NEGATIVE_SEEK: u32 = 131;
const ERROR_NO_DATA: u32 = 232;
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_NOT_SAME_DEVICE: u32 = 17;
const ERROR_DIRECTORY: u32 = 267;

/// An open file or directory.
pub(crate) struct FileObj {
    pub file: Option<File>,
    pub host: PathBuf,
    pub guest: String,
    pub delete_on_close: bool,
    pub append: bool,
}

/// A directory listing in progress.
pub(crate) struct FindObj {
    entries: VecDeque<FindEntry>,
    wide: bool,
}

struct FindEntry {
    name: String,
    attrs: u32,
    size: u64,
    times: [u64; 3],
}

/// What backs a file mapping.
pub(crate) struct MappingBacking {
    file: Option<File>,
    /// Contents of an anonymous mapping (copied into and out of views).
    data: Vec<u8>,
    size: u64,
    writable: bool,
}

pub(crate) struct MappingObj(Rc<RefCell<MappingBacking>>);

/// A mapped view.
pub(crate) struct View {
    backing: Rc<RefCell<MappingBacking>>,
    offset: u64,
    len: u64,
    writable: bool,
}

#[derive(Default)]
pub(crate) struct PipeBuf {
    pub(crate) data: VecDeque<u8>,
    pub(crate) readers: u32,
    pub(crate) writers: u32,
}

/// One end of an anonymous pipe.
pub(crate) struct PipeEnd {
    pub(crate) buf: Rc<RefCell<PipeBuf>>,
    pub(crate) write: bool,
}

// ---------------------------------------------------------------------------
// Paths

/// Set the starting directory and the environment block.
pub(crate) fn init(
    t: &mut Thread<'_, '_>,
    program_dir: Option<&Path>,
    extra_env: &[(String, String)],
) {
    t.k.cwd = match program_dir {
        Some(d) => trim_trailing(&guest_path(t, d)),
        None => r"C:\".into(),
    };
    crate::sysinfo::init_env(t, extra_env);
}

/// Delete files opened with FILE_FLAG_DELETE_ON_CLOSE that are still open,
/// and write back mapped views.
pub(crate) fn shutdown(t: &mut Thread<'_, '_>) {
    let bases: Vec<u64> = t.k.views.keys().copied().collect();
    for b in bases {
        flush_view(t, b);
    }
    for o in t.k.objects_mut() {
        if let Object::File(f) = o {
            if f.delete_on_close {
                f.file = None;
                let _ = remove_path(&f.host);
            }
        }
    }
}

fn trim_trailing(p: &str) -> String {
    if p.len() > 3 {
        p.trim_end_matches('\\').to_string()
    } else {
        p.to_string()
    }
}

/// How the guest sees a host path: under `C:` when inside the environment's
/// drive, under `Z:` for other Unix paths, unchanged for Windows host paths.
pub(crate) fn guest_path(t: &Thread<'_, '_>, host: &Path) -> String {
    let host = std::fs::canonicalize(host).unwrap_or_else(|_| host.to_path_buf());
    if let Some(c) = t
        .host
        .drive_c
        .as_ref()
        .and_then(|c| std::fs::canonicalize(c).ok())
    {
        if let Ok(rest) = host.strip_prefix(&c) {
            let rest = rest.to_string_lossy().replace('/', "\\");
            return format!(r"C:\{rest}");
        }
    }
    crate::module::guest_path(&host)
}

/// The absolute, normalised Windows form of `p` (what GetFullPathName returns).
pub(crate) fn full_path(t: &Thread<'_, '_>, p: &str) -> String {
    let p = p.replace('/', "\\");
    let p = p
        .strip_prefix(r"\\?\")
        .or_else(|| p.strip_prefix(r"\\.\"))
        .unwrap_or(&p)
        .to_string();
    if p.starts_with(r"\\") {
        return p; // UNC: left alone
    }
    let cwd = &t.k.cwd;
    let cwd_drive = cwd.get(..2).unwrap_or("C:").to_string();
    let b = p.as_bytes();
    let joined = if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let drive = p[..2].to_ascii_uppercase();
        let rest = &p[2..];
        if rest.starts_with('\\') {
            format!("{drive}{rest}")
        } else if drive.eq_ignore_ascii_case(&cwd_drive) {
            format!(r"{cwd}\{rest}")
        } else {
            format!(r"{drive}\{rest}")
        }
    } else if p.starts_with('\\') {
        format!("{cwd_drive}{p}")
    } else {
        format!(r"{cwd}\{p}")
    };
    let trailing = joined.ends_with('\\');
    let drive = joined[..2].to_string();
    let mut parts: Vec<&str> = Vec::new();
    for c in joined[2..].split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = format!(r"{drive}\{}", parts.join("\\"));
    if trailing && !parts.is_empty() {
        out.push('\\');
    }
    out
}

/// The entry in `dir` named `name`, ignoring case when there is no exact match.
fn match_case(dir: &Path, name: &str) -> PathBuf {
    let exact = dir.join(name);
    if exact.exists() || cfg!(windows) {
        return exact;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
                return e.path();
            }
        }
    }
    exact
}

/// The host path for a guest path, or `None` when the drive is not mapped.
pub(crate) fn host_path(t: &Thread<'_, '_>, guest: &str) -> Option<PathBuf> {
    let full = full_path(t, guest);
    let b = full.as_bytes();
    if b.len() < 2 || b[1] != b':' {
        return None;
    }
    let drive = b[0].to_ascii_uppercase();
    let mut base = match (drive, &t.host.drive_c) {
        (b'C', Some(c)) => c.clone(),
        (b'Z', _) if cfg!(unix) => PathBuf::from("/"),
        _ if cfg!(windows) => PathBuf::from(format!("{}:\\", drive as char)),
        _ => return None,
    };
    for c in full[2..].split('\\').filter(|c| !c.is_empty()) {
        base = match_case(&base, c);
    }
    Some(base)
}

/// Read a path argument and map it, setting the last error on failure.
fn arg_path(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<(String, PathBuf), u32> {
    if ptr == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let Some(s) = read_str(t, ptr, wide) else {
        return Err(ERROR_NOACCESS);
    };
    if s.is_empty() {
        return Err(ERROR_PATH_NOT_FOUND);
    }
    let full = full_path(t, &s);
    match host_path(t, &full) {
        Some(h) => Ok((full, h)),
        None => Err(ERROR_PATH_NOT_FOUND),
    }
}

/// The Windows error for a failed host file operation on `path`.
pub(crate) fn io_error(e: &std::io::Error, path: &Path) -> u32 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => {
            if path.parent().is_some_and(Path::is_dir) {
                ERROR_FILE_NOT_FOUND
            } else {
                ERROR_PATH_NOT_FOUND
            }
        }
        PermissionDenied | IsADirectory | ReadOnlyFilesystem => ERROR_ACCESS_DENIED,
        AlreadyExists => ERROR_FILE_EXISTS,
        DirectoryNotEmpty => ERROR_DIR_NOT_EMPTY,
        NotADirectory => ERROR_DIRECTORY,
        InvalidInput | InvalidFilename => ERROR_INVALID_NAME,
        StorageFull => 112, // ERROR_DISK_FULL
        CrossesDevices => ERROR_NOT_SAME_DEVICE,
        _ => ERROR_ACCESS_DENIED,
    }
}

fn attributes_of(m: &std::fs::Metadata) -> u32 {
    let mut a = if m.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_ARCHIVE
    };
    if m.permissions().readonly() {
        a |= FILE_ATTRIBUTE_READONLY;
    }
    a
}

/// Creation, last access and last write times as FILETIMEs.
fn times_of(m: &std::fs::Metadata) -> [u64; 3] {
    let w = m.modified().map(filetime_of).unwrap_or(0);
    let a = m.accessed().map(filetime_of).unwrap_or(w);
    let c = m.created().map(filetime_of).unwrap_or(w);
    [c, a, w]
}

fn remove_path(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir(p)
    } else {
        std::fs::remove_file(p)
    }
}

// ---------------------------------------------------------------------------
// CreateFile

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const FILE_READ_DATA: u32 = 0x1;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_APPEND_DATA: u32 = 0x4;
const DELETE: u32 = 0x1_0000;
const CREATE_NEW: u32 = 1;
const CREATE_ALWAYS: u32 = 2;
const OPEN_EXISTING: u32 = 3;
const OPEN_ALWAYS: u32 = 4;
const TRUNCATE_EXISTING: u32 = 5;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;

/// Device names CreateFile opens instead of files.
fn device(name: &str) -> Option<u64> {
    let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let stem = last.split('.').next().unwrap_or(last).trim_end_matches(':');
    match stem.to_ascii_uppercase().as_str() {
        "CONIN$" => Some(console::STDIN_HANDLE),
        "CONOUT$" | "CON" => Some(console::STDOUT_HANDLE),
        "NUL" => Some(0),
        _ => None,
    }
}

pub(crate) fn create_file(
    t: &mut Thread<'_, '_>,
    name: &str,
    access: u32,
    disposition: u32,
    flags: u32,
) -> Result<u64, u32> {
    if let Some(dev) = device(name) {
        return Ok(if dev == 0 {
            t.k.insert(Object::Null, None)
        } else {
            dev
        });
    }
    // Named pipe client: \\.\pipe\<name>
    let pipe_name = name
        .strip_prefix(r"\\.\pipe\")
        .or_else(|| name.strip_prefix(r"\\.\Pipe\"))
        .or_else(|| name.strip_prefix(r"//./pipe/"));
    if let Some(pipe_name) = pipe_name {
        let key = pipe_name.to_ascii_lowercase();
        let server_h = t.k.named_pipes.get(&key).copied();
        let Some(server_h) = server_h else {
            return Err(ERROR_FILE_NOT_FOUND);
        };
        let (c2s, s2c) = match t.k.get(server_h) {
            Some(Object::NamedPipeServer { c2s, s2c, .. }) => (c2s.clone(), s2c.clone()),
            _ => return Err(ERROR_FILE_NOT_FOUND),
        };
        c2s.borrow_mut().writers += 1;
        c2s.borrow_mut().readers += 1;
        s2c.borrow_mut().writers += 1;
        s2c.borrow_mut().readers += 1;
        if let Some(Object::NamedPipeServer { connected, .. }) = t.k.get(server_h) {
            *connected = true;
        }
        t.wake_all();
        let h = t.k.insert(Object::NamedPipeClient { c2s, s2c }, None);
        return Ok(h);
    }
    let full = full_path(t, name);
    let host = host_path(t, &full).ok_or(ERROR_PATH_NOT_FOUND)?;
    let read = access & (GENERIC_READ | GENERIC_ALL | GENERIC_EXECUTE | FILE_READ_DATA) != 0;
    let append_only = access & FILE_APPEND_DATA != 0
        && access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA) == 0;
    let write = access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA) != 0;
    let existed = host.exists();
    if host.is_dir() {
        if flags & FILE_FLAG_BACKUP_SEMANTICS == 0 {
            return Err(ERROR_ACCESS_DENIED);
        }
        if disposition == CREATE_NEW {
            return Err(ERROR_FILE_EXISTS);
        }
        let obj = FileObj {
            file: None,
            host,
            guest: full,
            delete_on_close: flags & FILE_FLAG_DELETE_ON_CLOSE != 0,
            append: false,
        };
        return Ok(t.k.insert(Object::File(obj), None));
    }
    let mut o = OpenOptions::new();
    o.read(read || !write);
    o.write(write);
    match disposition {
        CREATE_NEW => {
            o.create_new(true).write(true);
        }
        CREATE_ALWAYS => {
            o.create(true).truncate(true).write(true);
        }
        OPEN_EXISTING => {}
        OPEN_ALWAYS => {
            o.create(true).write(write || !existed);
        }
        TRUNCATE_EXISTING => {
            if !write {
                return Err(ERROR_INVALID_PARAMETER);
            }
            o.truncate(true);
        }
        _ => return Err(ERROR_INVALID_PARAMETER),
    }
    let file = match o.open(&host) {
        Ok(f) => Some(f),
        // Opening for attributes only (no read or write access) still works
        // on a file we cannot read.
        Err(_) if !read && !write && existed && access & DELETE == 0 => None,
        Err(e) => return Err(io_error(&e, &host)),
    };
    let obj = FileObj {
        file,
        host,
        guest: full,
        delete_on_close: flags & FILE_FLAG_DELETE_ON_CLOSE != 0,
        append: append_only,
    };
    let h = t.k.insert(Object::File(obj), None);
    let opened_existing = existed && matches!(disposition, CREATE_ALWAYS | OPEN_ALWAYS);
    t.set_last_error(if opened_existing {
        ERROR_ALREADY_EXISTS
    } else {
        ERROR_SUCCESS
    });
    Ok(h)
}

fn create_file_api(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (name, access, disposition, flags) = (a[0], a[1] as u32, a[4] as u32, a[5] as u32);
    let invalid = invalid_handle(t);
    let Some(name) = (name != 0).then(|| read_str(t, name, wide)).flatten() else {
        return fail(t, ERROR_INVALID_PARAMETER, invalid);
    };
    match create_file(t, &name, access, disposition, flags) {
        Ok(h) => Ok(h),
        Err(e) => fail(t, e, invalid),
    }
}

fn create_file2(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // (name, access, share, disposition, CREATEFILE2_EXTENDED_PARAMETERS*)
    let flags = if a[4] != 0 {
        read_u32(t, a[4] + 4).unwrap_or(0) | read_u32(t, a[4] + 8).unwrap_or(0)
    } else {
        0
    };
    create_file_api(t, &[a[0], a[1], a[2], 0, a[3], flags as u64], true)
}

// ---------------------------------------------------------------------------
// Reading and writing

/// OVERLAPPED offsets: (Offset, OffsetHigh, hEvent, InternalHigh).
fn overlapped_layout(t: &Thread<'_, '_>) -> (u64, u64, u64, u64) {
    if t.long_mode() {
        (16, 20, 24, 8)
    } else {
        (8, 12, 16, 4)
    }
}

/// Position the file for an OVERLAPPED request, if one was passed.
fn seek_overlapped(t: &mut Thread<'_, '_>, f: &mut File, ov: u64) {
    if ov == 0 {
        return;
    }
    let (lo, hi, _, _) = overlapped_layout(t);
    let off =
        read_u32(t, ov + lo).unwrap_or(0) as u64 | (read_u32(t, ov + hi).unwrap_or(0) as u64) << 32;
    let _ = f.seek(SeekFrom::Start(off));
}

/// Complete an OVERLAPPED request synchronously.
fn complete_overlapped(t: &mut Thread<'_, '_>, ov: u64, n: u64) {
    if ov == 0 {
        return;
    }
    let (_, _, ev, internal_high) = overlapped_layout(t);
    write_ptr(t, ov, 0);
    write_ptr(t, ov + internal_high, n);
    if let Some(h) = read_ptr(t, ov + ev).filter(|&h| h != 0) {
        if let Some(Object::Event { signaled, .. }) = t.k.get(h) {
            *signaled = true;
        }
        t.wake_all();
    }
}

fn read_file(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, buf, len, read_out, ov) = (a[0], a[1], a[2] as u32, a[3], a[4]);
    store_u32(t, read_out, 0);
    if console::is_console(h) {
        if h & 0xFFFF_FFFF != console::STDIN_HANDLE {
            return fail(t, ERROR_INVALID_HANDLE, 0);
        }
        let data = console::read(len as usize);
        if !write_bytes(t, buf, &data) {
            return fail(t, ERROR_NOACCESS, 0);
        }
        store_u32(t, read_out, data.len() as u32);
        return Ok(1);
    }
    let mut data = vec![0u8; len as usize];
    let n = match t.k.get(h) {
        Some(Object::File(f)) => {
            let Some(mut file) = f.file.as_ref().and_then(|f| f.try_clone().ok()) else {
                return fail(t, ERROR_ACCESS_DENIED, 0);
            };
            seek_overlapped(t, &mut file, ov);
            let mut n = 0;
            while n < data.len() {
                match file.read(&mut data[n..]) {
                    Ok(0) => break,
                    Ok(k) => n += k,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return fail(t, ERROR_ACCESS_DENIED, 0),
                }
            }
            if n == 0 && ov != 0 && len > 0 {
                return fail(t, ERROR_HANDLE_EOF, 0);
            }
            n
        }
        Some(Object::Pipe(p)) if !p.write => {
            let mut b = p.buf.borrow_mut();
            if b.data.is_empty() {
                if b.writers == 0 {
                    drop(b);
                    return fail(t, ERROR_BROKEN_PIPE, 0);
                }
                drop(b);
                loop {
                    t.wait_or_block(None, false)?;
                }
            }
            let n = b.data.len().min(data.len());
            for (d, s) in data.iter_mut().zip(b.data.drain(..n)) {
                *d = s;
            }
            n
        }
        Some(Object::Null) => 0,
        Some(Object::NamedPipeServer { c2s, .. }) => {
            let c2s = c2s.clone();
            let mut b = c2s.borrow_mut();
            if b.data.is_empty() {
                if b.writers == 0 {
                    drop(b);
                    return fail(t, ERROR_BROKEN_PIPE, 0);
                }
                drop(b);
                loop {
                    t.wait_or_block(None, false)?;
                }
            }
            let n = b.data.len().min(data.len());
            for (d, s) in data.iter_mut().zip(b.data.drain(..n)) {
                *d = s;
            }
            n
        }
        Some(Object::NamedPipeClient { s2c, .. }) => {
            let s2c = s2c.clone();
            let mut b = s2c.borrow_mut();
            if b.data.is_empty() {
                if b.writers == 0 {
                    drop(b);
                    return fail(t, ERROR_BROKEN_PIPE, 0);
                }
                drop(b);
                loop {
                    t.wait_or_block(None, false)?;
                }
            }
            let n = b.data.len().min(data.len());
            for (d, s) in data.iter_mut().zip(b.data.drain(..n)) {
                *d = s;
            }
            n
        }
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    };
    if !write_bytes(t, buf, &data[..n]) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    store_u32(t, read_out, n as u32);
    complete_overlapped(t, ov, n as u64);
    Ok(1)
}

fn write_file(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, buf, len, written, ov) = (a[0], a[1], a[2] as u32, a[3], a[4]);
    let Some(data) = guest_bytes(t, buf, len as u64) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    if console::is_console(h) {
        return match console::write(t, h, &data) {
            Some(true) => {
                store_u32(t, written, len);
                Ok(1)
            }
            Some(false) => fail(t, ERROR_NOACCESS, 0),
            None => fail(t, ERROR_INVALID_HANDLE, 0),
        };
    }
    let r = match t.k.get(h) {
        Some(Object::File(f)) => {
            let append = f.append;
            match f.file.as_ref().and_then(|f| f.try_clone().ok()) {
                None => Err(ERROR_ACCESS_DENIED),
                Some(mut file) => {
                    if append {
                        let _ = file.seek(SeekFrom::End(0));
                    } else {
                        seek_overlapped(t, &mut file, ov);
                    }
                    file.write_all(&data).map_err(|_| ERROR_ACCESS_DENIED)
                }
            }
        }
        Some(Object::Pipe(p)) if p.write => {
            let mut b = p.buf.borrow_mut();
            if b.readers == 0 {
                Err(ERROR_NO_DATA)
            } else {
                b.data.extend(&data);
                Ok(())
            }
        }
        Some(Object::Null) => Ok(()),
        Some(Object::NamedPipeServer { s2c, .. }) => {
            let s2c = s2c.clone();
            let mut b = s2c.borrow_mut();
            if b.readers == 0 {
                Err(ERROR_NO_DATA)
            } else {
                b.data.extend(&data);
                Ok(())
            }
        }
        Some(Object::NamedPipeClient { c2s, .. }) => {
            let c2s = c2s.clone();
            let mut b = c2s.borrow_mut();
            if b.readers == 0 {
                Err(ERROR_NO_DATA)
            } else {
                b.data.extend(&data);
                Ok(())
            }
        }
        _ => Err(ERROR_INVALID_HANDLE),
    };
    match r {
        Ok(()) => {
            store_u32(t, written, len);
            complete_overlapped(t, ov, len as u64);
            t.wake_all();
            Ok(1)
        }
        Err(e) => {
            store_u32(t, written, 0);
            fail(t, e, 0)
        }
    }
}

/// The open host file behind `h`.
fn file_of<'t>(t: &'t mut Thread<'_, '_>, h: u64) -> Option<&'t mut FileObj> {
    match t.k.get(h) {
        Some(Object::File(f)) => Some(f),
        _ => None,
    }
}

fn metadata_of(t: &mut Thread<'_, '_>, h: u64) -> Option<std::fs::Metadata> {
    let f = file_of(t, h)?;
    match &f.file {
        Some(file) => file.metadata().ok(),
        None => std::fs::metadata(&f.host).ok(),
    }
}

fn get_file_size(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(m) = metadata_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, MINUS_ONE);
    };
    let size = m.len();
    store_u32(t, a[1], (size >> 32) as u32);
    t.set_last_error(ERROR_SUCCESS);
    Ok(size & 0xFFFF_FFFF)
}

fn get_file_size_ex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(m) = metadata_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    Ok(write_u64(t, a[1], m.len()) as u64)
}

/// Move the file pointer; `Err` is a Windows error code.
fn seek(t: &mut Thread<'_, '_>, h: u64, dist: i64, method: u32) -> Result<u64, u32> {
    let f = file_of(t, h).ok_or(ERROR_INVALID_HANDLE)?;
    let file = f.file.as_mut().ok_or(ERROR_INVALID_HANDLE)?;
    let base = match method {
        0 => 0,
        1 => file.stream_position().map_err(|_| ERROR_INVALID_HANDLE)? as i64,
        2 => file.metadata().map_err(|_| ERROR_INVALID_HANDLE)?.len() as i64,
        _ => return Err(ERROR_INVALID_PARAMETER),
    };
    let target = base.checked_add(dist).ok_or(ERROR_NEGATIVE_SEEK)?;
    if target < 0 {
        return Err(ERROR_NEGATIVE_SEEK);
    }
    file.seek(SeekFrom::Start(target as u64))
        .map_err(|_| ERROR_INVALID_PARAMETER)
}

fn set_file_pointer(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, lo, hi_ptr, method) = (a[0], a[1] as u32, a[2], a[3] as u32);
    let dist = if hi_ptr != 0 {
        let hi = read_u32(t, hi_ptr).unwrap_or(0);
        ((hi as u64) << 32 | lo as u64) as i64
    } else {
        lo as i32 as i64
    };
    match seek(t, h, dist, method) {
        Ok(pos) => {
            store_u32(t, hi_ptr, (pos >> 32) as u32);
            t.set_last_error(ERROR_SUCCESS);
            Ok(pos & 0xFFFF_FFFF)
        }
        Err(e) => fail(t, e, MINUS_ONE),
    }
}

fn set_file_pointer_ex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // x64: (h, dist, new_pos*, method); x86: (h, dist_lo, dist_hi, new_pos*, method).
    let (h, dist, out, method) = if t.long_mode() {
        (a[0], a[1] as i64, a[2], a[3] as u32)
    } else {
        (
            a[0],
            (a[1] & 0xFFFF_FFFF | a[2] << 32) as i64,
            a[3],
            a[4] as u32,
        )
    };
    match seek(t, h, dist, method) {
        Ok(pos) => {
            if out != 0 {
                write_u64(t, out, pos);
            }
            Ok(1)
        }
        Err(e) => fail(t, e, 0),
    }
}

fn set_end_of_file(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(file) = file_of(t, a[0]).and_then(|f| f.file.as_mut()) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let ok = file.stream_position().and_then(|p| file.set_len(p)).is_ok();
    if ok {
        Ok(1)
    } else {
        fail(t, ERROR_ACCESS_DENIED, 0)
    }
}

fn get_file_type(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    const FILE_TYPE_DISK: u64 = 1;
    const FILE_TYPE_CHAR: u64 = 2;
    const FILE_TYPE_PIPE: u64 = 3;
    if console::is_console(a[0]) {
        return Ok(FILE_TYPE_CHAR);
    }
    match t.k.get(a[0]) {
        Some(Object::File(_)) => Ok(FILE_TYPE_DISK),
        Some(Object::Pipe(_))
        | Some(Object::NamedPipeServer { .. })
        | Some(Object::NamedPipeClient { .. }) => Ok(FILE_TYPE_PIPE),
        Some(Object::Null) => Ok(FILE_TYPE_CHAR),
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn get_file_time(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(m) = metadata_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    for (ptr, v) in [a[1], a[2], a[3]].into_iter().zip(times_of(&m)) {
        if ptr != 0 && !write_u64(t, ptr, v) {
            return fail(t, ERROR_NOACCESS, 0);
        }
    }
    Ok(1)
}

fn set_file_time(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let accessed = (a[2] != 0).then(|| read_u64(t, a[2])).flatten();
    let modified = (a[3] != 0).then(|| read_u64(t, a[3])).flatten();
    let Some(f) = file_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let mut times = std::fs::FileTimes::new();
    if let Some(v) = accessed.filter(|&v| v != 0 && v != u64::MAX) {
        times = times.set_accessed(system_time_of(v));
    }
    if let Some(v) = modified.filter(|&v| v != 0 && v != u64::MAX) {
        times = times.set_modified(system_time_of(v));
    }
    let r = match &f.file {
        Some(file) => file.set_times(times),
        None => File::options()
            .write(true)
            .open(&f.host)
            .and_then(|file| file.set_times(times)),
    };
    // Directories and read-only files may refuse; Windows callers rarely care.
    let _ = r;
    Ok(1)
}

fn get_file_information_by_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(m) = metadata_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let mut b = Vec::with_capacity(52);
    b.extend_from_slice(&attributes_of(&m).to_le_bytes());
    for v in times_of(&m) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&VOLUME_SERIAL.to_le_bytes());
    b.extend_from_slice(&((m.len() >> 32) as u32).to_le_bytes());
    b.extend_from_slice(&(m.len() as u32).to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    let index = file_index(&m);
    b.extend_from_slice(&((index >> 32) as u32).to_le_bytes());
    b.extend_from_slice(&(index as u32).to_le_bytes());
    Ok(write_bytes(t, a[1], &b) as u64)
}

fn file_index(m: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}

fn get_file_information_by_handle_ex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, class, buf, size) = (a[0], a[1] as u32, a[2], a[3]);
    let Some(m) = metadata_of(t, h) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let mut b = Vec::new();
    match class {
        0 => {
            // FILE_BASIC_INFO
            let [c, ac, w] = times_of(&m);
            for v in [c, ac, w, w] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&attributes_of(&m).to_le_bytes());
            b.extend_from_slice(&[0; 4]);
        }
        1 => {
            // FILE_STANDARD_INFO
            b.extend_from_slice(&round_up(m.len(), 4096).to_le_bytes());
            b.extend_from_slice(&m.len().to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            let pending = file_of(t, h).is_some_and(|f| f.delete_on_close);
            b.extend_from_slice(&[pending as u8, m.is_dir() as u8, 0, 0]);
        }
        2 => {
            // FILE_NAME_INFO: the path without the drive.
            let guest = file_of(t, h).map(|f| f.guest.clone()).unwrap_or_default();
            let name: Vec<u8> = guest[2.min(guest.len())..]
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            b.extend_from_slice(&(name.len() as u32).to_le_bytes());
            b.extend_from_slice(&name);
            if size < b.len() as u64 {
                write_bytes(t, buf, &b[..size as usize]);
                return fail(t, ERROR_MORE_DATA, 0);
            }
        }
        _ => return fail(t, ERROR_INVALID_PARAMETER, 0),
    }
    if size < b.len() as u64 {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    Ok(write_bytes(t, buf, &b) as u64)
}

fn set_file_information_by_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, class, buf) = (a[0], a[1] as u32, a[2]);
    match class {
        4 => {
            // FILE_DISPOSITION_INFO
            let del = guest_bytes(t, buf, 1).is_some_and(|b| b[0] != 0);
            match file_of(t, h) {
                Some(f) => {
                    f.delete_on_close = del;
                    Ok(1)
                }
                None => fail(t, ERROR_INVALID_HANDLE, 0),
            }
        }
        6 => {
            // FILE_END_OF_FILE_INFO
            let len = read_u64(t, buf).unwrap_or(0);
            match file_of(t, h).and_then(|f| f.file.as_mut()) {
                Some(f) if f.set_len(len).is_ok() => Ok(1),
                _ => fail(t, ERROR_ACCESS_DENIED, 0),
            }
        }
        0 | 5 => Ok(1), // basic info, allocation size
        _ => fail(t, ERROR_NOT_SUPPORTED, 0),
    }
}

// ---------------------------------------------------------------------------
// Handles

/// Clean up an object whose last handle was closed.
pub(crate) fn finalize(t: &mut Thread<'_, '_>, obj: Object) {
    match obj {
        Object::File(mut f) => {
            if f.delete_on_close {
                f.file = None;
                let _ = remove_path(&f.host);
            }
        }
        Object::Pipe(p) => {
            let mut b = p.buf.borrow_mut();
            if p.write {
                b.writers -= 1;
            } else {
                b.readers -= 1;
            }
            drop(b);
            t.wake_all();
        }
        Object::NamedPipeServer { name, c2s, s2c, .. } => {
            let key = name.to_ascii_lowercase();
            t.k.named_pipes.remove(&key);
            c2s.borrow_mut().readers = c2s.borrow().readers.saturating_sub(1);
            c2s.borrow_mut().writers = c2s.borrow().writers.saturating_sub(1);
            s2c.borrow_mut().readers = s2c.borrow().readers.saturating_sub(1);
            s2c.borrow_mut().writers = s2c.borrow().writers.saturating_sub(1);
            t.wake_all();
        }
        Object::NamedPipeClient { c2s, s2c } => {
            c2s.borrow_mut().readers = c2s.borrow().readers.saturating_sub(1);
            c2s.borrow_mut().writers = c2s.borrow().writers.saturating_sub(1);
            s2c.borrow_mut().readers = s2c.borrow().readers.saturating_sub(1);
            s2c.borrow_mut().writers = s2c.borrow().writers.saturating_sub(1);
            t.wake_all();
        }
        _ => {}
    }
}

fn close_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let h = a[0];
    if console::is_console(h) || matches!(h as u32, 0xFFFF_FFFF | 0xFFFF_FFFE) {
        return Ok(1);
    }
    match t.k.close(h) {
        None => fail(t, ERROR_INVALID_HANDLE, 0),
        Some(None) => Ok(1),
        Some(Some(obj)) => {
            finalize(t, obj);
            Ok(1)
        }
    }
}

fn duplicate_handle(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    const DUPLICATE_CLOSE_SOURCE: u64 = 1;
    let (src, out, options) = (a[1], a[3], a[6]);
    let new = match src as u32 {
        0xFFFF_FFFE => Some(t.k.insert(Object::Thread(t.current), None)),
        0xFFFF_FFFF => Some(t.k.insert(Object::SelfProcess, None)),
        _ if console::is_console(src) => Some(src),
        _ => t.k.duplicate(src),
    };
    let Some(new) = new else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    if options & DUPLICATE_CLOSE_SOURCE != 0 && !console::is_console(src) {
        close_handle(t, &[src])?;
    }
    if !store_ptr(t, out, new) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn get_handle_information(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !console::is_console(a[0]) && !t.k.contains(a[0]) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    Ok(store_u32(t, a[1], 0) as u64)
}

// ---------------------------------------------------------------------------
// Files by name

fn path_api(
    t: &mut Thread<'_, '_>,
    ptr: u64,
    wide: bool,
    op: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<u64, Halt> {
    let (_, host) = match arg_path(t, ptr, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    match op(&host) {
        Ok(()) => Ok(1),
        Err(e) => {
            let code = io_error(&e, &host);
            fail(t, code, 0)
        }
    }
}

fn delete_file(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    path_api(t, a[0], wide, |p| {
        if p.is_dir() {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        std::fs::remove_file(p)
    })
}

fn create_directory(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<u64, Halt> {
    let (_, host) = match arg_path(t, ptr, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    if host.exists() {
        return fail(t, ERROR_ALREADY_EXISTS, 0);
    }
    match std::fs::create_dir(&host) {
        Ok(()) => Ok(1),
        Err(e) => {
            let code = io_error(&e, &host);
            fail(
                t,
                if code == ERROR_FILE_NOT_FOUND {
                    ERROR_PATH_NOT_FOUND
                } else {
                    code
                },
                0,
            )
        }
    }
}

fn remove_directory(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    path_api(t, a[0], wide, |p| {
        if p.is_file() {
            return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
        }
        std::fs::remove_dir(p)
    })
}

fn copy_file(
    t: &mut Thread<'_, '_>,
    src: u64,
    dst: u64,
    fail_if_exists: bool,
    wide: bool,
) -> Result<u64, Halt> {
    let from = match arg_path(t, src, wide) {
        Ok(p) => p.1,
        Err(e) => return fail(t, e, 0),
    };
    let to = match arg_path(t, dst, wide) {
        Ok(p) => p.1,
        Err(e) => return fail(t, e, 0),
    };
    if fail_if_exists && to.exists() {
        return fail(t, ERROR_FILE_EXISTS, 0);
    }
    if from.is_dir() {
        return fail(t, ERROR_ACCESS_DENIED, 0);
    }
    match std::fs::copy(&from, &to) {
        Ok(_) => Ok(1),
        Err(e) => {
            let p = if from.exists() { &to } else { &from };
            let code = io_error(&e, p);
            fail(t, code, 0)
        }
    }
}

const MOVEFILE_REPLACE_EXISTING: u64 = 1;
const MOVEFILE_COPY_ALLOWED: u64 = 2;
const MOVEFILE_DELAY_UNTIL_REBOOT: u64 = 4;

fn move_file(
    t: &mut Thread<'_, '_>,
    src: u64,
    dst: u64,
    flags: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let from = match arg_path(t, src, wide) {
        Ok(p) => p.1,
        Err(e) => return fail(t, e, 0),
    };
    if dst == 0 {
        // Delete at reboot: there is no reboot, so leave the file alone.
        return if flags & MOVEFILE_DELAY_UNTIL_REBOOT != 0 {
            Ok(1)
        } else {
            fail(t, ERROR_INVALID_PARAMETER, 0)
        };
    }
    let to = match arg_path(t, dst, wide) {
        Ok(p) => p.1,
        Err(e) => return fail(t, e, 0),
    };
    if !from.exists() {
        return fail(t, ERROR_FILE_NOT_FOUND, 0);
    }
    if to.exists()
        && !same_file(&from, &to)
        && (flags & MOVEFILE_REPLACE_EXISTING == 0 || to.is_dir())
    {
        return fail(t, ERROR_ALREADY_EXISTS, 0);
    }
    match std::fs::rename(&from, &to) {
        Ok(()) => Ok(1),
        Err(e)
            if e.kind() == std::io::ErrorKind::CrossesDevices
                && flags & MOVEFILE_COPY_ALLOWED != 0 =>
        {
            match std::fs::copy(&from, &to).and_then(|_| std::fs::remove_file(&from)) {
                Ok(()) => Ok(1),
                Err(e) => {
                    let code = io_error(&e, &to);
                    fail(t, code, 0)
                }
            }
        }
        Err(e) => {
            let code = io_error(&e, &to);
            fail(t, code, 0)
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn get_file_attributes(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (_, host) = match arg_path(t, a[0], wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, INVALID_FILE_ATTRIBUTES),
    };
    match std::fs::metadata(&host) {
        Ok(m) => Ok(attributes_of(&m) as u64),
        Err(e) => {
            let code = io_error(&e, &host);
            fail(t, code, INVALID_FILE_ATTRIBUTES)
        }
    }
}

fn get_file_attributes_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (_, host) = match arg_path(t, a[0], wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    let m = match std::fs::metadata(&host) {
        Ok(m) => m,
        Err(e) => {
            let code = io_error(&e, &host);
            return fail(t, code, 0);
        }
    };
    // WIN32_FILE_ATTRIBUTE_DATA
    let mut b = Vec::with_capacity(36);
    b.extend_from_slice(&attributes_of(&m).to_le_bytes());
    for v in times_of(&m) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    let size = if m.is_dir() { 0 } else { m.len() };
    b.extend_from_slice(&((size >> 32) as u32).to_le_bytes());
    b.extend_from_slice(&(size as u32).to_le_bytes());
    Ok(write_bytes(t, a[2], &b) as u64)
}

fn set_file_attributes(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let attrs = a[1] as u32;
    path_api(t, a[0], wide, |p| {
        let m = std::fs::metadata(p)?;
        let mut perm = m.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(attrs & FILE_ATTRIBUTE_READONLY != 0);
        std::fs::set_permissions(p, perm)
    })
}

fn get_current_directory(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let cwd = t.k.cwd.clone();
    Ok(copy_str_out(t, a[1], a[0], &cwd, wide))
}

fn set_current_directory(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (full, host) = match arg_path(t, a[0], wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    if !host.is_dir() {
        let code = if host.exists() {
            ERROR_DIRECTORY
        } else {
            ERROR_FILE_NOT_FOUND
        };
        return fail(t, code, 0);
    }
    t.k.cwd = trim_trailing(&full);
    Ok(1)
}

/// The temporary directory, with a trailing backslash, created if missing.
pub(crate) fn temp_dir(t: &Thread<'_, '_>) -> String {
    let tmp = crate::sysinfo::env_var(t, "TMP")
        .or_else(|| crate::sysinfo::env_var(t, "TEMP"))
        .unwrap_or_else(|| r"C:\Windows\Temp".into());
    if let Some(h) = host_path(t, &tmp) {
        let _ = std::fs::create_dir_all(h);
    }
    let mut tmp = full_path(t, &tmp);
    if !tmp.ends_with('\\') {
        tmp.push('\\');
    }
    tmp
}

fn get_temp_path(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let tmp = temp_dir(t);
    Ok(copy_str_out(t, a[1], a[0], &tmp, wide))
}

fn get_temp_file_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (dir, prefix, unique, out) = (a[0], a[1], a[2] as u32, a[3]);
    let Some(dir) = read_str(t, dir, wide) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let prefix: String = opt_str(t, prefix, wide)
        .ok()
        .flatten()
        .unwrap_or_default()
        .chars()
        .take(3)
        .collect();
    let dir = full_path(t, &dir);
    let Some(host_dir) = host_path(t, &dir).filter(|d| d.is_dir()) else {
        return fail(t, ERROR_DIRECTORY, 0);
    };
    let name = |u: u32| {
        format!(
            "{}{prefix}{:X}.tmp",
            if dir.ends_with('\\') {
                dir.clone()
            } else {
                format!(r"{dir}\")
            },
            u
        )
    };
    let (path, u) = if unique != 0 {
        (name(unique & 0xFFFF), unique & 0xFFFF)
    } else {
        let mut u = ((crate::time::filetime_now() / 10_000) & 0xFFFF) as u32;
        let mut found = None;
        for _ in 0..0x10000 {
            u = if u >= 0xFFFF { 1 } else { u + 1 };
            let candidate = format!("{prefix}{u:X}.tmp");
            let host = host_dir.join(&candidate);
            if OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&host)
                .is_ok()
            {
                found = Some(u);
                break;
            }
        }
        match found {
            Some(u) => (name(u), u),
            None => return fail(t, 80, 0),
        }
    };
    if !write_bytes(t, out, &encode(&path, wide)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(u as u64)
}

fn get_full_path_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (name, size, buf, part) = (a[0], a[1], a[2], a[3]);
    let Some(name) = (name != 0).then(|| read_str(t, name, wide)).flatten() else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    if name.is_empty() {
        return fail(t, ERROR_INVALID_NAME, 0);
    }
    let full = full_path(t, &name);
    let n = copy_str_out(t, buf, size, &full, wide);
    if n == char_len(&full, wide) && part != 0 {
        let file_part = if full.ends_with('\\') {
            0
        } else {
            let last = full.rsplit('\\').next().unwrap_or("");
            let off = char_len(&full, wide) - char_len(last, wide);
            buf + off * if wide { 2 } else { 1 }
        };
        store_ptr(t, part, file_part);
    }
    Ok(n)
}

/// GetLongPathName / GetShortPathName: AnyOS has no 8.3 names, so the path
/// comes back as given once it exists.
fn same_path_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (src, buf, size) = (a[0], a[1], a[2]);
    let (_, host) = match arg_path(t, src, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    if !host.exists() {
        return fail(t, ERROR_FILE_NOT_FOUND, 0);
    }
    let s = read_str(t, src, wide).unwrap_or_default();
    Ok(copy_str_out(t, buf, size, &s, wide))
}

fn search_path(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (path, file, ext, size, buf, part) = (a[0], a[1], a[2], a[3], a[4], a[5]);
    let Ok(Some(file)) = opt_str(t, file, wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let ext = opt_str(t, ext, wide).ok().flatten();
    let name = match ext {
        Some(e) if !file.rsplit('\\').next().unwrap_or("").contains('.') => format!("{file}{e}"),
        _ => file.clone(),
    };
    let dirs: Vec<String> = if name.contains(['\\', '/', ':']) {
        vec![String::new()]
    } else if let Ok(Some(p)) = opt_str(t, path, wide) {
        p.split(';')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    } else {
        let mut d = Vec::new();
        if let Some(p) = t.process.modules[0].path.as_ref().and_then(|p| p.parent()) {
            d.push(guest_path(t, p));
        }
        d.push(t.k.cwd.clone());
        d.push(r"C:\Windows\System32".into());
        d.push(r"C:\Windows".into());
        if let Some(p) = crate::sysinfo::env_var(t, "PATH") {
            d.extend(p.split(';').filter(|s| !s.is_empty()).map(String::from));
        }
        d
    };
    for d in dirs {
        let candidate = if d.is_empty() {
            full_path(t, &name)
        } else {
            full_path(t, &format!(r"{d}\{name}"))
        };
        if host_path(t, &candidate).is_some_and(|h| h.exists()) {
            let n = copy_str_out(t, buf, size, &candidate, wide);
            if n == char_len(&candidate, wide) && part != 0 {
                let last = candidate.rsplit('\\').next().unwrap_or("");
                let off = char_len(&candidate, wide) - char_len(last, wide);
                store_ptr(t, part, buf + off * if wide { 2 } else { 1 });
            }
            return Ok(n);
        }
    }
    fail(t, ERROR_FILE_NOT_FOUND, 0)
}

// ---------------------------------------------------------------------------
// Drives and volumes

const VOLUME_SERIAL: u32 = 0x414E_594F;

fn drive_letters(t: &Thread<'_, '_>) -> Vec<char> {
    let mut v = Vec::new();
    if t.host.drive_c.is_some() || cfg!(windows) {
        v.push('C');
    }
    if cfg!(unix) {
        v.push('Z');
    }
    v
}

/// The drive letter of a root path argument (`NULL`: the current drive).
fn drive_arg(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Option<char> {
    let s = if ptr == 0 {
        t.k.cwd.clone()
    } else {
        read_str(t, ptr, wide)?
    };
    let full = full_path(t, &s);
    full.chars().next().map(|c| c.to_ascii_uppercase())
}

fn get_drive_type(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    const DRIVE_NO_ROOT_DIR: u64 = 1;
    const DRIVE_FIXED: u64 = 3;
    let d = drive_arg(t, a[0], wide);
    Ok(if d.is_some_and(|d| drive_letters(t).contains(&d)) {
        DRIVE_FIXED
    } else {
        DRIVE_NO_ROOT_DIR
    })
}

fn get_logical_drive_strings(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let mut s = String::new();
    for d in drive_letters(t) {
        s.push_str(&format!("{d}:\\\0"));
    }
    let len = char_len(&s, wide);
    if a[0] <= len {
        return Ok(len + 1);
    }
    write_bytes(t, a[1], &encode(&s, wide));
    Ok(len)
}

/// (free bytes for the caller, total bytes, free bytes) on the drive's host volume.
fn disk_space(t: &Thread<'_, '_>, drive: char) -> (u64, u64, u64) {
    #[cfg(unix)]
    if let Some(p) = host_path(t, &format!(r"{drive}:\")) {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) {
            let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
            // SAFETY: statvfs writes only the struct we pass.
            if unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0 {
                let unit = s.f_frsize as u64;
                return (
                    s.f_bavail as u64 * unit,
                    s.f_blocks as u64 * unit,
                    s.f_bfree as u64 * unit,
                );
            }
        }
    }
    let _ = (t, drive);
    (100 << 30, 250 << 30, 100 << 30)
}

fn get_disk_free_space_ex(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(d) = drive_arg(t, a[0], wide) else {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    };
    let (avail, total, free) = disk_space(t, d);
    for (p, v) in [(a[1], avail), (a[2], total), (a[3], free)] {
        if p != 0 {
            write_u64(t, p, v);
        }
    }
    Ok(1)
}

fn get_disk_free_space(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(d) = drive_arg(t, a[0], wide) else {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    };
    let (avail, total, _) = disk_space(t, d);
    let cluster = 4096u64;
    store_u32(t, a[1], 8);
    store_u32(t, a[2], 512);
    store_u32(t, a[3], (avail / cluster).min(u32::MAX as u64) as u32);
    store_u32(t, a[4], (total / cluster).min(u32::MAX as u64) as u32);
    Ok(1)
}

fn get_volume_information(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(d) = drive_arg(t, a[0], wide) else {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    };
    if !drive_letters(t).contains(&d) {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    }
    let label = if d == 'C' { "Windows" } else { "Host" };
    if a[1] != 0 {
        copy_str_truncate(t, a[1], a[2], label, wide);
    }
    store_u32(t, a[3], VOLUME_SERIAL);
    store_u32(t, a[4], 255);
    // Case preserved and Unicode names, persistent ACLs.
    store_u32(t, a[5], 0x0000_000E);
    if a[6] != 0 {
        copy_str_truncate(t, a[6], a[7], "NTFS", wide);
    }
    Ok(1)
}

// ---------------------------------------------------------------------------
// FindFirstFile

/// Windows wildcard match, ignoring case: `*` and `?`; `*.*` matches names
/// without a dot too.
pub(crate) fn wildcard_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    if p.ends_with(&['.', '*']) && !n.contains(&'.') && wildcard(&p[..p.len() - 2], &n) {
        return true;
    }
    wildcard(&p, &n)
}

fn wildcard(p: &[char], n: &[char]) -> bool {
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn find_entry(name: String, m: &std::fs::Metadata) -> FindEntry {
    FindEntry {
        name,
        attrs: attributes_of(m),
        size: if m.is_dir() { 0 } else { m.len() },
        times: times_of(m),
    }
}

fn write_find_data(t: &mut Thread<'_, '_>, buf: u64, e: &FindEntry, wide: bool) -> bool {
    let mut b = Vec::new();
    b.extend_from_slice(&e.attrs.to_le_bytes());
    for v in e.times {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&((e.size >> 32) as u32).to_le_bytes());
    b.extend_from_slice(&(e.size as u32).to_le_bytes());
    b.extend_from_slice(&[0; 8]);
    let (name_len, alt_len, total) = if wide { (520, 28, 592) } else { (260, 14, 320) };
    let mut name = encode(&e.name, wide);
    name.truncate(name_len);
    name.resize(name_len + alt_len, 0);
    b.extend_from_slice(&name);
    b.resize(total, 0);
    write_bytes(t, buf, &b)
}

fn find_first_file(
    t: &mut Thread<'_, '_>,
    pattern: u64,
    data: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let invalid = invalid_handle(t);
    let Some(pat) = (pattern != 0).then(|| read_str(t, pattern, wide)).flatten() else {
        return fail(t, ERROR_INVALID_PARAMETER, invalid);
    };
    let full = full_path(t, &pat);
    if full.ends_with('\\') {
        return fail(t, ERROR_FILE_NOT_FOUND, invalid);
    }
    let (dir, mask) = full.rsplit_once('\\').unwrap_or((&full, "*"));
    let dir = if dir.len() == 2 {
        format!(r"{dir}\")
    } else {
        dir.to_string()
    };
    let Some(host_dir) = host_path(t, &dir) else {
        return fail(t, ERROR_PATH_NOT_FOUND, invalid);
    };
    let rd = match std::fs::read_dir(&host_dir) {
        Ok(rd) => rd,
        Err(e) => {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                ERROR_PATH_NOT_FOUND
            } else {
                io_error(&e, &host_dir)
            };
            return fail(t, code, invalid);
        }
    };
    let mut entries = VecDeque::new();
    if dir.len() > 3 {
        if let Ok(m) = std::fs::metadata(&host_dir) {
            for dot in [".", ".."] {
                if wildcard_match(mask, dot) {
                    entries.push_back(find_entry(dot.into(), &m));
                }
            }
        }
    }
    let mut names: Vec<(String, std::fs::Metadata)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let m = std::fs::metadata(e.path()).ok()?;
            Some((name, m))
        })
        .filter(|(n, _)| wildcard_match(mask, n))
        .collect();
    names.sort_by_key(|a| a.0.to_lowercase());
    entries.extend(names.into_iter().map(|(n, m)| find_entry(n, &m)));
    let Some(first) = entries.pop_front() else {
        return fail(t, ERROR_FILE_NOT_FOUND, invalid);
    };
    if !write_find_data(t, data, &first, wide) {
        return fail(t, ERROR_NOACCESS, invalid);
    }
    Ok(t.k.insert(Object::Find(FindObj { entries, wide }), None))
}

fn find_next_file(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let next = match t.k.get(a[0]) {
        Some(Object::Find(f)) => f.entries.pop_front().map(|e| (e, f.wide)),
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    };
    match next {
        Some((e, wide)) => Ok(write_find_data(t, a[1], &e, wide) as u64),
        None => fail(t, ERROR_NO_MORE_FILES, 0),
    }
}

fn find_close(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !matches!(t.k.get(a[0]), Some(Object::Find(_))) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    t.k.close(a[0]);
    Ok(1)
}

// ---------------------------------------------------------------------------
// File mappings

const FILE_MAP_WRITE: u32 = 0x2;
const FILE_MAP_ALL_ACCESS: u32 = 0xF001F;
const FILE_MAP_COPY: u32 = 0x1;

fn create_file_mapping(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (hfile, protect, max_hi, max_lo, name) = (a[0], a[2] as u32, a[3], a[4], a[5]);
    let max = (max_hi & 0xFFFF_FFFF) << 32 | max_lo & 0xFFFF_FFFF;
    let writable = matches!(protect & 0xFF, 0x04 | 0x40);
    let name = match opt_str(t, name, wide) {
        Ok(n) => n,
        Err(()) => return Ok(0),
    };
    if let Some(n) = &name {
        if t.k.named(n).is_some() {
            let h = t.k.open_named(n).unwrap_or(0);
            t.set_last_error(ERROR_ALREADY_EXISTS);
            return Ok(h);
        }
    }
    let backing = if hfile as u32 == 0xFFFF_FFFF {
        if max == 0 {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        }
        MappingBacking {
            file: None,
            data: vec![0; max as usize],
            size: max,
            writable,
        }
    } else {
        let Some(f) = file_of(t, hfile) else {
            return fail(t, ERROR_INVALID_HANDLE, 0);
        };
        let Some(file) = f.file.as_ref().and_then(|f| f.try_clone().ok()) else {
            return fail(t, ERROR_ACCESS_DENIED, 0);
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let size = if max == 0 { len } else { max };
        if size == 0 {
            return fail(t, 1006, 0); // ERROR_FILE_INVALID
        }
        if size > len && writable {
            let _ = file.set_len(size);
        }
        MappingBacking {
            file: Some(file),
            data: Vec::new(),
            size,
            writable,
        }
    };
    let h = t.k.insert(
        Object::Mapping(MappingObj(Rc::new(RefCell::new(backing)))),
        name,
    );
    t.set_last_error(ERROR_SUCCESS);
    Ok(h)
}

fn open_file_mapping(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Ok(Some(name)) = opt_str(t, a[2], wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    match t.k.named(&name) {
        Some(Object::Mapping(_)) => Ok(t.k.open_named(&name).unwrap_or(0)),
        _ => fail(t, ERROR_FILE_NOT_FOUND, 0),
    }
}

fn map_view(t: &mut Thread<'_, '_>, a: &[u64], at: u64) -> Result<u64, Halt> {
    let (h, access, off_hi, off_lo, bytes) = (a[0], a[1] as u32, a[2], a[3], a[4]);
    let offset = (off_hi & 0xFFFF_FFFF) << 32 | off_lo & 0xFFFF_FFFF;
    let backing = match t.k.get(h) {
        Some(Object::Mapping(m)) => m.0.clone(),
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    };
    let (size, map_writable) = {
        let b = backing.borrow();
        (b.size, b.writable)
    };
    if offset >= size {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let len = if bytes == 0 {
        size - offset
    } else {
        bytes.min(size - offset)
    };
    let write = access & (FILE_MAP_WRITE | FILE_MAP_ALL_ACCESS) & FILE_MAP_WRITE != 0
        || access == FILE_MAP_ALL_ACCESS;
    let copy = access & FILE_MAP_COPY != 0;
    if write && !map_writable {
        return fail(t, ERROR_ACCESS_DENIED, 0);
    }
    let prot = if write || copy {
        Protection::RW
    } else {
        Protection::R
    };
    let map_len = round_up(len, PAGE_SIZE);
    let base = if at != 0 {
        match t
            .process
            .memory
            .map(at, map_len, Protection::RW, "file view")
        {
            Ok(()) => at,
            Err(_) => return fail(t, ERROR_INVALID_ADDRESS, 0),
        }
    } else {
        match t
            .process
            .memory
            .map_anywhere(map_len, Protection::RW, "file view", None)
        {
            Ok(b) => b,
            Err(_) => return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0),
        }
    };
    let mut data = vec![0u8; len as usize];
    {
        let mut b = backing.borrow_mut();
        match b.file.as_mut() {
            Some(f) => {
                let _ = f.seek(SeekFrom::Start(offset)).and_then(|_| {
                    let mut n = 0;
                    while n < data.len() {
                        match f.read(&mut data[n..])? {
                            0 => break,
                            k => n += k,
                        }
                    }
                    Ok(())
                });
            }
            None => data.copy_from_slice(&b.data[offset as usize..(offset + len) as usize]),
        }
    }
    t.process.memory.write_raw(base, &data)?;
    t.process.memory.protect(base, map_len, prot)?;
    t.k.views.insert(
        base,
        View {
            backing,
            offset,
            len,
            writable: write,
        },
    );
    Ok(base)
}

/// Write a writable view back to its file or anonymous backing.
fn flush_view(t: &mut Thread<'_, '_>, base: u64) {
    let Some(v) = t.k.views.get(&base) else {
        return;
    };
    if !v.writable {
        return;
    }
    let mut data = vec![0u8; v.len as usize];
    if t.process.memory.read_raw(base, &mut data).is_err() {
        return;
    }
    let mut b = v.backing.borrow_mut();
    let off = v.offset;
    match b.file.as_mut() {
        Some(f) => {
            let _ = f
                .seek(SeekFrom::Start(off))
                .and_then(|_| f.write_all(&data));
        }
        None => b.data[off as usize..off as usize + data.len()].copy_from_slice(&data),
    }
}

fn view_base(t: &Thread<'_, '_>, addr: u64) -> Option<u64> {
    t.k.views
        .range(..=addr)
        .next_back()
        .filter(|(&b, v)| addr < b + round_up(v.len.max(1), PAGE_SIZE))
        .map(|(&b, _)| b)
}

fn unmap_view(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(base) = view_base(t, a[0]) else {
        return fail(t, ERROR_INVALID_ADDRESS, 0);
    };
    flush_view(t, base);
    t.k.views.remove(&base);
    let _ = t.process.memory.unmap(base);
    Ok(1)
}

fn flush_view_api(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match view_base(t, a[0]) {
        Some(b) => {
            flush_view(t, b);
            Ok(1)
        }
        None => fail(t, ERROR_INVALID_ADDRESS, 0),
    }
}

// ---------------------------------------------------------------------------
// Anonymous pipes (within the process)

fn create_pipe(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let buf = Rc::new(RefCell::new(PipeBuf {
        data: VecDeque::new(),
        readers: 1,
        writers: 1,
    }));
    let r = t.k.insert(
        Object::Pipe(PipeEnd {
            buf: buf.clone(),
            write: false,
        }),
        None,
    );
    let w = t.k.insert(Object::Pipe(PipeEnd { buf, write: true }), None);
    if !store_ptr(t, a[0], r) || !store_ptr(t, a[1], w) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn peek_named_pipe(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, buf, size, read_out, avail, left) = (a[0], a[1], a[2] as usize, a[3], a[4], a[5]);
    let data: Vec<u8> = match t.k.get(h) {
        Some(Object::Pipe(p)) => {
            let b = p.buf.borrow();
            if b.data.is_empty() && b.writers == 0 {
                drop(b);
                return fail(t, ERROR_BROKEN_PIPE, 0);
            }
            b.data.iter().copied().collect()
        }
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    };
    let n = data.len().min(size);
    if buf != 0 && n > 0 {
        write_bytes(t, buf, &data[..n]);
    }
    store_u32(t, read_out, n as u32);
    store_u32(t, avail, data.len() as u32);
    store_u32(t, left, 0);
    Ok(1)
}

// ---------------------------------------------------------------------------
// INI files

struct Ini {
    /// (section, [(key, value)]), in file order. Lines that are not keys are kept as key "".
    sections: Vec<(String, Vec<(String, String)>)>,
    utf16: bool,
}

fn ini_path(t: &Thread<'_, '_>, file: &str) -> Option<PathBuf> {
    let full = if file.contains(['\\', '/']) {
        file.to_string()
    } else {
        format!(r"C:\Windows\{file}")
    };
    host_path(t, &full)
}

fn ini_load(path: &Path) -> Ini {
    let bytes = std::fs::read(path).unwrap_or_default();
    let (text, utf16) = if bytes.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        (String::from_utf16_lossy(&u), true)
    } else {
        let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        (String::from_utf8_lossy(b).into_owned(), false)
    };
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(name) = l
            .strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .map(|(n, _)| n.trim())
        {
            sections.push((name.to_string(), Vec::new()));
        } else if let Some(sec) = sections.last_mut() {
            if l.is_empty() || l.starts_with(';') {
                continue;
            }
            match l.split_once('=') {
                Some((k, v)) => sec.1.push((k.trim().to_string(), v.trim().to_string())),
                None => sec.1.push((l.to_string(), String::new())),
            }
        }
    }
    Ini { sections, utf16 }
}

fn ini_save(path: &Path, ini: &Ini) -> bool {
    let mut text = String::new();
    for (name, keys) in &ini.sections {
        text.push_str(&format!("[{name}]\r\n"));
        for (k, v) in keys {
            text.push_str(&format!("{k}={v}\r\n"));
        }
    }
    let bytes = if ini.utf16 {
        [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    } else {
        text.into_bytes()
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, bytes).is_ok()
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Copy a list of NUL-separated strings (double-NUL terminated) out, the way
/// the profile APIs do; returns the characters copied without the last NUL.
fn copy_list_out(t: &mut Thread<'_, '_>, buf: u64, size: u64, items: &[String], wide: bool) -> u64 {
    if size < 2 {
        return 0;
    }
    let mut units: Vec<u16> = Vec::new();
    let to_units = |s: &str| -> Vec<u16> {
        if wide {
            s.encode_utf16().collect()
        } else {
            crate::strings::string_to_ansi(s)
                .into_iter()
                .map(u16::from)
                .collect()
        }
    };
    let mut truncated = false;
    for it in items {
        let u = to_units(it);
        if units.len() + u.len() + 2 > size as usize {
            truncated = true;
            break;
        }
        units.extend(u);
        units.push(0);
    }
    let n = if truncated {
        size as usize - 2
    } else {
        units.len()
    };
    units.truncate(n);
    units.resize(n + 1, 0);
    if units.len() < 2 || units[units.len() - 2] != 0 {
        units.push(0);
    }
    let bytes: Vec<u8> = if wide {
        units.iter().flat_map(|u| u.to_le_bytes()).collect()
    } else {
        units.iter().map(|&u| u as u8).collect()
    };
    write_bytes(t, buf, &bytes);
    n as u64
}

fn get_private_profile_string(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
    file: Option<&str>,
) -> Result<u64, Halt> {
    let (section, key, default, buf, size) = (a[0], a[1], a[2], a[3], a[4]);
    let section = opt_str(t, section, wide).ok().flatten();
    let key = opt_str(t, key, wide).ok().flatten();
    let default = opt_str(t, default, wide).ok().flatten().unwrap_or_default();
    let file = match file {
        Some(f) => Some(f.to_string()),
        None => opt_str(t, a[5], wide).ok().flatten(),
    };
    let ini = file
        .as_deref()
        .and_then(|f| ini_path(t, f))
        .map(|p| ini_load(&p))
        .unwrap_or(Ini {
            sections: Vec::new(),
            utf16: false,
        });
    let Some(section) = section else {
        let names: Vec<String> = ini.sections.iter().map(|s| s.0.clone()).collect();
        return Ok(copy_list_out(t, buf, size, &names, wide));
    };
    let sec = ini
        .sections
        .iter()
        .find(|s| s.0.eq_ignore_ascii_case(&section));
    let Some(key) = key else {
        let keys: Vec<String> = sec
            .map(|s| s.1.iter().map(|k| k.0.clone()).collect())
            .unwrap_or_default();
        return Ok(copy_list_out(t, buf, size, &keys, wide));
    };
    let value = sec
        .and_then(|s| s.1.iter().find(|k| k.0.eq_ignore_ascii_case(&key)))
        .map(|k| unquote(&k.1).to_string());
    let found = value.is_some();
    let value = value.unwrap_or_else(|| default.trim_end().to_string());
    let (n, _) = copy_str_truncate(t, buf, size, &value, wide);
    t.set_last_error(if found {
        ERROR_SUCCESS
    } else {
        ERROR_FILE_NOT_FOUND
    });
    Ok(n)
}

fn get_private_profile_int(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
    file: Option<&str>,
) -> Result<u64, Halt> {
    let section = opt_str(t, a[0], wide).ok().flatten().unwrap_or_default();
    let key = opt_str(t, a[1], wide).ok().flatten().unwrap_or_default();
    let file = match file {
        Some(f) => Some(f.to_string()),
        None => opt_str(t, a[3], wide).ok().flatten(),
    };
    let ini = file
        .as_deref()
        .and_then(|f| ini_path(t, f))
        .map(|p| ini_load(&p));
    let value = ini.and_then(|ini| {
        ini.sections
            .iter()
            .find(|s| s.0.eq_ignore_ascii_case(&section))
            .and_then(|s| {
                s.1.iter()
                    .find(|k| k.0.eq_ignore_ascii_case(&key))
                    .map(|k| unquote(&k.1).to_string())
            })
    });
    let Some(v) = value else {
        return Ok(a[2] & 0xFFFF_FFFF);
    };
    let digits: String = v
        .trim()
        .chars()
        .enumerate()
        .take_while(|&(i, c)| c.is_ascii_digit() || (i == 0 && c == '-'))
        .map(|(_, c)| c)
        .collect();
    Ok(digits.parse::<i64>().unwrap_or(0) as u32 as u64)
}

fn write_private_profile_string(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
    file: Option<&str>,
) -> Result<u64, Halt> {
    let Ok(Some(section)) = opt_str(t, a[0], wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let key = opt_str(t, a[1], wide).ok().flatten();
    let value = opt_str(t, a[2], wide).ok().flatten();
    let file = match file {
        Some(f) => Some(f.to_string()),
        None => opt_str(t, a[3], wide).ok().flatten(),
    };
    let Some(path) = file.as_deref().and_then(|f| ini_path(t, f)) else {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    };
    let mut ini = ini_load(&path);
    let idx = ini
        .sections
        .iter()
        .position(|s| s.0.eq_ignore_ascii_case(&section));
    match (key, value, idx) {
        (None, _, Some(i)) => {
            ini.sections.remove(i);
        }
        (None, _, None) => {}
        (Some(k), None, Some(i)) => ini.sections[i].1.retain(|e| !e.0.eq_ignore_ascii_case(&k)),
        (Some(_), None, None) => {}
        (Some(k), Some(v), idx) => {
            let i = idx.unwrap_or_else(|| {
                ini.sections.push((section.clone(), Vec::new()));
                ini.sections.len() - 1
            });
            let keys = &mut ini.sections[i].1;
            match keys.iter_mut().find(|e| e.0.eq_ignore_ascii_case(&k)) {
                Some(e) => e.1 = v,
                None => keys.push((k, v)),
            }
        }
    }
    if ini_save(&path, &ini) {
        Ok(1)
    } else {
        fail(t, ERROR_ACCESS_DENIED, 0)
    }
}

fn get_private_profile_section(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let section = opt_str(t, a[0], wide).ok().flatten().unwrap_or_default();
    let file = opt_str(t, a[3], wide).ok().flatten().unwrap_or_default();
    let ini = ini_path(t, &file).map(|p| ini_load(&p));
    let items: Vec<String> = ini
        .and_then(|ini| {
            ini.sections
                .into_iter()
                .find(|s| s.0.eq_ignore_ascii_case(&section))
        })
        .map(|s| {
            s.1.into_iter()
                .map(|(k, v)| if v.is_empty() { k } else { format!("{k}={v}") })
                .collect()
        })
        .unwrap_or_default();
    Ok(copy_list_out(t, a[1], a[2], &items, wide))
}

fn get_private_profile_section_names(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
) -> Result<u64, Halt> {
    let file = opt_str(t, a[2], wide).ok().flatten().unwrap_or_default();
    let names: Vec<String> = ini_path(t, &file)
        .map(|p| ini_load(&p).sections.into_iter().map(|s| s.0).collect())
        .unwrap_or_default();
    Ok(copy_list_out(t, a[0], a[1], &names, wide))
}

fn write_private_profile_section(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
) -> Result<u64, Halt> {
    let Ok(Some(section)) = opt_str(t, a[0], wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let file = opt_str(t, a[2], wide).ok().flatten().unwrap_or_default();
    let Some(path) = ini_path(t, &file) else {
        return fail(t, ERROR_PATH_NOT_FOUND, 0);
    };
    // The data is "key=value\0key=value\0\0".
    let mut entries = Vec::new();
    let mut p = a[1];
    let step = if wide { 2 } else { 1 };
    while let Some(s) = read_str(t, p, wide) {
        if s.is_empty() {
            break;
        }
        p += (char_len(&s, wide) + 1) * step;
        entries.push(match s.split_once('=') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (s, String::new()),
        });
    }
    let mut ini = ini_load(&path);
    match ini
        .sections
        .iter_mut()
        .find(|s| s.0.eq_ignore_ascii_case(&section))
    {
        Some(s) => s.1 = entries,
        None => ini.sections.push((section, entries)),
    }
    if ini_save(&path, &ini) {
        Ok(1)
    } else {
        fail(t, ERROR_ACCESS_DENIED, 0)
    }
}

// ---------------------------------------------------------------------------
// Named pipes

fn create_named_pipe(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let invalid = invalid_handle(t);
    let Some(name) = (a[0] != 0).then(|| read_str(t, a[0], wide)).flatten() else {
        return fail(t, ERROR_INVALID_PARAMETER, invalid);
    };
    let pipe_name = name
        .strip_prefix(r"\\.\pipe\")
        .or_else(|| name.strip_prefix(r"\\.\Pipe\"))
        .or_else(|| name.strip_prefix(r"//./pipe/"))
        .unwrap_or(&name);
    let key = pipe_name.to_ascii_lowercase();
    if t.k.named_pipes.contains_key(&key) {
        return fail(t, ERROR_ALREADY_EXISTS, invalid);
    }
    let c2s = Rc::new(RefCell::new(PipeBuf {
        data: std::collections::VecDeque::new(),
        readers: 1,
        writers: 1,
    }));
    let s2c = Rc::new(RefCell::new(PipeBuf {
        data: std::collections::VecDeque::new(),
        readers: 1,
        writers: 1,
    }));
    let h = t.k.insert(
        Object::NamedPipeServer {
            name: key.clone(),
            c2s,
            s2c,
            connected: false,
        },
        None,
    );
    t.k.named_pipes.insert(key, h);
    t.set_last_error(ERROR_SUCCESS);
    Ok(h)
}

fn connect_named_pipe(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match t.k.get(a[0]) {
        Some(Object::NamedPipeServer { connected, .. }) => {
            if *connected {
                return Ok(1);
            }
        }
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    }
    loop {
        t.wait_or_block(None, false)?;
    }
}

fn disconnect_named_pipe(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match t.k.get(a[0]) {
        Some(Object::NamedPipeServer { connected, c2s, s2c, .. }) => {
            *connected = false;
            c2s.borrow_mut().data.clear();
            s2c.borrow_mut().data.clear();
            Ok(1)
        }
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn wait_named_pipe(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(name) = (a[0] != 0).then(|| read_str(t, a[0], wide)).flatten() else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let pipe_name = name
        .strip_prefix(r"\\.\pipe\")
        .or_else(|| name.strip_prefix(r"\\.\Pipe\"))
        .or_else(|| name.strip_prefix(r"//./pipe/"))
        .unwrap_or(&name);
    let key = pipe_name.to_ascii_lowercase();
    if t.k.named_pipes.contains_key(&key) {
        Ok(1)
    } else {
        fail(t, ERROR_FILE_NOT_FOUND, 0)
    }
}

fn get_named_pipe_info(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match t.k.get(a[0]) {
        Some(Object::NamedPipeServer { .. }) | Some(Object::NamedPipeClient { .. }) => {
            store_u32(t, a[1], 1); // PIPE_SERVER_END or PIPE_CLIENT_END
            store_u32(t, a[2], 4096);
            store_u32(t, a[3], 4096);
            store_u32(t, a[4], 1);
            Ok(1)
        }
        Some(Object::Pipe(_)) => {
            store_u32(t, a[1], 0);
            store_u32(t, a[2], 4096);
            store_u32(t, a[3], 4096);
            store_u32(t, a[4], 1);
            Ok(1)
        }
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn get_named_pipe_handle_state(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match t.k.get(a[0]) {
        Some(Object::NamedPipeServer { .. })
        | Some(Object::NamedPipeClient { .. })
        | Some(Object::Pipe(_)) => {
            store_u32(t, a[1], 0); // PIPE_READMODE_BYTE | PIPE_WAIT
            store_u32(t, a[2], 1);
            store_u32(t, a[3], 0);
            Ok(1)
        }
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn create_hard_link(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (link_name, existing_name) = (a[0], a[1]);
    let (_, existing_host) = match arg_path(t, existing_name, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    let (_, link_host) = match arg_path(t, link_name, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    match std::fs::hard_link(&existing_host, &link_host) {
        Ok(()) => Ok(1),
        Err(e) => {
            let code = io_error(&e, &link_host);
            fail(t, code, 0)
        }
    }
}

fn create_symbolic_link(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (link_name, target_name) = (a[0], a[1]);
    let (_, link_host) = match arg_path(t, link_name, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    let (_, target_host) = match arg_path(t, target_name, wide) {
        Ok(p) => p,
        Err(e) => return fail(t, e, 0),
    };
    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(&target_host, &link_host) {
            Ok(()) => return Ok(1),
            Err(e) => {
                let code = io_error(&e, &link_host);
                return fail(t, code, 0);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (link_host, target_host);
        fail(t, ERROR_NOT_SUPPORTED, 0)
    }
}

fn get_final_path_name(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(f) = file_of(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let path = format!(r"\\?\{}", f.guest);
    Ok(copy_str_out(t, a[1], a[2], &path, wide))
}

pub(crate) static APIS: &[Api] = &[
    k32("CreateFileA", 7, |t, a| create_file_api(t, a, false)),
    k32("CreateFileW", 7, |t, a| create_file_api(t, a, true)),
    k32("CreateFile2", 5, create_file2),
    k32("ReadFile", 5, read_file),
    k32("WriteFile", 5, write_file),
    k32("ReadFileEx", 5, |t, a| {
        read_file(t, &[a[0], a[1], a[2], 0, a[3]])
    }),
    k32("WriteFileEx", 5, |t, a| {
        write_file(t, &[a[0], a[1], a[2], 0, a[3]])
    }),
    k32("GetOverlappedResult", 4, |t, a| {
        let (_, _, _, internal_high) = overlapped_layout(t);
        let n = read_ptr(t, a[1] + internal_high).unwrap_or(0);
        store_u32(t, a[2], n as u32);
        Ok(1)
    }),
    k32("CancelIo", 1, |_, _| Ok(1)),
    k32("CancelIoEx", 2, |_, _| Ok(1)),
    k32("CloseHandle", 1, close_handle),
    k32("DuplicateHandle", 7, duplicate_handle),
    k32("GetHandleInformation", 2, get_handle_information),
    k32("SetHandleInformation", 3, |_, _| Ok(1)),
    k32("CompareObjectHandles", 2, |_, a| Ok((a[0] == a[1]) as u64)),
    k32("GetFileSize", 2, get_file_size),
    k32("GetFileSizeEx", 2, get_file_size_ex),
    k32("SetFilePointer", 4, set_file_pointer),
    k32x("SetFilePointerEx", 4, 5, set_file_pointer_ex),
    k32("SetEndOfFile", 1, set_end_of_file),
    k32x("SetFileValidData", 2, 3, |_, _| Ok(1)),
    k32("FlushFileBuffers", 1, |t, a| {
        if let Some(f) = file_of(t, a[0]).and_then(|f| f.file.as_mut()) {
            let _ = f.flush();
        }
        Ok(1)
    }),
    k32("GetFileType", 1, get_file_type),
    k32("GetFileTime", 4, get_file_time),
    k32("SetFileTime", 4, set_file_time),
    k32(
        "GetFileInformationByHandle",
        2,
        get_file_information_by_handle,
    ),
    k32(
        "GetFileInformationByHandleEx",
        4,
        get_file_information_by_handle_ex,
    ),
    k32(
        "SetFileInformationByHandle",
        4,
        set_file_information_by_handle,
    ),
    k32("GetFinalPathNameByHandleA", 4, |t, a| {
        get_final_path_name(t, a, false)
    }),
    k32("GetFinalPathNameByHandleW", 4, |t, a| {
        get_final_path_name(t, a, true)
    }),
    k32("LockFile", 5, |_, _| Ok(1)),
    k32("LockFileEx", 6, |_, _| Ok(1)),
    k32("UnlockFile", 5, |_, _| Ok(1)),
    k32("UnlockFileEx", 5, |_, _| Ok(1)),
    k32("DeleteFileA", 1, |t, a| delete_file(t, a, false)),
    k32("DeleteFileW", 1, |t, a| delete_file(t, a, true)),
    k32("CopyFileA", 3, |t, a| {
        copy_file(t, a[0], a[1], a[2] as u32 != 0, false)
    }),
    k32("CopyFileW", 3, |t, a| {
        copy_file(t, a[0], a[1], a[2] as u32 != 0, true)
    }),
    k32("CopyFileExA", 6, |t, a| {
        copy_file(t, a[0], a[1], a[5] & 1 != 0, false)
    }),
    k32("CopyFileExW", 6, |t, a| {
        copy_file(t, a[0], a[1], a[5] & 1 != 0, true)
    }),
    k32("MoveFileA", 2, |t, a| {
        move_file(t, a[0], a[1], MOVEFILE_COPY_ALLOWED, false)
    }),
    k32("MoveFileW", 2, |t, a| {
        move_file(t, a[0], a[1], MOVEFILE_COPY_ALLOWED, true)
    }),
    k32("MoveFileExA", 3, |t, a| {
        move_file(t, a[0], a[1], a[2], false)
    }),
    k32("MoveFileExW", 3, |t, a| {
        move_file(t, a[0], a[1], a[2], true)
    }),
    k32("MoveFileWithProgressA", 5, |t, a| {
        move_file(t, a[0], a[1], a[4], false)
    }),
    k32("MoveFileWithProgressW", 5, |t, a| {
        move_file(t, a[0], a[1], a[4], true)
    }),
    k32("ReplaceFileA", 6, |t, a| {
        move_file(t, a[1], a[0], MOVEFILE_REPLACE_EXISTING, false)
    }),
    k32("ReplaceFileW", 6, |t, a| {
        move_file(t, a[1], a[0], MOVEFILE_REPLACE_EXISTING, true)
    }),
    k32("CreateDirectoryA", 2, |t, a| {
        create_directory(t, a[0], false)
    }),
    k32("CreateDirectoryW", 2, |t, a| {
        create_directory(t, a[0], true)
    }),
    k32("CreateDirectoryExA", 3, |t, a| {
        create_directory(t, a[1], false)
    }),
    k32("CreateDirectoryExW", 3, |t, a| {
        create_directory(t, a[1], true)
    }),
    k32("RemoveDirectoryA", 1, |t, a| remove_directory(t, a, false)),
    k32("RemoveDirectoryW", 1, |t, a| remove_directory(t, a, true)),
    k32("GetCurrentDirectoryA", 2, |t, a| {
        get_current_directory(t, a, false)
    }),
    k32("GetCurrentDirectoryW", 2, |t, a| {
        get_current_directory(t, a, true)
    }),
    k32("SetCurrentDirectoryA", 1, |t, a| {
        set_current_directory(t, a, false)
    }),
    k32("SetCurrentDirectoryW", 1, |t, a| {
        set_current_directory(t, a, true)
    }),
    k32("FindFirstFileA", 2, |t, a| {
        find_first_file(t, a[0], a[1], false)
    }),
    k32("FindFirstFileW", 2, |t, a| {
        find_first_file(t, a[0], a[1], true)
    }),
    k32("FindFirstFileExA", 6, |t, a| {
        find_first_file(t, a[0], a[2], false)
    }),
    k32("FindFirstFileExW", 6, |t, a| {
        find_first_file(t, a[0], a[2], true)
    }),
    k32("FindNextFileA", 2, find_next_file),
    k32("FindNextFileW", 2, find_next_file),
    k32("FindClose", 1, find_close),
    k32("GetFileAttributesA", 1, |t, a| {
        get_file_attributes(t, a, false)
    }),
    k32("GetFileAttributesW", 1, |t, a| {
        get_file_attributes(t, a, true)
    }),
    k32("GetFileAttributesExA", 3, |t, a| {
        get_file_attributes_ex(t, a, false)
    }),
    k32("GetFileAttributesExW", 3, |t, a| {
        get_file_attributes_ex(t, a, true)
    }),
    k32("SetFileAttributesA", 2, |t, a| {
        set_file_attributes(t, a, false)
    }),
    k32("SetFileAttributesW", 2, |t, a| {
        set_file_attributes(t, a, true)
    }),
    k32("GetTempPathA", 2, |t, a| get_temp_path(t, a, false)),
    k32("GetTempPathW", 2, |t, a| get_temp_path(t, a, true)),
    k32("GetTempPath2A", 2, |t, a| get_temp_path(t, a, false)),
    k32("GetTempPath2W", 2, |t, a| get_temp_path(t, a, true)),
    k32("GetTempFileNameA", 4, |t, a| {
        get_temp_file_name(t, a, false)
    }),
    k32("GetTempFileNameW", 4, |t, a| get_temp_file_name(t, a, true)),
    k32("GetFullPathNameA", 4, |t, a| {
        get_full_path_name(t, a, false)
    }),
    k32("GetFullPathNameW", 4, |t, a| get_full_path_name(t, a, true)),
    k32("GetLongPathNameA", 3, |t, a| same_path_name(t, a, false)),
    k32("GetLongPathNameW", 3, |t, a| same_path_name(t, a, true)),
    k32("GetShortPathNameA", 3, |t, a| same_path_name(t, a, false)),
    k32("GetShortPathNameW", 3, |t, a| same_path_name(t, a, true)),
    k32("SearchPathA", 6, |t, a| search_path(t, a, false)),
    k32("SearchPathW", 6, |t, a| search_path(t, a, true)),
    k32("GetDriveTypeA", 1, |t, a| get_drive_type(t, a, false)),
    k32("GetDriveTypeW", 1, |t, a| get_drive_type(t, a, true)),
    k32("GetLogicalDrives", 0, |t, _| {
        Ok(drive_letters(t)
            .iter()
            .fold(0u64, |m, &d| m | 1 << (d as u8 - b'A')))
    }),
    k32("GetLogicalDriveStringsA", 2, |t, a| {
        get_logical_drive_strings(t, a, false)
    }),
    k32("GetLogicalDriveStringsW", 2, |t, a| {
        get_logical_drive_strings(t, a, true)
    }),
    k32("GetDiskFreeSpaceA", 5, |t, a| {
        get_disk_free_space(t, a, false)
    }),
    k32("GetDiskFreeSpaceW", 5, |t, a| {
        get_disk_free_space(t, a, true)
    }),
    k32("GetDiskFreeSpaceExA", 4, |t, a| {
        get_disk_free_space_ex(t, a, false)
    }),
    k32("GetDiskFreeSpaceExW", 4, |t, a| {
        get_disk_free_space_ex(t, a, true)
    }),
    k32("GetVolumeInformationA", 8, |t, a| {
        get_volume_information(t, a, false)
    }),
    k32("GetVolumeInformationW", 8, |t, a| {
        get_volume_information(t, a, true)
    }),
    k32("GetVolumePathNameW", 3, |t, a| {
        let d = drive_arg(t, a[0], true).unwrap_or('C');
        let (n, _) = copy_str_truncate(t, a[1], a[2], &format!(r"{d}:\"), true);
        Ok((n > 0) as u64)
    }),
    k32("AreFileApisANSI", 0, |_, _| Ok(1)),
    k32("SetFileApisToANSI", 0, |_, _| Ok(0)),
    k32("SetFileApisToOEM", 0, |_, _| Ok(0)),
    k32("Wow64DisableWow64FsRedirection", 1, |t, a| {
        Ok(store_ptr(t, a[0], 0) as u64)
    }),
    k32("Wow64RevertWow64FsRedirection", 1, |_, _| Ok(1)),
    k32("Wow64EnableWow64FsRedirection", 1, |_, _| Ok(1)),
    k32("CreateFileMappingA", 6, |t, a| {
        create_file_mapping(t, a, false)
    }),
    k32("CreateFileMappingW", 6, |t, a| {
        create_file_mapping(t, a, true)
    }),
    k32("OpenFileMappingA", 3, |t, a| open_file_mapping(t, a, false)),
    k32("OpenFileMappingW", 3, |t, a| open_file_mapping(t, a, true)),
    k32("MapViewOfFile", 5, |t, a| map_view(t, a, 0)),
    k32("MapViewOfFileEx", 6, |t, a| map_view(t, a, a[5])),
    k32("UnmapViewOfFile", 1, unmap_view),
    k32("UnmapViewOfFileEx", 2, unmap_view),
    k32("FlushViewOfFile", 2, flush_view_api),
    k32("CreatePipe", 4, create_pipe),
    k32("PeekNamedPipe", 6, peek_named_pipe),
    k32("CreateNamedPipeA", 8, |t, a| create_named_pipe(t, a, false)),
    k32("CreateNamedPipeW", 8, |t, a| create_named_pipe(t, a, true)),
    k32("ConnectNamedPipe", 2, |t, a| connect_named_pipe(t, a)),
    k32("DisconnectNamedPipe", 1, |t, a| disconnect_named_pipe(t, a)),
    k32("WaitNamedPipeA", 2, |t, a| wait_named_pipe(t, a, false)),
    k32("WaitNamedPipeW", 2, |t, a| wait_named_pipe(t, a, true)),
    k32("GetNamedPipeInfo", 5, get_named_pipe_info),
    k32("GetNamedPipeHandleStateA", 7, get_named_pipe_handle_state),
    k32("GetNamedPipeHandleStateW", 7, get_named_pipe_handle_state),
    k32("SetNamedPipeHandleState", 4, |_, _| Ok(1)),
    k32("TransactNamedPipe", 7, |_, _| Ok(0)),
    k32("CallNamedPipeA", 7, |_, _| Ok(0)),
    k32("CallNamedPipeW", 7, |_, _| Ok(0)),
    k32("CreateHardLinkA", 3, |t, a| create_hard_link(t, a, false)),
    k32("CreateHardLinkW", 3, |t, a| create_hard_link(t, a, true)),
    k32("CreateSymbolicLinkA", 3, |t, a| create_symbolic_link(t, a, false)),
    k32("CreateSymbolicLinkW", 3, |t, a| create_symbolic_link(t, a, true)),
    k32("FindFirstChangeNotificationA", 3, |t, _| Ok(invalid_handle(t))),
    k32("FindFirstChangeNotificationW", 3, |t, _| Ok(invalid_handle(t))),
    k32("FindNextChangeNotification", 1, |_, _| Ok(0)),
    k32("FindCloseChangeNotification", 1, |_, _| Ok(1)),
    k32("ReadDirectoryChangesW", 8, |_, _| Ok(0)),
    k32("GetPrivateProfileStringA", 6, |t, a| {
        get_private_profile_string(t, a, false, None)
    }),
    k32("GetPrivateProfileStringW", 6, |t, a| {
        get_private_profile_string(t, a, true, None)
    }),
    k32("GetProfileStringA", 5, |t, a| {
        get_private_profile_string(t, a, false, Some("win.ini"))
    }),
    k32("GetProfileStringW", 5, |t, a| {
        get_private_profile_string(t, a, true, Some("win.ini"))
    }),
    k32("GetPrivateProfileIntA", 4, |t, a| {
        get_private_profile_int(t, a, false, None)
    }),
    k32("GetPrivateProfileIntW", 4, |t, a| {
        get_private_profile_int(t, a, true, None)
    }),
    k32("GetProfileIntA", 3, |t, a| {
        get_private_profile_int(t, a, false, Some("win.ini"))
    }),
    k32("GetProfileIntW", 3, |t, a| {
        get_private_profile_int(t, a, true, Some("win.ini"))
    }),
    k32("WritePrivateProfileStringA", 4, |t, a| {
        write_private_profile_string(t, a, false, None)
    }),
    k32("WritePrivateProfileStringW", 4, |t, a| {
        write_private_profile_string(t, a, true, None)
    }),
    k32("WriteProfileStringA", 3, |t, a| {
        write_private_profile_string(t, a, false, Some("win.ini"))
    }),
    k32("WriteProfileStringW", 3, |t, a| {
        write_private_profile_string(t, a, true, Some("win.ini"))
    }),
    k32("GetPrivateProfileSectionA", 4, |t, a| {
        get_private_profile_section(t, a, false)
    }),
    k32("GetPrivateProfileSectionW", 4, |t, a| {
        get_private_profile_section(t, a, true)
    }),
    k32("GetPrivateProfileSectionNamesA", 3, |t, a| {
        get_private_profile_section_names(t, a, false)
    }),
    k32("GetPrivateProfileSectionNamesW", 3, |t, a| {
        get_private_profile_section_names(t, a, true)
    }),
    k32("WritePrivateProfileSectionA", 3, |t, a| {
        write_private_profile_section(t, a, false)
    }),
    k32("WritePrivateProfileSectionW", 3, |t, a| {
        write_private_profile_section(t, a, true)
    }),
];

#[cfg(test)]
mod tests {
    use super::wildcard_match;

    #[test]
    fn wildcards_match_like_windows() {
        assert!(wildcard_match("*", "anything.txt"));
        assert!(wildcard_match("*.*", "README"));
        assert!(wildcard_match("*.TXT", "notes.txt"));
        assert!(wildcard_match("data??.bin", "data01.bin"));
        assert!(!wildcard_match("data??.bin", "data1.bin"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(!wildcard_match("*.exe", "app.dll"));
    }
}
