//! Quota history (each account's main window as a step sparkline, grouped by provider; a row opens its focus chart)
//! and Upcoming resets and expiries (two balanced columns of whole days).

use super::view::*;
use super::{Ax, CardHead, runs};
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

#[component]
pub fn QuotaCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let quota = ax.part(|v| v.quota.clone());
    view! {
        <section class="plate ax-card quota-card" style="--i:10">
            <CardHead title="Quota history" sub=Signal::derive(move || quota.with(|q| q.sub.clone()))/>
            <div class="qh-row qh-colhead colhead">
                <span class="cap">"Account"</span>
                <span class="qh-ticks">
                    {move || quota.with(|q| q.ticks.clone()).into_iter().map(|t| view! {
                        <span class="cap" style=format!("left:{:.2}%", t.pos.clamp(0.0, 1.0) * 100.0)>{t.label}</span>
                    }).collect_view()}
                </span>
                <span class="cap r">"Used"</span>
                <span class="cap r">"Next reset"</span>
                <span></span>
            </div>
            {move || quota.with(|q| q.groups.clone()).into_iter().map(|g| {
                let provider = g.provider.clone();
                let collapsed = g.collapsed;
                view! {
                    <div class="qh-group" class:collapsed=collapsed>
                        <button class="qh-ghead" aria-expanded=(!collapsed).to_string() on:click=move |_| ax.act("analytics-group", &provider)>
                            <Icon name="chev-down" class="chev"/>
                            <ProviderMark provider=g.provider.clone() size=18/>
                            <b>{g.label.clone()}</b>
                            <span class="q3 qh-meta">{g.meta.clone()}</span>
                        </button>
                        {(!collapsed).then(|| g.rows.into_iter().map(|r| view! { <QuotaRow r=r quota=quota/> }).collect_view())}
                    </div>
                }
            }).collect_view()}
        </section>
    }
}

#[component]
fn QuotaRow(r: QRow, quota: Memo<Quota>) -> impl IntoView {
    let ax = expect_context::<Ax>();
    let id = r.id.clone();
    let open = r.open;
    let value_class = match r.sev {
        s if s >= 2 => "w-crit",
        1 => "w-warn",
        _ => "w-ink",
    };
    let fills = [
        (r.calm_fill.clone(), 0, 0.12),
        (r.warn_fill.clone(), 1, 0.16),
        (r.crit_fill.clone(), 2, 0.16),
        (r.over_fill.clone(), 3, 0.16),
    ];
    let lines = [
        (r.calm_line.clone(), 0),
        (r.warn_line.clone(), 1),
        (r.crit_line.clone(), 2),
        (r.over_line.clone(), 3),
    ];
    let focus_id = r.id.clone();
    let focus =
        Memo::new(move |_| quota.with(|q| q.focus.iter().find(|f| f.id == focus_id).cloned()));
    view! {
        <button class="qh-row" class:open=open data-probe=format!("qrow:{}", r.id) aria-expanded=open.to_string()
            on:click=move |_| ax.act("analytics-focus", &id)>
            <span class="qh-acct">
                <ProviderMark provider=r.provider.clone() size=18/>
                <span class="qh-label">{r.label.clone()}</span>
                {r.active.then(|| view! { <span class="w-good qh-active">"Active"</span> })}
                {(!r.plan.is_empty()).then(|| view! { <span class="q3 qh-plan">{r.plan.clone()}</span> })}
                {(!r.win.is_empty()).then(|| view! { <span class="q3">{r.win.clone()}</span> })}
            </span>
            <span class="qh-spark">
                {if !r.na.is_empty() {
                    view! { <span class="q3 qh-na">{r.na.clone()}</span> }.into_any()
                } else {
                    view! {
                        <svg viewBox="0 0 600 24" preserveAspectRatio="none" aria-hidden="true">
                            {fills.into_iter().filter(|f| !f.0.is_empty()).map(|(d, s, o)| view! { <path d=d fill=sev_color(s) fill-opacity=o/> }).collect_view()}
                            <path d=r.limit.clone() fill="none" stroke="var(--grid-strong)" stroke-width="1" vector-effect="non-scaling-stroke"/>
                            {lines.into_iter().filter(|l| !l.0.is_empty()).map(|(d, s)| view! {
                                <path class="qh-line" d=d fill="none" stroke=sev_color(s) vector-effect="non-scaling-stroke"/>
                            }).collect_view()}
                        </svg>
                    }.into_any()
                }}
            </span>
            <span class="qh-used r tnum">
                {if r.has_value {
                    view! { <b class=value_class>{r.value_text.clone()}</b><span class="q3">"%"</span> }.into_any()
                } else {
                    view! { <span class="q3">"Unavailable"</span> }.into_any()
                }}
            </span>
            <span class="qh-next r tnum q2" data-tip=(!r.next_tip.is_empty()).then(|| r.next_tip.clone())>{r.next.clone()}</span>
            <Icon name="chev-down" class=if open { "chev up" } else { "chev" }/>
        </button>
        {move || open.then(|| focus.get().map(|f| view! { <FocusPanel f=f/> })).flatten()}
    }
}

/// Where a pointer at box x `x` falls on the focus chart: its stop, from the look-up table (one entry per 4 px).
fn stop_at(f: &Focus, x: f64, sx: f64) -> i32 {
    if f.lut.is_empty() || sx <= 0.0 {
        return -1;
    }
    let i = ((x / sx / 4.0).floor().max(0.0) as usize).min(f.lut.len() - 1);
    f.lut[i]
}

/// The legend sample of a focus series (its dash), the projection and the threshold.
fn legend_path(kind: &str, dash: i32) -> &'static str {
    match (kind, dash) {
        ("proj", _) => "M1 5L2.5 5M6.5 5L8 5M12 5L13.5 5M17.5 5L19 5",
        ("thr", _) => "M1 5L7 5M11 5L17 5",
        (_, 1) => "M1 5L10 5M14 5L21 5",
        (_, 2) => "M1 5L10 5M13 5L15 5M18 5L21 5",
        _ => "M1 5L21 5",
    }
}

#[component]
fn FocusPanel(f: Focus) -> impl IntoView {
    let ax = expect_context::<Ax>();
    let size = RwSignal::new((0.0f64, 0.0f64));
    let boxed = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = boxed.get() {
            observe_size(&el, move |w, h| size.set((w.round(), h.round())));
        }
    });
    let (fw, fh) = (f.w, f.h);
    Effect::new(move |_| {
        let (w, h) = size.get();
        if w > 0.0 && h > 0.0 && ((w - fw).abs() >= 1.0 || (h - fh).abs() >= 1.0) {
            ax.act("analytics-layout", &format!("focus,{w},{h}"));
        }
    });
    let provider = f.provider.clone();
    let id = f.id.clone();
    let compare = f.compare;
    let cur = RwSignal::new(-1i32);
    let pinned = RwSignal::new(false);
    let stops = StoredValue::new(f.stops.clone());
    let lut = StoredValue::new(f.lut.clone());
    let legend = f.legend.clone();
    let foot = f.foot.clone();
    let empty = f.empty.clone();
    let chart = f.clone();
    let stop = Memo::new(move |_| {
        let i = cur.get();
        (i >= 0)
            .then(|| stops.with_value(|s| s.get(i as usize).cloned()))
            .flatten()
    });
    let locate = move |e: &web_sys::PointerEvent| -> i32 {
        let Some(el) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return -1;
        };
        let x = e.client_x() as f64 - el.get_bounding_client_rect().left();
        let (w, _) = size.get_untracked();
        let sx = if fw > 0.0 { w / fw } else { 1.0 };
        lut.with_value(|l| {
            stop_at(
                &Focus {
                    lut: l.clone(),
                    ..Default::default()
                },
                x,
                sx,
            )
        })
    };
    let p2 = provider.clone();
    view! {
        <div class="focus">
            <div class="focus-top">
                <span class="lgd-row tight">
                    {legend.into_iter().map(|l| {
                        let sample = if l.kind == "active" {
                            view! { <span class="sw act"></span> }.into_any()
                        } else {
                            let stroke = match l.kind.as_str() { "proj" => "var(--ink-3)".to_string(), "thr" => "var(--ink)".to_string(), _ => series_color(&provider, l.shade) };
                            view! { <svg class="fl-s" viewBox="0 0 22 10" aria-hidden="true"><path d=legend_path(&l.kind, l.dash) stroke=stroke stroke-width=l.width fill="none" stroke-linecap=if l.kind == "proj" { "round" } else { "butt" }/></svg> }.into_any()
                        };
                        view! { <span class="lgd">{sample}<span class="lgd-l">{l.name}</span></span> }
                    }).collect_view()}
                </span>
                {chart.comparable.then(|| view! {
                    <Switch on=Signal::derive(move || compare) label=chart.cmp_label.clone()
                        on_toggle=Callback::new(move |_| ax.act("analytics-compare", &id))/>
                })}
            </div>
            <div class="focus-box" node_ref=boxed
                on:pointermove=move |e: web_sys::PointerEvent| {
                    let i = locate(&e);
                    if e.pointer_type() == "mouse" { if !pinned.get_untracked() { cur.set(i) } } else if e.buttons() != 0 && i >= 0 { cur.set(i); pinned.set(true); }
                }
                on:pointerleave=move |e: web_sys::PointerEvent| if e.pointer_type() == "mouse" && !pinned.get_untracked() { cur.set(-1) }
                on:pointerup=move |e: web_sys::PointerEvent| {
                    if e.pointer_type() == "mouse" { return; }
                    let i = locate(&e);
                    if i < 0 || (pinned.get_untracked() && cur.get_untracked() == i) { cur.set(-1); pinned.set(false); } else { cur.set(i); pinned.set(true); }
                }>
                {move || {
                    let (w, h) = size.get();
                    (w > 0.0 && fw > 0.0).then(|| {
                        let (sx, sy) = (w / fw, h / fh.max(1.0));
                        let f = chart.clone();
                        let layer = |n: i32| f.shapes.iter().filter(|s| s.layer == n).map(|s| view! {
                            <path d=s.d.clone() fill=if s.fill.is_empty() { "none".to_string() } else { theme_key(&s.fill, &f.provider, s.shade) }
                                stroke=if s.stroke.is_empty() { "none".to_string() } else { theme_key(&s.stroke, &f.provider, s.shade) }
                                stroke-width=s.sw opacity=s.op stroke-linecap=if s.round { "round" } else { "butt" } vector-effect="non-scaling-stroke"/>
                        }).collect_view();
                        let dots = f.dots.iter().map(|d| {
                            let c = series_color(&f.provider, d.shade);
                            let paper = d.fill == "paper";
                            view! { <circle cx=d.x * sx cy=d.y * sy r=d.r fill=if paper { "var(--paper)".to_string() } else { c.clone() } stroke=if paper { c } else { "none".to_string() } stroke-width="2"/> }
                        }).collect_view();
                        let labels = f.labels.iter().map(|l| view! {
                            {l.plate.then(|| view! { <rect x=l.px * sx y=l.py * sy width=l.pw height=l.ph rx="3" class="fl-plate"/> })}
                            <text x=l.px * sx + (l.x - l.px) y=l.py * sy + (l.y - l.py) class="ax-t" fill=theme_key(&l.color, &f.provider, l.shade)>{l.text.clone()}</text>
                        }).collect_view();
                        let scale = format!("scale({sx:.4} {sy:.4})");
                        view! {
                            <svg width=w height=h viewBox=format!("0 0 {w} {h}") role="img" aria-label="Focus chart">
                                <g transform=scale.clone()>{layer(0)}</g>
                                <g transform=scale.clone() class="draw-on-svg">{layer(1)}</g>
                                <g transform=scale>{layer(2)}</g>
                                {dots}{labels}
                            </svg>
                        }
                    })
                }}
                {(!empty.is_empty()).then(|| view! { <p class="chart-empty">{empty.clone()}</p> })}
                {move || stop.get().map(|s| {
                    let (w, h) = size.get_untracked();
                    let sx = if fw > 0.0 { w / fw } else { 1.0 };
                    let sy = if fh > 0.0 { h / fh } else { 1.0 };
                    let x = s.x * sx;
                    let left = x + 236.0 <= w;
                    let p = p2.clone();
                    view! {
                        <span class="xh" style=format!("left:{x:.1}px;top:0;height:100%")></span>
                        <span class="xdot" style=format!("left:{x:.1}px;top:{:.1}px;border-color:{}", s.dot_y * sy, series_color(&p, 0))></span>
                        <div class="ro-card" style=if left { format!("left:{:.0}px;top:6px", x + 14.0) } else { format!("right:{:.0}px;top:6px", w - x + 14.0) }>
                            <div class="ro-time">{s.time.clone()}</div>
                            {s.rows.iter().map(|r| view! {
                                <div class="ro-row"><Swatch color=series_color(&p, r.shade)/><span>{r.name.clone()}</span><b class="tnum">{r.value.clone()}</b></div>
                            }).collect_view()}
                        </div>
                    }
                })}
            </div>
            <p class="q3 focus-foot">{foot}</p>
        </div>
    }
}

#[component]
pub fn AgendaCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let agenda = ax.part(|v| v.agenda.clone());
    let table = move |rows: Vec<AgendaRow>| {
        rows.into_iter()
            .map(|r| {
                if r.kind == "day" {
                    view! { <div class="ag-day"><b>{r.day}</b><span class="tnum q3">{r.hint}</span></div> }.into_any()
                } else {
                    view! {
                        <div class="ag-line" data-tip=(!r.tip.is_empty()).then(|| r.tip.clone())>
                            <span class="ag-time tnum"><b>{r.time}</b>{(!r.rel.is_empty()).then(|| view! { <span class="q3">{r.rel.clone()}</span> })}</span>
                            <span class="ag-who"><ProviderMark provider=r.provider.clone() size=18/><span>{r.who}</span></span>
                            <span class="ag-what"><span class="q2">{r.what}</span><span class="ag-now tnum">{runs(r.now)}</span></span>
                        </div>
                    }
                    .into_any()
                }
            })
            .collect_view()
    };
    view! {
        <section class="plate ax-card agenda-card" style="--i:11">
            <CardHead title="Upcoming resets and expiries" sub="When each limit resets to 0% used, and when credits or packs expire. Times are local."/>
            {move || {
                let a = agenda.get();
                if a.empty {
                    return view! { <p class="chart-empty static">"No upcoming resets or expiries reported."</p> }.into_any();
                }
                view! {
                    <div class="ax-body agenda">
                        <div class="ag-col">{table(a.a)}</div>
                        {(!a.b.is_empty()).then(|| view! { <div class="ag-col">{table(a.b)}</div> })}
                    </div>
                }.into_any()
            }}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_finds_its_focus_stop() {
        let f = Focus {
            lut: vec![0, 0, 1, 1, 2],
            ..Default::default()
        };
        assert_eq!(stop_at(&f, 0.0, 1.0), 0);
        assert_eq!(stop_at(&f, 9.0, 1.0), 1);
        assert_eq!(stop_at(&f, 400.0, 1.0), 2);
        assert_eq!(stop_at(&f, 18.0, 2.0), 1);
        assert_eq!(stop_at(&Focus::default(), 5.0, 1.0), -1);
    }
}
