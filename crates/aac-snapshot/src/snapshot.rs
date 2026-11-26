//! The snapshot and history files for one sample (`buildSnapshotFiles` in
//! `agent-usage-snapshot.ts`), and the publish-time sign-in deadline recheck
//! (`downgradeCrossedSignInRows`). A pace is published only for a current reading: not after a
//! passed reset, not for a failed or sign-in-needed account, and not older than the stale limit.

use crate::history::{LongBurn, usage_history};
use crate::js::{self, burn_key, date_parse, long_burn_key, member};
use crate::projection::{ProjectedAccount, ProjectedWindow, project_agent_usage};
use crate::{HISTORY_HOURS, SNAPSHOT_JSON};
use serde_json::{Value, json};
use std::collections::HashMap;

/// One long-burn span from the history store.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LongBurnSpan {
    /// The span's length in milliseconds.
    pub span_ms: i64,
    /// The points consumed over the span, when measured.
    pub consumed: Option<f64>,
    /// Points per hour, when measured.
    pub rate: Option<f64>,
    /// The share of the span the readings cover, 0 to 1.
    pub coverage: f64,
}

/// The 7- and 30-day burn of one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LongBurnEntry {
    /// The last-7-day span.
    pub last7_days: LongBurnSpan,
    /// The last-30-day span.
    pub last30_days: LongBurnSpan,
}

/// What one finished sample saw: the accounts it recorded, the quota history after recording
/// them (or null), the sample time, and the store's long burn keyed by `longBurnKey`.
#[derive(Clone, Debug)]
pub struct SampleEvent {
    /// The dashboard rows the sample recorded.
    pub accounts: Vec<Value>,
    /// The quota history, or null while it cannot be read.
    pub history: Option<Value>,
    /// The sample time, epoch milliseconds.
    pub sampled_at: i64,
    /// Each live window's 7- and 30-day burn, absent without the store.
    pub long_burn: Option<HashMap<String, LongBurnEntry>>,
}

/// Three missed samples, and never under 5 minutes.
pub fn stale_after_seconds(refresh_seconds: f64) -> f64 {
    (refresh_seconds * 3.0).max(300.0)
}

/// Whether a reading is current: the account read ok or cached, and the window's sample is
/// within the stale limit of now.
fn current(account: &ProjectedAccount, window: &ProjectedWindow, now: i64, fresh_ms: f64) -> bool {
    if account.status != "ok" && account.status != "cached" {
        return false;
    }
    match window.sampled_at.as_deref().and_then(js::date_parse_str) {
        Some(sampled) => (now - sampled) as f64 <= fresh_ms,
        None => false,
    }
}

/// The store's long burn for one projected window, merged into a current pace.
fn long_for(event: &SampleEvent, account_id: &str, window: &ProjectedWindow) -> Option<LongBurn> {
    let long_burn = event.long_burn.as_ref()?;
    let row = event
        .accounts
        .iter()
        .find(|candidate| member(candidate, "id").as_str() == Some(account_id))?;
    let source = member(row, "windows")
        .as_array()?
        .iter()
        .find(|candidate| {
            member(candidate, "key").as_str() == Some(window.key.as_str())
                && null_str(member(candidate, "unit")) == window.unit
                && null_str(member(candidate, "poolId")) == window.pool_id
        })?;
    let entry = long_burn.get(&long_burn_key(row, source))?;
    let rate = |span: &LongBurnSpan| {
        if span.coverage >= 0.5 {
            span.rate
        } else {
            None
        }
    };
    Some(LongBurn {
        last7_days: rate(&entry.last7_days),
        last30_days: rate(&entry.last30_days),
        coverage7_days: entry.last7_days.coverage,
        coverage30_days: entry.last30_days.coverage,
    })
}

fn null_str(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// The snapshot and history files for one sample, as the values the publisher writes.
pub fn build_snapshot_files(
    event: &SampleEvent,
    refresh_seconds: f64,
    recent: Option<&crate::history::RecentReadings>,
    sign_in: &[Value],
    sign_in_renewal: Option<&Value>,
) -> (Value, Value) {
    let now = event.sampled_at;
    let projected = project_agent_usage(&event.accounts, now, None, true);
    let (rates, series) = usage_history(
        &event.accounts,
        &projected,
        event.history.as_ref(),
        now,
        recent,
    );
    let fresh_ms = stale_after_seconds(refresh_seconds) * 1000.0;
    let accounts: Vec<Value> = projected
        .iter()
        .map(|account| {
            let mut account_value = account.value();
            let windows: Vec<Value> = account
                .windows
                .iter()
                .map(|window| {
                    let rate = if window.reset_passed || !current(account, window, now, fresh_ms) {
                        None
                    } else {
                        rates.get(&account.id).and_then(|per_window| {
                            per_window.get(&burn_key(
                                &window.key,
                                window.unit.as_deref(),
                                window.pool_id.as_deref(),
                            ))
                        })
                    };
                    let mut window_value = window.value();
                    let burn = match rate {
                        Some(rate) => {
                            let mut merged = rate.clone();
                            merged.long = long_for(event, &account.id, window);
                            merged.value()
                        }
                        None => Value::Null,
                    };
                    window_value["burn"] = burn;
                    window_value
                })
                .collect();
            account_value["windows"] = Value::Array(windows);
            account_value
        })
        .collect();
    let generated_at = js::iso_of(now).unwrap_or_default();
    let mut snapshot = js::object(vec![
        ("version", json!(1)),
        ("source", json!("AI Account Center")),
        ("generatedAt", json!(generated_at)),
        ("refreshSeconds", js::number(refresh_seconds)),
        (
            "staleAfterSeconds",
            js::number(stale_after_seconds(refresh_seconds)),
        ),
        ("accounts", Value::Array(accounts)),
    ]);
    if !sign_in.is_empty() {
        snapshot["signIn"] = Value::Array(sign_in.to_vec());
    }
    if let Some(renewal) = sign_in_renewal
        && !renewal.is_null()
    {
        snapshot["signInRenewal"] = renewal.clone();
    }
    let history = js::object(vec![
        ("version", json!(1)),
        ("generatedAt", json!(generated_at)),
        ("hours", json!(HISTORY_HOURS)),
        (
            "series",
            Value::Array(series.iter().map(|s| s.value()).collect()),
        ),
    ]);
    (snapshot, history)
}

/// Publication-time recheck, on copies: a live row whose deadline crossed in (since, now] must
/// not publish as a future end. The collected rows are never mutated. A known login end already
/// past at `now` counts crossed (a behind-clock remote); a past refresh end with no named end is
/// the same skew shape; the renewable stamps count only inside (since, now].
pub fn downgrade_crossed_sign_in_rows(rows: &[Value], since: i64, now: i64) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let state = member(row, "state").as_str().unwrap_or("");
            if state != "ok" && state != "warn" && state != "critical" {
                return row.clone();
            }
            let end = date_parse(member(row, "loggedOutAt"));
            let refresh = date_parse(member(row, "refreshExpiresAt"));
            let crossed = end.is_some_and(|at| at <= now)
                || (end.is_none() && refresh.is_some_and(|at| at <= now))
                || [
                    member(row, "accessExpiresAt"),
                    member(row, "refreshExpiresAt"),
                ]
                .iter()
                .any(|value| date_parse(value).is_some_and(|at| at > since && at <= now));
            if !crossed {
                return row.clone();
            }
            let mut copy = row.clone();
            copy["state"] = json!("unavailable");
            copy["accessExpiresAt"] = Value::Null;
            copy["refreshExpiresAt"] = Value::Null;
            copy["loggedOutAt"] = Value::Null;
            copy["error"] = json!("sign-in state changed during collection; recheck pending");
            copy
        })
        .collect()
}

/// The row a failed or timed-out sign-in fetch publishes.
pub fn sign_in_collection_failed_row(hostname: &str) -> Value {
    json!({
        "name": "sign-in collection",
        "provider": "claude",
        "platform": hostname,
        "machine": hostname,
        "accessExpiresAt": Value::Null,
        "refreshExpiresAt": Value::Null,
        "loggedOutAt": Value::Null,
        "state": "unavailable",
        "fixCommand": "",
        "error": "sign-in collection failed"
    })
}

/// The commit-point file name, re-exported for the publisher's convenience.
pub const COMMIT_FILE: &str = SNAPSHOT_JSON;

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_065_600_000;

    fn row() -> Value {
        json!({
            "id": "codex:gmail", "provider": "codex", "providerLabel": "Codex",
            "label": "Gmail", "email": "person@example.com", "plan": "Pro",
            "platform": "ubuntu", "source": "Codex usage on Ubuntu", "status": "ok",
            "sampledAt": "2026-10-15T12:00:00.000Z", "isActive": true,
            "windows": [{"key": "five_hour", "label": "5-hour", "usedPercent": 30, "remainingPercent": 70,
                          "resetAt": "2026-10-15T14:00:00.000Z", "windowMinutes": 300}],
            "capabilities": {"codexProfile": "gmail"}
        })
    }

    fn record(minutes_ago: i64, used: f64) -> Value {
        json!({
            "identity": crate::js::account_identity(&row()),
            "accountId": "codex:gmail",
            "sampledAt": js::iso_of(NOW - minutes_ago * 60_000),
            "observedAt": js::iso_of(NOW - minutes_ago * 60_000),
            "platform": "ubuntu", "source": "Codex usage on Ubuntu", "status": "ok", "isActive": true,
            "windows": [{"key": "five_hour", "label": "5-hour", "usedPercent": used, "remainingPercent": 100.0 - used,
                          "resetAt": "2026-10-15T14:00:00.000Z", "windowMinutes": 300}]
        })
    }

    #[test]
    fn writes_identity_readings_burn_and_history() {
        let event = SampleEvent {
            accounts: vec![row()],
            history: Some(
                json!({"schemaVersion": 1, "collectedSince": "2026-10-01T00:00:00.000Z",
                                  "records": [record(60, 10.0), record(30, 20.0), record(10, 25.0)]}),
            ),
            sampled_at: NOW,
            long_burn: None,
        };
        let (snapshot, history) = build_snapshot_files(&event, 60.0, None, &[], None);
        assert_eq!(member(&snapshot, "refreshSeconds").as_f64(), Some(60.0));
        assert_eq!(member(&snapshot, "staleAfterSeconds").as_f64(), Some(300.0));
        let window = &member(&snapshot, "accounts")[0]["windows"][0];
        assert_eq!(member(window, "burn")["last15Minutes"].as_f64(), Some(30.0));
        let series = member(&history, "series").as_array().expect("series");
        assert_eq!(series.len(), 1);
        let points: Vec<f64> = series[0]["points"]
            .as_array()
            .expect("points")
            .iter()
            .map(|p| p[1].as_f64().expect("used"))
            .collect();
        assert_eq!(points, vec![10.0, 20.0, 25.0, 30.0]);
        let words = crate::markdown::render_snapshot_markdown(&snapshot, NOW);
        assert!(
            words.contains("**Gmail** (person@example.com, profile gmail), Pro, Ubuntu VM, active")
        );
        assert!(words.contains("sampled just now"));
        assert!(words.contains("30% used, 70% left"));
        assert!(words.contains("burning 30%/h over the last 15 min"));
        assert!(words.contains("full in 2h 20m (after the reset)"));
    }

    #[test]
    fn restarted_weekly_window_publishes_no_pace_across_an_omitted_reset_time() {
        let weekly = |id: &str, sampled_at: &str, used: f64, reset: Option<&str>| {
            json!({
                "id": id, "provider": "opencode-go", "providerLabel": "OpenCode Go", "label": id,
                "status": "ok", "platform": "ubuntu", "sampledAt": sampled_at,
                "windows": [{"key": "weekly", "label": "Weekly", "kind": "rate_limit",
                             "usedPercent": used, "resetAt": reset}]
            })
        };
        let original = "opencode-go:original";
        let t32 = "2026-11-19T21:32:00.000Z";
        let t33 = "2026-11-19T21:33:00.000Z";
        let t35 = "2026-11-19T21:35:00.000Z";
        let next_reset = "2026-11-26T21:31:00.000Z";
        let mut older = weekly(original, t32, 0.0, Some("2026-11-19T21:31:00.000Z"));
        older["identity"] = json!(crate::js::account_identity(&older));
        let mut recent = crate::history::RecentReadings::new();
        recent.add(
            &[weekly(original, t33, 0.0, None)],
            js::date_parse_str(t33).expect("intermediate sample time"),
        );
        let event = SampleEvent {
            accounts: vec![
                weekly("opencode-go:new-2", t35, 52.0, Some(next_reset)),
                weekly(original, t35, 39.0, Some(next_reset)),
                weekly("opencode-go:new-1", t35, 12.0, Some(next_reset)),
            ],
            history: Some(json!({"records": [older]})),
            sampled_at: js::date_parse_str(t35).expect("latest sample time"),
            long_burn: None,
        };
        let (snapshot, _) = build_snapshot_files(&event, 60.0, Some(&recent), &[], None);
        let accounts = snapshot["accounts"].as_array().expect("published accounts");
        assert_eq!(accounts.len(), 3);
        for account in accounts {
            let burn = &account["windows"][0]["burn"];
            assert_eq!(burn["pace"], Value::Null, "{}", account["id"]);
            assert_eq!(burn["last5Minutes"], Value::Null);
            assert_eq!(burn["projectedFullAt"], Value::Null);
            assert_eq!(burn["periodStartedAt"], t35);
        }
        let words = crate::markdown::render_snapshot_markdown(&snapshot, event.sampled_at);
        assert!(words.contains("39% used"));
        assert!(!words.contains("full in"));
    }

    #[test]
    fn no_burn_for_passed_reset_or_sign_in_needed() {
        let mut passed = row();
        passed["sampledAt"] = json!("2026-10-15T09:00:00.000Z");
        passed["windows"][0]["resetAt"] = json!("2026-10-15T10:00:00.000Z");
        let mut sign_in = row();
        sign_in["id"] = json!("codex:party");
        sign_in["status"] = json!("needs_sign_in");
        let event = SampleEvent {
            accounts: vec![passed, sign_in],
            history: None,
            sampled_at: NOW,
            long_burn: None,
        };
        let (snapshot, _) = build_snapshot_files(&event, 60.0, None, &[], None);
        let accounts = member(&snapshot, "accounts").as_array().expect("accounts");
        assert!(member(&accounts[0]["windows"][0], "burn").is_null());
        assert!(member(&accounts[1]["windows"][0], "burn").is_null());
    }

    #[test]
    fn downgrade_crossed_rows_on_copies() {
        let rows = vec![
            json!({"name": "a", "state": "critical", "loggedOutAt": "2026-10-15T12:30:00Z"}),
            json!({"name": "later", "state": "critical", "loggedOutAt": "2026-10-16T12:00:00Z"}),
            json!({"name": "renewing", "state": "warn", "loggedOutAt": Value::Null,
                    "accessExpiresAt": "2026-10-15T11:00:00Z", "refreshExpiresAt": "2026-11-30T00:00:00Z"}),
        ];
        let out = downgrade_crossed_sign_in_rows(&rows, NOW, NOW + 60 * 60_000);
        assert_eq!(out[0]["state"], json!("unavailable"));
        assert_eq!(
            out[0]["error"],
            json!("sign-in state changed during collection; recheck pending")
        );
        assert!(out[0]["loggedOutAt"].is_null());
        assert_eq!(out[1]["state"], json!("critical"));
        assert_eq!(out[2]["state"], json!("warn"));
        assert_eq!(rows[0]["state"], json!("critical"));
        let skewed = downgrade_crossed_sign_in_rows(
            &[json!({"state": "critical", "loggedOutAt": "2026-10-15T11:00:00Z"})],
            NOW,
            NOW,
        );
        assert_eq!(skewed[0]["state"], json!("unavailable"));
    }
}
