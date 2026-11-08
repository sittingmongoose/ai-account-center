//! Private, durable file operations: 0700 folders, 0600 files, same-folder temporary files,
//! fsync of the file and its folder, and the one-writer OS lock.

use crate::error::Result;
use crate::fail;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates the folder (and parents) owner-only, refusing a symlink in its place.
pub fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                fail!("unsafe_path", "{} is not a private folder", path.display());
            }
        }
        Err(_) => fs::create_dir_all(path)?,
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Opens a file for reading and writing, created owner-only. A symlink is refused (the store
/// folder is 0700, so nothing else can swap one in between the check and the open).
pub fn open_private(path: &Path, create: bool) -> Result<File> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        fail!("unsafe_path", "{} is not a regular file", path.display());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

/// Synchronizes a folder's entries. Windows has no portable folder fsync; NTFS journals
/// metadata, so this is a no-op there.
pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub fn temp_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!(".{name}.tmp-{}-{n}", std::process::id()))
}

/// Same-folder temporary file, fsync, rename over the target, fsync the folder.
pub fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let temp = temp_path(target);
    let result = (|| -> Result<()> {
        let mut file = open_private(&temp, true)?;
        file.set_len(0)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, target)?;
        if let Some(parent) = target.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Copies a file durably through a temporary name.
pub fn copy_atomic(source: &Path, target: &Path) -> Result<()> {
    let temp = temp_path(target);
    let result = (|| -> Result<()> {
        let mut input = File::open(source)?;
        let mut output = open_private(&temp, true)?;
        output.set_len(0)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temp, target)?;
        if let Some(parent) = target.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Removes temporary files a crash left behind (names starting with `.` and holding `.tmp-`).
pub fn remove_temporaries(dir: &Path) -> Result<usize> {
    let mut removed = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.contains(".tmp-") && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// The one-writer lock (`store.lock`), held for the life of the returned file.
pub struct StoreLock {
    _file: File,
}

pub fn lock_store(dir: &Path) -> Result<StoreLock> {
    let file = open_private(&dir.join("store.lock"), true)?;
    match file.try_lock() {
        Ok(()) => Ok(StoreLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => {
            fail!("lock_held", "another process holds the store lock")
        }
        Err(std::fs::TryLockError::Error(e)) => fail!("lock_indeterminate", "{e}"),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    let mut out = String::with_capacity(64);
    for b in hasher.finalize().iter() {
        out.push_str(&format!("{b:02x}"));
    }
    Ok((out, total))
}

/// A random-looking hex id from the clock, the process and std's per-process hash seed.
pub fn random_hex(bytes: usize) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    let mut n = 0u64;
    while out.len() < bytes * 2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        hasher.write_u32(std::process::id());
        hasher.write_u64(n);
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        out.push_str(&format!("{:016x}", hasher.finish()));
        n += 1;
    }
    out.truncate(bytes * 2);
    out
}
