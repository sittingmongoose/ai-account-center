//! Update apps, redesigned (owner, 2026-11-10: "the update card is super long and after other cards"): a compact table
//! with one row per app and a column per computer, first on the page. The head line and every result word come from
//! `updateResultsView` (accounts-view.mjs); this only arranges them. A row opens to each computer's details (the
//! message and versions the Slint page showed as a tip). The table is open on wide pages and folded to its head line on
//! narrow ones. On a narrow card (a phone, the folded Duo) each app's name gets its own line over a line of short
//! computer columns: every head and cell also carries a short form (`short_label`, `tiny_word`, `host_tiny`) that the
//! stylesheet shows there instead of the words, which stay in the cell for screen readers, its tip and the details.

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

/// A narrow table's word for a cell: one short token that never wraps in a column a seventh of a phone wide. A failure
/// or a step for the user stays a word (drawn as a colored chip), never just a mark, and every failure reads "failed"
/// (a failed restart too: "restart" alone would read as an instruction); the full result stays in the
/// cell's tip and the row's details. Unknown words keep their own text (clipped, never broken).
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
        "Models not registered yet" | "Updated, models not registered yet" => "models",
        "Check timed out" | "Timed out" => "timeout",
        "Unknown" | "Unknown result" => "unknown",
        "Sign-in needed" => "sign-in",
        "Blocked by policy" => "policy",
        "Another copy runs" => "copy",
        "Updated the wrong copy" => "wrong",
        "Folder not writable" => "locked",
        "T3 reply lost" => "lost",
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
                <div class="upd-table" role="table" aria-label="Update apps results"
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
                                        Some(c) => view! {
                                            <span class=format!("ut-cell {}", tone_class(&c.tone)) class:running=c.running role="cell" title=full_words(&c)>
                                                <span class="ut-w">{short_word(&c.result)}</span>
                                                <span class="ut-t" aria-hidden="true">{tiny_word(&c.result)}</span>
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
}
