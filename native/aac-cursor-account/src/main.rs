//! `aac-cursor-account`: AAC's Cursor account reader (DESIGN 17). One JSON line out per command;
//! never a key, a token, the account's id or text Cursor wrote back.
//!
//!   aac-cursor-account key put    --ccs-dir <dir>   (the key on stdin)
//!   aac-cursor-account key status --ccs-dir <dir>
//!   aac-cursor-account key delete --ccs-dir <dir>
//!   aac-cursor-account read       --ccs-dir <dir>   (stdin: {"sinceMs": n, "untilMs": n})
//!   aac-cursor-account dedupe                       (stdin: {"account": rows, "local": rows})
//!   aac-cursor-account quota      --ccs-dir <dir>
//!   aac-cursor-account --version

use aac_cursor_account::{api, dedupe, keys};
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;

const MAX_KEY_INPUT: u64 = 4096;
const MAX_REQUEST: u64 = 16 * 1024 * 1024;

fn stdin(limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin().take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

fn print(value: Value) {
    println!("{value}");
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Tests point the client at a loopback fake; anything that is not loopback is ignored.
fn client() -> api::Client {
    api::Client::new(std::env::var("AAC_CURSOR_API_BASE").ok().as_deref())
}

/// The target this binary was built for, as the build script names it.
fn target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

fn usage() -> ! {
    eprintln!("aac-cursor-account: usage: key put|status|delete, read, dedupe or quota, with --ccs-dir <dir>");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        print(json!({"name": "aac-cursor-account", "version": env!("CARGO_PKG_VERSION"), "protocol": 1,
                     "target": target()}));
        return;
    }
    let ccs = args
        .iter()
        .position(|a| a == "--ccs-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let words: Vec<&str> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| *a != "--ccs-dir" && (*i == 0 || args[i - 1] != "--ccs-dir"))
        .map(|(_, a)| a.as_str())
        .collect();
    match (words.as_slice(), ccs) {
        (["key", "put"], Some(ccs)) => {
            let Some(input) = stdin(MAX_KEY_INPUT) else { usage() };
            let Ok(text) = String::from_utf8(input) else {
                print(json!({"ok": false, "error": "invalid_key"}));
                std::process::exit(1);
            };
            match keys::put(&ccs, text.trim()) {
                Ok(stored) => print(
                    json!({"ok": true, "keyId": stored.key_id, "fingerprint": stored.fingerprint,
                                          "last4": stored.last4}),
                ),
                Err(error) => {
                    let code = if error == keys::KeyError::Invalid {
                        "invalid_key"
                    } else {
                        "store_unsafe"
                    };
                    print(json!({"ok": false, "error": code}));
                    std::process::exit(1);
                }
            }
        }
        (["key", "status"], Some(ccs)) => match keys::load(&ccs) {
            Ok(Some(stored)) => print(json!({"present": true, "keyId": stored.key_id,
                                             "fingerprint": stored.fingerprint, "last4": stored.last4})),
            Ok(None) => print(json!({"present": false})),
            Err(_) => print(json!({"present": false, "error": "store_unsafe"})),
        },
        (["key", "delete"], Some(ccs)) => print(json!({"ok": true, "removed": keys::delete(&ccs)})),
        (["read"], Some(ccs)) => {
            let Some(input) = stdin(MAX_REQUEST) else { usage() };
            let request: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
            let (Some(since), Some(until)) = (
                request.get("sinceMs").and_then(Value::as_i64),
                request.get("untilMs").and_then(Value::as_i64),
            ) else {
                usage()
            };
            let stored = match keys::load(&ccs) {
                Ok(Some(stored)) => stored,
                Ok(None) => return print(json!({"state": "no_key"})),
                Err(_) => return print(json!({"state": "unavailable", "cause": "store_unsafe"})),
            };
            let started = now_ms();
            let client = client();
            let read = client.exchange(&stored.secret).and_then(|token| {
                let account = api::account_hash(&token);
                api::events(&client, &token, since, until).map(|(events, pages)| (account, events, pages))
            });
            match read {
                Ok((account, events, pages)) => print(json!({
                    "state": "read",
                    "readAt": started,
                    "account": account,
                    "key": {"fingerprint": stored.fingerprint, "last4": stored.last4},
                    "events": events.len(),
                    "pages": pages,
                    "rows": api::hourly(&events),
                })),
                Err(api::Failure::Rejected) => print(json!({"state": "key_rejected",
                    "key": {"fingerprint": stored.fingerprint, "last4": stored.last4}})),
                Err(failure) => print(json!({"state": "unavailable", "cause": failure.cause()})),
            }
        }
        (["dedupe"], _) => {
            let Some(input) = stdin(MAX_REQUEST) else { usage() };
            let request: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
            print(dedupe::dedupe(&request));
        }
        (["quota"], Some(ccs)) => {
            let stored = match keys::load(&ccs) {
                Ok(Some(stored)) => stored,
                Ok(None) => return print(json!({"state": "no_key"})),
                Err(_) => return print(json!({"state": "unavailable", "cause": "store_unsafe"})),
            };
            let client = client();
            match client
                .exchange(&stored.secret)
                .and_then(|token| api::quota(&client, &token))
            {
                Ok(quota) => print(json!({"state": "read", "readAt": now_ms(), "quota": quota})),
                Err(api::Failure::Rejected) => print(json!({"state": "key_rejected"})),
                Err(failure) => print(json!({"state": "unavailable", "cause": failure.cause()})),
            }
        }
        _ => usage(),
    }
}
