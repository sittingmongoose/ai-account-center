//! The retired per-computer caches contain raw Claude SSH mirrors. Choose ownership
//! across the whole archive before projecting any of those rows.
use super::super::{claude_mirrors, scan_state};
use super::Migration;
use aac_store::error::{Error, Result};
use aac_store::{activity, analytics_cache, collect, contract};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const PREFIX: &str = "legacy/cache/analytics-remote-v1/";
const OWNERS: &str = "legacy/cache/analytics-remote-v1/claude-mirror-owners.json";
const RECEIPT: &str = "analytics_remote_import_v3";

#[derive(Clone, Copy)]
struct Archive<'a> {
    path: &'a str,
    computer: &'a str,
    digest: &'a str,
}

#[derive(Deserialize)]
struct LegacyCache {
    #[serde(flatten)]
    scan: scan_state::Cache,
    #[serde(rename = "lastScanAt")]
    last_scan_at: Option<String>,
}

struct Selection {
    owners: claude_mirrors::Owners,
    originals: BTreeSet<String>,
}

fn read_cache(migration: &Migration<'_>, archive: Archive<'_>) -> Result<LegacyCache> {
    let bytes = analytics_cache::archived(migration.store, archive.path)?
        .ok_or_else(|| Error::new("io", "missing staged Analytics computer cache"))?;
    Ok(serde_json::from_slice(&bytes)?)
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
    let present: BTreeSet<_> = archives.iter().map(|archive| archive.computer).collect();
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

fn rows_for(
    cache: &scan_state::Cache,
    tool: &str,
    computer: &str,
    selection: Option<&Selection>,
) -> (Vec<Value>, Vec<Value>) {
    let prints = cache.fingerprints.get("claude").unwrap_or(&Value::Null);
    let keep = |row: &&Value| {
        row["k"] == tool
            && (tool != "claude"
                || selection.is_none_or(|selection| {
                    claude_mirrors::row_identity(row, prints).is_none_or(|(name, mirror)| {
                        (!mirror || !selection.originals.contains(name))
                            && selection
                                .owners
                                .get(name)
                                .is_none_or(|owner| owner == computer)
                    })
                }))
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

pub(super) fn commit(migration: &Migration<'_>, epoch: &str) -> Result<()> {
    let index = analytics_cache::archived_index(migration.store, PREFIX)?;
    let receipt = format!(
        "{epoch}:{}",
        aac_core::sha256_hex(&serde_json::to_vec(&index)?)
    );
    if migration.store.get_meta(RECEIPT)?.as_deref() == Some(&receipt) {
        return Ok(());
    }
    let archives: Vec<_> = index
        .iter()
        .filter_map(|(path, digest)| {
            let computer = path.strip_prefix(PREFIX)?.strip_suffix(".json")?;
            (path != OWNERS && contract::ident_ok(computer)).then_some(Archive {
                path,
                computer,
                digest,
            })
        })
        .collect();
    if archives.is_empty() {
        return Ok(());
    }
    let owners = choose_owners(migration, &archives)?;
    let ran_v2 = migration
        .store
        .get_meta("analytics_files_migrated_v2")?
        .is_some();
    let mut repair = Vec::new();
    for archive in archives {
        let cache = read_cache(migration, archive)?.scan;
        for tool in aac_providers::scanner_kinds() {
            let selected = rows_for(&cache, tool, archive.computer, Some(&owners));
            if tool == "claude"
                && selected.0.len() < cache.rows.iter().filter(|row| row["k"] == "claude").count()
                && (ran_v2
                    || migration
                        .store
                        .get_meta(&format!(
                            "analytics_import:{epoch}:{}:{}",
                            archive.path, archive.digest
                        ))?
                        .is_some())
            {
                repair.push(archive);
            }
            if !selected.0.is_empty() || !selected.1.is_empty() {
                let worker = worker(migration, archive.computer, tool, &selected);
                migration.import_rows(archive.computer, tool, &[worker])?;
            }
        }
    }
    // Versions through v2 could have imported these exact raw aggregates. Put
    // the selected data in the journal before removing any proven duplicates.
    for archive in repair {
        let cache = read_cache(migration, archive)?.scan;
        let report = |owners| -> Result<contract::Report> {
            let rows = rows_for(&cache, "claude", archive.computer, owners);
            let worker = worker(migration, archive.computer, "claude", &rows);
            let mut value = migration.rows_report(archive.computer, "claude", &[worker]);
            value["sessions"] = json!([]);
            Ok(serde_json::from_value(value)?)
        };
        activity::repair_claude_import(migration.store, &report(None)?, &report(Some(&owners))?)?;
    }
    migration.store.set_meta(RECEIPT, &receipt)
}
