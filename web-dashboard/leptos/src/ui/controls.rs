//! Small controls from the Slint component library: the toggle, the waiting spinner, a check line, the "coming" word,
//! the switch slot (Activate, or the plain Active mark) and the inline "Activate anyway?" line. Shared by Accounts &
//! Settings, Details and Home.

use super::icons::Icon;
use super::parts::runs_view;
use crate::api::Run;
use leptos::prelude::*;

/// A switch. It flips at once when pressed (as Slint's Toggle does) and follows the saved value again whenever that
/// value or `enabled` changes, so a refused save draws the saved state when it ends.
#[component]
pub fn Toggle(
    #[prop(into)] on: Signal<bool>,
    #[prop(into)] enabled: Signal<bool>,
    #[prop(optional, into)] label: Option<String>,
    /// The accessible name when there is no visible label.
    #[prop(optional, into)]
    aria: Option<String>,
    #[prop(optional, into)] tip: Option<Signal<String>>,
    #[prop(optional, into)] probe: Option<String>,
    #[prop(optional)] small: bool,
    on_toggle: Callback<bool>,
    /// A tap on the toggle while it is off and inert: the caller shows the reason.
    #[prop(optional)]
    on_reason: Option<Callback<()>>,
) -> impl IntoView {
    let shown = RwSignal::new(on.get_untracked());
    Effect::new(move |_| {
        let value = on.get();
        enabled.track();
        shown.set(value);
    });
    let has_label = label.is_some();
    let aria = aria.or_else(|| label.clone()).unwrap_or_default();
    view! {
        <button type="button" class="tg" class:sm=small class:labelled=has_label role="switch"
            aria-checked=move || if shown.get() { "true" } else { "false" }
            aria-disabled=move || if enabled.get() { "false" } else { "true" }
            aria-label=(!has_label).then(|| aria.clone())
            title=move || tip.map(|t| t.get()).filter(|t| !t.is_empty())
            data-probe=probe
            on:click=move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                if !enabled.get_untracked() {
                    if let Some(reason) = on_reason { reason.run(()); }
                    return;
                }
                let next = !shown.get_untracked();
                shown.set(next);
                on_toggle.run(next);
            }>
            {label.map(|l| view! { <span class="tg-label">{l}</span> })}
            <span class="tg-track" class:on=move || shown.get()><span class="tg-thumb"></span></span>
        </button>
    }
}

/// The waiting spinner: a ring and a turning arc (it turns only while it is on screen).
#[component]
pub fn Spinner() -> impl IntoView {
    view! {
        <svg class="spin" viewBox="0 0 24 24" aria-hidden="true">
            <path class="spin-ring" d="M12 3 A9 9 0 1 1 11.99 3"/>
            <path class="spin-arc" d="M21 12 A9 9 0 0 0 12 3"/>
        </svg>
    }
}

/// The caption beside a control the server has no flow or route for yet.
#[component]
pub fn ComingWord() -> impl IntoView {
    view! { <span class="coming">"coming"</span> }
}

/// A check box with its label and an optional note under it (the concept's `.aa-check`).
#[component]
pub fn CheckLine(
    checked: RwSignal<bool>,
    #[prop(into)] enabled: Signal<bool>,
    text: String,
    #[prop(optional, into)] sub: Option<Signal<String>>,
    #[prop(optional, into)] probe: Option<String>,
) -> impl IntoView {
    view! {
        <label class="check" class:off=move || !enabled.get() data-probe=probe>
            <input type="checkbox" prop:checked=move || checked.get() disabled=move || !enabled.get()
                on:change=move |e| checked.set(event_target_checked(&e))/>
            <span class="check-text">{text}{sub.map(|s| view! { <span class="check-sub">{move || s.get()}</span> })}</span>
        </label>
    }
}

/// What a switch slot shows for one account (the Home row's action slot).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SwitchState {
    pub active: bool,
    /// "on Ubuntu"; empty falls back to "on <platform>".
    pub active_label: String,
    pub platform: String,
    pub can_switch: bool,
    pub can_activate: bool,
    pub hint: String,
}

/// Activate (accent outline) or the plain Active mark with where it is active. A tap on an Activate that can't run
/// gives its reason; a running switch (`busy`) holds every Activate.
#[component]
pub fn SwitchSlot(
    #[prop(into)] state: Signal<SwitchState>,
    #[prop(into)] busy: Signal<bool>,
    #[prop(optional, into)] probe: Option<String>,
    on_activate: Callback<()>,
    #[prop(optional)] on_reason: Option<Callback<String>>,
) -> impl IntoView {
    view! {
        <span class="sw-slot">
            {move || {
                let s = state.get();
                if s.active {
                    let at = if s.active_label.is_empty() { format!("on {}", s.platform) } else { s.active_label.clone() };
                    return view! {
                        <span class="active-mark"><Icon name="check-circle"/><span class="am-text"><b>"Active"</b><span class="am-where">{at}</span></span></span>
                    }.into_any();
                }
                if !s.can_switch {
                    return ().into_any();
                }
                let can = s.can_activate;
                let hint = s.hint.clone();
                let tip = hint.clone();
                view! {
                    <button type="button" class="btn sm accent-line" data-probe=probe.clone()
                        aria-disabled=move || if can && !busy.get() { "false" } else { "true" }
                        title=(!tip.is_empty()).then_some(tip)
                        on:click=move |e: web_sys::MouseEvent| {
                            e.stop_propagation();
                            if busy.get_untracked() { return; }
                            if can { on_activate.run(()); }
                            else if let Some(reason) = on_reason { if !hint.is_empty() { reason.run(hint.clone()); } }
                        }>
                        <Icon name="swap"/>"Activate"
                    </button>
                }.into_any()
            }}
        </span>
    }
}

/// The inline question before activating an account already at or past its switch point (view model `confirmRuns`):
/// "Keep current" closes it, "Activate anyway" runs the same guarded activation.
#[component]
pub fn ConfirmLine(
    runs: Vec<Run>,
    #[prop(into)] busy: Signal<bool>,
    on_keep: Callback<()>,
    on_confirm: Callback<()>,
) -> impl IntoView {
    view! {
        <div class="confirm-line" role="group" on:click=|e: web_sys::MouseEvent| e.stop_propagation()>
            <Icon name="alert"/>
            <p class="cl-text">{runs_view(runs)}</p>
            <span class="cl-acts">
                <button type="button" class="btn sm ghost" disabled=move || busy.get() on:click=move |_| on_keep.run(())>"Keep current"</button>
                <button type="button" class="btn sm accent-line" disabled=move || busy.get() on:click=move |_| on_confirm.run(())>"Activate anyway"</button>
            </span>
        </div>
    }
}
