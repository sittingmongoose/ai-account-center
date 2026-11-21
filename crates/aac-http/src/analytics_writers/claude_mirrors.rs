//! Select Claude SSH mirrors across computers before the file keys are aggregated away.

use aac_store::{
    analytics_cache,
    engine::Engine,
    error::{Error, Result},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};

pub(super) type Owners = BTreeMap<String, String>;

pub(super) const LIVE: &str = "live/claude-mirror-owners";
pub(super) const HISTORY: &str = "history/claude-mirror-owners";
const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct Part<'a> {
    pub(super) host: &'a str,
    pub(super) state_index: usize,
    pub(super) rows: Vec<Value>,
    pub(super) sessions: Vec<Value>,
    pub(super) fingerprints: Value,
}

pub(super) fn load(store: &Engine, key: &str) -> Result<Owners> {
    match analytics_cache::get(store, key)? {
        Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
        None => Ok(Owners::new()),
    }
}

pub(super) fn save(store: &Engine, key: &str, owners: &Owners) -> Result<()> {
    let bytes = serde_json::to_vec(owners)?;
    if bytes.len() > MAX_BYTES {
        return Err(Error::new(
            "source_limit",
            "Claude mirror owner map size limit",
        ));
    }
    analytics_cache::put(store, key, &bytes)
}

pub(super) fn identity(print: &Value) -> Option<(&str, bool)> {
    let name = print["claudeName"]
        .as_str()
        .filter(|name| !name.is_empty())?;
    Some((name, print["claudeMirror"].as_bool()?))
}

pub(super) fn row_identity<'a>(row: &Value, fingerprints: &'a Value) -> Option<(&'a str, bool)> {
    identity(&fingerprints[row["f"].as_str()?])
}

/// A confirmed original always wins. Otherwise retain a prior owner across failures
/// and partial reads. An authoritative read without the log lets the next holder win.
pub(super) fn select_live(
    store: &Engine,
    parts: &mut [Part<'_>],
    states: &mut [Value],
) -> Result<()> {
    let previous = load(store, LIVE)?;
    if let Some(owners) = select(parts, states, &previous) {
        save(store, LIVE, &owners)?;
    }
    Ok(())
}

pub(super) fn select(
    parts: &mut [Part<'_>],
    states: &mut [Value],
    previous: &Owners,
) -> Option<Owners> {
    let owners = choose(parts, states, previous, &Owners::new());
    (owners != *previous).then_some(owners)
}

/// Keep committed mirror ownership across incomplete reads. A confirmed original
/// on any host is independent of that election.
pub(super) fn select_history(
    parts: &mut [Part<'_>],
    states: &mut [Value],
    committed: &Owners,
) -> Option<Owners> {
    let owners = choose(parts, states, &Owners::new(), committed);
    (owners != *committed).then_some(owners)
}

/// Store original readings while evidence from other computers is pending.
pub(super) fn keep_historical_originals(store: &Engine, part: &mut Part<'_>) -> Result<()> {
    let mut owners = load(store, HISTORY)?;
    let mut changed = false;
    let mut keep = |row: &Value| {
        let Some((name, false)) = row_identity(row, &part.fingerprints) else {
            return false;
        };
        owners.entry(name.to_owned()).or_insert_with(|| {
            changed = true;
            part.host.to_owned()
        });
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
        for print in part
            .fingerprints
            .as_object()
            .into_iter()
            .flat_map(|p| p.values())
        {
            if let Some((name, false)) = identity(print) {
                present.insert((host, name));
                originals
                    .entry(name.to_owned())
                    .or_insert_with(|| host.to_owned());
            }
        }
        for row in part.rows.iter().chain(&part.sessions) {
            if let Some((name, true)) = row_identity(row, &part.fingerprints) {
                present.insert((host, name));
                owners
                    .entry(name.to_owned())
                    .or_insert_with(|| host.to_owned());
            }
        }
    }
    for (name, host) in previous {
        if configured.contains(host.as_str())
            && (!complete.contains(host.as_str())
                || present.contains(&(host.as_str(), name.as_str())))
        {
            owners.insert(name.clone(), host.clone());
        }
    }
    owners.extend(
        originals
            .iter()
            .map(|(name, host)| (name.clone(), host.clone())),
    );

    owners.extend(
        committed
            .iter()
            .map(|(name, host)| (name.clone(), host.clone())),
    );

    for part in parts {
        let keep = |row: &Value| match row_identity(row, &part.fingerprints) {
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
    use aac_store::{collect, ops, report};

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
            rows: vec![row],
            sessions: vec![session],
            fingerprints: json!({
                "fixture-file": {"claudeName": "same-basename-digest", "claudeMirror": mirror}
            }),
        }
    }

    fn state(host: &str, word: &str) -> Value {
        json!({ "tool": "claude", "host": host, "state": word })
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
                let data = collect::worker_from_scan(
                    &part.rows,
                    &part.sessions,
                    "claude",
                    part.host,
                    now,
                    &pricing,
                );
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
            let rows = answer["rows"].as_array().expect("scanner rows").clone();
            assert_eq!(rows.len(), 1, "{host}: {answer}");
            assert_eq!(answer["srows"], json!([]));
            Part {
                host,
                state_index,
                rows,
                sessions: vec![],
                fingerprints: answer["kinds"]["claude"]["fingerprints"].clone(),
            }
        };
        let mut parts = [
            scan("odroid", 0, "ssh-12345678-1234-1234-1234-123456789abc"),
            scan("nas1", 1, "original-project"),
        ];
        let name = |part: &Part<'_>| {
            let (name, mirror) =
                row_identity(&part.rows[0], &part.fingerprints).expect("scanner fingerprint");
            (name.to_owned(), mirror)
        };
        let (mirror_name, mirror) = name(&parts[0]);
        let (original_name, original_is_mirror) = name(&parts[1]);
        assert_eq!(mirror_name, original_name);
        assert!(mirror);
        assert!(!original_is_mirror);
        let mut states = [state("odroid", "ok"), state("nas1", "ok")];
        select(&mut parts, &mut states, &Owners::new());
        let rows: Vec<&Value> = parts.iter().flat_map(|part| &part.rows).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["i"], 70);
        assert_eq!(rows[0]["o"], 30);
        assert_eq!(rows[0]["n"], 1);
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
                fingerprints: json!({}),
            },
            part("mac", 1, true, 0),
        ];
        let reelected = select(&mut parts, &mut states, &owners).expect("re-elected mirror owner");
        assert_eq!(
            reelected.get("same-basename-digest").map(String::as_str),
            Some("mac")
        );
        assert_eq!(parts[1].rows.len(), 1);
        assert_eq!(parts[1].sessions.len(), 1);
    }
}
