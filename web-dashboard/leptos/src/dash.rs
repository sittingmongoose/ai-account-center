//! The dashboard's data for the frame's pages that take it (Home, Accounts & Settings, Details and the switch approval).
//! The frame reads it itself (G5): `public/frame-dashboard.mjs` runs in this window the same server calls, timers,
//! guards and pure modules bridge.js runs in the classic app (the accounts controller, the target-bound switch
//! approvals, the one-use remove and restore tokens), and hands the same versioned views. No tab loads the Slint
//! dashboard or its bridge any more; Analytics reads its own (analytics/feed.rs).
//!
//! `index.html` installs `window.aacLoadDashboard()` (a dynamic import, started early on `/app/home` and
//! `/app/accounts`); it resolves once `window.aacFrameDashboard` holds the controller. Its views arrive as `aac-dash`
//! events (`{key, json}`) and the pages' actions go back through `aacFrameDashboard.act(kind, value)`, which runs only
//! the seam's page action kinds (`frame-seam.mjs PAGE_ACTIONS`). An action is never sent twice.

use crate::Page;
use leptos::prelude::*;
use wasm_bindgen::{JsCast, JsValue};

/// How many times a page asked for Details: a click outside the open panel that asked for another account swaps it
/// instead of closing it (details.rs, as bridge.js `detailsOpens` does for the canvas).
pub static DETAILS_ASKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The latest view of each key, as raw JSON (each page parses its own types). Empty until the first answer: the pages
/// show their loading state, never zeros.
#[derive(Clone, Copy)]
pub struct DashCtx {
    /// View model v2 (`set_dashboard`).
    pub dashboard: RwSignal<String>,
    /// Accounts & Settings v3 (`set_accounts`).
    pub accounts: RwSignal<String>,
    /// The change-password strength (`set_accounts_strength`).
    pub strength: RwSignal<String>,
    /// Details (`show_details`); empty while closed.
    pub details: RwSignal<String>,
    /// The switch approval (`show_activation_confirmation`); empty while closed.
    pub activation: RwSignal<String>,
    /// Update apps (`set_update_status`).
    pub update: RwSignal<String>,
    /// A switch or a save runs (`set_busy`).
    pub busy: RwSignal<bool>,
    /// `{"seconds": 60, "confirmed": true}` (`set_refresh_interval`).
    pub refresh_interval: RwSignal<String>,
    /// The frame header's line for Home and Accounts (`{statusLead, statusStrong, statusMore}`, view-model.mjs
    /// chromeView).
    pub chrome: RwSignal<String>,
    /// The controller has loaded and started: only then do actions go to it.
    pub(crate) live: RwSignal<bool>,
    /// The header's Refresh and Update apps, held while the controller loads (the first click counts).
    waiting: RwSignal<Vec<String>>,
}

impl DashCtx {
    pub(crate) fn new() -> Self {
        DashCtx {
            dashboard: RwSignal::new(String::new()),
            accounts: RwSignal::new(String::new()),
            strength: RwSignal::new(String::new()),
            details: RwSignal::new(String::new()),
            activation: RwSignal::new(String::new()),
            update: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            refresh_interval: RwSignal::new(String::new()),
            chrome: RwSignal::new(String::new()),
            live: RwSignal::new(false),
            waiting: RwSignal::new(Vec::new()),
        }
    }

    /// One view from the controller. Unknown keys are ignored (Analytics' among them: it reads its own).
    pub(crate) fn receive(&self, key: &str, json: String) {
        crate::ui::reconcile_overlay_focus(|| match key {
            "dashboard" => {
                let _ = self.dashboard.try_set(json);
            }
            "accounts" => {
                let _ = self.accounts.try_set(json);
            }
            "accounts-strength" => {
                let _ = self.strength.try_set(json);
            }
            "details" => {
                let _ = self.details.try_set(json);
            }
            "activation" => {
                let _ = self.activation.try_set(json);
            }
            "update" => {
                let _ = self.update.try_set(json);
            }
            "busy" => {
                let _ = self.busy.try_set(json == "true");
            }
            "refresh-interval" => {
                let _ = self.refresh_interval.try_set(json);
            }
            "chrome" => {
                let _ = self.chrome.try_set(json);
            }
            _ => {}
        });
    }

    /// The session is gone (sign-out): nothing of it stays on screen.
    pub(crate) fn clear(&self) {
        for signal in [
            self.dashboard,
            self.accounts,
            self.strength,
            self.details,
            self.activation,
            self.update,
            self.refresh_interval,
            self.chrome,
        ] {
            let _ = signal.try_set(String::new());
        }
        let _ = self.busy.try_set(false);
        let _ = self.live.try_set(false);
        let _ = self.waiting.try_set(Vec::new());
    }

    /// The controller could not be loaded: the pages say so instead of waiting.
    fn failed(&self) {
        let _ = self.dashboard.try_set(NO_START.into());
        let _ = self.accounts.try_set(NO_START.into());
    }

    /// Runs one of the dashboard's page actions. False, and nothing run, while the controller has not started (the page
    /// has no data to act on then either) or for a kind that isn't a page action.
    pub fn act(&self, kind: &str, value: &str) -> bool {
        if !self.live.get_untracked() {
            return false;
        }
        if kind == "details" {
            DETAILS_ASKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        call("act", &[kind.into(), value.into()])
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// The header's Refresh or Update apps. Held until the controller has started, then run once.
    pub fn command(&self, action: &str) {
        if self.live.get_untracked() {
            call("command", &[action.into()]);
            return;
        }
        self.waiting.update(|all| {
            if !all.iter().any(|a| a == action) {
                all.push(action.to_string());
            }
        });
    }

    /// Sign out through the controller (it waits while a switch confirmation is on screen and words a failure). None
    /// while it has not started: the frame's session signs out itself then.
    pub(crate) fn sign_out(&self) -> Option<()> {
        if !self.live.get_untracked() {
            return None;
        }
        call("signOut", &[]).map(|_| ())
    }
}

fn controller() -> Option<JsValue> {
    js_sys::Reflect::get(&window(), &"aacFrameDashboard".into())
        .ok()
        .filter(|v| v.is_object())
}

/// Calls one of the controller's methods.
fn call(method: &str, args: &[JsValue]) -> Option<JsValue> {
    let target = controller()?;
    let f = js_sys::Reflect::get(&target, &method.into()).ok()?;
    let f = f.dyn_ref::<js_sys::Function>()?;
    let list = js_sys::Array::new();
    for a in args {
        list.push(a);
    }
    f.apply(&target, &list).ok()
}

fn read_str(data: &JsValue, key: &str) -> String {
    js_sys::Reflect::get(data, &key.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

/// What the controller says beside the views.
pub(crate) enum Note {
    /// A result or a failure: `(kind, title, body, ms)`.
    Toast(String, String, String, i32),
    /// The session ended under the page (`expired`, with the reason) or Sign out finished (`default`).
    SignedOut(String, Option<String>),
}

/// The `toast` and `signed-out` details (JSON), read as the frame acts on them.
pub(crate) fn note(key: &str, json: &str) -> Option<Note> {
    let v = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    match key {
        "toast" => Some(Note::Toast(
            s("kind"),
            s("title"),
            s("body"),
            v.get("ms").and_then(|x| x.as_f64()).unwrap_or(4800.0) as i32,
        )),
        "signed-out" => {
            let state = s("state");
            if !matches!(state.as_str(), "expired" | "default") {
                return None;
            }
            let reason = s("reason");
            Some(Note::SignedOut(
                state,
                (!reason.is_empty()).then_some(reason),
            ))
        }
        _ => None,
    }
}

/// Loads the controller and follows its views. `route` is the tab on screen (Accounts & Settings reads its own blocks
/// when it comes on screen); `noted` gets each toast and the end of the session. Everything stops with the owner
/// (sign-out).
pub(crate) fn start(dash: DashCtx, route: Signal<Page>, noted: impl Fn(Note) + 'static) {
    let ready = RwSignal::new(false);
    let listener = window_event_listener_untyped("aac-dash", move |event| {
        let Some(event) = event.dyn_ref::<web_sys::CustomEvent>() else {
            return;
        };
        let detail = event.detail();
        let key = read_str(&detail, "key");
        let json = read_str(&detail, "json");
        match note(&key, &json) {
            Some(n) => {
                if matches!(n, Note::SignedOut(..)) {
                    let _ = dash.live.try_set(false);
                }
                noted(n);
            }
            None => {
                if key == "dashboard" {
                    // an account hidden, shown or removed: Analytics follows while its tab is away
                    crate::analytics::feed::dashboard_view(&json);
                }
                dash.receive(&key, json);
            }
        }
    });
    let load = js_sys::Reflect::get(&window(), &"aacLoadDashboard".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call0(&JsValue::NULL).ok())
        .and_then(|p| p.dyn_into::<js_sys::Promise>().ok());
    match load {
        Some(promise) => leptos::task::spawn_local(async move {
            let loaded = wasm_bindgen_futures::JsFuture::from(promise).await.is_ok();
            if !loaded {
                dash.failed();
            } else if ready.try_set(true).is_some() {
                // signed out while it loaded: the page is gone, so is its controller
                call("stop", &[]);
            }
        }),
        None => dash.failed(),
    }
    Effect::new(move |started: Option<bool>| {
        if !ready.get() {
            return false;
        }
        call("page", &[route.get().id().into()]);
        if started != Some(true) {
            call("start", &[]);
            dash.live.set(true);
            for action in dash.waiting.try_update(std::mem::take).unwrap_or_default() {
                call("command", &[action.as_str().into()]);
            }
        }
        true
    });
    on_cleanup(move || {
        listener.remove();
        call("stop", &[]);
        dash.clear();
    });
}

/// Handed to Home and Accounts when the controller could not be loaded at all: each page refuses a view it can't
/// read with its own sentence ("... could not be read. Reload the app."), never zeros.
const NO_START: &str = "unavailable";

/// Text that may arrive as `null` (NOTES.md: `default` is not enough for null).
pub fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    use serde::Deserialize;
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_land_in_their_signal_and_a_sign_out_clears_them() {
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        dash.receive("accounts", "{\"version\":3}".into());
        dash.receive("details", "{\"id\":\"a\"}".into());
        dash.receive("busy", "true".into());
        dash.receive("refresh-interval", "{\"seconds\":60}".into());
        dash.receive("chrome", "{\"statusLead\":\"Updated\"}".into());
        dash.receive("unknown", "x".into());
        assert_eq!(dash.accounts.get_untracked(), "{\"version\":3}");
        assert!(dash.busy.get_untracked());
        assert_eq!(dash.chrome.get_untracked(), "{\"statusLead\":\"Updated\"}");
        dash.clear();
        assert!(dash.accounts.get_untracked().is_empty());
        assert!(dash.details.get_untracked().is_empty());
        assert!(dash.refresh_interval.get_untracked().is_empty());
        assert!(!dash.busy.get_untracked());
    }

    #[test]
    fn analytics_views_are_dropped() {
        // Analytics reads its own data (analytics/feed.rs): a view with its keys must not land anywhere
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        for key in [
            "analytics",
            "analytics-motion",
            "analytics-head",
            "analytics-loading",
        ] {
            dash.receive(key, "{\"version\":7}".into());
        }
        assert!(
            dash.dashboard.get_untracked().is_empty() && dash.accounts.get_untracked().is_empty()
        );
    }

    #[test]
    fn nothing_is_run_before_the_controller_has_started_and_commands_wait() {
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        assert!(!dash.act("remove", "claude:a"));
        assert!(dash.sign_out().is_none());
        dash.command("update-apps");
        dash.command("refresh");
        dash.command("update-apps");
        // the first click counts, once
        assert_eq!(
            dash.waiting.get_untracked(),
            vec!["update-apps".to_string(), "refresh".to_string()]
        );
        dash.clear();
        assert!(dash.waiting.get_untracked().is_empty());
    }

    #[test]
    fn a_controller_that_never_loads_is_refused_in_words() {
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        dash.failed();
        assert!(matches!(
            crate::home::parse(&dash.dashboard.get_untracked()),
            crate::home::HomeState::Refused(_)
        ));
        assert!(crate::accounts::model::parse(&dash.accounts.get_untracked()).is_err());
    }

    #[test]
    fn toasts_and_the_end_of_a_session_are_read_from_their_json() {
        match note(
            "toast",
            r#"{"kind":"ok","title":"Codex account switched","body":"b","ms":7000}"#,
        ) {
            Some(Note::Toast(kind, title, body, ms)) => {
                assert_eq!(
                    (kind.as_str(), title.as_str(), body.as_str(), ms),
                    ("ok", "Codex account switched", "b", 7000)
                );
            }
            _ => panic!("a toast"),
        }
        match note("signed-out", r#"{"state":"expired","reason":"revoked"}"#) {
            Some(Note::SignedOut(state, reason)) => {
                assert_eq!(state, "expired");
                assert_eq!(reason.as_deref(), Some("revoked"));
            }
            _ => panic!("an ended session"),
        }
        match note("signed-out", r#"{"state":"default","reason":""}"#) {
            Some(Note::SignedOut(state, reason)) => {
                assert_eq!(state, "default");
                assert!(reason.is_none());
            }
            _ => panic!("a sign-out"),
        }
        // anything else is a view (or nothing): never a sign-out
        assert!(note("signed-out", r#"{"state":"loading"}"#).is_none());
        assert!(note("signed-out", "not json").is_none());
        assert!(note("dashboard", r#"{"version":2}"#).is_none());
    }
}
