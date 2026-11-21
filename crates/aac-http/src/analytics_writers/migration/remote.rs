//! Import independent history per cache. Elect an owner only for proven Claude mirrors.
use super::super::{claude_mirrors, scan_state};
use super::{Failures, Migration};
use aac_store::error::{Error, Result};
use aac_store::{activity, analytics_cache, collect, contract};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(super) const PREFIX: &str = scan_state::REMOTE_ARCHIVE;
const OWNERS: &str = "legacy/cache/analytics-remote-v1/claude-mirror-owners.json";
const RECEIPT: &str = "analytics_remote_import_v4";
const PREVIOUS_RECEIPT: &str = "analytics_remote_import_v3";
const PREVIOUS_SOURCES: &str = "analytics_remote_import_v3_sources";

fn index_digest(index: &[(String, String)]) -> Result<String> {
    Ok(aac_core::sha256_hex(&serde_json::to_vec(index)?))
}

/// Verify the previous group receipt before another source can change the
/// index. Keep the proven digests across per-file retries and restarts.
pub(super) fn previous_index(migration: &Migration<'_>) -> Result<BTreeMap<String, String>> {
    if let Some(sources) = migration.store.get_meta(PREVIOUS_SOURCES)? {
        return Ok(serde_json::from_str(&sources)?);
    }
    let Some(receipt) = migration.store.get_meta(PREVIOUS_RECEIPT)? else {
        return Ok(BTreeMap::new());
    };
    let index = analytics_cache::archived_index(migration.store, PREFIX)?;
    let digest = index_digest(&index)?;
    if receipt
        .split_once(':')
        .is_none_or(|(_, previous)| previous != digest)
    {
        return Ok(BTreeMap::new());
    }
    let sources: BTreeMap<_, _> = index.into_iter().collect();
    migration
        .store
        .set_meta(PREVIOUS_SOURCES, &serde_json::to_string(&sources)?)?;
    Ok(sources)
}

#[derive(Clone, Copy)]
struct Archive<'a> {
    path: &'a str,
    computer: &'a str,
    digest: &'a str,
}

struct Selection {
    owners: claude_mirrors::Owners,
    originals: BTreeSet<String>,
}

fn read_cache(migration: &Migration<'_>, archive: Archive<'_>) -> Result<scan_state::LegacyCache> {
    let bytes = analytics_cache::archived(migration.store, archive.path)?
        .ok_or_else(|| Error::new("io", "missing staged Analytics computer cache"))?;
    scan_state::LegacyCache::read(&bytes)
}

fn legacy_owners(migration: &Migration<'_>) -> Result<claude_mirrors::Owners> {
    let mut previous = claude_mirrors::load(migration.store, claude_mirrors::LIVE)?;
    let Some(value) = analytics_cache::archived(migration.store, OWNERS)?
        .filter(|bytes| bytes.len() <= 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|value| value["version"] == 1)
    else {
        return Ok(previous);
    };
    for (name, host) in value["owners"].as_object().into_iter().flatten() {
        if !name.is_empty()
            && let Some(host) = host.as_str().filter(|id| contract::ident_ok(id))
        {
            previous
                .entry(name.clone())
                .or_insert_with(|| host.to_owned());
        }
    }
    Ok(previous)
}

fn choose_owners(migration: &Migration<'_>, archives: &[Archive<'_>]) -> Result<Selection> {
    let mut parts = Vec::new();
    let mut states = Vec::new();
    let mut mirrors = BTreeSet::new();
    let mut originals = BTreeSet::new();
    let mut session_names = BTreeMap::<String, BTreeSet<String>>::new();
    let mut mirror_days = BTreeSet::new();
    for archive in archives {
        let mut cache = read_cache(migration, *archive)?;
        if cache.rebuilds("claude") {
            continue;
        }
        let prints = cache
            .scan
            .fingerprints
            .remove("claude")
            .unwrap_or(Value::Null);
        let mut fingerprints = Map::new();
        for (file, print) in prints.as_object().into_iter().flatten() {
            if let Some((name, mirror)) = claude_mirrors::identity(print) {
                if mirror {
                    mirrors.insert((name.to_owned(), archive.computer));
                } else {
                    originals.insert(name.to_owned());
                }
                fingerprints.insert(
                    file.clone(),
                    json!({"claudeName":name,"claudeMirror":mirror}),
                );
            }
        }
        let mut files = BTreeSet::new();
        for row in cache.scan.rows.iter().chain(&cache.scan.srows) {
            if row["k"] != "claude" {
                continue;
            }
            if let Some((name, true)) = claude_mirrors::row_identity(row, &prints) {
                if let Some(file) = row["f"].as_str() {
                    files.insert(file);
                }
                if let Some(session) = row["s"].as_str() {
                    session_names
                        .entry(session.to_owned())
                        .or_default()
                        .insert(name.to_owned());
                }
                if let Some(hour) = row["h"].as_str()
                    && let Some(ms) =
                        contract::parse_iso(&format!("{}:00Z", hour.replace(' ', "T")))
                {
                    mirror_days.insert((ms / 86_400_000) as u32);
                }
            }
        }
        parts.push(claude_mirrors::Part {
            host: archive.computer,
            state_index: states.len(),
            rows: files.into_iter().map(|file| json!({"f":file})).collect(),
            sessions: Vec::new(),
            fingerprints: Value::Object(fingerprints),
        });
        let state = if cache.last_scan_at.is_some() {
            "ok"
        } else {
            "cached"
        };
        states.push(json!({"tool":"claude","host":archive.computer,"state":state}));
    }
    // Without a raw cache an earlier owner is unobserved. A confirmed cache that
    // lacks the mirror, in contrast, lets another holder take ownership.
    let previous = legacy_owners(migration)?;
    let present: BTreeSet<_> = parts.iter().map(|part| part.host).collect();
    let absent: BTreeSet<_> = previous
        .values()
        .filter(|host| !present.contains(host.as_str()))
        .collect();
    for host in absent {
        states.push(json!({"tool":"claude","host":host,"state":"cached"}));
    }
    let stored_owners = claude_mirrors::load(migration.store, claude_mirrors::HISTORY)?;
    let mut committed = stored_owners.clone();
    committed.retain(|name, host| {
        !originals.contains(name) || !mirrors.contains(&(name.clone(), host.as_str()))
    });
    let keys = session_names.keys().cloned().collect();
    for (session, computers) in
        activity::session_computers(migration.store, "claude", &keys, &mirror_days)?
    {
        let names = &session_names[&session];
        if let Some(name) = names.first().filter(|_| names.len() == 1) {
            let mut candidates = computers
                .into_iter()
                .filter(|computer| !mirrors.contains(&(name.clone(), computer.as_str())));
            let Some(computer) = candidates.next().filter(|_| candidates.next().is_none()) else {
                continue;
            };
            // The same transcript is already in dated Store history on this source.
            // Never treat a host known to hold this mirror as an original.
            originals.insert(name.clone());
            committed.insert(name.clone(), computer);
        }
    }
    let selected = claude_mirrors::choose(&mut parts, &mut states, &previous, &committed);
    if selected != stored_owners {
        // Keep the same election if the boot stops between reports or before retirement.
        claude_mirrors::save(migration.store, claude_mirrors::HISTORY, &selected)?;
    }
    Ok(Selection {
        owners: selected,
        originals,
    })
}

#[derive(Clone, Copy)]
enum ClaudeRows<'a> {
    All,
    Independent,
    Selected(&'a Selection),
}

fn rows_for(
    cache: &scan_state::Cache,
    tool: &str,
    computer: &str,
    selection: ClaudeRows<'_>,
) -> (Vec<Value>, Vec<Value>) {
    let prints = cache.fingerprints.get("claude").unwrap_or(&Value::Null);
    let keep = |row: &&Value| {
        row["k"] == tool
            && (tool != "claude"
                || match claude_mirrors::row_identity(row, prints) {
                    Some((name, true)) => match selection {
                        ClaudeRows::All => true,
                        ClaudeRows::Independent => false,
                        ClaudeRows::Selected(selection) => {
                            !selection.originals.contains(name)
                                && selection
                                    .owners
                                    .get(name)
                                    .is_none_or(|owner| owner == computer)
                        }
                    },
                    _ => true,
                })
    };
    (
        cache.rows.iter().filter(keep).cloned().collect(),
        cache.srows.iter().filter(keep).cloned().collect(),
    )
}

fn worker(
    migration: &Migration<'_>,
    computer: &str,
    tool: &str,
    (rows, sessions): &(Vec<Value>, Vec<Value>),
) -> Value {
    collect::worker_from_scan(
        rows,
        sessions,
        tool,
        computer,
        migration.now,
        &migration.pricing,
    )
}

pub(super) struct Outcome {
    pub complete: bool,
    pub retire: HashSet<String>,
}

pub(super) fn commit(
    migration: &Migration<'_>,
    epoch: &str,
    blocked: &HashSet<String>,
    previous: &BTreeMap<String, String>,
    seen: &mut HashSet<String>,
    failures: &mut Failures,
) -> Result<Outcome> {
    let index = analytics_cache::archived_index(migration.store, PREFIX)?;
    let receipt = format!("{epoch}:{}", index_digest(&index)?);
    let mut result = Outcome {
        complete: blocked.is_empty(),
        retire: HashSet::new(),
    };
    let mut valid = Vec::new();
    let mut pending = Vec::new();
    let prior_epoch = migration.store.get_meta("analytics_files_migrated_v2")?;
    for (path, digest) in &index {
        seen.insert(path.clone());
        if blocked.contains(path) || failures.waiting(path, migration.now) {
            result.complete = false;
            continue;
        }
        let Some(computer) = path
            .strip_prefix(PREFIX)
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|name| path != OWNERS && contract::ident_ok(name))
        else {
            result.retire.insert(path.clone());
            failures.0.remove(path);
            continue;
        };
        let archive = Archive {
            path,
            computer,
            digest,
        };
        let cache = match read_cache(migration, archive) {
            Ok(cache) => cache,
            Err(error) => {
                let error = Error::new(error.code, "Analytics computer cache could not be decoded");
                failures.record(path, Some(digest), &error, migration.now);
                result.complete = false;
                continue;
            }
        };
        let old_epoch = prior_epoch.as_deref().unwrap_or(epoch);
        let old_receipt = format!("analytics_import:{old_epoch}:{path}:{digest}");
        let imported_before =
            previous.get(path) == Some(digest) || migration.store.get_meta(&old_receipt)?.is_some();
        if let Err(error) = import_independent(migration, &cache, archive, epoch, imported_before) {
            let error = Error::new(error.code, "Analytics computer cache projection failed");
            failures.record(path, Some(digest), &error, migration.now);
            result.complete = false;
            continue;
        }
        valid.push(archive);
        let scan = &cache.scan;
        let prints = scan.fingerprints.get("claude").unwrap_or(&Value::Null);
        if !cache.rebuilds("claude")
            && scan.rows.iter().chain(&scan.srows).any(|row| {
                row["k"] == "claude"
                    && claude_mirrors::row_identity(row, prints).is_some_and(|(_, mirror)| mirror)
            })
        {
            pending.push((archive, imported_before));
        } else {
            result.retire.insert(path.clone());
        }
        failures.0.remove(path);
    }
    if !result.complete {
        return Ok(result);
    }
    if migration.store.get_meta(RECEIPT)?.as_deref() != Some(&receipt) {
        let owners = choose_owners(migration, &valid)?;
        for (archive, imported_before) in &pending {
            import_mirrors(migration, *archive, &owners, *imported_before)?;
        }
        migration.store.set_meta(RECEIPT, &receipt)?;
    }
    result.retire.extend(
        pending
            .into_iter()
            .map(|(archive, _)| archive.path.to_owned()),
    );
    Ok(result)
}

fn import_independent(
    migration: &Migration<'_>,
    cache: &scan_state::LegacyCache,
    archive: Archive<'_>,
    epoch: &str,
    imported_before: bool,
) -> Result<()> {
    let Archive {
        path,
        computer,
        digest,
    } = archive;
    let receipt = format!("{RECEIPT}:independent:{epoch}:{path}:{digest}");
    if migration.store.get_meta(&receipt)?.is_some() {
        return Ok(());
    }
    for tool in aac_providers::scanner_kinds() {
        if cache.rebuilds(tool) {
            let raw = rows_for(&cache.scan, tool, computer, ClaudeRows::All);
            let has_rows = !raw.0.is_empty() || !raw.1.is_empty();
            if imported_before && has_rows {
                let previous = worker(migration, computer, tool, &raw);
                repair_obsolete(migration, computer, tool, previous)?;
            }
            if has_rows || cache.scan.fingerprints.contains_key(tool) {
                super::super::history::rescan_legacy_source(
                    migration.store,
                    computer,
                    tool,
                    activity::recovery_epoch(migration.store)?,
                )?;
            }
            continue;
        }
        let selected = rows_for(&cache.scan, tool, computer, ClaudeRows::Independent);
        if !selected.0.is_empty() || !selected.1.is_empty() {
            let worker = worker(migration, computer, tool, &selected);
            migration.import_rows(computer, tool, &[worker])?;
        }
    }
    migration.store.set_meta(&receipt, "1")
}

fn repair_obsolete(
    migration: &Migration<'_>,
    computer: &str,
    tool: &str,
    previous: Value,
) -> Result<()> {
    let mut before: contract::Report =
        serde_json::from_value(migration.rows_report(computer, tool, &[previous]))?;
    let empty = serde_json::from_value(migration.rows_report(computer, tool, &[]))?;
    let mut sessions = std::mem::take(&mut before.sessions).into_iter().peekable();
    loop {
        before.sessions = sessions.by_ref().take(contract::MAX_SESSIONS).collect();
        activity::repair_remote_import(migration.store, &before, &empty, &BTreeMap::new(), true)?;
        if sessions.peek().is_none() {
            return Ok(());
        }
        before.cells.clear();
    }
}

fn import_mirrors(
    migration: &Migration<'_>,
    archive: Archive<'_>,
    owners: &Selection,
    imported_before: bool,
) -> Result<()> {
    let legacy = read_cache(migration, archive)?;
    if legacy.rebuilds("claude") {
        return Ok(());
    }
    let cache = legacy.scan;
    let computer = archive.computer;
    let selected = rows_for(&cache, "claude", computer, ClaudeRows::Selected(owners));
    if !selected.0.is_empty() || !selected.1.is_empty() {
        let worker = worker(migration, computer, "claude", &selected);
        migration.import_rows(computer, "claude", &[worker])?;
    }
    if !imported_before {
        return Ok(());
    }
    let raw = rows_for(&cache, "claude", computer, ClaudeRows::All);
    if raw.0.len() == selected.0.len() {
        return Ok(());
    }
    let report = |rows| -> Result<contract::Report> {
        let worker = worker(migration, computer, "claude", rows);
        let mut value = migration.rows_report(computer, "claude", &[worker]);
        value["sessions"] = json!([]);
        Ok(serde_json::from_value(value)?)
    };
    let prints = cache.fingerprints.get("claude").unwrap_or(&Value::Null);
    let retained_sessions: HashSet<_> = selected
        .1
        .iter()
        .filter_map(|row| row["s"].as_str())
        .collect();
    let mut original_sessions = BTreeMap::<String, (String, u64)>::new();
    for row in cache.srows.iter().filter(|row| row["k"] == "claude") {
        if let Some((name, true)) = claude_mirrors::row_identity(row, prints)
            && owners.originals.contains(name)
            && let Some(owner) = owners.owners.get(name).filter(|owner| *owner != computer)
            && let (Some(session), Some(end)) = (row["s"].as_str(), row["z"].as_u64())
            && !retained_sessions.contains(session)
        {
            original_sessions
                .entry(session.to_owned())
                .and_modify(|entry| entry.1 = entry.1.max(end))
                .or_insert_with(|| (owner.clone(), end));
        }
    }
    activity::repair_remote_import(
        migration.store,
        &report(&raw)?,
        &report(&selected)?,
        &original_sessions,
        false,
    )
}
