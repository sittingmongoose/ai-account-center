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
        "download" => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M12 8v8"/><path d="m8 12 4 4 4-4"/>"#
        }
        "monitor" => {
            r#"<rect width="20" height="14" x="2" y="3" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>"#
        }
        "sun" => {
            r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>"#
        }
        "moon" => r#"<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>"#,
        "user" => {
            r#"<circle cx="12" cy="12" r="10"/><circle cx="12" cy="10" r="3"/><path d="M7 20.662V19a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v1.662"/>"#
        }
        "chev-down" => r#"<path d="m6 9 6 6 6-6"/>"#,
        "chev-right" => r#"<path d="m9 18 6-6-6-6"/>"#,
        "chev-left" => r#"<path d="m15 18-6-6 6-6"/>"#,
        "info" => r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#,
        "close" => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        "alert" => {
            r#"<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>"#
        }
        "alert-circle" => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M12 8v4"/><path d="M12 16h.01"/>"#
        }
        "app-window" => {
            r#"<rect x="2" y="4" width="20" height="16" rx="2"/><path d="M10 4v4"/><path d="M2 8h20"/><path d="M6 4v4"/>"#
        }
        "arrow-down" => r#"<path d="M12 5v14"/><path d="m19 12-7 7-7-7"/>"#,
        "check" => r#"<path d="M20 6 9 17l-5-5"/>"#,
        "check-circle" => r#"<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>"#,
        "chev-up" => r#"<path d="m18 15-6-6-6 6"/>"#,
        "code" => {
            r#"<rect width="20" height="12" x="2" y="6" rx="2"/><path d="M12 12h.01"/><path d="M17 12h.01"/><path d="M7 12h.01"/>"#
        }
        "copy" => {
            r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#
        }
        "external" => {
            r#"<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"#
        }
        "eye" => {
            r#"<path d="M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0"/><circle cx="12" cy="12" r="3"/>"#
        }
        "eye-off" => {
            r#"<path d="M10.733 5.076a10.744 10.744 0 0 1 11.205 6.575 1 1 0 0 1 0 .696 10.747 10.747 0 0 1-1.444 2.49"/><path d="M14.084 14.158a3 3 0 0 1-4.242-4.242"/><path d="M17.479 17.499a10.75 10.75 0 0 1-15.417-5.151 1 1 0 0 1 0-.696 10.75 10.75 0 0 1 4.446-5.143"/><path d="m2 2 20 20"/>"#
        }
        "globe" => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>"#
        }
        "key" => {
            r#"<path d="M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z"/><circle cx="16.5" cy="7.5" r=".5" fill="currentColor"/>"#
        }
        "laptop" => {
            r#"<path d="M20 16V7a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v9m16 0H4m16 0 1.28 2.55a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45L4 16"/>"#
        }
        "link" => {
            r#"<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>"#
        }
        "lock" => {
            r#"<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>"#
        }
        "lock-open" => {
            r#"<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 9.9-1"/>"#
        }
        "login" => {
            r#"<path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"/><path d="m10 17 5-5-5-5"/><path d="M15 12H3"/>"#
        }
        "logout" => {
            r#"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/>"#
        }
        "minus" => r#"<path d="M5 12h14"/>"#,
        "plus" => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
        "rotate" => {
            r#"<path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/><path d="M3 3v5h5"/>"#
        }
        "share" => {
            r#"<path d="M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8"/><path d="m16 6-4-4-4 4"/><path d="M12 2v13"/>"#
        }
        "swap" => {
            r#"<path d="M8 3 4 7l4 4"/><path d="M4 7h16"/><path d="m16 21 4-4-4-4"/><path d="M20 17H4"/>"#
        }
        "terminal" => r#"<path d="m4 17 6-6-6-6"/><path d="M12 19h8"/>"#,
        "trash" => {
            r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/>"#
        }
        "clock" => r#"<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>"#,
        "shield" => {
            r#"<path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/>"#
        }
        "server" => {
            r#"<rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><path d="M6 6h.01"/><path d="M6 18h.01"/>"#
        }
        "timer" => r#"<path d="M10 2h4"/><path d="m12 14 3-3"/><circle cx="12" cy="14" r="8"/>"#,
        "filter" => r#"<path d="M22 3H2l8 9.46V19l4 2v-8.54Z"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#,
        // Home: the meter legend, the amounts' icons and the cards' date notes
        "help" => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3"/><path d="M12 17h.01"/>"#
        }
        "wallet" => {
            r#"<path d="M19 7V4a1 1 0 0 0-1-1H5a2 2 0 0 0 0 4h15a1 1 0 0 1 1 1v4h-3a2 2 0 0 0 0 4h3a1 1 0 0 0 1-1v-2a1 1 0 0 0-1-1"/><path d="M3 5v14a2 2 0 0 0 2 2h15a1 1 0 0 0 1-1v-4"/>"#
        }
        "pack" => {
            r#"<path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z"/><path d="M12 22V12"/><path d="m3.3 7 7.703 4.734a2 2 0 0 0 1.994 0L20.7 7"/>"#
        }
        "zap" => {
            r#"<path d="M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z"/>"#
        }
        // Analytics: the scope lines and the custom range button (wallet is above)
        "layers" => {
            r#"<path d="M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/><path d="m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65"/><path d="m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65"/>"#
        }
        "calendar" => {
            r#"<path d="M8 2v4"/><path d="M16 2v4"/><rect width="18" height="18" x="3" y="4" rx="2"/><path d="M3 10h18"/>"#
        }
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

/// An icon the view names (an amount's `icon`); an unknown name draws nothing.
#[component]
pub fn IconOf(name: String) -> impl IntoView {
    view! {
        <svg class="ic" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" inner_html=path(&name)></svg>
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
        "antigravity" => include_str!("marks/antigravity.svg"),
        "cursor" => include_str!("marks/cursor.svg"),
        "cursor-light" => include_str!("marks/cursor-light.svg"),
        "qwen" => include_str!("marks/qwen.svg"),
        "qwen-light" => include_str!("marks/qwen-light.svg"),
        "opencode-go" => include_str!("marks/opencode-go.svg"),
        "opencode-go-light" => include_str!("marks/opencode-go-light.svg"),
        _ => "",
    }
}

/// A mark as an image, so a mark with gradients still draws when another copy of it is hidden.
fn mark_src(name: &str) -> String {
    if name == "kimi-code" {
        // Kimi Code's official app icon (a PNG with its own plate).
        return format!(
            "data:image/png;base64,{}",
            include_str!("marks/kimi-code.png.b64").trim()
        );
    }
    format!(
        "data:image/svg+xml,{}",
        String::from(js_sys::encode_uri_component(mark_svg(name)))
    )
}

/// Marks with their own artwork for light surfaces (ui/marks/sources.json).
fn has_light_art(name: &str) -> bool {
    matches!(name, "codex" | "cursor" | "qwen" | "zai" | "opencode-go")
}

/// The optical scale of each mark (sources.json; the plated Kimi icon a further 14% smaller), as Slint's
/// `ProviderMark` draws it.
pub fn mark_scale(name: &str) -> f64 {
    match name {
        "claude" => 0.93,
        "codex" => 0.95,
        "antigravity" => 0.97,
        "cursor" => 0.89,
        "muse" => 1.0,
        "kimi-code" => 0.97 * 0.86,
        "qwen" => 0.93,
        "zai" => 0.92,
        "opencode-go" => 0.84,
        _ => 1.0,
    }
}

/// A provider's mark in a square box at its optical size, as on Home and Accounts (Slint `ProviderMark`). Never
/// recoloured; light and dark artwork follow the theme.
#[component]
pub fn ProviderMark(provider: String, #[prop(default = 22)] size: u32) -> impl IntoView {
    let inner = (size as f64 * mark_scale(&provider)).round() as u32;
    let known = matches!(
        provider.as_str(),
        "claude"
            | "codex"
            | "antigravity"
            | "cursor"
            | "muse"
            | "kimi-code"
            | "qwen"
            | "zai"
            | "opencode-go"
    );
    let box_style = format!("width:{size}px;height:{size}px");
    view! {
        <span class="pmark" style=box_style aria-hidden="true">
            {known.then(|| view! { <Mark name=Some(provider.clone()) size=inner/> })}
        </span>
    }
}

/// A provider's mark. Codex and Z.ai have a light and a dark drawing; a model with no mark draws nothing.
#[component]
pub fn Mark(name: Option<String>, #[prop(default = 20)] size: u32) -> impl IntoView {
    let style = format!("width:{size}px;height:{size}px");
    name.map(|name| {
        if has_light_art(&name) {
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
    let svg = platform_svg(&id, crate::status::computer(&id));
    // The drawing's own <title> would show as a tooltip; the row already names the machine.
    let plain = match (svg.find("<title>"), svg.find("</title>")) {
        (Some(a), Some(b)) => format!("{}{}", &svg[..a], &svg[b + 8..]),
        _ => svg.to_string(),
    };
    let svg = plain.replacen(
        "<svg ",
        "<svg fill=\"currentColor\" aria-hidden=\"true\" ",
        1,
    );
    view! { <span class="glyph" inner_html=svg></span> }
}

// Accept either a registry machine id or the OS supplied by other icon callers.
fn platform_svg(id: &str, computer: Option<&serde_json::Value>) -> &'static str {
    match computer.and_then(|row| row["os"].as_str()).unwrap_or(id) {
        "mac" => include_str!("marks/platform-apple.svg"),
        "windows" => include_str!("marks/platform-windows.svg"),
        _ => include_str!("marks/platform-ubuntu.svg"),
    }
}

#[cfg(test)]
mod platform_tests {
    use super::platform_svg;

    #[test]
    fn registry_machines_and_os_callers_use_their_platform() {
        for (id, os) in [
            ("vm", "linux"),
            ("mac", "mac"),
            ("windows", "windows"),
            ("nas1", "linux"),
            ("alexis", "windows"),
        ] {
            assert_eq!(
                platform_svg(id, crate::status::computer(id)),
                platform_svg(os, None),
                "{id}"
            );
        }
        let second_windows = serde_json::json!({"machineId": "other-pc", "os": "windows"});
        assert_eq!(
            platform_svg("other-pc", Some(&second_windows)),
            platform_svg("windows", None)
        );
        assert_eq!(platform_svg("unknown", None), platform_svg("linux", None));
    }
}
