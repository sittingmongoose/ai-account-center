//! Home (PLAN step 5): the Claude section, the Codex and Antigravity sections, and the other providers as cards, drawn
//! from the dashboard's view model v3 (`public/view-model.mjs` `dashboardViewModel`), which the dashboard controller
//! (`public/frame-dashboard.mjs`) builds and sends (`dash.rs`). Every number, word and "unavailable" decision is in
//! that view; this page lays it out and animates it. Intent goes back as the dashboard's own action kinds, so every
//! server call stays in the controller. The Codex and Antigravity rows mark the live login on Ubuntu; AI Account
//! Center no longer switches accounts (2026-11-14).
//!
//! Layout follows the space the page has (container queries, `style/app.css` "Home"): each section is a table (the
//! account, one column per window, the action slot) once it is wide enough for its columns, and a stacked row (the
//! account line, then the meters side by side) below that; two sections share a row from 760 px below desktop and
//! from 1600 px on desktop; provider cards tile in equal-width rows with no gap at the end of a row.

use crate::Page;
use crate::dash::DashCtx;
use crate::frame::FrameCtx;
use crate::ui::*;
use leptos::prelude::*;

/// The view model version this page draws (view-model.mjs `VIEW_MODEL_VERSION`, aac-api `HOME_VIEW_VERSION`). Another
/// version is refused, not half drawn.
pub const VIEW_MODEL_VERSION: u32 = aac_api::home::HOME_VIEW_VERSION;

// ---------------------------------------------------------------- view model v3 (README-ARCHITECTURE.md)

// The types are the ones the Rust server writes (crates/aac-api, D7): the page reads the server's view and the
// controller's own JavaScript view through the same structs, so a shape drift is a compile error.
pub use aac_api::home::{HomeView as DashboardView, RowView, SectionView};

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

/// Where an address may wrap, as whole chunks: before the "@", and before the final domain (its last two labels)
/// when the domain has more. Never inside a domain label. Anything without an "@" is one chunk.
pub fn address_chunks(text: &str) -> Vec<String> {
    let Some(at) = text.rfind('@') else {
        return vec![text.to_string()];
    };
    let (local, domain) = text.split_at(at);
    let mut chunks = Vec::new();
    if !local.is_empty() {
        chunks.push(local.to_string());
    }
    let labels: Vec<&str> = domain[1..].split('.').collect();
    if labels.len() > 2 {
        let head = labels[..labels.len() - 2].join(".");
        chunks.push(format!("@{head}."));
        chunks.push(labels[labels.len() - 2..].join("."));
    } else {
        chunks.push(domain.to_string());
    }
    chunks
}

/// An address that wraps only between its chunks. The local part may still wrap where it must (a hyphen, or a word
/// too long for the column); the domain chunks never do.
fn breakable(text: &str) -> impl IntoView + use<> {
    let chunks = address_chunks(text);
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let domain = chunk.starts_with('@') || (i > 0 && i + 1 == n);
            view! {
                {(i > 0).then(|| view! { <wbr/> })}
                <span class:hdom=domain>{chunk}</span>
            }
        })
        .collect_view()
}

fn on_key_activate(ev: &web_sys::KeyboardEvent) -> bool {
    matches!(ev.key().as_str(), "Enter" | " ")
}

// ---------------------------------------------------------------- the page

#[component]
pub fn HomePage() -> impl IntoView {
    crate::ui::meter::provide_meter_tips();
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
    let waiting = Memo::new(move |_| state.with(|s| matches!(s, HomeState::Waiting)));
    crate::frame::mark_ready_when(Page::Home, move || !waiting.get());
    view! {
        <div class="page home" class:motion=motion>
            <Show when=move || waiting.get()><PageSplash name="Home"/></Show>
            {move || match state.get() {
                HomeState::Waiting => None,
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
    let row_ids =
        Memo::new(move |_| sec.with(|s| s.rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>()));
    let cols = Memo::new(move |_| sec.with(|s| s.columns.len()));
    let style = move || format!("--cols:{}", cols.get().max(1));
    let kind = move || sec.with(|s| s.kind.clone());
    let empty = move || sec.with(|s| (s.rows.is_empty()).then(|| s.empty.clone()));
    view! {
        <section class="plate hsec" data-sec=id.clone() data-kind=kind style=move || format!("--i:{index}")>
            <SectionHead sec=sec/>
            {move || empty().map(|text| view! { <p class="hempty">{text}</p> })}
            <Show when=move || !sec.with(|s| s.rows.is_empty())>
                // More than six windows (Antigravity's two quota services) never becomes a table: it stays
                // stacked with each cell named, so ten columns never overflow the section.
                <div class=move || match cols.get() { n if n > 6 => "htable cols-many".to_string(), n => format!("htable cols-{n}") } style=style>
                    <div class="hhead" aria-hidden="true">
                        <span class="cap">"Account"</span>
                        {move || sec.with(|s| s.columns.iter().map(|c| {
                            let short = if c.short_label.is_empty() { c.label.clone() } else { c.short_label.clone() };
                            view! { <span class="cap hcol"><span class="full">{c.label.clone()}</span><span class="short">{short}</span></span> }
                        }).collect_view())}
                        <span></span>
                    </div>
                    <For each=move || row_ids.get().into_iter().enumerate() key=|(_, id)| id.clone() let:item>
                        <Row id=item.1 sec=sec delay=meter_delay(first_row + item.0)/>
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

#[component]
fn SectionHead(sec: Memo<SectionView>) -> impl IntoView {
    let frame = use_context::<FrameCtx>();
    let id = sec.with_untracked(|s| s.id.clone());
    let is_claude = sec.with_untracked(|s| s.kind == "claude");
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
    let rows: [(&str, &str, &str); 5] = [
        ("calm", "Calm", "under 80% of the window used"),
        ("warn", "Warning", "80 to 95%"),
        ("crit", "Critical", "95 to 100%"),
        (
            "over",
            "Overage",
            "past 100%, drawn beyond the end of the track",
        ),
        ("na", "Unavailable", "no reading; never shown as zero"),
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

#[component]
fn Row(id: String, sec: Memo<SectionView>, delay: u32) -> impl IntoView {
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
    let is_claude = sec.with_untracked(|s| s.kind == "claude");
    let label = sec.with_untracked(|s| s.label.clone());
    let reason = RwSignal::new(String::new());
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
    let cells = move || {
        (0..n_cells.get())
            .map(|i| {
                let view = Signal::derive(move || {
                    row.with(|r| r.cells.get(i).cloned().unwrap_or_default())
                });
                // A Memo, so a new reading reaches the meter through `view` and never rebuilds it: rebuilding made
                // every meter replay its first sweep from 0 on each refresh (owner report 2026-11-20, the page
                // "flashes" every 30-60 s). Only a cell turning empty or back rebuilds.
                let empty = Memo::new(move |_| view.with(|v| v.key.is_empty()));
                let owner = owner.clone();
                // The column's name over the cell, shown when a section with many windows stacks (its heads would wrap).
                let caption = move || {
                    sec.with(|s| {
                        s.columns
                            .get(i)
                            .map(|c| {
                                if c.short_label.is_empty() {
                                    c.label.clone()
                                } else {
                                    c.short_label.clone()
                                }
                            })
                            .unwrap_or_default()
                    })
                };
                view! {
                    <div class="hcell" class:empty=empty>
                        {move || (!empty.get()).then(|| view! {
                            <span class="cap hcell-cap">{caption}</span>
                            <UsageMeter view=view delay=delay + i as u32 * 16 owner=owner.clone()/>
                        })}
                    </div>
                }
            })
            .collect_view()
    };
    // The action slot: Claude's Open buttons, Finish setup for a profile still signing in, or the Active mark on the
    // live login. Rebuilt only when its kind changes, so a reading's refresh never replays the mark's draw-on.
    let slot_kind = Memo::new(move |_| {
        if is_claude {
            0
        } else if row.with(|r| r.setup) {
            1
        } else {
            2
        }
    });
    let is_active = Memo::new(move |_| row.with(|r| r.active));
    let where_ = Memo::new(move |_| {
        row.with(|r| {
            if r.active_label.is_empty() {
                format!("on {}", r.platform)
            } else {
                r.active_label.clone()
            }
        })
    });
    let mark = move || {
        view! {
            <span class="hmark" class:on=move || is_active.get() aria-hidden=move || (!is_active.get()).to_string()>
                <svg class="hcheck" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10"/><path d="m7.6 12.4 3.1 3.1 5.7-6.2"/></svg>
                <span class="hmark-t"><b>"Active"</b><span>{move || where_.get()}</span></span>
            </span>
        }
    };
    let slot = move || match slot_kind.get() {
        0 => view! { <OpenButtons row=row reason=reason/> }.into_any(),
        1 => view! {
            <button class="btn accent-line sm hbtn" on:click=move |ev| {
                ev.stop_propagation();
                if let Some(f) = frame { f.go(Page::Accounts); }
            }><Icon name="terminal"/>"Finish setup"</button>
        }
        .into_any(),
        _ => view! { <span class="hswap">{mark()}</span> }.into_any(),
    };
    view! {
        <div class="hrow" class:active=move || row.with(|r| r.active)
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
    fn an_address_wraps_only_before_the_at_or_the_final_domain() {
        assert_eq!(
            address_chunks("a.very.long.example.name@example.invalid"),
            vec!["a.very.long.example.name", "@example.invalid"]
        );
        assert_eq!(
            address_chunks("someone@mail.corp.example.com"),
            vec!["someone", "@mail.corp.", "example.com"]
        );
        assert_eq!(address_chunks("Codex"), vec!["Codex"]);
        assert_eq!(address_chunks("@x.y"), vec!["@x.y"]);
    }

    #[test]
    fn another_version_is_refused_and_an_empty_push_waits() {
        assert_eq!(parse(""), HomeState::Waiting);
        assert!(matches!(
            parse(r#"{"version":2,"sections":[]}"#),
            HomeState::Refused(_)
        ));
        assert!(matches!(parse("not json"), HomeState::Refused(_)));
        let HomeState::Ready(view) = parse(r#"{"version":3,"sections":null,"cards":[]}"#) else {
            panic!("version 3 draws");
        };
        assert!(view.sections.is_empty());
    }

    #[test]
    fn the_view_model_reads_with_nulls_and_missing_values() {
        let json = r#"{"version":3,"sections":[{"id":"codex","kind":"accounts","label":"Codex",
            "metaRuns":[{"text":"3","strong":true,"tone":""}],"foot":{"shown":false,"warn":false,"runs":[],"when":null},
            "columns":[{"key":"five","label":"5-hour"}],
            "rows":[{"id":"codex:a","provider":"codex","profile":"a","email":"a@example.invalid","active":true,
            "cells":[{"key":"codex:a|five_hour","hasValue":false,"value":null,"naText":"Unavailable"}]}]}],
            "cards":[],"chrome":{"statusLead":"Updated"}}"#;
        let HomeState::Ready(view) = parse(json) else {
            panic!("reads")
        };
        let s = &view.sections[0];
        assert_eq!(s.kind, "accounts");
        assert!(s.rows[0].active);
        assert_eq!(s.rows[0].cells[0].value, None);
    }
}
