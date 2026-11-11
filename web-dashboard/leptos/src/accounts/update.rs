//! Update apps, redesigned (owner, 2026-11-10: "the update card is super long and after other cards"): a compact table
//! with one row per app and a column per computer, first on the page. The head line and every result word come from
//! `updateResultsView` (accounts-view.mjs); this only arranges them. A row opens to each computer's details (the
//! message and versions the Slint page showed as a tip). The table is open on wide pages and folded to its head line on
//! narrow ones.

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
        "Models not registered yet" | "Updated, models not registered yet" => "models pending",
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
    view! {
        <section class="plate ac-card upd" data-probe="update-card">
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
            <Show when=move || open.get() && update.with(|u| u.shown && !u.hosts.is_empty())>
                <div class="upd-table" role="table" aria-label="Update apps results"
                    style=move || format!("--hosts:{}", hosts.with(|h| h.len()))>
                    <div class="ut-row ut-head" role="row">
                        <span class="ut-app cap" role="columnheader">"App"</span>
                        {move || hosts.get().into_iter().map(|h| {
                            let state = host_state(&h);
                            view! {
                                <span class="ut-host" role="columnheader">
                                    <span class="ut-hname"><PlatformGlyph id=glyph_id(&h.platform)/><span class="cap">{h.label.clone()}</span></span>
                                    {state.map(|s| {
                                        let words = if s.result.is_empty() { s.app.clone() } else { format!("{} · {}", s.app, s.result) };
                                        view! { <span class=format!("ut-hstate {}", tone_class(&s.tone)) class:running=s.running>{words}</span> }
                                    })}
                                </span>
                            }
                        }).collect_view()}
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
                                        Some(c) => view! {
                                            <span class=format!("ut-cell {}", tone_class(&c.tone)) class:running=c.running role="cell" title=full_words(&c)>
                                                {short_word(&c.result)}
                                                {c.running.then(|| view! { <span class="ut-bar"></span> })}
                                            </span>
                                        }.into_any(),
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
}
