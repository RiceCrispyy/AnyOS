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
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_STAT: u64 = 4;
const SYS_FSTAT: u64 = 5;
const SYS_LSTAT: u64 = 6;
const SYS_LSEEK: u64 = 8;
const SYS_MMAP: u64 = 9;
const SYS_MUNMAP: u64 = 11;
const SYS_BRK: u64 = 12;
const SYS_IOCTL: u64 = 16;
const SYS_ACCESS: u64 = 21;
const SYS_WRITEV: u64 = 20;
const SYS_FCNTL: u64 = 72;
const SYS_GETCWD: u64 = 79;
const SYS_READLINK: u64 = 89;
const SYS_GETUID: u64 = 102;
const SYS_GETGID: u64 = 104;
const SYS_GETEUID: u64 = 107;
const SYS_GETEGID: u64 = 108;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_GETDENTS64: u64 = 217;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_CLOCK_GETTIME: u64 = 228;
const SYS_EXIT: u64 = 60;
const SYS_EXIT_GROUP: u64 = 231;
const SYS_OPENAT: u64 = 257;
const SYS_NEWFSTATAT: u64 = 262;

// ── errno values ───────────────────────────────────────────────────────────
const EBADF: u64 = 9;
const EINVAL: u64 = 22;
const ENOMEM: u64 = 12;
const EFAULT: u64 = 14;
const ENOENT: u64 = 2;
#[allow(dead_code)]
const ENOTDIR: u64 = 20;
#[allow(dead_code)]
const ENOSYS: u64 = 38;

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
        SYS_OPEN => sys_open(args, proc),
        SYS_CLOSE => sys_close(args, proc),
        SYS_STAT => sys_stat(args, proc),
        SYS_FSTAT => sys_fstat(args, proc),
        SYS_LSTAT => sys_lstat(args, proc),
        SYS_LSEEK => sys_lseek(args, proc),
        SYS_MMAP => sys_mmap(args, proc),
        SYS_MUNMAP => sys_munmap(args, proc),
        SYS_BRK => sys_brk(args, proc),
        SYS_IOCTL => sys_ioctl(args, proc),
        SYS_ACCESS => sys_access(args, proc),
        SYS_WRITEV => sys_writev(args, proc),
        SYS_FCNTL => sys_fcntl(args, proc),
        SYS_GETCWD => sys_getcwd(args, proc),
        SYS_READLINK => sys_readlink(args, proc),
        SYS_GETUID | SYS_GETEUID => ok(1000),
        SYS_GETGID | SYS_GETEGID => ok(1000),
        SYS_ARCH_PRCTL => sys_arch_prctl(args, proc),
        SYS_GETDENTS64 => sys_getdents64(args, proc),
        SYS_SET_TID_ADDRESS => ok(1), // return fake tid
        SYS_CLOCK_GETTIME => sys_clock_gettime(args, proc),
        SYS_EXIT | SYS_EXIT_GROUP => SyscallResult::Exit(args.a1 as i32),
        SYS_OPENAT => sys_openat(args, proc),
        SYS_NEWFSTATAT => sys_newfstatat(args, proc),
        _ => SyscallResult::Unimplemented(args.nr),
    }
}

// ── implementations ────────────────────────────────────────────────────────

fn sys_write(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let buf_addr = args.a2;
    let count = args.a3 as usize;

    let mut buf = vec![0u8; count];
    if proc.memory.read(buf_addr, &mut buf).is_err() {
        return err(EFAULT);
    }

    match proc.fds.get_mut(fd as usize).and_then(|s| s.as_mut()) {
        Some(GuestFd::Host(host_fd)) => {
            let hfd = *host_fd;
            let written = unsafe {
                #[cfg(unix)]
                {
                    libc_write(hfd, buf.as_ptr(), count)
                }
                #[cfg(not(unix))]
                {
                    win_write(hfd, &buf)
                }
            };
            if written < 0 {
                err(EINVAL)
            } else {
                ok(written as u64)
            }
        }
        Some(GuestFd::File(f)) => {
            use std::io::Write;
            match f.write(&buf) {
                Ok(n) => ok(n as u64),
                Err(_) => err(EINVAL),
            }
        }
        None => err(EBADF),
    }
}

fn sys_read(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let buf_addr = args.a2;
    let count = args.a3 as usize;

    let mut buf = vec![0u8; count];
    let n: usize = match proc.fds.get_mut(fd as usize).and_then(|s| s.as_mut()) {
        Some(GuestFd::Host(host_fd)) => {
            let hfd = *host_fd;
            let n = unsafe {
                #[cfg(unix)]
                {
                    libc_read(hfd, buf.as_mut_ptr(), count)
                }
                #[cfg(not(unix))]
                {
                    win_read(hfd, &mut buf)
                }
            };
            if n < 0 {
                return err(EINVAL);
            }
            n as usize
        }
        Some(GuestFd::File(f)) => {
            use std::io::Read;
            match f.read(&mut buf) {
                Ok(n) => n,
                Err(_) => return err(EINVAL),
            }
        }
        None => return err(EBADF),
    };

    if proc.memory.write(buf_addr, &buf[..n]).is_err() {
        return err(EFAULT);
    }
    ok(n as u64)
}

fn sys_close(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    if fd < 3 {
        return ok(0); // don't close host stdin/stdout/stderr
    }
    let idx = fd as usize;
    if idx < proc.fds.len() && proc.fds[idx].is_some() {
        proc.fds[idx] = None; // drop closes GuestFd::File automatically
        ok(0)
    } else {
        err(EBADF)
    }
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

    // Validate fd exists before touching iov.
    if proc.fds.get(fd as usize).and_then(|s| s.as_ref()).is_none() {
        return err(EBADF);
    }

    // Gather all iov bytes into one buffer, then write once.
    let mut gathered: Vec<u8> = Vec::new();
    for i in 0..iovcnt {
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
        let start = gathered.len();
        gathered.resize(start + len, 0);
        if proc.memory.read(base, &mut gathered[start..]).is_err() {
            return err(EFAULT);
        }
    }

    let count = gathered.len();
    match proc.fds.get_mut(fd as usize).and_then(|s| s.as_mut()) {
        Some(GuestFd::Host(hfd)) => {
            let hfd = *hfd;
            let n = unsafe {
                #[cfg(unix)]
                {
                    libc_write(hfd, gathered.as_ptr(), count)
                }
                #[cfg(not(unix))]
                {
                    win_write(hfd, &gathered)
                }
            };
            if n < 0 {
                err(EINVAL)
            } else {
                ok(n as u64)
            }
        }
        Some(GuestFd::File(f)) => {
            use std::io::Write;
            match f.write_all(&gathered) {
                Ok(()) => ok(count as u64),
                Err(_) => err(EINVAL),
            }
        }
        None => err(EBADF),
    }
}

fn sys_open(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let path_addr = args.a1;
    let flags = args.a2 as i32;
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    open_path(&path, flags, proc)
}

fn sys_openat(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    const AT_FDCWD: i32 = -100;
    let dirfd = args.a1 as i32;
    let path_addr = args.a2;
    let flags = args.a3 as i32;
    // We only support AT_FDCWD (relative to cwd) for now.
    if dirfd != AT_FDCWD {
        return err(EBADF);
    }
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    open_path(&path, flags, proc)
}

fn open_path(path: &str, _flags: i32, proc: &mut LinuxProcess) -> SyscallResult {
    match std::fs::File::open(path) {
        Ok(f) => {
            let fd = alloc_fd(proc, GuestFd::File(f));
            ok(fd as u64)
        }
        Err(e) => err(io_err_to_errno(&e)),
    }
}

fn alloc_fd(proc: &mut LinuxProcess, gfd: GuestFd) -> usize {
    // Reuse a slot or append.
    for (i, slot) in proc.fds.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(gfd);
            return i;
        }
    }
    proc.fds.push(Some(gfd));
    proc.fds.len() - 1
}

fn sys_lseek(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    use std::io::{Seek, SeekFrom};
    let fd = args.a1 as i32;
    let offset = args.a2 as i64;
    let whence = args.a3 as i32;
    let seek = match whence {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        2 => SeekFrom::End(offset),
        _ => return err(EINVAL),
    };
    match proc.fds.get_mut(fd as usize).and_then(|s| s.as_mut()) {
        Some(GuestFd::File(f)) => match f.seek(seek) {
            Ok(pos) => ok(pos),
            Err(_) => err(EINVAL),
        },
        _ => err(EBADF),
    }
}

fn sys_stat(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let path_addr = args.a1;
    let stat_addr = args.a2;
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match std::fs::metadata(&path) {
        Ok(m) => write_stat(proc, stat_addr, &m),
        Err(e) => err(io_err_to_errno(&e)),
    }
}

fn sys_lstat(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let path_addr = args.a1;
    let stat_addr = args.a2;
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match std::fs::symlink_metadata(&path) {
        Ok(m) => write_stat(proc, stat_addr, &m),
        Err(e) => err(io_err_to_errno(&e)),
    }
}

fn sys_newfstatat(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    const AT_FDCWD: i32 = -100;
    let dirfd = args.a1 as i32;
    let path_addr = args.a2;
    let stat_addr = args.a3;
    let flags = args.a4 as i32;

    if dirfd != AT_FDCWD {
        return err(EBADF);
    }
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    const AT_SYMLINK_NOFOLLOW: i32 = 0x100;
    let meta = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        std::fs::symlink_metadata(&path)
    } else {
        std::fs::metadata(&path)
    };
    match meta {
        Ok(m) => write_stat(proc, stat_addr, &m),
        Err(e) => err(io_err_to_errno(&e)),
    }
}

fn write_stat(proc: &mut LinuxProcess, addr: u64, meta: &std::fs::Metadata) -> SyscallResult {
    // x86-64 Linux struct stat layout (144 bytes).
    let mut buf = [0u8; 144];
    // st_mode at offset 24
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = meta.mode();
        buf[24..28].copy_from_slice(&mode.to_le_bytes());
        let size = meta.size();
        buf[48..56].copy_from_slice(&size.to_le_bytes()); // st_size at 48
    }
    #[cfg(not(unix))]
    {
        // Synthesise a minimal mode: regular file or directory.
        let mode: u32 = if meta.is_dir() { 0o040755 } else { 0o100644 };
        buf[24..28].copy_from_slice(&mode.to_le_bytes());
        let size = meta.len();
        buf[48..56].copy_from_slice(&size.to_le_bytes());
    }
    if proc.memory.write(addr, &buf).is_err() {
        return err(EFAULT);
    }
    ok(0)
}

fn sys_ioctl(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let request = args.a2;
    let arg = args.a3;
    const TIOCGWINSZ: u64 = 0x5413;
    const ENOTTY: u64 = 25;
    match request {
        TIOCGWINSZ => {
            // struct winsize: ws_row, ws_col, ws_xpixel, ws_ypixel (4 × u16 = 8 bytes)
            let ws = [0u16, 0u16, 0u16, 0u16];
            let mut buf = [0u8; 8];
            for (i, &v) in ws.iter().enumerate() {
                buf[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
            if proc.memory.write(arg, &buf).is_err() {
                return err(EFAULT);
            }
            ok(0)
        }
        _ => {
            let _ = fd;
            err(ENOTTY)
        }
    }
}

fn sys_access(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let path_addr = args.a1;
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    match std::fs::metadata(&path) {
        Ok(_) => ok(0),
        Err(e) => err(io_err_to_errno(&e)),
    }
}

fn sys_fcntl(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let cmd = args.a2 as i32;
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    const O_RDONLY: u64 = 0;
    match cmd {
        F_GETFL => match proc.fds.get(fd as usize).and_then(|s| s.as_ref()) {
            Some(GuestFd::Host(_)) | Some(GuestFd::File(_)) => ok(O_RDONLY),
            None => err(EBADF),
        },
        F_SETFL => ok(0), // no-op
        _ => err(EINVAL),
    }
}

fn sys_getcwd(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let buf_addr = args.a1;
    let size = args.a2 as usize;
    match std::env::current_dir() {
        Ok(p) => {
            let s = p.to_string_lossy();
            let bytes = s.as_bytes();
            if bytes.len() + 1 > size {
                return err(ENOMEM);
            }
            let mut buf = bytes.to_vec();
            buf.push(0);
            if proc.memory.write(buf_addr, &buf).is_err() {
                return err(EFAULT);
            }
            ok(buf_addr)
        }
        Err(_) => err(ENOENT),
    }
}

fn sys_readlink(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let path_addr = args.a1;
    let buf_addr = args.a2;
    let bufsiz = args.a3 as usize;
    let path = match read_cstr(proc, path_addr) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Special-case /proc/self/exe — return the path as-is (best-effort).
    let target = if path == "/proc/self/exe" {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/proc/self/exe".to_string())
    } else {
        match std::fs::read_link(&path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return err(io_err_to_errno(&e)),
        }
    };
    let bytes = target.as_bytes();
    let n = bytes.len().min(bufsiz);
    if proc.memory.write(buf_addr, &bytes[..n]).is_err() {
        return err(EFAULT);
    }
    ok(n as u64)
}

fn sys_arch_prctl(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    match args.a1 {
        ARCH_SET_FS => {
            proc.fs_base = args.a2;
            #[cfg(target_os = "linux")]
            unsafe {
                // Apply to this thread so the guest code can use FS immediately.
                extern "C" {
                    fn syscall(nr: i64, ...) -> i64;
                }
                syscall(158, ARCH_SET_FS as i64, args.a2 as i64);
            }
            ok(0)
        }
        ARCH_GET_FS => {
            if proc
                .memory
                .write(args.a2, &proc.fs_base.to_le_bytes())
                .is_err()
            {
                return err(EFAULT);
            }
            ok(0)
        }
        _ => err(EINVAL),
    }
}

fn sys_getdents64(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let fd = args.a1 as i32;
    let buf_addr = args.a2;
    let count = args.a3 as usize;

    let path = match proc.fds.get(fd as usize).and_then(|s| s.as_ref()) {
        Some(GuestFd::File(_)) => {
            // We need the path to call read_dir. For simplicity, this only works
            // when the fd refers to a directory we can get from /proc/self/fd.
            #[cfg(target_os = "linux")]
            {
                match std::fs::read_link(format!("/proc/self/fd/{fd}")) {
                    Ok(p) => p.to_string_lossy().into_owned(),
                    Err(_) => return err(EBADF),
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                return err(ENOSYS);
            }
        }
        _ => return err(EBADF),
    };

    let entries: Vec<_> = match std::fs::read_dir(&path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => return err(io_err_to_errno(&e)),
    };

    let mut buf = vec![0u8; count];
    let mut pos = 0usize;
    for entry in &entries {
        let name = entry.file_name();
        let name_bytes = name.as_encoded_bytes();
        // linux_dirent64: d_ino(8) + d_off(8) + d_reclen(2) + d_type(1) + d_name(variable) + null
        let reclen = (8 + 8 + 2 + 1 + name_bytes.len() + 1 + 7) & !7;
        if pos + reclen > count {
            break;
        }
        let d_type: u8 = match entry.file_type().ok() {
            Some(t) if t.is_dir() => 4,
            Some(t) if t.is_symlink() => 10,
            _ => 8,
        };
        // d_ino
        buf[pos..pos + 8].copy_from_slice(&1u64.to_le_bytes());
        // d_off
        buf[pos + 8..pos + 16].copy_from_slice(&((pos + reclen) as u64).to_le_bytes());
        // d_reclen
        buf[pos + 16..pos + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
        // d_type
        buf[pos + 18] = d_type;
        // d_name
        buf[pos + 19..pos + 19 + name_bytes.len()].copy_from_slice(name_bytes);
        buf[pos + 19 + name_bytes.len()] = 0;
        pos += reclen;
    }

    if proc.memory.write(buf_addr, &buf[..pos]).is_err() {
        return err(EFAULT);
    }
    ok(pos as u64)
}

fn sys_clock_gettime(args: SyscallArgs, proc: &mut LinuxProcess) -> SyscallResult {
    let ts_addr = args.a2;
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let tv_sec = dur.as_secs();
    let tv_nsec = dur.subsec_nanos() as u64;
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&tv_sec.to_le_bytes());
    buf[8..16].copy_from_slice(&tv_nsec.to_le_bytes());
    if proc.memory.write(ts_addr, &buf).is_err() {
        return err(EFAULT);
    }
    ok(0)
}

// ── helpers ────────────────────────────────────────────────────────────────

fn read_cstr(proc: &LinuxProcess, addr: u64) -> Result<String, SyscallResult> {
    let mut bytes = Vec::new();
    let mut cur = addr;
    loop {
        let mut b = [0u8; 1];
        if proc.memory.read(cur, &mut b).is_err() {
            return Err(err(EFAULT));
        }
        if b[0] == 0 {
            break;
        }
        bytes.push(b[0]);
        cur += 1;
        if bytes.len() > 4096 {
            return Err(err(ENOMEM));
        }
    }
    String::from_utf8(bytes).map_err(|_| err(EINVAL))
}

fn io_err_to_errno(e: &std::io::Error) -> u64 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => ENOENT,
        PermissionDenied => 13, // EACCES
        _ => EINVAL,
    }
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
