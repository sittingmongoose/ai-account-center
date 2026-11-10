//! The target-bound switch approval (Slint `dashboard.slint` dialog), drawn by the frame over any tab. bridge.js keeps
//! the one-use token in `activation-confirmation.mjs` (and the Antigravity one) and sends only what to show through the
//! seam's `activation` view; `''` closes it. The two buttons send the dashboard's own `activation-confirm` and
//! `activation-cancel`; nothing here can approve a different target or reuse an approval.

use crate::api::nullable;
use crate::dash::DashCtx;
use leptos::prelude::*;
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActivationView {
    #[serde(deserialize_with = "nullable")]
    pub product: String,
    #[serde(deserialize_with = "nullable")]
    pub target_profile: String,
    #[serde(deserialize_with = "nullable")]
    pub warning: String,
    /// Already worded by bridge.js ("Review valid until ...").
    #[serde(deserialize_with = "nullable")]
    pub expires_at: String,
    #[serde(deserialize_with = "nullable")]
    pub error: String,
    pub can_confirm: bool,
    pub in_progress: bool,
    #[serde(deserialize_with = "nullable")]
    pub processes: Vec<Process>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Process {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub pid: Option<i64>,
    #[serde(deserialize_with = "nullable")]
    pub role: String,
}

/// The dialog's view, or none (closed, or a view that can't be read: then nothing can be approved from here).
pub fn parse(json: &str) -> Option<ActivationView> {
    if json.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<ActivationView>(json).ok()
}

impl ActivationView {
    /// Only the two products the dashboard asks about; anything else reads as Codex, as the Slint dialog does.
    pub fn product_name(&self) -> &'static str {
        if self.product == "Antigravity" {
            "Antigravity"
        } else {
            "Codex"
        }
    }
}

#[component]
pub fn ActivationDialog() -> impl IntoView {
    let dash = expect_context::<DashCtx>();
    let view_m = Memo::new(move |_| dash.activation.with(|j| parse(j)));
    let open = Memo::new(move |_| view_m.with(|v| v.is_some()));
    let busy = Memo::new(move |_| view_m.with(|v| v.as_ref().is_some_and(|v| v.in_progress)));
    let cancel = move || {
        if !busy.get_untracked() {
            dash.act("activation-cancel", "");
        }
    };
    let esc = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && open.get_untracked() {
            cancel();
        }
    });
    on_cleanup(move || esc.remove());
    // The dialog takes the focus when it opens, so the keyboard and screen readers land on it.
    Effect::new(move |_| {
        if open.get() {
            let focus = wasm_bindgen::closure::Closure::once_into_js(|| {
                if let Some(el) = document()
                    .query_selector(".adialog .btn.primary, .adialog .btn")
                    .ok()
                    .flatten()
                    .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok())
                {
                    let _ = el.focus();
                }
            });
            let _ = window().request_animation_frame(wasm_bindgen::JsCast::unchecked_ref(&focus));
        }
    });
    move || {
        view_m.get().map(|v| {
            let product = v.product_name();
            let status = if v.in_progress {
                Some((format!("Stopping the listed processes and restoring {product}…"), false))
            } else if !v.error.is_empty() {
                Some((v.error.clone(), true))
            } else {
                None
            };
            let can = v.can_confirm && !v.in_progress;
            let keep_hint = format!("Keep every {product} process and account unchanged");
            let go_hint = format!("Stop the listed processes and switch the {product} account");
            view! {
                <div class="ascrim" on:click=move |_| cancel()>
                    <div class="adialog" role="dialog" aria-modal="true" aria-labelledby="adialog-title"
                        on:click=|ev| ev.stop_propagation()>
                        <h2 id="adialog-title">{format!("Another {product} process is running")}</h2>
                        <p class="a-target">{format!("Switch to {}?", v.target_profile)}</p>
                        <p class="a-warn">{v.warning.clone()}</p>
                        <ul class="a-procs">
                            {if v.processes.is_empty() {
                                view! { <li class="q3">"No running processes were listed."</li> }.into_any()
                            } else {
                                v.processes.iter().map(|p| {
                                    let pid = p.pid.map(|n| n.to_string()).unwrap_or_else(|| "–".into());
                                    let role = if p.role.is_empty() { String::new() } else { format!(" · {}", p.role) };
                                    view! { <li><b>{p.label.clone()}</b><span class="tnum">{format!("PID {pid}{role}")}</span></li> }
                                }).collect_view().into_any()
                            }}
                        </ul>
                        <p class="a-exp tnum">{v.expires_at.clone()}</p>
                        {status.map(|(text, bad)| view! { <p class="a-status" class:w-crit=bad>{text}</p> })}
                        <div class="a-btns">
                            <button class="btn" title=keep_hint aria-disabled=v.in_progress.to_string()
                                on:click=move |_| cancel()>"Cancel"</button>
                            <button class="btn primary" title=go_hint aria-disabled=(!can).to_string()
                                on:click=move |_| { if can { dash.act("activation-confirm", ""); } }>"Yes, stop and restart"</button>
                        </div>
                    </div>
                </div>
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_reads_the_bridge_prompt_and_closes_on_empty() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("{not json"), None);
        let v = parse(
            r#"{"product":"Codex","targetProfile":"two","expiresAt":"Review valid until 5:00 PM","processes":[{"label":"codex","pid":42,"role":"cli"}],
                "warning":"Active Codex work will stop.","canConfirm":true,"inProgress":false,"error":""}"#,
        )
        .unwrap();
        assert_eq!(v.target_profile, "two");
        assert_eq!(v.processes[0].pid, Some(42));
        assert!(v.can_confirm && !v.in_progress);
        assert_eq!(v.product_name(), "Codex");
        let agy =
            parse(r#"{"product":"Antigravity","targetProfile":"x","processes":null}"#).unwrap();
        assert_eq!(agy.product_name(), "Antigravity");
        assert!(agy.processes.is_empty());
        assert!(
            !agy.can_confirm,
            "a prompt that doesn't say it can be confirmed can't be"
        );
    }
}
