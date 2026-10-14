//! # anyos-interp
//!
//! A portable interpreter for 32-bit x86 and x86-64 user-mode code, the first
//! [`ExecutionBackend`]. It needs no JIT entitlement, no particular host CPU and
//! no host memory tricks, so it runs anywhere AnyOS builds, including Apple
//! Silicon Macs. It is the reference the faster backends (native execution,
//! translation) will be checked against.
//!
//! Coverage today: the general-purpose integer instruction set that compilers
//! emit for ordinary C code (moves, arithmetic, logic, shifts, multiply and
//! divide, bit operations, conditional moves and sets, string instructions with
//! `rep`), `fs`/`gs` segment bases for the TEB, SSE moves, and the SSE2
//! packed-integer operations that compilers auto-vectorise loops into. x87 and
//! SSE floating-point arithmetic are not implemented yet; reaching them stops
//! the guest with [`GuestFault::Unsupported`] naming the instruction.
//!
//! Every guest memory access goes through [`AddressSpace`]'s protection
//! checks, so a stray guest write faults instead of corrupting the runtime.

mod sse;
mod x86;

use anyos_backend::{ExecutionBackend, GuestArch, GuestContext, GuestFault, StopReason};
use anyos_memory::AddressSpace;

pub use anyos_backend;

/// The interpreter backend.
#[derive(Debug, Default)]
pub struct Interpreter {
    executed: u64,
}

impl Interpreter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ExecutionBackend for Interpreter {
    fn name(&self) -> &'static str {
        "interpreter"
    }

    fn supports(&self, arch: GuestArch) -> bool {
        matches!(arch, GuestArch::X86 | GuestArch::X86_64)
    }

    fn run(
        &mut self,
        ctx: &mut GuestContext,
        mem: &mut AddressSpace,
        limit: Option<u64>,
    ) -> StopReason {
        #[allow(unreachable_patterns)] // GuestContext is non_exhaustive.
        let c = match ctx {
            GuestContext::X86(c) => c,
            _ => {
                return StopReason::Fault(GuestFault::Unsupported {
                    pc: ctx.pc(),
                    what: "this guest architecture".into(),
                    bytes: Vec::new(),
                })
            }
        };
        let mut left = limit;
        loop {
            if let Some(n) = left.as_mut() {
                if *n == 0 {
                    return StopReason::InstructionLimit;
                }
                *n -= 1;
            }
            let result = x86::step(c, mem, self.executed);
            self.executed += 1;
            match result {
                Ok(None) => {}
                Ok(Some(stop)) => return stop,
                Err(fault) => {
                    self.executed -= 1;
                    return StopReason::Fault(fault);
                }
            }
        }
    }

    fn instructions_executed(&self) -> u64 {
        self.executed
    }
}
