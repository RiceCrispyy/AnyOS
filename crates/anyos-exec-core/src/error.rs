//! Execution errors and configuration types.

use anyos_memory::MemoryError;
use std::io;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    /// Enable debug output
    pub debug: bool,
    /// Maximum memory size in bytes
    pub max_memory: u64,
    /// Stack size in bytes
    pub stack_size: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            debug: false,
            max_memory: 128 * 1024 * 1024, // 128 MB
            stack_size: 1024 * 1024,       // 1 MB
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunResult {
    /// Exit code of the process
    pub exit_code: i32,
    /// Total number of system calls executed
    pub syscall_count: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("memory error: {0}")]
    Memory(#[from] MemoryError),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("thread creation error: {0}")]
    ThreadCreation(String),

    #[error("thread execution error: {0}")]
    ThreadExecution(String),

    #[error("invalid system call: {0}")]
    InvalidSyscall(u64),

    #[error("unsupported system call: {0}")]
    UnsupportedSyscall(u64),

    #[error("system call failed: {0}")]
    SyscallFailed(String),

    #[error("process exited with code {0}")]
    ProcessExit(i32),

    #[error("register access out of bounds: {0}")]
    RegisterAccess(String),

    #[error("stack underflow")]
    StackUnderflow,

    #[error("segmentation fault at 0x{0:x}")]
    SegmentationFault(u64),

    #[error("invalid instruction")]
    InvalidInstruction,

    #[error("general execution error: {0}")]
    Other(String),
}

pub type ExecResult<T> = Result<T, ExecError>;
