//! Move the retired Analytics files only from the active store owner. The content-addressed
//! archive preserves data the current readers cannot interpret and survives partial cleanup.
use aac_files::read::{Ancestors, Read, ReadPolicy};
use aac_store::analytics_cache;
use aac_store::engine::Engine;
use aac_store::error::{Error, Result};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MARKER: &str = "analytics_files_migrated_v1";
const MAX_FILE: u64 = 256 * 1024 * 1024;
const BATCH_BYTES: usize = 8 * 1024 * 1024;

fn files_under(
    root: &Path,
    depth: usize,
    accept: fn(&str) -> bool,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    match root.symlink_metadata() {
        Ok(meta) if meta.file_type().is_dir() => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        _ => {
            return Err(Error::new(
                "io",
                "Analytics cache directory is not a regular directory",
            ));
        }
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() && depth > 0 {
            files_under(&entry.path(), depth - 1, accept, files)?;
        } else if accept(&entry.file_name().to_string_lossy()) {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn candidates(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let json = |name: &str| name.ends_with(".json");
    let cache = root.join("cache");
    files_under(
        &cache,
        0,
        |name| {
            matches!(
                name,
                "cursor-account-v1.json"
                    | "cursor-quota-v1.json"
                    | "omp-session-roots-v1.json"
                    | "experiment-usage-roots-v1.json"
                    | "experiment-usage-walk-v1.json"
                    | "codex-native-usage-v1.json"
                    | "usage.json"
            )
        },
        &mut paths,
    )?;
    files_under(&cache.join("analytics-remote-v1"), 0, json, &mut paths)?;
    files_under(
        &cache.join("account-activity-passes-v1"),
        0,
        json,
        &mut paths,
    )?;
    files_under(&cache.join("account-activity-v1"), 2, json, &mut paths)?;
    files_under(
        &root.join("account-analytics"),
        0,
        |name| {
            matches!(name, "quota-history-v1.json" | "analytics-views-v1.json")
                || name.starts_with("quota-history-v1.json.") && name.ends_with(".tmp")
        },
        &mut paths,
    )?;
    paths.sort_unstable();
    Ok(paths)
}

fn read(root: &Path, file: &Path) -> Result<Option<Vec<u8>>> {
    let policy = ReadPolicy::capped(MAX_FILE).ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(file, &policy) {
        Read::Bytes(bytes, _) => Ok(Some(bytes)),
        Read::Absent => Ok(None),
        other => Err(Error::new(
            "io",
            format!("Analytics migration read: {other:?}"),
        )),
    }
}

fn flush(
    engine: &Engine,
    root: &Path,
    paths: &[PathBuf],
    batch: &[(String, Vec<u8>)],
    before_remove: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    analytics_cache::archive(engine, batch)?;
    for (file, (key, bytes)) in paths.iter().zip(batch) {
        if analytics_cache::archived(engine, key)?.as_deref() != Some(bytes) {
            return Err(Error::new("io", "Analytics migration verification failed"));
        }
        before_remove()?;
        match read(root, file)? {
            Some(current) if current == *bytes => fs::remove_file(file)?,
            Some(_) => return Err(Error::new("io", "Analytics migration source changed")),
            None => {}
        }
    }
    Ok(())
}

pub(super) fn run(engine: &Engine, root: &Path) -> Result<()> {
    migrate(engine, root, || Ok(()))
}

fn migrate(
    engine: &Engine,
    root: &Path,
    mut before_remove: impl FnMut() -> Result<()>,
) -> Result<()> {
    if engine.get_meta(MARKER)?.is_some() {
        return Ok(());
    }
    let mut batch = Vec::new();
    let mut pending = Vec::new();
    let mut size = 0;
    for path in candidates(root)? {
        let Some(bytes) = read(root, &path)? else {
            continue;
        };
        if size + bytes.len() > BATCH_BYTES && !batch.is_empty() {
            flush(engine, root, &pending, &batch, &mut before_remove)?;
            batch.clear();
            pending.clear();
            size = 0;
        }
        let relative = path.strip_prefix(root)?;
        let key = format!("legacy/{}", relative.to_string_lossy().replace('\\', "/"));
        size += bytes.len();
        batch.push((key, bytes));
        pending.push(path);
    }
    if !batch.is_empty() {
        flush(engine, root, &pending, &batch, &mut before_remove)?;
    }
    engine.set_meta(MARKER, "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_verifies_resumes_and_leaves_unrelated_history_alone() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let store = root.join("store");
        let engine = aac_store::ops::open_engine(&store.to_string_lossy(), None, None)
            .expect("open fixture store");
        let files = [
            (
                "account-analytics/quota-history-v1.json",
                &b"{\"readings\":[23]}"[..],
            ),
            (
                "account-analytics/quota-history-v1.json.stale.tmp",
                b"truncated history",
            ),
            (
                "cache/account-activity-v1/codex/checkpoint.json",
                b"{\"offset\":900}",
            ),
            ("cache/analytics-remote-v1/mac.json", b"{\"rows\":[1,2,3]}"),
            ("cache/cursor-quota-v1.json", b"{\"used\":49}"),
            (
                "cache/codex-native-usage-v1.json",
                b"{\"version\":1,\"files\":{}}",
            ),
        ];
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("create fixture directory");
            fs::write(path, bytes).expect("write legacy fixture");
        }
        let other = root.join("cache/another-writer.tmp");
        fs::write(&other, b"keep").expect("write unrelated fixture");
        let mut seen = 0;
        assert!(
            migrate(&engine, root, || {
                seen += 1;
                if seen == 2 {
                    Err(Error::new("io", "interrupted"))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(!root.join(files[0].0).exists());
        assert!(root.join(files[1].0).exists());
        assert!(
            engine
                .get_meta(MARKER)
                .expect("read migration marker")
                .is_none()
        );
        run(&engine, root).expect("migrate legacy fixtures");
        for (name, bytes) in files {
            assert!(!root.join(name).exists());
            assert_eq!(
                analytics_cache::archived(&engine, &format!("legacy/{name}"))
                    .expect("read archived fixture")
                    .as_deref(),
                Some(bytes)
            );
        }
        assert_eq!(fs::read(&other).expect("read unrelated fixture"), b"keep");
        assert!(migrate(&engine, root, || panic!("already migrated")).is_ok());
    }

    #[test]
    fn migration_preserves_rows_and_capture_can_remove_archived_folders() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let engine = aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None)
            .expect("open fixture store");
        let name = "cache/analytics-remote-v1/mac.json";
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        fs::write(
            &path,
            br#"{"fingerprints":{},"rows":[{"i":17}],"sessions":{"one":{"cwd":"/private"}}}"#,
        )
        .expect("write fixture record");
        run(&engine, root).expect("migrate legacy fixtures");
        assert!(!path.exists());
        assert_eq!(
            analytics_cache::clear_remote_folders(&engine).expect("clear archived working folders"),
            1
        );
        assert_eq!(
            analytics_cache::clear_remote_folders(&engine).expect("clear archived working folders"),
            0
        );
        let bytes = analytics_cache::archived(&engine, &format!("legacy/{name}"))
            .expect("read archived cache")
            .expect("archived cache exists");
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("decode archived cache");
        assert_eq!(value["rows"][0]["i"], 17);
        assert_eq!(value["sessions"]["one"]["cwd"], serde_json::Value::Null);
    }
}
