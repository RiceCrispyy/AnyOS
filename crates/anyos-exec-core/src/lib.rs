//! # anyos-exec-core
//!
//! Core execution engine for AnyOS. Handles:
//! - ELF binary loading and execution
//! - Guest process state and threading
//! - System call dispatching
//! - Memory management and page protections

pub mod error;
pub mod guest_memory;
pub mod guest_thread;
pub mod syscall_dispatch;
pub mod unix_process;

pub use error::{ExecError, RunOptions, RunResult};
pub use guest_memory::GuestRegion;
pub use guest_thread::GuestThread;
pub use syscall_dispatch::SyscallDispatcher;
pub use unix_process::UnixProcess;
