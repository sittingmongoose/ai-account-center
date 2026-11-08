#![allow(dead_code)]
use std::fs;
use std::path::{Path, PathBuf};

/// A fresh folder under the system temporary folder; never the real home.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("aac-store-tests").join(format!(
        "{name}-{}-{}",
        std::process::id(),
        aac_store::fsx::random_hex(4)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
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
