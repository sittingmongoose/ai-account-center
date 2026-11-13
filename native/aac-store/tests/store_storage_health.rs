mod common;
use aac_store::ops;
use common::temp_dir;
use std::fs::{self, File};
use std::io::Write;

#[test]
fn quota_temporary_health_counts_files_without_touching_a_live_writer() {
    let root = temp_dir("quota-temporary-health");
    let directory = root.join("account-analytics");
    let engine = ops::open_engine(root.join("store").to_str().unwrap(), None, None).unwrap();
    assert_eq!(
        ops::call(&engine, "status", "{}").unwrap()["storageHealth"]["quotaTemporaries"]["count"],
        0
    );
    fs::create_dir(&directory).unwrap();
    let main = directory.join("quota-history-v1.json");
    fs::write(&main, b"main sentinel").unwrap();
    fs::write(directory.join("unrelated.tmp"), b"untouched").unwrap();
    let live = directory.join("quota-history-v1.json.00000000-0000-4000-8000-000000000000.tmp");
    let old = directory.join("quota-history-v1.json.00000000-0000-4000-8000-000000000001.tmp");
    fs::write(&old, b"old").unwrap();
    let mut handle = File::create(&live).unwrap();
    handle.write_all(b"pending payload").unwrap();
    handle.sync_all().unwrap();
    handle
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600))
        .unwrap();
    let mtime = handle.metadata().unwrap().modified().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &main,
        directory.join("quota-history-v1.json.00000000-0000-4000-8000-000000000002.tmp"),
    )
    .unwrap();
    let health = ops::call(&engine, "status", "{}").unwrap()["storageHealth"]["quotaTemporaries"].clone();
    assert_eq!(health["count"], 2);
    assert_eq!(health["bytes"], 18);
    assert_eq!(health["scan"], "complete");
    assert_eq!(health["automaticDeletion"], false);
    assert!(health["diagnostic"].as_str().unwrap().contains("stop every writer"));
    assert_eq!(fs::read(&main).unwrap(), b"main sentinel");
    assert_eq!(fs::read(&old).unwrap(), b"old");
    assert_eq!(handle.metadata().unwrap().modified().unwrap(), mtime);
    drop(handle);
    let published = directory.join("published.json");
    fs::rename(&live, &published).unwrap();
    assert_eq!(fs::read(published).unwrap(), b"pending payload");
    assert_eq!(
        ops::call(&engine, "status", "{}").unwrap()["storageHealth"]["quotaTemporaries"]["count"],
        1
    );
    engine.close().unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn quota_temporary_health_refuses_to_follow_a_linked_directory() {
    let root = temp_dir("quota-temporary-linked");
    let target = root.join("target");
    fs::create_dir(&target).unwrap();
    fs::write(
        target.join("quota-history-v1.json.00000000-0000-4000-8000-000000000000.tmp"),
        b"private",
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, root.join("account-analytics")).unwrap();
    let engine = ops::open_engine(root.join("store").to_str().unwrap(), None, None).unwrap();
    let health = ops::call(&engine, "status", "{}").unwrap()["storageHealth"]["quotaTemporaries"].clone();
    assert_eq!(health["scan"], "incomplete");
    assert_eq!(health["count"], 0);
    assert_eq!(health["scanErrors"], 1);
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
    engine.close().unwrap();
    fs::remove_dir_all(root).unwrap();
}
