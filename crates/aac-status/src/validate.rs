//! The shared validators of the agent report documents, ported field for field from
//! `src/web-server/status/schema-helpers.ts`: same caps counted the same way (string lengths
//! are UTF-16 code units, as in JavaScript), same fixed refusal reasons, nothing echoing a
//! file's text.
//!
//! One secret filter per read, never two (owner decision 2026-11-09): on the NAPI path
//! TypeScript checks status reports for secrets (`jsonFromBytes` in schema-helpers.ts) and
//! the native module only reads reports TypeScript accepted, so [`json_from_bytes`] keeps no
//! secret check. The R4 owned readers in the service run where no TypeScript reader runs;
//! they read through [`share_json_from_bytes`], which applies the same packaged policy
//! (`src/shared/secret-policy.json`, compiled as `crate::secret_policy` exactly as aac-store
//! compiles it) in that reader's place.

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
    if !crate::computers::known_machine(machine, false) || utf16_len(value) > 64 || value.contains("--") {
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
/// size cap, no NUL bytes (a damaged write), UTF-8, valid JSON that Rust can hold (an
/// overflowing number, a lone surrogate or nesting past serde's limit is "not valid JSON",
/// as `nativeUnreadable` makes it in TypeScript). Every refusal is a fixed reason, the same
/// strings as `jsonFromBytes` in schema-helpers.ts.
///
/// The secret check is not here: TypeScript checks status reports for secrets
/// (`jsonFromBytes` in schema-helpers.ts, the one secret filter); the native module only
/// reads reports TypeScript accepted (owner decision 2026-11-09).
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

/// [`json_from_bytes`] plus the secret check, for the R4 owned share readers only: there the
/// service reads the share itself and no TypeScript `jsonFromBytes` runs, so this is the one
/// secret filter of that read (same packaged policy, same fixed words). Never call it on the
/// NAPI path, where TypeScript already filtered the bytes.
pub fn share_json_from_bytes(bytes: &[u8], cap_bytes: usize) -> Result<Json, String> {
    let value = json_from_bytes(bytes, cap_bytes)?;
    // Valid UTF-8 is established above.
    let source = std::str::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
    if let Some(kind) = crate::secret_policy::find_secret_in(source, &value) {
        return Err(format!("refused: it contains what looks like {kind}"));
    }
    Ok(value)
}

/// `optIso`: null or absent is null; anything else must be a real calendar moment.
pub fn opt_iso(value: Option<&Json>, field: &str) -> Valid<Option<String>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        other => iso(other, field).map(Some),
    }
}

/// `optSlug`: null or absent is null; anything else must be a slug.
pub fn opt_slug(value: Option<&Json>, field: &str) -> Valid<Option<String>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        other => slug(other, field).map(Some),
    }
}

/// `optObj`: null or absent is null; anything else is an object the reader projects.
pub fn opt_obj<T>(
    value: Option<&Json>,
    field: &str,
    read: impl FnOnce(&Map<String, Json>) -> Valid<T>,
) -> Valid<Option<T>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        other => read(obj(other, field)?).map(Some),
    }
}

/// `bool`: absent or null takes the fallback; anything else must be a boolean.
pub fn bool_value(value: Option<&Json>, field: &str, fallback: bool) -> Valid<bool> {
    match value {
        None | Some(Json::Null) => Ok(fallback),
        Some(Json::Bool(flag)) => Ok(*flag),
        _ => Err(fail(field)),
    }
}

/// `text`, with a refusal that says the length rule when that is the one broken.
pub fn bounded_text(value: Option<&Json>, field: &str, max: usize) -> Valid<String> {
    if let Some(Json::String(text)) = value {
        let len = utf16_len(text);
        if len > max {
            return Err(Invalid(format!(
                "{field} is too long: {len} characters, over the {max} allowed"
            )));
        }
    }
    text(value, field, max)
}

/// A report link: `{label, path}` in that order, the label's bound published per kind.
pub fn link(value: Option<&Json>, field: &str, label_max: usize) -> Valid<Json> {
    let item = obj(value, field)?;
    Ok(serde_json::json!({
        "label": bounded_text(item.get("label"), &format!("{field}.label"), label_max)?,
        "path": text(item.get("path"), &format!("{field}.path"), 600)?,
    }))
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$` (a card option key).
pub fn option_key(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    value.chars().count() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Which rule a `<writer>--<local id>` id breaks, in fixed words, or None when it keeps them
/// all: the writer prefix, then the local id's length, then its grammar.
pub fn id_problem(value: Option<&Json>, writer: &str, local_max: usize) -> Option<String> {
    let Some(Json::String(value)) = value else {
        return Some("id is missing or not valid".to_string());
    };
    if value.is_empty() {
        return Some("id is missing or not valid".to_string());
    }
    let prefix = format!("{writer}--");
    if !value.starts_with(&prefix) {
        return Some(format!(
            "id does not name its writer: it must start with \"{writer}--\""
        ));
    }
    let local = &value[prefix.len()..];
    if local.is_empty() {
        return Some("id has no local part after its writer and \"--\"".to_string());
    }
    let len = utf16_len(local);
    if len > local_max {
        return Some(format!(
            "id is too long: the part after \"{writer}--\" is {len} characters, over the {local_max} allowed"
        ));
    }
    if !local_id_grammar(local) {
        return Some(
            "id is not valid: the part after its writer and \"--\" must start with a letter or digit and hold only letters, digits, \".\", \"_\" and \"-\""
                .to_string(),
        );
    }
    None
}

/// `^[A-Za-z0-9][A-Za-z0-9._-]*$` with no bound (the local id grammar).
fn local_id_grammar(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A card id: `<writer>--<local id>`, at most 130 characters, the local part at most 64.
pub fn is_card_id(value: Option<&Json>) -> bool {
    let Some(Json::String(value)) = value else {
        return false;
    };
    if utf16_len(value) > 130 {
        return false;
    }
    let Some(at) = value.find("--") else {
        return false;
    };
    if at == 0 || !is_writer_name(&value[..at]) {
        return false;
    }
    let local = &value[at + 2..];
    local_id_grammar(local) && utf16_len(local) <= 64
}

/// pm-mail's own name rule (`NAME_OK`): lowercase, no dots; `all` is the broadcast, never a card target.
pub fn is_pm_mail_name(value: Option<&Json>) -> bool {
    let Some(Json::String(value)) = value else {
        return false;
    };
    if value == "all" {
        return false;
    }
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    value.chars().count() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// A relative path inside a writer's folder: plain segments, no `..`, no absolute or Windows
/// paths (`IMAGE_PATH`: at most six segments of at most 100 characters).
pub fn is_writer_file_path(value: Option<&Json>) -> bool {
    let Some(Json::String(value)) = value else {
        return false;
    };
    if utf16_len(value) > 300 {
        return false;
    }
    let segments: Vec<&str> = value.split('/').collect();
    if segments.len() > 6 {
        return false;
    }
    segments.iter().all(|segment| {
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return false;
        }
        let mut chars = segment.chars();
        let first_ok = match chars.next() {
            Some(first) => first.is_ascii_alphanumeric() || matches!(first, '_' | '-'),
            None => false,
        };
        first_ok
            && segment.chars().count() <= 100
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}
