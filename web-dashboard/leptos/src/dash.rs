//! The dashboard seam for the frame's native pages (Accounts & Settings, Details, Analytics, and Home once native).
//!
//! `bridge.js` keeps the network, the session, the timers, the accounts controller, switch approvals and every server
//! call. It runs in the frame's hidden dashboard iframe (`/?content=1`) and, once the frame subscribes, sends each view
//! it hands Slint as `{type: "aac-content", kind: "view", key, json}` (`public/frame-seam.mjs`). The pages here draw
//! that JSON (same versions and fields) and send its action kinds back with [`DashCtx::act`], which bridge.js runs as
//! its own (`{type: "aac-page", kind, value}`, an allowlist on its side). An action is never sent twice.
//!
//! The contract for both builders is `page-accounts/SEAM.md` in the share folder.

use leptos::prelude::*;

/// The overlays the frame draws itself; bridge.js stops drawing them in Slint once the frame says so: "details" (the
/// Details panel and sheet, details.rs) and "activation" (the switch approval dialog, activation.rs).
pub const OVERLAYS: &[&str] = &["details", "activation"];

/// Whether the frame draws this overlay itself.
pub fn owns(overlay: &str) -> bool {
    OVERLAYS.contains(&overlay)
}

/// How many times a page asked for Details: a click outside the open panel that asked for another account swaps it
/// instead of closing it (details.rs, as bridge.js `detailsOpens` does for the canvas).
pub static DETAILS_ASKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The pages the frame draws itself (`FRAME_PAGES` in frame-seam.mjs), named in the same subscription: bridge.js
/// then sends their views here only and takes their layout reports from here only.
pub const PAGES: &[&str] = &["analytics"];

/// The latest view of each key, as raw JSON (each page parses its own types). Empty until the first push.
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
    /// Analytics, `ANALYTICS_VIEW_VERSION` 7 (`set_analytics`); `usage.trend.paths` is the shape to end on.
    pub analytics: RwSignal<String>,
    /// How the trend reaches that shape: "morph" (glide) or "static". Arrives just before `analytics`.
    pub analytics_motion: RwSignal<String>,
    /// The Analytics header between refreshes (`set_analytics_head`).
    pub analytics_head: RwSignal<String>,
    /// `{"loading": bool, "error": "..."}` (`set_analytics_loading`).
    pub analytics_loading: RwSignal<String>,
    /// The dashboard has started and signed in: only then do actions go to it.
    pub(crate) live: RwSignal<bool>,
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
            analytics: RwSignal::new(String::new()),
            analytics_motion: RwSignal::new(String::new()),
            analytics_head: RwSignal::new(String::new()),
            analytics_loading: RwSignal::new(String::new()),
            live: RwSignal::new(false),
        }
    }

    /// One view from the dashboard. Unknown keys are ignored.
    pub(crate) fn receive(&self, key: &str, json: String) {
        crate::ui::reconcile_overlay_focus(|| match key {
            "dashboard" => self.dashboard.set(json),
            "accounts" => self.accounts.set(json),
            "accounts-strength" => self.strength.set(json),
            "details" => self.details.set(json),
            "activation" => self.activation.set(json),
            "update" => self.update.set(json),
            "busy" => self.busy.set(json == "true"),
            "refresh-interval" => self.refresh_interval.set(json),
            "analytics" => self.analytics.set(json),
            "analytics-motion" => self.analytics_motion.set(json),
            "analytics-head" => self.analytics_head.set(json),
            "analytics-loading" => self.analytics_loading.set(json),
            _ => {}
        });
    }

    /// The dashboard's page started again: what its overlays showed belongs to the page that is gone.
    pub(crate) fn restarted(&self) {
        self.live.set(false);
        self.details.set(String::new());
        self.activation.set(String::new());
        self.busy.set(false);
        self.analytics_loading.set(String::new());
    }

    /// The dashboard's page is gone (sign-out): nothing of that session stays on screen.
    pub(crate) fn clear(&self) {
        for signal in [
            self.dashboard,
            self.accounts,
            self.strength,
            self.details,
            self.activation,
            self.update,
            self.refresh_interval,
            self.analytics,
            self.analytics_motion,
            self.analytics_head,
            self.analytics_loading,
        ] {
            let _ = signal.try_set(String::new());
        }
        let _ = self.busy.try_set(false);
        let _ = self.live.try_set(false);
    }

    /// Runs one of the dashboard's page actions. False, and nothing sent, while the dashboard is not started and
    /// signed in (the page has no data to act on then either).
    pub fn act(&self, kind: &str, value: &str) -> bool {
        if !self.live.get_untracked() {
            return false;
        }
        if kind == "details" {
            DETAILS_ASKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let Some(target) = crate::frame::slint_window() else {
            return false;
        };
        let message = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&message, &"type".into(), &"aac-page".into());
        let _ = js_sys::Reflect::set(&message, &"kind".into(), &kind.into());
        let _ = js_sys::Reflect::set(&message, &"value".into(), &value.into());
        target
            .post_message(&message, &window().location().origin().unwrap_or_default())
            .is_ok()
    }
}

/// Asks the dashboard for its views (and says which overlays the frame draws). Sent when its page says it is ready.
pub(crate) fn subscribe(target: &web_sys::Window) {
    let message = js_sys::Object::new();
    let overlays = js_sys::Array::new();
    for name in OVERLAYS.iter().chain(PAGES) {
        overlays.push(&(*name).into());
    }
    let _ = js_sys::Reflect::set(&message, &"type".into(), &"aac-views".into());
    let _ = js_sys::Reflect::set(&message, &"overlays".into(), &overlays);
    let _ = target.post_message(&message, &window().location().origin().unwrap_or_default());
}

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
    fn views_land_in_their_signal_and_a_restart_closes_the_overlays() {
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        dash.receive("accounts", "{\"version\":3}".into());
        dash.receive("details", "{\"id\":\"a\"}".into());
        dash.receive("busy", "true".into());
        dash.receive("analytics", "{\"version\":7}".into());
        dash.receive("analytics-loading", "{\"loading\":true}".into());
        dash.receive("unknown", "x".into());
        assert_eq!(dash.accounts.get_untracked(), "{\"version\":3}");
        assert!(dash.busy.get_untracked());
        assert_eq!(dash.analytics.get_untracked(), "{\"version\":7}");
        dash.restarted();
        assert!(dash.details.get_untracked().is_empty());
        assert!(dash.analytics_loading.get_untracked().is_empty());
        assert_eq!(dash.analytics.get_untracked(), "{\"version\":7}");
        assert_eq!(dash.accounts.get_untracked(), "{\"version\":3}");
        dash.clear();
        assert!(dash.accounts.get_untracked().is_empty());
        assert!(dash.analytics.get_untracked().is_empty());
    }

    #[test]
    fn nothing_is_sent_before_the_dashboard_is_live() {
        let owner = Owner::new();
        owner.set();
        let dash = DashCtx::new();
        assert!(!dash.act("remove", "claude:a"));
    }
}
