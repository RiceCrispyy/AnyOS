//! SSE2 packed-integer arithmetic (`66 0F xx` with an xmm or m128 source).
//!
//! Compilers auto-vectorise ordinary integer loops into these, so even simple
//! programs reach them. Floating-point SSE is not implemented yet.

/// Apply `f` to each `w`-byte lane of `a` and `b`.
fn lanes(a: u128, b: u128, w: usize, f: impl Fn(u64, u64) -> u64) -> u128 {
    let bits = w * 8;
    let m: u128 = if bits == 64 {
        u64::MAX as u128
    } else {
        (1u128 << bits) - 1
    };
    let mut r = 0u128;
    for k in 0..16 / w {
        let sh = k * bits;
        let x = ((a >> sh) & m) as u64;
        let y = ((b >> sh) & m) as u64;
        r |= (f(x, y) as u128 & m) << sh;
    }
    r
}

fn sx(v: u64, w: usize) -> i64 {
    let s = 64 - w * 8;
    ((v << s) as i64) >> s
}

fn sat_s(v: i64, w: usize) -> u64 {
    let max = (1i64 << (w * 8 - 1)) - 1;
    v.clamp(-max - 1, max) as u64
}

fn sat_u(v: i64, w: usize) -> u64 {
    v.clamp(0, (1i64 << (w * 8)) - 1) as u64
}

/// Interleave the low (`high = false`) or high halves of `a` and `b`, `w`-byte lanes.
fn unpack(a: u128, b: u128, w: usize, high: bool) -> u128 {
    let bits = w * 8;
    let m: u128 = if bits == 64 {
        u64::MAX as u128
    } else {
        (1u128 << bits) - 1
    };
    let n = 8 / w;
    let base = if high { n } else { 0 };
    let mut r = 0u128;
    for k in 0..n {
        let x = (a >> ((base + k) * bits)) & m;
        let y = (b >> ((base + k) * bits)) & m;
        r |= x << (2 * k * bits);
        r |= y << ((2 * k + 1) * bits);
    }
    r
}

/// Narrow `w`-byte lanes of `a` then `b` to half width with saturation.
fn pack(a: u128, b: u128, w: usize, signed_out: bool) -> u128 {
    let bits = w * 8;
    let m = (1u128 << bits) - 1;
    let half = w / 2;
    let n = 16 / w;
    let mut r = 0u128;
    for (k, src) in [a, b].iter().enumerate() {
        for j in 0..n {
            let v = sx(((src >> (j * bits)) & m) as u64, w);
            let s = if signed_out {
                sat_s(v, half)
            } else {
                sat_u(v, half)
            };
            r |= ((s as u128) & ((1u128 << (half * 8)) - 1)) << ((k * n + j) * half * 8);
        }
    }
    r
}

/// Shift each `w`-byte lane: 0 = logical right, 1 = arithmetic right, 2 = left.
pub(crate) fn shift(a: u128, w: usize, kind: u8, count: u64) -> u128 {
    let bits = (w * 8) as u64;
    lanes(a, 0, w, |x, _| match kind {
        0 if count >= bits => 0,
        0 => x >> count,
        1 => sx(x, w).wrapping_shr(count.min(bits - 1) as u32) as u64,
        _ if count >= bits => 0,
        _ => x << count,
    })
}

/// The result of binary packed-integer op `op` on `dst` and `src`, if `op` is one.
pub(crate) fn binary(op: u8, d: u128, s: u128) -> Option<u128> {
    let low = |v: u128| v as u64;
    Some(match op {
        0xFC => lanes(d, s, 1, u64::wrapping_add),
        0xFD => lanes(d, s, 2, u64::wrapping_add),
        0xFE => lanes(d, s, 4, u64::wrapping_add),
        0xD4 => lanes(d, s, 8, u64::wrapping_add),
        0xF8 => lanes(d, s, 1, u64::wrapping_sub),
        0xF9 => lanes(d, s, 2, u64::wrapping_sub),
        0xFA => lanes(d, s, 4, u64::wrapping_sub),
        0xFB => lanes(d, s, 8, u64::wrapping_sub),
        0xEC => lanes(d, s, 1, |x, y| sat_s(sx(x, 1) + sx(y, 1), 1)),
        0xED => lanes(d, s, 2, |x, y| sat_s(sx(x, 2) + sx(y, 2), 2)),
        0xE8 => lanes(d, s, 1, |x, y| sat_s(sx(x, 1) - sx(y, 1), 1)),
        0xE9 => lanes(d, s, 2, |x, y| sat_s(sx(x, 2) - sx(y, 2), 2)),
        0xDC => lanes(d, s, 1, |x, y| sat_u(x as i64 + y as i64, 1)),
        0xDD => lanes(d, s, 2, |x, y| sat_u(x as i64 + y as i64, 2)),
        0xD8 => lanes(d, s, 1, |x, y| sat_u(x as i64 - y as i64, 1)),
        0xD9 => lanes(d, s, 2, |x, y| sat_u(x as i64 - y as i64, 2)),
        0xD5 => lanes(d, s, 2, |x, y| x.wrapping_mul(y)),
        0xE5 => lanes(d, s, 2, |x, y| ((sx(x, 2) * sx(y, 2)) >> 16) as u64),
        0xE4 => lanes(d, s, 2, |x, y| (x * y) >> 16),
        0xF4 => lanes(d, s, 8, |x, y| (x & 0xFFFF_FFFF) * (y & 0xFFFF_FFFF)),
        0xF5 => lanes(d, s, 4, |x, y| {
            let p = sx(x & 0xFFFF, 2) * sx(y & 0xFFFF, 2) + sx(x >> 16, 2) * sx(y >> 16, 2);
            p as u64
        }),
        0xDA => lanes(d, s, 1, u64::min),
        0xDE => lanes(d, s, 1, u64::max),
        0xEA => lanes(d, s, 2, |x, y| sx(x, 2).min(sx(y, 2)) as u64),
        0xEE => lanes(d, s, 2, |x, y| sx(x, 2).max(sx(y, 2)) as u64),
        0xE0 => lanes(d, s, 1, |x, y| (x + y + 1) >> 1),
        0xE3 => lanes(d, s, 2, |x, y| (x + y + 1) >> 1),
        0xF6 => {
            let sad = |h: usize| -> u128 {
                (0..8)
                    .map(|k| {
                        let sh = (h * 8 + k) * 8;
                        (((d >> sh) & 0xFF) as i64 - ((s >> sh) & 0xFF) as i64).unsigned_abs()
                            as u128
                    })
                    .sum()
            };
            sad(0) | (sad(1) << 64)
        }
        0x74 => lanes(d, s, 1, |x, y| if x == y { u64::MAX } else { 0 }),
        0x75 => lanes(d, s, 2, |x, y| if x == y { u64::MAX } else { 0 }),
        0x76 => lanes(d, s, 4, |x, y| if x == y { u64::MAX } else { 0 }),
        0x64 => lanes(
            d,
            s,
            1,
            |x, y| if sx(x, 1) > sx(y, 1) { u64::MAX } else { 0 },
        ),
        0x65 => lanes(
            d,
            s,
            2,
            |x, y| if sx(x, 2) > sx(y, 2) { u64::MAX } else { 0 },
        ),
        0x66 => lanes(
            d,
            s,
            4,
            |x, y| if sx(x, 4) > sx(y, 4) { u64::MAX } else { 0 },
        ),
        0x60 => unpack(d, s, 1, false),
        0x61 => unpack(d, s, 2, false),
        0x62 => unpack(d, s, 4, false),
        0x6C => unpack(d, s, 8, false),
        0x68 => unpack(d, s, 1, true),
        0x69 => unpack(d, s, 2, true),
        0x6A => unpack(d, s, 4, true),
        0x6D => unpack(d, s, 8, true),
        0x63 => pack(d, s, 2, true),
        0x6B => pack(d, s, 4, true),
        0x67 => pack(d, s, 2, false),
        0xDB => d & s,
        0xDF => !d & s,
        0xEB => d | s,
        0xEF => d ^ s,
        0xD1 => shift(d, 2, 0, low(s)),
        0xD2 => shift(d, 4, 0, low(s)),
        0xD3 => shift(d, 8, 0, low(s)),
        0xE1 => shift(d, 2, 1, low(s)),
        0xE2 => shift(d, 4, 1, low(s)),
        0xF1 => shift(d, 2, 2, low(s)),
        0xF2 => shift(d, 4, 2, low(s)),
        0xF3 => shift(d, 8, 2, low(s)),
        _ => return None,
    })
}

/// `pshufd` (w = 4, all lanes), `pshuflw` / `pshufhw` (w = 2, one half).
pub(crate) fn shuffle(src: u128, imm: u8, w: usize, half: Option<bool>) -> u128 {
    let bits = w * 8;
    let m = (1u128 << bits) - 1;
    let (base, keep) = match half {
        None => (0, 0u128),
        Some(false) => (0, src & !((1u128 << 64) - 1)),
        Some(true) => (4, src & ((1u128 << 64) - 1)),
    };
    let mut r = keep;
    for k in 0..4 {
        let sel = ((imm >> (2 * k)) & 3) as usize;
        r |= ((src >> ((base + sel) * bits)) & m) << ((base + k) * bits);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dw(v: [u32; 4]) -> u128 {
        v.iter()
            .enumerate()
            .fold(0, |r, (k, &x)| r | (x as u128) << (32 * k))
    }

    #[test]
    fn add_and_compare() {
        let a = dw([1, 2, u32::MAX, 0x8000_0000]);
        let b = dw([1, 1, 1, 0x8000_0000]);
        assert_eq!(binary(0xFE, a, b), Some(dw([2, 3, 0, 0])));
        assert_eq!(
            binary(0x66, a, b),
            Some(dw([0, u32::MAX, 0, 0])),
            "pcmpgtd is signed"
        );
    }

    #[test]
    fn unpack_pack_and_shuffle() {
        let a = dw([0x11, 0x22, 0x33, 0x44]);
        let b = dw([0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(binary(0x62, a, b), Some(dw([0x11, 0xAA, 0x22, 0xBB])));
        assert_eq!(binary(0x6A, a, b), Some(dw([0x33, 0xCC, 0x44, 0xDD])));
        assert_eq!(
            shuffle(a, 0b00_01_10_11, 4, None),
            dw([0x44, 0x33, 0x22, 0x11])
        );
        // packssdw saturates 0x7FFF_FFFF to 0x7FFF and -1 stays -1.
        let p = binary(0x6B, dw([0x7FFF_FFFF, u32::MAX, 5, 0]), 0).unwrap();
        assert_eq!(p & 0xFFFF_FFFF_FFFF_FFFF, 0x0000_0005_FFFF_7FFF);
    }

    #[test]
    fn lane_shifts() {
        let a = dw([0x8000_0000, 1, 2, 3]);
        assert_eq!(shift(a, 4, 1, 4), dw([0xF800_0000, 0, 0, 0]));
        assert_eq!(shift(a, 4, 0, 40), 0);
        assert_eq!(shift(a, 4, 2, 1), dw([0, 2, 4, 6]));
    }
}
