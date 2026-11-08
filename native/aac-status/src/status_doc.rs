//! The `agent-reports/status@1` document: one file per reporting thread at
//! `writers/<writer>/status.json` on the share, holding what `thread@1` and `tasks.json`
//! lack for a status update (its own words, percent estimate, done/left lists, blockers and
//! its looping answer). The schema is published beside the others at
//! `docs/agent-reports/schemas/status.json`.
//!
//! A document is projected field by field from an allowlist, like every other report kind:
//! unknown fields are ignored. The share rules applied here are the per-file ones (size cap,
//! NUL refused, the writer must match its folder, real calendar timestamps, no secrets);
//! listing first, keeping the last good copy and `seq` ordering belong to the reader that
//! owns the scan, as with the other kinds.

use crate::validate::{Invalid, Valid, count, fail, head, json_from_bytes, list, obj, one_of, opt_text, slug, text};
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

pub const STATUS_SCHEMA: &str = "agent-reports/status@1";

/// The size cap for one status file, as for a card.
pub const STATUS_SIZE_CAP: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDoc {
    pub writer: String,
    pub seq: u64,
    pub written_at: String,
    /// The T3 thread this update belongs to (matched against the pinned threads).
    pub t3_thread: String,
    /// One or two sentences, as when the owner asks for a status update.
    pub summary: String,
    /// The thread's own percent estimate (0-100), or null when it gives none.
    pub percent: Option<u64>,
    pub done: Vec<StatusItem>,
    pub left: Vec<StatusItem>,
    pub blockers: Vec<Blocker>,
    /// The thread's own looping answer, or null when it gives none.
    pub looping: Option<Looping>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusItem {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub text: String,
    /// Who it waits on. Documented kinds: owner, agent, review, lock, resource, quota; any
    /// short slug is accepted and shown as written, as for `blockedOn.kind`.
    pub waiting_on: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LoopingState {
    #[serde(rename = "yes")]
    Yes,
    #[serde(rename = "no")]
    No,
    #[serde(rename = "unsure")]
    Unsure,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Looping {
    pub state: LoopingState,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedStatus {
    pub doc: StatusDoc,
    /// SHA-256 of the file's bytes, as every other parsed report carries.
    pub sha256: String,
}

fn item(value: &Json, field: &str) -> Valid<StatusItem> {
    let row = obj(Some(value), field)?;
    Ok(StatusItem {
        id: text(row.get("id"), &format!("{field}.id"), 64)?,
        title: text(row.get("title"), &format!("{field}.title"), 300)?,
    })
}

fn items(value: Option<&Json>, field: &str) -> Valid<Vec<StatusItem>> {
    let rows = list(value, field, 200, item)?;
    let mut seen = std::collections::HashSet::new();
    if rows.iter().any(|row| !seen.insert(row.id.clone())) {
        return Err(Invalid(format!("{field} repeat an id")));
    }
    Ok(rows)
}

fn blocker(value: &Json, field: &str) -> Valid<Blocker> {
    let row = obj(Some(value), field)?;
    Ok(Blocker {
        text: text(row.get("text"), &format!("{field}.text"), 500)?,
        waiting_on: slug(row.get("waitingOn"), &format!("{field}.waitingOn"))?,
    })
}

fn looping(value: Option<&Json>) -> Valid<Option<Looping>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let row = obj(Some(value), "looping")?;
    let state = match one_of(row.get("state"), "looping.state", &["yes", "no", "unsure"])?.as_str() {
        "yes" => LoopingState::Yes,
        "no" => LoopingState::No,
        _ => LoopingState::Unsure,
    };
    Ok(Some(Looping {
        state,
        text: opt_text(row.get("text"), "looping.text", 500)?,
    }))
}

fn percent(value: Option<&Json>) -> Valid<Option<u64>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        other => {
            let value = count(other, "percent")?;
            if value > 100 {
                return Err(fail("percent"));
            }
            Ok(Some(value))
        }
    }
}

/// Bytes of one `status.json` -> its projected document, or the fixed refusal reason.
pub fn parse_status_file(bytes: &[u8], writer: &str) -> Result<ParsedStatus, String> {
    let raw = json_from_bytes(bytes, STATUS_SIZE_CAP)?;
    let doc = (|| -> Valid<StatusDoc> {
        let doc = obj(Some(&raw), "document")?;
        let head = head(doc, STATUS_SCHEMA, writer)?;
        Ok(StatusDoc {
            writer: head.writer,
            seq: head.seq,
            written_at: head.written_at,
            t3_thread: text(doc.get("t3Thread"), "t3Thread", 200)?,
            summary: text(doc.get("summary"), "summary", 500)?,
            percent: percent(doc.get("percent"))?,
            done: items(doc.get("done"), "done")?,
            left: items(doc.get("left"), "left")?,
            blockers: list(doc.get("blockers"), "blockers", 50, blocker)?,
            looping: looping(doc.get("looping"))?,
        })
    })()
    .map_err(|Invalid(reason)| reason)?;
    Ok(ParsedStatus {
        doc,
        sha256: Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect(),
    })
}
