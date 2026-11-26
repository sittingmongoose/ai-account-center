//! Update apps, redesigned (owner, 2026-11-10: "the update card is super long and after other cards"): a compact table
//! with one row per app and a column per computer, first on the page. The head line and every result word come from
//! `updateResultsView` (accounts-view.mjs); this only arranges them. A row opens to each computer's details (the
//! message and versions the Slint page showed as a tip). The table is open on wide pages and folded to its head line on
//! narrow ones. On a card too narrow for every computer's column to hold its full words (`compact`, measured from the
//! card's width and the number of computers: phones, the Duo, a narrow window) each app's name gets its own line over
//! a line of short computer columns: every head and cell also carries a short form (`short_label`, `tiny_word`,
//! `host_tiny`) that the stylesheet shows there instead of the words, which stay in the cell for screen readers, its
//! tip and the details. The wide table never cuts a word with an ellipsis: a rare longer one wraps between words.

use super::Page;
use super::model::{UpdHost, UpdItem};
use crate::ui::*;
use leptos::prelude::*;

/// Whether a result line is an app's own row (not a computer's state line: current app, waiting, more apps).
pub fn app_row(key: &str) -> bool {
    !(key.ends_with("|current") || key.ends_with("|state") || key.ends_with("|more"))
}

/// One table row: an app and its line on each computer (by the column's index).
#[derive(Clone, Debug, PartialEq)]
pub struct AppRow {
    pub app: String,
    pub cells: Vec<Option<UpdItem>>,
}

/// The apps in the order the computers list them, with each computer's line; the app a computer is working on now
/// lands in that app's row.
pub fn table(hosts: &[UpdHost]) -> Vec<AppRow> {
    let mut rows: Vec<AppRow> = Vec::new();
    for (col, host) in hosts.iter().enumerate() {
        for item in &host.items {
            let current = item.key.ends_with("|current");
            if !(app_row(&item.key) || current) {
                continue;
            }
            let at = match rows.iter().position(|r| r.app == item.app) {
                Some(i) => i,
                None => {
                    rows.push(AppRow {
                        app: item.app.clone(),
                        cells: vec![None; hosts.len()],
                    });
                    rows.len() - 1
                }
            };
            rows[at].cells[col] = Some(item.clone());
        }
    }
    rows
}

/// A computer's own state line (starting, waiting, running more apps, no results), shown under its column head.
pub fn host_state(host: &UpdHost) -> Option<UpdItem> {
    host.items
        .iter()
        .find(|i| !app_row(&i.key) && !i.key.ends_with("|current"))
        .cloned()
}

/// The cell's short state for a result line (`updateResultsView` words): one line in a table cell, the full sentence
/// stays in the cell's tip and the row's details. Unknown words keep their own text.
pub fn short_word(result: &str) -> String {
    let r = result.trim();
    if let Some(version) = r.strip_prefix("Updated to ") {
        return format!("→ {version}");
    }
    let short = match r {
        "Already current" => "current",
        "Not installed" => "not installed",
        "Failed" => "failed",
        "Updated, restart failed" => "restart failed",
        "Quit to finish update" => "quit to finish",
        "Needs action" => "needs action",
        "Codex busy, try later" => "busy, try later",
        "In use, try later" => "in use, try later",
        "Update it in the app" => "update in the app",
        "Update held" => "held",
        "Skipped: cancelled" => "skipped",
        "Updated, model refresh failed" => "updated, model refresh failed",
        "Model refresh failed" => "model refresh failed",
        "Check timed out" | "Timed out" => "timed out",
        "Unknown" | "Unknown result" => "unknown",
        "Running" => "running",
        _ if r.starts_with("Unknown: ") && r.ends_with("not reachable") => "not reachable",
        _ => "",
    };
    if !short.is_empty() {
        return short.to_string();
    }
    // "Checking · 45s", "Downloading": the same words, lower case
    let mut chars = r.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A narrow table's word for a cell: a word or two that fit a column a seventh of a phone wide and wrap only between
/// words. A failure or a step for the user stays words (drawn as a colored chip), never just a mark, and a failure says
/// what failed (a failed restart reads "failed": "restart" alone would read as an instruction; a failed model refresh
/// reads "models failed", deep review 2026-11-26); the full result stays in the cell's label and tip and the row's
/// details. Unknown words keep their own text (wrapped between words, never broken). An update's version is
/// `cell_words`'s to fit.
pub fn tiny_word(result: &str) -> String {
    let r = result.trim();
    if let Some(version) = r.strip_prefix("Updated to ") {
        return format!("→{version}");
    }
    // a running cell: "Checking · 45s" says its step; the time stays in the tip
    let step = r.split(" · ").next().unwrap_or(r);
    let tiny = match step {
        "Already current" => "✓",
        "Updated" => "updated",
        "Not installed" => "absent",
        "Failed" | "Installer failed" => "failed",
        "Updated, restart failed" => "failed",
        "Quit to finish update" => "quit",
        "Needs action" => "action",
        "Codex busy, try later" => "busy",
        "In use, try later" => "in use",
        "Update it in the app" => "in app",
        "Update held" => "held",
        "Skipped: cancelled" => "skipped",
        // the binary moved or not, its model refresh failed; an updated one also says "updated" (`tiny_lead`)
        "Updated, model refresh failed" | "Model refresh failed" => "models failed",
        "Check timed out" | "Timed out" => "timed out",
        "Unknown" | "Unknown result" => "unknown",
        "Sign-in needed" => "sign-in",
        "Blocked by policy" => "policy",
        "Another copy runs" => "other copy",
        "Updated the wrong copy" => "wrong copy",
        "Folder not writable" => "locked",
        "T3 reply lost" => "reply lost",
        "T3 not installed" => "no T3",
        "T3 not ready" => "T3 slow",
        "After T3 restarts" => "pending",
        "Running" => "running",
        "Checking" => "check",
        "Downloading" => "fetch",
        "Updating" => "update",
        _ if r.starts_with("Unknown: ") && r.ends_with("not reachable") => "offline",
        _ => "",
    };
    if tiny.is_empty() {
        short_word(r)
    } else {
        tiny.to_string()
    }
}

/// What a narrow cell says before its chip when the app did update although a later step failed: "updated" (in the
/// updated color) over "models failed", so a moved binary with a failed model refresh never reads like a failed
/// refresh on the version that was already there.
pub fn tiny_lead(result: &str) -> Option<&'static str> {
    (result.trim() == "Updated, model refresh failed").then_some("updated")
}

/// Advance width of `c` in Instrument Sans at weight 600 (an updated cell's weight), in ems: measured in Chrome
/// 2026-11-26 (canvas `measureText` at 1000 px). Anything else, "→" too (it comes from a fallback font), counts as a
/// full em, so a guess errs towards "updated".
fn em_600(c: char) -> f64 {
    let thousandths = match c {
        '0' => 677,
        '1' => 387,
        '2' => 557,
        '3' => 579,
        '4' => 615,
        '5' => 581,
        '6' => 615,
        '7' => 561,
        '8' => 598,
        '9' => 619,
        '.' => 275,
        '-' => 493,
        '+' | '~' => 544,
        '_' => 466,
        ' ' => 193,
        'a' | 'k' => 558,
        'b' | 'd' | 'g' | 'p' | 'q' => 621,
        'c' => 552,
        'e' => 571,
        'f' => 370,
        'h' | 'n' => 612,
        'i' | 'j' | 'l' => 260,
        'm' => 941,
        'o' => 597,
        'r' => 390,
        's' => 494,
        't' => 406,
        'u' => 604,
        'v' | 'y' => 536,
        'w' => 800,
        'x' => 587,
        'z' => 513,
        _ => 1000,
    };
    f64::from(thousandths) / 1000.0
}

/// How wide `text` is drawn at `px` in an updated cell's font.
pub fn text_width(text: &str, px: f64) -> f64 {
    text.chars().map(em_600).sum::<f64>() * px
}

/// The compact table's row padding, column gap and cell font size (app.css `.upd-table.compact`); the wide table's
/// cell font size.
const COMPACT_PAD: f64 = 8.0;
const COMPACT_GAP: f64 = 3.0;
const COMPACT_FONT: f64 = 10.5;
const WIDE_FONT: f64 = 12.0;

/// One computer column's width in a card `width` px wide with `hosts` columns: compact, the row's padding and gaps
/// shared out; wide, the grid's `minmax(130px, 1.3fr)` app column and 16 px sides and 12 px gaps (app.css).
pub fn column_width(width: f64, hosts: usize, compact: bool) -> f64 {
    let h = hosts.max(1) as f64;
    if compact {
        return (width - 2.0 * COMPACT_PAD - COMPACT_GAP * (h - 1.0)) / h;
    }
    let room = width - 32.0 - 12.0 * h;
    let share = room / (1.3 + h);
    if 1.3 * share >= 130.0 {
        share
    } else {
        (room - 130.0) / h
    }
}

/// A cell's two forms: the wide table's words for its `wide` px column and the compact table's for its `tiny` px one.
/// An update names its version only while the whole "→version" fits the column on one line (with a little room for
/// rounding), else it says "updated" (deep review 2026-11-26: a Windows MSIX 26.1113.36452.0 and a T3 nightly 0.1.10-nightly.20261113.4616
/// were cut or ran under the next computer); the exact version stays in the cell's label and tip and the row's details.
pub fn cell_words(result: &str, wide: f64, tiny: f64) -> (String, String) {
    let r = result.trim();
    if let Some(version) = r.strip_prefix("Updated to ") {
        let fit = |text: String, px: f64, col: f64| {
            if text_width(&text, px) * 1.03 + 1.0 <= col {
                text
            } else {
                "updated".to_string()
            }
        };
        return (
            fit(format!("→ {version}"), WIDE_FONT, wide),
            fit(format!("→{version}"), COMPACT_FONT, tiny),
        );
    }
    (short_word(r), tiny_word(r))
}

/// A narrow table's word under a computer's head (its state line): a word or two that wrap only between words.
pub fn host_tiny(state: &UpdItem) -> String {
    match state.app.as_str() {
        "No results from this computer" => "no results".into(),
        "Waiting for its turn" => "waiting".into(),
        "Starting" => "starting".into(),
        "Checking the apps" | "More apps" => "running".into(),
        other => short_word(other),
    }
}

/// Each computer's short head for a narrow table: its first word, or the first three letters of a long one
/// ("Windows" → "Win", "Alexis PC" → "Alexis"). Two computers that would share a head keep their full names.
pub fn short_labels(labels: &[String]) -> Vec<String> {
    let short: Vec<String> = labels
        .iter()
        .map(|label| {
            let first = label.split_whitespace().next().unwrap_or("");
            if first.chars().count() > 6 {
                first.chars().take(3).collect()
            } else {
                first.to_string()
            }
        })
        .collect();
    short
        .iter()
        .zip(labels)
        .map(|(s, full)| {
            if s.is_empty() || short.iter().filter(|o| *o == s).count() > 1 {
                full.clone()
            } else {
                s.clone()
            }
        })
        .collect()
}

/// The narrowest computer column that holds the usual full state words ("installer failed", "quit to finish", the
/// computer's name) on one line at the wide table's 12 px; rarer longer words wrap between words, never inside one.
const WIDE_COLUMN_MIN: f64 = 100.0;

/// Whether a card `width` px wide must use the compact table for `hosts` computer columns: the wide table gives the app
/// column 1.3 shares and each computer one share of what is left after its 16 px sides and 12 px gaps (app.css), and
/// below `WIDE_COLUMN_MIN` a share would cut its words (device recheck 2026-11-25: the open Duo's 640 px card showed
/// "installer …").
pub fn compact_table(width: f64, hosts: usize) -> bool {
    let hosts = hosts as f64;
    (width - 32.0 - 12.0 * hosts) / (1.3 + hosts) < WIDE_COLUMN_MIN
}

/// A cell's tip: its full result and what the computer said.
fn full_words(item: &UpdItem) -> String {
    [item.result.clone(), item.tip.clone()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

fn tone_class(tone: &str) -> &'static str {
    match tone {
        "good" => "w-good",
        "crit" => "w-crit",
        "warn" => "w-warn",
        "run" => "w-acc",
        _ => "q3",
    }
}

fn glyph_id(platform: &str) -> String {
    match platform {
        "apple" => "mac".into(),
        other => other.into(),
    }
}

#[component]
pub fn UpdateCard(page: Page) -> impl IntoView {
    let update = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.update.clone()).unwrap_or_default())
    });
    // open on wide pages, folded to the head line on narrow ones
    let wide = window()
        .inner_width()
        .ok()
        .and_then(|w| w.as_f64())
        .unwrap_or(1280.0)
        >= 900.0;
    let open = RwSignal::new(wide);
    let expanded = RwSignal::new(Vec::<String>::new());
    let rows = Memo::new(move |_| update.with(|u| table(&u.hosts)));
    let hosts = Memo::new(move |_| update.with(|u| u.hosts.clone()));
    // the card's measured width decides the layout (the observer reports before the first paint)
    let card = NodeRef::<leptos::html::Section>::new();
    let card_width = RwSignal::new(0.0f64);
    Effect::new(move |_| {
        if let Some(el) = card.get() {
            crate::ui::charts::observe_size(&el, move |w, _| card_width.set(w.round()));
        }
    });
    let compact = Memo::new(move |_| compact_table(card_width.get(), hosts.with(|h| h.len())));
    // each layout's computer column width, for the words an updated cell can fit
    let columns = Memo::new(move |_| {
        let (w, h) = (card_width.get(), hosts.with(|h| h.len()));
        (column_width(w, h, false), column_width(w, h, true))
    });
    view! {
        <section class="plate ac-card upd" data-probe="update-card" node_ref=card>
            <button type="button" class="upd-head" aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| open.update(|o| *o = !*o)>
                <span class="uh-title"><Icon name="download"/><h2>"Update apps"</h2></span>
                <span class="uh-line tnum">{move || runs_view(update.with(|u| u.head_runs.clone()))}</span>
                {move || update.with(|u| u.shown && !u.hosts.is_empty()).then(|| view! {
                    <span class="uh-chev" class:open=move || open.get()><Icon name="chev-down"/></span>
                })}
            </button>
            <Show when=move || update.with(|u| u.shown && u.running)>
                <div class="upd-cancel">
                    <button type="button" class="btn sm" data-probe="update-cancel"
                        title="Finishes the update running now, then skips the queued apps. Finished updates stay as they are."
                        disabled=move || update.with(|u| u.cancelling)
                        on:click=move |_| page.send("update-cancel", "")>
                        {move || if update.with(|u| u.cancelling) { "Cancelling…" } else { "Cancel" }}
                    </button>
                </div>
            </Show>
            <Show when=move || update.with(|u| u.shown && u.retry && !u.running)>
                <div class="upd-retry">
                    <button type="button" class="btn sm" data-probe="update-retry"
                        title="Runs only the apps that failed or were not checked in the last run, on the computers where they failed."
                        disabled=move || page.busy.get()
                        on:click=move |_| page.send("update-retry", "")>
                        "Retry failed"
                    </button>
                </div>
            </Show>
            <Show when=move || open.get() && update.with(|u| u.shown && !u.hosts.is_empty())>
                <div class="upd-table" class:compact=move || compact.get() role="table" aria-label="Update apps results"
                    style=move || format!("--hosts:{}", hosts.with(|h| h.len()))>
                    <div class="ut-row ut-head" role="row">
                        <span class="ut-app cap" role="columnheader">"App"</span>
                        {move || {
                            let hosts = hosts.get();
                            let short = short_labels(&hosts.iter().map(|h| h.label.clone()).collect::<Vec<_>>());
                            hosts.into_iter().zip(short).map(|(h, short)| {
                                let state = host_state(&h);
                                view! {
                                    <span class="ut-host" role="columnheader" title=h.label.clone()>
                                        <span class="ut-hname"><PlatformGlyph id=glyph_id(&h.platform)/><span class="cap">{h.label.clone()}</span><span class="ut-hs" aria-hidden="true">{short}</span></span>
                                        {state.map(|s| {
                                            let words = if s.result.is_empty() { s.app.clone() } else { format!("{} · {}", s.app, s.result) };
                                            let tone = tone_class(&s.tone);
                                            view! {
                                                <span class=format!("ut-hstate {tone}") class:running=s.running>{words}</span>
                                                <span class=format!("ut-hst {tone}") class:running=s.running aria-hidden="true">{host_tiny(&s)}</span>
                                            }
                                        })}
                                    </span>
                                }
                            }).collect_view()
                        }}
                    </div>
                    <For each=move || rows.get() key=|r| format!("{r:?}") children=move |r: AppRow| {
                        let app = r.app.clone();
                        let app2 = r.app.clone();
                        let app3 = r.app.clone();
                        let is_open = Memo::new(move |_| expanded.with(|e| e.contains(&app)));
                        let details: Vec<(String, String)> = r.cells.iter().enumerate().filter_map(|(i, c)| {
                            let c = c.as_ref()?;
                            let words = full_words(c);
                            if words.is_empty() { return None; }
                            let host = hosts.with_untracked(|h| h.get(i).map(|h| h.label.clone()).unwrap_or_default());
                            Some((host, words))
                        }).collect();
                        let has_details = !details.is_empty();
                        view! {
                            <div class="ut-item" class:open=move || is_open.get()>
                                <button type="button" class="ut-row" role="row" disabled=!has_details
                                    aria-expanded=move || if is_open.get() { "true" } else { "false" }
                                    on:click=move |_| expanded.update(|e| {
                                        if let Some(i) = e.iter().position(|a| a == &app2) { e.remove(i); } else { e.push(app2.clone()); }
                                    })>
                                    <span class="ut-app" role="rowheader">
                                        {has_details.then(|| view! { <Icon name="chev-right" class="ut-chev"/> })}
                                        <span>{app3}</span>
                                    </span>
                                    {r.cells.into_iter().map(|c| match c {
                                        Some(c) => {
                                            let (r1, r2) = (c.result.clone(), c.result.clone());
                                            view! {
                                                <span class=format!("ut-cell {}", tone_class(&c.tone)) class:running=c.running role="cell"
                                                    title=full_words(&c) aria-label=(!c.result.is_empty()).then(|| c.result.clone())>
                                                    <span class="ut-w">{move || cell_words(&r1, columns.get().0, columns.get().1).0}</span>
                                                    {tiny_lead(&c.result).map(|lead| view! { <span class="ut-l" aria-hidden="true">{lead}</span> })}
                                                    <span class="ut-t" aria-hidden="true">{move || cell_words(&r2, columns.get().0, columns.get().1).1}</span>
                                                    {c.running.then(|| view! { <span class="ut-bar"></span> })}
                                                </span>
                                            }.into_any()
                                        }
                                        None => view! { <span class="ut-cell q4" role="cell">"–"</span> }.into_any(),
                                    }).collect_view()}
                                </button>
                                <Show when=move || is_open.get()>
                                    <ul class="ut-details">
                                        {details.iter().map(|(host, tip)| view! { <li><b>{host.clone()}</b>" "{tip.clone()}</li> }).collect_view()}
                                    </ul>
                                </Show>
                            </div>
                        }
                    }/>
                </div>
            </Show>
            <p class="upd-note q3">
                <span class="by-header">"Runs from Update apps in the header"</span>
                <span class="by-topbar">"Runs from Update apps in the top bar"</span>
                <span class="by-rail">"Runs from Update apps in the side rail"</span>
            </p>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, app: &str, result: &str) -> UpdItem {
        UpdItem {
            key: key.into(),
            app: app.into(),
            result: result.into(),
            ..Default::default()
        }
    }

    #[test]
    fn one_row_per_app_with_a_line_per_computer() {
        let hosts = vec![
            UpdHost {
                id: "mac".into(),
                label: "Mac".into(),
                platform: "apple".into(),
                items: vec![
                    item("mac|codex-cli", "Codex CLI", "Already current"),
                    item("mac|claude-code", "Claude Code", "Updated to 2.1.0"),
                ],
            },
            UpdHost {
                id: "windows".into(),
                label: "Windows".into(),
                platform: "windows".into(),
                items: vec![item("windows|state", "Waiting for its turn", "")],
            },
            UpdHost {
                id: "ubuntu".into(),
                label: "Ubuntu".into(),
                platform: "ubuntu".into(),
                items: vec![
                    item("ubuntu|claude-code", "Claude Code", "Already current"),
                    item("ubuntu|current", "T3 Code", "Checking · 45s"),
                ],
            },
        ];
        let rows = table(&hosts);
        let apps: Vec<&str> = rows.iter().map(|r| r.app.as_str()).collect();
        assert_eq!(apps, ["Codex CLI", "Claude Code", "T3 Code"]);
        assert_eq!(rows[1].cells[2].as_ref().unwrap().result, "Already current");
        assert!(rows[1].cells[1].is_none());
        assert_eq!(rows[2].cells[2].as_ref().unwrap().result, "Checking · 45s");
        assert_eq!(host_state(&hosts[1]).unwrap().app, "Waiting for its turn");
        assert!(host_state(&hosts[2]).is_none());
    }

    #[test]
    fn cells_say_it_short_and_keep_the_sentence() {
        assert_eq!(short_word("Updated to 2.1.3"), "→ 2.1.3");
        assert_eq!(short_word("Already current"), "current");
        assert_eq!(short_word("Quit to finish update"), "quit to finish");
        assert_eq!(short_word("Unknown: Mac not reachable"), "not reachable");
        assert_eq!(short_word("Checking · 45s"), "checking · 45s");
        assert_eq!(
            short_word("A word from a newer server"),
            "a word from a newer server"
        );
        let item = UpdItem {
            result: "Failed".into(),
            tip: "The download stopped.".into(),
            ..Default::default()
        };
        assert_eq!(full_words(&item), "Failed · The download stopped.");
    }

    #[test]
    fn narrow_tables_use_short_words_that_never_break() {
        assert_eq!(tiny_word("Already current"), "✓");
        assert_eq!(tiny_word("Updated to 2.1.3"), "→2.1.3");
        assert_eq!(tiny_word("Quit to finish update"), "quit");
        assert_eq!(tiny_word("Installer failed"), "failed");
        // a failed restart is a failure, not an instruction to restart (review r1); the cell keeps "restart failed"
        assert_eq!(tiny_word("Updated, restart failed"), "failed");
        assert_eq!(short_word("Updated, restart failed"), "restart failed");
        assert_eq!(tiny_word("Unknown: Nas1 not reachable"), "offline");
        assert_eq!(tiny_word("Checking · 45s"), "check");
        assert_eq!(
            tiny_word("A word from a newer server"),
            "a word from a newer server"
        );
        let labels: Vec<String> = [
            "Mac",
            "Windows",
            "Ubuntu",
            "Nas1",
            "Alexis PC",
            "Odroid",
            "1070ti",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            short_labels(&labels),
            ["Mac", "Win", "Ubuntu", "Nas1", "Alexis", "Odroid", "1070ti"]
        );
        let twins: Vec<String> = ["Alexis PC", "Alexis Mac"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(short_labels(&twins), twins);
        let none = UpdItem {
            app: "No results from this computer".into(),
            ..Default::default()
        };
        assert_eq!(host_tiny(&none), "no results");
    }

    #[test]
    fn a_failed_model_refresh_reads_as_a_failure_and_says_whether_the_app_updated() {
        // deep review 2026-11-26 (High): both Muse cases read "models" in amber
        assert_eq!(tiny_word("Updated, model refresh failed"), "models failed");
        assert_eq!(tiny_lead("Updated, model refresh failed"), Some("updated"));
        assert_eq!(tiny_word("Model refresh failed"), "models failed");
        assert_eq!(tiny_lead("Model refresh failed"), None);
        assert_eq!(
            short_word("Updated, model refresh failed"),
            "updated, model refresh failed"
        );
        assert_eq!(short_word("Model refresh failed"), "model refresh failed");
        // critical results keep what went wrong in the cell
        assert_eq!(tiny_word("Updated the wrong copy"), "wrong copy");
        assert_eq!(tiny_word("T3 reply lost"), "reply lost");
        assert_eq!(tiny_word("Another copy runs"), "other copy");
        assert_eq!(tiny_word("Timed out"), "timed out");
        assert_eq!(tiny_lead("Updated, restart failed"), None);
    }

    #[test]
    fn an_update_names_its_version_only_where_the_whole_version_fits() {
        // a 375 px phone's card (357 px) with seven computers: about 46 px a column
        let (wide, tiny) = (column_width(357.0, 7, false), column_width(357.0, 7, true));
        assert!((tiny - 46.14).abs() < 0.01);
        assert_eq!(cell_words("Updated to 2.1.3", wide, tiny).1, "→2.1.3");
        // the committed Windows MSIX and T3 nightly results (tests/spec/agent/app-update-rows.json)
        assert_eq!(
            cell_words("Updated to 26.1113.36452.0", wide, tiny).1,
            "updated"
        );
        assert_eq!(
            cell_words("Updated to 0.1.10-nightly.20261113.4616", wide, tiny).1,
            "updated"
        );
        // just above the wide cutoff (947 px card, 100 px columns) the nightly and the MSIX say "updated" too
        let wide = column_width(947.0, 7, false);
        assert!(wide > 100.0 && wide < 100.5);
        assert_eq!(
            cell_words("Updated to 0.1.10-nightly.20261113.4616", wide, 0.0).0,
            "updated"
        );
        assert_eq!(
            cell_words("Updated to 26.1113.36452.0", wide, 0.0).0,
            "updated"
        );
        assert_eq!(cell_words("Updated to 2.1.3", wide, 0.0).0, "→ 2.1.3");
        // a 1920 desktop's card has room for both, as before
        let wide = column_width(1840.0, 7, false);
        assert_eq!(
            cell_words("Updated to 0.1.10-nightly.20261113.4616", wide, 0.0).0,
            "→ 0.1.10-nightly.20261113.4616"
        );
        assert_eq!(
            cell_words("Updated to 26.1113.36452.0", wide, 0.0).0,
            "→ 26.1113.36452.0"
        );
        // every other result keeps its words
        assert_eq!(
            cell_words("Quit to finish update", 50.0, 50.0),
            ("quit to finish".to_string(), "quit".to_string())
        );
        // the table's widths at 11 px are never below the reviewers' FreeType measure of the MSIX (77.9 px)
        assert!(text_width("26.1113.36452.0", 11.0) >= 77.9);
        assert!((text_width("updated", 11.0) - 44.0).abs() < 0.5);
    }

    #[test]
    fn the_table_is_compact_whenever_a_computer_column_would_cut_its_words() {
        // seven computers: the open Duo's 640 px card and a 760 px window are compact, a 1920 desktop is wide
        assert!(compact_table(374.0, 7));
        assert!(compact_table(640.0, 7));
        assert!(compact_table(730.0, 7));
        assert!(!compact_table(1840.0, 7));
        // wide from about 946 px: 100 px per computer
        assert!(compact_table(945.0, 7));
        assert!(!compact_table(947.0, 7));
        // a retry on one computer fits a phone's card wide
        assert!(!compact_table(374.0, 1));
        // before the first measurement (0 px) nothing is drawn wide
        assert!(compact_table(0.0, 7));
    }
}
