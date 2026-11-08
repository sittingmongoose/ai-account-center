//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. The readers are stateless, so there is no open/close and
//! no handle state (unlike aac-store's engine slot).
//!
//! Methods:
//! - `pins` `{ dbPath?: string | null }` -> `{ pinned: [...] }`, or
//!   `{ unavailable: "missing" | "shape" | "io" }` when T3's state file cannot be read as
//!   expected. Unavailability is an answer, not a rejected call.
//! - `parseRemotePins` `{ machine, json }` -> `{ ok: true, pinned: [...] }`, or
//!   `{ ok: false, reason: "shape" }` when a machine's `sqlite3 -json` answer is not the
//!   shape T3's state file has. The ssh run itself is the caller's; a machine that did not
//!   answer never reaches this method.
//! - `mergePins` `{ lists: [{ machine, pinned: [...] }] }` -> `{ pinned: [...] }`: every
//!   machine's good pins merged, de-duplicated by thread id and ordered by pinOrderKey then
//!   machine, each row tagged with its machine.
//! - `compose` `{ nowMs, utcOffsetMinutes, pins, machineProblems, threads, tasks, openCards,
//!   statuses }` -> the composed `pinned` view (pinned.rs), every word decided here.
//! - `parseStatus` `{ writer, contentBase64 }` -> `{ ok: true, doc, sha256 }`, or
//!   `{ ok: false, reason }` with the reader's fixed refusal reason. Bytes travel as base64
//!   so a NUL byte or invalid UTF-8 reaches the validator intact.

use crate::error::Result;
use crate::fail;
use crate::pinned;
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
struct ParseRemotePinsArgs {
    machine: String,
    json: String,
}

fn parse_remote_pins(args: &str) -> Result<Json> {
    let parsed: ParseRemotePinsArgs = parse(args)?;
    match pins::parse_remote_pins(&parsed.machine, &parsed.json) {
        Ok(list) => Ok(json!({ "ok": true, "pinned": list })),
        Err(error) if error.code == "shape" => Ok(json!({ "ok": false, "reason": "shape" })),
        Err(error) => fail!("invalid", "{}", error.message),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergePinsArgs {
    lists: Vec<pins::MachinePinsIn>,
}

/// The machines whose pins can be merged: this computer's own read plus the three remote ones.
const MERGE_MACHINES: [&str; 4] = ["vm", "mac", "windows", "nas1"];

fn merge_pins_method(args: &str) -> Result<Json> {
    let parsed: MergePinsArgs = parse(args)?;
    if parsed
        .lists
        .iter()
        .any(|list| !MERGE_MACHINES.contains(&list.machine.as_str()))
    {
        fail!("invalid", "unknown machine in the pin lists");
    }
    let lists = parsed
        .lists
        .into_iter()
        .map(|list| pins::MachinePins {
            name: list.machine,
            pinned: list.pinned,
        })
        .collect::<Vec<_>>();
    Ok(json!({ "pinned": pins::merge_pins(&lists) }))
}

fn compose(args: &str) -> Result<Json> {
    let input: pinned::ComposeInput = parse(args)?;
    serde_json::to_value(pinned::compose(&input)).map_err(|e| crate::error::Error::new("io", e.to_string()))
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
        "parseRemotePins" => parse_remote_pins(args)?,
        "mergePins" => merge_pins_method(args)?,
        "compose" => compose(args)?,
        "parseStatus" => parse_status(args)?,
        _ => fail!("unknown_method", "unknown method"),
    };
    Ok(answer.to_string())
}
