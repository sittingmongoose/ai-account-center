//! Details (PLAN step 6): one account's every visible window, its balances and packs, the facts of its reading, and
//! the row's own action slot, drawn from `detailsViewModel` (view-model.mjs) through the seam (dash.rs). A side panel
//! from 760 px of window width (right of the content, left of a Duo rail), a bottom sheet below.
//!
//! It opens over any tab when a row sends `details`. A row tapped while it is open swaps it in place; any other tap
//! outside closes it, except while a switch confirmation waits (as in Slint). Closing sends `details-closed`, so the
//! dashboard forgets it and consumes its Back entry; Back closes it from the dashboard's side.

use crate::Page;
use crate::api::Run;
use crate::dash::nullable;
use crate::frame::FrameCtx;
use crate::status::{SheetGrab, sheet_drag_style};
use crate::ui::*;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MeterView {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub full_label: String,
    pub has_value: bool,
    pub value: f64,
    #[serde(deserialize_with = "nullable")]
    pub value_text: String,
    #[serde(deserialize_with = "nullable")]
    pub over_text: String,
    #[serde(deserialize_with = "nullable")]
    pub reset: String,
    #[serde(deserialize_with = "nullable")]
    pub reset_exact: String,
    pub reset_soon: bool,
    #[serde(deserialize_with = "nullable")]
    pub na_text: String,
    #[serde(deserialize_with = "nullable")]
    pub na_sub: String,
    #[serde(deserialize_with = "nullable")]
    pub amount: String,
    #[serde(deserialize_with = "nullable")]
    pub left: String,
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    /// The auto-switch point in % used, when the row has one.
    pub notch: Option<f64>,
    pub notch_faint: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AmountView {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    pub spent: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct FactView {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    pub mono: bool,
}

/// `detailsViewModel` in view-model.mjs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DetailsView {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub sub_lead: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub active: bool,
    pub switchable: bool,
    pub can_activate: bool,
    #[serde(deserialize_with = "nullable")]
    pub activate_kind: String,
    pub can_switch: bool,
    #[serde(deserialize_with = "nullable")]
    pub active_label: String,
    #[serde(deserialize_with = "nullable")]
    pub activate_hint: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    pub confirm: bool,
    #[serde(deserialize_with = "nullable")]
    pub confirm_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub profile: String,
    pub can_mac: bool,
    pub can_windows: bool,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub meters: Vec<MeterView>,
    #[serde(deserialize_with = "nullable")]
    pub amounts: Vec<AmountView>,
    #[serde(deserialize_with = "nullable")]
    pub facts: Vec<FactView>,
}

/// Details as the dashboard sent it; `None` while closed (or unreadable).
pub fn parse(json: &str) -> Option<DetailsView> {
    if json.is_empty() {
        return None;
    }
    serde_json::from_str::<DetailsView>(json)
        .ok()
        .filter(|d| !d.id.is_empty())
}

/// Severity of a used share, as the Slint theme draws it: warn from 80, crit from 95.
fn severity(view: &MeterView) -> &'static str {
    if !view.has_value {
        "na"
    } else if view.value >= 95.0 {
        "crit"
    } else if view.value >= 80.0 {
        "warn"
    } else {
        ""
    }
}

/// One window: its caps label, the value with "used" and the reset, the bar with the switch notch, then the exact
/// reset, the sample and where it was read.
#[component]
fn DetailMeter(m: MeterView) -> impl IntoView {
    let sev = severity(&m);
    let label = if m.full_label.is_empty() {
        m.label.clone()
    } else {
        m.full_label.clone()
    };
    let width = m.value.clamp(0.0, 100.0);
    let value_tone = match sev {
        "crit" => "w-crit",
        "warn" => "w-warn",
        _ => "",
    };
    view! {
        <div class="dm">
            <span class="cap">{label}</span>
            {if m.has_value {
                view! {
                    <p class="dm-top">
                        <span class=format!("dm-val tnum {value_tone}")>{m.value_text.clone()}<span class="dm-unit">"%"</span></span>
                        <span class="q3">" used"</span>
                        {(!m.over_text.is_empty()).then(|| view! { <span class="w-crit">{format!(" · {}% over", m.over_text)}</span> })}
                        <span class="dm-reset q3" class:soon=m.reset_soon>{m.reset.clone()}</span>
                    </p>
                }.into_any()
            } else {
                let na = if m.na_text.is_empty() { "Unavailable".to_string() } else { m.na_text.clone() };
                view! { <p class="dm-top"><span class:q3=m.na_sub.is_empty()>{na}</span></p> }.into_any()
            }}
            {if m.has_value {
                view! {
                    <div class="track">
                        <div class=format!("fill {sev}") style=format!("width:{width:.1}%")></div>
                        {m.notch.filter(|n| n.is_finite()).map(|n| view! {
                            <span class="notch" class:faint=m.notch_faint style=format!("left:{:.1}%", n.clamp(0.0, 100.0))></span>
                        })}
                    </div>
                }.into_any()
            } else {
                view! { <div class="track na"></div> }.into_any()
            }}
            {(!m.na_sub.is_empty() && m.reset_exact.is_empty()).then(|| view! { <p class="dm-line q3">{m.na_sub.clone()}</p> })}
            {(!m.reset_exact.is_empty()).then(|| view! { <p class="dm-line q3 tnum">{m.reset_exact.clone()}</p> })}
            {(!m.amount.is_empty() || !m.left.is_empty()).then(|| view! {
                <p class="dm-line q3 tnum">{[m.amount.clone(), m.left.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")}</p>
            })}
            {(!m.sampled.is_empty()).then(|| view! { <p class="dm-line q3">{m.sampled.clone()}</p> })}
            {(!m.source.is_empty()).then(|| view! { <p class="dm-line q4">{m.source.clone()}</p> })}
        </div>
    }
}

/// True when the press landed inside the panel itself (or on the toasts over it).
fn inside(target: Option<web_sys::EventTarget>) -> bool {
    target
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|el| el.closest(".details, .toasts, .ascrim").ok().flatten())
        .is_some()
}

fn asks() -> u64 {
    crate::dash::DETAILS_ASKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[component]
pub fn DetailsOverlay(ctx: FrameCtx) -> impl IntoView {
    let dash = ctx.dash;
    let data = Memo::new(move |_| dash.details.with(|json| parse(json)));
    let open = Memo::new(move |_| data.with(|d| d.is_some()));
    let confirming = RwSignal::new(false);
    // a different account, or a closed panel, drops the question
    Effect::new(move |_| {
        data.with(|d| d.as_ref().map(|d| d.id.clone()));
        confirming.set(false);
    });
    let close = move || {
        if dash
            .details
            .try_with_untracked(|d| d.is_empty())
            .unwrap_or(true)
        {
            return;
        }
        dash.details.set(String::new());
        dash.act("details-closed", "");
    };
    // Escape, and a click anywhere else (desktop: the scrim lets clicks through, as the Slint slide-over did)
    let on_close = Callback::new(move |_| {
        if dash.activation.with_untracked(|a| a.is_empty()) {
            close();
        }
    });
    // A press outside the panel closes it once the click is handled, unless that click asked for another account
    // (a row: the panel swaps) or a switch confirmation waits. Captured first, so a button that stops its click
    // closes it too, as the Slint canvas did. iOS sends no click for a tap on a plain area (the page under the side
    // panel, which lets taps through), so a press that ends without its click closes it a moment after the lift.
    let pressed: StoredValue<Option<(u64, u64)>> = StoredValue::new(None);
    let presses = StoredValue::new(0u64);
    let finish = move |before: u64| {
        if asks() == before && dash.activation.try_with_untracked(|a| a.is_empty()) == Some(true) {
            close();
        }
    };
    let down =
        wasm_bindgen::closure::Closure::<dyn Fn(web_sys::Event)>::new(move |e: web_sys::Event| {
            let Some(is_open) = open.try_get_untracked() else {
                return;
            };
            let calm = dash
                .activation
                .try_with_untracked(|a| a.is_empty())
                .unwrap_or(false);
            let n = presses.try_get_value().unwrap_or(0) + 1;
            let _ = presses.try_set_value(n);
            let _ = pressed
                .try_set_value((is_open && calm && !inside(e.target())).then(|| (n, asks())));
        });
    let up =
        wasm_bindgen::closure::Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            let Some(Some((_, before))) = pressed.try_get_value() else {
                return;
            };
            let _ = pressed.try_set_value(None);
            let later = wasm_bindgen::closure::Closure::once_into_js(move || finish(before));
            let _ = window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(later.unchecked_ref(), 0);
        });
    let lifted =
        wasm_bindgen::closure::Closure::<dyn Fn(web_sys::Event)>::new(move |_: web_sys::Event| {
            let Some(Some(press)) = pressed.try_get_value() else {
                return;
            };
            // the click, when one comes, takes the press first; a tap that becomes a scroll is cancelled, not lifted
            let later = wasm_bindgen::closure::Closure::once_into_js(move || {
                if pressed.try_get_value() == Some(Some(press)) {
                    let _ = pressed.try_set_value(None);
                    finish(press.1);
                }
            });
            let _ = window()
                .set_timeout_with_callback_and_timeout_and_arguments_0(later.unchecked_ref(), 450);
        });
    // Window listeners for the frame's life: after a sign-out their signals are gone and they do nothing.
    let _ = window().add_event_listener_with_callback_and_bool(
        "pointerdown",
        down.as_ref().unchecked_ref(),
        true,
    );
    let _ = window().add_event_listener_with_callback_and_bool(
        "click",
        up.as_ref().unchecked_ref(),
        true,
    );
    let _ = window().add_event_listener_with_callback_and_bool(
        "pointerup",
        lifted.as_ref().unchecked_ref(),
        true,
    );
    down.forget();
    up.forget();
    lifted.forget();
    let busy = dash.busy;
    // The phone sheet's grabber drags it down to close it, as the Status and Scoreboard sheets do.
    let drag = RwSignal::new(0.0);
    Effect::new(move |_| {
        if !open.get() {
            drag.set(0.0);
        }
    });
    let grab_close = Callback::new(move |_| {
        close();
        drag.set(0.0);
    });
    view! {
        <Show when=move || open.get()>
            <OverlayPortal on_close=on_close trap_focus=false>
            // A button, so iOS sends the tap's click; the window's listeners above close the panel.
            <button type="button" class="details-scrim" tabindex="-1" aria-label="Close"></button>
            <aside class="details" role="dialog" aria-modal="false" data-probe="details"
                style=move || sheet_drag_style(drag.get())
                aria-label=move || data.with(|d| d.as_ref().map(|d| d.title.clone()).unwrap_or_default())>
                {move || data.get().map(|d| {
                    let d2 = d.clone();
                    let d3 = d.clone();
                    let state = SwitchState {
                        active: d.active,
                        active_label: d.active_label.clone(),
                        platform: d.platform.clone(),
                        can_switch: d.can_switch,
                        can_activate: d.can_activate,
                        hint: d.activate_hint.clone(),
                    };
                    let slot = d.switchable && (d.can_switch || d.active);
                    let n_meters = d.meters.len();
                    let n_amounts = d.amounts.len();
                    view! {
                        <div class="dt-grab"><SheetGrab offset=drag on_close=grab_close/></div>
                        <header class="dt-head">
                            <ProviderMark provider=d.provider.clone() size=30/>
                            <div class="dt-titles">
                                <h2 class="dt-title">{d.title.clone()}</h2>
                                <p class="dt-sub q3">
                                    {d.sub_lead.clone()}
                                    {(!d.state.is_empty()).then(|| view! { " · "<span class:w-acc=d.active>{d.state.clone()}</span> })}
                                </p>
                            </div>
                            <button type="button" class="icon-btn bare" aria-label="Close details" data-probe="details-close"
                                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); close(); }><Icon name="close"/></button>
                        </header>
                        <div class="dt-body">
                            <div class="dt-acts">
                                {slot.then(|| view! {
                                    <SwitchSlot state=state busy=busy probe=format!("details-activate:{}", d.id)
                                        on_activate=Callback::new(move |_| {
                                            if d2.confirm { confirming.set(true); } else { dash.act(&d2.activate_kind, &d2.profile); }
                                        })/>
                                })}
                                {(d.provider == "claude").then(|| {
                                    let mac = format!("{}:mac", d.profile);
                                    let win = format!("{}:windows", d.profile);
                                    let (can_mac, can_win) = (d.can_mac, d.can_windows);
                                    view! {
                                        <button type="button" class="btn sm" disabled=move || !can_mac || busy.get()
                                            on:click=move |_| { dash.act("launch", &mac); }><PlatformGlyph id="mac".to_string()/>"Open on Mac"</button>
                                        <button type="button" class="btn sm" disabled=move || !can_win || busy.get()
                                            on:click=move |_| { dash.act("launch", &win); }><PlatformGlyph id="windows".to_string()/>"Open on Windows"</button>
                                    }
                                })}
                                <button type="button" class="btn sm ghost" data-probe="details-manage"
                                    on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); close(); ctx.go(Page::Accounts); }>"Manage in Accounts & Settings"</button>
                            </div>
                            <Show when=move || confirming.get()>
                                <ConfirmLine runs=d3.confirm_runs.clone() busy=busy
                                    on_keep=Callback::new(move |_| confirming.set(false))
                                    on_confirm=Callback::new({ let d = d3.clone(); move |_| { confirming.set(false); dash.act(&d.activate_kind, &d.profile); } })/>
                            </Show>
                            <section class="dt-sec">
                                <span class="cap dt-cap">{format!("Usage windows · {n_meters}")}</span>
                                {if n_meters == 0 {
                                    view! { <p class="q3">"No usage windows reported yet."</p> }.into_any()
                                } else {
                                    view! { <div class="dt-meters">{d.meters.iter().cloned().map(|m| view! { <DetailMeter m=m/> }).collect_view()}</div> }.into_any()
                                }}
                                {(!d.note.is_empty()).then(|| view! { <p class="icon-note q3"><Icon name="info"/><span>{d.note.clone()}</span></p> })}
                            </section>
                            {(n_amounts > 0).then(|| view! {
                                <section class="dt-sec">
                                    <span class="cap dt-cap">{format!("Balances, packs and credits · {n_amounts}")}</span>
                                    <div class="dt-amounts">
                                        {d.amounts.iter().cloned().map(|a| view! {
                                            <div class="da">
                                                <span class="da-label q3">{a.label.clone()}</span>
                                                <span class="da-value tnum" class:w-crit=a.spent>{a.value.clone()}{(!a.unit.is_empty()).then(|| view! { " "<span class="q3">{a.unit.clone()}</span> })}</span>
                                                {(!a.sub.is_empty()).then(|| view! { <span class="da-sub q3">{a.sub.clone()}</span> })}
                                            </div>
                                        }).collect_view()}
                                    </div>
                                </section>
                            })}
                            <section class="dt-sec">
                                <span class="cap dt-cap">"Reading"</span>
                                <dl class="facts ac-facts">
                                    {d.facts.iter().cloned().map(|f| view! { <dt>{f.label.clone()}</dt><dd class:mono=f.mono>{f.value.clone()}</dd> }).collect_view()}
                                </dl>
                            </section>
                        </div>
                    }
                })}
            </aside>
            </OverlayPortal>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_details_and_nulls_read_safely() {
        assert!(parse("").is_none());
        assert!(parse("{\"id\":\"\"}").is_none());
        let d = parse(r#"{"id":"codex:a","title":"a@example.test","meters":[{"key":"k","hasValue":true,"value":96.5,"valueText":"96.5%","notch":95,"reset":null}],"amounts":null,"facts":[{"label":"Status","value":"OK"}]}"#).unwrap();
        assert_eq!(d.meters[0].notch, Some(95.0));
        assert_eq!(severity(&d.meters[0]), "crit");
        assert!(d.amounts.is_empty());
        assert_eq!(d.facts[0].value, "OK");
    }
}
