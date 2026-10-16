//! Instruction-level checks for the interpreter, on hand-assembled snippets.
//! Broader coverage comes from the self-test fixture, which is compared with a
//! real CPU (see anyos-win32's tests).

use anyos_backend::{
    x86_reg::*, ExecutionBackend, GuestContext, GuestFault, StopReason, X86Context,
};
use anyos_interp::Interpreter;
use anyos_memory::{Access, AddressSpace, Protection};

const CODE: u64 = 0x10000;
const DATA: u64 = 0x20000;
const STACK_TOP: u64 = 0x40000;

/// Run `code` (an `int3` is appended) and return the final context and stop.
fn run(long: bool, code: &[u8], setup: impl FnOnce(&mut X86Context)) -> (X86Context, StopReason) {
    let mut mem = AddressSpace::new(0x10000, 0x8000_0000, 1 << 24);
    mem.map(CODE, 0x1000, Protection::RX, "code").unwrap();
    mem.write_raw(CODE, code).unwrap();
    mem.write_raw(CODE + code.len() as u64, &[0xCC]).unwrap();
    mem.map(DATA, 0x1000, Protection::RW, "data").unwrap();
    mem.map(STACK_TOP - 0x1000, 0x1000, Protection::RW, "stack")
        .unwrap();
    let mut c = X86Context::new(long);
    c.rip = CODE;
    c.regs[RSP] = STACK_TOP - 0x100;
    setup(&mut c);
    let mut ctx = GuestContext::X86(c);
    let stop = Interpreter::new().run(&mut ctx, &mut mem, Some(10_000));
    let GuestContext::X86(c) = ctx else {
        unreachable!()
    };
    (c, stop)
}

fn ends_at_int3(code: &[u8], stop: &StopReason) {
    assert_eq!(
        *stop,
        StopReason::Breakpoint {
            address: CODE + code.len() as u64
        }
    );
}

const CF: u64 = 1;
const ZF: u64 = 1 << 6;
const SF: u64 = 1 << 7;
const OF: u64 = 1 << 11;

#[test]
fn add_sets_carry_and_overflow() {
    // mov eax, 0x7fffffff; add eax, 1
    let code = [0xB8, 0xFF, 0xFF, 0xFF, 0x7F, 0x83, 0xC0, 0x01];
    let (c, stop) = run(true, &code, |_| {});
    ends_at_int3(&code, &stop);
    assert_eq!(c.regs[RAX], 0x8000_0000);
    assert_eq!(c.rflags & (CF | ZF | SF | OF), SF | OF);
    // mov eax, -1; add eax, 1 -> carry and zero, no overflow
    let code = [0xB8, 0xFF, 0xFF, 0xFF, 0xFF, 0x83, 0xC0, 0x01];
    let (c, _) = run(true, &code, |_| {});
    assert_eq!(c.rflags & (CF | ZF | SF | OF), CF | ZF);
}

#[test]
fn thirty_two_bit_writes_zero_extend_in_long_mode() {
    // mov rax, -1; mov eax, 5; mov rbx, -1; mov bl, 7; mov bh, 9 (no REX: bh)
    let code = [
        0x48, 0xC7, 0xC0, 0xFF, 0xFF, 0xFF, 0xFF, 0xB8, 0x05, 0, 0, 0, 0x48, 0xC7, 0xC3, 0xFF,
        0xFF, 0xFF, 0xFF, 0xB3, 0x07, 0xB7, 0x09,
    ];
    let (c, _) = run(true, &code, |_| {});
    assert_eq!(c.regs[RAX], 5);
    assert_eq!(c.regs[RBX], 0xFFFF_FFFF_FFFF_0907);
}

#[test]
fn rip_relative_load_uses_next_instruction() {
    // lea rax, [rip+0x10] (7 bytes) -> CODE + 7 + 0x10
    let code = [0x48, 0x8D, 0x05, 0x10, 0, 0, 0];
    let (c, _) = run(true, &code, |_| {});
    assert_eq!(c.regs[RAX], CODE + 7 + 0x10);
}

#[test]
fn call_and_ret_balance_the_stack() {
    // call +1; int3 (skipped); ... target: mov eax, 42; ret -> back to the int3?
    // Layout: E8 01 00 00 00 | EB 06 | B8 2A 00 00 00 | C3 | (int3 appended)
    // call lands on mov, ret returns to the jmp, which skips over mov/ret to the int3.
    let code = [0xE8, 0x02, 0, 0, 0, 0xEB, 0x06, 0xB8, 0x2A, 0, 0, 0, 0xC3];
    let (c, stop) = run(true, &code, |_| {});
    ends_at_int3(&code, &stop);
    assert_eq!(c.regs[RAX], 42);
    assert_eq!(c.regs[RSP], STACK_TOP - 0x100);
}

#[test]
fn divide_by_zero_faults_without_moving_rip() {
    // xor ecx, ecx; div ecx
    let code = [0x31, 0xC9, 0xF7, 0xF1];
    let (c, stop) = run(true, &code, |_| {});
    assert_eq!(
        stop,
        StopReason::Fault(GuestFault::DivideError { pc: CODE + 2 })
    );
    assert_eq!(c.rip, CODE + 2);
}

#[test]
fn writes_to_read_only_memory_fault() {
    // mov dword [CODE], eax (32-bit absolute through SIB, no base)
    let code = [0x89, 0x04, 0x25, 0x00, 0x00, 0x01, 0x00];
    let (_, stop) = run(true, &code, |_| {});
    assert_eq!(
        stop,
        StopReason::Fault(GuestFault::Memory {
            pc: CODE,
            address: CODE,
            access: Access::Write
        })
    );
}

#[test]
fn executing_data_faults() {
    // jmp DATA via rax
    let code = [0xFF, 0xE0];
    let (_, stop) = run(true, &code, |c| c.regs[RAX] = DATA);
    assert_eq!(
        stop,
        StopReason::Fault(GuestFault::Memory {
            pc: DATA,
            address: DATA,
            access: Access::Execute
        })
    );
}

#[test]
fn fs_segment_reads_the_teb_in_32_bit_mode() {
    // mov eax, fs:[0x18]
    let code = [0x64, 0xA1, 0x18, 0, 0, 0];
    let (c, _) = run(false, &code, |c| c.fs_base = DATA);
    // DATA is zero-filled, so the read succeeds with 0.
    assert_eq!(c.regs[RAX], 0);
    // In 32-bit mode 0x40 is `inc eax`, not a REX prefix.
    let (c, _) = run(false, &[0x40, 0x40], |_| {});
    assert_eq!(c.regs[RAX], 2);
}

#[test]
fn rep_stos_fills_and_counts_down() {
    // mov rdi, DATA; mov ecx, 16; mov al, 0xAB; rep stosb; mov rax, [DATA+8]
    let code = [
        0x48, 0xC7, 0xC7, 0x00, 0x00, 0x02, 0x00, 0xB9, 0x10, 0, 0, 0, 0xB0, 0xAB, 0xF3, 0xAA,
        0x48, 0x8B, 0x04, 0x25, 0x08, 0x00, 0x02, 0x00,
    ];
    let (c, stop) = run(true, &code, |_| {});
    ends_at_int3(&code, &stop);
    assert_eq!(c.regs[RAX], 0xABAB_ABAB_ABAB_ABAB);
    assert_eq!(c.regs[RCX], 0);
    assert_eq!(c.regs[RDI], DATA + 16);
}

#[test]
fn unsupported_instructions_are_named() {
    // xsave [rax]
    let (_, stop) = run(true, &[0x0F, 0xAE, 0x20], |c| c.regs[RAX] = DATA);
    let StopReason::Fault(GuestFault::Unsupported { what, .. }) = stop else {
        panic!("{stop:?}")
    };
    assert_eq!(what, "xsave family");
}

#[test]
fn x87_arithmetic_and_exact_integer_round_trips() {
    let code = [
        0xD9, 0xE8, // fld1
        0xD9, 0xEB, // fldpi
        0xDE, 0xC1, // faddp st(1), st
        0xDB, 0x1C, 0x25, 0x00, 0x00, 0x02, 0x00, // fistp dword [DATA]
        0x48, 0xB8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, // mov rax, i64::MAX
        0x48, 0x89, 0x04, 0x25, 0x08, 0x00, 0x02, 0x00, // mov [DATA+8], rax
        0xDF, 0x2C, 0x25, 0x08, 0x00, 0x02, 0x00, // fild qword [DATA+8]
        0xDF, 0x3C, 0x25, 0x10, 0x00, 0x02, 0x00, // fistp qword [DATA+16]
        0xD9, 0xE8, // fld1
        0xD9, 0xEE, // fldz
        0xDF, 0xF1, // fcomip st, st(1): 0 < 1 sets CF
        0x8B, 0x04, 0x25, 0x00, 0x00, 0x02, 0x00, // mov eax, [DATA]
        0x48, 0x8B, 0x1C, 0x25, 0x10, 0x00, 0x02, 0x00, // mov rbx, [DATA+16]
    ];
    let (c, stop) = run(true, &code, |_| {});
    ends_at_int3(&code, &stop);
    assert_eq!(c.regs[RAX], 4); // 1 + pi rounds to 4
    assert_eq!(c.regs[RBX], i64::MAX as u64);
    assert_eq!(c.rflags & (CF | ZF), CF);
    assert_eq!(c.fpu.top, 7); // one value (the 1.0) still pushed
}

#[test]
fn instruction_limit_is_honoured() {
    // jmp $ (spins forever)
    let mut mem = AddressSpace::new(0x10000, 0x8000_0000, 1 << 24);
    mem.map(CODE, 0x1000, Protection::RX, "code").unwrap();
    mem.write_raw(CODE, &[0xEB, 0xFE]).unwrap();
    let mut c = X86Context::new(true);
    c.rip = CODE;
    let mut ctx = GuestContext::X86(c);
    let mut interp = Interpreter::new();
    assert_eq!(
        interp.run(&mut ctx, &mut mem, Some(500)),
        StopReason::InstructionLimit
    );
    assert_eq!(interp.instructions_executed(), 500);
}
