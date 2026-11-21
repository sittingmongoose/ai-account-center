//! Evidence for raise-only mirror cells. Store the exact priced projection before
//! publishing so a later original can remove only the copy from a settled hour.
use super::claude_mirrors::{self, Owners, Part};
use aac_store::{
    activity, analytics_cache, collect, contract, engine::Engine, error::Result, report,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PHASES: [&str; 3] = ["migration", "backfill", "live"];

#[derive(Default, Deserialize, Serialize)]
struct Hour {
    independent: Vec<Value>,
    mirrors: BTreeMap<String, Vec<Value>>,
}

impl Hour {
    fn projection(&self, host: &str, now: i64) -> Result<contract::Report> {
        let cells: Vec<_> = self
            .independent
            .iter()
            .chain(self.mirrors.values().flatten())
            .cloned()
            .collect();
        Ok(serde_json::from_value(report::rows_report(
            &report::RowsReportArgs {
                computer: host,
                tool: "claude",
                data: &[json!({"cells":cells})],
                kind: "import",
                read_at: now,
                complete: false,
                cause: Some("retired_cache"),
                now,
            },
        ))?)
    }
}

fn cell_hour(cell: &Value) -> Option<u64> {
    Some(contract::parse_iso(cell["hour"].as_str()?)? / contract::HOUR_MS)
}

fn key(host: &str, hour: u64, phase: &str) -> String {
    format!("claude/contributions/{host}/{hour}/{phase}")
}

fn split<'a>(
    part: &'a Part<'_>,
    mirror: bool,
) -> BTreeMap<Option<&'a str>, (Vec<Value>, Vec<Value>)> {
    let mut groups: BTreeMap<_, (Vec<Value>, Vec<Value>)> = BTreeMap::new();
    for (session, rows) in [(false, &part.rows), (true, &part.sessions)] {
        for row in rows {
            let owner = claude_mirrors::row_identity(row, &part.fingerprints);
            let group = if mirror {
                owner
                    .filter(|(_, is_mirror)| *is_mirror)
                    .map(|(name, _)| name)
            } else {
                let Some((name, false)) = owner else { continue };
                Some(name)
            };
            let pair = groups.entry(group).or_default();
            if session {
                pair.1.push(row.clone());
            } else {
                pair.0.push(row.clone());
            }
        }
    }
    groups
}

pub(super) fn remember(
    store: &Engine,
    part: &Part<'_>,
    phase: &str,
    now: i64,
    pricing: &dyn collect::Pricing,
) -> Result<()> {
    if !part.rows.iter().any(|r| {
        claude_mirrors::row_identity(r, &part.fingerprints).is_some_and(|(_, mirror)| mirror)
    }) {
        return Ok(());
    }
    let mut owners = claude_mirrors::load(store, claude_mirrors::HISTORY_MIRRORS)?;
    let mut snapshots = BTreeMap::<u64, Hour>::new();
    for (name, (rows, sessions)) in split(part, true) {
        let worker = collect::worker_from_scan(&rows, &sessions, "claude", part.host, now, pricing);
        for cell in worker["cells"].as_array().into_iter().flatten() {
            let Some(hour) = cell_hour(cell) else {
                continue;
            };
            let snapshot = snapshots.entry(hour).or_default();
            if let Some(name) = name {
                owners
                    .entry(name.to_owned())
                    .or_insert_with(|| part.host.to_owned());
                snapshot
                    .mirrors
                    .entry(name.to_owned())
                    .or_default()
                    .push(cell.clone());
            } else {
                snapshot.independent.push(cell.clone());
            }
        }
    }
    for (hour, snapshot) in snapshots {
        if !snapshot.mirrors.is_empty() {
            analytics_cache::put(
                store,
                &key(part.host, hour, phase),
                &serde_json::to_vec(&snapshot)?,
            )?;
        }
    }
    claude_mirrors::save(store, claude_mirrors::HISTORY_MIRRORS, &owners)
}

fn covered(mirror: &Value, original: &Value) -> bool {
    mirror["modelRaw"] == original["modelRaw"]
        && mirror["route"] == original["route"]
        && [
            "inputTokens",
            "outputTokens",
            "cacheCreationTokens",
            "cacheReadTokens",
            "events",
        ]
        .iter()
        .all(|field| {
            original[*field].as_u64().unwrap_or_default()
                >= mirror[*field].as_u64().unwrap_or_default()
        })
}

pub(super) fn reconcile(
    store: &Engine,
    parts: &[Part<'_>],
    now: i64,
    pricing: &dyn collect::Pricing,
) -> Result<()> {
    let mirrors: Owners = claude_mirrors::load(store, claude_mirrors::HISTORY_MIRRORS)?;
    let mut history = claude_mirrors::load(store, claude_mirrors::HISTORY)?;
    let before_history = history.clone();
    for part in parts {
        for (name, (rows, sessions)) in split(part, false) {
            let Some(name) = name else { continue };
            if rows.is_empty() {
                continue;
            }
            let Some(mirror_host) = mirrors.get(name) else {
                history
                    .entry(name.to_owned())
                    .or_insert_with(|| part.host.to_owned());
                continue;
            };
            let worker =
                collect::worker_from_scan(&rows, &sessions, "claude", part.host, now, pricing);
            let mut by_hour = BTreeMap::<u64, Vec<&Value>>::new();
            for cell in worker["cells"].as_array().into_iter().flatten() {
                if let Some(hour) = cell_hour(cell) {
                    by_hour.entry(hour).or_default().push(cell);
                }
            }
            let originals = sessions
                .iter()
                .filter_map(|row| {
                    Some((
                        row["s"].as_str()?.to_owned(),
                        (part.host.to_owned(), row["z"].as_u64()?),
                    ))
                })
                .collect::<BTreeMap<_, _>>();
            for (hour, candidates) in by_hour {
                for phase in PHASES {
                    let Some(bytes) = analytics_cache::get(store, &key(mirror_host, hour, phase))?
                    else {
                        continue;
                    };
                    let mut snapshot: Hour = serde_json::from_slice(&bytes)?;
                    let Some(mirrored) = snapshot.mirrors.get(name) else {
                        continue;
                    };
                    if !mirrored
                        .iter()
                        .any(|cell| candidates.iter().any(|original| covered(cell, original)))
                    {
                        continue;
                    }
                    let before = snapshot.projection(mirror_host, now)?;
                    if let Some(mirrored) = snapshot.mirrors.get_mut(name) {
                        mirrored.retain(|cell| {
                            !candidates.iter().any(|original| covered(cell, original))
                        });
                    }
                    let after = snapshot.projection(mirror_host, now)?;
                    activity::repair_remote_import(store, &before, &after, &originals, false)?;
                    history.insert(name.to_owned(), part.host.to_owned());
                }
            }
        }
    }
    if history != before_history {
        claude_mirrors::save(store, claude_mirrors::HISTORY, &history)?;
    }
    Ok(())
}

pub(super) fn prepare_live(
    store: &Engine,
    parts: &mut [Part<'_>],
    states: &mut [Value],
    now: i64,
    pricing: &dyn collect::Pricing,
) -> Result<()> {
    reconcile(store, parts, now, pricing)?;
    claude_mirrors::select_live(store, parts, states)?;
    for part in parts {
        remember(store, part, "live", now, pricing)?;
    }
    Ok(())
}
