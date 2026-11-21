//! L1 build check: what this running service is. TS records it in the boot descriptor and an
//! attaching front door refuses a boot whose identity differs from its own binary's. It travels
//! only on authenticated replies to `/api/health` as `x-aac-service-build`; the public health
//! body stays `{"status":"ok"}`.
use axum::http::HeaderValue;
use sha2::{Digest, Sha256};
use std::{fs::File, io, io::Read, path::PathBuf};

/// The private socket protocol. 1: M3; 2: L1 (table dispatch, named declines, feeds,
/// events); 3: R3 (the store host's internal route and boot store config).
pub const PROTOCOL: u32 = 3;

/// The header carrying the identity.
pub(crate) const HEADER: &str = "x-aac-service-build";
pub(crate) const LIFECYCLE_HEADER: &str = "x-aac-lifecycle-owner";

/// `{commit, sha256, protocol, storeSchema}`, already encoded as the header value.
#[derive(Clone)]
pub(crate) struct Identity(HeaderValue);

/// The running image: on Linux `/proc/self/exe` still names the bytes this process runs even
/// after an in-place upgrade replaced the file.
fn executable() -> io::Result<PathBuf> {
    let proc_exe = PathBuf::from("/proc/self/exe");
    if proc_exe.exists() {
        return Ok(proc_exe);
    }
    std::env::current_exe()
}

/// The installed binary's location for finding assets next to it. Some Unix
/// launchers report the path of the package-manager symlink.
pub(crate) fn executable_path_for_assets() -> io::Result<PathBuf> {
    std::env::current_exe().map(resolve_asset_path)
}

pub(crate) fn resolve_asset_path(path: PathBuf) -> PathBuf {
    // A running executable can lose its directory entry during an upgrade.
    #[cfg(unix)]
    let path = path.canonicalize().unwrap_or(path);
    path
}

fn sha256_hex(path: &PathBuf) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

impl Identity {
    /// Hash this process's own executable once, at start.
    pub(crate) fn current() -> io::Result<Self> {
        Self::for_binary(&executable()?)
    }

    pub(crate) fn for_binary(binary: &PathBuf) -> io::Result<Self> {
        let sha256 = sha256_hex(binary)?;
        let value = serde_json::json!({
            "commit": env!("AAC_BUILD_COMMIT"),
            "sha256": sha256,
            "protocol": PROTOCOL,
            "storeSchema": aac_store::db::STORE_SCHEMA_VERSION,
        });
        HeaderValue::from_str(&value.to_string())
            .map(Self)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "identity"))
    }

    pub(crate) fn header(&self) -> HeaderValue {
        self.0.clone()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::resolve_asset_path;
    use std::os::unix::fs::symlink;
    use std::{fs, path::Path};

    #[test]
    fn package_assets_follow_an_installed_executable_symlink() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let prefix = fixture.path();
        let installed = Path::new("lib/node_modules/fixture-package/dist/native/ai-account-center");
        let executable = prefix.join(installed);
        fs::create_dir_all(executable.parent().expect("native directory"))
            .expect("native directory");
        fs::create_dir(prefix.join("bin")).expect("bin directory");
        fs::write(&executable, b"fixture").expect("installed binary");
        let shim = prefix.join("bin/ai-account-center");
        symlink(Path::new("..").join(installed), &shim).expect("installed bin symlink");

        assert_eq!(
            resolve_asset_path(shim),
            executable.canonicalize().expect("installed binary path")
        );
        let replaced = prefix.join("dist/native/replaced");
        assert_eq!(resolve_asset_path(replaced.clone()), replaced);
    }
}
