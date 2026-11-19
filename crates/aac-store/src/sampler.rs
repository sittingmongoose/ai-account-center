//! G14 quota-history sampler, ported from `account-analytics-service.ts` (`sample`/`recordInStore`)
//! and `store/usage-backfill.ts` (`backfillUsageHistory`).
//!
//! The live sampler coalesces both platforms' dashboards into one reading per account identity
//! ([`merge_accounts`]), turns the bounded accounts into observations ([`sample_observations`]) and
//! hands them to the store ([`record_sample`]). The JSON history is imported once, first
//! ([`backfill`]); the store decides which readings are new. File enumeration and reading stay with
//! the host (it owns `aac-files`); this module shapes, dedupes, sorts and records, so it tests
//! against a real Engine without a socket or a provider call.
use crate::engine::Engine;
use crate::error::Result;
use crate::observe::{
    self, account_identity, decode_observation, observation_from_account, observation_from_record,
    provider_for,
};
use crate::usage::{self, Sampled};
use aac_num::ecma::parse_instant;
use serde_json::{json, Value};
use std::collections::HashMap;

/// `MAX_ACCOUNTS_PER_SAMPLE`: one tick reads at most this many accounts.
pub const MAX_ACCOUNTS_PER_SAMPLE: usize = 64;
/// The backfill's record batch (`usage-backfill.ts` `BATCH`).
const BATCH: usize = 2000;

/// `Date.parse(account.sampledAt ?? account.fetchedAt ?? '')`: null falls back, empty does not.
fn sampled_ms(account: &Value) -> Option<i64> {
    let text = match &account["sampledAt"] {
        Value::Null => &account["fetchedAt"],
        other => other,
    };
    text.as_str().and_then(parse_instant)
}

fn available(account: &Value) -> bool {
    matches!(account["status"].as_str(), Some("ok") | Some("cached"))
}

/// Coalesce both platforms' dashboards into one account per identity: an available reading wins
/// over an unavailable one, then the newer `sampledAt ?? fetchedAt`. First appearance keeps order.
pub fn merge_accounts(dashboards: &[Value]) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut unique: HashMap<String, Value> = HashMap::new();
    for dashboard in dashboards {
        let Some(accounts) = dashboard["accounts"].as_array() else {
            continue;
        };
        for account in accounts {
            let Some(row) = account.as_object() else {
                continue;
            };
            let Some(identity) = account_identity(row) else {
                continue;
            };
            let sampled = sampled_ms(account);
            let previous = unique.get(&identity);
            let old_sampled = previous.and_then(sampled_ms);
            let now_available = available(account);
            let was_available = previous.is_some_and(available);
            let replace = match previous {
                None => true,
                Some(_) => {
                    (now_available && !was_available)
                        || (now_available == was_available
                            && sampled.is_some_and(|s| {
                                old_sampled.is_none_or(|old| s >= old)
                            }))
                }
            };
            if replace {
                if previous.is_none() {
                    order.push(identity.clone());
                }
                unique.insert(identity, account.clone());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|identity| unique.remove(&identity))
        .collect()
}

/// The bounded accounts' observations at `now`, with infinite retention (the live sampler keeps
/// every reading; the store decides what is new).
pub fn sample_observations(accounts: &[Value], now: i64) -> Vec<Sampled> {
    accounts
        .iter()
        .take(MAX_ACCOUNTS_PER_SAMPLE)
        .filter_map(|account| observation_from_account(account, now, i64::MAX))
        .collect()
}

/// One live tick: record the coalesced accounts' observations. The store's `record` decides
/// sampled/seen/skipped/rejected; an error leaves the caller to mark the source unavailable.
pub fn record_sample(engine: &Engine, accounts: &[Value], now: i64) -> Result<usage::RecordResult> {
    usage::record(engine, sample_observations(accounts, now))
}

/// One backfill source the host read: its label, content hash and raw record objects.
pub struct BackfillSource {
    /// The import label (`archive/<rel>` or `quota-history-v1.json`).
    pub label: String,
    /// SHA-256 of the file's bytes (hex): the once-only import marker.
    pub sha256: String,
    /// The raw record objects `recordsFromText` read from the file.
    pub records: Vec<Value>,
}

/// The backfill's tally, as `backfill_report` persists it.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillReport {
    /// Sources considered (readable files).
    pub files: u64,
    /// Sources whose hash was already imported.
    pub files_already_imported: u64,
    /// Files the host could not read or parse (counted by the host).
    pub files_unreadable: u64,
    /// Observations queued for the store.
    pub records: u64,
    /// Records the whitelist or the observation shape refused.
    pub records_skipped: u64,
    /// Readings the store sampled (new).
    pub sampled: u64,
    /// Readings the store saw unchanged.
    pub seen: u64,
    /// Readings the store skipped as duplicates.
    pub duplicates: u64,
    /// Readings the store rejected.
    pub rejected: u64,
}

/// Import the JSON history once, oldest first across every file, then mark each imported source.
/// `providers` maps live account identities to their provider (else the record's own prefix);
/// `files_unreadable` is the host's count of files it could not read. `now` stamps the markers.
pub fn backfill(
    engine: &Engine,
    sources: &[BackfillSource],
    providers: &HashMap<String, String>,
    now: i64,
    files_unreadable: u64,
) -> Result<BackfillReport> {
    let mut report = BackfillReport {
        files_unreadable,
        ..Default::default()
    };
    let mut pending: Vec<Sampled> = Vec::new();
    let mut imported: Vec<(&BackfillSource, usize)> = Vec::new();
    for source in sources {
        report.files += 1;
        let marker = format!("ts:backfill:{}", source.sha256);
        if engine.get_meta(&marker)?.is_some() {
            report.files_already_imported += 1;
            continue;
        }
        let mut valid = Vec::new();
        for record in &source.records {
            match decode_observation(record) {
                Some(decoded) => valid.push(decoded),
                None => report.records_skipped += 1,
            }
        }
        for (index, decoded) in valid.iter().enumerate() {
            let provider = provider_for(decoded, providers);
            let import_ref = format!("{}#{}", source.label, index);
            match observation_from_record(decoded, &provider, &import_ref) {
                Some(observation) => pending.push(observation),
                None => report.records_skipped += 1,
            }
        }
        imported.push((source, valid.len()));
    }
    // Oldest first across every file, so archive and main file interleave correctly; an overlap
    // between them is skipped by the store as a duplicate sample.
    pending.sort_by_key(|o| (o.sampled_at_ms, o.observed_at_ms));
    report.records = pending.len() as u64;
    for chunk in pending.chunks(BATCH) {
        let result = usage::record(engine, chunk.to_vec())?;
        report.sampled += result.sampled as u64;
        report.seen += result.seen as u64;
        report.duplicates += result.skipped as u64;
        report.rejected += result.rejected as u64;
    }
    for (source, count) in &imported {
        let value = json!({ "file": source.label, "records": count, "at": now }).to_string();
        engine.set_meta(&format!("ts:backfill:{}", source.sha256), &value)?;
    }
    if !imported.is_empty() {
        engine.set_meta("ts:backfill_report", &serde_json::to_string(&report)?)?;
    }
    Ok(report)
}

/// The live providers map the backfill uses: each coalesced account's identity to its provider.
pub fn providers_of(accounts: &[Value]) -> HashMap<String, String> {
    let mut providers = HashMap::new();
    for account in accounts {
        let Some(row) = account.as_object() else {
            continue;
        };
        if let (Some(identity), Some(provider)) =
            (account_identity(row), account["provider"].as_str())
        {
            providers.insert(identity, provider.to_owned());
        }
    }
    providers
}

/// One full tick as the host runs it: import the history once (the caller guards the once flag),
/// then record the live readings. Returns the store's tally for the live readings.
pub fn tick(
    engine: &Engine,
    dashboards: &[Value],
    now: i64,
) -> Result<(Vec<Value>, usage::RecordResult)> {
    let accounts = merge_accounts(dashboards);
    let result = record_sample(engine, &accounts, now)?;
    Ok((accounts, result))
}

/// Re-exported for the host's backfill file reading: the archive shapes' record extractor.
pub fn records_from_text(text: &str) -> (Vec<Value>, bool) {
    observe::records_from_text(text)
}