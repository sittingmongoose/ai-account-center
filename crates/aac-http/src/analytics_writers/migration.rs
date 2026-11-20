//! Move the retired Analytics files only from the active store owner. The content-addressed
//! archive preserves data the current readers cannot interpret and survives partial cleanup.
use aac_files::read::{Ancestors, Read, ReadPolicy};
use aac_store::engine::Engine;
use aac_store::error::{Error, Result};
use aac_store::{activity, analytics_cache, collect, contract, legacy, report, sampler};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MARKER: &str = "analytics_files_migrated_v2";
pub(super) const QUOTA: &str = "legacy/account-analytics/quota-history-v1.json";
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

#[derive(Clone, Copy)]
enum Projection<'a> {
    Codex,
    Quota,
    Local,
    Remote(&'a str),
    Archive,
}

fn projection(key: &str) -> Projection<'_> {
    if key == "legacy/cache/codex-native-usage-v1.json" {
        Projection::Codex
    } else if key.starts_with(QUOTA) {
        Projection::Quota
    } else if key == "legacy/cache/account-activity-v1/analytics-activity-snapshot-v1.json" {
        Projection::Local
    } else if let Some(host) = key
        .strip_prefix("legacy/cache/analytics-remote-v1/")
        .and_then(|name| name.strip_suffix(".json"))
        .filter(|host| *host != "claude-mirror-owners" && contract::ident_ok(host))
    {
        Projection::Remote(host)
    } else {
        Projection::Archive
    }
}

pub(super) struct Migration<'a> {
    pub store: &'a Engine,
    pub root: &'a Path,
    pub local: Option<&'a str>,
    pub now: i64,
    pub pricing: super::ResolverPricing,
}

impl Migration<'_> {
    fn project(&self, key: &str, digest: &str, bytes: Option<&[u8]>, epoch: &str) -> Result<bool> {
        let kind = projection(key);
        if matches!(kind, Projection::Archive) {
            return Ok(true);
        }
        let receipt = match kind {
            Projection::Quota => format!("ts:backfill:{digest}"),
            _ => format!("analytics_import:{epoch}:{key}:{digest}"),
        };
        if matches!(kind, Projection::Codex) {
            if legacy::codex_v1_verified(self.store, digest)? {
                return Ok(true);
            }
        } else if self.store.get_meta(&receipt)?.is_some() {
            return Ok(true);
        }
        let archived;
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => {
                archived = analytics_cache::archived(self.store, key)?
                    .ok_or_else(|| Error::new("io", "missing staged Analytics file"))?;
                &archived
            }
        };
        match kind {
            Projection::Codex => {
                let Some(computer) = self.local else {
                    return Ok(false);
                };
                legacy::import_codex_v1_bytes(self.store, bytes, computer)?;
                return legacy::codex_v1_verified(self.store, digest);
            }
            // The sampler uses the Accounts provider map and publishes its receipt. A malformed
            // temporary file has no complete quota observation, so its verified archive suffices.
            Projection::Quota => {
                return Ok(key.ends_with(".tmp")
                    && !std::str::from_utf8(bytes)
                        .ok()
                        .is_some_and(|text| sampler::records_from_text(text).1));
            }
            Projection::Local => {
                let Some(computer) = self.local else {
                    return Ok(false);
                };
                let snapshot: Value = serde_json::from_slice(bytes)?;
                let Some(local) = snapshot["local"]
                    .as_array()
                    .filter(|_| snapshot["version"] == 3)
                else {
                    return Ok(false);
                };
                let mut tools = BTreeMap::<(&str, &str), Vec<Value>>::new();
                for source in local.iter().map(|entry| &entry[1]) {
                    if let (Some(tool), Some(data)) =
                        (source["provider"].as_str(), source["data"].as_array())
                    {
                        tools
                            .entry((computer, tool))
                            .or_default()
                            .extend(data.iter().cloned());
                    }
                }
                for result in snapshot["remote"]["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let data = &result["data"];
                    if let (Some(host), Some(tool)) = (
                        result["host"].as_str().or_else(|| data["host"].as_str()),
                        result["tool"].as_str(),
                    ) {
                        tools.entry((host, tool)).or_default().push(data.clone());
                    }
                }
                for ((computer, tool), data) in tools {
                    self.import_rows(computer, tool, &data)?;
                }
            }
            Projection::Remote(computer) => {
                let cache: super::scan_state::Cache = serde_json::from_slice(bytes)?;
                for tool in aac_providers::scanner_kinds() {
                    let rows: Vec<_> = cache
                        .rows
                        .iter()
                        .filter(|row| row["k"] == tool)
                        .cloned()
                        .collect();
                    let srows: Vec<_> = cache
                        .srows
                        .iter()
                        .filter(|row| row["k"] == tool)
                        .cloned()
                        .collect();
                    if !rows.is_empty() || !srows.is_empty() {
                        let worker = collect::worker_from_scan(
                            &rows,
                            &srows,
                            tool,
                            computer,
                            self.now,
                            &self.pricing,
                        );
                        self.import_rows(computer, tool, &[worker])?;
                    }
                }
            }
            Projection::Archive => return Ok(true),
        }
        self.store.set_meta(&receipt, "1")?;
        Ok(self.store.get_meta(&receipt)?.is_some())
    }

    fn import_rows(&self, computer: &str, tool: &str, data: &[Value]) -> Result<()> {
        let value = report::rows_report(&report::RowsReportArgs {
            computer,
            tool,
            data,
            kind: "import",
            read_at: self.now,
            complete: false,
            cause: Some("retired_cache"),
            now: self.now,
        });
        super::history::commit_report(self.store, value)
    }

    fn ready(&self, key: &str, digest: &str, bytes: Option<&[u8]>, epoch: &str) -> bool {
        match self.project(key, digest, bytes, epoch) {
            Ok(ready) => ready,
            Err(error) => {
                tracing::warn!(code = error.code, "Analytics source import incomplete");
                false
            }
        }
    }

    fn flush(
        &self,
        paths: &[PathBuf],
        batch: &[(String, Vec<u8>)],
        epoch: &str,
        before_remove: &mut impl FnMut() -> Result<()>,
    ) -> Result<bool> {
        let mut complete = true;
        analytics_cache::archive(self.store, batch)?;
        for (file, (key, bytes)) in paths.iter().zip(batch) {
            if analytics_cache::archived(self.store, key)?.as_deref() != Some(bytes) {
                return Err(Error::new("io", "Analytics migration verification failed"));
            }
            if !self.ready(key, &aac_core::sha256_hex(bytes), Some(bytes), epoch) {
                complete = false;
                continue;
            }
            before_remove()?;
            match read(self.root, file)? {
                Some(current) if current == *bytes => fs::remove_file(file)?,
                Some(_) => return Err(Error::new("io", "Analytics migration source changed")),
                None => {}
            }
        }
        Ok(complete)
    }

    pub(super) fn run(&self) -> Result<bool> {
        self.migrate(|| Ok(()))
    }

    fn migrate(&self, mut before_remove: impl FnMut() -> Result<()>) -> Result<bool> {
        let epoch = activity::recovery_epoch(self.store)?.to_string();
        if self.store.get_meta(MARKER)?.as_deref() == Some(&epoch) {
            return Ok(true);
        }
        let mut complete = true;
        let mut batch = Vec::new();
        let mut pending = Vec::new();
        let mut size = 0;
        for path in candidates(self.root)? {
            let Some(bytes) = read(self.root, &path)? else {
                continue;
            };
            if size + bytes.len() > BATCH_BYTES && !batch.is_empty() {
                complete &= self.flush(&pending, &batch, &epoch, &mut before_remove)?;
                batch.clear();
                pending.clear();
                size = 0;
            }
            let relative = path.strip_prefix(self.root)?;
            let key = format!("legacy/{}", relative.to_string_lossy().replace('\\', "/"));
            size += bytes.len();
            batch.push((key, bytes));
            pending.push(path);
        }
        if !batch.is_empty() {
            complete &= self.flush(&pending, &batch, &epoch, &mut before_remove)?;
        }
        // Recover copies staged by the earlier migration, and replay after a projection rebuild.
        for prefix in [
            "legacy/cache/codex-native-usage-v1.json",
            QUOTA,
            "legacy/cache/analytics-remote-v1/",
            "legacy/cache/account-activity-v1/analytics-activity-snapshot-v1.json",
        ] {
            for (key, digest) in analytics_cache::archived_index(self.store, prefix)? {
                complete &= self.ready(&key, &digest, None, &epoch);
            }
        }
        if complete {
            self.store.set_meta(MARKER, &epoch)?;
        }
        Ok(complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture<'a>(store: &'a Engine, root: &'a Path) -> Migration<'a> {
        Migration {
            store,
            root,
            local: Some("ubuntu"),
            now: aac_store::engine::now_ms() as i64,
            pricing: super::super::ResolverPricing::new(root),
        }
    }

    #[test]
    fn analytics_migration_projects_the_only_codex_copy_before_cleanup() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let dir = root.join("store");
        let open = || {
            aac_store::ops::open_engine(&dir.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let path = root.join("cache/codex-native-usage-v1.json");
        fs::create_dir_all(root.join("cache")).expect("fixture cache folder");
        let now = aac_store::engine::now_ms();
        let at = (now - 60 * 86_400_000) / 3_600_000 * 3_600_000;
        let time = aac_num::ecma::to_iso_string(at as i64).expect("fixture time");
        let body = json!({"version":1,"files":{"missing.jsonl":{"entries":[{
            "inputTokens":70,"outputTokens":7,"model":"gpt-6-astra","sessionId":"historical-fixture",
            "timestamp":time
        }]}}});
        fs::write(&path, body.to_string()).expect("only surviving source");
        let check = |store: &Engine| {
            let result = aac_store::ops::call(
                store,
                "activityRange",
                &json!({
                    "fromMs":at,"toMs":at + 3_600_000
                })
                .to_string(),
            )
            .expect("query recovered history");
            let range: Value = result;
            assert_eq!(range["rows"][0][5], 70);
            assert_eq!(range["rows"][0][6], 7);
            assert_eq!(range["rows"].as_array().expect("hourly rows").len(), 1);
        };
        let engine = open();
        assert!(
            fixture(&engine, root)
                .migrate(|| {
                    check(&engine);
                    Err(Error::new(
                        "io",
                        "interrupted before removing the only source",
                    ))
                })
                .is_err()
        );
        assert!(path.is_file());
        engine.close().expect("close interrupted store");
        drop(engine);

        let engine = open();
        fixture(&engine, root).run().expect("resume migration");
        assert!(!path.exists());
        check(&engine);
        let scans = aac_store::activity::counts(&engine).expect("scan count")["scans"].clone();
        engine.close().expect("close migrated store");
        drop(engine);

        let engine = open();
        fixture(&engine, root).run().expect("already migrated");
        check(&engine);
        assert_eq!(
            aac_store::activity::counts(&engine).expect("scan count")["scans"],
            scans
        );
        engine.close().expect("close for projection rebuild");
        drop(engine);
        fs::remove_file(dir.join("aac.redb")).expect("discard the fixture projection");
        let engine = open();
        check(&engine);

        let staged_only = tempfile::tempdir().expect("restore an earlier staging-only archive");
        let restored_root = staged_only.path();
        let restored =
            aac_store::ops::open_engine(&restored_root.join("store").to_string_lossy(), None, None)
                .expect("open restored fixture");
        analytics_cache::archive(
            &restored,
            &[(
                "legacy/cache/codex-native-usage-v1.json".to_owned(),
                body.to_string().into_bytes(),
            )],
        )
        .expect("stage the prior archive");
        restored
            .set_meta("analytics_files_migrated_v1", "1")
            .expect("legacy migration marker");
        assert!(
            fixture(&restored, restored_root)
                .run()
                .expect("recover staged-only history")
        );
        check(&restored);
    }

    #[test]
    fn analytics_migration_leaves_sources_until_an_import_is_verified() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let store = aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None)
            .expect("open fixture store");
        let codex = "cache/codex-native-usage-v1.json";
        let quota = "account-analytics/quota-history-v1.json";
        let body = b"{\"records\":[]}";
        for (name, bytes) in [
            (codex, &b"{\"version\":2,\"files\":{}}"[..]),
            (quota, &body[..]),
        ] {
            let path = root.join(name);
            fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("create fixture directory");
            fs::write(path, bytes).expect("write legacy source");
        }
        assert!(!fixture(&store, root).run().expect("stage unverified files"));
        assert!(root.join(codex).exists());
        assert!(root.join(quota).exists());
        assert!(
            store
                .get_meta(MARKER)
                .expect("read migration marker")
                .is_none()
        );
        assert_eq!(
            analytics_cache::archived(&store, QUOTA)
                .expect("staged quota bytes")
                .as_deref(),
            Some(&body[..])
        );
    }

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
                &b"{\"records\":[]}"[..],
            ),
            (
                "account-analytics/quota-history-v1.json.stale.tmp",
                b"truncated history",
            ),
            (
                "cache/account-activity-v1/codex/checkpoint.json",
                b"{\"offset\":900}",
            ),
            (
                "cache/analytics-remote-v1/mac.json",
                b"{\"fingerprints\":{},\"rows\":[1,2,3]}",
            ),
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
        // The quota sampler already confirmed this source, so migration can retire it.
        sampler::backfill(
            &engine,
            &[sampler::BackfillSource {
                label: "quota-history-v1.json".to_owned(),
                sha256: aac_core::sha256_hex(files[0].1),
                records: Vec::new(),
            }],
            &Default::default(),
            aac_store::engine::now_ms() as i64,
            0,
        )
        .expect("commit quota import receipt");
        let other = root.join("cache/another-writer.tmp");
        fs::write(&other, b"keep").expect("write unrelated fixture");
        let mut seen = 0;
        assert!(
            fixture(&engine, root)
                .migrate(|| {
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
        fixture(&engine, root)
            .run()
            .expect("migrate legacy fixtures");
        for (name, bytes) in files {
            assert!(!root.join(name).exists(), "{name} was not retired");
            assert_eq!(
                analytics_cache::archived(&engine, &format!("legacy/{name}"))
                    .expect("read archived fixture")
                    .as_deref(),
                Some(bytes)
            );
        }
        assert_eq!(fs::read(&other).expect("read unrelated fixture"), b"keep");
        assert!(
            fixture(&engine, root)
                .migrate(|| panic!("already migrated"))
                .is_ok()
        );
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
        fixture(&engine, root)
            .run()
            .expect("migrate legacy fixtures");
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
