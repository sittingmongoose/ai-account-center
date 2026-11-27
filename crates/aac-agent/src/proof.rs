//! One conservative grammar boundary: an unproven input delegates the whole verb.
use serde_json::{Value, json};
use std::cell::Cell;
thread_local! { static UNPROVEN: Cell<bool> = const { Cell::new(false) }; }
pub(crate) fn reject() {
    UNPROVEN.set(true);
}
pub(crate) fn fallback() -> Value {
    json!({"fallback":"python"})
}
pub(crate) fn scope(run: impl FnOnce() -> Value) -> Value {
    UNPROVEN.set(false);
    let value = run();
    if UNPROVEN.replace(false) {
        fallback()
    } else {
        value
    }
}
pub(crate) fn proven(value: &Value) -> bool {
    match value {
        Value::Number(n) => n
            .as_i64()
            .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991),
        Value::String(s) => ascii(s),
        Value::Array(a) => a.iter().all(proven),
        Value::Object(o) => o
            .iter()
            .all(|(k, v)| proven(&Value::String(k.clone())) && proven(v)),
        _ => true,
    }
}
pub(crate) fn json(bytes: &[u8], cap: usize, bom: bool) -> Option<Value> {
    let bytes = if bom {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    } else {
        bytes
    };
    let value = (bytes.len() <= cap)
        .then(|| serde_json::from_slice::<Value>(bytes).ok())
        .flatten();
    if let Some(value) = value.filter(proven) {
        Some(value)
    } else {
        reject();
        None
    }
}
/// Another program's JSON file (Claude Code, Codex, T3 Code, Muse): plain JSON under the byte
/// cap, floats and non-ASCII text included, because those programs write both and nothing
/// stands behind a rejection any more (LIVE r34). Callers read and prove only the fields they
/// use. Bytes that are not JSON still reject: unread, never guessed.
pub(crate) fn lenient_json(bytes: &[u8], cap: usize, bom: bool) -> Option<Value> {
    let bytes = if bom {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    } else {
        bytes
    };
    let value = (bytes.len() <= cap)
        .then(|| serde_json::from_slice::<Value>(bytes).ok())
        .flatten();
    if value.is_none() {
        reject();
    }
    value
}
pub(crate) fn decimal(raw: &str) -> bool {
    if raw.is_empty() {
        return true;
    }
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    let valid = !whole.is_empty()
        && whole.bytes().all(|c| c.is_ascii_digit())
        && fraction.is_none_or(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        && parts.next().is_none()
        && raw
            .parse::<f64>()
            .is_ok_and(|n| n.is_finite() && n.abs() <= 9_007_199_254_740_991.0);
    if !valid {
        reject();
    }
    valid
}

// The only whitespace admitted before parsing: the six shared ASCII characters.
pub(crate) fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}')
}
pub(crate) fn ascii(raw: &str) -> bool {
    let valid = raw
        .bytes()
        .all(|b| (0x20..=0x7e).contains(&b) || matches!(b, 9..=13));
    if !valid {
        reject();
    }
    valid
}
pub(crate) fn strip(raw: &str) -> &str {
    ascii(raw);
    raw.trim_matches(space)
}
pub(crate) fn words(raw: &str) -> impl Iterator<Item = &str> {
    ascii(raw);
    raw.split(space).filter(|s| !s.is_empty())
}
