//! The Scoreboard catalog (`catalog.ts`): `<reports>/v1/catalog/*.json`, owned by AAC and
//! edited on the share as data. Each file is projected from its allowlist; a file that later
//! fails keeps its last good copy; a catalog with no good copy shows as a Source problem and
//! the kinds it holds stay empty. The scan is the shared `ShareFiles` rule set.

use super::{Problem, ShareFs, TransientMisses, compare, is_missing, is_transient, stat_key, MISSING_GRACE_MS, PERSISTENT_SHARE_ERROR};
use crate::validate::{
    self, Invalid, Valid, bool_value, count, fail, list, obj, one_of, opt_iso, opt_text, text,
};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use std::collections::HashSet;
use std::path::PathBuf;

pub const CATALOG_CAP_BYTES: usize = 256 * 1024;
pub const DEFAULT_MIN_RUNS: u64 = 5;

/// The catalog files, in the order the reader visits and names them.
pub const CATALOG_FILES: [&str; 6] = [
    "models.json",
    "model-aliases.json",
    "task-types.json",
    "measures.json",
    "uses.json",
    "benchmarks.json",
];

const TASK_UNITS: [&str; 5] = ["record", "file", "item", "word", "task"];

/// `^[A-Za-z0-9][A-Za-z0-9._/-]{0,119}$`
fn id(value: Option<&Json>, field: &str) -> Valid<String> {
    let Some(Json::String(value)) = value else {
        return Err(fail(field));
    };
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return Err(fail(field)),
    }
    if value.chars().count() > 120
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        return Err(fail(field));
    }
    Ok(value.clone())
}

/// `^[A-Za-z][A-Za-z0-9_]{0,63}$`
fn metric_name(value: Option<&Json>, field: &str) -> Valid<String> {
    let Some(Json::String(value)) = value else {
        return Err(fail(field));
    };
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return Err(fail(field)),
    }
    if value.chars().count() > 64 || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(fail(field));
    }
    Ok(value.clone())
}

fn ids(value: Option<&Json>, field: &str) -> Valid<Vec<String>> {
    list(value, field, 100, |item, at| id(Some(item), at))
}

fn flag(value: Option<&Json>, field: &str) -> Valid<bool> {
    bool_value(value, field, false)
}

fn price(value: Option<&Json>, field: &str) -> Valid<Option<f64>> {
    match value {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(number)) => match number.as_f64() {
            Some(number) if number.is_finite() && number >= 0.0 => Ok(Some(number)),
            _ => Err(fail(field)),
        },
        _ => Err(fail(field)),
    }
}

fn no_repeats(rows: &[Json], key: &str, what: &str) -> Valid<()> {
    let mut seen = HashSet::new();
    for row in rows {
        if !seen.insert(row[key].as_str().unwrap_or_default().to_string()) {
            return Err(Invalid(format!("{what} repeat an id")));
        }
    }
    Ok(())
}

fn models(value: Option<&Json>) -> Valid<Vec<Json>> {
    let rows = list(value, "models", 500, |item, at| {
        let row = obj(Some(item), at)?;
        let prices = match row.get("prices") {
            None | Some(Json::Null) => None,
            other => Some(obj(other, at)?.clone()),
        };
        Ok(json!({
            "model": id(row.get("model"), &format!("{at}.model"))?,
            "label": text(row.get("label"), &format!("{at}.label"), 120)?,
            "family": opt_text(row.get("family"), &format!("{at}.family"), 120)?,
            "runtimes": ids(row.get("runtimes"), &format!("{at}.runtimes"))?,
            "efforts": ids(row.get("efforts"), &format!("{at}.efforts"))?,
            "prices": prices.map(|prices| -> Valid<Json> {
                Ok(json!({
                    "input": price(prices.get("input"), &format!("{at}.prices.input"))?,
                    "output": price(prices.get("output"), &format!("{at}.prices.output"))?,
                    "cachedInput": price(prices.get("cachedInput"), &format!("{at}.prices.cachedInput"))?,
                    "unit": opt_text(prices.get("unit"), &format!("{at}.prices.unit"), 60)?,
                    "asOf": opt_iso(prices.get("asOf"), &format!("{at}.prices.asOf"))?,
                }))
            }).transpose()?,
            "notes": opt_text(row.get("notes"), &format!("{at}.notes"), 500)?,
        }))
    })?;
    let mut seen = HashSet::new();
    for row in &rows {
        if !seen.insert(row["model"].as_str().unwrap_or_default().to_string()) {
            return Err(Invalid("models repeat a model".to_string()));
        }
    }
    Ok(rows)
}

fn task_types(value: Option<&Json>) -> Valid<Vec<Json>> {
    let rows = list(value, "taskTypes", 200, |item, at| {
        let row = obj(Some(item), at)?;
        Ok(json!({
            "id": id(row.get("id"), &format!("{at}.id"))?,
            "label": text(row.get("label"), &format!("{at}.label"), 120)?,
            "description": opt_text(row.get("description"), &format!("{at}.description"), 1000)?,
            "unit": one_of(row.get("unit"), &format!("{at}.unit"), &TASK_UNITS)?,
            "measures": ids(row.get("measures"), &format!("{at}.measures"))?,
            "byProcess": flag(row.get("byProcess"), &format!("{at}.byProcess"))?,
        }))
    })?;
    no_repeats(&rows, "id", "taskTypes")?;
    Ok(rows)
}

fn measures(value: Option<&Json>) -> Valid<Vec<Json>> {
    let rows = list(value, "measures", 200, |item, at| {
        let row = obj(Some(item), at)?;
        let metric = match row.get("metric") {
            None | Some(Json::Null) => None,
            other => Some(obj(other, at)?.clone()),
        };
        Ok(json!({
            "id": id(row.get("id"), &format!("{at}.id"))?,
            "label": text(row.get("label"), &format!("{at}.label"), 120)?,
            "better": one_of(row.get("better"), &format!("{at}.better"), &["lower", "higher"])?,
            "unit": text(row.get("unit"), &format!("{at}.unit"), 40)?,
            "description": opt_text(row.get("description"), &format!("{at}.description"), 1000)?,
            "metric": metric.map(|metric| -> Valid<Json> {
                Ok(json!({
                    "of": metric_name(metric.get("of"), &format!("{at}.metric.of"))?,
                    "per": metric_name(metric.get("per"), &format!("{at}.metric.per"))?,
                }))
            }).transpose()?,
            "preliminaryUnless": match row.get("preliminaryUnless") {
                None | Some(Json::Null) => Json::Null,
                other => json!(metric_name(other, &format!("{at}.preliminaryUnless"))?),
            },
        }))
    })?;
    no_repeats(&rows, "id", "measures")?;
    Ok(rows)
}

fn uses(value: Option<&Json>) -> Valid<Vec<Json>> {
    let rows = list(value, "uses", 100, |item, at| {
        let row = obj(Some(item), at)?;
        let raw = obj(row.get("weights"), &format!("{at}.weights"))?;
        let mut weights = Map::new();
        for (name, weight) in raw {
            let key = id(Some(&Json::from(name.clone())), &format!("{at}.weights"))?;
            match weight.as_f64() {
                Some(number) if number.is_finite() && number >= 0.0 => {
                    weights.insert(key, weight.clone());
                }
                _ => return Err(fail(&format!("{at}.weights"))),
            }
        }
        let types = list(row.get("taskTypes"), &format!("{at}.taskTypes"), 200, |type_, field| {
            if type_ == &json!("*") {
                return Ok("*".to_string());
            }
            id(Some(type_), field)
        })?;
        Ok(json!({
            "id": id(row.get("id"), &format!("{at}.id"))?,
            "label": text(row.get("label"), &format!("{at}.label"), 120)?,
            "description": opt_text(row.get("description"), &format!("{at}.description"), 1000)?,
            "taskTypes": types,
            "weights": Json::Object(weights),
        }))
    })?;
    no_repeats(&rows, "id", "uses")?;
    Ok(rows)
}

fn benchmarks(value: Option<&Json>) -> Valid<Vec<Json>> {
    let rows = list(value, "benchmarks", 200, |item, at| {
        let row = obj(Some(item), at)?;
        Ok(json!({
            "id": id(row.get("id"), &format!("{at}.id"))?,
            "label": text(row.get("label"), &format!("{at}.label"), 120)?,
            "taskType": match row.get("taskType") {
                None | Some(Json::Null) => Json::Null,
                other => json!(id(other, at)?),
            },
            "description": opt_text(row.get("description"), &format!("{at}.description"), 1000)?,
            "items": match row.get("items") {
                None | Some(Json::Null) => Json::Null,
                other => json!(count(other, at)?),
            },
            "scoring": opt_text(row.get("scoring"), &format!("{at}.scoring"), 1000)?,
        }))
    })?;
    no_repeats(&rows, "id", "benchmarks")?;
    Ok(rows)
}

/// `^-[a-z][a-z0-9-]{0,15}$`: one leading dash, then a lowercase word.
fn suffix_ok(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('-') else {
        return false;
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    rest.chars().count() <= 16 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn model_aliases(doc: &Map<String, Json>) -> Valid<(Map<String, Json>, Vec<String>)> {
    let raw = obj(doc.get("aliases"), "aliases")?;
    if raw.len() > 500 {
        return Err(fail("aliases"));
    }
    let mut aliases = Map::new();
    for (written, canonical) in raw {
        let check = |value: &Json| -> Valid<()> {
            match value {
                Json::String(word) if !word.is_empty() && word.encode_utf16().count() <= 120 => {
                    text(Some(value), "aliases", 120)?;
                    Ok(())
                }
                _ => Err(fail("aliases")),
            }
        };
        check(&Json::from(written.clone()))?;
        check(canonical)?;
        aliases.insert(written.clone(), canonical.clone());
    }
    let suffixes = list(doc.get("effortSuffixes"), "effortSuffixes", 32, |item, at| {
        let suffix = text(Some(item), at, 16)?;
        if !suffix_ok(&suffix) {
            return Err(fail(at));
        }
        Ok(suffix)
    })?;
    let mut deduped: Vec<String> = Vec::new();
    for suffix in suffixes {
        if !deduped.contains(&suffix) {
            deduped.push(suffix);
        }
    }
    Ok((aliases, deduped))
}

/// The schema and member each catalog file holds.
fn file_schema(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "models.json" => Some(("catalog-models@1", "models")),
        "model-aliases.json" => Some(("agent-reports/model-aliases@1", "aliases")),
        "task-types.json" => Some(("catalog-task-types@1", "taskTypes")),
        "measures.json" => Some(("catalog-measures@1", "measures")),
        "uses.json" => Some(("catalog-uses@1", "uses")),
        "benchmarks.json" => Some(("catalog-benchmarks@1", "benchmarks")),
        _ => None,
    }
}

/// Bytes of one catalog file -> its projected content (tagged with its kind), or a reason.
pub fn parse_catalog_file(name: &str, bytes: &[u8]) -> Result<Json, String> {
    let raw = validate::json_from_bytes(bytes, CATALOG_CAP_BYTES)?;
    let project = || -> Valid<Json> {
        let doc = obj(Some(&raw), "document")?;
        let (schema, key) = file_schema(name).ok_or_else(|| fail("schema"))?;
        if doc.get("schema").and_then(Json::as_str) != Some(schema) {
            return Err(fail("schema"));
        }
        if key == "aliases" {
            let (aliases, effort_suffixes) = model_aliases(doc)?;
            return Ok(json!({
                "kind": "aliases",
                "aliases": Json::Object(aliases),
                "effortSuffixes": effort_suffixes,
            }));
        }
        let rows = doc.get(key).filter(|rows| rows.is_array()).ok_or_else(|| fail(key))?;
        match key {
            "models" => Ok(json!({"kind": "models", "models": models(Some(rows))?})),
            "taskTypes" => Ok(json!({"kind": "taskTypes", "taskTypes": task_types(Some(rows))?})),
            "measures" => {
                let min_runs = match doc.get("minRuns") {
                    None => DEFAULT_MIN_RUNS,
                    other => count(other, "minRuns")?,
                };
                if min_runs < 1 {
                    return Err(fail("minRuns"));
                }
                Ok(json!({"kind": "measures", "minRuns": min_runs, "measures": measures(Some(rows))?}))
            }
            "uses" => Ok(json!({"kind": "uses", "uses": uses(Some(rows))?})),
            _ => Ok(json!({"kind": "benchmarks", "benchmarks": benchmarks(Some(rows))?})),
        }
    };
    project().map_err(|Invalid(reason)| reason)
}

// ------------------------------------------------------------------------------------
// The shared file set (share-files.ts), as the catalog reader uses it (no seq ordering).

#[derive(Clone)]
struct ShareEntry {
    stat_key: String,
    good: Option<(Json, u64, String)>, // doc, firstSeenAt, sha256
    problem: Option<Problem>,
    missing_since: Option<u64>,
}

struct ShareFiles {
    entries: IndexMap<String, ShareEntry>,
    misses: TransientMisses,
}

impl ShareFiles {
    fn new() -> Self {
        Self {
            entries: IndexMap::new(),
            misses: TransientMisses::default(),
        }
    }

    fn entry(&mut self, file: &str) -> &mut ShareEntry {
        self.entries.entry(file.to_string()).or_insert(ShareEntry {
            stat_key: String::new(),
            good: None,
            problem: None,
            missing_since: None,
        })
    }

    fn set_problem(&mut self, file: &str, reason: &str, stat_key: &str, since: u64) {
        let entry = self.entry(file);
        if entry.problem.as_ref().map(|p| p.reason.as_str()) != Some(reason) {
            entry.problem = Some(Problem {
                file: file.to_string(),
                reason: reason.to_string(),
                since,
            });
        }
        entry.stat_key = stat_key.to_string();
    }

    fn transient(&mut self, file: &str, now: u64) {
        if let Some(since) = self.misses.miss(file, now) {
            self.set_problem(file, PERSISTENT_SHARE_ERROR, "", since);
        }
    }

    /// Stat and, when changed, read one listed file. Resolves the file's size.
    fn visit(
        &mut self,
        fs: &dyn ShareFs,
        absolute: &std::path::Path,
        file: &str,
        seen: &mut HashSet<String>,
        now: u64,
        parse: impl Fn(&[u8]) -> Result<Json, String>,
    ) -> u64 {
        let stat = match fs.stat(absolute) {
            Ok(stat) => {
                seen.insert(file.to_string());
                stat
            }
            Err(error) => {
                seen.insert(file.to_string());
                if is_transient(&error) {
                    self.transient(file, now);
                } else {
                    self.misses.clear(file);
                    self.set_problem(file, "could not be read", "", now);
                }
                return 0;
            }
        };
        if !stat.file {
            self.set_problem(file, "is not a regular file", "", now);
            return 0;
        }
        let key = stat_key(&stat);
        let size = stat.size;
        // Before the unchanged check: a file over its cap is never read, on any scan.
        if size > CATALOG_CAP_BYTES as u64 {
            self.set_problem(
                file,
                &format!("larger than the {} KB cap", CATALOG_CAP_BYTES / 1024),
                &key,
                now,
            );
            return 0;
        }
        if self.entries.get(file).map(|entry| entry.stat_key.as_str()) == Some(key.as_str()) {
            self.misses.clear(file);
            return size;
        }
        let bytes = match fs.read(absolute, CATALOG_CAP_BYTES) {
            Ok(bytes) => bytes,
            Err(super::ReadError::OverCap) => {
                self.set_problem(
                    file,
                    &format!("larger than the {} KB cap", CATALOG_CAP_BYTES / 1024),
                    &key,
                    now,
                );
                return size;
            }
            Err(super::ReadError::Io(error)) => {
                if is_transient(&error) {
                    self.transient(file, now);
                } else {
                    self.misses.clear(file);
                    self.set_problem(file, "could not be read", "", now);
                }
                return size;
            }
        };
        self.misses.clear(file);
        let doc = match parse(&bytes) {
            Ok(doc) => doc,
            Err(reason) => {
                self.set_problem(file, &reason, &key, now);
                return size;
            }
        };
        let entry = self.entry(file);
        entry.stat_key = key;
        entry.problem = None;
        entry.good = Some((doc, now, super::sha256_hex(&bytes)));
        size
    }

    /// Marks every file under a folder that could not be listed: not listed is not withdrawn.
    fn keep(&mut self, prefix: &str, seen: &mut HashSet<String>) {
        for file in self.entries.keys() {
            if file.starts_with(prefix) {
                seen.insert(file.clone());
            }
        }
    }

    /// After a scan: forgets files absent from their listing for longer than the grace.
    fn sweep(&mut self, seen: &HashSet<String>, now: u64) {
        self.misses.keep_only(seen);
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for file in keys {
            let entry = self.entries.get_mut(&file).expect("listed entry");
            if seen.contains(&file) {
                entry.missing_since = None;
                continue;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            if now - missing_since >= MISSING_GRACE_MS {
                self.entries.shift_remove(&file);
            }
        }
    }

    fn get(&self, file: &str) -> Option<&Json> {
        self.entries.get(file)?.good.as_ref().map(|(doc, _, _)| doc)
    }

    fn problems(&self) -> Vec<Problem> {
        self.entries
            .values()
            .filter_map(|entry| entry.problem.clone())
            .collect()
    }
}

// ------------------------------------------------------------------------------------
// The catalog reader

pub struct CatalogState {
    pub state: &'static str,
    pub scanned_at: Option<u64>,
    pub missing_files: Vec<String>,
    pub problems: Vec<Problem>,
    pub min_runs: u64,
    pub models: Vec<Json>,
    pub aliases: Map<String, Json>,
    pub effort_suffixes: Vec<String>,
    pub task_types: Vec<Json>,
    pub measures: Vec<Json>,
    pub uses: Vec<Json>,
    pub benchmarks: Vec<Json>,
}

pub struct CatalogReader {
    root: PathBuf,
    files: ShareFiles,
    state: &'static str,
    scanned_at: Option<u64>,
}

impl CatalogReader {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            files: ShareFiles::new(),
            state: "missing",
            scanned_at: None,
        }
    }

    /// One rescan. Never fails: a listing failure keeps what is known.
    pub fn scan(&mut self, fs: &dyn ShareFs, now: u64) {
        let dir = self.root.join("v1").join("catalog");
        let mut seen = HashSet::new();
        let listed: HashSet<String> = match fs.list(&dir) {
            Ok(entries) => {
                self.state = "ok";
                entries.into_iter().map(|entry| entry.name).collect()
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound && is_transient(&error) {
                    return;
                }
                self.state = if is_missing(&error) { "missing" } else { "error" };
                // A folder that cannot be listed withdraws nothing; a removed one does,
                // after the grace.
                if self.state == "error" {
                    self.files.keep("catalog/", &mut seen);
                }
                self.files.sweep(&seen, now);
                self.scanned_at = Some(now);
                return;
            }
        };
        for name in CATALOG_FILES {
            if listed.contains(name) {
                self.files.visit(
                    fs,
                    &dir.join(name),
                    &format!("catalog/{name}"),
                    &mut seen,
                    now,
                    |bytes| parse_catalog_file(name, bytes),
                );
            }
        }
        self.files.sweep(&seen, now);
        self.scanned_at = Some(now);
    }

    pub fn snapshot(&self) -> CatalogState {
        let read = |name: &str, kind: &str| -> Option<&Json> {
            let doc = self.files.get(&format!("catalog/{name}"))?;
            (doc["kind"] == json!(kind)).then_some(doc)
        };
        let mut problems = self.files.problems();
        problems.sort_by(|a, b| compare(&a.file, &b.file));
        let named: HashSet<&str> = problems.iter().map(|problem| problem.file.as_str()).collect();
        let measures_file = read("measures.json", "measures");
        let aliases_file = read("model-aliases.json", "aliases");
        let array = |doc: Option<&Json>, key: &str| -> Vec<Json> {
            doc.and_then(|doc| doc[key].as_array())
                .cloned()
                .unwrap_or_default()
        };
        CatalogState {
            state: self.state,
            scanned_at: self.scanned_at,
            missing_files: CATALOG_FILES
                .iter()
                .filter(|name| {
                    self.files.get(&format!("catalog/{name}")).is_none()
                        && !named.contains(format!("catalog/{name}").as_str())
                })
                .map(|name| (*name).to_string())
                .collect(),
            problems,
            min_runs: measures_file
                .and_then(|doc| doc["minRuns"].as_u64())
                .unwrap_or(DEFAULT_MIN_RUNS),
            models: array(read("models.json", "models"), "models"),
            aliases: aliases_file
                .and_then(|doc| doc["aliases"].as_object())
                .cloned()
                .unwrap_or_default(),
            effort_suffixes: aliases_file
                .and_then(|doc| doc["effortSuffixes"].as_array())
                .map(|suffixes| {
                    suffixes
                        .iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            task_types: array(read("task-types.json", "taskTypes"), "taskTypes"),
            measures: array(measures_file, "measures"),
            uses: array(read("uses.json", "uses"), "uses"),
            benchmarks: array(read("benchmarks.json", "benchmarks"), "benchmarks"),
        }
    }
}

/// The snapshot as the Scoreboard envelope's `catalog` member.
pub fn state_json(state: &CatalogState) -> Json {
    json!({
        "state": state.state,
        "scannedAt": state.scanned_at,
        "missingFiles": state.missing_files,
        "problems": state.problems.iter().map(|problem| {
            json!({"file": problem.file, "reason": problem.reason, "since": problem.since})
        }).collect::<Vec<_>>(),
        "minRuns": state.min_runs,
        "models": state.models,
        "aliases": Json::Object(state.aliases.clone()),
        "effortSuffixes": state.effort_suffixes,
        "taskTypes": state.task_types,
        "measures": state.measures,
        "uses": state.uses,
        "benchmarks": state.benchmarks,
    })
}
