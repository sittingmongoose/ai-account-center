//! The frame's sign-in: a real HTML form, so password managers fill and save it. Every state and word is decided by
//! `public/frame-session.mjs` (the same server calls and auth-view.mjs words as today's dashboard); this file draws
//! its view and hands back what was typed. The view arrives as `aac-session` events; the calls go through
//! `window.aacFrameSession`, which `index.html` installs before the page starts.

use crate::frame::FrameCtx;
use crate::ui::*;
use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Strength {
    pub pct: f64,
    pub word: String,
    pub hint: String,
    pub matches: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionView {
    pub signed_in: bool,
    /// loading, default, connecting, wrong, limited, setup, expired, offline or success.
    pub state: String,
    pub host: String,
    pub username: String,
    pub transport_note: Option<String>,
    pub setup_form: bool,
    pub code_required: bool,
    pub busy: bool,
    pub nonce: u64,
    pub message: Option<String>,
    pub message_sub: Option<String>,
    pub notice: bool,
    pub field: Option<String>,
    pub banner_title: Option<String>,
    pub banner_body: Option<String>,
    pub banner_strong: Option<String>,
    pub retry_seconds: Option<f64>,
    pub offline_title: Option<String>,
    pub offline_strong: Option<String>,
    pub offline_body: Option<String>,
    pub offline_meta: Option<String>,
    pub strength: Option<Strength>,
}

fn session_object() -> Option<JsValue> {
    js_sys::Reflect::get(&window(), &"aacFrameSession".into())
        .ok()
        .filter(|v| v.is_object())
}

/// Calls one of the session's methods with one argument (or none).
fn call(method: &str, arg: Option<JsValue>) -> Option<JsValue> {
    let session = session_object()?;
    let f = js_sys::Reflect::get(&session, &method.into()).ok()?;
    let f = f.dyn_ref::<js_sys::Function>()?;
    match arg {
        Some(a) => f.call1(&session, &a).ok(),
        None => f.call0(&session).ok(),
    }
}

/// Follows the session's view and starts it.
pub fn start() -> RwSignal<SessionView> {
    let view = RwSignal::new(SessionView { state: "loading".into(), ..Default::default() });
    let handler = Closure::<dyn Fn(web_sys::CustomEvent)>::new(move |event: web_sys::CustomEvent| {
        if let Some(text) = event.detail().as_string() {
            if let Ok(next) = serde_json::from_str::<SessionView>(&text) {
                view.set(next);
            }
        }
    });
    let _ = window().add_event_listener_with_callback("aac-session", handler.as_ref().unchecked_ref());
    handler.forget();
    if call("start", None).is_none() {
        view.set(SessionView {
            state: "default".into(),
            message: Some("This page could not start its sign-in. Reload it, or open the classic app.".into()),
            ..Default::default()
        });
    }
    view
}

/// The dashboard (or a page's read) found the session gone: "expired" works out why; "default" is a sign-out.
pub fn dashboard_signed_out(state: &str) {
    let signed_in = call("view", None)
        .and_then(|v| js_sys::Reflect::get(&v, &"signedIn".into()).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !signed_in {
        return;
    }
    if state == "expired" {
        call("ended", None);
    } else {
        call("signedOutElsewhere", None);
    }
}

/// Sign out from the frame (no dashboard loaded): a failure is a toast, and this browser stays signed in.
pub fn sign_out(ctx: FrameCtx) {
    let Some(promise) = call("signOut", None).and_then(|p| p.dyn_into::<js_sys::Promise>().ok()) else { return };
    leptos::task::spawn_local(async move {
        let Ok(result) = wasm_bindgen_futures::JsFuture::from(promise).await else { return };
        let ok = js_sys::Reflect::get(&result, &"ok".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let failure = js_sys::Reflect::get(&result, &"failure".into()).unwrap_or(JsValue::UNDEFINED);
            let title = js_sys::Reflect::get(&failure, &"title".into()).ok().and_then(|v| v.as_string()).unwrap_or_else(|| "Sign out did not work".into());
            let body = js_sys::Reflect::get(&failure, &"body".into()).ok().and_then(|v| v.as_string()).unwrap_or_default();
            ctx.toast("err", &title, &body, 6400);
        }
    });
}

fn form_value(form: &web_sys::HtmlFormElement, name: &str) -> String {
    web_sys::FormData::new_with_form(form)
        .ok()
        .and_then(|data| data.get(name).as_string())
        .unwrap_or_default()
}

fn object(pairs: &[(&str, JsValue)]) -> JsValue {
    let o = js_sys::Object::new();
    for (k, v) in pairs {
        let _ = js_sys::Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

/// The limited state's countdown: seconds left from the view's `retrySeconds`, counted down here and handed back
/// when it runs out (the session then reopens sign-in).
fn countdown(view: RwSignal<SessionView>) -> RwSignal<i64> {
    let left = RwSignal::new(0i64);
    Effect::new(move |_| {
        let (state, seconds, nonce) = view.with(|v| (v.state.clone(), v.retry_seconds, v.nonce));
        let _ = nonce;
        if state == "limited" {
            left.set(seconds.unwrap_or(0.0).ceil() as i64);
        }
    });
    let tick = Closure::<dyn Fn()>::new(move || {
        // the form may be gone (signed in): its signals are then disposed, and the timer stops below
        let Some(now) = left.try_get_untracked() else { return };
        if now > 0 {
            left.set(now - 1);
            if now - 1 == 0 && view.try_with_untracked(|v| v.state == "limited").unwrap_or(false) {
                call("limitOver", None);
            }
        }
    });
    let timer = window()
        .set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), 1000)
        .ok();
    tick.forget();
    on_cleanup(move || {
        if let Some(id) = timer {
            window().clear_interval_with_handle(id);
        }
    });
    left
}

fn clock(seconds: i64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[component]
pub fn SignIn(view: RwSignal<SessionView>) -> impl IntoView {
    let left = countdown(view);
    let setup = Memo::new(move |_| view.with(|v| v.state == "setup" && v.setup_form));
    let setup_command = Memo::new(move |_| view.with(|v| v.state == "setup" && !v.setup_form));
    let limited = Memo::new(move |_| view.with(|v| v.state == "limited") && left.get() > 0);
    let offline = Memo::new(move |_| view.with(|v| v.state == "offline"));
    let busy = Memo::new(move |_| view.with(|v| v.busy || v.state == "connecting"));
    // The name the server last gave: a Memo, so the field is filled again only when it changes, never while typing.
    let known_user = Memo::new(move |_| view.with(|v| v.username.clone()));
    let on_submit = move |event: web_sys::SubmitEvent| {
        event.prevent_default();
        let Some(form) = event.target().and_then(|t| t.dyn_into::<web_sys::HtmlFormElement>().ok()) else { return };
        let user = form_value(&form, "username");
        let password = form_value(&form, "password");
        if setup.get_untracked() {
            call(
                "createSignIn",
                Some(object(&[
                    ("username", user.into()),
                    ("password", password.into()),
                    ("confirm", form_value(&form, "password-confirm").into()),
                    ("code", form_value(&form, "setup-code").into()),
                ])),
            );
        } else {
            let remember = web_sys::FormData::new_with_form(&form).ok().map(|d| d.has("remember-me")).unwrap_or(true);
            call("signIn", Some(object(&[("username", user.into()), ("password", password.into()), ("remember", remember.into())])));
        }
    };
    let on_typing = move |event: web_sys::Event| {
        if !setup.get_untracked() {
            return;
        }
        let Some(form) = event
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest("form").ok().flatten())
            .and_then(|f| f.dyn_into::<web_sys::HtmlFormElement>().ok())
        else {
            return;
        };
        let password = form_value(&form, "password");
        let confirm = form_value(&form, "password-confirm");
        if let (Some(session), Ok(f)) = (session_object(), js_sys::Reflect::get(&session_object().unwrap_or(JsValue::UNDEFINED), &"typing".into())) {
            if let Some(f) = f.dyn_ref::<js_sys::Function>() {
                let _ = f.call2(&session, &password.into(), &confirm.into());
            }
        }
    };
    view! {
        <div class="signin-page paper-grid">
            <form class="signin-card plate" method="post" action="/api/auth/login" autocomplete="on" on:submit=on_submit on:input=on_typing>
                <div class="signin-brand"><Logo/><span class="brand-name">"AI Account Center"</span></div>
                <h1 class="signin-title">{move || if setup.get() || setup_command.get() { "Set up sign-in" } else { "Sign in" }}</h1>
                <p class="signin-host q3">{move || view.with(|v| v.host.clone())}</p>
                {move || view.with(|v| (v.banner_title.clone(), v.banner_body.clone(), v.banner_strong.clone())).0.map(|title| {
                    let (_, body, strong) = view.with(|v| (v.banner_title.clone(), v.banner_body.clone(), v.banner_strong.clone()));
                    view! {
                        <div class="signin-banner" role="status">
                            <b data-tone=(view.with(|v| v.state != "setup")).then_some("warn")>{title}</b>
                            {body.map(|b| view! { <p>{b}</p> })}
                            {strong.map(|s| view! { <p class="q2"><b>{s}</b></p> })}
                        </div>
                    }
                })}
                <Show when=move || offline.get()>
                    <div class="signin-banner" role="status">
                        <b>{move || view.with(|v| v.offline_title.clone().unwrap_or_default())}</b>
                        <p><b>{move || view.with(|v| v.offline_strong.clone().unwrap_or_default())}</b>" "{move || view.with(|v| v.offline_body.clone().unwrap_or_default())}</p>
                        <p class="q3">{move || view.with(|v| v.offline_meta.clone().unwrap_or_default())}</p>
                    </div>
                </Show>
                <Show when=move || !setup_command.get() fallback=move || view! {
                    <button type="button" class="btn lg" on:click=move |_| { call("recheck", None); }>"Check again"</button>
                }>
                    <label class="field">
                        <span>"Username"</span>
                        <input name="username" type="text" autocomplete="username" autocapitalize="none" spellcheck="false" enterkeyhint="next"
                            prop:value=move || known_user.get() disabled=move || limited.get() || busy.get()/>
                    </label>
                    <label class="field">
                        <span>"Password"</span>
                        <input name="password" type="password" autocomplete=move || if setup.get() { "new-password" } else { "current-password" }
                            enterkeyhint=move || if setup.get() { "next" } else { "go" } disabled=move || limited.get() || busy.get()/>
                    </label>
                    <Show when=move || setup.get()>
                        <label class="field">
                            <span>"Confirm password"</span>
                            <input name="password-confirm" type="password" autocomplete="new-password" enterkeyhint="next"/>
                        </label>
                        {move || view.with(|v| v.strength.clone()).map(|s| view! {
                            <div class="strength">
                                <div class="track thin"><div class="fill" class:warn=s.pct < 60.0 style=format!("width:{:.0}%", s.pct.clamp(0.0, 100.0))></div></div>
                                <p class="q3">{(!s.word.is_empty()).then(|| view! { <b>{s.word.clone()}</b>" · " })}{s.hint.clone()}</p>
                            </div>
                        })}
                        <Show when=move || view.with(|v| v.code_required)>
                            <label class="field">
                                <span>"Setup code"</span>
                                <input name="setup-code" type="text" autocomplete="one-time-code" autocapitalize="characters" spellcheck="false" enterkeyhint="go"/>
                            </label>
                        </Show>
                    </Show>
                    <Show when=move || !setup.get()>
                        <label class="check"><input name="remember-me" type="checkbox" checked/>"Remember me on this device"</label>
                    </Show>
                    {move || view.with(|v| v.message.clone()).map(|m| {
                        let tone = view.with(|v| if v.notice { None } else if v.state == "wrong" { Some("crit") } else { Some("warn") });
                        let sub = view.with(|v| v.message_sub.clone());
                        view! { <p class="signin-message" role="alert"><span data-tone=tone>{m}</span>{sub.map(|s| view! { <span class="q3">{format!(" {s}")}</span> })}</p> }
                    })}
                    <button type="submit" class="btn primary lg" disabled=move || limited.get() || busy.get()>
                        {move || if limited.get() {
                            format!("Try again in {}", clock(left.get()))
                        } else if busy.get() {
                            "Signing in...".to_string()
                        } else if setup.get() {
                            "Create sign-in".to_string()
                        } else {
                            "Sign in".to_string()
                        }}
                    </button>
                </Show>
                {move || view.with(|v| v.transport_note.clone()).filter(|n| !n.is_empty()).map(|n| view! { <p class="signin-note q3">{n}</p> })}
                <p class="signin-note"><a class="link" href="/" on:click=|_| {
                    if let Some(s) = window().local_storage().ok().flatten() { let _ = s.set_item("aac-frame", "classic"); }
                }>"Open the classic app"</a></p>
            </form>
        </div>
    }
}
