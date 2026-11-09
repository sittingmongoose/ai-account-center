//! The shared validators of the agent report documents, ported field for field from
//! `src/web-server/status/schema-helpers.ts`: same caps counted the same way (string lengths
//! are UTF-16 code units, as in JavaScript), same fixed refusal reasons, nothing echoing a
//! file's text. The secret check is deliberately not ported: TypeScript checks status
//! reports for secrets; the native module only reads reports TypeScript accepted (owner
//! decision 2026-11-09).

use serde_json::{Map, Value as Json};

/// Thrown inside the projection; becomes the refusal reason.
#[derive(Debug)]
pub struct Invalid(pub String);

pub type Valid<T> = Result<T, Invalid>;

pub fn fail(field: &str) -> Invalid {
    Invalid(format!("{field} is missing or not valid"))
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Control characters other than tab and newline, and bidirectional overrides (the
/// `CONTROL` pattern in schema-helpers.ts).
fn is_control(c: char) -> bool {
    matches!(c,
        '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub fn obj<'a>(value: Option<&'a Json>, field: &str) -> Valid<&'a Map<String, Json>> {
    match value {
        Some(Json::Object(map)) => Ok(map),
        _ => Err(fail(field)),
    }
}

pub fn text(value: Option<&Json>, field: &str, max: usize) -> Valid<String> {
    match value {
        Some(Json::String(text)) if !text.is_empty() && utf16_len(text) <= max && !text.chars().any(is_control) => {
            Ok(text.clone())
        }
        _ => Err(fail(field)),
    }
}

pub fn opt_text(value: Option<&Json>, field: &str, max: usize) -> Valid<Option<String>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(text)) if text.is_empty() => Ok(None),
        other => text(other, field, max).map(Some),
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`
fn is_slug(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    value.chars().count() <= 64 && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn slug(value: Option<&Json>, field: &str) -> Valid<String> {
    match value {
        Some(Json::String(text)) if is_slug(text) => Ok(text.clone()),
        _ => Err(fail(field)),
    }
}

/// JavaScript's `Number.MAX_SAFE_INTEGER` (2^53-1): every count a document may carry is at
/// most this, so the JSON number never rounds when it becomes a JavaScript number. A larger
/// integer would (2^53+1 reads as 2^53 there), and two readers could then order the same
/// copies differently; the float path below has always refused those.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// A non-negative safe integer, as `count` in schema-helpers.ts counts one
/// (`Number.isSafeInteger`): a JSON `1.0` counts, as it does in JavaScript, and an integer
/// above `MAX_SAFE_INTEGER` does not.
pub fn count(value: Option<&Json>, field: &str) -> Valid<u64> {
    let number = match value {
        Some(Json::Number(number)) => number,
        _ => return Err(fail(field)),
    };
    if let Some(int) = number.as_u64()
        && int <= MAX_SAFE_INTEGER
    {
        return Ok(int);
    }
    if let Some(float) = number.as_f64()
        && (0.0..=MAX_SAFE_INTEGER as f64).contains(&float)
        && float.fract() == 0.0
    {
        return Ok(float as u64);
    }
    Err(fail(field))
}

/// The digits of a timestamp's numeric run, or None when a byte is not a digit.
fn digits(bytes: &[u8], at: usize, count: usize) -> Option<u32> {
    let mut out: u32 = 0;
    for i in 0..count {
        let byte = *bytes.get(at + i)?;
        if !byte.is_ascii_digit() {
            return None;
        }
        out = out * 10 + u32::from(byte - b'0');
    }
    Some(out)
}

/// A real calendar moment, as agreed with agent-report so the two never diverge: a real
/// month and day (leap years counted), hour 00-23, minute and second 00-59 (no leap second,
/// no 24:00), and an offset within +-14:00. The shape is the `ISO` pattern of
/// schema-helpers.ts: `YYYY-MM-DDTHH:MM(:SS(.fraction)?)?(Z|+-HH:MM)`.
pub fn real_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    // Fixed head: 0000-00-00T00:00
    if b.len() < 17 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' {
        return false;
    }
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute)) = (
        digits(b, 0, 4),
        digits(b, 5, 2),
        digits(b, 8, 2),
        digits(b, 11, 2),
        digits(b, 14, 2),
    ) else {
        return false;
    };
    let mut at = 16;
    let mut second = 0;
    if b.get(at) == Some(&b':') {
        let Some(parsed) = digits(b, at + 1, 2) else {
            return false;
        };
        second = parsed;
        at += 3;
        if b.get(at) == Some(&b'.') {
            let start = at + 1;
            let mut end = start;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == start || end - start > 9 {
                return false;
            }
            at = end;
        }
    }
    let offset = match b.get(at) {
        Some(b'Z') if at + 1 == b.len() => None,
        Some(b'+' | b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            match (digits(b, at + 1, 2), digits(b, at + 4, 2)) {
                (Some(hours), Some(minutes)) => Some((hours, minutes)),
                _ => return false,
            }
        }
        _ => return false,
    };
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    // Proleptic Gregorian by arithmetic, so every year counts as written.
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let last_day = if month == 2 {
        if leap { 29 } else { 28 }
    } else if [4, 6, 9, 11].contains(&month) {
        30
    } else {
        31
    };
    if day < 1 || day > last_day {
        return false;
    }
    if let Some((hours, minutes)) = offset
        && (minutes > 59 || hours * 60 + minutes > 14 * 60)
    {
        return false;
    }
    true
}

pub fn iso(value: Option<&Json>, field: &str) -> Valid<String> {
    match value {
        Some(Json::String(text)) if real_timestamp(text) => Ok(text.clone()),
        _ => Err(fail(field)),
    }
}

pub fn list<T>(value: Option<&Json>, field: &str, max: usize, each: impl Fn(&Json, &str) -> Valid<T>) -> Valid<Vec<T>> {
    match value {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Array(items)) if items.len() <= max => items
            .iter()
            .enumerate()
            .map(|(index, item)| each(item, &format!("{field}[{index}]")))
            .collect(),
        _ => Err(fail(field)),
    }
}

pub fn one_of(value: Option<&Json>, field: &str, allowed: &[&str]) -> Valid<String> {
    match value {
        Some(Json::String(text)) if allowed.contains(&text.as_str()) => Ok(text.clone()),
        _ => Err(fail(field)),
    }
}

/// `<machine>.<pm-mail name>`, lowercase, 64 characters at most, and never holding `--`
/// (a card id is split on the first one).
pub fn is_writer_name(value: &str) -> bool {
    let Some((machine, rest)) = value.split_once('.') else {
        return false;
    };
    if !matches!(machine, "vm" | "mac" | "windows" | "nas1") || utf16_len(value) > 64 || value.contains("--") {
        return false;
    }
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
}

/// The shared head of every document.
#[derive(Debug, Clone, PartialEq)]
pub struct DocHead {
    pub writer: String,
    pub seq: u64,
    pub written_at: String,
}

pub fn head(doc: &Map<String, Json>, schema: &str, writer: &str) -> Valid<DocHead> {
    if doc.get("schema").and_then(Json::as_str) != Some(schema) {
        return Err(fail("schema"));
    }
    if doc.get("writer").and_then(Json::as_str) != Some(writer) {
        return Err(Invalid("writer does not match its folder".to_string()));
    }
    Ok(DocHead {
        writer: writer.to_string(),
        seq: count(doc.get("seq"), "seq")?,
        written_at: iso(doc.get("writtenAt"), "writtenAt")?,
    })
}

/// Bytes of one share file -> its JSON value, under the byte rules every reader applies: the
/// size cap, no NUL bytes (a damaged write), UTF-8, valid JSON. Every refusal is a fixed
/// reason, the same strings as `jsonFromBytes` in schema-helpers.ts.
///
/// The secret check is not here: TypeScript checks status reports for secrets
/// (`jsonFromBytes` in schema-helpers.ts, the one secret filter); the native module only
/// reads reports TypeScript accepted (owner decision 2026-11-09).
///
/// One honest divergence: serde_json refuses JSON nested deeper than its limit (128) as
/// "not valid JSON", where the TypeScript reader parses it and then refuses or projects it.
/// Such a file is refused either way; only the reason differs.
pub fn json_from_bytes(bytes: &[u8], cap_bytes: usize) -> Result<Json, String> {
    if bytes.len() > cap_bytes {
        return Err(format!("larger than the {} KB cap", cap_bytes / 1024));
    }
    if bytes.is_empty() {
        return Err("empty".to_string());
    }
    if bytes.contains(&0) {
        return Err("contains NUL bytes from a damaged write".to_string());
    }
    let source = std::str::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
    serde_json::from_str(source).map_err(|_| "not valid JSON".to_string())
}
