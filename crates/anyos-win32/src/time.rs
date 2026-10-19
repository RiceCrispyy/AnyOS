//! Time: system and local time, FILETIME conversions, tick counts and the
//! performance counter.

use std::time::{SystemTime, UNIX_EPOCH};

use anyos_env::clock::{civil_from_days, days_from_civil, weekday};

use crate::api::*;
use crate::thread::{Halt, Thread};

/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const EPOCH_DIFF: u64 = 116_444_736_000_000_000;
const TICKS_PER_SEC: u64 = 10_000_000;
/// The performance counter runs at 10 MHz, as on Windows 10.
const QPC_FREQUENCY: u64 = 10_000_000;

/// FILETIME of a host time.
pub(crate) fn filetime_of(t: SystemTime) -> u64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => EPOCH_DIFF + d.as_secs() * TICKS_PER_SEC + d.subsec_nanos() as u64 / 100,
        Err(e) => {
            let d = e.duration();
            EPOCH_DIFF.saturating_sub(d.as_secs() * TICKS_PER_SEC + d.subsec_nanos() as u64 / 100)
        }
    }
}

pub(crate) fn filetime_now() -> u64 {
    filetime_of(SystemTime::now())
}

/// Host time of a FILETIME.
pub(crate) fn system_time_of(ft: u64) -> SystemTime {
    if ft >= EPOCH_DIFF {
        let d = ft - EPOCH_DIFF;
        UNIX_EPOCH + std::time::Duration::new(d / TICKS_PER_SEC, (d % TICKS_PER_SEC) as u32 * 100)
    } else {
        let d = EPOCH_DIFF - ft;
        UNIX_EPOCH - std::time::Duration::new(d / TICKS_PER_SEC, (d % TICKS_PER_SEC) as u32 * 100)
    }
}

/// SYSTEMTIME fields: year, month, day of week, day, hour, minute, second, ms.
pub(crate) type SystemTimeFields = [u16; 8];

pub(crate) fn filetime_to_fields(ft: u64) -> Option<SystemTimeFields> {
    if ft >= 0x8000_0000_0000_0000 {
        return None;
    }
    let secs = ft / TICKS_PER_SEC;
    let ms = (ft % TICKS_PER_SEC) / 10_000;
    let days = (secs / 86_400) as i64 - 134_774; // days since 1970
    let s = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    if y > 30827 {
        return None;
    }
    Some([
        y as u16,
        m as u16,
        weekday(days) as u16,
        d as u16,
        (s / 3600) as u16,
        (s / 60 % 60) as u16,
        (s % 60) as u16,
        ms as u16,
    ])
}

pub(crate) fn fields_to_filetime(f: &SystemTimeFields) -> Option<u64> {
    let [y, m, _, d, h, mi, s, ms] = *f;
    let days_in_month = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    if !(1601..=30827).contains(&y)
        || d == 0
        || d > days_in_month
        || h > 23
        || mi > 59
        || s > 59
        || ms > 999
    {
        return None;
    }
    let days = days_from_civil(y as i64, m as u32, d as u32) + 134_774;
    let secs = days as u64 * 86_400 + h as u64 * 3600 + mi as u64 * 60 + s as u64;
    Some(secs * TICKS_PER_SEC + ms as u64 * 10_000)
}

/// Local time's offset from UTC, in seconds (east positive), at `unix`.
pub(crate) fn utc_offset(unix: i64) -> i64 {
    #[cfg(unix)]
    {
        let t = unix as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: localtime_r only writes the `tm` we pass.
        if unsafe { !libc::localtime_r(&t, &mut tm).is_null() } {
            return tm.tm_gmtoff as i64;
        }
        0
    }
    #[cfg(not(unix))]
    {
        let _ = unix;
        0
    }
}

fn local_offset_ticks(ft: u64) -> i64 {
    let unix = (ft as i64 - EPOCH_DIFF as i64) / TICKS_PER_SEC as i64;
    utc_offset(unix) * TICKS_PER_SEC as i64
}

pub(crate) fn utc_to_local(ft: u64) -> u64 {
    (ft as i64 + local_offset_ticks(ft)) as u64
}

pub(crate) fn local_to_utc(ft: u64) -> u64 {
    (ft as i64 - local_offset_ticks(ft)) as u64
}

pub(crate) fn read_systemtime(t: &Thread<'_, '_>, p: u64) -> Option<SystemTimeFields> {
    let b = guest_bytes(t, p, 16)?;
    let mut f = [0u16; 8];
    for (i, v) in f.iter_mut().enumerate() {
        *v = u16::from_le_bytes([b[i * 2], b[i * 2 + 1]]);
    }
    Some(f)
}

pub(crate) fn write_systemtime(t: &mut Thread<'_, '_>, p: u64, f: &SystemTimeFields) -> bool {
    let b: Vec<u8> = f.iter().flat_map(|v| v.to_le_bytes()).collect();
    write_bytes(t, p, &b)
}

fn put_systemtime(t: &mut Thread<'_, '_>, p: u64, ft: u64) -> Result<u64, Halt> {
    match filetime_to_fields(ft) {
        Some(f) if write_systemtime(t, p, &f) => Ok(1),
        Some(_) => fail(t, ERROR_NOACCESS, 0),
        None => fail(t, ERROR_INVALID_PARAMETER, 0),
    }
}

/// Milliseconds since "boot": the start of the run plus a day, so programs
/// that subtract tick counts never see small values.
fn tick_count(t: &Thread<'_, '_>) -> u64 {
    86_400_000 + t.started_at.elapsed().as_millis() as u64
}

fn qpc(t: &Thread<'_, '_>) -> u64 {
    let e = t.started_at.elapsed();
    86_400 * QPC_FREQUENCY + e.as_secs() * QPC_FREQUENCY + e.subsec_nanos() as u64 / 100
}

fn file_time_to_dos(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let Some(ft) = read_u64(t, a[0]) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let Some([y, m, _, d, h, mi, s, _]) = filetime_to_fields(ft) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    if !(1980..=2107).contains(&y) {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    }
    let date = (y - 1980) << 9 | m << 5 | d;
    let time = h << 11 | mi << 5 | (s / 2);
    if !write_u16(t, a[1], date) || !write_u16(t, a[2], time) {
        return fail(t, ERROR_NOACCESS, 0);
    }
    Ok(1)
}

fn dos_to_file_time(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (date, time) = (a[0] as u16, a[1] as u16);
    let f = [
        1980 + (date >> 9),
        (date >> 5) & 0xF,
        0,
        date & 0x1F,
        time >> 11,
        (time >> 5) & 0x3F,
        (time & 0x1F) * 2,
        0,
    ];
    match fields_to_filetime(&f) {
        Some(ft) if write_u64(t, a[2], ft) => Ok(1),
        Some(_) => fail(t, ERROR_NOACCESS, 0),
        None => fail(t, ERROR_INVALID_PARAMETER, 0),
    }
}

/// TIME_ZONE_INFORMATION (172 bytes): the host's current offset, no DST rules.
fn get_time_zone_information(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let offset = utc_offset(anyos_env::clock::unix_now());
    let bias = (-offset / 60) as i32;
    let mut b = vec![0u8; 172];
    b[0..4].copy_from_slice(&bias.to_le_bytes());
    let name: Vec<u8> = "Local Time"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    b[4..4 + name.len()].copy_from_slice(&name);
    b[88..88 + name.len()].copy_from_slice(&name);
    if !write_bytes(t, a[0], &b) {
        return fail(t, ERROR_NOACCESS, MINUS_ONE);
    }
    Ok(0) // TIME_ZONE_ID_UNKNOWN
}

fn compare_file_time(t: &mut Thread<'_, '_>, a: &[u64]) -> Result<u64, Halt> {
    let (x, y) = (
        read_u64(t, a[0]).unwrap_or(0),
        read_u64(t, a[1]).unwrap_or(0),
    );
    Ok(match x.cmp(&y) {
        std::cmp::Ordering::Less => MINUS_ONE,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn convert_ft(t: &mut Thread<'_, '_>, a: &[u64], f: fn(u64) -> u64) -> Result<u64, Halt> {
    match read_u64(t, a[0]) {
        Some(v) if write_u64(t, a[1], f(v)) => Ok(1),
        _ => fail(t, ERROR_NOACCESS, 0),
    }
}

fn st_to_local(t: &mut Thread<'_, '_>, a: &[u64], to_local: bool) -> Result<u64, Halt> {
    // (TIME_ZONE_INFORMATION*, in SYSTEMTIME*, out SYSTEMTIME*); only the host zone.
    let Some(f) = read_systemtime(t, a[1]) else {
        return fail(t, ERROR_NOACCESS, 0);
    };
    let Some(ft) = fields_to_filetime(&f) else {
        return fail(t, ERROR_INVALID_PARAMETER, 0);
    };
    let out = if to_local {
        utc_to_local(ft)
    } else {
        local_to_utc(ft)
    };
    put_systemtime(t, a[2], out)
}

pub(crate) static APIS: &[Api] = &[
    k32("GetSystemTime", 1, |t, a| {
        put_systemtime(t, a[0], filetime_now()).map(|_| 0)
    }),
    k32("GetLocalTime", 1, |t, a| {
        put_systemtime(t, a[0], utc_to_local(filetime_now())).map(|_| 0)
    }),
    k32("SetSystemTime", 1, |t, _| fail(t, 1314, 0)), // ERROR_PRIVILEGE_NOT_HELD
    k32("SetLocalTime", 1, |t, _| fail(t, 1314, 0)),
    k32("GetSystemTimeAsFileTime", 1, |t, a| {
        write_u64(t, a[0], filetime_now());
        Ok(0)
    }),
    k32("GetSystemTimePreciseAsFileTime", 1, |t, a| {
        write_u64(t, a[0], filetime_now());
        Ok(0)
    }),
    k32("GetTickCount", 0, |t, _| Ok(tick_count(t) & 0xFFFF_FFFF)),
    k32("GetTickCount64", 0, |t, _| Ok(tick_count(t))),
    k32("QueryPerformanceCounter", 1, |t, a| {
        let v = qpc(t);
        Ok(write_u64(t, a[0], v) as u64)
    }),
    k32("QueryPerformanceFrequency", 1, |t, a| {
        Ok(write_u64(t, a[0], QPC_FREQUENCY) as u64)
    }),
    k32("QueryUnbiasedInterruptTime", 1, |t, a| {
        let v = tick_count(t) * 10_000;
        Ok(write_u64(t, a[0], v) as u64)
    }),
    k32("FileTimeToSystemTime", 2, |t, a| match read_u64(t, a[0]) {
        Some(ft) => put_systemtime(t, a[1], ft),
        None => fail(t, ERROR_NOACCESS, 0),
    }),
    k32("SystemTimeToFileTime", 2, |t, a| {
        let Some(f) = read_systemtime(t, a[0]) else {
            return fail(t, ERROR_NOACCESS, 0);
        };
        match fields_to_filetime(&f) {
            Some(ft) if write_u64(t, a[1], ft) => Ok(1),
            Some(_) => fail(t, ERROR_NOACCESS, 0),
            None => fail(t, ERROR_INVALID_PARAMETER, 0),
        }
    }),
    k32("FileTimeToLocalFileTime", 2, |t, a| {
        convert_ft(t, a, utc_to_local)
    }),
    k32("LocalFileTimeToFileTime", 2, |t, a| {
        convert_ft(t, a, local_to_utc)
    }),
    k32("CompareFileTime", 2, compare_file_time),
    k32("FileTimeToDosDateTime", 3, file_time_to_dos),
    k32("DosDateTimeToFileTime", 3, dos_to_file_time),
    k32("GetTimeZoneInformation", 1, get_time_zone_information),
    k32("GetDynamicTimeZoneInformation", 1, |t, a| {
        write_bytes(t, a[0], &[0; 432]);
        get_time_zone_information(t, a)
    }),
    k32("SystemTimeToTzSpecificLocalTime", 3, |t, a| {
        st_to_local(t, a, true)
    }),
    k32("TzSpecificLocalTimeToSystemTime", 3, |t, a| {
        st_to_local(t, a, false)
    }),
    k32("GetSystemTimeAdjustment", 3, |t, a| {
        store_u32(t, a[0], 156_250);
        store_u32(t, a[1], 156_250);
        store_u32(t, a[2], 1);
        Ok(1)
    }),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_and_systemtime_round_trip() {
        // 2026-10-14 06:15:05.250 UTC, a Wednesday.
        let f = [2026, 10, 3, 14, 6, 15, 5, 250];
        let ft = fields_to_filetime(&f).unwrap();
        assert_eq!(filetime_to_fields(ft), Some(f));
        assert_eq!(fields_to_filetime(&[1601, 1, 0, 1, 0, 0, 0, 0]), Some(0));
        assert_eq!(fields_to_filetime(&[2023, 2, 0, 29, 0, 0, 0, 0]), None);
        let unix = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_791_958_505);
        assert_eq!(filetime_of(unix), ft - 250 * 10_000);
        assert_eq!(system_time_of(filetime_of(unix)), unix);
    }
}
