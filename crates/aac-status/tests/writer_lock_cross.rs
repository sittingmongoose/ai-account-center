//! G16b gate: one `answers.lock` between a TypeScript process (`tests/spec/answer-lock-probe.ts`,
//! the real `writer-lock.ts`) and this Rust process (`writer_lock`). Either side sees the other's
//! live lock as busy, with the same holder words, and takes over a crashed holder's lock. Skips
//! when `bun` is not on PATH.
use aac_status::writer_lock::{LockError, WRITER_LOCK_FILE, hold_writer_lock, host_name, release_writer_lock};
use std::{
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn probe() -> Option<(String, PathBuf)> {
    let bun = if cfg!(windows) { "bun.exe" } else { "bun" };
    Command::new(bun).arg("--version").output().ok()?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/answer-lock-probe.ts");
    Some((bun.to_owned(), script))
}

fn run(bun: &str, script: &Path, store: &Path, mode: &str) -> String {
    let out = Command::new(bun).arg(script).arg(store).arg(mode).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn typescript_and_rust_share_one_writer_lock() {
    let Some((bun, script)) = probe() else {
        eprintln!("bun is not on PATH: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("status");

    // A live TypeScript holder: Rust is busy, named the TypeScript way.
    let mut child = Command::new(&bun)
        .arg(&script)
        .arg(&store)
        .arg("hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid: u32 = line.trim().strip_prefix("held ").unwrap().parse().unwrap();
    match hold_writer_lock(&store) {
        Err(error @ LockError::Busy(_)) => assert_eq!(
            error.message(),
            format!(
                "Another AI Account Center process (pid {pid} on {}) records answers from this folder.",
                host_name()
            )
        ),
        other => panic!("expected busy, got {other:?}"),
    }
    drop(child.stdin.take());
    child.wait().unwrap();
    // Released on exit: Rust gains it.
    assert!(!store.join(WRITER_LOCK_FILE).exists());
    assert!(hold_writer_lock(&store).unwrap());

    // A live Rust holder: TypeScript is busy, with this process's pid and the same host name.
    assert_eq!(
        run(&bun, &script, &store, "try"),
        format!(
            "busy Another AI Account Center process (pid {} on {}) records answers from this folder.",
            std::process::id(),
            host_name()
        )
    );
    release_writer_lock(&store);
    assert_eq!(run(&bun, &script, &store, "try"), "gained");

    // A crashed TypeScript holder (no release): Rust takes it over.
    let held = run(&bun, &script, &store, "crash");
    assert!(held.starts_with("held "), "{held}");
    assert!(store.join(WRITER_LOCK_FILE).exists());
    assert!(
        hold_writer_lock(&store).unwrap(),
        "a crashed holder's lock is taken over"
    );
    release_writer_lock(&store);
}
