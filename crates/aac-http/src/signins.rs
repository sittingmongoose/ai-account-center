//! Cross-host sign-in expiry and Claude renewal projection.
pub mod poll;
mod profile_rows;

use aac_views::{decline::Decline, js::parse_date};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const MIN_TTL: i64 = 30_000;
const MAX_TTL: i64 = 300_000;
pub(super) const STAMPS: [&str; 3] = ["accessExpiresAt", "refreshExpiresAt", "loggedOutAt"];
const RENEWAL_FIELDS: [&str; 7] = [
    "email",
    "autoRenew",
    "renewFrom",
    "lastRenewAt",
    "lastResult",
    "nextVerifyAt",
    "reverts",
];

fn time(value: &Value) -> Result<Option<i64>, Decline> {
    match value.as_str() {
        Some(stamp) => parse_date(stamp).map_err(|_| Decline::Date),
        None => Ok(None),
    }
}

fn is_live(row: &Value) -> bool {
    matches!(row["state"].as_str(), Some("ok" | "warn" | "critical"))
}

/// Whether a live row's observable deadline crossed after its host read.
pub fn crossed(row: &Value, since: i64, now: i64) -> Result<bool, Decline> {
    if !is_live(row) {
        return Ok(false);
    }
    let end = time(&row["loggedOutAt"])?;
    let refresh = time(&row["refreshExpiresAt"])?;
    let access = time(&row["accessExpiresAt"])?;
    let passed = |at: Option<i64>| at.is_some_and(|at| at <= now);
    let crossing = |at: Option<i64>| at.is_some_and(|at| at > since && at <= now);
    Ok(passed(end) || (end.is_none() && passed(refresh)) || crossing(access) || crossing(refresh))
}

/// Normalize concurrent host reports collected on different clocks and calculate the cache TTL.
pub fn settle(
    mut machines: Map<String, Value>,
    since: i64,
    now: i64,
) -> Result<(Value, i64), Decline> {
    machines.entry("ubuntu").or_insert_with(|| {
        json!([{
            "name":"ubuntu", "provider":"claude", "platform":"ubuntu", "machine":"ubuntu",
            "state":"unavailable", "fixCommand":"",
            "error":"no collection route from this host"
        }])
    });
    let mut changed = false;
    let mut earliest = i64::MAX;
    for rows in machines.values_mut() {
        for row in rows.as_array_mut().ok_or(Decline::Shape)? {
            if crossed(row, since, now)? {
                row["state"] = json!("unavailable");
                for stamp in STAMPS {
                    row[stamp] = Value::Null;
                }
                row["error"] = json!("sign-in state changed during collection; recheck pending");
                changed = true;
            } else {
                for stamp in STAMPS {
                    if let Some(at) = time(&row[stamp])?.filter(|at| *at > now) {
                        earliest = earliest.min(at);
                    }
                }
            }
        }
    }
    let ttl = if changed {
        MIN_TTL
    } else {
        earliest.saturating_sub(now).clamp(MIN_TTL, MAX_TTL)
    };
    Ok((Value::Object(machines), ttl))
}

fn copy_renewal(row: &mut Map<String, Value>, renewal: &Value) -> Result<(), Decline> {
    let renewal = renewal.as_object().ok_or(Decline::Shape)?;
    for field in RENEWAL_FIELDS {
        if let Some(value) = renewal.get(field) {
            row.insert(field.to_owned(), value.clone());
        }
    }
    Ok(())
}

fn renewed(row: &Value, host: &str, now: i64) -> Result<Value, Decline> {
    let end = time(&row["logoutAt"])?;
    let state = match row["state"].as_str() {
        Some("logged_out" | "no_refresh_token") => "logged_out",
        Some("ok") => match end.map(|end| end.saturating_sub(now)) {
            Some(delta) if delta <= 0 => "logged_out",
            Some(delta) if delta < 4 * 3_600_000 => "critical",
            Some(delta) if delta < 24 * 3_600_000 => "warn",
            _ => "ok",
        },
        _ => "unavailable",
    };
    let mut profile = json!({
        "name": row["profile"], "provider": "claude", "platform": host, "machine": host,
        "accessExpiresAt": null, "refreshExpiresAt": row["signInEndsAt"], "loggedOutAt": row["logoutAt"],
        "state": state, "fixCommand": "",
    });
    copy_renewal(profile.as_object_mut().ok_or(Decline::Internal)?, row)?;
    Ok(profile)
}

/// Merge the current renewal report. A host the runner read replaces its Claude rows.
/// An unsuccessfully read host keeps its collector rows and gets matching renewal annotations.
pub fn merge_renewal(
    mut machines: Map<String, Value>,
    renewal: &Value,
    now: i64,
) -> Result<Map<String, Value>, Decline> {
    if renewal.is_null() || renewal["stale"] == true {
        return Ok(machines);
    }
    let hosts = renewal["hosts"].as_array().ok_or(Decline::Shape)?;
    let rows = renewal["signIns"].as_array().ok_or(Decline::Shape)?;
    let mut by_host = Map::new();
    for row in rows {
        let host = row["host"].as_str().ok_or(Decline::Shape)?;
        let entry = by_host.entry(host).or_insert_with(|| json!([]));
        entry
            .as_array_mut()
            .ok_or(Decline::Internal)?
            .push(row.clone());
    }
    for (host, collection) in by_host {
        let entries = collection.as_array().ok_or(Decline::Internal)?;
        let read_ok = hosts.iter().any(|row| {
            row["host"] == host && (row["error"].is_null() || row["error"].as_str() == Some(""))
        });
        if read_ok {
            let mut result = entries
                .iter()
                .map(|r| renewed(r, &host, now))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(others) = machines.get(&host).and_then(Value::as_array) {
                result.extend(others.iter().filter(|r| r["provider"] != "claude").cloned());
            }
            machines.insert(host, Value::Array(result));
        } else if let Some(rows) = machines.get_mut(&host).and_then(Value::as_array_mut) {
            for row in rows {
                if let Some(entry) = entries.iter().find(|entry| entry["profile"] == row["name"]) {
                    copy_renewal(row.as_object_mut().ok_or(Decline::Shape)?, entry)?;
                }
            }
        }
    }
    Ok(machines)
}

pub(crate) fn answer(bytes: &[u8]) -> Result<Value, Decline> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Settle {
        machines: Map<String, Value>,
        collected_at: i64,
        now: i64,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Merge {
        machines: Map<String, Value>,
        renewal: Value,
        now: i64,
    }
    if let Ok(p) =
        crate::json_object::from_slice::<Settle>(bytes, &["machines", "collectedAt", "now"])
    {
        let (machines, ttl) = settle(p.machines, p.collected_at, p.now)?;
        return Ok(json!({"machines":machines,"ttlMs":ttl}));
    }
    let p: Merge = crate::json_object::from_slice(bytes, &["machines", "renewal", "now"])
        .map_err(|_| Decline::Shape)?;
    Ok(json!({"machines":merge_renewal(p.machines, &p.renewal, p.now)?}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: i64 = 1_767_225_600_000;

    #[test]
    fn settle_downgrades_a_crossed_row_and_adds_the_missing_ubuntu_route() {
        let input = json!({
            "machines": {"mac": [{
                "name": "fixture", "machine": "mac", "platform": "mac",
                "provider": "claude", "state": "ok", "fixCommand": "",
                "accessExpiresAt": "2026-01-01T00:00:01.000Z"
            }]},
            "collectedAt": AT,
            "now": AT + 5000
        });
        let answer = answer(input.to_string().as_bytes()).expect("settle");
        assert_eq!(
            answer,
            json!({
                "machines": {
                    "mac": [{
                        "name": "fixture", "machine": "mac", "platform": "mac",
                        "provider": "claude", "state": "unavailable", "fixCommand": "",
                        "accessExpiresAt": null, "refreshExpiresAt": null, "loggedOutAt": null,
                        "error": "sign-in state changed during collection; recheck pending"
                    }],
                    "ubuntu": [{
                        "name": "ubuntu", "provider": "claude", "platform": "ubuntu",
                        "machine": "ubuntu", "state": "unavailable", "fixCommand": "",
                        "error": "no collection route from this host"
                    }]
                },
                "ttlMs": 30_000
            })
        );
    }

    #[test]
    fn merge_replaces_a_read_host_and_annotates_an_unread_one() {
        let row = json!({
            "name": "fixture", "machine": "mac", "platform": "mac",
            "provider": "claude", "state": "ok", "fixCommand": "",
            "accessExpiresAt": null, "refreshExpiresAt": null, "loggedOutAt": null
        });
        let input = json!({
            "machines": {"mac": [row.clone()], "windows": [{
                "name": "fixture", "machine": "windows", "platform": "windows",
                "provider": "claude", "state": "ok", "fixCommand": "",
                "accessExpiresAt": null, "refreshExpiresAt": null, "loggedOutAt": null
            }]},
            "renewal": {
                "stale": false,
                "hosts": [{"host": "mac", "error": null}, {"host": "windows", "error": "unavailable"}],
                "signIns": [
                    {"profile": "fixture", "host": "mac", "state": "ok", "logoutAt": null,
                     "signInEndsAt": null, "email": "fixture@example.test", "autoRenew": true},
                    {"profile": "fixture", "host": "windows", "state": "logged_out",
                     "logoutAt": null, "signInEndsAt": null, "autoRenew": false}
                ]
            },
            "now": AT
        });
        let answer = answer(input.to_string().as_bytes()).expect("merge");
        let mac = &answer["machines"]["mac"][0];
        assert_eq!(mac["name"], "fixture");
        assert_eq!(mac["email"], "fixture@example.test");
        assert_eq!(mac["autoRenew"], true);
        assert_eq!(mac["state"], "ok");
        let windows = &answer["machines"]["windows"][0];
        assert_eq!(windows["autoRenew"], false);
        assert_eq!(windows["state"], "ok");
    }

    #[test]
    fn a_stale_renewal_report_leaves_the_collected_rows_alone() {
        let input = json!({
            "machines": {"mac": []},
            "renewal": {"stale": true, "hosts": [], "signIns": []},
            "now": AT
        });
        let answer = answer(input.to_string().as_bytes()).expect("merge");
        assert_eq!(answer, json!({"machines": {"mac": []}}));
    }
}
