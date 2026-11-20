//! Descriptor-relative retention for the Claude history writer's snapshot directories.
use crate::{BackupClass, Class, Registry, StoreSpec};
use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use serde::Serialize;
use std::{io, path::Path};

const PREFIX: &str = ".history-index-snapshot-";
const MANIFEST: &str = "snapshot-manifest.json";
const KEEP: usize = 3;
const DELETE_CAP: usize = 200;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Public receipt for an already authenticated snapshot root.
#[derive(Serialize)]
pub struct HistorySnapshotReceipt {
    /// The newest complete snapshots, newest first.
    pub kept: Vec<String>,
    /// The complete older snapshots removed by this call.
    pub deleted: Vec<String>,
    /// Matching snapshot names that could not be established as wholly owned.
    pub skipped: Vec<String>,
}

fn candidate(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn owned_child(name: &str) -> bool {
    name == MANIFEST
        || name
            .strip_prefix("protected-")
            .and_then(|s| s.strip_suffix(".bin"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn owned_names(folder: impl AsFd) -> io::Result<Vec<String>> {
    let folder = folder.as_fd();
    let mut names = Vec::new();
    for entry in Dir::read_from(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_str().map_err(io::Error::other)?;
        if matches!(name, "." | "..") {
            continue;
        }
        if names.len() == 64 || !owned_child(name) {
            return Err(io::Error::other("foreign snapshot contents"));
        }
        let stat = fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(io::Error::other("foreign snapshot contents"));
        }
        names.push(name.to_owned());
    }
    if !names.iter().any(|name| name == MANIFEST) {
        return Err(io::Error::other("snapshot manifest missing"));
    }
    names.sort_unstable();
    Ok(names)
}

fn child_folder(root: &OwnedFd, name: &str) -> io::Result<(OwnedFd, Stat)> {
    let folder = fs::openat(root, name, DIRECTORY_FLAGS, Mode::empty())?;
    let stat = fs::fstat(&folder)?;
    Ok((folder, stat))
}

fn delete(root: &OwnedFd, name: &str, original: &Stat) -> io::Result<()> {
    let (folder, stat) = child_folder(root, name)?;
    if (stat.st_dev, stat.st_ino) != (original.st_dev, original.st_ino) {
        return Err(io::Error::other("snapshot changed"));
    }
    // Finish the descriptor checks before the first unlink. The last component of
    // unlinkat is never followed, even if the producer removes it after validation.
    for child in owned_names(&folder)? {
        fs::unlinkat(&folder, child, AtFlags::empty())?;
    }
    let current = fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if (current.st_dev, current.st_ino) != (original.st_dev, original.st_ino) {
        return Err(io::Error::other("snapshot changed"));
    }
    fs::unlinkat(root, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

/// Keep three snapshots per bound profile and remove at most 200 fully owned older ones.
///
/// The original Python helper takes this registry lease on Unix as well.
/// Only pre-mutation failures are errors. A failed deletion goes in the receipt.
pub fn prune_history_snapshots(root: &Path) -> io::Result<HistorySnapshotReceipt> {
    let mut registry = Registry::default();
    let lock = Path::new("aac-history-snapshot-retention");
    registry
        .register(StoreSpec {
            path: lock.into(),
            class: Class::Cache,
            mode: 0o600,
            max_size: 4096,
            owner: "history-snapshots".to_owned(),
            schema: "history-snapshot-retention-v1".to_owned(),
            backup: BackupClass::Rebuild,
        })
        .map_err(io::Error::other)?;
    let _lease = registry
        .claim(root, lock, "history-snapshots")
        .map_err(io::Error::other)?;
    let root = fs::open(root, DIRECTORY_FLAGS, Mode::empty())?;
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    let mut examined = 0;
    for entry in Dir::read_from(&root)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if matches!(file_name.to_bytes(), b"." | b"..") {
            continue;
        }
        if examined == 1024 {
            break;
        }
        examined += 1;
        let Ok(name) = file_name.to_str() else {
            continue;
        };
        if !candidate(name) {
            continue;
        }
        match child_folder(&root, name) {
            Ok((folder, stat)) if owned_names(&folder).is_ok() => {
                if let Ok(stamp) = std::fs::File::from(folder)
                    .metadata()
                    .and_then(|meta| meta.modified())
                {
                    candidates.push((name.to_owned(), stat, stamp));
                } else {
                    skipped.push(name.to_owned());
                }
            }
            _ => skipped.push(name.to_owned()),
        }
    }
    candidates.sort_by(|(_, _, a), (_, _, b)| b.cmp(a));
    let mut kept = Vec::new();
    let mut deleted = Vec::new();
    for (index, (name, stat, _)) in candidates.into_iter().enumerate() {
        if index < KEEP {
            kept.push(name);
        } else if deleted.len() < DELETE_CAP && delete(&root, &name, &stat).is_ok() {
            deleted.push(name);
        } else {
            skipped.push(name);
        }
    }
    skipped.sort_unstable();
    Ok(HistorySnapshotReceipt {
        kept,
        deleted,
        skipped,
    })
}
