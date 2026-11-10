//! Home (PLAN step 5): the Claude section, the switchable Codex and Antigravity sections, and the other providers as
//! cards, drawn from the dashboard's view model v2 (`public/view-model.mjs` `dashboardViewModel`), which bridge.js
//! builds and sends through the seam (`dash.rs`). Every number, word and "unavailable" decision is in that view; this
//! page lays it out and animates it. Intent goes back as the dashboard's own action kinds, so switch approvals,
//! idle-only auto-switch, held actions during a pending switch and every server call stay in bridge.js.
//!
//! Layout follows the space the page has (container queries, `style/app.css` "Home"): each section is a table (the
//! account, one column per window, the action slot) once it is wide enough for its columns, and a stacked row (the
//! account line, then the meters side by side) below that; two sections share a row from 760 px below desktop and
//! from 1600 px on desktop; provider cards tile in equal-width rows with no gap at the end of a row.

use crate::Page;
use crate::api::{Run, nullable};
use crate::dash::DashCtx;
use crate::frame::FrameCtx;
use crate::ui::*;
use leptos::prelude::*;
use serde::Deserialize;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// The view model version this page draws (view-model.mjs `VIEW_MODEL_VERSION`). Another version is refused, not
/// half drawn.
pub const VIEW_MODEL_VERSION: u32 = 2;

// ---------------------------------------------------------------- view model v2 (README-ARCHITECTURE.md)

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DashboardView {
    pub version: u32,
    #[serde(deserialize_with = "nullable")]
    pub sections: Vec<SectionView>,
    #[serde(deserialize_with = "nullable")]
    pub cards: Vec<CardView>,
    pub chrome: ChromeWords,
}

/// The header's words; Home only uses them when there is nothing else to show (sign-in turned off).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChromeWords {
    #[serde(deserialize_with = "nullable")]
    pub status_lead: String,
    #[serde(deserialize_with = "nullable")]
    pub status_more: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SectionView {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub switchable: bool,
    pub can_switch: bool,
    #[serde(deserialize_with = "nullable")]
    pub meta_runs: Vec<Run>,
    pub foot: FootView,
    #[serde(deserialize_with = "nullable")]
    pub active_id: String,
    #[serde(deserialize_with = "nullable")]
    pub empty: String,
    pub auto: AutoView,
    #[serde(deserialize_with = "nullable")]
    pub columns: Vec<ColumnView>,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<RowView>,
}

/// What auto-switch does about the active account (the Codex footer).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FootView {
    pub shown: bool,
    pub warn: bool,
    #[serde(deserialize_with = "nullable")]
    pub runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub when: String,
}

/// A switchable section's auto-switch state, in "% used".
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutoView {
    pub known: bool,
    pub shown: bool,
    pub enabled: bool,
    pub available: bool,
    pub can_enable: bool,
    pub threshold_used: Option<i64>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    #[serde(deserialize_with = "nullable")]
    pub pool: String,
    #[serde(deserialize_with = "nullable")]
    pub off_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub setting: String,
    #[serde(deserialize_with = "nullable")]
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ColumnView {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub short_label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RowView {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub profile: String,
    #[serde(deserialize_with = "nullable")]
    pub email: String,
    #[serde(deserialize_with = "nullable")]
    pub meta: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    pub active: bool,
    #[serde(deserialize_with = "nullable")]
    pub active_label: String,
    pub setup: bool,
    pub can_activate: bool,
    #[serde(deserialize_with = "nullable")]
    pub activate_kind: String,
    #[serde(deserialize_with = "nullable")]
    pub activate_hint: String,
    pub can_mac: bool,
    pub can_windows: bool,
    #[serde(deserialize_with = "nullable")]
    pub amounts_runs: Vec<Run>,
    pub confirm: bool,
    #[serde(deserialize_with = "nullable")]
    pub confirm_runs: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub cells: Vec<MeterView>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CardView {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub account_id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub identity: String,
    #[serde(deserialize_with = "nullable")]
    pub plan: String,
    #[serde(deserialize_with = "nullable")]
    pub flag: String,
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    #[serde(deserialize_with = "nullable")]
    pub plan_note: String,
    #[serde(deserialize_with = "nullable")]
    pub packs_note: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub meters: Vec<MeterView>,
    #[serde(deserialize_with = "nullable")]
    pub amounts: Vec<AmountView>,
}

/// Packs, balances, credits and spend: amounts, never fake meters.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
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
    #[serde(deserialize_with = "nullable")]
    pub icon: String,
    pub spent: bool,
}

/// What the page can draw from the seam's `dashboard` JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum HomeState {
    /// Nothing pushed yet.
    Waiting,
    /// A view of another version, or one that can't be read: refused with a sentence, never half drawn.
    Refused(String),
    Ready(DashboardView),
}

pub fn parse(json: &str) -> HomeState {
    if json.trim().is_empty() {
        return HomeState::Waiting;
    }
    match serde_json::from_str::<DashboardView>(json) {
        Ok(view) if view.version == VIEW_MODEL_VERSION => HomeState::Ready(view),
        Ok(view) => HomeState::Refused(format!(
            "This app draws Home's view version {VIEW_MODEL_VERSION}, and the dashboard sent version {}. Reload the app.",
            view.version
        )),
        Err(_) => HomeState::Refused("Home's view could not be read. Reload the app.".into()),
    }
}

// ---------------------------------------------------------------- the cards' rows

/// The most cards a row takes at each column count the CSS offers (`--L1` .. `--L7`).
pub const CARD_COLUMNS: usize = 7;

/// Spreads `n` cards over the fewest rows of at most `cols`, the longer rows first, every row the full width
/// (Slint `HomePage`): returns the grid's column count (the rows' least common multiple) and each card's span.
pub fn card_rows(n: usize, cols: usize) -> (usize, Vec<usize>) {
    if n == 0 {
        return (1, Vec::new());
    }
    let cols = cols.clamp(1, n);
    let rows = n.div_ceil(cols);
    let base = n / rows;
    let extra = n % rows;
    let gcd = |mut a: usize, mut b: usize| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let lcm = if extra > 0 {
        base * (base + 1) / gcd(base, base + 1)
    } else {
        base
    };
    let mut spans = Vec::with_capacity(n);
    for row in 0..rows {
        let len = if row < extra { base + 1 } else { base };
        spans.extend(std::iter::repeat_n(lcm / len, len));
    }
    (lcm, spans)
}

// ---------------------------------------------------------------- small helpers

/// An email or identity with break chances after `@` and each dot, so a long one wraps at a sensible place instead
/// of being cut.
fn breakable(text: &str) -> impl IntoView + use<> {
    let mut parts: Vec<AnyView> = Vec::new();
    let mut word = String::new();
    for ch in text.chars() {
        word.push(ch);
        if ch == '@' || ch == '.' {
            parts.push(std::mem::take(&mut word).into_any());
            parts.push(view! { <wbr/> }.into_any());
        }
    }
    if !word.is_empty() {
        parts.push(word.into_any());
    }
    parts
}

/// Runs a closure on the next frame, outside any Leptos owner (NOTES.md: owned timers die with their branch).
fn next_frame(f: impl FnOnce() + 'static) {
    let cb = Closure::once_into_js(f);
    let _ = window().request_animation_frame(cb.unchecked_ref());
}

fn after(ms: i32, f: impl FnOnce() + 'static) -> i32 {
    let cb = Closure::once_into_js(f);
    window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms)
        .unwrap_or(0)
}

fn on_key_activate(ev: &web_sys::KeyboardEvent) -> bool {
    matches!(ev.key().as_str(), "Enter" | " ")
}

// ---------------------------------------------------------------- the page

#[component]
pub fn HomePage() -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let state = Memo::new(move |_| dash.dashboard.with(|json| parse(json)));
    let vm = Memo::new(move |_| match state.get() {
        HomeState::Ready(view) => Some(view),
        _ => None,
    });
    let section_ids = Memo::new(move |_| {
        vm.with(|v| {
            v.as_ref()
                .map(|v| v.sections.iter().map(|s| s.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
    });
    let has_cards =
        Memo::new(move |_| vm.with(|v| v.as_ref().is_some_and(|v| !v.cards.is_empty())));
    let nothing = Memo::new(move |_| {
        vm.with(|v| {
            v.as_ref()
                .filter(|v| v.sections.is_empty() && v.cards.is_empty())
                .map(|v| v.chrome.clone())
        })
    });
    let motion = motion_on();
    view! {
        <div class="page home" class:motion=motion>
            {move || match state.get() {
                HomeState::Waiting => Some(view! { <p class="hstate">"Reading your accounts…"</p> }.into_any()),
                HomeState::Refused(why) => Some(view! { <p class="hstate w-crit">{why}</p> }.into_any()),
                HomeState::Ready(_) => None,
            }}
            {move || nothing.get().map(|chrome| {
                let lead = if chrome.status_lead.is_empty() || chrome.status_lead == "Updated" {
                    "No accounts are reported yet.".to_string()
                } else {
                    chrome.status_lead
                };
                view! {
                    <p class="hstate"><b>{lead}</b>{(!chrome.status_more.is_empty()).then(|| view! { <span class="sep">"·"</span><span>{chrome.status_more}</span> })}</p>
                }
            })}
            <div class="hsecs">
                <For each=move || section_ids.get().into_iter().enumerate() key=|(_, id)| id.clone() let:item>
                    <Section id=item.1 index=item.0 vm=vm/>
                </For>
            </div>
            <Show when=move || has_cards.get()>
                <Cards vm=vm/>
            </Show>
        </div>
    }
}

/// The rows before a section, for the load-in delay of its meters (rows one and a half staggers apart, page-wide).
fn rows_before(vm: &Option<DashboardView>, index: usize) -> usize {
    vm.as_ref()
        .map(|v| v.sections.iter().take(index).map(|s| s.rows.len()).sum())
        .unwrap_or(0)
}

/// The load-in delay of a meter in row `row` (page-wide reading order), in ms.
fn meter_delay(row: usize) -> u32 {
    140 + row as u32 * 48
}

#[component]
fn Section(id: String, index: usize, vm: Memo<Option<DashboardView>>) -> impl IntoView {
    let sec = {
        let id = id.clone();
        Memo::new(move |_| {
            vm.with(|v| {
                v.as_ref()
                    .and_then(|v| v.sections.iter().find(|s| s.id == id).cloned())
                    .unwrap_or_default()
            })
        })
    };
    let first_row = vm.with_untracked(|v| rows_before(v, index));
    let confirm_id = RwSignal::new(String::new());
    // The switch point being picked, before the server has it.
    let pending = RwSignal::new(None::<i64>);
    let row_ids =
        Memo::new(move |_| sec.with(|s| s.rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>()));
    let cols = Memo::new(move |_| sec.with(|s| s.columns.len()));
    let active = Memo::new(move |_| {
        sec.with(|s| {
            if s.switchable {
                s.rows
                    .iter()
                    .find(|r| r.active)
                    .map(|r| r.id.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            }
        })
    });
    let sel = format!("[data-sec=\"{}\"]", id.replace('"', ""));
    place_glide(sel.clone(), sec, active, confirm_id);
    let style = move || format!("--cols:{}", cols.get().max(1));
    let kind = move || sec.with(|s| s.kind.clone());
    let empty = move || sec.with(|s| (s.rows.is_empty()).then(|| s.empty.clone()));
    view! {
        <section class="plate hsec" data-sec=id.clone() data-kind=kind style=move || format!("--i:{index}")>
            <SectionHead sec=sec pending=pending/>
            {move || empty().map(|text| view! { <p class="hempty">{text}</p> })}
            <Show when=move || !sec.with(|s| s.rows.is_empty())>
                <div class=move || format!("htable cols-{}", cols.get().min(6)) style=style>
                    <div class="hhead" aria-hidden="true">
                        <span class="cap">"Account"</span>
                        {move || sec.with(|s| s.columns.iter().map(|c| {
                            let short = if c.short_label.is_empty() { c.label.clone() } else { c.short_label.clone() };
                            view! { <span class="cap hcol"><span class="full">{c.label.clone()}</span><span class="short">{short}</span></span> }
                        }).collect_view())}
                        <span></span>
                    </div>
                    <div class="hglide" aria-hidden="true"></div>
                    <For each=move || row_ids.get().into_iter().enumerate() key=|(_, id)| id.clone() let:item>
                        <Row id=item.1 sec=sec delay=meter_delay(first_row + item.0) confirm_id=confirm_id pending=pending/>
                    </For>
                </div>
            </Show>
            {move || sec.with(|s| s.foot.shown.then(|| {
                let warn = s.foot.warn;
                view! {
                    <div class="plate-foot hfoot" class:warn=warn>
                        {if warn { view! { <Icon name="alert"/> }.into_any() } else { view! { <Icon name="swap"/> }.into_any() }}
                        <span class="hfoot-text">{runs_view(s.foot.runs.clone())}</span>
                        {(!s.foot.when.is_empty()).then(|| view! { <span class="when tnum">{s.foot.when.clone()}</span> })}
                    </div>
                }
            }))}
        </section>
    }
}

/// The selected-row highlight: one per switchable section, placed on the active row. A re-layout places it at once; a
/// switch glides it there (520 ms ease-out, never past the row).
fn place_glide(
    sel: String,
    sec: Memo<SectionView>,
    active: Memo<String>,
    confirm_id: RwSignal<String>,
) {
    let last = Rc::new(std::cell::RefCell::new(String::new()));
    let place = {
        let sel = sel.clone();
        Rc::new(move |id: String, glide: bool| {
            let doc = document();
            let Some(glider) = doc
                .query_selector(&format!("{sel} .hglide"))
                .ok()
                .flatten()
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            else {
                return;
            };
            let row = (!id.is_empty())
                .then(|| {
                    doc.query_selector(&format!(
                        "{sel} .hrow[data-row=\"{}\"]",
                        id.replace('"', "")
                    ))
                    .ok()
                    .flatten()
                    .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
                })
                .flatten();
            let style = glider.style();
            match row {
                Some(row) => {
                    let top = row.offset_top() as f64;
                    let height = row.offset_height() as f64;
                    let _ = style.set_property(
                        "transition",
                        if glide && motion_on() {
                            "transform 520ms cubic-bezier(.2,.8,.2,1), height 520ms cubic-bezier(.2,.8,.2,1)"
                        } else {
                            "none"
                        },
                    );
                    let _ = style.set_property("transform", &format!("translateY({top}px)"));
                    let _ = style.set_property("height", &format!("{}px", (height - 6.0).max(0.0)));
                    let _ = style.set_property("opacity", "1");
                }
                None => {
                    let _ = style.set_property("opacity", "0");
                }
            }
        })
    };
    {
        let place = place.clone();
        Effect::new(move |_| {
            let id = active.get();
            sec.track();
            confirm_id.track();
            let previous = last.replace(id.clone());
            let glide = !previous.is_empty() && previous != id;
            let place = place.clone();
            next_frame(move || place(id, glide));
        });
    }
    let handle = window_event_listener(leptos::ev::resize, move |_| {
        let id = active.get_untracked();
        let place = place.clone();
        next_frame(move || place(id, false));
    });
    on_cleanup(move || handle.remove());
}

#[component]
fn SectionHead(sec: Memo<SectionView>, pending: RwSignal<Option<i64>>) -> impl IntoView {
    let frame = use_context::<FrameCtx>();
    let id = sec.with_untracked(|s| s.id.clone());
    let is_claude = sec.with_untracked(|s| s.kind == "claude");
    // What the head shows on its right: rebuilt only when that changes, so the auto-switch control keeps its state
    // (a flip waiting for the server, the switch point being picked) through the minute's refresh.
    let mode = Memo::new(move |_| {
        sec.with(|s| {
            if s.kind == "claude" || !s.switchable || s.rows.is_empty() {
                0
            } else if s.auto.shown {
                1
            } else {
                2
            }
        })
    });
    let tools = move || match mode.get() {
        1 => Some(view! { <AutoSwitch sec=sec pending=pending/> }.into_any()),
        2 => {
            let tip = move || {
                sec.with(|s| {
                    format!(
                        "Switching moves between {} accounts, so it starts once a second account is signed in.",
                        s.label
                    )
                })
            };
            Some(
                view! {
                    <div class="hauto-off" title=tip><Icon name="swap"/>
                        <span>{move || runs_view(sec.with(|s| s.auto.off_runs.clone()))}</span></div>
                }
                .into_any(),
            )
        }
        _ => None,
    };
    view! {
        <header class="plate-head hhead-plate">
            <ProviderMark provider=id.clone() size=26/>
            <div class="htitle">
                <h2>{move || sec.with(|s| s.label.clone())}</h2>
                <p class="plate-meta">{move || runs_view(sec.with(|s| s.meta_runs.clone()))}</p>
            </div>
            {is_claude.then(|| view! {
                <div class="plate-tools">
                    <Legend/>
                    <button class="btn ghost hhist" title="Usage history in Analytics"
                        on:click=move |_| { if let Some(f) = frame { f.go(Page::Analytics); } }>
                        <Icon name="chart"/><span class="hhist-label">"History"</span>
                    </button>
                </div>
            })}
            {tools}
        </header>
    }
}

/// The meter legend behind one "?" (it used to be a permanent strip).
#[component]
fn Legend() -> impl IntoView {
    let open = RwSignal::new(false);
    let close = window_event_listener(leptos::ev::click, move |_| open.set(false));
    let esc = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" {
            open.set(false);
        }
    });
    on_cleanup(move || {
        close.remove();
        esc.remove();
    });
    let hover = crate::ui::meter::media_matches("(hover: hover)");
    let rows: [(&str, &str, &str); 6] = [
        ("calm", "Calm", "under 80% of the window used"),
        ("warn", "Warning", "80 to 95%"),
        ("crit", "Critical", "95 to 100%"),
        (
            "over",
            "Overage",
            "past 100%, drawn beyond the end of the track",
        ),
        ("na", "Unavailable", "no reading; never shown as zero"),
        ("notch", "Notch", "the auto-switch point, in % used"),
    ];
    view! {
        <div class="hlegend" on:click=|ev| ev.stop_propagation()>
            <button class="icon-btn bare" aria-label="Reading the meters" aria-expanded=move || open.get().to_string()
                class:on=move || open.get() on:click=move |_| open.update(|o| *o = !*o)>
                <Icon name="help"/>
            </button>
            <Show when=move || open.get()>
                <div class="hpop" role="dialog" aria-label="Reading the meters">
                    <b class="hpop-title">"Reading the meters"</b>
                    <dl>
                        {rows.iter().map(|(swatch, name, note)| view! {
                            <div class="hleg-row">
                                <dt><i class="hsw fill" data-sw=*swatch></i>{*name}</dt>
                                <dd>{*note}</dd>
                            </div>
                        }).collect_view()}
                    </dl>
                    <p class="hpop-foot">{if hover {
                        "Hover a meter for its reset time, sample time and source."
                    } else {
                        "Tap an account for its reset time, sample time and source."
                    }}</p>
                </div>
            </Show>
        </div>
    }
}

/// The auto-switch toggle and the "% used" switch point. The point is sent 600 ms after the last step; the notch moves
/// at once, and the picked value shows until the view reports it (or 5 s pass).
#[component]
fn AutoSwitch(sec: Memo<SectionView>, pending: RwSignal<Option<i64>>) -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let section = sec.with_untracked(|s| s.id.clone());
    let antigravity = section == "antigravity";
    let auto = Memo::new(move |_| sec.with(|s| s.auto.clone()));
    let flipped = RwSignal::new(None::<bool>);
    // The server's answer arrives with the next view: the toggle follows it.
    Effect::new(move |_| {
        auto.with(|a| a.enabled);
        flipped.set(None);
    });
    let reported = Memo::new(move |_| auto.with(|a| a.threshold_used));
    Effect::new(move |_| {
        if reported.get().is_some() && pending.get_untracked() == reported.get() {
            pending.set(None);
        }
    });
    let on = move || flipped.get().unwrap_or_else(|| auto.with(|a| a.enabled));
    let can_toggle =
        move || auto.with(|a| a.available && (a.enabled || a.can_enable)) && !dash.busy.get();
    let toggle = move |_| {
        if !can_toggle() {
            return;
        }
        let next = !on();
        flipped.set(Some(next));
        dash.act(
            if antigravity {
                "antigravity-automatic"
            } else {
                "automatic"
            },
            if next { "true" } else { "false" },
        );
    };
    let shown = move || pending.get().or_else(|| auto.with(|a| a.threshold_used));
    let timer = RwSignal::new(0i32);
    let release = RwSignal::new(0i32);
    let step = move |d: i64| {
        let (known, min, max) =
            auto.with_untracked(|a| (a.known, a.min.unwrap_or(1), a.max.unwrap_or(99)));
        if !known || dash.busy.get_untracked() {
            return;
        }
        let Some(now) = pending
            .get_untracked()
            .or_else(|| auto.with_untracked(|a| a.threshold_used))
        else {
            return;
        };
        let next = (now + d).clamp(min, max);
        pending.set(Some(next));
        window().clear_timeout_with_handle(timer.get_untracked());
        window().clear_timeout_with_handle(release.get_untracked());
        timer.set(after(600, move || {
            let Some(value) = pending.try_get_untracked().flatten() else {
                return;
            };
            dash.act(
                if antigravity {
                    "antigravity-threshold"
                } else {
                    "threshold"
                },
                &format!("{value}%"),
            );
            let _ = release.try_set(after(5000, move || {
                let _ = pending.try_set(None);
            }));
        }));
    };
    let tip = move || {
        auto.with(|a| {
            if a.setting.is_empty() {
                a.message.clone()
            } else {
                format!("{} · {}", a.message, a.setting)
            }
        })
    };
    let at_min = move || {
        shown()
            .zip(auto.with(|a| a.min))
            .is_some_and(|(v, m)| v <= m)
    };
    let at_max = move || {
        shown()
            .zip(auto.with(|a| a.max))
            .is_some_and(|(v, m)| v >= m)
    };
    let stepper_off = move || !auto.with(|a| a.known) || dash.busy.get();
    view! {
        <div class="hauto" title=tip>
            <button class="htoggle" role="switch" aria-checked=move || on().to_string()
                aria-disabled=move || (!can_toggle()).to_string() on:click=toggle>
                <span class="hknob" aria-hidden="true"></span>"Auto-switch"
            </button>
            <Show when=move || auto.with(|a| a.known)>
                <span class="hauto-at">
                    "at"
                    <span class="hstep" class:off=stepper_off>
                        <button aria-label="Switch earlier" aria-disabled=move || (stepper_off() || at_min()).to_string() on:click=move |_| step(-1)><Icon name="minus"/></button>
                        <span class="hstep-val tnum">{move || shown().map(|v| format!("{v}%")).unwrap_or_else(|| "–".into())}</span>
                        <button aria-label="Switch later" aria-disabled=move || (stepper_off() || at_max()).to_string() on:click=move |_| step(1)><Icon name="plus"/></button>
                    </span>
                    "used"
                </span>
            </Show>
            {move || auto.with(|a| (!a.pool.is_empty()).then(|| view! { <span class="hauto-pool">{a.pool.clone()}</span> }))}
        </div>
    }
}

#[component]
fn Row(
    id: String,
    sec: Memo<SectionView>,
    delay: u32,
    confirm_id: RwSignal<String>,
    pending: RwSignal<Option<i64>>,
) -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let frame = use_context::<FrameCtx>();
    let row = {
        let id = id.clone();
        Memo::new(move |_| {
            sec.with(|s| {
                s.rows
                    .iter()
                    .find(|r| r.id == id)
                    .cloned()
                    .unwrap_or_default()
            })
        })
    };
    let switchable = Memo::new(move |_| sec.with(|s| s.switchable));
    let can_switch = Memo::new(move |_| sec.with(|s| s.can_switch));
    let label = sec.with_untracked(|s| s.label.clone());
    let reason = RwSignal::new(String::new());
    let busy = dash.busy;
    let open_details = {
        let id = id.clone();
        move || {
            reason.set(String::new());
            dash.act("details", &id);
        }
    };
    let on_click = {
        let open = open_details.clone();
        move |_| open()
    };
    let on_key = move |ev: web_sys::KeyboardEvent| {
        if on_key_activate(&ev) {
            ev.prevent_default();
            open_details();
        }
    };
    let n_cells = Memo::new(move |_| row.with(|r| r.cells.len()));
    let email = Memo::new(move |_| row.with(|r| r.email.clone()));
    let meta = Memo::new(move |_| row.with(|r| r.meta.clone()));
    let owner = row.with_untracked(|r| format!("{label} · {}", r.email));
    // The notch moves with the switch point being picked.
    let notch_at = Signal::derive(move || pending.get().map(|v| v as f64));
    let cells = move || {
        (0..n_cells.get())
            .map(|i| {
                let view = Signal::derive(move || row.with(|r| r.cells.get(i).cloned().unwrap_or_default()));
                let empty = move || view.with(|v| v.key.is_empty());
                let owner = owner.clone();
                view! {
                    <div class="hcell" class:empty=empty>
                        {move || (!empty()).then(|| view! {
                            <UsageMeter view=view delay=delay + i as u32 * 16 owner=owner.clone() notch_at=notch_at/>
                        })}
                    </div>
                }
            })
            .collect_view()
    };
    // The action slot: rebuilt only when its kind changes, so a reading's refresh never replays the Active mark's
    // draw-on or the Activate cross-fade; a switch moves them through their classes.
    let slot_kind = Memo::new(move |_| {
        if !switchable.get() {
            0
        } else if row.with(|r| r.setup) {
            1
        } else if can_switch.get() {
            2
        } else {
            3
        }
    });
    let is_active = Memo::new(move |_| row.with(|r| r.active));
    let show_button = Memo::new(move |_| can_switch.get() && !is_active.get());
    let hint = Memo::new(move |_| row.with(|r| r.activate_hint.clone()));
    let where_ = Memo::new(move |_| {
        row.with(|r| {
            if r.active_label.is_empty() {
                format!("on {}", r.platform)
            } else {
                r.active_label.clone()
            }
        })
    });
    let activate = move |ev: web_sys::MouseEvent| {
        ev.stop_propagation();
        if busy.get_untracked() {
            return;
        }
        let r = row.get_untracked();
        if r.active || !can_switch.get_untracked() {
            return;
        }
        if !r.can_activate {
            reason.set(r.activate_hint.clone());
            return;
        }
        reason.set(String::new());
        if r.confirm {
            confirm_id.set(r.id.clone());
        } else {
            dash.act(&r.activate_kind, &r.profile);
        }
    };
    let mark = move || {
        view! {
            <span class="hmark" class:on=move || is_active.get() aria-hidden=move || (!is_active.get()).to_string()>
                <svg class="hcheck" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="m7.6 12.4 3.1 3.1 5.7-6.2"/></svg>
                <span class="hmark-t"><b>"Active"</b><span>{move || where_.get()}</span></span>
            </span>
        }
    };
    let slot = move || {
        match slot_kind.get() {
        0 => view! { <OpenButtons row=row reason=reason/> }.into_any(),
        1 => view! {
            <button class="btn accent-line sm hbtn" on:click=move |ev| {
                ev.stop_propagation();
                if let Some(f) = frame { f.go(Page::Accounts); }
            }><Icon name="terminal"/>"Finish setup"</button>
        }
        .into_any(),
        2 => view! {
            <span class="hswap">
                <button class="btn accent-line sm hbtn hact" class:on=move || show_button.get() title=move || hint.get()
                    aria-hidden=move || (!show_button.get()).to_string() tabindex=move || if show_button.get() { "0" } else { "-1" }
                    aria-disabled=move || (busy.get() || !row.with(|r| r.can_activate)).to_string()
                    on:click=activate>
                    <Icon name="swap"/>"Activate"
                </button>
                {mark()}
            </span>
        }
        .into_any(),
        _ => view! { <span class="hswap">{mark()}</span> }.into_any(),
    }
    };
    let confirming = Memo::new(move |_| {
        let id = confirm_id.get();
        !id.is_empty() && row.with(|r| r.id == id && r.confirm && !r.active)
    });
    let keep = move |ev: web_sys::MouseEvent| {
        ev.stop_propagation();
        confirm_id.set(String::new());
    };
    let anyway = move |ev: web_sys::MouseEvent| {
        ev.stop_propagation();
        if busy.get_untracked() {
            return;
        }
        let r = row.get_untracked();
        confirm_id.set(String::new());
        dash.act(&r.activate_kind, &r.profile);
    };
    view! {
        <div class="hrow" class:active=move || switchable.get() && row.with(|r| r.active)
            class:setup=move || row.with(|r| r.setup) data-row=id.clone()
            role="button" tabindex="0" aria-label=move || row.with(|r| format!("{} details", r.email))
            on:click=on_click on:keydown=on_key>
            <div class="hid">
                <ProviderMark provider=row.with_untracked(|r| r.provider.clone()) size=22/>
                <div class="hid-t">
                    <b class="hemail">{move || breakable(&email.get())}</b>
                    <span class="hmeta">{move || meta.get()}</span>
                    {move || row.with(|r| (!r.amounts_runs.is_empty()).then(|| view! {
                        <span class="hamts tnum">{runs_view(r.amounts_runs.clone())}</span>
                    }))}
                </div>
            </div>
            <div class="hcells">{cells}</div>
            <div class="hslot" on:click=|ev| ev.stop_propagation()>{slot}</div>
            <Show when=move || confirming.get()>
                <div class="hconfirm" role="group" aria-label="Activate anyway?" on:click=|ev| ev.stop_propagation()>
                    <Icon name="alert"/>
                    <p>{move || runs_view(row.with(|r| r.confirm_runs.clone()))}</p>
                    <div class="hconfirm-btns">
                        <button class="btn ghost sm" aria-disabled=move || busy.get().to_string() on:click=keep>"Keep current"</button>
                        <button class="btn accent-line sm" aria-disabled=move || busy.get().to_string() on:click=anyway>"Activate anyway"</button>
                    </div>
                </div>
            </Show>
            {move || {
                let why = reason.get();
                (!why.is_empty()).then(|| view! {
                    <p class="hreason" on:click=|ev| ev.stop_propagation()><Icon name="info"/><span>{why}</span></p>
                })
            }}
        </div>
    }
}

/// Claude's Open on Mac and Open on Windows: a control that can't open keeps its place and says why when tapped.
#[component]
fn OpenButtons(row: Memo<RowView>, reason: RwSignal<String>) -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let button = move |target: &'static str| {
        let (glyph, name) = if target == "mac" {
            ("mac", "Mac")
        } else {
            ("windows", "Windows")
        };
        let can = move || {
            row.with(|r| {
                if target == "mac" {
                    r.can_mac
                } else {
                    r.can_windows
                }
            })
        };
        let tip = move || {
            if can() {
                format!("Open on {name}")
            } else {
                format!("Open on {name} is unavailable for this profile")
            }
        };
        view! {
            <button class="icon-btn hopen" aria-label=tip title=tip
                aria-disabled=move || (!can() || dash.busy.get()).to_string()
                on:click=move |ev| {
                    ev.stop_propagation();
                    if dash.busy.get_untracked() { return; }
                    if !can() { reason.set(tip()); return; }
                    reason.set(String::new());
                    let profile = row.with_untracked(|r| r.profile.clone());
                    dash.act("launch", &format!("{profile}:{target}"));
                }>
                <PlatformGlyph id=glyph.to_string()/>
            </button>
        }
    };
    view! { <span class="hopens">{button("mac")}{button("windows")}</span> }
}

// ---------------------------------------------------------------- other providers

#[component]
fn Cards(vm: Memo<Option<DashboardView>>) -> impl IntoView {
    let ids = Memo::new(move |_| {
        vm.with(|v| {
            v.as_ref()
                .map(|v| v.cards.iter().map(|c| c.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
    });
    let first_row = vm.with_untracked(|v| {
        v.as_ref()
            .map(|v| v.sections.iter().map(|s| s.rows.len()).sum::<usize>())
            .unwrap_or(0)
    });
    let layout = Memo::new(move |_| {
        let n = ids.with(|i| i.len());
        (1..=CARD_COLUMNS)
            .map(|c| card_rows(n, c))
            .collect::<Vec<_>>()
    });
    let grid_style = move || {
        layout.with(|l| {
            l.iter()
                .enumerate()
                .map(|(i, (cols, _))| format!("--L{}:{cols};", i + 1))
                .collect::<String>()
        })
    };
    let count = move || {
        let n = ids.with(|i| i.len());
        if n == 1 {
            "1 account".to_string()
        } else {
            format!("{n} accounts")
        }
    };
    view! {
        <section class="hcards-wrap" aria-label="Other providers">
            <div class="hcards-head">
                <h2>"Other providers"</h2>
                <span class="plate-meta tnum">{count}</span>
            </div>
            <div class="hcards" style=grid_style>
                <For each=move || ids.get().into_iter().enumerate() key=|(_, id)| id.clone() let:item>
                    <ProviderCard id=item.1 index=item.0 vm=vm layout=layout delay=meter_delay(first_row + item.0)/>
                </For>
            </div>
        </section>
    }
}

#[component]
fn ProviderCard(
    id: String,
    index: usize,
    vm: Memo<Option<DashboardView>>,
    layout: Memo<Vec<(usize, Vec<usize>)>>,
    delay: u32,
) -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let card = {
        let id = id.clone();
        Memo::new(move |_| {
            vm.with(|v| {
                v.as_ref()
                    .and_then(|v| v.cards.iter().find(|c| c.id == id).cloned())
                    .unwrap_or_default()
            })
        })
    };
    // Its place in the list can move when another card comes or goes.
    let place = {
        let id = id.clone();
        Memo::new(move |_| {
            vm.with(|v| {
                v.as_ref()
                    .and_then(|v| v.cards.iter().position(|c| c.id == id))
                    .unwrap_or(index)
            })
        })
    };
    let style = move || {
        let at = place.get();
        layout.with(|l| {
            l.iter()
                .enumerate()
                .map(|(i, (_, spans))| {
                    format!("--s{}:{};", i + 1, spans.get(at).copied().unwrap_or(1))
                })
                .collect::<String>()
        })
    };
    let open = move || {
        let account = card.with_untracked(|c| c.account_id.clone());
        if !account.is_empty() {
            dash.act("details", &account);
        }
    };
    let n_meters = Memo::new(move |_| card.with(|c| c.meters.len()));
    let owner = card.with_untracked(|c| format!("{} · {}", c.label, c.identity));
    let meters = move || {
        (0..n_meters.get())
            .map(|i| {
                let view = Signal::derive(move || card.with(|c| c.meters.get(i).cloned().unwrap_or_default()));
                view! { <UsageMeter view=view mode=MeterMode::Inline delay=delay + i as u32 * 16 owner=owner.clone()/> }
            })
            .collect_view()
    };
    let note = |icon: bool, text: String| {
        view! {
            <p class="hnote">{icon.then(|| view! { <Icon name="calendar"/> })}<span>{text}</span></p>
        }
    };
    view! {
        <article class="hcard" style=style role="button" tabindex="0"
            aria-label=move || card.with(|c| format!("{} {} details", c.label, c.identity))
            on:click=move |_| open()
            on:keydown=move |ev| if on_key_activate(&ev) { ev.prevent_default(); open(); }>
            <header class="hcard-head">
                <ProviderMark provider=card.with_untracked(|c| c.provider.clone()) size=24/>
                <div class="hcard-who">
                    <div class="hcard-name"><b>{move || card.with(|c| c.label.clone())}</b>
                        {move || card.with(|c| (!c.plan.is_empty()).then(|| view! { <span>{c.plan.clone()}</span> }))}</div>
                    <div class="hcard-id">{move || breakable(&card.with(|c| c.identity.clone()))}</div>
                </div>
            </header>
            <div class="hcard-meters">{meters}</div>
            {move || card.with(|c| (c.meters.is_empty() && !c.note.is_empty()).then(|| note(false, c.note.clone())))}
            {move || card.with(|c| (!c.plan_note.is_empty()).then(|| note(true, c.plan_note.clone())))}
            {move || card.with(|c| (!c.amounts.is_empty()).then(|| view! {
                <dl class="hamounts">
                    {c.amounts.iter().map(|a| view! {
                        <div class="hamount">
                            <dt><IconOf name=a.icon.clone()/><span>{a.label.clone()}</span></dt>
                            <dd><b class="tnum" class:spent=a.spent>{a.value.clone()}</b>
                                {(!a.unit.is_empty()).then(|| view! { <span class="hunit">{a.unit.clone()}</span> })}
                                {(!a.sub.is_empty()).then(|| view! { <small>{a.sub.clone()}</small> })}
                            </dd>
                        </div>
                    }).collect_view()}
                </dl>
            }))}
            {move || card.with(|c| (!c.packs_note.is_empty()).then(|| note(true, c.packs_note.clone())))}
            {move || card.with(|c| (!c.meters.is_empty() && !c.note.is_empty()).then(|| note(false, c.note.clone())))}
            <footer class="hcard-foot">
                {move || card.with(|c| (!c.flag.is_empty()).then(|| view! { <b class="w-warn">{c.flag.clone()}</b><span class="sep">"·"</span> }))}
                <span class="tnum hsampled">{move || card.with(|c| c.sampled.clone())}</span>
                <span class="hplat">{move || card.with(|c| c.platform.clone())}</span>
            </footer>
        </article>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_fill_every_row_and_put_the_longer_rows_first() {
        // 4 cards, 3 a row at most: two rows of two, not three and one.
        assert_eq!(card_rows(4, 3), (2, vec![1, 1, 1, 1]));
        // 5 cards, 3 a row: 3 then 2, on a grid of 6.
        assert_eq!(card_rows(5, 3), (6, vec![2, 2, 2, 3, 3]));
        // 7 cards, 4 a row: 4 then 3, on a grid of 12.
        assert_eq!(card_rows(7, 4), (12, vec![3, 3, 3, 3, 4, 4, 4]));
        // fewer cards than columns: one row of them
        assert_eq!(card_rows(2, 7), (2, vec![1, 1]));
        assert_eq!(card_rows(1, 1), (1, vec![1]));
        assert_eq!(card_rows(0, 3), (1, vec![]));
        // every row adds up to the grid
        for n in 1..=12 {
            for c in 1..=CARD_COLUMNS {
                let (cols, spans) = card_rows(n, c);
                let mut row = 0;
                for s in spans {
                    row += s;
                    assert!(row <= cols);
                    if row == cols {
                        row = 0;
                    }
                }
                assert_eq!(row, 0, "no row is left short ({n} cards, {c} columns)");
            }
        }
    }

    #[test]
    fn another_version_is_refused_and_an_empty_push_waits() {
        assert_eq!(parse(""), HomeState::Waiting);
        assert!(matches!(
            parse(r#"{"version":3,"sections":[]}"#),
            HomeState::Refused(_)
        ));
        assert!(matches!(parse("not json"), HomeState::Refused(_)));
        let HomeState::Ready(view) = parse(r#"{"version":2,"sections":null,"cards":[]}"#) else {
            panic!("version 2 draws");
        };
        assert!(view.sections.is_empty());
    }

    #[test]
    fn the_view_model_reads_with_nulls_and_missing_values() {
        let json = r#"{"version":2,"sections":[{"id":"codex","kind":"switchable","label":"Codex","switchable":true,"canSwitch":true,
            "metaRuns":[{"text":"3","strong":true,"tone":""}],"foot":{"shown":true,"warn":false,"runs":[],"when":null},
            "auto":{"known":true,"shown":true,"enabled":true,"available":true,"canEnable":true,"thresholdUsed":null,"min":50,"max":99,
            "pool":"","offRuns":[],"setting":"","message":"Healthy"},"columns":[{"key":"five","label":"5-hour"}],
            "rows":[{"id":"codex:a","provider":"codex","profile":"a","email":"a@example.invalid","active":true,"confirm":false,
            "confirmRuns":null,"cells":[{"key":"codex:a|five_hour","hasValue":false,"value":null,"naText":"Unavailable","notch":95}]}]}],
            "cards":[],"chrome":{"statusLead":"Updated"}}"#;
        let HomeState::Ready(view) = parse(json) else {
            panic!("reads")
        };
        let s = &view.sections[0];
        assert_eq!(
            s.auto.threshold_used, None,
            "an unknown switch point stays unknown"
        );
        assert_eq!(s.rows[0].cells[0].value, None);
        assert_eq!(s.rows[0].cells[0].notch, Some(95.0));
        assert!(s.rows[0].confirm_runs.is_empty());
    }
}
