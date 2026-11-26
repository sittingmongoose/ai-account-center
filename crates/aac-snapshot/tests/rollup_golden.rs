//! R2 golden: the streamed rollup export (`exports::write_rollup_exports`) writes bytes
//! identical to the base `build_rollup_exports` + `to_file` value tree, serialized with
//! `JSON.stringify`. The reference below reconstructs that base file value independently
//! from the store's own series reads, so this pins the streaming writer's bytes against the
//! proven format rather than against itself.

use aac_snapshot::exports::write_rollup_exports;
use aac_snapshot::js::{account_identity, member, stringify};
use aac_snapshot::projection::{ProjectedAccount, project_agent_usage};
use aac_store::engine::Engine;
use aac_store::usage::{SeriesListQuery, SeriesQuery};
use serde_json::{Value, json};
use std::collections::HashMap;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const NOW: i64 = 1_792_065_600_000; // 2026-10-15T12:00:00Z, an exact hour.

/// One account row the projection and the export both read.
fn row(id: &str, provider: &str, email: Option<&str>, label: &str) -> Value {
    json!({
        "id": id, "provider": provider, "providerLabel": provider,
        "label": label, "email": email, "plan": "Pro",
        "platform": "ubuntu", "source": format!("{provider} quota on Ubuntu"),
        "status": "ok", "isActive": true,
        "sampledAt": "2026-10-15T12:00:00.000Z",
        "windows": [{"key": "five_hour", "label": "5-hour", "usedPercent": 40,
                     "remainingPercent": 60, "windowMinutes": 300}],
    })
}

/// Seed quota observations so the store holds hourly and daily rollups for each identity.
fn seed(engine: &Engine, identity: &str, provider: &str, account_id: &str, points: &[(i64, f64)]) {
    let observations: Vec<Value> = points
        .iter()
        .map(|&(t, used)| {
            json!({
                "account": {
                    "identity": identity, "provider": provider, "account_id": account_id,
                    "plan": null, "platform": "ubuntu", "source": "golden test",
                    "status": "ok", "is_active": true,
                },
                "sampled_at_ms": t, "observed_at_ms": t, "import_ref": null,
                "windows": [{
                    "identity": "[\"five_hour\",null,null]", "key": "five_hour", "label": "5-hour",
                    "used_pct": used, "remaining_pct": 100.0 - used,
                    "reset_at_ms": t + HOUR_MS, "window_minutes": 300.0,
                }],
            })
        })
        .collect();
    aac_store::ops::call(
        engine,
        "record",
        &json!({ "observations": observations }).to_string(),
    )
    .expect("record");
}

/// The base export file value, reconstructed from the store's series reads: `series_list`
/// metas in order, each series' points paged through `series_points` (the same paging the
/// export uses), joined to the projection by analytics identity, the first point element
/// turned into its ISO string. This is the `complete` + `to_file` value the base serialized.
fn reference_file(
    engine: &Engine,
    resolution: &str,
    days: i64,
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
) -> Value {
    let step = if resolution == "hour" {
        HOUR_MS
    } else {
        DAY_MS
    };
    let from_ms = ((now - days * DAY_MS) as f64 / step as f64).floor() as i64 * step;
    let list = aac_store::usage::series_list(
        engine,
        &SeriesListQuery {
            identities: None,
            account_ids: None,
            provider: None,
            window_key: None,
            from_ms: from_ms.max(0) as u64,
            to_ms: now.max(0) as u64,
        },
    )
    .expect("series_list");
    let metas = list.as_array().expect("series_list array");

    let mut by_identity: HashMap<String, &ProjectedAccount> = HashMap::new();
    for row in rows {
        let id = member(row, "id").as_str().unwrap_or("");
        if let Some(account) = projected.iter().find(|candidate| candidate.id == id) {
            by_identity.insert(account_identity(row), account);
        }
    }

    let mut series_out: Vec<Value> = Vec::new();
    for meta in metas {
        let identity = member(meta, "identity")
            .as_str()
            .expect("identity")
            .to_owned();
        let window_key = member(member(meta, "window"), "key")
            .as_str()
            .expect("key")
            .to_owned();
        let window_identity = member(member(meta, "window"), "identity")
            .as_str()
            .expect("window identity")
            .to_owned();
        let mut points: Vec<Value> = Vec::new();
        let mut from = from_ms;
        while from <= now {
            let to = now.min(from + 1000 * step - 1);
            let result = aac_store::usage::series_points(
                engine,
                &SeriesQuery {
                    identities: Some(vec![identity.clone()]),
                    account_ids: None,
                    provider: None,
                    window_key: Some(window_key.clone()),
                    from_ms: from.max(0) as u64,
                    to_ms: to.max(0) as u64,
                    resolution: resolution.to_owned(),
                    limit: None,
                },
            )
            .expect("series_points");
            assert_ne!(
                member(&result, "truncated"),
                &json!(true),
                "fixture must not truncate"
            );
            if let Some(items) = member(&result, "series").as_array() {
                for item in items {
                    if member(member(item, "window"), "identity").as_str() == Some(&window_identity)
                        && let Some(item_points) = member(item, "points").as_array()
                    {
                        points.extend(item_points.iter().cloned());
                    }
                }
            }
            from += 1000 * step;
        }
        if points.is_empty() {
            continue;
        }
        let account = by_identity.get(&identity).copied();
        let window = member(meta, "window");
        let points: Vec<Value> = points
            .iter()
            .map(|point| {
                let parts = point.as_array();
                let start = parts.and_then(|p| p.first()).and_then(Value::as_f64);
                let mut out = vec![json!(
                    start
                        .and_then(|ms| aac_snapshot::js::iso_of(ms as i64))
                        .unwrap_or_default()
                )];
                if let Some(parts) = parts {
                    out.extend(parts.iter().skip(1).cloned());
                }
                Value::Array(out)
            })
            .collect();
        series_out.push(json!({
            "accountId": member(meta, "accountId"),
            "provider": member(meta, "provider"),
            "label": account.map(|a| json!(a.label)).unwrap_or(Value::Null),
            "email": account.and_then(|a| a.email.as_ref().map(|e| json!(e))).unwrap_or(Value::Null),
            "profile": account.and_then(|a| a.profile.as_ref().map(|p| json!(p))).unwrap_or(Value::Null),
            "hidden": account.map(|a| a.hidden).unwrap_or(false),
            "window": member(window, "key"),
            "windowLabel": member(window, "label"),
            "pool": member(window, "poolLabel"),
            "kind": member(window, "kind"),
            "unit": member(window, "unit"),
            "points": points,
        }));
    }
    json!({
        "version": 1,
        "generatedAt": aac_snapshot::js::iso_of(now).unwrap_or_default(),
        "resolution": resolution,
        "columns": ["start", "consumed", "lastUsed", "maxUsed", "coveredMinutes", "readings", "resets", "creditsSpent", "creditsAdded"],
        "series": series_out,
    })
}

#[test]
fn the_streamed_rollup_export_is_byte_identical_to_the_base_file() {
    let dir = tempfile::tempdir().expect("fixture store");
    let engine = aac_store::ops::open_engine(dir.path().to_str().expect("path"), None, None)
        .expect("open store");

    // Two identified accounts (one with an email, one without) plus a stored series whose
    // identity matches no live row: its label/email/profile must be null and hidden false.
    let id_a = "a".repeat(64);
    let id_b = "b".repeat(64);
    let id_orphan = "c".repeat(64);
    let rows = vec![
        row(
            "codex:alpha",
            "codex",
            Some("alpha@example.invalid"),
            "Alpha",
        ),
        row("claude:beta", "claude", None, "Beta"),
    ];
    // Hourly points across several hours and days, with resets and rising usage so the
    // rollups carry consumed/lastUsed/maxUsed/coveredMinutes/readings/resets.
    let points_a: Vec<(i64, f64)> = (0..60)
        .map(|i| (NOW - 50 * HOUR_MS + i * HOUR_MS, (i % 90) as f64))
        .collect();
    let points_b: Vec<(i64, f64)> = (0..40)
        .map(|i| (NOW - 30 * HOUR_MS + i * HOUR_MS, (i % 70) as f64))
        .collect();
    let points_orphan: Vec<(i64, f64)> = (0..20)
        .map(|i| (NOW - 10 * HOUR_MS + i * HOUR_MS, (i % 50) as f64))
        .collect();
    seed(&engine, &id_a, "codex", "codex:alpha", &points_a);
    seed(&engine, &id_b, "claude", "claude:beta", &points_b);
    seed(&engine, &id_orphan, "codex", "codex:orphan", &points_orphan);

    let projected = project_agent_usage(&rows, NOW, None, true);

    for (resolution, days) in [("hour", 90i64), ("day", 3650i64)] {
        let expected = format!(
            "{}\n",
            stringify(&reference_file(
                &engine, resolution, days, &rows, &projected, NOW
            ))
        );
        let mut written: Vec<(String, String)> = Vec::new();
        write_rollup_exports(&engine, &rows, &projected, NOW, |res, body| {
            written.push((res.to_owned(), format!("{body}\n")));
            Ok(())
        })
        .expect("write rollup exports");
        let got = written
            .iter()
            .find(|(res, _)| res == resolution)
            .map(|(_, body)| body.clone())
            .expect("resolution written");
        assert_eq!(
            got, expected,
            "{resolution} rollup bytes differ from the base file"
        );
    }
}

#[test]
fn an_empty_store_writes_the_empty_envelope() {
    let dir = tempfile::tempdir().expect("fixture store");
    let engine = aac_store::ops::open_engine(dir.path().to_str().expect("path"), None, None)
        .expect("open store");
    let rows: Vec<Value> = vec![];
    let projected = project_agent_usage(&rows, NOW, None, true);
    for (resolution, days) in [("hour", 90i64), ("day", 3650i64)] {
        let expected = format!(
            "{}\n",
            stringify(&reference_file(
                &engine, resolution, days, &rows, &projected, NOW
            ))
        );
        let mut written: Vec<(String, String)> = Vec::new();
        write_rollup_exports(&engine, &rows, &projected, NOW, |res, body| {
            written.push((res.to_owned(), format!("{body}\n")));
            Ok(())
        })
        .expect("write rollup exports");
        let got = written
            .iter()
            .find(|(res, _)| res == resolution)
            .map(|(_, b)| b.clone())
            .expect("written");
        assert_eq!(got, expected, "{resolution} empty envelope differs");
        assert!(got.contains("\"series\":[]"), "empty series array: {got}");
    }
}
