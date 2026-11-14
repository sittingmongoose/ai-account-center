//! Private-store durability, ownership and bounded IO tests.
#![allow(clippy::unwrap_used)]
use aac_core::Cause;
use aac_files::*;
use std::path::{Path, PathBuf};

fn registry(max: usize) -> Registry {
    let mut registry = Registry::default();
    registry
        .register(StoreSpec {
            path: "state.json".into(),
            class: Class::State,
            mode: 0o600,
            max_size: max,
            owner: "fixture-writer".into(),
            schema: "fixture.v1".into(),
            backup: BackupClass::Snapshot,
        })
        .unwrap();
    registry
}

#[test]
fn writer_ownership_and_advisory_lock_are_enforced() {
    let root = tempfile::tempdir().unwrap();
    let registry = registry(32);
    assert_eq!(
        registry
            .claim(root.path(), Path::new("state.json"), "other-role")
            .unwrap_err()
            .cause,
        Cause::Refused
    );
    let first = registry
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    assert_eq!(
        registry
            .claim(root.path(), Path::new("state.json"), "fixture-writer")
            .unwrap_err()
            .cause,
        Cause::Busy
    );
    drop(first);
    assert!(
        registry
            .claim(root.path(), Path::new("state.json"), "fixture-writer")
            .is_ok()
    );
}

#[test]
fn failed_partial_write_never_replaces_original_and_staging_is_removed() {
    let root = tempfile::tempdir().unwrap();
    let store = registry(32)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    store.write(b"original").unwrap();
    let result = store.write_with(|out| {
        out.write_all(b"partial")?;
        Err(std::io::Error::other("injected failure"))
    });
    assert!(result.is_err());
    assert_eq!(store.read().unwrap(), b"original");
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.write_with(|out| {
            out.write_all(b"partial")?;
            panic!("injected writer crash");
        })
    }));
    assert!(panic.is_err());
    assert_eq!(store.read().unwrap(), b"original");
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}

#[test]
fn caps_apply_to_streaming_writes_and_reads() {
    let root = tempfile::tempdir().unwrap();
    let store = registry(8)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    store.write(b"12345678").unwrap();
    assert_eq!(
        store.write(b"123456789").unwrap_err().cause,
        Cause::OutputTooLarge
    );
    assert_eq!(store.read().unwrap(), b"12345678");
    // Even a callback swallowing the cap error cannot commit its partial stage.
    assert_eq!(
        store
            .write_with(|out| {
                let _ = out.write_all(b"too much output");
                Ok(())
            })
            .unwrap_err()
            .cause,
        Cause::OutputTooLarge
    );
    std::fs::write(root.path().join("state.json"), b"123456789").unwrap();
    assert_eq!(store.read().unwrap_err().cause, Cause::OutputTooLarge);
}

#[test]
fn trash_is_atomic_and_retains_original_bytes() {
    let root = tempfile::tempdir().unwrap();
    let store = registry(32)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    store.write(b"retained").unwrap();
    let trash = store.trash().unwrap();
    assert!(!root.path().join("state.json").exists());
    assert_eq!(read_bounded(&trash, 32).unwrap(), b"retained");
    store.write(b"new").unwrap();
    assert_eq!(std::fs::read(trash).unwrap(), b"retained");
}

#[test]
fn registry_refuses_weak_modes_duplicates_and_path_escape() {
    for path in [
        "../escape",
        "/absolute",
        "x/../../escape",
        "x\\escape",
        "C:escape",
    ] {
        let mut registry = Registry::default();
        assert!(
            registry
                .register(StoreSpec {
                    path: path.into(),
                    class: Class::State,
                    mode: 0o600,
                    max_size: 8,
                    owner: "writer".into(),
                    schema: "v1".into(),
                    backup: BackupClass::Snapshot
                })
                .is_err()
        );
    }
    let mut registry = registry(32);
    assert!(
        registry
            .register(StoreSpec {
                path: PathBuf::from("state.json"),
                class: Class::State,
                mode: 0o644,
                max_size: 8,
                owner: "writer".into(),
                schema: "v1".into(),
                backup: BackupClass::Snapshot
            })
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn private_modes_and_symlink_refusal() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let store = registry(32)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    store.write(b"secret-free fixture").unwrap();
    for file in ["state.json", "state.json.lock"] {
        assert_eq!(
            std::fs::metadata(root.path().join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let target = root.path().join("outside");
    std::fs::write(&target, b"outside").unwrap();
    std::fs::remove_file(root.path().join("state.json")).unwrap();
    symlink(&target, root.path().join("state.json")).unwrap();
    assert!(store.write(b"replacement").is_err());
    assert!(store.read().is_err());
    assert!(store.trash().is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"outside");
    let alias = root.path().join("alias");
    symlink(root.path(), &alias).unwrap();
    assert!(
        registry(32)
            .claim(&alias, Path::new("state.json"), "fixture-writer")
            .is_err()
    );
}
