//! Objects the TS composers spread (`{...account, hidden}`), carried as ordered entry lists.
//!
//! JSON drops an own member whose value is undefined, but a spread keeps its position, so a
//! later assignment to that key stays in place. TS sends such objects as `[[key, value], [key]]`
//! (`[key]` is the undefined member); assignment here keeps JavaScript's position rule, and
//! [`Spread::into_value`] omits undefined members exactly like `JSON.stringify`.
use serde_json::{Map, Value};

/// The refusal every composer returns for an input whose JavaScript meaning it has not proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refuse;

/// The result of an admitted operation.
pub type Admit<T> = Result<T, Refuse>;

/// `Some` or [`Refuse`].
pub fn req<T>(value: Option<T>) -> Admit<T> {
    value.ok_or(Refuse)
}

/// Ordered members; `None` is a member whose value is undefined.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spread(Vec<(String, Option<Value>)>);

impl Spread {
    /// Decode a plain JSON object (native path, zero `[[k, v]]` round-trip) or fall back to
    /// `Spread::decode` for TS entry lists.
    pub(crate) fn decode_or_object(value: &Value) -> Admit<Self> {
        if let Some(map) = value.as_object() {
            let mut object = Self(Vec::with_capacity(map.len()));
            for (key, val) in map {
                if key == "__proto__" {
                    return Err(Refuse);
                }
                object.0.push((key.clone(), Some(val.clone())));
            }
            return Ok(object);
        }
        Self::decode(value)
    }

    /// Decode a TS entry list.
    pub fn decode(value: &Value) -> Admit<Self> {
        let mut object = Self::default();
        for entry in req(value.as_array())? {
            let pair = req(entry.as_array())?;
            let key = req(pair.first().and_then(Value::as_str))?;
            if key == "__proto__" || object.0.iter().any(|(name, _)| name == key) {
                return Err(Refuse);
            }
            match pair.len() {
                1 => object.0.push((key.to_owned(), None)),
                2 => object.0.push((key.to_owned(), Some(pair[1].clone()))),
                _ => return Err(Refuse),
            }
        }
        Ok(object)
    }

    /// The defined value of `key`; absent and undefined members both give `None`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_ref())
    }

    /// JavaScript assignment: an existing member keeps its position.
    pub fn set(&mut self, key: &str, value: Option<Value>) {
        if let Some(slot) = self.0.iter_mut().find(|(name, _)| name == key) {
            slot.1 = value;
        } else {
            self.0.push((key.to_owned(), value));
        }
    }

    /// The object `JSON.stringify` would write.
    pub fn into_value(self) -> Value {
        let mut map = Map::new();
        for (key, value) in self.0 {
            if let Some(value) = value {
                map.insert(key, value);
            }
        }
        Value::Object(map)
    }
}

/// JavaScript truthiness of a JSON value; `None` is undefined.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `String.prototype.length`: UTF-16 code units.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn undefined_members_keep_their_position_and_are_omitted() {
        let mut object = Spread::decode(&json!([["a", 1], ["u"], ["c", 3]])).expect("entries");
        object.set("u", Some(json!(2)));
        object.set("d", None);
        object.set("a", None);
        assert_eq!(
            serde_json::to_string(&object.into_value()).expect("json"),
            r#"{"u":2,"c":3}"#
        );
        for bad in [
            json!([["a", 1, 2]]),
            json!([[1, 2]]),
            json!([["a"], ["a", 1]]),
            json!({"a": 1}),
            json!([["__proto__", 1]]),
        ] {
            assert_eq!(Spread::decode(&bad), Err(Refuse), "{bad}");
        }
    }

    #[test]
    fn truthiness_and_lengths_match_javascript() {
        for (value, expected) in [
            (json!(null), false),
            (json!(false), false),
            (json!(0), false),
            (json!(-0.0), false),
            (json!(""), false),
            (json!(true), true),
            (json!(1e-300), true),
            (json!("0"), true),
            (json!([]), true),
            (json!({}), true),
        ] {
            assert_eq!(truthy(Some(&value)), expected, "{value}");
        }
        assert!(!truthy(None));
        assert_eq!(utf16_len("a\u{1F600}é"), 4);
    }
}
