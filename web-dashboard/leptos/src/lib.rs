//! AI Account Center pages drawn with Leptos (client-side rendered, served by the TypeScript server).
//!
//! Opened on its own, `/app/<page>` is the app frame (PLAN step 4): the header, tab bar, Duo strip and band, sign-in,
//! and all five tabs, with Status and Scoreboard drawn here and Home, Analytics and Accounts shown from the Slint
//! dashboard content-only. With `?embed=1` today's Slint shell shows the Status or Scoreboard content in its iframe
//! and sends the theme (`{type: "aac-theme", mode}`).

pub mod api;
mod blocks;
mod cards;
pub mod dash;
mod drafts;
pub mod fmt;
mod frame;
mod live;
mod media;
mod owned;
mod scoreboard;
mod session;
mod status;
mod ui;

use leptos::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use wasm_bindgen::JsCast;

/// True while the shell shows another tab: a hidden iframe's document still reports itself visible.
static HIDDEN_BY_SHELL: AtomicBool = AtomicBool::new(false);
/// The page on screen: in the shell's iframe it is the page itself; in the app frame it follows the tabs.
static ACTIVE: AtomicU8 = AtomicU8::new(Page::Status as u8);

/// Whether `page` is on screen: the tab is visible, the shell shows this page, and the frame's tab is this page.
pub fn page_on_screen(page: Page) -> bool {
    document().visibility_state() == web_sys::VisibilityState::Visible
        && !HIDDEN_BY_SHELL.load(Ordering::Relaxed)
        && ACTIVE.load(Ordering::Relaxed) == page as u8
}

/// The frame shows `page`: readers of a page that comes back on screen read at once (`SHOWN_EVENT`).
pub fn set_active(page: Page) {
    let was = ACTIVE.swap(page as u8, Ordering::Relaxed);
    if was != page as u8 {
        if let Ok(shown) = web_sys::Event::new(SHOWN_EVENT) {
            let _ = window().dispatch_event(&shown);
        }
    }
}

/// Fired on the window when the shell shows this page again, so readers refresh at once.
pub const SHOWN_EVENT: &str = "aac-shown";
use wasm_bindgen::prelude::*;

/// The five tabs. Home, Analytics and Accounts are the Slint dashboard's pages, shown content-only inside the frame
/// until they are ported; Status and Scoreboard are drawn here. Home, Analytics and Accounts take their data from the
/// dashboard (`bridged`), drawn by Slint or natively (dash.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Home = 0,
    Analytics = 1,
    Accounts = 2,
    Status = 3,
    Scoreboard = 4,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::Home,
        Page::Analytics,
        Page::Accounts,
        Page::Status,
        Page::Scoreboard,
    ];

    /// The frame's address for the page.
    pub fn path(self) -> &'static str {
        match self {
            Page::Home => "/app/home",
            Page::Analytics => "/app/analytics",
            Page::Accounts => "/app/accounts",
            Page::Status => "/app/status",
            Page::Scoreboard => "/app/scoreboard",
        }
    }

    /// The page's id in the Slint dashboard (bridge.js `navigate`).
    pub fn id(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Analytics => "analytics",
            Page::Accounts => "accounts",
            Page::Status => "status",
            Page::Scoreboard => "scoreboard",
        }
    }

    pub fn from_id(id: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Analytics => "Analytics",
            Page::Accounts => "Accounts",
            Page::Status => "Status",
            Page::Scoreboard => "Scoreboard",
        }
    }

    /// The page's name where the chrome has room for it (the Duo band).
    pub fn title(self) -> &'static str {
        match self {
            Page::Accounts => "Accounts & Settings",
            Page::Scoreboard => "Model Scoreboard",
            other => other.label(),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Analytics => "chart",
            Page::Accounts => "users",
            Page::Status => "activity",
            Page::Scoreboard => "trophy",
        }
    }

    /// Drawn by the Slint dashboard, inside the frame.
    pub fn slint(self) -> bool {
        matches!(self, Page::Home | Page::Analytics | Page::Accounts)
    }

    /// Its data, actions and header line come from the dashboard (bridge.js in the frame's hidden iframe, dash.rs): the
    /// frame loads the dashboard for it and moves it to this page.
    pub fn bridged(self) -> bool {
        matches!(self, Page::Home | Page::Analytics | Page::Accounts)
    }
}

pub(crate) fn page_from_path(path: &str) -> Page {
    let path = path.trim_end_matches('/');
    Page::ALL
        .into_iter()
        .find(|p| path.ends_with(&format!("/{}", p.id())))
        .unwrap_or(Page::Status)
}

fn embedded(search: &str) -> bool {
    web_sys::UrlSearchParams::new_with_str(search)
        .ok()
        .and_then(|p| p.get("embed"))
        .as_deref()
        == Some("1")
}

pub(crate) fn set_theme(mode: &str) {
    if let Some(root) = document().document_element() {
        match mode {
            "dark" | "light" => {
                let _ = root.set_attribute("data-theme", mode);
            }
            _ => {
                let _ = root.remove_attribute("data-theme");
            }
        }
    }
}

/// Same-origin messages from the shell only; anything else is ignored.
fn listen_to_shell() {
    let origin = window().location().origin().unwrap_or_default();
    let handler =
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            // Only the shell that framed this page: same origin and the parent window itself, not any other
            // same-origin window or frame.
            let from_parent = match (event.source(), window().parent().ok().flatten()) {
                (Some(source), Some(parent)) => {
                    let parent: &wasm_bindgen::JsValue = parent.as_ref();
                    js_sys::Object::is(&source, parent)
                }
                _ => false,
            };
            if event.origin() != origin || !from_parent {
                return;
            }
            let data = event.data();
            let kind = js_sys::Reflect::get(&data, &"type".into())
                .ok()
                .and_then(|v| v.as_string());
            if kind.as_deref() == Some("aac-visible") {
                let visible = js_sys::Reflect::get(&data, &"visible".into())
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let was_hidden = HIDDEN_BY_SHELL.swap(!visible, Ordering::Relaxed);
                if visible && was_hidden {
                    if let Ok(shown) = web_sys::Event::new(SHOWN_EVENT) {
                        let _ = window().dispatch_event(&shown);
                    }
                }
                return;
            }
            if kind.as_deref() == Some("aac-theme") {
                if let Some(mode) = js_sys::Reflect::get(&data, &"mode".into())
                    .ok()
                    .and_then(|v| v.as_string())
                {
                    set_theme(&mode);
                }
            }
        });
    let _ = window().add_event_listener_with_callback("message", handler.as_ref().unchecked_ref());
    handler.forget();
    // Tell the shell the page is ready so it sends the theme.
    if let Ok(Some(parent)) = window().parent() {
        let message = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&message, &"type".into(), &"aac-ready".into());
        let _ = parent.post_message(&message, &window().location().origin().unwrap_or_default());
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let location = window().location();
    let page = page_from_path(&location.pathname().unwrap_or_default());
    let embed = embedded(&location.search().unwrap_or_default());
    ui::device::start(embed);
    if embed {
        // Today's Slint shell shows this page in its iframe: the content only.
        ACTIVE.store(page as u8, Ordering::Relaxed);
        listen_to_shell();
        mount_to_body(move || {
            media::provide_viewer();
            let page_view = match page {
                Page::Scoreboard => {
                    view! { <scoreboard::ScoreboardPage embedded=true/> }.into_any()
                }
                _ => {
                    provide_context(live::start());
                    view! { <status::StatusPage embedded=true/> }.into_any()
                }
            };
            view! {
                {page_view}
                <media::ViewerOverlay/>
            }
        });
        return;
    }
    // Opened on its own: the app frame, with every tab in it.
    ACTIVE.store(page as u8, Ordering::Relaxed);
    mount_to_body(move || {
        media::provide_viewer();
        view! {
            <frame::Frame initial=page/>
            <media::ViewerOverlay/>
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_pick_the_page() {
        assert!(page_from_path("/app/status") == Page::Status);
        assert!(page_from_path("/app/scoreboard") == Page::Scoreboard);
        assert!(page_from_path("/app/scoreboard/") == Page::Scoreboard);
        assert!(page_from_path("/app/home") == Page::Home);
        assert!(page_from_path("/app/analytics") == Page::Analytics);
        assert!(page_from_path("/app/accounts") == Page::Accounts);
        assert!(page_from_path("/app/") == Page::Status);
    }

    #[test]
    fn each_page_has_its_own_address_and_dashboard_id() {
        for page in Page::ALL {
            assert_eq!(page_from_path(page.path()), page);
            assert_eq!(Page::from_id(page.id()), Some(page));
        }
        assert!(Page::Home.slint() && Page::Accounts.slint() && !Page::Status.slint());
        assert!(Page::Accounts.bridged() && Page::Analytics.bridged() && !Page::Status.bridged());
    }
}
