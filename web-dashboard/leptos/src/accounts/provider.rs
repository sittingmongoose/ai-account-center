//! One provider card (Slint `ProviderSection`): the head with its mark, kind line, count and the provider's Show on
//! dashboard / Show in tray switches; one row per account; the Claude trash; the Antigravity policy box; the flow
//! panel; and the footer with how the provider signs in and its add or check actions.
//!
//! A row lays out by the card's own width (container queries in app.css): a table row with every column on wide
//! cards, two lines on medium ones, and three on narrow ones (the remove control lifted beside the name). The action
//! slots sit right-aligned, so a longer status never moves them.

use super::Page;
use super::actions::{ActionControl, ReasonLine, RowLine, icon_name};
use super::flow::FlowPanel;
use super::model::{AgPolicy, Provider, Row};
use crate::ui::*;
use leptos::prelude::*;

fn find(page: Page, id: &str) -> Option<Provider> {
    page.view.with(|v| {
        v.as_ref().and_then(|v| {
            v.col_a
                .iter()
                .chain(v.col_b.iter())
                .find(|p| p.id == id)
                .cloned()
        })
    })
}

/// The head's fields only, so a row's change doesn't redraw it.
#[derive(Clone, Debug, PartialEq, Default)]
struct Head {
    label: String,
    kind_label: String,
    kind_icon: String,
    count_text: String,
    visible: bool,
    hidden_note: String,
    toggle_enabled: bool,
    toggle_tip: String,
    tray_visible: bool,
    tray_enabled: bool,
    tray_coming: bool,
    tray_tip: String,
}

#[component]
pub fn ProviderCard(page: Page, id: String) -> impl IntoView {
    let pid = id.clone();
    let prov = Memo::new(move |_| find(page, &pid));
    let head = Memo::new(move |_| {
        prov.with(|p| {
            p.as_ref()
                .map(|p| Head {
                    label: p.label.clone(),
                    kind_label: p.kind_label.clone(),
                    kind_icon: p.kind_icon.clone(),
                    count_text: p.count_text.clone(),
                    visible: p.visible,
                    hidden_note: p.hidden_note.clone(),
                    toggle_enabled: p.toggle_enabled,
                    toggle_tip: p.toggle_tip.clone(),
                    tray_visible: p.tray_visible,
                    tray_enabled: p.tray_enabled,
                    tray_coming: p.tray_coming,
                    tray_tip: p.tray_tip.clone(),
                })
                .unwrap_or_default()
        })
    });
    let row_ids = Memo::new(move |_| {
        prov.with(|p| {
            p.as_ref()
                .map(|p| p.rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        })
    });
    let switchable = Memo::new(move |_| prov.with(|p| p.as_ref().is_some_and(|p| p.switchable)));
    let can_switch = Memo::new(move |_| prov.with(|p| p.as_ref().is_some_and(|p| p.can_switch)));
    let empty = Memo::new(move |_| {
        prov.with(|p| {
            p.as_ref()
                .filter(|p| p.count == 0)
                .map(|p| p.empty.clone())
                .unwrap_or_default()
        })
    });
    let trash =
        Memo::new(move |_| prov.with(|p| p.as_ref().map(|p| p.trash.clone()).unwrap_or_default()));
    let ag = Memo::new(move |_| prov.with(|p| p.as_ref().is_some_and(|p| p.ag)));
    let flow =
        Memo::new(move |_| prov.with(|p| p.as_ref().map(|p| p.flow.clone()).unwrap_or_default()));
    let foot = Memo::new(move |_| {
        prov.with(|p| {
            p.as_ref()
                .map(|p| (p.how.clone(), p.needs, p.foot.clone()))
                .unwrap_or_default()
        })
    });
    let provider = id.clone();
    let probe = format!("provider:{id}");
    let show_id = id.clone();
    let tray_id = id.clone();
    view! {
        <section class="plate ac-card prov" data-provider=id.clone() data-probe=probe>
            <div class="prov-head">
                <ProviderMark provider=provider.clone() size=26/>
                <div class="ph-text">
                    <h2>{move || head.with(|h| h.label.clone())}</h2>
                    <p class="ph-kind">
                        <Icon name=icon_name(&head.get_untracked().kind_icon)/>
                        <span>{move || head.with(|h| format!("{} · {}", h.kind_label, h.count_text))}</span>
                    </p>
                    {move || head.with(|h| (!h.hidden_note.is_empty()).then(|| view! { <p class="ph-hidden q3">{h.hidden_note.clone()}</p> }))}
                </div>
                <div class="ph-switches">
                    <Toggle small=true label="Show on dashboard"
                        on=Signal::derive(move || head.with(|h| h.visible))
                        enabled=Signal::derive(move || head.with(|h| h.toggle_enabled))
                        tip=Signal::derive(move || head.with(|h| h.toggle_tip.clone()))
                        probe=format!("show:{id}")
                        on_toggle=Callback::new(move |on: bool| page.send("accounts-show", &format!("{show_id}:{}", if on { "show" } else { "hide" })))/>
                    <span class="ph-tray">
                        <Toggle small=true label="Show in tray"
                            on=Signal::derive(move || head.with(|h| h.tray_visible))
                            enabled=Signal::derive(move || head.with(|h| h.tray_enabled))
                            tip=Signal::derive(move || head.with(|h| h.tray_tip.clone()))
                            probe=format!("tray:{id}")
                            on_toggle=Callback::new(move |on: bool| page.send("accounts-tray", &format!("{tray_id}:{}", if on { "show" } else { "hide" })))/>
                        {move || head.with(|h| h.tray_coming).then(|| view! { <ComingWord/> })}
                    </span>
                </div>
            </div>
            <div class="acc-rows" class:switchable=move || switchable.get()>
                <For each=move || row_ids.get() key=|id| id.clone() children=move |rid| {
                    let row = Memo::new(move |_| prov.with(|p| p.as_ref().and_then(|p| p.rows.iter().find(|r| r.id == rid).cloned())));
                    view! { <RowItem page=page row=row can_switch=can_switch/> }
                }/>
                {move || { let e = empty.get(); (!e.is_empty()).then(|| view! { <p class="acc-empty q3">{e}</p> }) }}
            </div>
            <Show when=move || trash.with(|t| !t.is_empty())>
                <div class="trash">
                    <p class="trash-head"><span class="cap">"In the trash"</span><span class="q3">"Restored within 30 days, then deleted for good"</span></p>
                    {move || trash.get().into_iter().map(|t| {
                        let line = t.line.clone();
                        view! {
                            <div class="trash-item">
                                <div class="trash-row">
                                    <div class="tr-text"><span class="tr-label">{t.label.clone()}</span><span class="tr-sub tnum q3">{t.sub.clone()}</span></div>
                                    <span class="tr-acts">
                                        {t.actions.iter().cloned().map(|a| view! { <ActionControl a=a page=page/> }).collect_view()}
                                        {t.actions.last().is_some_and(|a| a.coming).then(|| view! { <ComingWord/> })}
                                    </span>
                                </div>
                                <RowLine line=Signal::derive(move || line.clone()) page=page/>
                            </div>
                        }
                    }).collect_view()}
                </div>
            </Show>
            <Show when=move || ag.get()>
                <AgPolicyBox page=page/>
            </Show>
            <FlowPanel page=page provider=provider.clone() flow=flow/>
            <div class="prov-foot">
                {move || {
                    let (how, needs, _) = foot.get();
                    view! {
                        <p class="pf-how" class:needs=needs>
                            <Icon name=if needs { "alert" } else { "info" }/>
                            <span>{how}</span>
                        </p>
                    }
                }}
                <span class="pf-acts">
                    {move || {
                        let (_, _, actions) = foot.get();
                        let coming = actions.iter().any(|a| a.coming);
                        let single = actions.len() == 1;
                        view! {
                            {actions.into_iter().map(|a| view! { <ActionControl a=a page=page full=single/> }).collect_view()}
                            {coming.then(|| view! { <ComingWord/> })}
                        }
                    }}
                </span>
            </div>
        </section>
    }
}

/// One account row with its lines: the inline "Activate anyway?", the remove / restore / refusal line, and the reason
/// a tap on an off control opened.
#[component]
fn RowItem(page: Page, row: Memo<Option<Row>>, can_switch: Memo<bool>) -> impl IntoView {
    let reason = RwSignal::new(String::new());
    let confirming = RwSignal::new(false);
    // a row that no longer asks (its reading moved under the switch point) closes its question
    Effect::new(move |_| {
        if !row.with(|r| r.as_ref().is_some_and(|r| r.confirm)) {
            confirming.set(false);
        }
    });
    let get = move || row.get().unwrap_or_default();
    let id = Memo::new(move |_| row.with(|r| r.as_ref().map(|r| r.id.clone()).unwrap_or_default()));
    let active = Memo::new(move |_| row.with(|r| r.as_ref().is_some_and(|r| r.active)));
    let line = Signal::derive(move || {
        row.with(|r| r.as_ref().map(|r| r.line.clone()).unwrap_or_default())
    });
    let selected = Memo::new(move |_| {
        let id = id.get();
        !id.is_empty() && page.selected.get() == id
    });
    let set_reason = Callback::new(move |r: String| reason.set(r));
    let activate = move |r: &Row| {
        if r.confirm {
            confirming.set(true);
        } else {
            page.send(&r.activate_kind, &r.profile);
        }
    };
    let confirm_runs = Memo::new(move |_| {
        row.with(|r| {
            r.as_ref()
                .map(|r| r.confirm_runs.clone())
                .unwrap_or_default()
        })
    });
    view! {
        <div class="acc-item" class:active=move || active.get() class:sel=move || selected.get()
            class:gone=move || row.with(|r| r.as_ref().is_some_and(|r| r.gone))
            data-row=move || id.get()>
            <div class="acc-row" role="button" tabindex="0"
                aria-label=move || format!("{}: details", row.with(|r| r.as_ref().map(|r| r.email.clone()).unwrap_or_default()))
                data-probe=move || format!("row:{}", id.get())
                on:click=move |_| { reason.set(String::new()); page.send("details", &id.get_untracked()); }
                on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" { page.send("details", &id.get_untracked()); }>
                <div class="acc-id" class:dim=move || row.with(|r| r.as_ref().is_some_and(|r| !r.shown_dash))>
                    <span class="acc-email">{move || email_parts(get().email)}</span>
                </div>
                <div class="acc-info">
                    {move || { let m = get().meta; (!m.is_empty()).then(|| view! { <span class="acc-meta">{m}</span> }) }}
                    <span class="acc-state">
                        {move || { let s = get().status; (!s.is_empty()).then(|| view! { <span class="acc-status w-warn">{s}</span> }) }}
                        <span class="acc-sampled tnum" title=move || get().sampled_tip>{move || get().sampled}</span>
                    </span>
                    <span class="acc-src">
                        <span class="acc-src-a">{move || get().src}</span>
                        {move || { let s = get().src_sub; (!s.is_empty()).then(|| view! { <span class="acc-src-b">{s}</span> }) }}
                    </span>
                </div>
                <div class="acc-sw">
                    <Toggle small=true label="Dashboard"
                        on=Signal::derive(move || row.with(|r| r.as_ref().is_some_and(|r| r.shown_dash)))
                        enabled=Signal::derive(move || row.with(|r| r.as_ref().is_some_and(|r| r.dash_enabled)))
                        tip=Signal::derive(move || row.with(|r| r.as_ref().map(|r| r.dash_tip.clone()).unwrap_or_default()))
                        probe=format!("acct-show:{}", id.get_untracked())
                        on_toggle=Callback::new(move |on: bool| page.send("account-show", &format!("{}|{}", id.get_untracked(), if on { "show" } else { "hide" })))
                        on_reason=Callback::new(move |_| { let tip = row.with(|r| r.as_ref().map(|r| r.dash_tip.clone()).unwrap_or_default()); if !tip.is_empty() { reason.set(tip); } })/>
                    <Toggle small=true label="Tray"
                        on=Signal::derive(move || row.with(|r| r.as_ref().is_some_and(|r| r.shown_tray)))
                        enabled=Signal::derive(move || row.with(|r| r.as_ref().is_some_and(|r| r.tray_acct_enabled)))
                        tip=Signal::derive(move || row.with(|r| r.as_ref().map(|r| r.tray_acct_tip.clone()).unwrap_or_default()))
                        probe=format!("acct-tray:{}", id.get_untracked())
                        on_toggle=Callback::new(move |on: bool| page.send("account-tray", &format!("{}|{}", id.get_untracked(), if on { "show" } else { "hide" })))
                        on_reason=Callback::new(move |_| { let tip = row.with(|r| r.as_ref().map(|r| r.tray_acct_tip.clone()).unwrap_or_default()); if !tip.is_empty() { reason.set(tip); } })/>
                </div>
                <div class="acc-acts">
                    {move || {
                        let r = get();
                        let n = r.actions.len();
                        // the remove control has its own cell (beside the name on narrow cards)
                        r.actions.iter().cloned().enumerate().filter(|(i, a)| !(i + 1 == n && a.icon == "trash")).map(|(_, a)| {
                            if a.kind == "switch" {
                                let r2 = r.clone();
                                let state = SwitchState {
                                    active: r.active,
                                    active_label: r.active_label.clone(),
                                    platform: "Ubuntu".into(),
                                    can_switch: can_switch.get(),
                                    can_activate: a.enabled,
                                    hint: r.activate_hint.clone(),
                                };
                                view! {
                                    <SwitchSlot state=state busy=page.busy probe=a.probe.clone()
                                        on_activate=Callback::new(move |_| activate(&r2))
                                        on_reason=set_reason/>
                                }.into_any()
                            } else {
                                view! { <ActionControl a=a page=page reason=set_reason/> }.into_any()
                            }
                        }).collect_view()
                    }}
                </div>
                <div class="acc-trash">
                    {move || {
                        let r = get();
                        r.actions.last().filter(|a| a.icon == "trash").cloned().map(|a| view! { <ActionControl a=a page=page reason=set_reason/> })
                    }}
                </div>
            </div>
            <Show when=move || confirming.get() && row.with(|r| r.as_ref().is_some_and(|r| r.confirm))>
                <ConfirmLine runs=confirm_runs.get_untracked() busy=page.busy
                    on_keep=Callback::new(move |_| confirming.set(false))
                    on_confirm=Callback::new(move |_| {
                        confirming.set(false);
                        let r = row.get_untracked().unwrap_or_default();
                        page.send(&r.activate_kind, &r.profile);
                    })/>
            </Show>
            <RowLine line=line page=page/>
            <ReasonLine reason=reason/>
        </div>
    }
}

/// An email that may break after its `@` (and only mid-word when even that doesn't fit), never cut short.
fn email_parts(email: String) -> impl IntoView {
    match email.split_once('@') {
        Some((local, domain)) => {
            view! { {local.to_string()}<wbr/>{format!("@{domain}")} }.into_any()
        }
        None => view! { {email} }.into_any(),
    }
}

/// The Antigravity policy box: the note, the Auto-switch toggle, "Switch at % used", the watched pool and the cooldown.
/// The % stepper sends 600 ms after the last step and shows the pending value until the server reports it (or 5 s).
#[component]
fn AgPolicyBox(page: Page) -> impl IntoView {
    let ag = Memo::new(move |_| {
        page.view
            .with(|v| v.as_ref().map(|v| v.ag.clone()).unwrap_or_default())
    });
    let reported = Signal::derive(move || ag.with(|a| a.threshold));
    let bounds = Signal::derive(move || ag.with(|a| (a.min, a.max)));
    let enabled = Signal::derive(move || ag.with(|a| a.step_enabled && a.known && a.live));
    let busy = page.busy;
    let note = move || {
        let a: AgPolicy = ag.get();
        view! { <p class="ag-note">{(!a.note_strong.is_empty()).then(|| view! { <b>{a.note_strong.clone()}</b>" " })}{a.note.clone()}</p> }
    };
    view! {
        <div class="ag-box" data-probe="ag-policy">
            {note}
            <div class="ag-ctls" class:off=move || !ag.with(|a| a.live)>
                <div class="ag-param">
                    <span class="cap">"Auto-switch"</span>
                    <span class="ag-auto">
                    <Toggle aria="Auto-switch"
                        on=Signal::derive(move || ag.with(|a| a.enabled))
                        enabled=Signal::derive(move || ag.with(|a| a.toggle_enabled) && !busy.get())
                        tip=Signal::derive(move || ag.with(|a| a.toggle_tip.clone()))
                        probe="ag-auto"
                        on_toggle=Callback::new(move |on: bool| page.send("antigravity-automatic", if on { "true" } else { "false" }))/>
                    <span class="q3">{move || if ag.with(|a| a.enabled) { "On" } else { "Off" }}</span>
                    </span>
                </div>
                <div class="ag-param">
                    <span class="cap">"Switch at % used"</span>
                    <Stepper reported=reported bounds=bounds enabled=Signal::derive(move || enabled.get() && !busy.get())
                        probe="ag-threshold"
                        send=Callback::new(move |v: i32| page.send("antigravity-threshold", &format!("{v}%")))/>
                </div>
                <div class="ag-param grow">
                    <span class="cap">"Watch pool"</span>
                    {move || {
                        let a = ag.get();
                        let on = a.pool_enabled && !busy.get();
                        if a.pools.is_empty() {
                            view! { <span class="ag-none q3">"No quota pool reported yet"</span> }.into_any()
                        } else {
                            view! { <Choices items=a.pools value=a.pool enabled=on send=Callback::new(move |v: String| page.send("antigravity-pool", &v))/> }.into_any()
                        }
                    }}
                </div>
                <div class="ag-param">
                    <span class="cap">"Cooldown"</span>
                    {move || {
                        let a = ag.get();
                        let on = a.cooldown_enabled && !busy.get();
                        view! { <Choices items=a.cooldowns value=a.cooldown enabled=on send=Callback::new(move |v: String| page.send("antigravity-cooldown", &v))/> }
                    }}
                </div>
            </div>
        </div>
    }
}

/// A segmented choice that marks the picked item at once and follows the saved value again when it changes.
#[component]
pub(crate) fn Choices(
    items: Vec<super::model::Choice>,
    value: String,
    enabled: bool,
    send: Callback<String>,
) -> impl IntoView {
    let picked = RwSignal::new(value);
    view! {
        <div class="seg ac-seg" class:off=!enabled role="radiogroup">
            {items.into_iter().map(|c| {
                let v = c.value.clone();
                let v2 = c.value.clone();
                view! {
                    <button type="button" role="radio" class:on=move || picked.get() == v
                        aria-checked=move || if picked.get() == v2 { "true" } else { "false" }
                        disabled=!enabled
                        on:click=move |_| {
                            if picked.get_untracked() != c.value {
                                picked.set(c.value.clone());
                                send.run(c.value.clone());
                            }
                        }>{c.label.clone()}</button>
                }
            }).collect_view()}
        </div>
    }
}

/// The "% used" stepper: [-] 95% [+]. Steps queue a value that is sent 600 ms after the last one; it shows until the
/// server reports it, or for 5 s.
#[component]
pub(crate) fn Stepper(
    #[prop(into)] reported: Signal<i32>,
    #[prop(into)] bounds: Signal<(i32, i32)>,
    #[prop(into)] enabled: Signal<bool>,
    probe: &'static str,
    send: Callback<i32>,
) -> impl IntoView {
    let pending = RwSignal::new(-1_i32);
    let serial = RwSignal::new(0_u64);
    Effect::new(move |_| {
        let r = reported.get();
        if pending.get_untracked() == r {
            pending.set(-1);
        }
    });
    let shown = move || {
        let p = pending.get();
        if p >= 0 { p } else { reported.get() }
    };
    let step = move |d: i32| {
        let (min, max) = bounds.get_untracked();
        let base = {
            let p = pending.get_untracked();
            if p >= 0 { p } else { reported.get_untracked() }
        };
        if base < 0 {
            return;
        }
        let next = (base + d).clamp(min, max);
        pending.set(next);
        serial.update(|s| *s += 1);
        let mine = serial.get_untracked();
        // plain browser timers: a reactive owner rebuilt meanwhile would cancel Leptos timers (NOTES.md)
        let fire = wasm_bindgen::closure::Closure::once_into_js(move || {
            if serial.try_get_untracked() != Some(mine) {
                return;
            }
            let value = pending.get_untracked();
            if value >= 0 {
                send.run(value);
                let release = wasm_bindgen::closure::Closure::once_into_js(move || {
                    if serial.try_get_untracked() == Some(mine) {
                        let _ = pending.try_set(-1);
                    }
                });
                let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
                    wasm_bindgen::JsCast::unchecked_ref(&release),
                    5000,
                );
            }
        });
        let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            wasm_bindgen::JsCast::unchecked_ref(&fire),
            600,
        );
    };
    view! {
        <span class="stepper" class:off=move || !enabled.get() data-probe=probe>
            <button type="button" class="st-btn" aria-label="Lower threshold"
                disabled={move || !enabled.get() || shown() <= bounds.get().0 || shown() < 0}
                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); step(-1); }><Icon name="minus"/></button>
            <span class="st-val tnum">{move || { let v = shown(); if v < 0 { "–".to_string() } else { format!("{v}%") } }}</span>
            <button type="button" class="st-btn" aria-label="Raise threshold"
                disabled={move || !enabled.get() || shown() >= bounds.get().1 || shown() < 0}
                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); step(1); }><Icon name="plus"/></button>
        </span>
    }
}
