//! Fixture JSON only: every test builds its own `sqlite3 -json` text. No machine is
//! contacted and no real T3 database is read.

use aac_status::pins::{MachinePins, PinnedThread, merge_pins, parse_remote_pins};
use serde_json::{Value, json};

fn row(id: &str, title: &str, pinned: Value, pinned_type: &str, order: Value, order_type: &str) -> Value {
    // A top-level row: no lineage at all, so both lineage columns print null.
    json!({
        "thread_id": id,
        "title": title,
        "pinnedAt": pinned,
        "pinnedType": pinned_type,
        "pinOrderKey": order,
        "orderType": order_type,
        "rootType": "object",
        "lineageType": null,
        "parentType": null,
    })
}

fn pin(id: &str, title: &str, order: Value) -> PinnedThread {
    PinnedThread {
        id: id.to_string(),
        title: title.to_string(),
        pinned_at: json!("2026-11-08T10:00:00Z"),
        pin_order_key: order,
    }
}

fn machine(name: &str, pinned: Vec<PinnedThread>) -> MachinePins {
    MachinePins {
        name: name.to_string(),
        pinned,
    }
}

#[test]
fn one_machines_rows_parse_with_the_same_fields_as_the_local_read() {
    let text = format!(
        "[{},{}]",
        row(
            "t1",
            "Title One",
            json!("2026-11-08T10:00:00Z"),
            "text",
            json!("a1"),
            "text"
        ),
        row(
            "t2",
            "Title Two",
            json!(1_700_000_000_012_u64),
            "integer",
            json!(7),
            "integer"
        ),
    );
    let rows = parse_remote_pins("mac", &text).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "t1");
    assert_eq!(rows[0].title, "Title One");
    assert_eq!(rows[0].pinned_at, json!("2026-11-08T10:00:00Z"));
    assert_eq!(rows[0].pin_order_key, json!("a1"));
    assert_eq!(rows[1].pinned_at, json!(1_700_000_000_012_u64));
    assert_eq!(rows[1].pin_order_key, json!(7));
}

#[test]
fn sqlite_wraps_long_output_across_lines_and_empty_output_means_no_pins() {
    // sqlite3 -json prints one row object per line inside the array; both parse the same.
    let wrapped = "[{\"thread_id\":\"a\",\"title\":\"A\",\"pinnedAt\":\"x\",\"pinnedType\":\"text\",\"pinOrderKey\":\"k\",\"orderType\":\"text\",\"rootType\":\"object\",\"lineageType\":null,\"parentType\":null},\n{\"thread_id\":\"b\",\"title\":\"B\",\"pinnedAt\":\"x\",\"pinnedType\":\"text\",\"pinOrderKey\":\"k\",\"orderType\":\"text\",\"rootType\":\"object\",\"lineageType\":null,\"parentType\":null}]";
    assert_eq!(parse_remote_pins("nas1", wrapped).unwrap().len(), 2);
    // Zero rows print nothing at all; a blank answer is an empty list, not a failure.
    assert!(parse_remote_pins("nas1", "").unwrap().is_empty());
    assert!(parse_remote_pins("windows", "  \n").unwrap().is_empty());
    assert!(parse_remote_pins("mac", "[]").unwrap().is_empty());
}

#[test]
fn a_changed_shape_is_refused_never_guessed() {
    // Not JSON at all (an error sqlite printed, a changed CLI).
    assert_eq!(
        parse_remote_pins("mac", "Runtime error: database is locked")
            .unwrap_err()
            .code,
        "shape"
    );
    // A row with a column renamed or missing.
    let renamed = json!([{ "thread_id": "t", "title": "T", "pinnedAt": "x", "pinnedType": "text",
        "pinOrderKey": "k", "orderType": "text", "payloadRoot": "object" }])
    .to_string();
    assert_eq!(parse_remote_pins("mac", &renamed).unwrap_err().code, "shape");
    // An extra column the query does not select.
    let extra = json!([{ "thread_id": "t", "title": "T", "pinnedAt": "x", "pinnedType": "text",
        "pinOrderKey": "k", "orderType": "text", "rootType": "object", "surprise": 1 }])
    .to_string();
    assert_eq!(parse_remote_pins("mac", &extra).unwrap_err().code, "shape");
    // A payload whose root is not an object.
    let mut bad = row("t", "T", json!("x"), "text", json!("k"), "text");
    bad["rootType"] = json!("array");
    assert_eq!(parse_remote_pins("mac", &format!("[{bad}]")).unwrap_err().code, "shape");
    // A value whose declared type does not match it.
    let mut lied = row("t", "T", json!("x"), "integer", json!("k"), "text");
    lied["pinnedType"] = json!("integer");
    assert_eq!(
        parse_remote_pins("mac", &format!("[{lied}]")).unwrap_err().code,
        "shape"
    );
    let mut real_mismatch = row("t", "T", json!(1.5), "real", json!("k"), "text");
    real_mismatch["pinnedType"] = json!("integer");
    assert_eq!(
        parse_remote_pins("mac", &format!("[{real_mismatch}]"))
            .unwrap_err()
            .code,
        "shape"
    );
    // A null pin value or order key cannot happen under the query's WHERE; if it does, shape.
    let mut nulls = row("t", "T", json!("x"), "text", json!("k"), "text");
    nulls["pinOrderKey"] = Value::Null;
    assert_eq!(
        parse_remote_pins("mac", &format!("[{nulls}]")).unwrap_err().code,
        "shape"
    );
}

#[test]
fn lineage_shape_facts_decide_top_level_and_wrong_shapes_are_refused() {
    // The query carries the lineage's own json_type facts; the parser applies the same rule
    // as the local read: absent, null, or an object whose parent is absent or null is
    // top-level, an object with a thread-id parent is a child, and every other shape is a
    // changed shape for that machine (Astra M1), never a silently included or excluded row.
    let mut child = row("child", "Child", json!("x"), "text", json!("k"), "text");
    child["lineageType"] = json!("object");
    child["parentType"] = json!("text");
    let mut object_null = row(
        "object-null",
        "Object null parent",
        json!("x"),
        "text",
        json!("k"),
        "text",
    );
    object_null["lineageType"] = json!("object");
    object_null["parentType"] = json!("null");
    let bare = row("bare", "No lineage", json!("x"), "text", json!("k"), "text");
    let rows = parse_remote_pins("mac", &format!("[{child},{object_null},{bare}]")).unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["object-null", "bare"]);

    // A `lineage` that is JSON null prints "null" in its type column: top-level too.
    let mut null_lineage = row("null-lineage", "Null lineage", json!("x"), "text", json!("k"), "text");
    null_lineage["lineageType"] = json!("null");
    assert_eq!(parse_remote_pins("mac", &format!("[{null_lineage}]")).unwrap().len(), 1);

    // Any other lineage container, or an object whose parent is not absent/null/text, is a
    // shape refusal.
    for (lineage, parent) in [
        (json!("array"), json!(null)),
        (json!("text"), json!(null)),
        (json!("integer"), json!(null)),
        (json!("true"), json!(null)),
        (json!("object"), json!("integer")),
        (json!("object"), json!("object")),
        (json!("object"), json!("array")),
        (json!("object"), json!("false")),
    ] {
        let mut bad = row("drift", "Drift", json!("x"), "text", json!("k"), "text");
        bad["lineageType"] = lineage.clone();
        bad["parentType"] = parent.clone();
        assert_eq!(
            parse_remote_pins("mac", &format!("[{bad}]")).unwrap_err().code,
            "shape",
            "lineageType {lineage} parentType {parent} must be refused"
        );
    }
}

#[test]
fn only_the_remote_machines_are_accepted() {
    assert_eq!(parse_remote_pins("vm", "[]").unwrap_err().code, "invalid");
    assert_eq!(parse_remote_pins("laptop", "[]").unwrap_err().code, "invalid");
}

#[test]
fn machines_merge_ordered_by_pin_order_key_then_machine() {
    let merged = merge_pins(&[
        machine(
            "vm",
            vec![pin("vm-b", "VM B", json!("b")), pin("vm-a", "VM A", json!("a"))],
        ),
        machine(
            "mac",
            vec![
                pin("mac-late", "Mac late", json!("z")),
                pin("mac-a2", "Mac a2", json!("a2")),
            ],
        ),
    ]);
    let ids: Vec<&str> = merged.iter().map(|pin| pin.id.as_str()).collect();
    assert_eq!(ids, ["vm-a", "mac-a2", "vm-b", "mac-late"]);
    assert_eq!(merged[0].machine, "vm");
    assert_eq!(merged[1].machine, "mac");
}

#[test]
fn numbers_sort_before_text_as_sqlite_orders_them() {
    let merged = merge_pins(&[
        machine("mac", vec![pin("text-key", "Text key", json!("2026-11-08"))]),
        machine(
            "vm",
            vec![pin("num-10", "Ten", json!(10)), pin("num-2", "Two", json!(2))],
        ),
    ]);
    let ids: Vec<&str> = merged.iter().map(|pin| pin.id.as_str()).collect();
    assert_eq!(ids, ["num-2", "num-10", "text-key"]);
}

#[test]
fn equal_keys_break_the_tie_by_machine_then_id() {
    let merged = merge_pins(&[
        machine(
            "nas1",
            vec![pin("n-b", "Nas1 B", json!("k")), pin("n-a", "Nas1 A", json!("k"))],
        ),
        machine("mac", vec![pin("m-a", "Mac A", json!("k"))]),
    ]);
    let ids: Vec<&str> = merged.iter().map(|pin| pin.id.as_str()).collect();
    assert_eq!(ids, ["m-a", "n-a", "n-b"]);
}

#[test]
fn a_thread_pinned_on_two_machines_appears_once_from_the_first_machine() {
    let merged = merge_pins(&[
        machine("windows", vec![pin("shared", "Windows copy", json!("m"))]),
        machine(
            "mac",
            vec![
                pin("shared", "Mac copy", json!("a")),
                pin("own", "Mac only", json!("z")),
            ],
        ),
    ]);
    let ids: Vec<&str> = merged.iter().map(|pin| pin.id.as_str()).collect();
    assert_eq!(ids, ["shared", "own"]);
    // The Mac's key sorts first, so the Mac's copy is the one kept.
    assert_eq!(merged[0].machine, "mac");
    assert_eq!(merged[0].title, "Mac copy");
}

#[test]
fn empty_machines_merge_to_an_empty_list() {
    assert!(merge_pins(&[]).is_empty());
    assert!(merge_pins(&[machine("vm", vec![]), machine("mac", vec![])]).is_empty());
}
