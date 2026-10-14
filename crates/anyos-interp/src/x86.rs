//! Decoder and executor for one x86 / x86-64 instruction at a time.
//!
//! Each instruction is decoded and executed in one pass. Register values used
//! for addressing are read while decoding, before the instruction changes
//! anything. The instruction pointer only moves once the instruction has
//! completed, so a fault leaves `rip` on the faulting instruction.

use anyos_backend::{GuestFault, StopReason, X86Context};
use anyos_memory::{Access, AddressSpace, MemoryError, PAGE_SIZE};

use crate::sse;

mod x87;

type R<T> = Result<T, GuestFault>;

const CF: u64 = 1;
const PF: u64 = 1 << 2;
const AF: u64 = 1 << 4;
const ZF: u64 = 1 << 6;
const SF: u64 = 1 << 7;
const DF: u64 = 1 << 10;
const OF: u64 = 1 << 11;
/// Flags `popf` may change from user mode: the arithmetic flags, DF, AC and ID.
const POPF_MASK: u64 = CF | PF | AF | ZF | SF | DF | OF | (1 << 18) | (1 << 21);

const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RSP: u8 = 4;
const RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;

const MAX_INSN: usize = 15;

#[derive(Clone, Copy, Debug)]
enum Rm {
    Reg(u8),
    /// Effective address before segment base and RIP are added.
    Mem {
        disp: u64,
        rip_rel: bool,
    },
}

enum Flow {
    Next,
    Jump(u64),
    Stop(StopReason),
}

struct Insn<'a> {
    c: &'a mut X86Context,
    mem: &'a mut AddressSpace,
    pc: u64,
    code: [u8; 16],
    avail: usize,
    pos: usize,
    rex: u8,
    opsize: bool,
    addrsize: bool,
    rep: u8,
    seg: u64,
    tsc: u64,
}

/// Execute one instruction. `Ok(Some(_))` means the thread stopped.
pub(crate) fn step(c: &mut X86Context, mem: &mut AddressSpace, tsc: u64) -> R<Option<StopReason>> {
    let pc = c.rip;
    let (code, avail) = fetch(mem, pc)?;
    let mut i = Insn {
        c,
        mem,
        pc,
        code,
        avail,
        pos: 0,
        rex: 0,
        opsize: false,
        addrsize: false,
        rep: 0,
        seg: 0,
        tsc,
    };
    let flow = i.exec()?;
    let next = i.next();
    let c = i.c;
    match flow {
        Flow::Next => c.rip = next,
        Flow::Jump(t) => c.rip = if c.long_mode { t } else { t & 0xFFFF_FFFF },
        Flow::Stop(s) => {
            c.rip = next;
            return Ok(Some(s));
        }
    }
    Ok(None)
}

/// Copy up to 16 executable bytes at `pc`, stopping at the first page that is
/// not executable.
fn fetch(mem: &AddressSpace, pc: u64) -> R<([u8; 16], usize)> {
    let mut code = [0u8; 16];
    let mut n = 0;
    while n < code.len() {
        let at = pc.wrapping_add(n as u64);
        if mem.check_execute(at).is_err() {
            break;
        }
        let page_end = (at | (PAGE_SIZE - 1)).wrapping_add(1);
        let take = (page_end.wrapping_sub(at) as usize).min(code.len() - n);
        if mem.read_raw(at, &mut code[n..n + take]).is_err() {
            break;
        }
        n += take;
    }
    if n == 0 {
        return Err(GuestFault::Memory {
            pc,
            address: pc,
            access: Access::Execute,
        });
    }
    Ok((code, n))
}

fn mask(size: usize) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

fn sign(size: usize) -> u64 {
    1u64 << (size * 8 - 1)
}

/// Sign-extend the low `size` bytes of `v`.
fn sx(v: u64, size: usize) -> i64 {
    let shift = 64 - size * 8;
    ((v << shift) as i64) >> shift
}

impl Insn<'_> {
    fn next(&self) -> u64 {
        self.pc.wrapping_add(self.pos as u64)
    }

    // ── faults ──────────────────────────────────────────────────────────────

    fn bytes(&self) -> Vec<u8> {
        self.code[..self.pos.max(1).min(self.avail)].to_vec()
    }

    fn invalid(&self) -> GuestFault {
        GuestFault::InvalidOpcode {
            pc: self.pc,
            bytes: self.bytes(),
        }
    }

    fn unsupported(&self, what: &str) -> GuestFault {
        GuestFault::Unsupported {
            pc: self.pc,
            what: what.to_string(),
            bytes: self.bytes(),
        }
    }

    fn mem_fault(&self, e: MemoryError, access: Access) -> GuestFault {
        let address = match e {
            MemoryError::Unmapped { addr } | MemoryError::AccessViolation { addr, .. } => addr,
            _ => 0,
        };
        GuestFault::Memory {
            pc: self.pc,
            address,
            access,
        }
    }

    // ── instruction stream ──────────────────────────────────────────────────

    fn u8(&mut self) -> R<u8> {
        if self.pos >= MAX_INSN {
            return Err(self.invalid());
        }
        if self.pos >= self.avail {
            return Err(GuestFault::Memory {
                pc: self.pc,
                address: self.next(),
                access: Access::Execute,
            });
        }
        let b = self.code[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn le(&mut self, n: usize) -> R<u64> {
        let mut v = 0u64;
        for k in 0..n {
            v |= (self.u8()? as u64) << (8 * k);
        }
        Ok(v)
    }

    fn i8(&mut self) -> R<u64> {
        Ok(self.u8()? as i8 as i64 as u64)
    }

    fn i32(&mut self) -> R<u64> {
        Ok(self.le(4)? as u32 as i32 as i64 as u64)
    }

    /// An immediate of operand size `size` (imm32 sign-extended for 64-bit).
    fn imm(&mut self, size: usize) -> R<u64> {
        match size {
            1 => Ok(self.u8()? as u64),
            2 => self.le(2),
            4 => self.le(4),
            _ => self.i32(),
        }
    }

    // ── sizes ───────────────────────────────────────────────────────────────

    fn rex_w(&self) -> bool {
        self.rex & 8 != 0
    }

    fn osize(&self) -> usize {
        if self.rex_w() {
            8
        } else if self.opsize {
            2
        } else {
            4
        }
    }

    fn asize(&self) -> usize {
        match (self.c.long_mode, self.addrsize) {
            (true, false) => 8,
            (true, true) | (false, false) => 4,
            (false, true) => 2,
        }
    }

    /// Size of a pushed value.
    fn ssize(&self) -> usize {
        if self.opsize {
            2
        } else if self.c.long_mode {
            8
        } else {
            4
        }
    }

    fn addr_mask(&self) -> u64 {
        mask(self.asize())
    }

    // ── registers ───────────────────────────────────────────────────────────

    fn get_reg(&self, r: u8, size: usize) -> u64 {
        let r = r as usize;
        match size {
            1 if self.rex == 0 && (4..8).contains(&r) => (self.c.regs[r - 4] >> 8) & 0xFF,
            _ => self.c.regs[r] & mask(size),
        }
    }

    fn set_reg(&mut self, r: u8, size: usize, v: u64) {
        let r = r as usize;
        let slot = match size {
            1 if self.rex == 0 && (4..8).contains(&r) => {
                let s = &mut self.c.regs[r - 4];
                *s = (*s & !0xFF00) | ((v & 0xFF) << 8);
                return;
            }
            _ => &mut self.c.regs[r],
        };
        *slot = match size {
            1 => (*slot & !0xFF) | (v & 0xFF),
            2 => (*slot & !0xFFFF) | (v & 0xFFFF),
            4 => v & 0xFFFF_FFFF,
            _ => v,
        };
    }

    /// Write an address-sized register (string counters and pointers).
    fn set_areg(&mut self, r: u8, v: u64) {
        let size = self.asize();
        self.set_reg(r, size, v);
    }

    fn sp(&self) -> u64 {
        self.c.regs[RSP as usize]
            & if self.c.long_mode {
                u64::MAX
            } else {
                0xFFFF_FFFF
            }
    }

    fn set_sp(&mut self, v: u64) {
        self.c.regs[RSP as usize] = if self.c.long_mode { v } else { v & 0xFFFF_FFFF };
    }

    // ── flags ───────────────────────────────────────────────────────────────

    fn flag(&self, f: u64) -> bool {
        self.c.rflags & f != 0
    }

    fn set_flag(&mut self, f: u64, on: bool) {
        if on {
            self.c.rflags |= f;
        } else {
            self.c.rflags &= !f;
        }
    }

    fn szp(&mut self, r: u64, size: usize) {
        let r = r & mask(size);
        self.set_flag(ZF, r == 0);
        self.set_flag(SF, r & sign(size) != 0);
        self.set_flag(PF, (r as u8).count_ones().is_multiple_of(2));
    }

    fn cond(&self, cc: u8) -> bool {
        let r = match cc >> 1 {
            0 => self.flag(OF),
            1 => self.flag(CF),
            2 => self.flag(ZF),
            3 => self.flag(CF) || self.flag(ZF),
            4 => self.flag(SF),
            5 => self.flag(PF),
            6 => self.flag(SF) != self.flag(OF),
            _ => self.flag(ZF) || (self.flag(SF) != self.flag(OF)),
        };
        r ^ (cc & 1 == 1)
    }

    // ── memory and operands ─────────────────────────────────────────────────

    fn modrm(&mut self) -> R<(u8, Rm)> {
        let m = self.u8()?;
        let md = m >> 6;
        let reg = ((m >> 3) & 7) | ((self.rex & 4) << 1);
        let rm = m & 7;
        if md == 3 {
            return Ok((reg, Rm::Reg(rm | ((self.rex & 1) << 3))));
        }
        if self.asize() == 2 {
            return Err(self.unsupported("16-bit addressing"));
        }
        let mut disp: u64 = 0;
        let mut rip_rel = false;
        if rm == 4 {
            let sib = self.u8()?;
            let scale = sib >> 6;
            let idx = ((sib >> 3) & 7) | ((self.rex & 2) << 2);
            let base = sib & 7;
            if idx != 4 {
                disp = disp.wrapping_add(self.c.regs[idx as usize] << scale);
            }
            if base == 5 && md == 0 {
                disp = disp.wrapping_add(self.i32()?);
            } else {
                disp = disp.wrapping_add(self.c.regs[(base | ((self.rex & 1) << 3)) as usize]);
            }
        } else if rm == 5 && md == 0 {
            disp = self.i32()?;
            rip_rel = self.c.long_mode;
        } else {
            disp = self.c.regs[(rm | ((self.rex & 1) << 3)) as usize];
        }
        match md {
            1 => disp = disp.wrapping_add(self.i8()?),
            2 => disp = disp.wrapping_add(self.i32()?),
            _ => {}
        }
        Ok((reg, Rm::Mem { disp, rip_rel }))
    }

    /// Final linear address. Call only after every immediate has been read,
    /// because RIP-relative addresses are relative to the next instruction.
    fn ea(&self, rm: Rm) -> u64 {
        let Rm::Mem { disp, rip_rel } = rm else {
            unreachable!("ea of a register operand")
        };
        let mut a = disp;
        if rip_rel {
            a = a.wrapping_add(self.next());
        }
        a &= self.addr_mask();
        a = a.wrapping_add(self.seg);
        if self.c.long_mode {
            a
        } else {
            a & 0xFFFF_FFFF
        }
    }

    fn read_mem(&self, addr: u64, size: usize) -> R<u64> {
        let mut b = [0u8; 8];
        self.mem
            .read(addr, &mut b[..size])
            .map_err(|e| self.mem_fault(e, Access::Read))?;
        Ok(u64::from_le_bytes(b))
    }

    fn write_mem(&mut self, addr: u64, size: usize, v: u64) -> R<()> {
        let b = v.to_le_bytes();
        self.mem
            .write(addr, &b[..size])
            .map_err(|e| self.mem_fault(e, Access::Write))
    }

    fn read_bytes(&self, addr: u64, buf: &mut [u8]) -> R<()> {
        self.mem
            .read(addr, buf)
            .map_err(|e| self.mem_fault(e, Access::Read))
    }

    fn write_bytes(&mut self, addr: u64, buf: &[u8]) -> R<()> {
        self.mem
            .write(addr, buf)
            .map_err(|e| self.mem_fault(e, Access::Write))
    }

    fn get_rm(&self, rm: Rm, size: usize) -> R<u64> {
        match rm {
            Rm::Reg(r) => Ok(self.get_reg(r, size)),
            Rm::Mem { .. } => self.read_mem(self.ea(rm), size),
        }
    }

    fn set_rm(&mut self, rm: Rm, size: usize, v: u64) -> R<()> {
        match rm {
            Rm::Reg(r) => {
                self.set_reg(r, size, v);
                Ok(())
            }
            Rm::Mem { .. } => {
                let a = self.ea(rm);
                self.write_mem(a, size, v)
            }
        }
    }

    fn push(&mut self, v: u64, size: usize) -> R<()> {
        let sp = self.sp().wrapping_sub(size as u64);
        self.write_mem(sp, size, v)?;
        self.set_sp(sp);
        Ok(())
    }

    fn pop(&mut self, size: usize) -> R<u64> {
        let sp = self.sp();
        let v = self.read_mem(sp, size)?;
        self.set_sp(sp.wrapping_add(size as u64));
        Ok(v)
    }

    // ── arithmetic ──────────────────────────────────────────────────────────

    /// ALU operation `op` (add, or, adc, sbb, and, sub, xor, cmp); sets flags.
    fn alu(&mut self, op: u8, a: u64, b: u64, size: usize) -> u64 {
        let m = mask(size);
        let (a, b) = (a & m, b & m);
        let s = sign(size);
        match op {
            0 | 2 => {
                let cin = (op == 2 && self.flag(CF)) as u64;
                let r = a.wrapping_add(b).wrapping_add(cin) & m;
                self.set_flag(CF, a as u128 + b as u128 + cin as u128 > m as u128);
                self.set_flag(OF, (a ^ r) & (b ^ r) & s != 0);
                self.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
                self.szp(r, size);
                r
            }
            3 | 5 | 7 => {
                let bin = (op == 3 && self.flag(CF)) as u64;
                let r = a.wrapping_sub(b).wrapping_sub(bin) & m;
                self.set_flag(CF, (a as u128) < b as u128 + bin as u128);
                self.set_flag(OF, (a ^ b) & (a ^ r) & s != 0);
                self.set_flag(AF, (a ^ b ^ r) & 0x10 != 0);
                self.szp(r, size);
                r
            }
            _ => {
                let r = match op {
                    1 => a | b,
                    4 => a & b,
                    _ => a ^ b,
                };
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.szp(r, size);
                r
            }
        }
    }

    /// `inc` (dec = false) or `dec`: like add/sub 1 but CF is preserved.
    fn incdec(&mut self, a: u64, dec: bool, size: usize) -> u64 {
        let cf = self.flag(CF);
        let r = self.alu(if dec { 5 } else { 0 }, a, 1, size);
        self.set_flag(CF, cf);
        r
    }

    /// Shift/rotate group: rol, ror, rcl, rcr, shl, shr, sal, sar.
    fn shift(&mut self, kind: u8, a: u64, count: u64, size: usize) -> u64 {
        let bits = (size * 8) as u32;
        let m = mask(size);
        let a = a & m;
        let c = (count & if size == 8 { 0x3F } else { 0x1F }) as u32;
        if c == 0 {
            return a;
        }
        let msb = |v: u64| v & sign(size) != 0;
        match kind {
            0 | 1 => {
                let n = c % bits;
                let r = if kind == 0 {
                    ((a << n) | a.checked_shr(bits - n).unwrap_or(0)) & m
                } else {
                    ((a >> n) | a.checked_shl(bits - n).unwrap_or(0)) & m
                };
                if kind == 0 {
                    self.set_flag(CF, r & 1 != 0);
                    self.set_flag(OF, msb(r) ^ (r & 1 != 0));
                } else {
                    self.set_flag(CF, msb(r));
                    self.set_flag(OF, msb(r) ^ msb(r << 1));
                }
                r
            }
            2 | 3 => {
                // Rotate through carry, one bit at a time (counts are small).
                let n = c % (bits + 1);
                let mut r = a;
                let mut cf = self.flag(CF);
                for _ in 0..n {
                    if kind == 2 {
                        let out = msb(r);
                        r = ((r << 1) | cf as u64) & m;
                        cf = out;
                    } else {
                        let out = r & 1 != 0;
                        r = (r >> 1) | if cf { sign(size) } else { 0 };
                        cf = out;
                    }
                }
                self.set_flag(CF, cf);
                if kind == 2 {
                    self.set_flag(OF, msb(r) ^ cf);
                } else {
                    self.set_flag(OF, msb(r) ^ msb(r << 1));
                }
                r
            }
            4 | 6 => {
                let r = a.checked_shl(c).unwrap_or(0) & m;
                self.set_flag(CF, c <= bits && (a >> (bits - c)) & 1 != 0);
                self.set_flag(OF, msb(r) ^ self.flag(CF));
                self.set_flag(AF, false);
                self.szp(r, size);
                r
            }
            5 => {
                let r = a.checked_shr(c).unwrap_or(0);
                self.set_flag(CF, a.checked_shr(c - 1).unwrap_or(0) & 1 != 0);
                self.set_flag(OF, msb(a));
                self.set_flag(AF, false);
                self.szp(r, size);
                r
            }
            _ => {
                let s = sx(a, size);
                let r = (s >> c.min(63)) as u64 & m;
                self.set_flag(CF, (s >> (c - 1).min(63)) & 1 != 0);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                self.szp(r, size);
                r
            }
        }
    }

    /// Signed multiply truncated to `size`; CF/OF report overflow.
    fn imul(&mut self, a: u64, b: u64, size: usize) -> u64 {
        let full = sx(a, size) as i128 * sx(b, size) as i128;
        let r = full as u64 & mask(size);
        let over = sx(r, size) as i128 != full;
        self.set_flag(CF, over);
        self.set_flag(OF, over);
        self.szp(r, size);
        r
    }

    /// Accumulator halves for one-operand mul/div: (low, high) registers.
    fn acc_pair(&self, size: usize) -> (u64, u64) {
        if size == 1 {
            let ax = self.get_reg(RAX, 2);
            (ax & 0xFF, ax >> 8)
        } else {
            (self.get_reg(RAX, size), self.get_reg(RDX, size))
        }
    }

    fn set_acc_pair(&mut self, size: usize, lo: u64, hi: u64) {
        if size == 1 {
            self.set_reg(RAX, 2, (lo & 0xFF) | ((hi & 0xFF) << 8));
        } else {
            self.set_reg(RAX, size, lo);
            self.set_reg(RDX, size, hi);
        }
    }

    fn group3(&mut self, sub: u8, rm: Rm, size: usize) -> R<()> {
        let bits = size * 8;
        let m = mask(size);
        match sub {
            0 | 1 => {
                let imm = self.imm(size)?;
                let a = self.get_rm(rm, size)?;
                self.alu(4, a, imm, size);
            }
            2 => {
                let a = self.get_rm(rm, size)?;
                self.set_rm(rm, size, !a & m)?;
            }
            3 => {
                let a = self.get_rm(rm, size)?;
                let r = self.alu(5, 0, a, size);
                self.set_rm(rm, size, r)?;
            }
            4 | 5 => {
                let b = self.get_rm(rm, size)?;
                let (a, _) = self.acc_pair(size);
                let (lo, hi, over) = if sub == 4 {
                    let p = a as u128 * b as u128;
                    let hi = (p >> bits) as u64 & m;
                    (p as u64 & m, hi, hi != 0)
                } else {
                    let p = sx(a, size) as i128 * sx(b, size) as i128;
                    let lo = p as u64 & m;
                    (lo, (p >> bits) as u64 & m, sx(lo, size) as i128 != p)
                };
                self.set_acc_pair(size, lo, hi);
                self.set_flag(CF, over);
                self.set_flag(OF, over);
                self.szp(lo, size);
            }
            _ => {
                let d = self.get_rm(rm, size)?;
                if d == 0 {
                    return Err(GuestFault::DivideError { pc: self.pc });
                }
                let (lo, hi) = self.acc_pair(size);
                let n = ((hi as u128) << bits) | lo as u128;
                if sub == 6 {
                    let q = n / d as u128;
                    if q > m as u128 {
                        return Err(GuestFault::DivideError { pc: self.pc });
                    }
                    self.set_acc_pair(size, q as u64, (n % d as u128) as u64);
                } else {
                    // Sign-extend the 2*size-bit dividend.
                    let shift = 128 - 2 * bits as u32;
                    let n = ((n << shift) as i128) >> shift;
                    let d = sx(d, size) as i128;
                    let (q, r) = match (n.checked_div(d), n.checked_rem(d)) {
                        (Some(q), Some(r)) => (q, r),
                        _ => return Err(GuestFault::DivideError { pc: self.pc }),
                    };
                    if q != sx(q as u64 & m, size) as i128 {
                        return Err(GuestFault::DivideError { pc: self.pc });
                    }
                    self.set_acc_pair(size, q as u64 & m, r as u64 & m);
                }
            }
        }
        Ok(())
    }

    // ── string instructions ─────────────────────────────────────────────────

    fn string_op(&mut self, op: u8) -> R<()> {
        let size = if op & 1 == 0 { 1 } else { self.osize() };
        let am = self.addr_mask();
        let step = if self.flag(DF) {
            (size as u64).wrapping_neg()
        } else {
            size as u64
        };
        let rep = self.rep != 0;
        let src_seg = self.seg;
        let lin = |a: u64, seg: u64, long: bool| {
            let a = (a & am).wrapping_add(seg);
            if long {
                a
            } else {
                a & 0xFFFF_FFFF
            }
        };
        let long = self.c.long_mode;
        loop {
            if rep && self.c.regs[RCX as usize] & am == 0 {
                break;
            }
            let si = self.c.regs[RSI as usize];
            let di = self.c.regs[RDI as usize];
            match op {
                0xA4 | 0xA5 => {
                    let v = self.read_mem(lin(si, src_seg, long), size)?;
                    self.write_mem(lin(di, 0, long), size, v)?;
                    self.set_areg(RSI, si.wrapping_add(step));
                    self.set_areg(RDI, di.wrapping_add(step));
                }
                0xAA | 0xAB => {
                    let v = self.get_reg(RAX, size);
                    self.write_mem(lin(di, 0, long), size, v)?;
                    self.set_areg(RDI, di.wrapping_add(step));
                }
                0xAC | 0xAD => {
                    let v = self.read_mem(lin(si, src_seg, long), size)?;
                    self.set_reg(RAX, size, v);
                    self.set_areg(RSI, si.wrapping_add(step));
                }
                0xA6 | 0xA7 => {
                    let a = self.read_mem(lin(si, src_seg, long), size)?;
                    let b = self.read_mem(lin(di, 0, long), size)?;
                    self.alu(7, a, b, size);
                    self.set_areg(RSI, si.wrapping_add(step));
                    self.set_areg(RDI, di.wrapping_add(step));
                }
                _ => {
                    let a = self.get_reg(RAX, size);
                    let b = self.read_mem(lin(di, 0, long), size)?;
                    self.alu(7, a, b, size);
                    self.set_areg(RDI, di.wrapping_add(step));
                }
            }
            if !rep {
                break;
            }
            let cx = self.c.regs[RCX as usize].wrapping_sub(1);
            self.set_areg(RCX, cx);
            if matches!(op, 0xA6 | 0xA7 | 0xAE | 0xAF) {
                let zf = self.flag(ZF);
                if (self.rep == 0xF3 && !zf) || (self.rep == 0xF2 && zf) {
                    break;
                }
            }
        }
        Ok(())
    }

    // ── main dispatch ───────────────────────────────────────────────────────

    fn exec(&mut self) -> R<Flow> {
        // Legacy prefixes, then REX (64-bit mode only, must directly precede the opcode).
        let mut op = loop {
            let b = self.u8()?;
            match b {
                0x66 => self.opsize = true,
                0x67 => self.addrsize = true,
                0xF2 | 0xF3 => self.rep = b,
                0xF0 | 0x2E | 0x36 | 0x3E | 0x26 => {}
                0x64 => self.seg = self.c.fs_base,
                0x65 => self.seg = self.c.gs_base,
                _ => break b,
            }
        };
        if self.c.long_mode && (0x40..0x50).contains(&op) {
            self.rex = op;
            op = self.u8()?;
        }

        match op {
            // ALU r/m, reg / reg, r/m / acc, imm
            0x00..=0x3F if op & 7 < 6 => {
                let alu = op >> 3;
                let size = if op & 1 == 0 { 1 } else { self.osize() };
                match op & 7 {
                    0 | 1 => {
                        let (reg, rm) = self.modrm()?;
                        let a = self.get_rm(rm, size)?;
                        let b = self.get_reg(reg, size);
                        let r = self.alu(alu, a, b, size);
                        if alu != 7 {
                            self.set_rm(rm, size, r)?;
                        }
                    }
                    2 | 3 => {
                        let (reg, rm) = self.modrm()?;
                        let b = self.get_rm(rm, size)?;
                        let a = self.get_reg(reg, size);
                        let r = self.alu(alu, a, b, size);
                        if alu != 7 {
                            self.set_reg(reg, size, r);
                        }
                    }
                    _ => {
                        let b = self.imm(size)?;
                        let a = self.get_reg(RAX, size);
                        let r = self.alu(alu, a, b, size);
                        if alu != 7 {
                            self.set_reg(RAX, size, r);
                        }
                    }
                }
            }
            0x0F => return self.exec_0f(),
            0x40..=0x4F => {
                // inc/dec r (32-bit mode; in 64-bit mode these bytes are REX).
                let size = self.osize();
                let r = op & 7;
                let v = self.incdec(self.get_reg(r, size), op >= 0x48, size);
                self.set_reg(r, size, v);
            }
            0x50..=0x57 => {
                let size = self.ssize();
                let v = self.get_reg((op & 7) | ((self.rex & 1) << 3), size);
                self.push(v, size)?;
            }
            0x58..=0x5F => {
                let size = self.ssize();
                let v = self.pop(size)?;
                self.set_reg((op & 7) | ((self.rex & 1) << 3), size, v);
            }
            0x63 if self.c.long_mode => {
                let (reg, rm) = self.modrm()?;
                let v = self.get_rm(rm, 4)?;
                let size = self.osize();
                self.set_reg(reg, size, sx(v, 4) as u64);
            }
            0x68 | 0x6A => {
                let size = self.ssize();
                let v = if op == 0x6A {
                    self.i8()?
                } else if size == 2 {
                    self.le(2)?
                } else {
                    self.i32()?
                };
                self.push(v, size)?;
            }
            0x69 | 0x6B => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let imm = if op == 0x6B {
                    self.i8()?
                } else {
                    self.imm(size)?
                };
                let a = self.get_rm(rm, size)?;
                let r = self.imul(a, imm, size);
                self.set_reg(reg, size, r);
            }
            0x70..=0x7F => {
                let rel = self.i8()?;
                if self.cond(op & 0xF) {
                    return Ok(Flow::Jump(self.next().wrapping_add(rel)));
                }
            }
            0x80..=0x83 => {
                if op == 0x82 && self.c.long_mode {
                    return Err(self.invalid());
                }
                let size = if op & 1 == 0 { 1 } else { self.osize() };
                let (sub, rm) = self.modrm()?;
                let imm = if op == 0x83 {
                    self.i8()?
                } else {
                    self.imm(size)?
                };
                let a = self.get_rm(rm, size)?;
                let r = self.alu(sub & 7, a, imm, size);
                if sub & 7 != 7 {
                    self.set_rm(rm, size, r)?;
                }
            }
            0x84 | 0x85 => {
                let size = if op == 0x84 { 1 } else { self.osize() };
                let (reg, rm) = self.modrm()?;
                let a = self.get_rm(rm, size)?;
                let b = self.get_reg(reg, size);
                self.alu(4, a, b, size);
            }
            0x86 | 0x87 => {
                let size = if op == 0x86 { 1 } else { self.osize() };
                let (reg, rm) = self.modrm()?;
                let a = self.get_rm(rm, size)?;
                let b = self.get_reg(reg, size);
                self.set_rm(rm, size, b)?;
                self.set_reg(reg, size, a);
            }
            0x88..=0x8B => {
                let size = if op & 1 == 0 { 1 } else { self.osize() };
                let (reg, rm) = self.modrm()?;
                if op < 0x8A {
                    let v = self.get_reg(reg, size);
                    self.set_rm(rm, size, v)?;
                } else {
                    let v = self.get_rm(rm, size)?;
                    self.set_reg(reg, size, v);
                }
            }
            0x8D => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let Rm::Mem { disp, rip_rel } = rm else {
                    return Err(self.invalid());
                };
                let mut a = disp;
                if rip_rel {
                    a = a.wrapping_add(self.next());
                }
                self.set_reg(reg, size, a & self.addr_mask());
            }
            0x8F => {
                let size = self.ssize();
                let (_, rm) = self.modrm()?;
                let v = self.pop(size)?;
                self.set_rm(rm, size, v)?;
            }
            0x90 if self.rex & 1 == 0 => {} // nop / pause
            0x90..=0x97 => {
                let size = self.osize();
                let r = (op & 7) | ((self.rex & 1) << 3);
                let a = self.get_reg(RAX, size);
                let b = self.get_reg(r, size);
                self.set_reg(RAX, size, b);
                self.set_reg(r, size, a);
            }
            0x98 => {
                let size = self.osize();
                let v = sx(self.get_reg(RAX, size / 2), size / 2) as u64;
                self.set_reg(RAX, size, v);
            }
            0x99 => {
                let size = self.osize();
                let neg = self.get_reg(RAX, size) & sign(size) != 0;
                self.set_reg(RDX, size, if neg { u64::MAX } else { 0 });
            }
            0x9B => {} // fwait
            0x9C => {
                let size = self.ssize();
                let v = self.c.rflags;
                self.push(v, size)?;
            }
            0x9D => {
                let size = self.ssize();
                let v = self.pop(size)?;
                self.c.rflags = (self.c.rflags & !POPF_MASK) | (v & POPF_MASK) | 2;
            }
            0x9E => {
                let ah = self.get_reg(4, 1) & (CF | PF | AF | ZF | SF);
                self.c.rflags = (self.c.rflags & !(CF | PF | AF | ZF | SF)) | ah;
            }
            0x9F => {
                let v = (self.c.rflags & 0xFF) | 2;
                self.set_reg(4, 1, v);
            }
            0xA0..=0xA3 => {
                let size = if op & 1 == 0 { 1 } else { self.osize() };
                let n = self.asize();
                let off = self.le(n)?;
                let mut a = off.wrapping_add(self.seg);
                if !self.c.long_mode {
                    a &= 0xFFFF_FFFF;
                }
                if op < 0xA2 {
                    let v = self.read_mem(a, size)?;
                    self.set_reg(RAX, size, v);
                } else {
                    let v = self.get_reg(RAX, size);
                    self.write_mem(a, size, v)?;
                }
            }
            0xA4..=0xA7 | 0xAA..=0xAF => self.string_op(op)?,
            0xA8 | 0xA9 => {
                let size = if op == 0xA8 { 1 } else { self.osize() };
                let imm = self.imm(size)?;
                let a = self.get_reg(RAX, size);
                self.alu(4, a, imm, size);
            }
            0xB0..=0xB7 => {
                let v = self.u8()? as u64;
                self.set_reg((op & 7) | ((self.rex & 1) << 3), 1, v);
            }
            0xB8..=0xBF => {
                let size = self.osize();
                let v = if size == 8 {
                    self.le(8)?
                } else {
                    self.le(size)?
                };
                self.set_reg((op & 7) | ((self.rex & 1) << 3), size, v);
            }
            0xC0 | 0xC1 | 0xD0..=0xD3 => {
                let size = if op & 1 == 0 { 1 } else { self.osize() };
                let (sub, rm) = self.modrm()?;
                let count = match op {
                    0xC0 | 0xC1 => self.u8()? as u64,
                    0xD0 | 0xD1 => 1,
                    _ => self.get_reg(RCX, 1),
                };
                let a = self.get_rm(rm, size)?;
                let r = self.shift(sub & 7, a, count, size);
                self.set_rm(rm, size, r)?;
            }
            0xC2 | 0xC3 => {
                let extra = if op == 0xC2 { self.le(2)? } else { 0 };
                let size = self.ssize();
                let target = self.pop(size)?;
                let sp = self.sp().wrapping_add(extra);
                self.set_sp(sp);
                return Ok(Flow::Jump(target));
            }
            0xC6 | 0xC7 => {
                let size = if op == 0xC6 { 1 } else { self.osize() };
                let (sub, rm) = self.modrm()?;
                if sub & 7 != 0 {
                    return Err(self.invalid());
                }
                let imm = self.imm(size)?;
                self.set_rm(rm, size, imm)?;
            }
            0xC9 => {
                let bp = self.c.regs[RBP as usize];
                self.set_sp(bp);
                let size = self.ssize();
                let v = self.pop(size)?;
                self.set_reg(RBP, size, v);
            }
            0xCC => {
                return Ok(Flow::Stop(StopReason::Breakpoint { address: self.pc }));
            }
            0xCD => {
                let vector = self.u8()?;
                return Err(GuestFault::Interrupt {
                    pc: self.pc,
                    vector,
                });
            }
            0xD8..=0xDF => return self.x87(op),
            0xE0..=0xE3 => {
                let rel = self.i8()?;
                let am = self.addr_mask();
                let taken = if op == 0xE3 {
                    self.c.regs[RCX as usize] & am == 0
                } else {
                    let cx = self.c.regs[RCX as usize].wrapping_sub(1);
                    self.set_areg(RCX, cx);
                    cx & am != 0
                        && match op {
                            0xE0 => !self.flag(ZF),
                            0xE1 => self.flag(ZF),
                            _ => true,
                        }
                };
                if taken {
                    return Ok(Flow::Jump(self.next().wrapping_add(rel)));
                }
            }
            0xE8 => {
                let rel = self.i32()?;
                let next = self.next();
                let size = self.ssize();
                self.push(next, size)?;
                return Ok(Flow::Jump(next.wrapping_add(rel)));
            }
            0xE9 => {
                let rel = self.i32()?;
                return Ok(Flow::Jump(self.next().wrapping_add(rel)));
            }
            0xEB => {
                let rel = self.i8()?;
                return Ok(Flow::Jump(self.next().wrapping_add(rel)));
            }
            0xE4..=0xE7 | 0xEC..=0xEF | 0xF4 | 0xFA | 0xFB => {
                return Err(GuestFault::Privileged { pc: self.pc })
            }
            0xF5 => {
                let cf = self.flag(CF);
                self.set_flag(CF, !cf);
            }
            0xF6 | 0xF7 => {
                let size = if op == 0xF6 { 1 } else { self.osize() };
                let (sub, rm) = self.modrm()?;
                self.group3(sub & 7, rm, size)?;
            }
            0xF8 => self.set_flag(CF, false),
            0xF9 => self.set_flag(CF, true),
            0xFC => self.set_flag(DF, false),
            0xFD => self.set_flag(DF, true),
            0xFE => {
                let (sub, rm) = self.modrm()?;
                if sub & 7 > 1 {
                    return Err(self.invalid());
                }
                let a = self.get_rm(rm, 1)?;
                let r = self.incdec(a, sub & 7 == 1, 1);
                self.set_rm(rm, 1, r)?;
            }
            0xFF => {
                let (sub, rm) = self.modrm()?;
                match sub & 7 {
                    0 | 1 => {
                        let size = self.osize();
                        let a = self.get_rm(rm, size)?;
                        let r = self.incdec(a, sub & 7 == 1, size);
                        self.set_rm(rm, size, r)?;
                    }
                    2 | 4 => {
                        let size = if self.c.long_mode { 8 } else { 4 };
                        let target = self.get_rm(rm, size)?;
                        if sub & 7 == 2 {
                            let next = self.next();
                            self.push(next, size)?;
                        }
                        return Ok(Flow::Jump(target));
                    }
                    6 => {
                        let size = self.ssize();
                        let v = self.get_rm(rm, size)?;
                        self.push(v, size)?;
                    }
                    _ => return Err(self.unsupported("far call/jmp")),
                }
            }
            _ => return Err(self.invalid()),
        }
        Ok(Flow::Next)
    }

    fn exec_0f(&mut self) -> R<Flow> {
        let op = self.u8()?;
        match op {
            0x05 | 0x34 => return Err(self.unsupported("direct system call")),
            0x0B => return Err(self.invalid()), // ud2
            0x0D | 0x18..=0x1F => {
                // prefetch, hint nops, endbr32/64
                self.modrm()?;
            }
            0x10..=0x17 | 0x28 | 0x29 | 0x54..=0x57 | 0x60..=0x7F | 0xD1..=0xFE => {
                return self.sse(op)
            }
            0x31 => {
                let t = self.tsc;
                self.set_reg(RAX, 4, t);
                self.set_reg(RDX, 4, t >> 32);
            }
            0x40..=0x4F => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let v = self.get_rm(rm, size)?;
                if self.cond(op & 0xF) {
                    self.set_reg(reg, size, v);
                } else if size == 4 {
                    // A 32-bit cmov always zero-extends its destination.
                    let keep = self.get_reg(reg, 4);
                    self.set_reg(reg, 4, keep);
                }
            }
            0x80..=0x8F => {
                let rel = self.i32()?;
                if self.cond(op & 0xF) {
                    return Ok(Flow::Jump(self.next().wrapping_add(rel)));
                }
            }
            0x90..=0x9F => {
                let (_, rm) = self.modrm()?;
                let v = self.cond(op & 0xF) as u64;
                self.set_rm(rm, 1, v)?;
            }
            0xA2 => self.cpuid(),
            0xA3 | 0xAB | 0xB3 | 0xBB | 0xBA => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let (kind, offset) = if op == 0xBA {
                    let sub = reg & 7;
                    if sub < 4 {
                        return Err(self.invalid());
                    }
                    (sub - 4, self.u8()? as u64 & (size as u64 * 8 - 1))
                } else {
                    let k = match op {
                        0xA3 => 0,
                        0xAB => 1,
                        0xB3 => 2,
                        _ => 3,
                    };
                    (k, self.get_reg(reg, size))
                };
                self.bit_op(kind, rm, offset, size, op == 0xBA)?;
            }
            0xA4 | 0xA5 | 0xAC | 0xAD => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let count = if op & 1 == 0 {
                    self.u8()? as u64
                } else {
                    self.get_reg(RCX, 1)
                };
                let dst = self.get_rm(rm, size)?;
                let src = self.get_reg(reg, size);
                let r = self.double_shift(op < 0xAC, dst, src, count, size);
                self.set_rm(rm, size, r)?;
            }
            0xAE => {
                let (sub, rm) = self.modrm()?;
                match (sub & 7, rm) {
                    (5..=7, Rm::Reg(_)) => {} // lfence, mfence, sfence
                    (0..=3, Rm::Mem { .. }) => self.fxsave_group(sub & 7, rm)?,
                    (7, Rm::Mem { .. }) => {} // clflush
                    _ => return Err(self.unsupported("xsave family")),
                }
            }
            0xAF => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let b = self.get_rm(rm, size)?;
                let a = self.get_reg(reg, size);
                let r = self.imul(a, b, size);
                self.set_reg(reg, size, r);
            }
            0xB0 | 0xB1 => {
                let size = if op == 0xB0 { 1 } else { self.osize() };
                let (reg, rm) = self.modrm()?;
                let dst = self.get_rm(rm, size)?;
                let acc = self.get_reg(RAX, size);
                self.alu(7, acc, dst, size);
                if self.flag(ZF) {
                    let src = self.get_reg(reg, size);
                    self.set_rm(rm, size, src)?;
                } else {
                    self.set_rm(rm, size, dst)?;
                    self.set_reg(RAX, size, dst);
                }
            }
            0xB6 | 0xB7 | 0xBE | 0xBF => {
                let size = self.osize();
                let from = if op & 1 == 0 { 1 } else { 2 };
                let (reg, rm) = self.modrm()?;
                let v = self.get_rm(rm, from)?;
                let v = if op >= 0xBE { sx(v, from) as u64 } else { v };
                self.set_reg(reg, size, v);
            }
            0xBC | 0xBD => {
                let size = self.osize();
                let (reg, rm) = self.modrm()?;
                let v = self.get_rm(rm, size)?;
                let bits = size as u32 * 8;
                if self.rep == 0xF3 {
                    // tzcnt / lzcnt
                    let r = if v == 0 {
                        bits
                    } else if op == 0xBC {
                        v.trailing_zeros()
                    } else {
                        v.leading_zeros() - (64 - bits)
                    };
                    self.set_flag(CF, v == 0);
                    self.set_flag(ZF, r == 0);
                    self.set_reg(reg, size, r as u64);
                } else if v == 0 {
                    self.set_flag(ZF, true);
                } else {
                    self.set_flag(ZF, false);
                    let r = if op == 0xBC {
                        v.trailing_zeros()
                    } else {
                        63 - v.leading_zeros()
                    };
                    self.set_reg(reg, size, r as u64);
                }
            }
            0xC0 | 0xC1 => {
                let size = if op == 0xC0 { 1 } else { self.osize() };
                let (reg, rm) = self.modrm()?;
                let dst = self.get_rm(rm, size)?;
                let src = self.get_reg(reg, size);
                let r = self.alu(0, dst, src, size);
                self.set_rm(rm, size, r)?;
                self.set_reg(reg, size, dst);
            }
            0xC7 => {
                let (sub, rm) = self.modrm()?;
                if sub & 7 != 1 || matches!(rm, Rm::Reg(_)) {
                    return Err(self.unsupported("0f c7 extension"));
                }
                // cmpxchg8b / cmpxchg16b
                let half = if self.rex_w() { 8 } else { 4 };
                let a = self.ea(rm);
                let lo = self.read_mem(a, half)?;
                let hi = self.read_mem(a + half as u64, half)?;
                if lo == self.get_reg(RAX, half) && hi == self.get_reg(RDX, half) {
                    let (bl, ch) = (self.get_reg(RBX, half), self.get_reg(RCX, half));
                    self.write_mem(a, half, bl)?;
                    self.write_mem(a + half as u64, half, ch)?;
                    self.set_flag(ZF, true);
                } else {
                    self.set_reg(RAX, half, lo);
                    self.set_reg(RDX, half, hi);
                    self.set_flag(ZF, false);
                }
            }
            0xC8..=0xCF => {
                let size = self.osize();
                let r = (op & 7) | ((self.rex & 1) << 3);
                let v = self.get_reg(r, size);
                let v = if size == 8 {
                    v.swap_bytes()
                } else {
                    (v as u32).swap_bytes() as u64
                };
                self.set_reg(r, size, v);
            }
            _ => return Err(self.invalid()),
        }
        Ok(Flow::Next)
    }

    fn cpuid(&mut self) {
        let leaf = self.get_reg(RAX, 4);
        let (a, b, c, d) = match leaf {
            // Highest leaf 1; vendor "AnyOSAnyOSIn" as EBX, EDX, ECX.
            0 => (1, 0x4F79_6E41, 0x6E49_534F, 0x796E_4153),
            // Family 6. EDX: FPU, TSC, CX8, CMOV, FXSR, SSE, SSE2.
            1 => (
                0x0000_0600,
                0,
                0,
                1 | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 24) | (1 << 25) | (1 << 26),
            ),
            _ => (0, 0, 0, 0),
        };
        self.set_reg(RAX, 4, a);
        self.set_reg(RBX, 4, b);
        self.set_reg(RCX, 4, c);
        self.set_reg(RDX, 4, d);
    }

    /// bt (0), bts (1), btr (2), btc (3).
    fn bit_op(&mut self, kind: u8, rm: Rm, offset: u64, size: usize, imm: bool) -> R<()> {
        let bits = size as i64 * 8;
        let (rm, bit) = match rm {
            Rm::Reg(_) => (rm, offset & (bits as u64 - 1)),
            Rm::Mem { disp, rip_rel } if !imm => {
                // A register bit offset can address beyond the operand.
                let off = sx(offset, size);
                let delta = off.div_euclid(bits) * size as i64;
                (
                    Rm::Mem {
                        disp: disp.wrapping_add(delta as u64),
                        rip_rel,
                    },
                    off.rem_euclid(bits) as u64,
                )
            }
            Rm::Mem { .. } => (rm, offset),
        };
        let v = self.get_rm(rm, size)?;
        self.set_flag(CF, (v >> bit) & 1 != 0);
        let r = match kind {
            1 => v | (1 << bit),
            2 => v & !(1 << bit),
            3 => v ^ (1 << bit),
            _ => return Ok(()),
        };
        self.set_rm(rm, size, r)
    }

    /// shld (left = true) / shrd.
    fn double_shift(&mut self, left: bool, dst: u64, src: u64, count: u64, size: usize) -> u64 {
        let bits = size as u32 * 8;
        let m = mask(size);
        let c = (count & if size == 8 { 0x3F } else { 0x1F }) as u32;
        if c == 0 {
            return dst;
        }
        let c = c.min(bits);
        let (d, s) = (dst as u128 & m as u128, src as u128 & m as u128);
        let (r, cf) = if left {
            let r = ((d << c) | (s >> (bits - c))) as u64 & m;
            (r, (d >> (bits - c)) & 1 != 0)
        } else {
            let r = ((d >> c) | (s << (bits - c))) as u64 & m;
            (r, (d >> (c - 1)) & 1 != 0)
        };
        self.set_flag(CF, cf);
        self.set_flag(OF, (r ^ dst) & sign(size) != 0);
        self.szp(r, size);
        r
    }

    // ── SSE (moves and bitwise operations only) ─────────────────────────────

    fn get_x(&self, rm: Rm, n: usize) -> R<u128> {
        match rm {
            Rm::Reg(r) => Ok(self.c.xmm[r as usize] & xmask(n)),
            Rm::Mem { .. } => {
                let mut b = [0u8; 16];
                self.read_bytes(self.ea(rm), &mut b[..n])?;
                Ok(u128::from_le_bytes(b))
            }
        }
    }

    /// Store the low `n` bytes of `v`; a register destination merges.
    fn set_x(&mut self, rm: Rm, n: usize, v: u128) -> R<()> {
        match rm {
            Rm::Reg(r) => {
                let x = &mut self.c.xmm[r as usize];
                *x = (*x & !xmask(n)) | (v & xmask(n));
                Ok(())
            }
            Rm::Mem { .. } => {
                let a = self.ea(rm);
                self.write_bytes(a, &v.to_le_bytes()[..n])
            }
        }
    }

    fn sse(&mut self, op: u8) -> R<Flow> {
        // Mandatory prefix: F3 and F2 win over 66.
        let pfx = match self.rep {
            0xF3 => 0xF3,
            0xF2 => 0xF2,
            _ if self.opsize => 0x66,
            _ => 0,
        };
        let (reg, rm) = self.modrm()?;
        let x = reg as usize;
        let mem = matches!(rm, Rm::Mem { .. });
        match (pfx, op) {
            (0 | 0x66, 0x10 | 0x28) | (0x66 | 0xF3, 0x6F) => {
                self.c.xmm[x] = self.get_x(rm, 16)?;
            }
            (0 | 0x66, 0x11 | 0x29) | (0x66 | 0xF3, 0x7F) => {
                let v = self.c.xmm[x];
                self.set_x(rm, 16, v)?;
            }
            (0xF3 | 0xF2, 0x10) => {
                let n = if pfx == 0xF3 { 4 } else { 8 };
                let v = self.get_x(rm, n)?;
                if mem {
                    self.c.xmm[x] = v;
                } else {
                    self.set_x(Rm::Reg(reg), n, v)?;
                }
            }
            (0xF3 | 0xF2, 0x11) => {
                let n = if pfx == 0xF3 { 4 } else { 8 };
                let v = self.c.xmm[x];
                self.set_x(rm, n, v)?;
            }
            (0 | 0x66, 0x12 | 0x16) if mem => {
                // movlps/movlpd, movhps/movhpd loads
                let v = self.get_x(rm, 8)?;
                let cur = self.c.xmm[x];
                self.c.xmm[x] = if op == 0x12 {
                    (cur & !xmask(8)) | v
                } else {
                    (cur & xmask(8)) | (v << 64)
                };
            }
            (0 | 0x66, 0x13 | 0x17) if mem => {
                let cur = self.c.xmm[x];
                let v = if op == 0x13 { cur } else { cur >> 64 };
                self.set_x(rm, 8, v)?;
            }
            (0 | 0x66, 0x54..=0x57) | (0x66, 0xDB | 0xDF | 0xEB | 0xEF) => {
                let b = self.get_x(rm, 16)?;
                let a = self.c.xmm[x];
                self.c.xmm[x] = match op {
                    0x54 | 0xDB => a & b,
                    0x55 | 0xDF => !a & b,
                    0x56 | 0xEB => a | b,
                    _ => a ^ b,
                };
            }
            (0x66 | 0xF2 | 0xF3, 0x70) => {
                let imm = self.u8()?;
                let src = self.get_x(rm, 16)?;
                self.c.xmm[x] = match pfx {
                    0x66 => sse::shuffle(src, imm, 4, None),
                    0xF2 => sse::shuffle(src, imm, 2, Some(false)),
                    _ => sse::shuffle(src, imm, 2, Some(true)),
                };
            }
            (0x66, 0x71..=0x73) => {
                let Rm::Reg(r) = rm else {
                    return Err(self.invalid());
                };
                let imm = self.u8()? as u64;
                let v = self.c.xmm[r as usize];
                let w = match op {
                    0x71 => 2,
                    0x72 => 4,
                    _ => 8,
                };
                self.c.xmm[r as usize] = match (op, reg & 7) {
                    (_, 2) => sse::shift(v, w, 0, imm),
                    (0x71 | 0x72, 4) => sse::shift(v, w, 1, imm),
                    (_, 6) => sse::shift(v, w, 2, imm),
                    (0x73, 3) => v.checked_shr(imm as u32 * 8).unwrap_or(0),
                    (0x73, 7) => v.checked_shl(imm as u32 * 8).unwrap_or(0),
                    _ => return Err(self.invalid()),
                };
            }
            (0x66, 0x6E) => {
                let n = if self.rex_w() { 8 } else { 4 };
                self.c.xmm[x] = self.get_rm(rm, n)? as u128;
            }
            (0x66, 0x7E) => {
                let n = if self.rex_w() { 8 } else { 4 };
                let v = self.c.xmm[x] as u64;
                self.set_rm(rm, n, v)?;
            }
            (0xF3, 0x7E) => {
                self.c.xmm[x] = self.get_x(rm, 8)?;
            }
            (0x66, 0xD6) => {
                let v = self.c.xmm[x] & xmask(8);
                match rm {
                    Rm::Reg(r) => self.c.xmm[r as usize] = v,
                    _ => self.set_x(rm, 8, v)?,
                }
            }
            (0x66, 0xD7) => {
                let Rm::Reg(r) = rm else {
                    return Err(self.invalid());
                };
                let bytes = self.c.xmm[r as usize].to_le_bytes();
                let m = bytes
                    .iter()
                    .enumerate()
                    .fold(0u64, |m, (k, b)| m | (((b >> 7) as u64) << k));
                self.set_reg(reg, 4, m);
            }
            (0x66, _) => {
                let src = self.get_x(rm, 16)?;
                match sse::binary(op, self.c.xmm[x], src) {
                    Some(r) => self.c.xmm[x] = r,
                    None => return Err(self.unsupported("SSE instruction")),
                }
            }
            _ => return Err(self.unsupported("SSE instruction")),
        }
        Ok(Flow::Next)
    }
}

fn xmask(n: usize) -> u128 {
    if n >= 16 {
        u128::MAX
    } else {
        (1u128 << (n * 8)) - 1
    }
}
