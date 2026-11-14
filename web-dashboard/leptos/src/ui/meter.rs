//! The usage meter of Home and Details (Slint `components/meter.slint`): a track with ticks at a quarter, half and three
//! quarters, a severity fill under a white wash, the auto-switch notch, an overage segment past 100%, and a dashed
//! track with words for a window that has no reading (never a zero bar).
//!
//! Motion (README-ARCHITECTURE.md, Motion): on its first reading a meter sweeps from 0 after its `delay` while its number
//! counts up, and its colour climbs the ramp with the moving value, so a mid-sweep "24%" is never red. A later reading
//! moves from the one before, at once. Every move uses the non-overshooting ease-out `cubic-bezier(0.2, 0.8, 0.2, 1)`:
//! the fill never shows more than the reading, not even for a frame. Nothing runs once a move has settled.

use crate::api::nullable;
use leptos::prelude::*;
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

/// One window's reading as view-model.mjs `meterView` words it (view model v2, `MeterView`). `key` empty is an
/// intentionally empty slot (a Pro account in the Fable column); `has_value` false is a real window with no reading.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeterView {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub full_label: String,
    pub has_value: bool,
    pub value: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub value_text: String,
    #[serde(deserialize_with = "nullable")]
    pub over_text: String,
    #[serde(deserialize_with = "nullable")]
    pub reset: String,
    #[serde(deserialize_with = "nullable")]
    pub reset_exact: String,
    pub reset_soon: bool,
    #[serde(deserialize_with = "nullable")]
    pub na_text: String,
    #[serde(deserialize_with = "nullable")]
    pub na_sub: String,
    #[serde(deserialize_with = "nullable")]
    pub amount: String,
    #[serde(deserialize_with = "nullable")]
    pub left: String,
    #[serde(deserialize_with = "nullable")]
    pub sampled: String,
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    #[serde(deserialize_with = "nullable")]
    pub caption: String,
    #[serde(deserialize_with = "nullable")]
    pub caption_tip: String,
    pub notch: Option<f64>,
    pub notch_faint: bool,
    pub notch_off: bool,
}

/// Severity of a used share (theme.slint `severity`): calm under 80, warn from 80, crit from 95, over past 100.
pub fn severity_of(used: f64) -> &'static str {
    if used > 100.0 {
        "over"
    } else if used >= 95.0 {
        "crit"
    } else if used >= 80.0 {
        "warn"
    } else {
        "calm"
    }
}

/// `cubic-bezier(0.2, 0.8, 0.2, 1)` at progress `x` (0..1). Its control points stay at or under 1, so it never
/// overshoots.
pub fn ease_out(x: f64) -> f64 {
    fn curve(t: f64, p1: f64, p2: f64) -> f64 {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    }
    let x = x.clamp(0.0, 1.0);
    let (mut lo, mut hi, mut t) = (0.0, 1.0, x);
    for _ in 0..30 {
        let at = curve(t, 0.2, 0.2);
        if (at - x).abs() < 1e-6 {
            break;
        }
        if at < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    curve(t, 0.8, 1.0)
}

/// The meter sweep's length (Slint `Motion.meter`).
const SWEEP_MS: f64 = 1050.0;

thread_local! {
    static MOTION: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Whether a media query matches (`window.matchMedia`), false where it can't be asked.
pub fn media_matches(query: &str) -> bool {
    let win = window();
    js_sys::Reflect::get(&win, &"matchMedia".into())
        .ok()
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
        .and_then(|f| f.call1(&win, &query.into()).ok())
        .and_then(|list| js_sys::Reflect::get(&list, &"matches".into()).ok())
        .and_then(|m| m.as_bool())
        .unwrap_or(false)
}

/// Motion is on unless the system asks for less, or a headless capture runs without `?motion` (the screenshot
/// guard the Slint dashboard had).
pub fn motion_on() -> bool {
    MOTION.with(|m| {
        if let Some(on) = m.get() {
            return on;
        }
        let reduced = media_matches("(prefers-reduced-motion: reduce)");
        let headless = window()
            .navigator()
            .user_agent()
            .map(|ua| ua.contains("HeadlessChrome"))
            .unwrap_or(false);
        let asked = window()
            .location()
            .search()
            .unwrap_or_default()
            .split(['?', '&'])
            .any(|p| p == "motion");
        let on = !reduced && (!headless || asked);
        m.set(Some(on));
        on
    })
}

/// Drives one value to its target with the ease-out, frame by frame, and stops when it lands. A plain browser frame
/// loop (not a Leptos timer), so a re-render around it never cancels a move half way (NOTES.md).
#[derive(Clone)]
struct Mover {
    shown: RwSignal<f64>,
    /// from, to, start time (ms), delay (ms)
    run: Rc<RefCell<Option<(f64, f64, f64, f64)>>>,
    frame: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>,
    looping: Rc<Cell<bool>>,
}

impl Mover {
    fn new(start: f64) -> Self {
        Mover {
            shown: RwSignal::new(start),
            run: Rc::new(RefCell::new(None)),
            frame: Rc::new(RefCell::new(None)),
            looping: Rc::new(Cell::new(false)),
        }
    }

    fn go(&self, to: f64, delay: f64) {
        let from = self.shown.get_untracked();
        if !motion_on() || (from - to).abs() < 0.005 {
            *self.run.borrow_mut() = None;
            self.shown.set(to);
            return;
        }
        *self.run.borrow_mut() = Some((from, to, js_sys::Date::now(), delay));
        if self.looping.get() {
            return;
        }
        self.looping.set(true);
        let me = self.clone();
        let tick = Closure::<dyn FnMut(f64)>::new(move |_t: f64| {
            // The meter is gone: its value went with it.
            if me.shown.try_get_untracked().is_none() {
                me.looping.set(false);
                return;
            }
            let state = *me.run.borrow();
            let Some((from, to, started, delay)) = state else {
                me.looping.set(false);
                return;
            };
            let elapsed = js_sys::Date::now() - started - delay;
            if elapsed < 0.0 {
                me.request();
                return;
            }
            let p = (elapsed / SWEEP_MS).min(1.0);
            let _ = me.shown.try_set(from + (to - from) * ease_out(p));
            if p >= 1.0 {
                *me.run.borrow_mut() = None;
                me.looping.set(false);
            } else {
                me.request();
            }
        });
        *self.frame.borrow_mut() = Some(tick);
        self.request();
    }

    fn request(&self) {
        if let Some(f) = self.frame.borrow().as_ref() {
            let _ = window().request_animation_frame(f.as_ref().unchecked_ref());
        }
    }
}

/// How a meter is laid out: in a row's cell (number and "used" over the bar, the reset under it), or inline (a card's
/// or Details' label and number on one line over the bar).
#[derive(Clone, Copy, Default, PartialEq)]
pub enum MeterMode {
    #[default]
    Cell,
    Inline,
}

/// One meter bound to a reading that may change; `owner` names whose reading it is in the hover card ("Claude ·
/// a@example.com"); `notch_at` moves the notch to a switch point being picked, before it is saved.
#[component]
pub fn UsageMeter(
    #[prop(into)] view: Signal<MeterView>,
    #[prop(optional)] mode: MeterMode,
    #[prop(optional)] delay: u32,
    #[prop(optional, into)] owner: String,
    #[prop(optional, into)] notch_at: Option<Signal<Option<f64>>>,
) -> impl IntoView {
    let target = Memo::new(move |_| {
        view.with(|v| {
            if v.has_value {
                v.value.unwrap_or(0.0).max(0.0)
            } else {
                0.0
            }
        })
    });
    let mover = Mover::new(0.0);
    let shown = mover.shown;
    {
        let mover = mover.clone();
        let first = Cell::new(true);
        Effect::new(move |_| {
            let to = target.get();
            let wait = if first.replace(false) {
                delay as f64
            } else {
                0.0
            };
            mover.go(to, wait);
        });
    }
    let settled = Memo::new(move |_| (shown.get() - target.get()).abs() < 0.005);
    let has = Memo::new(move |_| view.with(|v| v.has_value));
    let sev = Memo::new(move |_| {
        if has.get() {
            severity_of(shown.get())
        } else {
            "na"
        }
    });
    let number = move || {
        if settled.get() {
            view.with(|v| v.value_text.clone())
        } else {
            format!("{}", shown.get().round() as i64)
        }
    };
    let over = Memo::new(move |_| has.get() && target.get() > 100.0);
    let notch = Memo::new(move |_| {
        let own = view.with(|v| v.notch);
        match (own, notch_at.and_then(|n| n.get())) {
            (Some(_), Some(picked)) => Some(picked),
            (own, _) => own,
        }
    });
    let fill_style = move || format!("width:{:.2}%", shown.get().clamp(0.0, 100.0));
    let over_style = move || {
        if !over.get() {
            return "width:0px".to_string();
        }
        let target = target.get();
        let full = (6.0 + (target - 100.0) * 2.0).min(26.0);
        let p = ((shown.get() - 100.0) / (target - 100.0).max(0.01)).clamp(0.0, 1.0);
        format!("width:{:.1}px", full * p)
    };
    let notch_style = move || match notch.get() {
        Some(n) if has.get() => format!("left:{:.2}%", n.clamp(0.0, 100.0)),
        _ => "display:none".to_string(),
    };
    let faint = move || view.with(|v| v.notch_faint);
    let off = move || view.with(|v| v.notch_off);
    let bar = view! {
        <div class="um-bar" class:over=move || over.get()>
            <div class="um-track" class:na=move || !has.get()>
                <i class="um-tick" style="left:25%"></i><i class="um-tick" style="left:50%"></i><i class="um-tick" style="left:75%"></i>
                <div class="fill" style=fill_style></div>
                <span class="um-notch" class:faint=faint class:off=off style=notch_style></span>
            </div>
            <span class="um-ovr" style=over_style></span>
        </div>
    };
    let sub = move || {
        let v = view.get();
        let text = if v.has_value {
            v.reset.clone()
        } else {
            v.na_sub.clone()
        };
        (!text.is_empty() || !v.amount.is_empty()).then(|| {
            view! {
                <div class="um-sub">
                    <span class:soon=v.reset_soon>{text}</span>
                    {(!v.amount.is_empty()).then(|| view! { <span class="um-amt">{v.amount.clone()}</span> })}
                </div>
            }
        })
    };
    let card = {
        let owner = owner.clone();
        move || {
            let v = view.get();
            let used = if v.has_value {
                let amount = if v.amount.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", v.amount)
                };
                format!("{}%{amount}", v.value_text)
            } else {
                "Unavailable".to_string()
            };
            let used_note = if v.has_value {
                (!v.over_text.is_empty()).then(|| format!("{}% over the limit", v.over_text))
            } else {
                Some("No reading, not zero".to_string())
            };
            let reset = if v.reset_exact.is_empty() {
                "No reset reported".to_string()
            } else {
                v.reset_exact.clone()
            };
            let reset_note =
                (!v.reset_exact.is_empty() && !v.reset.is_empty()).then(|| v.reset.clone());
            view! {
                <div class="um-card" role="tooltip">
                    <b>{if v.full_label.is_empty() { v.label.clone() } else { v.full_label.clone() }}</b>
                    {(!owner.is_empty()).then(|| view! { <span class="um-card-sub">{owner.clone()}</span> })}
                    <dl>
                        <dt>"Used"</dt><dd>{used}{used_note.map(|n| view! { <small>{n}</small> })}</dd>
                        {(!v.left.is_empty()).then(|| view! { <dt>"Left"</dt><dd>{v.left.clone()}</dd> })}
                        <dt>"Reset"</dt><dd>{reset}{reset_note.map(|n| view! { <small>{n}</small> })}</dd>
                        <dt>"Sampled"</dt><dd>{if v.sampled.is_empty() { "Never".to_string() } else { v.sampled.clone() }}</dd>
                        <dt>"Source"</dt><dd>{if v.source.is_empty() { "Unknown".to_string() } else { v.source.clone() }}</dd>
                    </dl>
                </div>
            }
        }
    };
    let caption = move || {
        view.with(|v| {
            (v.has_value && !v.caption.is_empty()).then(|| {
                view! { <span class="cap um-cap" class:example=v.caption == "example" title=v.caption_tip.clone()>{v.caption.clone()}</span> }
            })
        })
    };
    let top = move || {
        let v = view.get();
        match (mode, v.has_value) {
            (MeterMode::Cell, true) => view! {
                <div class="um-top">
                    <span class="um-num tnum">{number}<small>"%"</small></span>
                    <span class="um-used">"used"</span>
                    {caption}
                    {(!v.over_text.is_empty()).then(|| view! { <span class="um-overtext tnum">{format!("{}% over", v.over_text)}</span> })}
                </div>
            }
            .into_any(),
            (MeterMode::Cell, false) => view! {
                <div class="um-top"><span class="um-na">{if v.na_text.is_empty() { "Unavailable".to_string() } else { v.na_text.clone() }}</span></div>
            }
            .into_any(),
            (MeterMode::Inline, has) => view! {
                <div class="um-top">
                    <span class="um-label">{v.label.clone()}</span>
                    {caption}
                    {if has {
                        view! { <span class="um-num tnum">{number}<small>"%"</small></span> }.into_any()
                    } else {
                        view! { <span class="um-na">{if v.na_text.is_empty() { "Unavailable".to_string() } else { v.na_text.clone() }}</span> }.into_any()
                    }}
                </div>
            }
            .into_any(),
        }
    };
    view! {
        <div class="um" class:inline=mode == MeterMode::Inline data-sev=move || sev.get() tabindex="-1">
            {top}
            {bar}
            {sub}
            {card}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ease_out_never_overshoots_and_lands() {
        let mut last = 0.0;
        for i in 0..=1000 {
            let y = ease_out(i as f64 / 1000.0);
            assert!(y >= last - 1e-9, "the curve only rises");
            assert!(y <= 1.0 + 1e-9, "never past its target");
            last = y;
        }
        assert!((ease_out(1.0) - 1.0).abs() < 1e-6);
        assert!(ease_out(0.0).abs() < 1e-6);
        // ease-out: well past half way at half time
        assert!(ease_out(0.5) > 0.8);
    }

    #[test]
    fn severity_follows_the_slint_ramp() {
        assert_eq!(severity_of(79.9), "calm");
        assert_eq!(severity_of(80.0), "warn");
        assert_eq!(severity_of(95.0), "crit");
        assert_eq!(severity_of(100.0), "crit");
        assert_eq!(severity_of(100.5), "over");
    }

    #[test]
    fn a_missing_reading_stays_missing() {
        let v: MeterView = serde_json::from_str(
            r#"{"key":"a|five_hour","hasValue":false,"value":null,"naText":"Not reported yet","notch":null,"reset":null}"#,
        )
        .unwrap();
        assert!(!v.has_value);
        assert_eq!(v.value, None);
        assert_eq!(v.na_text, "Not reported yet");
        assert_eq!(v.reset, "");
    }
}
