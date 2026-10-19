//! Processes and threads: thread creation and control, thread-local and
//! fiber-local storage, child processes and process information.
//!
//! Guest threads all run inside this process on the cooperative scheduler in
//! `thread.rs`. A child process is another AnyOS run on the host, started
//! through [`RunOptions::child_runner`](crate::RunOptions::child_runner).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::api::*;
use crate::objects::{classify, Object, Target};
use crate::thread::{Halt, Thread, PROCESS_ID};

const CREATE_SUSPENDED: u64 = 0x4;
const STACK_SIZE_PARAM_IS_A_RESERVATION: u64 = 0x1_0000;
const CREATE_UNICODE_ENVIRONMENT: u64 = 0x400;
const TLS_OUT_OF_INDEXES: u64 = 0xFFFF_FFFF;
/// 64 slots in the TEB plus 1024 in the expansion block.
const TLS_SLOTS: usize = 64 + 1024;
const FLS_SLOTS: usize = 128;
const THREAD_PRIORITY_ERROR_RETURN: u64 = 0x7FFF_FFFF;
const NORMAL_PRIORITY_CLASS: u32 = 0x20;
const ERROR_BAD_EXE_FORMAT: u32 = 193;

/// A Windows program AnyOS runs as a separate host process.
#[derive(Clone)]
pub(crate) struct ChildProcess(Rc<RefCell<ChildState>>);

struct ChildState {
    child: std::process::Child,
    exit: Option<u32>,
    tid: u32,
}

impl ChildProcess {
    /// The exit code, once the process has ended.
    pub fn poll_exit(&self) -> Option<u32> {
        let mut s = self.0.borrow_mut();
        if s.exit.is_none() {
            if let Ok(Some(status)) = s.child.try_wait() {
                s.exit = Some(status.code().map_or(1, |c| c as u32));
            }
        }
        s.exit
    }

    pub fn pid(&self) -> u32 {
        self.0.borrow().child.id()
    }

    fn tid(&self) -> u32 {
        self.0.borrow().tid
    }

    fn kill(&self, code: u32) {
        let mut s = self.0.borrow_mut();
        if s.exit.is_none() {
            let _ = s.child.kill();
            let _ = s.child.wait();
            s.exit = Some(code);
        }
    }
}

/// The guest thread a handle refers to, if it is one of ours.
fn thread_index(t: &mut Thread<'_, '_>, h: u64) -> Option<usize> {
    match classify(t, h) {
        Target::CurrentThread => Some(t.current),
        Target::CurrentProcess => None,
        Target::Object => match t.k.get(h)? {
            Object::Thread(i) => Some(*i),
            _ => None,
        },
    }
}

fn pseudo_thread(t: &Thread<'_, '_>) -> u64 {
    invalid_handle(t) - 1
}

// ---------------------------------------------------------------------------
// Threads

fn create_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (stack, start, param, flags, tid_ptr) = (a[1], a[2], a[3], a[4], a[5]);
    if start == 0 {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let _ = flags & STACK_SIZE_PARAM_IS_A_RESERVATION; // Either way, a size.
    let i = t.create_thread(start, param, stack, flags & CREATE_SUSPENDED != 0)?;
    let tid = t.threads[i].id;
    let h = t.k.insert(Object::Thread(i), None);
    store_u32(t, tid_ptr, tid);
    t.trace_note(&format!("thread {tid:#x} created at {}", t.describe(start)));
    Ok(h)
}

fn create_remote_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !is_self(t, a[0]) {
        return fail(t, ERROR_ACCESS_DENIED, 0);
    }
    create_thread(t, &[a[1], a[2], a[3], a[4], a[5], a[6]])
}

fn terminate_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(i) = thread_index(t, a[0]) else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    let code = a[1] as u32;
    if i == t.current {
        return Err(Halt::ExitThread(code));
    }
    if t.threads[i].alive() {
        t.threads[i].exit_code = Some(code);
        t.release_thread_memory(i);
        t.wake_all();
    }
    Ok(1)
}

fn get_exit_code_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let code = if let Some(i) = thread_index(t, a[0]) {
        t.threads[i].exit_code.unwrap_or(STILL_ACTIVE)
    } else if let Some(Object::ChildThread(c)) = t.k.get(a[0]) {
        c.poll_exit().unwrap_or(STILL_ACTIVE)
    } else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    if !write_u32(t, a[1], code) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn suspend_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(i) = thread_index(t, a[0]) else {
        if let Some(Object::ChildThread(_)) = t.k.get(a[0]) {
            return Ok(0);
        }
        return fail(t, ERROR_INVALID_HANDLE, MINUS_ONE);
    };
    let prev = t.threads[i].suspend;
    t.threads[i].suspend += 1;
    if i == t.current {
        t.yield_now = true;
    }
    Ok(prev as u64)
}

fn resume_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(i) = thread_index(t, a[0]) else {
        if let Some(Object::ChildThread(_)) = t.k.get(a[0]) {
            // Child processes are never really started suspended.
            return Ok(1);
        }
        return fail(t, ERROR_INVALID_HANDLE, MINUS_ONE);
    };
    let prev = t.threads[i].suspend;
    t.threads[i].suspend = prev.saturating_sub(1);
    Ok(prev as u64)
}

fn get_thread_priority(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match thread_index(t, a[0]) {
        Some(i) => Ok(t.threads[i].priority as u32 as u64),
        None => fail(t, ERROR_INVALID_HANDLE, THREAD_PRIORITY_ERROR_RETURN),
    }
}

fn set_thread_priority(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let p = a[1] as u32 as i32;
    if !matches!(p, -15 | -2..=2 | 15) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    match thread_index(t, a[0]) {
        Some(i) => {
            t.threads[i].priority = p;
            Ok(1)
        }
        None => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn get_thread_id(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if let Some(i) = thread_index(t, a[0]) {
        return Ok(t.threads[i].id as u64);
    }
    match t.k.get(a[0]) {
        Some(Object::ChildThread(c)) => Ok(c.tid() as u64),
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn open_thread(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let tid = a[2] as u32;
    match t.threads.iter().position(|th| th.id == tid && th.alive()) {
        Some(i) => Ok(t.k.insert(Object::Thread(i), None)),
        None => fail(t, ERROR_INVALID_PARAMETER, 0),
    }
}

/// FILETIME of when the process started.
fn start_filetime(t: &Thread<'_, '_>) -> u64 {
    let elapsed = t.started_at.elapsed().as_nanos() as u64 / 100;
    crate::time::filetime_now().saturating_sub(elapsed)
}

/// GetThreadTimes / GetProcessTimes: creation, exit, kernel and user time.
fn get_times(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let start = start_filetime(t);
    let user = t.started_at.elapsed().as_nanos() as u64 / 100;
    let ok = write_u64(t, a[1], start)
        && write_u64(t, a[2], 0)
        && write_u64(t, a[3], 0)
        && write_u64(t, a[4], user);
    if !ok {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn set_thread_description(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(i) = thread_index(t, a[0]) else {
        return Ok(0x8007_0006); // HRESULT_FROM_WIN32(ERROR_INVALID_HANDLE)
    };
    let d = read_wide(t, a[1]).unwrap_or_default();
    t.threads[i].description = d;
    Ok(0)
}

fn get_thread_description(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(i) = thread_index(t, a[0]) else {
        return Ok(0x8007_0006);
    };
    let d = encode(&t.threads[i].description.clone(), true);
    let heap = t.k.process_heap;
    let p = crate::memory::heap_alloc(t, heap, d.len() as u64, false);
    if p == 0 {
        return Ok(0x8007_000E); // E_OUTOFMEMORY
    }
    write_bytes(t, p, &d);
    write_ptr(t, a[1], p);
    Ok(0)
}

fn stack_limits(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let th = &t.threads[t.current];
    let (low, high) = (th.stack_base, t.stack_top + 0x100);
    write_ptr(t, a[0], low);
    write_ptr(t, a[1], high);
    Ok(0)
}

// ---------------------------------------------------------------------------
// Thread-local storage

/// Address of TLS slot `i` in the thread whose TEB is at `teb`. Slots past the
/// first 64 live in an expansion block allocated on first use.
fn tls_slot(t: &mut Thread<'_, '_>, teb: u64, i: usize, create: bool) -> Option<u64> {
    let p = t.ptr_size() as u64;
    if i < 64 {
        return Some(teb + t.layout.teb_tls_slots + i as u64 * p);
    }
    let at = teb + t.layout.teb_tls_expansion;
    let mut block = read_ptr(t, at)?;
    if block == 0 {
        if !create {
            return None;
        }
        let heap = t.k.process_heap;
        block = crate::memory::heap_alloc(t, heap, 1024 * p, true);
        if block == 0 || !write_ptr(t, at, block) {
            return None;
        }
    }
    Some(block + (i as u64 - 64) * p)
}

/// Set slot `i` to zero in every thread.
fn clear_tls_slot(t: &mut Thread<'_, '_>, i: usize) {
    let tebs: Vec<u64> = t
        .threads
        .iter()
        .filter(|th| th.alive())
        .map(|th| th.teb)
        .collect();
    for teb in tebs {
        if let Some(at) = tls_slot(t, teb, i, false) {
            write_ptr(t, at, 0);
        }
    }
}

fn tls_alloc(t: &mut Thread<'_, '_>, _: &[u64]) -> Result<u64, Halt> {
    if t.k.tls_used.len() < TLS_SLOTS {
        t.k.tls_used.resize(TLS_SLOTS, false);
    }
    // Static TLS (the image's TLS directory) uses ThreadLocalStoragePointer,
    // not these slots, so every slot is free to hand out.
    let Some(i) = t.k.tls_used.iter().position(|u| !u) else {
        return fail(t, ERROR_NO_MORE_ITEMS, TLS_OUT_OF_INDEXES);
    };
    t.k.tls_used[i] = true;
    clear_tls_slot(t, i);
    Ok(i as u64)
}

fn tls_in_use(t: &Thread<'_, '_>, i: u64) -> bool {
    t.k.tls_used.get(i as usize).copied().unwrap_or(false)
}

fn tls_get(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as u64;
    if i as usize >= TLS_SLOTS {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let teb = t.teb;
    let v = tls_slot(t, teb, i as usize, false)
        .and_then(|at| read_ptr(t, at))
        .unwrap_or(0);
    // TlsGetValue clears the last error so 0 can be told from a failure.
    t.set_last_error(ERROR_SUCCESS);
    Ok(v)
}

fn tls_set(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as u64;
    if i as usize >= TLS_SLOTS {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let teb = t.teb;
    match tls_slot(t, teb, i as usize, true) {
        Some(at) if write_ptr(t, at, a[1]) => Ok(1),
        _ => fail(t, ERROR_NOT_ENOUGH_MEMORY, 0),
    }
}

fn tls_free(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as u64;
    if !tls_in_use(t, i) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    t.k.tls_used[i as usize] = false;
    clear_tls_slot(t, i as usize);
    Ok(1)
}

fn fls_alloc(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = match t.k.fls.iter().position(Option::is_none) {
        Some(i) => i,
        None if t.k.fls.len() < FLS_SLOTS => {
            t.k.fls.push(None);
            t.k.fls.len() - 1
        }
        None => return fail(t, ERROR_NO_MORE_ITEMS, TLS_OUT_OF_INDEXES),
    };
    t.k.fls[i] = Some(a[0]);
    for th in &mut t.threads {
        if let Some(v) = th.fls.get_mut(i) {
            *v = 0;
        }
    }
    Ok(i as u64)
}

fn fls_in_use(t: &Thread<'_, '_>, i: u64) -> bool {
    t.k.fls.get(i as usize).is_some_and(Option::is_some)
}

fn fls_get(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as u64;
    if !fls_in_use(t, i) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    t.set_last_error(ERROR_SUCCESS);
    Ok(t.threads[t.current]
        .fls
        .get(i as usize)
        .copied()
        .unwrap_or(0))
}

fn fls_set(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as usize;
    if !fls_in_use(t, i as u64) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let fls = &mut t.threads[t.current].fls;
    if fls.len() <= i {
        fls.resize(i + 1, 0);
    }
    fls[i] = a[1];
    Ok(1)
}

fn fls_free(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let i = a[0] as u32 as usize;
    if !fls_in_use(t, i as u64) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let cb = t.k.fls[i].take().unwrap_or(0);
    // The callback runs for the calling thread's value; other threads' values
    // are dropped (their callbacks would need to run on those threads).
    let value = t.threads[t.current].fls.get(i).copied().unwrap_or(0);
    for th in &mut t.threads {
        if let Some(v) = th.fls.get_mut(i) {
            *v = 0;
        }
    }
    if cb != 0 && value != 0 {
        t.call(cb, &[value])?;
    }
    Ok(1)
}

/// Run the FLS callbacks for the ending thread's non-null values.
pub(crate) fn run_fls_callbacks(t: &mut Thread<'_, '_>) -> Result<(), Halt> {
    let cur = t.current;
    for i in 0..t.k.fls.len() {
        let Some(cb) = t.k.fls[i] else { continue };
        let value = t.threads[cur].fls.get(i).copied().unwrap_or(0);
        if cb != 0 && value != 0 {
            t.threads[cur].fls[i] = 0;
            t.call(cb, &[value])?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Processes

fn is_self(t: &mut Thread<'_, '_>, h: u64) -> bool {
    match classify(t, h) {
        Target::CurrentProcess => true,
        Target::CurrentThread => false,
        Target::Object => matches!(t.k.get(h), Some(Object::SelfProcess)),
    }
}

fn child_of(t: &mut Thread<'_, '_>, h: u64) -> Option<ChildProcess> {
    match t.k.get(h)? {
        Object::Child(c) => Some(c.clone()),
        _ => None,
    }
}

/// Split a command line the way `CommandLineToArgvW` and the C runtime do.
/// The first argument (the program) only honours quotes.
pub(crate) fn split_command_line(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    // Program name: quoted or up to whitespace, no escapes.
    if !chars.is_empty() {
        let mut prog = String::new();
        if chars[0] == '"' {
            i = 1;
            while i < chars.len() && chars[i] != '"' {
                prog.push(chars[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                prog.push(chars[i]);
                i += 1;
            }
        }
        args.push(prog);
    }
    loop {
        while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let start = i;
                while i < chars.len() && chars[i] == '\\' {
                    i += 1;
                }
                let n = i - start;
                if i < chars.len() && chars[i] == '"' {
                    arg.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        arg.push('"');
                        i += 1;
                    }
                } else {
                    arg.extend(std::iter::repeat_n('\\', n));
                }
                continue;
            }
            if c == '"' {
                if quoted && chars.get(i + 1) == Some(&'"') {
                    arg.push('"');
                    i += 2;
                    continue;
                }
                quoted = !quoted;
                i += 1;
                continue;
            }
            if !quoted && (c == ' ' || c == '\t') {
                break;
            }
            arg.push(c);
            i += 1;
        }
        args.push(arg);
    }
    args
}

/// The host path of the program `name` names, searched for the way
/// CreateProcess does when the name has no directory.
fn resolve_program(t: &Thread<'_, '_>, name: &str) -> Option<(String, PathBuf)> {
    let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let name = if file.contains('.') {
        name.to_string()
    } else {
        format!("{name}.exe")
    };
    let has_dir = name.contains(['\\', '/', ':']);
    let mut candidates = Vec::new();
    if has_dir {
        candidates.push(crate::file::full_path(t, &name));
    } else {
        let main = crate::module::main_module_path(t);
        if let Some((dir, _)) = main.rsplit_once('\\') {
            candidates.push(format!(r"{dir}\{name}"));
        }
        candidates.push(format!(r"{}\{name}", t.k.cwd.trim_end_matches('\\')));
        candidates.push(format!(r"C:\Windows\System32\{name}"));
        candidates.push(format!(r"C:\Windows\{name}"));
        if let Some(path) = crate::sysinfo::env_var(t, "PATH") {
            for d in path.split(';').filter(|d| !d.is_empty()) {
                candidates.push(format!(r"{}\{name}", d.trim_end_matches('\\')));
            }
        }
    }
    candidates.into_iter().find_map(|g| {
        let h = crate::file::host_path(t, &g)?;
        h.is_file().then(|| (crate::file::full_path(t, &g), h))
    })
}

/// The program and its arguments from CreateProcess's two strings.
fn program_and_args(
    t: &Thread<'_, '_>,
    app: Option<String>,
    cmdline: Option<String>,
) -> Option<((String, PathBuf), Vec<String>)> {
    let cmdline = cmdline.unwrap_or_default();
    let mut argv = split_command_line(&cmdline);
    if let Some(app) = app {
        let prog = resolve_program(t, &app)?;
        if !argv.is_empty() {
            argv.remove(0);
        }
        return Some((prog, argv));
    }
    let trimmed = cmdline.trim_start();
    if trimmed.starts_with('"') {
        let prog = resolve_program(t, argv.first()?)?;
        argv.remove(0);
        return Some((prog, argv));
    }
    // Unquoted: try `C:\Program`, then `C:\Program Files\App`, and so on.
    let words: Vec<&str> = trimmed.split(' ').collect();
    for n in 1..=words.len() {
        let candidate = words[..n].join(" ");
        if let Some(prog) = resolve_program(t, &candidate) {
            let rest = words[n..].join(" ");
            let mut args = split_command_line(&format!("x {rest}"));
            args.remove(0);
            return Some((prog, args));
        }
    }
    None
}

fn create_process(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (app, cmd, flags, env, cwd, info) = (a[0], a[1], a[5], a[6], a[7], a[9]);
    let Ok(app) = opt_str(t, app, wide) else {
        return Ok(0);
    };
    let Ok(cmd) = opt_str(t, cmd, wide) else {
        return Ok(0);
    };
    let Ok(cwd) = opt_str(t, cwd, wide) else {
        return Ok(0);
    };
    if app.is_none() && cmd.as_deref().is_none_or(|c| c.trim().is_empty()) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Some(((guest, host), args)) = program_and_args(t, app, cmd.clone()) else {
        t.trace_note(&format!("CreateProcess: program not found in {cmd:?}"));
        return fail(t, ERROR_FILE_NOT_FOUND, 0);
    };
    if t.host.child_runner.is_empty() {
        t.trace_note(&format!(
            "CreateProcess {guest}: child processes are not enabled"
        ));
        return fail(t, ERROR_NOT_SUPPORTED, 0);
    }
    let is_pe = std::fs::read(&host)
        .map(|b| b.starts_with(b"MZ"))
        .unwrap_or(false);
    if !is_pe {
        return fail(t, ERROR_BAD_EXE_FORMAT, 0);
    }
    let runner = t.host.child_runner.clone();
    let mut command = std::process::Command::new(&runner[0]);
    command.args(&runner[1..]).arg(&host).args(&args);
    let dir = cwd.unwrap_or_else(|| t.k.cwd.clone());
    if let Some(d) = crate::file::host_path(t, &dir).filter(|d| d.is_dir()) {
        command.current_dir(d);
    }
    if env != 0 {
        for (k, v) in read_env_block(t, env, flags & CREATE_UNICODE_ENVIRONMENT != 0) {
            command.env(format!("ANYOS_GUEST_ENV_{k}"), v);
        }
    }
    let child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let code = crate::file::io_error(&e, &host);
            return fail(t, code, 0);
        }
    };
    let tid = t.next_thread_id;
    t.next_thread_id += 4;
    let c = ChildProcess(Rc::new(RefCell::new(ChildState {
        child,
        exit: None,
        tid,
    })));
    let pid = c.pid();
    t.trace_note(&format!("CreateProcess: started {guest} as process {pid}"));
    let hp = t.k.insert(Object::Child(c.clone()), None);
    let ht = t.k.insert(Object::ChildThread(c), None);
    // PROCESS_INFORMATION: hProcess, hThread, dwProcessId, dwThreadId.
    let p = t.ptr_size() as u64;
    let ok = write_ptr(t, info, hp)
        && write_ptr(t, info + p, ht)
        && write_u32(t, info + 2 * p, pid)
        && write_u32(t, info + 2 * p + 4, tid);
    if !ok {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

/// A `KEY=value\0...\0\0` environment block.
fn read_env_block(t: &Thread<'_, '_>, mut p: u64, wide: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for _ in 0..4096 {
        let Some(s) = read_str(t, p, wide) else { break };
        if s.is_empty() {
            break;
        }
        p += (char_len(&s, wide) + 1) * if wide { 2 } else { 1 };
        // Entries like `=C:=C:\dir` start with `=`.
        if let Some((k, v)) = s[1..].split_once('=') {
            out.push((format!("{}{k}", &s[..1]), v.to_string()));
        }
    }
    out
}

fn open_process(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let pid = a[2] as u32;
    if pid as u64 == PROCESS_ID {
        return Ok(t.k.insert(Object::SelfProcess, None));
    }
    let found = t.k.objects_mut().find_map(|o| match o {
        Object::Child(c) if c.pid() == pid => Some(c.clone()),
        _ => None,
    });
    match found {
        Some(c) => Ok(t.k.insert(Object::Child(c), None)),
        None => fail(t, ERROR_INVALID_PARAMETER, 0),
    }
}

fn terminate_process(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let code = a[1] as u32;
    if is_self(t, a[0]) {
        return Err(Halt::Exit(code));
    }
    match child_of(t, a[0]) {
        Some(c) => {
            c.kill(code);
            t.wake_all();
            Ok(1)
        }
        None => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn get_exit_code_process(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let code = if is_self(t, a[0]) {
        STILL_ACTIVE
    } else if let Some(c) = child_of(t, a[0]) {
        c.poll_exit().unwrap_or(STILL_ACTIVE)
    } else {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    };
    if !write_u32(t, a[1], code) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn get_process_id(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if is_self(t, a[0]) {
        return Ok(PROCESS_ID);
    }
    match child_of(t, a[0]) {
        Some(c) => Ok(c.pid() as u64),
        None => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

/// STARTUPINFOA/W: no window settings and no redirected handles.
fn get_startup_info(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let size: u32 = if t.long_mode() { 104 } else { 68 };
    let mut info = vec![0u8; size as usize];
    info[..4].copy_from_slice(&size.to_le_bytes());
    write_bytes(t, a[0], &info);
    Ok(0)
}

fn query_full_process_image_name(
    t: &mut Thread<'_, '_>,
    a: &[u64],
    wide: bool,
) -> Result<u64, Halt> {
    if !is_self(t, a[0]) {
        return fail(t, ERROR_ACCESS_DENIED, 0);
    }
    let name = crate::module::main_module_path(t);
    copy_str_inout(t, a[2], a[3], &name, wide, false)
}

fn processor_count() -> u64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u64)
}

fn affinity_mask(t: &Thread<'_, '_>) -> u64 {
    let n = processor_count().min(if t.long_mode() { 64 } else { 32 });
    if n >= 64 {
        u64::MAX
    } else {
        (1u64 << n) - 1
    }
}

/// `CommandLineToArgvW`: one LocalAlloc block holding the pointer array and
/// the strings.
fn command_line_to_argv(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(line) = read_wide(t, a[0]) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let args = if line.is_empty() {
        vec![crate::module::main_module_path(t)]
    } else {
        split_command_line(&line)
    };
    let p = t.ptr_size() as u64;
    let table = (args.len() as u64 + 1) * p;
    let strings: Vec<Vec<u8>> = args.iter().map(|s| encode(s, true)).collect();
    let total = table + strings.iter().map(|s| s.len() as u64).sum::<u64>();
    let heap = t.k.process_heap;
    let block = crate::memory::heap_alloc(t, heap, total, true);
    if block == 0 {
        return fail(t, ERROR_NOT_ENOUGH_MEMORY, 0);
    }
    let mut at = block + table;
    for (i, s) in strings.iter().enumerate() {
        write_ptr(t, block + i as u64 * p, at);
        write_bytes(t, at, s);
        at += s.len() as u64;
    }
    store_u32(t, a[1], args.len() as u32);
    Ok(block)
}

pub(crate) static APIS: &[Api] = &[
    k32("GetCurrentProcess", 0, |t, _| Ok(invalid_handle(t))),
    k32("GetCurrentThread", 0, |t, _| Ok(pseudo_thread(t))),
    k32("CreateThread", 6, create_thread),
    k32("CreateRemoteThread", 7, create_remote_thread),
    k32("CreateRemoteThreadEx", 8, create_remote_thread),
    k32("ExitThread", 1, |_, a| Err(Halt::ExitThread(a[0] as u32))),
    k32("TerminateThread", 2, terminate_thread),
    k32("GetExitCodeThread", 2, get_exit_code_thread),
    k32("SuspendThread", 1, suspend_thread),
    k32("Wow64SuspendThread", 1, suspend_thread),
    k32("ResumeThread", 1, resume_thread),
    k32("GetThreadPriority", 1, get_thread_priority),
    k32("SetThreadPriority", 2, set_thread_priority),
    k32("GetThreadPriorityBoost", 2, |t, a| {
        Ok(u64::from(store_u32(t, a[1], 0)))
    }),
    k32("SetThreadPriorityBoost", 2, |_, _| Ok(1)),
    k32("GetThreadId", 1, get_thread_id),
    k32("GetProcessIdOfThread", 1, |t, a| {
        if thread_index(t, a[0]).is_some() {
            return Ok(PROCESS_ID);
        }
        match t.k.get(a[0]) {
            Some(Object::ChildThread(c)) => Ok(c.pid() as u64),
            _ => fail(t, ERROR_INVALID_HANDLE, 0),
        }
    }),
    k32("OpenThread", 3, open_thread),
    k32("GetThreadTimes", 5, get_times),
    k32("GetProcessTimes", 5, get_times),
    k32("SetThreadDescription", 2, set_thread_description),
    k32("GetThreadDescription", 2, get_thread_description),
    k32("SetThreadAffinityMask", 2, |t, _| Ok(affinity_mask(t))),
    k32("SetThreadIdealProcessor", 2, |_, _| Ok(0)),
    k32("SetThreadIdealProcessorEx", 3, |_, _| Ok(1)),
    k32("SetThreadGroupAffinity", 3, |_, _| Ok(1)),
    k32("GetCurrentProcessorNumber", 0, |_, _| Ok(0)),
    k32("GetCurrentProcessorNumberEx", 1, |t, a| {
        write_bytes(t, a[0], &[0; 4]);
        Ok(0)
    }),
    k32("GetCurrentThreadStackLimits", 2, stack_limits),
    k32("SetThreadStackGuarantee", 1, |_, _| Ok(1)),
    k32("QueryThreadCycleTime", 2, |t, a| {
        let n = t.backend.instructions_executed();
        Ok(write_u64(t, a[1], n) as u64)
    }),
    k32("QueryProcessCycleTime", 2, |t, a| {
        let n = t.backend.instructions_executed();
        Ok(write_u64(t, a[1], n) as u64)
    }),
    k32("TlsAlloc", 0, tls_alloc),
    k32("TlsGetValue", 1, tls_get),
    k32("TlsGetValue2", 1, tls_get),
    k32("TlsSetValue", 2, tls_set),
    k32("TlsFree", 1, tls_free),
    k32("FlsAlloc", 1, fls_alloc),
    k32("FlsGetValue", 1, fls_get),
    k32("FlsGetValue2", 1, fls_get),
    k32("FlsSetValue", 2, fls_set),
    k32("FlsFree", 1, fls_free),
    k32("IsThreadAFiber", 0, |_, _| Ok(0)),
    k32("CreateProcessA", 10, |t, a| create_process(t, a, false)),
    k32("CreateProcessW", 10, |t, a| create_process(t, a, true)),
    k32("OpenProcess", 3, open_process),
    k32("TerminateProcess", 2, terminate_process),
    k32("GetExitCodeProcess", 2, get_exit_code_process),
    k32("GetProcessId", 1, get_process_id),
    k32("GetStartupInfoA", 1, get_startup_info),
    k32("GetStartupInfoW", 1, get_startup_info),
    k32("IsWow64Process", 2, |t, a| {
        if !write_u32(t, a[1], 0) {
            return fail(t, ERROR_NOACCESS, 0);
        }
        Ok(1)
    }),
    k32("IsWow64Process2", 3, |t, a| {
        // IMAGE_FILE_MACHINE_UNKNOWN: not a WOW64 process; the host is x64.
        write_u16(t, a[1], 0);
        if a[2] != 0 {
            write_u16(t, a[2], 0x8664);
        }
        Ok(1)
    }),
    k32("GetProcessAffinityMask", 3, |t, a| {
        let m = affinity_mask(t);
        store_ptr(t, a[1], m);
        store_ptr(t, a[2], m);
        Ok(1)
    }),
    k32("SetProcessAffinityMask", 2, |_, _| Ok(1)),
    k32("GetPriorityClass", 1, |t, _| {
        Ok(match t.k.priority_class {
            0 => NORMAL_PRIORITY_CLASS,
            c => c,
        } as u64)
    }),
    k32("SetPriorityClass", 2, |t, a| {
        t.k.priority_class = a[1] as u32;
        Ok(1)
    }),
    k32("GetProcessVersion", 1, |_, _| Ok(0x000A_0000)),
    k32("GetProcessHandleCount", 2, |t, a| {
        let n = t.k.handle_count() as u32 + 3;
        Ok(u64::from(store_u32(t, a[1], n)))
    }),
    // Legacy 16-bit-era API: the handle count it set no longer exists, so the
    // real one just echoes the requested count back (the CRT calls it at start).
    k32("SetHandleCount", 1, |_, a| Ok(a[0])),
    k32("GetProcessWorkingSetSize", 3, |t, a| {
        store_ptr(t, a[1], 200 * 4096);
        store_ptr(t, a[2], 1380 * 4096);
        Ok(1)
    }),
    k32("GetProcessWorkingSetSizeEx", 4, |t, a| {
        store_ptr(t, a[1], 200 * 4096);
        store_ptr(t, a[2], 1380 * 4096);
        store_u32(t, a[3], 0);
        Ok(1)
    }),
    k32("SetProcessWorkingSetSize", 3, |_, _| Ok(1)),
    k32("SetProcessWorkingSetSizeEx", 4, |_, _| Ok(1)),
    k32("GetProcessPriorityBoost", 2, |t, a| {
        Ok(u64::from(store_u32(t, a[1], 0)))
    }),
    k32("SetProcessPriorityBoost", 2, |_, _| Ok(1)),
    k32("GetProcessIoCounters", 2, |t, a| {
        Ok(write_bytes(t, a[1], &[0; 48]) as u64)
    }),
    k32("QueryFullProcessImageNameA", 4, |t, a| {
        query_full_process_image_name(t, a, false)
    }),
    k32("QueryFullProcessImageNameW", 4, |t, a| {
        query_full_process_image_name(t, a, true)
    }),
    k32("GetProcessShutdownParameters", 2, |t, a| {
        store_u32(t, a[0], 0x280);
        store_u32(t, a[1], 0);
        Ok(1)
    }),
    k32("SetProcessShutdownParameters", 2, |_, _| Ok(1)),
    k32("ProcessIdToSessionId", 2, |t, a| {
        Ok(u64::from(store_u32(t, a[1], 1)))
    }),
    k32("WTSGetActiveConsoleSessionId", 0, |_, _| Ok(1)),
    k32("SetProcessDEPPolicy", 1, |_, _| Ok(1)),
    k32("GetProcessDEPPolicy", 3, |t, a| {
        store_u32(t, a[1], 1);
        store_u32(t, a[2], 1);
        Ok(1)
    }),
    k32("GetProcessMitigationPolicy", 4, |t, a| {
        let n = a[3];
        Ok(write_bytes(t, a[2], &vec![0; n.min(64) as usize]) as u64)
    }),
    k32("SetProcessMitigationPolicy", 3, |_, _| Ok(1)),
    k32("SetProcessPriorityClass", 2, |_, _| Ok(1)),
    k32("GetGuiResources", 2, |_, _| Ok(0)),
    api("user32", "WaitForInputIdle", 2, |_, _| Ok(0)),
    api("shell32", "CommandLineToArgvW", 2, command_line_to_argv),
];

#[cfg(test)]
mod tests {
    use super::split_command_line;

    #[test]
    fn command_lines_split_like_the_c_runtime() {
        assert_eq!(
            split_command_line(r#""C:\Program Files\a.exe" one "two three" four"#),
            [r"C:\Program Files\a.exe", "one", "two three", "four"]
        );
        assert_eq!(
            split_command_line(r#"a.exe a\\\"b c\\"d e" f\g"#),
            ["a.exe", r#"a\"b"#, r"c\d e", r"f\g"]
        );
        assert_eq!(
            split_command_line(r#"a.exe "say ""hi"""#),
            ["a.exe", r#"say "hi""#]
        );
        assert_eq!(split_command_line("a.exe   "), ["a.exe"]);
        assert_eq!(split_command_line(r#"a.exe """#), ["a.exe", ""]);
    }
}
