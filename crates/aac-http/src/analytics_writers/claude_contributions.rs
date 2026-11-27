//! Evidence for raise-only mirror cells. Store the exact priced projection before
//! publishing so a later original can remove only the copy from a settled hour.
use super::claude_mirrors::{self, Part};
use aac_store::{
    activity, analytics_cache, collect, contract, engine::Engine, error::Result, report,
    scan_row::ScanRow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

const PHASES: [&str; 3] = ["migration", "backfill", "live"];
type ScanRows<'a> = (Vec<&'a ScanRow>, Vec<&'a ScanRow>);

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
                unreadable_records: None,
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

fn split<'a>(part: &'a Part<'_>, mirror: bool) -> BTreeMap<Option<&'a str>, ScanRows<'a>> {
    let mut groups: BTreeMap<_, ScanRows> = BTreeMap::new();
    for (session, rows) in [(false, &part.rows), (true, &part.sessions)] {
        for row in rows {
            let owner = claude_mirrors::row_identity(row, &part.identities);
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
                pair.1.push(row);
            } else {
                pair.0.push(row);
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
        claude_mirrors::row_identity(r, &part.identities).is_some_and(|(_, mirror)| mirror)
    }) {
        return Ok(());
    }
    let mut owners = claude_mirrors::load(store, claude_mirrors::HISTORY_MIRRORS)?;
    let mut history = claude_mirrors::load(store, claude_mirrors::HISTORY)?;
    let previous = history.len();
    let mut snapshots = BTreeMap::<u64, Hour>::new();
    for (name, (rows, sessions)) in split(part, true) {
        let worker = collect::worker_from_rows(&rows, &sessions, part.host, now, pricing);
        for cell in worker["cells"].as_array().into_iter().flatten() {
            let Some(hour) = cell_hour(cell) else {
                continue;
            };
            let snapshot = snapshots.entry(hour).or_default();
            if let Some(name) = name {
                // Copy a shared map only to add a name it lacks.
                if !owners.contains_key(name) {
                    Arc::make_mut(&mut owners).insert(name, part.host);
                }
                if !history.contains_key(name) {
                    Arc::make_mut(&mut history).insert(name, part.host);
                }
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
    let checkpoints = snapshots
        .into_iter()
        .filter(|(_, snapshot)| !snapshot.mirrors.is_empty())
        .map(|(hour, snapshot)| Ok((key(part.host, hour, phase), serde_json::to_vec(&snapshot)?)))
        .collect::<Result<Vec<_>>>()?;
    analytics_cache::put_many(store, &checkpoints)?;
    claude_mirrors::save(store, claude_mirrors::HISTORY_MIRRORS, &owners)?;
    if history.len() != previous {
        claude_mirrors::save(store, claude_mirrors::HISTORY, &history)?;
    }
    Ok(())
}

fn covered(mirror: &Value, original: &Value) -> bool {
    mirror["modelRaw"] == original["modelRaw"]
        && mirror["route"] == original["route"]
        && (mirror["sessionKey"].is_null()
            || original["sessionKey"].is_null()
            || mirror["sessionKey"] == original["sessionKey"])
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
    let mirrors = claude_mirrors::load(store, claude_mirrors::HISTORY_MIRRORS)?;
    let mut history = claude_mirrors::load(store, claude_mirrors::HISTORY)?;
    let before_history = history.clone();
    for part in parts {
        for (name, (rows, sessions)) in split(part, false) {
            let Some(name) = name else { continue };
            if rows.is_empty() {
                continue;
            }
            let Some(mirror_host) = mirrors.get(name) else {
                if !history.contains_key(name) {
                    Arc::make_mut(&mut history).insert(name, part.host);
                }
                continue;
            };
            let worker = collect::worker_from_rows(&rows, &sessions, part.host, now, pricing);
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
                        row.session.as_deref()?.to_owned(),
                        (part.host.to_owned(), u64::try_from(row.last?).ok()?),
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
                    if activity::reconcile_claude_projection(store, &before, &after, &originals)? {
                        Arc::make_mut(&mut history).insert(name, part.host);
                        claude_mirrors::save(store, claude_mirrors::HISTORY, &history)?;
                        snapshot.mirrors.retain(|_, cells| !cells.is_empty());
                        let lookup = key(mirror_host, hour, phase);
                        if snapshot.mirrors.is_empty() {
                            analytics_cache::remove(store, &lookup)?;
                        } else {
                            analytics_cache::put(store, &lookup, &serde_json::to_vec(&snapshot)?)?;
                        }
                    }
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
