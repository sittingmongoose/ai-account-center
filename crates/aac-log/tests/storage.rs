//! Files, settings, rotation and retention, as `log-storage.ts` and `log-config.ts` keep them.
#![allow(clippy::unwrap_used, missing_docs)]
use aac_log::{Extra, Level, Log, LogConfig};
use serde_json::{Map, json};
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

fn write(log: &Log, level: Level, message: &str) {
    let mut context = Map::new();
    context.insert("token".into(), json!("abc"));
    log.write(
        "test",
        level,
        "test.event",
        message,
        &context,
        &Extra::default(),
    );
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn settings_follow_logging_with_ts_defaults() {
    assert_eq!(LogConfig::from_yaml(""), LogConfig::default());
    assert_eq!(
        LogConfig::from_yaml("version: 10\nlogging: null\n"),
        LogConfig::default()
    );
    let c = LogConfig::from_yaml(
        "version: 10\nlogging:\n  enabled: false\n  level: debug\n  rotate_mb: 2\n  retain_days: 1\n  redact: false\n",
    );
    assert!(!c.enabled && !c.redact);
    assert_eq!(
        (c.level, c.rotate_mb, c.retain_days),
        (Some(Level::Debug), 2.0, 1.0)
    );
    // `??`: null keeps the default; an unknown level writes nothing.
    let c = LogConfig::from_yaml("version: 10\nlogging:\n  level: null\n  redact: null\n");
    assert_eq!((c.level, c.redact), (Some(Level::Info), true));
    assert_eq!(
        LogConfig::from_yaml("version: 10\nlogging:\n  level: verbose\n").level,
        None
    );
    // `isUnifiedConfig`: without a numeric version >= 1 the TS loader reads no section at all.
    assert_eq!(
        LogConfig::from_yaml("logging:\n  enabled: false\n"),
        LogConfig::default()
    );
    assert_eq!(
        LogConfig::from_yaml("version: 0\nlogging:\n  enabled: false\n"),
        LogConfig::default()
    );
}

#[test]
fn entries_append_with_private_modes_levels_and_redaction() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(dir.path().to_path_buf());
    write(&log, Level::Info, "first Bearer abcdefghijkl");
    write(&log, Level::Debug, "filtered at the default info level");
    let current = dir.path().join("logs/current.jsonl");
    let got = lines(&current);
    assert_eq!(got.len(), 1);
    let keys: Vec<&str> = got[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "timestamp",
            "level",
            "source",
            "event",
            "message",
            "processId",
            "runId",
            "context"
        ]
    );
    assert_eq!(got[0]["message"], "first Bearer [redacted]");
    assert_eq!(got[0]["context"]["token"], "[redacted]");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&current).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(dir.path().join("logs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.path().join("logs/archive"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    // Turning logging off (the config is rechecked within a second) writes nothing.
    fs::write(
        dir.path().join("config.yaml"),
        "version: 10\nlogging:\n  enabled: false\n",
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    write(&log, Level::Error, "off");
    assert_eq!(lines(&current).len(), 1);
}

#[test]
fn the_current_file_rotates_by_size_and_archives_are_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let log = Log::new(dir.path().to_path_buf());
    write(&log, Level::Info, "seed");
    let current = dir.path().join("logs/current.jsonl");
    // Past the 1 MiB floor (rotate_mb below 1 counts as 1).
    fs::write(
        dir.path().join("config.yaml"),
        "version: 10\nlogging:\n  rotate_mb: 0.1\n",
    )
    .unwrap();
    fs::write(&current, vec![b'x'; 1024 * 1024]).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    write(&log, Level::Info, "after rotation");
    let archives: Vec<_> = fs::read_dir(dir.path().join("logs/archive"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(archives.len(), 1);
    let name = archives[0].file_name().into_string().unwrap();
    assert!(
        name.starts_with("ccs-") && name.ends_with("Z.jsonl") && !name.contains(':'),
        "{name}"
    );
    assert_eq!(lines(&current).len(), 1);

    // Retention: an archive older than retain_days goes; then the oldest beyond 32 MiB.
    let archive = dir.path().join("logs/archive");
    let old = archive.join("ccs-old.jsonl");
    fs::write(&old, b"old").unwrap();
    let past = SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(past)
        .unwrap();
    for (i, name) in ["a", "b", "c"].iter().enumerate() {
        let file = archive.join(format!("ccs-big-{name}.jsonl"));
        fs::write(&file, vec![b'y'; 12 * 1024 * 1024]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(3600 * (3 - i as u64));
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    log.prune(&log.config());
    assert!(!old.exists());
    assert!(
        !archive.join("ccs-big-a.jsonl").exists(),
        "the oldest beyond the cap goes first"
    );
    assert!(archive.join("ccs-big-b.jsonl").exists() && archive.join("ccs-big-c.jsonl").exists());
}

/// A 10 MiB archive with the given age, so three distinct rotations fit the 32 MiB cap.
fn archive_file(archive: &Path, name: &str, age: Duration) -> std::path::PathBuf {
    fs::create_dir_all(archive).unwrap();
    let path = archive.join(name);
    fs::write(&path, vec![b'z'; 10 * 1024 * 1024]).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
    path
}

/// Leave `current.jsonl` linked from the archive, as a stop between link and unlink does.
fn interrupted_rotation(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let logs = dir.join("logs");
    let archive = logs.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let current = logs.join("current.jsonl");
    fs::write(&current, vec![b'x'; 10 * 1024 * 1024]).unwrap();
    let linked = archive.join("ccs-interrupted.jsonl");
    fs::hard_link(&current, &linked).unwrap();
    (current, linked)
}

#[test]
fn an_interrupted_rotation_is_finished_before_the_next_append() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        "version: 10\nlogging:\n  rotate_mb: 10\n",
    )
    .unwrap();
    let archive = dir.path().join("logs/archive");
    let first = archive_file(&archive, "ccs-big-a.jsonl", Duration::from_secs(2 * 3600));
    let second = archive_file(&archive, "ccs-big-b.jsonl", Duration::from_secs(3600));
    let (current, linked) = interrupted_rotation(dir.path());
    let log = Log::new(dir.path().to_path_buf());

    write(&log, Level::Info, "after interrupted rotation");

    // The archive keeps the old bytes; the still-full current file is not rotated again.
    assert_eq!(fs::read_dir(&archive).unwrap().count(), 3);
    assert!(first.exists() && second.exists() && linked.exists());
    assert_eq!(lines(&current).len(), 1);
    assert_eq!(lines(&current)[0]["message"], "after interrupted rotation");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert!(fs::metadata(&linked).unwrap().nlink() == 1);
    }
}

#[test]
fn prune_finishes_an_interrupted_rotation_without_waiting_for_an_entry() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        "version: 10\nlogging:\n  rotate_mb: 10\n",
    )
    .unwrap();
    let archive = dir.path().join("logs/archive");
    let first = archive_file(&archive, "ccs-big-a.jsonl", Duration::from_secs(2 * 3600));
    let second = archive_file(&archive, "ccs-big-b.jsonl", Duration::from_secs(3600));
    let (current, linked) = interrupted_rotation(dir.path());
    let log = Log::new(dir.path().to_path_buf());

    log.prune(&log.config());

    assert!(!current.exists(), "the interrupted rotation is finished");
    assert_eq!(fs::read_dir(&archive).unwrap().count(), 3);
    assert!(first.exists() && second.exists() && linked.exists());
}

#[test]
fn rotation_and_append_wait_for_another_log_writer() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.yaml"),
        "version: 10\nlogging:\n  rotate_mb: 1\n",
    )
    .unwrap();
    let logs = dir.path().join("logs");
    let archives = logs.join("archive");
    fs::create_dir_all(&archives).unwrap();
    let current = logs.join("current.jsonl");
    fs::write(&current, vec![b'x'; 1024 * 1024]).unwrap();
    let lease = aac_files::WriterLease::try_take(&logs.join("current.jsonl.writer.lock")).unwrap();
    let log = Log::new(dir.path().to_path_buf());
    std::thread::scope(|scope| {
        let (begun, waiting) = std::sync::mpsc::channel();
        let job = scope.spawn(move || {
            begun.send(()).unwrap();
            write(&log, Level::Info, "after other writer");
        });
        waiting.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(fs::read_dir(&archives).unwrap().next().is_none());
        assert_eq!(fs::metadata(&current).unwrap().len(), 1024 * 1024);
        drop(lease);
        job.join().unwrap();
    });
    assert_eq!(fs::read_dir(&archives).unwrap().count(), 1);
    assert_eq!(lines(&current)[0]["message"], "after other writer");
}
