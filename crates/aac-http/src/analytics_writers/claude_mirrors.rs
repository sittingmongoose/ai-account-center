//! Select Claude SSH mirrors across computers before the file keys are aggregated away.

use aac_store::{
    analytics_cache, collect,
    engine::Engine,
    error::{Error, Result},
    scan_row::ScanRow,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub(super) use super::claude_owners::Owners;
pub(super) use super::scan_print::Identities;

pub(super) const LIVE: &str = "live/claude-mirror-owners";
pub(super) const HISTORY: &str = "history/claude-mirror-owners";
pub(super) const HISTORY_MIRRORS: &str = "history/claude-published-mirrors";
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct Part<'a> {
    pub(super) host: &'a str,
    pub(super) state_index: usize,
    pub(super) rows: Vec<ScanRow>,
    pub(super) sessions: Vec<ScanRow>,
    /// The Claude identities of the computer's prints, by file key.
    pub(super) identities: Identities,
}

impl Part<'_> {
    /// The part's priced cells and sessions.
    pub(super) fn worker(&self, now: i64, pricing: &dyn collect::Pricing) -> Value {
        collect::worker_from_rows(
            &self.rows.iter().collect::<Vec<_>>(),
            &self.sessions.iter().collect::<Vec<_>>(),
            self.host,
            now,
            pricing,
        )
    }
}

/// A computer's Claude part set aside while the pass reads the other computers: its rows,
/// sessions and identities, written in the checkpoint's row format and compressed. Rows and
/// sessions come back in their order, every field as it was.
pub(super) enum Parked<'a> {
    /// Kept as it is (it could not be written).
    Held(Part<'a>),
    Aside {
        host: &'a str,
        state_index: usize,
        bytes: Vec<u8>,
    },
}

#[derive(serde::Serialize)]
struct PartRef<'a> {
    rows: &'a [ScanRow],
    sessions: &'a [ScanRow],
    identities: Vec<(&'a str, &'a str, bool)>,
}

#[derive(serde::Deserialize)]
struct PartOwned {
    rows: Vec<ScanRow>,
    sessions: Vec<ScanRow>,
    identities: Vec<(String, String, bool)>,
}

impl<'a> Part<'a> {
    /// Set the part aside, compressed (a part that cannot be written is kept as it is).
    pub(super) fn park(self) -> Parked<'a> {
        let bytes = (|| {
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).ok()?;
            serde_json::to_writer(
                &mut encoder,
                &PartRef {
                    rows: &self.rows,
                    sessions: &self.sessions,
                    identities: self.identities.entries().collect(),
                },
            )
            .ok()?;
            encoder.finish().ok()
        })();
        match bytes {
            Some(bytes) => Parked::Aside {
                host: self.host,
                state_index: self.state_index,
                bytes,
            },
            None => Parked::Held(self),
        }
    }
}

impl<'a> Parked<'a> {
    /// The index of the part's source in the pass's states.
    pub(super) fn state_index(&self) -> usize {
        match self {
            Parked::Held(part) => part.state_index,
            Parked::Aside { state_index, .. } => *state_index,
        }
    }

    /// The part as it was set aside.
    pub(super) fn restore(self) -> Result<Part<'a>> {
        let (host, state_index, bytes) = match self {
            Parked::Held(part) => return Ok(part),
            Parked::Aside {
                host,
                state_index,
                bytes,
            } => (host, state_index, bytes),
        };
        let (owned, _) = aac_store::scan_row::with_interner(Default::default(), || {
            zstd::stream::read::Decoder::new(&bytes[..])
                .map_err(Error::from)
                .and_then(|decoder| {
                    serde_json::from_reader::<_, PartOwned>(std::io::BufReader::new(decoder))
                        .map_err(Error::from)
                })
        });
        let owned = owned?;
        Ok(Part {
            host,
            state_index,
            rows: owned.rows,
            sessions: owned.sessions,
            identities: Identities::from_entries(owned.identities),
        })
    }
}

/// One parsed owner map per (store instance, key) for the whole process: the activity pass
/// re-reads these maps every minute, and the redb read plus JSON parse of the published-mirror
/// map was about 100 MB of transient work per pass. `save` refreshes the entry; a new store
/// instance (a fresh database, a rebuilt one) starts cold. Bounded: a service holds one store's
/// three maps; the cap only evicts test fixtures' entries.
type MemoMap = HashMap<(String, String), Arc<Owners>>;
static MEMO: Mutex<Option<MemoMap>> = Mutex::new(None);
/// One store holds three maps (`LIVE`/`HISTORY`/`HISTORY_MIRRORS`); the memo never
/// retains a fourth (MEM5 Lane C bounded eviction, was 32): overflow evicts one
/// entry instead of keeping test fixtures' maps resident beside the service's.
const MEMO_ENTRIES: usize = 3;

fn memo_lock() -> std::sync::MutexGuard<'static, Option<MemoMap>> {
    MEMO.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn memo_remember(instance: &str, key: &str, owners: Arc<Owners>) {
    let mut held = memo_lock();
    let map = held.get_or_insert_with(HashMap::new);
    // Re-`save` of a held key refreshes it without growing the map.
    if !map.contains_key(&(instance.to_owned(), key.to_owned())) && map.len() >= MEMO_ENTRIES {
        // Evict one entry (any): the service's three keys stay the steady set,
        // and an evicted map is simply re-read on demand next pass.
        if let Some(evict) = map.keys().next().cloned() {
            map.remove(&evict);
        }
    }
    map.insert((instance.to_owned(), key.to_owned()), owners);
}

pub(super) fn load(store: &Engine, key: &str) -> Result<Arc<Owners>> {
    if let Some(owners) = memo_lock()
        .as_ref()
        .and_then(|map| map.get(&(store.instance_id.clone(), key.to_owned())))
    {
        return Ok(Arc::clone(owners));
    }
    let owners: Owners = match analytics_cache::get(store, key)? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => Owners::new(),
    };
    let owners = Arc::new(owners);
    memo_remember(&store.instance_id, key, Arc::clone(&owners));
    Ok(owners)
}

pub(super) fn save(store: &Engine, key: &str, owners: &Owners) -> Result<()> {
    let bytes = serde_json::to_vec(owners)?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(
            "source_limit",
            "Claude mirror owner map size limit",
        ));
    }
    analytics_cache::put(store, key, &bytes)?;
    memo_remember(&store.instance_id, key, Arc::new(owners.clone()));
    Ok(())
}

pub(super) fn row_identity<'a>(
    row: &ScanRow,
    identities: &'a Identities,
) -> Option<(&'a str, bool)> {
    identities.get(&row.file)
}

/// A confirmed original always wins. Otherwise retain a prior owner across failures
/// and partial reads. An authoritative read without the log lets the next holder win.
pub(super) fn select_live(
    store: &Engine,
    parts: &mut [Part<'_>],
    states: &mut [Value],
) -> Result<()> {
    let previous = load(store, LIVE)?;
    let historical = load(store, HISTORY)?;
    let owners = choose(parts, states, &previous, &historical);
    if owners != *previous {
        save(store, LIVE, &owners)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn select(
    parts: &mut [Part<'_>],
    states: &mut [Value],
    previous: &Owners,
) -> Option<Owners> {
    let owners = choose(parts, states, previous, &Owners::new());
    (owners != *previous).then_some(owners)
}

/// Store original readings while evidence from other computers is pending.
pub(super) fn keep_historical_originals(store: &Engine, part: &mut Part<'_>) -> Result<()> {
    let mut owners = load(store, HISTORY)?;
    let mut changed = false;
    let mut keep = |row: &ScanRow| {
        let Some((name, false)) = row_identity(row, &part.identities) else {
            return false;
        };
        // Copy the shared map only to add a name it lacks.
        if !owners.contains_key(name) {
            Arc::make_mut(&mut owners).insert(name, part.host);
            changed = true;
        }
        true
    };
    part.rows.retain(&mut keep);
    part.sessions.retain(keep);
    if changed {
        save(store, HISTORY, &owners)?;
    }
    Ok(())
}

pub(super) fn choose(
    parts: &mut [Part<'_>],
    states: &mut [Value],
    previous: &Owners,
    committed: &Owners,
) -> Owners {
    let configured: HashSet<&str> = states
        .iter()
        .filter(|state| state["tool"] == "claude")
        .filter_map(|state| state["host"].as_str())
        .collect();
    let mut complete = HashSet::new();
    let mut present = HashSet::new();
    let mut originals = Owners::new();
    let mut owners = Owners::new();
    // Parts arrive in registry order, independent of any future scan concurrency.
    for part in parts.iter() {
        let host = part.host;
        if matches!(
            states[part.state_index]["state"].as_str(),
            Some("ok" | "no_usage" | "not_installed")
        ) {
            complete.insert(host);
        }
        for identity in part.identities.values() {
            if let (name, false) = identity {
                present.insert((host, name));
                originals.insert_absent(name, host);
            }
        }
        for row in part.rows.iter().chain(&part.sessions) {
            if let Some((name, true)) = row_identity(row, &part.identities) {
                present.insert((host, name));
                owners.insert_absent(name, host);
            }
        }
    }
    for (name, host) in previous.iter() {
        if configured.contains(host)
            && (!complete.contains(host) || present.contains(&(host, &*name)))
        {
            owners.insert(&name, host);
        }
    }
    owners.extend_from(committed);
    owners.extend_from(&originals);

    for part in parts {
        let keep = |row: &ScanRow| match row_identity(row, &part.identities) {
            Some((name, true)) => {
                !originals.contains_key(name)
                    && owners.get(name).is_some_and(|host| host == part.host)
            }
            _ => true,
        };
        part.rows.retain(keep);
        part.sessions.retain(keep);
        let state = &mut states[part.state_index]["state"];
        if *state == "ok" && part.rows.is_empty() && part.sessions.is_empty() {
            *state = json!("no_usage");
        }
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::*;
    use aac_store::{ops, report};

    fn typed(rows: &[Value]) -> Vec<ScanRow> {
        rows.iter()
            .map(|row| serde_json::from_value(row.clone()).expect("scanner row"))
            .collect()
    }

    fn part(host: &'static str, state_index: usize, mirror: bool, time: i64) -> Part<'static> {
        let hour = aac_store::usage::iso(time);
        let mut row = json!({
            "f": "fixture-file", "k": "claude", "p": "anthropic", "m": "claude-opus-4-5",
            "i": 100, "o": 0, "cr": 0, "cw": 0, "c": 0.01, "n": 1,
        });
        let mut session = row.clone();
        row["h"] = json!(hour[..13].replace('T', " ") + ":00");
        session["s"] = json!("0123456789abcdef");
        session["a"] = json!(time);
        session["z"] = json!(time);
        Part {
            host,
            state_index,
            rows: typed(&[row]),
            sessions: typed(&[session]),
            identities: Identities::from_value(&json!({
                "fixture-file": {"claudeName": "same-basename-digest", "claudeMirror": mirror}
            })),
        }
    }

    fn state(host: &str, word: &str) -> Value {
        json!({ "tool": "claude", "host": host, "state": word })
    }

    #[test]
    fn a_parked_part_comes_back_as_it_was() {
        let mut rows = Vec::new();
        let mut sessions = Vec::new();
        let mut prints = serde_json::Map::new();
        for index in 0..40u64 {
            let file = format!("file-{index}");
            rows.push(json!({
                "f": file, "k": "claude", "m": format!("model-{}", index % 3),
                "h": format!("2026-11-{:02} {:02}:00", 1 + index % 27, index % 24),
                "i": index * 7, "o": index, "cr": 3, "cw": 4, "c": 0.1 + index as f64 / 3.0,
                "n": index % 5,
                "p": if index % 2 == 0 { json!("anthropic") } else { Value::Null },
                "costBasis": if index % 7 == 0 { json!("unknown") } else { Value::Null },
            }));
            sessions.push(json!({
                "f": file, "k": "claude", "m": "claude-opus-4-5", "s": format!("{index:016x}"),
                "a": 1_795_000_000_000u64 + index, "z": 1_795_000_100_000u64 + index,
                "i": 1, "o": 2, "cr": 3, "cw": 4, "c": 0.0, "n": 1,
            }));
            prints.insert(
                file,
                json!({"claudeName": format!("name-{}", index % 4),
                    "claudeMirror": index % 3 == 0}),
            );
        }
        let part = Part {
            host: "nas1",
            state_index: 7,
            rows: typed(&rows),
            sessions: typed(&sessions),
            identities: Identities::from_value(&Value::Object(prints)),
        };
        let (rows, sessions, identities) = (
            part.rows.clone(),
            part.sessions.clone(),
            part.identities.clone(),
        );
        let parked = part.park();
        assert!(matches!(parked, Parked::Aside { .. }), "set aside");
        assert_eq!(parked.state_index(), 7);
        let back = parked.restore().expect("restore");
        assert_eq!((back.host, back.state_index), ("nas1", 7));
        assert_eq!(back.rows, rows);
        assert_eq!(back.sessions, sessions);
        let sorted = |identities: &Identities| {
            let mut entries: Vec<(String, String, bool)> = identities
                .entries()
                .map(|(file, name, mirror)| (file.to_owned(), name.to_owned(), mirror))
                .collect();
            entries.sort();
            entries
        };
        assert_eq!(back.identities.len(), identities.len());
        assert_eq!(sorted(&back.identities), sorted(&identities));
    }

    #[test]
    fn analytics_claude_mirror_counts_one_report_and_one_session() {
        let now = aac_store::engine::now_ms() as i64;
        let time = now - 3_600_000;
        let mut parts = [part("mac", 0, true, time), part("ubuntu", 1, false, time)];
        let mut states = [state("mac", "ok"), state("ubuntu", "ok")];
        select(&mut parts, &mut states, &Owners::new());
        let fixture = tempfile::tempdir().expect("fixture directory");
        let pricing = super::super::ResolverPricing::new(fixture.path());
        let workers = parts
            .iter()
            .map(|part| {
                let data = part.worker(now, &pricing);
                json!({ "tool": "claude", "host": part.host, "data": data })
            })
            .collect::<Vec<_>>();
        let reports = report::build_activity_reports(&report::ReportArgs {
            local: &Default::default(),
            remote: &workers,
            states: &states,
            window_from_ms: now - 86_400_000,
            now,
            local_complete: &Default::default(),
            scanner_owned: &["claude".into()].into_iter().collect(),
        });
        let measured = reports
            .iter()
            .filter(|r| r["state"] == "ok")
            .collect::<Vec<_>>();
        assert_eq!(measured.len(), 1);
        assert_eq!(reports[0]["state"], "no_usage");
        assert_eq!(measured[0]["computer"], "ubuntu");
        assert_eq!(reports[0]["sessions"], json!([]));
        assert_eq!(
            measured[0]["sessions"]
                .as_array()
                .expect("session rows")
                .len(),
            1
        );

        let engine = ops::open_engine(fixture.path().to_str().expect("fixture path"), None, None)
            .expect("open activity store");
        let receipt = ops::call(
            &engine,
            "activityIngest",
            &json!({ "reports": reports }).to_string(),
        )
        .expect("ingest activity reports");
        assert_eq!(receipt["refused"], json!([]));
        let range = ops::call(
            &engine,
            "activityRange",
            &json!({ "fromMs": now - 86_400_000, "toMs": now }).to_string(),
        )
        .expect("read activity range");
        let tokens: u64 = range["rows"]
            .as_array()
            .expect("activity range rows")
            .iter()
            .map(|row| row[5].as_u64().expect("input token count"))
            .sum();
        assert_eq!(tokens, 100);
    }

    #[test]
    fn analytics_claude_scanner_mirror_counts_one_aggregate_event() {
        let now = aac_store::engine::now_ms() as i64;
        // The real reader emits only an hourly row for a message without a session ID.
        let event = json!({
            "type": "assistant",
            "timestamp": aac_store::usage::iso(now - 3_600_000),
            "message": {
                "id":"same-assistant-message", "model":"claude-opus-4-5",
                "usage":{"input_tokens":70,"output_tokens":30}
            }
        });
        let scan = |host, state_index, directory| {
            let home = tempfile::tempdir().expect("scanner fixture home");
            let project = home.path().join(".claude/projects").join(directory);
            std::fs::create_dir_all(&project).expect("project directory");
            std::fs::write(project.join("same-log.jsonl"), format!("{event}\n"))
                .expect("assistant message");
            let request = json!({"kinds":["claude"], "minDateMs":now - 86_400_000, "cwd":false});
            let request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("scanner request");
            let answer = aac_usage::protocol::scan(
                &request,
                home.path(),
                &aac_usage::discover::Env::default(),
            );
            let rows = typed(answer["rows"].as_array().expect("scanner rows"));
            assert_eq!(rows.len(), 1, "{host}: {answer}");
            assert_eq!(answer["srows"], json!([]));
            Part {
                host,
                state_index,
                rows,
                sessions: vec![],
                identities: Identities::from_value(&answer["kinds"]["claude"]["fingerprints"]),
            }
        };
        let mut parts = [
            scan("odroid", 0, "ssh-12345678-1234-1234-1234-123456789abc"),
            scan("nas1", 1, "original-project"),
        ];
        let name = |part: &Part<'_>| {
            let (name, mirror) =
                row_identity(&part.rows[0], &part.identities).expect("scanner fingerprint");
            (name.to_owned(), mirror)
        };
        let (mirror_name, mirror) = name(&parts[0]);
        let (original_name, original_is_mirror) = name(&parts[1]);
        assert_eq!(mirror_name, original_name);
        assert!(mirror);
        assert!(!original_is_mirror);
        let mut states = [state("odroid", "ok"), state("nas1", "ok")];
        select(&mut parts, &mut states, &Owners::new());
        let rows: Vec<&ScanRow> = parts.iter().flat_map(|part| &part.rows).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens[0], 70);
        assert_eq!(rows[0].tokens[1], 30);
        assert_eq!(rows[0].events, 1);
    }

    #[test]
    fn analytics_claude_mirror_keeps_the_saved_owner_until_a_complete_read() {
        let mut states = [state("nas1", "ok"), state("mac", "ok")];
        let mut parts = [part("nas1", 0, true, 0), part("mac", 1, true, 0)];
        let owners = select(&mut parts, &mut states, &Owners::new()).expect("initial mirror owner");
        assert_eq!(parts[0].rows.len(), 1);
        assert!(parts[1].sessions.is_empty());

        let fixture = tempfile::tempdir().expect("fixture directory");
        let open = || {
            ops::open_engine(fixture.path().to_str().expect("path"), None, None)
                .expect("open activity store")
        };
        let engine = open();
        save(&engine, LIVE, &owners).expect("persist owner in the Store");
        engine.close().expect("close owner Store");
        drop(engine);
        let owners = load(&open(), LIVE).expect("owner survives reopening");
        let mut states = [state("nas1", "unreadable"), state("mac", "ok")];
        let mut parts = [part("mac", 1, true, 0)];
        assert!(select(&mut parts, &mut states, &owners).is_none());
        assert!(parts[0].rows.is_empty());
        assert!(parts[0].sessions.is_empty());

        let mut states = [state("nas1", "no_usage"), state("mac", "ok")];
        let mut parts = [
            Part {
                host: "nas1",
                state_index: 0,
                rows: vec![],
                sessions: vec![],
                identities: Identities::default(),
            },
            part("mac", 1, true, 0),
        ];
        let reelected = select(&mut parts, &mut states, &owners).expect("re-elected mirror owner");
        assert_eq!(reelected.get("same-basename-digest"), Some("mac"));
        assert_eq!(parts[1].rows.len(), 1);
        assert_eq!(parts[1].sessions.len(), 1);
    }

    #[test]
    fn the_owner_map_is_parsed_once_and_shared_across_loads() {
        let fixture = tempfile::tempdir().expect("fixture directory");
        let engine = ops::open_engine(fixture.path().to_str().expect("path"), None, None)
            .expect("open activity store");
        let mut owners = Owners::new();
        owners.insert("same-basename-digest", "nas1");
        save(&engine, LIVE, &owners).expect("save");
        let first = load(&engine, LIVE).expect("first load");
        let second = load(&engine, LIVE).expect("second load");
        assert!(
            Arc::ptr_eq(&first, &second),
            "loads of an unchanged map share the one parse"
        );
        // A save refreshes the shared map for the next load.
        let mut updated = Owners::new();
        updated.insert("same-basename-digest", "mac");
        save(&engine, LIVE, &updated).expect("re-save");
        let third = load(&engine, LIVE).expect("third load");
        assert!(!Arc::ptr_eq(&first, &third));
        assert_eq!(third.get("same-basename-digest"), Some("mac"));
        engine.close().expect("close");
    }

    #[test]
    fn two_stores_do_not_share_one_memo_entry() {
        let a = tempfile::tempdir().expect("store a");
        let b = tempfile::tempdir().expect("store b");
        let ea = ops::open_engine(a.path().to_str().expect("path"), None, None).expect("open a");
        let eb = ops::open_engine(b.path().to_str().expect("path"), None, None).expect("open b");
        let mut owners = Owners::new();
        owners.insert("name", "nas1");
        save(&ea, HISTORY, &owners).expect("save a");
        let from_b = load(&eb, HISTORY).expect("load b");
        assert_eq!(from_b.len(), 0, "another store's map is not this store's");
        ea.close().expect("close a");
        eb.close().expect("close b");
    }
}
