//! G16c: `simlock status --json` as the Status feed carries it.
//!
//! A port of `parseSimlockStatus` (`status-collectors.ts`): capacity per platform and the
//! leases, never a lease id, owner id or token. A daemon still starting, a failed daemon or an
//! answer without its capacity and leases is a fixed refusal; a worker that is not connected
//! names a problem and its devices' leases are marked last-known.

use super::browsers::short_text;
use serde::Serialize;
use serde_json::Value as Json;

/// A refused answer, in the TypeScript error's fixed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimlockError {
    /// `simlock answered in an unknown shape.`
    Shape,
    /// `Simlock is starting and has not read its devices yet.`
    Starting,
    /// `The Simlock daemon reports that it failed.`
    Failed,
}

impl SimlockError {
    /// The error's message, as the collector puts it on the source.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Shape => "simlock answered in an unknown shape.",
            Self::Starting => "Simlock is starting and has not read its devices yet.",
            Self::Failed => "The Simlock daemon reports that it failed.",
        }
    }
}

/// One platform's capacity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capacity {
    pub platform: String,
    pub running: Option<i64>,
    pub max: Option<i64>,
    pub reserved: Option<i64>,
    pub over_limit: bool,
}

/// One lease.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub device: String,
    pub device_label: Option<String>,
    pub platform: Option<String>,
    pub holder: Option<String>,
    pub since: Option<String>,
    pub expires_at: Option<String>,
    /// The lease is on a worker the gateway cannot reach: what it last knew.
    pub last_known: bool,
}

/// A gateway worker that is not connected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerProblem {
    pub item: String,
    pub reason: String,
    pub last_seen_at: Option<String>,
}

/// The whole reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub capacity: Vec<Capacity>,
    pub leases: Vec<Lease>,
    pub problems: Vec<WorkerProblem>,
}

fn num(value: Option<&Json>) -> Option<f64> {
    value.and_then(Json::as_f64).filter(|n| n.is_finite())
}

fn int(value: Option<&Json>) -> Option<i64> {
    num(value).map(|n| {
        #[allow(clippy::cast_possible_truncation)]
        let rounded = aac_num::ecma::math_round(n) as i64;
        rounded
    })
}

fn iso_from_ms(value: Option<&Json>) -> Option<String> {
    let ms = num(value)?;
    if ms <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    aac_num::ecma::to_iso_string(ms as i64)
}

/// `capacityFromWorkers`: the fleet's capacity added up from its workers.
///
/// Every worker reports an iPhone and an Android row, but runs only the platforms its catalog
/// lists, and a drained worker takes no new phones. So running and reserved come from every
/// worker, a platform's cap only from a worker that can run it (any platform when the worker
/// lists no catalog) and is not drained, and `global` from every worker that is not drained.
/// `None` when no worker carries capacity: an older gateway's own sum stands.
fn capacity_from_workers(workers: Option<&Json>) -> Option<Json> {
    let mut rows = serde_json::Map::new();
    let mut carried = false;
    for worker in workers?.as_array()? {
        let Some(capacity) = worker.get("capacity").filter(|cap| cap.is_object()) else {
            continue;
        };
        carried = true;
        let catalog: Option<Vec<&str>> = match worker.get("catalog") {
            Some(catalog) if catalog.is_object() => {
                catalog.as_object().map(|map| map.keys().map(String::as_str).collect())
            }
            _ => None,
        };
        let drained = worker.get("drained") == Some(&Json::Bool(true));
        let Some(capacity) = capacity.as_object() else {
            continue;
        };
        for (platform, value) in capacity {
            if !value.is_object() {
                continue;
            }
            let row = rows.entry(platform.clone()).or_insert_with(|| {
                serde_json::json!({
                    "running": 0, "maxRunning": 0, "reserved": 0, "overLimit": false
                })
            });
            let add = |row: &mut Json, field: &str, from: Option<&Json>| {
                let total = int(row.get(field)).unwrap_or(0) + int(from).unwrap_or(0);
                row[field] = Json::from(total);
            };
            add(row, "running", value.get("running"));
            add(row, "reserved", value.get("reserved"));
            if value.get("overLimit") == Some(&Json::Bool(true)) {
                row["overLimit"] = Json::Bool(true);
            }
            let runs = platform == "global"
                || catalog
                    .as_ref()
                    .is_none_or(|catalog| catalog.contains(&platform.as_str()));
            if runs && !drained {
                add(
                    row,
                    "maxRunning",
                    value.get("maxRunning").or_else(|| value.get("limit")),
                );
            }
        }
    }
    carried.then(|| Json::Object(rows))
}

/// `parseSimlockStatus`.
///
/// # Errors
/// See [`SimlockError`].
pub fn parse_status(raw: &Json) -> Result<Status, SimlockError> {
    let rec = raw.as_object().ok_or(SimlockError::Shape)?;
    let health = raw
        .get("daemon")
        .and_then(|daemon| daemon.get("health"))
        .and_then(Json::as_str);
    if health == Some("starting") {
        return Err(SimlockError::Starting);
    }
    if health == Some("failed") {
        return Err(SimlockError::Failed);
    }
    if rec.contains_key("daemon") && (!rec.contains_key("capacity") || !raw.get("leases").is_some_and(Json::is_array)) {
        return Err(SimlockError::Shape);
    }
    let capacity_raw = capacity_from_workers(raw.get("workers"))
        .or_else(|| raw.get("capacity").cloned())
        .unwrap_or(Json::Null);
    if !capacity_raw.is_object() {
        return Err(SimlockError::Shape);
    }
    let mut offline = std::collections::HashSet::new();
    let mut problems = Vec::new();
    for worker in raw
        .get("workers")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(id) = worker.get("id").and_then(Json::as_str) else {
            continue;
        };
        if worker.get("connection").and_then(Json::as_str) == Some("connected") {
            continue;
        }
        offline.insert(id.to_owned());
        let reason = if worker.get("connection").and_then(Json::as_str) == Some("incompatible") {
            "incompatible with the gateway; its devices and leases are the last known"
        } else {
            "disconnected; its devices and leases are the last known"
        };
        problems.push(WorkerProblem {
            item: short_text(worker.get("label"), 80)
                .or_else(|| short_text(worker.get("id"), 80))
                .unwrap_or_else(|| "A worker".to_owned()),
            reason: reason.to_owned(),
            last_seen_at: iso_from_ms(worker.get("lastSeenAt")),
        });
    }
    let mut capacity = Vec::new();
    if let Json::Object(map) = &capacity_raw {
        for (platform, value) in map {
            if !(1..=20).contains(&platform.len())
                || !platform
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || !value.is_object()
            {
                continue;
            }
            capacity.push(Capacity {
                platform: platform.clone(),
                running: int(value.get("running")),
                max: int(value.get("maxRunning").or_else(|| value.get("limit"))),
                reserved: int(value.get("reserved")),
                over_limit: value.get("overLimit") == Some(&Json::Bool(true)),
            });
        }
    }
    let mut devices = std::collections::HashMap::new();
    for device in raw
        .get("devices")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let Some(id) = device.get("id").and_then(Json::as_str) {
            devices.insert(
                id.to_owned(),
                (
                    short_text(device.get("spec").and_then(|s| s.get("model")), 60),
                    short_text(device.get("spec").and_then(|s| s.get("platform")), 20),
                ),
            );
        }
    }
    let mut leases = Vec::new();
    for lease in raw
        .get("leases")
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(device) = short_text(lease.get("deviceId"), 80) else {
            continue;
        };
        let (device_label, platform) = devices.get(&device).cloned().unwrap_or((None, None));
        leases.push(Lease {
            device: device.clone(),
            device_label,
            platform,
            holder: short_text(lease.get("worker").and_then(|w| w.get("label")), 80)
                .or_else(|| short_text(lease.get("requesterId"), 80)),
            since: iso_from_ms(lease.get("grantedAt")),
            expires_at: iso_from_ms(lease.get("ttlDeadline")),
            last_known: lease
                .get("workerId")
                .and_then(Json::as_str)
                .is_some_and(|id| offline.contains(id)),
        });
    }
    Ok(Status {
        capacity,
        leases,
        problems,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_capacity_and_leases_without_ids_or_tokens() {
        let status = parse_status(&json!({
            "devices": [{ "id": "dev_1", "spec": { "platform": "ios", "model": "iPhone Duo" } }],
            "leases": [{
                "id": "lease-secret-id", "deviceId": "dev_1", "requesterId": "agent-7",
                "ownerId": "owner-x",
                "grantedAt": 1793980800000_i64, "ttlDeadline": 1793984400000_i64,
                "token": "abc"
            }],
            "capacity": {
                "ios": { "running": 1, "maxRunning": 1, "reserved": 0, "overLimit": false },
                "android": { "running": 0, "maxRunning": 1, "reserved": 1, "overLimit": true }
            }
        }))
        .unwrap();
        // Platform order follows the answer's own object order (Node's), which this crate's
        // JSON map keeps only with `preserve_order`; compare the rows by platform.
        let mut capacity = status.capacity.clone();
        capacity.sort_by(|a, b| a.platform.cmp(&b.platform));
        assert_eq!(
            capacity,
            vec![
                Capacity {
                    platform: "android".into(),
                    running: Some(0),
                    max: Some(1),
                    reserved: Some(1),
                    over_limit: true
                },
                Capacity {
                    platform: "ios".into(),
                    running: Some(1),
                    max: Some(1),
                    reserved: Some(0),
                    over_limit: false
                },
            ]
        );
        assert_eq!(
            status.leases,
            vec![Lease {
                device: "dev_1".into(),
                device_label: Some("iPhone Duo".into()),
                platform: Some("ios".into()),
                holder: Some("agent-7".into()),
                since: Some("2026-11-06T16:00:00.000Z".into()),
                expires_at: Some("2026-11-06T17:00:00.000Z".into()),
                last_known: false,
            }]
        );
        assert!(status.problems.is_empty());
        assert!(!serde_json::to_string(&status).unwrap().contains("lease-secret-id"));
    }

    #[test]
    fn refuses_starting_failed_and_incomplete_answers() {
        assert_eq!(
            parse_status(&json!({ "daemon": { "health": "starting", "mode": "worker" }, "host": {} })),
            Err(SimlockError::Starting)
        );
        assert_eq!(
            parse_status(
                &json!({ "daemon": { "health": "failed", "mode": "worker" }, "capacity": {}, "devices": [], "leases": [] })
            ),
            Err(SimlockError::Failed)
        );
        assert_eq!(
            parse_status(&json!({ "daemon": { "health": "running" }, "host": {} })),
            Err(SimlockError::Shape)
        );
    }

    #[test]
    fn adds_the_fleets_phone_caps_from_the_workers_that_can_run_each_platform() {
        // The real gateway on 2026-11-20: every worker reports iPhone and Android rows, the
        // gateway summed 6 iPhones where only the Mac's 2 exist.
        let worker = |label: &str, catalog: &str, ios: u64, android: u64, drained: bool| {
            let mut worker = json!({
                "id": label,
                "label": label,
                "connection": "connected",
                "drained": drained,
                "catalog": {},
                "capacity": {
                    "ios": { "running": 0, "maxRunning": ios, "reserved": 0, "overLimit": false, "limit": ios },
                    "android": {
                        "running": if label == "vm" { 1 } else { 0 },
                        "maxRunning": android,
                        "reserved": 0,
                        "overLimit": false
                    },
                    "global": {
                        "running": if label == "vm" { 1 } else { 0 },
                        "maxRunning": ios.max(android),
                        "reserved": 0,
                        "overLimit": false
                    }
                }
            });
            worker["catalog"][catalog] = json!([]);
            worker
        };
        let status = parse_status(&json!({
            "daemon": { "health": "running", "mode": "gateway" },
            "capacity": {
                "ios": { "running": 0, "maxRunning": 6, "reserved": 0, "overLimit": false },
                "android": { "running": 1, "maxRunning": 5, "reserved": 0, "overLimit": false },
                "global": { "running": 1, "maxRunning": 7, "reserved": 0, "overLimit": false }
            },
            "devices": [],
            "leases": [],
            "workers": [
                worker("nas1", "android", 1, 1, false),
                worker("vm", "android", 1, 1, false),
                worker("mac", "ios", 2, 1, false),
                worker("1070ti", "android", 2, 2, true)
            ]
        }))
        .unwrap();
        let mut capacity = status.capacity.clone();
        capacity.sort_by(|a, b| a.platform.cmp(&b.platform));
        assert_eq!(
            capacity,
            vec![
                Capacity {
                    platform: "android".into(),
                    running: Some(1),
                    max: Some(2),
                    reserved: Some(0),
                    over_limit: false
                },
                Capacity {
                    platform: "global".into(),
                    running: Some(1),
                    max: Some(4),
                    reserved: Some(0),
                    over_limit: false
                },
                Capacity {
                    platform: "ios".into(),
                    running: Some(0),
                    max: Some(2),
                    reserved: Some(0),
                    over_limit: false
                },
            ]
        );
    }

    #[test]
    fn an_older_gateway_without_worker_capacity_keeps_its_own_sum() {
        let status = parse_status(&json!({
            "daemon": { "health": "running", "mode": "gateway" },
            "capacity": {
                "ios": { "running": 0, "maxRunning": 6, "reserved": 0, "overLimit": false },
                "android": { "running": 1, "maxRunning": 5, "reserved": 0, "overLimit": false }
            },
            "devices": [],
            "leases": [],
            "workers": [
                { "id": "mac", "label": "Mac", "connection": "connected" },
                { "id": "vm", "label": "VM", "connection": "connected" }
            ]
        }))
        .unwrap();
        let mut capacity = status.capacity.clone();
        capacity.sort_by(|a, b| a.platform.cmp(&b.platform));
        assert_eq!(
            capacity
                .iter()
                .map(|row| (row.platform.as_str(), row.max))
                .collect::<Vec<_>>(),
            [("android", Some(5)), ("ios", Some(6))]
        );
    }

    #[test]
    fn a_worker_without_a_catalog_runs_every_platform_and_an_over_limit_row_stays_visible() {
        let status = parse_status(&json!({
            "daemon": { "health": "running", "mode": "gateway" },
            "capacity": {},
            "devices": [],
            "leases": [],
            "workers": [{
                "id": "lone", "label": "Lone", "connection": "connected",
                "capacity": {
                    "ios": { "running": 2, "maxRunning": 1, "reserved": 1, "overLimit": true }
                }
            }]
        }))
        .unwrap();
        assert_eq!(
            status.capacity,
            vec![Capacity {
                platform: "ios".into(),
                running: Some(2),
                max: Some(1),
                reserved: Some(1),
                over_limit: true
            }]
        );
    }

    #[test]
    fn names_a_disconnected_worker_and_marks_its_leases() {
        let status = parse_status(&json!({
            "daemon": { "health": "running", "mode": "gateway" },
            "capacity": { "ios": { "running": 0, "maxRunning": 1, "reserved": 0, "overLimit": false } },
            "devices": [
                { "id": "dev_1", "workerId": "mac", "spec": { "platform": "ios", "model": "iPhone Duo" } },
                { "id": "dev_2", "workerId": "vm", "spec": { "platform": "android", "model": "Pixel" } }
            ],
            "leases": [
                { "deviceId": "dev_1", "workerId": "mac", "requesterId": "agent-1", "grantedAt": 1 },
                { "deviceId": "dev_2", "workerId": "vm", "requesterId": "agent-2", "grantedAt": 1 }
            ],
            "workers": [
                { "id": "mac", "label": "Mac", "connection": "disconnected", "drained": false, "lastSeenAt": 1793980800000_i64 },
                { "id": "vm", "label": "VM", "connection": "connected", "drained": false, "lastSeenAt": 2 }
            ]
        }))
        .unwrap();
        assert_eq!(
            status.problems,
            vec![WorkerProblem {
                item: "Mac".into(),
                reason: "disconnected; its devices and leases are the last known".into(),
                last_seen_at: Some("2026-11-06T16:00:00.000Z".into()),
            }]
        );
        assert_eq!(
            status
                .leases
                .iter()
                .map(|l| (l.device.as_str(), l.last_known))
                .collect::<Vec<_>>(),
            [("dev_1", true), ("dev_2", false)]
        );
    }
}
