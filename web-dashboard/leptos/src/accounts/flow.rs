//! The flow panel under a provider (Slint `FlowPanel`; words and states from `flowView` in accounts-view.mjs): the
//! step bar, title and body, one field (a profile name, an email, a masked API key with an optional label, a pasted
//! code, or DELETE), the verification page and code, the waiting line, success or the error, and its buttons.
//!
//! What is typed stays here until it is submitted: `flow-submit` carries "<provider>\n<field>\n<label>" once, and a key
//! or a code field is cleared at once, so the page never keeps it. A new flow (its key) starts from the suggested
//! value; a step that asks for a different kind of input starts from its own.

use super::Page;
use super::actions::ActionControl;
use super::model::{Action, Flow};
use crate::ui::*;
use leptos::prelude::*;

#[component]
pub fn FlowPanel(page: Page, provider: String, flow: Memo<super::model::Flow>) -> impl IntoView {
    let input = RwSignal::new(String::new());
    let label = RwSignal::new(String::new());
    let key = Memo::new(move |_| flow.with(|f| f.key.clone()));
    let kind = Memo::new(move |_| flow.with(|f| f.input_kind.clone()));
    // A new flow starts from its suggested value with an empty label.
    Effect::new(move |prev: Option<String>| {
        let k = key.get();
        if prev.as_ref() != Some(&k) {
            input.set(flow.with_untracked(|f| f.input_seed.clone()));
            label.set(String::new());
        }
        k
    });
    // A step that asks for another kind of input starts from its own seed (the supervised Antigravity flow asks a
    // name, then a code).
    Effect::new(move |prev: Option<String>| {
        let k = kind.get();
        if prev.is_some() && prev.as_ref() != Some(&k) {
            input.set(flow.with_untracked(|f| f.input_seed.clone()));
        }
        k
    });
    let busy = page.busy;
    // Enter in a field goes through the same check as the button: the flow's submit action enabled, and not busy
    // unless that action is the one running (review L2).
    let can_submit = move || {
        flow.with_untracked(|f| {
            f.actions
                .iter()
                .find(|a| a.act == "flow-submit")
                .is_some_and(|a| a.enabled && (!busy.get_untracked() || a.busy))
        })
    };
    let prov = provider.clone();
    let submit = Callback::new(move |_: ()| {
        page.send(
            "flow-submit",
            &format!(
                "{prov}\n{}\n{}",
                input.get_untracked(),
                label.get_untracked()
            ),
        );
        // a key or a code is sent once and never kept here
        let secret = flow.with_untracked(|f| f.input_password || f.input_kind == "code");
        if secret {
            input.set(String::new());
        }
    });
    let open = Memo::new(move |_| flow.with(|f| f.open));
    let steps = Memo::new(move |_| flow.with(|f| (f.steps.clone(), f.cur)));
    let field_kind = Memo::new(move |_| {
        flow.with(|f| {
            (
                f.input_kind.clone(),
                f.input_label.clone(),
                f.input_placeholder.clone(),
                f.input_password,
                f.label_field,
            )
        })
    });
    let bad = Memo::new(move |_| flow.with(|f| !f.error.is_empty()));
    let name = StoredValue::new(provider);
    view! {
        <Show when=move || open.get()>
            <div class="flow" data-probe=move || format!("flow:{}", name.get_value()) role="group"
                aria-label=move || flow.with(|f| f.title.clone())>
                {move || {
                    let (steps, cur) = steps.get();
                    (!steps.is_empty()).then(|| view! { <Steps steps=steps cur=cur/> })
                }}
                {move || flow.with(|f| (!f.title.is_empty()).then(|| view! { <h3 class="flow-title">{f.title.clone()}</h3> }))}
                {move || flow.with(|f| (!f.done.is_empty()).then(|| view! {
                    <p class="flow-done w-good"><Icon name="check-circle"/><span>{f.done.clone()}</span></p>
                }))}
                {move || flow.with(|f| (!f.body.is_empty()).then(|| view! { <p class="flow-body">{f.body.clone()}</p> }))}
                {move || flow.with(|f| f.code_shown.then(|| code_box(f, &name.get_value())))}
                <Show when=move || field_kind.with(|k| !k.0.is_empty())>
                    <div class="flow-fields">
                        <label class="field grow">
                            <span class="field-label">{move || field_kind.with(|k| k.1.clone())}</span>
                            <input class="field-input" class:bad=move || bad.get()
                                type=move || match field_kind.with(|k| (k.0.clone(), k.3)) {
                                    (_, true) => "password",
                                    (k, _) if k == "email" => "email",
                                    _ => "text",
                                }
                                autocomplete=move || match field_kind.with(|k| (k.0.clone(), k.3)) {
                                    (_, true) => "off",
                                    (k, _) if k == "code" => "one-time-code",
                                    (k, _) if k == "email" => "email",
                                    _ => "off",
                                }
                                autocapitalize="off" spellcheck="false"
                                placeholder=move || field_kind.with(|k| k.2.clone())
                                disabled=move || busy.get()
                                data-probe=move || format!("flow-input:{}", name.get_value())
                                prop:value=move || input.get()
                                on:input=move |e| input.set(event_target_value(&e))
                                on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" { e.prevent_default(); if can_submit() { submit.run(()); } }/>
                        </label>
                        <Show when=move || field_kind.with(|k| k.4)>
                            <label class="field label-field">
                                <span class="field-label">"Label (optional)"</span>
                                <input class="field-input" type="text" placeholder="Work" autocomplete="off"
                                    disabled=move || busy.get()
                                    prop:value=move || label.get()
                                    on:input=move |e| label.set(event_target_value(&e))
                                    on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" { e.prevent_default(); if can_submit() { submit.run(()); } }/>
                            </label>
                        </Show>
                    </div>
                </Show>
                {move || flow.with(|f| (!f.waiting.is_empty()).then(|| view! {
                    <p class="flow-wait"><Spinner/><span>{f.waiting.clone()}</span></p>
                }))}
                {move || flow.with(|f| (!f.error.is_empty()).then(|| view! {
                    <div class="flow-error"><Icon name="alert-circle"/><p><b class="w-crit">{f.error.clone()}</b>{(!f.error_body.is_empty()).then(|| view! { " "{f.error_body.clone()} })}</p></div>
                }))}
                {move || {
                    let actions: Vec<Action> = flow.with(|f| f.actions.clone());
                    let n = actions.len();
                    (n > 0).then(|| view! {
                        <div class="flow-acts" class:two=n == 2>
                            {actions.into_iter().map(|a| {
                                if a.act == "flow-submit" {
                                    let enabled = a.enabled;
                                    let busy_btn = a.busy;
                                    let class = format!("btn {}", if a.style == "danger-solid" { "danger-solid" } else if a.style == "primary" { "primary" } else { "" });
                                    view! {
                                        <button type="button" class=class data-probe=a.probe.clone()
                                            aria-disabled=move || if enabled && (!busy.get() || busy_btn) { "false" } else { "true" }
                                            on:click=move |_| if enabled && (!busy.get_untracked() || busy_btn) { submit.run(()) }>
                                            {busy_btn.then(|| view! { <Spinner/> })}<span>{a.label.clone()}</span>
                                        </button>
                                    }.into_any()
                                } else {
                                    // Opening the verification page and copying the code need this tap's user activation, which a
                                    // message to the dashboard's hidden page doesn't carry: the frame does them itself, the same
                                    // way (an https page only; the code on screen). The terminal command is copied by the dashboard.
                                    let (url, code) = flow.with_untracked(|f| (f.code_url.clone(), f.code_text.clone()));
                                    if a.act == "flow-open-url" && url.starts_with("https://") {
                                        let label = a.label.clone();
                                        let class = format!("btn sm {}", if a.style == "primary" { "primary" } else { "" });
                                        view! {
                                            <button type="button" class=class data-probe=a.probe.clone()
                                                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); open_page(&url); }>
                                                <Icon name="external"/><span>{label}</span>
                                            </button>
                                        }.into_any()
                                    } else if a.act == "flow-copy" && !code.is_empty() {
                                        let label = a.label.clone();
                                        view! {
                                            <button type="button" class="btn sm" data-probe=a.probe.clone()
                                                on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); copy_code(page, &code); }>
                                                <Icon name="copy"/><span>{label}</span>
                                            </button>
                                        }.into_any()
                                    } else {
                                        view! { <ActionControl a=a page=page/> }.into_any()
                                    }
                                }
                            }).collect_view()}
                        </div>
                    })
                }}
                {move || flow.with(|f| (!f.note.is_empty()).then(|| view! {
                    <p class="flow-note q3"><Icon name="lock-open"/><span>{f.note.clone()}</span></p>
                }))}
            </div>
        </Show>
    }
}

/// The step bar: one segment per step, done and current in the accent, with the current step's name (every name when
/// there is room).
#[component]
fn Steps(steps: Vec<String>, cur: u32) -> impl IntoView {
    let cur = cur as usize;
    let current = steps.get(cur).cloned().unwrap_or_default();
    let n = steps.len();
    view! {
        <div class="steps" style=format!("--steps:{n}")>
            <div class="steps-bar">
                {(0..n).map(|i| view! { <span class="seg-step" class:done=i < cur class:cur=i == cur></span> }).collect_view()}
            </div>
            <div class="steps-names">
                {steps.into_iter().enumerate().map(|(i, s)| view! {
                    <span class="step-name" class:done=i < cur class:cur=i == cur>{format!("{} {}", i + 1, s)}</span>
                }).collect_view()}
            </div>
            <div class="steps-current">{current}</div>
        </div>
    }
}

/// The verification page and the code (device-code and supervised sign-ins).
fn code_box(f: &Flow, provider: &str) -> AnyView {
    view! {
        <div class="code-box" data-probe=format!("flow-code:{provider}")>
            <div class="cb-page">
                <span class="cap">"Verification page"</span>
                <span class="cb-url">{f.code_url.clone()}</span>
                {(!f.code_expires.is_empty()).then(|| view! { <span class="cb-exp tnum q3">{f.code_expires.clone()}</span> })}
            </div>
            {(!f.code_text.is_empty()).then(|| view! {
                <div class="cb-code"><span class="cap">"Code"</span><span class="cb-text">{f.code_text.clone()}</span></div>
            })}
        </div>
    }
    .into_any()
}

/// Opens the sign-in job's verification page in a new tab (accounts-controller.mjs `flow-open-url`: https only).
fn open_page(url: &str) {
    if url.starts_with("https://") {
        let _ = window().open_with_url_and_target_and_features(url, "_blank", "noopener");
    }
}

/// Copies the code on screen, with the words accounts-controller.mjs `flow-copy` uses.
fn copy_code(page: Page, code: &str) {
    use wasm_bindgen::JsCast;
    let ctx = page.ctx;
    let said = move |ok: bool| {
        if ok {
            ctx.toast(
                "ok",
                "Code copied",
                "Paste it on the verification page.",
                4800,
            );
        } else {
            ctx.toast(
                "info",
                "Copy the code by hand",
                "This browser did not allow copying.",
                4800,
            );
        }
    };
    let navigator = window().navigator();
    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into())
        .unwrap_or(wasm_bindgen::JsValue::UNDEFINED);
    let write = js_sys::Reflect::get(&clipboard, &"writeText".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    if let Some(promise) = write
        .and_then(|f| f.call1(&clipboard, &code.into()).ok())
        .and_then(|p| p.dyn_into::<js_sys::Promise>().ok())
    {
        let fallback = code.to_string();
        leptos::task::spawn_local(async move {
            let ok = wasm_bindgen_futures::JsFuture::from(promise).await.is_ok();
            said(ok || copy_by_selection(&fallback));
        });
    } else {
        said(copy_by_selection(code));
    }
}

/// The older way to copy, for browsers without the clipboard API: a hidden field, selected and copied.
fn copy_by_selection(text: &str) -> bool {
    use wasm_bindgen::JsCast;
    let doc = document();
    let Ok(area) = doc.create_element("textarea") else {
        return false;
    };
    let _ = area.set_attribute("readonly", "");
    let _ = area.set_attribute("style", "position:fixed;opacity:0");
    area.set_text_content(Some(text));
    let Some(body) = doc.body() else { return false };
    let _ = body.append_child(&area);
    if let Ok(select) =
        js_sys::Reflect::get(&area, &"select".into()).and_then(|f| f.dyn_into::<js_sys::Function>())
    {
        let _ = select.call0(&area);
    }
    let ok = js_sys::Reflect::get(&doc, &"execCommand".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call1(&doc, &"copy".into()).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    area.remove();
    ok
}
