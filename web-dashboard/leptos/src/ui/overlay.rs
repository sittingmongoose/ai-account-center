//! Viewport overlays live under body, outside page containers and entrance transforms. The shared lifetime owns
//! the scroll lock and focus return, including nested overlays (a switch approval above Details, for example).
use leptos::portal::Portal;
use leptos::prelude::*;
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

struct ScrollLock {
    x: f64,
    y: f64,
    body: Vec<(&'static str, String, String)>,
    root_overflow: (String, String),
}

thread_local! {
    static LOCK: RefCell<Option<ScrollLock>> = const { RefCell::new(None) };
    static STACK: RefCell<Vec<(u64, bool)>> = const { RefCell::new(Vec::new()) };
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn focus(el: &web_sys::HtmlElement) {
    let options = web_sys::FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = el.focus_with_options(&options);
}

fn lock(id: u64, scroll: bool) {
    STACK.with(|s| s.borrow_mut().push((id, scroll)));
    if !scroll {
        return;
    }
    LOCK.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let Some(body) = document().body() else {
            return;
        };
        let Some(root) = document()
            .document_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let style = body.style();
        let x = window().scroll_x().unwrap_or(0.0);
        let y = window().scroll_y().unwrap_or(0.0);
        let saved = ScrollLock {
            x,
            y,
            body: ["position", "top", "left", "width", "overflow"]
                .into_iter()
                .map(|p| {
                    (
                        p,
                        style.get_property_value(p).unwrap_or_default(),
                        style.get_property_priority(p),
                    )
                })
                .collect(),
            root_overflow: (
                root.style()
                    .get_property_value("overflow")
                    .unwrap_or_default(),
                root.style().get_property_priority("overflow"),
            ),
        };
        // Fixed body also prevents iOS rubber-band/background scrolling. Preserve the page at its current offset.
        for (p, v) in [
            ("position", "fixed".into()),
            ("top", format!("{}px", -y)),
            ("left", format!("{}px", -x)),
            ("width", "100%".into()),
            ("overflow", "hidden".into()),
        ] {
            let _ = style.set_property(p, &v);
        }
        let _ = root.style().set_property("overflow", "hidden");
        *slot.borrow_mut() = Some(saved);
    });
}

fn unlock(id: u64) {
    let empty = STACK.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|(n, _)| *n != id);
        !s.iter().any(|(_, locked)| *locked)
    });
    if !empty {
        return;
    }
    LOCK.with(|slot| {
        let Some(saved) = slot.borrow_mut().take() else {
            return;
        };
        if let Some(body) = document().body() {
            for (p, v, priority) in saved.body {
                let _ = body.style().set_property_with_priority(p, &v, &priority);
            }
        }
        if let Some(root) = document()
            .document_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = root.style().set_property_with_priority(
                "overflow",
                &saved.root_overflow.0,
                &saved.root_overflow.1,
            );
            let behavior = root
                .style()
                .get_property_value("scroll-behavior")
                .unwrap_or_default();
            let priority = root.style().get_property_priority("scroll-behavior");
            let _ = root.style().set_property("scroll-behavior", "auto");
            window().scroll_to_with_x_and_y(saved.x, saved.y);
            let _ =
                root.style()
                    .set_property_with_priority("scroll-behavior", &behavior, &priority);
        }
    });
}

/// Boundary destinations in the browser's sequential order. Navigation *within* the overlay stays native, so
/// user-agent media controls and iframe documents keep their own internal Tab stops.
fn tab_stops(root: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    let Ok(nodes) = root.query_selector_all("button, input, select, textarea, a[href], area[href], summary, iframe, object, embed, audio[controls], video[controls], [contenteditable], [tabindex]") else { return Vec::new(); };
    let editable = |el: &web_sys::Element| {
        js_sys::Reflect::get(el, &"isContentEditable".into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    let mut elements: Vec<_> = (0..nodes.length())
        .filter_map(|i| nodes.item(i)?.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|el| {
            if el.has_attribute("data-overlay-guard")
                || el.matches(":disabled").unwrap_or(false)
                || el.closest("[inert]").ok().flatten().is_some()
            {
                return false;
            }
            let rect = el.get_bounding_client_rect();
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                return false;
            }
            if window()
                .get_computed_style(el)
                .ok()
                .flatten()
                .is_some_and(|s| {
                    matches!(
                        s.get_property_value("visibility")
                            .unwrap_or_default()
                            .as_str(),
                        "hidden" | "collapse"
                    )
                })
            {
                return false;
            }
            if !el.has_attribute("tabindex") {
                // Only the first direct summary is natively sequentially focusable.
                if el.tag_name() == "SUMMARY" {
                    return el
                        .parent_element()
                        .filter(|p| p.tag_name() == "DETAILS")
                        .and_then(|p| p.query_selector(":scope > summary").ok().flatten())
                        .is_some_and(|first| js_sys::Object::is(first.as_ref(), el.as_ref()));
                }
                // An editing host is focusable without tabindex; a nested editable needs an explicit tabindex.
                if editable(el.as_ref()) {
                    return !el.parent_element().is_some_and(|p| editable(&p));
                }
                if matches!(el.tag_name().as_str(), "VIDEO" | "AUDIO")
                    && el.has_attribute("controls")
                {
                    return true;
                }
            }
            el.tab_index() >= 0
        })
        .collect();
    // Positive tabindex precedes zero/native stops. Stable sorting keeps DOM order for equal tabindex.
    elements.sort_by_key(tab_rank);
    elements
}

fn tab_rank(el: &web_sys::HtmlElement) -> i32 {
    if el.tab_index() > 0 {
        el.tab_index()
    } else {
        i32::MAX
    }
}

fn place_start_guard(root: &web_sys::Element, elements: &[web_sys::HtmlElement]) {
    if let Ok(Some(guard)) = root.query_selector("[data-overlay-guard='start']") {
        if let Ok(guard) = guard.dyn_into::<web_sys::HtmlElement>() {
            // A positive guard tied with the first positive stop precedes it in DOM order. Reverse Tab from
            // that stop (including a media/iframe host) reaches the guard before it can leave the document.
            guard.set_tab_index(
                elements
                    .first()
                    .filter(|el| el.tab_index() > 0)
                    .map(|el| el.tab_index())
                    .unwrap_or(0),
            );
        }
    }
}

fn top_overlay(id: u64) -> bool {
    STACK.with(|s| s.borrow().last().map(|(n, _)| *n) == Some(id))
}

/// Mount only while open. Existing scrim/grab/X handlers stay with the sheet; Escape and Tab use the top overlay.
#[component]
pub fn OverlayPortal(
    children: ChildrenFn,
    #[prop(optional)] on_close: Option<Callback<()>>,
    #[prop(default = true)] trap_focus: bool,
    #[prop(default = true)] lock_scroll: bool,
) -> impl IntoView {
    let id = NEXT.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    let previous = StoredValue::new_local(
        document()
            .active_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()),
    );
    let node = NodeRef::<leptos::html::Div>::new();
    lock(id, lock_scroll);
    let backwards = StoredValue::new(false);
    let last_focus = StoredValue::new_local(None::<web_sys::HtmlElement>);
    let preparing_reverse = StoredValue::new(false);
    let keys = window_event_listener(leptos::ev::keydown, move |e| {
        if !top_overlay(id) {
            return;
        }
        if e.key() == "Escape" && !e.default_prevented() {
            if let Some(close) = on_close {
                e.prevent_default();
                close.run(());
            }
        } else if e.key() == "Tab"
            && trap_focus
            && !e.default_prevented()
            && !e.ctrl_key()
            && !e.alt_key()
            && !e.meta_key()
        {
            // Do not prevent Tab here: native media/shadow controls and iframe documents own their inner order.
            backwards.set_value(e.shift_key());
            if let Some(root) = node.get_untracked() {
                let stops = tab_stops(root.as_ref());
                place_start_guard(root.as_ref(), &stops);
                // Move the navigation starting point before the trusted key's default action. Focusing a
                // media/frame host after navigation has ended cannot enter its last internal Tab stop.
                // Do not intercept keys inside a browser-owned focus scope: its own reverse order is native.
                if e.shift_key() {
                    if let Some(first) = stops.first() {
                        let at_first = document()
                            .active_element()
                            .is_some_and(|el| js_sys::Object::is(el.as_ref(), first.as_ref()));
                        let scope = first
                            .matches("video[controls], audio[controls], iframe, object, embed")
                            .unwrap_or(false);
                        if at_first && !scope {
                            if let Ok(Some(end)) = root.query_selector("[data-overlay-guard='end']")
                            {
                                if let Ok(end) = end.dyn_into::<web_sys::HtmlElement>() {
                                    preparing_reverse.set_value(true);
                                    let _ = end.focus();
                                    preparing_reverse.set_value(false);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    // Redirecting focus dispatches focusin synchronously. This callback must be Fn (reentrant), rather than
    // window_event_listener's FnMut wrapper, which rejects a recursive invocation during focus().
    let containment = Closure::<dyn Fn(web_sys::FocusEvent)>::new(move |e: web_sys::FocusEvent| {
        if !trap_focus || !top_overlay(id) || preparing_reverse.get_value() {
            return;
        }
        let Some(root) = node.get_untracked() else {
            return;
        };
        let Some(target) = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let edge = target.get_attribute("data-overlay-guard");
        let elements = tab_stops(root.as_ref());
        place_start_guard(root.as_ref(), &elements);
        if edge.is_none() && root.contains(Some(target.as_ref())) {
            last_focus.set_value(Some(target));
            return;
        }
        let destination = match edge.as_deref() {
            Some("start") => elements.last(),
            Some("end") => elements.first(),
            _ => {
                // Positive tabindex in the background can lie between this overlay's positive and zero stops.
                // Correct only that attempted exit, using the last focused host and the Tab direction.
                let last = last_focus.get_value();
                // Focus in a frame does not bubble its keydown to this document. Infer an attempted exit's
                // direction from the actual browser order as well as the parent document's last Tab key.
                let reverse = last
                    .as_ref()
                    .map(|last| {
                        let (a, b) = (tab_rank(last), tab_rank(&target));
                        if a != b {
                            b < a
                        } else {
                            last.compare_document_position(target.as_ref())
                                & web_sys::Node::DOCUMENT_POSITION_PRECEDING
                                != 0
                        }
                    })
                    .unwrap_or_else(|| backwards.get_value());
                let at = last.and_then(|last| elements.iter().position(|el| *el == last));
                at.and_then(|at| {
                    let next = if reverse {
                        (at + elements.len() - 1) % elements.len()
                    } else {
                        (at + 1) % elements.len()
                    };
                    elements.get(next)
                })
                .or_else(|| {
                    if reverse {
                        elements.last()
                    } else {
                        elements.first()
                    }
                })
            }
        };
        if let Some(el) = destination {
            // Native focus scrolls the destination into its modal scroller; only initial/return focus suppresses scroll.
            let _ = el.focus();
        } else if let Ok(Some(dialog)) = root.query_selector("[role='dialog']") {
            if let Ok(dialog) = dialog.dyn_into::<web_sys::HtmlElement>() {
                let _ = dialog.set_attribute("tabindex", "-1");
                focus(&dialog);
            }
        }
    });
    let _ =
        window().add_event_listener_with_callback("focusin", containment.as_ref().unchecked_ref());
    let containment = StoredValue::new_local(Some(containment));
    // Portal mounting happens in an effect. Wait for its node, then focus without scrolling the fixed body/page.
    Effect::new(move |_| {
        if let Some(node) = node.get() {
            if !top_overlay(id) {
                return;
            }
            place_start_guard(node.as_ref(), &tab_stops(node.as_ref()));
            let target = [
                ".sheet-close",
                "[data-probe='details-close']",
                ".btn.primary",
                ".viewer-btn.close",
                "button:not(.sheet-scrim):not(.details-scrim)",
            ]
            .into_iter()
            .find_map(|selector| node.query_selector(selector).ok().flatten());
            if let Some(el) = target.and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok()) {
                focus(&el);
            }
        }
    });
    on_cleanup(move || {
        keys.remove();
        if let Some(Some(handler)) = containment.try_update_value(|h| h.take()) {
            let _ = window()
                .remove_event_listener_with_callback("focusin", handler.as_ref().unchecked_ref());
        }
        // Removing a lower overlay (for example a pin deleted by a poll) must not refocus over its viewer.
        let was_top = top_overlay(id);
        unlock(id);
        if was_top {
            if let Some(Some(el)) = previous.try_get_value() {
                if el.is_connected() {
                    focus(&el);
                }
            }
        }
    });
    view! {
        <Portal><div class="overlay-root" style:z-index=(70 + id).to_string() node_ref=node>
            {trap_focus.then(|| view! { <span class="overlay-focus-guard" tabindex="0" aria-hidden="true" data-overlay-guard="start"></span> })}
            {children()}
            {trap_focus.then(|| view! { <span class="overlay-focus-guard" tabindex="0" aria-hidden="true" data-overlay-guard="end"></span> })}
        </div></Portal>
    }
}
