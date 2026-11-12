//! Viewport overlays live under body, outside page containers and entrance transforms. The shared lifetime owns
//! the scroll lock and focus return, including nested overlays (a switch approval above Details, for example).
use leptos::portal::Portal;
use leptos::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
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
    static REPAIR: RefCell<std::collections::BTreeMap<u64, Rc<dyn Fn()>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static BEFORE_UPDATE: RefCell<std::collections::BTreeMap<u64, Rc<dyn Fn(u64)>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static UPDATE_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

// insertBefore preserves keyed DOM objects but can silently clear focus to BODY. Keep a semantic
// bookmark as well as the exact node, so reconciliation can recover moved or replaced controls.
#[derive(Clone)]
struct FocusTarget {
    element: web_sys::HtmlElement,
    path: Vec<String>,
    role: String,
}

impl FocusTarget {
    fn new(root: &web_sys::Element, element: web_sys::HtmlElement) -> Self {
        let mut path = Vec::new();
        let mut ancestor = Some(element.clone().unchecked_into::<web_sys::Element>());
        while let Some(el) = ancestor {
            if let Some(key) = el.get_attribute("data-focus-key") {
                path.push(key);
            }
            if el == *root {
                break;
            }
            ancestor = el.parent_element();
        }
        path.reverse();
        let role = element.get_attribute("data-focus-role").unwrap_or_else(|| {
            format!(
                "{}:{}:{}",
                element.tag_name(),
                element.get_attribute("type").unwrap_or_default(),
                element
                    .get_attribute("aria-label")
                    .or_else(|| element.get_attribute("data-probe"))
                    .unwrap_or_else(|| element.text_content().unwrap_or_default())
            )
        });
        Self {
            element,
            path,
            role,
        }
    }

    fn resolve(&self, current: &[FocusTarget]) -> Option<web_sys::HtmlElement> {
        current
            .iter()
            .find(|t| t.element == self.element)
            .or_else(|| {
                current
                    .iter()
                    .find(|t| t.path == self.path && t.role == self.role)
            })
            .map(|t| t.element.clone())
    }
}

#[derive(Clone, Default)]
struct FocusMemory {
    target: Option<FocusTarget>,
    order: Vec<FocusTarget>,
}

impl FocusMemory {
    fn remember(&mut self, root: &web_sys::Element, target: web_sys::HtmlElement) {
        self.target = Some(FocusTarget::new(root, target));
        self.order = tab_stops(root)
            .into_iter()
            .map(|el| FocusTarget::new(root, el))
            .collect();
    }

    fn destination(&self, root: &web_sys::Element) -> Option<web_sys::HtmlElement> {
        let current: Vec<_> = tab_stops(root)
            .into_iter()
            .map(|el| FocusTarget::new(root, el))
            .collect();
        if let Some(target) = &self.target {
            if let Some(el) = target.resolve(&current) {
                return Some(el);
            }
            if let Some(at) = self.order.iter().position(|t| t.element == target.element) {
                // Prefer the next row's same action, or the previous row when the last row was removed.
                let neighbours: Vec<_> = self.order[at + 1..]
                    .iter()
                    .chain(self.order[..at].iter().rev())
                    .collect();
                for neighbour in neighbours
                    .iter()
                    .copied()
                    .filter(|t| t.role == target.role)
                    .chain(neighbours.iter().copied())
                {
                    if let Some(el) = neighbour.resolve(&current) {
                        return Some(el);
                    }
                }
            }
        }
        current.first().map(|t| t.element.clone())
    }
}

struct FocusUpdate {
    id: u64,
    before: FocusMemory,
}

/// Capture actual focus after a response arrives, immediately before its reactive data is applied.
/// Only DOM-caused losses during this update may recover; historical focus is not a selection.
pub(crate) fn reconcile_overlay_focus<T>(update: impl FnOnce() -> T) -> T {
    let id = UPDATE_ID.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    let top = STACK.with(|s| s.borrow().last().map(|(id, _)| *id));
    let before = top.and_then(|id| BEFORE_UPDATE.with(|all| all.borrow().get(&id).cloned()));
    if let Some(before) = before {
        before(id);
    }
    update()
}

fn touched_focus(records: &js_sys::Array, focused: &web_sys::HtmlElement) -> bool {
    records.iter().any(|record| {
        let get = |key: &str| js_sys::Reflect::get(&record, &key.into()).ok();
        if get("type").and_then(|v| v.as_string()).as_deref() == Some("attributes") {
            return get("target")
                .and_then(|v| v.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|node| node.contains(Some(focused.as_ref())));
        }
        get("removedNodes")
            .and_then(|v| v.dyn_into::<web_sys::NodeList>().ok())
            .is_some_and(|nodes| {
                (0..nodes.length()).any(|i| {
                    nodes
                        .item(i)
                        .is_some_and(|node| node.contains(Some(focused.as_ref())))
                })
            })
    })
}

fn repair_top_focus() {
    let top = STACK.with(|s| s.borrow().last().map(|(id, _)| *id));
    let repair = top.and_then(|id| REPAIR.with(|r| r.borrow().get(&id).cloned()));
    if let Some(repair) = repair {
        repair();
    }
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
    let memory = Rc::new(RefCell::new(FocusMemory::default()));
    let repair: Rc<dyn Fn()> = {
        let memory = memory.clone();
        Rc::new(move || {
            if !trap_focus || !top_overlay(id) || preparing_reverse.get_value() {
                return;
            }
            let Some(root) = node.get_untracked() else {
                return;
            };
            if let Some(active) = document()
                .active_element()
                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
            {
                if root.contains(Some(active.as_ref()))
                    && !active.has_attribute("data-overlay-guard")
                {
                    memory.borrow_mut().remember(root.as_ref(), active);
                    return;
                }
            }
            // Release the borrow before focus(), whose synchronous focusin records the new bookmark.
            let destination = memory.borrow().destination(root.as_ref());
            if let Some(el) = destination {
                focus(&el);
            } else if let Ok(Some(dialog)) = root.query_selector("[role='dialog']") {
                if let Ok(dialog) = dialog.dyn_into::<web_sys::HtmlElement>() {
                    let _ = dialog.set_attribute("tabindex", "-1");
                    focus(&dialog);
                }
            }
        })
    };
    REPAIR.with(|r| {
        r.borrow_mut().insert(id, repair.clone());
    });
    let pending = Rc::new(RefCell::new(None::<FocusUpdate>));
    let capture = {
        let pending = pending.clone();
        Rc::new(move |update_id: u64| {
            pending.borrow_mut().take();
            if !trap_focus || !top_overlay(id) || !document().has_focus().unwrap_or(false) {
                return;
            }
            let Some(root) = node.get_untracked() else {
                return;
            };
            let Some(active) = document()
                .active_element()
                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
            else {
                return;
            };
            if !root.contains(Some(active.as_ref())) || active.has_attribute("data-overlay-guard") {
                return;
            }
            let mut before = FocusMemory::default();
            before.remember(root.as_ref(), active);
            *pending.borrow_mut() = Some(FocusUpdate {
                id: update_id,
                before,
            });
            // Render effects and mutation delivery run before the next paint. A no-op update must not arm
            // recovery for a later, unrelated mutation. New updates have their own expiration token.
            let pending = pending.clone();
            let expire = Closure::once_into_js(move || {
                let same = pending.borrow().as_ref().is_some_and(|p| p.id == update_id);
                if same {
                    pending.borrow_mut().take();
                }
            });
            let _ = window().request_animation_frame(expire.unchecked_ref());
        })
    };
    BEFORE_UPDATE.with(|all| {
        all.borrow_mut().insert(id, capture);
    });
    let observer = StoredValue::new_local(
        None::<(
            web_sys::MutationObserver,
            Closure<dyn Fn(js_sys::Array, web_sys::MutationObserver)>,
        )>,
    );
    let pending_mutation = pending.clone();
    Effect::new(move |_| {
        let Some(root) = node.get() else {
            return;
        };
        let pending = pending_mutation.clone();
        let observed_root = root.clone();
        let callback =
            Closure::<dyn Fn(js_sys::Array, web_sys::MutationObserver)>::new(move |records, _| {
                if !trap_focus
                    || !top_overlay(id)
                    || preparing_reverse.get_value()
                    || !document().has_focus().unwrap_or(false)
                {
                    return;
                }
                let cleared = document().active_element().is_none_or(|active| {
                    !active.is_connected()
                        || document()
                            .body()
                            .is_some_and(|body| js_sys::Object::is(active.as_ref(), body.as_ref()))
                });
                if !cleared {
                    return;
                }
                let eligible = pending.borrow().as_ref().is_some_and(|update| {
                    update
                        .before
                        .target
                        .as_ref()
                        .is_some_and(|target| touched_focus(&records, &target.element))
                });
                if !eligible {
                    return;
                }
                let Some(update) = pending.borrow_mut().take() else {
                    return;
                };
                if let Some(destination) = update.before.destination(observed_root.as_ref()) {
                    focus(&destination);
                } else if let Ok(Some(dialog)) = observed_root.query_selector("[role='dialog']") {
                    if let Ok(dialog) = dialog.dyn_into::<web_sys::HtmlElement>() {
                        let _ = dialog.set_attribute("tabindex", "-1");
                        focus(&dialog);
                    }
                }
            });
        if let Ok(observer_value) =
            web_sys::MutationObserver::new(callback.as_ref().unchecked_ref())
        {
            let options = web_sys::MutationObserverInit::new();
            options.set_child_list(true);
            options.set_subtree(true);
            options.set_attributes(true);
            options.set_attribute_filter(&js_sys::Array::of4(
                &"disabled".into(),
                &"hidden".into(),
                &"inert".into(),
                &"aria-hidden".into(),
            ));
            let _ = observer_value.observe_with_options(root.as_ref(), &options);
            observer.set_value(Some((observer_value, callback)));
        }
    });
    // Capture phase observes user intent even when a child consumes the key/pointer event.
    // Cancellation never changes focus: reading text may deliberately leave BODY selected.
    let intent_pending = pending.clone();
    let intent = Closure::<dyn Fn(web_sys::Event)>::new(move |_| {
        intent_pending.borrow_mut().take();
    });
    for event in ["pointerdown", "keydown", "focusin"] {
        let _ = window().add_event_listener_with_callback_and_bool(
            event,
            intent.as_ref().unchecked_ref(),
            true,
        );
    }
    let intent = StoredValue::new_local(Some(intent));
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
    let focus_memory = memory.clone();
    let focus_pending = pending.clone();
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
        focus_pending.borrow_mut().take();
        let edge = target.get_attribute("data-overlay-guard");
        let elements = tab_stops(root.as_ref());
        place_start_guard(root.as_ref(), &elements);
        if edge.is_none() && root.contains(Some(target.as_ref())) {
            focus_memory
                .borrow_mut()
                .remember(root.as_ref(), target.clone());
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
    let cleanup_pending = StoredValue::new_local(pending);
    on_cleanup(move || {
        keys.remove();
        if let Some(Some(handler)) = intent.try_update_value(|h| h.take()) {
            for event in ["pointerdown", "keydown", "focusin"] {
                let _ = window().remove_event_listener_with_callback_and_bool(
                    event,
                    handler.as_ref().unchecked_ref(),
                    true,
                );
            }
        }
        if let Some(pending) = cleanup_pending.try_get_value() {
            pending.borrow_mut().take();
        }
        BEFORE_UPDATE.with(|all| {
            all.borrow_mut().remove(&id);
        });
        if let Some(Some((observer, _callback))) = observer.try_update_value(|o| o.take()) {
            observer.disconnect();
        }
        REPAIR.with(|r| {
            r.borrow_mut().remove(&id);
        });
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
            repair_top_focus();
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
