//! Analytics' own data: the frame reads `/api/accounts/analytics` itself (`public/frame-analytics.mjs`), as Status reads
//! `/api/status`, and builds the view with the same builders bridge.js uses (`ANALYTICS_VIEW_VERSION` 7). No Slint
//! dashboard is loaded for this tab and nothing waits on its bridge.
//!
//! `index.html` installs `window.aacLoadAnalytics()` (a dynamic import, started early on `/app/analytics`); it resolves
//! to the module, and the page installs its controller (`installFrameAnalytics`, which makes it
//! `window.aacFrameAnalytics`) only if it is still there. Its views arrive as `aac-analytics` events (`{key, json}`) and
//! the page's actions go back through `aacFrameAnalytics.act(kind, value)`. A page drives and stops only the controller
//! it installed (its `id`): a load that lands after sign-out never touches the next session's (review L4).

use leptos::prelude::*;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use wasm_bindgen::{JsCast, JsValue};

/// The latest of each view, as raw JSON (the page parses its own types). Empty until the first answer: the page shows
/// its loading state, never zeros.
#[derive(Clone, Copy)]
pub struct Feed {
    /// Analytics, `ANALYTICS_VIEW_VERSION` 7; `usage.trend.paths` is the shape to end on.
    pub analytics: RwSignal<String>,
    /// How the trend reaches that shape: "morph" (glide) or "static". Arrives just before `analytics`.
    pub motion: RwSignal<String>,
    /// The header between refreshes.
    pub head: RwSignal<String>,
    /// `{"loading": bool, "error": "..."}`.
    pub loading: RwSignal<String>,
    /// `{"shown", "updating", "stale", "asOf", "note"}`: whether the numbers on screen are current and whether newer ones
    /// are on their way (the page's updating strip).
    pub fresh: RwSignal<String>,
    /// The frame header's line for this tab (`{statusLead, statusStrong, statusMore}`, as the dashboard words it).
    pub chrome: RwSignal<String>,
    /// The id of the controller this page installed; 0 until then.
    pub(crate) controller: RwSignal<u64>,
}

/// The frame header's line, as the controller sends it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChromeLine {
    #[serde(deserialize_with = "crate::dash::nullable")]
    pub status_lead: String,
    #[serde(deserialize_with = "crate::dash::nullable")]
    pub status_strong: String,
    #[serde(deserialize_with = "crate::dash::nullable")]
    pub status_more: String,
}

/// Said when the controller could not be loaded at all (no module, or an old page without the loader).
const NO_START: &str =
    r#"{"loading":false,"error":"The Analytics page could not start. Reload the app."}"#;

impl Feed {
    pub fn new() -> Self {
        Feed {
            analytics: RwSignal::new(String::new()),
            motion: RwSignal::new(String::new()),
            head: RwSignal::new(String::new()),
            loading: RwSignal::new(String::new()),
            fresh: RwSignal::new(String::new()),
            chrome: RwSignal::new(String::new()),
            controller: RwSignal::new(0),
        }
    }

    /// One view from the controller. Unknown keys are ignored; a page already gone takes nothing.
    pub(crate) fn receive(&self, key: &str, json: String) {
        crate::ui::reconcile_overlay_focus(|| {
            let _ = match key {
                "analytics" => self.analytics.try_set(json),
                "analytics-motion" => self.motion.try_set(json),
                "analytics-head" => self.head.try_set(json),
                "analytics-loading" => self.loading.try_set(json),
                "analytics-fresh" => self.fresh.try_set(json),
                "chrome" => self.chrome.try_set(json),
                _ => None,
            };
        });
    }

    /// Runs one of the page's action kinds. False, and nothing done, until the controller has loaded (a layout report
    /// is then sent again with the first view).
    pub fn act(&self, kind: &str, value: &str) -> bool {
        let id = self.controller.try_get_untracked().unwrap_or(0);
        call_on(id, "act", &[kind.into(), value.into()])
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// The frame header's line for this tab.
    pub fn chrome_line(&self) -> Memo<ChromeLine> {
        let chrome = self.chrome;
        Memo::new(move |_| {
            chrome.with(|j| serde_json::from_str::<ChromeLine>(j).unwrap_or_default())
        })
    }
}

impl Default for Feed {
    fn default() -> Self {
        Self::new()
    }
}

fn controller() -> Option<JsValue> {
    js_sys::Reflect::get(&window(), &"aacFrameAnalytics".into())
        .ok()
        .filter(|v| v.is_object())
}

/// A controller's id (`installFrameAnalytics` numbers them from 1); 0 for anything else.
fn id_of(controller: &JsValue) -> u64 {
    js_sys::Reflect::get(controller, &"id".into())
        .ok()
        .and_then(|v| v.as_f64())
        .filter(|v| *v >= 1.0 && v.fract() == 0.0)
        .map(|v| v as u64)
        .unwrap_or(0)
}

/// Calls one of the current controller's methods, whichever page installed it.
fn call(method: &str, args: &[JsValue]) -> Option<JsValue> {
    apply(&controller()?, method, args)
}

/// Calls one of the controller's methods only if it is the one with this id (this page's).
fn call_on(id: u64, method: &str, args: &[JsValue]) -> Option<JsValue> {
    let target = controller()?;
    if id == 0 || id_of(&target) != id {
        return None;
    }
    apply(&target, method, args)
}

fn apply(target: &JsValue, method: &str, args: &[JsValue]) -> Option<JsValue> {
    let f = js_sys::Reflect::get(target, &method.into()).ok()?;
    let f = f.dyn_ref::<js_sys::Function>()?;
    let list = js_sys::Array::new();
    for a in args {
        list.push(a);
    }
    f.apply(target, &list).ok()
}

fn read_str(data: &JsValue, key: &str) -> String {
    js_sys::Reflect::get(data, &key.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default()
}

/// The ids a dashboard view shows (every string `id`: accounts and providers), sorted and once each. Hiding, showing
/// or removing an account changes them; a clock tick (the view's relative times) does not.
pub(crate) fn view_ids(json: &str) -> Vec<String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Array(all) => all.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(map) => {
                for (k, x) in map {
                    if k == "id" {
                        out.extend(x.as_str().map(str::to_string));
                    }
                    walk(x, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
        walk(&v, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

thread_local! {
    /// The ids of the last dashboard view the frame was sent.
    static DASHBOARD_IDS: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// The dashboard sent Home and Accounts a new view (bridge.js). When the accounts in it changed (one hidden, shown or
/// removed), the Analytics controller reads the dashboard again while its tab is away, so it never comes back with an
/// account drawn the old way (frame-analytics.mjs `dashboardChanged`); a view that only ticked reads nothing.
pub(crate) fn dashboard_view(json: &str) {
    let ids = view_ids(json);
    let changed = DASHBOARD_IDS.with(|last| {
        let mut last = last.borrow_mut();
        let changed = last.as_ref() != Some(&ids);
        *last = Some(ids);
        changed
    });
    if changed {
        call("dashboardChanged", &[]);
    }
}

/// Loads the controller and follows its views. `shown` says whether the Analytics tab is on screen (reads and timers
/// run only then); `ended` runs when a read found the session gone (its reason: revoked, or empty). Everything stops
/// with the owner (sign-out).
pub fn start(shown: Signal<bool>, ended: impl Fn(String) + 'static) -> Feed {
    let feed = Feed::new();
    let ready = RwSignal::new(false);
    // Whether the page is still there, and the controller it installed: read after its signals are gone (sign-out).
    let alive = Arc::new(AtomicBool::new(true));
    let own = Arc::new(AtomicU64::new(0));
    let listener = window_event_listener_untyped("aac-analytics", move |event| {
        let Some(event) = event.dyn_ref::<web_sys::CustomEvent>() else {
            return;
        };
        let detail = event.detail();
        let key = read_str(&detail, "key");
        let json = read_str(&detail, "json");
        if key == "signed-out" {
            ended(json);
            return;
        }
        feed.receive(&key, json);
    });
    let load = js_sys::Reflect::get(&window(), &"aacLoadAnalytics".into())
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
                let controller = module
                    .ok()
                    .and_then(|m| js_sys::Reflect::get(&m, &"installFrameAnalytics".into()).ok())
                    .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    .and_then(|f| f.call1(&JsValue::NULL, &window()).ok())
                    .map(|c| id_of(&c))
                    .filter(|id| *id != 0);
                let Some(id) = controller else {
                    let _ = feed.loading.try_set(NO_START.into());
                    return;
                };
                own.store(id, Ordering::Relaxed);
                let _ = feed.controller.try_set(id);
                let _ = ready.try_set(true);
            });
        }
        None => feed.loading.set(NO_START.into()),
    }
    Effect::new(move |_| {
        if !ready.get() {
            return;
        }
        let id = feed.controller.get_untracked();
        call_on(id, if shown.get() { "show" } else { "hide" }, &[]);
    });
    on_cleanup(move || {
        alive.store(false, Ordering::Relaxed);
        listener.remove();
        call_on(own.load(Ordering::Relaxed), "stop", &[]);
    });
    feed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_land_in_their_signal() {
        let owner = Owner::new();
        owner.set();
        let feed = Feed::new();
        feed.receive("analytics", "{\"version\":7}".into());
        feed.receive("analytics-motion", "morph".into());
        feed.receive(
            "analytics-loading",
            "{\"loading\":true,\"error\":\"\"}".into(),
        );
        feed.receive(
            "chrome",
            "{\"statusLead\":\"Updated\",\"statusStrong\":\"1m ago\",\"statusMore\":\"live\"}"
                .into(),
        );
        feed.receive("analytics-fresh", "{\"shown\":true}".into());
        feed.receive("unknown", "x".into());
        assert_eq!(feed.analytics.get_untracked(), "{\"version\":7}");
        assert_eq!(feed.fresh.get_untracked(), "{\"shown\":true}");
        assert_eq!(feed.motion.get_untracked(), "morph");
        assert!(feed.head.get_untracked().is_empty());
        let line = feed.chrome_line();
        assert_eq!(line.get_untracked().status_strong, "1m ago");
        assert_eq!(line.get_untracked().status_more, "live");
    }

    #[test]
    fn a_chrome_line_with_nulls_or_garbage_reads_empty() {
        let owner = Owner::new();
        owner.set();
        let feed = Feed::new();
        feed.receive("chrome", "{\"statusLead\":null}".into());
        assert_eq!(feed.chrome_line().get_untracked(), ChromeLine::default());
        feed.receive("chrome", "not json".into());
        assert_eq!(feed.chrome_line().get_untracked(), ChromeLine::default());
    }

    #[test]
    fn a_dashboard_view_changes_its_ids_only_when_its_accounts_change() {
        let a = r#"{"sections":[{"id":"codex","cards":[{"id":"codex:one","line":"1m ago"},{"id":"codex:two"}]}]}"#;
        let ticked = r#"{"sections":[{"id":"codex","cards":[{"id":"codex:two"},{"id":"codex:one","line":"2m ago"}]}]}"#;
        let hidden =
            r#"{"sections":[{"id":"codex","cards":[{"id":"codex:two","line":"2m ago"}]}]}"#;
        assert_eq!(view_ids(a), vec!["codex", "codex:one", "codex:two"]);
        assert_eq!(view_ids(a), view_ids(ticked));
        assert_ne!(view_ids(a), view_ids(hidden));
        assert!(view_ids("").is_empty() && view_ids("not json").is_empty());
    }

    #[test]
    fn a_failed_start_says_so_in_the_load_state() {
        // the page reads this as its banner (view::LoadState), never as zeros
        let state: super::super::view::LoadState =
            serde_json::from_str(NO_START).expect("load state");
        assert!(!state.loading && state.error.contains("could not start"));
    }
}
