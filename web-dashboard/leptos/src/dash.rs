//! The dashboard's data for the frame's pages that take it (Home, Accounts & Settings, Details and the switch approval).
//! The frame reads it itself (G5): `public/frame-dashboard.mjs` runs in this window the same server calls, timers,
//! guards and pure modules bridge.js runs in the classic app (the accounts controller, the target-bound switch
//! approvals, the one-use remove and restore tokens), and hands the same versioned views. No tab loads the Slint
//! dashboard or its bridge any more; Analytics reads its own (analytics/feed.rs).
//!
//! `index.html` installs `window.aacLoadDashboard()` (a dynamic import, started early on `/app/home` and
//! `/app/accounts`); it resolves to the module, and the page installs its controller (`installFrameDashboard`, which
//! makes it `window.aacFrameDashboard` and numbers it) only if the page is still there. A page drives and stops only
//! the controller it installed (as analytics/feed.rs does), never one a later sign-in installed. Its views arrive as `aac-dash`
//! events (`{key, json}`) and the pages' actions go back through `aacFrameDashboard.act(kind, value)`, which runs only
//! the seam's page action kinds (`frame-seam.mjs PAGE_ACTIONS`). An action is never sent twice.
//!
//! Contract with Analytics (perf/analytics-native fix round, L2): every `dashboard` view goes to
//! `analytics::feed::dashboard_view` before Home draws it (`deliver`, `FOLLOW_DASHBOARD`). The Analytics controller
//! relies on it to notice an account hidden, shown or removed on Home or Accounts and redraw while its tab is away.
//! Bridged, the frame called it for the iframe's views; de-bridged, the views come from here.

use crate::Page;
use leptos::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The id of the controller this page installed (0 until then): every call goes to that one only.
    controller: RwSignal<u64>,
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
            controller: RwSignal::new(0),
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
        let _ = self.controller.try_set(0);
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
        call_on(
            self.controller.get_untracked(),
            "act",
            &[kind.into(), value.into()],
        )
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    }

    /// The header's Refresh or Update apps. Held until the controller has started, then run once.
    pub fn command(&self, action: &str) {
        if self.live.get_untracked() {
            call_on(self.controller.get_untracked(), "command", &[action.into()]);
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
        call_on(self.controller.get_untracked(), "signOut", &[]).map(|_| ())
    }
}

fn controller() -> Option<JsValue> {
    js_sys::Reflect::get(&window(), &"aacFrameDashboard".into())
        .ok()
        .filter(|v| v.is_object())
}

/// A controller's id (`installFrameDashboard` numbers them from 1); 0 for anything else.
fn id_of(controller: &JsValue) -> u64 {
    js_sys::Reflect::get(controller, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .filter(|v| *v >= 1.0 && v.fract() == 0.0)
        .map(|v| v as u64)
        .unwrap_or(0)
}

/// Calls one of the controller's methods only if it is the one with this id (this page's).
fn call_on(id: u64, method: &str, args: &[JsValue]) -> Option<JsValue> {
    let target = controller()?;
    if id == 0 || id_of(&target) != id {
        return None;
    }
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

/// Where a `dashboard` view goes before Home draws it: Analytics' follower (the L2 contract above).
pub(crate) const FOLLOW_DASHBOARD: fn(&str) = crate::analytics::feed::dashboard_view;

/// One view from the controller: a `dashboard` view goes to `follow` first (in the frame always
/// `FOLLOW_DASHBOARD`), then every view lands in its signal.
pub(crate) fn deliver(dash: &DashCtx, key: &str, json: String, follow: fn(&str)) {
    if key == "dashboard" {
        // an account hidden, shown or removed: Analytics follows while its tab is away
        follow(&json);
    }
    dash.receive(key, json);
}

/// Loads the controller and follows its views. `route` is the tab on screen (Accounts & Settings reads its own blocks
/// when it comes on screen); `noted` gets each toast and the end of the session. Everything stops with the owner
/// (sign-out).
pub(crate) fn start(dash: DashCtx, route: Signal<Page>, noted: impl Fn(Note) + 'static) {
    let ready = RwSignal::new(false);
    // Whether the page is still there, and the controller it installed: read after its signals are gone (sign-out).
    let alive = Arc::new(AtomicBool::new(true));
    let own = Arc::new(AtomicU64::new(0));
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
            None => deliver(&dash, &key, json, FOLLOW_DASHBOARD),
        }
    });
    let load = js_sys::Reflect::get(&window(), &"aacLoadDashboard".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call0(&JsValue::NULL).ok())
        .and_then(|p| p.dyn_into::<js_sys::Promise>().ok());
    match load {
        Some(promise) => {
            let (alive, own) = (alive.clone(), own.clone());
            leptos::task::spawn_local(async move {
                let module = wasm_bindgen_futures::JsFuture::from(promise).await;
                // signed out while it loaded: this page is gone, and it installs nothing
                if !alive.load(Ordering::Relaxed) {
                    return;
                }
                let installed = module
                    .ok()
                    .and_then(|m| js_sys::Reflect::get(&m, &"installFrameDashboard".into()).ok())
                    .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    .and_then(|f| f.call1(&JsValue::NULL, &window()).ok())
                    .map(|c| id_of(&c))
                    .filter(|id| *id != 0);
                let Some(id) = installed else {
                    dash.failed();
                    return;
                };
                own.store(id, Ordering::Relaxed);
                let _ = dash.controller.try_set(id);
                let _ = ready.try_set(true);
            });
        }
        None => dash.failed(),
    }
    Effect::new(move |started: Option<bool>| {
        if !ready.get() {
            return false;
        }
        let id = dash.controller.get_untracked();
        call_on(id, "page", &[route.get().id().into()]);
        if started != Some(true) {
            call_on(id, "start", &[]);
            dash.live.set(true);
            for action in dash.waiting.try_update(std::mem::take).unwrap_or_default() {
                call_on(id, "command", &[action.as_str().into()]);
            }
        }
        true
    });
    on_cleanup(move || {
        alive.store(false, Ordering::Relaxed);
        listener.remove();
        call_on(own.load(Ordering::Relaxed), "stop", &[]);
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

    thread_local! {
        static FOLLOWED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    #[test]
    fn de_bridged_dashboard_views_still_reach_analytics_first() {
        // The L2 contract: the frame's own dashboard views (Home and Accounts no longer bridged) go to
        // analytics::feed::dashboard_view, as the iframe's did, and only `dashboard` views do.
        assert!(std::ptr::fn_addr_eq(
            FOLLOW_DASHBOARD,
            crate::analytics::feed::dashboard_view as fn(&str)
        ));
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        fn record(json: &str) {
            FOLLOWED.with(|f| f.borrow_mut().push(json.to_string()));
        }
        let home = r#"{"version":2,"sections":[{"id":"codex","rows":[{"id":"codex:one"}]}]}"#;
        deliver(&dash, "accounts", "{\"version\":3}".into(), record);
        deliver(&dash, "details", "{\"id\":\"codex:one\"}".into(), record);
        deliver(&dash, "dashboard", home.into(), record);
        FOLLOWED.with(|f| assert_eq!(*f.borrow(), vec![home.to_string()]));
        // and Home still draws it
        assert_eq!(dash.dashboard.get_untracked(), home);
        assert_eq!(dash.accounts.get_untracked(), "{\"version\":3}");
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
