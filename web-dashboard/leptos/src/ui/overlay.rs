//! Viewport overlays live under body, outside page containers and entrance transforms. The shared lifetime owns
//! the scroll lock and focus return, including nested overlays (a switch approval above Details, for example).
use leptos::portal::Portal;
use leptos::prelude::*;
use std::cell::RefCell;
use wasm_bindgen::JsCast;

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
    let keys = window_event_listener(leptos::ev::keydown, move |e| {
        if !STACK.with(|s| s.borrow().last().map(|(n, _)| *n) == Some(id)) {
            return;
        }
        if e.key() == "Escape" {
            if let Some(close) = on_close {
                e.prevent_default();
                close.run(());
            }
        } else if e.key() == "Tab" && trap_focus {
            let Some(node) = node.get_untracked() else {
                return;
            };
            let Ok(all) = node.query_selector_all("button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex='0']") else { return; };
            let elements: Vec<_> = (0..all.length())
                .filter_map(|i| all.item(i)?.dyn_into::<web_sys::HtmlElement>().ok())
                .filter(|el| el.get_bounding_client_rect().height() > 0.0)
                .collect();
            let current = document().active_element();
            let at = elements
                .iter()
                .position(|el| current.as_ref() == Some(el.as_ref()));
            if !elements.is_empty() {
                let next = if e.shift_key() {
                    at.filter(|i| *i > 0)
                        .map(|i| i - 1)
                        .unwrap_or(elements.len() - 1)
                } else {
                    at.map(|i| (i + 1) % elements.len()).unwrap_or(0)
                };
                e.prevent_default();
                focus(&elements[next]);
            }
        }
    });
    // Portal mounting happens in an effect. Wait for its node, then focus without scrolling the fixed body/page.
    Effect::new(move |_| {
        if let Some(node) = node.get() {
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
        unlock(id);
        if let Some(Some(el)) = previous.try_get_value() {
            if el.is_connected() {
                focus(&el);
            }
        }
    });
    view! { <Portal><div class="overlay-root" node_ref=node>{children()}</div></Portal> }
}
