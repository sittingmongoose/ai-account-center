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
    trigger: &str,
) {
    LAST.with(|l| *l.borrow_mut() = (insets, chrome, duo, reserve));
    let d = js_sys::Date::new_0();
    let line = format!(
        "{:02}:{:02}:{:02} {:.0}x{:.0} safe {:.0}/{:.0}/{:.0}/{:.0} -> {chrome} reserve {reserve} [{trigger}]",
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
/// Duo shows no status cluster in its strip, on either side. The open landscape panel must reserve its cluster
/// even while iOS briefly reports no strip at launch, on resume or during rotation.
pub fn rail_reserve(width: f64, height: f64, _insets: Insets) -> f64 {
    if width >= 760.0 {
        136.0
    } else if width > height {
        12.0
    } else {
        184.0
    }
}

/// The rail's width. On the Duo the rail sits in the status strip: the strip on its side when iOS reports one, else
/// the strip's usual 84 px. On a phone on its side the inset is the Dynamic Island's band, not a strip, so the rail's
/// 84 px sit inside the safe area, after the inset (rail_pad).
pub fn rail_width(insets: Insets, duo: bool) -> f64 {
    if duo {
        strip(insets).max(84.0)
    } else {
        strip(insets) + 84.0
    }
}

/// The rail's padding on the screen's edge: the safe inset on a phone, so no button sits in the island's band; none on
/// the Duo, whose rail belongs in its strip.
pub fn rail_pad(insets: Insets, duo: bool) -> f64 {
    if duo { 0.0 } else { strip(insets) }
}

/// While a text field has focus the on-screen keyboard shortens the window (open upright 669x869 can become 669x560),
/// which would read as "not upright" and swap the bars for the rail in the middle of a note. So while typing, a change
/// that keeps the window's width and the rail's side keeps the chrome; a rotation (a new width, or a folded Duo
/// turned 180 to the same width with the strip on the other side) still chooses again (review L1).
pub fn keep_chrome_while_typing(
    typing: bool,
    last_width: Option<f64>,
    width: f64,
    last_side: Option<&'static str>,
    side: &'static str,
) -> bool {
    typing && last_width.is_some_and(|w| (w - width).abs() < 1.0) && last_side == Some(side)
}

/// Whether a change of the window is only a pinch-zoom, which keeps the chrome. Only a page zoomed in counts: while it
/// settles a fold iOS reports a scale below 1 (0.71 from open landscape to folded), which is no zoom. And a fold or a
/// rotation always moves the layout width or the insets, so either change chooses the chrome again even when zoomed.
pub fn zoom_only(
    scale: f64,
    last: Option<(f64, Insets)>,
    layout_width: f64,
    insets: Insets,
) -> bool {
    scale > 1.01 && last.is_some_and(|(w, i)| (w - layout_width).abs() < 1.0 && i == insets)
}

/// Whether a reload would steal the screen (review L5): a field has focus, the dashboard iframe
/// has focus, its raised confirmation shows, or the frame's own switch dialog is open.
pub(crate) fn busy_for_reload(
    field_typing: bool,
    iframe_focused: bool,
    dashboard_raised: bool,
    dialog_open: bool,
) -> bool {
    field_typing || iframe_focused || dashboard_raised || dialog_open
}

/// A text field or editable element has the focus (the keyboard is, or is about to be, up), the dashboard iframe
/// has the focus, or a switch confirmation (the dashboard's raised one, or the frame's own dialog) is open: a
/// resume reload offers instead of reloading over any of them (review L5).
pub fn typing() -> bool {
    let (field_typing, iframe_focused) = document()
        .active_element()
        .map(|el| {
            let tag = el.tag_name().to_ascii_lowercase();
            let editable = js_sys::Reflect::get(&el, &"isContentEditable".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (
                editable
                    || tag == "textarea"
                    || (tag == "input"
                        && !matches!(
                            el.get_attribute("type").unwrap_or_default().as_str(),
                            "checkbox" | "radio" | "button" | "submit" | "range"
                        )),
                tag == "iframe",
            )
        })
        .unwrap_or((false, false));
    let dashboard_raised = document()
        .query_selector(".slint-host.raised")
        .ok()
        .flatten()
        .is_some();
    let dialog_open = document()
        .query_selector(".adialog")
        .ok()
        .flatten()
        .is_some();
    busy_for_reload(field_typing, iframe_focused, dashboard_raised, dialog_open)
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

/// Follows both the viewport and the env() padding probe, including late launch/resume insets without a resize.
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
        if let Some(i) = forced {
            // Display-only test seam: changing this probe's padding exercises the real inset observer.
            let _ = probe.set_attribute("data-safe-probe", "");
            let _ = probe.style().set_property(
                "padding",
                &format!("{}px {}px {}px {}px", i.top, i.right, i.bottom, i.left),
            );
        }
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
    // The width, chrome and rail side last chosen, so typing can keep them (keep_chrome_while_typing).
    let last: std::rc::Rc<std::cell::Cell<Option<(f64, &'static str, &'static str)>>> =
        std::rc::Rc::new(std::cell::Cell::new(None));
    // The layout width and insets the chrome was last chosen for, so a pinch-zoom can be told from a fold (zoom_only).
    let seen: std::rc::Rc<std::cell::Cell<Option<(f64, Insets)>>> =
        std::rc::Rc::new(std::cell::Cell::new(None));
    let read_insets = {
        let probe = probe.clone();
        move || {
            probe
                .as_ref()
                .filter(|_| !embed)
                .map(measure)
                .unwrap_or_default()
        }
    };
    let layout_width = || {
        document()
            .document_element()
            .map(|e| e.client_width() as f64)
            .unwrap_or(0.0)
    };
    let apply = {
        let read_insets = read_insets.clone();
        let seen = seen.clone();
        move |trigger: &str| {
            let insets = read_insets();
            let scale = window().visual_viewport().map(|v| v.scale()).unwrap_or(1.0);
            if zoom_only(scale, seen.get(), layout_width(), insets) {
                return;
            }
            let size = |v: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>, or: f64| {
                v.ok().and_then(|v| v.as_f64()).unwrap_or(or)
            };
            let width = size(window().inner_width(), 1440.0);
            let height = size(window().inner_height(), 900.0);
            let previous = last.get();
            let side = rail_side(insets);
            if keep_chrome_while_typing(
                typing(),
                previous.map(|(w, _, _)| w),
                width,
                previous.map(|(_, _, s)| s),
                side,
            ) {
                return;
            }
            let chrome = chrome_for(embed, duo, width, height, insets);
            last.set(Some((width, chrome, side)));
            seen.set(Some((layout_width(), insets)));
            let _ = root.set_attribute("data-chrome", chrome);
            // The rail sits in the strip, on whichever side iOS puts it (app.css mirrors the rail's layout for the left).
            if chrome == "rail" {
                let _ = root.set_attribute("data-rail-side", side);
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
            remember(insets, chrome, duo, reserve, width, height, trigger);
            let _ = style.set_property(
                "--rail-w",
                &px(if chrome == "rail" {
                    rail_width(insets, duo)
                } else {
                    0.0
                }),
            );
            let _ = style.set_property(
                "--rail-pad",
                &px(if chrome == "rail" {
                    rail_pad(insets, duo)
                } else {
                    0.0
                }),
            );
            let _ = style.set_property(
                "--band-reserve",
                &px(if chrome == "band" { 112.0 } else { 0.0 }),
            );
        }
    };
    let apply = std::rc::Rc::new(apply);
    // Observe the padding box (border-box with no border). The probe's content box stays 0x0, so the default
    // content-box observation would miss env() padding changes. Keep the observer alive for this document.
    if let Some(probe) = probe {
        let apply = apply.clone();
        let observed =
            Closure::<dyn Fn(js_sys::Array, web_sys::ResizeObserver)>::new(move |_, _| {
                apply("insets")
            });
        if let Ok(observer) = web_sys::ResizeObserver::new(observed.as_ref().unchecked_ref()) {
            let options = web_sys::ResizeObserverOptions::new();
            options.set_box(web_sys::ResizeObserverBoxOptions::BorderBox);
            observer.observe_with_options(&probe, &options);
            // ResizeObserver remains active until document teardown, like the window listeners below.
            std::mem::forget(observer);
            observed.forget();
        }
    }
    // A new lifecycle/viewport event replaces outstanding settle passes instead of stacking timers during resize.
    let generation = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let changed = {
        let apply = apply.clone();
        move |trigger: &'static str| {
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            apply(trigger);
            // Measured iOS insets can arrive beyond 350 ms on launch/resume. The observer handles later changes too.
            for (ms, pass) in [
                (120, "settle120"),
                (350, "settle350"),
                (750, "settle750"),
                (1500, "settle1500"),
            ] {
                let apply = apply.clone();
                let generation = generation.clone();
                let later = Closure::once_into_js(move || {
                    if generation.get() == next {
                        apply(&format!("{trigger}/{pass}"));
                    }
                });
                let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
                    later.unchecked_ref(),
                    ms,
                );
            }
        }
    };
    changed("start");
    for event in ["resize", "orientationchange", "pageshow", "focus"] {
        let changed = changed.clone();
        let handler = Closure::<dyn Fn()>::new(move || changed(event));
        let _ = window().add_event_listener_with_callback(event, handler.as_ref().unchecked_ref());
        handler.forget();
    }
    if let Some(viewport) = window().visual_viewport() {
        let changed = changed.clone();
        let handler = Closure::<dyn Fn()>::new(move || changed("visualViewport"));
        let _ =
            viewport.add_event_listener_with_callback("resize", handler.as_ref().unchecked_ref());
        handler.forget();
    }
    {
        let changed = changed.clone();
        let handler = Closure::<dyn Fn()>::new(move || {
            if document().visibility_state() == web_sys::VisibilityState::Visible {
                changed("visibilitychange");
            }
        });
        let _ = document()
            .add_event_listener_with_callback("visibilitychange", handler.as_ref().unchecked_ref());
        handler.forget();
    }
    // Blur may outrun the keyboard-close resize: re-choose on the next tick (review L1).
    let handler = Closure::<dyn Fn()>::new(move || {
        let apply = apply.clone();
        let later = Closure::once_into_js(move || apply("focusout"));
        let _ = window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(later.unchecked_ref(), 0);
    });
    let _ =
        document().add_event_listener_with_callback("focusout", handler.as_ref().unchecked_ref());
    handler.forget();
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
    fn late_portrait_insets_choose_the_band_without_a_size_change() {
        assert_eq!(
            chrome_for(false, true, 669.0, 869.0, Insets::default()),
            "phone"
        );
        assert_eq!(
            chrome_for(false, true, 669.0, 869.0, insets(82.0, 0.0, 34.0, 0.0)),
            "band"
        );
    }

    #[test]
    fn landscape_reserve_survives_transient_zero_insets() {
        for height in [669.0, 587.0] {
            for i in [
                insets(0.0, 84.0, 34.0, 0.0),
                Insets::default(),
                insets(0.0, 0.0, 34.0, 84.0),
            ] {
                assert_eq!(rail_reserve(951.0, height, i), 136.0, "{height} {i:?}");
            }
        }
    }

    #[test]
    fn a_fold_is_never_taken_for_a_pinch_zoom() {
        let open = insets(0.0, 84.0, 34.0, 0.0);
        let folded = insets(0.0, 84.0, 34.0, 0.0);
        // Open landscape to folded on its side: iOS reports scale 0.71 while the width drops from 951 to 678.
        assert!(!zoom_only(0.71, Some((951.0, open)), 678.0, folded));
        // A scale below 1 is never a zoom, even when nothing else has moved yet.
        assert!(!zoom_only(0.71, Some((678.0, folded)), 678.0, folded));
        assert!(!zoom_only(1.0, Some((402.0, open)), 402.0, open));
    }

    #[test]
    fn a_pinch_zoom_keeps_the_chrome_until_the_width_or_insets_move() {
        let phone = insets(62.0, 0.0, 34.0, 0.0);
        let side = insets(0.0, 62.0, 21.0, 62.0);
        assert!(zoom_only(2.0, Some((402.0, phone)), 402.0, phone));
        // Zoomed in and then rotated: the layout width and the insets move, so the chrome is chosen again.
        assert!(!zoom_only(2.0, Some((402.0, phone)), 874.0, side));
        assert!(!zoom_only(2.0, Some((402.0, phone)), 402.0, side));
        assert!(!zoom_only(2.0, Some((402.0, phone)), 874.0, phone));
        // Nothing chosen yet: choose.
        assert!(!zoom_only(2.0, None, 402.0, phone));
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
        assert_eq!(rail_width(Insets::default(), true), 84.0);
        assert_eq!(rail_width(insets(0.0, 90.0, 0.0, 0.0), true), 90.0);
        assert_eq!(rail_pad(insets(0.0, 90.0, 0.0, 0.0), true), 0.0);
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
        assert!(keep_chrome_while_typing(
            true,
            Some(669.0),
            669.0,
            Some("right"),
            "right"
        ));
        // turned while typing: a new width chooses again
        assert!(!keep_chrome_while_typing(
            true,
            Some(669.0),
            951.0,
            Some("right"),
            "right"
        ));
        // turned 180 the same width (the folded Duo 678): the strip moves side, so choose again (review L1)
        assert!(!keep_chrome_while_typing(
            true,
            Some(678.0),
            678.0,
            Some("right"),
            "left"
        ));
        // not typing, or nothing chosen yet: choose as usual
        assert!(!keep_chrome_while_typing(
            false,
            Some(669.0),
            669.0,
            Some("right"),
            "right"
        ));
        assert!(!keep_chrome_while_typing(true, None, 669.0, None, "right"));
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
            136.0,
            "open landscape clears the cluster even without a reported strip"
        );
        // folded on its side: no status cluster in the strip, whichever side it is on
        assert_eq!(rail_reserve(678.0, 466.0, right), 12.0);
        assert_eq!(rail_reserve(678.0, 466.0, left), 12.0);
    }

    #[test]
    fn the_rail_goes_on_the_side_of_the_strip() {
        // the real Duo folded and turned the other way (2026-11-10, log 18:56:23)
        assert_eq!(rail_side(insets(0.0, 0.0, 34.0, 84.0)), "left");
        assert_eq!(rail_width(insets(0.0, 0.0, 34.0, 84.0), true), 84.0);
        assert_eq!(rail_width(insets(0.0, 0.0, 34.0, 96.0), true), 96.0);
        // folded upright, open on its side, folded turned the usual way
        assert_eq!(rail_side(insets(0.0, 84.0, 34.0, 0.0)), "right");
        // no strip yet, or one on each side (an iPhone on its side): the right, as before
        assert_eq!(rail_side(Insets::default()), "right");
        assert_eq!(rail_side(insets(0.0, 62.0, 21.0, 62.0)), "right");
        // an iPhone on its side: the rail's 84 px sit inside the 62 px band the island may take, on either side
        assert_eq!(rail_width(insets(0.0, 62.0, 21.0, 62.0), false), 146.0);
        assert_eq!(rail_pad(insets(0.0, 62.0, 21.0, 62.0), false), 62.0);
        // a left inset too small to be a strip does not move the rail
        assert_eq!(rail_side(insets(0.0, 0.0, 34.0, 30.0)), "right");
    }

    #[test]
    fn an_iframe_or_open_confirmation_keeps_the_screen() {
        assert!(!busy_for_reload(false, false, false, false));
        assert!(
            busy_for_reload(true, false, false, false),
            "a field has focus"
        );
        assert!(
            busy_for_reload(false, true, false, false),
            "the dashboard iframe has focus (review L5)"
        );
        assert!(
            busy_for_reload(false, false, true, false),
            "a raised dashboard confirmation"
        );
        assert!(
            busy_for_reload(false, false, false, true),
            "the frame's switch dialog is open"
        );
    }
}
