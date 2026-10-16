//! x86 Structured Exception Handling (SEH) dispatch.
//!
//! When the guest calls `RaiseException` on a 32-bit thread, Windows walks the
//! per-thread exception registration chain rooted at `FS:[0]` (TEB+0x00) and
//! calls each registered filter function with an `EXCEPTION_RECORD`, the frame
//! pointer, a `CONTEXT`, and a dispatcher context.
//!
//! Delphi installers (including Inno Setup) register their own handler that
//! catches exception code `0x0EEDFADE`, modifies the `CONTEXT` to resume inside
//! the matching `except` block, and returns `ExceptionContinueExecution (0)`.
//!
//! This module emulates that walk so software exceptions can be caught by guest
//! code instead of killing the process.

use anyos_backend::x86_reg;

use crate::api::{read_u32, write_u32};
use crate::thread::{Halt, Thread};
use crate::RunError;

// ---------------------------------------------------------------------------
// EXCEPTION_DISPOSITION return values from a handler function.
// ---------------------------------------------------------------------------

/// The handler fixed the fault; resume execution from the (possibly modified)
/// `CONTEXT`.
pub(crate) const EXCEPTION_CONTINUE_EXECUTION: u64 = 0;
/// This handler is not interested; try the next one in the chain.
pub(crate) const _EXCEPTION_CONTINUE_SEARCH: u64 = 1;

/// `EXCEPTION_UNWINDING`: the handler is called as the exception leaves its
/// frame, so it should run the frame's `finally` code and keep searching.
const EXCEPTION_UNWINDING: u32 = 0x02;
/// `EXCEPTION_EXIT_UNWIND`: `RtlUnwind` was called with no target frame, so the
/// thread is leaving every frame.
const EXCEPTION_EXIT_UNWIND: u32 = 0x04;
/// The record code used when a caller passes no `EXCEPTION_RECORD` to
/// [`unwind_32`].
const STATUS_UNWIND: u32 = 0xC000_0027;

// ---------------------------------------------------------------------------
// EXCEPTION_RECORD layout (32-bit, flat offsets in bytes).
// ---------------------------------------------------------------------------

const EXCEPTION_MAXIMUM_PARAMETERS: usize = 15;
/// Size of a 32-bit `EXCEPTION_RECORD`.
const EXCEPTION_RECORD_32_SIZE: u64 = 0x50; // 80 bytes

// ExceptionCode    +0x00  DWORD
// ExceptionFlags   +0x04  DWORD
// ExceptionRecord  +0x08  DWORD (ptr to nested record, NULL here)
// ExceptionAddress +0x0C  DWORD
// NumberParameters +0x10  DWORD
// ExceptionInfo[]  +0x14  DWORD[15]

// ---------------------------------------------------------------------------
// CONTEXT layout (32-bit), key fields only.
// ---------------------------------------------------------------------------

mod ctx32 {
    /// `ContextFlags` field (we set CONTEXT_FULL = 0x1001F).
    pub(crate) const FLAGS: u64 = 0x00;
    // Debug registers (+0x04..+0x1B): left as zero.
    // FloatSave (+0x1C, 112 bytes): left as zero.
    // Segment registers:
    pub(crate) const SEG_GS: u64 = 0x8C;
    pub(crate) const SEG_FS: u64 = 0x90;
    pub(crate) const SEG_ES: u64 = 0x94;
    pub(crate) const SEG_DS: u64 = 0x98;
    // Integer registers:
    pub(crate) const EDI: u64 = 0x9C;
    pub(crate) const ESI: u64 = 0xA0;
    pub(crate) const EBX: u64 = 0xA4;
    pub(crate) const EDX: u64 = 0xA8;
    pub(crate) const ECX: u64 = 0xAC;
    pub(crate) const EAX: u64 = 0xB0;
    // Control registers:
    pub(crate) const EBP: u64 = 0xB4;
    pub(crate) const EIP: u64 = 0xB8;
    pub(crate) const SEG_CS: u64 = 0xBC;
    pub(crate) const EFLAGS: u64 = 0xC0;
    pub(crate) const ESP: u64 = 0xC4;
    pub(crate) const SEG_SS: u64 = 0xC8;
    // ExtendedRegisters (+0xCC, 512 bytes): left as zero.
    /// Total size of the structure (0xCC + 512 = 0x2CC).
    pub(crate) const SIZE: u64 = 0x2CC;
    pub(crate) const CONTEXT_FULL: u32 = 0x0001_001F;
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn write_exception_record_32(
    t: &mut Thread<'_, '_>,
    addr: u64,
    code: u32,
    flags: u32,
    exception_addr: u64,
    params: &[u64],
) -> bool {
    let n = params.len().min(EXCEPTION_MAXIMUM_PARAMETERS) as u32;
    write_u32(t, addr, code)
        && write_u32(t, addr + 0x04, flags)
        && write_u32(t, addr + 0x08, 0) // next record: NULL
        && write_u32(t, addr + 0x0C, exception_addr as u32)
        && write_u32(t, addr + 0x10, n)
        && (0..n as usize).all(|i| write_u32(t, addr + 0x14 + i as u64 * 4, params[i] as u32))
}

fn write_context_32(t: &mut Thread<'_, '_>, addr: u64) -> bool {
    let r = t.ctx.regs;
    write_u32(t, addr + ctx32::FLAGS, ctx32::CONTEXT_FULL)
        && write_u32(t, addr + ctx32::EDI, r[x86_reg::RDI] as u32)
        && write_u32(t, addr + ctx32::ESI, r[x86_reg::RSI] as u32)
        && write_u32(t, addr + ctx32::EBX, r[x86_reg::RBX] as u32)
        && write_u32(t, addr + ctx32::EDX, r[x86_reg::RDX] as u32)
        && write_u32(t, addr + ctx32::ECX, r[x86_reg::RCX] as u32)
        && write_u32(t, addr + ctx32::EAX, r[x86_reg::RAX] as u32)
        && write_u32(t, addr + ctx32::EBP, r[x86_reg::RBP] as u32)
        && write_u32(t, addr + ctx32::EIP, t.ctx.rip as u32)
        && write_u32(t, addr + ctx32::EFLAGS, t.ctx.rflags as u32)
        && write_u32(t, addr + ctx32::ESP, r[x86_reg::RSP] as u32)
        && write_u32(t, addr + ctx32::SEG_FS, (t.ctx.fs_base & 0xFFFF) as u32)
        && write_u32(t, addr + ctx32::SEG_CS, 0x0023)
        && write_u32(t, addr + ctx32::SEG_SS, 0x002B)
        && write_u32(t, addr + ctx32::SEG_DS, 0x002B)
        && write_u32(t, addr + ctx32::SEG_ES, 0x002B)
        && write_u32(t, addr + ctx32::SEG_GS, 0)
}

fn read_context_32(t: &mut Thread<'_, '_>, addr: u64) {
    macro_rules! r {
        ($off:expr) => {
            read_u32(t, $off).unwrap_or(0) as u64
        };
    }
    t.ctx.regs[x86_reg::RDI] = r!(addr + ctx32::EDI);
    t.ctx.regs[x86_reg::RSI] = r!(addr + ctx32::ESI);
    t.ctx.regs[x86_reg::RBX] = r!(addr + ctx32::EBX);
    t.ctx.regs[x86_reg::RDX] = r!(addr + ctx32::EDX);
    t.ctx.regs[x86_reg::RCX] = r!(addr + ctx32::ECX);
    t.ctx.regs[x86_reg::RAX] = r!(addr + ctx32::EAX);
    t.ctx.regs[x86_reg::RBP] = r!(addr + ctx32::EBP);
    t.ctx.rip = r!(addr + ctx32::EIP);
    t.ctx.rflags = r!(addr + ctx32::EFLAGS);
    t.ctx.regs[x86_reg::RSP] = r!(addr + ctx32::ESP);
}

// ---------------------------------------------------------------------------
// Main entry point.
// ---------------------------------------------------------------------------

/// Walk the x86 SEH chain (`FS:[0]`) for a software exception.
///
/// On `ExceptionContinueExecution` the CPU context is restored from the
/// handler-modified `CONTEXT` structure and the caller must resume execution
/// (returning `Err(Halt::DirectResume)` so `dispatch()` skips its normal
/// `ret()` path).
///
/// Returns `Err(Halt::Error(GuestException))` if no handler in the chain
/// handles the exception.
pub(crate) fn dispatch_exception_32(
    t: &mut Thread<'_, '_>,
    code: u32,
    flags: u32,
    exception_addr: u64,
    params: &[u64],
) -> Result<u64, Halt> {
    let sp = t.ctx.regs[x86_reg::RSP];

    // Carve temporary space below the stack pointer for the two structures.
    // We leave 0x1000 bytes as a safety margin so the handler's own stack
    // frame does not collide with our buffers.
    let ctx_addr = (sp.saturating_sub(0x1000 + ctx32::SIZE)) & !0xF;
    let exc_addr = (ctx_addr.saturating_sub(EXCEPTION_RECORD_32_SIZE + 0x10)) & !0xF;

    if !write_exception_record_32(t, exc_addr, code, flags, exception_addr, params) {
        return Err(RunError::GuestException {
            code,
            location: t.describe(exception_addr),
        }
        .into());
    }
    if !write_context_32(t, ctx_addr) {
        return Err(RunError::GuestException {
            code,
            location: t.describe(exception_addr),
        }
        .into());
    }

    // Walk the chain: TEB+0x00 (FS:[0]) holds the head pointer.
    let exception_list_ptr = t.teb; // teb_exception_list == 0x00 on x86
    let mut frame = read_u32(t, exception_list_ptr).unwrap_or(0xFFFF_FFFF) as u64;

    let mut iterations = 0usize;
    while frame != 0 && frame != 0xFFFF_FFFF {
        iterations += 1;
        if iterations > 256 {
            // Corrupt / infinite chain; give up.
            break;
        }

        // _EXCEPTION_REGISTRATION_RECORD: { next: DWORD, handler: DWORD }
        let next = read_u32(t, frame).unwrap_or(0xFFFF_FFFF) as u64;
        let handler = read_u32(t, frame + 4).unwrap_or(0) as u64;

        if handler == 0 {
            frame = next;
            continue;
        }

        // Call handler(ExceptionRecord*, EstablisherFrame*, ContextRecord*, DispatcherContext*)
        // On x86 this is stdcall (4 args).
        // t.call() saves and restores ctx around the nested call, so we must
        // read the (possibly modified) CONTEXT back AFTER t.call() returns.
        let result = match t.call(handler, &[exc_addr, frame, ctx_addr, 0]) {
            Ok(v) => v,
            Err(Halt::Exit(code)) => return Err(Halt::Exit(code)),
            Err(Halt::ExitThread(code)) => return Err(Halt::ExitThread(code)),
            Err(Halt::DirectResume) => return Err(Halt::DirectResume),
            Err(_) => {
                // Handler faulted or called an unimplemented API; skip it.
                frame = next;
                continue;
            }
        };

        match result & 0xFFFF_FFFF {
            EXCEPTION_CONTINUE_EXECUTION => {
                // Restore registers from the (possibly modified) CONTEXT.
                read_context_32(t, ctx_addr);
                // Signal the caller to skip the normal `ret()` path.
                return Err(Halt::DirectResume);
            }
            _ => {
                // ExceptionContinueSearch or anything else: try the next frame.
                frame = next;
            }
        }
    }

    // No handler consumed the exception.
    Err(RunError::GuestException {
        code,
        location: t.describe(exception_addr),
    }
    .into())
}

/// x86 `RtlUnwind(TargetFrame, TargetIp, ExceptionRecord, ReturnValue)`.
///
/// Windows walks the registration chain from its head, calls the handler of
/// every frame *inside* `target_frame` with `EXCEPTION_UNWINDING` set (so their
/// `finally` code runs) and pops those frames off the chain. The target frame's
/// own handler is *not* called, and the thread then carries on as if `RtlUnwind`
/// had returned, with `return_value` in `EAX` -- which is what Delphi's RTL
/// expects, since it passes the address just after its own `call RtlUnwind` as
/// `TargetIp`. `TargetIp` itself is unused here, the way Wine and ReactOS also
/// ignore it.
///
/// A handler that answers `ExceptionContinueExecution` has installed a new
/// `CONTEXT`; the thread resumes from it instead, which is how an exception
/// ends up inside the matching `except` block.
///
/// Where Windows would leave the thread raised, a `target_frame` that is null
/// or missing from the chain unwinds what it can and returns -- the same choice
/// Wine makes, and what a guest on its way to `ExitProcess` wants.
pub(crate) fn unwind_32(
    t: &mut Thread<'_, '_>,
    target_frame: u64,
    record: u64,
    return_value: u64,
) -> Result<u64, Halt> {
    let sp = t.ctx.regs[x86_reg::RSP];
    let ctx_addr = (sp.saturating_sub(0x1000 + ctx32::SIZE)) & !0xF;
    let exc_addr = (ctx_addr.saturating_sub(EXCEPTION_RECORD_32_SIZE + 0x10)) & !0xF;

    // The unwinding flags go on a copy; the caller's record is left alone.
    let (code, mut flags, exception_addr, params) = if record != 0 {
        let code = read_u32(t, record).unwrap_or(0);
        let flags = read_u32(t, record + 0x04).unwrap_or(0);
        let address = read_u32(t, record + 0x0C).unwrap_or(0) as u64;
        let n = read_u32(t, record + 0x10)
            .unwrap_or(0)
            .min(EXCEPTION_MAXIMUM_PARAMETERS as u32);
        let params: Vec<u64> = (0..n)
            .map(|i| read_u32(t, record + 0x14 + i as u64 * 4).unwrap_or(0) as u64)
            .collect();
        (code, flags, address, params)
    } else {
        (STATUS_UNWIND, 0, t.ctx.rip, Vec::new())
    };
    flags |= EXCEPTION_UNWINDING;
    if target_frame == 0 {
        flags |= EXCEPTION_EXIT_UNWIND;
    }
    // Windows gives the handlers a `CONTEXT` to continue with, already holding
    // the return value in `EAX`.
    t.ctx.regs[x86_reg::RAX] = return_value;
    if !write_exception_record_32(t, exc_addr, code, flags, exception_addr, &params)
        || !write_context_32(t, ctx_addr)
    {
        return Err(RunError::GuestException {
            code,
            location: t.describe(exception_addr),
        }
        .into());
    }

    let mut frame = read_u32(t, t.teb).unwrap_or(0xFFFF_FFFF) as u64;
    let mut iterations = 0usize;
    while frame != 0 && frame != 0xFFFF_FFFF && frame != target_frame {
        iterations += 1;
        if iterations > 256 {
            // Corrupt / infinite chain; unwound what was readable.
            break;
        }
        let next = read_u32(t, frame).unwrap_or(0xFFFF_FFFF) as u64;
        let handler = read_u32(t, frame + 4).unwrap_or(0) as u64;
        if handler != 0 {
            match t.call(handler, &[exc_addr, frame, ctx_addr, 0]) {
                Ok(v) if v & 0xFFFF_FFFF == EXCEPTION_CONTINUE_EXECUTION => {
                    // The handler set up the context to continue in.
                    read_context_32(t, ctx_addr);
                    return Err(Halt::DirectResume);
                }
                Ok(_) => {}
                Err(Halt::Exit(code)) => return Err(Halt::Exit(code)),
                Err(Halt::ExitThread(code)) => return Err(Halt::ExitThread(code)),
                Err(Halt::DirectResume) => return Err(Halt::DirectResume),
                // A handler that faulted or called an unimplemented API is left
                // alone, as in the search walk above.
                Err(_) => {}
            }
        }
        // The frame has been unwound, so it leaves the chain.
        write_u32(t, t.teb, next as u32);
        frame = next;
    }
    Ok(return_value)
}
