//! Usage trends: tokens on the left axis and estimated cost on the right, as one area or stacked by token type. The
//! paths come laid out in plot pixels for the box this card reported; on a range, filter or toggle change the view
//! says "morph" and the shape glides number by number from what is on screen (520 ms, bridge.js's easing), as the
//! Slint page's did. The crosshair follows the pointer (desktop) or a tap pins a reading (touch, readout bar).

use super::Ax;
use super::view::*;
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;
use std::sync::atomic::{AtomicU32, Ordering};
use wasm_bindgen::JsCast;

/// The plot's pads inside the chart box (bridge.js `TREND_PAD`).
const L: f64 = 62.0;
const R: f64 = 70.0;
const T: f64 = 30.0;
const B: f64 = 34.0;

const TYPES: [&str; 4] = ["in", "out", "cw", "cr"];

#[component]
pub fn TrendCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let trend = ax.part(|v| v.usage.trend.clone());
    let split = ax.part(|v| v.state.split);
    let cache = ax.part(|v| v.state.cache);
    let range = ax.part(|v| (v.state.range.clone(), v.state.prov.clone()));
    let size = RwSignal::new((0.0f64, 0.0f64));
    let boxed = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = boxed.get() {
            observe_size(&el, move |w, h| size.set((w.round(), h.round())));
        }
    });
    // The view is laid out for the box bridge.js last heard of: report this box whenever they differ.
    Effect::new(move |_| {
        let (w, h) = size.get();
        let (tw, th) = trend.with(|t| (t.w, t.h));
        if w > 0.0 && h > 0.0 && ((w - tw).abs() >= 1.0 || (h - th).abs() >= 1.0) {
            ax.act("analytics-layout", &format!("trend,{w},{h}"));
        }
    });
    let trace = RwSignal::new(String::new());
    let shown = shown_paths(trend, ax.dash.analytics_motion, trace);
    let e2e = window()
        .location()
        .search()
        .unwrap_or_default()
        .split(['?', '&'])
        .any(|p| p == "e2e");
    let cur = RwSignal::new(-1i32);
    let pinned = RwSignal::new(false);
    // a range or provider change clears the pinned reading
    Effect::new(move |_| {
        range.track();
        cur.set(-1);
        pinned.set(false);
    });
    Effect::new(move |_| {
        let n = trend.with(|t| t.buckets.len()) as i32;
        if cur.get_untracked() >= n {
            cur.set(-1);
            pinned.set(false);
        }
    });
    let first = StoredValue::new(true);
    let bucket = Memo::new(move |_| {
        let i = cur.get();
        trend.with(|t| {
            (i >= 0)
                .then(|| t.buckets.get(i as usize).cloned())
                .flatten()
        })
    });
    view! {
        <section class="plate ax-card trend-card" style="--i:2">
            <super::CardHead title="Usage trends" sub=Signal::derive(move || trend.with(|t| t.sub.clone()))>
                <Switch on=split label="Token types" tip="Stack the tokens by type"
                    on_toggle=Callback::new(move |v: bool| ax.act("analytics-split", if v { "true" } else { "false" }))/>
                <Switch on=cache label="Cache reads" tip="Cache reads dwarf the other types, so they start hidden"
                    on_toggle=Callback::new(move |v: bool| ax.act("analytics-cache", if v { "true" } else { "false" }))/>
            </super::CardHead>
            <div class="lgd-row">
                {move || trend.with(|t| t.legend.clone()).into_iter().map(|item| view! {
                    <LegendEntry color=key_color(&item.key) shape=if item.key == "cost" { "line" } else { "" } label=item.label note=item.note/>
                }).collect_view()}
            </div>
            <ReadoutBar bucket=bucket pinned=pinned cur=cur/>
            {e2e.then(|| view! { <p class="e2e-trace tnum" data-probe="trend-frames">{move || trace.get()}</p> })}
            <div class="trend-box" node_ref=boxed>
                {move || {
                    let (bw, bh) = size.get();
                    (bw > 0.0).then(|| {
                        // the data draws on the first time only, not on each resize
                        let draw = first.get_value();
                        first.set_value(false);
                        view! { <TrendChart trend=trend shown=shown split=split cache=cache bw=bw bh=bh cur=cur pinned=pinned bucket=bucket draw=draw/> }
                    })
                }}
            </div>
        </section>
    }
}

/// The eleven paths on screen: the view's, or the frames of a glide to them.
fn shown_paths(
    trend: Memo<Trend>,
    motion: RwSignal<String>,
    trace: RwSignal<String>,
) -> RwSignal<Vec<String>> {
    let shown = RwSignal::new(Vec::<String>::new());
    let generation = StoredValue::new(0u64);
    Effect::new(move |prev: Option<(f64, f64)>| {
        let (next, size) = trend.with(|t| {
            (
                t.paths
                    .all()
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                (t.pw, t.ph),
            )
        });
        generation.update_value(|g| *g += 1);
        let from = shown.get_untracked();
        let glide = motion.get_untracked() == "morph"
            && prev == Some(size)
            && from.len() == next.len()
            && !motion_reduced();
        let morphs: Vec<Option<PathMorph>> = if glide {
            from.iter()
                .zip(&next)
                .map(|(a, b)| PathMorph::new(a, b))
                .collect()
        } else {
            Vec::new()
        };
        if morphs.iter().all(|m| m.is_none()) {
            shown.set(next);
            return size;
        }
        let mine = generation.get_value();
        // the glide's frame times, for the `?e2e` trace (the iPhone simulator check reads it off the screen)
        let stamps = StoredValue::new(Vec::<f64>::new());
        animate(
            520.0,
            move || generation.try_with_value(|g| *g) == Some(mine),
            move |k| {
                stamps.update_value(|v| v.push(js_sys::Date::now()));
                if k >= 1.0 {
                    let line = stamps.with_value(|v| frame_trace(v));
                    let _ = trace.try_set(line);
                }
                let frame = morphs
                    .iter()
                    .zip(&next)
                    .map(|(m, n)| match m {
                        Some(m) if k < 1.0 => m.at(k),
                        _ => n.clone(),
                    })
                    .collect();
                let _ = shown.try_set(frame);
            },
        );
        size
    });
    shown
}

/// "31 frames in 520 ms, slowest 18 ms, 0 over 34 ms": how smoothly a glide ran.
fn frame_trace(stamps: &[f64]) -> String {
    if stamps.len() < 2 {
        return String::new();
    }
    let gaps: Vec<f64> = stamps.windows(2).map(|w| w[1] - w[0]).collect();
    let slowest = gaps.iter().copied().fold(0.0, f64::max);
    let over = gaps.iter().filter(|g| **g > 34.0).count();
    format!(
        "{} frames in {:.0} ms, slowest {:.0} ms, {} over 34 ms",
        stamps.len(),
        stamps[stamps.len() - 1] - stamps[0],
        slowest,
        over
    )
}

/// Where a pointer at box x `x` falls: the bucket under it, from the view's look-up table (one entry per 2 plot px).
fn bucket_at(t: &Trend, x: f64, sx: f64) -> i32 {
    if t.lut.is_empty() || sx <= 0.0 {
        return -1;
    }
    let px = (x - L) / sx;
    if px < -8.0 || px > t.pw + 8.0 {
        return -1;
    }
    let i = ((px / 2.0).floor().max(0.0) as usize).min(t.lut.len() - 1);
    t.lut[i]
}

#[component]
#[allow(clippy::too_many_arguments)]
fn TrendChart(
    trend: Memo<Trend>,
    shown: RwSignal<Vec<String>>,
    split: Memo<bool>,
    cache: Memo<bool>,
    bw: f64,
    bh: f64,
    cur: RwSignal<i32>,
    pinned: RwSignal<bool>,
    bucket: Memo<Option<Bucket>>,
    draw: bool,
) -> impl IntoView {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let id = format!("tg{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let pw = (bw - L - R).max(40.0);
    let ph = (bh - T - B).max(40.0);
    // the paths are laid out for the size bridge.js last saw; scale until it lays them out again
    let scale = Memo::new(move |_| {
        trend.with(|t| {
            (
                if t.pw > 0.0 { pw / t.pw } else { 1.0 },
                if t.ph > 0.0 { ph / t.ph } else { 1.0 },
            )
        })
    });
    let plot = move || {
        let (sx, sy) = scale.get();
        format!("translate({L} {T}) scale({sx:.4} {sy:.4})")
    };
    let path = move |i: usize| move || shown.with(|p| p.get(i).cloned().unwrap_or_default());
    let gradients = {
        let id = id.clone();
        let stops = TYPES
            .iter()
            .map(|k| {
                let c = key_color(k);
                view! {
                    <linearGradient id=format!("{id}-{k}") x1="0" y1="0" x2="0" y2="1">
                        <stop offset="0" stop-color=c.clone() stop-opacity="0.78"/>
                        <stop offset="1" stop-color=c stop-opacity="0.46"/>
                    </linearGradient>
                }
            })
            .collect_view();
        let tok = key_color("tok");
        view! {
            <defs>
                <linearGradient id=format!("{id}-tok") x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0" stop-color=tok.clone() stop-opacity="0.40"/>
                    <stop offset="1" stop-color=tok stop-opacity="0.04"/>
                </linearGradient>
                {stops}
            </defs>
        }
    };
    // The axes redraw (and fade in) only when what they show changes, not on every refresh of the same range.
    let axes_view = Memo::new(move |_| {
        trend.with(|t| Trend {
            y_ticks: t.y_ticks.clone(),
            x_ticks: t.x_ticks.clone(),
            minor: t.minor.clone(),
            tail_x: t.tail_x,
            tail_label: t.tail_label.clone(),
            hatch: t.hatch.clone(),
            clip_note: t.clip_note.clone(),
            cost_shown: t.cost_shown,
            ..Default::default()
        })
    });
    let axes = move || {
        let t = axes_view.get();
        let (sx, sy) = scale.get();
        let y = t.y_ticks.iter().map(|k| {
            let yy = T + k.y * sy;
            view! {
                <line x1=L x2=L + pw y1=yy y2=yy class=if k.base { "ax-base" } else { "ax-gl" }/>
                <text x=L - 10.0 y=yy + 4.0 text-anchor="end" class="ax-t">{k.left.clone()}</text>
                <text x=L + pw + 10.0 y=yy + 4.0 class="ax-t ink2">{k.right.clone()}</text>
            }
        }).collect_view();
        let x = t.x_ticks.iter().map(|k| {
            let xx = L + k.x * sx;
            view! {
                {k.major.then(|| view! { <line x1=xx x2=xx y1=T y2=T + ph class="ax-gl"/> })}
                <text x=xx y=T + ph + 21.0 text-anchor="middle" class=if k.major { "ax-t ink2" } else { "ax-t" }>{k.label.clone()}</text>
            }
        }).collect_view();
        let tail = (t.tail_x >= 0.0).then(|| {
            let tx = L + t.tail_x * sx;
            view! {
                <svg x=tx y=T width=(L + pw - tx).max(0.0) height=ph overflow="hidden">
                    <path d=t.hatch.clone() transform=format!("translate({:.2} 0) scale({sx:.4} {sy:.4})", -t.tail_x * sx) class="ax-hatch" vector-effect="non-scaling-stroke"/>
                </svg>
                <line x1=tx x2=tx y1=T - 4.0 y2=T + ph class="ax-tail"/>
                <text x=tx - 6.0 y=T + 14.0 text-anchor="end" class="ax-t ax-cap">{t.tail_label.clone()}</text>
            }
        });
        view! {
            <g class="ax-layer">
                {y}{x}
                <text x=L - 10.0 y=14 text-anchor="end" class="ax-t ax-cap">"TOKENS"</text>
                {t.cost_shown.then(|| view! { <text x=L + pw + 10.0 y=14 class="ax-t ax-cap">"USD"</text> })}
                <path d=t.minor.clone() transform=format!("translate({L} {T}) scale({sx:.4} {sy:.4})") class="ax-gl" vector-effect="non-scaling-stroke"/>
                {tail}
                {(!t.clip_note.is_empty()).then(|| view! { <text x=L + 6.0 y=T + 14.0 class="ax-t ax-cap">{t.clip_note.clone()}</text> })}
            </g>
        }
    };
    let id_fill = move |k: &str| format!("url(#{}-{k})", id);
    let fill_tok = id_fill("tok");
    let band_fills: Vec<String> = TYPES.iter().map(|k| id_fill(k)).collect();
    let move_at = move |e: web_sys::PointerEvent| {
        let Some(target) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let x = e.client_x() as f64 - target.get_bounding_client_rect().left();
        let (sx, _) = scale.get_untracked();
        let i = trend.with_untracked(|t| bucket_at(t, x, sx));
        if e.pointer_type() == "mouse" {
            if !pinned.get_untracked() {
                cur.set(i);
            }
        } else if e.buttons() != 0 && i >= 0 {
            // a horizontal drag scrubs across the buckets
            cur.set(i);
            pinned.set(true);
        }
    };
    let tap = move |e: web_sys::PointerEvent| {
        let Some(target) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let x = e.client_x() as f64 - target.get_bounding_client_rect().left();
        let (sx, _) = scale.get_untracked();
        let i = trend.with_untracked(|t| bucket_at(t, x, sx));
        if e.pointer_type() == "mouse" {
            return;
        }
        if i < 0 || (pinned.get_untracked() && cur.get_untracked() == i) {
            cur.set(-1);
            pinned.set(false);
        } else {
            cur.set(i);
            pinned.set(true);
        }
    };
    let leave = move |e: web_sys::PointerEvent| {
        if e.pointer_type() == "mouse" && !pinned.get_untracked() {
            cur.set(-1);
        }
    };
    let bx = move || {
        let (sx, _) = scale.get();
        bucket.with(|b| b.as_ref().map(|b| L + b.x * sx).unwrap_or(-100.0))
    };
    view! {
        <svg class="trend-svg axes" width=bw height=bh viewBox=format!("0 0 {bw} {bh}") aria-hidden="true">
            {axes}
        </svg>
        <div class="trend-data" class:draw-on=draw class:split=move || split.get()>
            <svg class="trend-svg" width=bw height=bh viewBox=format!("0 0 {bw} {bh}") role="img" aria-label="Usage trend">
                {gradients}
                <g transform=plot>
                    <g class="tr-total">
                        <path d=path(0) fill=fill_tok/>
                        <path d=path(1) fill="none" stroke=key_color("tok") stroke-width="1.75" vector-effect="non-scaling-stroke"/>
                    </g>
                    <g class="tr-split">
                        {band_fills.into_iter().enumerate().map(|(i, f)| view! { <path d=path(3 + i) fill=f/> }).collect_view()}
                        {TYPES.iter().enumerate().map(|(i, k)| {
                            let hide = *k == "cr";
                            view! { <path d=path(7 + i) fill="none" stroke=key_color(k) stroke-width="1.25" vector-effect="non-scaling-stroke"
                                visibility=move || if hide && !cache.get() { "hidden" } else { "visible" }/> }
                        }).collect_view()}
                    </g>
                    <path d=path(2) fill="none" stroke=key_color("cost") stroke-width="2" stroke-linecap="round" vector-effect="non-scaling-stroke"
                        visibility=move || if trend.with(|t| t.cost_shown) { "visible" } else { "hidden" }/>
                </g>
            </svg>
        </div>
        <Show when=move || trend.with(|t| t.empty)>
            <p class="chart-empty">{move || trend.with(|t| t.empty_text.clone())}</p>
        </Show>
        <div class="trend-hit" on:pointermove=move_at on:pointerdown=move_at on:pointerup=tap on:pointerleave=leave>
            <Show when=move || bucket.with(|b| b.is_some())>
                {move || {
                    let (_, sy) = scale.get();
                    let x = bx();
                    bucket.get().map(|b| {
                        let cost_y = (b.y_cost >= 0.0).then(|| T + b.y_cost * sy);
                        let left = x + 16.0 + 236.0 <= bw - 6.0;
                        view! {
                            <span class="xh" style=format!("left:{x:.1}px;top:{T}px;height:{ph:.0}px")></span>
                            <span class="xdot" style=format!("left:{x:.1}px;top:{:.1}px;border-color:{}", T + b.y_tok * sy, key_color("tok"))></span>
                            {cost_y.filter(|_| trend.with_untracked(|t| t.cost_shown)).map(|y| view! {
                                <span class="xdot" style=format!("left:{x:.1}px;top:{y:.1}px;border-color:{}", key_color("cost"))></span>
                            })}
                            <div class="ro-card" style=if left { format!("left:{:.0}px;top:{}px", x + 16.0, T + 4.0) } else { format!("right:{:.0}px;top:{}px", bw - x + 16.0, T + 4.0) }>
                                <div class="ro-time">{b.time.clone()}</div>
                                <RoRow k="in" label="Input" value=b.vin.clone()/>
                                <RoRow k="out" label="Output" value=b.vout.clone()/>
                                <RoRow k="cw" label="Cache write" value=b.vcw.clone()/>
                                <RoRow k="cr" label="Cache read" value=b.vcr.clone() dim=b.cr_dim/>
                                <hr/>
                                <RoRow k="tok" label="All tokens" value=b.all.clone() strong=true/>
                                <RoRow k="cost" label="Estimated cost" value=b.cost.clone() strong=true/>
                                {(!b.by_provider.is_empty()).then(|| view! { <p class="ro-foot tnum">{b.by_provider.clone()}</p> })}
                                {(!b.foot.is_empty()).then(|| view! { <p class="ro-foot tnum">{b.foot.clone()}</p> })}
                                {(!b.foot2.is_empty()).then(|| view! { <p class="ro-foot">{b.foot2.clone()}</p> })}
                            </div>
                        }
                    })
                }}
            </Show>
        </div>
    }
}

#[component]
fn RoRow(
    k: &'static str,
    label: &'static str,
    value: String,
    #[prop(optional)] strong: bool,
    #[prop(optional)] dim: bool,
) -> impl IntoView {
    view! {
        <div class="ro-row" class:dim=dim class:strong=strong>
            <Swatch color=key_color(k) shape=if k == "cost" { "line" } else { "" }/>
            <span>{label}</span>
            <b class="tnum">{value}</b>
        </div>
    }
}

/// Below desktop: the reading pinned by a tap (or followed by a mouse), in one row above the chart.
#[component]
fn ReadoutBar(
    bucket: Memo<Option<Bucket>>,
    pinned: RwSignal<bool>,
    cur: RwSignal<i32>,
) -> impl IntoView {
    let coarse = window()
        .match_media("(pointer: coarse)")
        .ok()
        .flatten()
        .is_some_and(|m| m.matches());
    view! {
        <div class="ro-bar" data-probe="trend-ro">
            {move || match bucket.get() {
                None => view! { <span class="q3">{if coarse { "Tap the chart for a reading" } else { "Point at the chart for a reading" }}</span> }.into_any(),
                Some(b) => {
                    let extra = if !b.foot.is_empty() || !b.foot2.is_empty() || !b.by_provider.is_empty() {
                        let first = if b.foot.is_empty() { b.by_provider.clone() } else { b.foot.clone() };
                        let text = if b.foot2.is_empty() { first } else { format!("{first} · {}", b.foot2) };
                        view! { <span class="ro-l2 q3">{text}</span> }.into_any()
                    } else {
                        view! {
                            <span class="ro-l2 tnum">
                                <span class="ro-v"><Swatch color=key_color("in")/>{b.vin.clone()}</span>
                                <span class="ro-v"><Swatch color=key_color("out")/>{b.vout.clone()}</span>
                                <span class="ro-v"><Swatch color=key_color("cw")/>{b.vcw.clone()}</span>
                                <span class="ro-v"><Swatch color=key_color("cr")/>{b.vcr.clone()}</span>
                            </span>
                        }.into_any()
                    };
                    view! {
                        <span class="ro-l1">
                            <span class="ro-time">{b.time.clone()}</span>
                            <span class="ro-v tnum"><Swatch color=key_color("tok")/>"All "{b.all.clone()}</span>
                            <span class="ro-v tnum"><Swatch color=key_color("cost") shape="line"/>{b.cost.clone()}</span>
                        </span>
                        {extra}
                        <button class="icon-btn bare ro-close" aria-label="Close the reading" hidden=move || !pinned.get()
                            on:click=move |_| { cur.set(-1); pinned.set(false); }><Icon name="close"/></button>
                    }.into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_trace_counts_slow_frames() {
        assert_eq!(frame_trace(&[0.0]), "");
        assert_eq!(
            frame_trace(&[0.0, 16.0, 33.0, 80.0]),
            "4 frames in 80 ms, slowest 47 ms, 1 over 34 ms"
        );
    }

    #[test]
    fn a_pointer_finds_its_bucket_through_the_look_up_table() {
        let t = Trend {
            pw: 10.0,
            lut: vec![0, 0, 1, 1, 2, 2],
            ..Default::default()
        };
        assert_eq!(bucket_at(&t, L + 0.5, 1.0), 0);
        assert_eq!(bucket_at(&t, L + 5.0, 1.0), 1);
        assert_eq!(bucket_at(&t, L + 10.0, 1.0), 2);
        // past the plot by more than 8 px: nothing
        assert_eq!(bucket_at(&t, L - 9.0, 1.0), -1);
        assert_eq!(bucket_at(&t, L + 19.0, 1.0), -1);
        // a box twice as wide as the layout: the pointer is scaled back into the table
        assert_eq!(bucket_at(&t, L + 10.0, 2.0), 1);
    }
}
