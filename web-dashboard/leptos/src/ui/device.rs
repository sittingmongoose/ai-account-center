//! Which chrome the page draws, from the screen it is on. The pages are laid out by container queries; only this
//! choice needs the safe-area insets, which CSS cannot branch on, so a probe reads `env()` and the result is written
//! to `html[data-chrome]` and the `--safe-*` custom properties (DESIGN-SYSTEM.md section 6, DUO-MEASURED.md).
//!
//! `?safe=t,r,b,l` overrides the measured insets for tests in a desktop browser. It is display-only.

use leptos::prelude::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

thread_local! {
    /// The last chrome decision: insets, chrome, Duo detected, rail reserve. For the Device info readout only.
    static LAST: RefCell<(Insets, &'static str, bool, f64)> = RefCell::new((Insets::default(), "", false, 0.0));
    /// The most recent chrome decisions, oldest first, so a fold or rotation sequence can be read back.
    static LOG: RefCell<VecDeque<String>> = RefCell::new(VecDeque::new());
}

/// A plain-text readout of what this device reports and which chrome it got (the account menu's "Device info").
/// Display-only: it changes nothing, and holds no account or usage data.
pub fn device_report() -> String {
    let w = window();
    let num = |v: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        v.ok().and_then(|v| v.as_f64()).unwrap_or(-1.0)
    };
    let get = |o: &wasm_bindgen::JsValue, k: &str| js_sys::Reflect::get(o, &k.into()).ok();
    let mut out = String::from("AAC device info\n");
    if let Ok(s) = w.screen() {
        out += &format!(
            "screen {}x{}  avail {}x{}\n",
            s.width().unwrap_or(-1),
            s.height().unwrap_or(-1),
            s.avail_width().unwrap_or(-1),
            s.avail_height().unwrap_or(-1)
        );
        let o = get(&s, "orientation");
        let ty = o
            .as_ref()
            .and_then(|o| get(o, "type"))
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        let angle = o
            .as_ref()
            .and_then(|o| get(o, "angle"))
            .and_then(|v| v.as_f64())
            .unwrap_or(-1.0);
        out += &format!("orientation {ty} {angle}\n");
    }
    out += &format!(
        "inner {}x{}  dpr {}\n",
        num(w.inner_width()),
        num(w.inner_height()),
        w.device_pixel_ratio()
    );
    if let Some(v) = w.visual_viewport() {
        out += &format!(
            "visual {:.0}x{:.0} scale {:.2} top {:.0}\n",
            v.width(),
            v.height(),
            v.scale(),
            v.offset_top()
        );
    }
    let nav = w.navigator();
    let standalone = get(&nav, "standalone").and_then(|v| v.as_bool());
    out += &format!("standalone {standalone:?}\n");
    LAST.with(|l| {
        let (i, chrome, duo, reserve) = *l.borrow();
        out += &format!(
            "safe t{} r{} b{} l{}\nchrome {chrome}{}  duo {duo}  reserve {reserve}\n",
            i.top,
            i.right,
            i.bottom,
            i.left,
            if chrome == "rail" {
                format!(" ({})", rail_side(i))
            } else {
                String::new()
            }
        );
    });
    let ua = nav.user_agent().unwrap_or_default();
    let model = ua.split(['(', ')']).nth(1).unwrap_or("").to_string();
    out += &format!("ua {model}\nrecent:\n");
    LOG.with(|log| {
        for line in log.borrow().iter() {
            out += line;
            out += "\n";
        }
    });
    out
}

fn remember(
    insets: Insets,
    chrome: &'static str,
    duo: bool,
    reserve: f64,
    width: f64,
    height: f64,
) {
    LAST.with(|l| *l.borrow_mut() = (insets, chrome, duo, reserve));
    let d = js_sys::Date::new_0();
    let line = format!(
        "{:02}:{:02}:{:02} {:.0}x{:.0} safe {:.0}/{:.0}/{:.0}/{:.0} -> {chrome}",
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds(),
        width,
        height,
        insets.top,
        insets.right,
        insets.bottom,
        insets.left
    );
    LOG.with(|log| {
        let mut log = log.borrow_mut();
        if log.back() != Some(&line) {
            log.push_back(line);
        }
        while log.len() > 14 {
            log.pop_front();
        }
    });
}

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

/// Which side the rail goes on: the left when iOS reports the status strip there and not on the right (the folded Duo
/// turned the other way, safe 0/0/34/84 on the device), else the right, also while a fold's insets have not arrived.
pub fn rail_side(insets: Insets) -> &'static str {
    if insets.left >= 44.0 && insets.right < 44.0 {
        "left"
    } else {
        "right"
    }
}

/// The status strip on the rail's side, or 0 when iOS reports none.
fn strip(insets: Insets) -> f64 {
    if rail_side(insets) == "left" {
        insets.left
    } else {
        insets.right
    }
}

/// How far below the top of the strip the app's tabs may start: past the iOS clock and the signal and battery ring
/// (measured on the device, 2026-11-10: the cluster ends near 168 pt folded and 116 pt open). On its side the folded
/// Duo shows no status cluster in its strip, on either side, and with no strip at all (iOS kept its status bar on top)
/// the rail starts near the top too.
pub fn rail_reserve(width: f64, height: f64, insets: Insets) -> f64 {
    if strip(insets) < 44.0 {
        12.0
    } else if width >= 760.0 {
        136.0
    } else if width > height {
        12.0
    } else {
        184.0
    }
}

/// The rail's width: the status strip on its side when iOS reports one, else the strip's usual 84 px.
pub fn rail_width(insets: Insets) -> f64 {
    strip(insets).max(84.0)
}

/// While a text field has focus the on-screen keyboard shortens the window (open upright 669x869 can become 669x560),
/// which would read as "not upright" and swap the bars for the rail in the middle of a note. So while typing, a change
/// that keeps the window's width keeps the chrome; a rotation (a new width) still chooses again.
pub fn keep_chrome_while_typing(typing: bool, last_width: Option<f64>, width: f64) -> bool {
    typing && last_width.is_some_and(|w| (w - width).abs() < 1.0)
}

/// A text field or editable element has the focus (the keyboard is, or is about to be, up).
pub fn typing() -> bool {
    document()
        .active_element()
        .map(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            let editable = js_sys::Reflect::get(&el, &"isContentEditable".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            editable
                || tag == "textarea"
                || (tag == "input"
                    && !matches!(
                        el.get_attribute("type").unwrap_or_default().as_str(),
                        "checkbox" | "radio" | "button" | "submit" | "range"
                    ))
        })
        .unwrap_or(false)
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
    // The width and chrome last chosen, so typing can keep them (keep_chrome_while_typing).
    let last: std::rc::Rc<std::cell::Cell<Option<(f64, &'static str)>>> =
        std::rc::Rc::new(std::cell::Cell::new(None));
    let apply = move || {
        let insets = forced
            .or_else(|| probe.as_ref().filter(|_| !embed).map(measure))
            .unwrap_or_default();
        let size = |v: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>, or: f64| {
            v.ok().and_then(|v| v.as_f64()).unwrap_or(or)
        };
        let width = size(window().inner_width(), 1440.0);
        let height = size(window().inner_height(), 900.0);
        let previous = last.get();
        if keep_chrome_while_typing(typing(), previous.map(|(w, _)| w), width) {
            return;
        }
        let chrome = chrome_for(embed, duo, width, height, insets);
        last.set(Some((width, chrome)));
        let _ = root.set_attribute("data-chrome", chrome);
        // The rail sits in the strip, on whichever side iOS puts it (app.css mirrors the rail's layout for the left).
        if chrome == "rail" {
            let _ = root.set_attribute("data-rail-side", rail_side(insets));
        } else {
            let _ = root.remove_attribute("data-rail-side");
        }
        let style = root.style();
        // Inside the iframe nothing is reserved: the shell's own chrome already sits outside the content.
        let px = |n: f64| format!("{n}px");
        let _ = style.set_property("--safe-top", &px(insets.top));
        let _ = style.set_property("--safe-right", &px(insets.right));
        let _ = style.set_property("--safe-bottom", &px(insets.bottom));
        let _ = style.set_property("--safe-left", &px(insets.left));
        // The tall reserve clears the Duo's clock and signal ring; any other device's rail starts near the top.
        let reserve = if chrome == "rail" {
            if duo {
                rail_reserve(width, height, insets)
            } else {
                12.0
            }
        } else {
            0.0
        };
        let _ = style.set_property("--rail-reserve", &px(reserve));
        remember(insets, chrome, duo, reserve, width, height);
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
    fn typing_keeps_the_chrome_until_the_width_changes() {
        // the keyboard shortens open upright from 669x869: same width, so the bars stay
        assert!(keep_chrome_while_typing(true, Some(669.0), 669.0));
        // turned while typing: a new width chooses again
        assert!(!keep_chrome_while_typing(true, Some(669.0), 951.0));
        // not typing, or nothing chosen yet: choose as usual
        assert!(!keep_chrome_while_typing(false, Some(669.0), 669.0));
        assert!(!keep_chrome_while_typing(true, None, 669.0));
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
        let right = insets(0.0, 84.0, 34.0, 0.0);
        let left = insets(0.0, 0.0, 34.0, 84.0);
        assert_eq!(rail_reserve(466.0, 678.0, right), 184.0, "folded upright");
        assert_eq!(rail_reserve(951.0, 669.0, right), 136.0, "open on its side");
        assert_eq!(
            rail_reserve(951.0, 669.0, Insets::default()),
            12.0,
            "no strip reported"
        );
        // folded on its side: no status cluster in the strip, whichever side it is on
        assert_eq!(rail_reserve(678.0, 466.0, right), 12.0);
        assert_eq!(rail_reserve(678.0, 466.0, left), 12.0);
    }

    #[test]
    fn the_rail_goes_on_the_side_of_the_strip() {
        // the real Duo folded and turned the other way (2026-11-10, log 18:56:23)
        assert_eq!(rail_side(insets(0.0, 0.0, 34.0, 84.0)), "left");
        assert_eq!(rail_width(insets(0.0, 0.0, 34.0, 84.0)), 84.0);
        assert_eq!(rail_width(insets(0.0, 0.0, 34.0, 96.0)), 96.0);
        // folded upright, open on its side, folded turned the usual way
        assert_eq!(rail_side(insets(0.0, 84.0, 34.0, 0.0)), "right");
        // no strip yet, or one on each side (an iPhone on its side): the right, as before
        assert_eq!(rail_side(Insets::default()), "right");
        assert_eq!(rail_side(insets(0.0, 62.0, 21.0, 62.0)), "right");
        assert_eq!(rail_width(insets(0.0, 62.0, 21.0, 62.0)), 84.0);
        // a left inset too small to be a strip does not move the rail
        assert_eq!(rail_side(insets(0.0, 0.0, 34.0, 30.0)), "right");
    }
}
