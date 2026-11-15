//! JavaScript semantics the Rust compositions rely on, reproduced exactly or refused. One set of
//! helpers serves both callers: the M3 profile read routes (string patterns, the envelope's
//! `Date.parse`, `toISOString`) and Home's view model (D7: values, strings, browser-side dates, the
//! page's en-US formatters and numbers).
//!
//! Every helper either gives the answer JavaScript gives for the same input or refuses: the caller then serves
//! today's JavaScript path instead. Nothing here guesses: a date string outside the forms a helper admits, a lookup
//! key that is an `Object.prototype` name, a non-ASCII collation pair or a missing presentation fact all refuse.
//! Display-zone clock and day words, `Date.parse` epochs and `localeCompare` ranks come from the shared M3
//! presentation envelope ([`Context`]); the page's own formatter style comes from its format probe.
pub use crate::spread::{truthy, utf16_len};
pub use aac_status::read_context::js_whitespace;
use aac_status::read_context::{Context, PROTO_NAMES};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;

/// Why a view could not be proven equal to the JavaScript one. The word is logged; no data is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decline(pub &'static str);

/// Result of a proven computation.
pub type R<T> = Result<T, Decline>;

// ---------------------------------------------------------------- values

/// `a?.b` on a JSON value: only objects have properties here (arrays and primitives give `undefined`).
pub fn get<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match v {
        Some(Value::Object(map)) => map.get(key),
        _ => None,
    }
}

/// The object behind a value, if it is one.
pub fn obj(v: Option<&Value>) -> Option<&Map<String, Value>> {
    match v {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// `typeof v === 'string' ? v : ''`.
pub fn text(v: Option<&Value>) -> &str {
    match v {
        Some(Value::String(s)) => s,
        _ => "",
    }
}

/// A field the views coerce as text: a string, or `null`/absent. Anything else declines.
pub fn str_field(v: Option<&Value>) -> R<Option<&str>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(Decline("type")),
    }
}

/// `typeof v === 'number' && Number.isFinite(v)`, as the number.
pub fn finite(v: Option<&Value>) -> Option<f64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|x| x.is_finite()),
        _ => None,
    }
}

/// `a === b` between two values read from JSON. Distinct objects and arrays are never identical.
pub fn strict_eq(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x == y,
        (Some(Value::String(x)), Some(Value::String(y))) => x == y,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        _ => false,
    }
}

/// `value === true`.
pub fn is_true(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
}

/// `v === false`.
pub fn is_false(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(false)))
}

/// `v === 'word'`.
pub fn is_str(v: Option<&Value>, word: &str) -> bool {
    matches!(v, Some(Value::String(s)) if s == word)
}

/// `Array.isArray(v) ? v : []`.
pub fn array(v: Option<&Value>) -> &[Value] {
    match v {
        Some(Value::Array(list)) => list,
        _ => &[],
    }
}

/// `{...}[key]` on an object literal: an `Object.prototype` name would find a function there, so it declines.
pub fn lookup<'m>(table: &[(&str, &'m str)], key: &str) -> R<Option<&'m str>> {
    if PROTO_NAMES.contains(&key) {
        return Err(Decline("proto-key"));
    }
    Ok(table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v))
}

// ---------------------------------------------------------------- strings

/// What `.` never matches.
pub fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `.` as one UTF-16 unit: a BMP character that is not a line terminator.
pub fn dot(c: char) -> bool {
    (c as u32) <= 0xffff && !is_line_terminator(c)
}

/// `/\w/` without the `u` flag.
pub fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `String.prototype.trim`: ECMAScript WhiteSpace and LineTerminator (BOM yes, NEL no).
pub fn trim(text: &str) -> &str {
    text.trim_matches(js_whitespace)
}

/// `/[\u0000-\u001f\u007f]/.test(text)`.
pub fn has_control(text: &str) -> bool {
    text.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
}

/// `/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(text)`: one `@` with text before it, and a dot after it
/// that is neither the first nor the last character, all without whitespace.
pub fn email_shape(text: &str) -> bool {
    if text.chars().any(js_whitespace) {
        return false;
    }
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && domain
            .char_indices()
            .any(|(at, c)| c == '.' && at > 0 && at + 1 < domain.len())
}

/// A whole-string match of `first` then `rest*` over ASCII bytes, with a length bound on `rest`.
pub fn ascii_word(
    text: &str,
    first: fn(u8) -> bool,
    rest: fn(u8) -> bool,
    max_rest: usize,
) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && first(bytes[0])
        && bytes.len() - 1 <= max_rest
        && bytes[1..].iter().all(|b| rest(*b))
}

/// `s.split(/\s+/).filter(w => w !== '')`.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(js_whitespace).filter(|w| !w.is_empty()).collect()
}

/// Whether `hay[at..]` starts with the ASCII `needle`, ignoring ASCII case (the `i` flag without `u`).
pub fn ci_at(hay: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    at + n.len() <= hay.len()
        && hay[at..at + n.len()]
            .iter()
            .zip(&n)
            .all(|(h, p)| h.eq_ignore_ascii_case(p) && (h.is_ascii() || h == p))
}

/// `/needle/i.test(s)` for an ASCII literal needle.
pub fn ci_contains(s: &str, needle: &str) -> bool {
    let hay: Vec<char> = s.chars().collect();
    (0..=hay.len()).any(|i| ci_at(&hay, i, needle))
}

/// `word[0].toUpperCase() + word.slice(1)`: ASCII and astral first letters exactly; a BMP letter with a case mapping
/// declines (Unicode tables could differ from the browser's).
pub fn title_word(word: &str) -> R<String> {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return Ok(String::new());
    };
    let rest = chars.as_str();
    if first.is_ascii() {
        return Ok(format!("{}{rest}", first.to_ascii_uppercase()));
    }
    if (first as u32) > 0xffff {
        return Ok(word.to_string());
    }
    let mut upper = first.to_uppercase();
    if upper.next() == Some(first) && upper.next().is_none() {
        Ok(word.to_string())
    } else {
        Err(Decline("case-map"))
    }
}

/// `String(v)` for a primitive; objects decline.
pub fn string_of(v: Option<&Value>) -> R<String> {
    Ok(match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n
            .as_f64()
            .and_then(aac_status::read_context::js_number)
            .ok_or(Decline("number"))?,
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(Decline("type")),
    })
}

// ---------------------------------------------------------------- dates

/// `/^\d{4}-\d{2}-\d{2}T/`: the strings the presentation envelope gives a `Date.parse` epoch.
pub fn iso_prefix(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
}

/// `Date.parse(text)` from the envelope: `Some(None)` is NaN (the engine supplied no finite
/// epoch for an ISO-prefixed string, or the string is empty); any other string is one only
/// the engine's legacy parser decides, so it refuses (`None`).
pub fn date_parse(ctx: &Context, text: &str) -> Option<Option<i64>> {
    if text.is_empty() {
        return Some(None);
    }
    iso_prefix(text).then(|| ctx.epoch(&json!(text)))
}

/// `new Date(ms).toISOString()` for years 1970 through 9999 (`0 <= ms <= 253402300799999`);
/// any other time value refuses rather than guessing the extended-year form.
pub fn iso(ms: i64) -> Option<String> {
    if !(0..=253_402_300_799_999).contains(&ms) {
        return None;
    }
    let days = ms.div_euclid(86_400_000);
    let of_day = ms.rem_euclid(86_400_000);
    // Proleptic Gregorian civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    ))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar date `days` after 1970-01-01 (the inverse of [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
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

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `Date.parse(s)` as the browser reads it, for a view the page draws (Home): unlike [`date_parse`], which takes
/// the server engine's epoch from the envelope, only the forms every engine reads alike are admitted. That is the
/// ECMAScript date-time string format with an explicit zone: `YYYY-MM-DDTHH:mm`, with `:ss`
/// and `.sss`, ending in `Z` or `±HH:mm`. `""` is NaN everywhere (`None`). These forms are engine-independent by the
/// spec, so the browser reads the same instant the server's envelope did. Local-time forms (no zone) differ by
/// engine and machine, other spellings are implementation-defined, and date-only forms have no clock or day words in
/// the envelope: all decline.
pub fn parse_date(s: &str) -> R<Option<i64>> {
    if s.is_empty() {
        return Ok(None);
    }
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let bad = Err(Decline("date"));
    let Some(year) = num(0, 4) else { return bad };
    let (mut month, mut day, mut at) = (1, 1, 4);
    if b.get(at) == Some(&b'-') {
        let Some(m) = num(at + 1, 2) else { return bad };
        month = m;
        at += 3;
        if b.get(at) == Some(&b'-') {
            let Some(d) = num(at + 1, 2) else { return bad };
            day = d;
            at += 3;
        }
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return bad;
    }
    let mut ms = days_from_civil(year, month, day) * 86_400_000;
    // Date-only forms are UTC by the spec, but the M3 envelope carries clock and day words only for date-time
    // strings, so their instant could not be formatted: decline.
    if at != 10 || b.get(at) != Some(&b'T') {
        return bad;
    }
    let (Some(h), Some(mi)) = (num(11, 2), num(14, 2)) else {
        return bad;
    };
    if b.get(13) != Some(&b':') || h > 23 || mi > 59 {
        return bad;
    }
    ms += h * 3_600_000 + mi * 60_000;
    at = 16;
    if b.get(at) == Some(&b':') {
        let Some(sec) = num(at + 1, 2) else {
            return bad;
        };
        if sec > 59 {
            return bad;
        }
        ms += sec * 1000;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let Some(frac) = num(at + 1, 3) else {
                return bad;
            };
            ms += frac;
            at += 4;
        }
    }
    match b.get(at) {
        Some(b'Z') if at + 1 == b.len() => Ok(Some(ms)),
        Some(sign @ (b'+' | b'-')) if at + 6 == b.len() && b.get(at + 3) == Some(&b':') => {
            let (Some(oh), Some(om)) = (num(at + 1, 2), num(at + 4, 2)) else {
                return bad;
            };
            if oh > 23 || om > 59 {
                return bad;
            }
            let offset = (oh * 60 + om) * 60_000;
            Ok(Some(if *sign == b'+' {
                ms - offset
            } else {
                ms + offset
            }))
        }
        _ => bad,
    }
}

/// `validDate(v)`: a string `Date.parse` reads, as its time.
pub fn valid_date(v: Option<&Value>) -> R<Option<i64>> {
    match v {
        Some(Value::String(s)) => parse_date(s),
        _ => Ok(None),
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// How this browser's en-US date formatters join the pieces: the space before AM/PM (U+0020 in Chrome, Node and
/// Safari; U+202F in older ICU) and the joiner between the date and the time (", " in Chrome and Node, " at " in iOS
/// Safari: "Jan 5 at 12:07 AM").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    /// Before AM/PM.
    pub sep: &'static str,
    /// Between "Jan 5" and "3:05 PM".
    pub joiner: &'static str,
}

/// The styles the probe can prove; anything else declines.
const STYLES: [Style; 4] = [
    Style {
        sep: " ",
        joiner: ", ",
    },
    Style {
        sep: "\u{202f}",
        joiner: ", ",
    },
    Style {
        sep: " ",
        joiner: " at ",
    },
    Style {
        sep: "\u{202f}",
        joiner: " at ",
    },
];

/// The page's display-zone offsets (view-model.mjs `homeZoneFacts`), checked for shape. The format probe proves only
/// how the page's formatters punctuate; another engine's time zone data can put a zone's clock an hour elsewhere
/// (Africa/Casablanca from 2026-09-20 in one tzdata, not in another), so every clock and day word the server's facts
/// give must be the one these offsets give, or the build declines (`zone`). An instant outside the window declines too.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    from: i64,
    to: i64,
    /// `(start ms, offset minutes)`, starts strictly increasing, the first at `from`.
    offsets: Vec<(i64, i64)>,
}

impl Zone {
    /// The page's facts, or a decline when they are not one ordered, bounded segment list over a real window.
    pub fn proven(facts: &aac_api::home::HomeZone) -> R<Zone> {
        let bad = Err(Decline("zone"));
        let rows = &facts.offsets;
        if facts.from >= facts.to
            || facts.from < 0
            || facts.to > 8_640_000_000_000_000
            || rows.is_empty()
            || rows.len() > 64
            || rows[0][0] != facts.from
            || rows
                .iter()
                .any(|[start, minutes]| *start > facts.to || minutes.abs() > 1080)
            || rows
                .windows(2)
                .any(|w| w[1][0] <= w[0][0] || w[1][1] == w[0][1])
        {
            return bad;
        }
        Ok(Zone {
            from: facts.from,
            to: facts.to,
            offsets: rows
                .iter()
                .map(|[start, minutes]| (*start, *minutes))
                .collect(),
        })
    }

    /// The en-US `{hour:'numeric', minute:'2-digit'}` and `{month:'short', day:'numeric'}` words the page writes for
    /// `ms` ("3:05 PM", "Nov 14"; ASCII space, as the envelope writes them).
    fn words(&self, ms: i64) -> R<(String, String)> {
        if ms < self.from || ms > self.to {
            return Err(Decline("zone"));
        }
        let minutes = self
            .offsets
            .iter()
            .take_while(|(start, _)| *start <= ms)
            .last()
            .map_or(0, |(_, minutes)| *minutes);
        // Intl truncates to the minute: whole local minutes since 1970.
        let local = (ms + minutes * 60_000).div_euclid(60_000);
        let (_, m, d) = civil_from_days(local.div_euclid(1440));
        let (hour, minute) = (local.rem_euclid(1440) / 60, local.rem_euclid(60));
        let clock = format!(
            "{}:{minute:02} {}",
            if hour % 12 == 0 { 12 } else { hour % 12 },
            if hour < 12 { "AM" } else { "PM" }
        );
        Ok((clock, format!("{} {d}", MONTHS[(m - 1) as usize])))
    }
}

/// The en-US formatters of view-model.mjs at one `now`: the display-zone clock and day words of each instant and the
/// collation ranks come from the presentation envelope, joined in the page's proven [`Style`]. A fact the envelope
/// lacks marks the context invalid, and the build then declines (`fact`); a word the page's [`Zone`] would write
/// differently declines (`zone`).
pub struct Fmt {
    ctx: Context,
    style: Style,
    zone: Zone,
    /// The browser's clock.
    pub now: i64,
}

impl Fmt {
    /// Formatters over the envelope's facts in the style proven by the format probe, checked against the page's zone.
    pub fn new(ctx: Context, style: Style, zone: Zone, now: i64) -> Self {
        Fmt {
            ctx,
            style,
            zone,
            now,
        }
    }

    /// Every fact asked for was in the envelope.
    pub fn valid(&self) -> bool {
        self.ctx.valid()
    }

    /// The envelope's clock word ("3:05 PM", ASCII space) in the page's style.
    pub fn clock(&self, ms: i64) -> R<String> {
        let word = self.ctx.clock(ms);
        let fact = Err(Decline("fact"));
        let Some((hm, half)) = word.split_once(' ') else {
            return fact;
        };
        let digits = hm.split_once(':').is_some_and(|(h, m)| {
            (1..=2).contains(&h.len())
                && m.len() == 2
                && h.bytes().chain(m.bytes()).all(|c| c.is_ascii_digit())
        });
        if !digits || !matches!(half, "AM" | "PM") {
            return fact;
        }
        if self.zone.words(ms)?.0 != word {
            return Err(Decline("zone"));
        }
        Ok(format!("{hm}{}{half}", self.style.sep))
    }

    /// `{month:'short', day:'numeric'}`: the envelope's day word ("Dec 2"), checked.
    pub fn day(&self, ms: i64) -> R<String> {
        self.date(ms)?;
        Ok(self.ctx.day(ms))
    }

    /// The display-zone calendar date of `ms`: month and day from the envelope's day word, the year from the
    /// envelope's UTC ISO word (a zone moves the date by less than two days, so one year fits).
    pub fn date(&self, ms: i64) -> R<(i64, i64, i64)> {
        let fact = Err(Decline("fact"));
        let word = self.ctx.day(ms);
        let Some((name, day)) = word.split_once(' ') else {
            return fact;
        };
        let Some(month) = MONTHS.iter().position(|m| *m == name) else {
            return fact;
        };
        let month = month as i64 + 1;
        let Ok(day) = day.parse::<i64>() else {
            return fact;
        };
        if day.to_string() != word[name.len() + 1..] || !(1..=31).contains(&day) {
            return fact;
        }
        if self.zone.words(ms)?.1 != word {
            return Err(Decline("zone"));
        }
        let iso = self.ctx.iso(Some(ms));
        let Some(iso) = iso.as_str() else { return fact };
        let utc = iso.as_bytes();
        let num = |from: usize, len: usize| -> Option<i64> {
            let part = utc.get(from..from + len)?;
            part.iter()
                .all(u8::is_ascii_digit)
                .then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
        };
        let (Some(y), Some(m), Some(d)) = (num(0, 4), num(5, 2), num(8, 2)) else {
            return fact;
        };
        if utc.get(4) != Some(&b'-') || utc.get(7) != Some(&b'-') {
            return fact;
        }
        let at = days_from_civil(y, m, d);
        [y - 1, y, y + 1]
            .into_iter()
            .find(|year| {
                day <= days_in_month(*year, month)
                    && (days_from_civil(*year, month, day) - at).abs() <= 1
            })
            .map(|year| (year, month, day))
            .ok_or(Decline("fact"))
    }

    /// The two instants fall on the same display-zone calendar day.
    pub fn same_day(&self, a: i64, b: i64) -> R<bool> {
        Ok(self.date(a)? == self.date(b)?)
    }

    /// `{month:'short', day:'numeric', hour:'numeric', minute:'2-digit'}`: "Nov 14, 3:05 PM" ("Nov 14 at 3:05 PM").
    pub fn date_time(&self, ms: i64) -> R<String> {
        Ok(format!(
            "{}{}{}",
            self.day(ms)?,
            self.style.joiner,
            self.clock(ms)?
        ))
    }

    /// `{weekday:'short', month:'short', day:'numeric', hour, minute}`: "Thu, Oct 1, 10:15 AM" ("... Oct 1 at 10:15 AM").
    pub fn day_clock(&self, ms: i64) -> R<String> {
        let (y, m, d) = self.date(ms)?;
        let weekday = WEEKDAYS[(days_from_civil(y, m, d) + 4).rem_euclid(7) as usize];
        Ok(format!("{weekday}, {}", self.date_time(ms)?))
    }

    /// `a.localeCompare(b)` from the envelope's ranks, for printable ASCII strings (every ICU orders those alike, so
    /// the server's rank is the browser's); other strings decline.
    pub fn compare(&self, a: &str, b: &str) -> R<Ordering> {
        if a == b {
            return Ok(Ordering::Equal);
        }
        let printable = |s: &str| s.bytes().all(|c| (0x20..=0x7e).contains(&c));
        if !printable(a) || !printable(b) {
            return Err(Decline("collation"));
        }
        Ok(self.ctx.compare(a, b))
    }

    /// The probe strings this formatter set writes (view-model.mjs `homeFormatProbe`), at UTC reference instants.
    pub fn probe(style: Style) -> Vec<String> {
        let Style { sep, joiner } = style;
        vec![
            "-1,234.57".into(),
            "1.23M".into(),
            "-$1,234.50".into(),
            format!("Jan 5{joiner}12:07{sep}AM"),
            format!("1:07{sep}PM"),
            format!("Mon, Jan 5{joiner}1:07{sep}PM"),
            "Jan 5".into(),
            "_abB".into(),
        ]
    }

    /// The style the page's probe proves, or a decline when the page's locale writes anything else.
    pub fn proven_style(probe: &[String]) -> R<Style> {
        STYLES
            .into_iter()
            .find(|style| Self::probe(*style) == probe)
            .ok_or(Decline("locale"))
    }
}

// ---------------------------------------------------------------- numbers

/// A non-negative decimal: `int` digits (no leading zeros, "0" for zero) and `frac` digits.
struct Decimal {
    int: String,
    frac: String,
}

/// The digits ECMAScript and ICU write for `|x|`, split at the point. They come from Status's
/// `read_context::js_number`, the one conversion proved equal to JS `String(x)`: on its domain the
/// shortest round-trip digits are unique, so the host's choice is the browser's. Rust's own shortest
/// formatter breaks some ties differently (2251799813685247.25 writes .3 where JS writes .2), so
/// anything outside that domain declines and the page builds its own view.
fn decimal(x: f64) -> R<Decimal> {
    let text = aac_status::read_context::js_number(x.abs()).ok_or(Decline("number"))?;
    let (mantissa, exp) = match text.split_once('e') {
        Some((m, e)) => (m, e.parse::<i64>().map_err(|_| Decline("number"))?),
        None => (text.as_str(), 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all = format!("{int}{frac}");
    let digits = all.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(Decimal {
            int: "0".into(),
            frac: String::new(),
        });
    }
    let k = digits.len() as i64;
    // Where the point falls after the first significant digit's position in `digits`.
    let point = int.len() as i64 + exp - (all.len() - digits.len()) as i64;
    Ok(if point <= 0 {
        Decimal {
            int: "0".into(),
            frac: format!("{}{digits}", "0".repeat((-point) as usize)),
        }
    } else if point >= k {
        Decimal {
            int: format!("{digits}{}", "0".repeat((point - k) as usize)),
            frac: String::new(),
        }
    } else {
        Decimal {
            int: digits[..point as usize].into(),
            frac: digits[point as usize..].into(),
        }
    })
}

/// Moves the decimal point left by `places` (dividing by 10^places).
fn shift_left(d: &Decimal, places: usize) -> Decimal {
    let mut int = d.int.clone();
    if int.len() <= places {
        int = format!("{}{int}", "0".repeat(places - int.len() + 1));
    }
    let cut = int.len() - places;
    let frac = format!("{}{}", &int[cut..], d.frac);
    let int = int[..cut].trim_start_matches('0');
    Decimal {
        int: if int.is_empty() {
            "0".into()
        } else {
            int.into()
        },
        frac,
    }
}

/// Rounds half away from zero to `places` fraction digits.
fn round(d: &Decimal, places: usize) -> Decimal {
    if d.frac.len() <= places {
        return Decimal {
            int: d.int.clone(),
            frac: d.frac.clone(),
        };
    }
    let up = d.frac.as_bytes()[places] >= b'5';
    let mut digits: Vec<u8> = format!("{}{}", d.int, &d.frac[..places]).into_bytes();
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let text = String::from_utf8(digits).unwrap_or_default();
    let cut = text.len() - places;
    let int = text[..cut].trim_start_matches('0');
    Decimal {
        int: if int.is_empty() {
            "0".into()
        } else {
            int.into()
        },
        frac: text[cut..].into(),
    }
}

fn grouped(int: &str, min_grouping: usize) -> String {
    if int.len() < 3 + min_grouping {
        return int.into();
    }
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn written(d: &Decimal, min_frac: usize, min_grouping: usize) -> String {
    let mut frac = d.frac.trim_end_matches('0').to_string();
    while frac.len() < min_frac {
        frac.push('0');
    }
    let int = grouped(&d.int, min_grouping);
    if frac.is_empty() {
        int
    } else {
        format!("{int}.{frac}")
    }
}

fn sign(x: f64) -> &'static str {
    if x.is_sign_negative() { "-" } else { "" }
}

/// `new Intl.NumberFormat('en-US', {maximumFractionDigits: 2}).format(x)`, or a decline outside
/// [`decimal`]'s proved domain.
pub fn nf2(x: f64) -> R<String> {
    if !x.is_finite() {
        return Ok(if x.is_nan() {
            "NaN".into()
        } else {
            format!("{}∞", sign(x))
        });
    }
    Ok(format!(
        "{}{}",
        sign(x),
        written(&round(&decimal(x)?, 2), 0, 1)
    ))
}

/// `new Intl.NumberFormat('en-US', {style:'currency', currency:'USD', maximumFractionDigits: 2}).format(x)`,
/// or a decline outside [`decimal`]'s proved domain.
pub fn money(x: f64) -> R<String> {
    if !x.is_finite() {
        return Ok(if x.is_nan() {
            "$NaN".into()
        } else {
            format!("{}$∞", sign(x))
        });
    }
    Ok(format!(
        "{}${}",
        sign(x),
        written(&round(&decimal(x)?, 2), 2, 1)
    ))
}

/// `new Intl.NumberFormat('en-US', {notation:'compact', maximumFractionDigits: 2}).format(x)` (ICU's short compact
/// patterns K, M, B, T; a rounding that reaches the next power picks that power's pattern; grouping from 5 digits),
/// or a decline outside [`decimal`]'s proved domain.
pub fn compact(x: f64) -> R<String> {
    if !x.is_finite() {
        return Ok(if x.is_nan() {
            "NaN".into()
        } else {
            format!("{}∞", sign(x))
        });
    }
    let d = decimal(x)?;
    let magnitude = |d: &Decimal| -> i64 {
        if d.int != "0" {
            d.int.len() as i64 - 1
        } else {
            match d.frac.bytes().position(|c| c != b'0') {
                Some(i) => -(i as i64) - 1,
                None => 0,
            }
        }
    };
    let multiplier = |m: i64| -> usize {
        match m {
            ..=2 => 0,
            3..=5 => 3,
            6..=8 => 6,
            9..=11 => 9,
            _ => 12,
        }
    };
    let zero = |d: &Decimal| d.int == "0" && d.frac.bytes().all(|c| c == b'0');
    let mag = magnitude(&d);
    let mut mult = multiplier(mag);
    let mut r = round(&shift_left(&d, mult), 2);
    if !zero(&r) && magnitude(&r) != mag - mult as i64 {
        let next = multiplier(mag + 1);
        if next != mult {
            r = round(&shift_left(&r, next - mult), 2);
            mult = next;
        }
    }
    let suffix = match mult {
        3 => "K",
        6 => "M",
        9 => "B",
        12 => "T",
        _ => "",
    };
    Ok(format!("{}{}{suffix}", sign(x), written(&r, 0, 2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_node_intl() {
        let ok = |s: &str| Ok(s.to_string());
        let nf: [(f64, &str); 11] = [
            (-0.0, "-0"),
            (1.005, "1.01"),
            (1234.565, "1,234.57"),
            (0.005, "0.01"),
            (-1234.5, "-1,234.5"),
            (2.675, "2.68"),
            (1e-7, "0"),
            (4.999e-3, "0"),
            (-0.001, "-0"),
            (999.999, "1,000"),
            (12.0, "12"),
        ];
        for (x, want) in nf {
            assert_eq!(nf2(x), ok(want), "nf2 {x}");
        }
        let compacts: [(f64, &str); 18] = [
            (999.0, "999"),
            (999.995, "1K"),
            (1000.0, "1K"),
            (9999.5, "10K"),
            (62500.0, "62.5K"),
            (999_999.0, "1M"),
            (999_995.0, "1M"),
            (1_234_567.0, "1.23M"),
            (1e9, "1B"),
            (1e12, "1T"),
            (-62500.0, "-62.5K"),
            (0.001, "0"),
            (-0.0, "-0"),
            (1049.99, "1.05K"),
            (1004.9, "1K"),
            (12345.678, "12.35K"),
            (0.005, "0.01"),
            (99.995, "100"),
        ];
        for (x, want) in compacts {
            assert_eq!(compact(x), ok(want), "compact {x}");
        }
        for (x, want) in [
            (12.5, "$12.50"),
            (-5.0, "-$5.00"),
            (1234.567, "$1,234.57"),
            (0.005, "$0.01"),
            (-0.0, "-$0.00"),
            (1.5e-7, "$0.00"),
            (999_999_999_999.0, "$999,999,999,999.00"),
        ] {
            assert_eq!(money(x), ok(want), "money {x}");
        }
    }

    /// Big review M4/L4: Rust's shortest digits break ties differently from ECMAScript (JS writes
    /// 140737488355327.12 and 2251799813685247.2; Rust's formatter wrote .13 and .3). Only the
    /// domain Status's `js_number` proves is formatted; these and their neighbouring doubles decline.
    #[test]
    fn numbers_outside_the_proved_domain_decline() {
        let mut witnesses = vec![1e21, 1e15, 1.5e18, 9_007_199_254_740_992.0, 1e12 + 0.5];
        for x in [
            100000000000000.12_f64,
            140737488355327.12,
            2251799813685247.2,
        ] {
            witnesses.extend([
                f64::from_bits(x.to_bits() - 1),
                x,
                f64::from_bits(x.to_bits() + 1),
            ]);
        }
        witnesses.extend([33.333333333333336, 0.1 + 0.2, 1e-23]);
        for x in witnesses {
            for v in [x, -x] {
                assert_eq!(nf2(v), Err(Decline("number")), "nf2 {v}");
                assert_eq!(money(v), Err(Decline("number")), "money {v}");
                assert_eq!(compact(v), Err(Decline("number")), "compact {v}");
                assert_eq!(
                    string_of(Some(&json!(v))),
                    Err(Decline("number")),
                    "String {v}"
                );
            }
        }
    }

    /// The meter's `value` is written by aac-api's serializer, which cannot depend on Status; Home
    /// only sets it to numbers `nf2` admitted, and there both writers agree with JS `String(x)`.
    #[test]
    fn the_view_serializer_agrees_with_the_proved_writer_on_its_domain() {
        for x in [
            0.5,
            37.5,
            0.1,
            1e-7,
            1.5e-7,
            0.000001,
            123.456,
            1e12,
            -2.5,
            0.146484375,
            99.85351562,
            100.5,
            79.99,
            1234.5678,
        ] {
            assert!(nf2(x).is_ok(), "{x}");
            assert_eq!(
                Some(aac_api::home::js_number_text(x)),
                aac_status::read_context::js_number(x),
                "{x}"
            );
        }
    }

    #[test]
    fn only_the_ecmascript_utc_forms_parse() {
        assert_eq!(
            parse_date("2026-01-05T00:07:00.000Z"),
            Ok(Some(1_767_571_620_000))
        );
        assert_eq!(parse_date("2026-01-05T00:07Z"), Ok(Some(1_767_571_620_000)));
        assert_eq!(
            parse_date("2026-01-05T00:07:00+05:30"),
            Ok(Some(1_767_551_820_000))
        );
        assert_eq!(parse_date(""), Ok(None));
        for s in [
            "2026-01-05",
            "2026-01",
            "2026-01-05T00:07:00",
            "2026-02-30T00:00:00Z",
            "2026-01-05T24:00:00Z",
            "2026-01-05 00:07:00Z",
            "2026-01-05T00:07:00.123456Z",
            "+002026-01-05T00:07:00Z",
            "never",
            "2026-01-05t00:07z",
        ] {
            assert_eq!(parse_date(s), Err(Decline("date")), "{s}");
        }
    }

    #[test]
    fn the_probe_proves_chrome_and_safari_styles_and_nothing_else() {
        let chrome: Vec<String> = [
            "-1,234.57",
            "1.23M",
            "-$1,234.50",
            "Jan 5, 12:07 AM",
            "1:07 PM",
            "Mon, Jan 5, 1:07 PM",
            "Jan 5",
            "_abB",
        ]
        .map(String::from)
        .to_vec();
        let safari: Vec<String> = [
            "-1,234.57",
            "1.23M",
            "-$1,234.50",
            "Jan 5 at 12:07 AM",
            "1:07 PM",
            "Mon, Jan 5 at 1:07 PM",
            "Jan 5",
            "_abB",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(Fmt::proven_style(&chrome), Ok(STYLES[0]));
        let narrow: Vec<String> = chrome
            .iter()
            .map(|s| s.replace(" AM", "\u{202f}AM").replace(" PM", "\u{202f}PM"))
            .collect();
        assert_eq!(Fmt::proven_style(&narrow), Ok(STYLES[1]));
        assert_eq!(Fmt::proven_style(&safari), Ok(STYLES[2]));
        let mut german = safari.clone();
        german[0] = "-1.234,57".into();
        assert_eq!(Fmt::proven_style(&german), Err(Decline("locale")));
        let mut other = chrome.clone();
        other[3] = "Jan 5, 00:07".into();
        assert_eq!(Fmt::proven_style(&other), Err(Decline("locale")));
    }

    /// A context over envelope facts as `rustReadInput` writes them (America/New_York).
    fn facts(dates: Value, order: Value) -> Context {
        let presentation = serde_json::from_value(serde_json::json!({
            "dates": dates, "epochs": {}, "order": order
        }));
        Context::new(presentation.unwrap_or_else(|e| panic!("{e}")))
    }

    /// The page's zone facts as view-model.mjs `homeZoneFacts` sends them.
    fn zone(from: i64, to: i64, offsets: &[[i64; 2]]) -> Zone {
        Zone::proven(&aac_api::home::HomeZone {
            from,
            to,
            offsets: offsets.to_vec(),
        })
        .unwrap_or_else(|e| panic!("{e:?}"))
    }

    /// New York's facts from Nov 8, 2026 (EST) to Feb 1, 2027.
    fn new_york() -> Zone {
        zone(
            1_794_096_000_000,
            1_801_440_000_000,
            &[[1_794_096_000_000, -300]],
        )
    }

    #[test]
    fn the_envelope_words_are_joined_in_the_proven_style() {
        // 2026-11-14T20:05Z is Sat Nov 14, 3:05 PM in New York; 2027-01-01T04:30Z is Dec 31, 11:30 PM there.
        let f = |style| {
            Fmt::new(
                facts(
                    serde_json::json!({
                        "1794686700000": {"iso": "2026-11-14T20:05:00.000Z", "clock": "3:05 PM", "day": "Nov 14"},
                        "1798777800000": {"iso": "2027-01-01T04:30:00.000Z", "clock": "11:30 PM", "day": "Dec 31"},
                        "1": {"iso": "1970-01-01T00:00:00.001Z"}
                    }),
                    serde_json::json!({}),
                ),
                style,
                new_york(),
                1_794_686_700_000,
            )
        };
        let safari = f(STYLES[2]);
        assert_eq!(
            safari.date_time(1_794_686_700_000),
            Ok("Nov 14 at 3:05 PM".into())
        );
        assert_eq!(
            safari.day_clock(1_794_686_700_000),
            Ok("Sat, Nov 14 at 3:05 PM".into())
        );
        assert_eq!(safari.clock(1_794_686_700_000), Ok("3:05 PM".into()));
        let chrome = f(STYLES[0]);
        assert_eq!(
            chrome.day_clock(1_798_777_800_000),
            Ok("Thu, Dec 31, 11:30 PM".into())
        );
        assert_eq!(
            chrome.date(1_798_777_800_000),
            Ok((2026, 12, 31)),
            "the zone date's year, not the UTC one"
        );
        assert_eq!(
            chrome.same_day(1_794_686_700_000, 1_798_777_800_000),
            Ok(false)
        );
        assert!(chrome.valid());
        let narrow = f(STYLES[1]);
        assert_eq!(narrow.clock(1_794_686_700_000), Ok("3:05\u{202f}PM".into()));
        // a clock or day the envelope does not have (an ISO-only entry, or no entry) is a missing fact
        assert_eq!(chrome.clock(1), Err(Decline("fact")));
        assert!(!chrome.valid());
        assert_eq!(f(STYLES[0]).day(2), Err(Decline("fact")));
    }

    /// Big review (Fable M2): the server's words for Africa/Casablanca come from its own tzdata. On 2026-10-01T15:16Z
    /// Bun writes "4:16 PM" (+01:00 still), Node with tzdata 2026e "3:16 PM" (+00:00 from 2026-09-20). The page's
    /// offsets decide: a different word, or an instant outside the facts' window, declines.
    #[test]
    fn every_clock_and_day_word_is_the_one_the_page_zone_gives() {
        let at = 1_790_867_760_000; // 2026-10-01T15:16:00Z
        let server = || {
            facts(
                serde_json::json!({
                    "1790867760000": {"iso": "2026-10-01T15:16:00.000Z", "clock": "4:16 PM", "day": "Oct 1"},
                    "1790983200000": {"iso": "2026-10-02T23:20:00.000Z", "clock": "12:20 AM", "day": "Oct 3"}
                }),
                serde_json::json!({}),
            )
        };
        let from = 1_784_246_400_000; // 2026-07-17
        let node = zone(
            from,
            1_818_806_400_000,
            &[[from, 60], [1_789_866_000_000, 0]],
        );
        let bun = zone(from, 1_818_806_400_000, &[[from, 60]]);
        let page = |z| Fmt::new(server(), STYLES[0], z, at);
        assert_eq!(page(node.clone()).clock(at), Err(Decline("zone")));
        assert_eq!(
            page(node.clone()).day(1_790_983_200_000),
            Err(Decline("zone"))
        );
        assert_eq!(page(node).date_time(at), Err(Decline("zone")));
        assert_eq!(page(bun.clone()).clock(at), Ok("4:16 PM".into()));
        assert_eq!(page(bun.clone()).day(1_790_983_200_000), Ok("Oct 3".into()));
        assert_eq!(
            page(bun).date_time(1_790_983_200_000),
            Ok("Oct 3, 12:20 AM".into())
        );
        // the window bounds the proof
        let short = zone(from, at - 1, &[[from, 60]]);
        assert_eq!(page(short).clock(at), Err(Decline("zone")));
        // malformed facts prove nothing
        for (from, to, rows) in [
            (from, from, vec![[from, 0]]),
            (from, from + 1, vec![]),
            (from, from + 9, vec![[from + 1, 0]]),
            (from, from + 9, vec![[from, 0], [from + 5, 0]]),
            (
                from,
                from + 9,
                vec![[from, 0], [from + 5, 60], [from + 4, 0]],
            ),
            (from, from + 9, vec![[from, 1081]]),
            (from, from + 9, vec![[from, 0], [from + 10, 60]]),
        ] {
            let facts = aac_api::home::HomeZone {
                from,
                to,
                offsets: rows,
            };
            assert_eq!(Zone::proven(&facts), Err(Decline("zone")), "{facts:?}");
        }
        assert_eq!(
            Zone::proven(&aac_api::home::HomeZone::default()),
            Err(Decline("zone"))
        );
    }

    #[test]
    fn collation_comes_from_the_envelope_for_ascii_and_is_refused_otherwise() {
        let f = Fmt::new(
            facts(
                serde_json::json!({}),
                serde_json::json!({"a-1": 0, "a_1": 1, "aB": 3, "ab": 2, "cursor:A": 4, "cursor:b": 5}),
            ),
            STYLES[0],
            new_york(),
            0,
        );
        assert_eq!(f.compare("a_1", "a-1"), Ok(Ordering::Greater));
        assert_eq!(f.compare("aB", "ab"), Ok(Ordering::Greater));
        assert_eq!(f.compare("cursor:b", "cursor:A"), Ok(Ordering::Greater));
        assert_eq!(f.compare("x", "x"), Ok(Ordering::Equal));
        assert!(f.valid());
        assert_eq!(f.compare("é", "e"), Err(Decline("collation")));
        let _ = f.compare("a-1", "unranked");
        assert!(!f.valid(), "a word without a rank is a missing fact");
    }

    #[test]
    fn iso_words_match_to_iso_string() {
        for (ms, word) in [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"),
            (1_793_577_600_000, "2026-11-02T00:00:00.000Z"),
            (1_793_612_345_678, "2026-11-02T09:39:05.678Z"),
            (4_107_542_399_999, "2100-02-28T23:59:59.999Z"),
            (253_402_300_799_999, "9999-12-31T23:59:59.999Z"),
        ] {
            assert_eq!(iso(ms).as_deref(), Some(word), "{ms}");
        }
        assert_eq!(iso(-1), None);
        assert_eq!(iso(253_402_300_800_000), None);
    }

    #[test]
    fn patterns_match_their_javascript_regular_expressions() {
        for (text, expected) in [
            ("a@b.c", true),
            ("a.b@c.d.e", true),
            ("@b.c", false),
            ("a@.c", false),
            ("a@b.", false),
            ("a@b", false),
            ("a@b@c.d", false),
            ("a b@c.d", false),
            ("a@b\u{feff}.c", false),
            ("a\u{85}@b.c", true),
            ("é@ü.ß", true),
        ] {
            assert_eq!(email_shape(text), expected, "{text:?}");
        }
        assert_eq!(trim("\u{feff}\u{3000} x \u{2029}"), "x");
        assert_eq!(trim("\u{85}x"), "\u{85}x");
        assert!(has_control("a\u{7f}") && has_control("\u{0}") && !has_control("é\u{80}"));
        assert!(
            iso_prefix("2026-11-15T") && !iso_prefix("2026-11-15 ") && !iso_prefix("26-11-15T")
        );
    }
}
