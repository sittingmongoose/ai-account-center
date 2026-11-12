//! Keyed overlay collections keep the browser's actual focused subtree in place, including UA controls.
use leptos::prelude::*;
use std::{cell::Cell, hash::Hash};
use wasm_bindgen::{JsCast, closure::Closure};

thread_local! { static NEXT: Cell<u64> = const { Cell::new(0) }; }

fn held_order<K: Clone + Eq + Hash>(
    previous: Option<&Vec<K>>,
    available: Vec<K>,
    limit: usize,
    focused: bool,
) -> Vec<K> {
    let desired: Vec<_> = available.iter().take(limit).cloned().collect();
    let Some(previous) = previous.filter(|_| focused) else {
        return desired;
    };
    let present: std::collections::HashSet<_> = available.iter().collect();
    let mut held: Vec<_> = previous
        .iter()
        .filter(|key| present.contains(key))
        .take(limit)
        .cloned()
        .collect();
    // Tachys may reinsert a survivor when a single diff combines deletions and additions. Separate
    // those passes while focused; the hook schedules a safe append on the next frame.
    if held.len() != previous.len() {
        return held;
    }
    for key in desired {
        if held.len() >= limit {
            break;
        }
        if !held.contains(&key) {
            held.push(key);
        }
    }
    held
}

/// Apply to a collection's full available keys, with its display limit separately. Bind the returned
/// id to data-focus-list on the existing list container. Nested lists independently hold/release as
/// focus crosses their boundaries; data memos stay live and genuine removals are never retained.
pub(crate) fn hold_order<K: Clone + Eq + Hash + Send + Sync + 'static>(
    available: Memo<Vec<K>>,
    limit: Option<Memo<usize>>,
) -> (Memo<Vec<K>>, u64) {
    let id = NEXT.with(|next| {
        next.set(next.get() + 1);
        next.get()
    });
    let selector = format!("[data-focus-list=\"{id}\"]");
    let changed = RwSignal::new(0_u64);
    let keys = Memo::new(move |previous: Option<&Vec<K>>| {
        changed.track();
        let focused = document()
            .active_element()
            .is_some_and(|el| el.closest(&selector).ok().flatten().is_some());
        let full = available.get();
        let n = limit.map(|limit| limit.get()).unwrap_or(full.len());
        let split =
            focused && previous.is_some_and(|old| old.iter().any(|key| !full.contains(key)));
        let order = held_order(previous, full, n, focused);
        if split {
            let _ = request_animation_frame(move || {
                changed.try_update(|n| *n += 1);
            });
        }
        order
    });
    let handler = Closure::<dyn Fn(web_sys::Event)>::new(move |_| {
        // Native focusout can precede the next selected stop. Read its settled selection next frame.
        let _ = request_animation_frame(move || {
            changed.try_update(|n| *n += 1);
        });
    });
    for event in ["focusin", "focusout"] {
        let _ = window().add_event_listener_with_callback_and_bool(
            event,
            handler.as_ref().unchecked_ref(),
            true,
        );
    }
    let handler = StoredValue::new_local(Some(handler));
    on_cleanup(move || {
        if let Some(Some(handler)) = handler.try_update_value(|value| value.take()) {
            for event in ["focusin", "focusout"] {
                let _ = window().remove_event_listener_with_callback_and_bool(
                    event,
                    handler.as_ref().unchecked_ref(),
                    true,
                );
            }
        }
    });
    (keys, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(s: &str) -> Vec<char> {
        s.chars().collect()
    }
    #[test]
    fn holds_order_and_releases_the_latest_values() {
        assert_eq!(
            held_order(Some(&keys("ABC")), keys("CAB"), 6, true),
            keys("ABC")
        );
        assert_eq!(
            held_order(Some(&keys("ABC")), keys("BCA"), 6, false),
            keys("BCA")
        );
    }
    #[test]
    fn separates_removal_and_addition_without_moving_survivors() {
        assert_eq!(
            held_order(Some(&keys("ABC")), keys("DCA"), 6, true),
            keys("AC")
        );
        assert_eq!(
            held_order(Some(&keys("AC")), keys("DCA"), 6, true),
            keys("ACD")
        );
    }
    #[test]
    fn a_display_limit_never_evicts_a_surviving_focused_item() {
        assert_eq!(
            held_order(Some(&keys("ABCDEF")), keys("GFEDCBA"), 6, true),
            keys("ABCDEF")
        );
        assert_eq!(
            held_order(Some(&keys("ABCDEF")), keys("GFEDCBA"), 6, false),
            keys("GFEDCB")
        );
    }
    #[test]
    fn composite_keys_keep_duplicate_occurrences_distinct() {
        let old = vec![("a", 0), ("b", 0), ("a", 1)];
        assert_eq!(held_order(None, old.clone(), 6, true), old);
        assert_eq!(
            held_order(Some(&old), vec![("a", 0), ("a", 1), ("b", 0)], 6, true),
            old
        );
    }
}
