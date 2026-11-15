//! `Intl` as AAC's pages use it, in en-US, as V8 and ICU give it:
//! - `Intl.NumberFormat` for two-fraction decimals, US dollars and short compact notation. ICU rounds the number's
//!   shortest round-trip digits ([`crate::digits::shortest`]) half-expand, then groups; it does not round the exact
//!   binary value (so 1.005 writes `1.01`, where `toFixed(2)` writes `1.00`).
//! - clock and day words of an instant from the page's display-zone offsets, joined in the punctuation style its
//!   format probe proved;
//! - `localeCompare` order of printable ASCII strings (the CLDR root order every ICU shares for them).
//!
//! Numbers are admitted on the [`crate::view`] domain only; anything else is `None` and the caller declines.
use crate::civil::{self, MONTHS};
use crate::digits::shortest;
use alloc::{format, string::String, vec::Vec};
use core::cmp::Ordering;

/// A non-negative decimal split at the point: `int` digits ("0" for none) and `frac` digits.
struct Split {
    int: String,
    frac: String,
}

/// The shortest digits of `|x|` split at the point, or `None` outside the [`crate::view`] domain.
fn split(x: f64) -> Option<Split> {
    crate::view::string_admitted(crate::abs(x)).then(|| split_any(x))
}

/// [`split`] without the domain: only the corpus test uses it, to show the formatting is exact beyond the domain.
fn split_any(x: f64) -> Split {
    let d = shortest(x);
    if d.is_zero() {
        return Split {
            int: "0".into(),
            frac: String::new(),
        };
    }
    let digits = d.text();
    let k = digits.len() as i32;
    let point = d.lead() + 1;
    if point <= 0 {
        Split {
            int: "0".into(),
            frac: format!("{}{digits}", "0".repeat((-point) as usize)),
        }
    } else if point >= k {
        Split {
            int: format!("{digits}{}", "0".repeat((point - k) as usize)),
            frac: String::new(),
        }
    } else {
        Split {
            int: digits[..point as usize].into(),
            frac: digits[point as usize..].into(),
        }
    }
}

/// Moves the point left by `places` (divides by 10^places).
fn shift_left(d: &Split, places: usize) -> Split {
    let mut int = d.int.clone();
    if int.len() <= places {
        int = format!("{}{int}", "0".repeat(places - int.len() + 1));
    }
    let cut = int.len() - places;
    let frac = format!("{}{}", &int[cut..], d.frac);
    let int = int[..cut].trim_start_matches('0');
    Split {
        int: if int.is_empty() {
            "0".into()
        } else {
            int.into()
        },
        frac,
    }
}

/// Rounds half away from zero (`halfExpand`) to `places` fraction digits.
fn round(d: &Split, places: usize) -> Split {
    if d.frac.len() <= places {
        return Split {
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
    Split {
        int: if int.is_empty() {
            "0".into()
        } else {
            int.into()
        },
        frac: text[cut..].into(),
    }
}

/// Thousands separators once the integer has at least `3 + min_grouping` digits.
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

fn written(d: &Split, min_frac: usize, min_grouping: usize) -> String {
    let mut frac = String::from(d.frac.trim_end_matches('0'));
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

/// `new Intl.NumberFormat('en-US', {maximumFractionDigits: 2}).format(x)`. NaN and the infinities write `NaN` and
/// `∞`; a finite `x` outside the [`crate::view`] domain is `None`.
pub fn decimal2(x: f64) -> Option<String> {
    decimal2_with(x, split)
}

fn decimal2_with(x: f64, split: fn(f64) -> Option<Split>) -> Option<String> {
    if !x.is_finite() {
        return Some(if x.is_nan() {
            "NaN".into()
        } else {
            format!("{}∞", sign(x))
        });
    }
    Some(format!(
        "{}{}",
        sign(x),
        written(&round(&split(x)?, 2), 0, 1)
    ))
}

/// `new Intl.NumberFormat('en-US', {style: 'currency', currency: 'USD', maximumFractionDigits: 2}).format(x)`.
/// Same domain as [`decimal2`].
pub fn usd(x: f64) -> Option<String> {
    usd_with(x, split)
}

fn usd_with(x: f64, split: fn(f64) -> Option<Split>) -> Option<String> {
    if !x.is_finite() {
        return Some(if x.is_nan() {
            "$NaN".into()
        } else {
            format!("{}$∞", sign(x))
        });
    }
    Some(format!(
        "{}${}",
        sign(x),
        written(&round(&split(x)?, 2), 2, 1)
    ))
}

/// `new Intl.NumberFormat('en-US', {notation: 'compact', maximumFractionDigits: 2}).format(x)`: ICU's short compact
/// patterns K, M, B and T; a rounding that reaches the next power takes that power's pattern; grouping from five
/// digits. Same domain as [`decimal2`].
pub fn compact(x: f64) -> Option<String> {
    compact_with(x, split)
}

fn compact_with(x: f64, split: fn(f64) -> Option<Split>) -> Option<String> {
    if !x.is_finite() {
        return Some(if x.is_nan() {
            "NaN".into()
        } else {
            format!("{}∞", sign(x))
        });
    }
    let d = split(x)?;
    let magnitude = |d: &Split| -> i64 {
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
    let zero = |d: &Split| d.int == "0" && d.frac.bytes().all(|c| c == b'0');
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
    Some(format!("{}{}{suffix}", sign(x), written(&r, 0, 2)))
}

// ---------------------------------------------------------------- clock and day words

/// How a page's en-US date formatters join the pieces: the space before AM/PM (U+0020 in Chrome, Node and Safari;
/// U+202F in older ICU) and the joiner between the date and the time (", " in Chrome and Node, " at " in iOS Safari:
/// "Jan 5 at 12:07 AM").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    /// Before AM/PM.
    pub sep: &'static str,
    /// Between "Jan 5" and "3:05 PM".
    pub joiner: &'static str,
}

/// The styles a format probe can prove; anything else declines.
pub const STYLES: [Style; 4] = [
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

/// The probe strings a formatter set in `style` writes (view-model.mjs `homeFormatProbe`, at UTC reference instants).
pub fn probe(style: Style) -> Vec<String> {
    let Style { sep, joiner } = style;
    alloc::vec![
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

/// The style a page's probe proves, or `None` when the page's locale writes anything else.
pub fn proven_style(probe_words: &[String]) -> Option<Style> {
    STYLES
        .into_iter()
        .find(|style| probe(*style) == probe_words)
}

/// A page's display-zone UTC offsets over a window (view-model.mjs `homeZoneFacts`), checked for shape: the format
/// probe proves punctuation only, and another engine's zone data can put a clock an hour elsewhere, so every word
/// is derived from these offsets. An instant outside the window has no word.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    from: i64,
    to: i64,
    /// `(start ms, offset minutes)`, starts strictly increasing, the first at `from`.
    offsets: Vec<(i64, i64)>,
}

impl Zone {
    /// The facts as one ordered, bounded segment list over a real window, or `None`.
    pub fn proven(from: i64, to: i64, rows: &[[i64; 2]]) -> Option<Zone> {
        if from >= to
            || from < 0
            || to > crate::ecma::MAX_TIME
            || rows.is_empty()
            || rows.len() > 64
            || rows[0][0] != from
            || rows
                .iter()
                .any(|[start, minutes]| *start > to || minutes.abs() > 1080)
            || rows
                .windows(2)
                .any(|w| w[1][0] <= w[0][0] || w[1][1] == w[0][1])
        {
            return None;
        }
        Some(Zone {
            from,
            to,
            offsets: rows
                .iter()
                .map(|[start, minutes]| (*start, *minutes))
                .collect(),
        })
    }

    /// Whole local minutes since 1970 at `ms` (Intl truncates to the minute), or `None` outside the window.
    fn local_minutes(&self, ms: i64) -> Option<i64> {
        if ms < self.from || ms > self.to {
            return None;
        }
        let minutes = self
            .offsets
            .iter()
            .take_while(|(start, _)| *start <= ms)
            .last()
            .map_or(0, |(_, minutes)| *minutes);
        Some((ms + minutes * 60_000).div_euclid(60_000))
    }

    /// The display-zone calendar date `(year, month, day)` of `ms`, from the same offsets as [`Zone::words`] (R1: the
    /// page's own date, so no server word is needed for the year), or `None` outside the window.
    pub fn local_date(&self, ms: i64) -> Option<(i64, i64, i64)> {
        Some(civil::civil_from_days(
            self.local_minutes(ms)?.div_euclid(1440),
        ))
    }

    /// The en-US `{hour:'numeric', minute:'2-digit'}` and `{month:'short', day:'numeric'}` words for `ms` ("3:05 PM",
    /// "Nov 14", ASCII space), or `None` outside the window.
    pub fn words(&self, ms: i64) -> Option<(String, String)> {
        let local = self.local_minutes(ms)?;
        let (_, m, d) = civil::civil_from_days(local.div_euclid(1440));
        let (hour, minute) = (local.rem_euclid(1440) / 60, local.rem_euclid(60));
        let clock = format!(
            "{}:{minute:02} {}",
            if hour % 12 == 0 { 12 } else { hour % 12 },
            if hour < 12 { "AM" } else { "PM" }
        );
        Some((clock, format!("{} {d}", MONTHS[(m - 1) as usize])))
    }
}

// ---------------------------------------------------------------- collation

/// Printable ASCII in `localeCompare` order (ICU root, en-US, Node 26): each letter's lowercase precedes its
/// uppercase and shares its primary weight; every other character has a primary weight of its own.
pub const ASCII_ORDER: &str = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";

/// `(primary, tertiary)` weights of a printable ASCII byte, from [`ASCII_ORDER`].
fn weights(c: u8) -> Option<(u8, u8)> {
    let at = ASCII_ORDER.bytes().position(|o| o == c)? as u8;
    let digits_end = ASCII_ORDER.bytes().position(|o| o == b'a')? as u8;
    Some(if c.is_ascii_alphabetic() {
        // a A b B ...: one primary per letter pair, lowercase first.
        (
            digits_end + (at - digits_end) / 2,
            u8::from(c.is_ascii_uppercase()),
        )
    } else {
        (at, 0)
    })
}

/// `a.localeCompare(b)` for two printable ASCII strings (U+0020 to U+007E): primary weights left to right (a prefix
/// first), then tertiary (case) weights. Any other string is `None`.
pub fn ascii_compare(a: &str, b: &str) -> Option<Ordering> {
    let keys = |s: &str| -> Option<Vec<(u8, u8)>> { s.bytes().map(weights).collect() };
    let (ka, kb) = (keys(a)?, keys(b)?);
    let primary = ka.iter().map(|k| k.0).cmp(kb.iter().map(|k| k.0));
    Some(primary.then_with(|| ka.iter().map(|k| k.1).cmp(kb.iter().map(|k| k.1))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_match_node_intl() {
        for (x, want) in [
            (-0.0, "-0"),
            (1.005, "1.01"),
            (1234.565, "1,234.57"),
            (0.005, "0.01"),
            (-1234.5, "-1,234.5"),
            (2.675, "2.68"),
            (1e-7, "0"),
            (-0.001, "-0"),
            (999.999, "1,000"),
        ] {
            assert_eq!(decimal2(x).as_deref(), Some(want), "decimal2 {x}");
        }
        for (x, want) in [
            (999.995, "1K"),
            (9999.5, "10K"),
            (999_995.0, "1M"),
            (1_234_567.0, "1.23M"),
            (1e12, "1T"),
            (99.995, "100"),
        ] {
            assert_eq!(compact(x).as_deref(), Some(want), "compact {x}");
        }
        assert_eq!(usd(-0.0).as_deref(), Some("-$0.00"));
        assert_eq!(usd(f64::NEG_INFINITY).as_deref(), Some("-$∞"));
        assert_eq!(decimal2(2251799813685247.2), None);
    }

    /// The formatting itself is exact on every value of the Node corpus, admitted or not; only the domain declines.
    #[test]
    fn the_formatting_is_exact_beyond_the_admitted_domain() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/spec/num/intl.json"
        );
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        let rows = doc["rows"].as_array().cloned().unwrap_or_default();
        assert!(rows.len() > 10_000, "{path}");
        let any = |x: f64| Some(split_any(x));
        let mut failures = Vec::new();
        for row in &rows {
            let x =
                f64::from_bits(u64::from_str_radix(row[0].as_str().unwrap_or(""), 16).unwrap_or(0));
            for (i, got) in [
                decimal2_with(x, any),
                usd_with(x, any),
                compact_with(x, any),
            ]
            .into_iter()
            .enumerate()
            {
                if got.as_deref() != row[i + 1].as_str() {
                    failures.push(format!("{x:e} #{i}: {got:?} vs {}", row[i + 1]));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failed: {:?}",
            failures.len(),
            &failures[..failures.len().min(20)]
        );
    }

    #[test]
    fn ascii_order_is_the_icu_order() {
        assert_eq!(ascii_compare("a_1", "a-1"), Some(Ordering::Less));
        assert_eq!(ascii_compare("a-b", "ab"), Some(Ordering::Less));
        assert_eq!(ascii_compare("aB", "Ab"), Some(Ordering::Less));
        assert_eq!(ascii_compare("A", "a b"), Some(Ordering::Less));
        assert_eq!(ascii_compare("x", "x"), Some(Ordering::Equal));
        assert_eq!(ascii_compare("é", "e"), None);
        assert_eq!(ascii_compare("a\tb", "ab"), None);
        assert_eq!(ASCII_ORDER.len(), 95);
    }

    #[test]
    fn zone_words_and_styles() {
        let ny = Zone::proven(
            1_794_096_000_000,
            1_801_440_000_000,
            &[[1_794_096_000_000, -300]],
        );
        let words = ny.as_ref().and_then(|z| z.words(1_794_686_700_000));
        assert_eq!(words, Some(("3:05 PM".into(), "Nov 14".into())));
        // R1: the local date comes from the same offsets: 2026-11-14 15:05 in New York is that day there, while
        // 2027-01-01 03:00Z is still New Year's Eve (the year from the page's offsets, not the UTC word).
        let ny = ny.expect("a proven zone");
        assert_eq!(ny.local_date(1_794_686_700_000), Some((2026, 11, 14)));
        assert_eq!(ny.local_date(1_798_772_400_000), Some((2026, 12, 31)));
        assert_eq!(ny.local_date(1_794_096_000_000 - 1), None);
        assert_eq!(Zone::proven(5, 5, &[[5, 0]]), None);
        assert_eq!(proven_style(&probe(STYLES[2])), Some(STYLES[2]));
    }
}
