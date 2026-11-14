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
fn proven(value: &Value) -> bool {
    match value {
        Value::Number(n) => n
            .as_i64()
            .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991),
        Value::String(s) => !s.chars().any(|c| !c.is_ascii() && c.is_numeric()),
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
