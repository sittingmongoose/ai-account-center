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

/// The iPhone Duo, from its two panels' sizes in points (closed 466x678, open 669x951, either way up). Read once from
/// the screen: unlike the window and the insets, it does not change while iOS settles a fold or a rotation.
pub fn is_duo(screen_width: f64, screen_height: f64) -> bool {
    let (short, long) = if screen_width < screen_height {
        (screen_width, screen_height)
    } else {
        (screen_height, screen_width)
    };
    let near = |a: f64, b: f64| (a - b).abs() <= 2.0;
    (near(short, 466.0) && near(long, 678.0)) || (near(short, 669.0) && near(long, 951.0))
}

/// The chrome for a screen: `desk` header, `phone` top bar and tab bar, `rail` in the right status strip, `band` under
/// the top status band. Inside the shell's iframe the shell draws the chrome, so the page draws none. On the Duo the
/// rail is the navigation in every pose but open and upright, whatever insets iOS reports during a fold or a
/// rotation (it may report none for a moment, or keep its status bar on top after a rotation).
pub fn chrome_for(embed: bool, duo: bool, width: f64, height: f64, insets: Insets) -> &'static str {
    if embed {
        return "none";
    }
    if duo && !(width >= 600.0 && height > width) {
        return "rail";
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

/// How far below the top of the strip the app's tabs may start: past the iOS clock and the signal and battery ring
/// (measured on the device, 2026-11-10: the cluster ends near 168 pt folded and 116 pt open). With no strip on the
/// right (iOS kept its status bar on top) the rail starts near the top.
pub fn rail_reserve(width: f64, insets: Insets) -> f64 {
    if insets.right < 44.0 {
        12.0
    } else if width >= 760.0 {
        136.0
    } else {
        184.0
    }
}

/// The rail's width: the status strip when iOS reports one, else the strip's usual 84 px.
pub fn rail_width(insets: Insets) -> f64 {
    insets.right.max(84.0)
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
    let screen = window().screen().ok();
    let duo = screen
        .map(|s| {
            is_duo(
                s.width().unwrap_or(0) as f64,
                s.height().unwrap_or(0) as f64,
            )
        })
        .unwrap_or(false);
    let apply = move || {
        let insets = forced
            .or_else(|| probe.as_ref().filter(|_| !embed).map(measure))
            .unwrap_or_default();
        let size = |v: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>, or: f64| {
            v.ok().and_then(|v| v.as_f64()).unwrap_or(or)
        };
        let width = size(window().inner_width(), 1440.0);
        let height = size(window().inner_height(), 900.0);
        let chrome = chrome_for(embed, duo, width, height, insets);
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
            // The tall reserve clears the Duo's clock and signal ring; any other device's rail starts near the top.
            &px(if chrome == "rail" {
                if duo {
                    rail_reserve(width, insets)
                } else {
                    12.0
                }
            } else {
                0.0
            }),
        );
        let _ = style.set_property(
            "--rail-w",
            &px(if chrome == "rail" {
                rail_width(insets)
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
    // iOS reports a fold's or a rotation's new insets after the resize, so the chrome is worked out again on every
    // change of the window, the visual viewport or the orientation, and twice more while iOS settles.
    let settle = Closure::<dyn Fn()>::new(apply.clone());
    let on_change = Closure::<dyn Fn()>::new(move || {
        // A pinch-zoom resizes the visual viewport too: the chrome is chosen at the page's own scale only.
        let zoomed = window()
            .visual_viewport()
            .map(|v| (v.scale() - 1.0).abs() > 0.01)
            .unwrap_or(false);
        if zoomed {
            return;
        }
        apply();
        for ms in [120, 350] {
            let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
                settle.as_ref().unchecked_ref(),
                ms,
            );
        }
    });
    for event in ["resize", "orientationchange"] {
        let _ =
            window().add_event_listener_with_callback(event, on_change.as_ref().unchecked_ref());
    }
    if let Some(viewport) = window().visual_viewport() {
        let _ =
            viewport.add_event_listener_with_callback("resize", on_change.as_ref().unchecked_ref());
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
        assert_eq!(
            chrome_for(false, false, 1440.0, 900.0, Insets::default()),
            "desk"
        );
        assert_eq!(
            chrome_for(false, false, 402.0, 874.0, insets(62.0, 0.0, 34.0, 0.0)),
            "phone"
        );
        assert_eq!(
            chrome_for(false, true, 466.0, 678.0, insets(0.0, 84.0, 34.0, 0.0)),
            "rail"
        );
        assert_eq!(
            chrome_for(false, true, 951.0, 669.0, insets(0.0, 84.0, 34.0, 0.0)),
            "rail"
        );
        assert_eq!(
            chrome_for(false, true, 669.0, 951.0, insets(82.0, 0.0, 34.0, 0.0)),
            "band"
        );
        assert_eq!(
            chrome_for(false, true, 669.0, 869.0, insets(0.0, 0.0, 34.0, 0.0)),
            "phone"
        );
    }

    #[test]
    fn the_duo_keeps_its_rail_however_ios_reports_a_fold_or_a_rotation() {
        // folded and turned on its side, a fold whose insets have not arrived yet, an open app turned to landscape
        // whose status bar stayed on top: always the rail, never the bottom bar
        for (w, h, i) in [
            (678.0, 466.0, Insets::default()),
            (466.0, 678.0, Insets::default()),
            (951.0, 669.0, Insets::default()),
            (951.0, 587.0, insets(0.0, 0.0, 21.0, 0.0)),
            (678.0, 466.0, insets(0.0, 0.0, 21.0, 84.0)),
        ] {
            assert_eq!(chrome_for(false, true, w, h, i), "rail", "{w}x{h} {i:?}");
        }
        assert_eq!(rail_width(Insets::default()), 84.0);
        assert_eq!(rail_width(insets(0.0, 90.0, 0.0, 0.0)), 90.0);
    }

    #[test]
    fn the_duo_is_known_by_its_panels() {
        assert!(
            is_duo(466.0, 678.0)
                && is_duo(678.0, 466.0)
                && is_duo(669.0, 951.0)
                && is_duo(951.0, 669.0)
        );
        assert!(!is_duo(402.0, 874.0) && !is_duo(1440.0, 900.0) && !is_duo(1024.0, 1366.0));
    }

    #[test]
    fn a_window_without_insets_gets_bars_when_it_is_narrow() {
        assert_eq!(
            chrome_for(false, false, 669.0, 951.0, Insets::default()),
            "phone"
        );
        assert_eq!(
            chrome_for(false, false, 867.0, 600.0, Insets::default()),
            "desk"
        );
    }

    #[test]
    fn inside_the_shell_the_page_draws_no_chrome() {
        assert_eq!(
            chrome_for(true, true, 466.0, 678.0, insets(0.0, 84.0, 34.0, 0.0)),
            "none"
        );
        assert_eq!(
            chrome_for(true, false, 1440.0, 900.0, Insets::default()),
            "none"
        );
    }

    #[test]
    fn reserve_keeps_the_clock_and_signal_clear() {
        let strip = insets(0.0, 84.0, 34.0, 0.0);
        assert_eq!(rail_reserve(466.0, strip), 184.0);
        assert_eq!(rail_reserve(951.0, strip), 136.0);
        assert_eq!(rail_reserve(951.0, Insets::default()), 12.0);
    }
}
