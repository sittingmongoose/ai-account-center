//! Cost by model (every model with usage, by cost or by tokens, each bar split by token type) and Model usage (the
//! donut by tokens or cost, with its legend). Selecting a model opens its detail: shares, token types, the input to
//! output ratio and the rate it was priced at.

use super::view::*;
use super::{Ax, CardHead};
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

const TYPES: [(&str, &str); 4] = [
    ("in", "Input"),
    ("out", "Output"),
    ("cw", "Cache write"),
    ("cr", "Cache read"),
];

/// The model whose detail is open (its key) and where it hangs from; one per page.
#[derive(Clone, Copy)]
pub struct Detail {
    pub open: RwSignal<bool>,
    pub key: RwSignal<String>,
    pub anchor: Anchor,
}

impl Detail {
    fn show(&self, key: &str, target: Option<web_sys::EventTarget>) {
        if key.is_empty() {
            return;
        }
        if let Some(el) = target.and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
            let row = el.closest("[data-model]").ok().flatten().unwrap_or(el);
            self.anchor.set(row);
        }
        self.key.set(key.to_string());
        self.open.set(true);
    }
}

/// One model detail for the page: Cost by model's rows and the donut open the same one.
pub fn provide_detail() {
    provide_context(Detail {
        open: RwSignal::new(false),
        key: RwSignal::new(String::new()),
        anchor: Anchor::tapped(),
    });
}

fn detail_ctx() -> Detail {
    expect_context::<Detail>()
}

#[component]
pub fn CbmCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let cbm = ax.part(|v| v.usage.cbm.clone());
    let sort = ax.part(|v| {
        if v.state.cbm_sort == "tokens" {
            "tokens".to_string()
        } else {
            "cost".to_string()
        }
    });
    let detail = detail_ctx();
    view! {
        <section class="plate ax-card cbm-card" style="--i:3">
            <CardHead title="Cost by model" sub=Signal::derive(move || cbm.with(|c| c.sub.clone())) note=Signal::derive(move || cbm.with(|c| c.note.clone()))>
                <span class="lgd-row tight">
                    {TYPES.iter().map(|(k, l)| view! { <LegendEntry color=key_color(k) label=*l/> }).collect_view()}
                </span>
                <span class="sort"><span class="cap">"Sort"</span>
                    <Segmented options=vec![("cost".into(), "Cost".into()), ("tokens".into(), "Tokens".into())] selected=sort label="Sort"
                        on_select=Callback::new(move |v: String| ax.act("analytics-cbm-sort", &v))/>
                </span>
            </CardHead>
            <div class="cbm-cols colhead">
                <span class="cap">"Estimated cost by token type"</span>
                <span class="cap r">"Tokens"</span><span class="cap r">"Cost"</span><span class="cap r">"Share"</span>
            </div>
            <div class="cbm-rows">
                {move || {
                    let c = cbm.get();
                    if c.rows.is_empty() {
                        let text = if ax.ready.get_untracked() { c.empty.clone() } else { "Loading model activity".into() };
                        return view! { <p class="chart-empty static">{text}</p> }.into_any();
                    }
                    c.rows.into_iter().map(|m| {
                        let key = m.key.clone();
                        let parts = [("in", m.fin, m.tip_in.clone()), ("out", m.fout, m.tip_out.clone()), ("cw", m.fcw, m.tip_cw.clone()), ("cr", m.fcr, m.tip_cr.clone())];
                        let total: f64 = parts.iter().map(|p| p.1.max(0.0)).sum();
                        view! {
                            <button class="cbm-row" data-model=m.key.clone() on:click=move |e| detail.show(&key, e.target())
                                class:sel=move || detail.open.get() && detail.key.with(|k| *k == m.key)>
                                <span class="cbm-name"><ProviderMark provider=m.provider.clone() size=18/><span>{m.name.clone()}</span></span>
                                <span class="cbm-bar"><span class="cbm-fill" style=format!("width:{:.2}%", m.w.clamp(0.0, 100.0))>
                                    {parts.into_iter().filter(|p| p.1 > 0.0).map(|(k, f, tip)| view! {
                                        <i style=format!("flex:{:.4} 1 0;background:{}", f / total.max(1e-9), key_color(k)) data-tip=tip></i>
                                    }).collect_view()}
                                </span></span>
                                <span class="cbm-tok tnum q2" data-tip=m.tok_tip.clone()>{m.tok.clone()}</span>
                                <span class="cbm-cost tnum" class:na=m.cost_na>{m.cost.clone()}{m.partial.then(|| view! { <small>"partial"</small> })}</span>
                                <span class="cbm-share tnum q2">{(!m.share.is_empty() && m.share != "Unavailable").then(|| format!("{}%", m.share))}</span>
                            </button>
                        }
                    }).collect_view().into_any()
                }}
            </div>
            <Show when=move || cbm.with(|c| !c.foot.is_empty())>
                <p class="ax-foot">{move || cbm.with(|c| c.foot.clone())}</p>
            </Show>
        </section>
    }
}

/// The open model's detail, from Cost by model's rows (one popover for the page).
#[component]
pub fn DetailPopover() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let detail = detail_ctx();
    let rows = ax.part(|v| v.usage.cbm.rows.clone());
    let model = Memo::new(move |_| {
        let key = detail.key.get();
        rows.with(|r| r.iter().find(|m| m.key == key).cloned())
    });
    // a model that left the list (another range or provider) closes its detail
    Effect::new(move |_| {
        if detail.open.get_untracked() && model.with(|m| m.is_none()) {
            detail.open.set(false);
        }
    });
    view! {
        <Popover open=detail.open anchor=detail.anchor width=440 label="Model detail">
            {move || model.get().map(|m| view! { <ModelDetail m=m close=Callback::new(move |_| detail.open.set(false))/> })}
        </Popover>
    }
}

#[component]
fn ModelDetail(m: ModelRow, close: Callback<()>) -> impl IntoView {
    view! {
        <div class="mdetail">
            <div class="md-top">
                {(!m.provider.is_empty()).then(|| view! { <ProviderMark provider=m.provider.clone() size=22/> })}
                <div class="md-name"><b>{m.name.clone()}</b><span class="q3">{m.sub.clone()}</span></div>
                <button class="icon-btn bare" aria-label="Close" on:click=move |_| close.run(())><Icon name="close"/></button>
            </div>
            <div class="md-stats">
                <div class="md-stat"><b class="tnum">{format!("{}%", m.usage)}</b><span>"of all tokens"</span></div>
                <div class="md-stat"><b class="tnum">{m.cost.clone()}</b><span>{if m.partial { "estimated cost, partial" } else { "estimated cost" }}</span></div>
                <div class="md-stat" data-tip=m.tok_tip.clone()><b class="tnum">{m.tok.clone()}</b><span>"tokens, all types"</span></div>
            </div>
            <p class="cap md-cap">"Token types"</p>
            {m.types.iter().map(|t| view! {
                <div class="md-type" class:none=t.none>
                    <Swatch color=key_color(&t.key)/>
                    <span class="md-tl">{t.label.clone()}</span>
                    <span class="md-bar"><i style=format!("width:{:.1}%;background:{}", t.w.clamp(0.0, 100.0), key_color(&t.key))></i></span>
                    <b class="tnum" data-tip=t.tip.clone()>{t.tok.clone()}</b>
                    <b class="tnum md-cost">{t.cost.clone()}</b>
                </div>
            }).collect_view()}
            <p class="cap md-cap">"Input to output ratio"</p>
            <div class="md-io">
                <b class="tnum">{m.io.clone()}</b>
                <p class="q2">{m.io_note.clone()}</p>
                {(!m.io_quiet.is_empty()).then(|| view! { <p class="q3">{m.io_quiet.clone()}</p> })}
            </div>
            <p class="md-rate q3">{m.rate.clone()}</p>
        </div>
    }
}

// ---------------------------------------------------------------- the donut

/// One arc of the ring, from `a0` to `a1` radians clockwise from the top, with a hairline gap at each end.
fn arc_path(c: f64, r0: f64, r1: f64, a0: f64, a1: f64) -> Option<String> {
    let (g0, g1) = (a0 + 0.01, a1 - 0.01);
    if g1 - g0 <= 0.002 {
        return None;
    }
    let large = if g1 - g0 > std::f64::consts::PI { 1 } else { 0 };
    let p = |r: f64, a: f64| (c + r * a.sin(), c - r * a.cos());
    let (x0, y0) = p(r1, g0);
    let (x1, y1) = p(r1, g1);
    let (x2, y2) = p(r0, g1);
    let (x3, y3) = p(r0, g0);
    Some(format!(
        "M{x0:.2} {y0:.2}A{r1:.2} {r1:.2} 0 {large} 1 {x1:.2} {y1:.2}L{x2:.2} {y2:.2}A{r0:.2} {r0:.2} 0 {large} 0 {x3:.2} {y3:.2}Z"
    ))
}

/// The ring's radii for a donut `size` px across (the Slint page's proportions).
fn radii(size: f64) -> (f64, f64) {
    let r1 = size / 2.0 - 42.0;
    let r0 = r1 - (size * 0.11).round().max(22.0);
    (r0, r1)
}

#[component]
pub fn DonutCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let donut = ax.part(|v| v.usage.donut.clone());
    let mode = ax.part(|v| {
        if v.state.donut == "cost" {
            "cost".to_string()
        } else {
            "tokens".to_string()
        }
    });
    let detail = detail_ctx();
    let hot = RwSignal::new(-1i32);
    let pin = RwSignal::new(-1i32);
    let width = RwSignal::new(0.0f64);
    let boxed = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = boxed.get() {
            observe_size(&el, move |w, _| width.set(w.round()));
        }
    });
    // another range, provider or mode drops the pinned arc
    Effect::new(move |_| {
        mode.track();
        donut.with(|d| d.segs.len());
        pin.set(-1);
    });
    let size = Memo::new(move |_| {
        let w = width.get();
        // the share labels sit outside the ring: keep 12 px for them on each side
        if w <= 0.0 {
            0.0
        } else {
            (w - 24.0).clamp(180.0, 340.0).min(w)
        }
    });
    let show = Memo::new(move |_| {
        let p = pin.get();
        if p >= 0 { p } else { hot.get() }
    });
    let seg_at = move |x: f64, y: f64| -> i32 {
        let s = size.get_untracked();
        let (r0, r1) = radii(s);
        let (dx, dy) = (x - s / 2.0, y - s / 2.0);
        let rr = (dx * dx + dy * dy).sqrt();
        donut.with_untracked(|d| {
            if rr < r0 - 2.0 || rr > r1 + 2.0 || d.lut.is_empty() {
                return -1;
            }
            let deg = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
            d.lut[(deg.floor() as usize).min(d.lut.len() - 1)]
        })
    };
    let local = |e: &web_sys::PointerEvent| -> Option<(f64, f64)> {
        let el = e.current_target()?.dyn_into::<web_sys::Element>().ok()?;
        let r = el.get_bounding_client_rect();
        Some((
            e.client_x() as f64 - r.left(),
            e.client_y() as f64 - r.top(),
        ))
    };
    let ring = move || {
        let s = size.get();
        if s <= 0.0 {
            return None;
        }
        let d = donut.get();
        let (r0, r1) = radii(s);
        let c = s / 2.0;
        let showing = show.get();
        let arcs = d
            .segs
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let dim = showing >= 0 && showing != i as i32;
                arc_path(c, r0, r1, seg.a0, seg.a1).map(|p| view! {
                <path d=p fill=model_color(&seg.provider, seg.mix, seg.other) class:dim=dim/>
            })
            })
            .collect_view();
        let labels = d.segs.iter().filter(|seg| seg.label).map(|seg| {
            let m = (seg.a0 + seg.a1) / 2.0;
            let sn = m.sin();
            let lx = c + (r1 + 12.0) * sn;
            let ly = c - (r1 + 12.0) * m.cos();
            let anchor = if sn.abs() < 0.3 { "middle" } else if sn > 0.0 { "start" } else { "end" };
            view! { <text x=format!("{lx:.1}") y=format!("{:.1}", ly + 4.0) text-anchor=anchor class="ax-t ink2">{format!("{}%", seg.share)}</text> }
        }).collect_view();
        let mid = (r0 + r1) / 2.0;
        Some(view! {
            <svg class="donut-svg" width=s height=s viewBox=format!("0 0 {s} {s}") role="img" aria-label="Model usage">
                <circle cx=c cy=c r=mid fill="none" stroke="var(--track)" stroke-width=r1 - r0/>
                <g class="arcs">{arcs}</g>
                <g class="d-labels">{labels}</g>
            </svg>
            <div class="donut-centre" style=format!("width:{:.0}px", (r0 * 2.0 - 12.0).max(40.0))>
                {move || {
                    let i = show.get();
                    let d = donut.get();
                    match (i >= 0).then(|| d.segs.get(i as usize).cloned()).flatten() {
                        Some(seg) => view! {
                            <b class="tnum">{format!("{}%", seg.share)}</b>
                            <span>{format!("{} · {}{}", seg.name, seg.value, d.unit)}</span>
                        }.into_any(),
                        None => view! {
                            <b class="tnum">{d.centre.clone()}</b>
                            <span>{if r0 * 2.0 >= 130.0 { d.centre_label.clone() } else { d.centre_short.clone() }}</span>
                        }.into_any(),
                    }
                }}
            </div>
        })
    };
    view! {
        <section class="plate ax-card donut-card" style="--i:4">
            <CardHead title="Model usage" sub=Signal::derive(move || donut.with(|d| d.sub.clone())) note=Signal::derive(move || donut.with(|d| d.note.clone()))>
                <Segmented options=vec![("tokens".into(), "Tokens".into()), ("cost".into(), "Cost".into())] selected=mode label="Model usage by"
                    on_select=Callback::new(move |v: String| ax.act("analytics-donut", &v))/>
            </CardHead>
            <div class="donut-body">
                <div class="donut-box" node_ref=boxed>
                    <Show when=move || donut.with(|d| d.empty)>
                        <p class="chart-empty">{move || donut.with(|d| d.empty_text.clone())}</p>
                    </Show>
                    <div class="donut-hit" style=move || { let s = size.get(); format!("width:{s:.0}px;height:{s:.0}px") }
                        on:pointermove=move |e: web_sys::PointerEvent| {
                            if let Some((x, y)) = local(&e) {
                                let i = seg_at(x, y);
                                if e.pointer_type() == "mouse" { hot.set(i) }
                            }
                        }
                        on:pointerleave=move |_| hot.set(-1)
                        on:click=move |e: web_sys::MouseEvent| {
                            let el = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok());
                            let Some(el) = el else { return };
                            let r = el.get_bounding_client_rect();
                            let i = seg_at(e.client_x() as f64 - r.left(), e.client_y() as f64 - r.top());
                            let fine = window().match_media("(hover: hover)").ok().flatten().is_some_and(|m| m.matches());
                            if fine {
                                let key = donut.with_untracked(|d| (i >= 0).then(|| d.segs.get(i as usize).map(|s| s.model.clone())).flatten().unwrap_or_default());
                                if !key.is_empty() { detail.anchor.set(el); detail.key.set(key); detail.open.set(true); }
                            } else if i < 0 || pin.get_untracked() == i { pin.set(-1) } else { pin.set(i) }
                        }>
                        {ring}
                    </div>
                </div>
                <div class="donut-legend">
                    {move || donut.get().legend.into_iter().map(|l| {
                        let seg = l.seg.clone();
                        let kind = l.kind.clone();
                        let model = seg.model.clone();
                        let idx = donut.with_untracked(|d| d.segs.iter().position(|s| s.key == seg.key).map(|i| i as i32).unwrap_or(-1));
                        let key = seg.key.clone();
                        let group = kind == "group";
                        view! {
                            <button class=format!("dl-row{}{}", if kind == "child" { " child" } else { "" }, if group { " group" } else { "" }) class:hot={move || idx >= 0 && show.get() == idx}
                                data-model=(!model.is_empty()).then(|| model.clone()) data-tip=(!seg.tip.is_empty() && !(group && l.open)).then(|| seg.tip.clone())
                                aria-expanded=group.then(|| l.open.to_string())
                                on:pointerenter=move |_| { if idx >= 0 { hot.set(idx) } } on:pointerleave=move |_| hot.set(-1)
                                on:click=move |ev: web_sys::MouseEvent| { if group { ax.act("analytics-donut-open", &key) } else { detail.show(&model, ev.target()) } }>
                                {(kind != "child").then(|| view! { <Swatch color=model_color(&seg.provider, seg.mix, seg.other)/> })}
                                <span class="dl-name">{seg.name.clone()}{group.then(|| view! { <Icon name="chev-down" class=if l.open { "chev up" } else { "chev" }/> })}</span>
                                <b class="tnum" class:na=seg.value == "Not logged" data-tip=(!seg.value_tip.is_empty()).then(|| seg.value_tip.clone())>{seg.value.clone()}</b>
                                <span class="dl-share tnum q2">{(!seg.share.is_empty()).then(|| format!("{}%", seg.share))}</span>
                            </button>
                        }
                    }).collect_view()}
                </div>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcs_leave_a_hairline_and_skip_slivers() {
        let p = arc_path(100.0, 50.0, 80.0, 0.0, std::f64::consts::FRAC_PI_2).expect("a quarter");
        assert!(p.starts_with("M100.80 20.00A80.00 80.00 0 0 1"), "{p}");
        // more than half the ring takes the large arc
        assert!(
            arc_path(100.0, 50.0, 80.0, 0.0, 4.0)
                .unwrap()
                .contains(" 0 1 1 ")
        );
        assert!(arc_path(100.0, 50.0, 80.0, 1.0, 1.02).is_none());
    }

    #[test]
    fn the_ring_keeps_the_slint_proportions() {
        assert_eq!(radii(300.0), (75.0, 108.0));
        assert_eq!(radii(180.0), (26.0, 48.0));
    }
}
