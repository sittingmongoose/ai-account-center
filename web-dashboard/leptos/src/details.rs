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
use crate::resets::{ReadingRow, ResetCtx, ResetDialog, ResetHistory, ResetsGroup};
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

/// A fact group's words: the head's end, the answer (bold lead, then the rest) and label/value rows.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct FactGroup {
    #[serde(deserialize_with = "nullable")]
    pub head: String,
    #[serde(deserialize_with = "nullable")]
    pub lead: String,
    #[serde(deserialize_with = "nullable")]
    pub rest: String,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<FactView>,
}

/// One sign-in: `claudeSignInGroup` in claude-signins.mjs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct SignInRow {
    #[serde(deserialize_with = "nullable")]
    pub computer: String,
    #[serde(deserialize_with = "nullable")]
    pub profile: String,
    #[serde(deserialize_with = "nullable")]
    pub ends: String,
    #[serde(deserialize_with = "nullable")]
    pub renews: String,
    #[serde(deserialize_with = "nullable")]
    pub problem: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct SignInGroup {
    #[serde(deserialize_with = "nullable")]
    pub head: String,
    #[serde(deserialize_with = "nullable")]
    pub lead: String,
    #[serde(deserialize_with = "nullable")]
    pub rest: String,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<SignInRow>,
    #[serde(deserialize_with = "nullable")]
    pub browser_session: String,
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
    /// "claim" (Claude, Codex), "zai" (listed only), "qwen" (none on the plan) or "".
    #[serde(deserialize_with = "nullable")]
    pub resets_mode: String,
    /// The resets the usage reading lists (shown until, or unless, the reset inventory is read).
    #[serde(deserialize_with = "nullable")]
    pub reset_rows: Vec<ReadingRow>,
    /// Every other balance, pack and credit, as label/value pairs.
    #[serde(deserialize_with = "nullable")]
    pub balances: Vec<AmountView>,
    pub reading: Option<FactGroup>,
    pub sign_ins: Option<SignInGroup>,
}

impl DetailsView {
    /// Whether the panel reads the reset inventory: only where AAC can use a reset and the reading lists some.
    pub fn reads_resets(&self) -> bool {
        self.resets_mode == "claim" && !self.reset_rows.is_empty()
    }
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
    // The reset claims of the account on show: its inventory, the confirm dialog and the result line.
    let rc = ResetCtx::new();
    rc.watch_changes();
    Effect::new(move |_| {
        let (id, provider, reads) = data.with(|d| {
            d.as_ref()
                .map(|d| (d.id.clone(), d.provider.clone(), d.reads_resets()))
                .unwrap_or_default()
        });
        rc.show(&id, &provider, reads);
    });
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
        if dash.activation.with_untracked(|a| a.is_empty()) && !rc.dialog_open() {
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
        if asks() == before
            && dash.activation.try_with_untracked(|a| a.is_empty()) == Some(true)
            && !rc.dialog_open()
        {
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
                .unwrap_or(false)
                && !rc.dialog_open();
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
                            <section class="fg">
                                <div class="fg-cap"><h3 class="cap">"Usage windows"</h3><span class="end"><b>{n_meters}</b></span></div>
                                {if n_meters == 0 {
                                    view! { <p class="q3">"No usage windows reported yet."</p> }.into_any()
                                } else {
                                    view! { <div class="dt-meters">{d.meters.iter().cloned().map(|m| view! { <DetailMeter m=m/> }).collect_view()}</div> }.into_any()
                                }}
                                {(!d.note.is_empty()).then(|| view! { <p class="icon-note q3"><Icon name="info"/><span>{d.note.clone()}</span></p> })}
                            </section>
                            {(!d.resets_mode.is_empty() || !d.reset_rows.is_empty() || !d.balances.is_empty()).then(|| view! {
                                <ResetsGroup rc=rc mode=d.resets_mode.clone() reading_rows=d.reset_rows.clone() balances=d.balances.clone()/>
                            })}
                            {match d.reading.clone() {
                                Some(g) => view! { <ReadingGroup g=g/> }.into_any(),
                                // a dashboard that sends no groups yet: the old label/value list
                                None => view! {
                                    <section class="dt-sec">
                                        <span class="cap dt-cap">"Reading"</span>
                                        <dl class="facts ac-facts">
                                            {d.facts.iter().map(|f| view! { <dt>{f.label.clone()}</dt><dd class:mono=f.mono>{f.value.clone()}</dd> }).collect_view()}
                                        </dl>
                                    </section>
                                }.into_any(),
                            }}
                            {d.sign_ins.clone().map(|g| view! { <SignInsGroup g=g/> })}
                            <ResetHistory rc=rc/>
                        </div>
                    }
                })}
            </aside>
            </OverlayPortal>
            <ResetDialog rc=rc/>
        </Show>
    }
}

/// The "Reading" fact group: when it was sampled, the status as the answer, then Read from and Profile.
#[component]
fn ReadingGroup(g: FactGroup) -> impl IntoView {
    view! {
        <section class="fg" data-probe="details-reading">
            <div class="fg-cap"><h3 class="cap">"Reading"</h3><span class="end tnum">{g.head.clone()}</span></div>
            <p class="fg-sum"><b>{g.lead.clone()}</b>{(!g.rest.is_empty()).then(|| format!(" {}", g.rest))}</p>
            <dl class="fp top">
                {g.rows.into_iter().map(|f| view! { <dt>{f.label.clone()}</dt><dd class:mono=f.mono>{f.value.clone()}</dd> }).collect_view()}
            </dl>
        </section>
    }
}

/// Rows a fact group's table shows before "and N more" (the facts pattern).
const FOLD_AFTER: usize = 6;

/// The "Sign-ins" fact group: how many are good, the answer, then one row per computer and profile.
#[component]
fn SignInsGroup(g: SignInGroup) -> impl IntoView {
    let all = RwSignal::new(false);
    let n = g.rows.len();
    let rows = StoredValue::new(g.rows.clone());
    view! {
        <section class="fg" data-probe="details-signins">
            <div class="fg-cap"><h3 class="cap">"Sign-ins"</h3><span class="end">{g.head.clone()}</span></div>
            <p class="fg-sum"><b>{g.lead.clone()}</b>{(!g.rest.is_empty()).then(|| format!(" {}", g.rest))}</p>
            <table class="ft">
                <thead><tr><th class="cap">"Computer"</th><th class="cap">"Profile"</th><th class="cap">"Ends"</th><th class="cap r">"Renews"</th></tr></thead>
                <tbody>
                    {move || {
                        let take = if all.get() { usize::MAX } else { FOLD_AFTER };
                        rows.get_value().into_iter().take(take).map(|r| {
                            let bad = !r.problem.is_empty();
                            view! {
                                <tr class:bad=bad>
                                    <td class="w">{r.computer.clone()}{bad.then(|| view! { <small class="w-crit">{r.problem.clone()}</small> })}</td>
                                    <td class="mono">{r.profile.clone()}</td>
                                    <td class="nw">{r.ends.clone()}</td>
                                    <td class="r nw">{r.renews.clone()}</td>
                                </tr>
                            }
                        }).collect_view()
                    }}
                </tbody>
            </table>
            {(n > FOLD_AFTER).then(|| view! {
                <p class="fg-line">
                    {move || (!all.get()).then(|| format!("and {} more", n - FOLD_AFTER))}
                    <button type="button" class="btn sm ghost" on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); all.update(|v| *v = !*v); }>
                        {move || if all.get() { "Show fewer" } else { "Show" }}
                    </button>
                </p>
            })}
            {(!g.browser_session.is_empty()).then(|| view! {
                <dl class="fp"><dt>"Browser session"</dt><dd>{g.browser_session.clone()}</dd></dl>
            })}
        </section>
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
        assert!(d.reading.is_none() && d.sign_ins.is_none() && !d.reads_resets());
    }

    #[test]
    fn the_fact_groups_read_from_the_view_model() {
        let d = parse(r#"{"id":"claude:p","provider":"claude","resetsMode":"claim",
            "resetRows":[{"key":"k","refills":"Weekly","state":"ready","left":1,"count":1,"expiresAtMs":5}],
            "balances":[{"key":"b","label":"Extra usage","value":"Off","unit":"","sub":"","spent":false}],
            "reading":{"head":"9:47 AM, 1m ago","lead":"Cached.","rest":"This is the last successful reading.","rows":[{"label":"Read from","value":"Verified quota, on Windows","mono":false}]},
            "signIns":{"head":"1 of 2 good","lead":"1 of 2 needs a look.","rest":"","rows":[{"computer":"Mac","profile":"party","ends":"Nov 30","renews":"","problem":"Logged out"}],"browserSession":null}}"#).unwrap();
        assert!(d.reads_resets());
        assert_eq!(d.reset_rows[0].expires_at_ms, Some(5.0));
        assert_eq!(d.balances[0].label, "Extra usage");
        assert_eq!(d.reading.unwrap().rows[0].label, "Read from");
        let s = d.sign_ins.unwrap();
        assert_eq!(s.rows[0].problem, "Logged out");
        assert!(s.browser_session.is_empty());
        // a Z.ai or Qwen account never reads the claim inventory
        let z = parse(r#"{"id":"zai:a","provider":"zai","resetsMode":"zai","resetRows":[{"key":"z","state":"ready","left":2}]}"#).unwrap();
        assert!(!z.reads_resets());
    }
}
