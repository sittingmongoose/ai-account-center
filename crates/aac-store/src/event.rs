//! The event envelope (docs/store.md 2.3): an eight-element header tuple and a canonical
//! MessagePack payload map whose identity fields must equal the header's.

use crate::error::Result;
use crate::fail;
use crate::frame::FrameRef;
use crate::msgpack::{self, Value};

pub const SCHEMA_ID: &str = "aac.event.v1";
pub const SCOPE_KIND: &str = "application";

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub event_id: String,
    pub event_type: String,
    pub schema_version: String,
    pub payload_schema_id: String,
    pub sequence_id: u64,
    pub observed_at_ms: u64,
    pub source_at_ms: Option<u64>,
    pub idempotency_key: Option<String>,
    pub payload: Value,
}

impl Event {
    /// A new event; the log assigns `sequence_id` and `event_id` when it appends it.
    pub fn new(event_type: &str, observed_at_ms: u64, payload: Value) -> Event {
        let major = 1;
        Event {
            event_id: String::new(),
            event_type: event_type.to_string(),
            schema_version: "1.0.0".to_string(),
            payload_schema_id: format!("aac.{event_type}.v{major}"),
            sequence_id: 0,
            observed_at_ms,
            source_at_ms: None,
            idempotency_key: None,
            payload,
        }
    }

    pub fn header_bytes(&self) -> Result<Vec<u8>> {
        msgpack::encode(&Value::Array(vec![
            Value::UInt(1),
            Value::Str(self.event_id.clone()),
            Value::Str(self.event_type.clone()),
            Value::Str(SCHEMA_ID.to_string()),
            Value::Str(self.schema_version.clone()),
            Value::Str(self.payload_schema_id.clone()),
            Value::Str(SCOPE_KIND.to_string()),
            Value::Nil,
        ]))
    }

    pub fn payload_bytes(&self) -> Result<Vec<u8>> {
        let opt_u = |v: Option<u64>| v.map(Value::UInt).unwrap_or(Value::Nil);
        msgpack::encode(&Value::map(vec![
            ("event_id".into(), Value::Str(self.event_id.clone())),
            ("event_type".into(), Value::Str(self.event_type.clone())),
            ("schema_id".into(), Value::Str(SCHEMA_ID.into())),
            ("schema_version".into(), Value::Str(self.schema_version.clone())),
            ("payload_schema_id".into(), Value::Str(self.payload_schema_id.clone())),
            ("scope_kind".into(), Value::Str(SCOPE_KIND.into())),
            ("project_id".into(), Value::Nil),
            ("sequence_id".into(), Value::UInt(self.sequence_id)),
            ("observed_at_ms".into(), Value::UInt(self.observed_at_ms)),
            ("source_at_ms".into(), opt_u(self.source_at_ms)),
            (
                "idempotency_key".into(),
                self.idempotency_key.clone().map(Value::Str).unwrap_or(Value::Nil),
            ),
            ("payload".into(), self.payload.clone()),
        ])?)
    }

    /// Steps 8 to 10 of the frame validation: decode both parts and cross-check them.
    pub fn decode(frame: &FrameRef<'_>) -> Result<Event> {
        let header = msgpack::decode(frame.header)?;
        let items = match header {
            Value::Array(items) if items.len() == 8 => items,
            _ => fail!("corrupt", "header is not an eight-element tuple"),
        };
        if items[0] != Value::UInt(1) {
            fail!("unsupported", "header version");
        }
        let text = |i: usize| -> Result<String> {
            match &items[i] {
                Value::Str(s) if !s.is_empty() => Ok(s.clone()),
                _ => fail!("corrupt", "header field {i} is not a nonempty string"),
            }
        };
        let (event_id, event_type, schema_id) = (text(1)?, text(2)?, text(3)?);
        let (schema_version, payload_schema_id, scope_kind) = (text(4)?, text(5)?, text(6)?);
        if items[7] != Value::Nil {
            fail!("corrupt", "application events carry no project");
        }
        if schema_id != SCHEMA_ID || scope_kind != SCOPE_KIND {
            fail!("unsupported", "event schema {schema_id}");
        }
        let body = msgpack::decode(frame.payload)?;
        let field = |key: &str| body.get(key).cloned().unwrap_or(Value::Nil);
        let same = |key: &str, expected: &str| field(key) == Value::Str(expected.to_string());
        if !same("event_id", &event_id)
            || !same("event_type", &event_type)
            || !same("schema_id", &schema_id)
            || !same("schema_version", &schema_version)
            || !same("payload_schema_id", &payload_schema_id)
            || !same("scope_kind", &scope_kind)
            || field("project_id") != Value::Nil
        {
            fail!("corrupt", "header and envelope disagree");
        }
        if field("sequence_id") != Value::UInt(frame.sequence) {
            fail!("corrupt", "prefix and envelope sequence disagree");
        }
        let observed_at_ms = match field("observed_at_ms") {
            Value::UInt(v) => v,
            _ => fail!("corrupt", "observed_at_ms"),
        };
        let source_at_ms = match field("source_at_ms") {
            Value::UInt(v) => Some(v),
            Value::Nil => None,
            _ => fail!("corrupt", "source_at_ms"),
        };
        let idempotency_key = match field("idempotency_key") {
            Value::Str(v) => Some(v),
            Value::Nil => None,
            _ => fail!("corrupt", "idempotency_key"),
        };
        if let Value::Map(entries) = &body
            && entries.len() != 12
        {
            fail!("corrupt", "envelope has unexpected fields");
        }
        Ok(Event {
            event_id,
            event_type,
            schema_version,
            payload_schema_id,
            sequence_id: frame.sequence,
            observed_at_ms,
            source_at_ms,
            idempotency_key,
            payload: field("payload"),
        })
    }
}
