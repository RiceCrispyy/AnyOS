//! # anyos-linux
//!
//! Linux x86-64 OS personality for AnyOS.
//!
//! Provides:
//! * [`LinuxProcess`] — runtime state for one guest Linux program (file
//!   descriptors, heap break, loaded image).
//! * [`syscall::dispatch`] — translate a Linux syscall number + register
//!   arguments into an action on [`LinuxProcess`], returning the value that
//!   goes back into `rax`.
//! * [`stack::build`] — write the POSIX initial stack (argc/argv/envp/auxv)
//!   into guest memory so the C runtime startup code can find its arguments.

pub mod stack;
pub mod syscall;

use anyos_elf::LoadedElf;
use anyos_memory::AddressSpace;
use serde::Serialize;

/// Open guest file descriptor.
#[derive(Debug)]
pub enum GuestFd {
    /// Forwarded to a real host file descriptor (e.g., stdin/stdout/stderr).
    Host(i32),
    // Future: guest pipes, anonymous memory objects, etc.
}

/// Runtime state for one Linux guest process.
pub struct LinuxProcess {
    /// Guest address space (loaded segments + stack + heap).
    pub memory: AddressSpace,
    /// Layout of the loaded ELF image.
    pub image: LoadedElf,
    /// Current program break (top of heap). Starts just above the last loaded segment.
    pub brk: u64,
    /// Open file descriptors (index = guest fd number).
    pub fds: Vec<Option<GuestFd>>,
}

impl LinuxProcess {
    /// Create a new process around a loaded ELF.
    ///
    /// Inherits stdin (0), stdout (1), and stderr (2) from the host.
    pub fn new(memory: AddressSpace, image: LoadedElf, initial_brk: u64) -> Self {
        LinuxProcess {
            memory,
            image,
            brk: initial_brk,
            fds: vec![
                Some(GuestFd::Host(0)),
                Some(GuestFd::Host(1)),
                Some(GuestFd::Host(2)),
            ],
        }
    }
}

/// Summary of a process's execution after it exits.
#[derive(Debug, Clone, Serialize)]
pub struct ExitStatus {
    /// Exit code returned by the guest program.
    pub code: i32,
}
