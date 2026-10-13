//! # anyos-exec
//!
//! Runs a Linux x86-64 ELF program on the current host.
//!
//! ## On Windows x64 (`NativeCpu` mode)
//!
//! The guest CPU matches the host, so no instruction translation is needed.
//! Only the OS interface differs. The strategy:
//!
//! 1. **Load** the ELF segments into real host memory with `VirtualAlloc`.
//! 2. **Patch** every `SYSCALL` instruction (`0F 05`) in executable segments
//!    to `INT3 + NOP` (`CC 90`), recording the original `rip + 2` → syscall id
//!    mapping.
//! 3. **Install** a Vectored Exception Handler (VEH) that catches `EXCEPTION_BREAKPOINT`,
//!    checks whether the `rip - 1` is in our patched set, reads the syscall
//!    number and arguments from `CONTEXT`, dispatches to [`anyos_linux::syscall::dispatch`],
//!    writes the return value back into `rax`, and resumes.
//! 4. **Build** the initial stack (argc/argv/envp/auxv) in host memory.
//! 5. **Jump** to the guest entry point.
//!
//! ## On Linux (for testing / CI)
//!
//! On Linux the same-architecture ELF can be executed directly in a child
//! process (no AnyOS mediation needed to test the loading path). This mode
//! is used only by tests; it is not shipped in release builds.

mod native;

pub use native::{run, RunError, RunOptions};
