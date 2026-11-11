//! The Status page (redesign of 2026-11-08): questions first, then progress, pinned threads, machines, phones and
//! spend. Every number and state word comes from the server's view; this file only lays them out.

use crate::api::*;
use crate::cards::CardsPanel;
use crate::fmt;
use crate::live::Live;
use crate::ui::*;
use leptos::prelude::*;

fn source_of(live: Live, id: &'static str) -> Signal<Option<Source>> {
    Signal::derive(move || {
        let view = live.view.get()?;
        view.sources.iter().find(|s| s.id == id).cloned()
    })
}

fn source_named(
    live: Live,
    id_of: impl Fn(&StatusView) -> String + Send + Sync + 'static,
) -> Signal<Option<Source>> {
    Signal::derive(move || {
        let view = live.view.get()?;
        let id = id_of(&view);
        view.sources.iter().find(|s| s.id == id).cloned()
    })
}

fn other_problems(view: &StatusView, ids: &[String]) -> Vec<String> {
    view.sources
        .iter()
        .filter(|s| ids.contains(&s.id))
        .filter_map(|s| s.problem())
        .collect()
}

/// The page line and sentence, and the band's summary sentence.
fn headline_runs(view: &StatusView) -> Vec<Run> {
    view.headline.runs.clone()
}

#[component]
pub fn StatusPage(embedded: bool) -> impl IntoView {
    let live = expect_context::<Live>();
    let info = ChromeInfo {
        updated: Signal::derive(move || {
            if live.view.with(|v| v.is_none()) {
                String::new()
            } else {
                fmt::age(live.elapsed_seconds())
            }
        }),
        live: Signal::derive(move || live.error.with(|e| e.is_none())),
        summary: Signal::derive(move || {
            live.view
                .with(|v| v.as_ref().map(headline_runs).unwrap_or_default())
        }),
        refresh: Callback::new(move |_| live.reload()),
        lead: None,
    };
    let banner = move || {
        let error = live.error.get()?;
        let old = if live.view.with(|v| v.is_some()) {
            format!(
                " Showing the reading from {}.",
                fmt::age(live.elapsed_seconds())
            )
        } else {
            String::new()
        };
        Some(view! { <p class="banner" role="status">{error.sentence()}{old}</p> })
    };
    view! {
        <PageShell page=crate::Page::Status embedded=embedded info=info>
            <div class="page" data-stale=move || live.error.with(|e| e.is_some()).then_some("1")>
                <div class="page-head">
                    <div>
                        <h1 class="page-title">"Status"</h1>
                        <p class="headline">{move || runs_view(live.view.with(|v| v.as_ref().map(headline_runs).unwrap_or_default()))}</p>
                    </div>
                </div>
                {banner}
                <Show
                    when=move || live.view.with(|v| v.is_some())
                    fallback=move || view! {
                        <p class="empty">{move || if live.error.with(|e| e.is_some()) { "" } else { "Reading the status..." }}</p>
                    }
                >
                    <div class="status-grid">
                        <CardsPanel/>
                        <ProgressPanel/>
                        <PinnedPanelView/>
                        <MachinesPanel/>
                        <PhonesPanel/>
                        <SpendPanel/>
                    </div>
                    {embedded.then(frame_offer)}
                </Show>
            </div>
        </PageShell>
    }
}

/// The way into the new app frame from today's app (PLAN step 4): off until this device turns it on here. The installed
/// app keeps its own storage on iOS, so it is turned on from inside the installed app.
fn frame_offer() -> impl IntoView {
    let turn_on = move |_| {
        if let Some(s) = window().local_storage().ok().flatten() {
            let _ = s.set_item("aac-frame", "new");
        }
        let target = window().top().ok().flatten().unwrap_or_else(window);
        let _ = target.location().set_href("/app/status");
    };
    view! {
        <p class="frame-offer">
            <b>"New app frame (preview)"</b>
            <span>"Every tab in the new layout, with the Duo strip and band. Classic app in its account menu brings this one back."</span>
            <button class="link" on:click=turn_on>"Try it"</button>
        </p>
    }
}

/// A plate that starts folded below 760 px with its heading and one-line summary; tapping the heading opens it.
/// Above 760 px the arrow is hidden and the body always shows (CSS).
#[component]
fn FoldedPlate(class: &'static str, children: Children) -> impl IntoView {
    let open = RwSignal::new(false);
    provide_context(open);
    view! {
        <section class=format!("plate {class}") class:collapsed=move || !open.get() class:collapsed-open=move || open.get()>
            {children()}
        </section>
    }
}

#[component]
fn FoldHead(children: Children) -> impl IntoView {
    let open = use_context::<RwSignal<bool>>();
    view! {
        <div class="plate-head plate-head-tap" on:click=move |_| { if let Some(o) = open { o.update(|x| *x = !*x); } }>
            {children()}
        </div>
    }
}

#[component]
fn Chev() -> impl IntoView {
    let open = use_context::<RwSignal<bool>>();
    view! {
        <button class="chev-toggle" aria-label="Show or hide this section" aria-expanded=move || open.map(|o| o.get()).unwrap_or(false).to_string()>
            <Icon name="chev-down"/>
        </button>
    }
}

// ---- Progress --------------------------------------------------------------------------------------------------

fn state_word(state: &str) -> &'static str {
    match state {
        "landed" => "Landed",
        "done" => "Done",
        "ready" => "Ready to land",
        "review" => "In review",
        "fixing" => "Fixing",
        "working" => "Working",
        "blocked" => "Blocked",
        "todo" => "To do",
        "dropped" => "Dropped",
        "waiting" => "Waiting",
        "paused" => "Paused",
        "idle" => "Idle",
        "" => "No report",
        _ => "Unknown",
    }
}

fn blocked_words(b: &BlockedOn) -> String {
    let who = match b.kind.as_str() {
        "owner" => "Waiting for you",
        "agent" => "Waiting on an agent",
        "review" => "Waiting on review",
        "lock" => "Waiting for the landing lock",
        "resource" => "Waiting for a machine",
        "quota" => "Out of quota",
        _ => "Blocked",
    };
    if b.what.is_empty() {
        who.to_string()
    } else {
        format!("{who}: {}", b.what)
    }
}

/// The counts of a stream that still has work, as words: `2 blocked` first and in amber.
pub fn counts_runs(c: &Counts) -> Vec<(String, Option<&'static str>)> {
    let mut parts: Vec<(String, Option<&'static str>)> = Vec::new();
    if c.blocked > 0 {
        parts.push((format!("{} blocked", c.blocked), Some("warn")));
    }
    for (n, word) in [
        (c.working, "working"),
        (c.review, "in review"),
        (c.fixing, "fixing"),
        (c.ready, "ready to land"),
        (c.todo, "to do"),
    ] {
        if n > 0 {
            parts.push((format!("{n} {word}"), None));
        }
    }
    parts
}

#[component]
fn ProgressPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, "agent-reports");
    let projects = move || {
        live.view
            .with(|v| v.as_ref().map(|v| v.projects.clone()).unwrap_or_default())
    };
    let totals = move || {
        let list = projects();
        let (done, of): (u32, u32) = list
            .iter()
            .filter_map(|p| p.counts.as_ref())
            .fold((0, 0), |(d, o), c| {
                (d + c.landed + c.done, o + c.total.saturating_sub(c.dropped))
            });
        (done, of, list.len())
    };
    let summary = move || {
        let list = projects();
        let blocked: u32 = list
            .iter()
            .filter_map(|p| p.counts.as_ref())
            .map(|c| c.blocked)
            .sum();
        let no_list = list
            .iter()
            .flat_map(|p| p.streams.iter())
            .filter(|s| s.completion.is_none() && s.counts.is_none())
            .count();
        // With no task list anywhere there is nothing to call clear: say so rather than "Nothing blocked".
        if !has_task_lists(&list) {
            return view! { <span>"No task lists yet."</span> }.into_any();
        }
        let mut parts: Vec<AnyView> = Vec::new();
        if blocked > 0 {
            parts.push(
                view! { <span data-tone="warn">{format!("{blocked} blocked")}</span> }.into_any(),
            );
        }
        if no_list > 0 {
            let words = if no_list == 1 {
                "1 stream has no task list yet".to_string()
            } else {
                format!("{no_list} streams have no task list yet")
            };
            parts.push(view! { <span>{words}</span> }.into_any());
        }
        if parts.is_empty() {
            parts.push(view! { <span>"Nothing blocked"</span> }.into_any());
        }
        parts
            .into_iter()
            .enumerate()
            .map(|(i, part)| view! { {(i > 0).then_some(" · ")}{part} })
            .collect_view()
            .into_any()
    };
    view! {
        <FoldedPlate class="sec-progress">
            <FoldHead>
                <h2>"Progress"</h2>
                <span class="plate-meta">{move || {
                    let (d, o, n) = totals();
                    // the same test as the summary: never "0 of 0 tasks done in 0 projects"
                    if !has_task_lists(&projects()) {
                        return view! { <span>"No task lists yet"</span> }.into_any();
                    }
                    view! { <b>{format!("{d} of {o}")}</b>{format!(" tasks done in {n} {}", if n == 1 { "project" } else { "projects" })}}.into_any()
                }}</span>
                <Chev/>
                <p class="sec-sum not-wide">{summary}</p>
            </FoldHead>
            {move || source_note(source.get(), "No reading from the agent reports yet.")}
            <div class="rows">
                {move || {
                    let list = projects();
                    if list.is_empty() {
                        return view! { <p class="empty">"No project reports yet."</p> }.into_any();
                    }
                    list.into_iter().map(|p| view! { <ProjectView project=p/> }).collect_view().into_any()
                }}
            </div>
        </FoldedPlate>
    }
}

/// Whether any project or stream has a task list: the Progress head and its summary both ask this one question.
fn has_task_lists(projects: &[Project]) -> bool {
    projects.iter().any(|p| p.counts.is_some())
        || projects
            .iter()
            .flat_map(|p| p.streams.iter())
            .any(|s| s.completion.is_some() || s.counts.is_some())
}

#[component]
fn ProjectView(project: Project) -> impl IntoView {
    let total = project.counts.as_ref().map(|c| {
        format!(
            "{} of {}",
            c.landed + c.done,
            c.total.saturating_sub(c.dropped)
        )
    });
    view! {
        <div class="proj">
            <div class="proj-head"><h3>{project.label.clone()}</h3>{total.map(|t| view! { <span class="tot">{t}</span> })}</div>
            {project.streams.into_iter().map(|s| view! { <StreamView stream=s/> }).collect_view()}
        </div>
    }
}

#[component]
fn StreamView(stream: Stream) -> impl IntoView {
    let open = RwSignal::new(false);
    let has_items = !stream.items.is_empty();
    let id = stream.id.clone();
    let label = if stream.label.is_empty() {
        stream.id.clone()
    } else {
        stream.label.clone()
    };
    let items = stream.items.clone();
    let body = match (
        stream
            .completion
            .as_ref()
            .and_then(|c| Some((c.done?, c.of?))),
        stream.counts.as_ref(),
    ) {
        (Some((done, of)), Some(counts)) => {
            let finished = of > 0 && done >= of;
            let parts = counts_runs(counts);
            let pct = if of == 0 {
                0.0
            } else {
                done as f64 / of as f64 * 100.0
            };
            let landed = stream
                .items
                .iter()
                .filter_map(|i| i.landed.as_ref().and_then(|l| l.at.clone()))
                .max();
            let why = if finished {
                view! { <span data-tone="good">"Finished"</span>{landed.map(|at| format!(" · landed {}", fmt::short_date(&at)))} }.into_any()
            } else {
                parts.into_iter().enumerate().map(|(i, (text, tone))| view! { {(i > 0).then_some(" · ")}<span data-tone=tone>{text}</span> }).collect_view().into_any()
            };
            view! {
                <span class="cnt">{format!("{done} of {of}")}</span>
                <Track pct=pct kind="ink" thin=true label=format!("{done} of {of}")/>
                <span class="why">{why}</span>
            }
            .into_any()
        }
        _ => {
            let board_age = stream
                .board
                .as_ref()
                .and_then(|b| b.age_seconds)
                .map(fmt::long_age);
            view! {
                <span class="cnt q3">"no list"</span>
                <span class="why">"No task list yet."{board_age.map(|a| format!(" Its board was updated {a}."))}</span>
            }
            .into_any()
        }
    };
    view! {
        <button class="stream" disabled=!has_items data-stale=stream.stale.then_some("1") aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|o| *o = !*o)>
            <span class="code">{id}</span>
            <span class="nm">{label}</span>
            {body}
        </button>
        {stream.problem.clone().map(|p| view! { <p class="plate-note problem">{format!("Task report unreadable: {p}")}</p> })}
        <Show when=move || open.get()>
            <ul class="stream-items">
                {items.clone().into_iter().map(|i| {
                    let blocked = i.blocked_on.as_ref().map(blocked_words);
                    let landed = i.landed.as_ref().map(|l| format!("landed {}", l.sha.chars().take(10).collect::<String>()));
                    let reviews = i.reviews.iter().map(|r| format!("{} {}", r.by, r.verdict)).collect::<Vec<_>>().join(" · ");
                    view! {
                        <li>
                            <span class="id">{i.id.clone()}</span>
                            <span>{i.title.clone()}</span>
                            <span class="st">{state_word(&i.state)}</span>
                            {blocked.map(|b| view! { <span class="note" data-tone="warn">{b}</span> })}
                            {landed.map(|l| view! { <span class="note">{l}</span> })}
                            {i.why.clone().filter(|w| !w.is_empty()).map(|w| view! { <span class="note">{w}</span> })}
                            {(!reviews.is_empty()).then(|| view! { <span class="note">{reviews}</span> })}
                        </li>
                    }
                }).collect_view()}
            </ul>
        </Show>
    }
}

// ---- Pinned threads --------------------------------------------------------------------------------------------

/// "Blocked" and "Looping" as the facts show them: the leading word in its state colour.
fn lead_word(text: &str, word: &str, tone: &'static str) -> AnyView {
    match text.strip_prefix(word) {
        Some(rest) => {
            view! { <span data-tone=tone>{word.to_string()}</span>{rest.to_string()} }.into_any()
        }
        None => view! { <span data-tone=tone>{text.to_string()}</span> }.into_any(),
    }
}

fn blocked_fact(b: &PinnedBlocked) -> AnyView {
    if b.text.trim().is_empty() {
        return view! { <span class="q3">"Can't tell."</span> }.into_any();
    }
    match b.text.split_once(':') {
        Some((head, rest)) => {
            let tone = "warn";
            view! { <span data-tone=tone>{format!("{head}:")}</span>{rest.to_string()} }.into_any()
        }
        None => {
            if b.text == "None." || b.text == "None" {
                view! { <span>{b.text.clone()}</span> }.into_any()
            } else {
                view! { <span data-tone="warn">{b.text.clone()}</span> }.into_any()
            }
        }
    }
}

fn looping_fact(l: &PinnedLooping) -> AnyView {
    if l.text.trim().is_empty() {
        return view! { <span class="q3">"Can't tell."</span> }.into_any();
    }
    match l.answer.as_str() {
        "yes" => lead_word(&l.text, "Yes.", "crit"),
        "cantTell" => view! { <span class="q3">{l.text.clone()}</span> }.into_any(),
        _ => view! { <span>{l.text.clone()}</span> }.into_any(),
    }
}

/// The one-line state of a pinned thread in its row ("Looping · waiting on you · 6 min ago").
fn row_line(p: &PinnedUpdate) -> AnyView {
    let mut parts: Vec<AnyView> = Vec::new();
    // A refused status.json comes with the cached last good copy (reportState "unreadable"): say why first.
    if let Some(reason) = refusal_reason(p) {
        let cached =
            is_unreadable(p).then_some(view! { " · "<span class="q3">"cached copy"</span> });
        return view! { <span data-tone="crit">"Status refused"</span>{format!(": {reason}")}{cached} }.into_any();
    }
    if p.report_state == "stale" {
        return view! { <span class="q3">"Last update "{p.updated.as_ref().map(|u| u.text.clone()).unwrap_or_default().replace("Updated ", "")}</span> }.into_any();
    }
    if is_unreadable(p) {
        let note = unreadable_words(p);
        return view! { <span class="w-ink">{note}</span>" · "<span class="q3">"cached copy"</span> }.into_any();
    }
    if p.report_state == "none" {
        return view! { <span class="q3">"Not reporting yet"</span> }.into_any();
    }
    let looping = p.looping.answer == "yes";
    // an empty blocked fact (no report says it) is unknown: neither waiting nor on track
    let blocked_known = !p.blocked.text.trim().is_empty();
    let waiting = blocked_known && !(p.blocked.text == "None." || p.blocked.text == "None");
    if looping {
        // The server says that a task is looping, not which: a blocked task can sit ahead of it in `left`.
        parts.push(view! { <span data-tone="crit">"Looping"</span> }.into_any());
    }
    if waiting {
        let words = if p.blocked.waiting_on_you {
            if looping {
                "waiting on you".to_string()
            } else {
                "Waiting on you".to_string()
            }
        } else {
            let short = p
                .blocked
                .text
                .split(':')
                .next()
                .unwrap_or(&p.blocked.text)
                .trim_end_matches('.');
            if looping {
                lowercase_first(short)
            } else {
                short.to_string()
            }
        };
        parts.push(view! { <span data-tone="warn">{words}</span> }.into_any());
    }
    if !looping {
        let looping_known = p.looping.answer == "no" && !p.looping.text.trim().is_empty();
        if !waiting && blocked_known && looping_known {
            parts.push(view! { <span>"On track"</span> }.into_any());
        }
        parts.push(view! { <span>{if looping_known { "not looping" } else { "can't tell if it's looping" }}</span> }.into_any());
    }
    // no percent: the slot stays empty and the line says why (never "no update" beside a fresh report)
    if p.percent.is_none() {
        let note = p
            .percent_note
            .clone()
            .map(|n| lowercase_first(n.trim_end_matches('.')))
            .unwrap_or_else(|| "no task list".to_string());
        parts.push(view! { <span class="q3">{note}</span> }.into_any());
    }
    if let Some(u) = p.updated.as_ref() {
        parts.push(view! { <span>{u.text.replace("Updated ", "")}</span> }.into_any());
    }
    let last = parts.len().saturating_sub(1);
    parts
        .into_iter()
        .enumerate()
        .map(|(i, part)| view! { {part}{(i < last).then_some(" · ")} })
        .collect_view()
        .into_any()
}

fn lowercase_first(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

fn pct_value(p: &PinnedUpdate) -> Option<f64> {
    let pct = p.percent.as_ref()?;
    match (pct.done, pct.of) {
        (Some(d), Some(o)) if o > 0 => Some(d as f64 / o as f64 * 100.0),
        _ => pct.text.trim_end_matches('%').parse::<f64>().ok(),
    }
}

fn item_list(items: &[PinnedItem], limit: usize) -> impl IntoView + use<> {
    let (shown, more) = (
        items.iter().take(limit).cloned().collect::<Vec<_>>(),
        items.len().saturating_sub(limit),
    );
    view! {
        <ul>
            {shown.into_iter().map(|i| view! { <li><span class="mono">{i.id}</span>{i.title}</li> }).collect_view()}
            {(more > 0).then(|| view! { <li class="more">{format!("and {more} more")}</li> })}
        </ul>
    }
}

/// The parts of an update every width shows: progress, facts and task lists. `limit` caps the task lists (cards do,
/// the sheet doesn't). `updated_fact` adds "Updated" to the facts, as the sheet does.
fn pin_body(
    p: &PinnedUpdate,
    left_limit: usize,
    done_limit: usize,
    updated_fact: bool,
) -> impl IntoView + use<> {
    let pct = pct_value(p);
    let percent = p.percent.clone();
    let of_text = percent
        .as_ref()
        .and_then(|x| Some(format!("{} of {} tasks done", x.done?, x.of?)))
        .or_else(|| p.percent_note.clone());
    let note = if percent.as_ref().is_some_and(|x| x.done.is_some()) {
        p.percent_note.clone()
    } else {
        None
    };
    let left = p.left.clone();
    let done = p.done.clone();
    let blocked = blocked_fact(&p.blocked);
    let looping = looping_fact(&p.looping);
    let now = p.now.clone();
    let updated = updated_fact
        .then(|| p.updated.as_ref().map(|u| u.text.replace("Updated ", "")))
        .flatten();
    view! {
        {match percent {
            Some(x) => view! {
                <div>
                    <div class="pin-pct"><span class="big">{x.text.clone()}</span><span class="of">{of_text}</span></div>
                    <div style="margin-top:8px"><Track pct=pct.unwrap_or(0.0) kind="acc" thin=true/></div>
                    {note.map(|n| view! { <p class="pin-who">{n}</p> })}
                </div>
            }.into_any(),
            None => view! { <p class="q2" style="font-size:13px">{p.percent_note.clone().unwrap_or_else(|| "No task list.".into())}</p> }.into_any(),
        }}
        <dl class="facts">
            {now.map(|n| view! { <dt>"Now"</dt><dd>{n}</dd> })}
            <dt>"Blocked"</dt><dd>{blocked}</dd>
            <dt>"Looping"</dt><dd>{looping}</dd>
            {updated.map(|u| view! { <dt>"Updated"</dt><dd>{u}</dd> })}
        </dl>
        // a list with nothing in it isn't drawn (no bare "Left · 0")
        {(!left.is_empty() || !done.is_empty()).then(|| view! {
            <div class="tasks">
                {(!left.is_empty()).then(|| view! { <div class="left"><h4 class="cap">{format!("Left · {}", left.len())}</h4>{item_list(&left, left_limit)}</div> })}
                {(!done.is_empty()).then(|| view! { <div class="done"><h4 class="cap">{format!("Done · {}", done.len())}</h4>{item_list(&done, done_limit)}</div> })}
            </div>
        })}
    }
}

/// A pinned thread with no current report: says so plainly, with what it last said. The server's note reads
/// "No status update for 25 days. If it is finished, unpin it in T3.": the second sentence goes last.
fn quiet_body(p: &PinnedUpdate) -> impl IntoView + use<> {
    let refusal = refusal_notice(p);
    let note = p
        .stale_note
        .clone()
        .or_else(|| p.report_note.clone())
        .or_else(|| p.unreadable_note.clone())
        .unwrap_or_default();
    let (first, rest) = match note.split_once(". ") {
        Some((a, b)) => (format!("{a}."), Some(b.to_string())),
        None => (note.clone(), None),
    };
    let summary = p.summary.clone();
    view! {
        {refusal}
        <p class="q2" style="font-size:13px;line-height:1.5">
            <span class="w-ink">{first}</span>
            {summary.map(|s| format!(" The last one said: {s}"))}
        </p>
        {rest.map(|r| view! { <p class="q3" style="font-size:12.5px">{r}</p> })}
    }
}

/// The desktop cards: reporting pins (fresh, or a cached last good copy, refused or not), at most six.
fn card_pins(shown: &[PinnedUpdate]) -> Vec<PinnedUpdate> {
    shown
        .iter()
        .filter(|u| !is_quiet(u))
        .take(6)
        .cloned()
        .collect()
}

fn is_quiet(p: &PinnedUpdate) -> bool {
    p.report_state == "stale" || p.report_state == "none"
}

/// Its report can't be read right now: what is shown is the last good copy, and everything in it is old.
fn is_unreadable(p: &PinnedUpdate) -> bool {
    p.report_state == "unreadable"
}

fn unreadable_words(p: &PinnedUpdate) -> String {
    p.unreadable_note
        .clone()
        .unwrap_or_else(|| "Could not be read.".to_string())
}

/// The sentence over a last good copy: why it is old, then that it is cached and from when.
fn cached_notice(p: &PinnedUpdate) -> impl IntoView + use<> {
    let note = unreadable_words(p);
    let from = p
        .updated
        .as_ref()
        .map(|u| fmt::clock(&u.at))
        .filter(|t| !t.is_empty())
        .map(|t| format!(", from {t}"))
        .unwrap_or_default();
    view! {
        <p class="pin-cached"><span class="w-ink">{note}</span>{format!(" Showing the cached last good copy{from}.")}</p>
    }
}

#[component]
fn PinCard(update: PinnedUpdate) -> impl IntoView {
    let quiet = is_quiet(&update);
    let cached = is_unreadable(&update);
    let who = update.who.clone();
    let foot = update.updated.as_ref().filter(|_| !cached).map(|u| {
        if quiet {
            format!("Last update {}", fmt::clock(&u.at))
        } else {
            u.text.clone()
        }
    });
    let body = if quiet {
        quiet_body(&update).into_any()
    } else if cached {
        view! { {refusal_notice(&update)}{cached_notice(&update)}<div class="pin-dim">{pin_body(&update, 4, 2, false)}</div> }.into_any()
    } else {
        pin_body(&update, 4, 2, false).into_any()
    };
    view! {
        <article class="pin" class:quiet=quiet>
            <div><h3 class="pin-name">{update.title.clone()}</h3>{who.map(|w| view! { <p class="pin-who">{w}</p> })}</div>
            {body}
            <div class="pin-foot"><span>{foot}</span></div>
        </article>
    }
}

#[component]
fn PinRow(update: PinnedUpdate, on_open: Callback<String>) -> impl IntoView {
    let id = update.id.clone();
    let pct_text = update
        .percent
        .as_ref()
        .map(|p| p.text.trim_end_matches('%').to_string());
    let bar = pct_value(&update);
    let quiet = is_quiet(&update);
    let cached = is_unreadable(&update);
    let line = row_line(&update);
    view! {
        <button class="pin-row" class:cached=cached on:click=move |_| on_open.run(id.clone())>
            <span class="pr-name" class:q2=quiet>{update.title.clone()}</span>
            {match pct_text {
                Some(t) => view! { <span class="pr-pct">{t}<small>"%"</small></span> }.into_any(),
                None => view! { <span class="pr-pct"></span> }.into_any(),
            }}
            {bar.map(|v| view! { <Track pct=v kind="acc" thin=true/> })}
            <p class="pr-line" class:q3=quiet>{line}</p>
        </button>
    }
}

/// The sheet draws the pin's current update, so a sheet held open follows its report. Only the contents redraw: the
/// sheet itself stays, and with it where it was scrolled.
#[component]
fn PinSheet(update: Memo<Option<PinnedUpdate>>, on_close: Callback<()>) -> impl IntoView {
    let offset = RwSignal::new(0.0f64);
    let contents = move || {
        update.get().map(|update| {
            let quiet = is_quiet(&update);
            let cached = is_unreadable(&update);
            let who = update.who.clone();
            let body = if quiet {
                quiet_body(&update).into_any()
            } else if cached {
                view! { {refusal_notice(&update)}{cached_notice(&update)}<div class="pin-dim">{pin_body(&update, 500, 500, false)}</div> }.into_any()
            } else {
                pin_body(&update, 500, 500, true).into_any()
            };
            view! {
                <div><h3 class="pin-name" style="font-size:18px">{update.title.clone()}</h3>{who.map(|w| view! { <p class="pin-who">{w}</p> })}</div>
                {body}
            }
        })
    };
    view! {
        <div class="sheet-host" role="dialog" aria-modal="true">
            <button class="sheet-scrim" aria-label="Close" on:click=move |_| on_close.run(())></button>
            <div class="sheet" style=move || sheet_drag_style(offset.get())>
                // The grabber and the close button stay at the top while the sheet scrolls (pkg P recheck N2).
                <div class="sheet-top">
                    <SheetGrab offset=offset on_close=on_close/>
                    <button class="icon-btn bare sheet-close" aria-label="Close" on:click=move |_| on_close.run(())><Icon name="close"/></button>
                </div>
                <article class="pin">{contents}</article>
            </div>
        </div>
    }
}

#[component]
fn PinnedPanelView() -> impl IntoView {
    let live = expect_context::<Live>();
    let panel = Memo::new(move |_| {
        live.view
            .with(|v| v.as_ref().and_then(|v| v.pinned.clone()))
    });
    // Only which pin is open: what the sheet shows always comes from the current panel.
    let sheet_id = RwSignal::new(None::<String>);
    let open_sheet = Callback::new(move |id: String| sheet_id.set(Some(id)));
    let close_sheet = Callback::new(move |_: ()| sheet_id.set(None));
    let sheet_update = Memo::new(move |_| {
        let id = sheet_id.get()?;
        panel.with(|p| {
            p.as_ref()
                .filter(|p| p.unavailable.is_none())
                .and_then(|p| p.updates.iter().find(|u| u.id == id).cloned())
        })
    });
    // A pin that was removed, or a panel that became unavailable, closes the sheet for good: a later return doesn't reopen it.
    Effect::new(move |_| {
        if sheet_id.with(|s| s.is_some()) && sheet_update.with(|u| u.is_none()) {
            sheet_id.set(None);
        }
    });
    let sheet_open = Memo::new(move |_| sheet_id.with(|s| s.is_some()));
    view! {
        <section class="plate sec-pinned" id="pinned">
            <div class="plate-head">
                <h2>"Pinned threads"</h2>
                <span class="plate-meta">{move || panel.get().map(|p| head_runs(&p.head, &p.counts))}</span>
                <div class="plate-tools only-d"><span class="q3" style="font-size:12px">"Pin or unpin threads in T3; this list follows."</span></div>
            </div>
            {move || match panel.get() {
                None => view! { <p class="empty">"Pinned threads are unavailable: this server doesn't send them yet."</p> }.into_any(),
                Some(p) if p.unavailable.is_some() => view! { <p class="empty">{p.unavailable.clone()}</p> }.into_any(),
                Some(p) if p.updates.is_empty() => view! {
                    {p.machine_problems.iter().map(|m| view! { <p class="plate-note problem">{m.text.clone()}</p> }).collect_view()}
                    <p class="empty">"No threads are pinned in T3."</p>
                }.into_any(),
                Some(p) => {
                    let problems = p.machine_problems.clone();
                    let refused = refused_lines(&p);
                    // Pins that don't report at all share one closing line; the rest get rows, and on desktop the
                    // reporting ones (fresh, or a cached last good copy, refused or not) get cards, stale ones rows.
                    let (silent, shown): (Vec<_>, Vec<_>) = p.updates.into_iter().partition(is_silent);
                    let cards = card_pins(&shown);
                    let card_ids = cards.iter().map(|u| u.id.clone()).collect::<Vec<_>>();
                    let rest = shown.iter().filter(|u| !card_ids.contains(&u.id)).cloned().collect::<Vec<_>>();
                    let names = silent.iter().map(|u| u.title.clone()).collect::<Vec<_>>();
                    let none_report = shown.is_empty();
                    view! {
                        {problems.into_iter().map(|m| view! { <p class="plate-note problem">{m.text}</p> }).collect_view()}
                        {refused}
                        <div class="pin-list">
                            {shown.into_iter().map(|u| view! { <PinRow update=u on_open=open_sheet/> }).collect_view()}
                            {silent_line(&names, none_report)}
                        </div>
                        {(!cards.is_empty()).then(|| view! {
                            <div class="pin-grid">
                                {cards.into_iter().map(|u| view! { <PinCard update=u/> }).collect_view()}
                            </div>
                        })}
                        {(!rest.is_empty() || !names.is_empty()).then(|| view! {
                            <div class="only-d pin-rest" class:first=card_ids.is_empty()>
                                {(!rest.is_empty()).then(|| view! { <div class="pin-more">{rest.into_iter().map(|u| view! { <PinRow update=u on_open=open_sheet/> }).collect_view()}</div> })}
                                {silent_line(&names, none_report)}
                            </div>
                        })}
                    }.into_any()
                }
            }}
            {move || sheet_open.get().then(|| view! { <PinSheet update=sheet_update on_close=close_sheet/> })}
        </section>
    }
}

/// "<file> was refused:", or "A status file was refused:" when the server names no file.
fn refused_head(file: &str) -> String {
    if file.trim().is_empty() {
        "A status file was refused:".to_string()
    } else {
        format!("{file} was refused:")
    }
}

/// The reason a pin's own status.json was refused, without its closing stop (it goes inside a line).
fn refusal_reason(p: &PinnedUpdate) -> Option<String> {
    let r = p.refusal.as_ref()?;
    let reason = r.reason.trim().trim_end_matches('.').to_string();
    Some(if reason.is_empty() {
        "no reason given".to_string()
    } else {
        reason
    })
}

/// The refusal over a pin's card or sheet: what was refused and why, in the server's words.
fn refusal_notice(p: &PinnedUpdate) -> Option<AnyView> {
    let r = p.refusal.as_ref()?;
    Some(view! { <p class="pin-cached"><span data-tone="crit">{refused_head(&r.file)}</span>{format!(" {}", r.reason)}</p> }.into_any())
}

/// A pin with no report at all (and no refused one): it has nothing to show beyond its name.
fn is_silent(p: &PinnedUpdate) -> bool {
    p.report_state == "none" && p.refusal.is_none()
}

/// The silent pins in one line: "None reports yet: A, B and C." with how a thread starts, or, when some report,
/// "Not reporting yet: A and B."
fn silent_line(names: &[String], none_report: bool) -> Option<AnyView> {
    if names.is_empty() {
        return None;
    }
    let list = match names {
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        [] => String::new(),
    };
    Some(if none_report {
        view! {
            <p class="pin-silent">
                <b>"None reports yet: "</b><span>{format!("{list}.")}</span>
                <span class="q3">" A thread shows here once it reports its status with agent-report update."</span>
            </p>
        }
        .into_any()
    } else {
        view! { <p class="pin-silent"><span class="q2">"Not reporting yet: "</span><span>{format!("{list}.")}</span></p> }.into_any()
    })
}

/// The refused status@1 files the server holds no good copy of, under the head. A pin whose own row says it was
/// refused isn't repeated here.
fn refused_lines(p: &PinnedPanel) -> impl IntoView + use<> {
    let on_pins: Vec<String> = p
        .updates
        .iter()
        .filter_map(|u| u.refusal.as_ref().map(|r| r.file.clone()))
        .collect();
    p.status_problems
        .iter()
        .filter(|s| s.file.is_empty() || !on_pins.contains(&s.file))
        .map(|s| {
            view! { <p class="plate-note problem"><span data-tone="crit">{refused_head(&s.file)}</span>{format!(" {}", s.reason)}</p> }
        })
        .collect_view()
}

/// The sheet's grabber: drag the sheet down to close it (past 80 px, or a quick flick down); otherwise it springs back.
#[component]
pub(crate) fn SheetGrab(offset: RwSignal<f64>, on_close: Callback<()>) -> impl IntoView {
    let start = StoredValue::new(None::<(f64, f64)>);
    let down = move |e: web_sys::PointerEvent| {
        if let Some(el) = e
            .current_target()
            .and_then(|t| wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok())
        {
            let _ = el.set_pointer_capture(e.pointer_id());
        }
        start.set_value(Some((e.client_y() as f64, js_sys::Date::now())));
    };
    let moved = move |e: web_sys::PointerEvent| {
        if let Some((y0, _)) = start.get_value() {
            offset.set((e.client_y() as f64 - y0).max(0.0));
        }
    };
    let up = move |e: web_sys::PointerEvent| {
        let Some((y0, t0)) = start.get_value() else {
            return;
        };
        start.set_value(None);
        let dy = e.client_y() as f64 - y0;
        let speed = dy / (js_sys::Date::now() - t0).max(1.0);
        if dy > 80.0 || (dy > 24.0 && speed > 0.6) {
            on_close.run(());
        } else {
            offset.set(0.0);
        }
    };
    view! {
        <div class="grabber-zone" aria-hidden="true" on:pointerdown=down on:pointermove=moved on:pointerup=up
            on:pointercancel=move |_| { start.set_value(None); offset.set(0.0); }>
            <div class="grabber"></div>
        </div>
    }
}

/// The sheet follows a drag on its grabber without easing; at rest it has no transform.
pub(crate) fn sheet_drag_style(offset: f64) -> String {
    if offset > 0.0 {
        format!("transform:translateY({offset:.0}px);transition:none")
    } else {
        String::new()
    }
}

/// The head's count line with its numbers bold and its problems coloured: "**5** pinned in T3 · 1 looping · 2 waiting on you".
fn head_runs(head: &str, counts: &PinnedCounts) -> impl IntoView + use<> {
    let parts: Vec<String> = head.split(" · ").map(str::to_string).collect();
    let pinned = counts.pinned;
    parts
        .into_iter()
        .enumerate()
        .map(move |(i, part)| {
            let sep = (i > 0).then_some(" · ");
            let tone = if part.contains("looping") {
                Some("crit")
            } else if part.contains("waiting") {
                Some("warn")
            } else {
                None
            };
            if i == 0 {
                let text = part
                    .strip_prefix(&format!("{pinned}"))
                    .unwrap_or(&part)
                    .to_string();
                view! { {sep}<b>{format!("{pinned}")}</b><span>{text}</span> }.into_any()
            } else {
                view! { {sep}<span data-tone=tone>{part}</span> }.into_any()
            }
        })
        .collect_view()
}

// ---- Machines --------------------------------------------------------------------------------------------------

fn slots_lines(m: &Machine) -> (String, String) {
    let slots = |s: &Option<Slots>| s.as_ref().and_then(|s| s.running.zip(s.max));
    let browsers = match slots(&m.browsers) {
        Some((r, max)) => format!("{r} of {max} browsers"),
        None => "Browsers not reported".to_string(),
    };
    let recordings = match slots(&m.recordings) {
        Some((r, max)) => format!("{r} of {max} recordings"),
        None => "recordings not reported".to_string(),
    };
    let gpu = m
        .gpu_pct
        .map(|g| format!(" · GPU {}", fmt::pct(g)))
        .unwrap_or_default();
    (browsers, format!("{recordings}{gpu}"))
}

fn free_share(free: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (free, total) {
        (Some(f), Some(t)) if t > 0.0 => Some(((t - f) / t * 100.0).clamp(0.0, 100.0)),
        _ => None,
    }
}

fn number_words(n: f64) -> String {
    if n >= 1000.0 {
        let whole = n.round() as u64;
        let (a, b) = (whole / 1000, whole % 1000);
        format!("{a},{b:03}")
    } else if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    }
}

fn gb_free(n: f64) -> String {
    format!("{} GB", number_words(n))
}

fn subtitle(id: &str) -> Option<&'static str> {
    match id {
        "vm" => Some("this computer"),
        "mac" => Some("MacBook"),
        "windows" => Some("desktop"),
        "nas1" => Some("second Ubuntu computer"),
        _ => None,
    }
}

fn machine_notes(m: &Machine) -> Vec<AnyView> {
    let mut out: Vec<AnyView> = m
        .warnings
        .iter()
        .map(|w| {
            view! { <span data-tone="warn">{format!("{}.", w.trim_end_matches('.'))}</span> }
                .into_any()
        })
        .collect();
    if let Some(detail) = m.detail.clone().filter(|d| !d.is_empty()) {
        out.push(
            match detail.split_once(". ") {
                Some((first, rest)) if m.state != "ok" => view! { <span class="q3"><span class="w-ink">{format!("{first}.")}</span>{format!(" {rest}")}</span> }.into_any(),
                _ => view! { <span class="q3">{detail}</span> }.into_any(),
            },
        );
    }
    out
}

#[component]
fn MachinesPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_named(live, |v| v.machines.source.clone());
    let also = Memo::new(move |_| {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| other_problems(v, &v.machines.also))
                .unwrap_or_default()
        })
    });
    let rows = Memo::new(move |_| {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| v.machines.rows.clone())
                .unwrap_or_default()
        })
    });
    let summary = move || {
        let list = rows.get();
        let mut parts: Vec<AnyView> = Vec::new();
        for m in &list {
            if m.state != "ok" {
                let name = m.label.clone();
                let when = m.detail.clone().unwrap_or_default();
                let since = when
                    .strip_prefix("Unreachable ")
                    .map(|w| w.split('.').next().unwrap_or("").to_string());
                parts.push(view! { <span class="nw"><span class="w-ink">{format!("{name} {}", m.state.replace('-', " "))}</span>{since.map(|s| format!(" {s}"))}</span> }.into_any());
            }
        }
        for m in list.iter().filter(|m| m.state == "ok") {
            if let Some(w) = m.warnings.first() {
                let short = m
                    .disk
                    .first()
                    .and_then(|d| d.free_gb)
                    .filter(|f| *f < 50.0)
                    .map(|_| format!("{} disk under 50 GB", m.label))
                    .unwrap_or_else(|| w.clone());
                parts.push(view! { <span class="nw" data-tone="warn">{short}</span> }.into_any());
            }
        }
        if parts.len() < list.len() || parts.is_empty() {
            parts.push(view! { <span class="nw">{if parts.is_empty() { "All normal" } else { "the rest normal" }}</span> }.into_any());
        }
        parts
            .into_iter()
            .enumerate()
            .map(|(i, part)| view! { {(i > 0).then_some(" · ")}{part} })
            .collect_view()
    };
    view! {
        <FoldedPlate class="sec-machines">
            <FoldHead>
                <h2>"Machines"</h2>
                <span class="plate-meta">"CPU, memory, disk and test browsers"</span>
                <Chev/>
                <p class="sec-sum not-wide">{summary}</p>
            </FoldHead>
            {move || source_note(source.get(), "No reading from the machines yet.")}
            {move || also.get().into_iter().map(|p| view! { <p class="plate-note problem">{p}</p> }).collect_view()}
            <div class="mach-table">
                <div class="colhead"><span class="cap">"Machine"</span><span class="cap">"CPU busy"</span><span class="cap">"Memory"</span><span class="cap">"Disk"</span><span class="cap">"Browsers and recordings"</span></div>
                <div class="rows">{move || rows.get().into_iter().map(|m| view! { <MachineRow m=m/> }).collect_view()}</div>
            </div>
            <div class="mach-cards rows">{move || rows.get().into_iter().map(|m| view! { <MachineCard m=m/> }).collect_view()}</div>
        </FoldedPlate>
    }
}

fn cpu_meter(m: &Machine) -> impl IntoView + use<> {
    let cpu = m.cpu.clone().unwrap_or_default();
    match (cpu.busy_pct, cpu.cores) {
        (Some(b), Some(c)) => view! { <Meter value=fmt::pct(b) unit=format!("of {c} cores") used=b/> }.into_any(),
        (Some(b), None) => view! { <Meter value=fmt::pct(b) unit="busy".to_string() used=b/> }.into_any(),
        (None, _) => match (cpu.load1, cpu.cores) {
            (Some(l), Some(c)) => view! { <Meter value=format!("{l:.1}") unit=format!("load of {c} cores") used=None/> }.into_any(),
            _ => view! { <Meter value="-".to_string() unit="Not reported".to_string() used=None/> }.into_any(),
        },
    }
}

fn mem_meter(m: &Machine) -> impl IntoView + use<> {
    let mem = m.memory.clone().unwrap_or_default();
    match (mem.available_gb, mem.total_gb) {
        (Some(a), Some(t)) => view! { <Meter value=gb_free(a) unit=format!("free of {}", number_words(t)) used=free_share(Some(a), Some(t))/> }.into_any(),
        (Some(a), None) => view! { <Meter value=gb_free(a) unit="free".to_string() used=None/> }.into_any(),
        _ => view! { <Meter value="-".to_string() unit="Not reported".to_string() used=None/> }.into_any(),
    }
}

fn disk_meter(m: &Machine) -> impl IntoView + use<> {
    let disk = m.disk.first().cloned().unwrap_or_default();
    match (disk.free_gb, disk.total_gb) {
        (Some(f), Some(t)) => view! { <Meter value=gb_free(f) unit=format!("free of {}", number_words(t)) used=free_share(Some(f), Some(t))/> }.into_any(),
        (Some(f), None) => view! { <Meter value=gb_free(f) unit="free".to_string() used=None/> }.into_any(),
        _ => view! { <Meter value="-".to_string() unit="Not reported".to_string() used=None/> }.into_any(),
    }
}

fn slots_note(m: &Machine, live: Live) -> Option<String> {
    m.slots_source
        .as_ref()
        .and_then(|s| s.reading_words(live.elapsed_seconds()))
}

#[component]
fn MachineRow(m: Machine) -> impl IntoView {
    let live = expect_context::<Live>();
    let dim = m.state != "ok";
    let (browsers, recordings) = slots_lines(&m);
    let note = slots_note(&m, live);
    let notes = machine_notes(&m);
    view! {
        <div class="row" class:dim=dim>
            <div class="mach-id">
                <PlatformGlyph id=m.id.clone()/>
                <div><div class="mach-name">{m.label.clone()}</div>{subtitle(&m.id).map(|s| view! { <div class="mach-sub">{s}</div> })}</div>
            </div>
            {cpu_meter(&m)}
            {mem_meter(&m)}
            {disk_meter(&m)}
            <div class="mach-slots">{browsers}<br/>{recordings}{note.map(|n| format!(" · {n}"))}</div>
            {notes.into_iter().map(|n| view! { <p class="row-note">{n}</p> }).collect_view()}
        </div>
    }
}

#[component]
fn MachineCard(m: Machine) -> impl IntoView {
    let dim = m.state != "ok";
    let cpu = m.cpu.clone().unwrap_or_default();
    let mem = m.memory.clone().unwrap_or_default();
    let disk = m.disk.first().cloned().unwrap_or_default();
    let cpu_v = match (cpu.busy_pct, cpu.cores) {
        (Some(b), Some(c)) => format!("{} of {c}", fmt::pct(b)),
        (Some(b), None) => fmt::pct(b),
        _ => "Not reported".to_string(),
    };
    let mem_share = free_share(mem.available_gb, mem.total_gb);
    let disk_share = free_share(disk.free_gb, disk.total_gb);
    let bar = |pct: Option<f64>| match pct {
        Some(p) => view! { <Track pct=p kind=severity(p) thin=true/> }.into_any(),
        None => view! { <TrackNa thin=true/> }.into_any(),
    };
    let (browsers, recordings) = slots_lines(&m);
    let notes = machine_notes(&m);
    view! {
        <div class="mcard" class:dim=dim>
            <div class="mach-id">
                <PlatformGlyph id=m.id.clone()/>
                <div><div class="mach-name">{m.label.clone()}</div>{subtitle(&m.id).map(|s| view! { <div class="mach-sub">{s}</div> })}</div>
            </div>
            {notes.into_iter().map(|n| view! { <p style="font-size:13px">{n}</p> }).collect_view()}
            <div class="mrow"><span class="k">"CPU"</span>{bar(cpu.busy_pct)}<span class="v">{cpu_v}</span></div>
            <div class="mrow"><span class="k">"Memory"</span>{bar(mem_share)}<span class="v">{mem.available_gb.map(|a| format!("{} free", gb_free(a))).unwrap_or_else(|| "Not reported".into())}</span></div>
            <div class="mrow"><span class="k">"Disk"</span>{bar(disk_share)}<span class="v">{disk.free_gb.map(|f| format!("{} free", gb_free(f))).unwrap_or_else(|| "Not reported".into())}</span></div>
            <div class="mrow"><span class="k">"Browsers"</span><span class="q2" style="font-size:12.5px">{format!("{browsers} · {recordings}")}</span><span></span></div>
        </div>
    }
}

// ---- Phones ----------------------------------------------------------------------------------------------------

fn platform_label(platform: &str) -> &'static str {
    match platform {
        "ios" => "iPhone",
        "android" => "Android",
        _ => "Other",
    }
}

#[component]
fn PhonesPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_named(live, |v| v.simlock.source.clone());
    let simlock = Memo::new(move |_| {
        live.view
            .with(|v| v.as_ref().map(|v| v.simlock.clone()).unwrap_or_default())
    });
    let summary = move || {
        let s = simlock.get();
        match s.leases.first() {
            Some(l) => {
                let until = l
                    .expires_at
                    .as_deref()
                    .map(|t| format!(" until {}", fmt::clock(t)))
                    .unwrap_or_default();
                let n = s.leases.iter().filter(|x| x.platform == l.platform).count();
                let kind = match l.platform.as_str() {
                    "ios" => "iPhone simulator",
                    "android" => "Android emulator",
                    _ => "phone",
                };
                view! { <span>{format!("{n} {kind}{} in use{until}", if n == 1 { "" } else { "s" })}</span> }.into_any()
            }
            None => view! { <span>"No phone in use"</span> }.into_any(),
        }
    };
    view! {
        <FoldedPlate class="sec-phones">
            <FoldHead>
                <h2>"Phones"</h2>
                <span class="plate-meta">"Simlock"</span>
                <Chev/>
                <p class="sec-sum not-wide">{summary}</p>
            </FoldHead>
            {move || source_note(source.get(), "No reading from Simlock yet.")}
            {move || {
                let s = simlock.get();
                view! {
                    <div class="ph-cap rows">
                        {s.capacity.iter().map(|c| {
                            let words = c.running.zip(c.max);
                            view! {
                                <div>
                                    <span class="cap">{platform_label(&c.platform)}</span>
                                    {match words {
                                        Some((r, m)) => view! { <p class="big" data-tone=c.over_limit.then_some("crit")>{r.to_string()}" "<small>{format!("of {m}{}", if c.platform == "ios" && r > 0 { " in use" } else { "" })}</small></p> }.into_any(),
                                        None => view! { <p class="big"><small>"Not reported"</small></p> }.into_any(),
                                    }}
                                </div>
                            }
                        }).collect_view()}
                    </div>
                    {s.leases.into_iter().map(|l| {
                        let since = l.since.as_deref().map(|t| format!(", since {}", fmt::clock(t))).unwrap_or_default();
                        let until = l.expires_at.as_deref().map(|t| format!(" The lease ends at {}.", fmt::clock(t))).unwrap_or_default();
                        view! {
                            <p class="lease rows">
                                <b>{l.device}</b>
                                {if l.last_known { format!(" was last known leased to {}{since}.{until} Its worker is unreachable.", l.holder) } else { format!(" is leased to {}{since}.{until}", l.holder) }}
                            </p>
                        }
                    }).collect_view()}
                }
            }}
        </FoldedPlate>
    }
}

// ---- Spend by stream -------------------------------------------------------------------------------------------

/// A cell's bar and text. `small` draws a trace share in the quieter weight.
fn spend_cell(cell: &Option<SpendCell>, hue: &str) -> impl IntoView + use<> {
    match cell {
        None => view! { <td class="cellnone">"–"</td> }.into_any(),
        Some(c) => {
            let share = c.share;
            let hue_class = fill_of(hue);
            let small = share.is_some_and(|s| s < 0.02);
            let text = c.text.clone();
            let pct = share.map(|s| (s * 100.0).min(100.0));
            view! {
                <td>
                    <div class="cellbar">
                        {match pct {
                            Some(p) => view! { <Track pct=p kind=hue_class thin=true/> }.into_any(),
                            None => view! { <TrackNa thin=true/> }.into_any(),
                        }}
                        <span class="v" class:small=small>{text}</span>
                    </div>
                </td>
            }
            .into_any()
        }
    }
}

/// A label whose words each stay whole, so "Claude 5-hour" breaks at the space, never inside "5-hour".
fn whole_words(label: &str) -> impl IntoView + use<> {
    label
        .split(' ')
        .map(|word| view! { <span class="nw">{word.to_string()}</span>" " })
        .collect_view()
}

fn fill_of(hue: &str) -> &'static str {
    match hue {
        "claude" => "claude",
        "codex" => "codex",
        _ => "ink",
    }
}

fn credits_cell(credits: &[SpendCreditCell]) -> impl IntoView + use<> {
    let items = credits.to_vec();
    if items.is_empty() {
        return view! { <td class="credits"><span class="cellnone">"–"</span></td> }.into_any();
    }
    let count = items.len();
    view! {
        <td class="credits">
            {items.into_iter().enumerate().map(|(i, c)| view! {
                {c.text}" "<small>{c.account_label}</small>
                {(i + 1 < count).then(|| view! { <br/> })}
            }).collect_view()}
        </td>
    }
    .into_any()
}

fn spend_block(row: &SpendRowView, columns: &[SpendColumn]) -> impl IntoView + use<> {
    let bars = row.block_order.iter().filter_map(|&i| {
        let cell = row.cells.get(i)?.clone()?;
        let col = columns.get(i)?.clone();
        let share = cell.share;
        // two limits of one account can share a short name (the weekly and the Fable weekly): then the long one
        let shared = columns.iter().filter(|c| c.short == col.short).count() > 1;
        let key = if shared {
            format!("{} · {}", col.label, col.account_label)
        } else {
            col.short.clone()
        };
        Some(view! {
            <div class="sbar">
                <span class="k">{whole_words(&key)}</span>
                {match share {
                    Some(s) => view! { <Track pct=(s * 100.0).min(100.0) kind=fill_of(&col.hue) thin=true/> }.into_any(),
                    None => view! { <TrackNa thin=true/> }.into_any(),
                }}
                <span class="v">{cell.text}</span>
            </div>
        })
    }).collect_view();
    let credits = (!row.credits.is_empty()).then(|| {
        let words = row.credits.iter().map(|c| format!("{} {}", c.text, c.account_label)).collect::<Vec<_>>().join(" · ");
        view! { <div class="sbar"><span class="k">"Codex credits"</span><span class="q2" style="font-size:12.5px">{words}</span><span></span></div> }
    });
    view! {
        <div class="sblock">
            <div class="sblock-head"><span class="nm">{row.stream.clone()}</span><span class="q3" style="font-size:12.5px">{row.label.clone()}</span></div>
            {bars}
            {credits}
        </div>
    }
}

/// The limit columns only bucket rows ("Not tied to a stream") fill, when there are stream rows at all.
fn folded_columns(rows: &[SpendRowView], count: usize) -> Vec<usize> {
    let streams = rows.iter().filter(|r| !r.bucket).collect::<Vec<_>>();
    if streams.is_empty() {
        return Vec::new();
    }
    (0..count)
        .filter(|&i| {
            streams
                .iter()
                .all(|r| r.cells.get(i).is_none_or(|c| c.is_none()))
                && rows
                    .iter()
                    .any(|r| r.bucket && r.cells.get(i).is_some_and(|c| c.is_some()))
        })
        .collect()
}

#[component]
fn SpendPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let data = Memo::new(move |_| {
        live.view
            .with(|v| v.as_ref().and_then(|v| v.spend_by_stream.clone()))
    });
    let source = source_of(live, "quota-watch");
    let period = RwSignal::new(None::<String>);
    let current_id = Signal::derive(move || {
        data.with(|d| {
            let d = d.as_ref()?;
            let want = period.get().or_else(|| d.default_period.clone());
            d.periods
                .iter()
                .find(|p| Some(&p.id) == want.as_ref())
                .or(d.periods.first())
                .map(|p| p.id.clone())
        })
        .unwrap_or_default()
    });
    let current = Memo::new(move |_| {
        data.with(|d| {
            let d = d.as_ref()?;
            let id = current_id.get();
            d.periods.iter().find(|p| p.id == id).cloned()
        })
    });
    let opts = Signal::derive(move || {
        data.with(|d| {
            d.as_ref()
                .map(|d| {
                    d.periods
                        .iter()
                        .map(|p| (p.id.clone(), p.label.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let on_select = Callback::new(move |id: String| period.set(Some(id)));
    let show_all = RwSignal::new(false);
    view! {
        <FoldedPlate class="sec-spend">
            <FoldHead>
                <h2>"Spend by stream"</h2>
                <span class="plate-meta">"Estimates from quota-watch"</span>
                <div class="plate-tools only-wide" on:click=|e| e.stop_propagation()>
                    {move || (opts.get().len() > 1).then(|| view! { <Segmented options=opts.get() selected=current_id on_select=on_select label="Period"/> })}
                </div>
                <Chev/>
                <p class="sec-sum not-wide">{move || match current.get() {
                    Some(p) => runs_view(p.summary).into_any(),
                    None => view! { <span>"Unavailable."</span> }.into_any(),
                }}</p>
            </FoldHead>
            {move || source_note(source.get().filter(|s| !s.is_ok() && s.state != "missing"), "")}
            {move || match (data.get(), current.get()) {
                (None, _) => view! { <p class="plate-note">"Spend by stream isn't available from this server yet."</p> }.into_any(),
                (Some(d), _) if d.state == "unavailable" || d.periods.is_empty() => view! { <p class="plate-note">{d.unavailable.clone().unwrap_or_else(|| "Spend by stream is unavailable.".into())}</p> }.into_any(),
                (Some(_), None) => view! { <p class="plate-note">"Spend by stream is unavailable."</p> }.into_any(),
                (Some(d), Some(p)) => {
                    let columns = p.columns.clone();
                    // Limits only the untied row used fold into that row, so the table keeps to the plate.
                    let folded = folded_columns(&p.rows, columns.len());
                    let kept = (0..columns.len()).filter(|i| !folded.contains(i)).collect::<Vec<_>>();
                    let head = kept.iter().map(|&i| { let c = &columns[i]; view! { <th><span class="cap">{whole_words(&c.label)}</span><span class="acct">{c.account_label.clone()}</span></th> } }).collect_view();
                    let credits_col = p.credits.clone();
                    let span = kept.len() + usize::from(p.credits.is_some());
                    let body = p.rows.iter().map(|r| {
                        let hues: Vec<String> = columns.iter().map(|c| c.hue.clone()).collect();
                        let also = folded.iter().filter_map(|&i| {
                            let cell = r.cells.get(i)?.as_ref()?;
                            let c = &columns[i];
                            Some(format!("{} · {} {}", c.label, c.account_label, cell.text))
                        }).collect::<Vec<_>>();
                        view! {
                            <tr class:bucket=r.bucket>
                                <td class="sname">{r.stream.clone()}<small>{r.label.clone()}</small></td>
                                {kept.iter().map(|&i| spend_cell(r.cells.get(i).unwrap_or(&None), hues.get(i).map(String::as_str).unwrap_or(""))).collect_view()}
                                {p.credits.is_some().then(|| credits_cell(&r.credits))}
                            </tr>
                            {(!also.is_empty()).then(|| view! {
                                <tr class="fold" class:bucket=r.bucket><td></td><td colspan=span.to_string()>
                                    <span class="q3">"Also: "</span>
                                    {also.iter().enumerate().map(|(n, a)| view! { {(n > 0).then_some(" · ")}<span class="nw">{a.clone()}</span> }).collect_view()}
                                </td></tr>
                            })}
                        }
                    }).collect_view();
                    let top = p.rows.iter().filter(|r| !r.bucket).take(4).cloned().collect::<Vec<_>>();
                    let top_ids = top.iter().map(|r| r.stream.clone()).collect::<Vec<_>>();
                    let rest = p.rows.iter().filter(|r| !top_ids.contains(&r.stream)).cloned().collect::<Vec<_>>();
                    let cols2 = columns.clone();
                    let cols3 = columns.clone();
                    view! {
                        // below 760 px the period switch sits in the body (the head's tools are hidden there)
                        {(opts.get_untracked().len() > 1).then(|| view! {
                            <div class="spend-period not-wide rows"><Segmented options=opts.get_untracked() selected=current_id on_select=on_select label="Period"/></div>
                        })}
                        <p class="spend-lead rows">{runs_view(p.lead.clone())}</p>
                        <div class="spend-table rows">
                            <table>
                                <thead><tr><th><span class="cap">"Stream"</span></th>{head}{credits_col.map(|c| view! { <th><span class="cap">{c.label}</span><span class="acct">{c.account_label}</span></th> })}</tr></thead>
                                <tbody>{body}</tbody>
                            </table>
                        </div>
                        <div class="spend-blocks rows">
                            {move || top.iter().map(|r| spend_block(r, &cols2)).collect_view()}
                            {p.more.clone().map(|more| view! {
                                <Show when=move || !show_all.get() fallback=|| ()>
                                    <div class="sblock"><button class="disclosure" on:click=move |_| show_all.set(true)><Icon name="chev-right"/>{more.clone()}</button></div>
                                </Show>
                            })}
                            <Show when=move || show_all.get()>
                                {rest.iter().map(|r| spend_block(r, &cols3)).collect_view()}
                            </Show>
                        </div>
                        <div class="spend-key rows">{p.key.iter().map(|k| view! { <span>{k.clone()}</span> }).collect_view()}</div>
                        <FootNote text=p.foot.clone() when=d.as_of_text.clone().unwrap_or_default()/>
                    }.into_any()
                }
            }}
        </FoldedPlate>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(stream: &str, bucket: bool, cells: &[bool]) -> SpendRowView {
        SpendRowView {
            stream: stream.into(),
            bucket,
            cells: cells
                .iter()
                .map(|&has| has.then(SpendCell::default))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_refusal_the_server_explains_is_shown_in_its_words() {
        let e = crate::api::answer_error(
            403,
            r#"{"error":"signin_required","message":"These tabs need dashboard sign-in turned on."}"#,
        );
        assert_eq!(e.sentence(), "These tabs need dashboard sign-in turned on.");
        // any other 403, or an answer that isn't the refusal's shape, keeps the code
        assert_eq!(
            crate::api::answer_error(403, r#"{"error":"forbidden"}"#),
            FetchError::Server(403)
        );
        assert_eq!(
            crate::api::answer_error(500, "oops"),
            FetchError::Server(500)
        );
    }

    #[test]
    fn limits_only_the_untied_row_used_fold_into_it() {
        let rows = vec![
            row("E", false, &[false, true, false]),
            row("F", false, &[false, true, true]),
            row("Not tied to a stream", true, &[true, true, false]),
        ];
        assert_eq!(folded_columns(&rows, 3), vec![0]);
        // no stream rows: nothing folds (the table is the untied row alone)
        assert!(folded_columns(&rows[2..], 3).is_empty());
    }

    #[test]
    fn a_silent_pin_is_one_with_no_report_and_no_refusal() {
        let mut p = PinnedUpdate {
            report_state: "none".into(),
            ..Default::default()
        };
        assert!(is_silent(&p));
        p.refusal = Some(StatusProblem {
            file: "mac.flash/status.json".into(),
            reason: "looping.state is missing or not valid".into(),
        });
        assert!(!is_silent(&p));
        p.refusal = None;
        p.report_state = "fresh".into();
        assert!(!is_silent(&p));
    }

    #[test]
    fn a_refused_pin_from_the_composer_is_a_card_that_says_why() {
        // the composer sends a pin's refusal only with its cached last good copy (reportState "unreadable")
        let json = r#"{"updates":[{"id":"t1","title":"One","reportState":"unreadable","unreadableNote":"Can't read the newest report.",
            "refusal":{"file":"mac.flash/status.json","reason":"looping.state is missing or not valid.","sinceMs":1}},
            {"id":"t2","title":"Two","reportState":"stale"},{"id":"t3","title":"Three","reportState":"none"}],
            "statusProblems":[{"file":"mac.flash/status.json","reason":"looping.state is missing or not valid."},{"file":null,"reason":"seq went backwards"}]}"#;
        let panel: PinnedPanel = serde_json::from_str(json).expect("panel reads");
        let pin = &panel.updates[0];
        assert!(!is_silent(pin) && !is_quiet(pin));
        assert_eq!(
            refusal_reason(pin).as_deref(),
            Some("looping.state is missing or not valid")
        );
        let (_, shown): (Vec<_>, Vec<_>) = panel.updates.iter().cloned().partition(is_silent);
        let cards = card_pins(&shown);
        assert_eq!(
            cards.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["t1"]
        );
        // a refusal with no file name still says what happened
        assert_eq!(refused_head(""), "A status file was refused:");
        assert_eq!(
            refused_head("mac.flash/status.json"),
            "mac.flash/status.json was refused:"
        );
    }

    #[test]
    fn the_progress_head_and_body_ask_the_same_task_list_question() {
        let none = vec![Project {
            id: "p".into(),
            ..Default::default()
        }];
        assert!(!has_task_lists(&none));
        let with_counts = vec![Project {
            counts: Some(Default::default()),
            ..Default::default()
        }];
        assert!(has_task_lists(&with_counts));
        let with_stream = vec![Project {
            streams: vec![crate::api::Stream {
                counts: Some(Default::default()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        assert!(has_task_lists(&with_stream));
    }

    #[test]
    fn null_facts_and_refusals_read_without_failing_the_panel() {
        let json = r#"{"head":"2 pinned in T3","counts":{"pinned":2},"statusProblems":[{"file":"a/status.json","reason":"seq went backwards","sinceMs":1}],
            "updates":[{"id":"t1","title":"One","reportState":"none","blocked":null,"looping":null,"refusal":{"file":"b/status.json","reason":"x","sinceMs":null}},
                       {"id":"t2","title":"Two","reportState":"fresh","blocked":{"text":"","waitingOnYou":false},"looping":{"answer":"cantTell","text":""}}]}"#;
        let panel: PinnedPanel = serde_json::from_str(json).expect("panel reads");
        assert_eq!(panel.status_problems.len(), 1);
        assert_eq!(
            panel.updates[0].refusal.as_ref().map(|r| r.file.as_str()),
            Some("b/status.json")
        );
        assert!(
            panel.updates[0].blocked.text.is_empty() && panel.updates[1].looping.text.is_empty()
        );
    }
}
