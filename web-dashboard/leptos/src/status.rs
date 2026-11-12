//! The Status page: questions first, then the pinned threads (concept C v2, pins.rs), machines, phones and
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
    // The summary counts (summary.rs); None from a server older than the strip, which keeps the sentence.
    let strip = Memo::new(move |_| {
        live.view
            .with(|v| v.as_ref().and_then(|v| v.headline.strip.clone()))
    });
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
        strip: Some(strip.into()),
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
                <div class="page-head st-head">
                    <h1 class="page-title">"Status"</h1>
                    <Show
                        when=move || strip.with(|s| s.is_some())
                        fallback=move || view! {
                            <p class="headline">{move || runs_view(live.view.with(|v| v.as_ref().map(headline_runs).unwrap_or_default()))}</p>
                        }
                    >
                        <crate::summary::SummaryStrip strip=strip/>
                    </Show>
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
                        <crate::pins::PinnedPanelView/>
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

/// The way back into the app frame from the classic app, for a device that chose Classic app (the frame is the default,
/// FRAME_DEFAULT in index.html). The installed app keeps its own storage on iOS, so it is chosen from inside the installed
/// app.
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
            <b>"New app frame"</b>
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

// ---- Pinned threads (concept C v2 lives in pins.rs; these are shared with it and Details) -------------------------

fn unreadable_words(p: &PinnedUpdate) -> String {
    p.unreadable_note
        .clone()
        .unwrap_or_else(|| "Could not be read.".to_string())
}

/// The sentence over a last good copy: why it is old, then that it is cached and from when.
pub(crate) fn cached_notice(p: &PinnedUpdate) -> impl IntoView + use<> {
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

/// "<file> was refused:", or "A status file was refused:" when the server names no file.
fn refused_head(file: &str) -> String {
    if file.trim().is_empty() {
        "A status file was refused:".to_string()
    } else {
        format!("{file} was refused:")
    }
}

/// The refusal over a pin's card or sheet: what was refused and why, in the server's words.
pub(crate) fn refusal_notice(p: &PinnedUpdate) -> Option<AnyView> {
    let r = p.refusal.as_ref()?;
    Some(view! { <p class="pin-cached"><span data-tone="crit">{refused_head(&r.file)}</span>{format!(" {}", r.reason)}</p> }.into_any())
}

/// The refused status@1 files the server holds no good copy of, under the head. A pin whose own row says it was
/// refused isn't repeated here.
pub(crate) fn refused_lines(p: &PinnedPanel) -> impl IntoView + use<> {
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

/// The machine a pin's T3 runs on, as the page names it.
pub(crate) fn computer(id: &str) -> Option<&'static serde_json::Value> {
    static COMPUTERS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../public/computers.json"))
            .expect("packaged computer registry")
    });
    COMPUTERS
        .as_object()?
        .values()
        .find(|row| row["machineId"].as_str() == Some(id))
}

pub(crate) fn machine_label(id: &str) -> Option<&'static str> {
    computer(id)?["shortLabel"].as_str()
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
    computer(id)?["subtitle"].as_str()
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
    fn a_refusal_with_or_without_a_file_says_what_was_refused() {
        // a refusal with no file name still says what happened
        assert_eq!(refused_head(""), "A status file was refused:");
        assert_eq!(
            refused_head("mac.flash/status.json"),
            "mac.flash/status.json was refused:"
        );
        assert_eq!(machine_label("nas1"), Some("Nas1"));
        assert_eq!(machine_label("alexis"), Some("Alexis PC"));
        assert_eq!(machine_label("elsewhere"), None);
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
