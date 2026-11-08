//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. The readers are stateless, so there is no open/close and
//! no handle state (unlike aac-store's engine slot).
//!
//! Methods:
//! - `pins` `{ dbPath?: string | null }` -> `{ pinned: [...] }`, or
//!   `{ unavailable: "missing" | "shape" | "io" }` when T3's state file cannot be read as
//!   expected. Unavailability is an answer, not a rejected call.
//! - `parseStatus` `{ writer, contentBase64 }` -> `{ ok: true, doc, sha256 }`, or
//!   `{ ok: false, reason }` with the reader's fixed refusal reason. Bytes travel as base64
//!   so a NUL byte or invalid UTF-8 reaches the validator intact.

use crate::error::Result;
use crate::fail;
use crate::pins;
use crate::status_doc;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value as Json, json};

fn parse<T: serde::de::DeserializeOwned>(args: &str) -> Result<T> {
    serde_json::from_str(args).map_err(|e| crate::error::Error::new("invalid", e.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinsArgs {
    db_path: Option<String>,
}

fn pins(args: &str) -> Result<Json> {
    let parsed: PinsArgs = if args.trim().is_empty() {
        PinsArgs { db_path: None }
    } else {
        parse(args)?
    };
    let Some(path) = pins::default_db_path(parsed.db_path.as_deref()) else {
        return Ok(json!({ "unavailable": "missing" }));
    };
    match pins::read_pinned_threads(&path) {
        Ok(list) => Ok(json!({ "pinned": list })),
        Err(error) => Ok(json!({ "unavailable": error.code })),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseStatusArgs {
    writer: String,
    content_base64: String,
}

fn parse_status(args: &str) -> Result<Json> {
    let parsed: ParseStatusArgs = parse(args)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&parsed.content_base64)
        .map_err(|_| crate::error::Error::new("invalid", "contentBase64 is not valid base64"))?;
    match status_doc::parse_status_file(&bytes, &parsed.writer) {
        Ok(document) => Ok(json!({ "ok": true, "doc": document.doc, "sha256": document.sha256 })),
        Err(reason) => Ok(json!({ "ok": false, "reason": reason })),
    }
}

pub fn dispatch(method: &str, args: &str) -> Result<String> {
    let answer = match method {
        "pins" => pins(args)?,
        "parseStatus" => parse_status(args)?,
        _ => fail!("unknown_method", "unknown method"),
    };
    Ok(answer.to_string())
}
