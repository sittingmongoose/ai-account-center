//! Accounts & Settings (PLAN step 6), drawn from the dashboard's Accounts & Settings view v3 (`public/accounts-view.mjs`)
//! through the seam (dash.rs). Every word, every enabled, refused or "coming" control and every server call stays
//! where it is: the view says what to draw, and each control sends the same action kind and value the Slint page sent
//! (`accounts-controller.mjs` and the dashboard controller run them, with the pending-switch hold, the one-use remove
//! and restore tokens and the sign-in jobs that survive a reload).
//!
//! Layout: every card (Update apps first, then one per provider, then the settings groups) goes into as many columns
//! as the width allows, filled in order and balanced by each card's expected height (`layout::columns`). The expected
//! heights leave out what changes while you work in a card (an open flow, a confirmation line, the password form), so
//! a card never jumps to another column while you type in it.

mod actions;
mod flow;
pub(crate) mod layout;
pub mod model;
mod provider;
mod settings;
mod update;

use crate::frame::FrameCtx;
use layout::CardKey;
use leptos::prelude::*;
use model::AccountsView;
use wasm_bindgen::JsCast;

/// Sends one of the dashboard's action kinds (dash.rs; nothing is sent while the dashboard is not ready).
pub(crate) type Act = Callback<(String, String)>;

/// The page's state every card reads: the view, the global busy flag, the open Details row and the action sender.
#[derive(Clone, Copy)]
pub(crate) struct Page {
    pub view: Memo<Option<AccountsView>>,
    pub busy: Signal<bool>,
    /// The account whose Details is open (its row is selected).
    pub selected: Memo<String>,
    pub act: Act,
    pub ctx: FrameCtx,
}

impl Page {
    pub fn send(&self, kind: &str, value: &str) {
        self.act.run((kind.to_string(), value.to_string()));
    }
}

/// The width the cards share: the page's content box, measured on resize (0 while the tab is hidden).
fn measure(el: &web_sys::HtmlElement) -> f64 {
    let style = window().get_computed_style(el).ok().flatten();
    let pad = |name: &str| {
        style
            .as_ref()
            .and_then(|s| s.get_property_value(name).ok())
            .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    el.client_width() as f64 - pad("padding-left") - pad("padding-right")
}

#[component]
pub fn AccountsPage(ctx: FrameCtx) -> impl IntoView {
    let dash = ctx.dash;
    let parsed = Memo::new(move |_| dash.accounts.with(|json| model::parse(json)));
    let view = Memo::new(move |_| parsed.with(|p| p.clone().ok().flatten()));
    let problem = Memo::new(move |_| parsed.with(|p| p.clone().err()));
    let selected = Memo::new(move |_| {
        dash.details.with(|json| {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(String::from))
                .unwrap_or_default()
        })
    });
    let act: Act = Callback::new(move |(kind, value): (String, String)| {
        dash.act(&kind, &value);
    });
    let page = Page {
        view,
        busy: dash.busy.into(),
        selected,
        act,
        ctx,
    };
    provide_context(page);

    // The columns follow the page's own width.
    let root = NodeRef::<leptos::html::Div>::new();
    let width = RwSignal::new(0.0_f64);
    // Also run from a frame callback: one queued just before a quick sign-out finds the page gone (its NodeRef and
    // signals disposed) and does nothing (Sol max L1).
    let remeasure = move || {
        let Some(Some(el)) = root.try_get_untracked() else {
            return;
        };
        let w = measure(el.unchecked_ref());
        if w > 0.0
            && width
                .try_get_untracked()
                .is_some_and(|old| (w - old).abs() >= 1.0)
        {
            let _ = width.try_set(w);
        }
    };
    let on_resize = window_event_listener(leptos::ev::resize, move |_| remeasure());
    on_cleanup(move || on_resize.remove());
    // Shown again after another tab (its width was 0 while hidden), and the first layout.
    Effect::new(move |_| {
        if ctx.route.get() == crate::Page::Accounts {
            let again = wasm_bindgen::closure::Closure::once_into_js(remeasure);
            let _ = window().request_animation_frame(again.unchecked_ref());
        }
    });
    Effect::new(move |_| {
        if root.get().is_some() {
            remeasure();
        }
    });

    // Each card's lasting height, measured after it is drawn (see `lasting_heights`).
    let measured = RwSignal::new(std::collections::HashMap::<String, f64>::new());
    let plan = Memo::new(move |prev: Option<&Option<Vec<Vec<CardKey>>>>| {
        let w = width.get();
        let cards = view.with(|v| v.as_ref().map(|v| layout::cards(v, w)))?;
        let cards = measured.with(|m| layout::with_measured(cards, m));
        Some(layout::choose(prev.and_then(|p| p.as_ref()), &cards, w))
    });
    // After each draw (a new view, a new cut, a new width), measure the cards again on the next frame.
    Effect::new(move |_| {
        plan.track();
        view.track();
        let again = wasm_bindgen::closure::Closure::once_into_js(move || {
            // the page may be gone by the next frame (a quick sign-out): then there is nothing to measure
            let Some(Some(el)) = root.try_get_untracked() else {
                return;
            };
            let found = lasting_heights(el.unchecked_ref());
            if found.is_empty() {
                return;
            }
            let changed = measured.try_with_untracked(|m| {
                found
                    .iter()
                    .any(|(k, h)| m.get(k).is_none_or(|old| (old - h).abs() > 4.0))
            });
            if changed == Some(true) {
                let _ = measured.try_set(found);
            }
        });
        let _ = window().request_animation_frame(again.unchecked_ref());
    });

    view! {
        <div class="page ac-page" node_ref=root>
            {move || problem.get().map(|msg| view! { <p class="ac-note crit">{msg}</p> })}
            {move || {
                let waiting = view.with(|v| v.is_none()) && problem.with(|p| p.is_none());
                waiting.then(|| view! { <p class="ac-note q3">"Reading accounts and settings..."</p> })
            }}
            {move || view.with(|v| v.as_ref().map(|v| v.access_message.clone()).filter(|m| !m.is_empty()))
                .map(|msg| view! { <p class="ac-note">{msg}</p> })}
            <Show when={move || layout::column_count(width.get()) >= 2 && view.with(|v| v.is_some())}>
                <div class="ac-top">{card(page, CardKey::Update)}</div>
            </Show>
            <div class="ac-cols" style=move || format!("--ac-cols:{}", plan.with(|p| p.as_ref().map(|c| c.len()).unwrap_or(1)))>
                <For each={move || (0..plan.with(|p| p.as_ref().map(|c| c.len()).unwrap_or(0))).collect::<Vec<usize>>()}
                    key=|i| *i
                    children=move |i| {
                        // each column keeps its cards; a card that moves to another column is drawn there anew
                        let keys = Memo::new(move |_| plan.with(|p| p.as_ref().and_then(|c| c.get(i).cloned()).unwrap_or_default()));
                        view! {
                            <div class="ac-col">
                                <For each=move || keys.get() key=|k| k.clone() children=move |k| card(page, k)/>
                            </div>
                        }
                    }/>
            </div>
        </div>
    }
}

/// Each card's height without what opens while it is used (a flow, a line under a row, the password form, an app's
/// details), by its slot's `data-card`.
fn lasting_heights(root: &web_sys::Element) -> std::collections::HashMap<String, f64> {
    let all = |el: &wasm_bindgen::JsValue, selector: &str| -> Vec<web_sys::Element> {
        js_sys::Reflect::get(el, &"querySelectorAll".into())
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
            .and_then(|f| f.call1(el, &selector.into()).ok())
            .map(|list| {
                js_sys::Array::from(&list)
                    .iter()
                    .filter_map(|n| n.dyn_into::<web_sys::Element>().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut out = std::collections::HashMap::new();
    for slot in all(root.as_ref(), ".ac-slot[data-card]") {
        let Some(name) = slot.get_attribute("data-card") else {
            continue;
        };
        let mut h = slot.get_bounding_client_rect().height();
        for part in all(
            slot.as_ref(),
            ".flow, .row-line, .confirm-line, .pw-form, .ut-details",
        ) {
            h -= part.get_bounding_client_rect().height() + 8.0;
        }
        if h > 0.0 {
            out.insert(name, h.max(40.0));
        }
    }
    out
}

fn card(page: Page, key: CardKey) -> AnyView {
    let name = key.name();
    view! { <div class="ac-slot" data-card=name>{card_view(page, key)}</div> }.into_any()
}

fn card_view(page: Page, key: CardKey) -> AnyView {
    match key {
        CardKey::Update => view! { <update::UpdateCard page=page/> }.into_any(),
        CardKey::Provider(id) => view! { <provider::ProviderCard page=page id=id/> }.into_any(),
        CardKey::Signin => view! { <settings::SigninCard page=page/> }.into_any(),
        CardKey::Settings => view! { <settings::SettingsCard page=page/> }.into_any(),
        CardKey::Cleanup => view! { <settings::CleanupCard page=page/> }.into_any(),
        CardKey::Logs => view! { <settings::LogSourcesCard page=page/> }.into_any(),
        CardKey::About => view! { <settings::AboutCard page=page/> }.into_any(),
    }
}

/// A card's head: its title and an optional meta line in ink-3.
#[component]
pub(crate) fn CardHead(
    title: &'static str,
    #[prop(optional, into)] meta: Option<Signal<String>>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="plate-head ac-head">
            <h2>{title}</h2>
            {meta.map(|m| view! { <span class="plate-meta">{move || m.get()}</span> })}
            {children.map(|c| view! { <div class="plate-tools">{c()}</div> })}
        </div>
    }
}
