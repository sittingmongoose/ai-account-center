//! The native reader launchers, guide and computer registry the snapshot folder keeps current.
//! Copies whose bytes differ are rewritten; a file already current is left untouched. File
//! mechanics live in `aac-files::share`.

use aac_files::share::{self, ClientFile};
use std::io;
use std::path::{Path, PathBuf};

/// The reader guide shipped in `scripts/agent-usage`.
pub const CLIENT_FILES: [ClientFile; 1] = [ClientFile {
    name: "README.md",
    mode: 0o644,
    crlf: false,
}];

fn sync_native_launchers(directory: &Path) -> io::Result<()> {
    use aac_files::read::{self, Read, ReadPolicy};
    for (name, body, mode) in [
        (
            "aac-usage",
            include_str!("../../../scripts/agent-usage/aac-usage-native.sh").to_string(),
            0o755,
        ),
        (
            "aac-usage.cmd",
            include_str!("../../../scripts/agent-usage/aac-usage-native.cmd")
                .replace("\r\n", "\n")
                .replace('\n', "\r\n"),
            0o644,
        ),
    ] {
        let same = matches!(read::read(&directory.join(name),&ReadPolicy::capped(8192)),Read::Bytes(b,_) if b==body.as_bytes());
        if !same {
            share::write_atomically(directory, name, &body, mode)?;
        }
    }
    Ok(())
}

/// Keep the native reader launchers, guide and computer registry current.
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
