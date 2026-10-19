//! Locale and National Language Support: code page information, locale
//! identifiers and settings, string comparison and mapping, character
//! types, and number, date and time formatting.
//!
//! AnyOS has one locale, English (United States), with UTF-8 as the ANSI code
//! page (see `strings.rs`).

use crate::api::*;
use crate::strings::{read_counted_bytes, read_counted_units, resolve_cp, valid_cp, CP_UTF8};
use crate::thread::{Halt, Thread};

const LCID_EN_US: u32 = 0x0409;
const LOCALE_NAME: &str = "en-US";
const ERROR_INVALID_FLAGS: u32 = 1004;

const LOCALE_NOUSEROVERRIDE: u32 = 0x8000_0000;
const LOCALE_USE_CP_ACP: u32 = 0x4000_0000;
const LOCALE_RETURN_NUMBER: u32 = 0x2000_0000;

const NORM_IGNORECASE: u32 = 0x1;
const LINGUISTIC_IGNORECASE: u32 = 0x10;
const LCMAP_LOWERCASE: u32 = 0x100;
const LCMAP_UPPERCASE: u32 = 0x200;
const LCMAP_SORTKEY: u32 = 0x400;

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A locale setting of en-US.
fn locale_info(lctype: u32) -> Option<String> {
    let s = match lctype {
        0x0001 | 0x0009 => "0409",
        0x0002 | 0x0073 | 0x0074 => "English (United States)",
        0x0003 => "ENU",
        0x0004 | 0x1001 | 0x006F | 0x0072 => "English",
        0x0005 | 0x000A => "1",
        0x0006 | 0x0008 | 0x1002 | 0x0070 | 0x0076 => "United States",
        0x0007 | 0x0068 => "USA",
        0x000B => "437",
        0x1004 => "65001",
        0x1011 => "10000",
        0x1012 => "037",
        0x000C => ",",
        0x000D => "1",
        0x000E | 0x0016 => ".",
        0x000F | 0x0017 => ",",
        0x0010 | 0x0018 => "3;0",
        0x0011 | 0x0019 | 0x001A => "2",
        0x0012 => "1",
        0x1010 => "1",
        0x0013 => "0123456789",
        0x0014 => "$",
        0x0015 => "USD",
        0x001B | 0x001C => "0",
        0x001D => "/",
        0x001E => ":",
        0x001F => "M/d/yyyy",
        0x0020 => "dddd, MMMM d, yyyy",
        0x1003 => "h:mm:ss tt",
        0x0079 => "h:mm tt",
        0x1006 => "MMMM yyyy",
        0x005D => "hh:mm:ss",
        0x0021 | 0x0022 | 0x0023 | 0x0024 | 0x0026 | 0x0027 => "0",
        0x0025 => "1",
        0x0028 => "AM",
        0x0029 => "PM",
        0x1009 => "1",
        0x100B => "0",
        0x100C => "6",
        0x100D => "0",
        0x100A => "1",
        0x1014 => "1",
        0x100E | 0x100F | 0x0050 => "",
        0x0051 => "-",
        0x0059 | 0x006D => "en",
        0x005A => "US",
        0x005B => "244",
        0x005C | 0x0071 => LOCALE_NAME,
        0x0067 => "eng",
        0x1007 | 0x1008 => "US Dollar",
        0x002A..=0x0030 => DAYS[((lctype - 0x2A + 1) % 7) as usize],
        0x0031..=0x0037 => &DAYS[((lctype - 0x31 + 1) % 7) as usize][..3],
        0x0060..=0x0066 => &DAYS[((lctype - 0x60 + 1) % 7) as usize][..2],
        0x0038..=0x0043 => MONTHS[(lctype - 0x38) as usize],
        0x0044..=0x004F => &MONTHS[(lctype - 0x44) as usize][..3],
        _ => return None,
    };
    Some(s.to_string())
}

fn locale_known(lcid: u32) -> bool {
    // LOCALE_SYSTEM_DEFAULT, LOCALE_USER_DEFAULT, LOCALE_INVARIANT,
    // LOCALE_NEUTRAL, LOCALE_CUSTOM_DEFAULT and English variants all read as en-US.
    matches!(lcid, 0 | 0x0400 | 0x0800 | 0x007F | 0x0C00 | 0x1000) || lcid & 0x3FF == 0x09
}

fn get_locale_info(
    t: &mut Thread<'_, '_>,
    known: bool,
    lctype: u32,
    buf: u64,
    cch: u64,
    wide: bool,
) -> Result<u64, Halt> {
    if !known {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let kind = lctype & !(LOCALE_NOUSEROVERRIDE | LOCALE_USE_CP_ACP | LOCALE_RETURN_NUMBER);
    let Some(value) = locale_info(kind) else {
        return fail(t, ERROR_INVALID_FLAGS, 0);
    };
    let cch = cch as u32 as u64;
    if lctype & LOCALE_RETURN_NUMBER != 0 {
        let Ok(n) = value.parse::<u32>() else {
            return fail(t, ERROR_INVALID_FLAGS, 0);
        };
        // The number takes a DWORD: 2 WCHARs or 4 CHARs.
        let need = if wide { 2 } else { 4 };
        if cch == 0 {
            return Ok(need);
        }
        if cch < need {
            return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        write_u32(t, buf, n);
        return Ok(need);
    }
    let need = char_len(&value, wide) + 1;
    if cch == 0 {
        return Ok(need);
    }
    if cch < need {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, buf, &encode(&value, wide)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(need)
}

fn locale_name_known(t: &mut Thread<'_, '_>, p: u64) -> bool {
    if p == 0 {
        return true; // LOCALE_NAME_USER_DEFAULT
    }
    let name = read_wide(t, p).unwrap_or_default();
    name.is_empty()
        || name.eq_ignore_ascii_case("!x-sys-default-locale")
        || name.to_ascii_lowercase().starts_with("en")
}

/// A `(name, cch)` buffer pair receiving a locale name; returns characters
/// including the terminator.
fn return_name(t: &mut Thread<'_, '_>, buf: u64, cch: u64, name: &str) -> Result<u64, Halt> {
    let need = char_len(name, true) + 1;
    let cch = cch as u32 as u64;
    if cch == 0 {
        return Ok(need);
    }
    if cch < need {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    write_bytes(t, buf, &encode(name, true));
    Ok(need)
}

/// `en-US\0\0`, the preferred UI language list.
fn preferred_languages(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (count, buf, cch) = (a[1], a[2], a[3]);
    let list = encode(&format!("{LOCALE_NAME}\0"), true);
    let need = (list.len() / 2) as u32;
    store_u32(t, count, 1);
    let Some(size) = read_u32(t, cch) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    write_u32(t, cch, need);
    if buf == 0 {
        return Ok(1);
    }
    if size < need {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    write_bytes(t, buf, &list);
    Ok(1)
}

// ---------------------------------------------------------------------------
// Code pages

/// CPINFO / CPINFOEX for a code page.
fn cp_info(t: &mut Thread<'_, '_>, cp: u32, ptr: u64, ex: bool, wide: bool) -> Result<u64, Halt> {
    if !valid_cp(cp) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let cp = resolve_cp(cp);
    let max: u32 = if cp == CP_UTF8 { 4 } else { 1 };
    let mut b = Vec::new();
    b.extend_from_slice(&max.to_le_bytes());
    b.extend_from_slice(&[b'?', 0]);
    b.extend_from_slice(&[0; 12]); // no lead bytes
    if ex {
        b.extend_from_slice(&0xFFFDu16.to_le_bytes());
        b.extend_from_slice(&cp.to_le_bytes());
        let name = match cp {
            CP_UTF8 => "65001 (UTF-8)",
            1252 => "1252  (ANSI - Latin I)",
            437 => "437   (OEM - United States)",
            850 => "850   (OEM - Multilingual Latin I)",
            28591 => "28591 (ISO 8859-1 Latin I)",
            _ => "20127 (US-ASCII)",
        };
        let mut n = encode(name, wide);
        n.resize(if wide { 520 } else { 260 }, 0);
        b.extend(n);
    }
    if !write_bytes(t, ptr, &b) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

// ---------------------------------------------------------------------------
// Comparison and mapping

/// A counted (`len >= 0`) or NUL-terminated (`len < 0`) string argument.
fn counted_str(t: &Thread<'_, '_>, p: u64, len: i32, wide: bool) -> Option<String> {
    if wide {
        let mut u = read_counted_units(t, p, len)?;
        if len < 0 {
            u.pop();
        }
        Some(String::from_utf16_lossy(&u))
    } else {
        let mut b = read_counted_bytes(t, p, len)?;
        if len < 0 {
            b.pop();
        }
        Some(crate::strings::ansi_to_string(&b))
    }
}

fn compare(
    t: &mut Thread<'_, '_>,
    flags: u32,
    s1: u64,
    n1: u64,
    s2: u64,
    n2: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let (Some(a), Some(b)) = (
        counted_str(t, s1, n1 as u32 as i32, wide),
        counted_str(t, s2, n2 as u32 as i32, wide),
    ) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    // Counted strings may contain their terminator; it does not take part.
    let a = a.trim_end_matches('\0');
    let b = b.trim_end_matches('\0');
    let ignore = flags & (NORM_IGNORECASE | LINGUISTIC_IGNORECASE) != 0;
    Ok((crate::strings::linguistic_cmp(a, b, ignore) + 2) as u64)
}

fn compare_ordinal(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (Some(x), Some(y)) = (
        read_counted_units(t, a[0], a[1] as u32 as i32),
        read_counted_units(t, a[2], a[3] as u32 as i32),
    ) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let strip = |mut v: Vec<u16>, len: i32| {
        if len < 0 {
            v.pop();
        }
        v
    };
    let x = strip(x, a[1] as u32 as i32);
    let y = strip(y, a[3] as u32 as i32);
    Ok((crate::strings::ordinal_cmp(&x, &y, a[4] as u32 != 0) + 2) as u64)
}

fn lcmap(
    t: &mut Thread<'_, '_>,
    flags: u32,
    src: u64,
    n: u64,
    dst: u64,
    cch: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let len = n as u32 as i32;
    let Some(s) = counted_str(t, src, len, wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let terminated = len < 0;
    let cch = cch as u32 as u64;
    if flags & LCMAP_SORTKEY != 0 {
        // A byte string that sorts like the text: case-folded UTF-16, big
        // endian, then the terminator.
        let mut key: Vec<u8> = s
            .to_uppercase()
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        key.push(0);
        if cch == 0 {
            return Ok(key.len() as u64);
        }
        if (cch as usize) < key.len() {
            return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        write_bytes(t, dst, &key);
        return Ok(key.len() as u64);
    }
    let out = if flags & LCMAP_UPPERCASE != 0 {
        s.to_uppercase()
    } else if flags & LCMAP_LOWERCASE != 0 {
        s.to_lowercase()
    } else {
        s
    };
    let mut bytes = encode(&out, wide);
    if !terminated {
        bytes.truncate(bytes.len() - if wide { 2 } else { 1 });
    }
    let unit = if wide { 2 } else { 1 };
    let count = (bytes.len() / unit) as u64;
    if cch == 0 {
        return Ok(count);
    }
    if cch < count {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, dst, &bytes) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(count)
}

/// CT_CTYPE1/2/3 bits for one character.
fn char_type(kind: u32, c: char) -> u16 {
    match kind {
        1 => {
            let mut v = 0u16;
            if c.is_uppercase() {
                v |= 0x1;
            }
            if c.is_lowercase() {
                v |= 0x2;
            }
            if c.is_ascii_digit() {
                v |= 0x4;
            }
            if c.is_whitespace() {
                v |= 0x8;
            }
            if c.is_ascii_punctuation()
                || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() && !c.is_ascii())
            {
                v |= 0x10;
            }
            if c.is_control() {
                v |= 0x20;
            }
            if c == ' ' || c == '\t' {
                v |= 0x40;
            }
            if c.is_ascii_hexdigit() {
                v |= 0x80;
            }
            if c.is_alphabetic() {
                v |= 0x100;
            }
            v | 0x200
        }
        2 => {
            if c.is_alphabetic() {
                1 // C2_LEFTTORIGHT
            } else if c.is_ascii_digit() {
                3 // C2_EUROPENUMBER
            } else if c.is_whitespace() {
                0xA // C2_WHITESPACE
            } else if c.is_control() {
                0
            } else {
                0xB // C2_OTHERNEUTRAL
            }
        }
        4 => {
            if c.is_alphabetic() {
                0x8000 // C3_ALPHA
            } else if c.is_ascii_punctuation() {
                0x0008 // C3_SYMBOL
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn get_string_type(
    t: &mut Thread<'_, '_>,
    kind: u32,
    src: u64,
    n: u64,
    out: u64,
    wide: bool,
) -> Result<u64, Halt> {
    if !matches!(kind, 1 | 2 | 4) {
        return fail(t, ERROR_INVALID_FLAGS, 0);
    }
    let len = n as u32 as i32;
    let units: Vec<u16> = if wide {
        let Some(mut u) = read_counted_units(t, src, len) else {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        };
        if len < 0 {
            u.pop();
        }
        u
    } else {
        // One result per byte; bytes above ASCII are classed as Latin-1.
        let Some(mut b) = read_counted_bytes(t, src, len) else {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        };
        if len < 0 {
            b.pop();
        }
        b.into_iter().map(u16::from).collect()
    };
    let types: Vec<u8> = units
        .iter()
        .map(|&u| char_type(kind, char::from_u32(u as u32).unwrap_or('\u{FFFD}')))
        .flat_map(u16::to_le_bytes)
        .collect();
    if !write_bytes(t, out, &types) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

// ---------------------------------------------------------------------------
// Number formatting

struct NumberFormat {
    digits: u32,
    leading_zero: bool,
    grouping: u32,
    decimal: String,
    thousand: String,
    negative: u32,
}

impl Default for NumberFormat {
    fn default() -> Self {
        NumberFormat {
            digits: 2,
            leading_zero: true,
            grouping: 3,
            decimal: ".".into(),
            thousand: ",".into(),
            negative: 1,
        }
    }
}

/// Read a NUMBERFMT (or the fields CURRENCYFMT shares with it).
fn read_number_format(t: &Thread<'_, '_>, p: u64, wide: bool) -> Option<NumberFormat> {
    let ptr = t.ptr_size() as u64;
    let strings = if ptr == 8 { 16 } else { 12 };
    Some(NumberFormat {
        digits: read_u32(t, p)?,
        leading_zero: read_u32(t, p + 4)? != 0,
        grouping: read_u32(t, p + 8)?,
        decimal: read_str(t, read_ptr(t, p + strings)?, wide)?,
        thousand: read_str(t, read_ptr(t, p + strings + ptr)?, wide)?,
        negative: read_u32(t, p + strings + 2 * ptr)?,
    })
}

/// Format the plain number string `value` ("-1234.5") as `f` says.
fn format_number(value: &str, f: &NumberFormat) -> Option<String> {
    let value = value.trim();
    let (neg, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if !int.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
        || (int.is_empty() && frac.is_empty())
    {
        return None;
    }
    // Round the fraction to `digits` places.
    let mut int_digits: Vec<u8> = int.bytes().map(|b| b - b'0').collect();
    let mut frac_digits: Vec<u8> = frac.bytes().map(|b| b - b'0').collect();
    let n = f.digits as usize;
    let round_up = frac_digits.get(n).is_some_and(|&d| d >= 5);
    frac_digits.resize(n, 0);
    if round_up {
        let mut carry = true;
        for d in frac_digits
            .iter_mut()
            .rev()
            .chain(int_digits.iter_mut().rev())
        {
            if !carry {
                break;
            }
            *d += 1;
            carry = *d == 10;
            if carry {
                *d = 0;
            }
        }
        if carry {
            int_digits.insert(0, 1);
        }
    }
    while int_digits.len() > 1 && int_digits[0] == 0 {
        int_digits.remove(0);
    }
    let mut int_str: String = int_digits.iter().map(|d| (b'0' + d) as char).collect();
    if int_str == "0" && !f.leading_zero && n > 0 {
        int_str.clear();
    }
    // Grouping: 3 = groups of 3; 32 = 3 then 2 (Indian style); 0 = none.
    let groups: Vec<usize> = match f.grouping {
        0 => vec![],
        g if g >= 10 => vec![(g / 10) as usize, (g % 10) as usize],
        g => vec![g as usize],
    };
    if let Some(&first) = groups.first() {
        let rest = *groups.get(1).unwrap_or(&first);
        let chars: Vec<char> = int_str.chars().collect();
        let mut parts = Vec::new();
        let mut end = chars.len();
        let mut size = first;
        while end > size && size > 0 {
            parts.push(chars[end - size..end].iter().collect::<String>());
            end -= size;
            size = rest;
        }
        parts.push(chars[..end].iter().collect::<String>());
        parts.reverse();
        int_str = parts.join(&f.thousand);
    }
    let mut s = int_str;
    if n > 0 {
        s.push_str(&f.decimal);
        s.extend(frac_digits.iter().map(|d| (b'0' + d) as char));
    }
    let zero = !s.chars().any(|c| c.is_ascii_digit() && c != '0');
    if neg && !zero {
        s = match f.negative {
            0 => format!("({s})"),
            2 => format!("- {s}"),
            3 => format!("{s}-"),
            4 => format!("{s} -"),
            _ => format!("-{s}"),
        };
    }
    Some(s)
}

fn get_number_format(
    t: &mut Thread<'_, '_>,
    value: u64,
    fmt: u64,
    buf: u64,
    cch: u64,
    wide: bool,
    currency: bool,
) -> Result<u64, Halt> {
    let Some(v) = read_str(t, value, wide) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let (f, positive, negative, symbol) = if fmt != 0 {
        let Some(f) = read_number_format(t, fmt, wide) else {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        };
        if currency {
            // CURRENCYFMT: ... NegativeOrder, PositiveOrder, lpCurrencySymbol.
            let p = t.ptr_size() as u64;
            let strings = if p == 8 { 16 } else { 12 };
            let pos = read_u32(t, fmt + strings + 2 * p + 4).unwrap_or(0);
            let sym_ptr = read_ptr(t, fmt + strings + 2 * p + 8).unwrap_or(0);
            let sym = read_str(t, sym_ptr, wide).unwrap_or_else(|| "$".into());
            let neg = f.negative;
            (NumberFormat { negative: 1, ..f }, pos, neg, sym)
        } else {
            (f, 0, 0, String::new())
        }
    } else {
        (NumberFormat::default(), 0, 0, "$".into())
    };
    let Some(mut out) = format_number(
        &v,
        &NumberFormat {
            negative: if currency { 99 } else { f.negative },
            ..f
        },
    ) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    if currency {
        let neg = out.starts_with('-');
        let body = out.trim_start_matches('-').to_string();
        out = if neg {
            match negative {
                0 => format!("({symbol}{body})"),
                1 => format!("-{symbol}{body}"),
                2 => format!("{symbol}-{body}"),
                3 => format!("{symbol}{body}-"),
                4 => format!("({body}{symbol})"),
                5 => format!("-{body}{symbol}"),
                8 => format!("-{body} {symbol}"),
                9 => format!("-{symbol} {body}"),
                _ => format!("({symbol}{body})"),
            }
        } else {
            match positive {
                1 => format!("{body}{symbol}"),
                2 => format!("{symbol} {body}"),
                3 => format!("{body} {symbol}"),
                _ => format!("{symbol}{body}"),
            }
        };
    }
    return_string(t, buf, cch, &out, wide)
}

/// Results counted in characters including the terminator; `cch == 0`
/// asks for the size.
fn return_string(
    t: &mut Thread<'_, '_>,
    buf: u64,
    cch: u64,
    s: &str,
    wide: bool,
) -> Result<u64, Halt> {
    let need = char_len(s, wide) + 1;
    let cch = cch as u32 as u64;
    if cch == 0 {
        return Ok(need);
    }
    if cch < need {
        return fail(t, ERROR_INSUFFICIENT_BUFFER, 0);
    }
    if !write_bytes(t, buf, &encode(s, wide)) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(need)
}

// ---------------------------------------------------------------------------
// Date and time formatting

const DATE_SHORTDATE: u32 = 0x1;
const DATE_LONGDATE: u32 = 0x2;
const DATE_YEARMONTH: u32 = 0x8;
const TIME_NOMINUTESORSECONDS: u32 = 0x1;
const TIME_NOSECONDS: u32 = 0x2;
const TIME_NOTIMEMARKER: u32 = 0x4;
const TIME_FORCE24HOURFORMAT: u32 = 0x8;

/// Expand a date or time picture (`dddd, MMMM d, yyyy`, `h:mm:ss tt`) for the
/// SYSTEMTIME fields `st`. Quoted text is copied.
pub(crate) fn format_picture(pic: &str, st: &[u16; 8], date: bool, flags: u32) -> String {
    let [year, month, dow, day, hour, minute, second, _] = *st;
    let chars: Vec<char> = pic.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                out.push(chars[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 1;
        while chars.get(i + n) == Some(&c) {
            n += 1;
        }
        let pad = |v: u16, n: usize| {
            if n >= 2 {
                format!("{v:02}")
            } else {
                v.to_string()
            }
        };
        let field = match (date, c) {
            (true, 'd') => match n {
                1 | 2 => pad(day, n),
                3 => DAYS[dow as usize % 7][..3].to_string(),
                _ => DAYS[dow as usize % 7].to_string(),
            },
            (true, 'M') => {
                let m = (month.clamp(1, 12) - 1) as usize;
                match n {
                    1 | 2 => pad(month, n),
                    3 => MONTHS[m][..3].to_string(),
                    _ => MONTHS[m].to_string(),
                }
            }
            (true, 'y') => match n {
                1 => (year % 100).to_string(),
                2 => format!("{:02}", year % 100),
                _ => year.to_string(),
            },
            (true, 'g') => "A.D.".to_string(),
            (false, 'h') if flags & TIME_FORCE24HOURFORMAT == 0 => {
                let h = match hour % 12 {
                    0 => 12,
                    h => h,
                };
                pad(h, n)
            }
            (false, 'h' | 'H') => pad(hour, n),
            (false, 'm') if flags & TIME_NOMINUTESORSECONDS == 0 => pad(minute, n),
            (false, 's') if flags & (TIME_NOSECONDS | TIME_NOMINUTESORSECONDS) == 0 => {
                pad(second, n)
            }
            (false, 'm' | 's') => {
                // Drop the field and the separator before it.
                while out.ends_with([':', '.', ' ']) {
                    out.pop();
                }
                String::new()
            }
            (false, 't')
                if flags & TIME_NOTIMEMARKER == 0 && flags & TIME_FORCE24HOURFORMAT == 0 =>
            {
                let m = if hour < 12 { "AM" } else { "PM" };
                if n == 1 {
                    m[..1].to_string()
                } else {
                    m.to_string()
                }
            }
            (false, 't') => {
                while out.ends_with(' ') {
                    out.pop();
                }
                String::new()
            }
            _ => std::iter::repeat_n(c, n).collect(),
        };
        out.push_str(&field);
        i += n;
    }
    out
}

/// The SYSTEMTIME to format: the argument, or the local time now.
fn time_arg(t: &Thread<'_, '_>, p: u64) -> Option<[u16; 8]> {
    if p == 0 {
        crate::time::filetime_to_fields(crate::time::utc_to_local(crate::time::filetime_now()))
    } else {
        crate::time::read_systemtime(t, p)
    }
}

fn get_date_format(
    t: &mut Thread<'_, '_>,
    flags: u32,
    st: u64,
    fmt: u64,
    buf: u64,
    cch: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let Some(mut st) = time_arg(t, st) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    // Recompute the day of the week from the date, as Windows does.
    if let Some(ft) = crate::time::fields_to_filetime(&st) {
        if let Some(f) = crate::time::filetime_to_fields(ft) {
            st[2] = f[2];
        }
    }
    let pic = match opt_str(t, fmt, wide) {
        Ok(Some(p)) => p,
        Ok(None) => {
            let kind = if flags & DATE_LONGDATE != 0 {
                0x20
            } else if flags & DATE_YEARMONTH != 0 {
                0x1006
            } else {
                let _ = DATE_SHORTDATE;
                0x1F
            };
            locale_info(kind).unwrap_or_default()
        }
        Err(()) => return Ok(0),
    };
    let s = format_picture(&pic, &st, true, flags);
    return_string(t, buf, cch, &s, wide)
}

fn get_time_format(
    t: &mut Thread<'_, '_>,
    flags: u32,
    st: u64,
    fmt: u64,
    buf: u64,
    cch: u64,
    wide: bool,
) -> Result<u64, Halt> {
    let Some(st) = time_arg(t, st) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let pic = match opt_str(t, fmt, wide) {
        Ok(Some(p)) => p,
        Ok(None) => locale_info(0x1003).unwrap_or_default(),
        Err(()) => return Ok(0),
    };
    let s = format_picture(&pic, &st, false, flags);
    return_string(t, buf, cch, &s, wide)
}

fn lcid_arg(a: u64) -> bool {
    locale_known(a as u32)
}

pub(crate) static APIS: &[Api] = &[
    k32("GetACP", 0, |_, _| Ok(CP_UTF8 as u64)),
    k32("GetOEMCP", 0, |_, _| Ok(CP_UTF8 as u64)),
    k32(
        "IsValidCodePage",
        1,
        |_, a| Ok(valid_cp(a[0] as u32) as u64),
    ),
    k32("GetCPInfo", 2, |t, a| {
        cp_info(t, a[0] as u32, a[1], false, false)
    }),
    k32("GetCPInfoExA", 3, |t, a| {
        cp_info(t, a[0] as u32, a[2], true, false)
    }),
    k32("GetCPInfoExW", 3, |t, a| {
        cp_info(t, a[0] as u32, a[2], true, true)
    }),
    // UTF-8 has no DBCS lead bytes.
    k32("IsDBCSLeadByte", 1, |_, _| Ok(0)),
    k32("IsDBCSLeadByteEx", 2, |_, _| Ok(0)),
    k32("GetUserDefaultLCID", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32("GetSystemDefaultLCID", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32("GetUserDefaultLangID", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32("GetSystemDefaultLangID", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32("GetUserDefaultUILanguage", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32(
        "GetSystemDefaultUILanguage",
        0,
        |_, _| Ok(LCID_EN_US as u64),
    ),
    k32("GetThreadLocale", 0, |t, _| {
        Ok(match t.k.thread_locale {
            0 => LCID_EN_US,
            l => l,
        } as u64)
    }),
    k32("SetThreadLocale", 1, |t, a| {
        t.k.thread_locale = a[0] as u32;
        Ok(1)
    }),
    k32("GetThreadUILanguage", 0, |_, _| Ok(LCID_EN_US as u64)),
    k32("SetThreadUILanguage", 1, |_, a| {
        Ok(if a[0] as u16 == 0 {
            LCID_EN_US as u64
        } else {
            a[0] as u16 as u64
        })
    }),
    k32("ConvertDefaultLocale", 1, |_, a| {
        Ok(match a[0] as u32 {
            0 | 0x400 | 0x800 => LCID_EN_US,
            l => l,
        } as u64)
    }),
    k32("IsValidLocale", 2, |_, a| Ok(lcid_arg(a[0]) as u64)),
    k32("IsValidLocaleName", 1, |t, a| {
        Ok(locale_name_known(t, a[0]) as u64)
    }),
    k32("GetUserDefaultLocaleName", 2, |t, a| {
        return_name(t, a[0], a[1], LOCALE_NAME)
    }),
    k32("GetSystemDefaultLocaleName", 2, |t, a| {
        return_name(t, a[0], a[1], LOCALE_NAME)
    }),
    k32("LCIDToLocaleName", 4, |t, a| {
        if !lcid_arg(a[0]) {
            return fail(t, ERROR_INVALID_PARAMETER, 0);
        }
        return_name(t, a[1], a[2], LOCALE_NAME)
    }),
    k32("LocaleNameToLCID", 2, |t, a| {
        if locale_name_known(t, a[0]) {
            Ok(LCID_EN_US as u64)
        } else {
            fail(t, ERROR_INVALID_PARAMETER, 0)
        }
    }),
    k32("ResolveLocaleName", 3, |t, a| {
        return_name(t, a[1], a[2], LOCALE_NAME)
    }),
    k32("GetUserGeoID", 1, |_, _| Ok(244)),
    k32("GetUserPreferredUILanguages", 4, preferred_languages),
    k32("GetThreadPreferredUILanguages", 4, preferred_languages),
    k32("GetProcessPreferredUILanguages", 4, preferred_languages),
    k32("GetSystemPreferredUILanguages", 4, preferred_languages),
    k32("SetProcessPreferredUILanguages", 3, |_, _| Ok(1)),
    k32("SetThreadPreferredUILanguages", 3, |_, _| Ok(1)),
    k32("GetLocaleInfoA", 4, |t, a| {
        get_locale_info(t, lcid_arg(a[0]), a[1] as u32, a[2], a[3], false)
    }),
    k32("GetLocaleInfoW", 4, |t, a| {
        get_locale_info(t, lcid_arg(a[0]), a[1] as u32, a[2], a[3], true)
    }),
    k32("GetLocaleInfoEx", 4, |t, a| {
        let known = locale_name_known(t, a[0]);
        get_locale_info(t, known, a[1] as u32, a[2], a[3], true)
    }),
    k32("SetLocaleInfoA", 3, |_, _| Ok(1)),
    k32("SetLocaleInfoW", 3, |_, _| Ok(1)),
    k32("CompareStringA", 6, |t, a| {
        compare(t, a[1] as u32, a[2], a[3], a[4], a[5], false)
    }),
    k32("CompareStringW", 6, |t, a| {
        compare(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("CompareStringEx", 9, |t, a| {
        compare(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("CompareStringOrdinal", 5, compare_ordinal),
    k32("LCMapStringA", 6, |t, a| {
        lcmap(t, a[1] as u32, a[2], a[3], a[4], a[5], false)
    }),
    k32("LCMapStringW", 6, |t, a| {
        lcmap(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("LCMapStringEx", 9, |t, a| {
        lcmap(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("FoldStringA", 5, |t, a| {
        lcmap(t, 0, a[1], a[2], a[3], a[4], false)
    }),
    k32("FoldStringW", 5, |t, a| {
        lcmap(t, 0, a[1], a[2], a[3], a[4], true)
    }),
    k32("NormalizeString", 5, |t, a| {
        lcmap(t, 0, a[1], a[2], a[3], a[4], true)
    }),
    k32("IsNormalizedString", 3, |_, _| Ok(1)),
    k32("GetStringTypeW", 4, |t, a| {
        get_string_type(t, a[0] as u32, a[1], a[2], a[3], true)
    }),
    k32("GetStringTypeExW", 5, |t, a| {
        get_string_type(t, a[1] as u32, a[2], a[3], a[4], true)
    }),
    k32("GetStringTypeA", 5, |t, a| {
        get_string_type(t, a[1] as u32, a[2], a[3], a[4], false)
    }),
    k32("GetStringTypeExA", 5, |t, a| {
        get_string_type(t, a[1] as u32, a[2], a[3], a[4], false)
    }),
    k32("GetNumberFormatA", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], false, false)
    }),
    k32("GetNumberFormatW", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], true, false)
    }),
    k32("GetNumberFormatEx", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], true, false)
    }),
    k32("GetCurrencyFormatA", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], false, true)
    }),
    k32("GetCurrencyFormatW", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], true, true)
    }),
    k32("GetCurrencyFormatEx", 6, |t, a| {
        get_number_format(t, a[2], a[3], a[4], a[5], true, true)
    }),
    k32("GetDateFormatA", 6, |t, a| {
        get_date_format(t, a[1] as u32, a[2], a[3], a[4], a[5], false)
    }),
    k32("GetDateFormatW", 6, |t, a| {
        get_date_format(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("GetDateFormatEx", 7, |t, a| {
        get_date_format(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("GetTimeFormatA", 6, |t, a| {
        get_time_format(t, a[1] as u32, a[2], a[3], a[4], a[5], false)
    }),
    k32("GetTimeFormatW", 6, |t, a| {
        get_time_format(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("GetTimeFormatEx", 6, |t, a| {
        get_time_format(t, a[1] as u32, a[2], a[3], a[4], a[5], true)
    }),
    k32("GetNLSVersion", 3, |t, a| {
        // NLSVERSIONINFO: size, NLS version, defined version.
        Ok((write_u32(t, a[2] + 4, 0x0006_0201) && write_u32(t, a[2] + 8, 0x0006_0201)) as u64)
    }),
    k32("GetNLSVersionEx", 3, |t, a| {
        Ok((write_u32(t, a[2] + 4, 0x0006_0201) && write_u32(t, a[2] + 8, 0x0006_0201)) as u64)
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_en_us() {
        let f = NumberFormat::default();
        assert_eq!(format_number("1234567.891", &f).unwrap(), "1,234,567.89");
        assert_eq!(format_number("-0.005", &f).unwrap(), "-0.01");
        assert_eq!(format_number("999.999", &f).unwrap(), "1,000.00");
        assert_eq!(format_number("-0.001", &f).unwrap(), "0.00");
        let indian = NumberFormat {
            grouping: 32,
            digits: 0,
            ..NumberFormat::default()
        };
        assert_eq!(format_number("12345678", &indian).unwrap(), "1,23,45,678");
        assert!(format_number("12a", &f).is_none());
    }

    #[test]
    fn date_and_time_pictures_expand() {
        // Wednesday 2026-10-14 15:04:05.
        let st = [2026, 10, 3, 14, 15, 4, 5, 0];
        assert_eq!(
            format_picture("dddd, MMMM d, yyyy", &st, true, 0),
            "Wednesday, October 14, 2026"
        );
        assert_eq!(
            format_picture("M/d/yy 'at' ddd", &st, true, 0),
            "10/14/26 at Wed"
        );
        assert_eq!(format_picture("h:mm:ss tt", &st, false, 0), "3:04:05 PM");
        assert_eq!(
            format_picture("h:mm:ss tt", &st, false, TIME_NOSECONDS),
            "3:04 PM"
        );
        assert_eq!(
            format_picture("hh:mm tt", &st, false, TIME_FORCE24HOURFORMAT),
            "15:04"
        );
    }

    #[test]
    fn locale_names_cover_days_and_months() {
        assert_eq!(locale_info(0x2A).unwrap(), "Monday");
        assert_eq!(locale_info(0x30).unwrap(), "Sunday");
        assert_eq!(locale_info(0x37).unwrap(), "Sun");
        assert_eq!(locale_info(0x43).unwrap(), "December");
        assert_eq!(locale_info(0x44).unwrap(), "Jan");
    }
}
