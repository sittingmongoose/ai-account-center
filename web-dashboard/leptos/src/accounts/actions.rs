//! One action slot (Slint `ActionControl`) and the line under a row (`RowLine`).
//!
//! A slot is live, off with its reason, "refused" (dimmed but clickable: the click opens the reason under the row
//! through the `refuse` action) or "coming". A running switch or save (`busy`) holds every live control except the one
//! that is itself running. An off control's reason shows on hover (`title`) and, on a tap, under its row.

use super::Page;
use super::model::{Action, Line};
use crate::ui::*;
use leptos::prelude::*;

fn style_class(style: &str) -> &'static str {
    match style {
        "primary" => "primary",
        "ghost" => "ghost",
        "accent-line" => "accent-line",
        "danger-solid" => "danger-solid",
        "danger-text" => "danger-text",
        "ink" => "ink",
        _ => "",
    }
}

/// Whether the control can run now: a button stays live while its own request runs (it shows the spinner), an icon
/// does not.
pub fn live(a: &Action, busy: bool) -> bool {
    match a.kind.as_str() {
        "icon" => a.enabled && !busy,
        _ => a.enabled && (!busy || a.busy),
    }
}

/// The platform glyph a button names ("apple", "windows").
fn glyph(platform: &str) -> Option<AnyView> {
    let id = match platform {
        "apple" | "mac" => "mac",
        "windows" => "windows",
        "ubuntu" => "ubuntu",
        _ => return None,
    };
    Some(view! { <PlatformGlyph id=id.to_string()/> }.into_any())
}

/// One slot. `reason` gets the tip of an off control that was tapped (rows show it under themselves).
#[component]
pub fn ActionControl(
    a: Action,
    page: Page,
    #[prop(optional)] reason: Option<Callback<String>>,
    #[prop(optional)] full: bool,
) -> impl IntoView {
    let busy = page.busy;
    match a.kind.as_str() {
        "quiet" => view! { <span class="quiet">{a.label}</span> }.into_any(),
        "button" => {
            let a2 = a.clone();
            let click = move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                if live(&a2, busy.get_untracked()) {
                    page.send(&a2.act, &a2.value);
                } else if !a2.enabled && !a2.tip.is_empty() && a2.act != "refuse" {
                    if let Some(r) = reason {
                        r.run(a2.tip.clone());
                    }
                }
            };
            let a3 = a.clone();
            let class = format!(
                "btn sm {}{}{}{}",
                style_class(&a.style),
                if a.refused { " refused" } else { "" },
                if a.coming { " coming-btn" } else { "" },
                if full { " full" } else { "" }
            );
            let icon = a.icon.clone();
            view! {
                <button type="button" class=class data-probe=(!a.probe.is_empty()).then(|| a.probe.clone())
                    aria-disabled=move || if live(&a3, busy.get()) { "false" } else { "true" }
                    aria-busy=a.busy.then_some("true")
                    title=(!a.tip.is_empty()).then(|| a.tip.clone())
                    on:click=click>
                    {if a.busy { Some(view! { <Spinner/> }.into_any()) } else { glyph(&a.platform).or_else(|| (!icon.is_empty()).then(|| view! { <Icon name=icon_name(&icon)/> }.into_any())) }}
                    <span>{a.label.clone()}</span>
                </button>
            }
            .into_any()
        }
        "icon" => {
            let a2 = a.clone();
            let a3 = a.clone();
            let click = move |e: web_sys::MouseEvent| {
                e.stop_propagation();
                if live(&a2, busy.get_untracked()) {
                    page.send(&a2.act, &a2.value);
                } else if !a2.enabled && !a2.tip.is_empty() && a2.act != "refuse" {
                    if let Some(r) = reason {
                        r.run(a2.tip.clone());
                    }
                }
            };
            let tip = if a.tip.is_empty() {
                a.label.clone()
            } else {
                a.tip.clone()
            };
            view! {
                <button type="button" class="icon-btn bare ac-icon" class:refused=a.refused
                    data-probe=(!a.probe.is_empty()).then(|| a.probe.clone())
                    aria-label=a.label.clone() title=tip
                    aria-disabled=move || if live(&a3, busy.get()) { "false" } else { "true" }
                    on:click=click>
                    <Icon name=icon_name(&a.icon)/>
                </button>
            }
            .into_any()
        }
        _ => ().into_any(),
    }
}

/// The view's icon names to the icons this build carries.
pub fn icon_name(name: &str) -> &'static str {
    match name {
        "trash" => "trash",
        "login" => "login",
        "logout" => "logout",
        "key" => "key",
        "plus" => "plus",
        "refresh" => "refresh",
        "globe" => "globe",
        "rotate" => "rotate",
        "external" => "external",
        "copy" => "copy",
        "check" => "check",
        "alert" => "alert",
        "info" => "info",
        "laptop" => "laptop",
        "code" => "code",
        "terminal" => "terminal",
        "app-window" => "app-window",
        "link" => "link",
        "lock" => "lock",
        "lock-open" => "lock-open",
        "share" => "share",
        "swap" => "swap",
        "download" => "download",
        _ => "info",
    }
}

/// The line under a row: a remove or restore confirmation with the server's effect sentences, a refusal with its
/// reason, or a request in flight.
#[component]
pub fn RowLine(#[prop(into)] line: Signal<Line>, page: Page) -> impl IntoView {
    view! {
        <Show when=move || line.with(|l| l.shown)>
            {move || {
                let l = line.get();
                let icon = icon_name(if l.icon.is_empty() { "info" } else { &l.icon });
                let tone = match l.kind.as_str() { "refuse" => "refuse", "busy" => "busy", _ => "confirm" };
                view! {
                    <div class=format!("row-line {tone}") role="group" on:click=|e: web_sys::MouseEvent| e.stop_propagation()>
                        {if l.kind == "busy" { view! { <Spinner/> }.into_any() } else { view! { <Icon name=icon/> }.into_any() }}
                        <p class="rl-text">{(!l.lead.is_empty()).then(|| view! { <b>{l.lead.clone()}</b>" " })}{l.text.clone()}</p>
                        {(!l.actions.is_empty()).then(|| view! {
                            <span class="rl-acts">{l.actions.into_iter().map(|a| view! { <ActionControl a=a page=page/> }).collect_view()}</span>
                        })}
                    </div>
                }
            }}
        </Show>
    }
}

/// The reason a tap on an off control opened under its row; the next tap anywhere in the row clears it.
#[component]
pub fn ReasonLine(reason: RwSignal<String>) -> impl IntoView {
    view! {
        <Show when=move || reason.with(|r| !r.is_empty())>
            <div class="row-line refuse reason" role="note">
                <Icon name="info"/>
                <p class="rl-text">{move || reason.get()}</p>
            </div>
        </Show>
    }
}
