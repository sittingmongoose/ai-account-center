//! Browser listeners and sockets that last exactly as long as the reactive owner that made them. In the app frame
//! Status and the Scoreboard unmount at sign-out and mount again at the next sign-in: whatever the old page started
//! must stop with it, or it would read the disposed page's signals (a panic). Leptos's own timers already stop with
//! their owner; browser listeners and the hint socket are held here and released by the owner's cleanup.

use leptos::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

type Teardown = Box<dyn FnOnce()>;

thread_local! {
    /// Teardowns by id: `on_cleanup` needs a Send + Sync closure, and these hold browser objects, so the cleanup
    /// carries only the id.
    static HELD: RefCell<(u32, HashMap<u32, Teardown>)> = RefCell::new((0, HashMap::new()));
}

/// Runs `teardown` when the current reactive owner is cleaned up (never, outside any owner).
pub fn on_owner_cleanup(teardown: impl FnOnce() + 'static) {
    let id = HELD.with(|held| {
        let mut held = held.borrow_mut();
        held.0 = held.0.wrapping_add(1);
        let id = held.0;
        held.1.insert(id, Box::new(teardown));
        id
    });
    on_cleanup(move || {
        // Taken out before it runs: a teardown may itself add or release one.
        let teardown = HELD.with(|held| held.borrow_mut().1.remove(&id));
        if let Some(teardown) = teardown {
            teardown();
        }
    });
}

/// `event` on `target` calls `f` until the current owner is cleaned up.
pub fn listen(target: &web_sys::EventTarget, event: &'static str, f: impl Fn() + 'static) {
    let closure = Closure::<dyn Fn()>::new(f);
    let _ = target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref());
    let target = target.clone();
    on_owner_cleanup(move || {
        let _ = target.remove_event_listener_with_callback(event, closure.as_ref().unchecked_ref());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn a_teardown_runs_once_when_its_owner_is_cleaned_up() {
        let owner = Owner::new();
        let ran = Rc::new(Cell::new(0));
        let seen = ran.clone();
        owner.with(|| on_owner_cleanup(move || seen.set(seen.get() + 1)));
        assert_eq!(ran.get(), 0);
        owner.cleanup();
        assert_eq!(ran.get(), 1);
        owner.cleanup();
        assert_eq!(ran.get(), 1);
    }
}
