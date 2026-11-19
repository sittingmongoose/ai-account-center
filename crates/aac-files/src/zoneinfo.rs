//! The display zone's UTC offsets, read from the host's IANA zoneinfo (R5/A2 live Analytics):
//! `Intl.DateTimeFormat` proved the window's offsets in TS; the native service has no JS engine,
//! so it reads the same tzdata files its host ships. Only whole segments are returned (the first
//! at `from`, one per offset change inside the window), through [`aac_num::intl::Zone::proven`];
//! an unreadable, ambiguous or oversized answer is `None` and the caller declines.
//!
//! The parser is TZif (RFC 8536) v1-v4: the 64-bit block when the file has one, else the 32-bit
//! block, then the POSIX TZ footer that governs instants after the last stored transition
//! (`std offset [dst [offset] ,start[/time],end[/time]]`, `M`/`J`/day-of-year rules).
use aac_num::civil;
use aac_num::intl::Zone;
use std::path::{Path, PathBuf};

/// The IANA database folders, in the order the C library searches them.
const FOLDERS: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/etc/zoneinfo",
];
/// A zoneinfo file is a few kilobytes; a damaged one is refused, never read whole.
const MAX_FILE: usize = 256 * 1024;
const DAY_MS: i64 = 86_400_000;

/// A zone name: one or more `/`-separated IANA components, never a path escape or a drive word.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+' | b'.'))
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn folder() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TZDIR") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    FOLDERS.iter().map(PathBuf::from).find(|path| path.is_dir())
}

/// The display zone's segment list `[[start ms, offset minutes], ...]` over `[from, to]`, or
/// `None` when the host has no readable zone for the name or the window cannot be proven.
pub fn segments(time_zone: &str, from: i64, to: i64) -> Option<Vec<[i64; 2]>> {
    if !valid_name(time_zone) || from < 0 || from >= to {
        return None;
    }
    if time_zone == "UTC" {
        return Some(vec![[from, 0]]);
    }
    let path = folder()?.join(time_zone);
    let bytes = read_file(&path)?;
    let table = parse(&bytes)?;
    table.segments(from, to)
}

/// The host knows this zone: `UTC`, or a readable zoneinfo file under a searched folder.
pub fn known(time_zone: &str) -> bool {
    if time_zone == "UTC" {
        return true;
    }
    valid_name(time_zone) && folder().is_some_and(|dir| dir.join(time_zone).is_file())
}

/// Read a regular zoneinfo file, refusing a link, a special file or an oversized one.
fn read_file(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_FILE as u64 {
        return None;
    }
    std::fs::read(path).ok()
}

/// One POSIX TZ rule.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Rule {
    /// `Jn`: day 1-365, February 29 never counted.
    Julian(i64),
    /// `n`: day 0-365, February 29 counted in leap years.
    Day(i64),
    /// `Mm.w.d`: the `w`-th `d` of month `m` (`w` 5 is the last).
    Month { month: i64, week: i64, day: i64 },
}

/// The file's POSIX footer: the standard offset and, when the zone observes DST, its rule pair.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Footer {
    /// Standard offset east of UTC, seconds.
    std_seconds: i64,
    /// DST offset east of UTC and the `(start, end)` rules with their local times (seconds).
    dst: Option<(i64, Rule, i64, Rule, i64)>,
}

/// One parsed TZif table.
#[derive(Debug, PartialEq)]
pub struct Table {
    /// Transition instants (ms, UTC), ascending.
    transitions: Vec<i64>,
    /// The type in effect from each transition (index into `types`).
    indexes: Vec<u8>,
    /// `(utc offset seconds, isdst)` per type.
    types: Vec<(i32, bool)>,
    /// The POSIX footer governing instants after the last transition.
    footer: Option<Footer>,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

struct Header {
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    fn at(bytes: &[u8], base: usize) -> Option<Header> {
        if bytes.get(base..base + 4)? != b"TZif".as_slice() {
            return None;
        }
        let header = Header {
            isutcnt: u32_at(bytes, base + 20)? as usize,
            isstdcnt: u32_at(bytes, base + 24)? as usize,
            leapcnt: u32_at(bytes, base + 28)? as usize,
            timecnt: u32_at(bytes, base + 32)? as usize,
            typecnt: u32_at(bytes, base + 36)? as usize,
            charcnt: u32_at(bytes, base + 40)? as usize,
        };
        if header.timecnt > 100_000
            || header.typecnt == 0
            || header.typecnt > 256
            || header.charcnt > 64 * 1024
            || header.leapcnt > 4096
            || header.isutcnt > header.typecnt
            || header.isstdcnt > header.typecnt
        {
            return None;
        }
        Some(header)
    }

    /// The block's byte length after the header: times, indexes, types, abbrevs, leaps, standards.
    fn body(&self, time_size: usize) -> Option<usize> {
        Some(
            self.timecnt.checked_mul(time_size + 1)?
                + self.typecnt.checked_mul(6)?
                + self.charcnt
                + self.leapcnt.checked_mul(time_size + 4)?
                + self.isstdcnt
                + self.isutcnt,
        )
    }
}

/// Parse one TZif file. Version 2+ uses its 64-bit block; version 1 uses its only block.
pub fn parse(bytes: &[u8]) -> Option<Table> {
    let version = *bytes.get(4)?;
    let first = Header::at(bytes, 0)?;
    let (header, base, time_size) = if matches!(version, b'2' | b'3' | b'4') {
        let second = 44 + first.body(4)?;
        (Header::at(bytes, second)?, second + 44, 8)
    } else {
        (first, 44, 4)
    };
    let times = base;
    let indexes = times + header.timecnt * time_size;
    let types = indexes + header.timecnt;
    if types + header.typecnt * 6 + header.charcnt > bytes.len() {
        return None;
    }
    let mut transitions = Vec::with_capacity(header.timecnt);
    let mut previous = i64::MIN;
    for index in 0..header.timecnt {
        let at = times + index * time_size;
        let ms = if time_size == 8 {
            i64_at(bytes, at)?.checked_mul(1000)?
        } else {
            i32_at(bytes, at)? as i64 * 1000
        };
        if ms <= previous {
            return None;
        }
        previous = ms;
        transitions.push(ms);
    }
    let indexes = bytes.get(indexes..indexes + header.timecnt)?.to_vec();
    if indexes
        .iter()
        .any(|index| *index as usize >= header.typecnt)
    {
        return None;
    }
    let mut kinds = Vec::with_capacity(header.typecnt);
    for index in 0..header.typecnt {
        let offset = i32_at(bytes, types + index * 6)?;
        let isdst = *bytes.get(types + index * 6 + 4)? != 0;
        if offset.abs() > 86_400 {
            return None;
        }
        kinds.push((offset, isdst));
    }
    // The footer starts after the last block's leaps, standards and UT/local indicators.
    let body_end = types + header.typecnt * 6 + header.charcnt;
    let tail_end = body_end + header.leapcnt * (time_size + 4) + header.isstdcnt + header.isutcnt;
    let footer = if matches!(version, b'2' | b'3' | b'4') {
        let tail = bytes.get(tail_end..)?;
        footer(tail)
    } else {
        None
    };
    Some(Table {
        transitions,
        indexes,
        types: kinds,
        footer,
    })
}

fn number(text: &[u8], at: &mut usize) -> Option<i64> {
    let start = *at;
    while text.get(*at).is_some_and(u8::is_ascii_digit) {
        *at += 1;
    }
    if *at == start || *at - start > 3 {
        return None;
    }
    std::str::from_utf8(text.get(start..*at)?)
        .ok()?
        .parse()
        .ok()
}

/// A POSIX `hh[:mm[:ss]]` duration in seconds, with the sign POSIX uses (west of UTC positive).
fn duration(text: &[u8], at: &mut usize) -> Option<i64> {
    let west = match text.get(*at) {
        Some(&b'-') => {
            *at += 1;
            false
        }
        Some(&b'+') => {
            *at += 1;
            true
        }
        _ => true,
    };
    let hours = number(text, at)?;
    let minutes = match text.get(*at) {
        Some(b':') => {
            *at += 1;
            number(text, at)?
        }
        _ => 0,
    };
    let seconds = match text.get(*at) {
        Some(b':') => {
            *at += 1;
            number(text, at)?
        }
        _ => 0,
    };
    if minutes > 59 || seconds > 59 || hours > 167 {
        return None;
    }
    let east = hours * 3600 + minutes * 60 + seconds;
    Some(if west { -east } else { east })
}

fn name(text: &[u8], at: &mut usize) -> Option<()> {
    if text.get(*at) == Some(&b'<') {
        let start = *at + 1;
        let end = text.get(start..)?.iter().position(|b| *b == b'>')? + start;
        if end == start || end - start > 32 {
            return None;
        }
        *at = end + 1;
        return Some(());
    }
    let start = *at;
    while text.get(*at).is_some_and(u8::is_ascii_alphabetic) {
        *at += 1;
    }
    (*at - start >= 3).then_some(())
}

fn rule(text: &[u8], at: &mut usize) -> Option<(Rule, i64)> {
    let parsed = match *text.get(*at)? {
        b'M' => {
            *at += 1;
            let month = number(text, at)?;
            if text.get(*at) != Some(&b'.') {
                return None;
            }
            *at += 1;
            let week = number(text, at)?;
            if text.get(*at) != Some(&b'.') {
                return None;
            }
            *at += 1;
            let day = number(text, at)?;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || day > 6 {
                return None;
            }
            Rule::Month { month, week, day }
        }
        b'J' => {
            *at += 1;
            let day = number(text, at)?;
            if !(1..=365).contains(&day) {
                return None;
            }
            Rule::Julian(day)
        }
        _ => {
            let day = number(text, at)?;
            if !(0..=365).contains(&day) {
                return None;
            }
            Rule::Day(day)
        }
    };
    let time_seconds = match text.get(*at) {
        Some(b'/') => {
            *at += 1;
            duration(text, at)?
        }
        _ => 2 * 3600,
    };
    Some((parsed, time_seconds))
}

/// Parse the footer text (which the file wraps in newlines) into its offsets and rules.
fn footer(tail: &[u8]) -> Option<Footer> {
    let start = tail.iter().position(|b| *b == b'\n')? + 1;
    let end = start + tail.get(start..)?.iter().position(|b| *b == b'\n')?;
    let text = tail.get(start..end)?;
    if text.is_empty() {
        return None;
    }
    let mut at = 0usize;
    name(text, &mut at)?;
    let std_seconds = duration(text, &mut at)?;
    if at == text.len() {
        return Some(Footer {
            std_seconds,
            dst: None,
        });
    }
    name(text, &mut at)?;
    let dst_seconds = match text.get(at) {
        Some(b',') => std_seconds + 3600,
        _ => duration(text, &mut at)?,
    };
    if text.get(at) != Some(&b',') {
        return None;
    }
    at += 1;
    let (start_rule, start_time) = rule(text, &mut at)?;
    if text.get(at) != Some(&b',') {
        return None;
    }
    at += 1;
    let (end_rule, end_time) = rule(text, &mut at)?;
    if at != text.len() {
        return None;
    }
    Some(Footer {
        std_seconds,
        dst: Some((dst_seconds, start_rule, start_time, end_rule, end_time)),
    })
}

/// The local wall-clock date a rule names in `year`, as a day count from 1970-01-01.
fn rule_day(year: i64, rule: Rule) -> Option<i64> {
    match rule {
        Rule::Julian(day) => {
            let leap = civil::days_in_month(year, 2) == 29;
            let day = if leap && day >= 60 { day + 1 } else { day };
            Some(civil::days_from_civil(year, 1, 1) + day - 1)
        }
        Rule::Day(day) => Some(civil::days_from_civil(year, 1, 1) + day),
        Rule::Month { month, week, day } => {
            let first = civil::days_from_civil(year, month, 1);
            let length = civil::days_in_month(year, month);
            let mut date = first + (day - civil::weekday(first) as i64).rem_euclid(7);
            date += (week - 1) * 7;
            while date >= first + length {
                date -= 7;
            }
            (date >= first).then_some(date)
        }
    }
}

/// The UTC instant a rule fires in `year`: its local wall time minus the offset in effect before.
fn rule_instant(year: i64, rule: Rule, time_seconds: i64, before_seconds: i64) -> Option<i64> {
    Some(rule_day(year, rule)?.checked_mul(DAY_MS)? + time_seconds * 1000 - before_seconds * 1000)
}

impl Table {
    /// The type in effect at `ms`: the last transition at or before it, else the first type.
    fn kind_at(&self, ms: i64) -> usize {
        match self.transitions.partition_point(|at| *at <= ms) {
            0 => self.first_standard(),
            at => self.indexes[at - 1] as usize,
        }
    }

    /// TZif's type before the first transition: the first standard type, else the first type.
    fn first_standard(&self) -> usize {
        self.types
            .iter()
            .position(|(_, isdst)| !*isdst)
            .unwrap_or(0)
    }

    fn offset_seconds(&self, ms: i64) -> Option<i64> {
        let past_last = self.transitions.last().is_none_or(|last| ms > *last);
        if !past_last {
            return Some(i64::from(self.types[self.kind_at(ms)].0));
        }
        let Some(footer) = self.footer else {
            return self
                .transitions
                .last()
                .map(|_| i64::from(self.types[self.kind_at(ms)].0));
        };
        let Some((dst_seconds, start_rule, start_time, end_rule, end_time)) = footer.dst else {
            return Some(footer.std_seconds);
        };
        // The rule's own year: a DST season never crosses two year boundaries.
        let year = civil::civil_from_days(ms.div_euclid(DAY_MS)).0;
        let mut latest: Option<(i64, i64)> = None;
        for candidate in (year - 1)..=(year + 1) {
            for (rule, time, before, after) in [
                (start_rule, start_time, footer.std_seconds, dst_seconds),
                (end_rule, end_time, dst_seconds, footer.std_seconds),
            ] {
                let at = rule_instant(candidate, rule, time, before)?;
                if at <= ms && latest.is_none_or(|(previous, _)| at >= previous) {
                    latest = Some((at, after));
                }
            }
        }
        Some(latest.map_or(footer.std_seconds, |(_, offset)| offset))
    }

    /// The instants where the offset changes inside `(from, to]`.
    fn changes(&self, from: i64, to: i64) -> Option<Vec<i64>> {
        let mut instants: Vec<i64> = self
            .transitions
            .iter()
            .copied()
            .skip(self.transitions.partition_point(|at| *at <= from))
            .take_while(|at| *at <= to)
            .collect();
        let last = self.transitions.last().copied();
        if last.is_none_or(|last| to > last)
            && let Some(Footer {
                std_seconds,
                dst: Some((dst_seconds, start, start_time, end, end_time)),
            }) = self.footer
        {
            let first_year = civil::civil_from_days(from.div_euclid(DAY_MS)).0 - 1;
            let last_year = civil::civil_from_days(to.div_euclid(DAY_MS)).0 + 1;
            for year in first_year..=last_year {
                for (rule, time, before) in [
                    (start, start_time, std_seconds),
                    (end, end_time, dst_seconds),
                ] {
                    let at = rule_instant(year, rule, time, before)?;
                    if at > from && at <= to {
                        instants.push(at);
                    }
                }
            }
        }
        instants.sort_unstable();
        instants.dedup();
        Some(instants)
    }

    /// The proven segment list over `[from, to]`: the offset at `from`, then one per change.
    pub fn segments(&self, from: i64, to: i64) -> Option<Vec<[i64; 2]>> {
        if self.transitions.is_empty() && self.footer.is_none() {
            return None;
        }
        let mut rows: Vec<[i64; 2]> = Vec::new();
        let mut current = self.offset_seconds(from)? / 60;
        rows.push([from, current]);
        for at in self.changes(from, to)? {
            let minutes = self.offset_seconds(at)? / 60;
            if minutes != current {
                rows.push([at, minutes]);
                current = minutes;
            }
        }
        (rows.len() <= 64).then_some(rows)
    }
}

/// The proven zone for `time_zone` over the Analytics window
/// (`analyticsZoneFacts`: `floor(from/DAY)*DAY - DAY .. floor(now/DAY)*DAY + 2*DAY`).
pub fn zone_for_window(time_zone: &str, window_from: i64, now: i64) -> Option<Zone> {
    let from = window_from.div_euclid(DAY_MS) * DAY_MS - DAY_MS;
    let to = now.div_euclid(DAY_MS) * DAY_MS + 2 * DAY_MS;
    let rows = segments(time_zone, from, to)?;
    Zone::proven(from, to, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal TZif v2 file with a 64-bit block and a POSIX footer.
    fn tzif(times: &[i64], indexes: &[u8], offsets_seconds: &[i32], footer_text: &str) -> Vec<u8> {
        fn header(out: &mut Vec<u8>, timecnt: usize, typecnt: usize) {
            out.extend_from_slice(b"TZif2");
            out.extend_from_slice(&[0; 15]);
            for count in [0u32, 0, 0, timecnt as u32, typecnt as u32, 0] {
                out.extend_from_slice(&count.to_be_bytes());
            }
        }
        let mut out = Vec::new();
        header(&mut out, 0, 1);
        out.extend_from_slice(&0i32.to_be_bytes());
        out.push(0);
        out.push(0);
        header(&mut out, times.len(), offsets_seconds.len());
        for time in times {
            out.extend_from_slice(&(time / 1000).to_be_bytes());
        }
        out.extend_from_slice(indexes);
        for offset in offsets_seconds {
            out.extend_from_slice(&offset.to_be_bytes());
            out.push(0);
            out.push(0);
        }
        out.push(b'\n');
        out.extend_from_slice(footer_text.as_bytes());
        out.push(b'\n');
        out
    }

    #[test]
    fn segments_start_at_the_window_and_change_at_each_transition() {
        let day = DAY_MS;
        let table = parse(&tzif(
            &[10 * day, 20 * day],
            &[1, 0],
            &[-18_000, -14_400],
            "EST5",
        ))
        .expect("tzif");
        let rows = table.segments(5 * day, 25 * day).expect("segments");
        assert_eq!(
            rows,
            vec![[5 * day, -300], [10 * day, -240], [20 * day, -300]]
        );
        // A window entirely after the last transition uses the footer.
        assert_eq!(
            table.segments(21 * day, 25 * day).expect("one"),
            vec![[21 * day, -300]]
        );
        // The same offset twice is not a change.
        let flat = parse(&tzif(&[10 * day], &[0], &[-18_000], "EST5")).expect("tzif");
        assert_eq!(
            flat.segments(5 * day, 25 * day).expect("flat"),
            vec![[5 * day, -300]]
        );
    }

    #[test]
    fn the_posix_footer_rules_each_year() {
        let table = parse(&tzif(&[0], &[0], &[-18_000], "EST5EDT,M3.2.0,M11.1.0")).expect("tzif");
        // 2026: DST starts Sunday March 8 at 02:00 EST and ends Sunday November 1 at 02:00 EDT.
        let year = 2026;
        let start = rule_instant(
            year,
            Rule::Month {
                month: 3,
                week: 2,
                day: 0,
            },
            7200,
            -18_000,
        )
        .expect("start");
        let end = rule_instant(
            year,
            Rule::Month {
                month: 11,
                week: 1,
                day: 0,
            },
            7200,
            -14_400,
        )
        .expect("end");
        assert_eq!(rows_utc(start), (2026, 3, 8, 7, 0));
        assert_eq!(rows_utc(end), (2026, 11, 1, 6, 0));
        let rows = table
            .segments(start - DAY_MS, end + DAY_MS)
            .expect("segments");
        assert_eq!(rows[0][1], -300);
        assert!(
            rows.iter()
                .any(|[at, minutes]| *at == start && *minutes == -240)
        );
        assert!(
            rows.iter()
                .any(|[at, minutes]| *at == end && *minutes == -300)
        );
    }

    fn rows_utc(ms: i64) -> (i64, i64, i64, i64, i64) {
        let (y, m, d, h, minute, _, _) = civil::utc_fields(ms);
        (y, m, d, h, minute)
    }

    #[test]
    fn names_and_versions_are_bounded() {
        for name in ["America/New_York", "UTC", "Etc/GMT+5", "Asia/Kolkata"] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "/etc/passwd",
            "America/../etc/passwd",
            "a//b",
            "C:\\x",
            "a b",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        assert!(parse(b"not a tzif file").is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn a_fixed_offset_zone_answers_without_transitions() {
        let table = parse(&tzif(&[], &[], &[0], "JST-9")).expect("tzif");
        assert_eq!(table.segments(0, DAY_MS).expect("seg"), vec![[0, 540]]);
    }

    #[cfg(unix)]
    #[test]
    fn the_host_zone_is_read_for_a_real_window() {
        let now = 1_795_046_400_000i64; // 2026-11-19T00:00:00Z
        let Some(rows) = segments("America/New_York", now - 31 * DAY_MS, now + DAY_MS) else {
            // A host without tzdata proves nothing here; the route declines there too.
            return;
        };
        // The window starts in EDT (Oct 19) and ends in EST: one change, on November 1.
        assert_eq!(rows[0][1], -240);
        assert_eq!(rows.last().map(|row| row[1]), Some(-300));
        assert!(
            rows.windows(2)
                .all(|w| w[1][1] != w[0][1] && w[1][0] > w[0][0])
        );
    }
}
