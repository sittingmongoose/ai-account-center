//! The loading splash (owner report 2026-11-20: "it shows the empty login screen then that goes away and the page
//! loads ... It should be some kind of loading splash screen"): the app's mark and name over a quiet progress bar, in
//! the current theme. `index.html` draws it (`#aac-boot`) before the app starts, and it stays until the frame knows
//! whether this browser is signed in and the first tab has its first data (frame.rs `follow_boot`); the sign-in form
//! is never drawn while that is unknown. A page's first read shows the same splash in its content area
//! (`PageSplash`); later refreshes never do. The splash replaces only what is drawn: every sign-in check runs as before.

use super::icons::Logo;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// index.html's splash, drawn before the app starts.
pub const BOOT_ID: &str = "aac-boot";

/// How long the whole-page splash waits for the first tab's data before the tab draws its own splash.
pub const BOOT_DATA_WAIT_MS: u64 = 4_000;

/// How long the whole-page splash takes to fade (app.css `.boot-splash.boot-done`).
const FADE_MS: i32 = 200;

/// Whether index.html's splash is still on the page.
pub fn boot_shown() -> bool {
    document().get_element_by_id(BOOT_ID).is_some()
}

/// index.html's splash leaves: with a short fade when motion is on (it stops taking clicks at once), else at once.
/// Once gone it never comes back.
pub fn dismiss_boot(fade: bool) {
    let Some(boot) = document().get_element_by_id(BOOT_ID) else {
        return;
    };
    // a new id, so a second call can't find it, and the fade's removal is its own
    boot.remove_attribute("id").ok();
    if !fade {
        boot.remove();
        return;
    }
    let _ = boot.class_list().add_1("boot-done");
    let remove = wasm_bindgen::closure::Closure::once_into_js(move || boot.remove());
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(remove.unchecked_ref(), FADE_MS);
}

fn splash_body(name: String) -> impl IntoView {
    view! {
        <div class="splash-in">
            <div class="splash-brand"><Logo/><span class="brand-name">{name}</span></div>
            <div class="splash-bar" aria-hidden="true"><i></i></div>
        </div>
    }
}

/// The whole-page splash, as index.html draws it: only for a start whose page has no `#aac-boot` (a page an older
/// service worker kept), so the two never stack.
#[component]
pub fn Splash() -> impl IntoView {
    view! {
        <div class="boot-splash" role="status" aria-label="AI Account Center is loading">
            {splash_body("AI Account Center".into())}
        </div>
    }
}

/// A page's first read: the same splash in the page's content area, named after the page.
#[component]
pub fn PageSplash(name: &'static str) -> impl IntoView {
    view! {
        <div class="page-splash" role="status" aria-label=format!("Loading {name}")>
            {splash_body(name.into())}
        </div>
    }
}
