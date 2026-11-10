#![allow(dead_code)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh folder under the system temporary folder; never the real home. Each caller gets its
/// own directory exclusive to this invocation, so parallel test runs on the same machine cannot
/// share a store lock.
pub fn temp_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join("aac-store-tests");
    fs::create_dir_all(&base).unwrap();
    for _ in 0..100 {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!(
            "{name}-{}-{}-{n}",
            std::process::id(),
            aac_store::fsx::random_hex(8)
        ));
        // Exclusive create: a collision retries with a fresh name instead of sharing a lock.
        match fs::create_dir(&dir) {
            Ok(()) => return dir,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("temp_dir: {}", e),
        }
    }
    panic!("temp_dir: could not create a unique folder for {name}")
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "store.lock" {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub fn find(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().ends_with(suffix))
        .collect();
    out.sort();
    out
}
