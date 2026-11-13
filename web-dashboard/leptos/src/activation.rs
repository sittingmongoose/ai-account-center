//! The target-bound switch approval (Slint `dashboard.slint` dialog), drawn by the frame over any tab. bridge.js keeps
//! the one-use token in `activation-confirmation.mjs` (and the Antigravity one) and sends only what to show through the
//! seam's `activation` view; `''` closes it. The two buttons send the dashboard's own `activation-confirm` and
//! `activation-cancel`; nothing here can approve a different target or reuse an approval.

use crate::api::nullable;
use crate::dash::DashCtx;
use crate::ui::OverlayPortal;
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
    let can = Memo::new(move |_| {
        view_m.with(|v| v.as_ref().is_some_and(|v| v.can_confirm && !v.in_progress))
    });
    let cancel = move || {
        if !busy.get_untracked() {
            dash.act("activation-cancel", "");
        }
    };
    let close = Callback::new(move |_| cancel());
    // Leaving the approval's page retires this opening (OverlayPortal). A request already running can't be cancelled,
    // so the retired dialog stays hidden until the request settles and is then cancelled: its late result (a failure,
    // a fresh review) updates the same hidden opening and can't bring a modal back over another page (Astra/Sol max
    // 6). The next Activate opens a fresh one.
    let retired = RwSignal::new(false);
    let on_retire = Callback::new(move |_| retired.set(true));
    Effect::new(move |_| {
        if !open.get() {
            if retired.get_untracked() {
                retired.set(false);
            }
        } else if retired.get() && !busy.get() {
            dash.act("activation-cancel", "");
        }
    });
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
    let read = move |f: fn(&ActivationView) -> String| {
        move || view_m.with(|v| v.as_ref().map(f).unwrap_or_default())
    };
    let status = move || {
        view_m.with(|v| {
            let v = v.as_ref()?;
            if v.in_progress {
                Some((
                    format!(
                        "Stopping the listed processes and restoring {}…",
                        v.product_name()
                    ),
                    false,
                ))
            } else if !v.error.is_empty() {
                Some((v.error.clone(), true))
            } else {
                None
            }
        })
    };
    let processes = move || {
        let list = view_m.with(|v| v.as_ref().map(|v| v.processes.clone()).unwrap_or_default());
        if list.is_empty() {
            view! { <li class="q3">"No running processes were listed."</li> }.into_any()
        } else {
            list.into_iter()
                .map(|p| {
                    let pid = p.pid.map(|n| n.to_string()).unwrap_or_else(|| "–".into());
                    let role = if p.role.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", p.role)
                    };
                    view! { <li><b>{p.label}</b><span class="tnum">{format!("PID {pid}{role}")}</span></li> }
                })
                .collect_view()
                .into_any()
        }
    };
    // One portal for one opening: status and error updates change its words in place, never its lifetime, lock or
    // focus. Only closed -> open makes a new one.
    move || {
        open.get().then(|| {
            view! {
                <OverlayPortal on_close=close on_retire=on_retire>
                <div class="ascrim" on:click=move |_| cancel()>
                    <div class="adialog" role="dialog" aria-modal="true" aria-labelledby="adialog-title"
                        on:click=|ev| ev.stop_propagation()>
                        <h2 id="adialog-title">{read(|v| format!("Another {} process is running", v.product_name()))}</h2>
                        <p class="a-target">{read(|v| format!("Switch to {}?", v.target_profile))}</p>
                        <p class="a-warn">{read(|v| v.warning.clone())}</p>
                        <ul class="a-procs">{processes}</ul>
                        <p class="a-exp tnum">{read(|v| v.expires_at.clone())}</p>
                        {move || status().map(|(text, bad)| view! { <p class="a-status" class:w-crit=bad>{text}</p> })}
                        <div class="a-btns">
                            <button class="btn" title=read(|v| format!("Keep every {} process and account unchanged", v.product_name()))
                                aria-disabled=move || busy.get().to_string()
                                on:click=move |_| cancel()>"Cancel"</button>
                            <button class="btn primary" title=read(|v| format!("Stop the listed processes and switch the {} account", v.product_name()))
                                aria-disabled=move || (!can.get()).to_string()
                                on:click=move |_| { if can.get_untracked() { dash.act("activation-confirm", ""); } }>"Yes, stop and restart"</button>
                        </div>
                    </div>
                </div>
                </OverlayPortal>
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
