//! The JavaScript value semantics the TypeScript copy core and its wire checks rely on:
//! `JSON.parse` numbers are all f64, `JSON.stringify` prints integer-valued numbers without a
//! fraction, object key order is insertion order, and string lengths in the display-metadata
//! guard are UTF-16 code units. The port keeps those rules so canonical bytes stay equal.
use serde_json::{Map, Value};

/// SHA-256 as lowercase hex (the core's `sha`).
pub fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One parsed JSON number as JavaScript keeps it: every number is an f64, and stringify prints
/// integer-valued magnitudes below 1e21 without a fraction or exponent. Larger magnitudes keep
/// serde's shortest round-trip form, which no guarded field reaches (counts, ports, timestamps).
fn js_number(value: &Value) -> Value {
    let Some(number) = value.as_number() else {
        return value.clone();
    };
    let Some(raw) = number.as_f64() else {
        return value.clone();
    };
    if raw.fract() == 0.0 && raw.abs() < 1e21 {
        if raw >= 0.0 {
            if let Ok(unsigned) = u64::try_from(raw as i128) {
                return Value::Number(unsigned.into());
            }
        } else if let Ok(signed) = i64::try_from(raw as i128) {
            return Value::Number(signed.into());
        }
    }
    // Keep the f64 spelling for non-integer or out-of-range magnitudes.
    serde_json::Number::from_f64(raw).map_or_else(|| value.clone(), Value::Number)
}

/// `JSON.parse` followed by the number normalization `JSON.stringify` applies: parse once, then
/// rewrite every number to its JavaScript form. Duplicate keys keep their first position with
/// the last value, exactly as the JavaScript object model does.
pub fn parse(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    Some(normalize(value))
}

/// Apply [`js_number`] through one parsed value.
pub fn normalize(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, normalize(item)))
                .collect::<Map<String, Value>>(),
        ),
        Value::Number(_) => js_number(&value),
        other => other,
    }
}

/// `JSON.stringify` of an already parsed value: compact text, insertion key order, JavaScript
/// number forms. The value must come from [`parse`] (or be built with those rules).
pub fn stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `JSON.stringify(JSON.parse(bytes))`: the canonical form the append-receipt and marker checks
/// compare against. `None` when the bytes are not one JSON value.
pub fn canonical(bytes: &[u8]) -> Option<String> {
    Some(stringify(&parse(bytes)?))
}

/// JavaScript `string.length`: UTF-16 code units, not Unicode scalars.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The core's `number`: a finite JSON number at least zero.
pub fn non_negative_number(value: &Value) -> bool {
    value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}

/// The core's `Number.isSafeInteger`.
pub fn safe_integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0)
}

/// A lowercase UUID of the core's `UUID` shape (`validId`).
pub fn uuid(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_take_the_javascript_form() {
        assert_eq!(canonical(b"1e2").as_deref(), Some("100"));
        assert_eq!(canonical(b"1.5").as_deref(), Some("1.5"));
        assert_eq!(canonical(b"9007199254740993").as_deref(), Some("9007199254740992"));
        assert_eq!(canonical(b"{\"a\":1,\"b\":2,\"a\":3}").as_deref(), Some("{\"a\":3,\"b\":2}"));
        assert_eq!(stringify(&json!({"b": 1, "a": 2})), "{\"b\":1,\"a\":2}");
    }

    #[test]
    fn utf16_lengths_count_surrogate_pairs() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("\u{1F600}"), 2);
    }

    #[test]
    fn uuid_shape_is_lowercase_only() {
        assert!(uuid("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!uuid("01234567-89AB-cdef-0123-456789abcdef"));
        assert!(!uuid("0123456789abcdef0123456789abcdef"));
    }
}
