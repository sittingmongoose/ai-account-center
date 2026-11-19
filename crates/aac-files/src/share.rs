//! Shared-folder write mechanics for the aac-usage snapshot (cut gap G18): the TypeScript
//! `writeAtomically` and `syncFiles` of `agent-usage-snapshot.ts`. The folder is shared, so the
//! temporary name is unpredictable and created exclusively: a link or file planted under that
//! name makes the write fail instead of being followed and truncated. The rename replaces a
//! planted link itself, never its target. Unlike the private stores, the share write does not
//! fsync: the TypeScript writer never did, and a hard network mount must not hold the dashboard.

use crate::read::{self, Read, ReadPolicy};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// The packaged-source read cap: the reader and runner helpers are small texts.
const PACKAGED_CAP: u64 = 8 * 1024 * 1024;

/// Writes `name` by renaming a new exclusive temporary file over it, with the given mode.
/// A failed write removes its temporary; the target keeps its previous bytes.
pub fn write_atomically(directory: &Path, name: &str, body: &str, mode: u32) -> io::Result<()> {
    let target = directory.join(name);
    let staged = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .suffix(".tmp")
        .tempfile_in(directory)?;
    set_mode(staged.as_file(), mode)?;
    let mut file = staged;
    file.as_file_mut().write_all(body.as_bytes())?;
    file.as_file_mut().flush()?;
    file.persist(&target)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
    Ok(())
}

fn set_mode(file: &fs::File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (file, mode);
    }
    Ok(())
}

/// `readPackagedSource`: a packaged helper as its canonical LF bytes. Only CR LF pairs change;
/// a lone CR is kept, so any other edit still fails its pin.
pub fn read_packaged_source(file: &Path) -> io::Result<Vec<u8>> {
    let bytes = match read::read(file, &ReadPolicy::capped(PACKAGED_CAP)) {
        Read::Bytes(bytes, _) => bytes,
        Read::Absent => return Err(io::Error::from(io::ErrorKind::NotFound)),
        Read::TooLarge => return Err(io::Error::from(io::ErrorKind::FileTooLarge)),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "packaged source refused",
            ));
        }
    };
    Ok(with_lf_line_endings(&bytes))
}

/// `withLfLineEndings`: the latin1 round trip that maps every byte to itself.
pub fn with_lf_line_endings(bytes: &[u8]) -> Vec<u8> {
    if !bytes.contains(&b'\r') {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
            out.push(b'\n');
            at += 2;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    out
}

/// One client file the snapshot folder keeps current.
pub struct ClientFile {
    /// The file name in both the package and the folder.
    pub name: &'static str,
    /// The mode the folder copy carries.
    pub mode: u32,
    /// Whether the folder copy uses CRLF line endings (the Windows wrapper).
    pub crlf: bool,
}

/// `syncFiles`: copies each file whose bytes differ; a file already current is left untouched.
pub fn sync_files(directory: &Path, source: &Path, files: &[ClientFile]) -> io::Result<()> {
    for file in files {
        let lf = read_packaged_source(&source.join(file.name))?;
        let text = String::from_utf8(lf).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "packaged source is not text")
        })?;
        let body = if file.crlf {
            text.replace('\n', "\r\n")
        } else {
            text
        };
        let current = read::read(
            &directory.join(file.name),
            &ReadPolicy::capped(PACKAGED_CAP),
        );
        let current_text = match current {
            Read::Bytes(bytes, _) => String::from_utf8(bytes).ok(),
            _ => None,
        };
        if current_text.as_deref() != Some(body.as_str()) {
            write_atomically(directory, file.name, &body, file.mode)?;
        }
    }
    Ok(())
}

/// The runner subfolder: made 0755 when missing; an existing non-folder or link is left alone
/// (the caller skips the sync), exactly as the TypeScript `lstat` guard.
pub fn runner_folder(directory: &Path, name: &str) -> io::Result<Option<std::path::PathBuf>> {
    let runner = directory.join(name);
    match fs::symlink_metadata(&runner) {
        Ok(meta) => {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                Ok(None)
            } else {
                Ok(Some(runner))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&runner)?;
            set_mode_dir(&runner, 0o755)?;
            Ok(Some(runner))
        }
        Err(error) => Err(error),
    }
}

fn set_mode_dir(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_planted_target_without_following_it() {
        let folder = tempfile::tempdir().expect("temp");
        write_atomically(folder.path(), "usage.json", "{}", 0o644).expect("write");
        assert_eq!(
            fs::read(folder.path().join("usage.json")).expect("read"),
            b"{}"
        );
        write_atomically(folder.path(), "usage.json", "{\"v\":1}", 0o644).expect("rewrite");
        assert_eq!(
            fs::read(folder.path().join("usage.json")).expect("read"),
            b"{\"v\":1}"
        );
        let leftovers: Vec<_> = fs::read_dir(folder.path())
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_is_replaced_not_written_through() {
        let folder = tempfile::tempdir().expect("temp");
        let victim = folder.path().join("victim.txt");
        fs::write(&victim, "keep me").expect("victim");
        std::os::unix::fs::symlink(&victim, folder.path().join("usage.json")).expect("link");
        write_atomically(folder.path(), "usage.json", "{}", 0o644).expect("write");
        assert_eq!(fs::read(&victim).expect("read"), b"keep me");
        assert!(
            !fs::symlink_metadata(folder.path().join("usage.json"))
                .expect("meta")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn lf_normalization_keeps_lone_cr() {
        assert_eq!(with_lf_line_endings(b"a\r\nb"), b"a\nb");
        assert_eq!(with_lf_line_endings(b"a\rb"), b"a\rb");
        assert_eq!(with_lf_line_endings(b"a\nb"), b"a\nb");
    }

    #[test]
    fn sync_leaves_current_files_untouched() {
        let source = tempfile::tempdir().expect("source");
        let target = tempfile::tempdir().expect("target");
        fs::write(source.path().join("reader"), "#!/bin/sh\necho hi\n").expect("source");
        let files = [ClientFile {
            name: "reader",
            mode: 0o755,
            crlf: false,
        }];
        sync_files(target.path(), source.path(), &files).expect("sync");
        let first = fs::metadata(target.path().join("reader"))
            .expect("meta")
            .modified()
            .expect("mtime");
        std::thread::sleep(std::time::Duration::from_millis(20));
        sync_files(target.path(), source.path(), &files).expect("resync");
        let second = fs::metadata(target.path().join("reader"))
            .expect("meta")
            .modified()
            .expect("mtime");
        assert_eq!(first, second);
    }
}
