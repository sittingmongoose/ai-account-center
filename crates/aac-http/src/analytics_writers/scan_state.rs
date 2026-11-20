//! Per-host incremental scanner state. A scan that was cut short retains unvisited files.
use aac_store::{analytics_cache, engine::Engine, error::Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

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

    pub fn save(&self, store: &Engine, host: &str) -> Result<()> {
        analytics_cache::put(
            store,
            &format!("live/scanner/{host}"),
            &serde_json::to_vec(self)?,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scanner_cache_keeps_rows_on_restart_and_adds_or_replaces_per_file() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("store");
        let open = || aac_store::ops::open_engine(&path.to_string_lossy(), None, None).unwrap();
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
        cache.save(&store, "fixture").unwrap();
        store.close().unwrap();
        drop(store);
        let store = open();
        cache = Cache::load(&store, "fixture").unwrap();
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
