//! The Scoreboard's `byModel` field (CONTRACT-spend-bymodel.md section 2): the
//! model-by-job matrix, the picks, the measures and the compare sentences, every word
//! decided here. A line-for-line port of the Leptos page builder's reference
//! (`reference/by-model-reference.mjs`, golden pair `scoreboard-view.json` ->
//! `scoreboard-by-model.json`); where this comment and the golden output disagree, the
//! golden output wins.
//!
//! A pure function over the Scoreboard view v1 the TypeScript server already builds
//! (`buildScoreboardView` in src/web-server/scoreboard/scoreboard-view.ts): nothing is
//! recomputed from raw runs. A view this port cannot read as expected is refused (the
//! route then serves no `byModel` field), never a guessed figure or verdict. The one
//! liberty taken against the reference: a measure the reference would crash on (a scout
//! row without its facts-kept measure, a null where a median is divided) is refused the
//! same way instead of panicking.
//!
//! Amendments of 2026-11-09 (fix round 1, after blind review), kept in step with the
//! reference: a measure whose value — or, where the words quote bounds, whose low or
//! high — is null is omitted, never printed as a measured 0%; an Avoid row without a
//! cheating measure falls back to the "N runs" cell; a value outside the ±2^53 safe
//! integer range refuses the view instead of printing a saturated number; ties order by
//! plain Unicode code points.
//!
//! Fix round 2: "at the same speed and cost" is said only when both models' speed and
//! cost are known finite numbers and each pair matches. Two unknown values are not a tie.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The catalog's minimum runs for a verdict; the view passes it, the words quote it.
type MinRuns = i64;

// ---------------------------------------------------------------------------------------------
// Input (the Scoreboard view v1 the TypeScript side builds; only what the port reads)

#[derive(Debug, Clone, Deserialize)]
pub struct ViewIn {
    pub catalog: CatalogIn,
    pub uses: Vec<UseIn>,
    pub sources: Vec<SourceIn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogIn {
    pub min_runs: MinRuns,
    pub models: Vec<CatalogModelIn>,
    pub task_types: Vec<CatalogTaskIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogModelIn {
    pub model: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTaskIn {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UseIn {
    pub id: String,
    pub task_types: Vec<TaskIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskIn {
    pub id: String,
    pub label: String,
    pub unit: String,
    pub rows: Vec<RowIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RowIn {
    pub model: String,
    pub label: String,
    pub runtime: Option<String>,
    pub effort: Option<String>,
    pub process: Option<ProcessIn>,
    pub n: CountsIn,
    /// observed | benchmark | null: which column the headline stands on.
    #[serde(default)]
    pub basis: Option<String>,
    #[serde(default)]
    pub ranked: bool,
    #[serde(default)]
    pub rank: Option<i64>,
    pub verdict: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub observed: Option<ColumnIn>,
    #[serde(default)]
    pub benchmarks: Vec<ColumnIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcessIn {
    pub id: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CountsIn {
    pub observed: i64,
    pub benchmark: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnIn {
    pub n: i64,
    #[serde(default)]
    pub reviews: i64,
    #[serde(default)]
    pub ranked: bool,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub measures: HashMap<String, MeasureIn>,
}

/// One measure of a column. `value`, `low` and `high` keep the JSON number exactly as the
/// view wrote it, so a range passthrough prints an integer the same way the view did.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MeasureIn {
    #[serde(default)]
    pub value: Json,
    #[serde(default)]
    pub n: i64,
    #[serde(default)]
    pub low: Json,
    #[serde(default)]
    pub high: Json,
    #[serde(default)]
    pub of: Option<i64>,
    #[serde(default)]
    pub per: Option<i64>,
    /// quality: the items that passed on their first review round, when the view says.
    #[serde(default)]
    pub first_try: Option<i64>,
    #[serde(default)]
    pub per_task: Option<StatIn>,
    #[serde(default)]
    pub estimate: Option<EstimateIn>,
}

impl MeasureIn {
    /// The measure's value as a number, mirroring JavaScript's coercion of `null` to 0
    /// where the reference compares one (`null >= 0.2` is `0 >= 0.2`).
    fn number(&self, value: &Json) -> Option<f64> {
        value.as_f64()
    }

    fn has_value(&self) -> bool {
        self.value.as_f64().is_some()
    }

    /// The measure is known enough to print: its value and, where the words quote the
    /// likely range, its low and high bounds are numbers. A null among them omits the
    /// measure — it is never formatted as a measured zero (2026-11-09 amendment, A2).
    fn span_known(&self) -> bool {
        self.value.as_f64().is_some() && self.low.as_f64().is_some() && self.high.as_f64().is_some()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct StatIn {
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub n: i64,
    #[serde(default)]
    pub low: Option<f64>,
    #[serde(default)]
    pub high: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct EstimateIn {
    #[serde(default)]
    pub credits: Option<CreditsRateIn>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CreditsRateIn {
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub n: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceIn {
    pub id: String,
    pub detail: String,
}

// ---------------------------------------------------------------------------------------------
// Output (the view the route passes through as the `byModel` field)

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ByModel {
    pub default_view: &'static str,
    pub intro: String,
    pub read_note: Vec<Run>,
    pub read_link: ReadLink,
    pub band_summary: String,
    pub views: Vec<ViewOut>,
}

/// A run of the sentence type `/api/status` already uses. Only the fields the reference
/// sets are ever set (`strong` for a bolded figure, `tone: crit` for a red Avoid).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strong: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<&'static str>,
}

fn run(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        strong: None,
        tone: None,
    }
}

fn strong(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        strong: Some(true),
        tone: None,
    }
}

fn crit(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        strong: Some(true),
        tone: Some("crit"),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadLink {
    pub label: &'static str,
    pub anchor: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewOut {
    pub id: String,
    pub label: String,
    pub short: String,
    pub meta: Vec<Run>,
    pub picks: Vec<PickOut>,
    pub jobs: Vec<JobRef>,
    pub models: Vec<ModelOut>,
    pub foot: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRef {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickOut {
    pub job: String,
    pub job_label: String,
    pub model: Option<PickModel>,
    pub none_label: Option<&'static str>,
    pub why: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PickModel {
    pub label: String,
    pub effort: Option<String>,
    pub mark: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOut {
    pub key: String,
    pub model: String,
    pub label: String,
    pub effort: Option<String>,
    pub effort_text: String,
    pub runtime: String,
    pub mark: Option<&'static str>,
    pub runs: i64,
    pub head: String,
    pub head_runs: Vec<Run>,
    pub sheet_head: String,
    pub cells: BTreeMap<String, Option<CellOut>>,
    pub report: Vec<ReportOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellOut {
    pub verdict: String,
    pub word: String,
    /// Chunks the page keeps whole: it breaks only between them, never mid-phrase.
    pub sub: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportOut {
    pub job: String,
    pub label: String,
    pub verdict: String,
    pub word: String,
    pub basis: String,
    pub measures: Vec<MeasureOut>,
    pub compare: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeasureOut {
    pub label: String,
    pub value: String,
    pub sub: String,
    pub note: String,
    pub range: Option<Json>,
}

// ---------------------------------------------------------------------------------------------
// Words

fn runtime_word(runtime: &str) -> &str {
    match runtime {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "zcode" => "zcode",
        "muse" => "Muse",
        "omp" => "OMP",
        other => other,
    }
}

fn mark_of(runtime: &str) -> Option<&'static str> {
    match runtime {
        "claude-code" => Some("claude"),
        "codex" => Some("codex"),
        "zcode" => Some("zai"),
        "muse" => Some("muse"),
        _ => None,
    }
}

fn verdict_word(verdict: &str) -> &'static str {
    match verdict {
        "recommended" => "Recommended",
        "fine" => "Fine",
        "avoid" => "Avoid",
        _ => "Not enough data",
    }
}

/// The unit word of a job's labels ("Time per task", "Cost per record"): the catalog's own
/// word, exactly as written (the reference's JOBWORD map passes every word through).
fn job_word(unit: &str) -> &str {
    unit
}

/// "Opus 5.5 · xhigh": the label with its effort, or the label alone with no effort.
fn name(label: &str, effort: &Option<String>) -> String {
    match effort {
        Some(effort) => format!("{label} · {effort}"),
        None => label.to_string(),
    }
}

/// JavaScript's Math.round for the non-negative figures this view uses.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// JavaScript's largest safe integer (2^53): the range within which every figure this
/// view prints is an honest whole number. Beyond it the float-to-integer casts saturate,
/// so the input is refused instead (`range`).
const SAFE_INTEGER: f64 = 9_007_199_254_740_992.0;

fn out_of_range(_value: f64) -> Error {
    Error::new(
        "range",
        "a figure outside the safe integer range; refused rather than printed",
    )
}

/// A rounded figure that must survive the cast to an integer; outside the safe integer
/// range the text would be a fabricated number, so the input is refused.
fn checked_round(value: f64) -> Result<i64> {
    if value.is_finite() && value.abs() <= SAFE_INTEGER {
        Ok(js_round(value) as i64)
    } else {
        Err(out_of_range(value))
    }
}

/// JavaScript's `toFixed(places)` for non-negative values: the integer n whose n/10^places
/// is closest to the value (ties to the larger n), with the decimals kept. The comparison
/// divides, as the ECMAScript spec does — multiplying instead turns 4.85 into "4.9" where
/// every JavaScript engine prints "4.8". Scaled units outside the safe integer range
/// refuse the input.
fn to_fixed(value: f64, places: u32) -> Result<String> {
    let scale = 10f64.powi(places as i32);
    let low = (value * scale).floor();
    let below = (low / scale - value).abs();
    let above = ((low + 1.0) / scale - value).abs();
    let units = if above <= below { low + 1.0 } else { low };
    if !units.is_finite() || units.abs() > SAFE_INTEGER {
        return Err(out_of_range(units));
    }
    let units = units as i64;
    let whole = units / 10i64.pow(places);
    let part = units % 10i64.pow(places);
    Ok(format!("{whole}.{part:0places$}", places = places as usize))
}

fn pct(value: f64) -> Result<String> {
    Ok(format!("{}%", checked_round(value * 100.0)?))
}

/// A percent of a JSON number field. A null or non-number is never formatted as a
/// measured zero: the caller shows the measure only when its figures are known, and a
/// stray non-number refuses the compose (2026-11-09 amendment, A2).
fn pct_json(value: &Json) -> Result<String> {
    match value.as_f64() {
        Some(number) => pct(number),
        None => Err(shape("a percent of a measure value that is not a number")),
    }
}

/// A duration the `minutes` helper writes: whole seconds under two minutes, else minutes.
fn minutes(seconds: f64) -> Result<String> {
    if seconds < 120.0 {
        Ok(format!("{} s", checked_round(seconds)?))
    } else {
        Ok(format!("{} min", checked_round(seconds / 60.0)?))
    }
}

fn seconds(value: f64) -> Result<String> {
    if value < 5.0 {
        Ok(format!("{} s", to_fixed(value, 1)?))
    } else if value < 120.0 {
        Ok(format!("{} s", checked_round(value)?))
    } else {
        Ok(format!("{} min", checked_round(value / 60.0)?))
    }
}

/// One decimal from 1 up, two below it.
fn round1(value: f64) -> Result<String> {
    if value >= 1.0 {
        to_fixed(value, 1)
    } else {
        to_fixed(value, 2)
    }
}

/// How a null count prints in the reference's sentences (JavaScript's `null`).
fn fmt_opt(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string())
}

fn shape(message: &str) -> Error {
    Error::new("shape", message)
}

// ---------------------------------------------------------------------------------------------
// The row helpers (ports of the reference's key/bestRow/column/onlyRanked)

fn row_key(row: &RowIn) -> String {
    format!(
        "{}|{}|{}",
        row.model,
        row.runtime.as_deref().unwrap_or(""),
        row.effort.as_deref().unwrap_or("")
    )
}

fn row_version(row: &RowIn) -> &str {
    row.process.as_ref().and_then(|p| p.version.as_deref()).unwrap_or("")
}

/// Sorts row indexes the way the reference picks one row per model at one effort for a
/// job: ranked first, then the newest process version, then the most runs.
fn sort_best(rows: &[RowIn], indexes: &mut [usize]) {
    indexes.sort_by(|&a, &b| {
        let ranked = (rows[b].ranked as i8).cmp(&(rows[a].ranked as i8));
        let version = row_version(&rows[b]).cmp(row_version(&rows[a]));
        let runs = (rows[b].n.observed + rows[b].n.benchmark).cmp(&(rows[a].n.observed + rows[a].n.benchmark));
        ranked.then(version).then(runs)
    });
}

/// Sorts row indexes by rank (the reference's `sort((a, b) => a.rank - b.rank)`; a null
/// rank reads 0), keeping the board's order among equals.
fn sort_by_rank(rows: &[RowIn], indexes: &mut [usize]) {
    indexes.sort_by_key(|i| rows[*i].rank.unwrap_or(0));
}

/// The evidence a row's headline stands on: its basis column, or the first ranked
/// benchmark column.
fn column(row: &RowIn) -> Option<&ColumnIn> {
    match row.basis.as_deref() {
        Some("observed") => row.observed.as_ref(),
        Some("benchmark") => row
            .benchmarks
            .iter()
            .find(|c| c.ranked)
            .or_else(|| row.benchmarks.first()),
        _ => row.observed.as_ref().or_else(|| row.benchmarks.first()),
    }
}

/// The view says in words when a row is the only ranked one; that sentence is what the
/// cell and the compare text key on.
fn only_ranked(row: &RowIn) -> bool {
    let why = column(row).map(|c| c.why.as_str()).unwrap_or(row.why.as_str());
    why.contains("only ranked row")
}

// ---------------------------------------------------------------------------------------------
// The cells

/// A matrix cell. `sub` is a list of chunks the page keeps whole.
fn cell(row: &RowIn, ranked1: bool, tried: usize, min_runs: MinRuns) -> Result<CellOut> {
    let c = column(row).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
    let word = verdict_word(&row.verdict).to_string();
    let quality = c.measures.get("quality");
    let n = c.n;
    if row.verdict == "avoid" {
        // A missing cheating measure is unknown, not zero: the row may be Avoid on another
        // measure, and the cell then says how many runs there were (2026-11-09, A3).
        if let Some(cheating) = c.measures.get("cheating")
            && cheating.number(&cheating.value).is_some_and(|v| v >= 0.2)
        {
            return Ok(CellOut {
                verdict: "avoid".to_string(),
                word,
                sub: vec![format!(
                    "cheated in {} of {} runs",
                    fmt_opt(cheating.of),
                    fmt_opt(cheating.per)
                )],
            });
        }
        return Ok(CellOut {
            verdict: "avoid".to_string(),
            word,
            sub: vec![format!("{n} runs")],
        });
    }
    if row.verdict == "not-enough-data" {
        if n < min_runs {
            return Ok(CellOut {
                verdict: "not-enough-data".to_string(),
                word: format!("{n} runs"),
                sub: vec![format!("needs {min_runs}")],
            });
        }
        return Ok(CellOut {
            verdict: "not-enough-data".to_string(),
            word,
            sub: vec![format!("{n} runs, none reviewed yet")],
        });
    }
    if row.basis.as_deref() == Some("benchmark") {
        return Ok(CellOut {
            verdict: row.verdict.clone(),
            word,
            sub: vec!["benchmark".to_string(), format!("{n} runs")],
        });
    }
    if only_ranked(row) {
        return Ok(CellOut {
            verdict: row.verdict.clone(),
            word,
            sub: if tried == 1 {
                vec![format!("{n} runs"), "only model tried".to_string()]
            } else {
                vec![format!("{n} real runs"), "nothing to compare".to_string()]
            },
        });
    }
    if let Some(q) = quality
        && q.per.is_some_and(|per| per >= 2)
    {
        // The rounds each item took to pass reach the cell: how many needed no fix round.
        return Ok(CellOut {
            verdict: row.verdict.clone(),
            word,
            sub: vec![
                format!("{n} runs"),
                format!(
                    "{} {} of {}{}",
                    if ranked1 { "passed review" } else { "passed" },
                    fmt_opt(q.of),
                    fmt_opt(q.per),
                    match q.first_try {
                        Some(first_try) => format!(", {first_try} on the first try"),
                        None => String::new(),
                    }
                ),
            ],
        });
    }
    Ok(CellOut {
        verdict: row.verdict.clone(),
        word,
        sub: vec![format!("{n} runs")],
    })
}

// ---------------------------------------------------------------------------------------------
// The report blocks

/// { low, high, value } as the view wrote the numbers, for the page's likely-range scale.
fn band(measure: &MeasureIn) -> Json {
    json!({ "low": measure.low, "high": measure.high, "value": measure.value })
}

fn over_words(n: i64) -> String {
    format!("Over {n} {}", if n == 1 { "run" } else { "runs" })
}

fn likely_between(measure: &MeasureIn) -> Result<String> {
    Ok(format!(
        "Likely between {} and {}.",
        pct_json(&measure.low)?,
        pct_json(&measure.high)?
    ))
}

/// The measures shown in a job's report block, in the contract's order, each only when
/// its value is known.
fn measures(job: &TaskIn, row: &RowIn, min_runs: MinRuns) -> Result<Vec<MeasureOut>> {
    let mut out: Vec<MeasureOut> = Vec::new();
    let c = column(row).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
    let m = &c.measures;
    let unit = job_word(&job.unit);

    if job.id == "scout" {
        let src = if row.basis.as_deref() == Some("benchmark") {
            "benchmark"
        } else {
            "real work"
        };
        let facts = m
            .get("facts-kept")
            .ok_or_else(|| shape("a scout row with no facts-kept measure"))?;
        let drift = m
            .get("verdict-drift")
            .ok_or_else(|| shape("a scout row with no verdict-drift measure"))?;
        // Each scout measure shows only when its value and both bounds are known; a null
        // among them omits the measure rather than printing a zero (2026-11-09, A2).
        for (label, measure) in [("Facts kept", facts), ("Verdicts changed on a re-check", drift)] {
            if !measure.span_known() {
                continue;
            }
            out.push(MeasureOut {
                label: label.to_string(),
                value: pct_json(&measure.value)?,
                sub: src.to_string(),
                note: format!(
                    "{}; likely between {} and {}.",
                    over_words(measure.n),
                    pct_json(&measure.low)?,
                    pct_json(&measure.high)?
                ),
                range: Some(band(measure)),
            });
        }
    } else if job.id == "review" {
        if let Some(q) = m.get("quality").filter(|q| q.span_known()) {
            let per = q.per.unwrap_or(0);
            let thin = per < min_runs;
            out.push(MeasureOut {
                label: "Its reviews held up".to_string(),
                value: pct_json(&q.value)?,
                sub: format!("{} of {}", fmt_opt(q.of), fmt_opt(q.per)),
                note: if thin {
                    format!(
                        "Only {per} of its reviews {} re-checked, so this says little yet (likely {} to {}).",
                        if per == 1 { "was" } else { "were" },
                        pct_json(&q.low)?,
                        pct_json(&q.high)?
                    )
                } else {
                    likely_between(q)?
                },
                range: Some(band(q)),
            });
        }
    } else if job.id == "plan" {
        if let Some(s) = m.get("silent-misses").filter(|s| s.span_known()) {
            out.push(MeasureOut {
                label: "Planned facts missed".to_string(),
                value: pct_json(&s.value)?,
                sub: (if row.basis.as_deref() == Some("benchmark") {
                    "benchmark"
                } else {
                    "real work"
                })
                .to_string(),
                note: format!(
                    "{}; likely between {} and {}.",
                    over_words(s.n),
                    pct_json(&s.low)?,
                    pct_json(&s.high)?
                ),
                range: Some(band(s)),
            });
        }
    } else {
        if let Some(q) = m.get("quality").filter(|q| q.span_known()) {
            out.push(MeasureOut {
                label: "Passed review".to_string(),
                value: pct_json(&q.value)?,
                sub: format!(
                    "{} of {}{}",
                    fmt_opt(q.of),
                    fmt_opt(q.per),
                    match q.first_try {
                        Some(first_try) => format!(", {first_try} on the first try"),
                        None => String::new(),
                    }
                ),
                note: format!(
                    "With {} reviews the true rate is likely between {} and {}.",
                    fmt_opt(q.per),
                    pct_json(&q.low)?,
                    pct_json(&q.high)?
                ),
                range: Some(band(q)),
            });
        }
        if let Some(o) = m.get("overclaim").filter(|o| o.span_known()) {
            out.push(MeasureOut {
                label: "Said done, reviewer disagreed".to_string(),
                value: pct_json(&o.value)?,
                sub: format!("{} of {}", fmt_opt(o.of), fmt_opt(o.per)),
                note: likely_between(o)?,
                range: Some(band(o)),
            });
        }
    }

    let speed = m.get("speed");
    if let Some(sp) = speed.filter(|sp| sp.has_value()) {
        let per_task = sp.per_task.as_ref().filter(|_| job.unit == "task");
        let value = match per_task {
            Some(stat) => minutes(stat.value.ok_or_else(|| shape("a per-task speed with no value"))?)?,
            None => seconds(sp.value.as_f64().ok_or_else(|| shape("a speed with no usable value"))?)?,
        };
        let note = if let Some(stat) = per_task {
            // A missing bound is not zero: the reference's number check refuses it.
            format!(
                "Fastest {}, slowest {}, over {} runs.",
                minutes(stat.low.ok_or_else(|| shape("a per-task speed with no value"))?)?,
                minutes(stat.high.ok_or_else(|| shape("a per-task speed with no value"))?)?,
                stat.n
            )
        } else if job.id == "scout"
            && sp.low.as_f64().is_some()
            && sp.high.as_f64().is_some()
            && sp.low.as_f64() != sp.high.as_f64()
        {
            format!(
                "Range {} to {} s.",
                to_fixed(sp.low.as_f64().ok_or_else(|| shape("a speed with no usable value"))?, 1)?,
                to_fixed(
                    sp.high.as_f64().ok_or_else(|| shape("a speed with no usable value"))?,
                    1
                )?
            )
        } else {
            format!("Over {} runs.", sp.n)
        };
        out.push(MeasureOut {
            label: format!("Time per {unit}"),
            value,
            sub: "median".to_string(),
            note,
            range: None,
        });
    }

    let cost = m.get("cost");
    if let Some(co) = cost.filter(|co| co.has_value()) {
        let label = if job.unit == "task" {
            format!("Cost per accepted {unit}")
        } else {
            format!("Cost per {unit}")
        };
        out.push(MeasureOut {
            label,
            value: round1(co.value.as_f64().unwrap_or(0.0))?,
            sub: "credits".to_string(),
            note: format!("All {} runs had a known cost.", co.n),
            range: None,
        });
    } else if let Some(credits) = cost
        .and_then(|co| co.estimate.as_ref())
        .and_then(|estimate| estimate.credits.as_ref())
        .filter(|credits| credits.value.is_some())
    {
        out.push(MeasureOut {
            label: format!("Cost per {unit}"),
            value: format!("≈ {}", round1(credits.value.unwrap_or(0.0))?),
            sub: "credits, estimated".to_string(),
            note: format!("From {} runs; an estimate, not a measured cost.", credits.n),
            range: None,
        });
    }

    let cheating = m.get("cheating");
    if let Some(ch) = cheating.filter(|ch| ch.has_value()) {
        let runs_total = if job.id == "scout" {
            row.n.observed + row.n.benchmark
        } else {
            c.n
        };
        let thorough = m.get("thoroughness");
        let read = if job.id != "scout" && thorough.is_some_and(|th| th.number(&th.value) == Some(1.0)) {
            format!(
                "Read everything it was given in {} of {} runs.",
                thorough.unwrap().n,
                c.n
            )
        } else {
            String::new()
        };
        let value = if ch.number(&ch.value) == Some(0.0) {
            "none".to_string()
        } else {
            format!("{} of {}", fmt_opt(ch.of), fmt_opt(ch.per))
        };
        let sub = if ch.number(&ch.value) == Some(0.0) {
            format!("in {runs_total} runs")
        } else {
            "runs flagged".to_string()
        };
        out.push(MeasureOut {
            label: "Cheating flags".to_string(),
            value,
            sub,
            note: read,
            range: None,
        });
    }
    Ok(out)
}

/// The sentence under a job's measures: who else ran this job.
fn compare(job: &TaskIn, row: &RowIn, row_index: usize, others: &[usize], min_runs: MinRuns) -> Result<Vec<Run>> {
    let mut runs: Vec<Run> = Vec::new();
    let c = column(row).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
    let recommended_other = others.iter().find(|i| job.rows[**i].verdict == "recommended").copied();
    if row.verdict == "recommended"
        && let Some(other_index) = recommended_other
    {
        let other = &job.rows[other_index];
        let oc = column(other).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
        // All four figures must be known finite numbers, and each pair must match.
        // Two missing reads are not a measured tie (2026-11-09, R1).
        let known_finite = |column: &ColumnIn, id: &str| {
            column
                .measures
                .get(id)
                .and_then(|measure| measure.value.as_f64())
                .filter(|value| value.is_finite())
        };
        let same_known = |id: &str| match (known_finite(oc, id), known_finite(c, id)) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        };
        runs.push(strong(name(&other.label, &other.effort)));
        runs.push(run(format!(
            " is also Recommended here, with {} runs{}.",
            oc.n,
            if same_known("speed") && same_known("cost") {
                " at the same speed and cost"
            } else {
                ""
            }
        )));
        return Ok(runs);
    }
    if only_ranked(row) && row.verdict == "fine" {
        runs.push(strong("Why only Fine:"));
        runs.push(run(format!(
            " no other model has {min_runs} real {} runs, so there is nothing to compare.",
            job.label
        )));
        let bench = row.benchmarks.iter().find(|b| b.ranked);
        let peer = others
            .iter()
            .find(|i| job.rows[**i].benchmarks.iter().any(|b| b.ranked))
            .map(|i| &job.rows[*i]);
        if let (Some(bench), Some(peer)) = (bench, peer)
            && job.id == "scout"
        {
            let peer_bench = peer.benchmarks.iter().find(|b| b.ranked);
            // Both facts-kept figures must be known: an unknown one omits the sentence,
            // never prints a zero (2026-11-09 amendment, A2).
            let kept = bench.measures.get("facts-kept").filter(|m| m.span_known());
            let peer_kept = peer_bench
                .and_then(|b| b.measures.get("facts-kept"))
                .filter(|m| m.span_known());
            if let (Some(kept), Some(peer_kept)) = (kept, peer_kept) {
                runs.push(run(format!(
                    " In the {} benchmark it kept {} of facts; {} kept {}.",
                    job.label,
                    pct_json(&kept.value)?,
                    name(&peer.label, &peer.effort),
                    pct_json(&peer_kept.value)?,
                )));
            }
        }
        let older = job
            .rows
            .iter()
            .enumerate()
            .find(|(i, o)| *i != row_index && o.model == row.model && o.effort == row.effort && o.process.is_some())
            .map(|(_, o)| o);
        if let Some(older) = older {
            let major = older
                .process
                .as_ref()
                .and_then(|p| p.version.as_deref())
                .unwrap_or("")
                .split('.')
                .next()
                .unwrap_or("");
            let n = older.n.observed + older.n.benchmark;
            runs.push(run(format!(
                " The older pipeline {major}.x has {n} benchmark runs, too few for a verdict."
            )));
        }
        return Ok(runs);
    }
    if !others.is_empty() {
        runs.push(run("Others on this job: "));
        for (at, index) in others.iter().enumerate() {
            let other = &job.rows[*index];
            let oc = column(other);
            runs.push(strong(name(&other.label, &other.effort)));
            if other.verdict == "avoid" {
                runs.push(run(" "));
                runs.push(crit("Avoid"));
                let cheating = oc.and_then(|c| c.measures.get("cheating"));
                let flagged = cheating.is_some_and(|ch| ch.number(&ch.value).is_some_and(|v| v >= 0.2));
                runs.push(run(if flagged {
                    format!(
                        ", cheated in {} of {} runs.",
                        fmt_opt(cheating.unwrap().of),
                        fmt_opt(cheating.unwrap().per)
                    )
                } else {
                    ".".to_string()
                }));
            } else {
                let mut parts: Vec<String> = Vec::new();
                if let Some(q) = oc.and_then(|c| c.measures.get("quality"))
                    && q.per.is_some_and(|per| per >= 2)
                {
                    parts.push(format!(
                        "passed {} of {} ({})",
                        fmt_opt(q.of),
                        fmt_opt(q.per),
                        pct_json(&q.value)?
                    ));
                }
                if let Some(sp) = oc
                    .and_then(|c| c.measures.get("speed"))
                    .and_then(|s| s.per_task.as_ref())
                    .filter(|_| job.unit == "task")
                    .filter(|stat| stat.value.is_some())
                {
                    parts.push(format!("{} per task", minutes(sp.value.unwrap_or(0.0))?));
                }
                runs.push(run(format!(
                    " {}{}.",
                    verdict_word(&other.verdict),
                    if parts.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", parts.join(", "))
                    }
                )));
            }
            if at < others.len() - 1 {
                runs.push(run(" "));
            }
        }
    }
    Ok(runs)
}

/// The basis sentence of a job's report block: what the verdict stands on, in runs.
fn report_basis(row: &RowIn) -> Result<String> {
    let c = column(row).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
    if row.basis.as_deref() == Some("benchmark") {
        return Ok(format!("{} benchmark runs", c.n));
    }
    if row.benchmarks.iter().any(|b| b.n > 0) && row.n.observed != 0 {
        let bench: i64 = row.benchmarks.iter().map(|b| b.n).sum();
        let process = match &row.process {
            Some(process) => format!(
                ", {} {}",
                if process.id == "scout-pipeline" {
                    "Scout pipeline"
                } else {
                    process.id.as_str()
                },
                process.version.clone().unwrap_or_else(|| "null".to_string())
            ),
            None => String::new(),
        };
        return Ok(format!(
            "{} real runs and {bench} benchmark runs{process}",
            row.n.observed
        ));
    }
    if let Some(q) = c.measures.get("quality").filter(|q| q.per.is_some_and(|per| per >= 2)) {
        return Ok(format!(
            "{} real runs, {} checked by a blind reviewer",
            c.n,
            fmt_opt(q.per)
        ));
    }
    Ok(format!("{} real runs", c.n))
}

/// The pick for one job: the top recommended row, or why there is none.
fn pick_for(job: &TaskIn, min_runs: MinRuns) -> Result<PickOut> {
    let mut ranked: Vec<usize> = job
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.ranked)
        .map(|(i, _)| i)
        .collect();
    sort_by_rank(&job.rows, &mut ranked);
    let recommended: Vec<usize> = ranked
        .iter()
        .copied()
        .filter(|i| job.rows[*i].verdict == "recommended")
        .collect();
    if recommended.is_empty() {
        let tried: usize = job.rows.iter().map(row_key).collect::<HashSet<_>>().len();
        let obs_top: Vec<usize> = job
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.n.observed >= min_runs)
            .map(|(i, _)| i)
            .collect();
        let why = if tried == 1 {
            let x = &job.rows[0];
            vec![run(format!(
                "{} is the only model tried ({} runs, {}).",
                name(&x.label, &x.effort),
                x.n.observed + x.n.benchmark,
                verdict_word(&x.verdict)
            ))]
        } else if obs_top.len() == 1 {
            let x = &job.rows[obs_top[0]];
            vec![run(format!(
                "Only {} has {} real runs, so there is nothing to compare it with.",
                name(&x.label, &x.effort),
                x.n.observed
            ))]
        } else {
            vec![run("No model has enough reviewed runs yet.")]
        };
        return Ok(PickOut {
            job: job.id.clone(),
            job_label: job.label.clone(),
            model: None,
            none_label: Some("No pick yet"),
            why,
        });
    }
    let top_index = recommended[0];
    let top = &job.rows[top_index];
    let c = column(top).ok_or_else(|| shape("a verdict row with no column to stand on"))?;
    let next_index = ranked
        .iter()
        .position(|i| *i == top_index)
        .and_then(|at| ranked.get(at + 1))
        .copied();
    let next = next_index.map(|i| &job.rows[i]);
    let mut why: Vec<Run> = Vec::new();
    if top.basis.as_deref() == Some("benchmark") {
        why.push(strong("Benchmark only"));
        why.push(run(format!(", {} runs.", c.n)));
        let miss = c.measures.get("silent-misses");
        let next_miss = next.and_then(column).and_then(|nc| nc.measures.get("silent-misses"));
        let missed = miss.is_some_and(|m| m.has_value());
        let next_missed = next_miss.is_some_and(|m| m.has_value());
        if missed && next_missed {
            why.push(run(format!(
                " Missed {} of planned facts; {} missed {}.",
                pct_json(&miss.unwrap().value)?,
                name(&next.unwrap().label, &next.unwrap().effort),
                pct_json(&next_miss.unwrap().value)?
            )));
        } else if let Some(next) = next {
            why.push(run(format!(" {} is next.", name(&next.label, &next.effort))));
        }
    } else {
        let quality = c.measures.get("quality");
        let next_c = next.and_then(column);
        let tied = next.is_some_and(|n| n.verdict == "recommended")
            && ["quality", "speed", "cost"].iter().all(|id| {
                let a = c.measures.get(*id).and_then(|m| m.number(&m.value));
                let b = next_c
                    .and_then(|nc| nc.measures.get(*id))
                    .and_then(|m| m.number(&m.value));
                match (a, b) {
                    (Some(a), Some(b)) => (a - b).abs() <= 0.05 * a.abs().max(b.abs()),
                    _ => false,
                }
            });
        if tied {
            let next_c = next_c.unwrap();
            why.push(run(format!(
                "Tied with {} on quality, speed and cost. ",
                name(&next.unwrap().label, &next.unwrap().effort)
            )));
            why.push(strong(c.n.to_string()));
            why.push(run(format!(
                " real runs{}.",
                if c.n > next_c.n { ", the most" } else { "" }
            )));
        } else {
            if let Some(q) = quality.filter(|q| q.per.is_some_and(|per| per >= 2)) {
                why.push(run("Passed blind review "));
                why.push(strong(format!("{} of {}", fmt_opt(q.of), fmt_opt(q.per))));
                why.push(run(". "));
            }
            why.push(run(format!("{} real runs.", c.n)));
            if let Some(next) = next {
                why.push(run(format!(" {} is next.", name(&next.label, &next.effort))));
            }
        }
    }
    Ok(PickOut {
        job: job.id.clone(),
        job_label: job.label.clone(),
        model: Some(PickModel {
            label: top.label.clone(),
            effort: top.effort.clone(),
            mark: top.runtime.as_deref().and_then(mark_of),
        }),
        none_label: None,
        why,
    })
}

// ---------------------------------------------------------------------------------------------
// One view (the title-line switch: `pipeline` or `pm-users`)

/// The task types a view shows, and from which use each one's verdicts come (the first
/// use that lists it).
fn view_jobs<'a>(view: &'a ViewIn, use_ids: &[&str]) -> Result<Vec<&'a TaskIn>> {
    let mut jobs: Vec<&TaskIn> = Vec::new();
    for use_id in use_ids {
        let use_in = view
            .uses
            .iter()
            .find(|u| u.id == *use_id)
            .ok_or_else(|| shape("a view over a use the scoreboard does not list"))?;
        for task in &use_in.task_types {
            if !jobs.iter().any(|job| job.id == task.id) {
                jobs.push(task);
            }
        }
    }
    Ok(jobs)
}

/// One model's work-in-progress while a view is built.
struct ModelWork {
    key: String,
    model: String,
    label: String,
    effort: Option<String>,
    runtime: Option<String>,
    /// The chosen row of each job this model ran, by that job's own row index.
    rows: Vec<(String, usize)>,
    cells: HashMap<String, CellOut>,
    real: i64,
    bench: i64,
    reviews: i64,
}

fn build_view(view: &ViewIn, id: &str, label: &str, short: &str, use_ids: &[&str]) -> Result<ViewOut> {
    let min_runs = view.catalog.min_runs;
    let jobs = view_jobs(view, use_ids)?;
    const ORDER: [&str; 4] = ["worknode-build", "review", "plan", "scout"];
    // Which task types get a column: at least one row with a real verdict, in the
    // design's order, then the catalog's.
    let mut shown: Vec<&TaskIn> = jobs
        .iter()
        .copied()
        .filter(|job| job.rows.iter().any(|row| row.verdict != "not-enough-data"))
        .collect();
    shown.sort_by_key(|job| ORDER.iter().position(|o| *o == job.id).unwrap_or(ORDER.len()));

    // One row per model at one effort across the view's jobs.
    let mut grouped: Vec<ModelWork> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for job in &shown {
        let mut by_model: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, row) in job.rows.iter().enumerate() {
            let key = row_key(row);
            match by_model.iter_mut().find(|(k, _)| *k == key) {
                Some((_, indexes)) => indexes.push(i),
                None => by_model.push((key, vec![i])),
            }
        }
        for (key, mut indexes) in by_model {
            sort_best(&job.rows, &mut indexes);
            let best = &job.rows[indexes[0]];
            let at = match index.get(&key) {
                Some(at) => *at,
                None => {
                    grouped.push(ModelWork {
                        key: key.clone(),
                        model: best.model.clone(),
                        label: best.label.clone(),
                        effort: best.effort.clone(),
                        runtime: best.runtime.clone(),
                        rows: Vec::new(),
                        cells: HashMap::new(),
                        real: 0,
                        bench: 0,
                        reviews: 0,
                    });
                    index.insert(key.clone(), grouped.len() - 1);
                    grouped.len() - 1
                }
            };
            grouped[at].rows.push((job.id.clone(), indexes[0]));
        }
    }

    // The cells, then the run counts behind each model row.
    let rank_one = |job: &TaskIn, row_index: usize| -> bool {
        let mut ranked: Vec<usize> = job
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.ranked)
            .map(|(i, _)| i)
            .collect();
        sort_by_rank(&job.rows, &mut ranked);
        ranked.first() == Some(&row_index)
    };
    for model in grouped.iter_mut() {
        for job in &shown {
            let Some((_, row_index)) = model.rows.iter().find(|(job_id, _)| job_id == &job.id) else {
                continue;
            };
            let row = &job.rows[*row_index];
            let tried = job.rows.iter().map(row_key).collect::<HashSet<_>>().len();
            let ranked1 = rank_one(job, *row_index);
            model.cells.insert(job.id.clone(), cell(row, ranked1, tried, min_runs)?);
        }
    }
    for model in grouped.iter_mut() {
        // Runs: every run of this model at this effort, in every task type of every use,
        // counted once by task type and process (the view repeats rows across uses).
        let mut seen: HashSet<String> = HashSet::new();
        let mut real = 0;
        let mut bench = 0;
        for use_in in &view.uses {
            for task in &use_in.task_types {
                for row in &task.rows {
                    if row_key(row) != model.key {
                        continue;
                    }
                    let dedupe = format!(
                        "{}|{}|{}",
                        task.id,
                        row.process.as_ref().map(|p| p.id.as_str()).unwrap_or(""),
                        row.process.as_ref().and_then(|p| p.version.as_deref()).unwrap_or("")
                    );
                    if seen.insert(dedupe) {
                        real += row.n.observed;
                        bench += row.n.benchmark;
                    }
                }
            }
        }
        let mut review_cols: HashMap<String, i64> = HashMap::new();
        for use_in in &view.uses {
            for task in &use_in.task_types {
                for row in &task.rows {
                    if row_key(row) == model.key
                        && let Some(observed) = &row.observed
                    {
                        review_cols.insert(
                            format!(
                                "{}|{}",
                                task.id,
                                row.process.as_ref().and_then(|p| p.version.as_deref()).unwrap_or("")
                            ),
                            observed.reviews,
                        );
                    }
                }
            }
        }
        model.real = real;
        model.bench = bench;
        model.reviews = review_cols.values().sum();
    }
    grouped.sort_by(|a, b| {
        let recommended = b
            .cells
            .values()
            .filter(|cell| cell.verdict == "recommended")
            .count()
            .cmp(&a.cells.values().filter(|cell| cell.verdict == "recommended").count());
        let any_verdict = b
            .cells
            .values()
            .any(|cell| cell.verdict != "not-enough-data")
            .cmp(&a.cells.values().any(|cell| cell.verdict != "not-enough-data"));
        let runs = (b.real + b.bench).cmp(&(a.real + a.bench));
        recommended
            .then(any_verdict)
            .then(runs)
            .then_with(|| a.label.cmp(&b.label))
    });

    let out_models: Vec<ModelOut> = grouped
        .iter()
        .map(|model| {
            let runtime = model.runtime.as_deref().map(runtime_word).unwrap_or("").to_string();
            let runs = model.real + model.bench;
            let cells: BTreeMap<String, Option<CellOut>> = shown
                .iter()
                .map(|job| (job.id.clone(), model.cells.get(&job.id).cloned()))
                .collect();
            let mut report_jobs: Vec<&&TaskIn> = shown
                .iter()
                .filter(|job| {
                    model.rows.iter().any(|(job_id, _)| job_id == &job.id)
                        && model
                            .cells
                            .get(&job.id)
                            .is_some_and(|cell| cell.verdict != "not-enough-data")
                })
                .collect();
            report_jobs.sort_by_key(|job| model.cells.get(&job.id).map(|c| c.verdict == "fine").unwrap_or(false));
            let report: Vec<ReportOut> = report_jobs
                .iter()
                .map(|job| -> Result<ReportOut> {
                    let cell = model.cells.get(&job.id).expect("filtered to jobs with a cell");
                    let (_, row_index) = model
                        .rows
                        .iter()
                        .find(|(job_id, _)| job_id == &job.id)
                        .expect("filtered to jobs with a row");
                    let row = &job.rows[*row_index];
                    let mut others: Vec<usize> = job
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(i, other)| *i != *row_index && other.ranked && row_key(other) != model.key)
                        .map(|(i, _)| i)
                        .collect();
                    sort_by_rank(&job.rows, &mut others);
                    Ok(ReportOut {
                        job: job.id.clone(),
                        label: job.label.clone(),
                        verdict: cell.verdict.clone(),
                        word: cell.word.clone(),
                        basis: report_basis(row)?,
                        measures: measures(job, row, min_runs)?,
                        compare: compare(job, row, *row_index, &others, min_runs)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ModelOut {
                key: model.key.clone(),
                model: model.model.clone(),
                label: model.label.clone(),
                effort: model.effort.clone(),
                effort_text: model.effort.clone().unwrap_or_else(|| "effort unknown".to_string()),
                runtime,
                mark: model.runtime.as_deref().and_then(mark_of),
                runs,
                head: format!("{} · ", model.runtime.as_deref().map(runtime_word).unwrap_or("")),
                head_runs: vec![
                    strong(runs.to_string()),
                    run(format!(
                        " runs: {} real work, {} benchmark · {} blind reviews",
                        model.real, model.bench, model.reviews
                    )),
                ],
                sheet_head: format!(
                    "{} · {runs} runs · {} blind reviews",
                    model.runtime.as_deref().map(runtime_word).unwrap_or(""),
                    model.reviews
                ),
                cells,
                report,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let picks: Vec<PickOut> = shown.iter().map(|job| pick_for(job, min_runs)).collect::<Result<_>>()?;

    // The foot: catalog task types without a column, then catalog models with no runs.
    let no_verdict: Vec<String> = view
        .catalog
        .task_types
        .iter()
        .filter(|t| !shown.iter().any(|job| job.id == t.id))
        .map(|t| {
            let job = jobs.iter().copied().find(|job| job.id == t.id).or_else(|| {
                view.uses
                    .iter()
                    .flat_map(|u| u.task_types.iter())
                    .find(|x| x.id == t.id)
            });
            match job.and_then(|job| {
                job.rows
                    .iter()
                    .find(|x| x.n.observed + x.n.benchmark > 0 && x.label != "example/example-1")
            }) {
                Some(some) => format!(
                    "{} ({} has {} runs)",
                    t.label,
                    name(&some.label, &some.effort),
                    some.n.observed + some.n.benchmark
                ),
                None => t.label.clone(),
            }
        })
        .collect();
    let tested: HashSet<&str> = view
        .uses
        .iter()
        .flat_map(|u| u.task_types.iter())
        .flat_map(|t| t.rows.iter())
        .map(|row| row.model.as_str())
        .collect();
    let no_runs: Vec<String> = view
        .catalog
        .models
        .iter()
        .filter(|m| !tested.contains(m.model.as_str()))
        .map(|m| m.label.clone())
        .collect();
    // A run naming a model the catalog does not list is flagged in the foot, with its runs
    // counted once per task type and process, the way a model row's own runs are (B11).
    let catalog_models: HashSet<&str> = view.catalog.models.iter().map(|m| m.model.as_str()).collect();
    let mut unknown: BTreeMap<String, (String, i64)> = BTreeMap::new();
    let mut seen_dedupe: HashSet<String> = HashSet::new();
    for use_in in &view.uses {
        for task in &use_in.task_types {
            for row in &task.rows {
                if catalog_models.contains(row.model.as_str()) {
                    continue;
                }
                // Runs count once per task type and process: the uses repeat the same rows.
                let dedupe = format!(
                    "{}|{}|{}|{}",
                    task.id,
                    row.model,
                    row.process.as_ref().map(|p| p.id.as_str()).unwrap_or(""),
                    row.process.as_ref().and_then(|p| p.version.as_deref()).unwrap_or("")
                );
                if !seen_dedupe.insert(dedupe) {
                    continue;
                }
                let entry = unknown
                    .entry(row.model.clone())
                    .or_insert_with(|| (row.label.clone(), 0));
                entry.1 += row.n.observed + row.n.benchmark;
            }
        }
    }
    let unknown_models: Vec<String> = unknown
        .into_values()
        .map(|(label, runs)| format!("{label} ({runs} {})", if runs == 1 { "run" } else { "runs" }))
        .collect();

    Ok(ViewOut {
        id: id.to_string(),
        label: label.to_string(),
        short: short.to_string(),
        meta: vec![
            strong(out_models.len().to_string()),
            run(format!(
                " models with runs · a verdict needs {min_runs} runs of that job"
            )),
        ],
        picks,
        jobs: shown
            .iter()
            .map(|job| JobRef {
                id: job.id.clone(),
                label: job.label.clone(),
            })
            .collect(),
        models: out_models,
        foot: [
            vec![
                strong("No verdicts yet:"),
                run(format!(" {}. ", no_verdict.join(", "))),
                strong("In the catalog with no runs:"),
                run(format!(" {}.", no_runs.join(", "))),
            ],
            if unknown_models.is_empty() {
                Vec::new()
            } else {
                vec![
                    strong("Models in runs but not in the catalog:"),
                    run(format!(" {}.", unknown_models.join(", "))),
                ]
            },
        ]
        .concat(),
    })
}

// ---------------------------------------------------------------------------------------------
// The read note (parsed from the scoreboard-runs source detail, as the reference does)

/// The first "N runs from M writers read ... (X observed, Y benchmark)" sentence, scanned
/// the way the reference's regular expression finds it: earliest start that matches.
fn scan_runs_sentence(text: &str) -> Option<(i64, String, i64, i64)> {
    fn try_at(rest: &str) -> Option<(i64, String, i64, i64)> {
        let (runs, after) = take_digits(rest)?;
        let after = after.strip_prefix(" run")?;
        let after = after.strip_prefix('s').unwrap_or(after);
        let after = after.strip_prefix(" from ")?;
        let (_, after) = take_digits(after)?;
        let after = after.strip_prefix(" writer")?;
        let after = after.strip_prefix('s').unwrap_or(after);
        let after = after.strip_prefix(" read ")?;
        // ([^(]+) followed by " (": every char up to the '(', the last one a space.
        let at = after.find('(')?;
        if at < 2 || !after[..at].ends_with(' ') {
            return None;
        }
        let captured = &after[..at - 1];
        let after = after[at..].strip_prefix('(')?;
        let (observed, after) = take_digits(after)?;
        let after = after.strip_prefix(" observed, ")?;
        let (benchmark, after) = take_digits(after)?;
        if !after.starts_with(" benchmark)") {
            return None;
        }
        Some((runs, captured.trim().to_string(), observed, benchmark))
    }
    text.char_indices().find_map(|(start, _)| try_at(&text[start..]))
}

/// "N files unreadable": the count of damaged run files.
fn scan_unreadable(text: &str) -> Option<i64> {
    fn try_at(rest: &str) -> Option<i64> {
        let (files, after) = take_digits(rest)?;
        let after = after.strip_prefix(" file")?;
        let after = after.strip_prefix('s').unwrap_or(after);
        after.starts_with(" unreadable").then_some(files)
    }
    text.char_indices().find_map(|(start, _)| try_at(&text[start..]))
}

fn take_digits(text: &str) -> Option<(i64, &str)> {
    let end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    let value: i64 = text[..end].parse().ok()?;
    Some((value, &text[end..]))
}

/// The quiet sentences at the end of the page, and the band summary's short form.
fn read_note(sources: &[SourceIn]) -> (Vec<Run>, Option<String>) {
    let detail = sources
        .iter()
        .find(|s| s.id == "scoreboard-runs")
        .map(|s| s.detail.as_str())
        .unwrap_or("");
    let mut note: Vec<Run> = Vec::new();
    let read = scan_runs_sentence(detail);
    if let Some((runs, at, observed, benchmark)) = &read {
        note.push(run(format!(
            "Read from {runs} runs at {at}: {observed} real work, {benchmark} benchmark."
        )));
    }
    if let Some(files) = scan_unreadable(detail) {
        note.push(run(if files == 1 {
            " One run file could not be read (a damaged write) and is left out.".to_string()
        } else {
            format!(" {files} run files could not be read (a damaged write) and are left out.")
        }));
    }
    let summary = read.map(|(runs, at, _, _)| format!("{runs} runs read at {at}"));
    (note, summary)
}

// ---------------------------------------------------------------------------------------------
// The composer

/// The composed `byModel` field: both views, the picks and the read note. Pure: the same
/// view gives the same words.
pub fn by_model(view: &ViewIn) -> Result<ByModel> {
    let min_runs = view.catalog.min_runs;
    let (note, summary) = read_note(&view.sources);
    let pipeline = build_view(
        view,
        "pipeline",
        "Our pipeline",
        "Our pipeline",
        &["planning", "building", "pm-users"],
    )?;
    let band_summary = match &summary {
        Some(summary) => format!("{} models with runs · {summary}", pipeline.models.len()),
        None => format!("{} models with runs", pipeline.models.len()),
    };
    Ok(ByModel {
        default_view: "pipeline",
        intro: format!(
            "Which model to use for which job. Each row is one model at one effort. A verdict needs at least {min_runs} \
             runs of that job, and real work also needs a blind review. Real work and benchmarks are never mixed."
        ),
        read_note: note,
        read_link: ReadLink {
            label: "What was read",
            anchor: "sources",
        },
        band_summary,
        views: vec![
            pipeline,
            build_view(view, "pm-users", "Puppet Master users", "PM users", &["pm-users"])?,
        ],
    })
}
