//! Canonical MessagePack (docs/store.md 2.3, Puppet Master's Case L-2 rules): string keys sorted
//! by UTF-8 bytes, shortest encodings, no binary/extension/timestamp tags, no NaN or infinity,
//! float32 only when it holds the exact float64 bits. The decoder is strict: it accepts only
//! bytes that re-encode to themselves.

use crate::error::{Error, Result};
use crate::fail;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    /// Negative integers only; non-negative integers are `UInt`.
    Int(i64),
    UInt(u64),
    F64(f64),
    Str(String),
    Array(Vec<Value>),
    /// Keys strictly increasing by UTF-8 bytes.
    Map(Vec<(String, Value)>),
}

const MAX_DEPTH: usize = 64;

impl Value {
    pub fn map(mut entries: Vec<(String, Value)>) -> Result<Value> {
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for pair in entries.windows(2) {
            if pair[0].0 == pair[1].0 {
                fail!("invalid", "duplicate map key");
            }
        }
        Ok(Value::Map(entries))
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::UInt(v) => Some(*v),
            _ => None,
        }
    }
}

pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128);
    write(&mut out, value, 0)?;
    Ok(out)
}

fn write(out: &mut Vec<u8>, value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        fail!("invalid", "value nests too deeply");
    }
    match value {
        Value::Nil => out.push(0xc0),
        Value::Bool(false) => out.push(0xc2),
        Value::Bool(true) => out.push(0xc3),
        Value::UInt(v) => {
            let v = *v;
            if v <= 0x7f {
                out.push(v as u8);
            } else if v <= 0xff {
                out.extend_from_slice(&[0xcc, v as u8]);
            } else if v <= 0xffff {
                out.push(0xcd);
                out.extend_from_slice(&(v as u16).to_be_bytes());
            } else if v <= 0xffff_ffff {
                out.push(0xce);
                out.extend_from_slice(&(v as u32).to_be_bytes());
            } else {
                out.push(0xcf);
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Value::Int(v) => {
            let v = *v;
            if v >= 0 {
                fail!("invalid", "non-negative integer must be unsigned");
            }
            if v >= -32 {
                out.push(v as i8 as u8);
            } else if v >= i8::MIN as i64 {
                out.extend_from_slice(&[0xd0, v as i8 as u8]);
            } else if v >= i16::MIN as i64 {
                out.push(0xd1);
                out.extend_from_slice(&(v as i16).to_be_bytes());
            } else if v >= i32::MIN as i64 {
                out.push(0xd2);
                out.extend_from_slice(&(v as i32).to_be_bytes());
            } else {
                out.push(0xd3);
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Value::F64(v) => {
            let v = *v;
            if !v.is_finite() {
                fail!("invalid", "NaN and infinity are not stored");
            }
            let narrow = v as f32;
            if (narrow as f64).to_bits() == v.to_bits() {
                out.push(0xca);
                out.extend_from_slice(&narrow.to_be_bytes());
            } else {
                out.push(0xcb);
                out.extend_from_slice(&v.to_be_bytes());
            }
        }
        Value::Str(s) => {
            let len = s.len();
            if len <= 31 {
                out.push(0xa0 | len as u8);
            } else if len <= 0xff {
                out.extend_from_slice(&[0xd9, len as u8]);
            } else if len <= 0xffff {
                out.push(0xda);
                out.extend_from_slice(&(len as u16).to_be_bytes());
            } else if len <= u32::MAX as usize {
                out.push(0xdb);
                out.extend_from_slice(&(len as u32).to_be_bytes());
            } else {
                fail!("invalid", "string too long");
            }
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(items) => {
            write_len(out, items.len(), 0x90, 0xdc, 0xdd)?;
            for item in items {
                write(out, item, depth + 1)?;
            }
        }
        Value::Map(entries) => {
            for pair in entries.windows(2) {
                if pair[0].0.as_bytes() >= pair[1].0.as_bytes() {
                    fail!("invalid", "map keys must be unique and sorted");
                }
            }
            write_len(out, entries.len(), 0x80, 0xde, 0xdf)?;
            for (key, item) in entries {
                write(out, &Value::Str(key.clone()), depth + 1)?;
                write(out, item, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn write_len(out: &mut Vec<u8>, len: usize, fix: u8, tag16: u8, tag32: u8) -> Result<()> {
    if len <= 15 {
        out.push(fix | len as u8);
    } else if len <= 0xffff {
        out.push(tag16);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else if len <= u32::MAX as usize {
        out.push(tag32);
        out.extend_from_slice(&(len as u32).to_be_bytes());
    } else {
        fail!("invalid", "container too long");
    }
    Ok(())
}

/// Decodes exactly one canonical value filling all of `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Value> {
    let mut reader = Reader { bytes, pos: 0 };
    let value = reader.value(0)?;
    if reader.pos != bytes.len() {
        fail!("corrupt", "trailing bytes after value");
    }
    // Canonical means the bytes are the one encoding of the value.
    if encode(&value)? != bytes {
        fail!("corrupt", "noncanonical MessagePack");
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| Error::new("corrupt", "length overflow"))?;
        if end > self.bytes.len() {
            fail!("corrupt", "value runs past its frame");
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn be(&mut self, n: usize) -> Result<u64> {
        let mut v = 0u64;
        for b in self.take(n)? {
            v = (v << 8) | *b as u64;
        }
        Ok(v)
    }

    fn string(&mut self, len: usize) -> Result<Value> {
        let raw = self.take(len)?;
        match std::str::from_utf8(raw) {
            Ok(s) => Ok(Value::Str(s.to_owned())),
            Err(_) => fail!("corrupt", "string is not UTF-8"),
        }
    }

    fn array(&mut self, len: usize, depth: usize) -> Result<Value> {
        // A declared count never allocates beyond the bytes actually present.
        let mut items = Vec::with_capacity(len.min(self.bytes.len() - self.pos));
        for _ in 0..len {
            items.push(self.value(depth + 1)?);
        }
        Ok(Value::Array(items))
    }

    fn map(&mut self, len: usize, depth: usize) -> Result<Value> {
        let mut entries: Vec<(String, Value)> = Vec::with_capacity(len.min(self.bytes.len() - self.pos));
        for _ in 0..len {
            let key = match self.value(depth + 1)? {
                Value::Str(s) => s,
                _ => fail!("corrupt", "map key is not a string"),
            };
            if let Some((last, _)) = entries.last()
                && last.as_bytes() >= key.as_bytes()
            {
                fail!("corrupt", "map keys are duplicated or unsorted");
            }
            let value = self.value(depth + 1)?;
            entries.push((key, value));
        }
        Ok(Value::Map(entries))
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            fail!("corrupt", "value nests too deeply");
        }
        let tag = self.byte()?;
        Ok(match tag {
            0x00..=0x7f => Value::UInt(tag as u64),
            0x80..=0x8f => return self.map((tag & 0x0f) as usize, depth),
            0x90..=0x9f => return self.array((tag & 0x0f) as usize, depth),
            0xa0..=0xbf => return self.string((tag & 0x1f) as usize),
            0xc0 => Value::Nil,
            0xc2 => Value::Bool(false),
            0xc3 => Value::Bool(true),
            0xca => {
                let bits = self.be(4)? as u32;
                let v = f32::from_bits(bits) as f64;
                if !v.is_finite() {
                    fail!("corrupt", "NaN or infinity");
                }
                Value::F64(v)
            }
            0xcb => {
                let v = f64::from_bits(self.be(8)?);
                if !v.is_finite() {
                    fail!("corrupt", "NaN or infinity");
                }
                Value::F64(v)
            }
            0xcc => Value::UInt(self.be(1)?),
            0xcd => Value::UInt(self.be(2)?),
            0xce => Value::UInt(self.be(4)?),
            0xcf => Value::UInt(self.be(8)?),
            0xd0 => Value::Int(self.be(1)? as u8 as i8 as i64),
            0xd1 => Value::Int(self.be(2)? as u16 as i16 as i64),
            0xd2 => Value::Int(self.be(4)? as u32 as i32 as i64),
            0xd3 => Value::Int(self.be(8)? as i64),
            0xd9 => {
                let n = self.be(1)? as usize;
                return self.string(n);
            }
            0xda => {
                let n = self.be(2)? as usize;
                return self.string(n);
            }
            0xdb => {
                let n = self.be(4)? as usize;
                return self.string(n);
            }
            0xdc => {
                let n = self.be(2)? as usize;
                return self.array(n, depth);
            }
            0xdd => {
                let n = self.be(4)? as usize;
                return self.array(n, depth);
            }
            0xde => {
                let n = self.be(2)? as usize;
                return self.map(n, depth);
            }
            0xdf => {
                let n = self.be(4)? as usize;
                return self.map(n, depth);
            }
            0xe0..=0xff => Value::Int(tag as i8 as i64),
            _ => fail!("corrupt", "binary, extension and timestamp tags are not stored"),
        })
    }
}

/// JSON to canonical MessagePack. Objects become sorted maps; integers keep their sign class.
pub fn from_json(value: &serde_json::Value) -> Result<Value> {
    from_json_depth(value, 0)
}

fn from_json_depth(value: &serde_json::Value, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
        fail!("invalid", "value nests too deeply");
    }
    Ok(match value {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Value::UInt(u)
            } else if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    fail!("invalid", "NaN and infinity are not stored");
                }
                Value::F64(f)
            } else {
                fail!("invalid", "unsupported number");
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| from_json_depth(item, depth + 1))
                .collect::<Result<_>>()?,
        ),
        serde_json::Value::Object(map) => Value::map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), from_json_depth(v, depth + 1)?)))
                .collect::<Result<Vec<_>>>()?,
        )?,
    })
}

pub fn to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Nil => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::from(*i),
        Value::UInt(u) => serde_json::Value::from(*u),
        Value::F64(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        Value::Map(entries) => {
            serde_json::Value::Object(entries.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

/// Serializes a serde value as canonical MessagePack.
pub fn to_bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    encode(&from_json(&serde_json::to_value(value)?)?)
}

/// Reads canonical MessagePack into a serde value.
pub fn from_bytes<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_value(to_json(&decode(bytes)?))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortest_integers_and_floats() {
        assert_eq!(encode(&Value::UInt(127)).unwrap(), vec![0x7f]);
        assert_eq!(encode(&Value::UInt(128)).unwrap(), vec![0xcc, 0x80]);
        assert_eq!(encode(&Value::Int(-1)).unwrap(), vec![0xff]);
        assert_eq!(encode(&Value::Int(-33)).unwrap(), vec![0xd0, 0xdf]);
        assert_eq!(encode(&Value::F64(1.5)).unwrap()[0], 0xca);
        assert_eq!(encode(&Value::F64(30.3)).unwrap()[0], 0xcb);
        assert!(encode(&Value::F64(f64::NAN)).is_err());
        assert!(encode(&Value::Int(5)).is_err());
    }

    #[test]
    fn decoder_rejects_noncanonical_bytes() {
        // 5 encoded as uint8 instead of fixint.
        assert!(decode(&[0xcc, 0x05]).is_err());
        // Unsorted map keys.
        assert!(decode(&[0x82, 0xa1, b'b', 0x01, 0xa1, b'a', 0x02]).is_err());
        // Duplicate keys.
        assert!(decode(&[0x82, 0xa1, b'a', 0x01, 0xa1, b'a', 0x02]).is_err());
        // Binary tag.
        assert!(decode(&[0xc4, 0x00]).is_err());
        // Trailing value.
        assert!(decode(&[0x01, 0x02]).is_err());
        // float64 that fits float32 exactly.
        let mut wide = vec![0xcb];
        wide.extend_from_slice(&1.5f64.to_be_bytes());
        assert!(decode(&wide).is_err());
        // Invalid UTF-8.
        assert!(decode(&[0xa1, 0xff]).is_err());
    }

    #[test]
    fn json_round_trip() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"z":[1,-2,30.3,null,true,"x"],"a":{"b":1.5,"a":18446744073709551615}}"#).unwrap();
        let bytes = encode(&from_json(&json).unwrap()).unwrap();
        assert_eq!(to_json(&decode(&bytes).unwrap()), json);
    }
}
