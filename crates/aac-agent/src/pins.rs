//! Read-only T3 adapter. The status library remains the sole SQLite reader.
use crate::proof;
use aac_core::{Cause, Failure, scrub};
use aac_status::pins::{read_remote_rows, resolve_db_path};
use serde_json::Value;

pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    if !proof::proven(args)
        || args
            .as_object()
            .is_none_or(|m| m.keys().any(|k| k != "dbPath"))
        || args.get("dbPath").is_some_and(|v| !v.is_string())
    {
        return Ok(proof::fallback());
    }
    for key in ["HOME", "USERPROFILE"] {
        if std::env::var_os(key).is_some_and(|s| s.to_str().is_none_or(|s| !proof::ascii(s))) {
            return Ok(proof::fallback());
        }
    }
    // The remote helper uses USERPROFILE on Windows and HOME on Unix, independent
    // of the dashboard process's optional local AAC_T3_STATE_DB override.
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from);
    let path = resolve_db_path(args.get("dbPath").and_then(Value::as_str), None, home)
        .ok_or_else(|| Failure::new(Cause::NotInstalled, "T3 state file is missing"))?;
    let rows = read_remote_rows(&path).map_err(|error| {
        let cause = match error.code {
            "missing" => Cause::NotInstalled,
            "limit" => Cause::OutputTooLarge,
            "shape" => Cause::Damaged,
            _ => Cause::Refused,
        };
        let mut failure = Failure::new(cause, &error.message);
        failure.reason = Some(scrub(error.code));
        failure
    })?;
    // The dependency returns the exact SQL columns and order, including children.
    let result = Value::Array(rows);
    // Native status accepts Unicode; this Python/TS strangler boundary delegates it.
    if !proof::proven(&result) {
        return Ok(proof::fallback());
    }
    Ok(result)
}
