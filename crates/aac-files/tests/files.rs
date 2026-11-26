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
    // Re-enter this existing test as an isolated child to kill it between staging and rename.
    if let Some(root) = std::env::var_os("AAC_M1_CRASH_ROOT") {
        let root = PathBuf::from(root);
        let store = registry(32)
            .claim(&root, Path::new("state.json"), "fixture-writer")
            .unwrap();
        let _ = store.write_with(|out| {
            out.write_all(b"partial crash")?;
            out.flush()?;
            std::fs::write(root.join("stage-ready"), b"ready")?;
            std::thread::sleep(std::time::Duration::from_secs(30));
            Ok(())
        });
        return;
    }
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
fn killed_writer_keeps_the_committed_file_and_releases_its_lock() {
    let root = tempfile::tempdir().unwrap();
    let store = registry(32)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    store.write(b"committed original").unwrap();
    drop(store);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "failed_partial_write_never_replaces_original_and_staging_is_removed",
        ])
        .env("AAC_M1_CRASH_ROOT", root.path())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !root.path().join("stage-ready").exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash fixture did not reach staging");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let store = registry(32)
        .claim(root.path(), Path::new("state.json"), "fixture-writer")
        .unwrap();
    assert_eq!(store.read().unwrap(), b"committed original");
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
        "state.json.lock",
        ".trash/entry",
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

// Only the Unix takeover tests below use it (clippy -D warnings on Windows).
#[cfg(unix)]
fn private_file(path: &Path, bytes: &[u8]) {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(path).unwrap(), bytes).unwrap();
}

// Only the Unix takeover tests below use it (clippy -D warnings on Windows).
#[cfg(unix)]
fn private_dir(path: &Path) {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).unwrap();
}

// Only the Unix takeover tests below use it (clippy -D warnings on Windows).
#[cfg(unix)]
fn age(path: &Path, seconds: u64) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    std::fs::File::options()
        .read(true)
        .open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

// Only the Unix takeover tests below use it (clippy -D warnings on Windows).
#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
#[cfg(unix)]
fn an_aged_legacy_takeover_gate_does_not_block_the_next_session_owner() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("auth");
    private_dir(&auth);
    let (host, boot) = aac_files::process::host_boot().unwrap();
    let claim = serde_json::json!({
        "pid": dead_pid(), "host": host, "boot": boot, "writer": "rust", "token": "crashed"
    });
    private_file(
        &auth.join("sessions.owner"),
        &serde_json::to_vec(&claim).unwrap(),
    );
    // A pre-cut listener made the gate and stopped before replacing the dead claim.
    let gate = auth.join("sessions.owner.lock");
    std::fs::create_dir(&gate).unwrap();
    age(&gate, 61);
    let owner = auth_owner::SessionOwner::take(root.path()).expect("stale gate is reclaimed");
    owner.verify().unwrap();
    assert!(!gate.exists());
    drop(owner);
    assert!(!auth.join("sessions.owner").exists());
}

#[test]
#[cfg(target_os = "linux")]
fn a_crash_released_takeover_gate_is_reclaimed_immediately() {
    let root = tempfile::tempdir().unwrap();
    let auth = root.path().join("auth");
    private_dir(&auth);
    let (host, boot) = aac_files::process::host_boot().unwrap();
    let claim = serde_json::json!({
        "pid": dead_pid(), "host": host, "boot": boot, "writer": "rust", "token": "crashed"
    });
    private_file(
        &auth.join("sessions.owner"),
        &serde_json::to_vec(&claim).unwrap(),
    );
    // The gate a Rust takeover creates under its kernel lease; the crash released the lease.
    let gate = auth.join("sessions.owner.lock");
    let mut builder = std::fs::DirBuilder::new();
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o1700);
    }
    builder.create(&gate).unwrap();
    let owner = auth_owner::SessionOwner::take(root.path()).expect("crash-released gate");
    owner.verify().unwrap();
    assert!(!gate.exists());
    drop(owner);
}

#[test]
#[cfg(target_os = "linux")]
fn an_abandoned_empty_legacy_config_marker_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("config.yaml.lock");
    // The pre-cut writer's exclusive open, stopped before the first byte.
    private_file(&marker, b"");
    age(&marker, 6);
    let lock = ConfigLock::take(dir.path()).expect("stale empty marker is reclaimed");
    assert!(marker.exists());
    drop(lock);
    assert!(!marker.exists());
}

#[test]
#[cfg(target_os = "linux")]
fn a_fresh_or_open_empty_config_marker_stays_busy() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("config.yaml.lock");
    private_file(&marker, b"");
    assert_eq!(ConfigLock::take(dir.path()).unwrap_err().cause, Cause::Busy);
    age(&marker, 6);
    // A legacy writer stopped after its exclusive open still holds the descriptor.
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .unwrap();
    assert_eq!(ConfigLock::take(dir.path()).unwrap_err().cause, Cause::Busy);
    drop(writer);
    assert!(ConfigLock::take(dir.path()).is_ok());
}
