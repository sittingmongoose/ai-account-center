//! The Status page's summary strip (owner, 2026-11-12: the headline "just looks like an out of place paragraph").
//!
//! The headline's facts as counts: each a number in its tone over a short label, in one row, and one line naming the
//! worst pinned thread (PINNED-THREADS.md section 2). A tap on a count scrolls to the section that explains it; a
//! count no one section explains (sources not current, blocked items, items done) opens its sentence under the strip
//! instead, and so does the worst thread's line, whose title is cut to one line until then. Every number and word is
//! the server's (`headline.strip`, status-view-strip.ts); this file only lays them out. On the Duo upright the counts
//! sit in the display-only status band (`band_line`) and the worst thread's line stays on the page.

use crate::api::{Strip, StripItem, StripWorst};
use crate::ui::motion_on;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// The page's tone for a server tone: `bad` draws as `crit`, `quiet` as a faint zero or unknown, none as ink.
pub fn tone_attr(tone: Option<&str>) -> Option<&'static str> {
    match tone {
        Some("warn") => Some("warn"),
        Some("bad" | "crit") => Some("crit"),
        Some("good") => Some("good"),
        Some("quiet") => Some("quiet"),
        _ => None,
    }
}

/// The plate a count scrolls to, or None when no one section explains it (its sentence opens instead).
pub fn section_selector(section: Option<&str>) -> Option<&'static str> {
    match section? {
        "questions" => Some(".sec-questions"),
        "pinned" => Some(".sec-pinned"),
        "machines" => Some(".sec-machines"),
        "phones" => Some(".sec-phones"),
        "spend" => Some(".sec-spend"),
        _ => None,
    }
}

/// The key of the line opened under the strip for the worst pinned thread.
const WORST: &str = "worst";

/// The sentence opened under the strip for `key`, from the strip the server sent last. None when that count or the
/// worst thread is gone, so a refresh that drops it closes the line.
pub fn open_sentence(strip: &Strip, key: &str) -> Option<String> {
    if key == WORST {
        return strip.worst.as_ref().map(|w| w.detail.clone());
    }
    strip
        .items
        .iter()
        .find(|i| i.key == key && section_selector(i.section.as_deref()).is_none())
        .map(|i| i.detail.clone())
}

/// The worst thread's line as one sentence, for hover and screen readers.
pub fn worst_words(w: &StripWorst) -> String {
    let task = w
        .task
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .map(|t| format!(", task {t}"))
        .unwrap_or_default();
    // The composer's blocker often starts with the same words ("Waiting on you: approve."): say them once.
    let repeats = w.detail.to_lowercase().starts_with(&w.word.to_lowercase());
    if repeats {
        format!("{}{task}. {}", w.title, w.detail)
    } else {
        format!("{}: {}{task}. {}", w.word, w.title, w.detail)
    }
}

/// Scrolls the section to the top a moment later, once closing an open sentence has moved the page up: scrolling first
/// would leave the section that much too high, under the header. A plain browser timer, as Leptos cancels its own
/// when the branch that started it is redrawn (NOTES.md).
fn scroll_soon(selector: &'static str) {
    let callback = wasm_bindgen::closure::Closure::once_into_js(move || scroll_to(selector));
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 30);
}

fn scroll_to(selector: &str) {
    let Some(el) = document().query_selector(selector).ok().flatten() else {
        return;
    };
    // scrollIntoView with options, without another web-sys feature: the sections' scroll-margin-top keeps them clear
    // of a sticky header or band.
    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&opts, &"block".into(), &"start".into());
    let behavior = if motion_on() { "smooth" } else { "auto" };
    let _ = js_sys::Reflect::set(&opts, &"behavior".into(), &behavior.into());
    if let Ok(f) = js_sys::Reflect::get(&el, &"scrollIntoView".into()) {
        if let Ok(f) = f.dyn_into::<js_sys::Function>() {
            let _ = f.call1(&el, &opts);
        }
    }
}

fn toggle(open: RwSignal<Option<String>>, key: &str) {
    open.update(|o| {
        *o = if o.as_deref() == Some(key) {
            None
        } else {
            Some(key.to_string())
        }
    });
}

fn count_view(item: StripItem, open: RwSignal<Option<String>>) -> impl IntoView {
    let tone = tone_attr(item.tone.as_deref());
    let target = section_selector(item.section.as_deref());
    let key = item.key.clone();
    let key_open = item.key.clone();
    let on_click = move |_| match target {
        Some(sel) => {
            open.set(None);
            scroll_soon(sel);
        }
        None => toggle(open, &key),
    };
    // Only a count that opens a sentence is expandable; one that jumps is a plain button.
    let expanded = move || {
        target
            .is_none()
            .then(|| (open.get().as_deref() == Some(key_open.as_str())).to_string())
    };
    view! {
        <button type="button" class="sum-i" data-key=item.key.clone() title=item.detail.clone()
            aria-label=item.detail.clone() aria-expanded=expanded on:click=on_click>
            <b class="sum-n" data-tone=tone>{item.value}</b>
            <span class="sum-l"><span class="lbl-long">{item.label}</span><span class="lbl-short">{item.short}</span></span>
        </button>
    }
}

fn worst_view(w: StripWorst, open: RwSignal<Option<String>>) -> impl IntoView {
    let tone = tone_attr(w.tone.as_deref());
    let words = worst_words(&w);
    let is_open = move || open.get().as_deref() == Some(WORST);
    let task = w.task.filter(|t| !t.trim().is_empty());
    view! {
        <button type="button" class="sum-worst" class:open=is_open data-state=w.state title=words.clone()
            aria-label=words aria-expanded=move || is_open().to_string() on:click=move |_| toggle(open, WORST)>
            <span class="sw-word" data-tone=tone>{w.word}</span>
            <span class="sw-title">{w.title}</span>
            {task.map(|t| view! { <span class="sw-task">{t}</span> })}
        </button>
    }
}

/// The strip under the page title. `strip` is a memo, so a poll that changes nothing redraws nothing.
#[component]
pub fn SummaryStrip(strip: Memo<Option<Strip>>) -> impl IntoView {
    // The count or the worst thread whose sentence is open under the strip.
    let open = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let gone = strip.with(|s| {
            open.with(|o| match (s, o) {
                (Some(s), Some(k)) => open_sentence(s, k).is_none(),
                _ => false,
            })
        });
        if gone {
            open.set(None);
        }
    });
    let sentence = move || {
        let key = open.get()?;
        let text = strip.with(|s| s.as_ref().and_then(|s| open_sentence(s, &key)))?;
        Some(view! { <p class="sum-more" role="status" data-for=key>{text}</p> })
    };
    view! {
        <nav class="sum" aria-label="Status summary">
            {move || strip.get().map(|s| {
                let Strip { items, worst } = s;
                let has_worst = worst.is_some();
                // Seven counts or more (decisions or budgets on top of the usual six) need their own layout below
                // desktop, so no count is left alone on a line (CSS `.many`).
                let many = items.len() >= 7;
                view! {
                    <div class="sum-row" class:has-worst=has_worst class:many=many>
                        {items.into_iter().map(|i| count_view(i, open)).collect_view()}
                        {worst.map(|w| worst_view(w, open))}
                    </div>
                }
            })}
            {sentence}
        </nav>
    }
}

/// The counts in one line for the Duo's status band, which takes no taps (DUO-MEASURED.md): the short labels, cut
/// at the band's edge rather than wrapped into its top corner.
pub fn band_line(strip: &Strip) -> impl IntoView + use<> {
    strip
        .items
        .iter()
        .map(|i| {
            let tone = tone_attr(i.tone.as_deref());
            view! { <span class="bs-i" title=i.detail.clone()><b data-tone=tone>{i.value.clone()}</b>" "{i.short.clone()}</span> }
        })
        .collect_view()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> Strip {
        serde_json::from_str(
            r#"{"items":[
                {"key":"questions","value":"2","label":"questions for you","short":"questions","tone":"warn","section":"questions","detail":"2 questions for you."},
                {"key":"looping","value":"1","label":"thread looping","short":"looping","tone":"bad","section":"pinned","detail":"1 pinned thread is looping: A (task F-07)."},
                {"key":"done","value":"51/83","label":"items done","short":"done","tone":null,"section":null,"detail":"51 of 83 items landed or done."},
                {"key":"sources","value":"1","label":"source not current","short":"not current","tone":"warn","section":null,"detail":"Not current: Simlock: unreadable."}],
              "worst":{"state":"looping","word":"Looping","tone":"bad","threadId":"t1","title":"A","task":"F-07","detail":"Yes. F-07 failed 5 times."}}"#,
        )
        .expect("strip reads")
    }

    #[test]
    fn server_tones_draw_as_the_page_tones() {
        assert_eq!(tone_attr(Some("bad")), Some("crit"));
        assert_eq!(tone_attr(Some("warn")), Some("warn"));
        assert_eq!(tone_attr(Some("quiet")), Some("quiet"));
        assert_eq!(tone_attr(None), None);
        assert_eq!(tone_attr(Some("loud")), None);
    }

    #[test]
    fn a_count_jumps_to_its_section_or_opens_its_sentence() {
        let s = strip();
        assert_eq!(section_selector(Some("pinned")), Some(".sec-pinned"));
        assert_eq!(section_selector(Some("elsewhere")), None);
        assert_eq!(section_selector(None), None);
        // A count that jumps has no sentence to open; one with no section does.
        assert_eq!(open_sentence(&s, "questions"), None);
        assert_eq!(
            open_sentence(&s, "sources").as_deref(),
            Some("Not current: Simlock: unreadable.")
        );
        assert_eq!(
            open_sentence(&s, "worst").as_deref(),
            Some("Yes. F-07 failed 5 times.")
        );
        // A refresh that drops the count closes its sentence.
        let mut later = s.clone();
        later.items.retain(|i| i.key != "sources");
        later.worst = None;
        assert_eq!(open_sentence(&later, "sources"), None);
        assert_eq!(open_sentence(&later, "worst"), None);
    }

    #[test]
    fn the_worst_line_reads_as_one_sentence_with_its_task() {
        let s = strip();
        let w = s.worst.as_ref().expect("worst");
        assert_eq!(
            worst_words(w),
            "Looping: A, task F-07. Yes. F-07 failed 5 times."
        );
        let mut waiting = w.clone();
        waiting.word = "Waiting on you".into();
        waiting.task = None;
        waiting.detail = "Waiting on you: approve.".into();
        assert_eq!(worst_words(&waiting), "A. Waiting on you: approve.");
    }

    #[test]
    fn an_older_server_without_a_strip_still_reads() {
        let h: crate::api::Headline =
            serde_json::from_str(r#"{"tone":"warn","runs":[{"text":"2 questions"}],"counts":{}}"#)
                .expect("headline reads");
        assert!(h.strip.is_none());
        let nulls: crate::api::Headline = serde_json::from_str(
            r#"{"tone":"warn","runs":[],"counts":{},"strip":{"items":null,"worst":null}}"#,
        )
        .expect("nulls read");
        assert_eq!(nulls.strip.map(|s| s.items.len()), Some(0));
    }
}
