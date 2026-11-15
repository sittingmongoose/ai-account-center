//! `JSON.stringify` of a parsed JSON value, byte for byte (R1: the service sends the final bytes and the front door
//! writes them without parsing, so every byte must be the one Node would write). It also writes, in the same pass,
//! the revalidated-view identity (`routes/revalidated-view.ts`): the same text with the clock-only members
//! (`generatedAt`, `ageSeconds`, `cacheAgeSeconds`) left out at any depth, whose SHA-256 is the `ETag`.
//!
//! The rules are JavaScript's:
//! - **Keys:** an object writes its array-index keys (`"0"` to `"4294967294"`, canonical) first in ascending order,
//!   then the other keys in insertion order (`OrdinaryOwnPropertyKeys`), so a value Rust built is written as Node
//!   would write it after `JSON.parse`. Text Node wrote is already in that order.
//! - **Strings:** `QuoteJSONString`: `"` `\` and U+0000 to U+001F are escaped (`\b \t \n \f \r`, else `\u00xx` in
//!   lowercase); everything else is written as is (a Rust string holds no lone surrogate).
//! - **Numbers:** integers as their digits; other doubles as `Number#toString` (`aac_num::ecma::to_string`), so
//!   `-0` writes `0` and `1e21` writes `1e+21`.
//!
//! A caller that re-writes text Node produced (a fed generation) proves the round trip once per generation by
//! comparing the bytes ([`write`] of the parsed value equals the text), and declines when it does not hold.
use serde_json::{Map, Value};

/// The clock-only keys the revalidated-view identity leaves out.
pub const CLOCK_KEYS: [&str; 3] = ["generatedAt", "ageSeconds", "cacheAgeSeconds"];

/// The writer: the full text and the identity text, written together.
#[derive(Default)]
pub struct Writer {
    full: Vec<u8>,
    identity: Vec<u8>,
    /// Inside a clock-only member: the identity text skips everything written.
    hidden: usize,
}

/// True when `key` is an array index as JavaScript orders it: a canonical integer from 0 to 2^32 - 2.
fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || (bytes[0] == b'0' && bytes.len() > 1) {
        return None;
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: u64 = key.parse().ok()?;
    (n <= 4_294_967_294).then_some(n as u32)
}

/// An object's members in JavaScript's own-key order.
pub fn ordered(map: &Map<String, Value>) -> Vec<(&str, &Value)> {
    let mut members: Vec<(&str, &Value)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
    if members.iter().any(|(k, _)| array_index(k).is_some()) {
        let (mut indices, rest): (Vec<_>, Vec<_>) = members
            .into_iter()
            .partition(|(k, _)| array_index(k).is_some());
        indices.sort_by_key(|(k, _)| array_index(k));
        indices.extend(rest);
        members = indices;
    }
    members
}

impl Writer {
    /// A writer with room for `capacity` bytes.
    pub fn with_capacity(capacity: usize) -> Self {
        Writer {
            full: Vec::with_capacity(capacity),
            identity: Vec::with_capacity(capacity),
            hidden: 0,
        }
    }

    /// R1b: a writer of the full text only (no identity text is kept).
    pub fn plain(capacity: usize) -> Self {
        Writer {
            full: Vec::with_capacity(capacity),
            identity: Vec::new(),
            // Permanently inside a "hidden" member: nothing reaches the identity side.
            hidden: 1,
        }
    }

    /// Raw text, on both sides unless inside a clock-only member.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.full.extend_from_slice(bytes);
        if self.hidden == 0 {
            self.identity.extend_from_slice(bytes);
        }
    }

    /// `JSON.stringify` of a string.
    pub fn string(&mut self, text: &str) {
        let bytes = text.as_bytes();
        self.raw(b"\"");
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let escape: &[u8] = match b {
                b'"' => b"\\\"",
                b'\\' => b"\\\\",
                0x08 => b"\\b",
                0x09 => b"\\t",
                0x0a => b"\\n",
                0x0c => b"\\f",
                0x0d => b"\\r",
                0x00..=0x1f => b"",
                _ => continue,
            };
            self.raw(&bytes[start..i]);
            if escape.is_empty() {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                self.raw(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[usize::from(b >> 4)],
                    HEX[usize::from(b & 0xf)],
                ]);
            } else {
                self.raw(escape);
            }
            start = i + 1;
        }
        self.raw(&bytes[start..]);
        self.raw(b"\"");
    }

    /// `JSON.stringify` of a number as `JSON.parse` would have read it: an integer up to 2^53 as its digits, any
    /// other value as the nearest double's `Number#toString`.
    pub fn number(&mut self, n: &serde_json::Number) {
        const EXACT: u64 = 1 << 53;
        #[allow(clippy::cast_precision_loss)]
        let x = match (n.as_u64(), n.as_i64()) {
            (Some(u), _) if u <= EXACT => return self.raw(u.to_string().as_bytes()),
            (_, Some(i)) if i.unsigned_abs() <= EXACT => {
                return self.raw(i.to_string().as_bytes());
            }
            (Some(u), _) => u as f64,
            (_, Some(i)) => i as f64,
            _ => n.as_f64().unwrap_or(f64::NAN),
        };
        if x.is_finite() {
            self.raw(aac_num::ecma::to_string(x).as_bytes());
        } else {
            self.raw(b"null");
        }
    }

    /// Any value.
    pub fn value(&mut self, value: &Value) {
        match value {
            Value::Null => self.raw(b"null"),
            Value::Bool(true) => self.raw(b"true"),
            Value::Bool(false) => self.raw(b"false"),
            Value::Number(n) => self.number(n),
            Value::String(s) => self.string(s),
            Value::Array(items) => {
                self.raw(b"[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.raw(b",");
                    }
                    self.value(item);
                }
                self.raw(b"]");
            }
            Value::Object(map) => {
                let mut object = self.object();
                if map.keys().any(|k| array_index(k).is_some()) {
                    for (key, member) in ordered(map) {
                        object.member(key, |w| w.value(member));
                    }
                } else {
                    for (key, member) in map {
                        object.member(key, |w| w.value(member));
                    }
                }
                object.end();
            }
        }
    }

    /// Start an object; members follow through [`Object::member`].
    pub fn object(&mut self) -> Object<'_> {
        self.raw(b"{");
        Object {
            writer: self,
            first: true,
            first_identity: true,
        }
    }

    /// Text already written for both sides (a template's part, a serialized view without clock-only members).
    pub fn raw_pair(&mut self, full: &[u8], identity: &[u8]) {
        self.full.extend_from_slice(full);
        if self.hidden == 0 {
            self.identity.extend_from_slice(identity);
        }
    }

    /// Take what was written so far (both sides), leaving the writer empty: a template's static part.
    pub fn cut(&mut self) -> (Vec<u8>, Vec<u8>) {
        (
            std::mem::take(&mut self.full),
            std::mem::take(&mut self.identity),
        )
    }

    /// The full text and the identity text (the same bytes when no clock-only member was written).
    pub fn finish(self) -> (Vec<u8>, Vec<u8>) {
        (self.full, self.identity)
    }

    /// The full text only.
    pub fn into_bytes(self) -> Vec<u8> {
        self.full
    }
}

/// An object being written: commas are kept apart for the full text and the identity text.
pub struct Object<'w> {
    writer: &'w mut Writer,
    first: bool,
    first_identity: bool,
}

impl Object<'_> {
    /// One member; `write` writes its value.
    pub fn member(&mut self, key: &str, write: impl FnOnce(&mut Writer)) {
        let clock = CLOCK_KEYS.contains(&key);
        let w = &mut *self.writer;
        if !self.first {
            w.full.push(b',');
        }
        self.first = false;
        if w.hidden == 0 && !clock {
            if !self.first_identity {
                w.identity.push(b',');
            }
            self.first_identity = false;
        }
        if clock {
            w.hidden += 1;
        }
        w.string(key);
        w.raw(b":");
        write(w);
        if clock {
            w.hidden -= 1;
        }
    }

    /// Close the object.
    pub fn end(self) {
        self.writer.raw(b"}");
    }
}

/// `JSON.stringify(value)`.
pub fn write(value: &Value) -> Vec<u8> {
    let mut w = Writer::default();
    w.value(value);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strings_numbers_and_keys_are_written_as_node_writes_them() {
        // Node: JSON.stringify(JSON.parse(text)) for each text.
        for (text, node) in [
            (
                r#"{"b":1,"2":2,"a":3,"1":4}"#,
                r#"{"1":4,"2":2,"b":1,"a":3}"#,
            ),
            (
                r#"{"01":1,"4294967295":2,"4294967294":3}"#,
                r#"{"4294967294":3,"01":1,"4294967295":2}"#,
            ),
            (
                r#"[1e21,1e-7,0.1,-0,5e-324,123456789012345680000,1.5]"#,
                "[1e+21,1e-7,0.1,0,5e-324,123456789012345680000,1.5]",
            ),
            (
                r#"["\u0000\u001f\b\t\n\f\r\"\\\/\u007f é😀"]"#,
                "[\"\\u0000\\u001f\\b\\t\\n\\f\\r\\\"\\\\/\u{7f}\u{2028}é😀\"]",
            ),
            (
                r#"{"__proto__":{"x":null},"a":[true,false,{}],"e":[]}"#,
                r#"{"__proto__":{"x":null},"a":[true,false,{}],"e":[]}"#,
            ),
            ("9007199254740993", "9007199254740992"),
            ("-9007199254740993", "-9007199254740992"),
            ("9007199254740992", "9007199254740992"),
            ("-12", "-12"),
            ("18446744073709551616", "18446744073709552000"),
        ] {
            let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            assert_eq!(
                String::from_utf8(write(&value)).unwrap_or_default(),
                node,
                "{text}"
            );
        }
    }

    #[test]
    fn the_identity_leaves_out_clock_members_at_any_depth() {
        let value = json!({"generatedAt":"x","a":1,"rows":[{"ageSeconds":3,"v":"é"},{"cacheAgeSeconds":1}],"z":{"generatedAt":1}});
        let mut w = Writer::with_capacity(64);
        w.value(&value);
        let (full, identity) = w.finish();
        assert_eq!(full, write(&value));
        // Node: JSON.stringify(value, (k, v) => ['generatedAt','ageSeconds','cacheAgeSeconds'].includes(k) ? undefined : v)
        assert_eq!(
            String::from_utf8(identity).unwrap_or_default(),
            r#"{"a":1,"rows":[{"v":"é"},{}],"z":{}}"#
        );
        let mut w = Writer::with_capacity(8);
        w.value(&json!({"a":[1]}));
        let (full, identity) = w.finish();
        assert_eq!(full, identity);
    }
}
