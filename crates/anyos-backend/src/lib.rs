//! # anyos-backend
//!
//! The interface between an OS personality (Windows today) and whatever runs
//! guest instructions: a portable interpreter, direct execution on a matching
//! CPU, or a translator.
//!
//! A backend runs one guest thread until something needs the runtime's
//! attention and reports why it stopped ([`StopReason`]). The personality then
//! decides what that means. A [`StopReason::Breakpoint`] inside the loader's
//! call-gate region is a Windows API call; anywhere else it is a real guest
//! breakpoint. Backends know nothing about Windows, so the same call-gate
//! dispatch works for every backend.
//!
//! Guest state lives in a [`GuestContext`] owned by the caller, so the runtime
//! can read API arguments and write results between runs.

use anyos_memory::{Access, AddressSpace};

/// Instruction sets a backend may support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    /// 32-bit x86 (protected mode, flat segments).
    X86,
    /// x86-64 (long mode).
    X86_64,
    Arm64,
}

/// Indices into [`X86Context::regs`].
pub mod x86_reg {
    pub const RAX: usize = 0;
    pub const RCX: usize = 1;
    pub const RDX: usize = 2;
    pub const RBX: usize = 3;
    pub const RSP: usize = 4;
    pub const RBP: usize = 5;
    pub const RSI: usize = 6;
    pub const RDI: usize = 7;
    pub const R8: usize = 8;
    pub const R9: usize = 9;
}

/// Register state of one x86 or x86-64 thread.
///
/// In 32-bit mode only the low halves of `regs[0..8]` are meaningful; the
/// upper halves are kept at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X86Context {
    /// RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15 (hardware encoding order).
    pub regs: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    /// Base of the `fs` segment (the TEB on 32-bit Windows).
    pub fs_base: u64,
    /// Base of the `gs` segment (the TEB on 64-bit Windows).
    pub gs_base: u64,
    pub xmm: [u128; 16],
    /// x87 registers and the control/status words, plus MXCSR.
    pub fpu: X87State,
    /// `true` for x86-64, `false` for 32-bit x86.
    pub long_mode: bool,
}

/// An 80-bit x87 extended-precision value: a 64-bit significand with an
/// explicit integer bit, and the sign and 15-bit exponent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct F80 {
    pub mant: u64,
    pub sign_exp: u16,
}

/// Floating-point unit state of an x86 thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X87State {
    /// Physical registers R0..R7; `st(i)` is `regs[(top + i) % 8]`.
    pub regs: [F80; 8],
    pub top: u8,
    /// Bit `n` set: physical register `n` is empty.
    pub empty: u8,
    pub control: u16,
    /// Status word without the TOP field (bits 11-13).
    pub status: u16,
    pub mxcsr: u32,
}

impl X87State {
    /// The state Windows gives a new thread: all registers empty, 53-bit
    /// precision, every exception masked.
    pub fn new() -> Self {
        X87State {
            regs: [F80::default(); 8],
            top: 0,
            empty: 0xFF,
            control: 0x27F,
            status: 0,
            mxcsr: 0x1F80,
        }
    }

    /// The status word including TOP.
    pub fn status_word(&self) -> u16 {
        (self.status & !0x3800) | ((self.top as u16 & 7) << 11)
    }
}

impl Default for X87State {
    fn default() -> Self {
        Self::new()
    }
}

impl X86Context {
    /// Bit 1 of RFLAGS is always set; IF is set for user-mode threads.
    pub const INITIAL_FLAGS: u64 = 0x202;

    pub fn new(long_mode: bool) -> Self {
        X86Context {
            regs: [0; 16],
            rip: 0,
            rflags: Self::INITIAL_FLAGS,
            fs_base: 0,
            gs_base: 0,
            xmm: [0; 16],
            fpu: X87State::new(),
            long_mode,
        }
    }

    pub fn pointer_size(&self) -> usize {
        if self.long_mode {
            8
        } else {
            4
        }
    }
}

/// The register state of a guest thread, whatever its architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuestContext {
    X86(X86Context),
}

impl GuestContext {
    pub fn arch(&self) -> GuestArch {
        match self {
            GuestContext::X86(c) if c.long_mode => GuestArch::X86_64,
            GuestContext::X86(_) => GuestArch::X86,
        }
    }

    /// Program counter.
    pub fn pc(&self) -> u64 {
        match self {
            GuestContext::X86(c) => c.rip,
        }
    }

    pub fn set_pc(&mut self, pc: u64) {
        match self {
            GuestContext::X86(c) => c.rip = pc,
        }
    }

    /// Stack pointer.
    pub fn sp(&self) -> u64 {
        match self {
            GuestContext::X86(c) => c.regs[x86_reg::RSP],
        }
    }

    pub fn set_sp(&mut self, sp: u64) {
        match self {
            GuestContext::X86(c) => c.regs[x86_reg::RSP] = sp,
        }
    }
}

/// Why a backend returned control to the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// A trap instruction (`int3`, `brk`) ran at `address`. The program counter
    /// already points past it, as after a hardware breakpoint exception.
    /// Call-gate stubs are made of these.
    Breakpoint { address: u64 },
    /// The guest did something that would raise an exception on Windows. The
    /// program counter still points at the faulting instruction.
    Fault(GuestFault),
    /// The instruction budget passed to [`ExecutionBackend::run`] ran out.
    InstructionLimit,
}

/// A guest exception, with the address of the instruction that raised it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuestFault {
    #[error("{access:?} access violation at 0x{address:x} (instruction at 0x{pc:x})")]
    Memory {
        pc: u64,
        address: u64,
        access: Access,
    },
    #[error("invalid instruction at 0x{pc:x} (bytes {})", hex(bytes))]
    InvalidOpcode { pc: u64, bytes: Vec<u8> },
    #[error(
        "instruction at 0x{pc:x} is not supported yet: {what} (bytes {})",
        hex(bytes)
    )]
    Unsupported {
        pc: u64,
        what: String,
        bytes: Vec<u8>,
    },
    #[error("integer divide error at 0x{pc:x}")]
    DivideError { pc: u64 },
    #[error("software interrupt 0x{vector:x} at 0x{pc:x}")]
    Interrupt { pc: u64, vector: u8 },
    #[error("privileged instruction at 0x{pc:x}")]
    Privileged { pc: u64 },
}

impl GuestFault {
    /// Address of the instruction that raised the fault.
    pub fn pc(&self) -> u64 {
        match self {
            GuestFault::Memory { pc, .. }
            | GuestFault::InvalidOpcode { pc, .. }
            | GuestFault::Unsupported { pc, .. }
            | GuestFault::DivideError { pc }
            | GuestFault::Interrupt { pc, .. }
            | GuestFault::Privileged { pc } => *pc,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Something that runs guest instructions.
///
/// Memory is the emulated [`AddressSpace`] for now; a native backend will
/// need host-mapped guest memory behind the same operations (milestone 3,
/// step 2).
pub trait ExecutionBackend {
    /// Short name for diagnostics, e.g. `"interpreter"`.
    fn name(&self) -> &'static str;

    fn supports(&self, arch: GuestArch) -> bool;

    /// Run the thread in `ctx` until it stops. `limit` caps the number of
    /// instructions executed in this call.
    fn run(
        &mut self,
        ctx: &mut GuestContext,
        mem: &mut AddressSpace,
        limit: Option<u64>,
    ) -> StopReason;

    /// Instructions executed so far, across all calls (0 if not counted).
    fn instructions_executed(&self) -> u64 {
        0
    }
}
