//! AI Account Center pages drawn with Leptos (client-side rendered, served by the TypeScript server).
//!
//! The server answers every page address with this app (`/app/<page>`, and the classic app's `/`, `/analytics`,
//! `/accounts/<provider>` and the rest, which open the same tabs: `page_from_location`). It is the app frame (PLAN step
//! 4): the header, tab bar, Duo strip and band, sign-in, and all five tabs, every one drawn here and reading its own
//! data: Home and Accounts the dashboard's (dash.rs, `public/frame-dashboard.mjs`). The Slint app it replaced was
//! retired in Leptos finish (step 9). With `?embed=1` a Status or Scoreboard page is its content alone (it was shown in
//! the retired Slint shell's iframe, which sent the theme as `{type: "aac-theme", mode}`).

mod accounts;
mod activation;
mod analytics;
pub mod api;
mod blocks;
mod cards;
pub mod dash;
mod details;
mod drafts;
pub mod fmt;
mod frame;
mod home;
mod live;
mod media;
mod owned;
mod pins;
mod question_index;
mod resets;
mod roster;
mod scoreboard;
pub mod scoreboard_arrange;
mod session;
mod status;
mod summary;
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
    document().visibility_state() == web_sys::VisibilityState::Visible && owns_page(page)
}

/// Logical ownership ignores backgrounding: overlays stay open across app suspension, but not another route.
pub(crate) fn owns_page(page: Page) -> bool {
    !HIDDEN_BY_SHELL.load(Ordering::Relaxed) && ACTIVE.load(Ordering::Relaxed) == page as u8
}

pub(crate) fn active_page() -> Page {
    Page::ALL
        .into_iter()
        .find(|p| *p as u8 == ACTIVE.load(Ordering::Relaxed))
        .unwrap_or(Page::Status)
}

pub(crate) const PAGE_EVENT: &str = "aac-page";

fn page_owner_changed() {
    if let Ok(event) = web_sys::Event::new(PAGE_EVENT) {
        let _ = window().dispatch_event(&event);
    }
}

/// The frame shows `page`: readers of a page that comes back on screen read at once (`SHOWN_EVENT`).
pub fn set_active(page: Page) {
    let was = ACTIVE.swap(page as u8, Ordering::Relaxed);
    if was != page as u8 {
        page_owner_changed();
        if let Ok(shown) = web_sys::Event::new(SHOWN_EVENT) {
            let _ = window().dispatch_event(&shown);
        }
    }
}

/// Fired on the window when the shell shows this page again, so readers refresh at once.
pub const SHOWN_EVENT: &str = "aac-shown";
use wasm_bindgen::prelude::*;

/// The five tabs, all drawn here. Home and Accounts take the dashboard's data (`reads_dashboard`, dash.rs), read by the
/// frame itself; no tab loads or shows the Slint dashboard any more.
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

    /// Its data, actions and header line come from the dashboard controller (dash.rs, `public/frame-dashboard.mjs`),
    /// which the frame starts the first time such a tab is shown; until G5 these were the bridged tabs (`bridged()`).
    /// Analytics reads its own (analytics/feed.rs). Contract (analytics-native L2): the `dashboard` view these tabs
    /// draw also goes to `analytics::feed::dashboard_view` (dash.rs `deliver`), so Analytics follows an account hidden,
    /// shown or removed here while its own tab is away.
    pub fn reads_dashboard(self) -> bool {
        matches!(self, Page::Home | Page::Accounts)
    }
}

pub(crate) fn page_from_path(path: &str) -> Page {
    let path = path.trim_end_matches('/');
    Page::ALL
        .into_iter()
        .find(|p| path.ends_with(&format!("/{}", p.id())))
        .unwrap_or(Page::Status)
}

/// The tab an address opens. The frame's own addresses are `/app/<page>` (anything else under `/app` is Status, as
/// before). The classic app's addresses, which bookmarks, old links and the installed app's start page still use, open
/// the same tabs (the rules of the classic `page-route.mjs`): `?view=<page>` first, then `/analytics`, `/accounts` and
/// `/accounts/<provider>`, `/status` and `/scoreboard`; `/`, `/login` and anything else is Home.
pub(crate) fn page_from_location(path: &str, search: &str) -> Page {
    if path == "/app" || path.starts_with("/app/") {
        return page_from_path(path);
    }
    if let Some(page) = query_value(search, "view").and_then(Page::from_id) {
        return page;
    }
    match path.trim_end_matches('/') {
        "/analytics" => Page::Analytics,
        "/accounts" => Page::Accounts,
        "/status" => Page::Status,
        "/scoreboard" => Page::Scoreboard,
        other => match other.strip_prefix("/accounts/") {
            Some(provider) if provider_id(provider) => Page::Accounts,
            _ => Page::Home,
        },
    }
}

/// A provider id in an `/accounts/<provider>` address: a lower-case letter or digit, then up to 63 of them or `-`.
fn provider_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One query value, as written (page ids need no decoding).
fn query_value<'a>(search: &'a str, key: &str) -> Option<&'a str> {
    search.trim_start_matches('?').split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then_some(v)
    })
}

/// The query the frame keeps when it writes its own address: the classic app's `view`, `classic` and `content` go (the
/// tab is in the path now); everything else (`e2e`, `safe` and the rest) stays.
pub(crate) fn kept_search(search: &str) -> String {
    let kept: Vec<&str> = search
        .trim_start_matches('?')
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            !pair.is_empty() && !matches!(key, "view" | "classic" | "content")
        })
        .collect();
    if kept.is_empty() {
        String::new()
    } else {
        format!("?{}", kept.join("&"))
    }
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
    sync_theme_color(mode);
}

/// index.html's theme-color for each scheme (the page background).
const THEME_COLOR_LIGHT: &str = "#ECF0F3";
const THEME_COLOR_DARK: &str = "#0C1217";

/// The colour a theme-color meta takes: an explicit Light or Dark wins over the meta's own scheme (`media`), Auto keeps
/// the scheme's own colour (as the classic app's `syncThemeColor` did).
pub(crate) fn theme_color(mode: &str, meta_media: &str) -> &'static str {
    let dark = match mode {
        "dark" => true,
        "light" => false,
        _ => meta_media.contains("dark"),
    };
    if dark {
        THEME_COLOR_DARK
    } else {
        THEME_COLOR_LIGHT
    }
}

/// The browser's bar follows the chosen theme: each theme-color meta keeps its media query and takes the colour above.
fn sync_theme_color(mode: &str) {
    let Ok(metas) = document().query_selector_all("meta[name=\"theme-color\"]") else {
        return;
    };
    for i in 0..metas.length() {
        let Some(meta) = metas
            .item(i)
            .and_then(|n| n.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let media = meta.get_attribute("media").unwrap_or_default();
        let _ = meta.set_attribute("content", theme_color(mode, &media));
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
                if was_hidden != !visible {
                    page_owner_changed();
                }
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
    let search = location.search().unwrap_or_default();
    let page = page_from_location(&location.pathname().unwrap_or_default(), &search);
    let embed = embedded(&search);
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
        view! { <frame::Frame initial=page/> }
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
    fn the_bar_colour_follows_an_explicit_theme_and_auto_keeps_each_scheme() {
        let light = "(prefers-color-scheme: light)";
        let dark = "(prefers-color-scheme: dark)";
        assert_eq!(theme_color("auto", light), THEME_COLOR_LIGHT);
        assert_eq!(theme_color("auto", dark), THEME_COLOR_DARK);
        for media in [light, dark, ""] {
            assert_eq!(theme_color("dark", media), THEME_COLOR_DARK);
            assert_eq!(theme_color("light", media), THEME_COLOR_LIGHT);
        }
    }

    #[test]
    fn the_classic_addresses_open_the_same_tabs() {
        for (path, search, page) in [
            ("/", "", Page::Home),
            ("/login", "", Page::Home),
            ("/analytics", "", Page::Analytics),
            ("/analytics/", "", Page::Analytics),
            ("/accounts", "", Page::Accounts),
            ("/accounts/claude", "", Page::Accounts),
            ("/accounts/opencode-go", "", Page::Accounts),
            ("/accounts/Claude", "", Page::Home),
            ("/accounts/-x", "", Page::Home),
            ("/status", "", Page::Status),
            ("/scoreboard", "", Page::Scoreboard),
            ("/nowhere", "", Page::Home),
            // ?view= is the alias, before the path
            ("/", "?view=analytics", Page::Analytics),
            ("/", "?e2e&view=accounts", Page::Accounts),
            ("/analytics", "?view=status", Page::Status),
            ("/", "?view=bogus", Page::Home),
            ("/", "?classic=1", Page::Home),
            ("/", "?content=1", Page::Home),
            // the frame's own addresses are read as before; ?view= doesn't move them
            ("/app/scoreboard", "?view=home", Page::Scoreboard),
            ("/app/home", "", Page::Home),
            ("/app/", "", Page::Status),
        ] {
            assert_eq!(page_from_location(path, search), page, "{path}{search}");
        }
    }

    #[test]
    fn the_frame_drops_the_classic_query_from_its_own_address() {
        assert_eq!(kept_search(""), "");
        assert_eq!(kept_search("?view=analytics"), "");
        assert_eq!(kept_search("?classic=1"), "");
        assert_eq!(kept_search("?content=1&e2e"), "?e2e");
        assert_eq!(
            kept_search("?e2e&view=accounts&safe=1,2,3,4"),
            "?e2e&safe=1,2,3,4"
        );
        assert_eq!(kept_search("?embed=1"), "?embed=1");
    }

    #[test]
    fn each_page_has_its_own_address_and_dashboard_id() {
        for page in Page::ALL {
            assert_eq!(page_from_path(page.path()), page);
            assert_eq!(Page::from_id(page.id()), Some(page));
        }
        // Home and Accounts take the dashboard controller's views; the other three read their own
        assert!(Page::Home.reads_dashboard() && Page::Accounts.reads_dashboard());
        assert!(
            !Page::Status.reads_dashboard()
                && !Page::Scoreboard.reads_dashboard()
                && !Page::Analytics.reads_dashboard()
        );
    }
}
