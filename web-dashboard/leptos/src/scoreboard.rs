//! The Model Scoreboard (docs/scoreboard.md, view v1): which models to use for which work.
//!
//! The server computes every value, interval, rank and verdict from the agent reports and the catalog; this page only
//! lays them out. Observed work and planned benchmarks stay in separate columns, every value shows how many runs are
//! behind it, and a row without enough runs is shown apart, unranked.

use crate::api::{FetchError, Link, Refresh, Run, nullable, send};
use crate::fmt;
use crate::status::{SheetGrab, sheet_drag_style};
use crate::ui::*;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

pub const SCOREBOARD_VIEW_VERSION: u32 = 1;
const VIEW_KEY: &str = "aac.scoreboard.view.v1";

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
    /// Models by effort with verdicts per job (CONTRACT-spend-bymodel.md). Absent until the server sends it: the page
    /// says so and invents nothing.
    pub by_model: Option<ByModel>,
}

/// The by-model view: one row per model at one effort. Every word and number is decided on the server.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ByModel {
    pub default_view: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub intro: String,
    #[serde(deserialize_with = "nullable")]
    pub read_note: Vec<Run>,
    pub read_link: Option<ReadLink>,
    #[serde(deserialize_with = "nullable")]
    pub band_summary: String,
    #[serde(deserialize_with = "nullable")]
    pub views: Vec<ModelsView>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReadLink {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelsView {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub short: String,
    #[serde(deserialize_with = "nullable")]
    pub meta: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub picks: Vec<Pick>,
    #[serde(deserialize_with = "nullable")]
    pub jobs: Vec<Job>,
    #[serde(deserialize_with = "nullable")]
    pub models: Vec<ModelRow>,
    #[serde(deserialize_with = "nullable")]
    pub foot: Vec<Run>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Pick {
    #[serde(deserialize_with = "nullable")]
    pub job_label: String,
    pub model: Option<PickModel>,
    pub none_label: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub why: Vec<Run>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PickModel {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub effort: Option<String>,
    pub mark: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Job {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelRow {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub effort_text: String,
    #[serde(deserialize_with = "nullable")]
    pub runtime: String,
    pub mark: Option<String>,
    pub runs: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub head: String,
    #[serde(deserialize_with = "nullable")]
    pub head_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub sheet_head: String,
    #[serde(deserialize_with = "nullable")]
    pub cells: BTreeMap<String, Option<ModelCell>>,
    #[serde(deserialize_with = "nullable")]
    pub report: Vec<JobReport>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelCell {
    /// recommended, fine, avoid or not-enough-data.
    #[serde(deserialize_with = "nullable")]
    pub verdict: String,
    #[serde(deserialize_with = "nullable")]
    pub word: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JobReport {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub verdict: String,
    #[serde(deserialize_with = "nullable")]
    pub word: String,
    #[serde(deserialize_with = "nullable")]
    pub basis: String,
    #[serde(deserialize_with = "nullable")]
    pub measures: Vec<JobMeasure>,
    #[serde(deserialize_with = "nullable")]
    pub compare: Vec<Run>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JobMeasure {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    pub range: Option<MeasureRange>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeasureRange {
    pub low: f64,
    pub high: f64,
    pub value: f64,
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
    /// Counts the server didn't send stay unknown, never 0.
    pub n: Option<u32>,
    pub imported: Option<u32>,
    pub reviews: Option<u32>,
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
    pub n: Option<u32>,
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
    pub n: Option<u32>,
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
    /// The harness the run used (claude-code, codex ...): the middle part of a row's key.
    pub runtime: Option<String>,
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
        return Err(crate::api::answer_error(status, &text));
    }
    let view: ScoreboardView = serde_json::from_str(&text).map_err(|_| FetchError::Malformed)?;
    if view.version != SCOREBOARD_VIEW_VERSION {
        return Err(FetchError::Version(view.version));
    }
    Ok(view)
}

// ---- Page --------------------------------------------------------------------------------------------------------

fn stored_view() -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(VIEW_KEY).ok().flatten())
}

fn store_view(id: &str) {
    if let Some(s) = window().local_storage().ok().flatten() {
        let _ = s.set_item(VIEW_KEY, id);
    }
}

fn verdict_class(verdict: &str) -> &'static str {
    match verdict {
        "recommended" => "v-rec",
        "fine" => "v-fine",
        "avoid" => "v-avoid",
        _ => "v-nd",
    }
}

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
        // A page the frame has unmounted (at sign-out) reads nothing: its signals are gone.
        if self.loading.try_get_untracked().unwrap_or(true) {
            return;
        }
        self.loading.set(true);
        spawn_local(async move {
            let result = fetch_scoreboard().await;
            if self.loading.try_get_untracked().is_none() {
                return;
            }
            crate::ui::reconcile_overlay_focus(|| {
                match result {
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
    // In the app frame a read the server refused (the session ended) brings the frame's sign-in, as Status does.
    // Inside the classic shell (`embedded`) the shell's own sign-in follows the session, as before.
    if !embedded {
        Effect::new(move |_| {
            if board
                .error
                .with(|e| matches!(e, Some(FetchError::SignedOut)))
            {
                crate::session::dashboard_signed_out("expired", None);
            }
        });
    }
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
                if crate::page_on_screen(crate::Page::Scoreboard)
                    && !crate::live::refused(&board.error)
                {
                    board.reload();
                }
            } else {
                ticks.set(t);
            }
        },
        Duration::from_secs(15),
    );
    // Shown again by the shell, or back online: read at once (a refusal the server explains still doesn't), until
    // the page is unmounted (owned.rs).
    for event in [crate::SHOWN_EVENT, "online"] {
        crate::owned::listen(&window(), event, move || {
            if crate::page_on_screen(crate::Page::Scoreboard) && !crate::live::refused(&board.error)
            {
                board.reload();
            }
        });
    }
    let by_model = Memo::new(move |_| {
        board
            .view
            .with(|v| v.as_ref().and_then(|v| v.by_model.clone()))
    });
    let chosen = RwSignal::new(stored_view());
    let current = Memo::new(move |_| {
        by_model.with(|b| {
            let b = b.as_ref()?;
            let pick = chosen.get().or_else(|| b.default_view.clone());
            b.views
                .iter()
                .find(|v| Some(&v.id) == pick.as_ref())
                .or(b.views.first())
                .cloned()
        })
    });
    let selected_id = Signal::derive(move || {
        current.with(|c| c.as_ref().map(|v| v.id.clone()).unwrap_or_default())
    });
    let select_view = Callback::new(move |id: String| {
        store_view(&id);
        chosen.set(Some(id));
    });
    let info = ChromeInfo {
        updated: Signal::derive(move || {
            if board.view.with(|v| v.is_none()) {
                String::new()
            } else {
                fmt::age(board.elapsed_seconds())
            }
        }),
        live: Signal::derive(move || board.error.with(|e| e.is_none())),
        summary: Signal::derive(move || {
            let text = by_model.with(|b| {
                b.as_ref()
                    .map(|b| b.band_summary.clone())
                    .unwrap_or_default()
            });
            vec![Run {
                text,
                ..Default::default()
            }]
        }),
        strip: None,
        refresh: Callback::new(move |_| board.reload()),
        lead: None,
    };
    let banner = move || {
        let error = board.error.get()?;
        let old = if board.view.with(|v| v.is_some()) {
            format!(
                " Showing the reading from {}.",
                fmt::age(board.elapsed_seconds())
            )
        } else {
            String::new()
        };
        Some(view! { <p class="banner" role="status">{error.sentence()}{old}</p> })
    };
    let sources_open = RwSignal::new(false);
    // The sheet holds the model's key; what it shows comes from the current view, so it follows a refresh.
    let sheet = RwSignal::new(None::<String>);
    let open_sheet = Callback::new(move |m: ModelRow| sheet.set(Some(m.key)));
    let close_sheet = Callback::new(move |_: ()| sheet.set(None));
    if let Some(frame) = use_context::<crate::frame::FrameCtx>() {
        Effect::new(move |_| {
            if frame.route.get() != crate::Page::Scoreboard {
                sheet.set(None);
            }
        });
    }
    let sheet_key = Memo::new(move |_| sheet.get());
    let sheet_model = Memo::new(move |_| {
        let key = sheet.get()?;
        current.with(|v| {
            v.as_ref()
                .and_then(|v| v.models.iter().find(|m| m.key == key).cloned())
        })
    });
    // the model left the view: the sheet closes (outside any render)
    Effect::new(move |_| {
        if sheet_key.with(|k| k.is_some()) && sheet_model.with(|m| m.is_none()) {
            sheet.set(None);
        }
    });
    let selected = RwSignal::new(None::<String>);
    let on_row = Callback::new(move |key: String| selected.set(Some(key)));
    view! {
        <PageShell page=crate::Page::Scoreboard embedded=embedded info=info>
            <div class="page" data-stale=move || board.error.with(|e| e.is_some()).then_some("1")>
                <div class="page-head sb-head">
                    <div class="sb-title">
                        <h1 class="page-title">"Model Scoreboard"</h1>
                        {move || by_model.get().map(|b| {
                            let options: Vec<(String, AnyView)> = b.views.iter().map(|v| (v.id.clone(), view! {
                                <span class="lbl-long">{v.label.clone()}</span><span class="lbl-short">{v.short.clone()}</span>
                            }.into_any())).collect();
                            view! { <SwitchSeg options=options selected=selected_id on_select=select_view/> }
                        })}
                    </div>
                    <p class="page-sub">{move || by_model.with(|b| b.as_ref().map(|b| b.intro.clone())).unwrap_or_default()}</p>
                </div>
                {banner}
                <Show
                    when=move || board.view.with(|v| v.is_some())
                    fallback=move || view! {
                        <p class="empty">{move || if board.error.with(|e| e.is_some()) { "" } else { "Reading the scoreboard..." }}</p>
                    }
                >
                    <div class="sb-stack">
                        {move || match current.get() {
                            None => view! { <section class="plate"><p class="sb-unavail">"The by-model scoreboard isn't available from this server yet. Every model's runs are still recorded."</p></section> }.into_any(),
                            Some(v) => view! { <ModelsSections view=v selected=selected on_row=on_row on_sheet=open_sheet board=board/> }.into_any(),
                        }}
                        <p class="sb-read">
                            {move || by_model.get().map(|b| {
                                let link = b.read_link.clone().map(|l| l.label);
                                view! {
                                    {runs_view(b.read_note)}" "
                                    {link.map(|l| view! { <a class="link" href="#sources" on:click=move |e| { e.prevent_default(); sources_open.update(|o| *o = !*o); }>{l}</a> })}
                                }
                            })}
                        </p>
                        <SourcesList board=board open=sources_open/>
                    </div>
                </Show>
                // mounted once per key: a refresh redraws its contents, not the sheet (drag and disclosure stay)
                {move || sheet_key.get().map(|_| view! { <ModelSheet model=sheet_model on_close=close_sheet board=board/> })}
            </div>
        </PageShell>
    }
}

/// The title-line switch. Its buttons carry a long and a short label; the page shows the one that fits.
#[component]
fn SwitchSeg(
    options: Vec<(String, AnyView)>,
    selected: Signal<String>,
    on_select: Callback<String>,
) -> impl IntoView {
    view! {
        <div class="seg" role="tablist" aria-label="Whose verdicts">
            {options.into_iter().map(|(id, text)| {
                let current = id.clone();
                let click = id.clone();
                view! {
                    <button role="tab" class:on=move || selected.get() == current
                        aria-selected={let id = id.clone(); move || (selected.get() == id).to_string()}
                        on:click=move |_| on_select.run(click.clone())>{text}</button>
                }
            }).collect_view()}
        </div>
    }
}

fn pick_card(pick: Pick) -> impl IntoView {
    let none = pick.model.is_none();
    view! {
        <div class="pick" class:none=none>
            <span class="cap">{pick.job_label.clone()}</span>
            {match pick.model.clone() {
                Some(m) => view! {
                    <div class="pick-model">
                        <Mark name=m.mark.clone() size=18/>
                        {m.label.clone()}
                        {m.effort.clone().map(|e| view! { <span class="eff">{format!(" · {e}")}</span> })}
                    </div>
                }.into_any(),
                None => view! { <div class="pick-model">{pick.none_label.clone().unwrap_or_else(|| "No pick yet".into())}</div> }.into_any(),
            }}
            <p class="pick-why">{runs_view(pick.why.clone())}</p>
        </div>
    }
}

fn model_name(m: &ModelRow, sub: bool, chevron: bool) -> impl IntoView + use<> {
    let eff = m.effort_text.clone();
    let runtime = m.runtime.clone();
    view! {
        <div class="mname">
            <Mark name=m.mark.clone()/>
            <div>
                <div class="t">{m.label.clone()}<span class="eff">{format!(" · {eff}")}</span></div>
                {sub.then(|| view! { <div class="r">{runtime}</div> })}
            </div>
            {chevron.then(|| view! { <Icon name="chev-right" class="chev"/> })}
        </div>
    }
}

/// A verdict cell's note: each piece stays whole, breaks fall only between them.
fn sub_chunks(sub: &[String]) -> impl IntoView + use<> {
    let last = sub.len().saturating_sub(1);
    let parts: Vec<(String, bool)> = sub
        .iter()
        .enumerate()
        .map(|(i, s)| (s.clone(), i < last))
        .collect();
    parts
        .into_iter()
        .map(|(text, more)| {
            view! {
                <span class="nw">{text}{more.then_some(" ·")}</span>{more.then_some(" ")}
            }
        })
        .collect_view()
}

#[component]
fn ModelsSections(
    view: ModelsView,
    selected: RwSignal<Option<String>>,
    on_row: Callback<String>,
    on_sheet: Callback<ModelRow>,
    board: Board,
) -> impl IntoView {
    let how_open = RwSignal::new(false);
    let runs_open = RwSignal::new(false);
    let measures = move || {
        board.view.with(|v| {
            v.as_ref()
                .map(|v| v.catalog.measures.clone())
                .unwrap_or_default()
        })
    };
    let recent = move || {
        board
            .view
            .with(|v| v.as_ref().map(|v| v.recent.clone()).unwrap_or_default())
    };
    let jobs = view.jobs.clone();
    let first_key = view.models.first().map(|m| m.key.clone());
    let models = StoredValue::new(view.models.clone());
    let selected_key = Signal::derive({
        let first = first_key.clone();
        move || {
            selected
                .get()
                .filter(|k| models.with_value(|list| list.iter().any(|m| &m.key == k)))
                .or_else(|| first.clone())
        }
    });
    let meta = view.meta.clone();
    let foot = view.foot.clone();
    let detail = move || {
        let key = selected_key.get()?;
        models.with_value(|list| list.iter().find(|m| m.key == key).cloned())
    };
    view! {
        <section class="plate">
            <div class="plate-head"><h2>"Best pick for each job"</h2><span class="plate-meta">"From real work unless it says benchmark"</span></div>
            <div class="picks">{view.picks.clone().into_iter().map(pick_card).collect_view()}</div>
        </section>
        <section class="plate">
            <div class="plate-head">
                <h2>"By model and effort"</h2>
                <span class="plate-meta">{runs_view(meta)}</span>
                <div class="plate-tools only-d"><Disclosure open=how_open label="How verdicts work".to_string()/></div>
            </div>
            <div class="sb-how-toggle not-d"><Disclosure open=how_open label="How verdicts work".to_string()/></div>
            <Show when=move || how_open.get()>
                <ul class="sb-how">
                    {move || measures().into_iter().map(|m| {
                        let better = if m.better == "lower" { "lower is better" } else { "higher is better" };
                        view! { <li><b>{m.label.clone()}</b>{format!(": {} ({better}).", m.description.trim_end_matches('.'))}</li> }
                    }).collect_view()}
                </ul>
            </Show>
            <div class="sb-matrix">
                <table>
                    <thead>
                        <tr>
                            <th><span class="cap">"Model · effort"</span></th>
                            {jobs.iter().map(|j| view! { <th><span class="cap">{j.label.clone()}</span></th> }).collect_view()}
                            <th style="text-align:right"><span class="cap">"Runs"</span></th>
                        </tr>
                    </thead>
                    <tbody>
                        {view.models.clone().into_iter().map(|m| {
                            let key = m.key.clone();
                            let k2 = m.key.clone();
                            let cells = jobs.iter().map(|j| {
                                match m.cells.get(&j.id).cloned().flatten() {
                                    Some(c) => view! { <td><span class=format!("verdict {}", verdict_class(&c.verdict))>{c.word.clone()}</span><span class="vsub">{sub_chunks(&c.sub)}</span></td> }.into_any(),
                                    None => view! { <td><span class="verdict v-none">"–"</span></td> }.into_any(),
                                }
                            }).collect_view();
                            view! {
                                <tr data-row="1" class:sel=move || selected_key.get().as_ref() == Some(&k2) on:click=move |_| on_row.run(key.clone())>
                                    <td>{model_name(&m, true, false)}</td>
                                    {cells}
                                    <td class="runs-col">{m.runs.map(|r| r.to_string()).unwrap_or_else(|| "–".into())}</td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <div class="sb-models">
                {view.models.clone().into_iter().map(|m| {
                    let open = m.clone();
                    let job_rows = jobs.iter().filter_map(|j| {
                        let c = m.cells.get(&j.id).cloned().flatten()?;
                        Some(view! { <div class="sbm-job"><span class="k">{j.label.clone()}</span><span><span class=verdict_class(&c.verdict)>{c.word.clone()}</span></span></div> })
                    }).collect_view();
                    view! {
                        <button class="sbm" class:sel={let k = m.key.clone(); move || selected_key.get().as_ref() == Some(&k)} on:click=move |_| { on_row.run(open.key.clone()); on_sheet.run(open.clone()); }>
                            {model_name(&m, true, true)}
                            <div class="sbm-jobs">{job_rows}</div>
                        </button>
                    }
                }).collect_view()}
            </div>
            <div class="plate-foot note"><Icon name="info"/><span>{runs_view(foot)}</span></div>
        </section>
        <section class="plate only-wide">
            {move || detail().map(|m| view! {
                <div class="plate-head">
                    <Mark name=m.mark.clone() size=22/>
                    <h2>{format!("{} · {}", m.label, m.effort_text)}</h2>
                    <span class="plate-meta">{m.head.clone()}{runs_view(m.head_runs.clone())}</span>
                    <div class="plate-tools"><button class="link" style="font-size:12.5px" on:click=move |_| runs_open.update(|o| *o = !*o)>"Latest runs of this model and effort"</button></div>
                </div>
                <Show when=move || runs_open.get()>
                    {let key = m.key.clone(); move || runs_list(&recent(), &key)}
                </Show>
                <DetailGrid report=m.report.clone()/>
            })}
        </section>
    }
}

#[component]
fn DetailGrid(report: Vec<JobReport>) -> impl IntoView {
    view! {
        <div class="detail-grid">
            {report.into_iter().map(|j| view! {
                <div class="job">
                    <h3>{j.label.clone()}" "<span class=verdict_class(&j.verdict)>{j.word.clone()}</span></h3>
                    <p class="basis">{j.basis.clone()}</p>
                    {j.measures.iter().map(|m| {
                        let note = m.note.clone();
                        view! {
                            <div class="measure">
                                <div class="mt"><span class="ml">{m.label.clone()}</span><span class="mv">{m.value.clone()}" "<small>{m.sub.clone()}</small></span></div>
                                {m.range.map(|r| view! { <RangeBar low=r.low high=r.high value=r.value/> })}
                                {(!note.is_empty()).then(|| view! { <p class="mn">{note}</p> })}
                            </div>
                        }
                    }).collect_view()}
                    {(!j.compare.is_empty()).then(|| view! { <p class="cmp">{runs_view(j.compare.clone())}</p> })}
                </div>
            }).collect_view()}
        </div>
    }
}

/// The newest runs of one model and effort (the row's key is "model|runtime|effort"), with their evidence.
/// The runs of one row: the row's key is the server's `model|runtime|effort`, a missing part read as "" (as the
/// server builds it), so a row with no effort never lists another effort's runs.
fn runs_of(recent: &[RunSummary], key: &str) -> Vec<RunSummary> {
    let mut parts = key.splitn(3, '|');
    let (model, runtime, effort) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
    );
    recent
        .iter()
        .filter(|r| {
            r.model.as_deref().unwrap_or("") == model
                && r.runtime.as_deref().unwrap_or("") == runtime
                && r.effort.as_deref().unwrap_or("") == effort
        })
        .take(20)
        .cloned()
        .collect()
}

fn runs_list(recent: &[RunSummary], key: &str) -> AnyView {
    let runs = runs_of(recent, key);
    if runs.is_empty() {
        // recent is the newest runs of every model, not this row's history
        return view! { <p class="sb-how q3">{format!("None of the {} newest runs is this model and effort.", recent.len())}</p> }
            .into_any();
    }
    view! {
        <ul class="sb-how">
            {runs.into_iter().map(|r| {
                let meta = [Some(if r.source == "benchmark" { "benchmark".to_string() } else { "real work".to_string() }), Some(fmt::clock(&r.ended_at)).filter(|s| !s.is_empty()), Some(r.outcome.clone()).filter(|s| !s.is_empty())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                view! { <li><b>{r.task.title.clone()}</b>{format!(" · {meta}")}{r.evidence.first().map(|e| view! { <code class="sb-ev">{e.path.clone()}</code> })}</li> }
            }).collect_view()}
        </ul>
    }
    .into_any()
}

#[component]
fn ModelSheet(
    model: Memo<Option<ModelRow>>,
    on_close: Callback<()>,
    board: Board,
) -> impl IntoView {
    let offset = RwSignal::new(0.0f64);
    let runs_open = RwSignal::new(false);
    let recent = move || {
        board
            .view
            .with(|v| v.as_ref().map(|v| v.recent.clone()).unwrap_or_default())
    };
    view! {
        <OverlayPortal on_close=on_close>
        <div class="sheet-host" role="dialog" aria-modal="true">
            <button class="sheet-scrim" aria-label="Close" on:click=move |_| on_close.run(())></button>
            <div class="sheet" style=move || sheet_drag_style(offset.get())>
                // The grabber and the close button stay at the top while the sheet scrolls (pkg P recheck N2).
                <div class="sheet-top">
                    <SheetGrab offset=offset on_close=on_close/>
                    <button class="icon-btn bare sheet-close" aria-label="Close" on:click=move |_| on_close.run(())><Icon name="close"/></button>
                </div>
                {move || model.get().map(|m| view! {
                    <div class="sheet-title">
                        <Mark name=m.mark.clone() size=24/>
                        <div><div class="t">{format!("{} · {}", m.label, m.effort_text)}</div><div class="s">{m.sheet_head.clone()}</div></div>
                    </div>
                    <DetailGrid report=m.report/>
                })}
                // the desktop detail's runs, here too: the phone shows no detail plate
                <div class="sb-sheet-runs"><Disclosure open=runs_open label="Latest runs of this model and effort".to_string()/></div>
                <Show when=move || runs_open.get()>
                    {move || model.with(|m| m.as_ref().map(|m| m.key.clone())).map(|key| runs_list(&recent(), &key))}
                </Show>
            </div>
        </div>
        </OverlayPortal>
    }
}

#[component]
fn SourcesList(board: Board, open: RwSignal<bool>) -> impl IntoView {
    view! {
        <ul class="sb-sources" id="sources" hidden=move || !open.get()>
            {move || board.view.with(|v| v.as_ref().map(|v| v.sources.clone()).unwrap_or_default()).into_iter().map(|s| {
                let age = fmt::live_age(s.age_seconds, board.elapsed_seconds()).map(fmt::age);
                view! { <li><b>{s.label.clone()}</b>{age.map(|a| format!(" · {a}"))}{format!(": {}", s.detail)}</li> }
            }).collect_view()}
        </ul>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, model: &str, runtime: Option<&str>, effort: Option<&str>) -> RunSummary {
        RunSummary {
            id: id.into(),
            model: Some(model.into()),
            runtime: runtime.map(String::from),
            effort: effort.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn a_rows_runs_match_model_runtime_and_effort_exactly() {
        let recent = vec![
            run(
                "a",
                "anthropic/claude-opus-5-5",
                Some("claude-code"),
                Some("xhigh"),
            ),
            run(
                "b",
                "anthropic/claude-opus-5-5",
                Some("codex"),
                Some("xhigh"),
            ),
            run(
                "c",
                "anthropic/claude-opus-5-5",
                Some("claude-code"),
                Some("high"),
            ),
            run("d", "anthropic/claude-opus-5-5", Some("claude-code"), None),
        ];
        let ids = |key: &str| {
            runs_of(&recent, key)
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids("anthropic/claude-opus-5-5|claude-code|xhigh"),
            vec!["a"]
        );
        // another runtime is another row
        assert_eq!(ids("anthropic/claude-opus-5-5|codex|xhigh"), vec!["b"]);
        // a row with no effort lists only the runs with none, never every effort
        assert_eq!(ids("anthropic/claude-opus-5-5|claude-code|"), vec!["d"]);
        assert!(ids("zai/glm-5.3|zcode|max").is_empty());
    }

    #[test]
    fn the_example_view_reads() {
        let text = include_str!("../fixtures/scoreboard-view.json");
        let view: ScoreboardView = serde_json::from_str(text).expect("example parses");
        assert_eq!(view.version, SCOREBOARD_VIEW_VERSION);
        assert_eq!(view.uses.len(), 3);
        assert!(view.by_model.is_none());
    }

    #[test]
    fn by_model_is_read_and_a_cells_null_stays_empty() {
        let text = include_str!("../fixtures/scoreboard-by-model.json");
        let by: ByModel = serde_json::from_str(text).expect("by-model parses");
        assert_eq!(by.default_view.as_deref(), Some("pipeline"));
        let pipeline = &by.views[0];
        // Package M's regenerated example: rows below the review bar are never ranked, so the
        // pipeline keeps three jobs, and a pass rate names its first-try count (B6).
        assert_eq!(pipeline.picks.len(), 3);
        let opus = &pipeline.models[0];
        assert_eq!(opus.label, "Opus 5.5");
        assert_eq!(opus.effort_text, "xhigh");
        assert!(opus.cells.contains_key("plan"));
        assert_eq!(opus.cells["plan"], None);
        let cell = opus.cells["worknode-build"].clone().unwrap();
        assert_eq!(cell.word, "Recommended");
        assert_eq!(
            cell.sub,
            vec![
                "7 runs".to_string(),
                "passed review 6 of 6, 5 on the first try".to_string()
            ]
        );
    }

    #[test]
    fn verdicts_are_words_with_one_class_each() {
        assert_eq!(verdict_class("recommended"), "v-rec");
        assert_eq!(verdict_class("avoid"), "v-avoid");
        assert_eq!(verdict_class("not-enough-data"), "v-nd");
    }
}
