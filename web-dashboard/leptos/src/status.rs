//! The Status page: questions for the owner first, then progress, threads, machines, phones and usage.

use crate::api::*;
use crate::cards::CardsPanel;
use crate::fmt;
use crate::live::Live;
use leptos::prelude::*;

/// A panel with its source's state and age in the header. A source that is not `ok` greys the body and says why.
/// `also` are the panel's secondary sources: each one that is not `ok` is named under the header too, so a healthy
/// main source never hides them.
#[component]
pub fn Panel(
    title: &'static str,
    #[prop(into)] source: Signal<Option<Source>>,
    #[prop(optional)] id: &'static str,
    #[prop(optional)] also: Option<Signal<Vec<Source>>>,
    children: Children,
) -> impl IntoView {
    let live = expect_context::<Live>();
    let state = move || {
        source
            .get()
            .map(|s| s.state)
            .unwrap_or_else(|| "missing".into())
    };
    let age = move || {
        let s = source.get()?;
        let seconds = fmt::live_age(s.age_seconds, live.elapsed_seconds())?;
        Some(fmt::age(seconds))
    };
    let note = move || {
        let s = source.get();
        match s {
            Some(s) if s.is_ok() => None,
            Some(s) => Some(s.detail),
            None => Some("No reading from this source yet.".to_string()),
        }
    };
    view! {
        <section class="panel" id=id data-state=state>
            <header class="panel-head">
                <h2>{title}</h2>
                <span class="src" data-state=state title=move || source.get().map(|s| s.detail).unwrap_or_default()>
                    <span class="dot"></span>
                    {move || match (state().as_str(), age()) {
                        ("ok", Some(a)) => format!("Updated {a}"),
                        ("stale", Some(a)) => format!("Stale · {a}"),
                        ("stale", None) => "Stale".to_string(),
                        ("unconfigured", _) => "Not set up".to_string(),
                        ("missing", _) => "Missing".to_string(),
                        ("unreadable", _) => "Unreadable".to_string(),
                        ("error", _) => "Error".to_string(),
                        (_, Some(a)) => a,
                        _ => String::new(),
                    }}
                </span>
            </header>
            {move || note().map(|n| view! { <p class="src-note">{n}</p> })}
            {move || {
                also.map(|a| a.get())
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|s| s.problem().map(|p| (s.state, p)))
                    .map(|(state, p)| view! { <p class="src-note" data-state=state>{p}</p> })
                    .collect_view()
            }}
            <div class="panel-body">{children()}</div>
        </section>
    }
}

pub fn runs_view(runs: Vec<Run>) -> impl IntoView {
    runs.into_iter()
        .map(|r| {
            let tone = r.tone.unwrap_or_default();
            if r.strong {
                view! { <strong data-tone=tone>{r.text}</strong> }.into_any()
            } else {
                view! { <span data-tone=tone>{r.text}</span> }.into_any()
            }
        })
        .collect_view()
}

fn source_of(
    live: Live,
    id_of: impl Fn(&StatusView) -> String + Send + Sync + 'static,
) -> Signal<Option<Source>> {
    Signal::derive(move || {
        let view = live.view.get()?;
        let id = id_of(&view);
        view.sources.iter().find(|s| s.id == id).cloned()
    })
}

/// A panel's secondary sources (`also`), as the server lists them.
fn also_of(
    live: Live,
    ids_of: impl Fn(&StatusView) -> Vec<String> + Send + Sync + 'static,
) -> Signal<Vec<Source>> {
    Signal::derive(move || {
        let Some(view) = live.view.get() else {
            return Vec::new();
        };
        let ids = ids_of(&view);
        view.sources
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect()
    })
}

#[component]
pub fn StatusPage(embedded: bool) -> impl IntoView {
    let live = expect_context::<Live>();
    let banner = move || {
        let error = live.error.get()?;
        let since = live.failed_since_ms.get();
        let have_view = live.view.with(|v| v.is_some());
        let old = if have_view {
            let seconds = live.elapsed_seconds();
            format!(" Showing the reading from {}.", fmt::age(seconds))
        } else {
            String::new()
        };
        let _ = since;
        Some(
            view! { <div class="banner" role="status" data-tone="crit">{error.sentence()}{old}</div> },
        )
    };

    view! {
        <main class="page status" data-embedded=embedded.then_some("1")>
            <Show when=move || !embedded>
                <header class="page-head">
                    <h1>"Status"</h1>
                </header>
            </Show>
            {banner}
            <Show
                when=move || live.view.with(|v| v.is_some())
                fallback=move || view! {
                    <p class="empty">{move || if live.error.with(|e| e.is_some()) { "" } else { "Reading the status..." }}</p>
                }
            >
                <Headline/>
                <div class="grid" data-stale=move || live.error.with(|e| e.is_some()).then_some("1")>
                    <div class="col main"><CardsPanel/></div>
                    <div class="side">
                        <div class="col"><ProgressPanel/><ThreadsPanel/></div>
                        <div class="col"><MachinesPanel/><SimlockPanel/><UsagePanel/><SpendPanel/></div>
                    </div>
                </div>
            </Show>
        </main>
    }
}

#[component]
fn Headline() -> impl IntoView {
    let live = expect_context::<Live>();
    let headline = move || {
        live.view
            .with(|v| v.as_ref().map(|v| v.headline.clone()).unwrap_or_default())
    };
    view! {
        <div class="headline" data-tone=move || headline().tone>
            <p>{move || runs_view(headline().runs)}</p>
            <p class="as-of">
                {move || {
                    let generated = live.view.with(|v| v.as_ref().map(|v| v.generated_at.clone()).unwrap_or_default());
                    format!("As of {} · {}", fmt::clock(&generated), fmt::age(live.elapsed_seconds()))
                }}
            </p>
        </div>
    }
}

fn state_label(state: &str) -> &'static str {
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

#[component]
fn ProgressPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, |_| "agent-reports".into());
    let projects = move || {
        live.view
            .with(|v| v.as_ref().map(|v| v.projects.clone()).unwrap_or_default())
    };
    view! {
        <Panel title="Progress" source=source id="progress">
            {move || {
                let list = projects();
                if list.is_empty() {
                    return view! { <p class="empty">"No project reports yet."</p> }.into_any();
                }
                list.into_iter().map(|p| view! { <ProjectView project=p/> }).collect_view().into_any()
            }}
        </Panel>
    }
}

fn bar(done: u32, of: u32) -> impl IntoView {
    let pct = if of == 0 {
        0.0
    } else {
        (done as f64 / of as f64 * 100.0).clamp(0.0, 100.0)
    };
    view! { <span class="bar" role="img" aria-label=format!("{done} of {of}")><span style=format!("width:{pct:.1}%")></span></span> }
}

fn count_chips(c: &Counts) -> impl IntoView + use<> {
    let chips = [
        ("working", c.working),
        ("review", c.review),
        ("fixing", c.fixing),
        ("ready", c.ready),
        ("blocked", c.blocked),
        ("todo", c.todo),
    ];
    chips
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(state, n)| view! { <span class="chip" data-state=state>{format!("{n} {}", state_label(state).to_lowercase())}</span> })
        .collect_view()
}

#[component]
fn ProjectView(project: Project) -> impl IntoView {
    let summary = project.counts.as_ref().map(|c| {
        let done = c.landed + c.done;
        let of = c.total.saturating_sub(c.dropped);
        view! {
            <div class="proj-sum">
                {bar(done, of)}
                <span class="num">{format!("{done} of {of}")}</span>
                <span class="muted">" landed or done"</span>
            </div>
        }
    });
    view! {
        <div class="project">
            <h3>{project.label.clone()}</h3>
            {summary}
            <ul class="streams">
                {project.streams.into_iter().map(|s| view! { <StreamView stream=s/> }).collect_view()}
            </ul>
        </div>
    }
}

#[component]
fn StreamView(stream: Stream) -> impl IntoView {
    let open = RwSignal::new(false);
    let has_items = !stream.items.is_empty();
    let completion = match (stream.completion.as_ref().and_then(|c| Some((c.done?, c.of?))), &stream.counts) {
        (Some((done, of)), Some(counts)) => view! {
            <div class="stream-line">
                {bar(done, of)}
                <span class="num">{format!("{done} of {of}")}</span>
                {count_chips(counts)}
            </div>
        }
        .into_any(),
        _ => view! {
            <div class="stream-line muted">
                "No task report yet"
                {stream.board.as_ref().and_then(|b| b.age_seconds).map(|a| format!(" · board updated {}", fmt::age(a)))}
            </div>
        }
        .into_any(),
    };
    let items = stream.items.clone();
    view! {
        <li class="stream" data-stale=stream.stale.then_some("1")>
            <button class="stream-head" disabled=!has_items aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|o| *o = !*o)>
                <span class="stream-id">{stream.id.clone()}</span>
                <span class="stream-label">{stream.label.clone()}</span>
                <span class="muted owner">{stream.owner.clone()}</span>
                {stream.stale.then(|| view! { <span class="chip" data-state="stale">"stale"</span> })}
            </button>
            {completion}
            {stream.problem.clone().map(|p| view! { <div class="item-note" data-tone="warn">{format!("Task report unreadable: {p}")}</div> })}
            {stream.board.clone().map(|b| {
                let short = b.path.rsplit('/').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
                view! { <div class="path" title=b.path.clone()>{format!("Board: {short}")}</div> }
            })}
            <Show when=move || open.get()>
                <ul class="items">
                    {items.clone().into_iter().map(|i| {
                        let blocked = i.blocked_on.as_ref().map(blocked_words);
                        let landed = i.landed.as_ref().map(|l| format!("landed {}", l.sha.chars().take(10).collect::<String>()));
                        let reviews = i.reviews.iter().map(|r| format!("{} {}", r.by, r.verdict)).collect::<Vec<_>>().join(" · ");
                        view! {
                            <li class="item" data-state=i.state.clone()>
                                <span class="chip" data-state=i.state.clone()>{state_label(&i.state)}</span>
                                <span class="item-id">{i.id.clone()}</span>
                                <span class="item-title">{i.title.clone()}</span>
                                {blocked.map(|b| view! { <div class="item-note" data-tone="warn">{b}</div> })}
                                {landed.map(|l| view! { <div class="item-note muted">{l}</div> })}
                                {i.why.clone().filter(|w| !w.is_empty()).map(|w| view! { <div class="item-note muted">{w}</div> })}
                                {(!reviews.is_empty()).then(|| view! { <div class="item-note muted">{reviews}</div> })}
                            </li>
                        }
                    }).collect_view()}
                </ul>
            </Show>
        </li>
    }
}

#[component]
fn ThreadsPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, |v| v.threads.source.clone());
    let also = also_of(live, |v| v.threads.also.clone());
    let rows = move || {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| v.threads.rows.clone())
                .unwrap_or_default()
        })
    };
    view! {
        <Panel title="Threads" source=source id="threads" also=also>
            <ul class="threads">
                {move || {
                    let elapsed = live.elapsed_seconds();
                    rows().into_iter().map(|t| {
                        let age = fmt::live_age(t.age_seconds, elapsed).map(fmt::age).unwrap_or_default();
                        let model = [t.model.clone(), t.effort.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ");
                        let place = [t.machine.clone(), t.stream.clone().map(|s| format!("stream {s}"))].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                        let blocked = t.blocked_on.as_ref().map(blocked_words);
                        let progress = t.progress.as_ref().and_then(|p| Some(format!("{} of {} {}", p.done?, p.total?, p.unit)));
                        // A pm-mail row while the registry fails is its last good scan: say so beside its age.
                        let cached = t.registry_source.as_ref().and_then(|s| s.reading_words(elapsed));
                        let when = match (t.stale, cached) {
                            (true, Some(c)) => format!("stale · {c} · {age}"),
                            (false, Some(c)) => format!("{c} · {age}"),
                            (true, None) => format!("stale · {age}"),
                            (false, None) => age,
                        };
                        view! {
                            <li class="thread" data-state=t.state.clone() data-stale=t.stale.then_some("1")>
                                <div class="thread-top">
                                    <span class="state-dot" data-state=t.state.clone() aria-hidden="true"></span>
                                    <span class="thread-name">{t.name.clone()}</span>
                                    <span class="muted">{state_label(&t.state)}</span>
                                    <span class="muted right">{when}</span>
                                </div>
                                {t.doing.clone().map(|d| view! { <div class="thread-doing">{d}</div> })}
                                {blocked.map(|b| view! { <div class="item-note" data-tone="warn">{b}</div> })}
                                {t.problem.clone().map(|p| view! { <div class="item-note" data-tone="warn">{p}</div> })}
                                {t.clock_ahead_seconds.filter(|s| *s > 0).map(|s| view! {
                                    <div class="item-note muted">{format!("Its clock runs {} ahead of this server's; its times may look early.", fmt::span(s))}</div>
                                })}
                                <div class="thread-meta muted">
                                    {place}
                                    {(!model.is_empty()).then(|| format!(" · {model}"))}
                                    {progress.map(|p| format!(" · {p}"))}
                                    {(t.source == "pm-mail").then_some(" · from pm-mail")}
                                </div>
                            </li>
                        }
                    }).collect_view()
                }}
            </ul>
        </Panel>
    }
}

fn meter(
    label: &'static str,
    used: Option<f64>,
    of: Option<f64>,
    words: String,
    warn: bool,
) -> impl IntoView {
    let pct = match (used, of) {
        (Some(u), Some(o)) if o > 0.0 => Some((u / o * 100.0).clamp(0.0, 100.0)),
        _ => None,
    };
    view! {
        <div class="meter" data-tone=warn.then_some("warn")>
            <span class="meter-label">{label}</span>
            {match pct {
                Some(p) => view! { <span class="bar"><span style=format!("width:{p:.1}%")></span></span> }.into_any(),
                None => view! { <span class="bar missing"></span> }.into_any(),
            }}
            <span class="num">{words}</span>
        </div>
    }
}

#[component]
fn MachinesPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, |v| v.machines.source.clone());
    let also = also_of(live, |v| v.machines.also.clone());
    let rows = move || {
        live.view.with(|v| {
            v.as_ref()
                .map(|v| v.machines.rows.clone())
                .unwrap_or_default()
        })
    };
    view! {
        <Panel title="Machines" source=source id="machines" also=also>
            <div class="machines">
                {move || rows().into_iter().map(|m| {
                    // browser-check's readings carry its own age and problem, apart from the machine's.
                    let slots_cached = m.slots_source.as_ref().and_then(|s| s.reading_words(live.elapsed_seconds()));
                    let slots_state = m.slots_source.as_ref().map(|s| s.state.clone()).filter(|s| s != "ok");
                    let cpu = m.cpu.clone().unwrap_or_default();
                    let mem = m.memory.clone().unwrap_or_default();
                    let disk = m.disk.first().cloned().unwrap_or_default();
                    let mem_used = match (mem.total_gb, mem.available_gb) { (Some(t), Some(a)) => Some(t - a), _ => None };
                    let disk_used = match (disk.total_gb, disk.free_gb) { (Some(t), Some(f)) => Some(t - f), _ => None };
                    let cpu_words = match (cpu.busy_pct, cpu.load1, cpu.cores) {
                        (Some(b), _, Some(c)) => format!("{} of {c} cores", fmt::pct(b)),
                        (Some(b), _, None) => fmt::pct(b),
                        (None, Some(l), Some(c)) => format!("load {l:.1} / {c}"),
                        _ => "Not reported".into(),
                    };
                    let mem_words = mem.available_gb.map(|a| format!("{} free", fmt::gb(a))).unwrap_or_else(|| "Not reported".into());
                    let disk_words = disk.free_gb.map(|f| format!("{} free", fmt::gb(f))).unwrap_or_else(|| "Not reported".into());
                    let slots = |s: &Option<Slots>| match s.as_ref().and_then(|s| s.running.zip(s.max)) {
                        Some((r, m)) => format!("{r}/{m}"),
                        None => "-".into(),
                    };
                    let low_mem = mem.available_gb.is_some_and(|a| a < 6.0);
                    let low_disk = disk.free_gb.is_some_and(|f| f < 50.0);
                    view! {
                        <div class="machine" data-state=m.state.clone()>
                            <div class="machine-head">
                                <span class="state-dot" data-state=m.state.clone() aria-hidden="true"></span>
                                <span class="machine-name">{m.label.clone()}</span>
                                <span class="muted right">{if m.state == "ok" { String::new() } else { m.state.replace('-', " ") }}</span>
                            </div>
                            {meter("CPU", cpu.busy_pct, Some(100.0), cpu_words, false)}
                            {meter("Memory", mem_used, mem.total_gb, mem_words, low_mem)}
                            {meter("Disk", disk_used, disk.total_gb, disk_words, low_disk)}
                            <div class="machine-slots muted" data-state=slots_state>
                                {format!("Browsers {} · Recordings {}", slots(&m.browsers), slots(&m.recordings))}
                                {m.gpu_pct.map(|g| format!(" · GPU {}", fmt::pct(g)))}
                                {slots_cached.map(|c| format!(" · {c}"))}
                            </div>
                            {m.detail.clone().filter(|d| !d.is_empty()).map(|d| view! { <div class="item-note" data-tone="warn">{d}</div> })}
                            {m.warnings.into_iter().map(|w| view! { <div class="item-note" data-tone="warn">{w}</div> }).collect_view()}
                        </div>
                    }
                }).collect_view()}
            </div>
        </Panel>
    }
}

#[component]
fn SimlockPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, |v| v.simlock.source.clone());
    let simlock = move || {
        live.view
            .with(|v| v.as_ref().map(|v| v.simlock.clone()).unwrap_or_default())
    };
    view! {
        <Panel title="Phones (Simlock)" source=source id="simlock">
            {move || {
                let s = simlock();
                view! {
                    <div class="capacity">
                        {s.capacity.into_iter().map(|c| {
                            let platform = if c.platform == "ios" { "iOS" } else if c.platform == "android" { "Android" } else { "Other" };
                            let words = match (c.running, c.max) { (Some(r), Some(m)) => format!("{r} of {m} running"), _ => "Not reported".into() };
                            view! { <span class="chip" data-state=if c.over_limit { "blocked" } else { "idle" }>{format!("{platform}: {words}")}</span> }
                        }).collect_view()}
                    </div>
                    {if s.leases.is_empty() {
                        view! { <p class="empty">"No phone is leased."</p> }.into_any()
                    } else {
                        view! {
                            <ul class="leases">
                                {s.leases.into_iter().map(|l| view! {
                                    <li>
                                        <span class="lease-device">{l.device}</span>
                                        <span class="muted">{format!(" · {}", l.holder)}</span>
                                        <span class="muted right">
                                            {l.since.as_deref().map(|t| format!("since {}", fmt::clock(t)))}
                                            {l.expires_at.as_deref().map(|t| format!(" · until {}", fmt::clock(t)))}
                                        </span>
                                    </li>
                                }).collect_view()}
                            </ul>
                        }.into_any()
                    }}
                }
            }}
        </Panel>
    }
}

#[component]
fn UsagePanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let source = source_of(live, |v| v.usage.source.clone());
    let usage = move || {
        live.view
            .with(|v| v.as_ref().map(|v| v.usage.clone()).unwrap_or_default())
    };
    view! {
        <Panel title="Usage" source=source id="usage">
            {move || {
                let u = usage();
                view! {
                    <ul class="warnings">
                        {u.warnings.into_iter().map(|w| view! { <li data-tone=w.tone>{w.text}</li> }).collect_view()}
                    </ul>
                    <table class="usage">
                        <tbody>
                            {u.accounts.into_iter().map(|a| {
                                let tone = if a.at_limit { "crit" } else if a.runs_out_before_reset == Some(true) { "warn" } else { "" };
                                let used = a.used_pct.map(fmt::pct).unwrap_or_else(|| "-".into());
                                let reset = a.resets_at.as_deref().map(fmt::clock).unwrap_or_default();
                                let state = if a.at_limit {
                                    "at its limit".to_string()
                                } else if a.runs_out_before_reset == Some(true) {
                                    a.projected_full_at.as_deref().map(|t| format!("full {}", fmt::clock(t))).unwrap_or_else(|| "full before reset".into())
                                } else {
                                    String::new()
                                };
                                // a stale reading, or one from before a passed reset, says so in words, not only by fading
                                let note = a.note(state);
                                view! {
                                    <tr data-tone=tone data-stale=a.stale.then_some("1")>
                                        <th scope="row">{a.label}</th>
                                        <td class="muted">{a.window}</td>
                                        <td class="num">{used}</td>
                                        <td class="muted">{if reset.is_empty() { String::new() } else { format!("resets {reset}") }}</td>
                                        <td data-tone=tone>{note}</td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                }
            }}
        </Panel>
    }
}

/// Estimated spend per stream (quota-watch). Every number is an estimate split by token share; the panel says so.
#[component]
fn SpendPanel() -> impl IntoView {
    let live = expect_context::<Live>();
    let spend = Memo::new(move |_| live.view.with(|v| v.as_ref().and_then(|v| v.spend.clone())));
    let source = source_of(live, |v| {
        v.spend
            .as_ref()
            .map(|s| s.source.clone())
            .unwrap_or_else(|| "quota-watch".into())
    });
    let period = RwSignal::new("day".to_string());
    view! {
        <Panel title="Spend by stream (estimates)" source=source id="spend">
            {move || match spend.get() {
                None => view! { <p class="empty">"This server doesn't read quota-watch's report yet."</p> }.into_any(),
                Some(s) => {
                    let current = s.periods.iter().find(|p| p.id == period.get()).or(s.periods.first()).cloned();
                    view! {
                        <div class="sb-uses" role="tablist" aria-label="Period">
                            {s.periods.iter().map(|p| {
                                let id = p.id.clone();
                                let selected = { let id = id.clone(); move || current_id(&spend, &period) == id };
                                view! {
                                    <button role="tab" class="chip-btn" aria-selected=move || selected().to_string()
                                        on:click=move |_| period.set(id.clone())>{p.label.clone()}</button>
                                }
                            }).collect_view()}
                        </div>
                        {current.map(|p| view! {
                            {p.coverage_text.clone().map(|c| view! { <p class="item-note" data-tone="warn">{format!("This period's estimate {c}.")}</p> })}
                            <ul class="spend-streams">
                                {p.streams.into_iter().map(|st| view! {
                                    <li data-bucket=st.bucket.then_some("1")>
                                        <span class="spend-stream">{if st.label.is_empty() { st.stream.clone() } else { st.label.clone() }}</span>
                                        <ul class="spend-rows">
                                            {st.rows.into_iter().map(|r| view! {
                                                <li><span class="muted">{format!("{} · {}", r.account, r.meter_label)}</span><span class="num">{r.value_text}</span></li>
                                            }).collect_view()}
                                        </ul>
                                    </li>
                                }).collect_view()}
                            </ul>
                        })}
                        {(!s.budgets.is_empty()).then(|| view! {
                            <ul class="warnings">
                                {s.budgets.iter().map(|b| view! {
                                    <li data-tone=if b.over_limit { "crit" } else { "" }>{format!("{} · {}: {}", b.stream_label, b.desc, b.text)}</li>
                                }).collect_view()}
                            </ul>
                        })}
                        {(!s.machines_not_scanned.is_empty()).then(|| view! {
                            <p class="item-note" data-tone="warn">{format!("Not scanned: {}. Their use sits inside the other estimates.",
                                s.machines_not_scanned.iter().map(|m| m.name.clone()).collect::<Vec<_>>().join(", "))}</p>
                        })}
                        {(!s.unattributed.is_empty()).then(|| view! {
                            <details class="sb-thin">
                                <summary>{format!("Not attributed to a stream ({})", s.unattributed.len())}</summary>
                                <ul class="spend-rows">
                                    {s.unattributed.iter().map(|u| view! {
                                        <li><span class="muted">{format!("{} · {} · {}", u.account, u.meter_label, u.reason)}</span><span class="num">{u.amount_text.clone()}</span></li>
                                    }).collect_view()}
                                </ul>
                            </details>
                        })}
                        {(!s.method.is_empty()).then(|| view! { <p class="fine">{format!("How: {}", s.method)}</p> })}
                    }.into_any()
                }
            }}
        </Panel>
    }
}

fn current_id(spend: &Memo<Option<Spend>>, period: &RwSignal<String>) -> String {
    spend
        .with(|s| {
            let s = s.as_ref()?;
            let want = period.get();
            s.periods
                .iter()
                .find(|p| p.id == want)
                .or(s.periods.first())
                .map(|p| p.id.clone())
        })
        .unwrap_or_default()
}
