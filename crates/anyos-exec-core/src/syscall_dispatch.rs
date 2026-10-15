//! System call dispatching and handling.

use crate::error::{ExecError, ExecResult};
use std::io;

/// System call dispatcher for guest code.
pub struct SyscallDispatcher {
    /// Whether to trace syscalls
    trace: bool,
}

impl SyscallDispatcher {
    /// Create a new syscall dispatcher.
    pub fn new(trace: bool) -> Self {
        SyscallDispatcher { trace }
    }

    /// Dispatch a system call.
    pub fn dispatch(&self, syscall_num: u64, args: &[u64; 6]) -> ExecResult<u64> {
        if self.trace {
            println!("syscall {}: {:?}", syscall_num, args);
        }

        match syscall_num {
            // read(fd, buf, count)
            0 => self.sys_read(args[0], args[1], args[2]),
            // write(fd, buf, count)
            1 => self.sys_write(args[0], args[1], args[2]),
            // open(filename, flags, mode)
            2 => self.sys_open(args[0], args[1], args[2]),
            // close(fd)
            3 => self.sys_close(args[0]),
            // exit(code)
            60 => Err(ExecError::ProcessExit(args[0] as i32)),
            // exit_group(code)
            231 => Err(ExecError::ProcessExit(args[0] as i32)),
            _ => Err(ExecError::UnsupportedSyscall(syscall_num)),
        }
    }

    /// sys_read(fd, buf, count) -> count_read
    fn sys_read(&self, _fd: u64, _buf: u64, _count: u64) -> ExecResult<u64> {
        // Read from stdin and copy to guest buffer
        // This is a stub that handles the pattern match properly
        match io::stdin().read(&mut [0u8; 1]) {
            Ok(_n) => Ok(0),
            Err(_e) => Ok(0),
        }
    }

    /// sys_write(fd, buf, count) -> count_written
    fn sys_write(&self, _fd: u64, _buf: u64, _count: u64) -> ExecResult<u64> {
        // Write to stdout from guest buffer
        Ok(0)
    }

    /// sys_open(filename, flags, mode) -> fd
    fn sys_open(&self, _filename: u64, _flags: u64, _mode: u64) -> ExecResult<u64> {
        // Open a file
        Ok(0)
    }

    /// sys_close(fd) -> result
    fn sys_close(&self, _fd: u64) -> ExecResult<u64> {
        // Close a file descriptor
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_creation() {
        let _disp = SyscallDispatcher::new(false);
    }

    #[test]
    fn exit_syscall() {
        let disp = SyscallDispatcher::new(false);
        let args = [42, 0, 0, 0, 0, 0];
        match disp.dispatch(60, &args) {
            Err(ExecError::ProcessExit(code)) => assert_eq!(code, 42),
            _ => panic!("expected ProcessExit"),
        }
    }

    #[test]
    fn unsupported_syscall() {
        let disp = SyscallDispatcher::new(false);
        let args = [0; 6];
        match disp.dispatch(9999, &args) {
            Err(ExecError::UnsupportedSyscall(num)) => assert_eq!(num, 9999),
            _ => panic!("expected UnsupportedSyscall"),
        }
    }
}
