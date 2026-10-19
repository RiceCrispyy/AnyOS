//! x64 (AMD64) table-driven structured exception handling.
//!
//! A 64-bit Windows program has no chain of frames in `FS:[0]`; instead every
//! function with a stack frame has an entry in its image's function table
//! (`.pdata`), which points at unwind information describing the stack it
//! allocates, the registers it saves, and -- for a function with a `try` block
//! -- a language handler to run.
//!
//! That is what makes an x64 `try..except` work: raising walks the function
//! table up the stack calling each function's language handler, and the handler
//! that decides to take the exception calls `RtlUnwindEx` -- or the older
//! four-argument `RtlUnwind`, which is the same walk with the raise point's
//! context and no history -- which walks the same table again running the
//! `finally` blocks it passes on the way to the frame that handles it. The
//! result is a thread resumed at the handler's continuation address with the
//! target frame's stack, which is how control reaches the `except` block.
//!
//! The walk below follows the layout Microsoft documents for `UNWIND_INFO` and
//! the algorithm ReactOS implements in `sdk/lib/rtl/amd64/unwind.c`, because
//! that is the behaviour Windows itself has.

use anyos_backend::{x86_reg, X86Context};

use crate::api::{guest_bytes, read_u16, read_u32, read_u64, write_bytes, write_u32, write_u64};
use crate::thread::{Halt, Thread};
use crate::RunError;

// ---------------------------------------------------------------------------
// UNWIND_INFO flags and exception flags.
// ---------------------------------------------------------------------------

/// The unwind info names a language handler to run while searching.
pub(crate) const UNW_FLAG_EHANDLER: u8 = 0x1;
/// The unwind info names a language handler to run while unwinding.
pub(crate) const UNW_FLAG_UHANDLER: u8 = 0x2;
/// The unwind info ends with another function's entry and its own codes.
const UNW_FLAG_CHAININFO: u8 = 0x4;

/// The handler is called as the exception leaves its frame.
const EXCEPTION_UNWINDING: u32 = 0x02;
/// `RtlUnwind` was called with no target frame: the thread is leaving them all.
const EXCEPTION_EXIT_UNWIND: u32 = 0x04;
/// The handler is being called for the frame the unwind is heading to.
const EXCEPTION_TARGET_UNWIND: u32 = 0x20;
/// The record code used when a caller passes no `EXCEPTION_RECORD`.
const STATUS_UNWIND: u32 = 0xC000_0027;

/// `ExceptionContinueExecution` from a language handler: the context it
/// installed is where execution continues.
const EXCEPTION_CONTINUE_EXECUTION: u64 = 0;

// ---------------------------------------------------------------------------
// Unwind codes.
// ---------------------------------------------------------------------------

const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_EPILOG: u8 = 6;
const UWOP_SPARE_CODE: u8 = 7;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
const UWOP_PUSH_MACHFRAME: u8 = 10;

/// How many two-byte slots an unwind code occupies, including its own.
fn code_slots(op: u8, op_info: u8) -> u64 {
    match op {
        UWOP_ALLOC_LARGE => {
            if op_info != 0 {
                3
            } else {
                2
            }
        }
        UWOP_SAVE_NONVOL | UWOP_EPILOG | UWOP_SAVE_XMM128 => 2,
        UWOP_SAVE_NONVOL_FAR | UWOP_SPARE_CODE | UWOP_SAVE_XMM128_FAR => 3,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Structures in guest memory.
// ---------------------------------------------------------------------------

/// `RUNTIME_FUNCTION`: the range a function covers, and where its unwind
/// information is (`UNWIND_INFO`), both as RVAs.
const RUNTIME_FUNCTION_SIZE: u64 = 12;

/// `IMAGE_DIRECTORY_ENTRY_EXCEPTION`, the `.pdata` table.
const DIRECTORY_ENTRY_EXCEPTION: u64 = 3;

/// `EXCEPTION_RECORD` on x64: `ExceptionInformation` is pointer sized.
///
/// ```text
/// ExceptionCode     +0x00  DWORD
/// ExceptionFlags    +0x04  DWORD
/// ExceptionRecord   +0x08  pointer
/// ExceptionAddress  +0x10  pointer
/// NumberParameters  +0x18  DWORD
/// ExceptionInfo[]   +0x20  pointer[15]
/// ```
const EXCEPTION_RECORD_64_SIZE: u64 = 0x98;

/// `CONTEXT` on x64. The integer registers run in hardware encoding order from
/// `Rax`, so `[regs]` in this runtime indexes them directly.
mod ctx64 {
    pub(crate) const CONTEXT_FLAGS: u64 = 0x30;
    pub(crate) const MX_CSR: u64 = 0x34;
    pub(crate) const SEG_CS: u64 = 0x38;
    pub(crate) const SEG_DS: u64 = 0x3A;
    pub(crate) const SEG_ES: u64 = 0x3C;
    pub(crate) const SEG_FS: u64 = 0x3E;
    pub(crate) const SEG_GS: u64 = 0x40;
    pub(crate) const SEG_SS: u64 = 0x42;
    pub(crate) const EFLAGS: u64 = 0x44;
    pub(crate) const RAX: u64 = 0x78;
    pub(crate) const RIP: u64 = 0xF8;
    /// `VectorRegister[0]`, which is also `Xmm0`.
    pub(crate) const XMM0: u64 = 0x300;
    pub(crate) const SIZE: u64 = 0x4C0;

    pub(crate) const CONTEXT_AMD64: u32 = 0x0010_0000;
    pub(crate) const CONTEXT_CONTROL: u32 = CONTEXT_AMD64 | 0x1;
    pub(crate) const CONTEXT_INTEGER: u32 = CONTEXT_AMD64 | 0x2;
    pub(crate) const CONTEXT_FLOATING_POINT: u32 = CONTEXT_AMD64 | 0x8;
    pub(crate) const CONTEXT_FULL: u32 = CONTEXT_CONTROL | CONTEXT_INTEGER | CONTEXT_FLOATING_POINT;
}

/// `DISPATCHER_CONTEXT` on x64: what a language handler is told about the frame
/// it is being called for.
///
/// ```text
/// ControlPc        +0x00   ImageBase        +0x08
/// FunctionEntry    +0x10   EstablisherFrame +0x18
/// TargetIp         +0x20   ContextRecord    +0x28
/// LanguageHandler  +0x30   HandlerData      +0x38
/// HistoryTable     +0x40   ScopeIndex       +0x48
/// ```
const DISPATCHER_CONTEXT_64_SIZE: u64 = 0x50;

// ---------------------------------------------------------------------------
// Shared with the unwind machinery: the exception context of a dispatch.
// ---------------------------------------------------------------------------

/// Whether `handler_type` (an `UNW_FLAG_*`) wants the handler in these flags.
fn wants_handler(flags: u8, handler_type: u8) -> bool {
    flags & handler_type & (UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER) != 0
}

fn byte(t: &Thread<'_, '_>, addr: u64) -> Option<u8> {
    guest_bytes(t, addr, 1)?.first().copied()
}

fn write_u16(t: &mut Thread<'_, '_>, addr: u64, value: u16) -> bool {
    write_bytes(t, addr, &value.to_le_bytes())
}

/// The image base and `.pdata` table of the module `rip` belongs to.
fn function_table(t: &Thread<'_, '_>, rip: u64) -> Option<(u64, u64, u32)> {
    let m = t
        .process
        .modules
        .iter()
        .find(|m| rip >= m.base && rip < m.base + m.size)?;
    let base = m.base;
    if read_u16(t, base)? != 0x5A4D {
        return None;
    }
    let pe = base + read_u32(t, base + 0x3C)? as u64;
    if read_u32(t, pe)? != 0x4550 {
        return None;
    }
    let opt = pe + 24;
    let dirs = match read_u16(t, opt)? {
        0x10B => opt + 96,
        0x20B => opt + 112,
        _ => return None,
    };
    let at = dirs + DIRECTORY_ENTRY_EXCEPTION * 8;
    let rva = read_u32(t, at)?;
    let size = read_u32(t, at + 4)?;
    (rva != 0 && size >= RUNTIME_FUNCTION_SIZE as u32).then_some((base, base + rva as u64, size))
}

/// The `RUNTIME_FUNCTION` covering `rip`, as `(image base, its guest address)`.
///
/// `None` means the address is in a leaf function, which is what a function
/// with no `.pdata` entry is: it allocates nothing on entry, so its caller's
/// stack is recovered by popping the return address.
pub(crate) fn lookup_function_entry(t: &Thread<'_, '_>, rip: u64) -> Option<(u64, u64)> {
    let (base, table, size) = function_table(t, rip)?;
    let rva = (rip - base) as u32;
    let count = size / RUNTIME_FUNCTION_SIZE as u32;
    let (mut lo, mut hi) = (0u32, count);
    while hi > lo {
        let mid = (lo + hi) / 2;
        let at = table + mid as u64 * RUNTIME_FUNCTION_SIZE;
        let begin = read_u32(t, at)?;
        let end = read_u32(t, at + 4)?;
        if rva < begin {
            hi = mid;
        } else if rva >= end {
            lo = mid + 1;
        } else {
            return Some((base, at));
        }
    }
    None
}

/// One function's unwind information, read from the image in guest memory.
struct UnwindInfo {
    flags: u8,
    size_of_prolog: u8,
    count: u64,
    frame_register: u8,
    frame_offset: u64,
    /// Guest address of `UnwindCode[0]`.
    codes: u64,
}

impl UnwindInfo {
    /// The unwind codes, as `(offset into the function, operation, operand)`.
    fn code(&self, t: &Thread<'_, '_>, i: u64) -> Option<(u8, u8, u8)> {
        let at = self.codes + i * 2;
        let offset = byte(t, at)?;
        let op = byte(t, at + 1)?;
        Some((offset, op & 0xF, op >> 4))
    }

    /// The two-byte operand following the code at slot `i`, scaled by the
    /// operation that owns it (`offset / 8` or `offset / 16`).
    fn slot(&self, t: &Thread<'_, '_>, i: u64) -> Option<u16> {
        read_u16(t, self.codes + i * 2)
    }

    /// The 32-bit operand spanning slots `i` and `i + 1`.
    fn slot32(&self, t: &Thread<'_, '_>, i: u64) -> Option<u32> {
        read_u32(t, self.codes + i * 2)
    }

    /// Guest address of the language handler's RVA, which follows the codes.
    fn handler_slot(&self) -> u64 {
        self.codes + ((self.count + 1) & !1) * 2
    }

    /// Guest address of the chained `RUNTIME_FUNCTION`, for `UNW_FLAG_CHAININFO`.
    fn chained_entry(&self) -> u64 {
        self.codes + ((self.count + 1) & !1) * 2
    }
}

fn unwind_info(t: &Thread<'_, '_>, base: u64, entry: u64) -> Option<UnwindInfo> {
    let rva = read_u32(t, entry + 8)?;
    let at = base + rva as u64;
    let header = guest_bytes(t, at, 4)?;
    let version = header[0] & 0x7;
    let flags = header[0] >> 3;
    // Versions 1 and 2 share the layout; version 3 is a different format.
    if version != 1 && version != 2 {
        return None;
    }
    Some(UnwindInfo {
        flags,
        size_of_prolog: header[1],
        count: header[2] as u64,
        frame_register: header[3] & 0xF,
        frame_offset: (header[3] >> 4) as u64,
        codes: at + 4,
    })
}

/// The functable range of a `RUNTIME_FUNCTION` entry: `(begin, end)` as RVAs.
fn entry_range(t: &Thread<'_, '_>, entry: u64) -> Option<(u32, u32)> {
    Some((read_u32(t, entry)?, read_u32(t, entry + 4)?))
}

// ---------------------------------------------------------------------------
// The frame a function establishes, and the virtual unwind.
// ---------------------------------------------------------------------------

/// The stack pointer the function's frame is based on: the frame register's
/// value when it uses one (established at `FrameOffset * 16` above the frame),
/// otherwise the current stack pointer.
fn establisher_frame(
    t: &Thread<'_, '_>,
    ctx: &X86Context,
    info: &UnwindInfo,
    code_offset: u64,
) -> u64 {
    if info.frame_register == 0 {
        return ctx.regs[x86_reg::RSP];
    }
    if code_offset >= info.size_of_prolog as u64 || info.flags & UNW_FLAG_CHAININFO != 0 {
        return frame_register_value(ctx, info);
    }
    // Inside the prolog the frame register holds something else until the
    // `mov <reg>, rsp` the unwind info places: find where that is.
    let mut i = 0;
    while i < info.count {
        let Some((offset, op, op_info)) = info.code(t, i) else {
            break;
        };
        if offset as u64 <= code_offset && op == UWOP_SET_FPREG {
            return frame_register_value(ctx, info);
        }
        i += code_slots(op, op_info);
    }
    ctx.regs[x86_reg::RSP]
}

fn frame_register_value(ctx: &X86Context, info: &UnwindInfo) -> u64 {
    ctx.regs[info.frame_register as usize].wrapping_sub(info.frame_offset * 16)
}

/// If the thread stopped inside a function's epilog, its unwind information
/// describes the prolog only, so the epilog's own instructions are decoded
/// instead: the `add rsp`/`pop`s leading up to the `ret`. Returns whether the
/// epilog was recognised, in which case `ctx` has been unwound.
fn try_unwind_epilog(
    t: &Thread<'_, '_>,
    control_pc: u64,
    function_end: u64,
    ctx: &mut X86Context,
) -> bool {
    let Some(end) = function_end.checked_sub(1) else {
        return false;
    };
    let mut c = ctx.clone();
    let mut pc = control_pc;
    let Some(b) = guest_bytes(t, pc, 7) else {
        return false;
    };
    if b[0] == 0x48 && b[1] == 0x83 && b[2] == 0xC4 {
        c.regs[x86_reg::RSP] = c.regs[x86_reg::RSP].wrapping_add(b[3] as u64);
        pc += 4;
    } else if b[0] == 0x48 && b[1] == 0x81 && b[2] == 0xC4 {
        let n = u32::from_le_bytes([b[3], b[4], b[5], b[6]]) as u64;
        c.regs[x86_reg::RSP] = c.regs[x86_reg::RSP].wrapping_add(n);
        pc += 7;
    }
    while pc < end {
        let Some(b) = guest_bytes(t, pc, 2) else {
            return false;
        };
        let register = if b[0] & 0xF8 == 0x58 {
            pc += 1;
            (b[0] & 0x7) as usize
        } else if b[0] == 0x41 && b[1] & 0xF8 == 0x58 {
            pc += 2;
            8 + (b[1] & 0x7) as usize
        } else {
            return false;
        };
        let Some(value) = read_u64(t, c.regs[x86_reg::RSP]) else {
            return false;
        };
        c.regs[register] = value;
        c.regs[x86_reg::RSP] = c.regs[x86_reg::RSP].wrapping_add(8);
    }
    if pc != end || byte(t, end) != Some(0xC3) {
        return false;
    }
    let Some(rip) = read_u64(t, c.regs[x86_reg::RSP]) else {
        return false;
    };
    c.rip = rip;
    c.regs[x86_reg::RSP] = c.regs[x86_reg::RSP].wrapping_add(8);
    *ctx = c;
    true
}

/// What unwinding one frame found.
struct VirtualUnwind {
    /// The language handler the frame names, or 0 for a frame without one.
    handler: u64,
    /// The handler's own data, which for a C `__try` is its scope table.
    handler_data: u64,
    /// The frame this function established, which is what a handler is given
    /// and what `RtlUnwind` names as its target.
    establisher: u64,
}

/// Undo one frame: apply the function's unwind codes to `ctx` so that it
/// describes the caller, and report the handler the frame names.
///
/// `handler_type` is `UNW_FLAG_EHANDLER` while searching for a handler and
/// `UNW_FLAG_UHANDLER` while unwinding.
fn virtual_unwind(
    t: &Thread<'_, '_>,
    handler_type: u8,
    image_base: u64,
    control_pc: u64,
    entry: u64,
    ctx: &mut X86Context,
) -> Option<VirtualUnwind> {
    let control_rva = control_pc.checked_sub(image_base)? as u32;
    let (begin, end) = entry_range(t, entry)?;
    if control_rva < begin || control_rva >= end {
        return None;
    }
    let mut info = unwind_info(t, image_base, entry)?;
    let control_offset = (control_rva - begin) as u64;
    let establisher = establisher_frame(t, ctx, &info, control_offset);

    // An exception inside an epilog has no unwind codes for the instructions
    // already executed, so decode them instead of trusting the prolog's.
    if control_offset > info.size_of_prolog as u64 && info.count > 0 {
        let function_end = image_base + end as u64;
        if try_unwind_epilog(t, control_pc, function_end, ctx) {
            return Some(VirtualUnwind {
                handler: 0,
                handler_data: 0,
                establisher,
            });
        }
    }

    // Skip the codes for prolog steps the thread has not reached yet: they are
    // in the order the prolog pushed them, so the first code at or below the
    // control offset is where unwinding starts.
    let mut i = 0;
    while i < info.count {
        let (offset, op, op_info) = info.code(t, i)?;
        if (offset as u64) <= control_offset {
            break;
        }
        i += code_slots(op, op_info);
    }

    let mut handler = 0;
    let mut handler_data = 0;
    loop {
        while i < info.count {
            let (_, op, op_info) = info.code(t, i)?;
            let register = op_info as usize;
            match op {
                UWOP_PUSH_NONVOL => {
                    ctx.regs[register] = read_u64(t, ctx.regs[x86_reg::RSP])?;
                    ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(8);
                    i += 1;
                }
                UWOP_ALLOC_LARGE => {
                    if op_info == 0 {
                        let n = info.slot(t, i + 1)? as u64 * 8;
                        ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(n);
                        i += 2;
                    } else {
                        let n = info.slot32(t, i + 1)? as u64;
                        ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(n);
                        i += 3;
                    }
                }
                UWOP_ALLOC_SMALL => {
                    let n = (op_info as u64 + 1) * 8;
                    ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(n);
                    i += 1;
                }
                UWOP_SET_FPREG => {
                    ctx.regs[x86_reg::RSP] = frame_register_value(ctx, &info);
                    i += 1;
                }
                UWOP_SAVE_NONVOL => {
                    // The slot holds the offset divided by 8.
                    let offset = info.slot(t, i + 1)? as u64 * 8;
                    let at = ctx.regs[x86_reg::RSP].wrapping_add(offset);
                    ctx.regs[register] = read_u64(t, at)?;
                    i += 2;
                }
                UWOP_SAVE_NONVOL_FAR => {
                    let offset = info.slot32(t, i + 1)? as u64;
                    let at = ctx.regs[x86_reg::RSP].wrapping_add(offset);
                    ctx.regs[register] = read_u64(t, at)?;
                    i += 3;
                }
                UWOP_SAVE_XMM128 => {
                    let offset = info.slot(t, i + 1)? as u64 * 16;
                    let at = ctx.regs[x86_reg::RSP].wrapping_add(offset);
                    ctx.xmm[register & 0xF] = read_xmm(t, at)?;
                    i += 2;
                }
                UWOP_SAVE_XMM128_FAR => {
                    let offset = info.slot32(t, i + 1)? as u64;
                    let at = ctx.regs[x86_reg::RSP].wrapping_add(offset);
                    ctx.xmm[register & 0xF] = read_xmm(t, at)?;
                    i += 3;
                }
                UWOP_EPILOG => {
                    // Version 2 metadata; nothing to undo.
                    i += 2;
                }
                UWOP_SPARE_CODE => {
                    i += 3;
                }
                UWOP_PUSH_MACHFRAME => {
                    // A hardware interrupt frame: the error code, if any, sits
                    // first, then RIP, CS, EFLAGS and the stack pointer it came
                    // from.
                    let skip = op_info as u64 * 8;
                    let at = ctx.regs[x86_reg::RSP].wrapping_add(skip);
                    ctx.rip = read_u64(t, at)?;
                    ctx.regs[x86_reg::RSP] = read_u64(t, at + 0x18)?;
                    return Some(VirtualUnwind {
                        handler: 0,
                        handler_data: 0,
                        establisher,
                    });
                }
                _ => return None,
            }
        }
        if info.flags & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        // The record chains to another function's unwind info, which describes
        // the rest of the frame (a caller's prolog, for instance).
        let chained = info.chained_entry();
        info = unwind_info(t, image_base, chained)?;
        i = 0;
    }

    // Undoing the frame leaves the stack pointer on the return address.
    let sp = ctx.regs[x86_reg::RSP];
    if sp != 0 {
        ctx.rip = read_u64(t, sp)?;
        ctx.regs[x86_reg::RSP] = sp.wrapping_add(8);
    }

    if wants_handler(info.flags, handler_type) {
        let slot = info.handler_slot();
        let rva = read_u32(t, slot)?;
        if rva != 0 {
            handler = image_base + rva as u64;
            handler_data = slot + 4;
        }
    }
    Some(VirtualUnwind {
        handler,
        handler_data,
        establisher,
    })
}

/// The stack pointer the function containing `control_pc` established, without
/// unwinding it: what a language handler is given as its frame, and what
/// `RtlUnwindEx` names as the frame it is unwinding to.
fn establisher_at(t: &Thread<'_, '_>, control_pc: u64, ctx: &X86Context) -> Option<u64> {
    let (base, entry) = lookup_function_entry(t, control_pc)?;
    let (begin, end) = entry_range(t, entry)?;
    let rva = control_pc.checked_sub(base)? as u32;
    if rva < begin || rva >= end {
        return None;
    }
    let info = unwind_info(t, base, entry)?;
    Some(establisher_frame(t, ctx, &info, (rva - begin) as u64))
}

/// The language handler the function at `entry` names for `handler_type`, and
/// the handler data that follows it (a C scope table, for a `__try`).
fn entry_handler(
    t: &Thread<'_, '_>,
    base: u64,
    entry: u64,
    handler_type: u8,
) -> Option<(u64, u64)> {
    let info = unwind_info(t, base, entry)?;
    if !wants_handler(info.flags, handler_type) {
        return None;
    }
    let slot = info.handler_slot();
    let rva = read_u32(t, slot)?;
    (rva != 0).then_some((base + rva as u64, slot + 4))
}

fn read_xmm(t: &Thread<'_, '_>, addr: u64) -> Option<u128> {
    let b = guest_bytes(t, addr, 16)?;
    Some(u128::from_le_bytes(b.try_into().ok()?))
}

// ---------------------------------------------------------------------------
// CONTEXT and EXCEPTION_RECORD in guest memory.
// ---------------------------------------------------------------------------

/// The `CONTEXT` a handler sees for `ctx`.
fn write_context_64(t: &mut Thread<'_, '_>, addr: u64, ctx: &X86Context) -> bool {
    let mut ok = write_u32(t, addr + ctx64::CONTEXT_FLAGS, ctx64::CONTEXT_FULL)
        && write_u32(t, addr + ctx64::MX_CSR, 0x1F80);
    for (i, value) in ctx.regs.iter().enumerate() {
        ok &= write_u64(t, addr + ctx64::RAX + i as u64 * 8, *value);
    }
    ok &= write_u64(t, addr + ctx64::RIP, ctx.rip)
        && write_u32(t, addr + ctx64::EFLAGS, ctx.rflags as u32)
        && write_u16(t, addr + ctx64::SEG_CS, 0x33)
        && write_u16(t, addr + ctx64::SEG_DS, 0x2B)
        && write_u16(t, addr + ctx64::SEG_ES, 0x2B)
        && write_u16(t, addr + ctx64::SEG_SS, 0x2B)
        && write_u16(t, addr + ctx64::SEG_FS, 0)
        && write_u16(t, addr + ctx64::SEG_GS, 0);
    for (i, value) in ctx.xmm.iter().enumerate() {
        ok &= write_bytes(t, addr + ctx64::XMM0 + i as u64 * 16, &value.to_le_bytes());
    }
    ok
}

/// Read back a `CONTEXT` a handler may have changed.
fn read_context_64(t: &mut Thread<'_, '_>, addr: u64) -> X86Context {
    let mut ctx = t.ctx.clone();
    for (i, slot) in ctx.regs.iter_mut().enumerate() {
        if let Some(v) = read_u64(t, addr + ctx64::RAX + i as u64 * 8) {
            *slot = v;
        }
    }
    if let Some(rip) = read_u64(t, addr + ctx64::RIP) {
        ctx.rip = rip;
    }
    if let Some(flags) = read_u32(t, addr + ctx64::EFLAGS) {
        ctx.rflags = flags as u64;
    }
    for i in 0..16u64 {
        if let Some(v) = read_xmm(t, addr + ctx64::XMM0 + i * 16) {
            ctx.xmm[i as usize] = v;
        }
    }
    ctx
}

fn write_exception_record_64(
    t: &mut Thread<'_, '_>,
    addr: u64,
    code: u32,
    flags: u32,
    exception_addr: u64,
    params: &[u64],
) -> bool {
    let n = params.len().min(15) as u32;
    let mut ok = write_u32(t, addr, code)
        && write_u32(t, addr + 0x04, flags)
        && write_u64(t, addr + 0x08, 0)
        && write_u64(t, addr + 0x10, exception_addr)
        && write_u32(t, addr + 0x18, n)
        && write_u32(t, addr + 0x1C, 0);
    for (i, p) in params.iter().take(n as usize).enumerate() {
        ok &= write_u64(t, addr + 0x20 + i as u64 * 8, *p);
    }
    ok
}

fn read_exception_record_64(t: &Thread<'_, '_>, addr: u64) -> Option<(u32, u32, u64, Vec<u64>)> {
    let code = read_u32(t, addr)?;
    let flags = read_u32(t, addr + 0x04)?;
    let exception_addr = read_u64(t, addr + 0x10)?;
    let n = read_u32(t, addr + 0x18)?.min(15);
    let mut params = Vec::with_capacity(n as usize);
    for i in 0..n as u64 {
        params.push(read_u64(t, addr + 0x20 + i * 8)?);
    }
    Some((code, flags, exception_addr, params))
}

/// `DISPATCHER_CONTEXT` for one frame: what the language handler is told.
#[allow(clippy::too_many_arguments)]
fn write_dispatcher_context(
    t: &mut Thread<'_, '_>,
    addr: u64,
    control_pc: u64,
    image_base: u64,
    entry: u64,
    establisher: u64,
    target_ip: u64,
    context_record: u64,
    language_handler: u64,
    handler_data: u64,
) -> bool {
    write_u64(t, addr, control_pc)
        && write_u64(t, addr + 0x08, image_base)
        && write_u64(t, addr + 0x10, entry)
        && write_u64(t, addr + 0x18, establisher)
        && write_u64(t, addr + 0x20, target_ip)
        && write_u64(t, addr + 0x28, context_record)
        && write_u64(t, addr + 0x30, language_handler)
        && write_u64(t, addr + 0x38, handler_data)
        && write_u64(t, addr + 0x40, 0)
        && write_u32(t, addr + 0x48, 0)
        && write_u32(t, addr + 0x4C, 0)
}

/// Scratch space for a dispatch: the `CONTEXT`, `EXCEPTION_RECORD` and
/// `DISPATCHER_CONTEXT` carved below the stack pointer, clear of the frames the
/// handler itself will use.
struct Scratch {
    ctx: u64,
    record: u64,
    dispatcher: u64,
}

fn scratch(from: u64) -> Scratch {
    let dispatcher = from
        .saturating_sub(0x1000 + DISPATCHER_CONTEXT_64_SIZE)
        .saturating_sub(0x10)
        & !0xF;
    let record = dispatcher
        .saturating_sub(EXCEPTION_RECORD_64_SIZE)
        .saturating_sub(0x10)
        & !0xF;
    let ctx = record.saturating_sub(ctx64::SIZE).saturating_sub(0x10) & !0xF;
    Scratch {
        ctx,
        record,
        dispatcher,
    }
}

/// The stack limits of the running thread: outside them a walk has lost its way.
///
/// The thread's own limits come from its TEB rather than from where the runtime
/// happens to have put the frames it is running: a language handler unwinds from
/// the point that raised, which is above every frame the runtime added to reach
/// the handler.
fn stack_bounds(t: &Thread<'_, '_>) -> (u64, u64) {
    let (limit, base) = t.stack_bounds();
    if limit != 0 && base > limit {
        return (limit, base);
    }
    let index = t.current.min(t.threads.len().saturating_sub(1));
    let low = t.threads.get(index).map_or(0, |th| th.stack_base);
    (low, t.stack_top + 0x100)
}

/// The guest context at the call that raised: the stack top holds its return
/// address, exactly as an unwind of a function with no frame would leave it.
fn caller_context(t: &Thread<'_, '_>) -> X86Context {
    let mut ctx = t.ctx.clone();
    if let Some(rip) = read_u64(t, ctx.regs[x86_reg::RSP]) {
        ctx.rip = rip;
        ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(8);
    }
    ctx
}

// ---------------------------------------------------------------------------
// Dispatch and unwind.
// ---------------------------------------------------------------------------

/// Walk the function table from the raise point, calling each frame's language
/// handler until one takes the exception.
///
/// Returns `Err(Halt::DirectResume)` once a handler has installed the context
/// execution continues at; the exception is reported when no handler takes it.
pub(crate) fn dispatch_exception_64(
    t: &mut Thread<'_, '_>,
    code: u32,
    flags: u32,
    exception_addr: u64,
    params: &[u64],
) -> Result<u64, Halt> {
    let mut ctx = caller_context(t);
    let raise = ctx.clone();
    let space = scratch(ctx.regs[x86_reg::RSP]);
    if !write_context_64(t, space.ctx, &raise)
        || !write_exception_record_64(t, space.record, code, flags, exception_addr, params)
    {
        return Err(RunError::GuestException {
            code,
            location: t.describe(exception_addr),
        }
        .into());
    }

    // A language handler reached from here unwinds from the raise point, not
    // from its own frame: it runs below the runtime's dispatcher frames.
    let previous = t.exception_ctx64.replace(raise);
    let result = dispatch_walk(t, &mut ctx, &space, code, exception_addr);
    t.exception_ctx64 = previous;
    result
}

fn dispatch_walk(
    t: &mut Thread<'_, '_>,
    ctx: &mut X86Context,
    space: &Scratch,
    code: u32,
    exception_addr: u64,
) -> Result<u64, Halt> {
    let (low, high) = stack_bounds(t);
    for _ in 0..512 {
        let control_pc = ctx.rip;
        let sp = ctx.regs[x86_reg::RSP];
        if sp < low || sp >= high || sp & 7 != 0 {
            break;
        }
        let step = match lookup_function_entry(t, control_pc) {
            Some((base, entry)) => {
                match virtual_unwind(t, UNW_FLAG_EHANDLER, base, control_pc, entry, ctx) {
                    Some(step) => Some((base, entry, step)),
                    None => break,
                }
            }
            None => {
                // A leaf function: its caller is at the top of the stack.
                let Some(rip) = read_u64(t, ctx.regs[x86_reg::RSP]) else {
                    break;
                };
                ctx.rip = rip;
                ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(8);
                continue;
            }
        };
        let Some((base, entry, step)) = step else {
            break;
        };
        if step.handler == 0 {
            continue;
        }
        // The handler is given the context of the raise, so a filter that
        // continues execution can send the thread back to where it faulted.
        if !write_dispatcher_context(
            t,
            space.dispatcher,
            control_pc,
            base,
            entry,
            step.establisher,
            0,
            space.ctx,
            step.handler,
            step.handler_data,
        ) {
            break;
        }
        let call = t.call(
            step.handler,
            &[space.record, step.establisher, space.ctx, space.dispatcher],
        );
        let disposition = match call {
            Ok(v) => v,
            Err(Halt::Exit(code)) => return Err(Halt::Exit(code)),
            Err(Halt::ExitThread(code)) => return Err(Halt::ExitThread(code)),
            // A handler that unwound to its `except` block has already set the
            // context execution continues at.
            Err(Halt::DirectResume) => return Err(Halt::DirectResume),
            // A handler that faulted or called an API with no implementation is
            // passed over, as in the x86 walk.
            Err(_) => continue,
        };
        if disposition & 0xFFFF_FFFF == EXCEPTION_CONTINUE_EXECUTION {
            // The filter fixed things up: resume wherever the context it left
            // in the record now points.
            let resumed = read_context_64(t, space.ctx);
            t.ctx = resumed;
            return Err(Halt::DirectResume);
        }
        // Anything else is `ExceptionContinueSearch`: keep looking outward.
    }
    Err(RunError::GuestException {
        code,
        location: t.describe(exception_addr),
    }
    .into())
}

/// `RtlUnwind` and `RtlUnwindEx`: run the `finally` handlers of every frame
/// between the current one and `target_frame`, then continue at `target_ip`
/// with `return_value` in `Rax`.
///
/// A null `target_frame` unwinds every frame it can and returns, which is what
/// a program on its way out wants.
pub(crate) fn unwind_64(
    t: &mut Thread<'_, '_>,
    target_frame: u64,
    target_ip: u64,
    record: u64,
    return_value: u64,
    context_record: u64,
) -> Result<u64, Halt> {
    // Inside a handler the frames to unwind are the ones from the raise point
    // outward; called directly they start at our caller.
    let mut ctx = t
        .exception_ctx64
        .clone()
        .unwrap_or_else(|| caller_context(t));
    let space = scratch(ctx.regs[x86_reg::RSP]);
    let context_record = if context_record != 0 {
        context_record
    } else {
        space.ctx
    };

    // The unwinding flags go on a copy: the caller's record is left alone.
    let (code, mut flags, exception_addr, params) = match (record != 0)
        .then(|| read_exception_record_64(t, record))
        .flatten()
    {
        Some((code, flags, addr, params)) => (code, flags, addr, params),
        None => (STATUS_UNWIND, 0, ctx.rip, Vec::new()),
    };
    flags |= EXCEPTION_UNWINDING;
    if target_frame == 0 {
        flags |= EXCEPTION_EXIT_UNWIND;
    }
    ctx.regs[x86_reg::RAX] = return_value;
    if !write_context_64(t, context_record, &ctx)
        || !write_exception_record_64(t, space.record, code, flags, exception_addr, &params)
    {
        return Err(RunError::GuestException {
            code,
            location: t.describe(exception_addr),
        }
        .into());
    }

    let (low, high) = stack_bounds(t);
    let mut reached_target = false;
    for _ in 0..512 {
        let control_pc = ctx.rip;
        let sp = ctx.regs[x86_reg::RSP];
        if sp < low || sp >= high || sp & 7 != 0 {
            break;
        }
        // The frame the unwind is heading to keeps its own stack: its prolog is
        // *not* undone, since the code waiting at `target_ip` is usually inside
        // it and needs the frame it established. Its handler is still called,
        // so a `finally` at that level can run.
        if target_frame != 0 && establisher_at(t, control_pc, &ctx) == Some(target_frame) {
            if let Some((base, entry)) = lookup_function_entry(t, control_pc) {
                if let Some((handler, handler_data)) =
                    entry_handler(t, base, entry, UNW_FLAG_UHANDLER)
                {
                    if !write_context_64(t, context_record, &ctx)
                        || !write_exception_record_64(
                            t,
                            space.record,
                            code,
                            flags | EXCEPTION_TARGET_UNWIND,
                            exception_addr,
                            &params,
                        )
                        || !write_dispatcher_context(
                            t,
                            space.dispatcher,
                            control_pc,
                            base,
                            entry,
                            target_frame,
                            target_ip,
                            context_record,
                            handler,
                            handler_data,
                        )
                    {
                        break;
                    }
                    match t.call(
                        handler,
                        &[space.record, target_frame, context_record, space.dispatcher],
                    ) {
                        Ok(_) => {}
                        Err(Halt::Exit(code)) => return Err(Halt::Exit(code)),
                        Err(Halt::ExitThread(code)) => return Err(Halt::ExitThread(code)),
                        Err(Halt::DirectResume) => return Err(Halt::DirectResume),
                        Err(_) => {}
                    }
                }
            }
            reached_target = true;
            break;
        }
        let step = match lookup_function_entry(t, control_pc) {
            Some((base, entry)) => {
                match virtual_unwind(t, UNW_FLAG_UHANDLER, base, control_pc, entry, &mut ctx) {
                    Some(step) => (base, entry, step),
                    None => break,
                }
            }
            None => {
                // A leaf function: its caller is at the top of the stack.
                let Some(rip) = read_u64(t, ctx.regs[x86_reg::RSP]) else {
                    break;
                };
                ctx.rip = rip;
                ctx.regs[x86_reg::RSP] = ctx.regs[x86_reg::RSP].wrapping_add(8);
                continue;
            }
        };
        let (base, entry, step) = step;
        if step.handler != 0 {
            ctx.regs[x86_reg::RAX] = return_value;
            if !write_context_64(t, context_record, &ctx)
                || !write_exception_record_64(t, space.record, code, flags, exception_addr, &params)
                || !write_dispatcher_context(
                    t,
                    space.dispatcher,
                    control_pc,
                    base,
                    entry,
                    step.establisher,
                    target_ip,
                    context_record,
                    step.handler,
                    step.handler_data,
                )
            {
                break;
            }
            match t.call(
                step.handler,
                &[
                    space.record,
                    step.establisher,
                    context_record,
                    space.dispatcher,
                ],
            ) {
                Ok(_) => {}
                Err(Halt::Exit(code)) => return Err(Halt::Exit(code)),
                Err(Halt::ExitThread(code)) => return Err(Halt::ExitThread(code)),
                Err(Halt::DirectResume) => return Err(Halt::DirectResume),
                // A handler that faulted is left alone, as in the x86 walk.
                Err(_) => {}
            }
        }
        // Each frame unwound gives the handler the next stack pointer, which
        // is how a `finally` block that raises again can be told apart.
        if !write_context_64(t, context_record, &ctx) {
            break;
        }
    }

    // A null target unwinds everything it can and returns, as does a walk that
    // ran out of frames before reaching the target; a program on its way out
    // wants the call to come back rather than the thread raised.
    if target_frame == 0 || !reached_target {
        return Ok(return_value);
    }

    // Continue where the target asked, on the target frame's own stack: the
    // handler may have moved the context, so its copy is what runs.
    let mut resumed = read_context_64(t, context_record);
    resumed.rip = target_ip;
    resumed.regs[x86_reg::RSP] = target_frame;
    resumed.regs[x86_reg::RAX] = return_value;
    t.ctx = resumed;
    Err(Halt::DirectResume)
}

/// `RtlCaptureContext`: fill in a `CONTEXT` with the calling thread's registers.
pub(crate) fn capture_context_64(t: &mut Thread<'_, '_>, addr: u64) -> bool {
    let ctx = t.ctx.clone();
    write_context_64(t, addr, &ctx)
}
