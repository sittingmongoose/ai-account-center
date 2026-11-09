//! Which chrome the page draws, from the screen it is on. The pages are laid out by container queries; only this
//! choice needs the safe-area insets, which CSS cannot branch on, so a probe reads `env()` and the result is written
//! to `html[data-chrome]` and the `--safe-*` custom properties (DESIGN-SYSTEM.md section 6, DUO-MEASURED.md).
//!
//! `?safe=t,r,b,l` overrides the measured insets for tests in a desktop browser. It is display-only.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// The chrome for a screen: `desk` header, `phone` top bar and tab bar, `rail` in the right status strip, `band` under
/// the top status band. Inside the shell's iframe the shell draws the chrome, so the page draws none.
pub fn chrome_for(embed: bool, width: f64, insets: Insets) -> &'static str {
    if embed {
        return "none";
    }
    // A strip down the right with no top inset: folded, or open on its side.
    if insets.right >= 44.0 && insets.top < 24.0 {
        return "rail";
    }
    let content = width - insets.left - insets.right;
    // Open and upright: a band across the top. On a phone the same inset is the Dynamic Island.
    if content >= 600.0 && content < 1280.0 && insets.top >= 20.0 {
        return "band";
    }
    // The top and tab bars below the width the desktop header fits in.
    if content < 760.0 { "phone" } else { "desk" }
}

/// How far below the top of the strip the app's tabs may start: past the iOS clock and Wi-Fi.
pub fn rail_reserve(width: f64) -> f64 {
    if width >= 760.0 { 106.0 } else { 156.0 }
}

fn parse_override(search: &str) -> Option<Insets> {
    let safe = web_sys::UrlSearchParams::new_with_str(search)
        .ok()?
        .get("safe")?;
    let parts: Vec<f64> = safe
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    (parts.len() == 4).then(|| Insets {
        top: parts[0],
        right: parts[1],
        bottom: parts[2],
        left: parts[3],
    })
}

fn measure(probe: &web_sys::HtmlElement) -> Insets {
    let Some(style) = window().get_computed_style(probe).ok().flatten() else {
        return Insets::default();
    };
    let side = |name: &str| {
        style
            .get_property_value(name)
            .ok()
            .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    Insets {
        top: side("padding-top"),
        right: side("padding-right"),
        bottom: side("padding-bottom"),
        left: side("padding-left"),
    }
}

/// Sets the chrome now and whenever the window changes shape (a fold or a rotation changes the insets too).
pub fn start(embed: bool) {
    let doc = document();
    let root: web_sys::HtmlElement = match doc.document_element().and_then(|e| e.dyn_into().ok()) {
        Some(root) => root,
        None => return,
    };
    let search = window().location().search().unwrap_or_default();
    let forced = if embed { None } else { parse_override(&search) };
    let probe: Option<web_sys::HtmlElement> = doc
        .create_element("div")
        .ok()
        .and_then(|e| e.dyn_into().ok());
    if let (Some(probe), Some(body)) = (probe.as_ref(), doc.body()) {
        let _ = probe.style().set_css_text(
            "position:fixed;left:0;top:0;width:0;height:0;overflow:hidden;pointer-events:none;visibility:hidden;\
             padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left);",
        );
        let _ = body.append_child(probe);
    }
    let apply = move || {
        let insets = forced
            .or_else(|| probe.as_ref().filter(|_| !embed).map(measure))
            .unwrap_or_default();
        let width = window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1440.0);
        let chrome = chrome_for(embed, width, insets);
        let _ = root.set_attribute("data-chrome", chrome);
        let style = root.style();
        // Inside the iframe nothing is reserved: the shell's own chrome already sits outside the content.
        let px = |n: f64| format!("{n}px");
        let _ = style.set_property("--safe-top", &px(insets.top));
        let _ = style.set_property("--safe-right", &px(insets.right));
        let _ = style.set_property("--safe-bottom", &px(insets.bottom));
        let _ = style.set_property("--safe-left", &px(insets.left));
        let _ = style.set_property(
            "--rail-reserve",
            &px(if chrome == "rail" {
                rail_reserve(width)
            } else {
                0.0
            }),
        );
        let _ = style.set_property(
            "--band-reserve",
            &px(if chrome == "band" { 112.0 } else { 0.0 }),
        );
    };
    apply();
    let on_change = Closure::<dyn Fn()>::new(apply);
    for event in ["resize", "orientationchange"] {
        let _ =
            window().add_event_listener_with_callback(event, on_change.as_ref().unchecked_ref());
    }
    on_change.forget();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insets(top: f64, right: f64, bottom: f64, left: f64) -> Insets {
        Insets {
            top,
            right,
            bottom,
            left,
        }
    }

    #[test]
    fn each_measured_pose_gets_its_chrome() {
        assert_eq!(chrome_for(false, 1440.0, Insets::default()), "desk");
        assert_eq!(
            chrome_for(false, 402.0, insets(62.0, 0.0, 34.0, 0.0)),
            "phone"
        );
        assert_eq!(
            chrome_for(false, 466.0, insets(0.0, 84.0, 34.0, 0.0)),
            "rail"
        );
        assert_eq!(
            chrome_for(false, 951.0, insets(0.0, 84.0, 34.0, 0.0)),
            "rail"
        );
        assert_eq!(
            chrome_for(false, 669.0, insets(82.0, 0.0, 34.0, 0.0)),
            "band"
        );
    }

    #[test]
    fn a_window_without_insets_gets_bars_when_it_is_narrow() {
        assert_eq!(chrome_for(false, 669.0, Insets::default()), "phone");
        assert_eq!(chrome_for(false, 867.0, Insets::default()), "desk");
    }

    #[test]
    fn inside_the_shell_the_page_draws_no_chrome() {
        assert_eq!(
            chrome_for(true, 466.0, insets(0.0, 84.0, 34.0, 0.0)),
            "none"
        );
        assert_eq!(chrome_for(true, 1440.0, Insets::default()), "none");
    }

    #[test]
    fn reserve_keeps_the_clock_and_wifi_clear() {
        assert_eq!(rail_reserve(466.0), 156.0);
        assert_eq!(rail_reserve(951.0), 106.0);
    }
}
