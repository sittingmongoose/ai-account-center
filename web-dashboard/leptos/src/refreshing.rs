//! The header's Refresh while a refresh runs (owner report 2026-11-20: "it should animate while it's refreshing").
//! A press starts one refresh and the button spins until that refresh says it has finished: the dashboard controller's
//! or Analytics' `done` callback (public/frame-dashboard.mjs `command`, public/frame-analytics.mjs `act`), or the page's
//! own read for Status and the Scoreboard (`ChromeInfo::refresh`). It spins for at least `MIN_MS`, so a fast refresh
//! still shows, and at most `CAP_MS`: a refresh that never answers stops spinning then, and the page's own words
//! (failed, old reading) say what happened. A press while one runs starts nothing.
//!
//! The times are the page's monotonic clock (`performance.now()`), so a wall-clock step during a refresh never
//! strands it (review L2). `Refreshing` is pure, so the rules are tested without a browser; frame.rs keeps it in a
//! signal and this module arms the timers.

use leptos::prelude::*;
use wasm_bindgen::prelude::Closure;

/// The shortest time the button spins.
pub const MIN_MS: f64 = 600.0;
/// The longest: a refresh still running then is left to the page's own words.
pub const CAP_MS: f64 = 60_000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Refreshing {
    /// Numbers each press, so a late answer or timer of an earlier refresh never ends a later one.
    run: u64,
    /// When the refresh on screen was pressed; None while none runs.
    since: Option<f64>,
    /// When it said it had finished.
    finished: Option<f64>,
}

impl Refreshing {
    /// Whether the button spins.
    pub fn on(&self) -> bool {
        self.since.is_some()
    }

    /// A press: the number of the refresh to start, or None while one runs (a second press starts nothing).
    pub fn press(&mut self, now: f64) -> Option<u64> {
        if self.on() {
            return None;
        }
        self.run += 1;
        self.since = Some(now);
        self.finished = None;
        Some(self.run)
    }

    /// When the spinning ends: the cap until the refresh has finished, then its finish but not before `MIN_MS`.
    pub fn ends_at(&self) -> Option<f64> {
        let since = self.since?;
        Some(match self.finished {
            Some(at) => at.max(since + MIN_MS).min(since + CAP_MS),
            None => since + CAP_MS,
        })
    }

    /// Refresh `run` has finished. Returns the milliseconds to wait before it stops spinning (to arm a timer for
    /// `expire`); None when nothing is left to wait for: it stopped now, or the answer is not this refresh's.
    pub fn finish(&mut self, run: u64, now: f64) -> Option<f64> {
        if run != self.run || !self.on() || self.finished.is_some() {
            return None;
        }
        self.finished = Some(now);
        let wait = self.ends_at()? - now;
        if wait <= 0.0 {
            self.stop();
            return None;
        }
        Some(wait)
    }

    /// A timer of refresh `run` fired: it stops spinning if its time is up. True when it stopped.
    // Native lib builds do not arm browser timers; wasm and the unit tests call this.
    #[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
    pub fn expire(&mut self, run: u64, now: f64) -> bool {
        if run != self.run || !self.on() {
            return false;
        }
        // a browser timer may fire a millisecond early by Date.now()'s clock
        if self.ends_at().is_some_and(|end| now + 2.0 >= end) {
            self.stop();
            return true;
        }
        false
    }

    /// The session ended: nothing spins any more, and an answer of the refresh that ran is not a later one's.
    pub fn cancel(&mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.since = None;
        self.finished = None;
    }
}

/// One pressed refresh, handed to whatever runs it: `finished` once it has (Copy, so a page or the dashboard
/// controller's callback can hold it).
#[derive(Clone, Copy)]
pub struct Done {
    state: RwSignal<Refreshing>,
    run: u64,
}

impl Done {
    /// The refresh has finished (answered, failed, or was not run at all): the button stops spinning, after the rest
    /// of `MIN_MS`.
    pub fn finished(self) {
        let now = clock();
        if let Some(wait) = self.state.try_update(|r| r.finish(self.run, now)).flatten() {
            arm(self.state, self.run, wait);
        }
    }
}

/// Hands `done` to a controller as its JavaScript `done` callback: `call` makes the call with it and says whether the
/// controller took the command. Taken, the callback is the controller's (it frees itself once called); refused, it is
/// dropped here, never leaked, and every press finishes at once (review L3).
pub fn hand_over(done: Vec<Done>, call: impl FnOnce(&wasm_bindgen::JsValue) -> bool) {
    let told = done.clone();
    let callback = Closure::once(move || {
        for d in told {
            d.finished();
        }
    });
    if call(callback.as_ref()) {
        let _ = callback.into_js_value();
    } else {
        drop(callback);
        for d in done {
            d.finished();
        }
    }
}

/// The page's monotonic clock in milliseconds (`performance.now()`); `Date.now()` only where there is none.
#[cfg(target_arch = "wasm32")]
fn clock() -> f64 {
    use wasm_bindgen::JsCast;
    js_sys::Reflect::get(&window(), &"performance".into())
        .ok()
        .filter(|p| p.is_object())
        .and_then(|p| {
            let now = js_sys::Reflect::get(&p, &"now".into()).ok()?;
            now.dyn_ref::<js_sys::Function>()?.call0(&p).ok()?.as_f64()
        })
        .unwrap_or_else(js_sys::Date::now)
}

/// Off the browser (unit tests) the clock and the timers are the test's own: `test_clock`.
#[cfg(not(target_arch = "wasm32"))]
fn clock() -> f64 {
    test_clock::NOW.with(|n| n.get())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod test_clock {
    use std::cell::{Cell, RefCell};
    thread_local! {
        /// What `clock()` reads.
        pub static NOW: Cell<f64> = const { Cell::new(0.0) };
        /// Each armed timer: the refresh's run and its wait in milliseconds.
        pub static ARMED: RefCell<Vec<(u64, f64)>> = const { RefCell::new(Vec::new()) };
    }
}

/// A press of the header's Refresh: the refresh to run, or None while one runs (it starts nothing then).
pub fn press(state: RwSignal<Refreshing>) -> Option<Done> {
    let now = clock();
    let run = state.try_update(|r| r.press(now)).flatten()?;
    arm(state, run, CAP_MS);
    Some(Done { state, run })
}

/// A plain browser timer (a Leptos one is cancelled with the owner that started it, NOTES.md): refresh `run` stops
/// spinning then if its time is up.
#[cfg(target_arch = "wasm32")]
fn arm(state: RwSignal<Refreshing>, run: u64, wait_ms: f64) {
    use wasm_bindgen::JsCast;
    let due = Closure::once_into_js(move || expire_now(state, run));
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
        due.unchecked_ref(),
        wait_ms.ceil().max(0.0) as i32,
    );
}

/// Off the browser the test fires the timer itself (`expire_now`).
#[cfg(not(target_arch = "wasm32"))]
fn arm(_state: RwSignal<Refreshing>, run: u64, wait_ms: f64) {
    test_clock::ARMED.with(|a| a.borrow_mut().push((run, wait_ms)));
}

/// A timer of refresh `run` fired. The state changes only when it stops: a timer of an earlier refresh redraws
/// nothing.
// Native lib builds do not arm browser timers; wasm and the unit tests call this.
#[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
pub(crate) fn expire_now(state: RwSignal<Refreshing>, run: u64) {
    let now = clock();
    let stopped = state
        .try_with_untracked(|r| {
            let mut next = *r;
            next.expire(run, now).then_some(next)
        })
        .flatten();
    if let Some(next) = stopped {
        let _ = state.try_set(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_spins_until_the_refresh_finishes_and_at_least_the_minimum() {
        let mut r = Refreshing::default();
        assert!(!r.on());
        let run = r.press(1_000.0).expect("a press starts a refresh");
        assert!(r.on());
        assert_eq!(
            r.ends_at(),
            Some(1_000.0 + CAP_MS),
            "until it finishes, the cap"
        );
        // it finished 100 ms after the press: it keeps spinning for the rest of the minimum
        let wait = r.finish(run, 1_100.0).expect("waits for the minimum");
        assert_eq!(wait, MIN_MS - 100.0);
        assert!(r.on());
        assert!(!r.expire(run, 1_300.0), "not before the minimum");
        assert!(r.on());
        assert!(r.expire(run, 1_000.0 + MIN_MS));
        assert!(!r.on());
    }

    #[test]
    fn a_slow_refresh_stops_spinning_when_it_finishes() {
        let mut r = Refreshing::default();
        let run = r.press(0.0).unwrap();
        assert!(!r.expire(run, 5_000.0), "a cap timer is not due yet");
        assert_eq!(r.finish(run, 5_000.0), None, "nothing left to wait for");
        assert!(!r.on());
    }

    #[test]
    fn a_refresh_that_never_answers_stops_at_the_cap() {
        let mut r = Refreshing::default();
        let run = r.press(0.0).unwrap();
        assert!(!r.expire(run, CAP_MS - 1_000.0));
        assert!(r.on());
        assert!(r.expire(run, CAP_MS));
        assert!(!r.on());
        // its answer, arriving later, changes nothing
        assert_eq!(r.finish(run, CAP_MS + 5_000.0), None);
        assert!(!r.on());
    }

    #[test]
    fn a_second_press_while_refreshing_starts_nothing() {
        let mut r = Refreshing::default();
        let run = r.press(0.0).unwrap();
        assert_eq!(r.press(10.0), None);
        assert_eq!(r.press(700.0), None);
        // still the same refresh, finishing on its own answer
        assert_eq!(r.finish(run, 800.0), None);
        assert!(!r.on());
        // once it stopped, a press starts a new one
        assert_eq!(r.press(900.0), Some(run + 1));
    }

    #[test]
    fn an_earlier_refreshs_answer_or_timer_never_ends_a_later_one() {
        let mut r = Refreshing::default();
        let first = r.press(0.0).unwrap();
        assert!(r.expire(first, CAP_MS));
        let second = r.press(CAP_MS + 1.0).unwrap();
        assert_ne!(first, second);
        assert_eq!(r.finish(first, CAP_MS + 2.0), None);
        assert!(!r.expire(first, CAP_MS * 3.0));
        assert!(r.on(), "the later refresh still spins");
        // a second answer of the same refresh is ignored too
        assert!(r.finish(second, CAP_MS + 100.0).is_some());
        assert_eq!(r.finish(second, CAP_MS + 200.0), None);
        assert!(r.on());
    }

    #[test]
    fn a_press_finished_by_its_page_spins_for_the_minimum_on_the_monotonic_clock() {
        let owner = Owner::new();
        owner.set();
        test_clock::ARMED.with(|a| a.borrow_mut().clear());
        test_clock::NOW.with(|n| n.set(1_000.0));
        let state = RwSignal::new(Refreshing::default());
        let done = press(state).expect("a press");
        assert!(press(state).is_none(), "a second press starts nothing");
        test_clock::NOW.with(|n| n.set(1_100.0));
        done.finished();
        let armed = test_clock::ARMED.with(|a| a.borrow().clone());
        assert_eq!(armed, vec![(1, CAP_MS), (1, MIN_MS - 100.0)]);
        expire_now(state, 1);
        assert!(state.get_untracked().on(), "not before the minimum");
        test_clock::NOW.with(|n| n.set(1_600.0));
        expire_now(state, 1);
        assert!(!state.get_untracked().on());
    }

    #[test]
    fn signing_out_stops_it_and_its_late_answer_ends_no_later_refresh() {
        let mut r = Refreshing::default();
        let first = r.press(0.0).unwrap();
        r.cancel();
        assert!(!r.on());
        let second = r
            .press(10.0)
            .expect("a press after sign-out starts a refresh");
        assert_ne!(first, second);
        assert_eq!(r.finish(first, 20.0), None);
        assert!(!r.expire(first, CAP_MS));
        assert!(r.on());
    }
}
