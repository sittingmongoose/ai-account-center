//! Plates, meters, bars, segmented controls and the other pieces every section is made of.

use super::icons::Icon;
use crate::api::{Run, Source};
use leptos::prelude::*;

/// A piece of a line: `strong` is bold; a tone colours it (warn, crit, good, acc).
pub fn runs_view(runs: Vec<Run>) -> impl IntoView {
    runs.into_iter()
        .map(|r| {
            let tone = r.tone;
            if r.strong {
                view! { <b data-tone=tone>{r.text}</b> }.into_any()
            } else {
                view! { <span data-tone=tone>{r.text}</span> }.into_any()
            }
        })
        .collect_view()
}

/// The 8 px track with a fill. `kind` is the fill's class: "", "warn", "crit", "acc", "ink", "claude" or "codex".
#[component]
pub fn Track(
    pct: f64,
    #[prop(optional)] kind: &'static str,
    #[prop(optional)] thin: bool,
    #[prop(optional)] label: Option<String>,
) -> impl IntoView {
    let width = pct.clamp(0.0, 100.0);
    view! {
        <div class="track" class:thin=thin role=label.is_some().then_some("img") aria-label=label>
            <div class=format!("fill {kind}") style=format!("width:{width:.1}%")></div>
        </div>
    }
}

/// The dashed track of a value that isn't known.
#[component]
pub fn TrackNa(#[prop(optional)] thin: bool) -> impl IntoView {
    view! { <div class="track na" class:thin=thin></div> }
}

/// Severity fill from a used percentage: warn from 80, crit from 95.
pub fn severity(pct: f64) -> &'static str {
    if pct >= 95.0 {
        "crit"
    } else if pct >= 80.0 {
        "warn"
    } else {
        ""
    }
}

/// The Home meter, smaller: a value with its unit, a severity-coloured track and an optional foot.
#[component]
pub fn Meter(
    value: String,
    unit: String,
    #[prop(into)] used: Option<f64>,
    #[prop(optional)] kind: Option<&'static str>,
) -> impl IntoView {
    view! {
        <div class="meter">
            <div class="meter-top"><span class="meter-val">{value}</span><span class="meter-unit">{unit}</span></div>
            {match used {
                Some(pct) => view! { <Track pct=pct kind=kind.unwrap_or_else(|| severity(pct))/> }.into_any(),
                None => view! { <TrackNa/> }.into_any(),
            }}
        </div>
    }
}

/// A rate with its likely range: the band is the 90% interval, the notch is the value.
#[component]
pub fn RangeBar(low: f64, high: f64, value: f64) -> impl IntoView {
    let (l, h) = (
        (low * 100.0).clamp(0.0, 100.0),
        (high * 100.0).clamp(0.0, 100.0),
    );
    let v = (value * 100.0).clamp(0.0, 100.0);
    view! {
        <div class="rangebar">
            <span class="rb-band" style=format!("left:{l:.1}%;width:{:.1}%", (h - l).max(0.0))></span>
            <span class="rb-notch" style=format!("left:{v:.1}%")></span>
        </div>
        <div class="rb-scale"><span>"0%"</span><span>"100%"</span></div>
    }
}

/// A chevron that opens or closes what follows it.
#[component]
pub fn Disclosure(open: RwSignal<bool>, label: String) -> impl IntoView {
    view! {
        <button class="disclosure" class:open=move || open.get() aria-expanded=move || open.get().to_string() on:click=move |_| open.update(|o| *o = !*o)>
            <Icon name="chev-right"/>{label}
        </button>
    }
}

/// A plate's segmented control. `options` are (id, label); the selected item sits on paper with a hairline.
#[component]
pub fn Segmented(
    options: Vec<(String, String)>,
    #[prop(into)] selected: Signal<String>,
    on_select: Callback<String>,
    #[prop(optional)] label: Option<&'static str>,
) -> impl IntoView {
    view! {
        <div class="seg" role="tablist" aria-label=label>
            {options.into_iter().map(|(id, text)| {
                let current = id.clone();
                let click = id.clone();
                view! {
                    <button role="tab" class:on=move || selected.get() == current
                        aria-selected={let id = id.clone(); move || (selected.get() == id).to_string()}
                        on:click=move |_| on_select.run(click.clone())>{text}</button>
                }
            }).collect_view()}
        </div>
    }
}

/// One line of a source that isn't `ok`, under a plate's head: why its reading can't be trusted right now.
pub fn source_note(source: Option<Source>, missing: &'static str) -> Option<AnyView> {
    match source {
        Some(s) if s.is_ok() => None,
        Some(s) => {
            let detail = if s.detail.is_empty() {
                s.label.clone()
            } else {
                s.detail.clone()
            };
            Some(view! { <p class="plate-note problem">{detail}</p> }.into_any())
        }
        None => Some(view! { <p class="plate-note problem">{missing}</p> }.into_any()),
    }
}

/// A plate's foot note: an info icon and one sentence, with an optional time on the right.
#[component]
pub fn FootNote(
    #[prop(into)] text: String,
    #[prop(optional)] when: Option<String>,
) -> impl IntoView {
    view! {
        <div class="plate-foot note">
            <Icon name="info"/><span>{text}</span>
            {when.map(|w| view! { <span class="when">{w}</span> })}
        </div>
    }
}
