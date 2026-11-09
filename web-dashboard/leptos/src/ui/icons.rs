//! Lucide line icons, the app logo and the provider and platform marks.

use leptos::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};

/// The Lucide path data of each icon the pages use.
fn path(name: &str) -> &'static str {
    match name {
        "home" => {
            r#"<path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"/><path d="M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        "chart" => r#"<path d="M3 3v16a2 2 0 0 0 2 2h16"/><path d="m19 9-5 5-4-4-3 3"/>"#,
        "users" => {
            r#"<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>"#
        }
        "activity" => {
            r#"<path d="M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2"/>"#
        }
        "trophy" => {
            r#"<path d="M6 9H4.5a2.5 2.5 0 0 1 0-5H6"/><path d="M18 9h1.5a2.5 2.5 0 0 0 0-5H18"/><path d="M4 22h16"/><path d="M10 14.66V17c0 .55-.47.98-.97 1.21C7.85 18.75 7 20.24 7 22"/><path d="M14 14.66V17c0 .55.47.98.97 1.21C16.15 18.75 17 20.24 17 22"/><path d="M18 2H6v7a6 6 0 0 0 12 0V2Z"/>"#
        }
        "refresh" => {
            r#"<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>"#
        }
        "download" => r#"<circle cx="12" cy="12" r="10"/><path d="M12 8v8"/><path d="m8 12 4 4 4-4"/>"#,
        "monitor" => r#"<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>"#,
        "sun" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>"#
        }
        "moon" => r#"<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>"#,
        "user" => {
            r#"<circle cx="12" cy="12" r="10"/><circle cx="12" cy="10" r="3"/><path d="M7 20.662V19a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v1.662"/>"#
        }
        "chev-down" => r#"<path d="m6 9 6 6 6-6"/>"#,
        "chev-right" => r#"<path d="m9 18 6-6-6-6"/>"#,
        "info" => r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#,
        "close" => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        _ => "",
    }
}

#[component]
pub fn Icon(name: &'static str, #[prop(optional)] class: Option<&'static str>) -> impl IntoView {
    view! {
        <svg class=class.unwrap_or("ic") viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" inner_html=path(name)></svg>
    }
}

/// The app logo, as in the Slint header. Each copy names its own gradient: the chrome draws several copies and hides
/// all but one, and a reference to a gradient inside a hidden copy paints nothing.
#[component]
pub fn Logo() -> impl IntoView {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!("apx{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let body = format!(
        r##"<defs><linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="3.497" y1="11" x2="8" y2="11"><stop offset="0" stop-color="#3466FA"/><stop offset=".355" stop-color="#3466FA"/><stop offset="1" stop-color="#2C96FF"/></linearGradient></defs><path fill="url(#{id})" d="M4.396 10L7 10A1 1 0 0 1 7 12L3.497 12Z"/><path fill="currentColor" d="M2.127 15A0.729 0.729 0 0 1 1.462 13.972L6.997 1.649A1.1 1.1 0 0 1 9.003 1.649L14.538 13.972A0.729 0.729 0 0 1 13.873 15L12.894 15A0.3 0.3 0 0 1 12.62 14.823L8 4.536L3.38 14.823A0.3 0.3 0 0 1 3.106 15Z"/>"##
    );
    view! { <svg class="logo" viewBox="0 0 16 16" aria-hidden="true" inner_html=body></svg> }
}

fn mark_svg(name: &str) -> &'static str {
    match name {
        "claude" => include_str!("marks/claude.svg"),
        "codex" => include_str!("marks/codex.svg"),
        "codex-light" => include_str!("marks/codex-light.svg"),
        "zai" => include_str!("marks/zai.svg"),
        "zai-light" => include_str!("marks/zai-light.svg"),
        "muse" => include_str!("marks/muse.svg"),
        _ => "",
    }
}

/// A mark as an image, so a mark with gradients still draws when another copy of it is hidden.
fn mark_src(name: &str) -> String {
    format!("data:image/svg+xml,{}", String::from(js_sys::encode_uri_component(mark_svg(name))))
}

/// A provider's mark. Codex and Z.ai have a light and a dark drawing; a model with no mark draws nothing.
#[component]
pub fn Mark(name: Option<String>, #[prop(default = 20)] size: u32) -> impl IntoView {
    let style = format!("width:{size}px;height:{size}px");
    name.map(|name| {
        if name == "codex" || name == "zai" {
            let light = format!("{name}-light");
            view! {
                <img class="mk lt" alt="" style=style.clone() src=mark_src(&light)/>
                <img class="mk dk" alt="" style=style.clone() src=mark_src(&name)/>
            }
            .into_any()
        } else {
            view! { <img class="mk" alt="" style=style.clone() src=mark_src(&name)/> }.into_any()
        }
    })
}

/// The platform glyph of a machine row.
#[component]
pub fn PlatformGlyph(id: String) -> impl IntoView {
    let svg = match id.as_str() {
        "mac" => include_str!("marks/platform-apple.svg"),
        "windows" => include_str!("marks/platform-windows.svg"),
        _ => include_str!("marks/platform-ubuntu.svg"),
    };
    // The drawing's own <title> would show as a tooltip; the row already names the machine.
    let plain = match (svg.find("<title>"), svg.find("</title>")) {
        (Some(a), Some(b)) => format!("{}{}", &svg[..a], &svg[b + 8..]),
        _ => svg.to_string(),
    };
    let svg = plain.replacen("<svg ", "<svg fill=\"currentColor\" aria-hidden=\"true\" ", 1);
    view! { <span class="glyph" inner_html=svg></span> }
}
