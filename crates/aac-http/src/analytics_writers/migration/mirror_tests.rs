#![cfg(test)]
#![allow(clippy::unwrap_used)]

use super::*;
use aac_store::collect;
use serde_json::json;

const MIRROR: &str = "legacy/cache/analytics-remote-v1/odroid.json";

fn scan(home: &Path, project: &str, at: i64, session: &str, usage: (u64, u64)) -> Value {
    let folder = home.join(".claude/projects").join(project);
    fs::create_dir_all(&folder).unwrap();
    let event = json!({
        "type":"assistant", "timestamp":aac_num::ecma::to_iso_string(at).unwrap(),
        "sessionId":session,
        "message":{"id":format!("fixture-{at}"),"model":"claude-opus-4-5",
            "usage":{"input_tokens":usage.0,"output_tokens":usage.1}}
    });
    use std::io::Write;
    writeln!(
        fs::File::options()
            .create(true)
            .append(true)
            .open(folder.join(format!("{session}.jsonl")))
            .unwrap(),
        "{event}"
    )
    .unwrap();
    let request =
        aac_usage::protocol::parse_request(br#"{"kinds":["claude"],"cwd":false,"minDateMs":0}"#)
            .unwrap();
    let answer = aac_usage::protocol::scan(&request, home, &aac_usage::discover::Env::default());
    json!({
        "version":8, "lastScanAt":aac_num::ecma::to_iso_string(at).unwrap(),
        "fingerprints":{"claude": answer["kinds"]["claude"]["fingerprints"]},
        "rows":answer["rows"], "srows":answer["srows"],
    })
}

fn import(migration: &Migration<'_>, host: &str, cache: &Value, valued: bool) {
    import_tool(migration, host, "claude", cache, valued);
}

fn import_tool(migration: &Migration<'_>, host: &str, tool: &str, cache: &Value, valued: bool) {
    let only = |key: &str| {
        cache[key]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["k"] == tool)
            .cloned()
            .collect::<Vec<_>>()
    };
    let worker = collect::worker_from_scan(
        &only("rows"),
        &only("srows"),
        tool,
        host,
        migration.now,
        &migration.pricing,
    );
    let mut report = migration.rows_report(host, tool, &[worker]);
    if !valued {
        // Wave 84 can save a session without a price. A later raw mirror can then
        // win the session index's same-token price tie.
        for session in report["sessions"].as_array_mut().unwrap() {
            for part in session["parts"].as_array_mut().unwrap() {
                for cost in ["costUsd", "loggedCostUsd", "fallbackCostUsd"] {
                    part[cost] = Value::Null;
                }
            }
        }
    }
    super::super::history::commit_report(migration.store, report).unwrap();
}

fn stage(root: &Path, host: &str, cache: &Value) -> PathBuf {
    let path = root.join(format!("cache/analytics-remote-v1/{host}.json"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(cache).unwrap()).unwrap();
    path
}

fn hour(now: i64) -> i64 {
    (now - 6 * 86_400_000).div_euclid(86_400_000) * 86_400_000 + 13 * 3_600_000
}

fn totals(store: &Engine) -> Vec<(String, u64, u64)> {
    activity::dump_cells(store)
        .unwrap()
        .into_iter()
        .map(|(host, _, cell)| (host, cell.i, cell.o))
        .collect()
}

fn mark_previous_migration(store: &Engine, marker: &str) {
    store
        .set_meta(
            marker,
            &activity::recovery_epoch(store).unwrap().to_string(),
        )
        .unwrap();
}

fn continued_session(repair: bool) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let dir = root.join("store");
    let open = || aac_store::ops::open_engine(&dir.to_string_lossy(), None, None).unwrap();
    let now = aac_store::engine::now_ms() as i64;
    let at = hour(now);
    let original = scan(&root.join("original"), "project", at, "resumed", (70, 30));
    scan(&root.join("continued"), "project", at, "resumed", (70, 30));
    let later = scan(
        &root.join("continued"),
        "project",
        at + 3 * 3_600_000,
        "resumed",
        (130, 40),
    );
    for cache in [&original, &later] {
        assert!(
            cache["fingerprints"]["claude"]
                .as_object()
                .unwrap()
                .values()
                .all(|print| print["claudeMirror"] == false)
        );
    }
    let store = open();
    let current = migration(&store, root, now);
    if repair {
        import(&current, "nas1", &original, true);
        import(&current, "odroid", &later, true);
        mark_previous_migration(&store, "analytics_files_migrated_v2");
        assert!(totals(&store).contains(&("odroid".into(), 130, 40)));
    }
    stage(root, "nas1", &original);
    stage(root, "odroid", &later);
    let check = |store: &Engine| {
        let rows = totals(store);
        assert!(rows.contains(&("nas1".into(), 70, 30)), "{rows:?}");
        assert!(rows.contains(&("odroid".into(), 130, 40)), "{rows:?}");
    };
    assert!(current.run().unwrap());
    check(&store);
    store.close().unwrap();
    drop(store);
    check(&open());
}

#[test]
fn resumed_originals_keep_both_computers_in_fresh_migration() {
    continued_session(false);
}

#[test]
fn resumed_originals_keep_both_computers_in_repair() {
    continued_session(true);
}

fn cache_row(tool: &str, at: i64, file: &str, session: &str, input: u64) -> (Value, Value) {
    let time = aac_num::ecma::to_iso_string(at).unwrap();
    let route = match tool {
        "codex" => "openai",
        "claude" => "anthropic",
        _ => tool,
    };
    let mut row = json!({
        "f":file, "k":tool, "p":route, "m":"fixture-model",
        "i":input, "o":10, "cw":0, "cr":0, "n":1,
    });
    let mut aggregate = row.clone();
    row["h"] = json!(time[..13].replace('T', " ") + ":00");
    aggregate["s"] = json!(session);
    aggregate["a"] = json!(at);
    aggregate["z"] = json!(at);
    (row, aggregate)
}

fn legacy_cache(version: u8, at: i64, entries: Vec<(Value, Value)>) -> Value {
    let (rows, srows): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
    let mut prints = json!({});
    for row in &rows {
        let kind = row["k"].as_str().unwrap();
        let file = row["f"].as_str().unwrap();
        if prints[kind].is_null() {
            prints[kind] = json!({});
        }
        prints[kind][file] = json!({"mtimeMs":at, "size":40});
    }
    json!({"version":version, "lastScanAt":aac_num::ecma::to_iso_string(at).unwrap(),
        "fingerprints":prints,"rows":rows,"srows":srows})
}

fn scan_codex_copies(home: &Path, at: i64) -> Value {
    let time = aac_num::ecma::to_iso_string(at).unwrap();
    let id = "aaaaaaaa-1111-2222-3333-444444444444";
    let events = [
        json!({"timestamp":time,"type":"session_meta","payload":{"id":id,"model_provider":"openai"}}),
        json!({"timestamp":time,"type":"turn_context","payload":{"model":"fixture-model"}}),
        json!({"timestamp":time,"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10},
            "last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10}
        }}}),
        json!({"timestamp":time,"type":"event_msg","payload":{"type":"task_complete"}}),
    ];
    let filename = format!("rollout-{}T13-00-00-{id}.jsonl", &time[..10]);
    for (folder, entries) in [
        ("sessions", &events[..]),
        ("archived_sessions", &events[..3]),
    ] {
        let dir = home.join(".codex").join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(&filename),
            entries
                .iter()
                .map(|event| format!("{event}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }
    let ask =
        aac_usage::protocol::parse_request(br#"{"kinds":["codex"],"cwd":false,"minDateMs":0}"#)
            .unwrap();
    let answer = aac_usage::protocol::scan(&ask, home, &aac_usage::discover::Env::default());
    assert_eq!(answer["rows"].as_array().unwrap().len(), 1, "{answer}");
    assert_eq!(answer["rows"][0]["n"], 1, "{answer}");
    answer
}

fn obsolete_cache(repair: bool) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let store =
        aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None).unwrap();
    let now = aac_store::engine::now_ms() as i64;
    let at = hour(now);
    let old = legacy_cache(
        6,
        at,
        vec![
            cache_row("codex", at, "rollout-original", "1111111111111111", 100),
            cache_row("codex", at, "rollout-short-copy", "2222222222222222", 100),
            cache_row("muse", at, "old-path", "3333333333333333", 33),
            cache_row("claude", at, "valid-claude", "4444444444444444", 70),
        ],
    );
    let current = migration(&store, root, now);
    let path = stage(root, "nas1", &old);
    if repair {
        for tool in ["codex", "muse", "claude"] {
            import_tool(&current, "nas1", tool, &old, true);
        }
        let codex: Vec<_> = activity::dump_cells(&store)
            .unwrap()
            .into_iter()
            .filter(|(_, tool, _)| tool == "codex")
            .collect();
        assert_eq!(codex.iter().map(|(_, _, cell)| cell.i).sum::<u64>(), 200);
        assert_eq!(codex.iter().map(|(_, _, cell)| cell.o).sum::<u64>(), 20);
        mark_previous_migration(&store, "analytics_files_migrated_v2");
        mark_previous_migration(&store, "analytics_files_migrated_v3");
        analytics_cache::archive(
            &store,
            &[(
                "legacy/cache/analytics-remote-v1/nas1.json".into(),
                serde_json::to_vec(&old).unwrap(),
            )],
        )
        .unwrap();
        analytics_cache::put(
            &store,
            "live/scanner/nas1",
            &serde_json::to_vec(&json!({
                "fingerprints":old["fingerprints"], "rows":old["rows"], "srows":old["srows"]
            }))
            .unwrap(),
        )
        .unwrap();
    }
    activity::close_days(&store, now as u64).unwrap();
    assert!(current.run().unwrap());
    assert_eq!(totals(&store), vec![("nas1".into(), 70, 10)]);
    let live = super::super::scan_state::Cache::load(&store, "nas1").unwrap();
    assert_eq!(live.fingerprints.keys().collect::<Vec<_>>(), ["claude"]);
    assert_eq!(live.rows.len(), 1);
    assert_eq!(live.srows.len(), 1);
    assert!(!path.exists());
    assert_eq!(
        analytics_cache::archived(&store, "legacy/cache/analytics-remote-v1/nas1.json").unwrap(),
        Some(serde_json::to_vec(&old).unwrap())
    );
    let rebuilt = scan_codex_copies(&root.join("traces"), at);
    import_tool(&current, "nas1", "codex", &rebuilt, true);
    assert!(totals(&store).contains(&("nas1".into(), 100, 10)));
    let range = aac_store::ops::call(
        &store,
        "activityRange",
        &json!({"fromMs":at,"toMs":at+3_600_000}).to_string(),
    )
    .unwrap();
    assert_eq!(range["sessions"]["total"], 2, "{range}");
}

#[test]
fn pre_v7_paths_never_seed_historical_or_live_rows() {
    obsolete_cache(false);
}

#[test]
fn pre_v7_paths_are_repaired_even_after_a_previous_migration() {
    obsolete_cache(true);
}

#[test]
fn pre_v6_claude_rebuilds_and_v7_paths_remain_usable() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let store =
        aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None).unwrap();
    let at = hour(aac_store::engine::now_ms() as i64);
    for (host, version) in [("nas1", 5), ("odroid", 7)] {
        let cache = legacy_cache(
            version,
            at,
            vec![
                cache_row("claude", at, "claude", "1111111111111111", 70),
                cache_row("codex", at, "codex", "2222222222222222", 100),
                cache_row("muse", at, "muse", "3333333333333333", 80),
            ],
        );
        analytics_cache::archive(
            &store,
            &[(
                format!("legacy/cache/analytics-remote-v1/{host}.json"),
                serde_json::to_vec(&cache).unwrap(),
            )],
        )
        .unwrap();
        let cache = super::super::scan_state::Cache::load(&store, host).unwrap();
        assert_eq!(cache.rows.len(), if version == 5 { 0 } else { 3 });
        assert_eq!(cache.srows.len(), cache.rows.len());
        assert_eq!(cache.fingerprints.len(), cache.rows.len());
    }
}

fn partial_remote(bad: impl FnOnce(&Path)) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let store =
        aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None).unwrap();
    let now = aac_store::engine::now_ms() as i64;
    let at = hour(now);
    let codex = legacy_cache(
        8,
        at,
        vec![cache_row("codex", at, "file", "1111111111111111", 100)],
    );
    let independent = stage(root, "nas1", &codex);
    let mirror = scan(
        &root.join("mirror"),
        "ssh-12345678-1234-1234-1234-123456789abc",
        at,
        "shared",
        (70, 30),
    );
    let waiting = stage(root, "mac", &mirror);
    let corrupt = independent.with_file_name("odroid.json");
    bad(&corrupt);
    let current = migration(&store, root, now);
    assert!(!current.run().unwrap());
    assert_eq!(totals(&store), vec![("nas1".into(), 100, 10)]);
    assert!(!independent.exists());
    assert!(waiting.exists() && corrupt.exists());
    let health: Value = serde_json::from_str(&store.get_meta(FAILURES).unwrap().unwrap()).unwrap();
    assert_eq!(health[MIRROR]["attempts"], 1);
    assert!(!migration(&store, root, now + 1).run().unwrap());
    assert_eq!(
        store.get_meta(FAILURES).unwrap().unwrap(),
        health.to_string()
    );
    let fixed = corrupt.with_file_name("odroid.ready");
    fs::write(
        &fixed,
        serde_json::to_vec(&legacy_cache(8, at, vec![])).unwrap(),
    )
    .unwrap();
    fs::rename(fixed, corrupt).unwrap();
    assert!(migration(&store, root, now + RETRY_MS).run().unwrap());
    assert_eq!(
        totals(&store),
        vec![("mac".into(), 70, 30), ("nas1".into(), 100, 10)]
    );
    assert!(!waiting.exists());
}

#[test]
fn one_damaged_host_delays_only_mirror_history_until_its_retry() {
    partial_remote(|path| fs::write(path, b"{broken").unwrap());
}

#[cfg(unix)]
#[test]
fn one_unreadable_host_delays_only_mirror_history_until_its_retry() {
    use std::os::unix::fs::PermissionsExt;
    partial_remote(|path| {
        fs::write(path, b"{}").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0)).unwrap();
    });
}

fn migration<'a>(store: &'a Engine, root: &'a Path, now: i64) -> Migration<'a> {
    Migration {
        store,
        root,
        local: Some("nas1"),
        now,
        pricing: super::super::ResolverPricing::new(root),
    }
}

fn run_fixture(repair: bool) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let dir = root.join("store");
    let open = || aac_store::ops::open_engine(&dir.to_string_lossy(), None, None).unwrap();
    let now = aac_store::engine::now_ms() as i64;
    let at = (now - 6 * 86_400_000).div_euclid(3_600_000) * 3_600_000;
    let original = scan(&root.join("original"), "original", at, "shared", (70, 30));
    let mirror_home = root.join("mirror");
    scan(&mirror_home, "unrelated", at, "other", (13, 7));
    let mirror = scan(
        &mirror_home,
        "ssh-12345678-1234-1234-1234-123456789abc",
        at,
        "shared",
        (70, 30),
    );
    assert_eq!(mirror["rows"].as_array().unwrap().len(), 2);
    let path = root.join("cache/analytics-remote-v1/odroid.json");
    let store = open();
    import(&migration(&store, root, now), "nas1", &original, false);
    assert_eq!(
        activity::close_days(&store, now as u64).unwrap()["closed"],
        json!([at / 86_400_000])
    );
    let staged = vec![(MIRROR.to_owned(), serde_json::to_vec(&mirror).unwrap())];
    if repair {
        import(&migration(&store, root, now), "odroid", &mirror, true);
        let before = aac_store::ops::call(
            &store,
            "activityRange",
            &json!({"fromMs":at,"toMs":at+3_600_000}).to_string(),
        )
        .unwrap();
        assert_eq!(
            before["sessions"]["sample"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["sessionKey"] == original["srows"][0]["s"])
                .unwrap()["computer"],
            "odroid"
        );
        let epoch = activity::recovery_epoch(&store).unwrap();
        store
            .set_meta("analytics_files_migrated_v2", &epoch.to_string())
            .unwrap();
    } else {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &staged[0].1).unwrap();
    }
    analytics_cache::archive(&store, &staged).unwrap();
    store.close().unwrap();
    drop(store);

    let check = |store: &Engine| {
        let cells = activity::dump_cells(store).unwrap();
        let observed: Vec<_> = cells
            .iter()
            .map(|(host, _, cell)| (host.as_str(), cell.i, cell.o))
            .collect();
        assert_eq!(observed, vec![("nas1", 70, 30), ("odroid", 13, 7)]);
        let range = aac_store::ops::call(
            store,
            "activityRange",
            &json!({"fromMs":at,"toMs":at+3_600_000}).to_string(),
        )
        .unwrap();
        assert_eq!(range["sessions"]["total"], 2);
        assert_eq!(
            range["sessions"]["sample"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["sessionKey"] == original["srows"][0]["s"])
                .unwrap()["computer"],
            "nas1",
        );
    };
    let store = open();
    assert!(migration(&store, root, now).run().unwrap());
    assert!(!path.exists());
    check(&store);
    store.close().unwrap();
    drop(store);
    let store = open();
    assert!(migration(&store, root, now).run().unwrap());
    check(&store);
}

#[test]
fn settled_original_survives_mirror_migration_after_staging_restart() {
    run_fixture(false);
}

#[test]
fn repair_removes_the_imported_mirror_and_keeps_unrelated_usage() {
    run_fixture(true);
}

#[test]
fn all_mirror_caches_are_staged_before_the_legacy_owner_is_applied() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let store =
        aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None).unwrap();
    let now = aac_store::engine::now_ms() as i64;
    let mirror = scan(
        &root.join("mirror"),
        "ssh-12345678-1234-1234-1234-123456789abc",
        now - 6 * 86_400_000,
        "shared",
        (70, 30),
    );
    let print = mirror["fingerprints"]["claude"].as_object().unwrap();
    let (name, true) =
        super::super::claude_mirrors::identity(print.values().next().unwrap()).unwrap()
    else {
        panic!("expected a mirror fingerprint");
    };
    let cache = root.join("cache/analytics-remote-v1");
    fs::create_dir_all(&cache).unwrap();
    let bytes = serde_json::to_vec(&mirror).unwrap();
    for host in ["mac", "odroid"] {
        fs::write(cache.join(format!("{host}.json")), &bytes).unwrap();
    }
    let old_map = json!({"version":1, "owners":{(name): "odroid"}});
    fs::write(cache.join("claude-mirror-owners.json"), old_map.to_string()).unwrap();

    assert!(migration(&store, root, now).run().unwrap());
    let cells = activity::dump_cells(&store).unwrap();
    let [(host, _, cell)] = cells.as_slice() else {
        panic!("expected one mirror host cell");
    };
    assert_eq!((host.as_str(), cell.i, cell.o), ("odroid", 70, 30));
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
}
