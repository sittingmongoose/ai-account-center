//! Proleptic Gregorian day arithmetic (H. Hinnant's algorithms), shared by the ECMAScript and Python date forms.
//! Days count from 1970-01-01; every function is exact over the `i64` ranges the date forms admit.

/// Milliseconds in a day.
pub const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 of a year, month (1-12) and day.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The year, month (1-12) and day `days` after 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days in a month (1-12) of a year.
pub fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The weekday of a day count, 0 = Sunday (1970-01-01 was a Thursday).
pub fn weekday(days: i64) -> usize {
    (days + 4).rem_euclid(7) as usize
}

/// A UTC instant in milliseconds as `(year, month, day, hour, minute, second, millisecond)`.
pub fn utc_fields(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let (y, m, d) = civil_from_days(ms.div_euclid(DAY_MS));
    let of_day = ms.rem_euclid(DAY_MS);
    (
        y,
        m,
        d,
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000,
    )
}

/// The English short month names, January first.
pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The English short weekday names, Sunday first.
pub const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip_across_eras() {
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert!((1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d));
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(weekday(days_from_civil(2026, 11, 15)), 0);
        assert_eq!(utc_fields(-1), (1969, 12, 31, 23, 59, 59, 999));
    }
}
