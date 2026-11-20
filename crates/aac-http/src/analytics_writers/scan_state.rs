//! Per-host incremental scanner state. A scan that was cut short retains unvisited files.
use aac_store::{analytics_cache, engine::Engine, error::Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

const REQUEST_BYTES: usize = 8 * 1024 * 1024;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_ROWS: usize = 100_000;

pub(super) fn requests(
    mut args: Value,
    kinds: &[String],
    fingerprints: &Map<String, Value>,
    settled: &HashMap<String, u64>,
    now: i64,
) -> Vec<(Vec<String>, Value)> {
    let slot = now.max(0) as u64 / aac_usage::collector::SETTLED_CHECK_MS;
    let settings = json!({ "settledBefore": settled, "settledSlot": slot });
    args["fingerprints"] = json!(fingerprints);
    args["fingerprints"]["_store"] = settings.clone();
    if args.to_string().len() <= REQUEST_BYTES {
        return vec![(kinds.to_vec(), args)];
    }

    args["fingerprints"] = json!({"_store":settings});
    kinds
        .iter()
        .map(|kind| {
            let mut one = args.clone();
            one["kinds"] = json!([kind]);
            let mut selected = Map::new();
            if let Some(prints) = fingerprints.get(kind).and_then(Value::as_object) {
                let cutoff = settled.get(kind).copied().unwrap_or(0) as i64;
                let (settled, mut files): (Vec<_>, Vec<_>) = prints
                    .iter()
                    .partition(|(_, print)| aac_usage::collector::settled_print(print, cutoff));
                if !settled.is_empty() {
                    let settled = settled
                        .into_iter()
                        .map(|(key, print)| (key.clone(), print.clone()))
                        .collect();
                    let Some(packed) = aac_usage::protocol::pack_settled(&settled) else {
                        return (vec![kind.clone()], Value::Null);
                    };
                    one["fingerprints"]["_store"]["settledPacked"][kind] = json!(packed);
                }
                // Carry unfinished cursors first. Other files can be re-read to recover any
                // fingerprint omitted to fit the existing scanner frame.
                files.sort_by_key(|(_, print)| {
                    (
                        print["readThrough"].is_null(),
                        std::cmp::Reverse(print["mtimeMs"].as_i64()),
                    )
                });
                let mut remaining =
                    REQUEST_BYTES.saturating_sub(one.to_string().len() + kind.len() + 12);
                for (key, print) in files {
                    let bytes = json!(key).to_string().len() + print.to_string().len() + 2;
                    if bytes <= remaining {
                        selected.insert(key.clone(), print.clone());
                        remaining -= bytes;
                    }
                }
            }
            one["fingerprints"][kind] = Value::Object(selected);
            if one.to_string().len() > REQUEST_BYTES {
                return (vec![kind.clone()], Value::Null);
            }
            (vec![kind.clone()], one)
        })
        .collect()
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Cache {
    pub fingerprints: Map<String, Value>,
    pub rows: Vec<Value>,
    #[serde(default)]
    pub srows: Vec<Value>,
    #[serde(default)]
    states: Map<String, Value>,
}

impl Cache {
    pub fn load(store: &Engine, host: &str) -> Result<Self> {
        if let Some(bytes) = analytics_cache::get(store, &format!("live/scanner/{host}"))? {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let archived = analytics_cache::archived(
            store,
            &format!("legacy/cache/analytics-remote-v1/{host}.json"),
        )?;
        Ok(archived
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let prints: usize = self
            .fingerprints
            .values()
            .filter_map(Value::as_object)
            .map(Map::len)
            .sum();
        if self.rows.len() > CACHE_ROWS || self.srows.len() > CACHE_ROWS || prints > CACHE_ROWS {
            return Err(aac_store::error::Error::new(
                "source_limit",
                "Analytics scanner row limit",
            ));
        }
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > CACHE_BYTES {
            return Err(aac_store::error::Error::new(
                "source_limit",
                "Analytics scanner state size limit",
            ));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    fn save(&self, store: &Engine, host: &str) -> Result<()> {
        analytics_cache::put(store, &format!("live/scanner/{host}"), &self.encode()?)
    }

    pub fn apply(&mut self, kind: &str, answer: &Value, mut complete: bool, cutoff: i64) -> bool {
        let state = &answer["kinds"][kind];
        let Some(fresh) = state["fingerprints"].as_object() else {
            return false;
        };
        if matches!(
            state["state"].as_str(),
            Some("pending" | "error" | "unreadable")
        ) || kind == "antigravity" && state["unreadable"] == true
        {
            return false;
        }
        let empty = Map::new();
        let prior = self
            .fingerprints
            .get(kind)
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let mut replace: HashSet<&str> = state["replacedFiles"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let mut accepted = HashSet::new();
        let mut rejected = Vec::new();
        let mut prints = Map::new();
        for (key, print) in fresh {
            let old = prior.get(key);
            if print["settledConfirmed"] == true {
                if let Some(mut saved) = old.and_then(Value::as_object).cloned() {
                    saved.insert(
                        "settledCheckedSlot".into(),
                        print["settledCheckedSlot"].clone(),
                    );
                    prints.insert(key.clone(), Value::Object(saved));
                } else {
                    complete = false;
                }
                continue;
            }
            if let Some(from) = print["readFrom"].as_u64() {
                let matches = old.is_some_and(|old| {
                    (old["readThrough"].as_u64() == Some(from) && same_file(old, print))
                        || (old["readThrough"].is_null()
                            && old.get("appendState").is_some()
                            && old["size"].as_u64() == Some(from)
                            && print["fileIdentity"].is_string()
                            && old["fileIdentity"] == print["fileIdentity"])
                });
                if !matches {
                    rejected.push(key);
                    complete = false;
                    replace.remove(key.as_str());
                    continue;
                }
                replace.remove(key.as_str());
            } else if !old.is_some_and(|old| same_file(old, print)) {
                replace.insert(key);
            }
            accepted.insert(key.as_str());
            let mut saved = print.clone();
            if let Some(fields) = saved.as_object_mut() {
                fields.remove("readFrom");
            }
            prints.insert(key.clone(), saved);
        }
        if !complete {
            for (key, old) in prior {
                if !replace.contains(key.as_str()) && !rejected.contains(&key) {
                    prints.entry(key.clone()).or_insert_with(|| old.clone());
                }
            }
        }
        let incoming = |key: &str| {
            answer[key].as_array().into_iter().flatten().filter(|row| {
                row["k"] == kind
                    && row["f"]
                        .as_str()
                        .is_some_and(|file| accepted.contains(file))
            })
        };
        for row in incoming("rows").chain(incoming("srows")) {
            if let Some(file) = row["f"].as_str()
                && fresh[file]["readFrom"].is_null()
            {
                replace.insert(file);
            }
        }
        let retain = |row: &Value| {
            row["k"] != kind
                || row["f"].as_str().is_some_and(|file| {
                    !replace.contains(file) && (!complete || prints.contains_key(file))
                })
        };
        self.rows
            .retain(|row| retain(row) && in_window(row, false, cutoff));
        self.srows
            .retain(|row| retain(row) && in_window(row, true, cutoff));
        self.rows.extend(
            incoming("rows")
                .filter(|row| in_window(row, false, cutoff))
                .cloned(),
        );
        self.srows.extend(
            incoming("srows")
                .filter(|row| in_window(row, true, cutoff))
                .cloned(),
        );
        fold(&mut self.rows, false);
        fold(&mut self.srows, true);
        self.fingerprints
            .insert(kind.to_owned(), Value::Object(prints));
        let mut metadata = state.clone();
        if let Some(fields) = metadata.as_object_mut() {
            fields.remove("fingerprints");
            fields.remove("replacedFiles");
        }
        self.states.insert(kind.to_owned(), metadata);
        complete
    }
}

fn same_file(old: &Value, fresh: &Value) -> bool {
    ["size", "mtimeMs", "head", "tail", "walSize", "walMtimeMs"]
        .into_iter()
        .all(|key| old[key] == fresh[key])
        && (old["fileIdentity"].is_null() || old["fileIdentity"] == fresh["fileIdentity"])
}

fn in_window(row: &Value, session: bool, cutoff: i64) -> bool {
    if session {
        row["z"].as_i64().is_some_and(|end| end >= cutoff)
    } else {
        row["h"]
            .as_str()
            .and_then(|hour| {
                aac_views::js::parse_date(&format!("{}:00Z", hour.replace(' ', "T")))
                    .ok()
                    .flatten()
            })
            .is_some_and(|hour| hour >= cutoff.div_euclid(3_600_000) * 3_600_000)
    }
}

fn fold(rows: &mut Vec<Value>, session: bool) {
    let mut positions = HashMap::new();
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        let key = ["k", "f", "m", "p", if session { "s" } else { "h" }]
            .map(|key| row[key].as_str().unwrap_or_default().to_owned());
        let key = (key, row["c"].as_f64().unwrap_or_default() > 0.0);
        let position = *positions.entry(key).or_insert(out.len());
        if let Some(previous) = out.get_mut(position) {
            for key in ["i", "o", "cr", "cw", "n"] {
                if let (Some(left), Some(right)) = (previous[key].as_u64(), row[key].as_u64()) {
                    previous[key] = Value::from(left.saturating_add(right));
                }
            }
            if let (Some(left), Some(right)) = (previous["c"].as_f64(), row["c"].as_f64()) {
                previous["c"] = Value::from(left + right);
            }
            if session {
                for (key, compare) in [("a", i64::min as fn(i64, i64) -> i64), ("z", i64::max)] {
                    if let (Some(left), Some(right)) = (previous[key].as_i64(), row[key].as_i64()) {
                        previous[key] = Value::from(compare(left, right));
                    }
                }
            }
        } else {
            out.push(row);
        }
    }
    *rows = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn scanner_cache_keeps_rows_on_restart_and_adds_or_replaces_per_file() {
        let home = tempfile::tempdir().expect("fixture home");
        let path = home.path().join("store");
        let open = || {
            aac_store::ops::open_engine(&path.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let first = json!({"size":100,"mtimeMs":1,"fileIdentity":"9:1","appendState":{}});
        let second =
            json!({"size":150,"mtimeMs":2,"fileIdentity":"9:1","appendState":{},"readFrom":100});
        let replaced = json!({"size":80,"mtimeMs":3,"fileIdentity":"9:2","appendState":{}});
        let answer = |fingerprints: Value, rows: Vec<Value>| {
            json!({
                "kinds":{"codex":{"state":"ok","fingerprints":fingerprints}},"rows":rows,
            })
        };
        let row = |tokens| json!({"k":"codex","f":"file","m":"model","h":"2026-11-03 03:00","i":tokens,"n":1,"c":0});
        let store = open();
        let mut cache = Cache::default();
        assert!(cache.apply(
            "codex",
            &answer(json!({"file":first}), vec![row(100)]),
            true,
            0
        ));
        cache.save(&store, "fixture").expect("save scan cache");
        store.close().expect("close fixture store");
        drop(store);
        let store = open();
        cache = Cache::load(&store, "fixture").expect("load scan cache");
        assert!(cache.apply(
            "codex",
            &answer(json!({"file":second}), vec![row(50)]),
            true,
            0
        ));
        assert_eq!(cache.rows[0]["i"], 150);
        assert_eq!(cache.rows[0]["n"], 2);
        let prints = cache.fingerprints["codex"].clone();
        assert!(cache.apply("codex", &answer(prints, vec![]), true, 0));
        assert_eq!(cache.rows[0]["i"], 150);
        assert!(!cache.apply("codex", &answer(json!({}), vec![]), false, 0));
        assert_eq!(cache.rows[0]["i"], 150);
        assert!(cache.apply(
            "codex",
            &answer(json!({"file":replaced}), vec![row(20)]),
            true,
            0
        ));
        assert_eq!(cache.rows[0]["i"], 20);
    }

    #[test]
    fn analytics_split_request_keeps_settled_fingerprints_and_cached_usage() {
        let kinds = ["codex".to_owned(), "claude".to_owned()];
        let print = json!({
            "pathKey":"settled-path", "size":100, "mtimeMs":1, "newestUsageMs":1,
            "fileIdentity":"9:1", "head":"first", "tail":"last", "appendState":{"model":"fixture"},
        });
        let fingerprints: Map<String, Value> = serde_json::from_value(json!({
            "codex":{"settled":print,"too-large-active":{"extra":"a".repeat(REQUEST_BYTES)}},
        }))
        .unwrap();
        let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
        let batches = requests(
            args,
            &kinds,
            &fingerprints,
            &[("codex".into(), 2)].into_iter().collect(),
            600_000,
        );
        assert_eq!(batches.len(), 2);
        let args = &batches[0].1;
        assert_eq!(args["fingerprints"]["codex"], json!({}));
        let request = aac_usage::protocol::parse_request(args.to_string().as_bytes()).unwrap();
        assert_eq!(request.fingerprints["codex"]["settled"], print);

        let mut cache = Cache {
            fingerprints,
            rows: vec![
                json!({"k":"codex","f":"settled","m":"fixture","h":"2026-11-03 03:00","i":100}),
            ],
            ..Cache::default()
        };
        let confirmed = json!({"kinds":{"codex":{"state":"ok","settledPacked":true,
            "fingerprints":{"settled":{"settledConfirmed":true,"settledCheckedSlot":1}}
        }}});
        assert!(cache.apply("codex", &confirmed, true, 0));
        assert_eq!(cache.rows[0]["i"], 100);
        assert_eq!(
            cache.fingerprints["codex"]["settled"]["appendState"],
            print["appendState"]
        );
        assert_eq!(
            cache.fingerprints["codex"]["settled"]["settledCheckedSlot"],
            1
        );
    }

    #[test]
    fn analytics_deadline_slices_reach_rollout_tail_with_full_read_totals() {
        let home = tempfile::tempdir().expect("fixture home");
        let store_dir = home.path().join("store");
        let store = aac_store::ops::open_engine(&store_dir.to_string_lossy(), None, None)
            .expect("open fixture store");
        let path = home.path().join(".codex/sessions/2026/11/15/rollout-2026-11-15T13-00-00-4a16b43d-60b9-4aab-bbf0-2908136b7bde.jsonl");
        std::fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        let mut file = std::fs::File::create(path).expect("create fixture rollout");
        writeln!(file, "{}", json!({"type":"session_meta","payload":{"id":"long-fixture","originator":"codex_cli_rs","cli_version":"1"}})).expect("write fixture record");
        writeln!(
            file,
            "{}",
            json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}})
        )
        .expect("write fixture record");
        let filler = "a".repeat(4096);
        let n = 600;
        for index in 1..=n {
            writeln!(
                file,
                "{}",
                json!({"type":"response_item","payload":{"type":"message","content":filler}})
            )
            .expect("write fixture record");
            let at = if index == n {
                "2026-11-15T14:00:00Z"
            } else {
                "2026-11-15T13:00:00Z"
            };
            writeln!(file, "{}", json!({"timestamp":at,"type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":100*index,"output_tokens":index}}
            }})).expect("write fixture record");
        }
        drop(file);
        let kinds = ["codex".to_owned()];
        let scan = |cache: &Cache, line_budget: Option<u64>| {
            let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
            let [(batch, request)] =
                requests(args, &kinds, &cache.fingerprints, &HashMap::new(), 0)
                    .try_into()
                    .expect("one scanner request");
            let mut request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("parse writer request");
            request.line_budget = line_budget;
            let answer = aac_usage::protocol::scan(
                &request,
                home.path(),
                &aac_usage::discover::Env::default(),
            );
            assert!(super::super::valid_scan_answer(&answer, &batch));
            answer
        };
        let mut full = Cache::default();
        let full_scan = scan(&full, None);
        assert!(full.apply("codex", &full_scan, true, 0));
        assert_eq!(full.rows.len(), 2);

        let mut last_cursor = 0;
        for pass in 1..30 {
            let mut sliced = Cache::load(&store, "fixture").expect("load scan cache");
            let answer = scan(&sliced, Some(79));
            let print = answer["kinds"]["codex"]["fingerprints"]
                .as_object()
                .expect("scanner fingerprints")
                .values()
                .next()
                .expect("rollout fingerprint");
            if let Some(cursor) = print["readThrough"].as_u64() {
                assert!(cursor > last_cursor, "pass {pass} returned an old cursor");
                last_cursor = cursor;
            }
            let complete = super::super::scan_state(&answer, "codex").0 == "ok";
            assert_eq!(sliced.apply("codex", &answer, complete, 0), complete);
            sliced.save(&store, "fixture").expect("save sliced cache");
            if complete {
                assert!(pass > 10);
                assert_eq!(sliced.rows, full.rows);
                assert_eq!(sliced.srows, full.srows);
                assert_eq!(sliced.srows[0]["n"], n);
                return;
            }
        }
        panic!("the writer never reached the tail of the rollout");
    }
}
