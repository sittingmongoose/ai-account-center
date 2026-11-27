//! Pure-Rust base Status and Scoreboard fixtures and shared JS-parity JSON/time helpers.
//!
//! Produces byte-for-byte identical output to `scripts/status-fixture.mjs` (called with
//! `{ ccsDir: null, withKinds: true, withVideo: false, withQuota: true, now }`) and
//! `scripts/scoreboard-fixture.mjs` (called with `{ now }`).

use base64::Engine as _;
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const MIN_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MIN_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

const TINY_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAFklEQVR4nGP4z8DwHwyBNAMDA8MZBgBVlgf5j5xnFAAAAABJRU5ErkJggg==";

/// Convert an `f64` into a JSON number formatted identically to ECMAScript `JSON.stringify`:
/// integral values within `[-2^53+1, 2^53-1]` serialize without `.0`, `-0.0` becomes `0`.
pub fn js_num(v: f64) -> Json {
    if !v.is_finite() {
        return Json::Null;
    }
    if v == 0.0 {
        return Json::from(0_i64);
    }
    if v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 {
        return Json::from(v as i64);
    }
    serde_json::Number::from_f64(v)
        .map(Json::Number)
        .unwrap_or(Json::Null)
}

/// Round `v` using ECMAScript `Number.prototype.toFixed(places)` and parse back to `f64`.
pub fn to_fixed_num(v: f64, places: u32) -> f64 {
    aac_num::ecma::to_fixed(v, places)
        .and_then(|s| s.parse::<f64>().ok())
        .map(|n| if n == 0.0 { 0.0 } else { n })
        .unwrap_or(v)
}

/// Round `v` with `to_fixed_num` and encode as an ECMAScript-identical JSON number.
pub fn to_fixed_json(v: f64, places: u32) -> Json {
    js_num(to_fixed_num(v, places))
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ` (`Date.prototype.toISOString`).
pub fn iso_ms(ms: i64) -> String {
    aac_num::ecma::to_iso_string(ms).unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// Format epoch milliseconds as `YYYY-MM-DDTHH:MM:SSZ` (`isoMs` with `.sss` stripped).
pub fn iso_sec(ms: i64) -> String {
    let full = iso_ms(ms);
    if let Some((prefix, _)) = full.rsplit_once('.') {
        format!("{prefix}Z")
    } else {
        full
    }
}

/// Set access and modification timestamps on a file or directory.
pub fn set_path_mtime_ms(path: &Path, mtime_ms: i64) -> Result<(), Box<dyn Error>> {
    let time = if mtime_ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(mtime_ms as u64)
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_millis(mtime_ms.unsigned_abs()))
            .unwrap_or(UNIX_EPOCH)
    };
    let times = FileTimes::new().set_accessed(time).set_modified(time);
    let file = File::open(path)?;
    file.set_times(times)?;
    Ok(())
}

/// Write compact JSON followed by `\n`, optionally stamping `mtime_ms`.
pub fn write_json_compact(
    file: &Path,
    value: &Json,
    mtime_ms: Option<i64>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    fs::write(file, &bytes)?;
    if let Some(ms) = mtime_ms {
        set_path_mtime_ms(file, ms)?;
    }
    Ok(bytes)
}

/// Write pretty-printed JSON with `indent_spaces` spaces per level followed by `\n`.
pub fn write_json_pretty(
    file: &Path,
    value: &Json,
    indent_spaces: usize,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let indent = vec![b' '; indent_spaces];
    let formatter = serde_json::ser::PrettyFormatter::with_indent(&indent);
    let mut bytes = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    serde::Serialize::serialize(value, &mut ser)?;
    bytes.push(b'\n');
    fs::write(file, &bytes)?;
    Ok(bytes)
}

/// Canonical JSON matching `canonicalJson` in `memory-fixture.mjs`:
/// object keys sorted by UTF-16 code unit at every depth, compact, arrays in order.
pub fn canonical_json(value: &Json) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (idx, key) in keys.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                if let Ok(encoded) = serde_json::to_string(key) {
                    out.push_str(&encoded);
                }
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Json::String(text) => {
            if let Ok(encoded) = serde_json::to_string(text) {
                out.push_str(&encoded);
            }
        }
        Json::Number(number) => {
            const EXACT: u64 = 1 << 53;
            match (number.as_u64(), number.as_i64()) {
                (Some(u), _) if u <= EXACT => out.push_str(&u.to_string()),
                (_, Some(i)) if i.unsigned_abs() <= EXACT => out.push_str(&i.to_string()),
                _ => {
                    let x = number.as_f64().unwrap_or(f64::NAN);
                    if x.is_finite() {
                        out.push_str(&aac_num::ecma::to_string(x));
                    } else {
                        out.push_str("null");
                    }
                }
            }
        }
        Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Json::Null => out.push_str("null"),
    }
}

fn insert_fields(target: &mut Map<String, Json>, extra: Json) {
    if let Json::Object(map) = extra {
        for (k, v) in map {
            target.insert(k, v);
        }
    }
}

fn status_option(
    key: &str,
    label: &str,
    gets: Option<&str>,
    costs: Option<&str>,
    media: Vec<Json>,
    sections: Vec<Json>,
) -> Json {
    json!({
        "key": key,
        "label": label,
        "gets": gets,
        "costs": costs,
        "media": media,
        "sections": sections,
    })
}

fn quota_report(now_ms: i64) -> Json {
    let at = |ago: i64| iso_ms(now_ms - ago);
    let method = "token-share-at-list-price";
    let row = |stream: &str, account: &str, meter: &str, unit: &str, value: Option<f64>| {
        json!({
            "stream": stream,
            "account": account,
            "meter": meter,
            "unit": unit,
            "value": value.map(js_num).unwrap_or(Json::Null),
            "estimate": true,
            "method": method,
        })
    };
    let rows = |scale: f64| {
        vec![
            row(
                "F",
                "codex:account-a",
                "credits_balance",
                "credits",
                Some(18.28 * scale),
            ),
            row(
                "F",
                "claude:account-b",
                "seven_day",
                "percent",
                Some(4.4 * scale),
            ),
            row(
                "TOOLS",
                "codex:account-a",
                "credits_balance",
                "credits",
                Some(6.1 * scale),
            ),
            row(
                "TOOLS",
                "claude:account-b",
                "five_hour",
                "percent",
                Some(12.5 * scale),
            ),
            row(
                "STATUS",
                "claude:account-c",
                "seven_day",
                "percent",
                Some(2.2 * scale),
            ),
            row(
                "(no stream)",
                "claude:account-b",
                "five_hour",
                "percent",
                Some(34.3 * scale),
            ),
            row(
                "(no stream)",
                "codex:account-a",
                "credits_balance",
                "credits",
                None,
            ),
        ]
    };
    let period = |id: Option<&str>, start: i64, coverage: f64, scale: f64| {
        json!({
            "period": id,
            "start": at(start),
            "end": at(3 * MIN_MS),
            "coverage": js_num(coverage),
            "estimate": true,
            "method": method,
            "rows": rows(scale),
        })
    };
    let budget = |stream: &str,
                  desc: &str,
                  account: &str,
                  unit: &str,
                  meter: &str,
                  per: &str,
                  limit: Option<f64>,
                  use_val: f64,
                  known: bool,
                  percent: Option<f64>,
                  coverage: f64| {
        let period_val = if per == "window" {
            Json::Null
        } else {
            Json::String(format!("{per}-fixture"))
        };
        json!({
            "stream": stream,
            "index": 0,
            "desc": desc,
            "accounts": [account],
            "unit": unit,
            "meter": meter,
            "per": per,
            "limit": limit.map(js_num).unwrap_or(Json::Null),
            "use": js_num(use_val),
            "known": known,
            "percent": percent.map(js_num).unwrap_or(Json::Null),
            "period": period_val,
            "coverage": js_num(coverage),
            "estimate": true,
            "method": method,
        })
    };
    json!({
        "schema": "quota-report@1",
        "writer": "vm.quota-watch",
        "seq": 42,
        "writtenAt": at(3 * MIN_MS),
        "generatedAt": at(3 * MIN_MS),
        "estimate": true,
        "methodId": method,
        "method": "Each account meter's observed use over the window (credits spent, or percentage points of a usage window, from AI Account Center's readings) is split by each session's share of that account's tokens, weighted by list prices per model. Every per-thread and per-stream number is an estimate.",
        "streamEstimates": {
            "day": period(Some("day-fixture"), 10 * HOUR_MS, 0.67, 1.0),
            "week": period(Some("week-fixture"), 4 * DAY_MS, 0.09, 2.4),
            "window": period(None, 6 * HOUR_MS, 1.0, 0.8),
        },
        "budgetRows": [
            budget("F", "Codex credits", "codex:account-a", "credits", "credits_balance", "day", None, 18.28, true, None, 0.67),
            budget("TOOLS", "Claude 5-hour", "claude:account-b", "percent", "five_hour", "window", Some(25.0), 26.1, true, Some(104.4), 1.0),
            budget("STATUS", "Claude weekly", "claude:account-c", "percent", "seven_day", "week", Some(20.0), 5.3, false, Some(26.5), 0.09),
        ],
        "unattributedRows": [
            {
                "account": "codex:account-a",
                "meter": "credits_balance",
                "unit": "credits",
                "amount": 470.53,
                "reason": "(no stream)",
                "why": "threads no stream rule claims",
                "estimate": true,
                "method": method,
            },
            {
                "account": "claude:account-b",
                "meter": "seven_day",
                "unit": "percent",
                "amount": null,
                "reason": "(no reading)",
                "why": "the meter had no reading in the window",
                "estimate": true,
                "method": method,
            },
            {
                "account": "claude:account-c",
                "meter": "five_hour",
                "unit": "percent",
                "amount": 1.2,
                "reason": "(no session seen)",
                "why": "measured use with no session to split it",
                "estimate": false,
                "method": method,
            },
        ],
        "machinesNotScanned": [
            {
                "name": "windows",
                "thisPass": true,
                "error": "ssh: connect timed out",
                "runsMissed": 2,
                "runsInWindow": 36,
            }
        ],
    })
}

/// Write the base Status fixture matching `writeStatusFixture(root, { ccsDir: null, withKinds: true, withVideo: false, withQuota: true, now })`.
pub fn write_status_fixture_for_perf(root: &Path, now_ms: i64) -> Result<(), Box<dyn Error>> {
    let v1 = root.join("v1");
    let writers = v1.join("writers");
    if writers.exists() {
        return Err(format!(
            "{} already holds v1/writers; use an empty folder.",
            root.display()
        )
        .into());
    }
    let at = |ago: i64| iso_ms(now_ms - ago);
    let mut seqs: BTreeMap<String, i64> = BTreeMap::new();
    let mut next_head = |schema: &str, writer: &str, ago: i64| -> Map<String, Json> {
        let entry = seqs.entry(writer.to_string()).or_insert(10);
        *entry += 1;
        let seq = *entry;
        let mut m = Map::new();
        m.insert("schema".into(), Json::String(schema.into()));
        m.insert("writer".into(), Json::String(writer.into()));
        m.insert("seq".into(), Json::from(seq));
        m.insert("writtenAt".into(), Json::String(at(ago)));
        m
    };

    let boards_dir = root.join("boards");
    let make_board = |name: &str, ago: i64| -> Result<String, Box<dyn Error>> {
        fs::create_dir_all(&boards_dir)?;
        let file = boards_dir.join(format!("{name}.md"));
        fs::write(
            &file,
            format!("# {name} board\n\nThe human record of this stream.\n"),
        )?;
        set_path_mtime_ms(&file, now_ms - ago)?;
        Ok(file.to_string_lossy().into_owned())
    };

    let closure_board = make_board("C-closure", 3 * HOUR_MS)?;

    // Helper for threads
    struct ThreadSpec<'a> {
        writer: &'a str,
        ago: i64,
        project: Option<&'a str>,
        stream: Option<&'a str>,
        state: &'a str,
        ttl_seconds: Option<i64>,
        doing: Option<&'a str>,
        harness: &'a str,
        model: &'a str,
        effort: &'a str,
        account: &'a str,
        parent: Option<&'a str>,
        blocked_on: Option<Json>,
        progress: Option<Json>,
        links: Vec<Json>,
    }
    let default_thread = |writer: &'static str, ago: i64, state: &'static str| ThreadSpec {
        writer,
        ago,
        project: None,
        stream: None,
        state,
        ttl_seconds: Some(1800),
        doing: None,
        harness: "claude-code",
        model: "claude-opus-5-5",
        effort: "high",
        account: "claude_main",
        parent: None,
        blocked_on: None,
        progress: None,
        links: Vec::new(),
    };

    let thread_specs = vec![
        ThreadSpec {
            project: Some("puppet-master"),
            stream: Some("F"),
            doing: Some("Landing group 4 of the PM7 follow-ups"),
            progress: Some(json!({"done": 7, "total": 12, "unit": "items"})),
            ..default_thread("vm.pm-fo-lead", 4 * MIN_MS, "working")
        },
        ThreadSpec {
            project: Some("puppet-master"),
            stream: Some("TOOLS"),
            doing: Some("Blind pre-check of browser-check 2.1"),
            effort: "xhigh",
            ..default_thread("vm.tools-lead", 9 * MIN_MS, "waiting")
        },
        ThreadSpec {
            project: Some("ai-account-center"),
            stream: Some("STATUS"),
            doing: Some("Status tab page and phone layout"),
            progress: Some(json!({"done": 3, "total": 8, "unit": "items"})),
            ..default_thread("vm.aac-status-lead", 2 * MIN_MS, "working")
        },
        ThreadSpec {
            project: Some("ai-account-center"),
            stream: Some("SIGNIN"),
            ttl_seconds: None,
            doing: Some("Sign-in renewal landed; runner reinstalled"),
            ..default_thread("mac.aac-claude-relogin", 50 * MIN_MS, "done")
        },
        ThreadSpec {
            project: Some("puppet-master"),
            stream: Some("C"),
            doing: Some("Re-audit batch B09"),
            blocked_on: Some(json!({
                "kind": "owner",
                "what": "Which reviewer pair to trust for B09",
                "since": at(40 * MIN_MS),
            })),
            links: vec![json!({"label": "Board", "path": closure_board})],
            ..default_thread("vm.pm-closure-lead", 12 * MIN_MS, "blocked")
        },
        ThreadSpec {
            project: Some("puppet-master"),
            stream: Some("F"),
            doing: Some("Windows release build (waiting for a quiet PC)"),
            model: "gpt-6.1-sol",
            harness: "codex",
            account: "codex_main",
            parent: Some("pm-fo-lead"),
            ..default_thread("windows.pm-win-build", 3 * HOUR_MS, "paused")
        },
        ThreadSpec {
            doing: Some("Nightly share backup check"),
            model: "glm-5.3",
            harness: "zcode",
            account: "zcode",
            ..default_thread("nas1.nas-backup", 20 * MIN_MS, "idle")
        },
        ThreadSpec {
            project: Some("puppet-master"),
            stream: Some("F"),
            doing: Some("Welcome PiP look glitch (FO-41)"),
            parent: Some("pm-fo-lead"),
            ..default_thread("vm.fo-welcome", 6 * MIN_MS, "working")
        },
    ];

    for spec in thread_specs {
        let dot = spec.writer.find('.').unwrap_or(0);
        let machine = &spec.writer[..dot];
        let name = &spec.writer[dot + 1..];
        let mut doc = next_head("agent-reports/thread@1", spec.writer, spec.ago);
        doc.insert(
            "ttlSeconds".into(),
            spec.ttl_seconds.map(Json::from).unwrap_or(Json::Null),
        );
        doc.insert(
            "agent".into(),
            json!({
                "name": name,
                "harness": spec.harness,
                "model": spec.model,
                "effort": spec.effort,
                "account": spec.account,
                "machine": machine,
                "t3Thread": null,
                "parent": spec.parent,
            }),
        );
        doc.insert(
            "project".into(),
            spec.project.map(Into::into).unwrap_or(Json::Null),
        );
        doc.insert(
            "stream".into(),
            spec.stream.map(Into::into).unwrap_or(Json::Null),
        );
        doc.insert("state".into(), Json::String(spec.state.into()));
        doc.insert(
            "doing".into(),
            spec.doing.map(Into::into).unwrap_or(Json::Null),
        );
        doc.insert("blockedOn".into(), spec.blocked_on.unwrap_or(Json::Null));
        doc.insert("progress".into(), spec.progress.unwrap_or(Json::Null));
        doc.insert("links".into(), Json::Array(spec.links));
        write_json_pretty(
            &writers.join(spec.writer).join("thread.json"),
            &Json::Object(doc),
            2,
        )?;
    }

    // Streams with tasks.json (4)
    let task_item = |id: &str,
                     title: &str,
                     state: &str,
                     ago: i64,
                     owner: Option<&str>,
                     branch: Option<&str>,
                     head_sha: Option<&str>,
                     reviews: Vec<Json>,
                     blocked_on: Option<Json>,
                     why: Option<&str>| {
        let landed = if state == "landed" {
            json!({"sha": "c76d1cf756", "at": at(ago)})
        } else {
            Json::Null
        };
        json!({
            "id": id,
            "title": title,
            "state": state,
            "owner": owner,
            "branch": branch,
            "head": head_sha,
            "reviews": reviews,
            "landed": landed,
            "blockedOn": blocked_on,
            "why": why,
            "updatedAt": at(ago),
        })
    };

    let f_board = make_board("F-follow-ups", 30 * MIN_MS)?;
    let tools_board = make_board("TOOLS", 2 * HOUR_MS)?;
    let status_board = make_board("STATUS", 20 * MIN_MS)?;

    let task_docs = vec![
        (
            "vm.pm-fo-lead",
            5 * MIN_MS,
            json!({
                "project": "puppet-master",
                "projectLabel": "Puppet Master",
                "stream": "F",
                "streamLabel": "PM7 follow-ups",
                "owner": "pm-fo-lead",
                "goal": "Fix every PM7 follow-up before the owner\u{2019}s QA round",
                "board": f_board,
            }),
            vec![
                task_item(
                    "FO-38",
                    "Welcome screen focus ring",
                    "landed",
                    26 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![
                        json!({"by": "gpt-6.1-sol/max", "verdict": "pass", "at": at(27 * HOUR_MS)}),
                    ],
                    None,
                    None,
                ),
                task_item(
                    "FO-39",
                    "Settings search highlights",
                    "landed",
                    20 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-40",
                    "Welcome PiP look glitch",
                    "landed",
                    5 * HOUR_MS,
                    Some("fo-welcome"),
                    Some("t3/fix/welcome-pip-look-glitches"),
                    Some("369a1bcdfa"),
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-41",
                    "PiP shadow on resize",
                    "working",
                    6 * MIN_MS,
                    Some("fo-welcome"),
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-33",
                    "Lines chart label placement",
                    "blocked",
                    2 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    Some(
                        json!({"kind": "owner", "what": "Card vm.pm-fo-lead--Q3", "since": at(2 * HOUR_MS)}),
                    ),
                    None,
                ),
                task_item(
                    "FO-34",
                    "Empty state copy",
                    "review",
                    40 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-35",
                    "Toast stacking order",
                    "fixing",
                    25 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-36",
                    "Export button spacing",
                    "ready",
                    15 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-37",
                    "Keyboard shortcut sheet",
                    "todo",
                    3 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-42",
                    "Old splash animation",
                    "dropped",
                    8 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    Some("Replaced by the new welcome"),
                ),
                task_item(
                    "FO-43",
                    "Docs link in About",
                    "done",
                    9 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "FO-44",
                    "Window title casing",
                    "todo",
                    4 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
            ],
        ),
        (
            "vm.tools-lead",
            10 * MIN_MS,
            json!({
                "project": "puppet-master",
                "projectLabel": "Puppet Master",
                "stream": "TOOLS",
                "streamLabel": "Agent tools",
                "owner": "tools-lead",
                "goal": "Shared tools every agent can rely on",
                "board": tools_board,
            }),
            vec![
                task_item(
                    "T-16",
                    "browser-check 2.1 slots",
                    "review",
                    30 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "T-17",
                    "simlock iOS warm pool",
                    "working",
                    50 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "T-18",
                    "agent-report command",
                    "working",
                    20 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "T-14",
                    "quota-watch per-stream attribution",
                    "landed",
                    2 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "T-15",
                    "pm-mail rings through T3",
                    "landed",
                    30 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "T-19",
                    "Recording codec fallback",
                    "blocked",
                    3 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    Some(
                        json!({"kind": "resource", "what": "Windows GPU busy", "since": at(3 * HOUR_MS)}),
                    ),
                    None,
                ),
            ],
        ),
        (
            "vm.aac-status-lead",
            3 * MIN_MS,
            json!({
                "project": "ai-account-center",
                "projectLabel": "AI Account Center",
                "stream": "STATUS",
                "streamLabel": "Status tab",
                "owner": "aac-status-lead",
                "goal": "One page that answers \"how are we looking?\"",
                "board": status_board,
            }),
            vec![
                task_item(
                    "ST-1",
                    "Contract",
                    "landed",
                    6 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "ST-2",
                    "Server view and answers",
                    "review",
                    10 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "ST-3",
                    "Leptos page",
                    "working",
                    15 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "ST-4",
                    "Shell tab and iframe",
                    "working",
                    25 * MIN_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "ST-5",
                    "Phone layout",
                    "todo",
                    HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "ST-6",
                    "Scoreboard tab",
                    "todo",
                    HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
            ],
        ),
        (
            "mac.aac-claude-relogin",
            55 * MIN_MS,
            json!({
                "project": "ai-account-center",
                "projectLabel": "AI Account Center",
                "stream": "SIGNIN",
                "streamLabel": "Sign-in renewal",
                "owner": "aac-claude-relogin",
                "goal": "Claude sign-ins renew by themselves",
                "board": null,
            }),
            vec![
                task_item(
                    "SR-1",
                    "Runner on the Mac",
                    "landed",
                    30 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "SR-2",
                    "CAPTCHA wait clean-up",
                    "landed",
                    2 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
                task_item(
                    "SR-3",
                    "Windows runner",
                    "done",
                    3 * HOUR_MS,
                    None,
                    None,
                    None,
                    vec![],
                    None,
                    None,
                ),
            ],
        ),
    ];

    for (writer, ago, fields, items) in task_docs {
        let mut doc = next_head("agent-reports/tasks@1", writer, ago);
        insert_fields(&mut doc, fields);
        doc.insert("items".into(), Json::Array(items));
        write_json_pretty(
            &writers.join(writer).join("tasks.json"),
            &Json::Object(doc),
            2,
        )?;
    }

    // Images for F-Q3
    let images = writers.join("vm.pm-fo-lead").join("images");
    fs::create_dir_all(&images)?;
    let tiny_png = base64::engine::general_purpose::STANDARD.decode(TINY_PNG_B64)?;
    fs::write(images.join("fo33-inside.png"), &tiny_png)?;
    fs::write(images.join("fo33-under-axis.png"), &tiny_png)?;

    // Cards helper
    let mut write_card =
        |writer: &str, local: &str, ago: i64, fields: Json| -> Result<(), Box<dyn Error>> {
            let id = format!("{writer}--{local}");
            let dot = writer.find('.').unwrap_or(0);
            let asked_by = &writer[dot + 1..];
            let mut doc = next_head("agent-reports/card@1", writer, ago);
            let f = fields.as_object();
            let get = |k: &str, default: Json| -> Json {
                f.and_then(|m| m.get(k)).cloned().unwrap_or(default)
            };
            doc.insert("id".into(), Json::String(id.clone()));
            doc.insert("label".into(), get("label", Json::Null));
            doc.insert("project".into(), get("project", Json::Null));
            doc.insert("stream".into(), get("stream", Json::Null));
            doc.insert("askedBy".into(), Json::String(asked_by.into()));
            doc.insert(
                "replyTo".into(),
                json!({"pmMail": asked_by, "t3Thread": null}),
            );
            doc.insert("state".into(), Json::String("open".into()));
            doc.insert("title".into(), get("title", Json::Null));
            doc.insert("now".into(), get("now", Json::Null));
            doc.insert("nowAsOf".into(), get("nowAsOf", Json::Null));
            doc.insert("oldAnswer".into(), get("oldAnswer", Json::Null));
            doc.insert("whatChanged".into(), get("whatChanged", Json::Null));
            doc.insert("options".into(), get("options", Json::Array(vec![])));
            doc.insert("recommendation".into(), get("recommendation", Json::Null));
            doc.insert("multiple".into(), get("multiple", Json::Bool(false)));
            doc.insert(
                "urgency".into(),
                get("urgency", Json::String("today".into())),
            );
            doc.insert("blocks".into(), get("blocks", Json::Null));
            doc.insert("sections".into(), get("sections", Json::Array(vec![])));
            doc.insert("batch".into(), get("batch", Json::Null));
            doc.insert("evidence".into(), get("evidence", Json::Array(vec![])));
            doc.insert("replies".into(), get("replies", Json::Array(vec![])));
            doc.insert("kind".into(), get("kind", Json::String("question".into())));
            doc.insert("neededBy".into(), get("neededBy", Json::Null));
            doc.insert("exception".into(), get("exception", Json::Null));
            doc.insert("reversible".into(), get("reversible", Json::Null));
            doc.insert("verdict".into(), get("verdict", Json::Null));
            doc.insert("group".into(), get("group", Json::Null));
            doc.insert("agreement".into(), get("agreement", Json::Null));
            doc.insert("doubt".into(), get("doubt", Json::Null));
            doc.insert("appliesTo".into(), get("appliesTo", Json::Null));
            doc.insert(
                "changesCanon".into(),
                get("changesCanon", Json::Array(vec![])),
            );
            doc.insert("related".into(), get("related", Json::Array(vec![])));
            doc.insert("askedAt".into(), Json::String(at(ago)));
            write_json_pretty(
                &writers
                    .join(writer)
                    .join("cards")
                    .join(format!("{id}.json")),
                &Json::Object(doc),
                2,
            )?;
            Ok(())
        };

    let batch = "F-questions-2026-11-05";
    write_card(
        "vm.pm-fo-lead",
        "Q3",
        2 * HOUR_MS,
        json!({
            "label": "F-Q3",
            "project": "puppet-master",
            "stream": "F",
            "title": "Where should the lines chart put its label?",
            "now": "On small windows the chart label covers the newest point, so the latest value is hidden.",
            "nowAsOf": at(2 * HOUR_MS),
            "options": [
                status_option(
                    "A",
                    "Keep the label inside the plot",
                    Some("The label moves left when it would cover a point."),
                    Some("Can still feel crowded on a phone."),
                    vec![json!({
                        "kind": "image",
                        "path": "images/fo33-inside.png",
                        "caption": "The label inside the plot at 320 px",
                        "label": "mockup",
                        "poster": null,
                    })],
                    vec![json!({
                        "type": "text",
                        "title": "How it moves",
                        "fold": true,
                        "body": "It slides left of the newest point.",
                    })],
                ),
                status_option(
                    "B",
                    "Move it under the axis",
                    Some("The label never covers data."),
                    Some("It sits further from the line it names."),
                    vec![json!({
                        "kind": "image",
                        "path": "images/fo33-under-axis.png",
                        "caption": "The label under the axis",
                        "label": "mockup",
                        "poster": null,
                    })],
                    vec![],
                ),
            ],
            "recommendation": {
                "key": "A",
                "why": "Matches the other charts.",
                "confidence": "medium",
                "flip": "If the chart gets narrower than 280 px, B reads better.",
            },
            "blocks": "FO-33 landing (group 4)",
            "batch": batch,
            "sections": [
                {
                    "type": "facts",
                    "title": "Where it shows",
                    "fold": false,
                    "rows": [
                        {"label": "Windows narrower than", "value": "420 px"},
                        {"label": "Charts affected", "value": "3 of 11"},
                    ],
                },
                {
                    "type": "callout",
                    "title": null,
                    "fold": false,
                    "tone": "warn",
                    "body": "The newest value is the one people read first.",
                },
                {"type": "timeline", "title": "A section type this page does not know", "fold": true},
            ],
        }),
    )?;

    write_card(
        "vm.pm-fo-lead",
        "Q4",
        100 * MIN_MS,
        json!({
            "label": "F-Q4",
            "project": "puppet-master",
            "stream": "F",
            "title": "Should the newest toast appear on top or at the bottom?",
            "now": "Toasts stack in arrival order, so a new one can appear under older ones.",
            "options": [
                status_option("A", "Newest on top", Some("The latest message is where the eye lands first."), None, vec![], vec![]),
                status_option("B", "Newest at the bottom", Some("Reads like a chat log."), Some("New messages push older ones up."), vec![], vec![]),
            ],
            "recommendation": {"key": "A", "why": "The eye lands on the top first."},
            "batch": batch,
            "verdict": "adapt",
            "group": "toasts",
            "agreement": {"same": false, "note": "Two reviewers split on this one."},
            "related": ["vm.pm-fo-lead--Q3"],
        }),
    )?;

    write_card(
        "vm.pm-fo-lead",
        "Q5",
        90 * MIN_MS,
        json!({
            "label": "F-Q5",
            "project": "puppet-master",
            "stream": "F",
            "title": "Drop the old splash animation for good?",
            "now": "The new welcome screen replaced it; the old one is still in the build behind a flag.",
            "oldAnswer": {"text": "Keep it for one more release.", "at": at(9 * DAY_MS)},
            "whatChanged": "That release shipped and nobody turned the flag on.",
            "options": [
                status_option("A", "Drop it", Some("A smaller build and one less setting."), None, vec![], vec![]),
                status_option("B", "Keep it behind a setting", None, Some("Another setting to test every release."), vec![], vec![]),
            ],
            "recommendation": {"key": "A", "why": "Nobody has turned it on since PM6."},
            "urgency": "this-week",
            "batch": batch,
            "appliesTo": {"count": 4, "description": "Every feature still behind a PM6 flag"},
            "changesCanon": [{"ref": "PM7 plan > Flags", "what": "Old flags are removed, not kept"}],
            "doubt": "One tester may still use the old splash on a slow machine.",
        }),
    )?;

    let tools_md_path = root
        .join("boards")
        .join("TOOLS.md")
        .to_string_lossy()
        .into_owned();
    write_card(
        "vm.tools-lead",
        "Q1",
        25 * MIN_MS,
        json!({
            "label": "T-Q1",
            "project": "puppet-master",
            "stream": "TOOLS",
            "title": "Which machines may record during the owner\u{2019}s streaming hours?",
            "now": "Recording on the VM drops frames while Moonlight streams.",
            "options": [
                status_option("vm", "Ubuntu VM", Some("Smoothest video."), Some("Drops frames while streaming."), vec![], vec![]),
                status_option("mac", "Mac", None, None, vec![], vec![]),
                status_option("windows", "Windows PC", Some("Fastest encoder."), Some("It is the owner\u{2019}s own PC."), vec![], vec![]),
                status_option("nas1", "Nas1", None, Some("No GPU encoder."), vec![], vec![]),
            ],
            "multiple": true,
            "urgency": "now",
            "blocks": "T-19",
            "evidence": [{"label": "Frame drop log", "path": tools_md_path}],
            "sections": [
                {
                    "type": "compare",
                    "title": "What other setups do",
                    "fold": false,
                    "items": [{
                        "name": "OBS on the gaming PC",
                        "does": "Records with the GPU encoder while streaming.",
                        "problem": "Competes with the game for the encoder.",
                        "link": {"label": "OBS", "url": "https://obsproject.com/"},
                        "version": "31",
                    }],
                },
                {
                    "type": "notCopying",
                    "title": null,
                    "fold": true,
                    "items": [{"what": "Recording at 60 fps", "why": "Doubles the encoder load."}],
                },
                {
                    "type": "terms",
                    "title": null,
                    "fold": true,
                    "items": [{"term": "Moonlight", "meaning": "The app that streams the VM desktop to the owner."}],
                },
                {
                    "type": "unknowns",
                    "title": null,
                    "fold": false,
                    "items": ["Whether the Mac encoder holds up at 1080p."],
                },
                {
                    "type": "list",
                    "title": "Steps if VM is chosen",
                    "fold": true,
                    "ordered": true,
                    "items": ["Pause Moonlight", "Record", "Resume"],
                },
                {
                    "type": "table",
                    "title": "Encoders",
                    "fold": true,
                    "columns": ["Machine", "Encoder"],
                    "rows": [["VM", "NVENC"], ["Nas1", ""]],
                },
                {
                    "type": "links",
                    "title": null,
                    "fold": true,
                    "items": [{"label": "Board", "url": "/mnt/Cursor/share/puppet-master/TOOLS.md"}],
                },
            ],
        }),
    )?;

    write_card(
        "vm.aac-status-lead",
        "Q2",
        40 * MIN_MS,
        json!({
            "label": "ST-Q2",
            "project": "ai-account-center",
            "stream": "STATUS",
            "title": "Show stale threads collapsed or greyed?",
            "now": "Threads that stopped reporting still take a full row.",
            "options": [
                status_option("A", "Collapsed under \"quiet\"", Some("A shorter list."), Some("One tap to see them."), vec![], vec![]),
                status_option("B", "Greyed in place", Some("Nothing moves while you read."), Some("A longer list."), vec![], vec![]),
            ],
            "recommendation": {"key": "B", "why": "Nothing moves while you read."},
            "urgency": "whenever",
            "sections": [
                {
                    "type": "quote",
                    "title": "What the Status contract says",
                    "fold": false,
                    "items": [{
                        "text": "Nothing is ever shown as live when its source is stale.",
                        "where": "GET /api/status > Source",
                        "context": ["Agreed 2026-11-06"],
                        "source": "docs/status-tab.md",
                        "link": null,
                        "version": "v1",
                    }],
                },
                {
                    "type": "planChange",
                    "title": null,
                    "fold": true,
                    "doc": "docs/status-tab.md",
                    "unit": "Thread",
                    "now": "Stale threads take a full row.",
                    "proposed": "Stale threads are greyed in place.",
                },
            ],
            "replies": [
                {"at": at(10 * MIN_MS), "text": "Both are in the screenshot set now; greyed is on the left."}
            ],
        }),
    )?;

    // withKinds = true
    write_card(
        "vm.pm-fo-lead",
        "O1",
        70 * MIN_MS,
        json!({
            "kind": "objection",
            "label": "F-O1",
            "project": "puppet-master",
            "stream": "F",
            "title": "I am moving the export button into the toolbar",
            "now": "Two people missed it at the bottom of the panel during QA.",
            "options": [],
            "reversible": {"yes": true, "how": "One commit to revert."},
            "neededBy": {"at": at(-4 * HOUR_MS), "ifNoAnswer": "It lands with group 4 tonight."},
        }),
    )?;

    write_card(
        "vm.aac-status-lead",
        "D1",
        15 * MIN_MS,
        json!({
            "kind": "action",
            "label": "ST-D1",
            "project": "ai-account-center",
            "stream": "STATUS",
            "title": "Sign in to the second Claude account on the Mac",
            "now": "Its sign-in ended this morning; only you can complete the CAPTCHA.",
            "options": [],
            "urgency": "now",
            "exception": {"rule": "Agents never sign in for you", "text": "This one needs your hands."},
        }),
    )?;

    write_card(
        "vm.pm-closure-lead",
        "J1",
        50 * MIN_MS,
        json!({
            "kind": "judgement",
            "label": "C-J1",
            "project": "puppet-master",
            "stream": "C",
            "title": "Do these two summaries say the same thing?",
            "now": "A measurement card: your answer becomes part of the answer key.",
            "options": [
                status_option("same", "Same meaning", None, None, vec![], vec![]),
                status_option("different", "Different meaning", None, None, vec![], vec![]),
                status_option("cant-tell", "Can\u{2019}t tell", None, None, vec![], vec![]),
            ],
            "group": "retention-key",
            "batch": "C-judgements",
        }),
    )?;

    write_card(
        "vm.pm-closure-lead",
        "S1",
        45 * MIN_MS,
        json!({
            "kind": "sample",
            "label": "C-S1",
            "project": "puppet-master",
            "stream": "C",
            "title": "Format sample: a card with a quote section",
            "options": [
                status_option("A", "First", None, None, vec![], vec![]),
                status_option("B", "Second", None, None, vec![], vec![]),
            ],
        }),
    )?;

    write_card(
        "vm.pm-closure-lead",
        "Q1",
        5 * HOUR_MS,
        json!({
            "label": "C-Q1",
            "project": "puppet-master",
            "stream": "C",
            "title": "Re-audit B09 with a second reviewer pair?",
            "now": "B09 passed one review but carries a shortcut flag.",
            "options": [
                status_option("A", "Yes, Sol and Astra", Some("A second opinion on the flagged batch."), Some("About an hour of quota."), vec![], vec![]),
                status_option("B", "No, one pass is enough", Some("Saves the quota."), Some("The flag stays unexplained."), vec![], vec![]),
            ],
            "recommendation": {"key": "A", "why": "B09 had a shortcut flag."},
        }),
    )?;

    write_card(
        "mac.aac-claude-relogin",
        "Q7",
        26 * HOUR_MS,
        json!({
            "label": "SR-Q7",
            "project": "ai-account-center",
            "stream": "SIGNIN",
            "title": "Renew Windows sign-ins from the Mac runner?",
            "options": [
                status_option("A", "Yes", None, None, vec![], vec![]),
                status_option("B", "No, give Windows its own runner", None, None, vec![], vec![]),
            ],
            "recommendation": {"key": "B"},
        }),
    )?;

    // Quota report
    write_json_pretty(
        &v1.join("quota").join("report.json"),
        &quota_report(now_ms),
        2,
    )?;

    // pm-mail registry
    let registry = root.join("pm-mail-registry");
    fs::create_dir_all(&registry)?;
    for (name, ago, about, machine) in [
        (
            "pm-docs-sweep",
            15 * MIN_MS,
            "Docs sweep for PM7 release notes",
            "vm",
        ),
        ("aac-mac-bar", 45 * MIN_MS, "Mac bar quota colours", "mac"),
        ("old-scout", 4 * DAY_MS, "Scouting run from last week", "vm"),
    ] {
        write_json_pretty(
            &registry.join(format!("{name}.json")),
            &json!({
                "name": name,
                "kind": "claude",
                "cwd": if machine == "mac" { "/Users/agent/work" } else { "/home/agent/work" },
                "about": about,
                "registered": at(ago + HOUR_MS),
                "last_seen": at(ago),
                "machine": if machine == "mac" { "agent-mac.local" } else { "ubuntuserver" },
            }),
            2,
        )?;
    }

    Ok(())
}

const SB_LEAD: &str = "vm.scoreboard-lead";
const SB_REVIEWS: &str = "mac.scoreboard-reviews";
const SB_BENCH: &str = "windows.scoreboard-bench";

fn sb_opus() -> Json {
    json!({
        "provider": "anthropic",
        "id": "claude-opus-5-5",
        "harness": "claude-code",
        "effort": "xhigh",
        "account": "account-a",
    })
}
fn sb_opus_high() -> Json {
    json!({
        "provider": "anthropic",
        "id": "claude-opus-5-5",
        "harness": "claude-code",
        "effort": "high",
        "account": "account-a",
    })
}
fn sb_sol() -> Json {
    json!({
        "provider": "openai",
        "id": "openai/gpt-6.1-sol",
        "harness": "codex",
        "effort": "high",
        "account": "account-b",
    })
}
fn sb_sol_medium() -> Json {
    json!({
        "provider": "openai",
        "id": "openai/gpt-6.1-sol",
        "harness": "codex",
        "effort": "medium",
        "account": "account-b",
    })
}
fn sb_glm() -> Json {
    json!({
        "provider": "zai",
        "id": "glm-5.3",
        "harness": "zcode",
        "effort": "max",
        "account": "account-c",
    })
}
fn sb_sonnet() -> Json {
    json!({
        "provider": "anthropic",
        "id": "claude-sonnet-5-5",
        "harness": "claude-code",
        "effort": null,
        "account": "account-a",
    })
}
fn sb_haiku() -> Json {
    json!({
        "provider": "anthropic",
        "id": "claude-haiku-5-5",
        "harness": "claude-code",
        "effort": null,
        "account": "account-a",
    })
}
fn sb_muse() -> Json {
    json!({
        "provider": "muse",
        "id": "muse-spark-1.3-contributor",
        "harness": "muse",
        "effort": "max",
        "account": "account-d",
    })
}
fn sb_unlisted() -> Json {
    json!({
        "provider": "example",
        "id": "example-1",
        "harness": "omp",
        "effort": null,
        "account": null,
    })
}

#[allow(clippy::too_many_arguments)]
fn sb_add_run(
    runs: &mut Vec<Json>,
    now_ms: i64,
    writer: &str,
    local: &str,
    at_hours: f64,
    task_type: Option<&str>,
    task_id: &str,
    title: Option<String>,
    size: Json,
    wall: i64,
    model: Json,
    rest: Json,
) -> String {
    let ended = now_ms - (at_hours * (HOUR_MS as f64)) as i64;
    let id = format!("{writer}--{local}");
    let title_str =
        title.unwrap_or_else(|| format!("{} {task_id}", task_type.unwrap_or("Unknown work")));
    let mut doc = Map::new();
    doc.insert("schema".into(), Json::String("agent-reports/run@1".into()));
    doc.insert("id".into(), Json::String(id.clone()));
    doc.insert("writer".into(), Json::String(writer.into()));
    doc.insert("seq".into(), Json::from(1));
    doc.insert("writtenAt".into(), Json::String(iso_sec(ended + 60_000)));
    doc.insert("source".into(), Json::String("observed".into()));
    doc.insert("project".into(), Json::String("example-project".into()));
    doc.insert("stream".into(), Json::String("S".into()));
    doc.insert(
        "task".into(),
        json!({
            "id": task_id,
            "type": task_type,
            "title": title_str,
            "size": size,
        }),
    );
    doc.insert("model".into(), model);
    doc.insert(
        "startedAt".into(),
        Json::String(iso_sec(ended - wall * 1000)),
    );
    doc.insert("endedAt".into(), Json::String(iso_sec(ended)));
    doc.insert("wallSeconds".into(), Json::from(wall));
    doc.insert("outcome".into(), Json::String("done".into()));
    doc.insert("flags".into(), Json::Array(vec![]));
    doc.insert("evidence".into(), Json::Array(vec![]));
    insert_fields(&mut doc, rest);
    runs.push(Json::Object(doc));
    id
}

#[derive(Default)]
struct VerdictOpts<'a> {
    verdict: Option<&'a str>,
    overturned: bool,
    by: Option<&'a str>,
    disputed: bool,
    wall: Option<i64>,
}

fn sb_review(
    runs: &mut Vec<Json>,
    now_ms: i64,
    local: &str,
    of: &str,
    at_hours: f64,
    model: Json,
    v: VerdictOpts<'_>,
) -> String {
    let evidence = if v.overturned {
        vec![json!({
            "label": "Review notes",
            "path": format!("/mnt/Cursor/share/example-project/reviews/{local}.md"),
        })]
    } else {
        vec![]
    };
    sb_add_run(
        runs,
        now_ms,
        SB_REVIEWS,
        local,
        at_hours,
        Some("review"),
        local,
        Some(format!("Blind review of {of}")),
        json!({"records": 40}),
        v.wall.unwrap_or(300),
        model,
        json!({
            "cost": {"credits": 2, "usd": null, "quota": null, "tokens": null},
            "scope": {"filesAssigned": 10, "filesRead": 10, "recordsAssigned": 40, "recordsJudged": 40},
            "review": {
                "of": of,
                "verdict": v.verdict.unwrap_or("pass"),
                "findings": {"high": if v.overturned { 1 } else { 0 }, "medium": 0, "low": 1},
                "overturned": v.overturned,
                "by": v.by,
                "disputed": v.disputed,
            },
            "evidence": evidence,
        }),
    )
}

fn sb_flag(kind: &str, detail: &str, local: &str) -> Json {
    json!({
        "kind": kind,
        "detail": detail,
        "evidence": format!("/mnt/Cursor/share/example-project/evidence/{local}.md"),
        "reportedBy": "scoreboard-reviews",
    })
}

#[allow(clippy::too_many_arguments)]
fn sb_build(
    runs: &mut Vec<Json>,
    now_ms: i64,
    local: &str,
    task: &str,
    model: Json,
    at_hours: f64,
    wall: i64,
    extra: Json,
) -> String {
    let mut rest = Map::new();
    rest.insert(
        "scope".into(),
        json!({"filesAssigned": 12, "filesRead": 12, "recordsAssigned": 20, "recordsJudged": 20}),
    );
    insert_fields(&mut rest, extra);
    sb_add_run(
        runs,
        now_ms,
        SB_LEAD,
        local,
        at_hours,
        Some("worknode-build"),
        task,
        None,
        json!({"tasks": 1}),
        wall,
        model,
        Json::Object(rest),
    )
}

fn make_scoreboard_runs(now_ms: i64) -> Vec<Json> {
    let mut runs = Vec::new();

    for i in 1..=5_i64 {
        let local = format!("WN-{i}");
        let id = sb_build(
            &mut runs,
            now_ms,
            &local,
            &local,
            sb_opus(),
            (100 - i) as f64,
            1100 + i * 100,
            json!({
                "seq": if i == 1 { 2 } else { 1 },
                "cost": {
                    "credits": 10 + i,
                    "usd": 1.5,
                    "quota": {"account": "account-a", "window": "5h", "deltaPct": 3},
                    "tokens": null,
                }
            }),
        );
        sb_review(
            &mut runs,
            now_ms,
            &format!("R-WN-{i}"),
            &id,
            (90 - i) as f64,
            sb_sol(),
            VerdictOpts {
                by: Some("openai/gpt-6.1-sol high"),
                ..VerdictOpts::default()
            },
        );
    }

    let round1 = sb_build(
        &mut runs,
        now_ms,
        "WN-6-a",
        "WN-6",
        sb_opus(),
        80.0,
        1500,
        json!({"cost": {"credits": 12}}),
    );
    let round2 = sb_build(
        &mut runs,
        now_ms,
        "WN-6-b",
        "WN-6",
        sb_opus(),
        76.0,
        700,
        json!({"cost": {"credits": 6}}),
    );
    sb_review(
        &mut runs,
        now_ms,
        "R-WN-6-a",
        &round1,
        78.0,
        sb_sol(),
        VerdictOpts {
            overturned: true,
            verdict: Some("fail"),
            by: Some("openai/gpt-6.1-sol high"),
            ..VerdictOpts::default()
        },
    );
    sb_review(
        &mut runs,
        now_ms,
        "R-WN-6-b",
        &round2,
        74.0,
        sb_sol(),
        VerdictOpts {
            by: Some("openai/gpt-6.1-sol high"),
            ..VerdictOpts::default()
        },
    );

    let sol_cases = [
        (11_i64, VerdictOpts::default()),
        (12, VerdictOpts::default()),
        (13, VerdictOpts::default()),
        (
            14,
            VerdictOpts {
                overturned: true,
                verdict: Some("fail"),
                by: Some("anthropic/claude-opus-5-5 xhigh"),
                ..VerdictOpts::default()
            },
        ),
        (
            15,
            VerdictOpts {
                overturned: true,
                verdict: Some("fail"),
                disputed: true,
                by: Some("anthropic/claude-opus-5-5 xhigh"),
                ..VerdictOpts::default()
            },
        ),
        (
            16,
            VerdictOpts {
                verdict: Some("cannot-verify"),
                ..VerdictOpts::default()
            },
        ),
    ];
    for (i, verdict) in sol_cases {
        let local = format!("WN-{i}");
        let id = sb_build(
            &mut runs,
            now_ms,
            &local,
            &local,
            sb_sol(),
            (70 - i) as f64,
            800 + i * 10,
            json!({
                "cost": {
                    "credits": null,
                    "estimate": {
                        "source": "quota-watch",
                        "method": "token share at list price",
                        "credits": 8,
                        "quota": {"account": "account-b", "window": "5h", "deltaPct": 2},
                        "tokens": null,
                    },
                }
            }),
        );
        sb_review(
            &mut runs,
            now_ms,
            &format!("R-WN-{i}"),
            &id,
            (50 - i) as f64,
            sb_opus(),
            verdict,
        );
    }
    sb_build(
        &mut runs,
        now_ms,
        "WN-17",
        "WN-17",
        sb_sol(),
        40.0,
        30,
        json!({"outcome": "failed", "scope": {}}),
    );

    for i in 21..=25_i64 {
        let flags = match i {
            21 => vec![sb_flag(
                "shortcut",
                "Wrote a script to assign verdicts instead of judging each record",
                "WN-21",
            )],
            22 => vec![sb_flag(
                "test-weakened",
                "Removed the failing assertion instead of fixing the code",
                "WN-22",
            )],
            23 => vec![sb_flag(
                "incomplete-read",
                "Read 4 of the 12 files it was assigned",
                "WN-23",
            )],
            _ => vec![],
        };
        let scope = if i == 23 {
            json!({"filesAssigned": 12, "filesRead": 4, "recordsAssigned": 20, "recordsJudged": 20})
        } else {
            json!({"filesAssigned": 12, "filesRead": 12, "recordsAssigned": 20, "recordsJudged": 20})
        };
        let local = format!("WN-{i}");
        let id = sb_build(
            &mut runs,
            now_ms,
            &local,
            &local,
            sb_glm(),
            (60 - i) as f64,
            400,
            json!({
                "flags": flags,
                "scope": scope,
                "cost": {
                    "credits": 2,
                    "usd": null,
                    "quota": {"account": "account-c", "window": "5h", "deltaPct": 1},
                    "tokens": null,
                }
            }),
        );
        sb_review(
            &mut runs,
            now_ms,
            &format!("R-WN-{i}"),
            &id,
            30.0 - (i as f64) / 10.0,
            sb_sol(),
            VerdictOpts {
                overturned: i <= 24,
                verdict: Some(if i <= 24 { "fail" } else { "pass" }),
                ..VerdictOpts::default()
            },
        );
    }

    for i in 31..=32_i64 {
        let local = format!("WN-{i}");
        sb_build(
            &mut runs,
            now_ms,
            &local,
            &local,
            sb_sonnet(),
            20.0 + (i as f64) / 10.0,
            600,
            json!({}),
        );
    }
    for i in 41..=45_i64 {
        let local = format!("WN-{i}");
        sb_build(
            &mut runs,
            now_ms,
            &local,
            &local,
            sb_haiku(),
            15.0 + (i as f64) / 10.0,
            600,
            json!({}),
        );
    }

    sb_review(
        &mut runs,
        now_ms,
        "R-of-R-WN-11",
        &format!("{SB_REVIEWS}--R-WN-11"),
        8.0,
        sb_sol(),
        VerdictOpts {
            by: Some("openai/gpt-6.1-sol high"),
            ..VerdictOpts::default()
        },
    );
    sb_review(
        &mut runs,
        now_ms,
        "R-of-R-WN-1",
        &format!("{SB_REVIEWS}--R-WN-1"),
        7.5,
        sb_opus(),
        VerdictOpts {
            by: Some("anthropic/claude-opus-5-5 xhigh"),
            ..VerdictOpts::default()
        },
    );
    sb_review(
        &mut runs,
        now_ms,
        "R-nobody",
        "vm.scoreboard-lead--no-such-run",
        7.0,
        sb_opus(),
        VerdictOpts::default(),
    );

    let scout = |runs: &mut Vec<Json>,
                 local: &str,
                 model: Json,
                 process: Json,
                 at_hours: f64,
                 kept: i64,
                 drift: i64,
                 is_bench: bool| {
        let writer = if is_bench { SB_BENCH } else { SB_LEAD };
        let mut rest = Map::new();
        rest.insert("process".into(), process);
        rest.insert(
            "metrics".into(),
            json!({
                "factsKept": kept,
                "factsTotal": 127,
                "verdictsChanged": drift,
                "verdictsTotal": 40,
            }),
        );
        if is_bench {
            rest.insert("source".into(), Json::String("benchmark".into()));
            rest.insert("benchmark".into(), Json::String("scout-sv3-t9".into()));
        }
        sb_add_run(
            runs,
            now_ms,
            writer,
            local,
            at_hours,
            Some("scout"),
            local,
            None,
            json!({"items": 127}),
            500 + kept,
            model,
            Json::Object(rest),
        )
    };
    let v1 = json!({"id": "scout-pipeline", "version": "1.0", "sha256": null});
    let v2 = json!({"id": "scout-pipeline", "version": "2.0", "sha256": null});
    for i in 1..=5_i64 {
        scout(
            &mut runs,
            &format!("bench-opus-v2-{i}"),
            sb_opus(),
            v2.clone(),
            (200 - i) as f64,
            118 + i,
            2,
            true,
        );
        scout(
            &mut runs,
            &format!("bench-sol-v2-{i}"),
            sb_sol(),
            v2.clone(),
            (190 - i) as f64,
            100 + i,
            6,
            true,
        );
    }
    for i in 1..=3_i64 {
        scout(
            &mut runs,
            &format!("bench-opus-v1-{i}"),
            sb_opus(),
            v1.clone(),
            (210 - i) as f64,
            70 + i,
            12,
            true,
        );
    }
    for i in 1..=5_i64 {
        let id = scout(
            &mut runs,
            &format!("scout-opus-v2-{i}"),
            sb_opus(),
            v2.clone(),
            (170 - i) as f64,
            115 + i,
            3,
            false,
        );
        if i == 1 {
            sb_review(
                &mut runs,
                now_ms,
                &format!("R-scout-{i}"),
                &id,
                160.0,
                sb_sol(),
                VerdictOpts {
                    by: Some("openai/gpt-6.1-sol high"),
                    ..VerdictOpts::default()
                },
            );
        }
    }
    for i in 1..=2_i64 {
        scout(
            &mut runs,
            &format!("scout-sol-{i}"),
            sb_sol(),
            Json::Null,
            (150 - i) as f64,
            90,
            5,
            false,
        );
    }

    let plan = |runs: &mut Vec<Json>, local: &str, model: Json, at_hours: i64, silent: i64| {
        sb_add_run(
            runs,
            now_ms,
            SB_BENCH,
            local,
            at_hours as f64,
            Some("plan"),
            local,
            None,
            json!({"words": 1500 + at_hours}),
            60 + silent * 5,
            model,
            json!({
                "source": "benchmark",
                "benchmark": "plan-pilot2",
                "process": {"id": "a", "version": "pilot/run_a.py", "sha256": null},
                "metrics": {"silent": silent, "goldAgreed": 60, "answerKeyFinal": 0, "modelSeconds": 50},
                "imported": {"from": "/mnt/Cursor/archive/example/pilot/runs.json", "at": iso_sec(now_ms - 6 * HOUR_MS)},
                "evidence": [{
                    "label": "Pilot results (preliminary)",
                    "path": "/mnt/Cursor/archive/example/pilot/RESULTS.md",
                }],
            }),
        );
    };
    for i in 1..=5_i64 {
        plan(
            &mut runs,
            &format!("plan-sol-{i}"),
            sb_sol_medium(),
            300 - i,
            2 + i,
        );
    }
    for i in 1..=6_i64 {
        plan(
            &mut runs,
            &format!("plan-opus-{i}"),
            sb_opus_high(),
            290 - i,
            1,
        );
    }
    for i in 1..=2_i64 {
        let local = format!("plan-observed-{i}");
        sb_add_run(
            &mut runs,
            now_ms,
            SB_LEAD,
            &local,
            (12 + i) as f64,
            Some("plan"),
            &local,
            None,
            json!({"words": 9000}),
            2000,
            sb_opus_high(),
            json!({}),
        );
    }
    for i in 1..=2_i64 {
        let local = format!("imported-{i}");
        sb_add_run(
            &mut runs,
            now_ms,
            SB_LEAD,
            &local,
            (500 + i) as f64,
            Some("build"),
            &local,
            None,
            json!({"tasks": 1}),
            600,
            sb_sonnet(),
            json!({
                "imported": {
                    "from": "/mnt/Cursor/archive/example/closure-reaudit.md",
                    "at": iso_sec(now_ms - 5 * HOUR_MS),
                }
            }),
        );
    }
    for i in 1..=5_i64 {
        let local = format!("mech-muse-{i}");
        let id = sb_add_run(
            &mut runs,
            now_ms,
            SB_LEAD,
            &local,
            11.0 - (i as f64) / 10.0,
            Some("mechanical"),
            &local,
            None,
            json!({"files": 20}),
            200,
            sb_muse(),
            json!({
                "cost": {"credits": null, "quota": {"account": "account-d", "window": "5h", "deltaPct": 4}},
                "scope": {"filesAssigned": 20, "filesRead": 20},
            }),
        );
        if i == 1 {
            sb_review(
                &mut runs,
                now_ms,
                &format!("R-mech-{i}"),
                &id,
                9.5,
                sb_sol(),
                VerdictOpts::default(),
            );
        }
    }

    sb_add_run(
        &mut runs,
        now_ms,
        SB_LEAD,
        "unlisted-model",
        5.0,
        Some("build"),
        "unlisted",
        None,
        json!({"tasks": 1}),
        600,
        sb_unlisted(),
        json!({}),
    );
    sb_add_run(
        &mut runs,
        now_ms,
        SB_LEAD,
        "unlisted-type",
        4.0,
        Some("audit-read"),
        "B09",
        None,
        json!({"records": 40}),
        600,
        sb_glm(),
        json!({}),
    );
    sb_add_run(
        &mut runs,
        now_ms,
        SB_LEAD,
        "no-type",
        3.0,
        None,
        "unknown-work",
        None,
        json!({"tasks": 1}),
        600,
        sb_glm(),
        json!({}),
    );

    runs
}

fn write_sb_run(root: &Path, doc: &Json) -> Result<PathBuf, Box<dyn Error>> {
    let writer = doc["writer"].as_str().ok_or("missing writer")?;
    let ended_at = doc["endedAt"].as_str().ok_or("missing endedAt")?;
    let id = doc["id"].as_str().ok_or("missing id")?;
    let date_dir = &ended_at[..10];
    let dir = root
        .join("v1")
        .join("writers")
        .join(writer)
        .join("runs")
        .join(date_dir);
    write_json_pretty(&dir.join(format!("{id}.json")), doc, 1)?;
    Ok(dir)
}

/// Write the base Scoreboard fixture matching `writeScoreboardFixture(root, { now })`,
/// returning the number of base runs written.
pub fn write_scoreboard_fixture_for_perf(
    repo_root: &Path,
    root: &Path,
    now_ms: i64,
) -> Result<usize, Box<dyn Error>> {
    let catalog = root.join("v1").join("catalog");
    if catalog.join("models.json").exists() {
        return Err(format!("{} already has a catalog", root.display()).into());
    }
    fs::create_dir_all(&catalog)?;
    let seeds_dir = repo_root.join("docs").join("agent-reports").join("catalog");
    for entry in fs::read_dir(&seeds_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), catalog.join(entry.file_name()))?;
        }
    }

    let runs = make_scoreboard_runs(now_ms);
    for doc in &runs {
        write_sb_run(root, doc)?;
    }

    // Older copy of WN-1 with lower seq
    let target_id = format!("{SB_LEAD}--WN-1");
    if let Some(first) = runs.iter().find(|d| d["id"].as_str() == Some(&target_id)) {
        let ended_ms = aac_num::ecma::parse_instant(first["endedAt"].as_str().unwrap_or(""))
            .ok_or("invalid WN-1 endedAt")?;
        let older = iso_sec(ended_ms - 72 * HOUR_MS);
        let mut older_doc = first.clone();
        if let Some(obj) = older_doc.as_object_mut() {
            obj.insert("seq".into(), Json::from(1));
            obj.insert("endedAt".into(), Json::String(older.clone()));
            obj.insert("writtenAt".into(), Json::String(older));
            obj.insert("outcome".into(), Json::String("failed".into()));
        }
        write_sb_run(root, &older_doc)?;
    }

    if let Some(last) = runs.last() {
        let dir = write_sb_run(root, last)?;
        fs::write(
            dir.join(format!("{SB_LEAD}--damaged.json")),
            b"{\"schema\":\0\0\0",
        )?;
        fs::write(
            dir.join(format!(".{SB_LEAD}--half-written.json.tmp-1")),
            b"{\"schema\"",
        )?;
    }

    Ok(runs.len())
}
