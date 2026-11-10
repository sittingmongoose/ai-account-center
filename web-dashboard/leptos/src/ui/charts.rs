//! Pieces the chart pages share (Analytics first): swatches and legend entries, the small switch, a box that reports
//! its size, the shape morph, the hover tip and the anchored popover. Drawing only: every value and label comes from
//! the page's view.

use super::icons::Icon;
use leptos::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;

// ---------------------------------------------------------------- colours

/// The CSS colour of a chart key: token types, cost and the provider hues (`--h-*`), never on marks.
pub fn key_color(key: &str) -> String {
    match key {
        "in" => "var(--chart-input)".into(),
        "out" => "var(--chart-output)".into(),
        "cw" => "var(--chart-cache-write)".into(),
        "cr" => "var(--chart-cache-read)".into(),
        "tok" => "var(--chart-tokens)".into(),
        "cost" => "var(--chart-cost)".into(),
        other => provider_hue(other),
    }
}

/// A provider's chart hue, or the quiet ink for one without a hue (Slint `Theme.provider-hue`).
pub fn provider_hue(provider: &str) -> String {
    match provider {
        "claude" | "codex" | "antigravity" | "cursor" | "muse" | "kimi-code" | "qwen" | "zai"
        | "opencode-go" => format!("var(--h-{provider})"),
        _ => "var(--ink-3)".into(),
    }
}

/// A focus-chart series: the provider hue, then lighter steps; Codex has its own second and third tones.
pub fn series_color(provider: &str, shade: i32) -> String {
    if shade <= 0 {
        return provider_hue(provider);
    }
    if provider == "codex" {
        return if shade == 1 {
            "var(--h-codex-2)".into()
        } else {
            "var(--h-codex-3)".into()
        };
    }
    let part = if shade == 1 { 62 } else { 38 };
    format!(
        "color-mix(in srgb, {} {part}%, var(--paper))",
        provider_hue(provider)
    )
}

/// A model's colour in its provider's family (the donut): `mix` of the hue into paper; "other" models are quiet.
pub fn model_color(provider: &str, mix: f64, other: bool) -> String {
    if other {
        return "var(--ink-4)".into();
    }
    let hue = if provider.is_empty() {
        "var(--h-model)".to_string()
    } else {
        provider_hue(provider)
    };
    if mix >= 0.999 {
        hue
    } else {
        let part = (mix.clamp(0.0, 1.0) * 100.0).round();
        format!("color-mix(in srgb, {hue} {part}%, var(--paper))")
    }
}

/// The theme colour a focus-chart key names (`AxColor.key`).
pub fn theme_key(key: &str, provider: &str, shade: i32) -> String {
    match key {
        "grid" => "var(--grid)".into(),
        "grid-strong" => "var(--grid-strong)".into(),
        "rule-strong" => "var(--rule-strong)".into(),
        "warn" => "var(--warn)".into(),
        "crit" => "var(--crit)".into(),
        "accent" => "var(--accent)".into(),
        "ink" => "var(--ink)".into(),
        "ink-2" => "var(--ink-2)".into(),
        "ink-3" => "var(--ink-3)".into(),
        "paper" => "var(--paper)".into(),
        "paper-2" => "var(--paper-2)".into(),
        "crit-text" => "var(--crit-text)".into(),
        "accent-text" => "var(--accent-text)".into(),
        "series" => series_color(provider, shade),
        "series-ink" => format!(
            "color-mix(in srgb, {} 62%, var(--ink))",
            series_color(provider, shade)
        ),
        _ => "var(--ink-3)".into(),
    }
}

/// The severity line colour of a sparkline run (0 calm, 1 warn, 2 crit, 3 over).
pub fn sev_color(sev: i32) -> &'static str {
    match sev {
        3 => "var(--over)",
        2 => "var(--crit)",
        1 => "var(--warn)",
        _ => "var(--calm)",
    }
}

// ---------------------------------------------------------------- swatches and legends

/// A legend swatch: a box, a short line (cost), or two halves (cache write and read).
#[component]
pub fn Swatch(
    #[prop(into)] color: String,
    #[prop(optional)] shape: &'static str,
    #[prop(optional, into)] color2: Option<String>,
) -> impl IntoView {
    match shape {
        "line" => view! { <span class="sw sw-line" style=format!("background:{color}")></span> }
            .into_any(),
        "half" => {
            let second = color2.unwrap_or_else(|| color.clone());
            view! {
                <span class="sw sw-half"><i style=format!("background:{color}")></i><i style=format!("background:{second}")></i></span>
            }
            .into_any()
        }
        _ => view! { <span class="sw" style=format!("background:{color}")></span> }.into_any(),
    }
}

/// A legend entry: swatch, label and an optional quiet note ("left axis").
#[component]
pub fn LegendEntry(
    #[prop(into)] color: String,
    #[prop(into)] label: String,
    #[prop(optional)] shape: &'static str,
    #[prop(optional, into)] note: String,
) -> impl IntoView {
    view! {
        <span class="lgd">
            <Swatch color=color shape=shape/>
            <span class="lgd-l">{label}</span>
            {(!note.is_empty()).then(|| view! { <span class="lgd-n">{note}</span> })}
        </span>
    }
}

/// A small labelled switch (Slint `AxSwitch`): it shows the saved value and asks for the other one.
#[component]
pub fn Switch(
    #[prop(into)] on: Signal<bool>,
    #[prop(into)] label: String,
    #[prop(optional, into)] tip: String,
    on_toggle: Callback<bool>,
) -> impl IntoView {
    view! {
        <button class="switch" role="switch" aria-checked=move || on.get().to_string() data-tip=(!tip.is_empty()).then_some(tip)
            on:click=move |_| on_toggle.run(!on.get_untracked())>
            <span class="sw-track" class:on=move || on.get()><span class="sw-knob"></span></span>
            <span>{label}</span>
        </button>
    }
}

// ---------------------------------------------------------------- sizes

/// Calls `report(width, height)` with the element's content-box size now and whenever it changes. The observer goes
/// with the current owner.
pub fn observe_size(el: &web_sys::Element, report: impl Fn(f64, f64) + 'static) {
    let callback = Closure::<dyn Fn(js_sys::Array)>::new(move |entries: js_sys::Array| {
        if let Some(entry) = entries
            .get(0)
            .dyn_ref::<web_sys::ResizeObserverEntry>()
            .cloned()
        {
            let rect = entry.content_rect();
            report(rect.width(), rect.height());
        }
    });
    let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
        return;
    };
    observer.observe(el);
    // The observer and its callback live until the owner is cleaned up.
    let kept = StoredValue::new_local(Some((observer, callback)));
    on_cleanup(move || {
        if let Some(Some((observer, _callback))) = kept.try_update_value(|v| v.take()) {
            observer.disconnect();
        }
    });
}

// ---------------------------------------------------------------- the shape morph

/// Splits a path into its numbers and the text between them. Two paths morph only when the text between numbers
/// is the same (the same commands in the same order).
fn tokens(path: &str) -> (Vec<&str>, Vec<f64>) {
    let mut glue = Vec::new();
    let mut nums = Vec::new();
    let bytes = path.as_bytes();
    let (mut i, mut start) = (0, 0);
    while i < bytes.len() {
        let c = bytes[i];
        let begins = c.is_ascii_digit()
            || c == b'.'
            || (c == b'-'
                && i + 1 < bytes.len()
                && (bytes[i + 1].is_ascii_digit() || bytes[i + 1] == b'.'));
        if begins {
            glue.push(&path[start..i]);
            let from = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b'.' || bytes[i] == b'e')
            {
                i += 1;
            }
            nums.push(path[from..i].parse().unwrap_or(0.0));
            start = i;
        } else {
            i += 1;
        }
    }
    glue.push(&path[start..]);
    (glue, nums)
}

/// A morph between two paths of the same structure, or None (draw the new one at once).
pub struct PathMorph {
    glue: Vec<String>,
    from: Vec<f64>,
    to: Vec<f64>,
}

impl PathMorph {
    pub fn new(from: &str, to: &str) -> Option<PathMorph> {
        let (g0, n0) = tokens(from);
        let (g1, n1) = tokens(to);
        if g0 != g1 || n0.len() != n1.len() || n0.is_empty() {
            return None;
        }
        Some(PathMorph {
            glue: g1.into_iter().map(String::from).collect(),
            from: n0,
            to: n1,
        })
    }

    /// The path at `k` (0 = from, 1 = to), one decimal like bridge.js writes them.
    pub fn at(&self, k: f64) -> String {
        let mut out = String::with_capacity(
            self.glue.iter().map(|g| g.len()).sum::<usize>() + self.to.len() * 6,
        );
        for (i, glue) in self.glue.iter().enumerate() {
            out.push_str(glue);
            if i < self.to.len() {
                let v = self.from[i] + (self.to[i] - self.from[i]) * k;
                let r = (v * 10.0).round() / 10.0;
                if r == r.trunc() {
                    out.push_str(&format!("{}", r as i64));
                } else {
                    out.push_str(&format!("{r:.1}"));
                }
            }
        }
        out
    }
}

/// bridge.js's easeInOut (cubic), so the glide matches the Slint page's.
pub fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Whether the person asked for less motion (or the page is under test without motion).
pub fn motion_reduced() -> bool {
    window()
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

/// Runs `frame(k)` on animation frames for `ms`, with k eased 0..1, until done or `alive()` turns false. Uses the
/// window's own frames (not an owner's), so a redraw of the surrounding view never cancels it half way.
pub fn animate(ms: f64, alive: impl Fn() -> bool + 'static, frame: impl Fn(f64) + 'static) {
    let start = Rc::new(RefCell::new(None::<f64>));
    let step: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>> = Rc::new(RefCell::new(None));
    let again = step.clone();
    *step.borrow_mut() = Some(Closure::<dyn FnMut(f64)>::new(move |now: f64| {
        if !alive() {
            again.borrow_mut().take();
            return;
        }
        let t0 = *start.borrow_mut().get_or_insert(now);
        let t = ((now - t0) / ms).clamp(0.0, 1.0);
        frame(ease_in_out(t));
        if t < 1.0 {
            if let Some(cb) = again.borrow().as_ref() {
                let _ = window().request_animation_frame(cb.as_ref().unchecked_ref());
            }
        } else {
            again.borrow_mut().take();
        }
    }));
    if let Some(cb) = step.borrow().as_ref() {
        let _ = window().request_animation_frame(cb.as_ref().unchecked_ref());
    }
}

// ---------------------------------------------------------------- tips

/// The hover tip: one floating card for every element with `data-tip` inside `children`. A mouse shows it on hover;
/// a touch shows it on a tap that does nothing else (a plain value), until the next tap.
#[component]
pub fn TipLayer(children: Children) -> impl IntoView {
    let text = RwSignal::new(String::new());
    let pos = RwSignal::new((0.0f64, 0.0f64, false));
    let find = |target: Option<web_sys::EventTarget>| -> Option<(String, web_sys::DomRect, bool)> {
        let el = target?.dyn_into::<web_sys::Element>().ok()?;
        let tipped = el.closest("[data-tip]").ok().flatten()?;
        let tip = tipped.get_attribute("data-tip").unwrap_or_default();
        if tip.is_empty() {
            return None;
        }
        let acts = el.closest("button, a, [data-act]").ok().flatten().is_some();
        Some((tip, tipped.get_bounding_client_rect(), acts))
    };
    let place = move |tip: String, rect: web_sys::DomRect| {
        let vw = window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1024.0);
        let below = rect.top() < 120.0;
        let x = (rect.left() + rect.width() / 2.0).clamp(12.0, vw - 12.0);
        let y = if below {
            rect.bottom() + 8.0
        } else {
            rect.top() - 8.0
        };
        text.set(tip);
        pos.set((x, y, below));
    };
    let over = move |e: web_sys::PointerEvent| {
        if e.pointer_type() != "mouse" {
            return;
        }
        match find(e.target()) {
            Some((tip, rect, _)) => place(tip, rect),
            None => text.set(String::new()),
        }
    };
    let up = move |e: web_sys::PointerEvent| {
        if e.pointer_type() == "mouse" {
            return;
        }
        match find(e.target()) {
            Some((tip, rect, false)) if text.get_untracked() != tip => place(tip, rect),
            _ => text.set(String::new()),
        }
    };
    let hide = window_event_listener(leptos::ev::scroll, move |_| text.set(String::new()));
    on_cleanup(move || hide.remove());
    view! {
        <div class="tip-scope" on:pointerover=over on:pointerup=up on:pointerleave=move |_| text.set(String::new())>
            {children()}
            <div class="tip-card" class:below=move || pos.get().2 hidden=move || text.with(|t| t.is_empty())
                style=move || { let (x, y, _) = pos.get(); format!("left:{x:.0}px;top:{y:.0}px") }>{move || text.get()}</div>
        </div>
    }
}

// ---------------------------------------------------------------- popovers

/// What a popover hangs from: its own button, or the element last tapped (a chart row, a donut arc).
#[derive(Clone, Copy)]
pub enum Anchor {
    Button(NodeRef<leptos::html::Button>),
    Element(StoredValue<Option<web_sys::Element>, LocalStorage>),
}

impl Anchor {
    /// An anchor set by the tap that opens the popover.
    pub fn tapped() -> Anchor {
        Anchor::Element(StoredValue::new_local(None))
    }

    /// Hangs the popover from `el` (for `Anchor::tapped`).
    pub fn set(&self, el: web_sys::Element) {
        if let Anchor::Element(slot) = self {
            slot.set_value(Some(el));
        }
    }

    fn element(&self) -> Option<web_sys::Element> {
        match self {
            Anchor::Button(r) => r.get_untracked().map(Into::into),
            Anchor::Element(slot) => slot.try_with_value(|e| e.clone()).flatten(),
        }
    }
}

/// A panel opened from a button or a tapped element: under it on wider screens (kept inside the window), a sheet
/// from the bottom on phones. Closes on Escape, a tap outside, or (anchored) a scroll of the page.
#[component]
pub fn Popover(
    open: RwSignal<bool>,
    #[prop(into)] anchor: Anchor,
    #[prop(optional)] width: Option<u32>,
    #[prop(optional, into)] label: String,
    children: ChildrenFn,
) -> impl IntoView {
    let width = width.unwrap_or(320) as f64;
    let style = RwSignal::new(String::new());
    let panel = NodeRef::<leptos::html::Div>::new();
    let viewport = || {
        window()
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(1024.0)
    };
    // Under the anchor when it fits, else above it, never past the window; a sheet (no style) on phones. False once
    // the anchor has left the window.
    let place = move || {
        let vw = viewport();
        let Some(el) = anchor.element().filter(|_| vw >= 600.0) else {
            style.set(String::new());
            return true;
        };
        let rect = el.get_bounding_client_rect();
        let vh = window()
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(800.0);
        if rect.bottom() < 0.0 || rect.top() > vh {
            return false;
        }
        let w = width.min(vw - 16.0);
        let left = (rect.right() - w)
            .max(8.0)
            .min(rect.left().max(8.0))
            .min(vw - w - 8.0);
        let top = rect.bottom() + 8.0;
        let at = if top + 240.0 <= vh || rect.top() < vh / 2.0 {
            format!(
                "top:{top:.0}px;max-height:{:.0}px",
                (vh - top - 8.0).max(160.0)
            )
        } else {
            format!(
                "bottom:{:.0}px;max-height:{:.0}px",
                vh - rect.top() + 8.0,
                (rect.top() - 16.0).max(160.0)
            )
        };
        style.set(format!(
            "position:fixed;left:{left:.0}px;width:{w:.0}px;{at}"
        ));
        true
    };
    Effect::new(move |_| {
        if open.get() {
            place();
        }
    });
    let keys = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            open.set(false);
        }
    });
    let outside = window_event_listener(leptos::ev::pointerdown, move |e| {
        if !open.get_untracked() {
            return;
        }
        let Some(target) = e.target().and_then(|t| t.dyn_into::<web_sys::Node>().ok()) else {
            return;
        };
        let inside = panel
            .get_untracked()
            .is_some_and(|p| p.contains(Some(&target)))
            || anchor.element().is_some_and(|a| a.contains(Some(&target)));
        if !inside {
            open.set(false);
        }
    });
    // An anchored panel follows its anchor while the page scrolls (a click that first scrolled its row into view
    // lands its scroll event after the panel opened), and closes once the anchor has left the window.
    let scrolled = window_event_listener(leptos::ev::scroll, move |_| {
        if open.get_untracked() && !place() {
            open.set(false);
        }
    });
    on_cleanup(move || {
        keys.remove();
        outside.remove();
        scrolled.remove();
    });
    view! {
        <Show when=move || open.get()>
            <div class="pop-scrim" on:click=move |_| open.set(false)></div>
            <div class="pop" node_ref=panel role="dialog" aria-label=label.clone() style=move || style.get()>
                <div class="pop-grab" aria-hidden="true"></div>
                {children()}
            </div>
        </Show>
    }
}

impl From<NodeRef<leptos::html::Button>> for Anchor {
    fn from(r: NodeRef<leptos::html::Button>) -> Self {
        Anchor::Button(r)
    }
}

/// The quiet button that opens a popover: label, chevron turning with it.
#[component]
pub fn PopButton(
    open: RwSignal<bool>,
    node_ref: NodeRef<leptos::html::Button>,
    #[prop(optional)] class: &'static str,
    #[prop(optional, into)] tip: String,
    children: Children,
) -> impl IntoView {
    view! {
        <button class=format!("pop-btn {class}") node_ref=node_ref aria-expanded=move || open.get().to_string()
            data-tip=(!tip.is_empty()).then_some(tip) class:open=move || open.get()
            on:click=move |_| open.update(|o| *o = !*o)>
            {children()}
            <Icon name="chev-down" class="chev"/>
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_the_same_shape_morph_number_by_number() {
        let m = PathMorph::new("M0 10L5 20L10 0", "M0 20L5 0L10 10").expect("same commands");
        assert_eq!(m.at(0.0), "M0 10L5 20L10 0");
        assert_eq!(m.at(1.0), "M0 20L5 0L10 10");
        assert_eq!(m.at(0.5), "M0 15L5 10L10 5");
        // negative and fractional numbers keep their place
        let m = PathMorph::new("M-1.5 2V3", "M1.5 4V5").unwrap();
        assert_eq!(m.at(0.5), "M0 3V4");
        // another shape (more points, other commands) does not morph
        assert!(PathMorph::new("M0 0L1 1", "M0 0L1 1L2 2").is_none());
        assert!(PathMorph::new("M0 0L1 1", "M0 0H1").is_none());
        assert!(PathMorph::new("", "").is_none());
    }

    #[test]
    fn the_glide_eases_without_overshoot() {
        assert_eq!(ease_in_out(0.0), 0.0);
        assert_eq!(ease_in_out(1.0), 1.0);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-9);
        let mut last = 0.0;
        for i in 0..=100 {
            let v = ease_in_out(i as f64 / 100.0);
            assert!(v >= last && v <= 1.0);
            last = v;
        }
    }

    #[test]
    fn colours_name_theme_tokens() {
        assert_eq!(key_color("cr"), "var(--chart-cache-read)");
        assert_eq!(key_color("claude"), "var(--h-claude)");
        assert_eq!(key_color("nobody"), "var(--ink-3)");
        assert_eq!(series_color("codex", 2), "var(--h-codex-3)");
        assert_eq!(
            series_color("zai", 1),
            "color-mix(in srgb, var(--h-zai) 62%, var(--paper))"
        );
        assert_eq!(model_color("", 1.0, false), "var(--h-model)");
        assert_eq!(
            model_color("claude", 0.5, false),
            "color-mix(in srgb, var(--h-claude) 50%, var(--paper))"
        );
        assert_eq!(model_color("claude", 0.5, true), "var(--ink-4)");
        assert_eq!(theme_key("grid", "", 0), "var(--grid)");
    }
}
