//! Permanent revisions of accepted report documents. The ledger is authoritative; compressed
//! content is deduplicated, while distinct source revisions and their correlation keys survive.
use crate::db::{self, Bytes, Index, Key};
use crate::engine::{Engine, LogId, Projector, now_ms};
use crate::error::Result;
use crate::event::Event;
use crate::log::Finding;
use crate::{fail, fsx, msgpack, report_secret};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, WriteTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

pub const HEAD: Bytes = redb::TableDefinition::new("capture_head");
pub const REVISION: Bytes = redb::TableDefinition::new("capture_revision");
pub const CONTENT: Bytes = redb::TableDefinition::new("capture_content");
pub const BY_TIME: Index = redb::TableDefinition::new("capture_by_time");
// A source tasks file is capped at 512 KiB; normalization adds nullable fields to 2,000 items.
const MAX_DOC: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub kind: String,
    pub writer: String,
    pub object_id: String,
    pub seq: u64,
    pub written_at_ms: u64,
    pub observed_at_ms: u64,
    pub source_sha256: String,
    pub ids: Json,
    pub doc: Json,
}

impl Report {
    fn validate(&self) -> Result<()> {
        let schema = format!("agent-reports/{}@1", self.kind);
        if !["thread", "status", "tasks", "card"].contains(&self.kind.as_str())
            || !crate::usage::text_ok(&self.writer, 128)
            || !crate::usage::text_ok(&self.object_id, 512)
            || self.seq > (1u64 << 53) - 1
            || self.doc["schema"].as_str() != Some(&schema)
            || self.doc["writer"].as_str() != Some(&self.writer)
            || self.doc["seq"].as_u64() != Some(self.seq)
            || self.source_sha256.len() != 64
            || !self
                .source_sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || (self.kind == "card" && self.doc["id"].as_str() != Some(&self.object_id))
            || (self.kind != "card" && self.object_id != self.writer)
            || !self.ids.is_object()
            || serde_json::to_vec(&self.ids)?.len() > 16 * 1024
            || serde_json::to_vec(&self.doc)?.len() > MAX_DOC
            || report_secret::find_secret_in("", &self.doc).is_some()
            || report_secret::find_secret_in("", &self.ids).is_some()
        {
            fail!("invalid", "capture report failed validation");
        }
        Ok(())
    }
    fn object_key(&self) -> Vec<u8> {
        Sha256::digest(serde_json::to_vec(&[&self.kind, &self.writer, &self.object_id]).unwrap()).to_vec()
    }
    fn revision_key(&self) -> Key {
        let hash: Vec<u8> = (0..64)
            .step_by(2)
            .map(|at| u8::from_str_radix(&self.source_sha256[at..at + 2], 16).unwrap())
            .collect();
        Key::new().bytes(&self.object_key()).u64(self.seq).bytes(&hash)
    }
}

fn create(txn: &WriteTransaction) -> Result<()> {
    for table in [HEAD, REVISION, CONTENT] {
        txn.open_table(table)?;
    }
    txn.open_table(BY_TIME)?;
    Ok(())
}

pub struct CaptureProjector;
impl Projector for CaptureProjector {
    fn name(&self) -> &'static str {
        "capture"
    }
    fn version(&self) -> u32 {
        1
    }
    fn reads(&self, log: LogId) -> bool {
        matches!(log, LogId::Ledger)
    }
    fn reset(&self, txn: &WriteTransaction) -> Result<()> {
        for table in [HEAD, REVISION, CONTENT] {
            txn.delete_table(table)?;
        }
        txn.delete_table(BY_TIME)?;
        create(txn)
    }
    fn apply(&self, txn: &WriteTransaction, _: LogId, event: &Event) -> Result<Vec<Finding>> {
        create(txn)?;
        if event.event_type != "capture.report" {
            return Ok(Vec::new());
        }
        let report: Report = serde_json::from_value(msgpack::to_json(&event.payload))?;
        report.validate()?;
        let key = report.revision_key();
        if txn.open_table(REVISION)?.get(key.as_slice())?.is_some() {
            return Ok(Vec::new());
        }
        let raw = msgpack::encode(&msgpack::from_json(&report.doc)?)?;
        let content_hash = fsx::sha256_hex(&raw);
        if txn.open_table(CONTENT)?.get(content_hash.as_bytes())?.is_none() {
            let compressed = zstd::bulk::compress(&raw, 3)?;
            let mut encoded = vec![if compressed.len() < raw.len() { 2 } else { 1 }];
            encoded.extend_from_slice(&(raw.len() as u32).to_be_bytes());
            encoded.extend_from_slice(if encoded[0] == 2 { &compressed } else { &raw });
            txn.open_table(CONTENT)?
                .insert(content_hash.as_bytes(), encoded.as_slice())?;
        }
        let mut metadata = serde_json::to_value(&report)?;
        metadata.as_object_mut().unwrap().remove("doc");
        metadata["contentHash"] = json!(content_hash);
        metadata["ingestedAtMs"] = json!(event.observed_at_ms);
        let bytes = db::pack(&metadata)?;
        txn.open_table(REVISION)?.insert(key.as_slice(), bytes.as_slice())?;
        txn.open_table(HEAD)?
            .insert(report.object_key().as_slice(), key.as_slice())?;
        txn.open_table(BY_TIME)?.insert(
            Key::new().u64(report.written_at_ms).bytes(key.as_slice()).as_slice(),
            (),
        )?;
        db::set_meta(txn, "capture_last_ingested_ms", &event.observed_at_ms.to_string())?;
        Ok(Vec::new())
    }
}

pub fn record(engine: &Engine, reports: Vec<Report>) -> Result<Json> {
    if reports.len() > 64 {
        fail!("invalid", "capture batch too large");
    }
    for report in &reports {
        report.validate()?;
        // Leave room for the event envelope, assigned event id and sequence. Reject before
        // any append rather than relying on the frame encoder's internal length assertion.
        if msgpack::encode(&msgpack::from_json(&serde_json::to_value(report)?)?)?.len()
            > crate::frame::MAX_PAYLOAD - 4096
        {
            fail!("invalid", "normalized capture exceeds the log frame budget");
        }
    }
    engine.commit(LogId::Ledger, |batch| {
        let mut stored = 0;
        for report in &reports {
            if batch
                .txn
                .open_table(REVISION)?
                .get(report.revision_key().as_slice())?
                .is_some()
            {
                continue;
            }
            let mut event = Event::new(
                "capture.report",
                now_ms(),
                msgpack::from_json(&serde_json::to_value(report)?)?,
            );
            event.source_at_ms = Some(report.written_at_ms);
            batch.push(event)?;
            stored += 1;
        }
        Ok(json!({ "stored": stored, "duplicates": reports.len() - stored }))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub kind: String,
    pub writer: String,
    pub object_id: String,
    pub limit: Option<usize>,
}

pub fn history(engine: &Engine, query: Query) -> Result<Json> {
    let source = Report {
        kind: query.kind,
        writer: query.writer,
        object_id: query.object_id,
        seq: 0,
        written_at_ms: 0,
        observed_at_ms: 0,
        source_sha256: String::new(),
        ids: Json::Null,
        doc: Json::Null,
    };
    engine.with_db(|database| {
        let read = database.begin_read()?;
        let revisions = read.open_table(REVISION)?;
        let content = read.open_table(CONTENT)?;
        let prefix = source.object_key();
        let mut out = Vec::new();
        let limit = query.limit.unwrap_or(100).clamp(1, 1000);
        let mut truncated = false;
        for row in revisions
            .range(prefix.as_slice()..db::prefix_end(&prefix).as_slice())?
            .take(limit + 1)
        {
            if out.len() == limit {
                truncated = true;
                break;
            }
            let (_, value) = row?;
            let mut metadata: Json = db::unpack(value.value())?;
            let hash = metadata["contentHash"].as_str().unwrap_or("");
            let Some(stored) = content.get(hash.as_bytes())? else {
                fail!("corrupt", "capture content missing");
            };
            let bytes = stored.value();
            if bytes.len() < 5 {
                fail!("corrupt", "capture content short");
            }
            let length = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
            if length > MAX_DOC {
                fail!("corrupt", "capture content too large");
            }
            let decoded = match bytes[0] {
                1 => bytes[5..].to_vec(),
                2 => zstd::bulk::decompress(&bytes[5..], length)?,
                _ => {
                    fail!("corrupt", "capture content version");
                }
            };
            if decoded.len() != length || fsx::sha256_hex(&decoded) != hash {
                fail!("corrupt", "capture content checksum");
            }
            metadata["doc"] = msgpack::to_json(&msgpack::decode(&decoded)?);
            out.push(metadata);
        }
        Ok(json!({ "revisions": out, "truncated": truncated }))
    })
}

pub fn health(engine: &Engine) -> Result<Json> {
    engine.with_db(|database| {
        let read = database.begin_read()?;
        let revisions = read.open_table(REVISION)?;
        let contents = read.open_table(CONTENT)?;
        let meta = read.open_table(db::META)?;
        let last = meta.get("capture_last_ingested_ms")?.and_then(|v| String::from_utf8_lossy(v.value()).parse::<u64>().ok());
        Ok(json!({ "policy":"forever", "lastIngestedAtMs":last, "objects":read.open_table(HEAD)?.len()?, "revisions":revisions.len()?,
            "contents":contents.len()?, "revisionBytes":revisions.stats()?.stored_bytes(), "contentBytes":contents.stats()?.stored_bytes(), }))
    })
}
