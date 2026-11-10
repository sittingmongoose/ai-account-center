//! When you use the CLIs (a weekday x hour heatmap of cost or tokens, local time) and Daily (or Hourly) cost by
//! provider. A mouse reads a cell or bar on hover; a touch pins it in the readout row.

use super::{Ax, CardHead};
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// The heatmap ramp's sample points for the legend (the Slint page's).
const RAMP: [f64; 6] = [0.055, 0.447, 0.632, 0.775, 0.894, 1.0];

fn coarse() -> bool {
    window()
        .match_media("(pointer: coarse)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

#[component]
pub fn HeatCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let heat = ax.part(|v| v.usage.heat.clone());
    let mode = ax.part(|v| {
        if v.state.heat == "tokens" {
            "tokens".to_string()
        } else {
            "cost".to_string()
        }
    });
    let reset = ax.part(|v| {
        (
            v.state.range.clone(),
            v.state.prov.clone(),
            v.state.heat.clone(),
        )
    });
    let sel = RwSignal::new(-1i32);
    let pinned = RwSignal::new(false);
    Effect::new(move |_| {
        reset.track();
        sel.set(-1);
        pinned.set(false);
    });
    let tip = Memo::new(move |_| {
        let i = sel.get();
        heat.with(|h| {
            (i >= 0)
                .then(|| h.cells.get(i as usize).map(|c| c.tip.clone()))
                .flatten()
        })
    });
    let cell_at = |e: &web_sys::PointerEvent| -> Option<i32> {
        let el = e.target()?.dyn_into::<web_sys::Element>().ok()?;
        el.closest("[data-cell]")
            .ok()
            .flatten()?
            .get_attribute("data-cell")?
            .parse()
            .ok()
    };
    view! {
        <section class="plate ax-card heat-card" style="--i:8">
            <CardHead title="When you use the CLIs" sub=Signal::derive(move || heat.with(|h| h.sub.clone()))>
                <Segmented options=vec![("cost".into(), "Cost".into()), ("tokens".into(), "Tokens".into())] selected=mode label="Heatmap by"
                    on_select=Callback::new(move |v: String| ax.act("analytics-heat", &v))/>
            </CardHead>
            <div class="ro-bar heat-ro" data-probe="heat-ro">
                {move || match tip.get() {
                    Some(t) => view! {
                        <span class="ro-l1">{t}</span>
                        <button class="icon-btn bare ro-close" aria-label="Close the reading" hidden=move || !pinned.get()
                            on:click=move |_| { sel.set(-1); pinned.set(false); }><Icon name="close"/></button>
                    }.into_any(),
                    None => view! { <span class="q3">{if coarse() { "Tap the chart for a reading" } else { "Point at the chart for a reading" }}</span> }.into_any(),
                }}
            </div>
            <div class="ax-body">
                <div class="heat-grid"
                    on:pointermove=move |e: web_sys::PointerEvent| {
                        let i = cell_at(&e);
                        if e.pointer_type() == "mouse" { if !pinned.get_untracked() { sel.set(i.unwrap_or(-1)) } }
                        else if e.buttons() != 0 { if let Some(i) = i { sel.set(i); pinned.set(true); } }
                    }
                    on:pointerleave=move |e: web_sys::PointerEvent| if e.pointer_type() == "mouse" && !pinned.get_untracked() { sel.set(-1) }
                    on:pointerup=move |e: web_sys::PointerEvent| {
                        if e.pointer_type() == "mouse" { return; }
                        match cell_at(&e) {
                            Some(i) if !(pinned.get_untracked() && sel.get_untracked() == i) => { sel.set(i); pinned.set(true); }
                            _ => { sel.set(-1); pinned.set(false); }
                        }
                    }>
                    <span></span>
                    {move || heat.with(|h| h.hours.clone()).into_iter().enumerate().map(|(i, label)| view! {
                        <span class="hh cap" class:six=i % 6 == 0>{label}</span>
                    }).collect_view()}
                    {move || {
                        let h = heat.get();
                        let days = if h.days.len() == 7 { h.days.clone() } else { ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].map(String::from).to_vec() };
                        days.into_iter().enumerate().map(|(d, name)| {
                            let row = h.cells.iter().skip(d * 24).take(24).cloned().enumerate().map(|(hr, c)| {
                                let n = (d * 24 + hr) as i32;
                                let style = (c.state == 2).then(|| format!("--k:{:.1}%", (c.k.clamp(0.0, 1.0) * 100.0)));
                                view! {
                                    <span class="hc" data-cell=n data-state=c.state style=style data-tip=c.tip.clone()
                                        class:sel=move || sel.get() == n></span>
                                }
                            }).collect_view();
                            view! { <span class="hd">{name}</span>{row} }
                        }).collect_view()
                    }}
                </div>
                <div class="heat-legend">
                    <span class="q3">"Less"</span>
                    {RAMP.iter().map(|k| view! { <span class="hc key" data-state="2" style=format!("--k:{:.1}%", k * 100.0)></span> }).collect_view()}
                    <span class="q3">"More · busiest hour"</span>
                    <b class="tnum q2">{move || heat.with(|h| h.busiest.clone())}</b>
                    <span class="hl-gap"></span>
                    <span class="hc key" data-state="1"></span><span class="q3">"Nothing logged"</span>
                    <span class="hc key" data-state="0"></span><span class="q3">"Outside the logs"</span>
                </div>
            </div>
        </section>
    }
}

#[component]
pub fn DailyCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let daily = ax.part(|v| v.usage.daily.clone());
    let reset = ax.part(|v| (v.state.range.clone(), v.state.prov.clone()));
    let sel = RwSignal::new(-1i32);
    let pinned = RwSignal::new(false);
    let size = RwSignal::new((0.0f64, 0.0f64));
    let boxed = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = boxed.get() {
            observe_size(&el, move |w, h| size.set((w.round(), h.round())));
        }
    });
    // The bars' labels are thinned for the reported width (bridge.js `dailyView`): report this box.
    let reported = StoredValue::new((0.0f64, 0.0f64));
    Effect::new(move |_| {
        let (w, h) = size.get();
        daily.track();
        if w > 0.0
            && h > 0.0
            && reported.get_value() != (w, h)
            && ax.dash.act("analytics-layout", &format!("daily,{w},{h}"))
        {
            reported.set_value((w, h));
        }
    });
    Effect::new(move |_| {
        reset.track();
        sel.set(-1);
        pinned.set(false);
    });
    let bar = Memo::new(move |_| {
        let i = sel.get();
        daily.with(|d| (i >= 0).then(|| d.bars.get(i as usize).cloned()).flatten())
    });
    let other = Memo::new(move |_| {
        daily.with(|d| {
            if d.other_hue.is_empty() {
                "var(--h-model)".to_string()
            } else {
                provider_hue(&d.other_hue)
            }
        })
    });
    let row_color = move |p: &str| match p {
        "claude" | "codex" => key_color(p),
        _ => other.get_untracked(),
    };
    let bar_at = |e: &web_sys::PointerEvent| -> Option<i32> {
        let el = e.target()?.dyn_into::<web_sys::Element>().ok()?;
        el.closest("[data-bar]")
            .ok()
            .flatten()?
            .get_attribute("data-bar")?
            .parse()
            .ok()
    };
    view! {
        <section class="plate ax-card daily-card" style="--i:9">
            <CardHead title=Signal::derive(move || daily.with(|d| d.title.clone())) sub=Signal::derive(move || daily.with(|d| d.sub.clone()))>
                <span class="lgd-row tight">
                    {move || daily.with(|d| d.show_claude).then(|| view! { <LegendEntry color=key_color("claude") label=daily.with_untracked(|d| d.claude_label.clone())/> })}
                    {move || daily.with(|d| d.show_codex).then(|| view! { <LegendEntry color=key_color("codex") label=daily.with_untracked(|d| d.codex_label.clone())/> })}
                    {move || daily.with(|d| d.show_other).then(|| view! { <LegendEntry color=other.get() label=daily.with_untracked(|d| d.other_label.clone())/> })}
                </span>
            </CardHead>
            <div class="ro-bar" data-probe="daily-ro">
                {move || match bar.get() {
                    Some(b) => view! {
                        <span class="ro-l1"><span class="ro-time">{b.time.clone()}</span>{(!b.total.is_empty()).then(|| view! { <b class="tnum">{b.total.clone()}</b> })}</span>
                        <span class="ro-l2 tnum">{b.rows.iter().map(|r| view! { <span class="ro-v"><Swatch color=row_color(&r.p)/>{format!("{} {}", r.label, r.value)}</span> }).collect_view()}</span>
                        <button class="icon-btn bare ro-close" aria-label="Close the reading" hidden=move || !pinned.get()
                            on:click=move |_| { sel.set(-1); pinned.set(false); }><Icon name="close"/></button>
                    }.into_any(),
                    None => view! { <span class="q3">{if coarse() { "Tap the chart for a reading" } else { "Point at the chart for a reading" }}</span> }.into_any(),
                }}
            </div>
            <div class="daily-box" node_ref=boxed>
                <div class="daily-plot">
                    {move || daily.with(|d| d.y_ticks.clone()).into_iter().map(|t| view! {
                        <span class="dy" class:base=t.base style=format!("bottom:{:.2}%", t.y * 100.0)><span class="ax-t">{t.label}</span></span>
                    }).collect_view()}
                    <div class="daily-bars"
                        on:pointermove=move |e: web_sys::PointerEvent| {
                            let i = bar_at(&e);
                            if e.pointer_type() == "mouse" { if !pinned.get_untracked() { sel.set(i.unwrap_or(-1)) } }
                            else if e.buttons() != 0 { if let Some(i) = i { sel.set(i); pinned.set(true); } }
                        }
                        on:pointerleave=move |e: web_sys::PointerEvent| if e.pointer_type() == "mouse" && !pinned.get_untracked() { sel.set(-1) }
                        on:pointerup=move |e: web_sys::PointerEvent| {
                            if e.pointer_type() == "mouse" { return; }
                            match bar_at(&e) {
                                Some(i) if !(pinned.get_untracked() && sel.get_untracked() == i) => { sel.set(i); pinned.set(true); }
                                _ => { sel.set(-1); pinned.set(false); }
                            }
                        }>
                        {move || daily.get().bars.into_iter().enumerate().map(|(i, b)| {
                            let n = i as i32;
                            let ot = (1.0 - b.cl - b.cx).max(0.0);
                            view! {
                                <div data-bar=n class=move || { let s = sel.get(); if s == n { "db hot" } else if s >= 0 { "db dim" } else { "db" } }>
                                    <div class="db-stack" style=format!("height:{:.2}%", (b.h.clamp(0.0, 1.0)) * 100.0)>
                                        {(b.cl > 0.0).then(|| view! { <i style=format!("flex:{:.4} 1 0;background:{}", b.cl, key_color("claude"))></i> })}
                                        {(b.cx > 0.0).then(|| view! { <i style=format!("flex:{:.4} 1 0;background:{}", b.cx, key_color("codex"))></i> })}
                                        {(ot > 0.0005).then(|| view! { <i style=move || format!("flex:{ot:.4} 1 0;background:{}", other.get())></i> })}
                                    </div>
                                    <span class="db-l ax-t">{b.label}</span>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                </div>
                <Show when=move || daily.with(|d| d.empty)>
                    <p class="chart-empty">{move || daily.with(|d| d.empty_text.clone())}</p>
                </Show>
                {move || bar.get().filter(|_| !coarse()).map(|b| view! {
                    <div class="ro-card daily-tip" style=move || {
                        let n = daily.with(|d| d.bars.len().max(1)) as f64;
                        let i = sel.get().max(0) as f64;
                        let x = (i + 0.5) / n;
                        if x < 0.6 { format!("left:calc(58px + (100% - 72px) * {x:.4} + 14px)") } else { format!("right:calc(14px + (100% - 72px) * {:.4} + 14px)", 1.0 - x) }
                    }>
                        <div class="ro-time">{b.time.clone()}</div>
                        {b.rows.iter().map(|r| view! {
                            <div class="ro-row"><Swatch color=row_color(&r.p)/><span>{r.label.clone()}</span><b class="tnum">{r.value.clone()}</b></div>
                        }).collect_view()}
                        {(!b.total.is_empty()).then(|| view! { <hr/><div class="ro-row strong"><span></span><span>"Total"</span><b class="tnum">{b.total.clone()}</b></div> })}
                        {(!b.foot.is_empty()).then(|| view! { <p class="ro-foot">{b.foot.clone()}</p> })}
                    </div>
                })}
            </div>
        </section>
    }
}
