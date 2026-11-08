//! Keeping the view live: read /api/status on start, on a /ws "status-changed" hint while the page is on screen,
//! when it comes back on screen (the shell's `aac-visible`, or the document's own visibility) or back online, and
//! every `pollSeconds` while it is on screen. A hidden page reads nothing, so the server's collectors can idle. A
//! failed read keeps the last good view on screen, marked as old; nothing is ever shown as live after a failure.

use crate::api::{FetchError, StatusView, fetch_status};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

#[derive(Clone, Copy)]
pub struct Live {
    pub view: RwSignal<Option<StatusView>>,
    /// Date.now() of the last good read.
    pub fetched_at_ms: RwSignal<f64>,
    /// Ticks every 15 s so ages keep running between reads.
    pub now_ms: RwSignal<f64>,
    pub error: RwSignal<Option<FetchError>>,
    /// Date.now() of the last read attempt that failed (the banner says since when).
    pub failed_since_ms: RwSignal<Option<f64>>,
    pub loading: RwSignal<bool>,
}

impl Live {
    pub fn elapsed_seconds(&self) -> i64 {
        ((self.now_ms.get() - self.fetched_at_ms.get()) / 1000.0).floor() as i64
    }

    pub fn reload(&self) {
        let live = *self;
        if live.loading.get_untracked() {
            return;
        }
        live.loading.set(true);
        spawn_local(async move {
            match fetch_status().await {
                Ok(view) => {
                    let now = js_sys::Date::now();
                    live.view.set(Some(view));
                    live.fetched_at_ms.set(now);
                    live.now_ms.set(now);
                    live.error.set(None);
                    live.failed_since_ms.set(None);
                }
                Err(error) => {
                    if live.failed_since_ms.get_untracked().is_none() {
                        live.failed_since_ms.set(Some(js_sys::Date::now()));
                    }
                    live.error.set(Some(error));
                }
            }
            live.loading.set(false);
        });
    }
}

fn visible() -> bool {
    crate::on_screen()
}

pub fn start() -> Live {
    let live = Live {
        view: RwSignal::new(None),
        fetched_at_ms: RwSignal::new(js_sys::Date::now()),
        now_ms: RwSignal::new(js_sys::Date::now()),
        error: RwSignal::new(None),
        failed_since_ms: RwSignal::new(None),
        loading: RwSignal::new(false),
    };
    live.reload();

    // Ages keep running between reads.
    let _ = set_interval(
        move || live.now_ms.set(js_sys::Date::now()),
        Duration::from_secs(15),
    );

    // Safety poll while visible; a missed hint only delays.
    let poll = Rc::new(Cell::new(0u32));
    let polled = poll.clone();
    let _ = set_interval(
        move || {
            let every = live
                .view
                .get_untracked()
                .map(|v| v.refresh.poll_seconds.max(10))
                .unwrap_or(30);
            let ticks = polled.get() + 5;
            if ticks >= every {
                polled.set(0);
                if visible() {
                    live.reload();
                }
            } else {
                polled.set(ticks);
            }
        },
        Duration::from_secs(5),
    );

    // Coming back to the page reads at once.
    let on_visible = Closure::<dyn Fn()>::new(move || {
        if visible() {
            live.reload();
        }
    });
    let _ = document()
        .add_event_listener_with_callback("visibilitychange", on_visible.as_ref().unchecked_ref());
    on_visible.forget();

    // Shown again by the shell, or back online: read at once.
    for event in [crate::SHOWN_EVENT, "online"] {
        let again = Closure::<dyn Fn()>::new(move || {
            if visible() {
                live.reload();
            }
        });
        let _ = window().add_event_listener_with_callback(event, again.as_ref().unchecked_ref());
        again.forget();
    }

    connect_hints(live, 1_000);
    live
}

/// The /ws socket only carries hints ("re-read the API"); it reconnects with a growing delay, up to a minute.
fn connect_hints(live: Live, delay_ms: u32) {
    let location = window().location();
    let scheme = if location.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = location.host().unwrap_or_default();
    let Ok(socket) = web_sys::WebSocket::new(&format!("{scheme}://{host}/ws")) else {
        return;
    };

    let on_message =
        Closure::<dyn Fn(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(text) = event.data().as_string() else {
                return;
            };
            // a hidden page ignores hints (its reads would keep the server's collectors busy); returning to it reads
            if text.contains("\"status-changed\"") && visible() {
                live.reload();
            }
        });
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let on_open = Closure::<dyn Fn()>::new(move || {
        if visible() {
            live.reload();
        }
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    let on_close = Closure::<dyn Fn()>::new(move || {
        let next = (delay_ms * 2).min(60_000);
        let _ = set_timeout(
            move || connect_hints(live, next),
            Duration::from_millis(delay_ms as u64),
        );
    });
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();
}
