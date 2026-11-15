//! JavaScript string, pattern and date semantics the profile compositions use. Each helper is
//! exact over the inputs it admits; the composers refuse whatever they cannot decide here.
use aac_status::read_context::{Context, js_whitespace};
use serde_json::{Value, json};

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

/// `value === true`.
pub fn is_true(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
