//! The Pinned threads plate of concept C v2: the roster, the wide pane and the phone sheet. Composition is in
//! `roster.rs`; this file draws it.

use crate::api::*;
use crate::fmt;
use crate::live::Live;
use crate::media::MediaList;
use crate::roster::*;
use crate::status::{SheetGrab, cached_notice, refusal_notice, refused_lines, sheet_drag_style};
use crate::ui::*;
use leptos::prelude::*;

const OPENED_KEY: &str = "aac-status-opened";

#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub u: PinnedUpdate,
    pub r: Reported,
    pub word: Word,
    pub cards: Vec<Card>,
    pub board: Option<Stream>,
}

impl Pin {
    fn title(&self) -> String {
        if self.u.title.trim().is_empty() {
            self.r.title.clone().unwrap_or_else(|| "Untitled pin".into())
        } else {
            self.u.title.clone()
        }
    }
    fn ready(&self) -> Vec<&Deliverable> {
        self.r
            .deliverables
            .iter()
            .filter(|d| d.is_ready_for_owner())
            .collect()
    }
    fn needs_you(&self) -> bool {
        matches!(self.word, Word::ReadyForYou | Word::WaitingOnYou)
            || (owner_stage_waiting(&self.r) && !matches!(self.word, Word::Quiet | Word::NotReporting))
    }
    fn group(&self) -> &'static str {
        if self.needs_you() {
            "Needs you"
        } else {
            self.word.group()
        }
    }
    /// The problem lines, short (roster) and full (sheet), worst first.
    fn problems(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(r) = self.u.refusal.as_ref() {
            let head = if r.file.trim().is_empty() {
                "A status file was refused".to_string()
            } else {
                format!("{} was refused", r.file)
            };
            let reason = r.reason.trim().trim_end_matches('.');
            out.push((format!("{head}."), format!("{head}: {reason}.")));
        } else if self.u.report_state == "unreadable" {
            let note = self
                .u
                .unreadable_note
                .clone()
                .unwrap_or_else(|| "Could not be read.".into());
            let full = format!("{note} Showing its cached copy.");
            out.push((full.clone(), full));
        }
        if self.u.looping.answer == "yes" {
            let text = self.u.looping.text.trim();
            let text = text.strip_prefix("Yes.").unwrap_or(text).trim();
            let full = if text.is_empty() { "Looping.".to_string() } else { format!("Looping: {text}") };
            out.push((cut(&full, 72), full));
        }
        if self.r.readme_questions && self.cards.is_empty() {
            out.push((
                "Questions in a README, not filed as cards.".into(),
                "Questions in a README, not filed as cards. This thread has no open card, so they cannot be answered here."
                    .into(),
            ));
        }
        if let Some(problem) = self.board.as_ref().and_then(|b| b.problem.clone()) {
            out.push((
                "Task list unreadable.".into(),
                format!("Task list unreadable: {problem} Showing its last good copy."),
            ));
        }
        out
    }
}

fn iso_ms(iso: &str) -> Option<f64> {
    let ms = js_sys::Date::parse(iso);
    (!ms.is_nan()).then_some(ms)
}

/// "just now", "4 min", "1 h 3 min", "2 d".
fn short_age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 45 {
        "just now".into()
    } else if s < 3_600 {
        format!("{} min", ((s + 30) / 60).max(1))
    } else if s < 86_400 {
        let (h, m) = (s / 3_600, (s % 3_600) / 60);
        if m == 0 || h >= 6 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    } else {
        format!("{} d", s / 86_400)
    }
}

fn since_ms(ms: f64) -> String {
    short_age(((js_sys::Date::now() - ms) / 1000.0) as i64)
}

/// The earliest time among ISO strings, as a clock time.
fn earliest(times: impl Iterator<Item = String>) -> Option<String> {
    times
        .filter_map(|t| iso_ms(&t).map(|ms| (ms, t)))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, t)| fmt::clock(&t))
        .filter(|c| !c.is_empty())
}

fn ready_since(p: &Pin) -> Option<String> {
    earliest(
        p.ready()
            .into_iter()
            .filter_map(|d| d.ready_at.clone().or_else(|| d.updated_at.clone())),
    )
}

fn cards_since(p: &Pin) -> Option<String> {
    earliest(p.cards.iter().map(|c| c.asked_at.clone()))
}

/// The roster lead: what to do or what is happening, at most 60 characters.
fn lead(p: &Pin) -> String {
    let text = match p.word {
        Word::ReadyForYou => {
            let n = p.ready().len();
            match ready_since(p) {
                Some(at) => format!("{}, since {at}.", things(n)),
                None => format!("{}.", things(n)),
            }
        }
        Word::WaitingOnYou if !p.cards.is_empty() => match cards_since(p) {
            Some(at) => format!("{}, since {at}.", questions(p.cards.len())),
            None => format!("{}.", questions(p.cards.len())),
        },
        Word::WaitingOnYou | Word::Waiting => waiting_words(p).unwrap_or_default(),
        Word::Quiet => p
            .u
            .stale_note
            .as_deref()
            .map(|n| first_sentence(n, 60))
            .unwrap_or_default(),
        _ => now_text(p).map(|n| first_sentence(&n, 60)).unwrap_or_default(),
    };
    cut(&text, 60)
}

/// What it waits on, in one line: the report's own blocker, else the composer's words after their head.
fn waiting_words(p: &Pin) -> Option<String> {
    let reported = p
        .r
        .blockers
        .iter()
        .chain(p.r.blocked_on.iter())
        .map(Blocker::words)
        .find(|w| !w.is_empty());
    reported.or_else(|| {
        let t = p.u.blocked.text.trim();
        (!t.is_empty() && t != "None." && t != "None").then(|| {
            t.split_once(": ")
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_else(|| t.to_string())
        })
    })
}

fn now_text(p: &Pin) -> Option<String> {
    p.r.now
        .clone()
        .or_else(|| p.u.now.clone().filter(|n| !n.trim().is_empty()).map(|n| first_sentence(&n, 140)))
}

fn foot_words(p: &Pin) -> String {
    if let Some((text, at, of)) = stage_line(&p.r.stages) {
        return format!("{at} of {of}  {text}");
    }
    match p.u.percent.as_ref() {
        Some(x) if x.done.is_none() => format!("Own estimate {}", x.text),
        Some(x) => match (x.done, x.of) {
            (Some(d), Some(o)) => format!("{d} of {o} tasks done"),
            _ => "No stages reported".into(),
        },
        None => match p.r.progress.as_ref() {
            Some(crate::roster::Progress { done: Some(d), total: Some(t), unit }) => {
                format!("{d} of {t} {}", unit.clone().unwrap_or_else(|| "done".into()))
            }
            _ => "No stages reported".into(),
        },
    }
}

// ---- What this browser opened ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct OpenedStore(RwSignal<Vec<(String, f64)>>);

fn opened_key(pin: &str, deliverable: &str, version: &str) -> String {
    format!("{pin}|{deliverable}|{version}")
}

fn load_opened() -> Vec<(String, f64)> {
    let raw = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(OPENED_KEY).ok().flatten())
        .unwrap_or_default();
    serde_json::from_str::<std::collections::BTreeMap<String, f64>>(&raw)
        .map(|m| m.into_iter().collect())
        .unwrap_or_default()
}

impl OpenedStore {
    fn for_one(&self, pin: &str, deliverable: &str) -> Opened {
        let prefix = format!("{pin}|{deliverable}|");
        self.0.with(|all| {
            all.iter()
                .filter_map(|(k, at)| k.strip_prefix(&prefix).map(|v| (v.to_string(), *at)))
                .collect()
        })
    }
    fn record(&self, key: String) {
        let now = js_sys::Date::now();
        self.0.update(|all| {
            all.retain(|(k, _)| *k != key);
            all.push((key, now));
            // Keep it bounded: the newest 400 opens.
            all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            all.truncate(400);
        });
        let map: std::collections::BTreeMap<String, f64> = self.0.get_untracked().into_iter().collect();
        if let (Some(s), Ok(text)) = (window().local_storage().ok().flatten(), serde_json::to_string(&map)) {
            let _ = s.set_item(OPENED_KEY, &text);
        }
    }
}

fn mark_view(m: Mark) -> AnyView {
    match m {
        Mark::New => view! { <span class="to-mark" data-tone="acc" title="Not opened from this browser">"New"</span> }.into_any(),
        Mark::NewSince(v) => view! { <span class="to-mark" data-tone="acc" title="Opened from this browser">{format!("New since you opened {v}")}</span> }.into_any(),
        Mark::Opened(at) => {
            let when = fmt::clock(&js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at)).to_iso_string().as_string().unwrap_or_default());
            view! { <span class="to-mark q3" title="Opened from this browser">{format!("Opened {when}")}</span> }.into_any()
        }
    }
}

fn copy_text(text: String, done: RwSignal<Option<bool>>) {
    let navigator = window().navigator();
    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into()).unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    let write = js_sys::Reflect::get(&clipboard, &"writeText".into())
        .ok()
        .and_then(|f| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(f).ok());
    match write
        .and_then(|f| f.call1(&clipboard, &text.into()).ok())
        .and_then(|p| wasm_bindgen::JsCast::dyn_into::<js_sys::Promise>(p).ok())
    {
        Some(promise) => leptos::task::spawn_local(async move {
            done.set(Some(wasm_bindgen_futures::JsFuture::from(promise).await.is_ok()));
        }),
        None => done.set(Some(false)),
    }
}

/// Open, Copy path, or the reason there is neither.
fn open_action(o: Opener, main: bool, on_open: Callback<()>) -> AnyView {
    match o {
        Opener::Route(href) | Opener::Link(href) => view! {
            <a class="btn sm" class:primary=main href=href target="_blank" rel="noopener noreferrer"
                on:click=move |_| on_open.run(())>"Open"</a>
        }
        .into_any(),
        Opener::CopyPath(path) => {
            let done = RwSignal::new(None::<bool>);
            let p2 = path.clone();
            view! {
                <button class="btn sm" on:click=move |_| copy_text(p2.clone(), done)>
                    {move || match done.get() { Some(true) => "Copied", _ => "Copy path" }}
                </button>
                {move || (done.get() == Some(false)).then(|| view! { <span class="to-path mono">{path.clone()}</span> })}
            }
            .into_any()
        }
        Opener::Nothing => view! { <span class="q3 to-none">"No file given"</span> }.into_any(),
    }
}

fn media_of(list: &[ReportedMedia]) -> (Vec<Media>, usize) {
    let mut out = Vec::new();
    let mut outside = 0;
    for m in list {
        let url = m
            .url
            .clone()
            .filter(|u| u.starts_with("https://"))
            .or_else(|| m.path.as_deref().and_then(|p| share_relative(p, true)).map(|r| route_href(&r)));
        match url {
            Some(url) => out.push(Media {
                kind: m.kind.clone(),
                url,
                poster_url: m
                    .poster
                    .as_deref()
                    .and_then(|p| share_relative(p, true))
                    .map(|r| route_href(&r)),
                caption: m.caption.clone(),
                label: m.label.clone(),
            }),
            None => outside += 1,
        }
    }
    (out, outside)
}

#[component]
fn MediaThumbs(items: Vec<ReportedMedia>) -> impl IntoView {
    let (media, outside) = media_of(&items);
    let all = RwSignal::new(false);
    let more = media.len().saturating_sub(3);
    view! {
        <div class="to-media">
            {move || {
                let shown: Vec<Media> = if all.get() { media.clone() } else { media.iter().take(3).cloned().collect() };
                view! { <MediaList items=shown/> }
            }}
            {(more > 0).then(|| view! {
                <button class="link" on:click=move |_| all.update(|a| *a = !*a)>
                    {move || if all.get() { "Fewer".to_string() } else { format!("{more} more") }}
                </button>
            })}
            {(outside > 0).then(|| view! { <p class="q3 to-sub">{format!("{} kept outside the share, so not shown here.", if outside == 1 { "1 more is".to_string() } else { format!("{outside} more are") })}</p> })}
        </div>
    }
}

#[component]
fn ToOpenRow(pin: String, d: Deliverable) -> impl IntoView {
    let store = expect_context::<OpenedStore>();
    let opened = {
        let (pin, id) = (pin.clone(), d.id.clone());
        Signal::derive(move || store.for_one(&pin, &id))
    };
    let main = d.main == Some(true);
    let version = d.version.clone();
    let key = opened_key(&pin, &d.id, &d.version);
    let on_open = Callback::new(move |_| store.record(key.clone()));
    let o = opener(d.path.as_deref(), d.url.as_deref());
    let outside = matches!(o, Opener::CopyPath(_));
    let updated = d.updated_at.as_deref().map(fmt::clock).filter(|c| !c.is_empty());
    let show_old = RwSignal::new(false);
    let older = d.versions.clone();
    let n_old = older.len();
    let current = d.version.clone();
    let (pin2, id2) = (pin.clone(), d.id.clone());
    view! {
        <li class="to-row" class:main=main>
            <span class="to-kind cap">{kind_words(&d.kind)}</span>
            <div class="to-main">
                <b class="to-name">{d.name.clone()}</b>
                <p class="to-line">
                    <b class="to-ver">{d.version.clone()}</b>
                    {updated.map(|u| view! { <span class="q3">{u}</span> })}
                    {move || mark_view(mark(&version, &opened.get()))}
                </p>
                {outside.then(|| view! { <p class="q3 to-sub">"Kept outside the share, so it cannot be opened here"</p> })}
                {d.note.clone().map(|n| view! { <p class="to-sub">{n}</p> })}
                {(!d.media.is_empty()).then(|| view! { <MediaThumbs items=d.media.clone()/> })}
                {(n_old > 0).then(|| view! {
                    <p class="to-sub q3">
                        {if n_old == 1 { "1 older version".to_string() } else { format!("{n_old} older versions") }}" "
                        <button class="link" aria-expanded=move || show_old.get().to_string() on:click=move |_| show_old.update(|s| *s = !*s)>
                            {move || if show_old.get() { "Hide" } else { "Show" }}
                        </button>
                    </p>
                })}
                <Show when=move || show_old.get()>
                    <ul class="to-older">
                        {older.iter().map(|v| {
                            let key = opened_key(&pin2, &id2, &v.version);
                            let on_open = Callback::new(move |_| store.record(key.clone()));
                            let mine = store.for_one(&pin2, &id2).into_iter().find(|(x, _)| *x == v.version);
                            let when = v.updated_at.as_deref().map(fmt::clock).filter(|c| !c.is_empty());
                            view! {
                                <li>
                                    <span><b>{v.version.clone()}</b>{when.map(|w| format!(", {w}"))}", "<span class="q3">{format!("Superseded by {current}")}</span>
                                    {mine.map(|(_, at)| view! { " "{mark_view(Mark::Opened(at))} })}</span>
                                    {open_action(opener(v.path.as_deref(), v.url.as_deref()), false, on_open)}
                                </li>
                            }
                        }).collect_view()}
                    </ul>
                </Show>
            </div>
            <span class="to-act">{open_action(o, main, on_open)}</span>
        </li>
    }
}

fn to_open_section(p: &Pin) -> AnyView {
    let list = ordered(&p.r.deliverables);
    if list.is_empty() {
        return view! {
            <section class="c2-sec">
                <div class="c2-sec-head"><h4 class="cap">"To open"</h4></div>
                <p class="q3 c2-none">"Nothing reported to open."</p>
            </section>
        }
        .into_any();
    }
    let store = expect_context::<OpenedStore>();
    let all_new = list
        .iter()
        .all(|d| store.for_one(&p.u.id, &d.id).is_empty());
    let more = list.len().saturating_sub(6);
    let all = RwSignal::new(false);
    let pin = p.u.id.clone();
    view! {
        <section class="c2-sec">
            <div class="c2-sec-head"><h4 class="cap">"To open"</h4><span class="n">{list.len().to_string()}</span>
                {all_new.then(|| view! { <span class="c2-sec-end">"all new to you"</span> })}</div>
            <ul class="to-list">
                {move || {
                    let rows: Vec<Deliverable> = if all.get() { list.clone() } else { list.iter().take(6).cloned().collect() };
                    rows.into_iter().map(|d| view! { <ToOpenRow pin=pin.clone() d=d/> }).collect_view()
                }}
            </ul>
            {(more > 0).then(|| view! {
                <p class="c2-fold"><span>{format!("and {more} more")}</span>
                    <button class="link" on:click=move |_| all.update(|a| *a = !*a)>{move || if all.get() { "Fewer" } else { "Show" }}</button></p>
            })}
        </section>
    }
    .into_any()
}

// ---- The five fixed facts --------------------------------------------------------------------------------------------

fn fact(label: &'static str, main: AnyView, right: Option<AnyView>) -> impl IntoView {
    view! { <dt>{label}</dt><dd>{main}</dd><dd class="t">{right}</dd> }
}

fn facts(p: &Pin, elapsed: i64) -> impl IntoView + use<> {
    // For you: the ready things first, then the cards, then a blocker on the owner.
    let mut for_you: Vec<AnyView> = Vec::new();
    let mut for_you_when: Option<String> = None;
    let ready = p.ready();
    if !ready.is_empty() {
        let then = ready
            .iter()
            .find(|d| d.main == Some(true))
            .or(ready.first())
            .and_then(|d| d.unlocks.clone());
        let then = then.unwrap_or_else(|| "Not reported: what your answers start.".into());
        for_you.push(view! { <p><b data-tone="warn">"Ready"</b>{format!(" {}, below", things(ready.len()))}<small>{then}</small></p> }.into_any());
        for_you_when = ready_since(p).map(|t| format!("since {t}"));
    }
    if !p.cards.is_empty() {
        for_you.push(view! { <p><b data-tone="warn">{questions(p.cards.len())}</b>" in Questions for you"</p> }.into_any());
        if for_you_when.is_none() {
            for_you_when = cards_since(p).map(|t| format!("since {t}"));
        }
    } else if p.u.blocked.waiting_on_you {
        let text = p.u.blocked.text.clone();
        for_you.push(view! { <p><b data-tone="warn">"Waiting"</b>{format!(" {}", text.split_once(": ").map(|(_, r)| r.to_string()).unwrap_or(text.clone()))}</p> }.into_any());
    }
    let for_you_view = if for_you.is_empty() {
        view! { <span class="q3">"Nothing"</span> }.into_any()
    } else {
        for_you.into_iter().collect_view().into_any()
    };

    // Waiting on: every blocker that is not the owner's.
    let now = js_sys::Date::now();
    let mut waits: Vec<AnyView> = Vec::new();
    let mut wait_when: Vec<AnyView> = Vec::new();
    for b in p.r.blockers.iter().chain(p.r.blocked_on.iter()).filter(|b| b.who() != "owner") {
        waits.push(view! { <p><b>{who_words(b.who())}</b>{format!(" {}", b.words())}</p> }.into_any());
        let late = b.until.as_deref().and_then(iso_ms).filter(|u| *u < now);
        wait_when.push(match (late, b.since.as_deref().and_then(iso_ms)) {
            (Some(_), _) => view! { <span data-tone="warn">{format!("was due {}", fmt::clock(b.until.as_deref().unwrap_or_default()))}</span> }.into_any(),
            (None, Some(s)) => view! { <span>{format!("for {}", since_ms(s))}</span> }.into_any(),
            _ => view! { <span></span> }.into_any(),
        });
    }
    if waits.is_empty() && !p.u.blocked.waiting_on_you {
        let t = p.u.blocked.text.trim().to_string();
        if !t.is_empty() && t != "None." && t != "None" {
            waits.push(view! { <p>{t}</p> }.into_any());
        }
    }
    let waiting_view = if waits.is_empty() {
        let unknown = p.u.blocked.text.trim().is_empty() && p.r.blockers.is_empty() && p.r.blocked_on.is_none();
        view! { <span class="q3">{if unknown { "Not reported" } else { "Nothing else" }}</span> }.into_any()
    } else {
        waits.into_iter().collect_view().into_any()
    };
    let now_view = match now_text(p) {
        Some(n) => view! { <span>{n}</span> }.into_any(),
        None => view! { <span class="q3">"Not reported"</span> }.into_any(),
    };
    let next_view = match p.r.next.clone() {
        Some(n) => view! { <span>{n}</span> }.into_any(),
        None => view! { <span class="q3">"Not reported"</span> }.into_any(),
    };
    let (upd, upd_age) = match p.u.updated.as_ref() {
        Some(u) => (
            view! { <span>{fmt::clock(&u.at)}</span> }.into_any(),
            u.age_seconds.map(|a| view! { <span>{fmt::long_age(a + elapsed)}</span> }.into_any()),
        ),
        None => (view! { <span class="q3">"Not reported"</span> }.into_any(), None),
    };
    view! {
        <dl class="fx">
            {fact("For you", for_you_view, for_you_when.map(|w| view! { <span>{w}</span> }.into_any()))}
            {fact("Waiting on", waiting_view, (!wait_when.is_empty()).then(|| wait_when.into_iter().map(|w| view! { <span class="fx-when">{w}</span> }).collect_view().into_any()))}
            {fact("Now", now_view, None)}
            {fact("Next", next_view, None)}
            {fact("Updated", upd, upd_age)}
        </dl>
    }
}

// ---- Stages and tasks ------------------------------------------------------------------------------------------------

fn stage_label(s: &Stage) -> String {
    let mut t = s.label.clone();
    if let Some(r) = s.round {
        t.push_str(&match s.of {
            Some(of) => format!(", round {r} of {of}"),
            None => format!(", round {r}"),
        });
    }
    if let Some(n) = s.note.as_ref().filter(|n| !n.is_empty()) {
        t.push_str(&format!(" {n}"));
    }
    t
}

/// The roster's small track: one segment per stage.
fn mini_track(stages: &[Stage]) -> Option<AnyView> {
    let shown: Vec<&Stage> = stages.iter().filter(|s| s.state != "skipped").collect();
    (shown.len() >= 2).then(|| {
        view! { <span class="c2-mini" aria-hidden="true">{shown.into_iter().map(|s| view! { <i class="fill" data-st=stage_tone(s)></i> }).collect_view()}</span> }.into_any()
    })
}

fn stages_section(p: &Pin) -> AnyView {
    let shown: Vec<Stage> = p.r.stages.iter().filter(|s| s.state != "skipped").cloned().collect();
    let line = stage_line(&p.r.stages);
    let yours = shown.iter().filter(|s| s.by.as_deref() == Some("owner")).count();
    view! {
        <section class="c2-sec">
            <div class="c2-sec-head"><h4 class="cap">"Stages"</h4>
                {line.map(|(t, at, of)| view! { <span class="c2-sec-end">{format!("{t}, ")}<b>{at.to_string()}</b>{format!(" of {of}")}</span> })}</div>
            {if shown.len() < 2 {
                view! { <p class="q3 c2-none">{foot_words(p)}</p> }.into_any()
            } else {
                view! {
                    <div class="c2-track" aria-hidden="true">{shown.iter().map(|s| view! { <i class="fill" data-st=stage_tone(s)></i> }).collect_view()}</div>
                    <ol class="c2-stages" style=format!("--n:{}", shown.len())>
                        {shown.iter().map(|s| view! {
                            <li data-st=stage_tone(s)>
                                <span class="seg fill"></span>
                                <span class="lab">{stage_label(s)}</span>
                                <span class="st">{stage_state_words(s)}</span>
                            </li>
                        }).collect_view()}
                    </ol>
                    {(yours > 0).then(|| view! { <p class="q3 c2-sub">{format!("{yours} of these {} steps {} yours.", shown.len(), if yours == 1 { "is" } else { "are" })}</p> })}
                }.into_any()
            }}
        </section>
    }
    .into_any()
}

#[component]
fn TaskGroupView(g: TaskGroup) -> impl IntoView {
    let open = RwSignal::new(false);
    let folds = g.folds();
    let n = g.rows.len();
    let rows = g.rows.clone();
    let label = if g.label == "Left" { "Not done" } else { g.label };
    view! {
        <div class="tg">
            <h5 class="cap" data-tone=g.warn.then_some("warn")>{label}" "<span class="n">{n.to_string()}</span></h5>
            {move || if folds && !open.get() {
                view! {
                    <p class="tg-fold"><span>{plural_tasks(n)}</span>
                        <button class="link" on:click=move |_| open.set(true)>{format!("Show all {n}")}</button></p>
                }.into_any()
            } else {
                view! {
                    <ul class="tg-rows">
                        {rows.iter().map(|r| view! {
                            <li><span class="tg-t">{r.title.clone()}</span>
                                {match (&r.odd_state, &r.note) {
                                    (Some(s), _) => view! { <span class="tg-n" data-tone="warn">{format!("reported as \"{s}\"")}</span> }.into_any(),
                                    (None, Some(n)) => view! { <span class="tg-n">{n.clone()}</span> }.into_any(),
                                    _ => view! { <span class="tg-n"></span> }.into_any(),
                                }}
                            </li>
                        }).collect_view()}
                        {(folds).then(|| view! { <li class="tg-less"><button class="link" on:click=move |_| open.set(false)>"Fewer"</button></li> })}
                    </ul>
                }.into_any()
            }}
        </div>
    }
}

fn plural_tasks(n: usize) -> String {
    if n == 1 { "1 task".into() } else { format!("{n} tasks") }
}

fn tasks_section(p: &Pin) -> AnyView {
    let count = p.u.left.len() + p.u.done.len();
    if count == 0 {
        let words = p.u.percent_note.clone().unwrap_or_else(|| "No task list reported.".into());
        return view! {
            <section class="c2-sec">
                <div class="c2-sec-head"><h4 class="cap">"Tasks"</h4></div>
                <p class="q3 c2-none">{words}</p>
            </section>
        }
        .into_any();
    }
    let groups = task_groups(&p.u, p.board.as_ref());
    // The list's own time, when its board is known; amber when 6 h older than the thread's report.
    let as_of = p.board.as_ref().and_then(|b| b.as_of.clone()).map(|at| {
        let list_ms = iso_ms(&at);
        let report_ms = p.u.updated.as_ref().and_then(|u| iso_ms(&u.at));
        let old = matches!((list_ms, report_ms), (Some(l), Some(r)) if r - l > 6.0 * 3_600_000.0)
            || p.board.as_ref().is_some_and(|b| b.stale);
        let words = match (old, list_ms) {
            (true, Some(l)) => format!("as of {}, {} old", fmt::clock(&at), since_ms(l)),
            _ => format!("as of {}", fmt::clock(&at)),
        };
        view! { <span class="c2-sec-end" data-tone=old.then_some("warn")>{words}</span> }
    });
    view! {
        <section class="c2-sec">
            <div class="c2-sec-head"><h4 class="cap">"Tasks"</h4><span class="n">{count.to_string()}</span>{as_of}</div>
            {groups.into_iter().map(|g| view! { <TaskGroupView g=g/> }).collect_view()}
        </section>
    }
    .into_any()
}

// ---- The sheet (wide pane, or the phone's bottom sheet) ----------------------------------------------------------------

fn sheet_body(p: &Pin, elapsed: i64) -> impl IntoView + use<> {
    let since = match p.word {
        Word::ReadyForYou => ready_since(p).map(|t| format!("since {t}")),
        Word::WaitingOnYou => cards_since(p).map(|t| format!("since {t}")),
        _ => None,
    };
    let quiet = matches!(p.word, Word::Quiet | Word::NotReporting);
    let cached = p.u.report_state == "unreadable";
    let meta = [
        p.u.who.clone(),
        crate::status::machine_label(&p.u.machine).map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let problems = p.problems();
    let body = if quiet {
        let note = p
            .u
            .stale_note
            .clone()
            .or_else(|| p.u.report_note.clone())
            .unwrap_or_default();
        let last = p.u.summary.clone().map(|s| format!(" The last one said: {s}"));
        view! { <p class="c2-quiet"><span class="w-ink">{note}</span>{last}</p> }.into_any()
    } else {
        view! {
            <div class="c2-cols" class:dim=cached>
                <div class="c2-col">{facts(p, elapsed)}{to_open_section(p)}</div>
                <div class="c2-col">{stages_section(p)}{tasks_section(p)}</div>
            </div>
        }
        .into_any()
    };
    view! {
        <header class="c2-head">
            <p class="c2-state"><b data-tone=p.word.tone()>{p.word.text()}</b>{since.map(|s| view! { " "<span class="q3">{s}</span> })}</p>
            <h3 class="c2-name">{p.title()}</h3>
            {(!meta.is_empty()).then(|| view! { <p class="c2-meta">{meta.join("  ·  ")}</p> })}
        </header>
        {refusal_notice(&p.u)}
        {cached.then(|| cached_notice(&p.u))}
        {problems.into_iter().skip(usize::from(p.u.refusal.is_some() || cached)).map(|(_, full)| view! { <p class="c2-prob" data-tone="warn">{full}</p> }).collect_view()}
        {body}
    }
}

// ---- The plate ---------------------------------------------------------------------------------------------------------

const ORDER: [&str; 8] = [
    "Needs you",
    "Working",
    "Waiting",
    "Idle",
    "Reporting",
    "Done",
    "Quiet",
    "Not reporting",
];

fn pins_of(view: &StatusView) -> Option<(PinnedPanel, Vec<Pin>)> {
    let panel = view.pinned.clone()?;
    let open = &view.cards.open;
    let mut pins: Vec<Pin> = panel
        .updates
        .iter()
        .map(|u| {
            let r = reported(u);
            let cards: Vec<Card> = open_cards_for(&r, open).into_iter().cloned().collect();
            let word = state_word(u, &r, cards.len());
            let board = board_for(u, &view.projects).cloned();
            Pin { u: u.clone(), r, word, cards, board }
        })
        .collect();
    pins.sort_by_key(|p| (ORDER.iter().position(|g| *g == p.group()).unwrap_or(9), p.word));
    Some((panel, pins))
}

fn is_silent(p: &Pin) -> bool {
    p.u.report_state == "none" && p.u.refusal.is_none()
}

#[component]
fn RosterRow(p: Pin, selected: Signal<bool>, open: Signal<bool>, on_pick: Callback<String>) -> impl IntoView {
    let id = p.u.id.clone();
    let age = p.u.updated.as_ref().and_then(|u| u.age_seconds).map(short_age);
    let model = model_of(p.u.who.as_deref());
    let problem = p.problems().into_iter().next().map(|(short, _)| short);
    let mute = matches!(p.word, Word::Quiet);
    view! {
        <li class="c2-item">
            <button class="c2-row" class:sel=move || selected.get() class:open=move || open.get() class:quiet=mute aria-current=move || selected.get().then_some("true")
                on:click=move |_| on_pick.run(id.clone())>
                <span class="c2-l1"><span class="c2-title">{p.title()}</span><span class="c2-age">{age}</span></span>
                <span class="c2-l2"><b data-tone=p.word.tone()>{p.word.text()}</b>{format!(" {}", lead(&p))}</span>
                <span class="c2-l3">{mini_track(&p.r.stages)}<span class="c2-stage">{foot_words(&p)}</span><span class="c2-model">{model}</span></span>
                {problem.map(|t| view! { <span class="c2-prob" data-tone="warn">{t}</span> })}
            </button>
        </li>
    }
}

#[component]
pub fn PinnedPanelView() -> impl IntoView {
    let live = expect_context::<Live>();
    provide_context(OpenedStore(RwSignal::new(load_opened())));
    let data = Memo::new(move |_| live.view.with(|v| v.as_ref().and_then(pins_of)));
    let picked = RwSignal::new(None::<String>);
    let sheet_id = RwSignal::new(None::<String>);
    // The thread the wide pane shows: the one picked, else the first in the roster.
    let shown_id = Memo::new(move |_| {
        data.with(|d| {
            let (_, pins) = d.as_ref()?;
            let live: Vec<&Pin> = pins.iter().filter(|p| !is_silent(p)).collect();
            picked
                .get()
                .filter(|id| live.iter().any(|p| p.u.id == *id))
                .or_else(|| live.first().map(|p| p.u.id.clone()))
        })
    });
    let pin_of = move |id: Option<String>| {
        let id = id?;
        data.with(|d| d.as_ref().and_then(|(_, pins)| pins.iter().find(|p| p.u.id == id).cloned()))
    };
    let shown = Memo::new(move |_| pin_of(shown_id.get()));
    let sheet_pin = Memo::new(move |_| pin_of(sheet_id.get()));
    Effect::new(move |_| {
        if sheet_id.with(|s| s.is_some()) && sheet_pin.with(|p| p.is_none()) {
            sheet_id.set(None);
        }
    });
    let narrow = move || {
        document()
            .query_selector(".sec-pinned")
            .ok()
            .flatten()
            .map(|el| el.client_width() < 900)
            .unwrap_or(true)
    };
    let on_pick = Callback::new(move |id: String| {
        picked.set(Some(id.clone()));
        if narrow() {
            sheet_id.set(Some(id));
        }
    });
    let close_sheet = Callback::new(move |_: ()| sheet_id.set(None));
    let offset = RwSignal::new(0.0f64);
    view! {
        <section class="plate sec-pinned c2" id="pinned">
            <div class="plate-head">
                <h2>"Pinned threads"</h2>
                <span class="plate-meta">{move || data.with(|d| d.as_ref().map(|(panel, pins)| {
                    let needs = pins.iter().filter(|p| p.needs_you()).count();
                    view! {
                        <b>{panel.counts.pinned.to_string()}</b>" pinned in T3"
                        {(needs > 0).then(|| view! { ", "<b data-tone="warn">{format!("{needs} need you")}</b> })}
                    }
                }))}</span>
            </div>
            {move || match data.get() {
                None => view! { <p class="empty">"Pinned threads are unavailable: this server doesn't send them yet."</p> }.into_any(),
                Some((p, _)) if p.unavailable.is_some() => view! { <p class="empty">{p.unavailable.clone()}</p> }.into_any(),
                Some((p, pins)) if pins.is_empty() => view! {
                    {p.machine_problems.iter().map(|m| view! { <p class="plate-note problem">{m.text.clone()}</p> }).collect_view()}
                    <p class="empty">"No threads are pinned in T3."</p>
                }.into_any(),
                Some((panel, pins)) => {
                    let elapsed = live.elapsed_seconds();
                    let (silent, live_pins): (Vec<Pin>, Vec<Pin>) = pins.into_iter().partition(is_silent);
                    let mut groups: Vec<(&'static str, Vec<Pin>)> = Vec::new();
                    for p in live_pins {
                        let g = p.group();
                        match groups.iter_mut().find(|(x, _)| *x == g) {
                            Some((_, list)) => list.push(p),
                            None => groups.push((g, vec![p])),
                        }
                    }
                    let no_rows = groups.is_empty();
                    view! {
                        {panel.machine_problems.iter().map(|m| view! { <p class="plate-note problem">{m.text.clone()}</p> }).collect_view()}
                        {refused_lines(&panel)}
                        <div class="c2-body" class:solo=no_rows>
                            {(!no_rows).then(|| view! {
                                <div class="c2-roster">
                                    {groups.into_iter().map(|(g, list)| view! {
                                        <h3 class="cap c2-group" data-tone=(g == "Needs you").then_some("warn")>{g}" "<span class="n">{list.len().to_string()}</span></h3>
                                        <ul class="c2-rows">
                                            {list.into_iter().map(|p| {
                                                let id = p.u.id.clone();
                                                let id2 = id.clone();
                                                let sel = Signal::derive(move || shown_id.get().as_deref() == Some(id.as_str()));
                                                let open = Signal::derive(move || sheet_id.get().as_deref() == Some(id2.as_str()));
                                                view! { <RosterRow p=p selected=sel open=open on_pick=on_pick/> }
                                            }).collect_view()}
                                        </ul>
                                    }).collect_view()}
                                </div>
                                <div class="c2-pane" aria-live="polite">
                                    {move || shown.get().map(|p| view! { <article class="c2-sheet">{sheet_body(&p, elapsed)}</article> })}
                                </div>
                            })}
                        </div>
                        {(!silent.is_empty()).then(|| view! {
                            <div class="c2-silent">
                                <h3 class="cap c2-group">"Not reporting "<span class="n">{silent.len().to_string()}</span></h3>
                                <ul>{silent.iter().map(|p| view! { <li><b>{p.title()}</b><span class="q3">"no report yet"</span></li> }).collect_view()}</ul>
                                <p class="c2-note">"A pin shows its status once its thread runs agent-report update."</p>
                            </div>
                        })}
                    }.into_any()
                }
            }}
            {move || sheet_pin.get().map(|p| view! {
                <div class="sheet-host c2-sheet-host" role="dialog" aria-modal="true" aria-label=p.title()>
                    <button class="sheet-scrim" aria-label="Close" on:click=move |_| close_sheet.run(())></button>
                    <div class="sheet" style=move || sheet_drag_style(offset.get())>
                        <div class="sheet-top">
                            <SheetGrab offset=offset on_close=close_sheet/>
                            <button class="icon-btn bare sheet-close" aria-label="Close" on:click=move |_| close_sheet.run(())><Icon name="close"/></button>
                        </div>
                        <article class="c2-sheet">{sheet_body(&p, live.elapsed_seconds())}</article>
                    </div>
                </div>
            })}
        </section>
    }
}
