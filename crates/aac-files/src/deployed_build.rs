//! Deploy lineage (cut gap G20): the build an installed dashboard runs, recorded so the next
//! installation can refuse a package that would remove it (`scripts/deploy-lineage.js`,
//! `scripts/postinstall.js`). Source checkouts record nothing. The Rust port of
//! `src/web-server/services/deployed-build.ts`: same validation, same directories, same record
//! bytes, same unchanged-record shortcut.

use crate::read::{self, Read, ReadPolicy};
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The ledger file name every installer reads.
pub const LEDGER: &str = "deployed-build.json";
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The running build: a 40-hex commit, an optional safe branch name, and the dirty flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagedBuild {
    /// The commit the package was built from.
    pub commit: String,
    /// The branch name, when it is a safe identifier.
    pub branch: Option<String>,
    /// Whether the build carried uncommitted changes.
    pub dirty: bool,
}

fn is_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `safeBranch`: an identifier start, then up to 127 branch characters.
fn safe_branch(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let bytes = s.as_bytes();
    if !(1..=128).contains(&bytes.len()) || !bytes[0].is_ascii_alphanumeric() {
        return None;
    }
    if !bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(b))
    {
        return None;
    }
    Some(s.to_owned())
}

/// `readPackagedBuild`: `dist/build-info.json` of the running package; None for a source
/// checkout or an older build. A `.git` folder or `src/ccs.ts` marks a checkout.
pub fn read_packaged_build(package_root: &Path) -> Option<PackagedBuild> {
    if package_root.join(".git").exists() || package_root.join("src").join("ccs.ts").exists() {
        return None;
    }
    let file = package_root.join("dist").join("build-info.json");
    let meta = fs::symlink_metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let Read::Bytes(bytes, _) = read::read(&file, &ReadPolicy::capped(MAX_FILE_BYTES)) else {
        return None;
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let info = value.as_object()?;
    if info.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let commit = info
        .get("commit")
        .and_then(Value::as_str)
        .filter(|c| is_commit(c))?;
    let dirty = info.get("dirty").and_then(Value::as_bool)?;
    Some(PackagedBuild {
        commit: commit.to_owned(),
        branch: safe_branch(info.get("branch").unwrap_or(&Value::Null)),
        dirty,
    })
}

/// `path.resolve` against the process working directory, as the TypeScript single-argument
/// form reads it.
fn resolved(path: &Path) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    crate::path::node::resolve(&cwd, path).ok()
}

/// `deployedBuildDirs`: every directory an installer may read the record from — the active CCS
/// directory (which `--config-dir` can move), the one CCS_DIR or CCS_HOME names, and the default
/// `~/.ccs`. Duplicates collapse, first occurrence kept.
pub fn deployed_build_dirs(
    active: &Path,
    ccs_dir: Option<&str>,
    ccs_home: Option<&str>,
    home: &Path,
) -> Vec<PathBuf> {
    let named = match ccs_dir.filter(|value| !value.is_empty()) {
        Some(dir) => resolved(Path::new(dir)),
        None => {
            let base = ccs_home
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.to_path_buf());
            resolved(&base).map(|base| base.join(".ccs"))
        }
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for candidate in [resolved(active), named, resolved(&home.join(".ccs"))]
        .into_iter()
        .flatten()
    {
        if !dirs.contains(&candidate) {
            dirs.push(candidate);
        }
    }
    dirs
}

/// `recordDeployedBuild`: write `<ccsDir>/deployed-build.json` atomically; an unchanged record
/// is left alone. Ok(false) without a build; a write error is the directory's failure.
pub fn record_deployed_build(
    ccs_dir: &Path,
    build: Option<&PackagedBuild>,
    now_ms: i64,
) -> io::Result<bool> {
    let Some(build) = build else { return Ok(false) };
    let file = ccs_dir.join(LEDGER);
    if let Read::Bytes(bytes, _) = read::read(&file, &ReadPolicy::capped(MAX_FILE_BYTES))
        && let Ok(Value::Object(previous)) = serde_json::from_slice::<Value>(&bytes)
    {
        let same_commit =
            previous.get("commit").and_then(Value::as_str) == Some(build.commit.as_str());
        let same_dirty = previous.get("dirty").and_then(Value::as_bool) == Some(build.dirty);
        let same_branch = match &build.branch {
            Some(branch) => previous.get("branch").and_then(Value::as_str) == Some(branch.as_str()),
            None => previous.get("branch") == Some(&Value::Null),
        };
        if same_commit && same_dirty && same_branch {
            return Ok(true);
        }
    }
    let recorded_at = aac_num::ecma::to_iso_string(now_ms).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "record time outside the ISO domain",
        )
    })?;
    let body = format!(
        "{{\n  \"version\": 1,\n  \"commit\": {},\n  \"branch\": {},\n  \"dirty\": {},\n  \"recordedAt\": {}\n}}\n",
        json_string(&build.commit),
        match &build.branch {
            Some(branch) => json_string(branch),
            None => "null".to_owned(),
        },
        build.dirty,
        json_string(&recorded_at)
    );
    let temporary = file.with_file_name(format!("{LEDGER}.{}.tmp", std::process::id()));
    let result = (|| -> io::Result<()> {
        let mut staged = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        set_private_mode(&staged)?;
        staged.write_all(body.as_bytes())?;
        staged.flush()?;
        drop(staged);
        fs::rename(&temporary, &file)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(true)
}

/// `JSON.stringify` of one string value (serde_json's escaping is the JSON escape set).
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn set_private_mode(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// `recordRunningBuild`: record the running build everywhere an installer may look; returns
/// the number of failed directories. The installer reads these even before anything else
/// created them, so missing directories are made 0700.
pub fn record_running_build(
    active: &Path,
    build: Option<&PackagedBuild>,
    ccs_dir: Option<&str>,
    ccs_home: Option<&str>,
    home: &Path,
    now_ms: i64,
) -> usize {
    if build.is_none() {
        return 0;
    }
    let mut failed = 0;
    for dir in deployed_build_dirs(active, ccs_dir, ccs_home, home) {
        let outcome = (|| -> io::Result<()> {
            let existed = dir.exists();
            fs::create_dir_all(&dir)?;
            if !existed {
                set_dir_private(&dir)?;
            }
            record_deployed_build(&dir, build, now_ms)?;
            Ok(())
        })();
        if outcome.is_err() {
            failed += 1;
        }
    }
    failed
}

fn set_dir_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn packaged(root: &Path, info: Value) {
        fs::create_dir_all(root.join("dist")).expect("dist");
        fs::write(root.join("dist").join("build-info.json"), info.to_string()).expect("info");
    }

    #[test]
    fn reads_an_installed_package_and_ignores_source_checkouts() {
        let scope = tempfile::tempdir().expect("scope");
        let root = scope.path().join("package");
        packaged(
            &root,
            json!({"version": 1, "commit": C, "branch": "main", "dirty": false, "onMain": true, "lineage": [C]}),
        );
        assert_eq!(
            read_packaged_build(&root),
            Some(PackagedBuild {
                commit: C.into(),
                branch: Some("main".into()),
                dirty: false
            })
        );
        packaged(
            &root,
            json!({"version": 1, "commit": null, "dirty": null, "lineage": []}),
        );
        assert_eq!(read_packaged_build(&root), None);
        fs::create_dir_all(root.join(".git")).expect("git");
        packaged(
            &root,
            json!({"version": 1, "commit": C, "branch": "main", "dirty": false}),
        );
        assert_eq!(read_packaged_build(&root), None);
    }

    #[test]
    fn validates_commit_branch_and_dirty() {
        let scope = tempfile::tempdir().expect("scope");
        let root = scope.path().join("package");
        packaged(
            &root,
            json!({"version": 1, "commit": "xyz", "branch": "main", "dirty": false}),
        );
        assert_eq!(read_packaged_build(&root), None);
        packaged(
            &root,
            json!({"version": 1, "commit": B, "branch": "bad branch!", "dirty": false}),
        );
        assert_eq!(read_packaged_build(&root).expect("build").branch, None);
        packaged(
            &root,
            json!({"version": 1, "commit": B, "branch": "main", "dirty": "no"}),
        );
        assert_eq!(read_packaged_build(&root), None);
        packaged(
            &root,
            json!({"version": 2, "commit": B, "branch": "main", "dirty": false}),
        );
        assert_eq!(read_packaged_build(&root), None);
    }

    #[test]
    fn records_where_an_installer_looks() {
        let scope = tempfile::tempdir().expect("scope");
        let home = scope.path().join("user");
        let custom = scope.path().join("custom-config");
        fs::create_dir_all(&custom).expect("custom");
        let named = scope.path().join("named");
        let build = PackagedBuild {
            commit: B.into(),
            branch: Some("main".into()),
            dirty: false,
        };
        let dirs = deployed_build_dirs(&custom, None, Some(named.to_str().expect("utf8")), &home);
        assert_eq!(
            dirs,
            vec![custom.clone(), named.join(".ccs"), home.join(".ccs")]
        );
        let failed = record_running_build(
            &custom,
            Some(&build),
            None,
            Some(named.to_str().expect("utf8")),
            &home,
            1_792_065_600_000,
        );
        assert_eq!(failed, 0);
        for dir in [&custom, &named.join(".ccs"), &home.join(".ccs")] {
            let body = fs::read_to_string(dir.join(LEDGER)).expect("ledger");
            let value: Value = serde_json::from_str(&body).expect("parse");
            assert_eq!(value["commit"], json!(B));
            assert_eq!(value["branch"], json!("main"));
            assert_eq!(value["dirty"], json!(false));
            assert_eq!(value["version"], json!(1));
            assert!(
                value["recordedAt"]
                    .as_str()
                    .expect("recordedAt")
                    .ends_with("Z")
            );
        }
        assert_eq!(record_running_build(&custom, None, None, None, &home, 0), 0);
    }

    #[test]
    fn dedups_the_active_and_default_directories() {
        let scope = tempfile::tempdir().expect("scope");
        let home = scope.path().join("user");
        let active = home.join(".ccs");
        let dirs = deployed_build_dirs(&active, None, None, &home);
        assert_eq!(dirs, vec![active.clone()]);
        let ccs_dir = scope.path().join("elsewhere");
        let dirs = deployed_build_dirs(&active, Some(ccs_dir.to_str().expect("utf8")), None, &home);
        assert_eq!(dirs, vec![active, ccs_dir]);
    }

    #[test]
    fn leaves_an_unchanged_record_alone() {
        let scope = tempfile::tempdir().expect("scope");
        let ccs_dir = scope.path().join(".ccs");
        fs::create_dir_all(&ccs_dir).expect("ccs");
        let build = PackagedBuild {
            commit: B.into(),
            branch: Some("main".into()),
            dirty: false,
        };
        assert!(record_deployed_build(&ccs_dir, Some(&build), 1_792_065_600_000).expect("record"));
        let file = ccs_dir.join(LEDGER);
        let first = fs::read_to_string(&file).expect("read");
        assert!(
            record_deployed_build(&ccs_dir, Some(&build), 1_792_069_200_000).expect("rerecord")
        );
        assert_eq!(fs::read_to_string(&file).expect("read"), first);
        let entries: Vec<_> = fs::read_dir(&ccs_dir)
            .expect("list")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from(LEDGER)]);
        assert!(!record_deployed_build(&ccs_dir, None, 0).expect("none"));
        let changed = PackagedBuild {
            commit: A.into(),
            branch: Some("main".into()),
            dirty: false,
        };
        assert!(
            record_deployed_build(&ccs_dir, Some(&changed), 1_792_069_200_000).expect("change")
        );
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("read")).expect("parse");
        assert_eq!(value["commit"], json!(A));
    }

    #[cfg(unix)]
    #[test]
    fn the_record_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let scope = tempfile::tempdir().expect("scope");
        let build = PackagedBuild {
            commit: B.into(),
            branch: None,
            dirty: false,
        };
        record_deployed_build(scope.path(), Some(&build), 1_792_065_600_000).expect("record");
        let mode = fs::metadata(scope.path().join(LEDGER))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let body = fs::read_to_string(scope.path().join(LEDGER)).expect("read");
        assert!(body.contains("\"branch\": null"));
    }
}
