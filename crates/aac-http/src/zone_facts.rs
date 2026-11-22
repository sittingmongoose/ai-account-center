//! The display zone's UTC offsets over a window of days (`serverZoneFacts`,
//! `src/web-server/views/home-zone.ts` and `view-model.mjs` `homeZoneFacts`): what the TypeScript
//! front door read from `Intl` and sent with each owned Status request.
//!
//! The algorithm is the reference's: an offset for the window's first instant, then a six-hour
//! walk; where the offset changes, a one-second bisection finds the transition. The offsets are
//! the zone's own IANA rules (chrono-tz), which are the rules `Intl` applies.

use aac_num::intl::Zone;
use chrono::Offset;

const ZONE_DAY: i64 = 86_400_000;
const ZONE_STEP: i64 = 6 * 3_600_000;
/// `HOME_ZONE_DAYS` in `view-model.mjs` (the page's zone facts; pinned by tables.test.ts).
const PAST_DAYS: i64 = 120;
const FUTURE_DAYS: i64 = 400;

/// One zone's offsets over the reference's window.
pub(crate) struct ZoneOffsets {
    pub from: i64,
    pub to: i64,
    pub offsets: Vec<[i64; 2]>,
    pub zone: Zone,
}

/// The offsets of `time_zone` from `from` to `to`, as `[start, minutes west of UTC]` rows.
/// `None` for a name the zone database does not hold (the reference's `Intl` throw).
pub(crate) fn server_zone_facts(time_zone: &str, now: i64) -> Option<ZoneOffsets> {
    let day = now.div_euclid(ZONE_DAY) * ZONE_DAY;
    let from = day - PAST_DAYS * ZONE_DAY;
    let to = day + FUTURE_DAYS * ZONE_DAY;
    window_facts(time_zone, from, to)
}

/// The display zone the Status producers format in: the saved preference, else the reference's
/// default (`America/New_York`). A zone the database does not hold declines (`None`).
pub(crate) fn status_zone(root: &std::path::Path, now: i64) -> Option<(String, ZoneOffsets)> {
    let name = crate::account_reads::preferences(root)
        .and_then(|preferences| {
            preferences
                .get("timeZone")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "America/New_York".to_owned());
    let facts = server_zone_facts(&name, now)?;
    Some((name, facts))
}

pub(crate) fn canonical_name(name: &str) -> String {
    aac_files::zoneinfo::canonical_name(name).unwrap_or_else(|| match name {
        "US/Eastern" => "America/New_York".to_owned(),
        "US/Central" => "America/Chicago".to_owned(),
        "US/Mountain" => "America/Denver".to_owned(),
        "US/Pacific" => "America/Los_Angeles".to_owned(),
        _ => name.to_owned(),
    })
}

/// Use the bundled IANA rules when the host has no TZif database.
pub(crate) fn analytics_zone(time_zone: &str, from: i64, now: i64) -> Option<Zone> {
    let from = from.div_euclid(ZONE_DAY) * ZONE_DAY - ZONE_DAY;
    let to = now.div_euclid(ZONE_DAY) * ZONE_DAY + 2 * ZONE_DAY;
    window_facts(time_zone, from, to).map(|facts| facts.zone)
}

fn window_facts(time_zone: &str, from: i64, to: i64) -> Option<ZoneOffsets> {
    let zone: chrono_tz::Tz = time_zone.parse().ok()?;
    let offset = |ms: i64| -> Option<i64> {
        let at = chrono::DateTime::from_timestamp_millis(ms)?;
        Some(i64::from(at.with_timezone(&zone).offset().fix().local_minus_utc()) / 60)
    };
    let mut offsets: Vec<[i64; 2]> = vec![[from, offset(from)?]];
    let mut t = from;
    while t < to {
        let current = offsets.last()?[1];
        let next = (t + ZONE_STEP).min(to);
        if offset(next)? == current {
            t = next;
            continue;
        }
        let (mut lo, mut hi) = (t / 1000, next / 1000);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if offset(mid * 1000)? == current {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        offsets.push([hi * 1000, offset(hi * 1000)?]);
        if offsets.len() > 64 {
            return None;
        }
        t = hi * 1000;
    }
    let zone = Zone::proven(from, to, &offsets)?;
    Some(ZoneOffsets {
        from,
        to,
        offsets,
        zone,
    })
}

/// `isCalendarDate`: a real `YYYY-MM-DD` day, 1970 or later.
pub(crate) fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let parsed = (
        value[..4].parse::<i32>(),
        value[5..7].parse::<u32>(),
        value[8..10].parse::<u32>(),
    );
    let (Ok(year), Ok(month), Ok(day)) = parsed else {
        return false;
    };
    year >= 1970 && chrono::NaiveDate::from_ymd_opt(year, month, day).is_some()
}

/// The next calendar day's text (`addCalendarDays(date, 1)`).
pub(crate) fn next_day(date: &str) -> Option<String> {
    let year = date.get(0..4)?.parse::<i32>().ok()?;
    let month = date.get(5..7)?.parse::<u32>().ok()?;
    let day = date.get(8..10)?.parse::<u32>().ok()?;
    let next = chrono::NaiveDate::from_ymd_opt(year, month, day)?.succ_opt()?;
    Some(next.format("%Y-%m-%d").to_string())
}

/// `localMidnight(date, zone)`: the first instant of a local calendar day, DST skips
/// included. The instant is found on the zone's own rules, for any date (the Signboard's
/// filters are not bounded by the page's offset window).
pub(crate) fn local_midnight(date: &str, time_zone: &str) -> Option<i64> {
    let zone: chrono_tz::Tz = time_zone.parse().ok()?;
    let year = date.get(0..4)?.parse::<i32>().ok()?;
    let month = date.get(5..7)?.parse::<u32>().ok()?;
    let day = date.get(8..10)?.parse::<u32>().ok()?;
    let target = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
    let local = |ms: i64| -> Option<chrono::NaiveDate> {
        let at = chrono::DateTime::from_timestamp_millis(ms)?;
        Some(at.with_timezone(&zone).date_naive())
    };
    // The day is inside [UTC midnight - 26 h, UTC midnight + 26 h] for every zone.
    let utc = target.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis();
    let (mut lo, mut hi) = (utc - 26 * 3_600_000, utc + 26 * 3_600_000);
    if local(hi)? < target {
        return None;
    }
    while hi - lo > 1000 {
        let mid = lo + (hi - lo) / 2;
        if local(mid)? >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    // The first whole second whose local date is the target (a skipped midnight starts the
    // day at the transition instant, which is whole-second in every IANA zone).
    let mut at = lo - (lo.rem_euclid(1000));
    while local(at)? < target {
        at += 1000;
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A winter and a summer instant in a zone with daylight saving and one without.
    #[test]
    fn the_offsets_are_the_zones_own_rules() {
        let winter = 1_767_225_600_000; // 2026-01-01T00:00:00Z
        let summer = 1_780_000_000_000; // 2026-05-01T00:00:00Z-ish
        let new_york = server_zone_facts("America/New_York", winter).unwrap();
        assert_eq!(new_york.zone.offset_minutes(winter), Some(-300));
        assert_eq!(new_york.zone.offset_minutes(summer), Some(-240));
        let utc = server_zone_facts("UTC", winter).unwrap();
        assert_eq!(utc.zone.offset_minutes(winter), Some(0));
        assert_eq!(utc.zone.offset_minutes(summer), Some(0));
        assert!(server_zone_facts("Not/AZone", winter).is_none());
    }

    #[test]
    fn local_midnight_is_the_zones_own_day_start() {
        assert_eq!(local_midnight("2026-01-01", "UTC"), Some(1_767_225_600_000));
        assert_eq!(
            local_midnight("2026-01-01", "America/New_York"),
            Some(1_767_225_600_000 + 5 * 3_600_000)
        );
        assert_eq!(
            local_midnight("2026-07-01", "America/New_York"),
            Some(1_782_864_000_000 + 4 * 3_600_000)
        );
        assert_eq!(local_midnight("2026-02-30", "UTC"), None);
        assert_eq!(next_day("2026-12-31").as_deref(), Some("2027-01-01"));
        assert!(is_calendar_date("2026-02-28"));
        assert!(!is_calendar_date("2026-02-30"));
        assert!(!is_calendar_date("1969-12-31"));
    }

    /// The documented window: the past 120 days and the next 400, day-aligned.
    #[test]
    fn the_window_is_the_references() {
        let now = 1_767_225_600_000 + 3_600_000;
        let facts = server_zone_facts("Europe/Berlin", now).unwrap();
        assert_eq!(
            facts.from,
            now.div_euclid(ZONE_DAY) * ZONE_DAY - PAST_DAYS * ZONE_DAY
        );
        assert_eq!(
            facts.to,
            now.div_euclid(ZONE_DAY) * ZONE_DAY + FUTURE_DAYS * ZONE_DAY
        );
    }
}
