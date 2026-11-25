//! Move the retired Analytics files only from the active store owner. The content-addressed
//! archive preserves data the current readers cannot interpret and survives partial cleanup.
use aac_files::read::{Ancestors, Read, ReadPolicy};
use aac_store::engine::Engine;
use aac_store::error::{Error, Result};
use aac_store::{activity, analytics_cache, legacy, report, sampler};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod mirror_tests;
mod remote;

const MARKER: &str = "analytics_files_migrated_v5";
pub(super) const QUOTA: &str = "legacy/account-analytics/quota-history-v1.json";
const MAX_FILE: u64 = 256 * 1024 * 1024;
const BATCH_BYTES: usize = 8 * 1024 * 1024;
const FAILURES: &str = "analytics_migration_failures";
const RETRY_MS: i64 = 120_000;
const MAX_RETRY_MS: i64 = 3_600_000;

#[derive(PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure {
    reason: String,
    digest: Option<String>,
    attempts: u8,
    retry_at_ms: i64,
}

#[derive(Default, PartialEq, Deserialize, Serialize)]
struct Failures(BTreeMap<String, Failure>);

impl Failures {
    fn waiting(&self, key: &str, now: i64) -> bool {
        self.0.get(key).is_some_and(|entry| now < entry.retry_at_ms)
    }

    fn record(&mut self, key: &str, digest: Option<&str>, error: &Error, now: i64) {
        let attempts = self
            .0
            .get(key)
            .filter(|old| old.digest.as_deref() == digest)
            .map_or(1, |old| old.attempts.saturating_add(1));
        let delay = (RETRY_MS << (attempts - 1).min(5)).min(MAX_RETRY_MS);
        let retry_at_ms = now.saturating_add(delay);
        tracing::warn!(file = key, reason = %error, retry_at_ms, "Analytics migration source retained");
        self.0.insert(
            key.to_owned(),
            Failure {
                reason: error.to_string(),
                digest: digest.map(str::to_owned),
                attempts,
                retry_at_ms,
            },
        );
    }
}

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
        Read::TooLarge => Err(Error::new("source_limit", "Analytics file exceeds 256 MiB")),
        other => Err(Error::new(
            "io",
            format!("Analytics migration read: {other:?}"),
        )),
    }
}

#[derive(Clone, Copy)]
enum Projection {
    Codex,
    Quota,
    Local,
    Archive,
}

fn projection(key: &str) -> Projection {
    if key == "legacy/cache/codex-native-usage-v1.json" {
        Projection::Codex
    } else if key.starts_with(QUOTA) {
        Projection::Quota
    } else if key == "legacy/cache/account-activity-v1/analytics-activity-snapshot-v1.json" {
        Projection::Local
    } else {
        Projection::Archive
    }
}

pub(super) struct Migration<'a> {
    pub store: &'a Engine,
    pub root: &'a Path,
    pub local: Option<&'a str>,
    pub local_originals: std::collections::BTreeSet<String>,
    pub now: i64,
    pub pricing: super::ResolverPricing,
}

impl Migration<'_> {
    pub(super) fn needed(store: &Engine) -> Result<bool> {
        let epoch = activity::recovery_epoch(store)?.to_string();
        Ok(store.get_meta(MARKER)?.as_deref() != Some(&epoch))
    }

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
            Projection::Archive => return Ok(true),
        }
        self.store.set_meta(&receipt, "1")?;
        Ok(self.store.get_meta(&receipt)?.is_some())
    }

    fn import_rows(&self, computer: &str, tool: &str, data: &[Value]) -> Result<()> {
        super::history::commit_report(self.store, self.rows_report(computer, tool, data))
    }

    fn rows_report(&self, computer: &str, tool: &str, data: &[Value]) -> Value {
        report::rows_report(&report::RowsReportArgs {
            computer,
            tool,
            data,
            kind: "import",
            read_at: self.now,
            complete: false,
            unreadable_records: None,
            cause: Some("retired_cache"),
            now: self.now,
        })
    }

    fn ready(
        &self,
        key: &str,
        digest: &str,
        bytes: Option<&[u8]>,
        epoch: &str,
        failures: &mut Failures,
    ) -> bool {
        match self.project(key, digest, bytes, epoch) {
            Ok(ready) => {
                failures.0.remove(key);
                ready
            }
            Err(error) => {
                let reason = match projection(key) {
                    Projection::Codex if error.code == "unsupported" => {
                        "unsupported Codex cache version"
                    }
                    Projection::Codex if error.code == "invalid" => {
                        "invalid Codex cache or no recoverable records"
                    }
                    _ => "Analytics projection could not be committed",
                };
                failures.record(key, Some(digest), &Error::new(error.code, reason), self.now);
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
        failures: &mut Failures,
    ) -> Result<bool> {
        let mut complete = true;
        analytics_cache::archive(self.store, batch)?;
        for (file, (key, bytes)) in paths.iter().zip(batch) {
            if analytics_cache::archived(self.store, key)?.as_deref() != Some(bytes) {
                return Err(Error::new("io", "Analytics migration verification failed"));
            }
            if key.starts_with(remote::PREFIX) {
                // Claude ownership requires all of the raw computer caches.
                continue;
            }
            if !self.ready(
                key,
                &aac_core::sha256_hex(bytes),
                Some(bytes),
                epoch,
                failures,
            ) {
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
        let previous_remote = remote::previous_index(self)?;
        let previous = self.store.get_meta(FAILURES)?;
        let mut failures: Failures = previous
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let mut complete = true;
        let mut batch = Vec::new();
        let mut pending = Vec::new();
        let mut size = 0;
        let mut seen = HashSet::new();
        let mut blocked_remote = HashSet::new();
        let mut staged_remote = Vec::new();
        for path in candidates(self.root)? {
            let relative = path.strip_prefix(self.root)?;
            let key = format!("legacy/{}", relative.to_string_lossy().replace('\\', "/"));
            seen.insert(key.clone());
            if failures.waiting(&key, self.now) {
                if key.starts_with(remote::PREFIX) {
                    blocked_remote.insert(key.clone());
                }
                complete = false;
                continue;
            }
            let bytes = match read(self.root, &path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(error) => {
                    if key.starts_with(remote::PREFIX) {
                        blocked_remote.insert(key.clone());
                    }
                    failures.record(&key, None, &error, self.now);
                    complete = false;
                    continue;
                }
            };
            if key.starts_with(remote::PREFIX) {
                staged_remote.push((path.clone(), key.clone(), aac_core::sha256_hex(&bytes)));
            }
            if size + bytes.len() > BATCH_BYTES && !batch.is_empty() {
                complete &=
                    self.flush(&pending, &batch, &epoch, &mut before_remove, &mut failures)?;
                batch.clear();
                pending.clear();
                size = 0;
            }
            size += bytes.len();
            batch.push((key, bytes));
            pending.push(path);
        }
        if !batch.is_empty() {
            complete &= self.flush(&pending, &batch, &epoch, &mut before_remove, &mut failures)?;
        }
        // Recover copies staged by the earlier migration, and replay after a projection rebuild.
        for prefix in [
            "legacy/cache/codex-native-usage-v1.json",
            QUOTA,
            "legacy/cache/account-activity-v1/analytics-activity-snapshot-v1.json",
        ] {
            for (key, digest) in analytics_cache::archived_index(self.store, prefix)? {
                if !seen.insert(key.clone()) {
                    continue;
                }
                complete &= !failures.waiting(&key, self.now)
                    && self.ready(&key, &digest, None, &epoch, &mut failures);
            }
        }
        let remote = remote::commit(
            self,
            &epoch,
            &blocked_remote,
            &previous_remote,
            &mut seen,
            &mut failures,
        )?;
        complete &= remote.complete;
        for (path, key, digest) in staged_remote {
            if !remote.retire.contains(key.as_str()) {
                continue;
            }
            before_remove()?;
            match read(self.root, &path)? {
                Some(current) if aac_core::sha256_hex(&current) == digest => fs::remove_file(path)?,
                Some(_) => return Err(Error::new("io", "Analytics migration source changed")),
                None => {}
            }
        }
        failures.0.retain(|key, _| seen.contains(key));
        let receipt = serde_json::to_string(&failures)?;
        if previous.as_deref() != Some(&receipt) && (previous.is_some() || !failures.0.is_empty()) {
            self.store.set_meta(FAILURES, &receipt)?;
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
            local_originals: Default::default(),
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
    fn analytics_migration_backs_off_heavy_failures_and_continues_other_files() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let dir = root.join("store");
        let open = || {
            aac_store::ops::open_engine(&dir.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let codex = root.join("cache/codex-native-usage-v1.json");
        let quota = root.join("account-analytics/quota-history-v1.json");
        let cursor = root.join("cache/cursor-quota-v1.json");
        fs::create_dir_all(root.join("cache")).expect("cache directory");
        fs::create_dir_all(root.join("account-analytics")).expect("quota directory");
        fs::write(&codex, b"{\"version\":2,\"files\":{}}").expect("write unsupported cache");
        fs::write(&cursor, b"{}").expect("write later migration candidate");
        fs::File::create(&quota)
            .expect("create quota fixture")
            .set_len(MAX_FILE + 1)
            .expect("size fixture above import cap");
        let store = open();
        let start = fixture(&store, root).now;
        assert!(!fixture(&store, root).run().expect("first migration pass"));
        assert!(codex.is_file() && quota.is_file() && !cursor.exists());
        store.close().expect("close before retry");
        drop(store);
        let store = open();
        let health = || {
            aac_store::ops::call(&store, "status", "{}").expect("read migration health")
            ["storageHealth"]["analyticsMigrationFailures"].clone()
        };
        let codex_key = "legacy/cache/codex-native-usage-v1.json";
        let first = health();
        assert!(
            first[QUOTA]["reason"]
                .as_str()
                .expect("quota failure reason")
                .contains("256 MiB")
        );
        assert!(
            first[codex_key]["reason"]
                .as_str()
                .expect("Codex failure reason")
                .contains("unsupported")
        );
        assert!(
            !Migration {
                now: start + 1,
                ..fixture(&store, root)
            }
            .run()
            .expect("skip during backoff")
        );
        assert_eq!(health(), first);
        for attempts in 2..=7 {
            let at = health()[codex_key]["retryAtMs"]
                .as_i64()
                .expect("Codex retry time")
                .max(
                    health()[QUOTA]["retryAtMs"]
                        .as_i64()
                        .expect("quota retry time"),
                );
            assert!(
                !Migration {
                    now: at,
                    ..fixture(&store, root)
                }
                .run()
                .expect("retry after backoff")
            );
            let state = health();
            assert_eq!(state[codex_key]["attempts"], attempts);
            assert_eq!(state[QUOTA]["attempts"], attempts);
            if attempts >= 6 {
                assert_eq!(state[codex_key]["retryAtMs"], at + MAX_RETRY_MS);
            }
        }
        assert!(codex.is_file() && quota.is_file());
    }

    #[test]
    fn analytics_migration_retires_quota_after_recording_valid_rows_and_named_rejects() {
        let home = tempfile::tempdir().expect("fixture home");
        let root = home.path();
        let store = aac_store::ops::open_engine(&root.join("store").to_string_lossy(), None, None)
            .expect("open fixture store");
        let mut record = json!({
            "identity":"a".repeat(64), "accountId":"claude:fixture",
            "sampledAt":"2026-11-15T08:40:00.000Z","observedAt":"2026-11-15T08:40:00.000Z",
            "platform":"mac","status":"ok","source":"cli",
            "windows":[{"key":"five_hour","label":"5-hour limit","kind":"rate_limit","windowMinutes":300,"usedPercent":30}]
        });
        let valid = record.clone();
        record["identity"] = json!("b".repeat(64));
        let records = vec![valid, record];
        let body = json!({"records":records}).to_string();
        let quota = root.join("account-analytics/quota-history-v1.json");
        fs::create_dir_all(root.join("account-analytics")).expect("quota directory");
        fs::write(&quota, &body).expect("write quota history with one invalid reading");
        let migration = fixture(&store, root);
        assert!(!migration.run().expect("stage quota history"));
        let digest = aac_core::sha256_hex(body.as_bytes());
        let source = sampler::BackfillSource {
            label: "quota-history-v1.json".into(),
            sha256: digest.clone(),
            records,
        };
        let providers = [("b".repeat(64), String::new())].into_iter().collect();
        let report = sampler::backfill(
            &store,
            std::slice::from_ref(&source),
            &providers,
            migration.now,
            0,
        )
        .expect("import valid quotas and record the rejection");
        assert_eq!((report.sampled, report.rejected), (1, 1));
        assert!(migration.run().expect("retire the verified quota file"));
        assert!(!quota.exists());
        assert_eq!(
            analytics_cache::archived(&store, QUOTA)
                .expect("read quota archive")
                .expect("quota bytes preserved"),
            body.as_bytes()
        );
        let replay = sampler::backfill(&store, &[source], &providers, migration.now, 0)
            .expect("rerun quota import");
        assert_eq!((replay.files_already_imported, replay.records), (1, 0));
        let receipt = store
            .get_meta(&format!("ts:backfill:{digest}"))
            .expect("read quota import receipt")
            .expect("quota import receipt exists");
        let receipt: Value = serde_json::from_str(&receipt).expect("parse quota receipt");
        assert_eq!(receipt["rejects"][0]["index"], 1);
        let status =
            aac_store::ops::call(&store, "status", "{}").expect("read quota rejection health");
        assert_eq!(
            status["storageHealth"]["quotaBackfillRejections"][digest]["count"],
            1
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
