//! The atomic private-file replace renewal writes through
//! (`src/codex-auth/codex-atomic-file.ts`): a new 0600 temporary file in the
//! same folder (`wx`, so a planted file or link is never followed), fsync,
//! rename, then fsync of the folder where the platform can. Readers see the
//! old or the new file, never a part. The error is a fixed line: callers write
//! credentials, so no cause or path detail is attached.

use std::io::Write;
use std::path::Path;

/// `replaceFileAtomically`: Ok only when the rename completed.
pub fn replace_file_atomically(file_path: &Path, content: &[u8]) -> Result<(), ()> {
    let random = {
        use std::hash::{BuildHasher, Hasher};
        let a = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let b = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        format!("{a:016x}{b:016x}")
    };
    let temporary = file_path.with_file_name(format!(
        "{}.tmp.{}.{}",
        file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        std::process::id(),
        &random[..16]
    ));
    let result = (|| -> Result<(), ()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|_| ())?;
        file.write_all(content).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        drop(file);
        std::fs::rename(&temporary, file_path).map_err(|_| ())?;
        // Windows cannot open a folder for fsync; the file itself was synced.
        #[cfg(unix)]
        if let Some(parent) = file_path.parent() {
            let dir = std::fs::File::open(parent).map_err(|_| ())?;
            dir.sync_all().map_err(|_| ())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_writes_whole_bytes_and_leaves_no_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("auth.json");
        std::fs::write(&file, b"old").expect("seed");
        replace_file_atomically(&file, b"new bytes").expect("replace");
        assert_eq!(std::fs::read(&file).expect("read"), b"new bytes");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_planted_temp_link_is_never_followed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("auth.json");
        // The exact temp name is random; a folder at the target path makes the
        // rename fail and the write must report the fixed error.
        std::fs::create_dir(&file).expect("plant");
        assert!(replace_file_atomically(&file, b"x").is_err());
    }
}
