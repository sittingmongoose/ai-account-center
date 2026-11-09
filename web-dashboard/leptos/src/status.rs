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

fn source_named(live: Live, id_of: impl Fn(&StatusView) -> String + Send + Sync + 'static) -> Signal<Option<Source>> {
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
        summary: Signal::derive(move || live.view.with(|v| v.as_ref().map(headline_runs).unwrap_or_default())),
        refresh: Callback::new(move |_| live.reload()),
    };
    let banner = move || {
        let error = live.error.get()?;
        let old = if live.view.with(|v| v.is_some()) {
            format!(" Showing the reading from {}.", fmt::age(live.elapsed_seconds()))
        } else {
            String::new()
        };
        Some(view! { <p class="banner" role="status">{error.sentence()}{old}</p> })
    };
    view! {
        <Shell embedded=embedded page="Status" title="Status" info=info>
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
                </Show>
            </div>
        </Shell>
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
    if b.what.is_empty() { who.to_string() } else { format!("{who}: {}", b.what) }
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
    let projects = move || live.view.with(|v| v.as_ref().map(|v| v.projects.clone()).unwrap_or_default());
    let totals = move || {
        let list = projects();
        let (done, of): (u32, u32) = list.iter().filter_map(|p| p.counts.as_ref()).fold((0, 0), |(d, o), c| {
            (d + c.landed + c.done, o + c.total.saturating_sub(c.dropped))
        });
        (done, of, list.len())
    };
    let summary = move || {
        let list = projects();
        let blocked: u32 = list.iter().filter_map(|p| p.counts.as_ref()).map(|c| c.blocked).sum();
        let no_list = list.iter().flat_map(|p| p.streams.iter()).filter(|s| s.completion.is_none() && s.counts.is_none()).count();
        let mut parts: Vec<AnyView> = Vec::new();
        if blocked > 0 {
            parts.push(view! { <span data-tone="warn">{format!("{blocked} blocked")}</span> }.into_any());
        }
        if no_list > 0 {
            let words = if no_list == 1 { "1 stream has no task list yet".to_string() } else { format!("{no_list} streams have no task list yet") };
            parts.push(view! { <span>{words}</span> }.into_any());
        }
        if parts.is_empty() {
            parts.push(view! { <span>"Nothing blocked"</span> }.into_any());
        }
        parts.into_iter().enumerate().map(|(i, part)| view! { {(i > 0).then_some(" · ")}{part} }).collect_view()
    };
    view! {
        <FoldedPlate class="sec-progress">
            <FoldHead>
                <h2>"Progress"</h2>
                <span class="plate-meta">{move || { let (d, o, n) = totals(); view! { <b>{format!("{d} of {o}")}</b>{format!(" tasks done in {n} {}", if n == 1 { "project" } else { "projects" })}} }}</span>
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

#[component]
fn ProjectView(project: Project) -> impl IntoView {
    let total = project.counts.as_ref().map(|c| format!("{} of {}", c.landed + c.done, c.total.saturating_sub(c.dropped)));
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
    let label = if stream.label.is_empty() { stream.id.clone() } else { stream.label.clone() };
    let items = stream.items.clone();
    let body = match (stream.completion.as_ref().and_then(|c| Some((c.done?, c.of?))), stream.counts.as_ref()) {
        (Some((done, of)), Some(counts)) => {
            let finished = of > 0 && done >= of;
            let parts = counts_runs(counts);
            let pct = if of == 0 { 0.0 } else { done as f64 / of as f64 * 100.0 };
            let landed = stream.items.iter().filter_map(|i| i.landed.as_ref().and_then(|l| l.at.clone())).max();
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
            let board_age = stream.board.as_ref().and_then(|b| b.age_seconds).map(fmt::long_age);
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
        Some(rest) => view! { <span data-tone=tone>{word.to_string()}</span>{rest.to_string()} }.into_any(),
        None => view! { <span data-tone=tone>{text.to_string()}</span> }.into_any(),
    }
}

fn blocked_fact(b: &PinnedBlocked) -> AnyView {
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
    match l.answer.as_str() {
        "yes" => lead_word(&l.text, "Yes.", "crit"),
        "cantTell" => view! { <span class="q3">{l.text.clone()}</span> }.into_any(),
        _ => view! { <span>{l.text.clone()}</span> }.into_any(),
    }
}

/// The one-line state of a pinned thread in its row ("Looping on F-07 · waiting on you · 6 min ago").
fn row_line(p: &PinnedUpdate) -> AnyView {
    let mut parts: Vec<AnyView> = Vec::new();
    if p.report_state == "stale" {
        return view! { <span class="q3">"Last update "{p.updated.as_ref().map(|u| u.text.clone()).unwrap_or_default().replace("Updated ", "")}</span> }.into_any();
    }
    if p.report_state == "none" {
        return view! { <span class="q3">"Doesn't report its status yet"</span> }.into_any();
    }
    let looping = p.looping.answer == "yes";
    let waiting = !(p.blocked.text == "None." || p.blocked.text == "None");
    if looping {
        let first = p.left.first().map(|i| i.id.clone()).unwrap_or_default();
        parts.push(view! { <span><span data-tone="crit">"Looping"</span>{(!first.is_empty()).then(|| format!(" on {first}"))}</span> }.into_any());
    }
    if waiting {
        let words = if p.blocked.waiting_on_you {
            if looping { "waiting on you".to_string() } else { "Waiting on you".to_string() }
        } else {
            let short = p.blocked.text.split(':').next().unwrap_or(&p.blocked.text).trim_end_matches('.');
            if looping { lowercase_first(short) } else { short.to_string() }
        };
        parts.push(view! { <span data-tone="warn">{words}</span> }.into_any());
    }
    if !looping {
        if !waiting {
            parts.push(view! { <span>"On track"</span> }.into_any());
        }
        parts.push(view! { <span>{if p.looping.answer == "cantTell" { "can't tell if it's looping" } else { "not looping" }}</span> }.into_any());
    }
    if let Some(u) = p.updated.as_ref() {
        parts.push(view! { <span>{u.text.replace("Updated ", "")}</span> }.into_any());
    }
    let last = parts.len().saturating_sub(1);
    parts.into_iter().enumerate().map(|(i, part)| view! { {part}{(i < last).then_some(" · ")} }).collect_view().into_any()
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
    let (shown, more) = (items.iter().take(limit).cloned().collect::<Vec<_>>(), items.len().saturating_sub(limit));
    view! {
        <ul>
            {shown.into_iter().map(|i| view! { <li><span class="mono">{i.id}</span>{i.title}</li> }).collect_view()}
            {(more > 0).then(|| view! { <li class="more">{format!("and {more} more")}</li> })}
        </ul>
    }
}

/// The parts of an update every width shows: progress, facts and task lists. `limit` caps the task lists (cards do,
/// the sheet doesn't). `updated_fact` adds "Updated" to the facts, as the sheet does.
fn pin_body(p: &PinnedUpdate, left_limit: usize, done_limit: usize, updated_fact: bool) -> impl IntoView + use<> {
    let pct = pct_value(p);
    let percent = p.percent.clone();
    let of_text = percent.as_ref().and_then(|x| Some(format!("{} of {} tasks done", x.done?, x.of?))).or_else(|| p.percent_note.clone());
    let note = if percent.as_ref().is_some_and(|x| x.done.is_some()) { p.percent_note.clone() } else { None };
    let left = p.left.clone();
    let done = p.done.clone();
    let blocked = blocked_fact(&p.blocked);
    let looping = looping_fact(&p.looping);
    let now = p.now.clone();
    let updated = updated_fact.then(|| p.updated.as_ref().map(|u| u.text.replace("Updated ", ""))).flatten();
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
        <div class="tasks">
            <div class="left"><h4 class="cap">{format!("Left · {}", left.len())}</h4>{item_list(&left, left_limit)}</div>
            <div class="done"><h4 class="cap">{format!("Done · {}", done.len())}</h4>{item_list(&done, done_limit)}</div>
        </div>
    }
}

/// A pinned thread with no current report: says so plainly, with what it last said. The server's note reads
/// "No status update for 25 days. If it is finished, unpin it in T3.": the second sentence goes last.
fn quiet_body(p: &PinnedUpdate) -> impl IntoView + use<> {
    let note = p.stale_note.clone().or_else(|| p.report_note.clone()).or_else(|| p.unreadable_note.clone()).unwrap_or_default();
    let (first, rest) = match note.split_once(". ") {
        Some((a, b)) => (format!("{a}."), Some(b.to_string())),
        None => (note.clone(), None),
    };
    let summary = p.summary.clone();
    view! {
        <p class="q2" style="font-size:13px;line-height:1.5">
            <span class="w-ink">{first}</span>
            {summary.map(|s| format!(" The last one said: {s}"))}
        </p>
        {rest.map(|r| view! { <p class="q3" style="font-size:12.5px">{r}</p> })}
    }
}

fn is_quiet(p: &PinnedUpdate) -> bool {
    p.report_state == "stale" || p.report_state == "none"
}

#[component]
fn PinCard(update: PinnedUpdate) -> impl IntoView {
    let quiet = is_quiet(&update);
    let who = update.who.clone();
    let foot = update.updated.as_ref().map(|u| {
        if quiet { format!("Last update {}", fmt::clock(&u.at)) } else { u.text.clone() }
    });
    let body = if quiet { quiet_body(&update).into_any() } else { pin_body(&update, 4, 2, false).into_any() };
    view! {
        <article class="pin" class:quiet=quiet>
            <div><h3 class="pin-name">{update.title.clone()}</h3>{who.map(|w| view! { <p class="pin-who">{w}</p> })}</div>
            {body}
            <div class="pin-foot"><span>{foot}</span></div>
        </article>
    }
}

#[component]
fn PinRow(update: PinnedUpdate, on_open: Callback<PinnedUpdate>) -> impl IntoView {
    let u = update.clone();
    let pct_text = update.percent.as_ref().map(|p| p.text.trim_end_matches('%').to_string());
    let bar = pct_value(&update);
    let quiet = is_quiet(&update);
    let line = row_line(&update);
    view! {
        <button class="pin-row" on:click=move |_| on_open.run(u.clone())>
            <span class="pr-name" class:q2=quiet>{update.title.clone()}</span>
            {match pct_text {
                Some(t) => view! { <span class="pr-pct">{t}<small>"%"</small></span> }.into_any(),
                None => view! { <span class="pr-pct q3" style="font-size:13px">"no update"</span> }.into_any(),
            }}
            {bar.map(|v| view! { <Track pct=v kind="acc" thin=true/> })}
            <p class="pr-line" class:q3=quiet>{line}</p>
        </button>
    }
}

#[component]
fn PinSheet(update: PinnedUpdate, on_close: Callback<()>) -> impl IntoView {
    let quiet = is_quiet(&update);
    let who = update.who.clone();
    let body = if quiet { quiet_body(&update).into_any() } else { pin_body(&update, 500, 500, true).into_any() };
    view! {
        <div class="sheet-host" role="dialog" aria-modal="true">
            <button class="sheet-scrim" aria-label="Close" on:click=move |_| on_close.run(())></button>
            <div class="sheet">
                <div class="grabber"></div>
                <button class="icon-btn bare sheet-close" aria-label="Close" on:click=move |_| on_close.run(())><Icon name="close"/></button>
                <article class="pin">
                    <div><h3 class="pin-name" style="font-size:18px">{update.title.clone()}</h3>{who.map(|w| view! { <p class="pin-who">{w}</p> })}</div>
                    {body}
                </article>
            </div>
        </div>
    }
}

#[component]
fn PinnedPanelView() -> impl IntoView {
    let live = expect_context::<Live>();
    let panel = Memo::new(move |_| live.view.with(|v| v.as_ref().and_then(|v| v.pinned.clone())));
    let sheet = RwSignal::new(None::<PinnedUpdate>);
    let open_sheet = Callback::new(move |u: PinnedUpdate| sheet.set(Some(u)));
    let close_sheet = Callback::new(move |_: ()| sheet.set(None));
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
                    let cards = p.updates.iter().take(6).cloned().collect::<Vec<_>>();
                    let rest = p.updates.iter().skip(6).cloned().collect::<Vec<_>>();
                    let problems = p.machine_problems.clone();
                    view! {
                        {problems.into_iter().map(|m| view! { <p class="plate-note problem">{m.text}</p> }).collect_view()}
                        <div class="pin-list">
                            {p.updates.into_iter().map(|u| view! { <PinRow update=u on_open=open_sheet/> }).collect_view()}
                        </div>
                        <div class="pin-grid">
                            {cards.into_iter().map(|u| view! { <PinCard update=u/> }).collect_view()}
                        </div>
                        {(!rest.is_empty()).then(|| view! {
                            <div class="only-d">
                                <div class="pin-more">{rest.into_iter().map(|u| view! { <PinRow update=u on_open=open_sheet/> }).collect_view()}</div>
                            </div>
                        })}
                    }.into_any()
                }
            }}
            {move || sheet.get().map(|u| view! { <PinSheet update=u on_close=close_sheet/> })}
        </section>
    }
}

/// The head's count line with its numbers bold and its problems coloured: "**5** pinned in T3 · 1 looping · 2 waiting on you".
fn head_runs(head: &str, counts: &PinnedCounts) -> impl IntoView + use<> {
    let parts: Vec<String> = head.split(" · ").map(str::to_string).collect();
    let pinned = counts.pinned;
    parts.into_iter().enumerate().map(move |(i, part)| {
        let sep = (i > 0).then_some(" · ");
        let tone = if part.contains("looping") { Some("crit") } else if part.contains("waiting") { Some("warn") } else { None };
        if i == 0 {
            let text = part.strip_prefix(&format!("{pinned}")).unwrap_or(&part).to_string();
            view! { {sep}<b>{format!("{pinned}")}</b><span>{text}</span> }.into_any()
        } else {
            view! { {sep}<span data-tone=tone>{part}</span> }.into_any()
        }
    }).collect_view()
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
    let gpu = m.gpu_pct.map(|g| format!(" · GPU {}", fmt::pct(g))).unwrap_or_default();
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
        .map(|w| view! { <span data-tone="warn">{format!("{}.", w.trim_end_matches('.'))}</span> }.into_any())
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
    let also = Memo::new(move |_| live.view.with(|v| v.as_ref().map(|v| other_problems(v, &v.machines.also)).unwrap_or_default()));
    let rows = Memo::new(move |_| live.view.with(|v| v.as_ref().map(|v| v.machines.rows.clone()).unwrap_or_default()));
    let summary = move || {
        let list = rows.get();
        let mut parts: Vec<AnyView> = Vec::new();
        for m in &list {
            if m.state != "ok" {
                let name = m.label.clone();
                let when = m.detail.clone().unwrap_or_default();
                let since = when.strip_prefix("Unreachable ").map(|w| w.split('.').next().unwrap_or("").to_string());
                parts.push(view! { <span class="nw"><span class="w-ink">{format!("{name} {}", m.state.replace('-', " "))}</span>{since.map(|s| format!(" {s}"))}</span> }.into_any());
            }
        }
        for m in list.iter().filter(|m| m.state == "ok") {
            if let Some(w) = m.warnings.first() {
                let short = m.disk.first().and_then(|d| d.free_gb).filter(|f| *f < 50.0).map(|_| format!("{} disk under 50 GB", m.label)).unwrap_or_else(|| w.clone());
                parts.push(view! { <span class="nw" data-tone="warn">{short}</span> }.into_any());
            }
        }
        if parts.len() < list.len() || parts.is_empty() {
            parts.push(view! { <span class="nw">{if parts.is_empty() { "All normal" } else { "the rest normal" }}</span> }.into_any());
        }
        parts.into_iter().enumerate().map(|(i, part)| view! { {(i > 0).then_some(" · ")}{part} }).collect_view()
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
    m.slots_source.as_ref().and_then(|s| s.reading_words(live.elapsed_seconds()))
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
    let simlock = Memo::new(move |_| live.view.with(|v| v.as_ref().map(|v| v.simlock.clone()).unwrap_or_default()));
    let summary = move || {
        let s = simlock.get();
        match s.leases.first() {
            Some(l) => {
                let until = l.expires_at.as_deref().map(|t| format!(" until {}", fmt::clock(t))).unwrap_or_default();
                let n = s.leases.iter().filter(|x| x.platform == l.platform).count();
                let kind = match l.platform.as_str() { "ios" => "iPhone simulator", "android" => "Android emulator", _ => "phone" };
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
        Some(view! {
            <div class="sbar">
                <span class="k">{col.short.clone()}</span>
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

#[component]
fn SpendPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let data = Memo::new(move |_| live.view.with(|v| v.as_ref().and_then(|v| v.spend_by_stream.clone())));
    let source = source_of(live, "quota-watch");
    let period = RwSignal::new(None::<String>);
    let current_id = Signal::derive(move || {
        data.with(|d| {
            let d = d.as_ref()?;
            let want = period.get().or_else(|| d.default_period.clone());
            d.periods.iter().find(|p| Some(&p.id) == want.as_ref()).or(d.periods.first()).map(|p| p.id.clone())
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
        data.with(|d| d.as_ref().map(|d| d.periods.iter().map(|p| (p.id.clone(), p.label.clone())).collect::<Vec<_>>()).unwrap_or_default())
    });
    let on_select = Callback::new(move |id: String| period.set(Some(id)));
    let show_all = RwSignal::new(false);
    view! {
        <FoldedPlate class="sec-spend">
            <FoldHead>
                <h2>"Spend by stream"</h2>
                <span class="plate-meta">"Estimates from quota-watch"</span>
                <div class="plate-tools only-wide" on:click=|e| e.stop_propagation()>
                    {move || (!opts.get().is_empty()).then(|| view! { <Segmented options=opts.get() selected=current_id on_select=on_select label="Period"/> })}
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
                    let head = columns.iter().map(|c| view! { <th><span class="cap">{c.label.clone()}</span><span class="acct">{c.account_label.clone()}</span></th> }).collect_view();
                    let credits_col = p.credits.clone();
                    let body = p.rows.iter().map(|r| {
                        let hues: Vec<String> = columns.iter().map(|c| c.hue.clone()).collect();
                        view! {
                            <tr class:bucket=r.bucket>
                                <td class="sname">{r.stream.clone()}<small>{r.label.clone()}</small></td>
                                {r.cells.iter().enumerate().map(|(i, c)| spend_cell(c, hues.get(i).map(String::as_str).unwrap_or(""))).collect_view()}
                                {p.credits.is_some().then(|| credits_cell(&r.credits))}
                            </tr>
                        }
                    }).collect_view();
                    let top = p.rows.iter().filter(|r| !r.bucket).take(4).cloned().collect::<Vec<_>>();
                    let top_ids = top.iter().map(|r| r.stream.clone()).collect::<Vec<_>>();
                    let rest = p.rows.iter().filter(|r| !top_ids.contains(&r.stream)).cloned().collect::<Vec<_>>();
                    let cols2 = columns.clone();
                    let cols3 = columns.clone();
                    view! {
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
