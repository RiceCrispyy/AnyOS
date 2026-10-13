//! Linux x86-64 syscall dispatcher.
//!
//! The execution backend calls [`dispatch`] after capturing the guest's
//! register state at a `syscall` instruction. The return value is placed
//! back into `rax`. Errors are returned as negative errno values.

use anyos_memory::round_up;

use crate::{GuestFd, LinuxProcess};

/// Registers passed at a `syscall` instruction (Linux x86-64 ABI).
/// rax = syscall number; rdi/rsi/rdx/r10/r8/r9 = args 1-6.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyscallArgs {
    pub nr: u64,
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
}

/// Outcome of a syscall dispatch.
#[derive(Debug, Clone)]
pub enum SyscallResult {
    /// Normal return; the given value goes into guest `rax`.
    Value(u64),
    /// The guest called `exit` or `exit_group`; the process should stop.
    Exit(i32),
    /// Syscall number is unknown or not yet implemented.
    Unimplemented(u64),
}

// ── x86-64 Linux syscall numbers ──────────────────────────────────────────

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_CLOSE: u64 = 3;
const SYS_FSTAT: u64 = 5;
const SYS_MMAP: u64 = 9;
const SYS_MUNMAP: u64 = 11;
const SYS_BRK: u64 = 12;
const SYS_WRITEV: u64 = 20;
const SYS_EXIT: u64 = 60;
const SYS_EXIT_GROUP: u64 = 231;

// ── errno values ───────────────────────────────────────────────────────────
const EBADF: u64 = 9;
const EINVAL: u64 = 22;
#[allow(dead_code)]
const ENOMEM: u64 = 12;
const EFAULT: u64 = 14;

fn ok(v: u64) -> SyscallResult {
    SyscallResult::Value(v)
}
fn err(e: u64) -> SyscallResult {
    SyscallResult::Value((-1i64 as u64).wrapping_sub(e.wrapping_sub(1)))
}

/// Dispatch one Linux syscall.
pub fn dispatch(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    match args.nr {
        SYS_READ => sys_read(args, proc),
        SYS_WRITE => sys_write(args, proc),
        SYS_CLOSE => sys_close(args, proc),
        SYS_FSTAT => sys_fstat(args, proc),
        SYS_MMAP => sys_mmap(args, proc),
        SYS_MUNMAP => sys_munmap(args, proc),
        SYS_BRK => sys_brk(args, proc),
        SYS_WRITEV => sys_writev(args, proc),
        SYS_EXIT | SYS_EXIT_GROUP => SyscallResult::Exit(args.a1 as i32),
        _ => SyscallResult::Unimplemented(args.nr),
    }
}

// ── implementations ────────────────────────────────────────────────────────

fn sys_write(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let buf_addr = args.a2;
    let count = args.a3 as usize;

    let host_fd = match guest_fd(proc, fd) {
        Some(GuestFd::Host(h)) => *h,
        _ => return err(EBADF),
    };

    // Read guest bytes.
    let mut buf = vec![0u8; count];
    if proc.memory.read(buf_addr, &mut buf).is_err() {
        return err(EFAULT);
    }

    // Write to the host file descriptor.
    let written = unsafe {
        #[cfg(unix)]
        {
            libc_write(host_fd, buf.as_ptr(), count)
        }
        #[cfg(not(unix))]
        {
            win_write(host_fd, &buf)
        }
    };

    if written < 0 {
        err(EINVAL)
    } else {
        ok(written as u64)
    }
}

fn sys_read(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let buf_addr = args.a2;
    let count = args.a3 as usize;

    let host_fd = match guest_fd(proc, fd) {
        Some(GuestFd::Host(h)) => *h,
        _ => return err(EBADF),
    };

    let mut buf = vec![0u8; count];
    let n = unsafe {
        #[cfg(unix)]
        {
            libc_read(host_fd, buf.as_mut_ptr(), count)
        }
        #[cfg(not(unix))]
        {
            win_read(host_fd, &mut buf)
        }
    };

    if n < 0 {
        return err(EINVAL);
    }
    if proc.memory.write(buf_addr, &buf[..n as usize]).is_err() {
        return err(EFAULT);
    }
    ok(n as u64)
}

fn sys_close(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    // Don't close host stdin/stdout/stderr.
    if fd < 3 {
        return ok(0);
    }
    if (fd as usize) < proc.fds.len() {
        proc.fds[fd as usize] = None;
    }
    ok(0)
}

fn sys_fstat(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let stat_addr = args.a2;

    let _host_fd = match guest_fd(proc, fd) {
        Some(GuestFd::Host(h)) => *h,
        _ => return err(EBADF),
    };

    // Return a minimal stat with st_mode = character device (tty-like) for
    // fd 0/1/2, which satisfies musl's isatty() check in __stdout_write.
    // Layout: x86-64 Linux struct stat (144 bytes).
    let mut stat = [0u8; 144];
    if fd < 3 {
        // st_mode offset = 24, S_IFCHR = 0x2000
        let mode: u32 = 0x2000 | 0o666;
        stat[24..28].copy_from_slice(&mode.to_le_bytes());
    }
    if proc.memory.write(stat_addr, &stat).is_err() {
        return err(EFAULT);
    }
    ok(0)
}

fn sys_brk(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let requested = args.a1;
    if requested == 0 || requested < proc.brk {
        // Return current break.
        return ok(proc.brk);
    }
    let new_brk = round_up(requested, anyos_memory::PAGE_SIZE);
    let grow = new_brk - proc.brk;
    if proc
        .memory
        .map(proc.brk, grow, anyos_memory::Protection::RW, "heap")
        .is_err()
    {
        return ok(proc.brk); // Linux returns the old brk on failure.
    }
    proc.brk = new_brk;
    ok(proc.brk)
}

fn sys_mmap(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let addr = args.a1;
    let length = args.a2;
    let _prot = args.a3;
    let flags = args.a4;
    let fd = args.a5 as i32;

    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_FAILED: u64 = (-1i64) as u64;

    if flags & MAP_ANONYMOUS == 0 || fd != -1 {
        // We only support anonymous mappings for now.
        return ok(MAP_FAILED);
    }

    let hint = if addr == 0 { None } else { Some(addr) };
    let size = round_up(length, anyos_memory::PAGE_SIZE);
    match proc
        .memory
        .map_anywhere(size, anyos_memory::Protection::RW, "mmap anon", hint)
    {
        Ok(base) => ok(base),
        Err(_) => ok(MAP_FAILED),
    }
}

fn sys_munmap(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let addr = args.a1;
    match proc.memory.unmap(addr) {
        Ok(()) => ok(0),
        Err(_) => err(EINVAL),
    }
}

fn sys_writev(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let iov_addr = args.a2;
    let iovcnt = args.a3 as usize;

    let host_fd = match guest_fd(proc, fd) {
        Some(GuestFd::Host(h)) => *h,
        _ => return err(EBADF),
    };

    let mut total: u64 = 0;
    for i in 0..iovcnt {
        // struct iovec { void *iov_base; size_t iov_len; } — 16 bytes on x64
        let entry_addr = iov_addr + i as u64 * 16;
        let base = match proc.memory.read_u64_raw(entry_addr) {
            Ok(v) => v,
            Err(_) => return err(EFAULT),
        };
        let len = match proc.memory.read_u64_raw(entry_addr + 8) {
            Ok(v) => v as usize,
            Err(_) => return err(EFAULT),
        };
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u8; len];
        if proc.memory.read(base, &mut buf).is_err() {
            return err(EFAULT);
        }
        let n = unsafe {
            #[cfg(unix)]
            {
                libc_write(host_fd, buf.as_ptr(), len)
            }
            #[cfg(not(unix))]
            {
                win_write(host_fd, &buf)
            }
        };
        if n < 0 {
            return if total > 0 { ok(total) } else { err(EINVAL) };
        }
        total += n as u64;
    }
    ok(total)
}

// ── file descriptor helper ─────────────────────────────────────────────────

fn guest_fd(proc: &LinuxProcess, fd: i32) -> Option<&GuestFd> {
    if fd < 0 {
        return None;
    }
    proc.fds.get(fd as usize)?.as_ref()
}

// ── platform I/O shims ─────────────────────────────────────────────────────

#[cfg(unix)]
unsafe fn libc_write(fd: i32, buf: *const u8, len: usize) -> isize {
    extern "C" {
        fn write(fd: i32, buf: *const core::ffi::c_void, count: usize) -> isize;
    }
    write(fd, buf.cast(), len)
}

#[cfg(unix)]
unsafe fn libc_read(fd: i32, buf: *mut u8, len: usize) -> isize {
    extern "C" {
        fn read(fd: i32, buf: *mut core::ffi::c_void, count: usize) -> isize;
    }
    read(fd, buf.cast(), len)
}

#[cfg(not(unix))]
unsafe fn win_write(fd: i32, buf: &[u8]) -> isize {
    // Windows: fd 1 = stdout, fd 2 = stderr.
    use std::io::Write;
    use std::os::windows::io::FromRawHandle;
    let handle = winapi_std_handle(fd);
    if handle.is_null() {
        return -1;
    }
    let mut f = std::fs::File::from_raw_handle(handle as _);
    let r = f.write(buf).map(|n| n as isize).unwrap_or(-1);
    // Don't drop the underlying handle.
    std::mem::forget(f);
    r
}

#[cfg(not(unix))]
unsafe fn win_read(fd: i32, buf: &mut [u8]) -> isize {
    use std::io::Read;
    use std::os::windows::io::FromRawHandle;
    let handle = winapi_std_handle(fd);
    if handle.is_null() {
        return -1;
    }
    let mut f = std::fs::File::from_raw_handle(handle as _);
    let r = f.read(buf).map(|n| n as isize).unwrap_or(-1);
    std::mem::forget(f);
    r
}

#[cfg(not(unix))]
unsafe fn winapi_std_handle(fd: i32) -> *mut core::ffi::c_void {
    extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
    }
    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const STD_ERROR_HANDLE: u32 = (-12i32) as u32;
    match fd {
        0 => GetStdHandle(STD_INPUT_HANDLE),
        1 => GetStdHandle(STD_OUTPUT_HANDLE),
        2 => GetStdHandle(STD_ERROR_HANDLE),
        _ => std::ptr::null_mut(),
    }
}
