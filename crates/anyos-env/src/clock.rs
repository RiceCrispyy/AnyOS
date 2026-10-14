//! Calendar arithmetic shared by the install index and the Windows time APIs,
//! so AnyOS needs no date crate. Dates are proleptic Gregorian, in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// 0 = Sunday, like Windows' `SYSTEMTIME::wDayOfWeek`.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

/// Seconds since the Unix epoch, now.
pub fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// `2026-10-14T06:15:05Z` for a Unix time.
pub fn format_rfc3339(unix: i64) -> String {
    let (y, mo, d) = civil_from_days(unix.div_euclid(86_400));
    let s = unix.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// Parse what [`format_rfc3339`] writes back into a Unix time.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo as u32, da as u32) * 86_400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1601, 1, 1), -134_774);
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(weekday(days_from_civil(2026, 10, 14)), 3); // a Wednesday
        for d in (-200_000..200_000).step_by(997) {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d);
        }
    }

    #[test]
    fn rfc3339_round_trips() {
        let t = 1_791_958_505; // 2026-10-14T06:15:05Z
        assert_eq!(format_rfc3339(t), "2026-10-14T06:15:05Z");
        assert_eq!(parse_rfc3339("2026-10-14T06:15:05Z"), Some(t));
        assert_eq!(parse_rfc3339("yesterday"), None);
    }
}
