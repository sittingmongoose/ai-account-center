//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. The readers are stateless, so there is no open/close and
//! no handle state (unlike aac-store's engine slot).
//!
//! Methods:
//! - `pins` `{ dbPath?: string | null }` -> `{ pinned: [...] }`, or
//!   `{ unavailable: "missing" | "shape" | "io" }` when T3's state file cannot be read as
//!   expected. Unavailability is an answer, not a rejected call.

use crate::error::Result;
use crate::fail;
use crate::pins;
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

pub fn dispatch(method: &str, args: &str) -> Result<String> {
    let answer = match method {
        "pins" => pins(args)?,
        _ => fail!("unknown_method", "unknown method"),
    };
    Ok(answer.to_string())
}
