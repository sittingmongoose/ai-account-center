//! The Model Scoreboard (docs/scoreboard.md, view v1): which models to use for which work.
//!
//! The server computes every value, interval, rank and verdict from the agent reports and the catalog; this page only
//! lays them out. Observed work and planned benchmarks stay in separate columns, every value shows how many runs are
//! behind it, and a row without enough runs is shown apart, unranked.

use crate::api::{FetchError, Link, Refresh, nullable, send};
use crate::fmt;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

pub const SCOREBOARD_VIEW_VERSION: u32 = 1;
const USE_KEY: &str = "aac.scoreboard.use.v1";

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScoreboardView {
    pub version: u32,
    #[serde(deserialize_with = "nullable")]
    pub generated_at: String,
    pub refresh: Option<Refresh>,
    #[serde(deserialize_with = "nullable")]
    pub sources: Vec<SbSource>,
    pub catalog: Catalog,
    #[serde(deserialize_with = "nullable")]
    pub uses: Vec<Use>,
    #[serde(deserialize_with = "nullable")]
    pub recent: Vec<RunSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SbSource {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub as_of: Option<String>,
    pub age_seconds: Option<i64>,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Catalog {
    pub min_runs: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub measures: Vec<CatalogMeasure>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogMeasure {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// "lower" or "higher".
    #[serde(deserialize_with = "nullable")]
    pub better: String,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Use {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub task_types: Vec<TaskType>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskType {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    #[serde(deserialize_with = "nullable")]
    pub measures: Vec<String>,
    pub by_process: bool,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Row {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub model: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub runtime: Option<String>,
    pub effort: Option<String>,
    pub process: Option<Process>,
    pub n: Counts,
    pub ranked: bool,
    pub rank: Option<u32>,
    /// "recommended", "fine", "avoid" or "not-enough-data".
    #[serde(deserialize_with = "nullable")]
    pub verdict: String,
    #[serde(deserialize_with = "nullable")]
    pub verdict_label: String,
    #[serde(deserialize_with = "nullable")]
    pub why: String,
    pub observed: Option<Column>,
    #[serde(deserialize_with = "nullable")]
    pub benchmarks: Vec<Column>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Process {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Counts {
    pub observed: u32,
    pub benchmark: u32,
}

/// One kind of evidence for a row: observed work, or one planned benchmark.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Column {
    /// "observed" or "benchmark".
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    pub benchmark_label: Option<String>,
    pub n: u32,
    pub imported: u32,
    pub reviews: u32,
    pub ranked: bool,
    pub preliminary: bool,
    #[serde(deserialize_with = "nullable")]
    pub measures: BTreeMap<String, MeasureValue>,
    #[serde(deserialize_with = "nullable")]
    pub flags: Vec<Flag>,
    #[serde(deserialize_with = "nullable")]
    pub runs: Vec<RunSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeasureValue {
    pub value: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    pub n: u32,
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// "wilson90" (a share's 90% interval) or "range" (a median's low and high).
    pub interval: Option<String>,
    pub preliminary: bool,
    pub disputed: Option<u32>,
    pub estimate: Option<Estimate>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Estimate {
    pub credits: Option<Rate>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rate {
    pub value: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    pub n: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Flag {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub run: String,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
    #[serde(deserialize_with = "nullable")]
    pub evidence: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunSummary {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    pub benchmark: Option<String>,
    pub task: RunTask,
    pub label: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub outcome: String,
    #[serde(deserialize_with = "nullable")]
    pub ended_at: String,
    pub wall_seconds: Option<f64>,
    pub imported: Option<serde_json::Value>,
    pub review: Option<RunReview>,
    pub flags: u32,
    #[serde(deserialize_with = "nullable")]
    pub evidence: Vec<Link>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunTask {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunReview {
    #[serde(deserialize_with = "nullable")]
    pub verdict: String,
    pub overturned: bool,
    pub disputed: bool,
}

pub async fn fetch_scoreboard() -> Result<ScoreboardView, FetchError> {
    let (status, text) = send("GET", "/api/scoreboard", None).await?;
    if status == 401 {
        return Err(FetchError::SignedOut);
    }
    if status != 200 {
        return Err(FetchError::Server(status));
    }
    let view: ScoreboardView = serde_json::from_str(&text).map_err(|_| FetchError::Malformed)?;
    if view.version != SCOREBOARD_VIEW_VERSION {
        return Err(FetchError::Version(view.version));
    }
    Ok(view)
}

// ---- Words for values --------------------------------------------------------------------------------------------

/// "s/item" -> "item"; "credits/word" -> "word".
fn per_unit(unit: &str) -> &str {
    unit.split_once('/').map(|(_, per)| per).unwrap_or("")
}

fn seconds(v: f64) -> String {
    if v < 10.0 {
        format!("{v:.1} s")
    } else if v < 120.0 {
        format!("{v:.0} s")
    } else {
        format!("{:.1} min", v / 60.0)
    }
}

fn share(v: f64) -> String {
    let p = v * 100.0;
    if p > 0.0 && p < 1.0 {
        "<1%".into()
    } else {
        format!("{p:.0}%")
    }
}

fn credits(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

/// The main words for a value and the line under it (interval and n). Missing stays missing, never zero.
pub fn measure_words(m: &MeasureValue) -> (String, String) {
    let n = format!("n = {}", m.n);
    let Some(v) = m.value else {
        if let Some(e) = m.estimate.as_ref().and_then(|e| e.credits.as_ref()) {
            if let Some(ev) = e.value {
                return (
                    format!("≈ {} credits per {}", credits(ev), per_unit(&e.unit)),
                    format!("estimate · n = {}", e.n),
                );
            }
        }
        return (
            if m.n == 0 {
                "No data".into()
            } else {
                "Not measured".into()
            },
            n,
        );
    };
    let (main, bounds) = if m.unit == "share" {
        (
            share(v),
            m.low
                .zip(m.high)
                .map(|(l, h)| format!("{}–{}", share(l), share(h))),
        )
    } else if m.unit.starts_with("s/") {
        (
            format!("{} per {}", seconds(v), per_unit(&m.unit)),
            m.low
                .zip(m.high)
                .map(|(l, h)| format!("{}–{}", seconds(l), seconds(h))),
        )
    } else if m.unit.starts_with("credits/") {
        (
            format!("{} credits per {}", credits(v), per_unit(&m.unit)),
            m.low
                .zip(m.high)
                .map(|(l, h)| format!("{}–{}", credits(l), credits(h))),
        )
    } else {
        (
            format!("{v:.2} {}", m.unit),
            m.low.zip(m.high).map(|(l, h)| format!("{l:.2}–{h:.2}")),
        )
    };
    let bounds = match (bounds, m.interval.as_deref()) {
        (Some(b), Some("wilson90")) => Some(format!("90%: {b}")),
        (Some(b), Some("range")) => Some(format!("range {b}")),
        (b, _) => b,
    };
    let mut detail = vec![n];
    if let Some(b) = bounds {
        detail.push(b);
    }
    if let Some(d) = m.disputed.filter(|d| *d > 0) {
        detail.push(format!("{d} disputed"));
    }
    (main, detail.join(" · "))
}

fn verdict_tone(verdict: &str) -> &'static str {
    match verdict {
        "recommended" => "good",
        "avoid" => "crit",
        "fine" => "info",
        _ => "muted",
    }
}

fn flag_words(kind: &str) -> &'static str {
    match kind {
        "cheating" => "Cheating",
        "fabricated" => "Made up",
        "false-claim" => "False claim",
        "incomplete-read" => "Didn't read it all",
        "scope-cut" => "Cut the scope",
        "shortcut" => "Shortcut",
        "test-weakened" => "Weakened a test",
        _ => "Flag",
    }
}

fn row_place(row: &Row) -> String {
    [
        row.runtime.clone(),
        row.effort.clone(),
        row.process.as_ref().map(|p| match &p.version {
            Some(v) => format!("{} {v}", p.id),
            None => p.id.clone(),
        }),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

fn stored_use() -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(USE_KEY).ok().flatten())
}

fn store_use(id: &str) {
    if let Some(s) = window().local_storage().ok().flatten() {
        let _ = s.set_item(USE_KEY, id);
    }
}

// ---- Page --------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Board {
    view: RwSignal<Option<ScoreboardView>>,
    error: RwSignal<Option<FetchError>>,
    fetched_at_ms: RwSignal<f64>,
    now_ms: RwSignal<f64>,
    loading: RwSignal<bool>,
}

impl Board {
    fn reload(self) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        spawn_local(async move {
            match fetch_scoreboard().await {
                Ok(view) => {
                    let now = js_sys::Date::now();
                    self.view.set(Some(view));
                    self.fetched_at_ms.set(now);
                    self.now_ms.set(now);
                    self.error.set(None);
                }
                Err(e) => self.error.set(Some(e)),
            }
            self.loading.set(false);
        });
    }

    fn elapsed_seconds(self) -> i64 {
        ((self.now_ms.get() - self.fetched_at_ms.get()) / 1000.0).floor() as i64
    }
}

#[component]
pub fn ScoreboardPage(embedded: bool) -> impl IntoView {
    let board = Board {
        view: RwSignal::new(None),
        error: RwSignal::new(None),
        fetched_at_ms: RwSignal::new(js_sys::Date::now()),
        now_ms: RwSignal::new(js_sys::Date::now()),
        loading: RwSignal::new(false),
    };
    board.reload();
    // The scoreboard moves slowly: re-read on its own poll while the page is visible, and keep ages running.
    let ticks = RwSignal::new(0u32);
    let _ = set_interval(
        move || {
            board.now_ms.set(js_sys::Date::now());
            let every = board
                .view
                .with_untracked(|v| {
                    v.as_ref()
                        .and_then(|v| v.refresh.as_ref())
                        .map(|r| r.poll_seconds)
                })
                .unwrap_or(60)
                .max(15);
            let t = ticks.get_untracked() + 15;
            if t >= every {
                ticks.set(0);
                if crate::on_screen() {
                    board.reload();
                }
            } else {
                ticks.set(t);
            }
        },
        Duration::from_secs(15),
    );

    // Shown again by the shell, or back online: read at once.
    for event in [crate::SHOWN_EVENT, "online"] {
        let again = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
            if crate::on_screen() {
                board.reload();
            }
        });
        let _ = window().add_event_listener_with_callback(
            event,
            wasm_bindgen::JsCast::unchecked_ref(again.as_ref()),
        );
        again.forget();
    }
    let chosen = RwSignal::new(stored_use());
    let current_use = Memo::new(move |_| {
        board.view.with(|v| {
            let v = v.as_ref()?;
            let pick = chosen.get();
            v.uses
                .iter()
                .find(|u| Some(&u.id) == pick.as_ref())
                .or_else(|| v.uses.first())
                .cloned()
        })
    });
    let measures = Memo::new(move |_| {
        board.view.with(|v| {
            v.as_ref()
                .map(|v| {
                    v.catalog
                        .measures
                        .iter()
                        .map(|m| (m.id.clone(), m.clone()))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default()
        })
    });
    let banner = move || {
        let error = board.error.get()?;
        let old = board
            .view
            .with(|v| v.is_some())
            .then(|| {
                format!(
                    " Showing the reading from {}.",
                    fmt::age(board.elapsed_seconds())
                )
            })
            .unwrap_or_default();
        Some(
            view! { <div class="banner" role="status" data-tone="crit">{error.sentence()}{old}</div> },
        )
    };

    view! {
        <main class="page scoreboard" data-embedded=embedded.then_some("1")>
            <Show when=move || !embedded>
                <header class="page-head"><h1>"Model Scoreboard"</h1></header>
            </Show>
            {banner}
            <Show
                when=move || board.view.with(|v| v.is_some())
                fallback=move || view! {
                    <p class="empty">{move || if board.error.with(|e| e.is_some()) { "" } else { "Reading the scoreboard..." }}</p>
                }
            >
                <p class="sb-intro">
                    {move || {
                        let min = board.view.with(|v| v.as_ref().and_then(|v| v.catalog.min_runs)).unwrap_or(5);
                        format!("Real work and planned benchmarks are kept apart. A model is ranked only after {min} runs of one kind of task; every number says how many runs are behind it.")
                    }}
                </p>
                <Sources board=board/>
                <div class="sb-uses" role="tablist" aria-label="What the models are for">
                    {move || board.view.with(|v| v.as_ref().map(|v| v.uses.clone()).unwrap_or_default()).into_iter().map(|u| {
                        let id = u.id.clone();
                        let selected = Signal::derive({ let id = id.clone(); move || current_use.with(|c| c.as_ref().map(|c| &c.id) == Some(&id)) });
                        view! {
                            <button role="tab" class="chip-btn" aria-selected=move || selected.get().to_string()
                                on:click=move |_| { store_use(&id); chosen.set(Some(id.clone())); }>{u.label.clone()}</button>
                        }
                    }).collect_view()}
                </div>
                {move || current_use.get().map(|u| view! {
                    <section class="sb-use">
                        {u.description.clone().filter(|d| !d.is_empty()).map(|d| view! { <p class="muted sb-use-desc">{d}</p> })}
                        {u.task_types.into_iter().map(|t| view! { <TaskTypeView task=t measures=measures/> }).collect_view()}
                    </section>
                })}
                <Recent board=board/>
            </Show>
        </main>
    }
}

#[component]
fn Sources(board: Board) -> impl IntoView {
    move || {
        let sources = board
            .view
            .with(|v| v.as_ref().map(|v| v.sources.clone()).unwrap_or_default());
        let elapsed = board.elapsed_seconds();
        view! {
            <ul class="sb-sources">
                {sources.into_iter().map(|s| {
                    let age = fmt::live_age(s.age_seconds, elapsed).map(fmt::age);
                    let ok = s.state == "ok";
                    view! {
                        <li data-state=s.state.clone()>
                            <span class="dot" aria-hidden="true"></span>
                            <strong>{s.label.clone()}</strong>
                            {age.map(|a| format!(" · {a}"))}
                            {(!ok || !s.detail.is_empty()).then(|| view! { <span class="sb-source-detail">{format!(" {}", s.detail)}</span> })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        }
    }
}

#[component]
fn TaskTypeView(task: TaskType, measures: Memo<BTreeMap<String, CatalogMeasure>>) -> impl IntoView {
    let (ranked, thin): (Vec<Row>, Vec<Row>) = task.rows.iter().cloned().partition(|r| r.ranked);
    let thin_count = thin.len();
    let ids = task.measures.clone();
    let ids_thin = ids.clone();
    let empty = task.rows.is_empty();
    view! {
        <section class="panel sb-task" data-task=task.id.clone()>
            <header class="panel-head">
                <h2>{task.label.clone()}</h2>
                <span class="src">{format!("per {}", task.unit)}</span>
            </header>
            {task.description.clone().filter(|d| !d.is_empty()).map(|d| view! { <p class="muted sb-task-desc">{d}</p> })}
            {empty.then(|| view! { <p class="empty">"No runs of this kind yet."</p> })}
            <div class="sb-rows">
                {ranked.into_iter().map(|r| view! { <RowView row=r ids=ids.clone() measures=measures/> }).collect_view()}
            </div>
            {(thin_count > 0).then(|| view! {
                <details class="sb-thin">
                    <summary>{format!("Too little data yet ({thin_count})")}</summary>
                    <div class="sb-rows">
                        {thin.into_iter().map(|r| view! { <RowView row=r ids=ids_thin.clone() measures=measures/> }).collect_view()}
                    </div>
                </details>
            })}
        </section>
    }
}

#[component]
fn RowView(
    row: Row,
    ids: Vec<String>,
    measures: Memo<BTreeMap<String, CatalogMeasure>>,
) -> impl IntoView {
    let place = row_place(&row);
    let columns: Vec<Column> = row
        .observed
        .clone()
        .into_iter()
        .chain(row.benchmarks.clone())
        .collect();
    view! {
        <article class="sb-row" data-verdict=row.verdict.clone()>
            <div class="sb-row-head">
                {row.rank.filter(|_| row.ranked).map(|r| view! { <span class="sb-rank">{format!("#{r}")}</span> })}
                <span class="sb-model">{row.label.clone()}</span>
                <span class="chip" data-tone=verdict_tone(&row.verdict)>{row.verdict_label.clone()}</span>
            </div>
            {(!place.is_empty()).then(|| view! { <p class="sb-place muted">{place}</p> })}
            <p class="sb-why">{row.why.clone()}</p>
            {columns.into_iter().map(|c| view! { <ColumnView column=c ids=ids.clone() measures=measures/> }).collect_view()}
        </article>
    }
}

#[component]
fn ColumnView(
    column: Column,
    ids: Vec<String>,
    measures: Memo<BTreeMap<String, CatalogMeasure>>,
) -> impl IntoView {
    let title = if column.source == "benchmark" {
        format!(
            "Benchmark: {}",
            column
                .benchmark_label
                .clone()
                .unwrap_or_else(|| "planned".into())
        )
    } else {
        "Observed in real work".to_string()
    };
    let mut facts = vec![format!("n = {}", column.n)];
    if column.reviews > 0 {
        facts.push(format!("{} reviewed", column.reviews));
    }
    if column.imported > 0 {
        facts.push(format!("{} imported", column.imported));
    }
    let runs = column.runs.clone();
    let run_count = runs.len();
    let runs_label = if (run_count as u32) < column.n {
        format!("Latest {run_count} of {} runs", column.n)
    } else {
        format!("Runs ({run_count})")
    };
    view! {
        <section class="sb-col" data-source=column.source.clone()>
            <h3>
                {title}
                <span class="muted">{format!(" · {}", facts.join(" · "))}</span>
                {column.preliminary.then(|| view! { <span class="chip" data-tone="warn">"Preliminary"</span> })}
            </h3>
            <dl class="sb-measures">
                {ids.into_iter().filter_map(|id| {
                    let m = column.measures.get(&id)?.clone();
                    let info = measures.with_untracked(|all| all.get(&id).cloned());
                    let label = info.as_ref().map(|i| i.label.clone()).unwrap_or_else(|| id.clone());
                    let hint = info.map(|i| format!("{} ({} is better)", i.description, i.better)).unwrap_or_default();
                    let (main, detail) = measure_words(&m);
                    Some(view! {
                        <div title=hint data-empty=(m.value.is_none() && m.estimate.as_ref().and_then(|e| e.credits.as_ref()).and_then(|c| c.value).is_none()).then_some("1")>
                            <dt>{label}</dt>
                            <dd>
                                <span class="sb-value">{main}</span>
                                {m.preliminary.then(|| view! { <span class="chip" data-tone="warn">"Preliminary"</span> })}
                                <span class="sb-detail">{detail}</span>
                            </dd>
                        </div>
                    })
                }).collect_view()}
            </dl>
            {(!column.flags.is_empty()).then(|| view! {
                <ul class="sb-flags">
                    {column.flags.iter().map(|f| view! {
                        <li><strong>{flag_words(&f.kind)}</strong>{format!(": {}", f.detail)}<code>{f.evidence.clone()}</code></li>
                    }).collect_view()}
                </ul>
            })}
            {(run_count > 0).then(|| view! {
                <details class="sb-runs">
                    <summary>{runs_label}</summary>
                    <ul>{runs.into_iter().map(|r| view! { <li><RunLine run=r/></li> }).collect_view()}</ul>
                </details>
            })}
        </section>
    }
}

#[component]
fn RunLine(run: RunSummary) -> impl IntoView {
    let review = run.review.as_ref().map(|r| {
        let mut w = format!("review {}", r.verdict);
        if r.overturned {
            w.push_str(", overturned");
        }
        if r.disputed {
            w.push_str(", disputed");
        }
        w
    });
    let line = [
        Some(fmt::clock(&run.ended_at)).filter(|s| !s.is_empty()),
        Some(run.outcome.clone()).filter(|s| !s.is_empty()),
        run.wall_seconds.map(seconds),
        review,
        (run.flags > 0).then(|| {
            format!(
                "{} flag{}",
                run.flags,
                if run.flags == 1 { "" } else { "s" }
            )
        }),
        run.imported.as_ref().map(|_| "imported".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    view! {
        <div class="sb-run">
            <span class="sb-run-title">{run.task.title.clone()}</span>
            <span class="muted">{line}</span>
            {run.evidence.first().map(|e| view! { <code>{e.path.clone()}</code> })}
        </div>
    }
}

#[component]
fn Recent(board: Board) -> impl IntoView {
    let runs = move || {
        board
            .view
            .with(|v| v.as_ref().map(|v| v.recent.clone()).unwrap_or_default())
    };
    view! {
        <section class="panel sb-recent">
            <header class="panel-head"><h2>"Latest runs"</h2></header>
            {move || {
                let list = runs();
                if list.is_empty() {
                    view! { <p class="empty">"No runs reported yet."</p> }.into_any()
                } else {
                    view! {
                        <ul class="sb-recent-list">
                            {list.into_iter().map(|r| {
                                let who = [r.label.clone().or(r.model.clone()), r.effort.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ");
                                let kind = if r.source == "benchmark" { "benchmark" } else { "observed" };
                                view! {
                                    <li>
                                        <span class="chip" data-tone=if kind == "benchmark" { "info" } else { "muted" }>{kind}</span>
                                        <strong>{who}</strong>
                                        <span class="muted">{format!(" · {}", r.task.kind.clone().unwrap_or_else(|| "no task type".into()))}</span>
                                        <RunLine run=r/>
                                    </li>
                                }
                            }).collect_view()}
                        </ul>
                    }.into_any()
                }
            }}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_example_view_reads() {
        let text = include_str!("../fixtures/scoreboard-view.json");
        let view: ScoreboardView = serde_json::from_str(text).expect("example parses");
        assert_eq!(view.version, SCOREBOARD_VIEW_VERSION);
        assert_eq!(view.uses.len(), 3);
        let scout = &view.uses[0].task_types[0];
        assert_eq!(scout.id, "scout");
        let row = &scout.rows[0];
        assert!(row.ranked && row.observed.is_some());
        assert!(!view.catalog.measures.is_empty());
        assert!(!view.recent.is_empty());
    }

    #[test]
    fn values_read_plainly_and_missing_stays_missing() {
        let share_m = MeasureValue {
            value: Some(0.9291),
            unit: "share".into(),
            n: 5,
            low: Some(0.9105),
            high: Some(0.9441),
            interval: Some("wilson90".into()),
            ..Default::default()
        };
        assert_eq!(
            measure_words(&share_m),
            ("93%".into(), "n = 5 · 90%: 91%–94%".into())
        );
        let speed = MeasureValue {
            value: Some(4.866),
            unit: "s/item".into(),
            n: 5,
            low: Some(4.85),
            high: Some(4.882),
            interval: Some("range".into()),
            ..Default::default()
        };
        assert_eq!(measure_words(&speed).0, "4.9 s per item");
        let none = MeasureValue {
            unit: "credits/item".into(),
            ..Default::default()
        };
        assert_eq!(measure_words(&none), ("No data".into(), "n = 0".into()));
        let estimated = MeasureValue {
            unit: "credits/item".into(),
            n: 3,
            estimate: Some(Estimate {
                credits: Some(Rate {
                    value: Some(1.25),
                    unit: "credits/item".into(),
                    n: 3,
                }),
            }),
            ..Default::default()
        };
        assert_eq!(measure_words(&estimated).1, "estimate · n = 3");
    }
}
