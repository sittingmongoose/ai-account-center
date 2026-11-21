use super::*;
use aac_store::collect;
use serde_json::json;

const ORIGINAL: &str = "legacy/cache/analytics-remote-v1/nas1.json";
const MIRROR: &str = "legacy/cache/analytics-remote-v1/odroid.json";

fn scan(home: &Path, project: &str, at: i64, session: &str, usage: (u64, u64)) -> Value {
    let folder = home.join(".claude/projects").join(project);
    fs::create_dir_all(&folder).unwrap();
    let event = json!({
        "type":"assistant", "timestamp":aac_num::ecma::to_iso_string(at).unwrap(),
        "sessionId":session,
        "message":{"id":"fixture-message","model":"claude-opus-4-5",
            "usage":{"input_tokens":usage.0,"output_tokens":usage.1}}
    });
    fs::write(
        folder.join(format!("{session}.jsonl")),
        format!("{event}\n"),
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

fn import(migration: &Migration<'_>, host: &str, cache: &Value) {
    let worker = collect::worker_from_scan(
        cache["rows"].as_array().unwrap(),
        cache["srows"].as_array().unwrap(),
        "claude",
        host,
        migration.now,
        &migration.pricing,
    );
    migration.import_rows(host, "claude", &[worker]).unwrap();
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
    import(&migration(&store, root, now), "nas1", &original);
    assert_eq!(
        activity::close_days(&store, now as u64).unwrap()["closed"],
        json!([at / 86_400_000])
    );
    let mut staged = vec![(MIRROR.to_owned(), serde_json::to_vec(&mirror).unwrap())];
    if repair {
        import(&migration(&store, root, now), "odroid", &mirror);
        staged.push((ORIGINAL.to_owned(), serde_json::to_vec(&original).unwrap()));
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
