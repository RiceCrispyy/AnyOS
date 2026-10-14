//! The x87 floating-point unit (`D8`-`DF`) and the MXCSR / FXSAVE family.
//!
//! Registers hold real 80-bit values, so loads and stores of `tbyte`, `qword`
//! integers and doubles round-trip exactly; that is how compilers copy
//! `Extended` and `Int64` values around. Arithmetic is done in `f64`, which is
//! what Windows' default 53-bit precision control gives anyway. Exceptions are
//! treated as masked: results become NaN or infinity and nothing traps.

use anyos_backend::F80;

use super::{Flow, Insn, Rm, CF, PF, R, RAX, ZF};

const C0: u16 = 1 << 8;
const C1: u16 = 1 << 9;
const C2: u16 = 1 << 10;
const C3: u16 = 1 << 14;
const CC: u16 = C0 | C1 | C2 | C3;
/// Zero-divide and invalid-operation status flags.
const ZE: u16 = 1 << 2;
const IE: u16 = 1 << 0;

const BIAS: i32 = 16383;

// ── conversions ─────────────────────────────────────────────────────────────

pub(crate) fn f80_from_f64(x: f64) -> F80 {
    let bits = x.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp = ((bits >> 52) & 0x7FF) as i32;
    let frac = bits & ((1 << 52) - 1);
    match exp {
        0 if frac == 0 => F80 {
            mant: 0,
            sign_exp: sign,
        },
        0 => {
            let lz = frac.leading_zeros() as i32;
            F80 {
                mant: frac << lz,
                // frac * 2^-1074 == (frac << lz) * 2^(-1074 - lz)
                sign_exp: sign | (-1074 - lz + 63 + BIAS) as u16,
            }
        }
        0x7FF => F80 {
            mant: (1 << 63) | (frac << 11),
            sign_exp: sign | 0x7FFF,
        },
        _ => F80 {
            mant: (1 << 63) | (frac << 11),
            sign_exp: sign | (exp - 1023 + BIAS) as u16,
        },
    }
}

/// `x * 2^n` without overflowing the intermediate power of two.
fn ldexp(mut x: f64, mut n: i32) -> f64 {
    while n > 1000 {
        x *= 2f64.powi(1000);
        n -= 1000;
    }
    while n < -1000 {
        x *= 2f64.powi(-1000);
        n += 1000;
    }
    x * 2f64.powi(n)
}

pub(crate) fn f80_to_f64(v: F80) -> f64 {
    let neg = v.sign_exp & 0x8000 != 0;
    let e = (v.sign_exp & 0x7FFF) as i32;
    let r = if e == 0x7FFF {
        if v.mant << 1 == 0 {
            f64::INFINITY
        } else {
            f64::from_bits(0x7FF8_0000_0000_0000 | ((v.mant << 1) >> 12))
        }
    } else if v.mant == 0 {
        0.0
    } else {
        let e = e.max(1);
        // Round the 64-bit significand to 53 bits first, then scale.
        ldexp(v.mant as f64, e - BIAS - 63)
    };
    if neg {
        -r
    } else {
        r
    }
}

fn f80_from_i64(n: i64) -> F80 {
    if n == 0 {
        return F80::default();
    }
    let sign = if n < 0 { 0x8000 } else { 0 };
    let a = n.unsigned_abs();
    let lz = a.leading_zeros();
    F80 {
        mant: a << lz,
        sign_exp: sign | (BIAS + 63 - lz as i32) as u16,
    }
}

/// Round to an integer with rounding control `rc` (0 nearest-even, 1 down,
/// 2 up, 3 toward zero). `None` for NaN, infinity or a value beyond i128.
fn f80_to_int(v: F80, rc: u16) -> Option<i128> {
    let neg = v.sign_exp & 0x8000 != 0;
    let e = (v.sign_exp & 0x7FFF) as i32;
    if e == 0x7FFF {
        return None;
    }
    if v.mant == 0 {
        return Some(0);
    }
    let sh = e.max(1) - BIAS - 63;
    let mag: u128 = if sh >= 0 {
        if sh > 63 {
            return None;
        }
        (v.mant as u128) << sh
    } else {
        let n = (-sh) as u32;
        let m = v.mant as u128;
        let (q, rem, half) = if n >= 128 {
            (0, m, None)
        } else {
            (m >> n, m & ((1u128 << n) - 1), Some(1u128 << (n - 1)))
        };
        // Whether the dropped fraction is above, at, or below one half.
        let cmp = match half {
            Some(h) => rem.cmp(&h),
            None => std::cmp::Ordering::Less,
        };
        let up = rem != 0
            && match rc {
                0 => cmp.is_gt() || (cmp.is_eq() && q & 1 == 1),
                1 => neg,
                2 => !neg,
                _ => false,
            };
        q + up as u128
    };
    let mag = i128::try_from(mag).ok()?;
    Some(if neg { -mag } else { mag })
}

fn round_f64(x: f64, rc: u16) -> f64 {
    match rc {
        0 => {
            let r = x.round();
            if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
                r - x.signum()
            } else {
                r
            }
        }
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

fn read_f80(b: &[u8]) -> F80 {
    F80 {
        mant: u64::from_le_bytes(b[..8].try_into().unwrap()),
        sign_exp: u16::from_le_bytes([b[8], b[9]]),
    }
}

fn f80_bytes(v: F80) -> [u8; 10] {
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&v.mant.to_le_bytes());
    b[8..].copy_from_slice(&v.sign_exp.to_le_bytes());
    b
}

const fn k(mant: u64, sign_exp: u16) -> F80 {
    F80 { mant, sign_exp }
}

/// fld1, fldl2t, fldl2e, fldpi, fldlg2, fldln2, fldz (D9 E8..EE).
const CONSTANTS: [F80; 7] = [
    k(0x8000_0000_0000_0000, 0x3FFF),
    k(0xD49A_784B_CD1B_8AFE, 0x4000),
    k(0xB8AA_3B29_5C17_F0BC, 0x3FFF),
    k(0xC90F_DAA2_2168_C235, 0x4000),
    k(0x9A20_9A84_FBCF_F799, 0x3FFD),
    k(0xB172_17F7_D1CF_79AC, 0x3FFE),
    k(0, 0),
];

// ── register stack ──────────────────────────────────────────────────────────

impl Insn<'_> {
    fn phys(&self, i: u8) -> usize {
        ((self.c.fpu.top + i) & 7) as usize
    }

    fn st_raw(&self, i: u8) -> F80 {
        self.c.fpu.regs[self.phys(i)]
    }

    fn st(&self, i: u8) -> f64 {
        f80_to_f64(self.st_raw(i))
    }

    fn set_st_raw(&mut self, i: u8, v: F80) {
        let p = self.phys(i);
        self.c.fpu.regs[p] = v;
        self.c.fpu.empty &= !(1 << p);
    }

    fn set_st(&mut self, i: u8, v: f64) {
        self.set_st_raw(i, f80_from_f64(v));
    }

    fn fpush_raw(&mut self, v: F80) {
        self.c.fpu.top = (self.c.fpu.top + 7) & 7;
        self.set_st_raw(0, v);
    }

    fn fpush(&mut self, v: f64) {
        self.fpush_raw(f80_from_f64(v));
    }

    fn fpop(&mut self) {
        let p = self.phys(0);
        self.c.fpu.empty |= 1 << p;
        self.c.fpu.top = (self.c.fpu.top + 1) & 7;
    }

    fn rc(&self) -> u16 {
        (self.c.fpu.control >> 10) & 3
    }

    fn set_cc(&mut self, bits: u16) {
        self.c.fpu.status = (self.c.fpu.status & !CC) | bits;
    }

    fn finit(&mut self) {
        let mxcsr = self.c.fpu.mxcsr;
        self.c.fpu = anyos_backend::X87State::new();
        self.c.fpu.control = 0x37F;
        self.c.fpu.mxcsr = mxcsr;
    }

    /// Condition codes for comparing `a` with `b` (fcom family).
    fn compare_cc(a: f64, b: f64) -> u16 {
        if a.is_nan() || b.is_nan() {
            C3 | C2 | C0
        } else if a < b {
            C0
        } else if a == b {
            C3
        } else {
            0
        }
    }

    /// EFLAGS for comparing `a` with `b` (fcomi family).
    fn compare_flags(&mut self, a: f64, b: f64) {
        let (z, p, c) = if a.is_nan() || b.is_nan() {
            (true, true, true)
        } else {
            (a == b, false, a < b)
        };
        self.set_flag(ZF, z);
        self.set_flag(PF, p);
        self.set_flag(CF, c);
        self.c.rflags &= !((1 << 11) | (1 << 7) | (1 << 4)); // OF, SF, AF
    }

    /// Arithmetic group `op` (add, mul, com, comp, sub, subr, div, divr) on
    /// `a` (the destination) and `b`.
    fn farith(&mut self, op: u8, a: f64, b: f64) -> Option<f64> {
        Some(match op {
            0 => a + b,
            1 => a * b,
            2 | 3 => {
                self.set_cc(Self::compare_cc(a, b));
                return None;
            }
            4 => a - b,
            5 => b - a,
            6 => {
                if b == 0.0 && !a.is_nan() {
                    self.c.fpu.status |= if a == 0.0 { IE } else { ZE };
                }
                a / b
            }
            _ => {
                if a == 0.0 && !b.is_nan() {
                    self.c.fpu.status |= if b == 0.0 { IE } else { ZE };
                }
                b / a
            }
        })
    }

    fn fpu_env_size(&self) -> u64 {
        if self.opsize {
            14
        } else {
            28
        }
    }

    /// The full 2-bit-per-register tag word.
    fn tag_word(&self) -> u16 {
        let f = &self.c.fpu;
        let mut w = 0u16;
        for p in 0..8 {
            let tag = if f.empty & (1 << p) != 0 {
                3
            } else {
                let r = f.regs[p];
                let e = r.sign_exp & 0x7FFF;
                if e == 0 && r.mant == 0 {
                    1
                } else if e == 0x7FFF || e == 0 || r.mant >> 63 == 0 {
                    2
                } else {
                    0
                }
            };
            w |= tag << (p * 2);
        }
        w
    }

    fn store_env(&mut self, a: u64) -> R<()> {
        let words = [
            self.c.fpu.control as u32 | 0xFFFF_0000,
            self.c.fpu.status_word() as u32 | 0xFFFF_0000,
            self.tag_word() as u32 | 0xFFFF_0000,
            0,
            0,
            0,
            0,
        ];
        if self.opsize {
            for (n, w) in words.iter().enumerate() {
                self.write_mem(a + n as u64 * 2, 2, *w as u64)?;
            }
        } else {
            for (n, w) in words.iter().enumerate() {
                self.write_mem(a + n as u64 * 4, 4, *w as u64)?;
            }
        }
        Ok(())
    }

    fn load_env(&mut self, a: u64) -> R<()> {
        let w = if self.opsize { 2 } else { 4 };
        let control = self.read_mem(a, 2)? as u16;
        let status = self.read_mem(a + w, 2)? as u16;
        let tags = self.read_mem(a + 2 * w, 2)? as u16;
        let f = &mut self.c.fpu;
        f.control = control;
        f.status = status & !0x3800;
        f.top = ((status >> 11) & 7) as u8;
        f.empty = 0;
        for p in 0..8 {
            if (tags >> (p * 2)) & 3 == 3 {
                f.empty |= 1 << p;
            }
        }
        Ok(())
    }

    // ── instructions ────────────────────────────────────────────────────────

    pub(super) fn x87(&mut self, op: u8) -> R<Flow> {
        let (reg, rm) = self.modrm()?;
        let sub = reg & 7;
        match rm {
            Rm::Mem { .. } => self.x87_mem(op, sub, rm)?,
            Rm::Reg(r) => self.x87_reg(op, sub, r & 7)?,
        }
        Ok(Flow::Next)
    }

    fn x87_mem(&mut self, op: u8, sub: u8, rm: Rm) -> R<()> {
        let a = self.ea(rm);
        match (op, sub) {
            // Arithmetic with a memory operand: m32fp, m32int, m64fp, m16int.
            (0xD8 | 0xDA | 0xDC | 0xDE, _) => {
                let b = match op {
                    0xD8 => f32::from_bits(self.read_mem(a, 4)? as u32) as f64,
                    0xDA => self.read_mem(a, 4)? as u32 as i32 as f64,
                    0xDC => f64::from_bits(self.read_mem(a, 8)?),
                    _ => self.read_mem(a, 2)? as u16 as i16 as f64,
                };
                let st0 = self.st(0);
                if let Some(r) = self.farith(sub, st0, b) {
                    self.set_st(0, r);
                }
                if sub == 3 {
                    self.fpop();
                }
            }
            (0xD9, 0) => {
                let v = f32::from_bits(self.read_mem(a, 4)? as u32) as f64;
                self.fpush(v);
            }
            (0xD9, 2 | 3) => {
                let v = (self.st(0) as f32).to_bits() as u64;
                self.write_mem(a, 4, v)?;
                if sub == 3 {
                    self.fpop();
                }
            }
            (0xD9, 4) => self.load_env(a)?,
            (0xD9, 5) => self.c.fpu.control = self.read_mem(a, 2)? as u16,
            (0xD9, 6) => self.store_env(a)?,
            (0xD9, 7) => {
                let v = self.c.fpu.control as u64;
                self.write_mem(a, 2, v)?;
            }
            // Integer loads.
            (0xDB, 0) => {
                let v = self.read_mem(a, 4)? as u32 as i32 as i64;
                self.fpush_raw(f80_from_i64(v));
            }
            (0xDF, 0) => {
                let v = self.read_mem(a, 2)? as u16 as i16 as i64;
                self.fpush_raw(f80_from_i64(v));
            }
            (0xDF, 5) => {
                let v = self.read_mem(a, 8)? as i64;
                self.fpush_raw(f80_from_i64(v));
            }
            // Integer stores: fisttp (1), fist (2), fistp (3), fistp m64 (DF /7).
            (0xDB | 0xDD | 0xDF, 1) | (0xDB | 0xDF, 2 | 3) | (0xDF, 7) => {
                let size = match (op, sub) {
                    (0xDB, _) => 4,
                    (0xDD, _) | (0xDF, 7) => 8,
                    _ => 2,
                };
                let rc = if sub == 1 { 3 } else { self.rc() };
                let bits = size * 8;
                let min = -(1i128 << (bits - 1));
                let max = (1i128 << (bits - 1)) - 1;
                let v = match f80_to_int(self.st_raw(0), rc) {
                    Some(v) if (min..=max).contains(&v) => v as u64,
                    _ => {
                        self.c.fpu.status |= IE;
                        min as u64 // the "integer indefinite" value
                    }
                };
                self.write_mem(a, size, v)?;
                if sub != 2 {
                    self.fpop();
                }
            }
            (0xDB, 5) => {
                let mut b = [0u8; 10];
                self.read_bytes(a, &mut b)?;
                self.fpush_raw(read_f80(&b));
            }
            (0xDB, 7) => {
                let b = f80_bytes(self.st_raw(0));
                self.write_bytes(a, &b)?;
                self.fpop();
            }
            (0xDD, 0) => {
                let v = self.read_mem(a, 8)?;
                self.fpush_raw(f80_from_f64(f64::from_bits(v)));
            }
            (0xDD, 2 | 3) => {
                let v = self.st(0).to_bits();
                self.write_mem(a, 8, v)?;
                if sub == 3 {
                    self.fpop();
                }
            }
            (0xDD, 4) => {
                // frstor
                self.load_env(a)?;
                let base = a + self.fpu_env_size();
                for i in 0..8u8 {
                    let mut b = [0u8; 10];
                    self.read_bytes(base + i as u64 * 10, &mut b)?;
                    let p = self.phys(i);
                    self.c.fpu.regs[p] = read_f80(&b);
                }
            }
            (0xDD, 6) => {
                // fnsave
                self.store_env(a)?;
                let base = a + self.fpu_env_size();
                for i in 0..8u8 {
                    let b = f80_bytes(self.st_raw(i));
                    self.write_bytes(base + i as u64 * 10, &b)?;
                }
                self.finit();
            }
            (0xDD, 7) => {
                let v = self.c.fpu.status_word() as u64;
                self.write_mem(a, 2, v)?;
            }
            (0xDF, 4) => {
                // fbld: 18 packed BCD digits and a sign byte.
                let mut b = [0u8; 10];
                self.read_bytes(a, &mut b)?;
                let mut v: i64 = 0;
                for k in (0..9).rev() {
                    v = v * 100 + (b[k] >> 4) as i64 * 10 + (b[k] & 0xF) as i64;
                }
                if b[9] & 0x80 != 0 {
                    v = -v;
                }
                self.fpush_raw(f80_from_i64(v));
            }
            (0xDF, 6) => {
                // fbstp
                let rc = self.rc();
                let v = f80_to_int(self.st_raw(0), rc).unwrap_or(0);
                let mut mag = v.unsigned_abs();
                let mut b = [0u8; 10];
                for byte in b.iter_mut().take(9) {
                    let lo = (mag % 10) as u8;
                    mag /= 10;
                    let hi = (mag % 10) as u8;
                    mag /= 10;
                    *byte = (hi << 4) | lo;
                }
                if v < 0 {
                    b[9] = 0x80;
                }
                self.write_bytes(a, &b)?;
                self.fpop();
            }
            _ => return Err(self.invalid()),
        }
        Ok(())
    }

    fn x87_reg(&mut self, op: u8, sub: u8, i: u8) -> R<()> {
        match (op, sub) {
            // st(0) = st(0) op st(i)
            (0xD8, _) => {
                let (a, b) = (self.st(0), self.st(i));
                if let Some(r) = self.farith(sub, a, b) {
                    self.set_st(0, r);
                }
                if sub == 3 {
                    self.fpop();
                }
            }
            // st(i) = st(i) op st(0), and the popping forms.
            (0xDC | 0xDE, 0 | 1 | 4..=7) => {
                // With these opcodes sub/subr and div/divr swap meaning.
                let op2 = match sub {
                    4 => 5,
                    5 => 4,
                    6 => 7,
                    7 => 6,
                    s => s,
                };
                let (a, b) = (self.st(i), self.st(0));
                if let Some(r) = self.farith(op2, a, b) {
                    self.set_st(i, r);
                }
                if op == 0xDE {
                    self.fpop();
                }
            }
            (0xDC, 2 | 3) => {
                let (a, b) = (self.st(0), self.st(i));
                self.set_cc(Self::compare_cc(a, b));
                if sub == 3 {
                    self.fpop();
                }
            }
            (0xDE, 3) if i == 1 => {
                // fcompp
                let (a, b) = (self.st(0), self.st(1));
                self.set_cc(Self::compare_cc(a, b));
                self.fpop();
                self.fpop();
            }
            (0xD9, 0) => {
                let v = self.st_raw(i);
                self.fpush_raw(v);
            }
            (0xD9, 1) => {
                let (a, b) = (self.st_raw(0), self.st_raw(i));
                self.set_st_raw(0, b);
                self.set_st_raw(i, a);
            }
            (0xD9, 2) => {} // fnop
            (0xD9, 4) => self.x87_d9_e0(i)?,
            (0xD9, 5) => {
                if i == 7 {
                    return Err(self.invalid());
                }
                self.fpush_raw(CONSTANTS[i as usize]);
            }
            (0xD9, 6) => self.x87_d9_f0(i)?,
            (0xD9, 7) => self.x87_d9_f8(i)?,
            // fcmovcc
            (0xDA | 0xDB, 0..=3) => {
                let cond = match sub {
                    0 => self.flag(CF),
                    1 => self.flag(ZF),
                    2 => self.flag(CF) || self.flag(ZF),
                    _ => self.flag(PF),
                };
                if cond == (op == 0xDA) {
                    let v = self.st_raw(i);
                    self.set_st_raw(0, v);
                }
            }
            (0xDA, 5) if i == 1 => {
                // fucompp
                let (a, b) = (self.st(0), self.st(1));
                self.set_cc(Self::compare_cc(a, b));
                self.fpop();
                self.fpop();
            }
            (0xDB, 4) => match i {
                0 | 1 | 4 => {} // feni, fdisi, fsetpm: no-ops on 387+
                2 => self.c.fpu.status &= !0x80FF, // fnclex
                3 => self.finit(),
                _ => return Err(self.invalid()),
            },
            (0xDB | 0xDF, 5 | 6) => {
                let (a, b) = (self.st(0), self.st(i));
                self.compare_flags(a, b);
                if op == 0xDF {
                    self.fpop();
                }
            }
            (0xDD, 0) | (0xDF, 0) => {
                // ffree / ffreep
                let p = self.phys(i);
                self.c.fpu.empty |= 1 << p;
                if op == 0xDF {
                    self.fpop();
                }
            }
            (0xDD, 2 | 3) => {
                let v = self.st_raw(0);
                self.set_st_raw(i, v);
                if sub == 3 {
                    self.fpop();
                }
            }
            (0xDD, 4 | 5) => {
                let (a, b) = (self.st(0), self.st(i));
                self.set_cc(Self::compare_cc(a, b));
                if sub == 5 {
                    self.fpop();
                }
            }
            (0xDF, 4) if i == 0 => {
                let v = self.c.fpu.status_word() as u64;
                self.set_reg(RAX, 2, v);
            }
            _ => return Err(self.invalid()),
        }
        Ok(())
    }

    /// fchs, fabs, ftst, fxam.
    fn x87_d9_e0(&mut self, i: u8) -> R<()> {
        match i {
            0 => {
                let mut v = self.st_raw(0);
                v.sign_exp ^= 0x8000;
                self.set_st_raw(0, v);
            }
            1 => {
                let mut v = self.st_raw(0);
                v.sign_exp &= 0x7FFF;
                self.set_st_raw(0, v);
            }
            4 => {
                let a = self.st(0);
                self.set_cc(Self::compare_cc(a, 0.0));
            }
            5 => {
                let p = self.phys(0);
                let v = self.st_raw(0);
                let sign = if v.sign_exp & 0x8000 != 0 { C1 } else { 0 };
                let e = v.sign_exp & 0x7FFF;
                let class = if self.c.fpu.empty & (1 << p) != 0 {
                    C3 | C0
                } else if e == 0x7FFF {
                    if v.mant << 1 == 0 {
                        C2 | C0
                    } else {
                        C0
                    }
                } else if e == 0 && v.mant == 0 {
                    C3
                } else if e == 0 {
                    C3 | C2
                } else if v.mant >> 63 == 0 {
                    0
                } else {
                    C2
                };
                self.set_cc(sign | class);
            }
            _ => return Err(self.invalid()),
        }
        Ok(())
    }

    /// f2xm1, fyl2x, fptan, fpatan, fxtract, fprem1, fdecstp, fincstp.
    fn x87_d9_f0(&mut self, i: u8) -> R<()> {
        match i {
            0 => {
                let x = self.st(0);
                self.set_st(0, x.exp2() - 1.0);
            }
            1 => {
                let (x, y) = (self.st(0), self.st(1));
                self.set_st(1, y * x.log2());
                self.fpop();
            }
            2 => {
                let x = self.st(0);
                self.set_st(0, x.tan());
                self.fpush(1.0);
                self.set_cc(0);
            }
            3 => {
                let (x, y) = (self.st(0), self.st(1));
                self.set_st(1, y.atan2(x));
                self.fpop();
            }
            4 => {
                let v = self.st_raw(0);
                let e = (v.sign_exp & 0x7FFF) as i32;
                if v.mant == 0 && e == 0 {
                    self.c.fpu.status |= ZE;
                    self.set_st(0, f64::NEG_INFINITY);
                    self.fpush_raw(v);
                } else {
                    let exponent = (e.max(1) - BIAS) as f64;
                    let sig = F80 {
                        mant: v.mant,
                        sign_exp: (v.sign_exp & 0x8000) | BIAS as u16,
                    };
                    self.set_st(0, exponent);
                    self.fpush_raw(sig);
                }
            }
            5 => {
                let (a, b) = (self.st(0), self.st(1));
                let q = round_f64(a / b, 0);
                self.set_st(0, a - q * b);
                self.set_quotient_cc(q);
            }
            6 => self.c.fpu.top = (self.c.fpu.top + 7) & 7,
            _ => self.c.fpu.top = (self.c.fpu.top + 1) & 7,
        }
        Ok(())
    }

    /// The low quotient bits fprem/fprem1 report in C0, C3, C1; C2 clear
    /// (the reduction is complete).
    fn set_quotient_cc(&mut self, q: f64) {
        let q = q.abs() as u64;
        let mut cc = 0;
        if q & 1 != 0 {
            cc |= C1;
        }
        if q & 2 != 0 {
            cc |= C3;
        }
        if q & 4 != 0 {
            cc |= C0;
        }
        self.set_cc(cc);
    }

    /// fprem, fyl2xp1, fsqrt, fsincos, frndint, fscale, fsin, fcos.
    fn x87_d9_f8(&mut self, i: u8) -> R<()> {
        match i {
            0 => {
                let (a, b) = (self.st(0), self.st(1));
                let q = (a / b).trunc();
                self.set_st(0, a % b);
                self.set_quotient_cc(q);
            }
            1 => {
                let (x, y) = (self.st(0), self.st(1));
                self.set_st(1, y * x.ln_1p() / std::f64::consts::LN_2);
                self.fpop();
            }
            2 => {
                let x = self.st(0);
                if x < 0.0 {
                    self.c.fpu.status |= IE;
                }
                self.set_st(0, x.sqrt());
            }
            3 => {
                let x = self.st(0);
                self.set_st(0, x.sin());
                self.fpush(x.cos());
                self.set_cc(0);
            }
            4 => {
                let rc = self.rc();
                let v = self.st_raw(0);
                // Exact for every magnitude an integer conversion can hold.
                let r = match f80_to_int(v, rc) {
                    Some(n) if n.unsigned_abs() < 1 << 63 => {
                        let mut r = f80_from_i64(n as i64);
                        r.sign_exp |= v.sign_exp & 0x8000;
                        r
                    }
                    _ => v,
                };
                self.set_st_raw(0, r);
            }
            5 => {
                let (x, y) = (self.st(0), self.st(1));
                let n = y.trunc().clamp(-100_000.0, 100_000.0) as i32;
                self.set_st(0, ldexp(x, n));
            }
            6 => {
                let x = self.st(0);
                self.set_st(0, x.sin());
                self.set_cc(0);
            }
            _ => {
                let x = self.st(0);
                self.set_st(0, x.cos());
                self.set_cc(0);
            }
        }
        Ok(())
    }

    /// `0F AE /0..3` with a memory operand: fxsave, fxrstor, ldmxcsr, stmxcsr.
    pub(super) fn fxsave_group(&mut self, sub: u8, rm: Rm) -> R<()> {
        let a = self.ea(rm);
        match sub {
            0 => {
                let mut b = [0u8; 512];
                let f = self.c.fpu.clone();
                b[0..2].copy_from_slice(&f.control.to_le_bytes());
                b[2..4].copy_from_slice(&f.status_word().to_le_bytes());
                b[4] = !f.empty;
                b[24..28].copy_from_slice(&f.mxcsr.to_le_bytes());
                b[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes());
                for i in 0..8u8 {
                    let off = 32 + i as usize * 16;
                    b[off..off + 10].copy_from_slice(&f80_bytes(self.st_raw(i)));
                }
                let n = if self.c.long_mode { 16 } else { 8 };
                for x in 0..n {
                    let off = 160 + x * 16;
                    b[off..off + 16].copy_from_slice(&self.c.xmm[x].to_le_bytes());
                }
                self.write_bytes(a, &b)?;
            }
            1 => {
                let mut b = [0u8; 512];
                self.read_bytes(a, &mut b)?;
                let status = u16::from_le_bytes([b[2], b[3]]);
                let f = &mut self.c.fpu;
                f.control = u16::from_le_bytes([b[0], b[1]]);
                f.status = status & !0x3800;
                f.top = ((status >> 11) & 7) as u8;
                f.empty = !b[4];
                f.mxcsr = u32::from_le_bytes(b[24..28].try_into().unwrap());
                for i in 0..8u8 {
                    let off = 32 + i as usize * 16;
                    let p = self.phys(i);
                    self.c.fpu.regs[p] = read_f80(&b[off..off + 10]);
                }
                let n = if self.c.long_mode { 16 } else { 8 };
                for x in 0..n {
                    let off = 160 + x * 16;
                    self.c.xmm[x] = u128::from_le_bytes(b[off..off + 16].try_into().unwrap());
                }
            }
            2 => self.c.fpu.mxcsr = self.read_mem(a, 4)? as u32,
            _ => {
                let v = self.c.fpu.mxcsr as u64;
                self.write_mem(a, 4, v)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_round_trip_through_80_bits() {
        for x in [
            0.0,
            -0.0,
            1.0,
            -2.5,
            1e300,
            1e-300,
            5e-324,
            f64::MAX,
            f64::INFINITY,
            std::f64::consts::PI,
        ] {
            assert_eq!(f80_to_f64(f80_from_f64(x)).to_bits(), x.to_bits(), "{x}");
        }
        assert!(f80_to_f64(f80_from_f64(f64::NAN)).is_nan());
    }

    #[test]
    fn integers_convert_exactly() {
        for n in [0i64, 1, -1, 12345, i64::MAX, i64::MIN, 1 << 53 | 1] {
            assert_eq!(f80_to_int(f80_from_i64(n), 0), Some(n as i128), "{n}");
        }
        let v = f80_from_f64(2.5);
        assert_eq!(f80_to_int(v, 0), Some(2));
        assert_eq!(f80_to_int(f80_from_f64(3.5), 0), Some(4));
        assert_eq!(f80_to_int(f80_from_f64(-2.7), 3), Some(-2));
        assert_eq!(f80_to_int(f80_from_f64(-2.2), 1), Some(-3));
        assert_eq!(f80_to_int(f80_from_f64(2.2), 2), Some(3));
        assert_eq!(f80_to_f64(CONSTANTS[3]), std::f64::consts::PI);
    }
}
