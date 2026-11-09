//! Fixture databases only: every test builds its own SQLite file under a temporary folder.
//! The real `~/.t3` is never opened.

use aac_status::pins::{read_pinned_threads, resolve_db_path};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The projection table as T3 writes it (column set checked 2026-11-08).
const SCHEMA: &str = "CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT NOT NULL,
    default_provider TEXT NOT NULL,
    runtime_mode TEXT NOT NULL,
    interaction_mode TEXT NOT NULL,
    active_provider_thread_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    deleted_at TEXT,
    payload_json TEXT NOT NULL,
    provider_instance_id TEXT
)";

fn fixture(dir: &Path) -> (PathBuf, Connection) {
    let file = dir.join("statev2.sqlite");
    let conn = Connection::open(&file).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    (file, conn)
}

fn payload(id: &str, title: &str, pinned_at: Value, pin_order: Value, parent: Value) -> String {
    json!({
        "id": id,
        "title": title,
        "pinnedAt": pinned_at,
        "pinOrderKey": pin_order,
        "lineage": { "parentThreadId": parent },
        "branch": "main",
        "unrelated": [1, 2, 3],
    })
    .to_string()
}

fn insert(conn: &Connection, id: &str, title: &str, archived: Option<&str>, deleted: Option<&str>, payload_json: &str) {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_threads \
         (thread_id, project_id, title, default_provider, runtime_mode, interaction_mode, \
          created_at, updated_at, archived_at, deleted_at, payload_json) \
         VALUES (?1, 'proj', ?2, 'claude', 'auto', 'default', '2026-11-01T00:00:00Z', \
                 '2026-11-02T00:00:00Z', ?3, ?4, ?5)",
        params![id, title, archived, deleted, payload_json],
    )
    .unwrap();
}

fn ids(rows: &[aac_status::pins::PinnedThread]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

#[test]
fn only_pinned_top_level_threads_are_returned() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    let at = json!("2026-11-08T10:00:00Z");
    insert(
        &conn,
        "top-null",
        "Top null parent",
        None,
        None,
        &payload("top-null", "Top null parent", at.clone(), json!("a1"), Value::Null),
    );
    // No lineage at all counts as top-level too.
    insert(
        &conn,
        "top-bare",
        "Top no lineage",
        None,
        None,
        &json!({"id": "top-bare", "title": "Top no lineage", "pinnedAt": at, "pinOrderKey": "a2"}).to_string(),
    );
    insert(
        &conn,
        "child",
        "Child thread",
        None,
        None,
        &payload(
            "child",
            "Child thread",
            json!("2026-11-08T10:00:00Z"),
            json!("a0"),
            json!("top-null"),
        ),
    );
    insert(
        &conn,
        "unpinned",
        "Not pinned",
        None,
        None,
        &payload("unpinned", "Not pinned", Value::Null, Value::Null, Value::Null),
    );
    insert(
        &conn,
        "no-field",
        "No pin field",
        None,
        None,
        &json!({"id": "no-field", "title": "No pin field"}).to_string(),
    );
    drop(conn);

    let rows = read_pinned_threads(&file).unwrap();
    assert_eq!(ids(&rows), ["top-null", "top-bare"]);
    assert_eq!(rows[0].title, "Top null parent");
    assert_eq!(rows[0].pinned_at, json!("2026-11-08T10:00:00Z"));
    assert_eq!(rows[0].pin_order_key, json!("a1"));
}

#[test]
fn archived_and_deleted_threads_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    let at = json!("2026-11-08T10:00:00Z");
    insert(
        &conn,
        "live",
        "Live",
        None,
        None,
        &payload("live", "Live", at.clone(), json!("a1"), Value::Null),
    );
    insert(
        &conn,
        "arch",
        "Archived",
        Some("2026-11-07T00:00:00Z"),
        None,
        &payload("arch", "Archived", at.clone(), json!("a2"), Value::Null),
    );
    insert(
        &conn,
        "gone",
        "Deleted",
        None,
        Some("2026-11-07T00:00:00Z"),
        &payload("gone", "Deleted", at, json!("a3"), Value::Null),
    );
    drop(conn);

    assert_eq!(ids(&read_pinned_threads(&file).unwrap()), ["live"]);
}

#[test]
fn rows_come_back_in_pin_order() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    for (id, key) in [("third", "c"), ("first", "a"), ("second", "b")] {
        insert(
            &conn,
            id,
            id,
            None,
            None,
            &payload(id, id, json!("2026-11-08T10:00:00Z"), json!(key), Value::Null),
        );
    }
    drop(conn);
    assert_eq!(ids(&read_pinned_threads(&file).unwrap()), ["first", "second", "third"]);
}

#[test]
fn numeric_order_keys_sort_numerically() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    for (id, key) in [("ten", 10), ("two", 2), ("one", 1)] {
        insert(
            &conn,
            id,
            id,
            None,
            None,
            &payload(id, id, json!(1_700_000_000_000_i64), json!(key), Value::Null),
        );
    }
    drop(conn);
    let rows = read_pinned_threads(&file).unwrap();
    assert_eq!(ids(&rows), ["one", "two", "ten"]);
    assert_eq!(rows[0].pinned_at, json!(1_700_000_000_000_i64));
}

#[test]
fn a_missing_file_is_missing_not_an_error_to_guess_around() {
    let dir = tempfile::tempdir().unwrap();
    let error = read_pinned_threads(&dir.path().join("nope.sqlite")).unwrap_err();
    assert_eq!(error.code, "missing");
}

#[test]
fn a_wrong_typed_lineage_is_a_shape_refusal_never_a_top_level_pin() {
    // T3's `lineage` is an object with a `parentThreadId`, or absent. A pinned row whose
    // lineage is another container once read as top-level, because the parent extract cannot
    // walk into it (Astra M1); it is a changed shape, never a guessed pin.
    for wrong in [
        json!([{ "parentThreadId": "parent-fixture" }]),
        json!("parent-fixture"),
        json!(7),
        json!(true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (file, conn) = fixture(dir.path());
        insert(
            &conn,
            "drift",
            "Drift",
            None,
            None,
            &json!({
                "id": "drift", "title": "Drift",
                "pinnedAt": "2026-11-08T10:00:00Z", "pinOrderKey": "a",
                "lineage": wrong
            })
            .to_string(),
        );
        drop(conn);
        assert_eq!(
            read_pinned_threads(&file).unwrap_err().code,
            "shape",
            "lineage {wrong} must be refused"
        );
    }

    // An object whose parent is neither absent, null nor a thread-id string is a change too:
    // the row was once silently excluded, which is just as much a guess.
    for wrong in [json!(7), json!({}), json!([]), json!(false)] {
        let dir = tempfile::tempdir().unwrap();
        let (file, conn) = fixture(dir.path());
        insert(
            &conn,
            "drift",
            "Drift",
            None,
            None,
            &json!({
                "id": "drift", "title": "Drift",
                "pinnedAt": "2026-11-08T10:00:00Z", "pinOrderKey": "a",
                "lineage": { "parentThreadId": wrong }
            })
            .to_string(),
        );
        drop(conn);
        assert_eq!(
            read_pinned_threads(&file).unwrap_err().code,
            "shape",
            "parentThreadId {wrong} must be refused"
        );
    }
}

#[test]
fn a_local_read_past_its_budget_is_refused_never_silently_truncated() {
    // The remote route's caps (a fixed byte budget per read) as the local read's model
    // (Astra L1): past the budget the read is a fixed refusal, never a truncated list. The
    // sizes here are one past this reader's `MAX_PIN_BYTES` (1 MiB) and `MAX_PINNED_ROWS`
    // (1000), which the check in `read_pinned_threads` enforces.

    // One row whose title alone passes the byte budget (the review's 2 MiB title).
    let bytes_dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(bytes_dir.path());
    insert(
        &conn,
        "big",
        &"x".repeat(2 * 1024 * 1024),
        None,
        None,
        &json!({
            "id": "big", "pinnedAt": "2026-11-08T10:00:00Z", "pinOrderKey": "a",
            "lineage": null
        })
        .to_string(),
    );
    drop(conn);
    assert_eq!(read_pinned_threads(&file).unwrap_err().code, "limit");

    // More rows than the count budget, each tiny.
    let rows_dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(rows_dir.path());
    for index in 0..=1000_u32 {
        insert(
            &conn,
            &format!("row-{index}"),
            "Row",
            None,
            None,
            &json!({
                "id": format!("row-{index}"), "pinnedAt": "2026-11-08T10:00:00Z",
                "pinOrderKey": index, "lineage": null
            })
            .to_string(),
        );
    }
    drop(conn);
    assert_eq!(read_pinned_threads(&file).unwrap_err().code, "limit");
}

#[test]
fn a_changed_shape_is_reported_never_guessed() {
    let dir = tempfile::tempdir().unwrap();

    // No projection table at all.
    let no_table = dir.path().join("empty.sqlite");
    Connection::open(&no_table)
        .unwrap()
        .execute_batch("CREATE TABLE other (id TEXT)")
        .unwrap();
    assert_eq!(read_pinned_threads(&no_table).unwrap_err().code, "shape");

    // The table without a column this reader relies on.
    let no_column = dir.path().join("nocol.sqlite");
    Connection::open(&no_column)
        .unwrap()
        .execute_batch("CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT, title TEXT)")
        .unwrap();
    assert_eq!(read_pinned_threads(&no_column).unwrap_err().code, "shape");

    // A pinned row whose payload is not a JSON object.
    let (file, conn) = fixture(dir.path());
    insert(&conn, "odd", "Odd", None, None, "[1,2,3]");
    drop(conn);
    // Not selected (pinnedAt is null on an array payload), so give it a pin-shaped object
    // with a non-scalar pinnedAt instead and expect the same refusal.
    let (file2, conn2) = {
        let file2 = dir.path().join("second.sqlite");
        let conn2 = Connection::open(&file2).unwrap();
        conn2.execute_batch(SCHEMA).unwrap();
        (file2, conn2)
    };
    insert(
        &conn2,
        "odd",
        "Odd",
        None,
        None,
        &json!({"pinnedAt": {"at": 1}, "pinOrderKey": "a1"}).to_string(),
    );
    drop(conn2);
    assert_eq!(read_pinned_threads(&file).unwrap().len(), 0);
    assert_eq!(read_pinned_threads(&file2).unwrap_err().code, "shape");

    // A pinned row with no order key: T3 writes the pair together, so this is a new shape.
    let file3 = dir.path().join("third.sqlite");
    let conn3 = Connection::open(&file3).unwrap();
    conn3.execute_batch(SCHEMA).unwrap();
    insert(
        &conn3,
        "odd",
        "Odd",
        None,
        None,
        &json!({"pinnedAt": "2026-11-08T10:00:00Z"}).to_string(),
    );
    drop(conn3);
    assert_eq!(read_pinned_threads(&file3).unwrap_err().code, "shape");
}

#[test]
fn reads_work_while_a_writer_holds_a_wal_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    insert(
        &conn,
        "before",
        "Before",
        None,
        None,
        &payload(
            "before",
            "Before",
            json!("2026-11-08T10:00:00Z"),
            json!("a1"),
            Value::Null,
        ),
    );

    // A second connection (T3 writing) holds an uncommitted insert: readers still get the
    // last committed state, promptly and without an error.
    let writer = Connection::open(&file).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    insert(
        &writer,
        "during",
        "During",
        None,
        None,
        &payload(
            "during",
            "During",
            json!("2026-11-08T10:00:00Z"),
            json!("a2"),
            Value::Null,
        ),
    );
    assert_eq!(ids(&read_pinned_threads(&file).unwrap()), ["before"]);

    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(ids(&read_pinned_threads(&file).unwrap()), ["before", "during"]);
}

#[test]
fn reading_never_writes_to_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (file, conn) = fixture(dir.path());
    insert(
        &conn,
        "one",
        "One",
        None,
        None,
        &payload("one", "One", json!("2026-11-08T10:00:00Z"), json!("a1"), Value::Null),
    );
    drop(conn);
    let before = std::fs::read(&file).unwrap();
    assert_eq!(read_pinned_threads(&file).unwrap().len(), 1);
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(!dir.path().join("statev2.sqlite-wal").exists());
}

#[test]
fn db_path_resolution_prefers_explicit_then_env_then_home() {
    let home = Some(PathBuf::from("/fixture/home"));
    assert_eq!(
        resolve_db_path(
            Some(" /fixture/explicit.sqlite "),
            Some("/fixture/env.sqlite".into()),
            home.clone()
        ),
        Some(PathBuf::from("/fixture/explicit.sqlite"))
    );
    assert_eq!(
        resolve_db_path(Some("  "), Some("/fixture/env.sqlite".into()), home.clone()),
        Some(PathBuf::from("/fixture/env.sqlite"))
    );
    assert_eq!(
        resolve_db_path(None, None, home),
        Some(PathBuf::from("/fixture/home/.t3/userdata/statev2.sqlite"))
    );
    assert_eq!(resolve_db_path(None, None, None), None);
}
