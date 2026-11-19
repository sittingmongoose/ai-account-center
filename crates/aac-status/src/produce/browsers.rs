//! G16c: `browser-check slots --json` as the Status feed carries it.
//!
//! A port of `status-collectors.ts` (`parseBrowserSlots`, `mergeBrowserRows`, `sessionUrl`,
//! `shortText`): one row per known computer, nulls kept null (never a zero), a host that could
//! not be read with unknown counts and its failure named, a good row's notes named as a
//! problem, and a session's URL reduced to host and path (no query, no fragment, file URLs
//! show their path). The one secret filter runs on every shown tool text.

use super::{next_problem, ReadProblem};
use serde::Serialize;
use serde_json::Value as Json;
use std::collections::HashMap;

/// The fixed refusal when the answer is not `{rows: [...]}`.
pub const UNKNOWN_SHAPE: &str = "browser-check answered in an unknown shape.";

/// The machines whose browser answer names phones (every registered computer that is not
/// Windows; elsewhere `phones: null` is the normal answer).
pub const SIMLOCK_MACHINES: [&str; 3] = ["vm", "mac", "nas1"];

/// `{running, max}`, each unknown as null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub running: Option<i64>,
    pub max: Option<i64>,
}

/// One browser session as the page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: Option<String>,
    /// Host and path only (a file URL shows its path).
    pub url: Option<String>,
    pub owner: Option<String>,
    pub recording: bool,
    pub age_seconds: Option<i64>,
}

/// The host's Simlock phones (running counts reserved ones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Phones {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ios: Option<Counts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub android: Option<Counts>,
}

/// One host's `browser-check` reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserMachine {
    pub machine: String,
    pub label: Option<String>,
    pub browsers: Counts,
    pub recordings: Counts,
    pub gpu_pct: Option<i64>,
    pub mem_gb: Option<f64>,
    pub sessions: Vec<Session>,
    /// Absent when browser-check left the field out; null when it said `phones: null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phones: Option<Option<Phones>>,
    /// What browser-check said is wrong with counts it did read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Vec<String>>,
    /// Why the host could not be read; the counts above are unknown.
    pub error: Option<String>,
    /// Set by the service: when these counts were read, null before a first good reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_at: Option<Option<i64>>,
    /// Set by the service: this host's current failure while the counts are the last good.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<Option<ReadProblem>>,
}

fn num(value: Option<&Json>) -> Option<f64> {
    value
        .and_then(Json::as_f64)
        .filter(|n| n.is_finite())
}

fn int(value: Option<&Json>) -> Option<i64> {
    num(value).map(|n| {
        #[allow(clippy::cast_possible_truncation)]
        let rounded = aac_num::ecma::math_round(n) as i64;
        rounded
    })
}

/// `shortText`: controls to spaces, trimmed, a secret refused, cut to `max` (`max - 1` and
/// `...` past it).
#[must_use]
pub fn short_text(value: Option<&Json>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    let clean: String = text
        .chars()
        .map(|c| {
            if c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() || crate::secret_policy::find_secret(clean).is_some() {
        return None;
    }
    if clean.chars().count() > max {
        let kept: String = clean.chars().take(max.saturating_sub(1)).collect();
        Some(format!("{kept}..."))
    } else {
        Some(clean.to_owned())
    }
}

/// `new URL(value)` reduced to what the page shows: a file URL's decoded path, or an
/// http(s) host and path. Anything else (another scheme, an unparseable URL) is null.
#[must_use]
pub fn session_url(value: Option<&Json>) -> Option<String> {
    let text = value?.as_str()?;
    if text.is_empty() {
        return None;
    }
    let (scheme, rest) = text.split_once(':')?;
    if !scheme_is(scheme) {
        return None;
    }
    let lower = scheme.to_ascii_lowercase();
    if lower == "file" {
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        // The authority (normally empty) ends at the first `/`; the rest is the path.
        let at = rest.find('/').unwrap_or(rest.len());
        let path = rest[at..].split(['?', '#']).next().unwrap_or("");
        let decoded = decode_uri_component(path)?;
        return short_text(Some(&Json::String(decoded)), 200);
    }
    if lower != "http" && lower != "https" {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host.is_empty() {
        return None;
    }
    let mut path = &rest[end..];
    if let Some(at) = path.find(['?', '#']) {
        path = &path[..at];
    }
    if path.is_empty() {
        path = "/";
    }
    let host = host.to_ascii_lowercase();
    let shown = format!("{host}{path}");
    short_text(Some(&Json::String(shown)), 200)
}

fn scheme_is(scheme: &str) -> bool {
    let mut bytes = scheme.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
}

/// `decodeURIComponent`: percent-decoded UTF-8; an invalid escape is null (the URL parse
/// throws, and the session has no shown URL).
fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = bytes.get(at + 1..at + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn phones(value: Option<&Json>) -> Option<Option<Phones>> {
    match value {
        None => None,
        Some(Json::Null) => Some(None),
        Some(Json::Object(map)) => {
            let read = |kind: &str| {
                map.get(kind).and_then(Json::as_object).map(|row| Counts {
                    running: int(row.get("running")),
                    max: int(row.get("max")),
                })
            };
            Some(Some(Phones {
                ios: read("ios"),
                android: read("android"),
            }))
        }
        Some(_) => Some(None),
    }
}

/// `parseBrowserSlots`: one row per known machine, in the answer's order.
///
/// # Errors
/// The answer is not an object with a `rows` array (`UNKNOWN_SHAPE`).
pub fn parse_slots(raw: &Json) -> Result<Vec<BrowserMachine>, &'static str> {
    let rows = raw
        .get("rows")
        .and_then(Json::as_array)
        .ok_or(UNKNOWN_SHAPE)?;
    let mut result = Vec::new();
    for row in rows {
        let machine = row.get("machine").cloned().unwrap_or(Json::Null);
        let name = machine.get("name").and_then(Json::as_str).unwrap_or("");
        if !crate::computers::known_machine(name, false) {
            continue;
        }
        let state = row.get("state").cloned().unwrap_or(Json::Null);
        if state.get("error").is_some_and(|e| !e.is_null()) {
            result.push(BrowserMachine {
                machine: name.to_owned(),
                label: short_text(machine.get("label"), 80),
                browsers: Counts {
                    running: None,
                    max: None,
                },
                recordings: Counts {
                    running: None,
                    max: None,
                },
                gpu_pct: None,
                mem_gb: None,
                sessions: Vec::new(),
                phones: None,
                notes: None,
                error: Some(
                    short_text(state.get("error"), 160)
                        .unwrap_or_else(|| "browser-check could not read this machine".to_owned()),
                ),
                read_at: None,
                problem: None,
            });
            continue;
        }
        let caps = state.get("caps").cloned().unwrap_or(Json::Null);
        let sessions = state
            .get("sessions")
            .and_then(Json::as_array)
            .map(|rows| rows.iter().take(50).collect::<Vec<_>>())
            .unwrap_or_default();
        let phone_rows = phones(state.get("phones"));
        let mut notes = Vec::new();
        if let Some(display) = short_text(state.get("display_problem"), 200) {
            notes.push(format!("browsers cannot start here: {display}"));
        }
        if let Some(snapshot) = short_text(state.get("snapshot"), 200) {
            notes.push(snapshot);
        }
        if phone_rows.as_ref().is_some_and(|phones| phones.is_none())
            && SIMLOCK_MACHINES.contains(&name)
        {
            notes.push(
                "reported no phone count (phones: null), so phones here are unknown".to_owned(),
            );
        }
        let mem = num(state.get("mem_gb")).map(|gb| aac_num::ecma::math_round(gb * 10.0) / 10.0);
        result.push(BrowserMachine {
            machine: name.to_owned(),
            label: short_text(machine.get("label"), 80),
            browsers: Counts {
                running: int(state.get("browsers")),
                max: int(caps.get("max_browsers")),
            },
            recordings: Counts {
                running: int(state.get("recordings")),
                max: int(caps.get("max_recordings")),
            },
            gpu_pct: int(state.get("gpu_util")),
            mem_gb: mem,
            sessions: sessions
                .into_iter()
                .map(|session| Session {
                    id: session
                        .get("id")
                        .and_then(Json::as_str)
                        .filter(|id| {
                            (1..=64).contains(&id.len())
                                && id
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        })
                        .map(str::to_owned),
                    url: session_url(session.get("url")),
                    owner: short_text(session.get("owner"), 120),
                    recording: session.get("recording") == Some(&Json::Bool(true)),
                    age_seconds: int(session.get("age_s")),
                })
                .collect(),
            phones: phone_rows,
            notes: Some(notes),
            error: None,
            read_at: None,
            problem: None,
        });
    }
    Ok(result)
}

/// `mergeBrowserRows`: a host that answered keeps its new counts and read time; a host that
/// failed keeps its last good counts with their time and the failure, or has unknown counts
/// and no read time when it never answered.
#[must_use]
pub fn merge_rows(
    before: Option<&[BrowserMachine]>,
    rows: Vec<BrowserMachine>,
    now: i64,
) -> Vec<BrowserMachine> {
    let index: HashMap<&str, &BrowserMachine> = before
        .map(|rows| {
            rows.iter()
                .map(|row| (row.machine.as_str(), row))
                .collect()
        })
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let last = index.get(row.machine.as_str()).copied();
            if row.error.is_none() {
                let notes = row.notes.clone().unwrap_or_default();
                let problem = if notes.is_empty() {
                    None
                } else {
                    Some(next_problem(
                        last.and_then(|last| last.problem.as_ref().and_then(Option::as_ref)),
                        row.label.as_deref().unwrap_or(row.machine.as_str()),
                        &notes.join("; "),
                        now,
                    ))
                };
                BrowserMachine {
                    read_at: Some(Some(now)),
                    problem: Some(problem),
                    ..row
                }
            } else {
                let reason = row.error.clone().unwrap_or_default();
                let item = row
                    .label
                    .clone()
                    .or_else(|| last.and_then(|last| last.label.clone()))
                    .unwrap_or_else(|| row.machine.clone());
                let problem = Some(next_problem(
                    last.and_then(|last| last.problem.as_ref().and_then(Option::as_ref)),
                    &item,
                    &reason,
                    now,
                ));
                match last.filter(|last| last.read_at.flatten().is_some()) {
                    Some(last) => BrowserMachine {
                        label: row.label.or_else(|| last.label.clone()),
                        error: row.error,
                        problem: Some(problem),
                        ..last.clone()
                    },
                    None => BrowserMachine {
                        read_at: Some(None),
                        problem: Some(problem),
                        ..row
                    },
                }
            }
        })
        .collect()
}

/// The rows' named notes as problems, each with the host as its item (`collectBrowsers`
/// reports `rows.flatMap(row => row.problem ? [row.problem] : [])`).
#[must_use]
pub fn problems(rows: &[BrowserMachine]) -> Vec<ReadProblem> {
    rows.iter()
        .filter_map(|row| row.problem.as_ref().and_then(Clone::clone))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn slots() -> Json {
        let state = json!({ "caps": { "max_browsers": 4 }, "browsers": 1, "recordings": 0, "sessions": [] });
        json!({
            "rows": [
                {
                    "machine": { "name": "mac", "label": "MacBook" },
                    "state": { "browsers": 1, "recordings": 0, "sessions": [],
                        "caps": { "max_browsers": 4 },
                        "phones": { "ios": { "running": 0, "max": 2 }, "android": { "running": 1, "max": null } } }
                },
                {
                    "machine": { "name": "vm", "label": "Ubuntu VM" },
                    "state": { "browsers": 1, "recordings": 0, "sessions": [],
                        "caps": { "max_browsers": 4 }, "phones": null,
                        "display_problem": "X desktop :0 refuses this account" }
                },
                {
                    "machine": { "name": "nas1", "label": "Nas1" },
                    "state": { "browsers": 1, "recordings": 0, "sessions": [],
                        "caps": { "max_browsers": 4 },
                        "snapshot": "backend busy (x): counted from its session records" }
                },
                { "machine": { "name": "windows", "label": "Windows PC" }, "state": state }
            ]
        })
    }

    #[test]
    fn reads_phones_notes_and_never_turns_null_into_zero() {
        let rows = parse_slots(&slots()).unwrap();
        let by: HashMap<_, _> = rows.iter().map(|r| (r.machine.as_str(), r)).collect();
        assert_eq!(
            by["mac"].phones,
            Some(Some(Phones {
                ios: Some(Counts { running: Some(0), max: Some(2) }),
                android: Some(Counts { running: Some(1), max: None }),
            }))
        );
        assert_eq!(by["mac"].notes.as_deref(), Some(&[][..]));
        assert_eq!(by["vm"].phones, Some(None));
        assert_eq!(
            by["vm"].notes.as_deref().unwrap(),
            &[
                "browsers cannot start here: X desktop :0 refuses this account",
                "reported no phone count (phones: null), so phones here are unknown"
            ]
        );
        assert_eq!(
            by["nas1"].notes.as_deref().unwrap(),
            &["backend busy (x): counted from its session records"]
        );
        assert_eq!(by["windows"].phones, None);
        assert_eq!(by["windows"].notes.as_deref(), Some(&[][..]));
        assert_eq!(by["windows"].error, None);
    }

    #[test]
    fn sessions_show_host_and_path_only() {
        let rows = parse_slots(&json!({
            "rows": [{
                "machine": { "name": "vm", "label": "Ubuntu VM" },
                "state": {
                    "caps": { "max_browsers": 12, "max_recordings": 5 },
                    "browsers": 2, "recordings": 1, "gpu_util": 39, "mem_gb": 15.44,
                    "sessions": [
                        { "id": "abc123", "url": "http://10.0.0.5:8963/head.html?cb=1&token=secret#x", "owner": "pid 5", "recording": true, "age_s": 466 },
                        { "id": "def456", "url": "file:///tmp/run/page%20one.html?x=1", "owner": "thread:abc", "age_s": 10 }
                    ]
                }
            }, { "machine": { "name": "laptop" }, "state": {} }]
        }))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].browsers, Counts { running: Some(2), max: Some(12) });
        assert_eq!(rows[0].recordings, Counts { running: Some(1), max: Some(5) });
        assert_eq!(rows[0].mem_gb, Some(15.4));
        assert_eq!(
            rows[0].sessions.iter().map(|s| s.url.as_deref().unwrap()).collect::<Vec<_>>(),
            ["10.0.0.5:8963/head.html", "/tmp/run/page one.html"]
        );
        assert_eq!(session_url(Some(&json!("javascript:alert(1)"))), None);
        assert!(parse_slots(&json!({})).is_err());
    }

    #[test]
    fn a_failed_host_keeps_unknown_counts_and_refuses_a_secret_text() {
        let rows = parse_slots(&json!({
            "rows": [
                { "machine": { "name": "nas1", "label": "Nas1" },
                  "state": { "error": "session list failed: exit 1: backend unavailable" }, "free": null },
                { "machine": { "name": "mac", "label": "Mac" },
                  "state": { "error": "token=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789" } }
            ]
        }))
        .unwrap();
        assert_eq!(rows[0].browsers, Counts { running: None, max: None });
        assert_eq!(rows[0].recordings, Counts { running: None, max: None });
        assert_eq!(rows[0].gpu_pct, None);
        assert!(rows[0].sessions.is_empty());
        assert_eq!(
            rows[0].error.as_deref(),
            Some("session list failed: exit 1: backend unavailable")
        );
        assert_eq!(
            rows[1].error.as_deref(),
            Some("browser-check could not read this machine")
        );
    }

    #[test]
    fn a_failed_host_keeps_its_last_good_counts_and_read_time() {
        let good = parse_slots(&json!({
            "rows": [{ "machine": { "name": "vm", "label": "Ubuntu VM" },
                       "state": { "browsers": 2, "recordings": 1, "sessions": [], "caps": { "max_browsers": 6 } } }]
        }))
        .unwrap();
        let merged = merge_rows(Some(&good), good.clone(), 1000);
        assert_eq!(merged[0].read_at, Some(Some(1000)));
        assert_eq!(merged[0].problem, Some(None));
        let failed = parse_slots(&json!({
            "rows": [{ "machine": { "name": "vm", "label": "Ubuntu VM" }, "state": { "error": "gone" } }]
        }))
        .unwrap();
        let merged = merge_rows(Some(&merged), failed, 2000);
        assert_eq!(merged[0].browsers.running, Some(2));
        assert_eq!(merged[0].read_at, Some(Some(1000)));
        assert_eq!(merged[0].error.as_deref(), Some("gone"));
        assert_eq!(
            merged[0].problem.as_ref().and_then(Option::as_ref).map(|p| p.since),
            Some(2000)
        );
        // A host that never answered has unknown counts and no read time.
        let first = parse_slots(&json!({
            "rows": [{ "machine": { "name": "mac", "label": "Mac" }, "state": { "error": "gone" } }]
        }))
        .unwrap();
        let merged = merge_rows(None, first, 3000);
        assert_eq!(merged[0].read_at, Some(None));
        assert_eq!(merged[0].browsers.running, None);
    }
}
