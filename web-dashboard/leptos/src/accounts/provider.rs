//! One provider card (Slint `ProviderSection`): the head with its mark, kind line, count and the provider's Show on
//! dashboard / Show in tray switches; one row per account; the Claude trash; the flow panel; and the footer with how the provider signs in and its add or check actions.
//!
//! A row lays out by the card's own width (container queries in app.css): a table row with every column on wide
//! cards, two lines on medium ones, and three on narrow ones (the remove control lifted beside the name). The action
//! slots sit right-aligned, so a longer status never moves them.

use super::Page;
use super::actions::{ActionControl, ReasonLine, RowLine, icon_name};
use super::flow::FlowPanel;
use super::model::{Action, Provider, Row};
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
            <div class="acc-rows">
                <For each=move || row_ids.get() key=|id| id.clone() children=move |rid| {
                    let row = Memo::new(move |_| prov.with(|p| p.as_ref().and_then(|p| p.rows.iter().find(|r| r.id == rid).cloned())));
                    view! { <RowItem page=page row=row/> }
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

/// A row's actions as the server view's Central Logins state draws them. With step 3 on, AAC's own Codex
/// device-code sign-in is not offered: the account signs in through T3 on this computer, and Remove stays where it was.
fn shown_actions(row: &Row, logins_step3: bool) -> Vec<Action> {
    let actions = row.actions.clone();
    if !logins_step3 || row.provider != "codex" {
        return actions;
    }
    let mut replaced = false;
    actions
        .into_iter()
        .map(|action| {
            if action.act == "signin-again" && !replaced {
                replaced = true;
                Action {
                    kind: "quiet".into(),
                    label: "Signs in through T3 on this computer".into(),
                    ..Action::default()
                }
            } else {
                action
            }
        })
        .collect()
}

/// One account row with its lines: the remove / restore / refusal line, and the reason a tap on an off control
/// opened.
#[component]
fn RowItem(page: Page, row: Memo<Option<Row>>) -> impl IntoView {
    let reason = RwSignal::new(String::new());
    let get = move || row.get().unwrap_or_default();
    let actions = Memo::new(move |_| {
        let step3 = page
            .view
            .with(|view| view.as_ref().is_some_and(|view| view.logins_step3));
        row.with(|row| {
            row.as_ref()
                .map(|row| shown_actions(row, step3))
                .unwrap_or_default()
        })
    });
    let id = Memo::new(move |_| row.with(|r| r.as_ref().map(|r| r.id.clone()).unwrap_or_default()));
    let line = Signal::derive(move || {
        row.with(|r| r.as_ref().map(|r| r.line.clone()).unwrap_or_default())
    });
    let selected = Memo::new(move |_| {
        let id = id.get();
        !id.is_empty() && page.selected.get() == id
    });
    let set_reason = Callback::new(move |r: String| reason.set(r));
    // The inline rename: the pencil beside the name opens an input in the same place; Enter saves, Esc cancels.
    let editing = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let rename_error = RwSignal::new(String::new());
    let can_rename = Memo::new(move |_| row.with(|r| r.as_ref().is_some_and(|r| r.can_rename)));
    let gone = Memo::new(move |_| row.with(|r| r.as_ref().is_some_and(|r| r.gone)));
    let open_rename = Callback::new(move |e: web_sys::MouseEvent| {
        e.stop_propagation();
        reason.set(String::new());
        let r = get();
        draft.set(if r.label.is_empty() {
            r.email.clone()
        } else {
            r.label.clone()
        });
        rename_error.set(String::new());
        editing.set(true);
    });
    let save_rename = Callback::new(move |_: ()| {
        let trimmed = draft.get_untracked().trim().to_string();
        if trimmed.is_empty() {
            rename_error.set("Name the account first.".to_string());
            return;
        }
        if trimmed.chars().count() > 48 {
            rename_error.set("Keep it within 48 characters.".to_string());
            return;
        }
        rename_error.set(String::new());
        editing.set(false);
        page.send(
            "account-rename",
            &format!("{}\n{}", id.get_untracked(), trimmed),
        );
    });
    let cancel_rename = Callback::new(move |e: web_sys::MouseEvent| {
        e.stop_propagation();
        rename_error.set(String::new());
        editing.set(false);
    });
    view! {
        <div class="acc-item" class:sel=move || selected.get()
            class:gone=move || row.with(|r| r.as_ref().is_some_and(|r| r.gone))
            data-row=move || id.get()>
            <div class="acc-row" role="button" tabindex="0"
                aria-label=move || format!("{}: details", row.with(|r| r.as_ref().map(|r| r.email.clone()).unwrap_or_default()))
                data-probe=move || format!("row:{}", id.get())
                on:click=move |_| { reason.set(String::new()); page.send("details", &id.get_untracked()); }
                on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" { page.send("details", &id.get_untracked()); }>
                <div class="acc-id" class:dim=move || row.with(|r| r.as_ref().is_some_and(|r| !r.shown_dash))>
                    <Show when=move || editing.get()>
                        <span class="acc-rename" on:click=|e: web_sys::MouseEvent| e.stop_propagation()>
                            <input class="acc-rename-input" type="text" placeholder="Name" autocomplete="off"
                                autocapitalize="off" spellcheck="false" maxlength="48" autofocus
                                data-probe=move || format!("rename-input:{}", id.get())
                                disabled=move || page.busy.get()
                                prop:value=move || draft.get()
                                on:input=move |e| { draft.set(event_target_value(&e)); rename_error.set(String::new()); }
                                on:keydown=move |e: web_sys::KeyboardEvent| {
                                    // keys typed here belong to the field: Enter on the row opens Details
                                    e.stop_propagation();
                                    if e.key() == "Enter" {
                                        e.prevent_default();
                                        if !page.busy.get_untracked() { save_rename.run(()); }
                                    } else if e.key() == "Escape" {
                                        e.prevent_default();
                                        rename_error.set(String::new());
                                        editing.set(false);
                                    }
                                }/>
                            <button type="button" class="btn sm" data-probe=move || format!("rename-save:{}", id.get())
                                disabled=move || page.busy.get()
                                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); if !page.busy.get_untracked() { save_rename.run(()); } }>"Save"</button>
                            <button type="button" class="btn sm ghost" data-probe=move || format!("rename-cancel:{}", id.get())
                                on:click=move |e: web_sys::MouseEvent| cancel_rename.run(e)>"Cancel"</button>
                        </span>
                        {move || { let e = rename_error.get(); (!e.is_empty()).then(|| view! { <span class="acc-rename-err" role="note">{e}</span> }) }}
                    </Show>
                    <Show when=move || !editing.get()>
                        <span class="acc-name-line">
                            <span class="acc-email" title=move || get().email>{move || email_parts(get().email)}</span>
                            {move || (can_rename.get() && !gone.get()).then(|| view! {
                                <button type="button" class="icon-btn bare acc-rename-btn"
                                    aria-label="Rename account" title="Rename"
                                    data-probe=move || format!("rename:{}", id.get())
                                    disabled=move || page.busy.get()
                                    on:click=move |e: web_sys::MouseEvent| open_rename.run(e)>
                                    <Icon name="pencil"/>
                                </button>
                            })}
                        </span>
                    </Show>
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
                        let acts = actions.get();
                        let n = acts.len();
                        // the remove control has its own cell (beside the name on narrow cards)
                        acts.iter().cloned().enumerate().filter(|(i, a)| !(i + 1 == n && a.icon == "trash"))
                            .map(|(_, a)| view! { <ActionControl a=a page=page reason=set_reason/> })
                            .collect_view()
                    }}
                </div>
                <div class="acc-trash">
                    {move || {
                        actions.with(|acts| acts.last().filter(|a| a.icon == "trash").cloned().map(|a| view! { <ActionControl a=a page=page reason=set_reason/> }))
                    }}
                </div>
            </div>
            <RowLine line=line page=page/>
            <ReasonLine reason=reason/>
        </div>
    }
}

/// An email with a break chance after its `@`. The Accounts name line keeps it on one line and ends it in an ellipsis
/// when it must (its full text is the title); other places may break it there.
fn email_parts(email: String) -> impl IntoView {
    match email.split_once('@') {
        Some((local, domain)) => {
            view! { {local.to_string()}<wbr/>{format!("@{domain}")} }.into_any()
        }
        None => view! { {email} }.into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::model::Action;

    fn signin() -> Action {
        Action {
            act: "signin-again".into(),
            value: "codex:one".into(),
            label: "Sign in again".into(),
            icon: "login".into(),
            enabled: true,
            probe: "signin-again:codex:one".into(),
            ..Action::default()
        }
    }

    fn remove() -> Action {
        Action {
            kind: "icon".into(),
            act: "remove".into(),
            value: "codex:one".into(),
            icon: "trash".into(),
            enabled: true,
            probe: "remove:codex:one".into(),
            ..Action::default()
        }
    }

    #[test]
    fn step_3_replaces_a_codex_sign_in_with_the_t3_note() {
        let row = Row {
            provider: "codex".into(),
            actions: vec![signin(), remove()],
            ..Row::default()
        };
        let shown = shown_actions(&row, true);
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].kind, "quiet");
        assert_eq!(shown[0].label, "Signs in through T3 on this computer");
        assert!(!shown.iter().any(|a| a.act == "signin-again"));
        assert_eq!(shown[1].icon, "trash");

        let unchanged = shown_actions(&row, false);
        assert_eq!(unchanged[0].act, "signin-again");
        assert_eq!(unchanged[0].label, "Sign in again");
    }

    #[test]
    fn step_3_never_rewrites_another_provider_or_a_missing_action() {
        let antigravity = Row {
            provider: "antigravity".into(),
            actions: vec![signin(), remove()],
            ..Row::default()
        };
        assert_eq!(shown_actions(&antigravity, true)[0].act, "signin-again");

        let codex = Row {
            provider: "codex".into(),
            actions: vec![remove()],
            ..Row::default()
        };
        let shown = shown_actions(&codex, true);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].icon, "trash");
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
