//! Per-host incremental scanner state. A scan that was cut short retains unvisited files.
use aac_store::scan_row::{Interner, ScanRow, with_interner};
use aac_store::{analytics_cache, engine::Engine, error::Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const REQUEST_BYTES: usize = 8 * 1024 * 1024;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_ROWS: usize = 100_000;
pub(super) const REMOTE_ARCHIVE: &str = "legacy/cache/analytics-remote-v1/";

/// A legacy remote cache, read once by the migration and as a first live scan's seed.
pub(super) struct LegacyCache {
    version: Option<u64>,
    pub scan: Cache,
    pub last_scan_at: Option<String>,
}

impl LegacyCache {
    /// A legacy cache version rebuilds a tool's rows up to the version the tool table names
    /// (`legacyRebuildThrough`); v7 and v8 wrote the current per-file cells.
    pub fn rebuilds(&self, tool: &str) -> bool {
        match self.version {
            Some(version @ 3..=6) => aac_providers::legacy_rebuild_through(tool)
                .is_some_and(|through| version <= u64::from(through)),
            Some(7..=8) => false,
            _ => true,
        }
    }

    pub fn read(bytes: &[u8]) -> Result<Self> {
        let stored = Stored::read(bytes, Interner::default())?;
        Ok(LegacyCache {
            version: stored.version,
            last_scan_at: stored.last_scan_at.clone(),
            scan: stored.into_cache(None),
        })
    }
}

/// A checkpoint as saved (`_nativeRevision` 1) or as a legacy cache wrote it, read straight into
/// typed rows: no whole-document tree, and each repeated string held once.
#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "_nativeRevision")]
    revision: Option<u8>,
    version: Option<u64>,
    #[serde(rename = "lastScanAt")]
    last_scan_at: Option<String>,
    fingerprints: Map<String, Value>,
    rows: Vec<ScanRow>,
    #[serde(default)]
    srows: Vec<ScanRow>,
    #[serde(default)]
    states: Map<String, Value>,
    #[serde(skip)]
    strings: Interner,
}

impl Stored {
    fn read(bytes: &[u8], strings: Interner) -> Result<Self> {
        let (stored, strings) = with_interner(strings, || serde_json::from_slice::<Stored>(bytes));
        let mut stored = stored?;
        stored.strings = strings;
        Ok(stored)
    }

    fn into_cache(self, saved: Option<[u8; 32]>) -> Cache {
        Cache {
            fingerprints: self.fingerprints,
            rows: self.rows,
            srows: self.srows,
            states: self.states,
            strings: self.strings,
            saved,
        }
    }
}

/// The serialized length of `value`, counted without building its text.
fn json_len(value: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

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
    if json_len(&args) <= REQUEST_BYTES {
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
                let mut remaining = REQUEST_BYTES.saturating_sub(json_len(&one) + kind.len() + 12);
                for (key, print) in files {
                    let bytes = json!(key).to_string().len() + json_len(print) + 2;
                    if bytes <= remaining {
                        selected.insert(key.clone(), print.clone());
                        remaining -= bytes;
                    }
                }
            }
            one["fingerprints"][kind] = Value::Object(selected);
            if json_len(&one) > REQUEST_BYTES {
                return (vec![kind.clone()], Value::Null);
            }
            (vec![kind.clone()], one)
        })
        .collect()
}

/// One computer's scanner state: per-file prints, the cached hour and session rows (typed, their
/// strings shared), and the read state of each tool.
#[derive(Default)]
pub(super) struct Cache {
    pub fingerprints: Map<String, Value>,
    pub rows: Vec<ScanRow>,
    pub srows: Vec<ScanRow>,
    states: Map<String, Value>,
    /// One copy of each row string, for rows this cache reads and gains.
    strings: Interner,
    /// The digest of the checkpoint this cache was loaded from or last saved as.
    saved: Option<[u8; 32]>,
}

/// The rows of one tool, borrowed.
pub(super) fn of_kind<'a>(rows: &'a [ScanRow], kind: &str) -> Vec<&'a ScanRow> {
    rows.iter().filter(|row| &*row.kind == kind).collect()
}

impl Cache {
    pub fn load(store: &Engine, host: &str) -> Result<Self> {
        let bytes = analytics_cache::get(store, &format!("live/scanner/{host}"))?;
        let saved = bytes
            .as_deref()
            .map(|bytes| Stored::read(bytes, Interner::default()))
            .transpose()?;
        let mut cache = match saved {
            Some(stored) if stored.revision == Some(1) => {
                return Ok(stored.into_cache(bytes.as_deref().map(digest)));
            }
            Some(stored) => Some(stored.into_cache(None)),
            None => None,
        };
        drop(bytes);
        if let Some(legacy) =
            analytics_cache::archived(store, &format!("{REMOTE_ARCHIVE}{host}.json"))?
                .and_then(|bytes| LegacyCache::read(&bytes).ok())
        {
            let rebuilt: HashSet<_> = aac_providers::scanner_kinds()
                .filter(|tool| legacy.rebuilds(tool))
                .collect();
            let scan = cache.get_or_insert(legacy.scan);
            scan.rows.retain(|row| !rebuilt.contains(&*row.kind));
            scan.srows.retain(|row| !rebuilt.contains(&*row.kind));
            for tool in rebuilt {
                scan.fingerprints.remove(tool);
                scan.states.remove(tool);
            }
        }
        Ok(cache.unwrap_or_default())
    }

    /// A checkpoint's bytes as a cache (any revision; a legacy form's version is not checked).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Stored::read(bytes, Interner::default())?.into_cache(None))
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
        #[derive(Serialize)]
        struct Checkpoint<'a> {
            #[serde(rename = "_nativeRevision")]
            revision: u8,
            fingerprints: &'a Map<String, Value>,
            rows: &'a [ScanRow],
            srows: &'a [ScanRow],
            states: &'a Map<String, Value>,
        }
        let bytes = serde_json::to_vec(&Checkpoint {
            revision: 1,
            fingerprints: &self.fingerprints,
            rows: &self.rows,
            srows: &self.srows,
            states: &self.states,
        })?;
        if bytes.len() > CACHE_BYTES {
            return Err(aac_store::error::Error::new(
                "source_limit",
                "Analytics scanner state size limit",
            ));
        }
        Ok(bytes)
    }

    /// Save the encoded checkpoint unless the store already holds these exact bytes. Returns
    /// whether it wrote.
    pub fn save_encoded(&mut self, store: &Engine, host: &str, bytes: &[u8]) -> Result<bool> {
        let sum = digest(bytes);
        if self.saved == Some(sum) {
            return Ok(false);
        }
        analytics_cache::put(store, &format!("live/scanner/{host}"), bytes)?;
        self.saved = Some(sum);
        Ok(true)
    }

    #[cfg(test)]
    fn save(&mut self, store: &Engine, host: &str) -> Result<()> {
        let bytes = self.encode()?;
        self.save_encoded(store, host, &bytes).map(|_| ())
    }

    /// Move one tool's rows and prints out, for a reader that outlives this cache.
    pub fn take_kind(&mut self, kind: &str) -> (Vec<ScanRow>, Vec<ScanRow>, Value) {
        let split = |rows: &mut Vec<ScanRow>| {
            let (taken, kept) = std::mem::take(rows)
                .into_iter()
                .partition(|row| &*row.kind == kind);
            *rows = kept;
            taken
        };
        let rows = split(&mut self.rows);
        let srows = split(&mut self.srows);
        let prints = self.fingerprints.remove(kind).unwrap_or(Value::Null);
        (rows, srows, prints)
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
        // Cursor deduplicates run IDs across every database it reads. With a full
        // inventory, only confirmed paths may contribute, even if some records in
        // those databases were unreadable. A limited inventory keeps missing paths.
        let enumerated = complete
            || kind == "cursor"
                && answer["truncated"] == false
                && state["state"] == "ok"
                && state["unreadableRecords"].as_u64().is_some_and(|n| n > 0)
                && ["readFailures", "unreadableFolders"]
                    .iter()
                    .all(|field| state[field].as_u64().unwrap_or(0) == 0)
                && state["searchLimit"].is_null()
                && state["walUnread"] != true;
        if !enumerated {
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
        let retain = |row: &ScanRow| {
            &*row.kind != kind
                || !replace.contains(&*row.file) && (!enumerated || prints.contains_key(&*row.file))
        };
        let mut window = Window::new(cutoff);
        self.rows
            .retain(|row| retain(row) && window.keeps(row, false));
        self.srows
            .retain(|row| retain(row) && window.keeps(row, true));
        let strings = &mut self.strings;
        for (key, rows, session) in [
            ("rows", &mut self.rows, false),
            ("srows", &mut self.srows, true),
        ] {
            rows.extend(
                incoming(key)
                    .map(|row| ScanRow::from_value(row, strings))
                    .filter(|row| window.keeps(row, session)),
            );
        }
        fold(&mut self.rows, false);
        fold(&mut self.srows, true);
        self.strings.prune();
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

/// The 31-day window: a session row by its last activity, an hour row by its hour (each distinct
/// hour parsed once).
struct Window {
    cutoff: i64,
    hours: HashMap<Arc<str>, bool>,
}

impl Window {
    fn new(cutoff: i64) -> Self {
        Window {
            cutoff,
            hours: HashMap::new(),
        }
    }

    fn keeps(&mut self, row: &ScanRow, session: bool) -> bool {
        if session {
            return row.last.is_some_and(|end| end >= self.cutoff);
        }
        let Some(hour) = &row.hour else {
            return false;
        };
        if let Some(keep) = self.hours.get(hour) {
            return *keep;
        }
        let keep = aac_views::js::parse_date(&format!("{}:00Z", hour.replace(' ', "T")))
            .ok()
            .flatten()
            .is_some_and(|at| at >= self.cutoff.div_euclid(3_600_000) * 3_600_000);
        self.hours.insert(Arc::clone(hour), keep);
        keep
    }
}

/// Merge rows of one (tool, file, model, provider, hour or session) and logged-cost class.
fn fold(rows: &mut Vec<ScanRow>, session: bool) {
    let none: Arc<str> = Arc::from("");
    let mut positions: HashMap<_, usize> = HashMap::new();
    let mut out: Vec<ScanRow> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        let slot = if session { &row.session } else { &row.hour };
        let key = (
            Arc::clone(&row.kind),
            Arc::clone(&row.file),
            Arc::clone(&row.model),
            row.provider.clone().unwrap_or_else(|| Arc::clone(&none)),
            slot.clone().unwrap_or_else(|| Arc::clone(&none)),
            row.cost > 0.0,
        );
        let position = *positions.entry(key).or_insert(out.len());
        if let Some(previous) = out.get_mut(position) {
            for (left, right) in previous.tokens.iter_mut().zip(row.tokens) {
                *left = left.saturating_add(right);
            }
            previous.events = previous.events.saturating_add(row.events);
            previous.cost += row.cost;
            if session {
                if let (Some(left), Some(right)) = (previous.first, row.first) {
                    previous.first = Some(left.min(right));
                }
                if let (Some(left), Some(right)) = (previous.last, row.last) {
                    previous.last = Some(left.max(right));
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
    fn analytics_deep_cursor_move_keeps_one_readable_contribution() {
        use aac_store::{collect, report};

        let home = tempfile::tempdir().expect("fixture home");
        let projects = home.path().join(".cursor/projects");
        let db = projects.join("original/sdk-agent-store/a/index.db");
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store directory");
        let now = aac_store::engine::now_ms() as i64;
        let at = aac_num::ecma::to_iso_string(now - 3_600_000).expect("run time");
        let conn = rusqlite::Connection::open(&db).expect("Cursor database");
        conn.execute_batch(
            "CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT,
            started_at TEXT, finished_at TEXT, usage_json TEXT);",
        )
        .expect("Cursor schema");
        for (id, usage) in [
            (
                "readable",
                r#"{"inputTokens":100,"outputTokens":10,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":110}"#,
            ),
            ("bad", r#"{"outputTokens":7}"#),
        ] {
            conn.execute(
                "INSERT INTO runs VALUES (?, 'fixture-agent', 'grok-4.7', ?, ?, ?)",
                rusqlite::params![id, at, at, usage],
            )
            .expect("fixture run");
        }
        conn.close().expect("close fixture database");

        let mut cache = Cache::default();
        let store = aac_store::ops::open_engine(
            home.path()
                .join("store")
                .to_str()
                .expect("fixture store path"),
            None,
            None,
        )
        .expect("activity store");
        let pricing = super::super::ResolverPricing::new(home.path());
        let env = aac_usage::discover::Env::new();
        for moved in [false, true] {
            if moved {
                std::fs::rename(projects.join("original"), projects.join("moved"))
                    .expect("move project");
            }
            let wire = json!({"kinds":["cursor"],"minDateMs":0,"fingerprints":cache.fingerprints});
            let request = aac_usage::protocol::parse_request(wire.to_string().as_bytes())
                .expect("scan request");
            let answer = aac_usage::protocol::scan(&request, home.path(), &env);
            assert!(!cache.apply("cursor", &answer, false, 0));
            assert_eq!(cache.states["cursor"]["unreadableRecords"], 1);
            let worker = collect::worker_from_rows(
                &of_kind(&cache.rows, "cursor"),
                &of_kind(&cache.srows, "cursor"),
                "ubuntu",
                now,
                &pricing,
            );
            super::super::history::commit_report(
                &store,
                report::rows_report(&report::RowsReportArgs {
                    computer: "ubuntu",
                    tool: "cursor",
                    data: &[worker],
                    kind: "backfill",
                    read_at: now,
                    complete: false,
                    unreadable_records: Some(1),
                    cause: None,
                    now,
                }),
            )
            .expect("commit scan report");
            let range = aac_store::ops::call(
                &store,
                "activityRange",
                &json!({"fromMs": now - 7_200_000, "toMs": now}).to_string(),
            )
            .expect("stored usage");
            let input: u64 = range["rows"]
                .as_array()
                .expect("hourly rows")
                .iter()
                .filter_map(|row| row[5].as_u64())
                .sum();
            assert_eq!(input, 100, "one run after moved={moved}: {range}");
        }
        let key = cache.rows[0].file.to_string();
        let inaccessible = json!({
            "truncated": false,
            "kinds":{"cursor":{"state":"ok","partial":true,"unreadableRecords":1,
                "unreadableFolders":1,"fingerprints":{}}},
        });
        assert!(!cache.apply("cursor", &inaccessible, false, 0));
        assert!(cache.rows.iter().any(|row| *row.file == *key));
    }

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
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert_eq!(cache.rows[0].events, 2);
        let prints = cache.fingerprints["codex"].clone();
        assert!(cache.apply("codex", &answer(prints, vec![]), true, 0));
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert!(!cache.apply("codex", &answer(json!({}), vec![]), false, 0));
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert!(cache.apply(
            "codex",
            &answer(json!({"file":replaced}), vec![row(20)]),
            true,
            0
        ));
        assert_eq!(cache.rows[0].tokens[0], 20);
    }

    #[test]
    fn an_unchanged_checkpoint_is_not_written_again() {
        let home = tempfile::tempdir().expect("fixture home");
        let store =
            aac_store::ops::open_engine(&home.path().join("store").to_string_lossy(), None, None)
                .expect("open fixture store");
        let print = json!({"size":100,"mtimeMs":1,"fileIdentity":"9:1","appendState":{}});
        let answer = json!({"kinds":{"codex":{"state":"ok","fingerprints":{"file":print}}},
            "rows":[{"k":"codex","f":"file","m":"model","h":"2026-11-03 03","i":5,"n":1,"c":0}]});
        let mut cache = Cache::default();
        assert!(cache.apply("codex", &answer, true, 0));
        let bytes = cache.encode().expect("encode");
        assert!(
            cache
                .save_encoded(&store, "fixture", &bytes)
                .expect("first save")
        );
        assert!(
            !cache
                .save_encoded(&store, "fixture", &bytes)
                .expect("same bytes")
        );
        // A fresh load knows the stored bytes: an unchanged pass skips its write.
        let mut loaded = Cache::load(&store, "fixture").expect("load");
        let again = loaded.encode().expect("re-encode");
        assert_eq!(again, bytes);
        assert!(
            !loaded
                .save_encoded(&store, "fixture", &again)
                .expect("unchanged")
        );
        let more = json!({"kinds":{"codex":{"state":"ok","fingerprints":{"file":
            {"size":150,"mtimeMs":2,"fileIdentity":"9:1","appendState":{},"readFrom":100}}}},
            "rows":[{"k":"codex","f":"file","m":"model","h":"2026-11-03 03","i":5,"n":1,"c":0}]});
        let mut stored = Cache::load(&store, "fixture").expect("reload");
        assert!(stored.apply("codex", &more, true, 0));
        let changed = stored.encode().expect("encode change");
        assert!(
            stored
                .save_encoded(&store, "fixture", &changed)
                .expect("changed")
        );
        assert_eq!(
            Cache::load(&store, "fixture").expect("load change").rows[0].tokens[0],
            10
        );
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
        .expect("fixture fingerprints");
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
        let request = aac_usage::protocol::parse_request(args.to_string().as_bytes())
            .expect("decode the split scanner frame");
        assert!(request.fingerprints.is_empty());
        assert_eq!(
            request.prior_print_json("codex", "settled").as_ref(),
            Some(&print)
        );

        let mut cache = Cache {
            fingerprints,
            rows: vec![
                serde_json::from_value(
                    json!({"k":"codex","f":"settled","m":"fixture","h":"2026-11-03 03:00","i":100}),
                )
                .expect("fixture row"),
            ],
            ..Cache::default()
        };
        let confirmed = json!({"kinds":{"codex":{"state":"ok","settledPacked":true,
            "fingerprints":{"settled":{"settledConfirmed":true,"settledCheckedSlot":1}}
        }}});
        assert!(cache.apply("codex", &confirmed, true, 0));
        assert_eq!(cache.rows[0].tokens[0], 100);
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
            let readable = super::super::has_readable_records(&answer, &sliced, "codex");
            let complete = super::super::scan_state(&answer, "codex", readable).0 == "ok";
            assert_eq!(sliced.apply("codex", &answer, complete, 0), complete);
            sliced.save(&store, "fixture").expect("save sliced cache");
            if complete {
                assert!(pass > 10);
                assert_eq!(sliced.rows, full.rows);
                assert_eq!(sliced.srows, full.srows);
                assert_eq!(sliced.srows[0].events, n);
                return;
            }
        }
        panic!("the writer never reached the tail of the rollout");
    }

    #[test]
    fn analytics_settled_scan_reports_current_complete_coverage() {
        use crate::analytics_writers::{ResolverPricing, report, scan_source_state, source_state};
        use std::time::{Duration, UNIX_EPOCH};
        const DAY: i64 = 86_400_000;
        let now: i64 = 1_795_212_000_000;
        let used: i64 = 1_794_657_600_000; // 2026-11-14T12:00:00Z
        let home = tempfile::tempdir().expect("fixture home");
        let path = home.path().join(
            ".codex/sessions/2026/11/14/rollout-2026-11-14T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl",
        );
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture folder");
        let mut file = std::fs::File::create(&path).expect("fixture rollout");
        for line in [
            json!({"type":"session_meta","payload":{"id":"quiet","model_provider":"openai"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
            json!({"timestamp":"2026-11-14T12:00:00Z","type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":1}}}}),
        ] {
            writeln!(file, "{line}").expect("fixture record");
        }
        file.set_modified(UNIX_EPOCH + Duration::from_millis((now - 5 * DAY) as u64))
            .expect("fixture time");
        drop(file);
        let kinds = ["codex".to_owned()];
        let settled_before: HashMap<String, u64> = [("codex".to_owned(), (now - 3 * DAY) as u64)]
            .into_iter()
            .collect();
        let cutoff = now - 31 * DAY;
        let pass = |cache: &mut Cache| {
            let args = super::super::scan_request(&kinds, cutoff, json!({}), false, None);
            let [(batch, request)] =
                requests(args, &kinds, &cache.fingerprints, &settled_before, now)
                    .try_into()
                    .expect("one scanner request");
            let request = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("parse writer request");
            let answer = aac_usage::protocol::scan(
                &request,
                home.path(),
                &aac_usage::discover::Env::default(),
            );
            assert!(super::super::valid_scan_answer(&answer, &batch));
            let readable = super::super::has_readable_records(&answer, cache, "codex");
            let (state, cause) = source_state(&answer, "codex", readable);
            assert!(cache.apply("codex", &answer, state == "ok", cutoff));
            let source = scan_source_state(&answer, "codex", "mac", now, state, cause);
            let worker = aac_store::collect::worker_from_rows(
                &of_kind(&cache.rows, "codex"),
                &of_kind(&cache.srows, "codex"),
                "mac",
                now,
                &ResolverPricing::new(home.path()),
            );
            let reports = report::build_activity_reports(&report::ReportArgs {
                local: &HashMap::new(),
                remote: &[json!({"tool":"codex","host":"mac","data":worker})],
                states: &[source],
                window_from_ms: cutoff,
                now,
                local_complete: &HashSet::new(),
                scanner_owned: &HashSet::new(),
            });
            (answer, reports.into_iter().next().expect("codex report"))
        };
        let mut cache = Cache::default();
        let (full, full_report) = pass(&mut cache);
        assert_eq!(full_report["state"], "ok");
        // An older print disclosed an estimated clock: settling must carry the disclosure.
        for print in cache.fingerprints["codex"]
            .as_object_mut()
            .expect("codex prints")
            .values_mut()
        {
            print["estimatedClock"] = json!(true);
        }
        let (settled, report) = pass(&mut cache);
        let print = settled["kinds"]["codex"]["fingerprints"]
            .as_object()
            .expect("settled prints")
            .values()
            .next()
            .expect("settled print");
        assert!(print["settledCheckedSlot"].is_u64(), "the rollout settled");
        let discovered = &settled["discovery"]["kinds"]["codex"];
        let before = &full["discovery"]["kinds"]["codex"];
        assert_eq!(discovered["freshness"]["state"], "current");
        assert_eq!(discovered["freshness"]["newestDataMs"], used);
        assert_eq!(
            discovered["counts"]["filesFound"],
            before["counts"]["filesFound"]
        );
        assert_eq!(
            discovered["counts"]["filesRead"],
            before["counts"]["filesRead"]
        );
        assert_eq!(discovered["counts"]["filesRead"], 1);
        assert_eq!(report["state"], "ok");
        assert_eq!(report["complete"], true);
        assert_eq!(report["cause"], Value::Null);
        assert!(report["coverFrom"].is_string());
        assert_eq!(report["counts"]["freshness"]["state"], "current");
        assert_eq!(report["counts"]["freshness"]["newestDataMs"], used);
        assert_eq!(report["counts"]["estimatedClock"], true);
        assert_eq!(report["cells"], full_report["cells"]);
    }
}
