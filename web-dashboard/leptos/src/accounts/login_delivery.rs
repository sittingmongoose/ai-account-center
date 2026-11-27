//! Static logins (Central Logins step 2): deliver a Claude `setup-token` token or an API key to the computers the
//! owner ticks, copy one to the rest, replace or remove it. **Write-only:** the secret is typed into a password field,
//! sent once in one request body and cleared from the page the moment it is sent; nothing the server answers carries
//! it (`aac_api::login_delivery` has no field for one), so it can never be shown again. Every change is confirmed on a
//! second step that names what will happen and the fingerprint the page showed.
use crate::api::{FetchError, send};
use aac_api::login_delivery::{
    DELIVERY_VIEW_VERSION, DeliveredLogin, DeliveryReceipt, DeliveryView,
};
use leptos::prelude::*;
use serde_json::json;

pub(crate) const PATH: &str = "/api/accounts/logins/delivery";

/// The form's fields. The secret lives here only until [`Form::take_body`] sends it; `Debug` never prints it.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Form {
    /// A replace keeps the login's id and the fingerprint the page showed.
    pub login_id: Option<String>,
    /// For a replace: what the page showed on each computer that holds this login (its fingerprint).
    pub shown: Vec<(String, String)>,
    pub kind: String,
    pub provider: String,
    pub account: String,
    pub secret: String,
    /// `YYYY-MM-DD` or empty.
    pub expires_on: String,
    pub computers: Vec<String>,
}

impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Form")
            .field("login_id", &self.login_id)
            .field("kind", &self.kind)
            .field("account", &self.account)
            .field(
                "secret",
                &if self.secret.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field("computers", &self.computers)
            .finish()
    }
}

/// `[{"computer":..,"expect":..}]`.
fn targets(list: &[(String, String)]) -> serde_json::Value {
    serde_json::Value::Array(
        list.iter()
            .map(|(c, e)| json!({"computer": c, "expect": e}))
            .collect(),
    )
}

impl Form {
    /// Ready to review: every field the server needs.
    pub(crate) fn ready(&self) -> bool {
        !self.kind.is_empty()
            && !self.account.is_empty()
            && !self.provider.is_empty()
            && (8..=512).contains(&self.secret.trim().len())
            && !self.computers.is_empty()
    }

    /// Each ticked computer with what the page showed there: the copy it holds, else `absent`.
    pub(crate) fn expects(&self) -> Vec<(String, String)> {
        self.computers
            .iter()
            .map(|c| {
                let shown = self
                    .shown
                    .iter()
                    .find(|(id, _)| id == c)
                    .map(|(_, fp)| fp.clone());
                (c.clone(), shown.unwrap_or_else(|| "absent".into()))
            })
            .collect()
    }

    /// The request body, built once: the secret is moved out of the form, so the page no longer holds it.
    pub(crate) fn take_body(&mut self) -> String {
        let secret = std::mem::take(&mut self.secret);
        let confirm = if self.login_id.is_some() {
            "replace"
        } else {
            "deliver"
        };
        json!({
            "loginId": self.login_id,
            "kind": self.kind,
            "provider": self.provider,
            "account": self.account,
            "secret": secret.trim(),
            "expiresOn": (!self.expires_on.is_empty()).then_some(&self.expires_on),
            "targets": targets(&self.expects()),
            "confirm": confirm,
        })
        .to_string()
    }
}

/// Computers that can be written now: enrolled ones.
pub(crate) fn writable(computers: &[aac_api::login_delivery::DeliveryComputer]) -> Vec<String> {
    computers
        .iter()
        .filter(|c| c.state == "enrolled")
        .map(|c| c.id.clone())
        .collect()
}

/// The body that copies the current value from a computer holding it to every enrolled computer that lacks it or
/// holds another value, naming what each one holds. `None` when no computer holds the current value or none needs it.
pub(crate) fn copy_body(login: &DeliveredLogin, computers: &[String]) -> Option<String> {
    if login.fingerprint.is_empty() {
        return None;
    }
    let from = login.holders.iter().find(|h| h.state == "ok")?;
    let to = missing(login, computers);
    (!to.is_empty()).then(|| {
        json!({"loginId": login.login_id,
            "from": {"computer": from.computer, "fingerprint": from.fingerprint},
            "to": targets(&to), "confirm": "copy"})
        .to_string()
    })
}

/// The body that removes a login from every computer where the page knows its value.
pub(crate) fn remove_body(login: &DeliveredLogin) -> Option<String> {
    let list: Vec<(String, String)> = login
        .holders
        .iter()
        .filter(|h| matches!(h.state.as_str(), "ok" | "differs") && !h.fingerprint.is_empty())
        .map(|h| (h.computer.clone(), h.fingerprint.clone()))
        .collect();
    (!list.is_empty()).then(|| {
        json!({"loginId": login.login_id, "targets": targets(&list), "confirm": "remove"})
            .to_string()
    })
}

/// Enrolled computers that do not hold the current value, with what each holds (`absent` when nothing). A computer
/// whose file cannot be read, or that holds another account, is left out: nothing can be confirmed there.
pub(crate) fn missing(login: &DeliveredLogin, computers: &[String]) -> Vec<(String, String)> {
    computers
        .iter()
        .filter_map(|c| match login.holders.iter().find(|h| &h.computer == c) {
            None => Some((c.clone(), "absent".to_owned())),
            Some(h) if h.state == "differs" && !h.fingerprint.is_empty() => {
                Some((c.clone(), h.fingerprint.clone()))
            }
            Some(_) => None,
        })
        .collect()
}

/// A fingerprint in short: its first 8 hex digits.
pub(crate) fn short(fp: &str) -> &str {
    fp.strip_prefix("sha256:")
        .and_then(|h| h.get(..8))
        .unwrap_or(fp)
}

/// "Mac ok · Nas1 differs 00112233": where it is, in words.
pub(crate) fn holders_line(login: &DeliveredLogin, labels: &[(String, String)]) -> String {
    if login.holders.is_empty() {
        return "On no computer".into();
    }
    login
        .holders
        .iter()
        .map(|h| {
            let name = labels
                .iter()
                .find(|(id, _)| id == &h.computer)
                .map_or(h.computer.as_str(), |(_, label)| label.as_str());
            let state = match h.state.as_str() {
                "ok" => "ok".to_owned(),
                "differs" => format!("other value {}", short(&h.fingerprint)),
                "unchecked" => "not checked".into(),
                "unreadable" => "unreadable file".into(),
                "other-account" => "another account".into(),
                "wrong-computer" => "another computer answered".into(),
                _ => "unknown".into(),
            };
            format!("{name} {state}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The receipt in short lines: "nas1: Delivered".
pub(crate) fn receipt_lines(receipt: &DeliveryReceipt, labels: &[(String, String)]) -> Vec<String> {
    receipt
        .results
        .iter()
        .map(|r| {
            let name = labels
                .iter()
                .find(|(id, _)| id == &r.computer)
                .map_or(r.computer.as_str(), |(_, label)| label.as_str());
            format!("{name}: {}", r.text)
        })
        .collect()
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "claude-token" => "Claude token",
        "api-key" => "API key",
        _ => "Static login",
    }
}

/// The server's own sentence for a refusal (fixed words; it never echoes the request), else its status.
pub(crate) fn refusal(status: u16, text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_owned))
        .filter(|m| !m.trim().is_empty() && m.len() <= 300)
        .unwrap_or_else(|| format!("The server answered {status}."))
}

pub(crate) fn parse_view(text: &str) -> Result<DeliveryView, FetchError> {
    let view: DeliveryView = serde_json::from_str(text).map_err(|_| FetchError::Malformed)?;
    if view.version != DELIVERY_VIEW_VERSION {
        return Err(FetchError::Version(view.version));
    }
    Ok(view)
}

async fn fetch() -> Result<DeliveryView, FetchError> {
    let (status, text) = send("GET", PATH, None).await?;
    if status == 401 {
        return Err(FetchError::SignedOut);
    }
    if status != 200 {
        return Err(FetchError::Refused(refusal(status, &text)));
    }
    parse_view(&text)
}

/// One write: the receipt lines, or the refusal in words.
async fn post(action: &str, body: String) -> Result<Option<DeliveryReceipt>, String> {
    let url = format!("{PATH}/{action}");
    match send("POST", &url, Some(body)).await {
        Ok((200, text)) if matches!(action, "check" | "trust") => {
            parse_view(&text).map(|_| None).map_err(|e| e.sentence())
        }
        Ok((200, text)) => serde_json::from_str::<DeliveryReceipt>(&text)
            .map(Some)
            .map_err(|_| FetchError::Malformed.sentence()),
        Ok((status, text)) => Err(refusal(status, &text)),
        Err(e) => Err(e.sentence()),
    }
}

/// What the second step asks the owner to confirm.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    Deliver,
    Copy(String, String),
    Remove(String, String),
    Trust(String, String),
}

/// The static-logins section inside the Logins plate.
#[component]
pub(crate) fn DeliveryPanel(ctx: crate::frame::FrameCtx) -> impl IntoView {
    let state = RwSignal::new(None::<Result<DeliveryView, FetchError>>);
    let form = RwSignal::new(Form::default());
    let open = RwSignal::new(false);
    let pending = RwSignal::new(None::<Pending>);
    let busy = RwSignal::new(false);
    let outcome = RwSignal::new(None::<Result<Vec<String>, String>>);
    let read = move || {
        leptos::task::spawn_local(async move {
            let result = fetch().await;
            let _ = state.try_set(Some(result));
        });
    };
    Effect::new(move |_| {
        if ctx.route.get() == crate::Page::Accounts {
            read();
        }
    });
    let labels = Signal::derive(move || {
        state.with(|s| match s {
            Some(Ok(v)) => v
                .computers
                .iter()
                .map(|c| (c.id.clone(), c.label.clone()))
                .collect::<Vec<_>>(),
            _ => vec![],
        })
    });
    let run = move |action: &'static str, body: String| {
        busy.set(true);
        pending.set(None);
        leptos::task::spawn_local(async move {
            let answer = post(action, body).await;
            let lines = answer.map(|r| {
                r.map(|r| receipt_lines(&r, &labels.get_untracked()))
                    .unwrap_or_default()
            });
            let _ = outcome.try_set(Some(lines));
            let _ = busy.try_set(false);
            read();
        });
    };
    let start = move |login: Option<DeliveredLogin>| {
        let mut f = Form::default();
        if let Some(l) = login {
            f.login_id = Some(l.login_id.clone());
            // What the page shows on each computer; a replace confirms exactly these.
            f.shown = l
                .holders
                .iter()
                .filter(|h| {
                    matches!(h.state.as_str(), "ok" | "differs") && !h.fingerprint.is_empty()
                })
                .map(|h| (h.computer.clone(), h.fingerprint.clone()))
                .collect();
            f.kind = l.kind.clone();
            f.provider = l.provider.clone();
            f.account = l.account.clone();
            f.computers = f.shown.iter().map(|(c, _)| c.clone()).collect();
        } else {
            f.kind = "claude-token".into();
        }
        form.set(f);
        outcome.set(None);
        pending.set(None);
        open.set(true);
    };
    let confirm_deliver = move |_| {
        // The secret leaves the page here: the body takes it out of the form.
        let body = form.try_update(|f| f.take_body()).unwrap_or_default();
        open.set(false);
        run("put", body);
    };
    view! {
        <div class="ld" data-probe="login-delivery">
            <div class="ld-head">
                <h3 class="ld-title">"Static logins"</h3>
                <div class="ld-tools">
                    <button type="button" class="btn sm" data-probe="ld-check" aria-disabled=move || if busy.get() { "true" } else { "false" }
                        on:click=move |_| if !busy.get_untracked() { run("check", "{}".into()) }>"Check computers"</button>
                    <button type="button" class="btn sm primary" data-probe="ld-new"
                        on:click=move |_| start(None)>"Deliver a login"</button>
                </div>
            </div>
            <p class="ac-note ld-lead">"A Claude setup-token token or an API key is entered once and written to the computers you tick. Sign-ins that renew themselves stay on their own computer. What you paste is never shown again; only its fingerprint is kept."</p>
            {move || outcome.get().map(|o| match o {
                Ok(lines) => view! { <ul class="ld-out" data-probe="ld-outcome">{lines.into_iter().map(|l| view! { <li>{l}</li> }).collect_view()}</ul> }.into_any(),
                Err(msg) => view! { <p class="ac-note crit" data-probe="ld-refused">{msg}</p> }.into_any(),
            })}
            {move || state.with(|s| match s {
                Some(Ok(v)) => v.computers.iter().filter(|c| c.state == "other").map(|c| {
                    let body = json!({"computer": c.id, "key": c.seen_key, "confirm": "trust"}).to_string();
                    let text = format!("Trust the computer answering as {} (host {}) from now on? Only if it was rebuilt or renamed: until then nothing is sent to it.", c.label, c.host);
                    let line = format!("{}: another computer answered (host {}). Nothing is sent to it.", c.label, c.host);
                    view! {
                        <p class="ac-note crit ld-other" data-probe=format!("ld-other:{}", c.id)>{line}" "
                            <button type="button" class="btn sm" data-probe="ld-trust" on:click=move |_| pending.set(Some(Pending::Trust(body.clone(), text.clone())))>"Trust it"</button>
                        </p>
                    }
                }).collect_view().into_any(),
                _ => ().into_any(),
            })}
            <Show when=move || open.get()>
                {move || state.with(|s| s.as_ref().and_then(|s| s.as_ref().ok()).cloned()).map(|view_now| {
                    let replace = form.with_untracked(|f| f.login_id.is_some());
                    let kinds = view_now.kinds.clone();
                    let accounts = view_now.accounts.clone();
                    let accounts_change = accounts.clone();
                    let computers = view_now.computers.clone();
                    view! {
                        <div class="ld-form" role="group" aria-label="Deliver a static login">
                            <label class="field">
                                <span class="field-label">"Kind"</span>
                                <select class="field-input" data-probe="ld-kind" disabled=replace
                                    on:change=move |e| { let k = event_target_value(&e); form.update(|f| { f.kind = k; f.account.clear(); f.provider.clear(); }); }>
                                    {kinds.iter().map(|k| {
                                        let value = k.kind.clone();
                                        let selected = form.with_untracked(|f| f.kind == value);
                                        view! { <option value=value selected=selected>{k.label.clone()}</option> }
                                    }).collect_view()}
                                </select>
                            </label>
                            <label class="field">
                                <span class="field-label">"Account"</span>
                                <select class="field-input" data-probe="ld-account" disabled=replace
                                    on:change={
                                        move |e| {
                                            let id = event_target_value(&e);
                                            let provider = accounts_change.iter().find(|a| a.id == id).map(|a| a.provider.clone()).unwrap_or_default();
                                            form.update(|f| { f.account = id; f.provider = provider; });
                                        }
                                    }>
                                    <option value="">"Pick an account"</option>
                                    {move || {
                                        let kind = form.with(|f| f.kind.clone());
                                        let allowed: Vec<String> = kinds.iter().find(|k| k.kind == kind).map(|k| k.providers.clone()).unwrap_or_default();
                                        accounts.iter().filter(|a| allowed.contains(&a.provider)).map(|a| {
                                            let id = a.id.clone();
                                            let selected = form.with_untracked(|f| f.account == id);
                                            view! { <option value=id selected=selected>{format!("{} · {}", a.label, a.provider)}</option> }
                                        }).collect_view()
                                    }}
                                </select>
                            </label>
                            <label class="field">
                                <span class="field-label">{move || if form.with(|f| f.kind == "claude-token") { "Token (from claude setup-token)" } else { "API key" }}</span>
                                <input class="field-input mono-in" type="password" autocomplete="off" autocapitalize="off" spellcheck="false"
                                    data-probe="ld-secret" prop:value=move || form.with(|f| f.secret.clone())
                                    on:input=move |e| { let v = event_target_value(&e); form.update(|f| f.secret = v); }/>
                            </label>
                            <label class="field">
                                <span class="field-label">"Ends on (optional, YYYY-MM-DD)"</span>
                                <input class="field-input" type="text" inputmode="numeric" placeholder="YYYY-MM-DD" maxlength="10" autocomplete="off" data-probe="ld-ends" prop:value=move || form.with(|f| f.expires_on.clone())
                                    on:input=move |e| { let v = event_target_value(&e); form.update(|f| f.expires_on = v); }/>
                            </label>
                            <fieldset class="ld-comps">
                                <legend class="field-label">"Computers"</legend>
                                {computers.into_iter().map(|c| {
                                    let id = c.id.clone();
                                    let id2 = c.id.clone();
                                    let enrolled = c.state == "enrolled";
                                    let note = match c.state.as_str() {
                                        "enrolled" => "",
                                        "new" => " (check computers first)",
                                        _ => " (another computer answered)",
                                    };
                                    view! {
                                        <label class="check ld-comp" class:off=!enrolled>
                                            <input type="checkbox" data-probe=format!("ld-comp:{}", c.id) disabled=!enrolled
                                                prop:checked=move || form.with(|f| f.computers.contains(&id))
                                                on:change=move |e| {
                                                    let on = event_target_checked(&e);
                                                    let id = id2.clone();
                                                    form.update(|f| { f.computers.retain(|x| x != &id); if on { f.computers.push(id); } });
                                                }/>
                                            <span>{format!("{}{note}", c.label)}</span>
                                        </label>
                                    }
                                }).collect_view()}
                            </fieldset>
                            <div class="ld-acts">
                                <button type="button" class="btn sm" on:click=move |_| { form.set(Form::default()); open.set(false); pending.set(None); }>"Cancel"</button>
                                <button type="button" class="btn sm primary" data-probe="ld-review"
                                    aria-disabled=move || if form.with(Form::ready) { "false" } else { "true" }
                                    on:click=move |_| if form.with_untracked(Form::ready) { pending.set(Some(Pending::Deliver)) }>
                                    {if replace { "Review the replace" } else { "Review" }}
                                </button>
                            </div>
                        </div>
                    }
                })}
            </Show>
            {move || pending.get().map(|p| {
                let (text, label, act) = match &p {
                    Pending::Deliver => {
                        let n = form.with_untracked(|f| f.computers.len());
                        let replace = form.with_untracked(|f| f.login_id.is_some());
                        let named = form.with_untracked(|f| {
                            f.expects()
                                .iter()
                                .map(|(c, e)| {
                                    let name = labels.get_untracked().iter().find(|(id, _)| id == c).map_or(c.clone(), |(_, l)| l.clone());
                                    if e == "absent" { format!("{name} (none there)") } else { format!("{name} (replaces {})", short(e)) }
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        });
                        let text = match replace {
                            true => format!("Replace this login on {named}. A computer holding anything else is left alone."),
                            false => format!("Write this {} to {n} computer{}. It is sent once and never shown again.", kind_label(&form.with_untracked(|f| f.kind.clone())), if n == 1 { "" } else { "s" }),
                        };
                        (text, "Deliver", "put")
                    }
                    Pending::Copy(_, text) => (text.clone(), "Copy", "copy"),
                    Pending::Remove(_, text) => (text.clone(), "Remove", "remove"),
                    Pending::Trust(_, text) => (text.clone(), "Trust it", "trust"),
                };
                let p2 = p.clone();
                view! {
                    <div class="ld-confirm" role="alertdialog" aria-label="Confirm" data-probe="ld-confirm">
                        <p>{text}</p>
                        <div class="ld-acts">
                            <button type="button" class="btn sm" on:click=move |_| pending.set(None)>"Back"</button>
                            <button type="button" class=if act == "remove" { "btn sm danger-solid" } else { "btn sm primary" } data-probe="ld-confirm-go"
                                on:click=move |e| match &p2 {
                                    Pending::Deliver => confirm_deliver(e),
                                    Pending::Copy(body, _) => run("copy", body.clone()),
                                    Pending::Remove(body, _) => run("remove", body.clone()),
                                    Pending::Trust(body, _) => run("trust", body.clone()),
                                }>{label}</button>
                        </div>
                    </div>
                }
            })}
            {move || state.with(|s| match s {
                None => view! { <p class="ac-note">"Reading static logins…"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="ac-note crit">{format!("Static logins could not be read. {}", e.sentence())}</p> }.into_any(),
                Some(Ok(v)) if v.logins.is_empty() => view! { <p class="ac-note" data-probe="ld-empty">"No static login has been delivered yet."</p> }.into_any(),
                Some(Ok(v)) => {
                    let all: Vec<String> = writable(&v.computers);
                    let labels_now = labels.get_untracked();
                    view! {
                        <div class="ld-rows" role="table" aria-label="Static logins">
                            {v.logins.clone().into_iter().map(|login| {
                                let copy = copy_body(&login, &all);
                                let missing_n = missing(&login, &all).len();
                                let holders = holders_line(&login, &labels_now);
                                let remove = remove_body(&login);
                                let fp = if login.fingerprint.is_empty() { "Computers disagree".to_owned() } else { login.fingerprint.clone() };
                                let fp_short = short(&login.fingerprint).to_owned();
                                let replace_login = login.clone();
                                let health = match login.health.as_str() {
                                    "ended" => "lg-h crit",
                                    "expiring" => "lg-h warn",
                                    _ => "lg-h",
                                };
                                view! {
                                    <div class="ld-row" role="row" data-probe=format!("ld-row:{}", login.login_id)>
                                        <span class="ld-acct" role="cell" title=login.account_label.clone()>{login.account_label.clone()}</span>
                                        <span class="ld-kind" role="cell">{kind_label(&login.kind)}</span>
                                        <span class="ld-fp mono" role="cell" title="Fingerprint of the current value (not the secret)">{fp.clone()}</span>
                                        <span class=health role="cell">{login.ends_text.clone()}</span>
                                        <span class="ld-where" role="cell" title=holders.clone()>{holders.clone()}</span>
                                        <span class="ld-row-acts" role="cell">
                                            {copy.map(|body| {
                                                let text = format!("Copy the value {fp_short} to {missing_n} more computer{}. A computer holding another value gets this one in its place; anything not shown here is left alone.", if missing_n == 1 { "" } else { "s" });
                                                view! { <button type="button" class="btn sm" data-probe="ld-copy" on:click=move |_| pending.set(Some(Pending::Copy(body.clone(), text.clone())))>"Copy to the rest"</button> }
                                            })}
                                            <button type="button" class="btn sm" data-probe="ld-replace" on:click=move |_| start(Some(replace_login.clone()))>"Replace"</button>
                                            {remove.map(|remove| {
                                                let text = "Remove this login from every computer where its value is shown. An unreadable file or another value there is left alone.".to_owned();
                                                view! { <button type="button" class="btn sm danger-text" data-probe="ld-remove" on:click=move |_| pending.set(Some(Pending::Remove(remove.clone(), text.clone())))>"Remove"</button> }
                                            })}
                                        </span>
                                    </div>
                                }
                            }).collect_view()}
                        </div>
                    }.into_any()
                }
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aac_api::login_delivery::{DeliveryComputer, LoginHolder};

    const SECRET: &str = "sk-ant-oat01-TESTONLY-page-token-0000000";
    const A: &str = "sha256:0011223344556677";
    const B: &str = "sha256:8899aabbccddeeff";

    fn holder(computer: &str, fingerprint: &str, state: &str) -> LoginHolder {
        LoginHolder {
            computer: computer.into(),
            delivered_at: String::new(),
            fingerprint: fingerprint.into(),
            state: state.into(),
        }
    }

    fn login() -> DeliveredLogin {
        DeliveredLogin {
            login_id: "0a1b2c3d".into(),
            kind: "claude-token".into(),
            provider: "claude".into(),
            account: "claude:me".into(),
            account_label: "me@example.com".into(),
            fingerprint: A.into(),
            holders: vec![
                holder("ubuntu", A, "ok"),
                holder("mac", B, "differs"),
                holder("win", "", "unreadable"),
            ],
            ..Default::default()
        }
    }

    /// Red/green: the page sends the secret once and keeps no copy of it.
    #[test]
    fn the_form_gives_up_its_secret_when_it_sends() {
        let mut form = Form {
            kind: "claude-token".into(),
            provider: "claude".into(),
            account: "claude:me".into(),
            secret: format!("  {SECRET}\n"),
            computers: vec!["ubuntu".into()],
            ..Default::default()
        };
        assert!(form.ready());
        assert!(!format!("{form:?}").contains("TESTONLY"));
        let body: serde_json::Value = serde_json::from_str(&form.take_body()).expect("test value");
        assert_eq!(body["secret"], SECRET);
        assert_eq!(body["confirm"], "deliver");
        assert_eq!(
            body["targets"],
            json!([{"computer":"ubuntu","expect":"absent"}])
        );
        assert_eq!(body["expiresOn"], serde_json::Value::Null);
        assert!(form.secret.is_empty());
        assert!(!format!("{form:?}").contains("TESTONLY"));
        assert!(!form.ready());
    }

    /// A replace names, per computer, the value the page showed there.
    #[test]
    fn a_replace_confirms_what_each_computer_holds() {
        let mut form = Form {
            login_id: Some("0a1b2c3d".into()),
            shown: vec![("ubuntu".into(), A.into()), ("mac".into(), B.into())],
            kind: "claude-token".into(),
            provider: "claude".into(),
            account: "claude:me".into(),
            secret: SECRET.into(),
            expires_on: "2027-11-01".into(),
            computers: vec!["ubuntu".into(), "mac".into(), "nas1".into()],
        };
        let body: serde_json::Value = serde_json::from_str(&form.take_body()).expect("test value");
        assert_eq!(body["confirm"], "replace");
        assert_eq!(
            body["targets"],
            json!([{"computer":"ubuntu","expect":A},{"computer":"mac","expect":B},
                {"computer":"nas1","expect":"absent"}])
        );
        assert_eq!(body["expiresOn"], "2027-11-01");
    }

    /// Red/green (review): copy names the source's value and what each destination holds; an
    /// unreadable computer is never a destination or a removal target; a login whose computers
    /// disagree offers no copy.
    #[test]
    fn copy_and_remove_bodies_name_each_computers_value() {
        let all = vec![
            "ubuntu".to_owned(),
            "mac".to_owned(),
            "nas1".to_owned(),
            "win".to_owned(),
        ];
        let copy: serde_json::Value =
            serde_json::from_str(&copy_body(&login(), &all).expect("test value"))
                .expect("test value");
        assert_eq!(copy["from"], json!({"computer":"ubuntu","fingerprint":A}));
        assert_eq!(
            copy["to"],
            json!([{"computer":"mac","expect":B},{"computer":"nas1","expect":"absent"}])
        );
        assert_eq!(copy["confirm"], "copy");
        let remove: serde_json::Value =
            serde_json::from_str(&remove_body(&login()).expect("test value")).expect("test value");
        assert_eq!(
            remove["targets"],
            json!([{"computer":"ubuntu","expect":A},{"computer":"mac","expect":B}])
        );
        let mut disagree = login();
        disagree.fingerprint.clear();
        assert!(copy_body(&disagree, &all).is_none());
        let computers = vec![
            DeliveryComputer {
                id: "ubuntu".into(),
                state: "enrolled".into(),
                ..Default::default()
            },
            DeliveryComputer {
                id: "mac".into(),
                state: "new".into(),
                ..Default::default()
            },
            DeliveryComputer {
                id: "nas1".into(),
                state: "other".into(),
                ..Default::default()
            },
        ];
        assert_eq!(writable(&computers), ["ubuntu"]);
    }

    #[test]
    fn holders_and_receipts_read_in_words() {
        let labels = vec![("ubuntu".to_owned(), "Ubuntu VM".to_owned())];
        assert_eq!(
            holders_line(&login(), &labels),
            "Ubuntu VM ok · mac other value 8899aabb · win unreadable file"
        );
        let receipt = DeliveryReceipt {
            login_id: "0a1b2c3d".into(),
            fingerprint: A.into(),
            results: vec![aac_api::login_delivery::DeliveryResult {
                computer: "ubuntu".into(),
                state: "delivered".into(),
                text: "Delivered".into(),
            }],
        };
        assert_eq!(
            receipt_lines(&receipt, &labels),
            vec!["Ubuntu VM: Delivered".to_owned()]
        );
        assert_eq!(refusal(422, r#"{"error":"Nope.","code":"x"}"#), "Nope.");
        assert_eq!(refusal(500, "garbage"), "The server answered 500.");
    }

    #[test]
    fn the_device_fixture_reads() {
        let view = parse_view(include_str!("../../fixtures/login-delivery-view.json"))
            .expect("test value");
        assert!(!view.logins.is_empty());
        assert!(!include_str!("../../fixtures/login-delivery-view.json").contains("sk-"));
        assert!(matches!(
            parse_view(r#"{"version":2}"#),
            Err(FetchError::Version(2))
        ));
    }
}
