//! Synchronisation: waits, events, mutexes, semaphores, waitable timers,
//! critical sections, SRW locks, condition variables, one-time
//! initialisation, interlocked operations and singly linked lists.
//!
//! Waiting never blocks the host: a wait that cannot finish yet returns
//! [`Halt::Block`] and the scheduler runs other threads until something
//! changes (see `thread.rs`).

use std::time::{Duration, Instant};

use crate::api::*;
use crate::objects::{classify, Object, Target};
use crate::thread::{Halt, Thread};

pub(crate) const WAIT_OBJECT_0: u64 = 0;
const WAIT_ABANDONED_0: u64 = 0x80;
pub(crate) const WAIT_TIMEOUT: u64 = 0x102;
const WAIT_FAILED: u64 = 0xFFFF_FFFF;
const ERROR_TOO_MANY_POSTS: u32 = 298;
const ERROR_INVALID_HANDLE_STATE: u32 = 1609;

/// A thread sleeping on a condition variable.
#[derive(Debug, Clone)]
pub(crate) struct CvWait {
    cv: u64,
    woken: bool,
    /// Recursion count of the critical section it released.
    recursion: u32,
}

/// Whether the object behind `h` is signaled; `None` for a bad handle. The
/// second value asks for polling (the state can change outside the guest).
fn signaled(t: &mut Thread<'_, '_>, h: u64) -> Option<(bool, bool)> {
    match classify(t, h) {
        Target::CurrentThread | Target::CurrentProcess => return Some((false, false)),
        Target::Object => {}
    }
    let tid = t.thread_id();
    let now = Instant::now();
    let threads_done: Vec<bool> = t.threads.iter().map(|th| !th.alive()).collect();
    Some(match t.k.get(h)? {
        Object::Event { signaled, .. } => (*signaled, false),
        Object::Mutex { owner, .. } => (owner.is_none_or(|(o, _)| o == tid), false),
        Object::Semaphore { count, .. } => (*count > 0, false),
        Object::Timer { due, signaled, .. } => {
            if due.is_some_and(|d| d <= now) {
                *signaled = true;
            }
            (*signaled, due.is_some() && !*signaled)
        }
        Object::Thread(i) => (threads_done[*i], false),
        Object::Child(c) | Object::ChildThread(c) => (c.poll_exit().is_some(), true),
        Object::SelfProcess => (false, false),
        Object::File(_) | Object::Pipe(_) | Object::Null => (true, false),
        Object::Find(_) | Object::Mapping(_) => return None,
    })
}

/// Take what a satisfied wait takes. Returns whether a mutex was abandoned.
fn acquire(t: &mut Thread<'_, '_>, h: u64) -> bool {
    let tid = t.thread_id();
    match t.k.get(h) {
        Some(Object::Event {
            manual: false,
            signaled,
        }) => *signaled = false,
        Some(Object::Mutex { owner, abandoned }) => {
            let count = owner.map_or(0, |(_, c)| c);
            *owner = Some((tid, count + 1));
            return std::mem::take(abandoned);
        }
        Some(Object::Semaphore { count, .. }) => *count -= 1,
        Some(Object::Timer {
            manual,
            due,
            period_ms,
            signaled,
        }) => {
            if *period_ms > 0 {
                *due = Some(Instant::now() + Duration::from_millis(*period_ms as u64));
            } else if !*manual {
                *due = None;
            }
            if !*manual {
                *signaled = false;
            }
        }
        _ => {}
    }
    false
}

/// WaitForMultipleObjects and friends.
pub(crate) fn wait(
    t: &mut Thread<'_, '_>,
    handles: &[u64],
    all: bool,
    ms: u32,
) -> Result<u64, Halt> {
    if handles.is_empty() || handles.len() > 64 {
        return fail(t, ERROR_INVALID_PARAMETER, WAIT_FAILED);
    }
    let deadline = t.deadline(ms);
    loop {
        let mut states = Vec::with_capacity(handles.len());
        let mut poll = false;
        for &h in handles {
            match signaled(t, h) {
                Some((s, p)) => {
                    states.push(s);
                    poll |= p;
                }
                None => return fail(t, ERROR_INVALID_HANDLE, WAIT_FAILED),
            }
        }
        if all && states.iter().all(|&s| s) {
            let abandoned = handles.iter().fold(false, |ab, &h| acquire(t, h) | ab);
            return Ok(if abandoned {
                WAIT_ABANDONED_0
            } else {
                WAIT_OBJECT_0
            });
        }
        if !all {
            if let Some(i) = states.iter().position(|&s| s) {
                let abandoned = acquire(t, handles[i]);
                let base = if abandoned {
                    WAIT_ABANDONED_0
                } else {
                    WAIT_OBJECT_0
                };
                return Ok(base + i as u64);
            }
        }
        if t.wait_or_block(deadline, poll)? {
            return Ok(WAIT_TIMEOUT);
        }
    }
}

fn wait_single(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    wait(t, &[a[0]], false, a[1] as u32)
}

fn wait_multiple(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (n, arr, all, ms) = (a[0] as u32, a[1], a[2] as u32 != 0, a[3] as u32);
    let p = t.ptr_size() as u64;
    let mut handles = Vec::new();
    for i in 0..n.min(65) as u64 {
        match read_ptr(t, arr + i * p) {
            Some(h) => handles.push(h),
            None => return fail(t, ERROR_NOACCESS, WAIT_FAILED),
        }
    }
    wait(t, &handles, all, ms)
}

fn signal_object_and_wait(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // Signal only on the first attempt, not on every retry of the wait.
    if t.threads[t.current].wait_started.is_none() {
        t.wait_start();
        if !signal(t, a[0]) {
            t.threads[t.current].wait_started = None;
            return fail(t, ERROR_INVALID_HANDLE, WAIT_FAILED);
        }
    }
    wait(t, &[a[1]], false, a[2] as u32)
}

/// Set an event, release a mutex or semaphore once.
fn signal(t: &mut Thread<'_, '_>, h: u64) -> bool {
    let tid = t.thread_id();
    let ok = match t.k.get(h) {
        Some(Object::Event { signaled, .. }) => {
            *signaled = true;
            true
        }
        Some(Object::Mutex { owner, .. }) => match *owner {
            Some((o, c)) if o == tid => {
                *owner = (c > 1).then_some((o, c - 1));
                true
            }
            _ => false,
        },
        Some(Object::Semaphore { count, max }) if *count < *max => {
            *count += 1;
            true
        }
        _ => false,
    };
    if ok {
        t.wake_all();
    }
    ok
}

/// `Global\` and `Local\` name the same namespace here.
fn object_name(t: &mut Thread<'_, '_>, ptr: u64, wide: bool) -> Result<Option<String>, ()> {
    let name = opt_str(t, ptr, wide)?;
    Ok(name.map(|n| {
        let n = n
            .strip_prefix("Global\\")
            .or_else(|| n.strip_prefix("Local\\"))
            .unwrap_or(&n);
        n.to_string()
    }))
}

/// Create (or open, when the name exists) a named or anonymous object.
fn create_named(
    t: &mut Thread<'_, '_>,
    name_ptr: u64,
    wide: bool,
    same_kind: fn(&Object) -> bool,
    make: impl FnOnce() -> Object,
) -> Result<u64, Halt> {
    let Ok(name) = object_name(t, name_ptr, wide) else {
        return Ok(0);
    };
    if let Some(n) = &name {
        if let Some(obj) = t.k.named(n) {
            if !same_kind(obj) {
                return fail(t, ERROR_INVALID_HANDLE, 0);
            }
            let h = t.k.open_named(n).unwrap_or(0);
            t.set_last_error(ERROR_ALREADY_EXISTS);
            return Ok(h);
        }
    }
    let h = t.k.insert(make(), name);
    t.set_last_error(ERROR_SUCCESS);
    Ok(h)
}

fn open_named(
    t: &mut Thread<'_, '_>,
    name_ptr: u64,
    wide: bool,
    same_kind: fn(&Object) -> bool,
) -> Result<u64, Halt> {
    let Ok(Some(name)) = object_name(t, name_ptr, wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    match t.k.named(&name) {
        Some(o) if same_kind(o) => Ok(t.k.open_named(&name).unwrap_or(0)),
        Some(_) => fail(t, ERROR_INVALID_HANDLE, 0),
        None => fail(t, ERROR_FILE_NOT_FOUND, 0),
    }
}

fn is_event(o: &Object) -> bool {
    matches!(o, Object::Event { .. })
}
fn is_mutex(o: &Object) -> bool {
    matches!(o, Object::Mutex { .. })
}
fn is_semaphore(o: &Object) -> bool {
    matches!(o, Object::Semaphore { .. })
}
fn is_timer(o: &Object) -> bool {
    matches!(o, Object::Timer { .. })
}

fn create_event(
    t: &mut Thread<'_, '_>,
    name: u64,
    manual: bool,
    initial: bool,
    wide: bool,
) -> Result<u64, Halt> {
    create_named(t, name, wide, is_event, || Object::Event {
        manual,
        signaled: initial,
    })
}

fn create_mutex(t: &mut Thread<'_, '_>, name: u64, owned: bool, wide: bool) -> Result<u64, Halt> {
    let tid = t.thread_id();
    let h = create_named(t, name, wide, is_mutex, || Object::Mutex {
        owner: owned.then_some((tid, 1)),
        abandoned: false,
    })?;
    Ok(h)
}

fn create_semaphore(
    t: &mut Thread<'_, '_>,
    name: u64,
    initial: u64,
    max: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let (initial, max) = (initial as u32 as i32, max as u32 as i32);
    if max <= 0 || initial < 0 || initial > max {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    create_named(t, name, wide, is_semaphore, || Object::Semaphore {
        count: initial as u32,
        max: max as u32,
    })
}

fn create_timer(t: &mut Thread<'_, '_>, name: u64, manual: bool, wide: bool) -> Result<u64, Halt> {
    create_named(t, name, wide, is_timer, || Object::Timer {
        manual,
        due: None,
        period_ms: 0,
        signaled: false,
    })
}

fn set_event(t: &mut Thread<'_, '_>, h: u64, state: bool) -> Result<u64, Halt> {
    match t.k.get(h) {
        Some(Object::Event { signaled, .. }) => *signaled = state,
        _ => return fail(t, ERROR_INVALID_HANDLE, 0),
    }
    if state {
        t.wake_all();
    }
    Ok(1)
}

fn pulse_event(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    // A waiter picks the signal up when it next runs; with none, nothing stays set.
    let waiting = t
        .threads
        .iter()
        .enumerate()
        .any(|(i, th)| i != t.current && th.alive() && th.blocked);
    set_event(t, a[0], waiting)
}

fn release_mutex(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    if !matches!(t.k.get(a[0]), Some(Object::Mutex { .. })) {
        return fail(t, ERROR_INVALID_HANDLE, 0);
    }
    if signal(t, a[0]) {
        Ok(1)
    } else {
        fail(t, ERROR_NOT_OWNER, 0)
    }
}

fn release_semaphore(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, n, prev) = (a[0], a[1] as u32 as i32, a[2]);
    let r = match t.k.get(h) {
        Some(Object::Semaphore { count, max }) => {
            if n <= 0 || *count as u64 + n as u64 > *max as u64 {
                Err(ERROR_TOO_MANY_POSTS)
            } else {
                let old = *count;
                *count += n as u32;
                Ok(old)
            }
        }
        _ => Err(ERROR_INVALID_HANDLE),
    };
    match r {
        Ok(old) => {
            store_u32(t, prev, old);
            t.wake_all();
            Ok(1)
        }
        Err(e) => fail(t, if n <= 0 { ERROR_INVALID_PARAMETER } else { e }, 0),
    }
}

fn set_waitable_timer(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (h, due_ptr, period) = (a[0], a[1], a[2] as u32);
    let Some(due) = read_u64(t, due_ptr) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let due = due as i64;
    let delay = if due < 0 {
        Duration::from_nanos(due.unsigned_abs() * 100)
    } else {
        // Absolute FILETIME.
        let now = crate::time::filetime_now() as i64;
        Duration::from_nanos((due - now).max(0) as u64 * 100)
    };
    match t.k.get(h) {
        Some(Object::Timer {
            due,
            period_ms,
            signaled,
            ..
        }) => {
            *due = Some(Instant::now() + delay);
            *period_ms = period;
            *signaled = false;
            Ok(1)
        }
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

fn cancel_waitable_timer(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    match t.k.get(a[0]) {
        Some(Object::Timer { due, .. }) => {
            *due = None;
            Ok(1)
        }
        _ => fail(t, ERROR_INVALID_HANDLE, 0),
    }
}

/// Mutexes held by a thread that ends become abandoned.
pub(crate) fn thread_ended(t: &mut Thread<'_, '_>) {
    let tid = t.thread_id();
    for o in t.k.objects_mut() {
        if let Object::Mutex { owner, abandoned } = o {
            if owner.is_some_and(|(o, _)| o == tid) {
                *owner = None;
                *abandoned = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sleeping and yielding

fn sleep(t: &mut Thread<'_, '_>, ms: u32) -> Result<u64, Halt> {
    if ms == 0 {
        t.yield_now = true;
        return Ok(0);
    }
    let deadline = t.deadline(ms);
    if ms == u32::MAX
        && !t
            .threads
            .iter()
            .enumerate()
            .any(|(i, th)| i != t.current && th.alive())
    {
        // Sleeping forever with nothing else running: the process is done.
        return Err(crate::RunError::Deadlock {
            detail: "Sleep(INFINITE) with no other thread".into(),
        }
        .into());
    }
    loop {
        if t.wait_or_block(deadline, false)? {
            return Ok(0);
        }
    }
}

// ---------------------------------------------------------------------------
// Critical sections. The guest owns the CRITICAL_SECTION memory; AnyOS keeps
// the owner and recursion count in it the way Windows does.

struct CsLayout {
    lock_count: u64,
    recursion: u64,
    owner: u64,
    spin: u64,
    size: u64,
}

fn cs_layout(t: &Thread<'_, '_>) -> CsLayout {
    if t.long_mode() {
        CsLayout {
            lock_count: 8,
            recursion: 12,
            owner: 16,
            spin: 32,
            size: 40,
        }
    } else {
        CsLayout {
            lock_count: 4,
            recursion: 8,
            owner: 12,
            spin: 20,
            size: 24,
        }
    }
}

fn init_cs(t: &mut Thread<'_, '_>, cs: u64, spin: u64) -> Result<u64, Halt> {
    let l = cs_layout(t);
    let mut b = vec![0u8; l.size as usize];
    b[l.lock_count as usize..l.lock_count as usize + 4].copy_from_slice(&(-1i32).to_le_bytes());
    if !write_bytes(t, cs, &b) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    write_ptr(t, cs + l.spin, spin);
    Ok(1)
}

/// Try to take the critical section; `Some(true)` when taken.
fn try_enter_cs(t: &mut Thread<'_, '_>, cs: u64) -> Option<bool> {
    let l = cs_layout(t);
    let tid = t.thread_id() as u64;
    let owner = read_ptr(t, cs + l.owner)?;
    let rec = read_u32(t, cs + l.recursion)?;
    if owner == 0 || rec == 0 {
        write_ptr(t, cs + l.owner, tid);
        write_u32(t, cs + l.recursion, 1);
        write_u32(t, cs + l.lock_count, (-2i32) as u32);
        Some(true)
    } else if owner == tid {
        write_u32(t, cs + l.recursion, rec + 1);
        Some(true)
    } else {
        Some(false)
    }
}

fn enter_cs(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    loop {
        match try_enter_cs(t, a[0]) {
            None => return fail(t, ERROR_NOACCESS, 0),
            Some(true) => return Ok(0),
            Some(false) => {
                t.wait_or_block(None, false)?;
            }
        }
    }
}

/// Release the critical section once (or fully, returning the recursion count).
fn leave_cs_inner(t: &mut Thread<'_, '_>, cs: u64, fully: bool) -> u32 {
    let l = cs_layout(t);
    let tid = t.thread_id() as u64;
    if read_ptr(t, cs + l.owner) != Some(tid) {
        return 0;
    }
    let rec = read_u32(t, cs + l.recursion).unwrap_or(1);
    if rec > 1 && !fully {
        write_u32(t, cs + l.recursion, rec - 1);
        return rec;
    }
    write_ptr(t, cs + l.owner, 0);
    write_u32(t, cs + l.recursion, 0);
    write_u32(t, cs + l.lock_count, (-1i32) as u32);
    t.wake_all();
    rec
}

fn leave_cs(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    leave_cs_inner(t, a[0], false);
    Ok(0)
}

// ---------------------------------------------------------------------------
// SRW locks: 0 free, 1 held exclusively, 2n held shared by n threads.

fn srw_try(t: &mut Thread<'_, '_>, lock: u64, exclusive: bool) -> Option<bool> {
    let v = read_ptr(t, lock)?;
    let new = if exclusive {
        (v == 0).then_some(1)
    } else {
        (v & 1 == 0).then_some(v + 2)
    };
    match new {
        Some(n) => Some(write_ptr(t, lock, n)),
        None => Some(false),
    }
}

fn srw_acquire(t: &mut Thread<'_, '_>, lock: u64, exclusive: bool) -> Result<u64, Halt> {
    loop {
        match srw_try(t, lock, exclusive) {
            None => return fail(t, ERROR_NOACCESS, 0),
            Some(true) => return Ok(0),
            Some(false) => {
                t.wait_or_block(None, false)?;
            }
        }
    }
}

fn srw_release(t: &mut Thread<'_, '_>, lock: u64, exclusive: bool) -> Result<u64, Halt> {
    let v = read_ptr(t, lock).unwrap_or(0);
    let new = if exclusive || v < 2 { 0 } else { v - 2 };
    write_ptr(t, lock, new);
    t.wake_all();
    Ok(0)
}

// ---------------------------------------------------------------------------
// Condition variables

enum CvLock {
    Cs(u64),
    Srw { lock: u64, shared: bool },
}

fn sleep_cv(t: &mut Thread<'_, '_>, cv: u64, lock: CvLock, ms: u32) -> Result<u64, Halt> {
    let deadline = t.deadline(ms);
    let cur = t.current;
    if t.threads[cur].cv.is_none() {
        let recursion = match lock {
            CvLock::Cs(cs) => leave_cs_inner(t, cs, true),
            CvLock::Srw { lock, shared } => {
                srw_release(t, lock, !shared)?;
                0
            }
        };
        t.threads[cur].cv = Some(CvWait {
            cv,
            woken: false,
            recursion,
        });
        t.wake_all();
    }
    loop {
        let w = t.threads[cur].cv.clone().expect("set above");
        let timed_out = !w.woken && deadline.is_some_and(|d| d <= Instant::now());
        if w.woken || timed_out {
            let got = match lock {
                CvLock::Cs(cs) => try_enter_cs(t, cs).inspect(|&got| {
                    if got && w.recursion > 1 {
                        let l = cs_layout(t);
                        write_u32(t, cs + l.recursion, w.recursion);
                    }
                }),
                CvLock::Srw { lock, shared } => srw_try(t, lock, !shared),
            };
            match got {
                Some(true) => {
                    t.threads[cur].cv = None;
                    return if timed_out {
                        fail(t, ERROR_TIMEOUT, 0)
                    } else {
                        Ok(1)
                    };
                }
                None => {
                    t.threads[cur].cv = None;
                    return fail(t, ERROR_NOACCESS, 0);
                }
                Some(false) => {
                    // Woken (or timed out) but the lock is busy: wait for it.
                    if let Some(w) = t.threads[cur].cv.as_mut() {
                        w.woken = true;
                    }
                    t.wait_or_block(None, false)?;
                    continue;
                }
            }
        }
        t.wait_or_block(deadline, false)?;
    }
}

fn wake_cv(t: &mut Thread<'_, '_>, cv: u64, all: bool) -> Result<u64, Halt> {
    for th in &mut t.threads {
        if let Some(w) = th.cv.as_mut() {
            if w.cv == cv && !w.woken && th.exit_code.is_none() {
                w.woken = true;
                if !all {
                    break;
                }
            }
        }
    }
    t.wake_all();
    Ok(0)
}

// ---------------------------------------------------------------------------
// One-time initialisation: the INIT_ONCE pointer holds 0 (not started), 1 (in
// progress) or the context with bit 1 set (done).

const INIT_ONCE_CHECK_ONLY: u32 = 1;
const INIT_ONCE_INIT_FAILED: u32 = 4;

fn init_once_execute(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (once, func, param, ctx_out) = (a[0], a[1], a[2], a[3]);
    loop {
        let Some(v) = read_ptr(t, once) else {
            return fail(t, ERROR_NOACCESS, 0);
        };
        if v & 2 != 0 {
            store_ptr(t, ctx_out, v & !3);
            return Ok(1);
        }
        if v == 1 {
            t.wait_or_block(None, false)?;
            continue;
        }
        write_ptr(t, once, 1);
        // The callback writes its context through a pointer: give it a slot
        // on the guest's own stack area below the caller.
        let slot = t.ctx.regs[anyos_backend::x86_reg::RSP].saturating_sub(0x80) & !0xF;
        write_ptr(t, slot, 0);
        let ok = t.call(func, &[once, param, slot])? as u32 != 0;
        if ok {
            let ctx = read_ptr(t, slot).unwrap_or(0) & !3;
            write_ptr(t, once, ctx | 2);
            store_ptr(t, ctx_out, ctx);
        } else {
            write_ptr(t, once, 0);
        }
        t.wake_all();
        return Ok(ok as u64);
    }
}

fn init_once_begin(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (once, flags, pending, ctx_out) = (a[0], a[1] as u32, a[2], a[3]);
    loop {
        let Some(v) = read_ptr(t, once) else {
            return fail(t, ERROR_NOACCESS, 0);
        };
        if v & 2 != 0 {
            store_u32(t, pending, 0);
            store_ptr(t, ctx_out, v & !3);
            return Ok(1);
        }
        if flags & INIT_ONCE_CHECK_ONLY != 0 {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        }
        if v == 0 {
            write_ptr(t, once, 1);
            store_u32(t, pending, 1);
            return Ok(1);
        }
        t.wait_or_block(None, false)?;
    }
}

fn init_once_complete(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (once, flags, ctx) = (a[0], a[1] as u32, a[2]);
    if read_ptr(t, once) != Some(1) {
        return fail(t, ERROR_INVALID_HANDLE_STATE, 0);
    }
    let v = if flags & INIT_ONCE_INIT_FAILED != 0 {
        0
    } else {
        (ctx & !3) | 2
    };
    write_ptr(t, once, v);
    t.wake_all();
    Ok(1)
}

// ---------------------------------------------------------------------------
// Interlocked operations (exported by 32-bit kernel32) and SLists.

fn interlocked(t: &mut Thread<'_, '_>, p: u64, f: impl FnOnce(u32) -> u32) -> Option<u32> {
    let old = read_u32(t, p)?;
    write_u32(t, p, f(old)).then_some(old)
}

fn cmpxchg64(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (p, exch, cmp) = if t.long_mode() {
        (a[0], a[1], a[2])
    } else {
        (
            a[0],
            a[1] & 0xFFFF_FFFF | a[2] << 32,
            a[3] & 0xFFFF_FFFF | a[4] << 32,
        )
    };
    let old = read_u64(t, p).unwrap_or(0);
    if old == cmp {
        write_u64(t, p, exch);
    }
    Ok(old)
}

/// (offset of the first entry, offset and width of the depth) in an SLIST_HEADER.
fn slist_layout(t: &Thread<'_, '_>) -> (u64, u64) {
    if t.long_mode() {
        (8, 0)
    } else {
        (0, 4)
    }
}

fn slist_push(t: &mut Thread<'_, '_>, head: u64, entry: u64) -> Result<u64, Halt> {
    let (next_off, depth_off) = slist_layout(t);
    let first = read_ptr(t, head + next_off).unwrap_or(0);
    let depth = read_u16(t, head + depth_off).unwrap_or(0);
    write_ptr(t, entry, first);
    write_ptr(t, head + next_off, entry);
    write_u16(t, head + depth_off, depth.wrapping_add(1));
    Ok(first)
}

fn slist_pop(t: &mut Thread<'_, '_>, head: u64) -> Result<u64, Halt> {
    let (next_off, depth_off) = slist_layout(t);
    let first = read_ptr(t, head + next_off).unwrap_or(0);
    if first != 0 {
        let next = read_ptr(t, first).unwrap_or(0);
        let depth = read_u16(t, head + depth_off).unwrap_or(1);
        write_ptr(t, head + next_off, next);
        write_u16(t, head + depth_off, depth.saturating_sub(1));
    }
    Ok(first)
}

fn slist_flush(t: &mut Thread<'_, '_>, head: u64) -> Result<u64, Halt> {
    let (next_off, depth_off) = slist_layout(t);
    let first = read_ptr(t, head + next_off).unwrap_or(0);
    write_ptr(t, head + next_off, 0);
    write_u16(t, head + depth_off, 0);
    Ok(first)
}

const CREATE_EVENT_MANUAL_RESET: u64 = 1;
const CREATE_EVENT_INITIAL_SET: u64 = 2;
const CREATE_MUTEX_INITIAL_OWNER: u64 = 1;
const CREATE_WAITABLE_TIMER_MANUAL_RESET: u64 = 1;

pub(crate) static APIS: &[Api] = &[
    k32("WaitForSingleObject", 2, wait_single),
    k32("WaitForSingleObjectEx", 3, wait_single),
    k32("WaitForMultipleObjects", 4, wait_multiple),
    k32("WaitForMultipleObjectsEx", 5, wait_multiple),
    k32("SignalObjectAndWait", 4, signal_object_and_wait),
    api("user32", "MsgWaitForMultipleObjects", 5, wait_multiple),
    api("user32", "MsgWaitForMultipleObjectsEx", 5, |t, a| {
        let all = a[4] & 1 != 0; // MWMO_WAITALL
        wait_multiple(t, &[a[0], a[1], all as u64, a[2]])
    }),
    k32("CreateEventA", 4, |t, a| {
        create_event(t, a[3], a[1] as u32 != 0, a[2] as u32 != 0, false)
    }),
    k32("CreateEventW", 4, |t, a| {
        create_event(t, a[3], a[1] as u32 != 0, a[2] as u32 != 0, true)
    }),
    k32("CreateEventExA", 4, |t, a| {
        create_event(
            t,
            a[1],
            a[2] & CREATE_EVENT_MANUAL_RESET != 0,
            a[2] & CREATE_EVENT_INITIAL_SET != 0,
            false,
        )
    }),
    k32("CreateEventExW", 4, |t, a| {
        create_event(
            t,
            a[1],
            a[2] & CREATE_EVENT_MANUAL_RESET != 0,
            a[2] & CREATE_EVENT_INITIAL_SET != 0,
            true,
        )
    }),
    k32("OpenEventA", 3, |t, a| open_named(t, a[2], false, is_event)),
    k32("OpenEventW", 3, |t, a| open_named(t, a[2], true, is_event)),
    k32("SetEvent", 1, |t, a| set_event(t, a[0], true)),
    k32("ResetEvent", 1, |t, a| set_event(t, a[0], false)),
    k32("PulseEvent", 1, pulse_event),
    k32("CreateMutexA", 3, |t, a| {
        create_mutex(t, a[2], a[1] as u32 != 0, false)
    }),
    k32("CreateMutexW", 3, |t, a| {
        create_mutex(t, a[2], a[1] as u32 != 0, true)
    }),
    k32("CreateMutexExA", 4, |t, a| {
        create_mutex(t, a[1], a[2] & CREATE_MUTEX_INITIAL_OWNER != 0, false)
    }),
    k32("CreateMutexExW", 4, |t, a| {
        create_mutex(t, a[1], a[2] & CREATE_MUTEX_INITIAL_OWNER != 0, true)
    }),
    k32("OpenMutexA", 3, |t, a| open_named(t, a[2], false, is_mutex)),
    k32("OpenMutexW", 3, |t, a| open_named(t, a[2], true, is_mutex)),
    k32("ReleaseMutex", 1, release_mutex),
    k32("CreateSemaphoreA", 4, |t, a| {
        create_semaphore(t, a[3], a[1], a[2], false)
    }),
    k32("CreateSemaphoreW", 4, |t, a| {
        create_semaphore(t, a[3], a[1], a[2], true)
    }),
    k32("CreateSemaphoreExA", 6, |t, a| {
        create_semaphore(t, a[3], a[1], a[2], false)
    }),
    k32("CreateSemaphoreExW", 6, |t, a| {
        create_semaphore(t, a[3], a[1], a[2], true)
    }),
    k32("OpenSemaphoreA", 3, |t, a| {
        open_named(t, a[2], false, is_semaphore)
    }),
    k32("OpenSemaphoreW", 3, |t, a| {
        open_named(t, a[2], true, is_semaphore)
    }),
    k32("ReleaseSemaphore", 3, release_semaphore),
    k32("CreateWaitableTimerA", 3, |t, a| {
        create_timer(t, a[2], a[1] as u32 != 0, false)
    }),
    k32("CreateWaitableTimerW", 3, |t, a| {
        create_timer(t, a[2], a[1] as u32 != 0, true)
    }),
    k32("CreateWaitableTimerExA", 4, |t, a| {
        create_timer(
            t,
            a[1],
            a[2] & CREATE_WAITABLE_TIMER_MANUAL_RESET != 0,
            false,
        )
    }),
    k32("CreateWaitableTimerExW", 4, |t, a| {
        create_timer(
            t,
            a[1],
            a[2] & CREATE_WAITABLE_TIMER_MANUAL_RESET != 0,
            true,
        )
    }),
    k32("OpenWaitableTimerA", 3, |t, a| {
        open_named(t, a[2], false, is_timer)
    }),
    k32("OpenWaitableTimerW", 3, |t, a| {
        open_named(t, a[2], true, is_timer)
    }),
    k32("SetWaitableTimer", 6, set_waitable_timer),
    k32("SetWaitableTimerEx", 7, set_waitable_timer),
    k32("CancelWaitableTimer", 1, cancel_waitable_timer),
    k32("Sleep", 1, |t, a| sleep(t, a[0] as u32)),
    k32("SleepEx", 2, |t, a| sleep(t, a[0] as u32)),
    k32("SwitchToThread", 0, |t, _| {
        t.yield_now = true;
        Ok(1)
    }),
    k32("InitializeCriticalSection", 1, |t, a| {
        init_cs(t, a[0], 0).map(|_| 0)
    }),
    k32("InitializeCriticalSectionAndSpinCount", 2, |t, a| {
        init_cs(t, a[0], a[1])
    }),
    k32("InitializeCriticalSectionEx", 3, |t, a| {
        init_cs(t, a[0], a[1])
    }),
    k32("DeleteCriticalSection", 1, |_, _| Ok(0)),
    k32("EnterCriticalSection", 1, enter_cs),
    k32("LeaveCriticalSection", 1, leave_cs),
    k32("TryEnterCriticalSection", 1, |t, a| {
        Ok(try_enter_cs(t, a[0]).unwrap_or(false) as u64)
    }),
    k32("SetCriticalSectionSpinCount", 2, |t, a| {
        let at = a[0] + cs_layout(t).spin;
        let old = read_ptr(t, at).unwrap_or(0);
        write_ptr(t, at, a[1]);
        Ok(old)
    }),
    k32("InitializeSRWLock", 1, |t, a| {
        write_ptr(t, a[0], 0);
        Ok(0)
    }),
    k32("AcquireSRWLockExclusive", 1, |t, a| {
        srw_acquire(t, a[0], true)
    }),
    k32("AcquireSRWLockShared", 1, |t, a| {
        srw_acquire(t, a[0], false)
    }),
    k32("ReleaseSRWLockExclusive", 1, |t, a| {
        srw_release(t, a[0], true)
    }),
    k32("ReleaseSRWLockShared", 1, |t, a| {
        srw_release(t, a[0], false)
    }),
    k32("TryAcquireSRWLockExclusive", 1, |t, a| {
        Ok(srw_try(t, a[0], true).unwrap_or(false) as u64)
    }),
    k32("TryAcquireSRWLockShared", 1, |t, a| {
        Ok(srw_try(t, a[0], false).unwrap_or(false) as u64)
    }),
    k32("InitializeConditionVariable", 1, |t, a| {
        write_ptr(t, a[0], 0);
        Ok(0)
    }),
    k32("SleepConditionVariableCS", 3, |t, a| {
        sleep_cv(t, a[0], CvLock::Cs(a[1]), a[2] as u32)
    }),
    k32("SleepConditionVariableSRW", 4, |t, a| {
        let shared = a[3] & 1 != 0; // CONDITION_VARIABLE_LOCKMODE_SHARED
        sleep_cv(t, a[0], CvLock::Srw { lock: a[1], shared }, a[2] as u32)
    }),
    k32("WakeConditionVariable", 1, |t, a| wake_cv(t, a[0], false)),
    k32("WakeAllConditionVariable", 1, |t, a| wake_cv(t, a[0], true)),
    k32("InitOnceInitialize", 1, |t, a| {
        write_ptr(t, a[0], 0);
        Ok(0)
    }),
    k32("InitOnceExecuteOnce", 4, init_once_execute),
    k32("InitOnceBeginInitialize", 4, init_once_begin),
    k32("InitOnceComplete", 3, init_once_complete),
    k32("InterlockedIncrement", 1, |t, a| {
        Ok(interlocked(t, a[0], |v| v.wrapping_add(1)).map_or(0, |v| v.wrapping_add(1)) as u64)
    }),
    k32("InterlockedDecrement", 1, |t, a| {
        Ok(interlocked(t, a[0], |v| v.wrapping_sub(1)).map_or(0, |v| v.wrapping_sub(1)) as u64)
    }),
    k32("InterlockedExchange", 2, |t, a| {
        let v = a[1] as u32;
        Ok(interlocked(t, a[0], |_| v).unwrap_or(0) as u64)
    }),
    k32("InterlockedExchangeAdd", 2, |t, a| {
        let v = a[1] as u32;
        Ok(interlocked(t, a[0], |o| o.wrapping_add(v)).unwrap_or(0) as u64)
    }),
    k32("InterlockedCompareExchange", 3, |t, a| {
        let (exch, cmp) = (a[1] as u32, a[2] as u32);
        Ok(interlocked(t, a[0], |o| if o == cmp { exch } else { o }).unwrap_or(0) as u64)
    }),
    k32x("InterlockedCompareExchange64", 3, 5, cmpxchg64),
    k32("InitializeSListHead", 1, |t, a| {
        let n = if t.long_mode() { 16 } else { 8 };
        write_bytes(t, a[0], &vec![0; n]);
        Ok(0)
    }),
    k32("InterlockedPushEntrySList", 2, |t, a| {
        slist_push(t, a[0], a[1])
    }),
    k32("InterlockedPopEntrySList", 1, |t, a| slist_pop(t, a[0])),
    k32("InterlockedFlushSList", 1, |t, a| slist_flush(t, a[0])),
    k32("QueryDepthSList", 1, |t, a| {
        let (_, depth_off) = slist_layout(t);
        Ok(read_u16(t, a[0] + depth_off).unwrap_or(0) as u64)
    }),
    k32("RtlFirstEntrySList", 1, |t, a| {
        let (next_off, _) = slist_layout(t);
        Ok(read_ptr(t, a[0] + next_off).unwrap_or(0))
    }),
];
