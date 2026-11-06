//! AI Account Center pages drawn with Leptos (client-side rendered, served by the TypeScript server).
//!
//! `/app/status` is the Status page; `/app/scoreboard` the Model Scoreboard. `?embed=1` hides the page's own
//! header when the Slint shell shows the page in its iframe. In the iframe the shell sends the theme
//! (`{type: "aac-theme", mode}`); standalone, the page follows the system setting.

pub mod api;
mod blocks;
mod cards;
mod drafts;
pub mod fmt;
mod live;
mod media;
mod scoreboard;
mod status;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Status,
    Scoreboard,
}

fn page_from_path(path: &str) -> Page {
    if path.trim_end_matches('/').ends_with("/scoreboard") {
        Page::Scoreboard
    } else {
        Page::Status
    }
}

fn embedded(search: &str) -> bool {
    web_sys::UrlSearchParams::new_with_str(search)
        .ok()
        .and_then(|p| p.get("embed"))
        .as_deref()
        == Some("1")
}

fn set_theme(mode: &str) {
    if let Some(root) = document().document_element() {
        let _ = root.set_attribute("data-theme", if mode == "dark" { "dark" } else { "light" });
    }
}

/// Same-origin messages from the shell only; anything else is ignored.
fn listen_to_shell() {
    let origin = window().location().origin().unwrap_or_default();
    let handler =
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if event.origin() != origin {
                return;
            }
            let data = event.data();
            let kind = js_sys::Reflect::get(&data, &"type".into())
                .ok()
                .and_then(|v| v.as_string());
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
    if embed {
        listen_to_shell();
    }
    mount_to_body(move || {
        media::provide_viewer();
        let page_view = match page {
            Page::Status => {
                // Only the Status page reads /api/status.
                provide_context(live::start());
                view! { <status::StatusPage embedded=embed/> }.into_any()
            }
            Page::Scoreboard => view! { <scoreboard::ScoreboardPage embedded=embed/> }.into_any(),
        };
        view! {
            {page_view}
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
        assert!(page_from_path("/app/") == Page::Status);
    }
}
