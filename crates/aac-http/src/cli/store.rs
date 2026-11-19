//! `ai-account-center dashboard store <command>`: the history store, ported from
//! src/commands/store-command.ts. The answers come from the same aac-store dispatch the Node
//! binding calls, so the JSON is the store's own; messages and exit codes match the TS command.
//! The Rust binary always carries the store, so the TS "not available on this computer" branch
//! has no counterpart here.

use aac_store::ops::{self, Slot};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const CODES: &[(&str, &str)] = &[
    (
        "lock_held",
        "The dashboard is running and holds the store. Stop it first.",
    ),
    (
        "newer_store",
        "The store was written by a newer AI Account Center. Update, or restore an older backup.",
    ),
    (
        "unreleased_store",
        "This store was written by an unreleased test build; move it aside and AAC will start a new one.",
    ),
    (
        "backup_unverified",
        "The backup does not match its own hashes. Nothing was changed.",
    ),
    (
        "invalid",
        "That is not an AI Account Center store backup folder.",
    ),
];

pub fn help() -> String {
    [
        "History store (usage readings, rollups and their backups)",
        "",
        "Usage: ai-account-center dashboard store <command>",
        "",
        "Commands:",
        "  status                 Show the store state, sizes and last backup",
        "  check                  Re-hash every sealed log segment (dashboard stopped)",
        "  backup                 Write a verified backup now (dashboard stopped)",
        "  restore <backup-dir>   Restore a backup folder (dashboard stopped)",
        "",
        "While the dashboard runs it holds the store; status then reads GET /api/store/status",
        "when AAC_DASHBOARD_URL and AAC_DEVICE_TOKEN (a paired device token) are set.",
        "",
    ]
    .join("\n")
}

fn line(text: &str) {
    println!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

fn code_message(code: &str) -> String {
    CODES
        .iter()
        .find(|(name, _)| *name == code)
        .map(|(_, text)| (*text).to_string())
        .unwrap_or_else(|| format!("The store refused this ({code})."))
}

fn call(slot: &Slot, method: &str, payload: Value) -> Result<Value, aac_store::error::Error> {
    let answer = ops::dispatch(slot, method, &payload.to_string())?;
    serde_json::from_str(&answer)
        .map_err(|error| aac_store::error::Error::new("invalid", error.to_string()))
}

/// The running dashboard holds the store: read its read-only status route when the environment
/// names it, otherwise say how to read it. Nothing secret is printed.
fn via_api() -> Result<i32, aac_store::error::Error> {
    let url = std::env::var("AAC_DASHBOARD_URL").unwrap_or_default();
    let token = std::env::var("AAC_DEVICE_TOKEN").unwrap_or_default();
    let url = url.trim();
    let token = token.trim();
    if url.is_empty() || token.is_empty() {
        line("[i] The dashboard is running and holds the store.");
        line("    Read GET /api/store/status in the dashboard, or set AAC_DASHBOARD_URL and");
        line("    AAC_DEVICE_TOKEN (a paired device token) and run this again.");
        return Ok(0);
    }
    let unknown = |message: String| aac_store::error::Error::new("unknown", message);
    let base = url::Url::parse(url).map_err(|e| unknown(e.to_string()))?;
    let endpoint = base
        .join("/api/store/status")
        .map_err(|e| unknown(e.to_string()))?;
    let client = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| unknown(e.to_string()))?;
    let response = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .map_err(|e| unknown(e.to_string()))?;
    if !response.status().is_success() {
        err(&format!(
            "[X] The dashboard answered {}.\n",
            response.status().as_u16()
        ));
        return Ok(1);
    }
    let body = response.text().map_err(|e| unknown(e.to_string()))?;
    let value: Value = serde_json::from_str(&body).map_err(|e| unknown(e.to_string()))?;
    line(&serde_json::to_string_pretty(&value).map_err(|e| unknown(e.to_string()))?);
    Ok(0)
}

pub fn run(args: &[String], root: &Path) -> i32 {
    let Some((command, rest)) = args.split_first() else {
        print!("{}", help());
        return 1;
    };
    if matches!(command.as_str(), "help" | "--help" | "-h") {
        print!("{}", help());
        return 0;
    }
    if !matches!(command.as_str(), "status" | "check" | "backup" | "restore") {
        err("[X] Unknown store command. Run: ai-account-center dashboard store help\n");
        return 1;
    }
    if (command == "restore" && rest.len() != 1) || (command != "restore" && !rest.is_empty()) {
        err("[X] Usage: ai-account-center dashboard store restore <backup-dir>\n");
        return 1;
    }
    let directory: PathBuf = root.join("store");
    let slot = Slot::new(None);
    let result = (|| -> Result<i32, aac_store::error::Error> {
        if command == "restore" {
            let backup_dir = std::path::absolute(&rest[0])
                .map_err(|error| aac_store::error::Error::new("invalid", error.to_string()))?;
            let answer = call(
                &slot,
                "restore",
                json!({ "dir": directory, "backupDir": backup_dir }),
            )?;
            line(&format!(
                "[OK] Restored backup {}.",
                answer.get("restored").and_then(Value::as_str).unwrap_or("")
            ));
            line(&format!(
                "     The files it replaced are in {}.",
                answer
                    .get("quarantine")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
            return Ok(0);
        }
        if let Err(error) = call(&slot, "open", json!({ "dir": directory })) {
            if error.code == "lock_held" && command == "status" {
                return via_api();
            }
            return Err(error);
        }
        let answer = (|| -> Result<i32, aac_store::error::Error> {
            if command == "status" {
                let status = call(&slot, "status", json!({}))?;
                line(&serde_json::to_string_pretty(&status).map_err(|error| {
                    aac_store::error::Error::new("invalid", error.to_string())
                })?);
            }
            if command == "check" {
                let answer = call(&slot, "maintain", json!({ "verify": true }))?;
                let findings = answer
                    .get("verified")
                    .and_then(|verified| verified.get("findings"))
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                let message = if findings == 0 {
                    "[OK] Every sealed segment matches its hash.".to_string()
                } else {
                    format!("[!] {findings} integrity finding(s); see status.")
                };
                line(&message);
            }
            if command == "backup" {
                let answer = call(
                    &slot,
                    "backup",
                    json!({ "roots": [directory.join("backups")], "withRedb": true }),
                )?;
                line(&format!(
                    "[OK] Backup {} written and verified ({} files).",
                    answer.get("id").and_then(Value::as_str).unwrap_or(""),
                    answer
                        .get("files")
                        .map(Value::to_string)
                        .unwrap_or_default()
                ));
            }
            Ok(0)
        })();
        let _ = call(&slot, "close", json!({}));
        answer
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            err(&format!("[X] {}\n", code_message(error.code)));
            1
        }
    }
}
