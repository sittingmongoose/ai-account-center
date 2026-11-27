//! The reader files the snapshot folder keeps current: the `aac-usage` reader agents run, its
//! native launchers, its guide and the computer registry. Copies whose bytes differ are rewritten;
//! a file already current is left untouched. File mechanics live in `aac-files::share`.

use aac_files::share::{self, ClientFile};
use std::io;
use std::path::{Path, PathBuf};

/// The reader files, shipped in `scripts/agent-usage`.
pub const CLIENT_FILES: [ClientFile; 1] = [ClientFile {
    name: "README.md",
    mode: 0o644,
    crlf: false,
}];

/// The share keeps the portable Python reader under `aac-usage` (every computer's agents run
/// `python3 <share>/aac-usage`, and most computers have no native `ai-account-center` on PATH),
/// and the native launchers beside it as `aac-usage-native` for computers that do.
fn sync_native_launchers(directory: &Path) -> io::Result<()> {
    use aac_files::read::{self, Read, ReadPolicy};
    let crlf = |text: &str| text.replace("\r\n", "\n").replace('\n', "\r\n");
    for (name, body, mode) in [
        (
            "aac-usage",
            include_str!("../../../scripts/agent-usage/aac-usage").to_string(),
            0o755,
        ),
        (
            "aac-usage.cmd",
            crlf(include_str!("../../../scripts/agent-usage/aac-usage.cmd")),
            0o644,
        ),
        (
            "aac-usage-native",
            include_str!("../../../scripts/agent-usage/aac-usage-native.sh").to_string(),
            0o755,
        ),
        (
            "aac-usage-native.cmd",
            crlf(include_str!(
                "../../../scripts/agent-usage/aac-usage-native.cmd"
            )),
            0o644,
        ),
    ] {
        let same = matches!(read::read(&directory.join(name),&ReadPolicy::capped(256 * 1024)),Read::Bytes(b,_) if b==body.as_bytes());
        if !same {
            share::write_atomically(directory, name, &body, mode)?;
        }
    }
    Ok(())
}

/// Keep the reader, its native launchers, the guide and the computer registry current.
/// `computers_json` is the packaged registry path.
pub fn sync_snapshot_client(
    directory: &Path,
    source: &Path,
    computers_json: &Path,
) -> io::Result<()> {
    sync_native_launchers(directory)?;
    share::sync_files(directory, source, &CLIENT_FILES)?;
    let computers = share::read_packaged_source(computers_json)?;
    let computers_text = String::from_utf8(computers)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "computer registry is not text"))?;
    share::write_atomically(directory, "computers.json", &computers_text, 0o644)?;
    Ok(())
}

/// The packaged source folder, resolved from the package root the service runs from.
pub fn client_source_folder(package_root: &Path) -> PathBuf {
    package_root.join("scripts").join("agent-usage")
}

/// The packaged computer registry file.
pub fn computer_registry_file(package_root: &Path) -> PathBuf {
    package_root
        .join("web-dashboard")
        .join("public")
        .join("computers.json")
}
