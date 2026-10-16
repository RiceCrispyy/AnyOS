//! Strings: code pages, the `lstr*` family, `wsprintf` and the user32
//! character helpers.
//!
//! AnyOS reports UTF-8 (code page 65001) as the ANSI and OEM code page, so
//! "A" APIs take and return UTF-8, which matches how host paths and text come
//! in. Programs that convert explicitly can also use Windows-1252, Latin-1 and
//! ASCII.

use crate::api::*;
use crate::thread::{Halt, Thread};

pub(crate) const CP_UTF8: u32 = 65001;
pub(crate) const CP_1252: u32 = 1252;
const CP_LATIN1: u32 = 28591;
const CP_ASCII: u32 = 20127;
const CP_UTF7: u32 = 65000;

/// The ANSI string bytes as text.
pub(crate) fn ansi_to_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub(crate) fn string_to_ansi(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Map `CP_ACP`, `CP_OEMCP`, `CP_MACCP` and `CP_THREAD_ACP` to a real page.
pub(crate) fn resolve_cp(cp: u32) -> u32 {
    match cp {
        0..=3 => CP_UTF8,
        other => other,
    }
}

pub(crate) fn valid_cp(cp: u32) -> bool {
    matches!(
        resolve_cp(cp),
        CP_UTF8 | CP_1252 | CP_LATIN1 | CP_ASCII | 437 | 850
    )
}

/// Windows-1252's 0x80..0x9F; the rest of the page is Latin-1. Undefined
/// bytes map to the same C1 control code point, as Windows does.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// Decode bytes in code page `cp` to UTF-16. `Err(())` when `strict` and a
/// byte sequence is invalid.
pub(crate) fn decode_cp(cp: u32, bytes: &[u8], strict: bool) -> Result<Vec<u16>, ()> {
    match resolve_cp(cp) {
        CP_1252 => Ok(bytes
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
                _ => b as u16,
            })
            .collect()),
        CP_ASCII => Ok(bytes
            .iter()
            .map(|&b| if b < 0x80 { b as u16 } else { b'?' as u16 })
            .collect()),
        CP_UTF8 => match std::str::from_utf8(bytes) {
            Ok(s) => Ok(s.encode_utf16().collect()),
            Err(_) if strict => Err(()),
            Err(_) => Ok(String::from_utf8_lossy(bytes).encode_utf16().collect()),
        },
        // Latin-1, and the OEM pages for their ASCII half.
        _ => Ok(bytes.iter().map(|&b| b as u16).collect()),
    }
}

/// Encode UTF-16 in code page `cp`. Returns the bytes and whether a default
/// character had to stand in for something.
pub(crate) fn encode_cp(cp: u32, units: &[u16], default: u8) -> (Vec<u8>, bool) {
    let cp = resolve_cp(cp);
    if cp == CP_UTF8 || cp == CP_UTF7 {
        return (String::from_utf16_lossy(units).into_bytes(), false);
    }
    let mut used = false;
    let out = units
        .iter()
        .map(|&u| {
            let b = match cp {
                CP_1252 => match u {
                    0..=0x7F | 0xA0..=0xFF => Some(u as u8),
                    _ => CP1252_HIGH
                        .iter()
                        .position(|&c| c == u)
                        .map(|i| 0x80 + i as u8),
                },
                CP_ASCII => (u < 0x80).then_some(u as u8),
                _ => (u < 0x100).then_some(u as u8),
            };
            b.unwrap_or_else(|| {
                used = true;
                default
            })
        })
        .collect();
    (out, used)
}

/// Read a counted (`len >= 0`) or NUL-terminated (`len == -1`) byte string.
pub(crate) fn read_counted_bytes(t: &Thread<'_, '_>, addr: u64, len: i32) -> Option<Vec<u8>> {
    if len < 0 {
        let mut b = t.process.memory.read_cstr(addr, 1 << 24).ok()?;
        b.push(0);
        Some(b)
    } else {
        guest_bytes(t, addr, len as u64)
    }
}

/// Read a counted or NUL-terminated (`len == -1`, terminator included) UTF-16 string.
pub(crate) fn read_counted_units(t: &Thread<'_, '_>, addr: u64, len: i32) -> Option<Vec<u16>> {
    if len < 0 {
        let mut u = read_wide_units(t, addr)?;
        u.push(0);
        Some(u)
    } else {
        let raw = guest_bytes(t, addr, len as u64 * 2)?;
        Some(
            raw.as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect(),
        )
    }
}

fn units_to_bytes(u: &[u16]) -> Vec<u8> {
    u.iter().flat_map(|c| c.to_le_bytes()).collect()
}

const MB_ERR_INVALID_CHARS: u64 = 0x8;
const ERROR_NO_UNICODE_TRANSLATION: u32 = 1113;

fn multi_byte_to_wide_char(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (cp, flags, src, len, dst, cap) = (a[0] as u32, a[1], a[2], a[3] as i32, a[4], a[5] as i32);
    if src == 0 || len == 0 || cap < 0 || !valid_cp(cp) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Some(bytes) = read_counted_bytes(t, src, len) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let Ok(units) = decode_cp(cp, &bytes, flags & MB_ERR_INVALID_CHARS != 0) else {
        return fail(t, ERROR_NO_UNICODE_TRANSLATION, 0);
    };
    if cap == 0 {
        return Ok(units.len() as u64);
    }
    if units.len() > cap as usize {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, dst, &units_to_bytes(&units)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(units.len() as u64)
}

fn wide_char_to_multi_byte(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (cp, src, len, dst, cap, default, used_ptr) = (
        a[0] as u32,
        a[2],
        a[3] as i32,
        a[4],
        a[5] as i32,
        a[6],
        a[7],
    );
    if src == 0 || len == 0 || cap < 0 || !valid_cp(cp) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let Some(units) = read_counted_units(t, src, len) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let def = if default != 0 {
        guest_bytes(t, default, 1).map(|b| b[0]).unwrap_or(b'?')
    } else {
        b'?'
    };
    let (bytes, used) = encode_cp(cp, &units, def);
    if used_ptr != 0 && resolve_cp(cp) != CP_UTF8 {
        write_u32(t, used_ptr, used as u32);
    }
    if cap == 0 {
        return Ok(bytes.len() as u64);
    }
    if bytes.len() > cap as usize {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, dst, &bytes) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(bytes.len() as u64)
}

// ---------------------------------------------------------------------------
// Comparison. Windows' linguistic order puts letters before case: "a" < "B",
// and when strings differ only in case, lower case sorts first.

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// -1, 0 or 1, comparing the way `CompareString` does in the default locale.
pub(crate) fn linguistic_cmp(a: &str, b: &str, ignore_case: bool) -> i32 {
    use std::cmp::Ordering::*;
    let folded = a.chars().map(fold).cmp(b.chars().map(fold));
    let ord = match folded {
        Equal if !ignore_case => {
            // Lower case first: compare "is upper" per character.
            a.chars()
                .map(char::is_uppercase)
                .cmp(b.chars().map(char::is_uppercase))
        }
        o => o,
    };
    match ord {
        Less => -1,
        Equal => 0,
        Greater => 1,
    }
}

pub(crate) fn ordinal_cmp(a: &[u16], b: &[u16], ignore_case: bool) -> i32 {
    let up = |u: u16| {
        if ignore_case {
            char::from_u32(u as u32)
                .and_then(|c| c.to_uppercase().next())
                .map(|c| c as u32 as u16)
                .unwrap_or(u)
        } else {
            u
        }
    };
    match a.iter().map(|&u| up(u)).cmp(b.iter().map(|&u| up(u))) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn lstrcmp(t: &mut Thread<'_, '_>, a: &[u64], wide: bool, ignore_case: bool) -> Result<u64, Halt> {
    let x = if a[0] == 0 {
        Some(String::new())
    } else {
        read_str(t, a[0], wide)
    };
    let y = if a[1] == 0 {
        Some(String::new())
    } else {
        read_str(t, a[1], wide)
    };
    match (x, y) {
        (Some(x), Some(y)) => Ok(linguistic_cmp(&x, &y, ignore_case) as u32 as u64),
        _ => Ok(0),
    }
}

/// lstrlen: characters before the terminator; NULL and bad pointers count 0.
fn lstrlen(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    if a[0] == 0 {
        return Ok(0);
    }
    Ok(if wide {
        read_wide_units(t, a[0]).map_or(0, |u| u.len() as u64)
    } else {
        t.process
            .memory
            .read_cstr(a[0], 1 << 24)
            .map_or(0, |b| b.len() as u64)
    })
}

/// The raw units of a string, so copies keep bytes that are not valid UTF-8.
fn raw_units(t: &Thread<'_, '_>, addr: u64, wide: bool) -> Option<Vec<u16>> {
    if wide {
        read_wide_units(t, addr)
    } else {
        t.process
            .memory
            .read_cstr(addr, 1 << 24)
            .ok()
            .map(|b| b.into_iter().map(u16::from).collect())
    }
}

fn write_units(t: &mut Thread<'_, '_>, addr: u64, units: &[u16], wide: bool) -> bool {
    let bytes: Vec<u8> = if wide {
        units
            .iter()
            .chain([&0])
            .flat_map(|u| u.to_le_bytes())
            .collect()
    } else {
        units.iter().map(|&u| u as u8).chain([0]).collect()
    };
    write_bytes(t, addr, &bytes)
}

fn lstrcpyn(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (dst, src, n) = (a[0], a[1], a[2] as i32);
    if dst == 0 || src == 0 || n <= 0 {
        return Ok(if n == 0 { dst } else { 0 });
    }
    let Some(mut u) = raw_units(t, src, wide) else {
        return Ok(0);
    };
    u.truncate(n as usize - 1);
    Ok(if write_units(t, dst, &u, wide) {
        dst
    } else {
        0
    })
}

fn lstrcpy(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let Some(u) = raw_units(t, a[1], wide) else {
        return Ok(0);
    };
    Ok(if write_units(t, a[0], &u, wide) {
        a[0]
    } else {
        0
    })
}

fn lstrcat(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (Some(mut d), Some(s)) = (raw_units(t, a[0], wide), raw_units(t, a[1], wide)) else {
        return Ok(0);
    };
    d.extend(s);
    Ok(if write_units(t, a[0], &d, wide) {
        a[0]
    } else {
        0
    })
}

// ---------------------------------------------------------------------------
// wsprintf

/// Where formatting arguments come from: the caller's stack (`wsprintf`,
/// varargs) or a `va_list` pointer (`wvsprintf`).
pub(crate) enum Args {
    Stack(u8),
    VaList(u64),
}

impl Args {
    pub fn next(&mut self, t: &Thread<'_, '_>) -> u64 {
        match self {
            Args::Stack(n) => {
                let v = t.arg(*n).unwrap_or(0);
                *n += 1;
                v
            }
            Args::VaList(p) => {
                let v = read_ptr(t, *p).unwrap_or(0);
                *p += t.ptr_size() as u64;
                v
            }
        }
    }

    /// A 64-bit argument: one slot on x64, two on x86.
    pub fn next64(&mut self, t: &Thread<'_, '_>) -> u64 {
        if t.long_mode() {
            self.next(t)
        } else {
            let lo = self.next(t) & 0xFFFF_FFFF;
            lo | (self.next(t) & 0xFFFF_FFFF) << 32
        }
    }
}

/// Format like `wsprintf` (no floating point, at most 1024 characters out).
/// `wide` says which string type plain `%s` and `%c` take.
pub(crate) fn wsprintf_format(
    t: &Thread<'_, '_>,
    fmt: &[u16],
    args: &mut Args,
    wide: bool,
) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    let at = |i: usize| fmt.get(i).copied().unwrap_or(0);
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' as u16 {
            out.push(c);
            continue;
        }
        let (mut left, mut alt, mut zero) = (false, false, false);
        loop {
            match at(i) {
                0x2D => left = true, // -
                0x23 => alt = true,  // #
                0x30 => zero = true, // 0
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        while (0x30..=0x39).contains(&at(i)) {
            width = width * 10 + (at(i) - 0x30) as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if at(i) == b'.' as u16 {
            i += 1;
            let mut p = 0;
            while (0x30..=0x39).contains(&at(i)) {
                p = p * 10 + (at(i) - 0x30) as usize;
                i += 1;
            }
            prec = Some(p);
        }
        // Size: h (narrow), l (wide / long), I64 and ll (64-bit).
        let (mut narrow, mut long, mut big) = (false, false, false);
        loop {
            match at(i) {
                0x68 => narrow = true,
                0x6C if at(i + 1) == 0x6C => {
                    big = true;
                    i += 1;
                }
                0x6C => long = true,
                0x49 if at(i + 1) == 0x36 && at(i + 2) == 0x34 => {
                    big = true;
                    i += 2;
                }
                _ => break,
            }
            i += 1;
        }
        let ty = at(i);
        i += 1;
        let body: Vec<u16> = match ty as u8 {
            b'%' => vec![b'%' as u16],
            b'c' | b'C' => {
                let as_wide = if narrow {
                    false
                } else if long {
                    true
                } else {
                    (ty == b'c' as u16) == wide
                };
                let v = args.next(t);
                let u = if as_wide {
                    v as u16
                } else {
                    decode_cp(0, &[v as u8], false).unwrap()[0]
                };
                if u == 0 {
                    Vec::new()
                } else {
                    vec![u]
                }
            }
            b's' | b'S' => {
                let as_wide = if narrow {
                    false
                } else if long {
                    true
                } else {
                    (ty == b's' as u16) == wide
                };
                let p = args.next(t);
                let mut s: Vec<u16> = if p == 0 {
                    "(null)".encode_utf16().collect()
                } else if as_wide {
                    read_wide_units(t, p).unwrap_or_default()
                } else {
                    let b = t.process.memory.read_cstr(p, 1 << 20).unwrap_or_default();
                    decode_cp(0, &b, false).unwrap_or_default()
                };
                if let Some(p) = prec {
                    s.truncate(p);
                }
                s
            }
            b'd' | b'i' | b'u' | b'x' | b'X' | b'p' => {
                let ptr = ty == b'p' as u16;
                let v = if big || (ptr && t.long_mode()) {
                    args.next64(t)
                } else {
                    args.next(t) & 0xFFFF_FFFF
                };
                let v = if narrow && !ptr { v & 0xFFFF } else { v };
                let s = match ty as u8 {
                    b'd' | b'i' => {
                        let n = if big {
                            v as i64
                        } else if narrow {
                            v as u16 as i16 as i64
                        } else {
                            v as u32 as i32 as i64
                        };
                        let digits = n.unsigned_abs().to_string();
                        let digits = pad_digits(digits, prec);
                        if n < 0 {
                            format!("-{digits}")
                        } else {
                            digits
                        }
                    }
                    b'u' => pad_digits(v.to_string(), prec),
                    b'x' => {
                        let d = pad_digits(format!("{v:x}"), prec);
                        if alt && v != 0 {
                            format!("0x{d}")
                        } else {
                            d
                        }
                    }
                    b'X' => {
                        let d = pad_digits(format!("{v:X}"), prec);
                        if alt && v != 0 {
                            format!("0X{d}")
                        } else {
                            d
                        }
                    }
                    _ => format!("{v:0w$X}", w = t.ptr_size() * 2),
                };
                s.encode_utf16().collect()
            }
            // Unknown: wsprintf copies the character itself.
            0 => break,
            _ => vec![ty],
        };
        let pad = width.saturating_sub(body.len());
        if left {
            out.extend(&body);
            out.extend(std::iter::repeat_n(b' ' as u16, pad));
        } else {
            let numeric = matches!(ty as u8, b'd' | b'i' | b'u' | b'x' | b'X');
            if zero && numeric && prec.is_none() {
                // Zero padding goes after a sign or 0x prefix.
                let is = |k: usize, c: u8| body.get(k) == Some(&(c as u16));
                let sign = if is(0, b'-') {
                    1
                } else if alt && is(0, b'0') && (is(1, b'x') || is(1, b'X')) {
                    2
                } else {
                    0
                };
                let (head, tail) = body.split_at(sign);
                out.extend(head);
                out.extend(std::iter::repeat_n(b'0' as u16, pad));
                out.extend(tail);
            } else {
                out.extend(std::iter::repeat_n(b' ' as u16, pad));
                out.extend(&body);
            }
        }
    }
    out.truncate(1023);
    out
}

fn pad_digits(d: String, prec: Option<usize>) -> String {
    match prec {
        Some(p) if d.len() < p => format!("{}{d}", "0".repeat(p - d.len())),
        _ => d,
    }
}

fn wsprintf(t: &mut Thread<'_, '_>, a: &[u64], wide: bool, mut args: Args) -> Result<u64, Halt> {
    let (buf, fmt) = (a[0], a[1]);
    let Some(fmt) = raw_units(t, fmt, wide).map(|u| {
        if wide {
            u
        } else {
            let b: Vec<u8> = u.iter().map(|&c| c as u8).collect();
            decode_cp(0, &b, false).unwrap_or_default()
        }
    }) else {
        return Ok(0);
    };
    let out = wsprintf_format(t, &fmt, &mut args, wide);
    let units: Vec<u16> = if wide {
        out
    } else {
        encode_cp(0, &out, b'?')
            .0
            .into_iter()
            .map(u16::from)
            .collect()
    };
    if !write_units(t, buf, &units, wide) {
        return Ok(0);
    }
    Ok(units.len() as u64)
}

// ---------------------------------------------------------------------------
// user32 character helpers. A pointer below 0x10000 is a single character.

fn map_chars(s: &[u16], upper: bool) -> Vec<u16> {
    let text = String::from_utf16_lossy(s);
    let mapped: String = if upper {
        text.to_uppercase()
    } else {
        text.to_lowercase()
    };
    let m: Vec<u16> = mapped.encode_utf16().collect();
    // Keep lengths stable: Windows maps one unit to one unit.
    if m.len() == s.len() {
        m
    } else {
        s.to_vec()
    }
}

fn char_case(t: &mut Thread<'_, '_>, a: &[u64], wide: bool, upper: bool) -> Result<u64, Halt> {
    let p = a[0];
    if p < 0x10000 {
        let c = if wide { p as u16 } else { p as u8 as u16 };
        return Ok(map_chars(&[c], upper)[0] as u64);
    }
    let Some(u) = raw_units(t, p, wide) else {
        return Ok(p);
    };
    let mapped = if wide {
        map_chars(&u, upper)
    } else {
        u.iter()
            .map(|&c| {
                let b = c as u8;
                (if upper {
                    b.to_ascii_uppercase()
                } else {
                    b.to_ascii_lowercase()
                }) as u16
            })
            .collect()
    };
    write_units(t, p, &mapped, wide);
    Ok(p)
}

fn char_case_buff(t: &mut Thread<'_, '_>, a: &[u64], wide: bool, upper: bool) -> Result<u64, Halt> {
    let (p, n) = (a[0], a[1] as u32);
    let size = if wide { 2 } else { 1 };
    let Some(raw) = guest_bytes(t, p, n as u64 * size) else {
        return Ok(0);
    };
    let out: Vec<u8> = if wide {
        let u: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        units_to_bytes(&map_chars(&u, upper))
    } else {
        raw.iter()
            .map(|b| {
                if upper {
                    b.to_ascii_uppercase()
                } else {
                    b.to_ascii_lowercase()
                }
            })
            .collect()
    };
    write_bytes(t, p, &out);
    Ok(n as u64)
}

fn char_class(a: &[u64], wide: bool, test: fn(char) -> bool) -> Result<u64, Halt> {
    let u = if wide { a[0] as u16 } else { a[0] as u8 as u16 };
    Ok(char::from_u32(u as u32).is_some_and(test) as u64)
}

fn char_next(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let p = a[0];
    let step = if wide { 2 } else { 1 };
    let at_end = if wide {
        read_u16(t, p) == Some(0)
    } else {
        guest_bytes(t, p, 1).is_none_or(|b| b[0] == 0)
    };
    if at_end {
        return Ok(p);
    }
    if !wide {
        // Step over a whole UTF-8 sequence.
        let b = guest_bytes(t, p, 1).map_or(0, |b| b[0]);
        let n = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        let mut k = 1;
        while k < n && guest_bytes(t, p + k, 1).is_some_and(|c| c[0] & 0xC0 == 0x80) {
            k += 1;
        }
        return Ok(p + k);
    }
    Ok(p + step)
}

fn char_prev(t: &mut Thread<'_, '_>, a: &[u64], wide: bool) -> Result<u64, Halt> {
    let (start, p) = (a[0], a[1]);
    if p <= start {
        return Ok(start);
    }
    if wide {
        return Ok(p - 2);
    }
    let mut q = p - 1;
    while q > start && guest_bytes(t, q, 1).is_some_and(|c| c[0] & 0xC0 == 0x80) {
        q -= 1;
    }
    Ok(q)
}

pub(crate) static APIS: &[Api] = &[
    k32("MultiByteToWideChar", 6, multi_byte_to_wide_char),
    k32("WideCharToMultiByte", 8, wide_char_to_multi_byte),
    k32("lstrcmpA", 2, |t, a| lstrcmp(t, a, false, false)),
    k32("lstrcmpW", 2, |t, a| lstrcmp(t, a, true, false)),
    k32("lstrcmpiA", 2, |t, a| lstrcmp(t, a, false, true)),
    k32("lstrcmpiW", 2, |t, a| lstrcmp(t, a, true, true)),
    k32("lstrcpyA", 2, |t, a| lstrcpy(t, a, false)),
    k32("lstrcpyW", 2, |t, a| lstrcpy(t, a, true)),
    k32("lstrcpynA", 3, |t, a| lstrcpyn(t, a, false)),
    k32("lstrcpynW", 3, |t, a| lstrcpyn(t, a, true)),
    k32("lstrcatA", 2, |t, a| lstrcat(t, a, false)),
    k32("lstrcatW", 2, |t, a| lstrcat(t, a, true)),
    k32("lstrlenA", 1, |t, a| lstrlen(t, a, false)),
    k32("lstrlenW", 1, |t, a| lstrlen(t, a, true)),
    k32("lstrcmp", 2, |t, a| lstrcmp(t, a, false, false)),
    k32("lstrcmpi", 2, |t, a| lstrcmp(t, a, false, true)),
    k32("lstrcpy", 2, |t, a| lstrcpy(t, a, false)),
    k32("lstrcpyn", 3, |t, a| lstrcpyn(t, a, false)),
    k32("lstrcat", 2, |t, a| lstrcat(t, a, false)),
    k32("lstrlen", 1, |t, a| lstrlen(t, a, false)),
    // wsprintfA/W are documented as user32 but shipped in kernel32 too.
    cdecl("kernel32", "wsprintfA", 2, |t, a| {
        wsprintf(t, a, false, Args::Stack(2))
    }),
    cdecl("kernel32", "wsprintfW", 2, |t, a| {
        wsprintf(t, a, true, Args::Stack(2))
    }),
];

pub(crate) static USER32: &[Api] = &[
    cdecl("user32", "wsprintfA", 2, |t, a| {
        wsprintf(t, a, false, Args::Stack(2))
    }),
    cdecl("user32", "wsprintfW", 2, |t, a| {
        wsprintf(t, a, true, Args::Stack(2))
    }),
    api("user32", "wvsprintfA", 3, |t, a| {
        wsprintf(t, a, false, Args::VaList(a[2]))
    }),
    api("user32", "wvsprintfW", 3, |t, a| {
        wsprintf(t, a, true, Args::VaList(a[2]))
    }),
    api("user32", "CharUpperA", 1, |t, a| {
        char_case(t, a, false, true)
    }),
    api("user32", "CharUpperW", 1, |t, a| {
        char_case(t, a, true, true)
    }),
    api("user32", "CharLowerA", 1, |t, a| {
        char_case(t, a, false, false)
    }),
    api("user32", "CharLowerW", 1, |t, a| {
        char_case(t, a, true, false)
    }),
    api("user32", "CharUpperBuffA", 2, |t, a| {
        char_case_buff(t, a, false, true)
    }),
    api("user32", "CharUpperBuffW", 2, |t, a| {
        char_case_buff(t, a, true, true)
    }),
    api("user32", "CharLowerBuffA", 2, |t, a| {
        char_case_buff(t, a, false, false)
    }),
    api("user32", "CharLowerBuffW", 2, |t, a| {
        char_case_buff(t, a, true, false)
    }),
    api("user32", "CharNextA", 1, |t, a| char_next(t, a, false)),
    api("user32", "CharNextW", 1, |t, a| char_next(t, a, true)),
    api("user32", "CharPrevA", 2, |t, a| char_prev(t, a, false)),
    api("user32", "CharPrevW", 2, |t, a| char_prev(t, a, true)),
    api("user32", "IsCharAlphaA", 1, |_, a| {
        char_class(a, false, char::is_alphabetic)
    }),
    api("user32", "IsCharAlphaW", 1, |_, a| {
        char_class(a, true, char::is_alphabetic)
    }),
    api("user32", "IsCharAlphaNumericA", 1, |_, a| {
        char_class(a, false, char::is_alphanumeric)
    }),
    api("user32", "IsCharAlphaNumericW", 1, |_, a| {
        char_class(a, true, char::is_alphanumeric)
    }),
    api("user32", "IsCharUpperA", 1, |_, a| {
        char_class(a, false, char::is_uppercase)
    }),
    api("user32", "IsCharUpperW", 1, |_, a| {
        char_class(a, true, char::is_uppercase)
    }),
    api("user32", "IsCharLowerA", 1, |_, a| {
        char_class(a, false, char::is_lowercase)
    }),
    api("user32", "IsCharLowerW", 1, |_, a| {
        char_class(a, true, char::is_lowercase)
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_pages_round_trip() {
        let euro = decode_cp(CP_1252, &[0x80, b'a', 0xE9], false).unwrap();
        assert_eq!(euro, [0x20AC, 0x61, 0xE9]);
        assert_eq!(
            encode_cp(CP_1252, &euro, b'?'),
            (vec![0x80, b'a', 0xE9], false)
        );
        assert_eq!(
            encode_cp(CP_ASCII, &euro, b'?'),
            (vec![b'?', b'a', b'?'], true)
        );
        assert!(decode_cp(CP_UTF8, &[0xFF], true).is_err());
        assert_eq!(decode_cp(0, "é".as_bytes(), true).unwrap(), [0xE9]);
    }

    #[test]
    fn linguistic_order_ignores_case_first() {
        assert_eq!(linguistic_cmp("a", "B", false), -1);
        assert_eq!(linguistic_cmp("a", "A", false), -1);
        assert_eq!(linguistic_cmp("A", "a", true), 0);
        assert_eq!(linguistic_cmp("abc", "ab", false), 1);
    }
}
