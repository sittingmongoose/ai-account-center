//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. The readers are stateless, so there is no open/close and
//! no handle state (unlike aac-store's engine slot).
//!
//! Methods:
//! - `pins` `{ dbPath?: string | null }` -> `{ pinned: [...], notShown: number }` (the
//!   first pins in pin order plus how many counted pins past the row budget were not
//!   shown), or `{ unavailable: "missing" | "shape" | "io" | "limit" }` when T3's state
//!   file cannot be read as expected (or its shown pins pass the byte budget).
//!   Unavailability is an answer, not a rejected call.
//! - `parseRemotePins` `{ machine, json }` -> `{ ok: true, pinned: [...], notShown: number }`, or
//!   `{ ok: false, reason: "shape" }` when a machine's `sqlite3 -json` answer is not the
//!   shape T3's state file has. The ssh run itself is the caller's; a machine that did not
//!   answer never reaches this method.
//! - `mergePins` `{ lists: [{ machine, pinned: [...] }] }` -> `{ pinned: [...] }`: every
//!   machine's good pins merged, de-duplicated by thread id and ordered by pinOrderKey then
//!   pinnedAt, then machine, each row tagged with its machine.
//! - `compose` `{ nowMs, utcOffsetMinutes, pins, machineProblems, threads, tasks, openCards,
//!   statuses }` -> the composed `pinned` view (pinned.rs), every word decided here.
//! - `composeSpend` `{ nowMs, utcOffsetMinutes, accountOrder, spend }` -> the composed
//!   `spendByStream` view (spend.rs), a pure function over the SpendView the TypeScript
//!   side builds. An input that does not read as expected rejects the call; the route then
//!   answers the contract's unavailable shape.
//! - `composeByModel` `{ ...scoreboard view v1 }` -> the composed `byModel` view
//!   (scoreboard.rs), a pure function over the Scoreboard view the TypeScript side builds.
//!   An input that does not read as expected rejects the call; the route then serves no
//!   `byModel` field.
//! - `parseStatus` `{ writer, contentBase64 }` -> `{ ok: true, doc, sha256 }`, or
//!   `{ ok: false, reason }` with the reader's fixed refusal reason. Bytes travel as base64
//!   so a NUL byte or invalid UTF-8 reaches the validator intact.

use crate::error::Result;
use crate::fail;
use crate::pinned;
use crate::pins;
use crate::scoreboard;
use crate::spend;
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
        Ok(list) => Ok(json!({ "pinned": list.pinned, "notShown": list.not_shown })),
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
        Ok(list) => Ok(json!({ "ok": true, "pinned": list.pinned, "notShown": list.not_shown })),
        Err(error) if error.code == "shape" => Ok(json!({ "ok": false, "reason": "shape" })),
        Err(error) => fail!("invalid", "{}", error.message),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergePinsArgs {
    lists: Vec<pins::MachinePinsIn>,
}

fn merge_pins_method(args: &str) -> Result<Json> {
    let parsed: MergePinsArgs = parse(args)?;
    if parsed
        .lists
        .iter()
        .any(|list| !crate::computers::known_machine(&list.machine, false))
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

fn compose_spend(args: &str) -> Result<Json> {
    let input: spend::SpendInput = parse(args)?;
    serde_json::to_value(spend::compose(&input)).map_err(|e| crate::error::Error::new("io", e.to_string()))
}

fn compose_groups(args: &str) -> Result<Json> {
    let input: scoreboard::ViewIn = parse(args)?;
    serde_json::to_value(scoreboard::group_keys(&input)?).map_err(|e| crate::error::Error::new("io", e.to_string()))
}

fn compose_by_model(args: &str) -> Result<Json> {
    let input: scoreboard::ViewIn = parse(args)?;
    serde_json::to_value(scoreboard::by_model(&input)?).map_err(|e| crate::error::Error::new("io", e.to_string()))
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

/// The answer keys this process loaded, by directory (the key file's writer lock is held by
/// the caller; the cache only avoids re-reading the PEM on every signature).
static ANSWER_KEYS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, crate::answer_signing::AnswerKey>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn answer_key(dir: &str) -> Result<crate::answer_signing::AnswerKey> {
    if let Some(key) = ANSWER_KEYS.lock().unwrap_or_else(|p| p.into_inner()).get(dir) {
        return Ok(key.clone());
    }
    let key = crate::answer_signing::load_or_create_answer_key(std::path::Path::new(dir))
        .map_err(|e| crate::error::Error::new("io", e))?;
    ANSWER_KEYS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(dir.to_string(), key.clone());
    Ok(key)
}

fn answer_key_load(args: &str) -> Result<Json> {
    #[derive(Deserialize)]
    struct Args {
        dir: String,
    }
    let args: Args = parse(args)?;
    let key = answer_key(&args.dir)?;
    Ok(json!({"publicKeyPem": key.public_key_pem, "keyId": key.key_id}))
}

fn answer_sign(args: &str) -> Result<Json> {
    #[derive(Deserialize)]
    struct Args {
        dir: String,
        unsigned: Json,
    }
    let args: Args = parse(args)?;
    let key = answer_key(&args.dir)?;
    Ok(crate::answer_signing::sign_answer(&args.unsigned, &key))
}

fn answer_verify(args: &str) -> Result<Json> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        text: String,
        public_key_pem: String,
        schema: String,
    }
    let args: Args = parse(args)?;
    match crate::answer_signing::verify_signed_document(args.text.as_bytes(), &args.public_key_pem, &args.schema) {
        Ok(answer) => Ok(json!({"ok": true, "answer": answer})),
        Err(reason) => Ok(json!({"ok": false, "reason": reason})),
    }
}

pub fn dispatch(method: &str, args: &str) -> Result<String> {
    let answer = match method {
        "resetCodexSource" => crate::reset_source::dispatch(args)
            .map_err(|code| crate::error::Error::new(code, "Reset source unavailable."))?,
        "resetClaudeSource" => crate::reset_claude_source::dispatch(args)
            .map_err(|code| crate::error::Error::new(code, "Reset source unavailable."))?,
        "resetNormalizeClaude" => crate::reset_inventory::normalize_claude(args)
            .map_err(|code| crate::error::Error::new(code, "Reset inventory unavailable."))?,
        "resetClaims" => {
            crate::reset::dispatch(args).map_err(|code| crate::error::Error::new(code, "Reset operation refused."))?
        }
        "pins" => pins(args)?,
        "parseRemotePins" => parse_remote_pins(args)?,
        "mergePins" => merge_pins_method(args)?,
        "compose" => compose(args)?,
        "composeSpend" => compose_spend(args)?,
        "composeByModel" => compose_by_model(args)?,
        "composeGroups" => compose_groups(args)?,
        "parseStatus" => parse_status(args)?,
        "answerKeyLoad" => answer_key_load(args)?,
        "answerSign" => answer_sign(args)?,
        "answerVerify" => answer_verify(args)?,
        _ => fail!("unknown_method", "unknown method"),
    };
    let mut answer = answer;
    if matches!(
        method,
        "resetClaims" | "resetCodexSource" | "resetClaudeSource" | "resetNormalizeClaude"
    ) {
        // Keep the pre-share-reader ABI on the confirmation and ledger routes.
        answer.sort_all_objects();
    }
    Ok(answer.to_string())
}
