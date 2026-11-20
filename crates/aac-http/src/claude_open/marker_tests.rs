//! G13 marker protocol fixtures: the port of `tests/unit/claude-history/pending-marker.test.cjs`.
//! Every case runs on a disposable 0700 temp root; no real profile, app or credential is touched.
use super::markers::{
    MarkerRefused, PendingMarker, open_held, open_held_without_core, pending_marker_state,
};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const PROFILE: &str = "gmail";

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = fs::canonicalize(dir.path()).expect("canonical");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("chmod");
        // Keep the temp folder alive for the test's lifetime.
        std::mem::forget(dir);
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn marker_root(&self) -> PathBuf {
        self.0.join("claude-history-pending")
    }
    fn marker_file(&self) -> PathBuf {
        let mut names: Vec<_> = fs::read_dir(self.marker_root())
            .expect("marker dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        names.sort();
        names.into_iter().next().expect("one marker")
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn receipt(changes: Value) -> Value {
    let mut base =
        json!({"status": "created_metadata", "createdCount": 1, "writerQuiescent": true});
    if let Some(map) = changes.as_object() {
        for (key, value) in map {
            base[key] = value.clone();
        }
    }
    base
}

fn held(root: &Root) -> bool {
    pending_marker_state(root.path(), PROFILE, "windows").held
}

fn write_pending(root: &Root, profile: &str, platform: &str, nonce_digit: char) -> PathBuf {
    let marker_root = root.marker_root();
    fs::create_dir_all(&marker_root).expect("mkdir");
    fs::set_permissions(&marker_root, fs::Permissions::from_mode(0o700)).expect("chmod");
    let nonce: String = std::iter::repeat_n(nonce_digit, 32).collect();
    let record = json!({
        "version": 1, "profileId": profile, "targetPlatform": platform, "nonce": nonce,
        "state": "pending", "createdAt": "2026-10-02T00:00:00.000Z"
    });
    let file = marker_root.join(format!("{profile}-{platform}-{nonce}.json"));
    fs::write(&file, serde_json::to_vec(&record).expect("json")).expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    file
}

#[test]
fn absent_marker_does_not_create_storage_or_hold() {
    let root = Root::new();
    assert!(!held(&root));
    assert!(fs::read_dir(root.path()).expect("root").next().is_none());
}

#[test]
fn pending_marker_is_private_and_survives_restart() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.release();
    assert_eq!(
        fs::metadata(root.marker_root())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.marker_file())
            .expect("file")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(held(&root));
}

#[test]
fn lost_append_retains_hold_and_blocks_a_second_writer() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.release();
    assert!(held(&root));
    assert_eq!(
        PendingMarker::arm(root.path(), PROFILE, "windows").expect_err("arm must refuse"),
        MarkerRefused::AppendPending
    );
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 1);
}

#[test]
fn trusted_receipt_finishes_only_the_owned_marker() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    marker.release();
    assert!(!held(&root));
    let row: Value =
        serde_json::from_slice(&fs::read(root.marker_file()).expect("read")).expect("json");
    assert_eq!(row["state"], json!("finished"));
}

#[test]
fn duplicate_json_state_cannot_disguise_a_marker() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    marker.release();
    let file = root.marker_file();
    let original = fs::read_to_string(&file).expect("read");
    fs::write(&file, format!("{{\"state\":\"pending\",{}", &original[1..])).expect("write");
    assert!(held(&root));
}

#[test]
fn terminal_refusal_finishes_without_zero_write_claims() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker
        .finish(&receipt(
            json!({"status": "refused", "createdCount": 0, "recoveryRequired": true}),
        ))
        .expect("finish");
    marker.release();
    assert!(!held(&root));
    let row: Value =
        serde_json::from_slice(&fs::read(root.marker_file()).expect("read")).expect("json");
    assert!(row.get("zeroWrites").is_none());
    assert!(row.get("rolledBack").is_none());
}

#[test]
fn malformed_receipts_retain_the_durable_hold() {
    for bad in [
        json!({}),
        receipt(json!({"writerQuiescent": false})),
        receipt(json!({"createdCount": -1})),
        receipt(json!({"status": "unknown"})),
    ] {
        let root = Root::new();
        let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
        assert!(marker.finish(&bad).is_err());
        marker.release();
        assert!(held(&root));
    }
}

#[test]
fn foreign_pathname_replacement_is_preserved() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    let file = root.marker_file();
    fs::rename(&file, root.path().join("preserved-original")).expect("rename");
    fs::write(&file, b"FOREIGN_REPLACEMENT").expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(marker.finish(&receipt(json!({}))).is_err());
    marker.release();
    assert_eq!(fs::read(&file).expect("read"), b"FOREIGN_REPLACEMENT");
    assert!(held(&root));
}

#[test]
fn symlink_marker_never_modifies_its_target() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    let file = root.marker_file();
    fs::rename(&file, root.path().join("preserved-original")).expect("rename");
    let target = root.path().join("foreign-target");
    fs::write(&target, b"FOREIGN").expect("write");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("chmod");
    std::os::unix::fs::symlink(&target, &file).expect("symlink");
    assert!(marker.finish(&receipt(json!({}))).is_err());
    marker.release();
    assert_eq!(fs::read(&target).expect("read"), b"FOREIGN");
    assert!(held(&root));
}

#[test]
fn privacy_drift_holds_and_is_never_chmodded() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    let file = root.marker_file();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(marker.finish(&receipt(json!({}))).is_err());
    marker.release();
    assert!(held(&root));
    assert_eq!(
        fs::metadata(&file).expect("meta").permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn pending_operation_is_profile_and_platform_bound() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.release();
    assert!(!pending_marker_state(root.path(), PROFILE, "mac").held);
    assert!(!pending_marker_state(root.path(), "gmail", "mac").held);
}

#[test]
fn unknown_storage_entries_hold_safely() {
    let root = Root::new();
    fs::create_dir(root.marker_root()).expect("mkdir");
    fs::set_permissions(root.marker_root(), fs::Permissions::from_mode(0o700)).expect("chmod");
    let canary = root.marker_root().join("unknown");
    fs::write(&canary, b"PRIVATE_CANARY").expect("write");
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(held(&root));
}

#[test]
fn finished_operations_are_independent_of_later_uncertain_ones() {
    let root = Root::new();
    let mut first = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    first.finish(&receipt(json!({}))).expect("finish");
    first.release();
    let mut second = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    second.release();
    assert!(held(&root));
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 2);
}

#[test]
fn nonprivate_root_prevents_append_without_a_hold() {
    let root = Root::new();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        PendingMarker::arm(root.path(), PROFILE, "windows").expect_err("arm must refuse"),
        MarkerRefused::Unavailable
    );
    assert!(!held(&root));
    assert!(fs::read_dir(root.path()).expect("root").next().is_none());
}

#[test]
fn full_store_skips_with_its_own_reason() {
    let root = Root::new();
    for index in 0..256u32 {
        let nonce = format!("{index:032x}");
        let record = json!({
            "version": 1, "profileId": PROFILE, "targetPlatform": "windows", "nonce": nonce,
            "state": "finished", "createdAt": "2026-10-02T00:00:00Z",
            "finishedAt": "2026-10-02T00:00:01Z", "terminalReceiptSha256": "0".repeat(64)
        });
        let marker_root = root.marker_root();
        fs::create_dir_all(&marker_root).expect("mkdir");
        fs::set_permissions(&marker_root, fs::Permissions::from_mode(0o700)).expect("chmod");
        let file = marker_root.join(format!("{PROFILE}-windows-{nonce}.json"));
        fs::write(&file, serde_json::to_vec(&record).expect("json")).expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    assert!(!held(&root));
    assert_eq!(
        PendingMarker::arm(root.path(), PROFILE, "windows").expect_err("arm must refuse"),
        MarkerRefused::StoreFull
    );
    assert!(!held(&root));
}

#[test]
fn batch_sequence_rearms_one_marker_slot() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    for batch in 0..5 {
        if batch > 0 {
            marker.rearm().expect("rearm");
        }
        assert!(held(&root));
        marker.finish(&receipt(json!({}))).expect("finish");
        assert!(!held(&root));
    }
    marker.release();
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 1);
    let row: Value =
        serde_json::from_slice(&fs::read(root.marker_file()).expect("read")).expect("json");
    assert_eq!(row["state"], json!("finished"));
}

#[test]
fn rearm_refuses_before_finish_after_release_and_after_second_finish() {
    let root = Root::new();
    let mut pending = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    assert_eq!(
        pending.rearm().expect_err("rearm must refuse"),
        MarkerRefused::AppendPending
    );
    assert!(held(&root));
    pending.release();
    fs::remove_dir_all(root.marker_root()).expect("rm");
    let mut released = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    released.finish(&receipt(json!({}))).expect("finish");
    released.release();
    assert_eq!(
        released.rearm().expect_err("rearm must refuse"),
        MarkerRefused::AppendPending
    );
    assert!(!held(&root));
    fs::remove_dir_all(root.marker_root()).expect("rm");
    let mut twice = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    twice.finish(&receipt(json!({}))).expect("finish");
    assert!(twice.finish(&receipt(json!({}))).is_err());
    twice.release();
    assert!(!held(&root));
}

#[test]
fn rearm_refuses_while_another_operation_holds() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    let finished = fs::read(root.marker_file()).expect("read");
    let foreign = write_pending(&root, PROFILE, "windows", 'f');
    assert_eq!(
        marker.rearm().expect_err("rearm must refuse"),
        MarkerRefused::AppendPending
    );
    marker.release();
    let own = fs::read_dir(root.marker_root())
        .expect("dir")
        .map(|entry| entry.expect("entry").path())
        .find(|path| *path != foreign)
        .expect("own marker");
    assert_eq!(fs::read(&own).expect("read"), finished);
    assert!(held(&root));
}

#[test]
fn other_profile_or_platform_pending_does_not_stop_rearm() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    write_pending(&root, PROFILE, "mac", 'a');
    write_pending(&root, "gmail", "windows", 'b');
    marker.rearm().expect("rearm");
    assert!(held(&root));
    marker.finish(&receipt(json!({}))).expect("finish");
    marker.release();
    assert!(!held(&root));
}

#[test]
fn replaced_or_edited_finished_marker_is_never_rearmed() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    let file = root.marker_file();
    let bytes = fs::read(&file).expect("read");
    fs::rename(&file, root.path().join("preserved-original")).expect("rename");
    fs::write(&file, &bytes).expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(marker.rearm().is_err());
    marker.release();
    assert_eq!(fs::read(&file).expect("read"), bytes);
    assert!(!held(&root));
    fs::remove_dir_all(root.marker_root()).expect("rm");
    let mut edited = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    edited.finish(&receipt(json!({}))).expect("finish");
    let edited_file = root.marker_file();
    let mut row: Value =
        serde_json::from_slice(&fs::read(&edited_file).expect("read")).expect("json");
    row["finishedAt"] = json!("2026-10-02T00:00:09.000Z");
    fs::write(&edited_file, serde_json::to_vec(&row).expect("json")).expect("write");
    assert!(edited.rearm().is_err());
    edited.release();
    let row: Value = serde_json::from_slice(&fs::read(&edited_file).expect("read")).expect("json");
    assert_eq!(row["state"], json!("finished"));
}

#[test]
fn rearm_needs_no_new_slot_at_full_capacity() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    for index in 1..256u32 {
        let nonce = format!("{index:032x}");
        let record = json!({
            "version": 1, "profileId": "gmail", "targetPlatform": "mac", "nonce": nonce,
            "state": "finished", "createdAt": "2026-10-02T00:00:00Z",
            "finishedAt": "2026-10-02T00:00:01Z", "terminalReceiptSha256": "0".repeat(64)
        });
        let file = root.marker_root().join(format!("gmail-mac-{nonce}.json"));
        fs::write(&file, serde_json::to_vec(&record).expect("json")).expect("write");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 256);
    marker.rearm().expect("rearm");
    marker.finish(&receipt(json!({}))).expect("finish");
    marker.release();
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 256);
    assert!(!held(&root));
}

#[test]
fn interrupted_rearm_leaves_an_empty_marker_that_holds() {
    let root = Root::new();
    let mut marker = PendingMarker::arm(root.path(), PROFILE, "windows").expect("arm");
    marker.finish(&receipt(json!({}))).expect("finish");
    assert!(!held(&root));
    // The rewrite is lost after truncation: the held inode is now empty.
    let file = root.marker_file();
    let handle = fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .expect("open");
    handle.set_len(0).expect("truncate");
    drop(handle);
    assert!(marker.rearm().is_err());
    marker.release();
    assert_eq!(fs::metadata(&file).expect("meta").len(), 0);
    assert!(held(&root));
    assert_eq!(
        PendingMarker::arm(root.path(), PROFILE, "windows").expect_err("arm must refuse"),
        MarkerRefused::AppendPending
    );
    assert_eq!(fs::read_dir(root.marker_root()).expect("dir").count(), 1);
}

#[test]
fn without_core_fallback_reads_marker_names_only() {
    let root = Root::new();
    assert!(!open_held_without_core(root.path(), PROFILE, "windows"));
    write_pending(&root, PROFILE, "windows", 'a');
    assert!(open_held_without_core(root.path(), PROFILE, "windows"));
    assert!(!open_held_without_core(root.path(), PROFILE, "mac"));
    assert!(open_held_without_core(root.path(), "../outside", "windows"));
    // The service wrapper never holds IDs outside the core's set.
    assert!(!open_held(root.path(), "unknown", "windows"));
}
