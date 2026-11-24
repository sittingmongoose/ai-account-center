//! The Pinned threads plate of concept C v2: the roster, the wide pane and the phone sheet. Composition is in
//! `roster.rs`; this file draws it.

use crate::api::*;
use crate::fmt;
use crate::live::Live;
use crate::media::LiveMediaList;
use crate::roster::*;
use crate::status::{SheetGrab, cached_notice, report_problem_lines, sheet_drag_style};
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
            self.r
                .title
                .clone()
                .unwrap_or_else(|| "Untitled pin".into())
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
    /// The problem lines, short (roster) and full (sheet), worst first.
    fn problems(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if self.u.report_state == "unreadable" {
            let note = self
                .u
                .unreadable_note
                .clone()
                .unwrap_or_else(|| "Could not be read.".into());
            let full = format!("{note} Showing its cached copy.");
            out.push((full.clone(), full));
        }
        if let Some(lines) = loop_lines(&self.u) {
            out.push(lines);
        }
        if self.r.readme_questions && self.cards.is_empty() {
            out.push((
                "Questions in a README, not filed as cards.".into(),
                "Questions in a README, not filed as cards. This thread has no open card, so they cannot be answered here."
                    .into(),
            ));
        }
        if self.board.as_ref().is_some_and(|b| b.problem.is_some()) {
            out.push((
                "Task list unreadable.".into(),
                "Task list unreadable. Showing its last good copy.".into(),
            ));
        }
        out
    }
}

fn iso_ms(iso: &str) -> Option<f64> {
    let ms = js_sys::Date::parse(iso);
    (!ms.is_nan()).then_some(ms)
}

fn since_ms(ms: f64) -> String {
    short_age(((js_sys::Date::now() - ms) / 1000.0) as i64)
}

/// Pinned Status uses EST (UTC-05:00) for report and activity times.
fn est_clock(iso: &str) -> String {
    let ms = js_sys::Date::parse(iso);
    if !ms.is_finite() {
        return String::new();
    }
    let est_day = |time: f64| ((time - 18_000_000.0) / 86_400_000.0).floor();
    let options = js_sys::Object::new();
    for (key, value) in [
        ("timeZone", "Etc/GMT+5"),
        ("hour", "numeric"),
        ("minute", "2-digit"),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    if est_day(ms) != est_day(js_sys::Date::now()) {
        let _ = js_sys::Reflect::set(&options, &"weekday".into(), &"short".into());
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    String::from(date.to_locale_string("en-US", &options)).replace(',', "")
}

/// The earliest time among ISO strings, as a clock time.
fn earliest(times: impl Iterator<Item = String>) -> Option<String> {
    times
        .filter_map(|t| iso_ms(&t).map(|ms| (ms, t)))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, t)| est_clock(&t))
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
    if p.word == Word::WaitingOnYou {
        return p
            .u
            .card_title
            .clone()
            .or_else(|| p.cards.first().map(|card| card.title.clone()))
            .unwrap_or_default();
    }
    let text = match p.word {
        Word::Waiting | Word::Blocked => {
            p.u.waits
                .iter()
                .find(|w| w.task.is_none())
                .map(wait_words)
                .or_else(|| now_text(p))
        }
        Word::Quiet => p.u.stale_note.clone(),
        _ => now_text(p),
    };
    text.map(|line| first_sentence(&line, 140))
        .unwrap_or_default()
}

fn now_text(p: &Pin) -> Option<String> {
    p.r.now.clone().or_else(|| {
        p.u.now
            .clone()
            .filter(|n| !n.trim().is_empty())
            .map(|n| first_sentence(&n, 140))
    })
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
            Some(crate::roster::Progress {
                done: Some(d),
                total: Some(t),
                unit,
            }) => {
                format!(
                    "{d} of {t} {}",
                    unit.clone().unwrap_or_else(|| "done".into())
                )
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
        let map: std::collections::BTreeMap<String, f64> =
            self.0.get_untracked().into_iter().collect();
        if let (Some(s), Ok(text)) = (
            window().local_storage().ok().flatten(),
            serde_json::to_string(&map),
        ) {
            let _ = s.set_item(OPENED_KEY, &text);
        }
    }
}

fn mark_view(m: Mark) -> AnyView {
    match m {
        Mark::New => view! { <span class="to-mark" data-tone="acc" title="Not opened from this browser">"New"</span> }.into_any(),
        Mark::NewSince(v) => view! { <span class="to-mark" data-tone="acc" title="Opened from this browser">{format!("New since you opened {v}")}</span> }.into_any(),
        Mark::Opened(at) => {
            let when = est_clock(&js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at)).to_iso_string().as_string().unwrap_or_default());
            view! { <span class="to-mark q3" title="Opened from this browser">{format!("Opened {when}")}</span> }.into_any()
        }
    }
}

fn copy_text(text: String, done: RwSignal<Option<bool>>) {
    let navigator = window().navigator();
    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into())
        .unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    let write = js_sys::Reflect::get(&clipboard, &"writeText".into())
        .ok()
        .and_then(|f| wasm_bindgen::JsCast::dyn_into::<js_sys::Function>(f).ok());
    match write
        .and_then(|f| f.call1(&clipboard, &text.into()).ok())
        .and_then(|p| wasm_bindgen::JsCast::dyn_into::<js_sys::Promise>(p).ok())
    {
        Some(promise) => leptos::task::spawn_local(async move {
            done.set(Some(
                wasm_bindgen_futures::JsFuture::from(promise).await.is_ok(),
            ));
        }),
        None => done.set(Some(false)),
    }
}

/// Open, Copy path, or the reason there is neither.
fn open_action(o: Opener, main: bool, on_open: Callback<()>) -> AnyView {
    match o {
        Opener::Route(href) | Opener::Link(href) => view! {
            <a class="btn sm" data-focus-role="deliverable-open" class:primary=main href=href target="_blank" rel="noopener noreferrer"
                on:click=move |_| on_open.run(())>"Open"</a>
        }
        .into_any(),
        Opener::CopyPath(path) => {
            let done = RwSignal::new(None::<bool>);
            let p2 = path.clone();
            view! {
                <button class="btn sm" data-focus-role="deliverable-open" on:click=move |_| copy_text(p2.clone(), done)>
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
            .or_else(|| {
                m.path
                    .as_deref()
                    .and_then(|p| share_relative(p, true))
                    .map(|r| route_href(&r))
            });
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

fn row_keys(values: impl Iterator<Item = String>) -> Vec<(String, usize)> {
    let mut seen = std::collections::BTreeMap::new();
    values
        .map(|id| {
            let occurrence = seen.entry(id.clone()).or_insert(0usize);
            let key = (id, *occurrence);
            *occurrence += 1;
            key
        })
        .collect()
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PinDisclosure {
    Tasks(&'static str),
    Deliverables,
    Media(String),
    Versions(String),
}

#[derive(Clone, Copy)]
struct PinDisclosures(RwSignal<std::collections::BTreeSet<PinDisclosure>>);

impl PinDisclosures {
    fn bind(self, key: PinDisclosure) -> (Memo<bool>, Callback<()>) {
        let key = StoredValue::new(key);
        let open = Memo::new(move |_| self.0.with(|keys| key.with_value(|k| keys.contains(k))));
        let toggle = Callback::new(move |_| {
            self.0.update(|keys| {
                let key = key.get_value();
                if !keys.remove(&key) {
                    keys.insert(key);
                }
            })
        });
        (open, toggle)
    }
}

#[component]
fn LiveMediaThumbs(
    items: Memo<Vec<ReportedMedia>>,
    all: Memo<bool>,
    toggle: Callback<()>,
) -> impl IntoView {
    let parsed = Memo::new(move |_| items.with(|items| media_of(items)));
    let more = Memo::new(move |_| parsed.with(|(media, _)| media.len().saturating_sub(3)));
    let media = Memo::new(move |_| parsed.with(|(media, _)| media.clone()));
    let limit = Memo::new(move |_| if all.get() { usize::MAX } else { 3 });
    view! {
        <div class="to-media">
            <LiveMediaList items=media limit=limit/>
            <Show when={move || more.get() > 0}><button class="link" data-focus-role="media-more" on:click=move |_| toggle.run(())>
                {move || if all.get() { "Fewer".to_string() } else { format!("{} more", more.get()) }}
            </button></Show>
            {move || parsed.with(|(_, outside)| (*outside > 0).then(|| view! { <p class="q3 to-sub">{format!("{} kept outside the share, so not shown here.", if *outside == 1 { "1 more is".to_string() } else { format!("{outside} more are") })}</p> }))}
        </div>
    }
}

#[component]
fn LiveToOpenRow(pin: String, d: Memo<Deliverable>, identity: String) -> impl IntoView {
    let store = expect_context::<OpenedStore>();
    let pin = StoredValue::new(pin);
    let id = StoredValue::new(d.get_untracked().id);
    let opened = Signal::derive(move || store.for_one(&pin.get_value(), &id.get_value()));
    let main = Memo::new(move |_| d.with(|d| d.main == Some(true)));
    let version = Memo::new(move |_| d.with(|d| d.version.clone()));
    let action = Memo::new(move |_| d.with(|d| opener(d.path.as_deref(), d.url.as_deref())));
    let media = Memo::new(move |_| d.with(|d| d.media.clone()));
    let older = Memo::new(move |_| d.with(|d| d.versions.clone()));
    let older_keys = Memo::new(move |_| {
        older.with(|versions| row_keys(versions.iter().map(|v| v.version.clone())))
    });
    let (older_keys, older_order_id) = hold_order(older_keys, None);
    let disclosures = expect_context::<PinDisclosures>();
    let (show_old, toggle_versions) = disclosures.bind(PinDisclosure::Versions(identity.clone()));
    let (show_media, toggle_media) = disclosures.bind(PinDisclosure::Media(identity.clone()));
    let on_open = Callback::new(move |_| {
        store.record(opened_key(
            &pin.get_value(),
            &id.get_value(),
            &version.get_untracked(),
        ))
    });
    view! {
        <li class="to-row" class:main=move || main.get() data-focus-key=identity>
            <span class="to-kind cap">{move || d.with(|d| kind_words(&d.kind).to_string())}</span>
            <div class="to-main">
                <b class="to-name">{move || d.with(|d| d.name.clone())}</b>
                <p class="to-line">
                    <b class="to-ver">{move || version.get()}</b>
                    {move || d.with(|d| d.updated_at.as_deref().map(est_clock).filter(|c| !c.is_empty()).map(|when| view! { <span class="q3">{when}</span> }))}
                    {move || mark_view(mark(&version.get(), &opened.get()))}
                </p>
                <Show when=move || action.with(|o| matches!(o, Opener::CopyPath(_)))><p class="q3 to-sub">"Kept outside the share, so it cannot be opened here"</p></Show>
                {move || d.with(|d| d.note.clone().map(|note| view! { <p class="to-sub">{note}</p> }))}
                <Show when=move || !media.with(Vec::is_empty)><LiveMediaThumbs items=media all=show_media toggle=toggle_media/></Show>
                <Show when=move || !older.with(Vec::is_empty)>
                    <p class="to-sub q3">
                        {move || older.with(|versions| if versions.len() == 1 { "1 older version".to_string() } else { format!("{} older versions", versions.len()) })}" "
                        <button class="link" data-focus-role="versions-toggle" aria-expanded=move || show_old.get().to_string() on:click=move |_| toggle_versions.run(())>{move || if show_old.get() { "Hide" } else { "Show" }}</button>
                    </p>
                </Show>
                <Show when=move || show_old.get()>
                    <ul class="to-older" data-focus-list=older_order_id>
                        <For each=move || older_keys.get() key=|key| key.clone() children=move |(label, occurrence)| {
                            let label = StoredValue::new(label);
                            let row = Memo::new(move |_| older.with(|versions| versions.iter().filter(|v| v.version == label.get_value()).nth(occurrence).cloned().unwrap_or_default()));
                            let action = Memo::new(move |_| row.with(|v| opener(v.path.as_deref(), v.url.as_deref())));
                            let on_open = Callback::new(move |_| store.record(opened_key(&pin.get_value(), &id.get_value(), &label.get_value())));
                            view! {
                                <li data-focus-key=format!("version:{}", serde_json::to_string(&(label.get_value(), occurrence)).unwrap_or_default())><span><b>{label.get_value()}</b>
                                    {move || row.with(|v| v.updated_at.as_deref().map(est_clock).filter(|s| !s.is_empty()).map(|when| format!(", {when}")))}
                                    ", "<span class="q3">{move || format!("Superseded by {}", version.get())}</span>
                                    {move || opened.with(|opened| opened.iter().find(|(v, _)| *v == label.get_value()).map(|(_, at)| view! { " "{mark_view(Mark::Opened(*at))} }))}
                                </span>{move || open_action(action.get(), false, on_open)}</li>
                            }
                        }/>
                    </ul>
                </Show>
            </div>
            <span class="to-act">{move || open_action(action.get(), main.get(), on_open)}</span>
        </li>
    }
}

#[component]
fn LiveToOpenSection(pin: String, list: Memo<Vec<Deliverable>>) -> impl IntoView {
    let store = expect_context::<OpenedStore>();
    let pin = StoredValue::new(pin);
    let (all, toggle) = expect_context::<PinDisclosures>().bind(PinDisclosure::Deliverables);
    let available =
        Memo::new(move |_| list.with(|list| row_keys(list.iter().map(|d| d.id.clone()))));
    let limit = Memo::new(move |_| if all.get() { usize::MAX } else { 6 });
    let (keys, order_id) = hold_order(available, Some(limit));
    let more = Memo::new(move |_| list.with(|list| list.len().saturating_sub(6)));
    view! {
        <section class="c2-sec">
            <div class="c2-sec-head"><h4 class="cap">"To open"</h4>
                <Show when=move || !list.with(Vec::is_empty)><span class="n">{move || list.with(|list| list.len().to_string())}</span>
                    <Show when=move || list.with(|list| list.iter().all(|d| store.for_one(&pin.get_value(), &d.id).is_empty()))><span class="c2-sec-end">"all new to you"</span></Show>
                </Show>
            </div>
            <Show when=move || !list.with(Vec::is_empty) fallback=|| view! { <p class="q3 c2-none">"Nothing reported to open."</p> }>
                <ul class="to-list" data-focus-list=order_id>
                    <For each=move || keys.get() key=|key| key.clone() children=move |(id, occurrence)| {
                        let identity = format!("deliverable:{}", serde_json::to_string(&(id.clone(), occurrence)).unwrap_or_default());
                        let d = Memo::new(move |_| list.with(|list| list.iter().filter(|d| d.id == id).nth(occurrence).cloned().unwrap_or_default()));
                        view! { <LiveToOpenRow pin=pin.get_value() d=d identity=identity/> }
                    }/>
                </ul>
                <Show when={move || more.get() > 0}><p class="c2-fold"><span>{move || format!("and {} more", more.get())}</span>
                    <button class="link" on:click=move |_| toggle.run(())>{move || if all.get() { "Fewer" } else { "Show" }}</button>
                </p></Show>
            </Show>
        </section>
    }
}

// ---- The five fixed facts --------------------------------------------------------------------------------------------

fn fact(label: &'static str, main: AnyView, right: Option<AnyView>) -> impl IntoView {
    view! { <dt>{label}</dt><dd>{main}</dd><dd class="t">{right}</dd> }
}

/// The Answer button: brings the question deck into view on this thread's first open card.
fn answer_button(p: &Pin) -> Option<AnyView> {
    let card = p.cards.first()?.id.clone();
    let jump = use_context::<CardJump>();
    Some(
        view! {
            <button class="btn sm primary c2-answer" on:click=move |e| {
                e.stop_propagation();
                if let Some(jump) = jump { jump.0.set(Some(card.clone())); }
            }>"Answer"</button>
        }
        .into_any(),
    )
}

fn facts(p: &Pin, elapsed: i64) -> impl IntoView + use<> {
    // Needs you: the open question itself, and ready deliverables (concept A). Always said, "Nothing" when nothing.
    let mut for_you: Vec<AnyView> = Vec::new();
    let mut for_you_when: Option<String> = None;
    let ready = p.ready();
    if !ready.is_empty() {
        let then = ready
            .iter()
            .find(|d| d.main == Some(true))
            .or(ready.first())
            .and_then(|d| d.unlocks.clone());
        for_you.push(view! { <p><b data-tone="warn">"Ready"</b>{format!(" {}, below", things(ready.len()))}{then.map(|then| view! { <small>{then}</small> })}</p> }.into_any());
        for_you_when = ready_since(p).map(|t| format!("since {t}"));
    }
    let mut answer = None;
    if p.word == Word::WaitingOnYou {
        if let Some(title) =
            p.u.card_title
                .clone()
                .or_else(|| p.cards.first().map(|card| card.title.clone()))
        {
            for_you.push(view! { <p><b data-tone="warn">{title}</b></p> }.into_any());
        }
        for_you_when = cards_since(p)
            .map(|t| format!("since {t}"))
            .or(for_you_when);
        answer = answer_button(p);
    }
    let needs_you = !for_you.is_empty();
    let for_you_view = if needs_you {
        for_you.into_iter().collect_view().into_any()
    } else {
        view! { <span class="q3">"Nothing"</span> }.into_any()
    };
    let for_you_right = (for_you_when.is_some() || answer.is_some())
        .then(|| view! { {for_you_when.map(|w| view! { <span>{w}</span> })}{answer} }.into_any());

    // Waiting on: every wait that is not the owner's, shown beside an owner wait, never behind it.
    let now = js_sys::Date::now();
    // Each wait's head, words and time ("for 20 h", or "was due 1:20 AM" in amber once past due).
    let mut rows: Vec<(String, String, Option<(String, bool)>)> = Vec::new();
    let when = |since: Option<&str>, until: Option<&str>| {
        let late = until.and_then(iso_ms).filter(|u| *u < now);
        match (late, since.and_then(iso_ms)) {
            (Some(_), _) => Some((
                format!("was due {}", est_clock(until.unwrap_or_default())),
                true,
            )),
            (None, Some(s)) => Some((format!("for {}", since_ms(s)), false)),
            _ => None,
        }
    };
    if !p.u.waits.is_empty() {
        for w in &p.u.waits {
            rows.push((
                other_wait_head(w),
                wait_words(w),
                w.until_time
                    .as_ref()
                    .map(|t| (t.text.clone(), t.late))
                    .or_else(|| when(w.since.as_deref(), w.until.as_deref())),
            ));
        }
    } else {
        // A server without structured waits: the report's own non-owner blockers.
        for b in
            p.r.blockers
                .iter()
                .chain(p.r.blocked_on.iter())
                .filter(|b| b.who() != "owner")
        {
            rows.push((
                who_words(b.who()).to_string(),
                b.words(),
                when(b.since.as_deref(), b.until.as_deref()),
            ));
        }
    }
    // One wait keeps its time in the right column. Several keep each time under its own wait: stacked in the right
    // column, three times sat apart from the waits they date (a phone put them all under the last one).
    let several = rows.len() > 1;
    let mut waits: Vec<AnyView> = Vec::new();
    let mut wait_when: Vec<AnyView> = Vec::new();
    for (head, words, time) in rows {
        let tone = |late: bool| late.then_some("warn");
        if several {
            waits.push(view! { <p><b>{head}</b>{format!(" {words}")}{time.map(|(t, late)| view! { <small class="fx-when" data-tone=tone(late)>{t}</small> })}</p> }.into_any());
        } else {
            waits.push(view! { <p><b>{head}</b>{format!(" {words}")}</p> }.into_any());
            if let Some((t, late)) = time {
                wait_when.push(view! { <span data-tone=tone(late)>{t}</span> }.into_any());
            }
        }
    }
    if waits.is_empty() && p.u.waits.is_empty() && !p.u.blocked.waiting_on_you {
        let t = p.u.blocked.text.trim().to_string();
        if !t.is_empty() && t != "None." && t != "None" {
            waits.push(view! { <p>{t}</p> }.into_any());
        }
    }
    let waiting_view = (!waits.is_empty()).then(|| waits.into_iter().collect_view().into_any());
    // A heartbeat carries the last doing line forward: when the human update is older, the Now line says the human
    // update's age, whether it is the summary, a reported `now` or a carried doing line (review 1, L4). A heartbeat
    // never dates the words it carries.
    let ages = two_ages(&p.u);
    let now_line = now_text(p);
    let now_view = now_line.map(|n| {
        view! { <span>{n}</span>{p.r.more.clone().map(|more| view! { <small>{more}</small> })} }
            .into_any()
    });
    let now_when = ages
        .map(|(human, _)| view! { <span data-tone={(human > 6 * 3_600).then_some("warn")}>{format!("as of {}", update_ago(human + elapsed))}</span> }.into_any());
    // Next always has a line: a thread that gives none says so, never a blank row (contract v3, section 8).
    let next_view = match p.r.next.clone() {
        Some(n) => view! { <span>{n}</span> }.into_any(),
        None => view! { <span class="q3">"Not said"</span> }.into_any(),
    };
    let next_when = p.u.next_time.clone().map(|time| {
        let soft = soft_time(&time.text).then_some("1");
        view! { <span class="c2-when" data-soft=soft data-tone=time.late.then_some("warn")>{time.text}</span> }.into_any()
    });
    let thin = p.u.thin_report.clone();
    view! {
        <dl class="fx">
            <dt data-k=needs_you.then_some("you")>"Needs you"</dt><dd data-k=needs_you.then_some("you")>{for_you_view}</dd><dd class="t" data-k=needs_you.then_some("you")>{for_you_right}</dd>
            {waiting_view.map(|v| fact("Waiting on", v, (!wait_when.is_empty()).then(|| wait_when.into_iter().map(|w| view! { <span class="fx-when">{w}</span> }).collect_view().into_any())).into_any())}
            {now_view.map(|v| fact("Now", v, now_when).into_any())}
            {fact("Next", next_view, next_when)}
        </dl>
        {thin.map(|line| view! { <p class="q3 c2-sub c2-thin">{line}</p> })}
    }
}

/// A time that is words rather than a clock ("no time: Event Authority has given no date", "after the merge lands")
/// sits under its line instead of in the narrow time column.
fn soft_time(text: &str) -> bool {
    let t = text.trim();
    !t.starts_with("late") && (t.starts_with("no time") || t.chars().count() > 16)
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

#[component]
fn LiveStages(pin: Memo<Option<Pin>>) -> impl IntoView {
    let stages = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref()
                .map(|p| {
                    p.r.stages
                        .iter()
                        .filter(|s| s.state != "skipped")
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    });
    let available =
        Memo::new(move |_| stages.with(|stages| row_keys(stages.iter().map(|s| s.label.clone()))));
    let (keys, order_id) = hold_order(available, None);
    view! {
        <section class="c2-sec" data-focus-list=order_id>
            <div class="c2-sec-head"><h4 class="cap">"Stages"</h4>
                {move || pin.with(|p| p.as_ref().and_then(|p| stage_line(&p.r.stages)).map(|(t, at, of)| view! { <span class="c2-sec-end">{format!("{t}, ")}<b>{at.to_string()}</b>{format!(" of {of}")}</span> }))}
            </div>
            <Show when={move || stages.with(Vec::len) >= 2} fallback=move || view! { <p class="q3 c2-none">{move || pin.with(|p| p.as_ref().map(foot_words).unwrap_or_default())}</p> }>
                <div class="c2-track" aria-hidden="true">
                    <For each=move || keys.get() key=|key| key.clone() children=move |(label, occurrence)| {
                        let s = Memo::new(move |_| stages.with(|stages| stages.iter().filter(|s| s.label == label).nth(occurrence).cloned().unwrap_or_default()));
                        view! { <i class="fill" data-st=move || s.with(stage_tone)></i> }
                    }/>
                </div>
                <ol class="c2-stages" style=move || format!("--n:{}", keys.with(Vec::len))>
                    <For each=move || keys.get() key=|key| key.clone() children=move |(label, occurrence)| {
                        let identity = format!("stage:{}", serde_json::to_string(&(label.clone(), occurrence)).unwrap_or_default());
                        let s = Memo::new(move |_| stages.with(|stages| stages.iter().filter(|s| s.label == label).nth(occurrence).cloned().unwrap_or_default()));
                        view! { <li data-focus-key=identity data-st=move || s.with(stage_tone)><span class="seg fill"></span><span class="lab">{move || s.with(stage_label)}</span><span class="st">{move || s.with(stage_state_words)}</span></li> }
                    }/>
                </ol>
                {move || stages.with(|stages| { let yours = stages.iter().filter(|s| s.by.as_deref() == Some("owner")).count(); (yours > 0).then(|| view! { <p class="q3 c2-sub">{format!("{yours} of these {} steps {} yours.", stages.len(), if yours == 1 { "is" } else { "are" })}</p> }) })}
            </Show>
        </section>
    }
}

#[component]
fn TaskGroupView(g: Signal<TaskGroup>, group: &'static str) -> impl IntoView {
    let (open, toggle) = expect_context::<PinDisclosures>().bind(PinDisclosure::Tasks(group));
    let folds = move || g.with(TaskGroup::folds);
    let n = move || g.with(|g| g.rows.len());
    let label = move || {
        g.with(|g| {
            if g.label == "Left" {
                "Not done"
            } else {
                g.label
            }
        })
    };
    let rows = Memo::new(move |_| g.with(|g| g.rows.clone()));
    let available =
        Memo::new(move |_| rows.with(|rows| row_keys(rows.iter().map(|r| r.id.clone()))));
    let (keys, order_id) = hold_order(available, None);
    view! {
        <div class="c2-tg" data-focus-key=move || g.with(|g| format!("task-group:{}", g.label))>
            <h5 class="c2-wg-h" data-tone=move || g.with(|g| g.warn.then_some("warn"))>{label}" "<span class="n">{move || n().to_string()}</span></h5>
            <Show when=move || folds() && !open.get() fallback=move || view! {
                    <ul class="c2-tg-rows" data-focus-list=order_id>
                        <For each=move || keys.get() key=|key| key.clone() children=move |(id, occurrence)| {
                            let identity = format!("task:{}", serde_json::to_string(&(id.clone(), occurrence)).unwrap_or_default());
                            let shown_id = id.clone();
                            let r = Memo::new(move |_| rows.with(|rows| rows.iter().filter(|r| r.id == id).nth(occurrence).cloned().unwrap_or_default()));
                            view! { <li data-focus-key=identity class="c2-wr" data-due=move || r.with(|r| r.time.as_ref().is_some_and(|t| soft_time(&t.text)).then_some("soft"))>
                                <span class="c2-w-title"><span class="c2-tg-t">{move || r.with(|r| r.title.clone())}</span>" "<span class="c2-id">{shown_id}</span></span>
                                <span class="c2-tg-n">
                                    {move || r.with(|r| r.odd_state.clone().map(|state| view! { <span data-tone="warn">{format!("reported as \"{state}\"")}</span>{r.note.is_some().then(|| view! { <br/> })} }))}
                                    {move || r.with(|r| r.note.clone().map(|note| view! { <span>{note}</span> }))}
                                    {move || r.with(|r| r.needs_you.clone().map(|title| view! { {(r.note.is_some() || r.odd_state.is_some()).then(|| view! { <br/> })}<span data-tone="warn">{format!("Needs you: {title}")}</span> }))}
                                </span>
                                <span class="c2-w-wait">{move || r.with(|r| r.wait.clone())}</span>
                                <span class="c2-w-due">{move || r.with(|r| r.time.clone().map(|time| view! { <span data-tone=time.late.then_some("warn")>{time.text}</span> }))}</span>
                            </li> }
                        }/>
                        <Show when=folds><li class="c2-tg-less"><button class="link" on:click=move |_| toggle.run(())>"Fewer"</button></li></Show>
                    </ul>
            }>
                <p class="c2-tg-fold"><span>{move || plural_tasks(n())}</span>
                    <button class="link" on:click=move |_| toggle.run(())>{move || format!("Show all {}", n())}</button></p>
            </Show>
        </div>
    }
}

fn plural_tasks(n: usize) -> String {
    if n == 1 {
        "1 task".into()
    } else {
        format!("{n} tasks")
    }
}

fn tasks_as_of(p: &Pin) -> Option<AnyView> {
    // Lists from the status document, not a board: as old as the human update, whatever the heartbeat says.
    if p.u.board.is_none()
        && let (Some((human, _)), Some(s)) = (two_ages(&p.u), p.u.status_updated.as_ref())
    {
        return Some(view! { <span class="c2-sec-end" data-tone={(human > 6 * 3_600).then_some("warn")}>{format!("as of {}, {} old", est_clock(&s.at), update_age(human))}</span> }.into_any());
    }
    // The list's own time, amber when it is 6 h older than the thread's report or its board is stale.
    let list_time = p.board.as_ref().and_then(|b| b.as_of.clone()).map(|at| {
        let list_ms = iso_ms(&at);
        let report_ms = p.u.updated.as_ref().and_then(|u| iso_ms(&u.at));
        let old = matches!((list_ms, report_ms), (Some(l), Some(r)) if r - l > 6.0 * 3_600_000.0)
            || p.board.as_ref().is_some_and(|b| b.stale);
        let words = match (old, list_ms) {
            (true, Some(l)) => format!("as of {}, {} old", est_clock(&at), since_ms(l)),
            _ => format!("as of {}", est_clock(&at)),
        };
        (words, old)
    });
    match (shared_board_words(&p.u), list_time) {
        // Another writer's board: say whose, so its rounds never read as this thread's own, and keep the list's time
        // beside it (wave 8 showed only whose, so a worker lost how old its tasks are).
        (Some(whose), time) => Some(view! { <span class="c2-sec-end" data-board="shared">{whose}{time.map(|(words, old)| view! { <span class="nw">{" · "}</span><span class="nw" data-tone=old.then_some("warn")>{words}</span> })}</span> }.into_any()),
        (None, Some((words, old))) => Some(view! { <span class="c2-sec-end" data-tone=old.then_some("warn")>{words}</span> }.into_any()),
        (None, None) => None,
    }
}

/// The open rows of a pin, in the sheet's groups (Done rows are under Finished today).
fn open_groups(p: &Pin) -> Vec<TaskGroup> {
    task_groups(&p.u, p.board.as_ref())
        .into_iter()
        .filter(|g| g.label != "Done")
        .collect()
}

/// The finished rows of a pin: those finished today (EST), newest first, and the earlier ones.
fn finished_rows(p: &Pin) -> (Vec<TaskRow>, Vec<TaskRow>) {
    let done = task_groups(&p.u, p.board.as_ref())
        .into_iter()
        .find(|g| g.label == "Done")
        .map(|g| g.rows)
        .unwrap_or_default();
    finished_today(&done, js_sys::Date::now(), iso_ms)
}

#[component]
fn LiveTasks(pin: Memo<Option<Pin>>) -> impl IntoView {
    let count = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref()
                .map(|p| p.u.left.len() + p.u.done.len())
                .unwrap_or(0)
        })
    });
    let groups = Memo::new(move |_| pin.with(|p| p.as_ref().map(open_groups).unwrap_or_default()));
    let open_n = Memo::new(move |_| groups.with(|g| g.iter().map(|g| g.rows.len()).sum::<usize>()));
    let counts = Memo::new(move |_| groups.with(|g| counts_words(&work_counts(g))));
    let available = Memo::new(move |_| {
        groups.with(|groups| groups.iter().map(|g| g.label).collect::<Vec<_>>())
    });
    let (keys, order_id) = hold_order(available, None);
    view! {
        <section class="c2-sec c2-work" data-focus-list=order_id>
            <div class="c2-sec-head"><h4 class="cap">"Open work"</h4><Show when={move || count.get() > 0}><span class="n">{move || open_n.get().to_string()}</span></Show>
                <span class="c2-sec-end c2-counts">{move || counts.get()}</span>
                {move || pin.with(|p| p.as_ref().and_then(tasks_as_of))}</div>
            <Show when={move || count.get() > 0} fallback=move || view! {
                <p class="q3 c2-none">{move || pin.with(|p| p.as_ref().and_then(|p| p.u.percent_note.clone()).unwrap_or_else(|| "No task list reported.".into()))}</p>
            }>
                <Show when={move || open_n.get() > 0} fallback=|| view! { <p class="q3 c2-none">"Nothing open."</p> }>
                    <div class="c2-wt-h cap" aria-hidden="true"><span>"Task"</span><span>"What is happening"</span><span>"Waits on"</span><span>"Due"</span></div>
                </Show>
                <For each=move || keys.get() key=|label| *label children=move |label| {
                    let group = Memo::new(move |_| groups.with(|groups| groups.iter().find(|g| g.label == label).cloned().unwrap_or_default()));
                    view! { <TaskGroupView g=group.into() group=label/> }
                }/>
            </Show>
        </section>
    }
}

#[component]
fn LiveFinished(pin: Memo<Option<Pin>>) -> impl IntoView {
    let split = Memo::new(move |_| pin.with(|p| p.as_ref().map(finished_rows).unwrap_or_default()));
    let (all, toggle) = expect_context::<PinDisclosures>().bind(PinDisclosure::Tasks("Finished"));
    let row = |r: TaskRow| {
        let words = r.note.clone().unwrap_or_else(|| r.title.clone());
        // The server's time words; else the finish time itself, in EST.
        let when = r.time.map(|t| t.text).or_else(|| {
            r.done_at
                .as_deref()
                .map(est_clock)
                .filter(|c| !c.is_empty())
        });
        view! { <li><span class="c2-fin-t">{when}</span><span class="c2-fin-w">{words}</span></li> }
    };
    view! {
        <Show when=move || split.with(|(today, earlier)| !today.is_empty() || !earlier.is_empty())>
            <section class="c2-sec c2-fin">
                <div class="c2-sec-head"><h4 class="cap">"Finished today"</h4><span class="n">{move || split.with(|(t, _)| t.len().to_string())}</span></div>
                <Show when=move || split.with(|(t, _)| !t.is_empty()) fallback=|| view! { <p class="q3 c2-none">"Nothing finished today."</p> }>
                    <ul class="c2-fin-rows">{move || split.with(|(t, _)| t.clone()).into_iter().map(row).collect_view()}</ul>
                </Show>
                <Show when=move || split.with(|(_, e)| !e.is_empty())>
                    <p class="c2-fin-more q3">
                        <span>{move || split.with(|(_, e)| if e.len() == 1 { "1 more finished earlier".to_string() } else { format!("{} more finished earlier", e.len()) })}</span>
                        " "<button class="link" aria-expanded=move || all.get().to_string() on:click=move |_| toggle.run(())>{move || if all.get() { "Hide" } else { "Show" }}</button>
                    </p>
                    <Show when=move || all.get()>
                        <ul class="c2-fin-rows c2-fin-earlier">{move || split.with(|(_, e)| e.clone()).into_iter().map(row).collect_view()}</ul>
                    </Show>
                </Show>
            </section>
        </Show>
    }
}

// ---- The sheet (wide pane, or the phone's bottom sheet) ----------------------------------------------------------------

/// "said 1:18 PM · active 1 min ago": when the words last changed, and when the thread was last active when that is
/// later (contract v3, section 8: two times). Amber when the words are six hours older than the heartbeat.
fn said_line(p: &Pin, elapsed: i64) -> Option<AnyView> {
    let said =
        p.u.reported
            .as_ref()
            .and_then(|r| r["saidAt"].as_str().map(str::to_string));
    let time = p.u.status_updated.as_ref().or(p.u.updated.as_ref())?;
    let clock = said
        .as_deref()
        .map(est_clock)
        .filter(|c| !c.is_empty())
        .or_else(|| time.clock.clone())
        .unwrap_or_else(|| est_clock(&time.at));
    let verb = if said.is_some() { "said" } else { "updated" };
    Some(match two_ages(&p.u) {
        Some((human, beat)) => view! {
            <span class="c2-said" data-tone={(human > 6 * 3_600).then_some("warn")} title=two_ages_words(human + elapsed, beat + elapsed)>
                <span class="nw">{format!("{verb} {clock}")}</span>" · "<span class="nw">{format!("active {}", update_ago(beat + elapsed))}</span>
            </span>
        }.into_any(),
        None => view! {
            <span class="c2-said" title=time.age_seconds.map(|a| fmt::long_age(a + elapsed))><span class="nw">{format!("{verb} {clock}")}</span></span>
        }.into_any(),
    })
}

fn sheet_heading(p: &Pin, elapsed: i64) -> impl IntoView + use<> {
    let since = match p.word {
        Word::WaitingOnYou => cards_since(p).map(|t| format!("since {t}")),
        _ => None,
    };
    let cached = p.u.report_state == "unreadable";
    let meta = [
        p.u.who.clone(),
        crate::status::machine_label(&p.u.machine).map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let problems = p.problems();
    let said = (!matches!(p.word, Word::NotReporting))
        .then(|| said_line(p, elapsed))
        .flatten();
    view! {
        <header class="c2-head">
            <p class="c2-state"><b data-tone=p.word.tone()>{p.word.text()}</b>{since.map(|s| view! { " "<span class="q3">{s}</span> })}{said.map(|s| view! { " "{s} })}</p>
            <h3 class="c2-name">{p.title()}</h3>
            {(!meta.is_empty()).then(|| view! { <p class="c2-meta">{meta.join("  ·  ")}</p> })}
            {p.r.goal.clone().map(|goal| view! { <p class="c2-goal"><span class="cap">"Goal"</span>" "{goal}</p> })}
        </header>
        {(p.u.refusal.is_some() && !cached).then(|| view! {
            <p class="pin-cached" data-tone="warn">"This report could not be read. See Report problems above the pinned list."</p>
        })}
        {cached.then(|| cached_notice(&p.u))}
        {problems.into_iter().skip(usize::from(cached)).map(|(_, full)| view! { <p class="c2-prob" data-tone="warn">{full}</p> }).collect_view()}
    }
}

fn quiet_body(p: &Pin) -> impl IntoView + use<> {
    let note =
        p.u.stale_note
            .clone()
            .or_else(|| p.u.report_note.clone())
            .unwrap_or_default();
    // The summary, else the last doing line with its age: a thread with no status file still said what it did.
    let last = quiet_last(&p.u, 0).map(|s| format!(" {s}"));
    view! { <p class="c2-quiet"><span class="w-ink">{note}</span>{last}</p> }
}

/// Whether a quiet thread reported anything beyond its words: stages, a task list or progress, or things to open.
fn quiet_has_lists(p: &Pin) -> bool {
    !p.r.deliverables.is_empty()
        || !p.r.stages.is_empty()
        || !p.u.left.is_empty()
        || !p.u.done.is_empty()
        || p.u.percent.is_some()
        || p.r.progress.is_some()
}

#[component]
fn LivePinBody(pin: Memo<Option<Pin>>) -> impl IntoView {
    provide_context(PinDisclosures(RwSignal::new(Default::default())));
    let live = expect_context::<Live>();
    // Not reporting, or quiet with nothing but words: the note alone. Quiet with lists: the note, then what it reported
    // (stages, tasks, things to open), each with its own time; only the five facts, which would repeat the note's last
    // words, are left out.
    let silent = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref().is_none_or(|p| {
                matches!(p.word, Word::NotReporting)
                    || (matches!(p.word, Word::Quiet) && !quiet_has_lists(p))
            })
        })
    });
    let quiet = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref()
                .is_some_and(|p| matches!(p.word, Word::Quiet) && quiet_has_lists(p))
        })
    });
    // A new Now/age/status must not recreate media controls, disclosure state or the focused action.
    let pin_id = pin.with_untracked(|p| p.as_ref().map(|p| p.u.id.clone()).unwrap_or_default());
    let deliverables = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref()
                .map(|p| ordered(&p.r.deliverables))
                .unwrap_or_default()
        })
    });
    let has_stages = Memo::new(move |_| {
        pin.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.r.stages.iter().filter(|s| s.state != "skipped").count() >= 2
                    || (p.u.left.is_empty()
                        && p.u.done.is_empty()
                        && (p.u.percent.is_some() || p.r.progress.is_some()))
                    || (quiet_has_lists(p) && matches!(p.word, Word::Quiet))
            })
        })
    });
    view! {
        {move || pin.with(|p| p.as_ref().map(|p| sheet_heading(p, live.elapsed_seconds())))}
        <Show when=move || quiet.get()>{move || pin.with(|p| p.as_ref().map(quiet_body))}</Show>
        <Show when=move || !silent.get() fallback=move || pin.with(|p| p.as_ref().map(quiet_body))>
            <div class="c2-cols" class:dim=move || pin.with(|p| p.as_ref().is_some_and(|p| p.u.report_state == "unreadable"))>
                <div class="c2-col c2-col-facts">
                    {move || pin.with(|p| p.as_ref().filter(|_| !quiet.get()).map(|p| facts(p, live.elapsed_seconds())))}
                    <Show when=move || has_stages.get()><LiveStages pin=pin/></Show>
                </div>
                <div class="c2-col c2-col-side">
                    <LiveFinished pin=pin/>
                    <LiveToOpenSection pin=pin_id.clone() list=deliverables/>
                </div>
                <div class="c2-col c2-col-work">
                    <LiveTasks pin=pin/>
                </div>
            </div>
        </Show>
    }
}

// ---- The plate ---------------------------------------------------------------------------------------------------------

const ORDER: [&str; 7] = [
    "Needs you",
    "Blocked",
    "Working",
    "Waiting",
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
            Pin {
                u: u.clone(),
                r,
                word,
                cards,
                board,
            }
        })
        .collect();
    pins.sort_by_key(|p| {
        (
            ORDER.iter().position(|g| *g == p.word.group()).unwrap_or(9),
            p.word,
        )
    });
    Some((panel, pins))
}

fn is_silent(p: &Pin) -> bool {
    p.u.report_state == "none" && p.u.refusal.is_none()
}

/// The roster corner: "said 1:18 PM", or "asked Wed 3:00 PM" for a question; amber when the words are old.
fn corner_words(p: &Pin) -> Option<String> {
    if p.word == Word::WaitingOnYou
        && let Some(asked) = cards_since(p)
    {
        return Some(format!("asked {asked}"));
    }
    let said =
        p.u.reported
            .as_ref()
            .and_then(|r| r["saidAt"].as_str().map(str::to_string));
    let time = p.u.status_updated.as_ref().or(p.u.updated.as_ref());
    let clock = said
        .as_deref()
        .map(est_clock)
        .filter(|c| !c.is_empty())
        .or_else(|| time.and_then(|t| t.clock.clone()))
        .or_else(|| time.map(|t| est_clock(&t.at)))
        .filter(|c| !c.is_empty())?;
    Some(format!(
        "{} {clock}",
        if said.is_some() { "said" } else { "updated" }
    ))
}

/// The roster's small bar of open work: one segment per state, as wide as its count.
fn work_strip(counts: &[(&'static str, usize, &'static str)]) -> AnyView {
    let total: usize = counts.iter().map(|c| c.1).sum();
    view! {
        <span class="c2-strip" aria-hidden="true">
            {counts.iter().map(|(key, n, _)| view! { <i class="fill" data-st=*key style=format!("flex:{n} 1 0")></i> }).collect_view()}
            {(total == 0).then(|| view! { <i class="fill" data-st="todo" style="flex:1 1 0"></i> })}
        </span>
    }
    .into_any()
}

/// The roster foot: the thread's stages when it has them, else its open work by state, else its progress words.
fn foot_view(p: &Pin) -> (Option<AnyView>, String) {
    if let Some((text, at, of)) = stage_line(&p.r.stages) {
        return (mini_track(&p.r.stages), format!("{at} of {of}  {text}"));
    }
    let counts = work_counts(&open_groups(p));
    if !counts.is_empty() {
        return (Some(work_strip(&counts)), counts_words(&counts));
    }
    let words = foot_words(p);
    (
        None,
        if words == "No stages reported" {
            String::new()
        } else {
            words
        },
    )
}

#[component]
fn RosterRow(
    p: Pin,
    selected: Signal<bool>,
    open: Signal<bool>,
    on_pick: Callback<String>,
) -> impl IntoView {
    let id = p.u.id.clone();
    // The words' time, not a heartbeat's: a hook keeps thread.json fresh while the summary stays as old as it is.
    let age = roster_age(&p.u);
    let age_tone = age.as_ref().is_some_and(|(_, old)| *old).then_some("warn");
    let age_title = two_ages(&p.u)
        .map(|(human, beat)| two_ages_words(human, beat))
        .or_else(|| age.as_ref().map(|(a, _)| format!("updated {a} ago")));
    let corner = corner_words(&p).or_else(|| age.map(|(a, _)| a));
    // And when a heartbeat is newer, how long ago it was last active, under the words' time.
    let active = roster_active(&p.u);
    let problem = p.problems().into_iter().next().map(|(short, _)| short);
    let late = (p.u.late_count > 0).then(|| format!("{} late", p.u.late_count));
    let mute = matches!(p.word, Word::Quiet);
    let reporting = !matches!(p.word, Word::Quiet | Word::NotReporting);
    let needs = p.word == Word::WaitingOnYou;
    // Next, with its time in bold (late in amber); a thread that gives none says "not said".
    let next = (reporting && !needs).then(|| {
        let time = p.u.next_time.clone();
        let soft = time.as_ref().is_some_and(|t| soft_time(&t.text));
        view! {
            <span class="c2-next" class:wrap=soft>
                <span class="k">"Next"</span>
                {match p.r.next.clone() {
                    Some(n) => view! { <span class="v">{n}</span> }.into_any(),
                    None => view! { <span class="v q3">"not said"</span> }.into_any(),
                }}
                {time.map(|t| view! { <span class="c2-when" class:soft=soft data-tone=t.late.then_some("warn")>{t.text}</span> })}
            </span>
        }
    });
    let waited = needs.then(|| {
        p.cards
            .iter()
            .filter_map(|c| iso_ms(&c.asked_at))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|at| format!("{}; the thread is stopped until you answer", since_ms(at)))
            .unwrap_or_else(|| "The thread is stopped until you answer".into())
    });
    let answer = needs.then(|| answer_button(&p)).flatten();
    let (strip, foot) = foot_view(&p);
    let done_today = (!needs)
        .then(|| finished_rows(&p).0.len())
        .filter(|n| *n > 0);
    let thin = (reporting && !needs && p.u.thin_report.is_some()).then_some("Thin report");
    view! {
        <li class="c2-item" class:needs=needs class:sel=move || selected.get()>
            <button class="c2-row" class:sel=move || selected.get() class:open=move || open.get() class:quiet=mute aria-current=move || selected.get().then_some("true")
                on:click=move |_| on_pick.run(id.clone())>
                <span class="c2-l1"><span class="c2-title">{p.title()}</span><span class="c2-ages" title=age_title><span class="c2-age" data-tone=age_tone>{corner}</span>{active.map(|a| view! { <span class="c2-active">{a}</span> })}</span></span>
                <span class="c2-l2"><b data-tone=p.word.tone()>{p.word.text()}</b>{format!(" {}", lead(&p))}</span>
                {next}
                {(!needs).then(|| view! {
                    <span class="c2-l3">{strip}<span class="c2-stage">{foot}</span>
                        {done_today.map(|n| view! { <span class="c2-fin-n">{format!("{n} done today")}</span> })}
                        {thin.map(|t| view! { <span class="c2-thin-n" data-tone="warn">{t}</span> })}
                    </span>
                })}
                {late.map(|t| view! { <span class="c2-prob" data-tone="warn">{t}</span> })}
                {problem.map(|t| view! { <span class="c2-prob" data-tone="warn">{t}</span> })}
            </button>
            {waited.map(|w| view! { <div class="c2-ask"><span class="k">"Waiting"</span><span class="v">{w}</span>{answer}</div> })}
        </li>
    }
}

/// The question deck's jump: a pin's Answer sets the card to show; the deck shows it and comes into view.
#[derive(Clone, Copy)]
pub struct CardJump(pub RwSignal<Option<String>>);

fn next_step(p: &Pin) -> NextStep {
    let field = |key: &str| {
        p.u.reported
            .as_ref()
            .and_then(|r| r[key].as_str().map(str::to_string))
    };
    NextStep {
        id: p.u.id.clone(),
        title: p.title(),
        next: p.r.next.clone(),
        time: p.u.next_time.clone(),
        at_ms: field("nextBy").as_deref().and_then(iso_ms),
        day: field("nextByDay").as_deref().and_then(day_number),
    }
}

/// Coming up (concept B's strip on concept A, SR2 = C): every thread's next step with a time, soonest first, by day.
#[component]
fn ComingUp(
    data: Memo<Option<(PinnedPanel, Vec<Pin>)>>,
    on_pick: Callback<String>,
) -> impl IntoView {
    let coming = Memo::new(move |_| {
        data.with(|d| {
            let (_, pins) = d.as_ref()?;
            let steps: Vec<NextStep> = pins
                .iter()
                .filter(|p| {
                    !is_silent(p)
                        && matches!(
                            p.word,
                            Word::Working | Word::Waiting | Word::Blocked | Word::WaitingOnYou
                        )
                })
                .map(next_step)
                .collect();
            Some(coming_up(&steps, js_sys::Date::now()))
        })
        .unwrap_or_default()
    });
    view! {
        <section class="c2-coming" aria-label="Coming up" hidden=move || coming.with(|c| c.days.is_empty() && c.untimed == 0)>
            <h3 class="cap c2-cu-h">"Coming up"</h3>
            <div class="c2-cu-days">
                {move || coming.get().days.into_iter().map(|day| view! {
                    <div class="c2-cu-day">
                        <h4 class="cap">{day.label}</h4>
                        <ol class="c2-cu-list">
                            {day.items.into_iter().map(|item| {
                                let id = item.id.clone();
                                view! {
                                    <li><button class="c2-cu" on:click=move |_| on_pick.run(id.clone())>
                                        <b data-tone=item.late.then_some("warn")>{item.when}</b>
                                        <span class="c2-cu-next">{item.next}</span>
                                        <small>{item.title}</small>
                                    </button></li>
                                }
                            }).collect_view()}
                        </ol>
                    </div>
                }).collect_view()}
                {move || coming.with(|c| (c.untimed > 0).then(|| view! { <p class="c2-cu-rest q3">{untimed_words(c.untimed)}</p> }))}
            </div>
        </section>
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
        data.with(|d| {
            d.as_ref()
                .and_then(|(_, pins)| pins.iter().find(|p| p.u.id == id).cloned())
        })
    };
    let sheet_pin = Memo::new(move |_| pin_of(sheet_id.get()));
    Effect::new(move |_| {
        if sheet_id.with(|s| s.is_some()) && sheet_pin.with(|p| p.is_none()) {
            sheet_id.set(None);
        }
    });
    // Narrow is whenever the CSS hides the wide pane (its container query decides), so the two never disagree.
    let narrow = move || {
        document()
            .query_selector(".sec-pinned .c2-pane")
            .ok()
            .flatten()
            .map(|el| el.client_width() == 0)
            .unwrap_or(true)
    };
    let on_pick = Callback::new(move |id: String| {
        picked.set(Some(id.clone()));
        if narrow() {
            sheet_id.set(Some(id));
        }
    });
    let close_sheet = Callback::new(move |_: ()| sheet_id.set(None));
    let report_problems = Memo::new(move |_| {
        live.view
            .with(|v| v.as_ref().map(report_problem_lines).unwrap_or_default())
    });
    if let Some(frame) = use_context::<crate::frame::FrameCtx>() {
        Effect::new(move |_| {
            if frame.route.get() != crate::Page::Status {
                sheet_id.set(None);
            }
        });
    }
    view! {
        <section class="plate sec-pinned c2" id="pinned">
            <div class="plate-head">
                <h2>"Pinned threads"</h2>
                <span class="plate-meta">{move || data.with(|d| d.as_ref().map(|(panel, pins)| {
                    let needs = pins.iter().filter(|p| p.word == Word::WaitingOnYou).count();
                    view! {
                        <b>{panel.counts.pinned.to_string()}</b>" pinned in T3"
                        {(needs > 0).then(|| view! { ", "<b data-tone="warn">{format!("{needs} need you")}</b> })}
                    }
                }))}</span>
            </div>
            <details class="c2-report-problems" hidden=move || report_problems.with(Vec::is_empty)>
                <summary data-tone="warn">{move || {
                    let count = report_problems.with(Vec::len);
                    format!("{count} report problem{}", if count == 1 { "" } else { "s" })
                }}</summary>
                <ul>{move || report_problems.get().into_iter().map(|line| view! { <li>{line}</li> }).collect_view()}</ul>
            </details>
            <ComingUp data=data on_pick=on_pick/>
            {move || data.with(|d| match d {
                None => Some("Pinned threads are unavailable: this server doesn't send them yet.".to_string()),
                Some((p, pins)) => p.unavailable.clone().or_else(|| pins.is_empty().then(|| "No threads are pinned in T3.".to_string())),
            }).map(|words| view! { <p class="empty">{words}</p> })}
            <div class="c2-body" hidden=move || shown_id.with(Option::is_none)>
                <div class="c2-roster">
                    {move || {
                        let mut groups: Vec<(&'static str, Vec<Pin>)> = Vec::new();
                        data.with(|d| {
                            if let Some((_, pins)) = d {
                                for p in pins.iter().filter(|p| !is_silent(p)) {
                                    let g = p.word.group();
                                    match groups.iter_mut().find(|(x, _)| *x == g) {
                                        Some((_, list)) => list.push(p.clone()),
                                        None => groups.push((g, vec![p.clone()])),
                                    }
                                }
                            }
                        });
                        groups.into_iter().map(|(g, list)| view! {
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
                        }).collect_view()
                    }}
                </div>
                <div class="c2-pane" aria-live="polite">
                    // Match the portal's ownership: only a different pin starts a new set of controls.
                    <For each=move || shown_id.get() key=|id| id.clone() children=move |id| {
                        let pin = Memo::new(move |_| pin_of(Some(id.clone())));
                        view! { <article class="c2-sheet"><LivePinBody pin=pin/></article> }
                    }/>
                </div>
            </div>
            {move || data.with(|d| {
                let (_, pins) = d.as_ref()?;
                let silent: Vec<_> = pins.iter().filter(|p| is_silent(p)).map(Pin::title).collect();
                (!silent.is_empty()).then(|| view! {
                    <div class="c2-silent">
                        <h3 class="cap c2-group">"Not reporting "<span class="n">{silent.len().to_string()}</span></h3>
                        <ul>{silent.into_iter().map(|title| view! { <li><b>{title}</b><span class="q3">"no report yet"</span></li> }).collect_view()}</ul>
                        <p class="c2-note">"A pin shows its status once its thread runs agent-report update."</p>
                    </div>
                })
            })}
            // A status poll changes the report, not the lifetime of the open overlay. Keep the portal,
            // scroll container, drag state and stack token until this pin closes or a different pin opens.
            <For each=move || sheet_id.get() key=|id| id.clone() children=move |_| {
                let offset = RwSignal::new(0.0f64);
                view! {
                <OverlayPortal on_close=close_sheet>
                <div class="sheet-host c2-sheet-host" role="dialog" aria-modal="true"
                    aria-label=move || sheet_pin.with(|p| p.as_ref().map(Pin::title).unwrap_or_default())>
                    <button class="sheet-scrim" aria-label="Close" on:click=move |_| close_sheet.run(())></button>
                    <div class="sheet" style=move || sheet_drag_style(offset.get())>
                        <div class="sheet-top">
                            <SheetGrab offset=offset on_close=close_sheet/>
                            <button class="icon-btn bare sheet-close" aria-label="Close" on:click=move |_| close_sheet.run(())><Icon name="close"/></button>
                        </div>
                        <article class="c2-sheet"><LivePinBody pin=sheet_pin/></article>
                    </div>
                </div>
                </OverlayPortal>
            } }/>
        </section>
    }
}
