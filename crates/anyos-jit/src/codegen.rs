//! ARM64 native code generation for hot x86-64 code paths
//!
//! Generates ARM64 code that executes x86-64 semantics directly.
//! Focus on common patterns:
//! - Integer arithmetic and logic
//! - Memory operations
//! - Function prologues/epilogues
//! - Common library calls

use crate::{CompiledCode, JitError};
use anyos_backend::GuestContext;
use crate::profiler::ProfileData;

/// ARM64 code generator
pub struct Arm64Generator {
    code: Vec<u8>,
    offset: u32,
}

/// ARM64 registers (16 registers available)
pub mod arm64_regs {
    pub const X0: u8 = 0;   // Return value, argument
    pub const X1: u8 = 1;   // Argument
    pub const X2: u8 = 2;   // Argument
    pub const X3: u8 = 3;   // Argument
    pub const X4: u8 = 4;   // Argument
    pub const X5: u8 = 5;   // Argument
    pub const X6: u8 = 6;   // Argument
    pub const X7: u8 = 7;   // Argument
    pub const X8: u8 = 8;   // Indirect result
    pub const X9: u8 = 9;   // Temporary
    pub const X10: u8 = 10; // Temporary
    pub const X11: u8 = 11; // Temporary
    pub const X12: u8 = 12; // Temporary
    pub const X13: u8 = 13; // Temporary
    pub const X14: u8 = 14; // Temporary
    pub const X15: u8 = 15; // Temporary
    pub const X16: u8 = 16; // IP0 (intra-procedure scratch)
    pub const X17: u8 = 17; // IP1 (intra-procedure scratch)
    pub const X18: u8 = 18; // Platform register
    pub const X19: u8 = 19; // Callee-saved
    pub const X20: u8 = 20; // Callee-saved
    pub const X21: u8 = 21; // Callee-saved
    pub const X22: u8 = 22; // Callee-saved
    pub const X23: u8 = 23; // Callee-saved
    pub const X24: u8 = 24; // Callee-saved
    pub const X25: u8 = 25; // Callee-saved
    pub const X26: u8 = 26; // Callee-saved
    pub const X27: u8 = 27; // Callee-saved
    pub const X28: u8 = 28; // Callee-saved
    pub const X29: u8 = 29; // Frame pointer
    pub const X30: u8 = 30; // Link register
    pub const SP: u8 = 31;  // Stack pointer
}

impl Arm64Generator {
    pub fn new() -> Self {
        Self {
            code: Vec::with_capacity(4096),
            offset: 0,
        }
    }

    /// Emit a 32-bit ARM64 instruction
    fn emit_u32(&mut self, instr: u32) {
        self.code.extend_from_slice(&instr.to_le_bytes());
        self.offset += 4;
    }

    /// MOV immediate (loading 16-bit chunks with MOVZ/MOVK)
    fn emit_movz(&mut self, rd: u8, imm: u16, shift: u8) {
        // MOVZ Xd, #imm, LSL #shift
        // sf=1 (64-bit), opc=10
        let instr = 0xD2800000u32
            | ((shift as u32 & 0x3) << 21)
            | ((imm as u32) << 5)
            | (rd as u32);
        self.emit_u32(instr);
    }

    /// ADD register
    fn emit_add(&mut self, rd: u8, rn: u8, rm: u8) {
        // ADD Xd, Xn, Xm
        // sf=1, op=0
        let instr = 0x8B000000u32
            | ((rm as u32) << 16)
            | ((rn as u32) << 5)
            | (rd as u32);
        self.emit_u32(instr);
    }

    /// SUB register
    fn emit_sub(&mut self, rd: u8, rn: u8, rm: u8) {
        // SUB Xd, Xn, Xm
        let instr = 0xCB000000u32
            | ((rm as u32) << 16)
            | ((rn as u32) << 5)
            | (rd as u32);
        self.emit_u32(instr);
    }

    /// LDR (load register) - load from memory with offset
    fn emit_ldr(&mut self, rt: u8, rn: u8, offset: u16) {
        // LDR Xt, [Xn, #offset]
        // imm12 = offset >> 3 (8-byte units)
        let imm12 = (offset as u32) >> 3;
        let instr = 0xF9400000u32
            | ((imm12 & 0xFFF) << 10)
            | ((rn as u32) << 5)
            | (rt as u32);
        self.emit_u32(instr);
    }

    /// STR (store register) - store to memory with offset
    fn emit_str(&mut self, rt: u8, rn: u8, offset: u16) {
        // STR Xt, [Xn, #offset]
        let imm12 = (offset as u32) >> 3;
        let instr = 0xF9000000u32
            | ((imm12 & 0xFFF) << 10)
            | ((rn as u32) << 5)
            | (rt as u32);
        self.emit_u32(instr);
    }

    /// RET - return from function
    fn emit_ret(&mut self, rn: u8) {
        // RET Xn
        let instr = 0xD65F0000u32 | ((rn as u32) << 5);
        self.emit_u32(instr);
    }

    /// Compile a small code region
    pub fn compile_region(&mut self, _addr: u64, _ctx: &GuestContext) -> Result<CompiledCode, JitError> {
        // Generate a minimal prologue
        // In production, this would analyze the bytecode and generate corresponding ARM64

        // Simple prologue: allocate stack frame
        self.emit_sub(arm64_regs::SP, arm64_regs::SP, arm64_regs::X9);

        // Return
        self.emit_ret(arm64_regs::X30);

        Ok(CompiledCode {
            code: self.code.clone(),
            entry_offset: 0,
            source_addr: _addr,
            size: self.offset,
            execution_count: 0,
            profile: ProfileData::default(),
        })
    }
}

impl Default for Arm64Generator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arm64_movz() {
        let mut gen = Arm64Generator::new();
        gen.emit_movz(arm64_regs::X0, 0x1234, 0);
        assert_eq!(gen.code.len(), 4);
    }

    #[test]
    fn test_arm64_add() {
        let mut gen = Arm64Generator::new();
        gen.emit_add(arm64_regs::X0, arm64_regs::X1, arm64_regs::X2);
        assert_eq!(gen.code.len(), 4);
    }
}
