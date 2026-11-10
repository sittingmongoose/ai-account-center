//! Session stats (counts, per-provider rows and the most recent sessions), Recent sessions (the ones after them) and
//! Tokens and cache (each type's share of tokens and of cost, the cache hit-rate gauge, savings, cache cost, and reads
//! against writes).

use super::view::*;
use super::{Ax, CardHead};
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;

#[component]
pub fn SessionsCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let s = ax.part(|v| v.usage.sessions.clone());
    view! {
        <section class="plate ax-card sess-card" style="--i:5">
            <CardHead title="Session stats" sub=Signal::derive(move || s.with(|s| s.sub.clone())) note=Signal::derive(move || s.with(|s| s.note.clone()))/>
            <div class="ax-body">
                <div class="stats3">
                    {move || s.with(|s| s.stats.clone()).into_iter().map(|st| view! {
                        <div class="stat" data-probe=format!("sess-stat:{}", st.key)>
                            <b class="tnum" class:na=!st.has>{st.text}</b>
                            <span>{st.label}</span>
                        </div>
                    }).collect_view()}
                </div>
                <div class="sess-prov">
                    <div class="sp-row sp-head">
                        <span class="cap">"Provider"</span><span class="cap r">"Sessions"</span>
                        <span class="cap r">"Cost per session"</span><span class="cap r">"Events per session"</span>
                    </div>
                    {move || s.with(|s| s.rows.clone()).into_iter().map(|r| view! {
                        <div class="sp-row">
                            <span class="sp-name">{(!r.provider.is_empty() && r.provider != "other").then(|| view! { <ProviderMark provider=r.provider.clone() size=16/> })}<span>{r.label}</span></span>
                            <span class="tnum r" data-label="Sessions">{r.sessions}</span>
                            <span class="tnum r" data-label="Cost per session">{r.per}</span>
                            <span class="tnum r" data-label="Events per session" data-tip=r.events_tip>{r.events}</span>
                        </div>
                    }).collect_view()}
                </div>
                <p class="ax-minor">"Most recent sessions"</p>
                <SessList rows=Signal::derive(move || s.with(|s| s.recent.clone())) empty="No matching sessions in the most recent 50-session sample."/>
            </div>
            <Show when=move || s.with(|s| !s.foot.is_empty())>
                <p class="ax-foot">{move || s.with(|s| s.foot.clone())}</p>
            </Show>
        </section>
    }
}

#[component]
pub fn RecentCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let s = ax.part(|v| v.usage.sessions.clone());
    view! {
        <section class="plate ax-card recent-card" style="--i:6">
            <CardHead title="Recent sessions" sub=Signal::derive(move || s.with(|s| s.more_sub.clone()))/>
            <div class="ax-body">
                <SessList rows=Signal::derive(move || s.with(|s| s.recent_more.clone())) empty="No more sessions in the most recent 50-session sample."/>
            </div>
        </section>
    }
}

/// A session list: who served it, its models, tokens, cost and when it was last active.
#[component]
fn SessList(#[prop(into)] rows: Signal<Vec<SessRow>>, empty: &'static str) -> impl IntoView {
    view! {
        <div class="sess-list">
            <div class="sl-row sl-head">
                <span class="cap">"Models"</span>
                <span class="cap r">"Tokens"</span><span class="cap r">"Cost"</span><span class="cap r">"Last active"</span>
            </div>
            {move || {
                let list = rows.get();
                if list.is_empty() {
                    return view! { <p class="q3 sl-empty">{empty}</p> }.into_any();
                }
                list.into_iter().map(|r| view! {
                    <div class="sl-row" data-tip=r.tip.clone()>
                        // the provider's mark leads its models, as in Cost by model; a route no provider claims says Other
                        <span class="sl-models">
                            {if r.tool.is_empty() || r.tool == "other" {
                                view! { <span class="q3 sl-other">"Other"</span> }.into_any()
                            } else {
                                view! { <ProviderMark provider=r.tool.clone() size=16/> }.into_any()
                            }}
                            <span>{r.models}</span>
                        </span>
                        <span class="tnum r">{r.tokens}</span>
                        <span class="tnum r"><b>{r.cost}</b></span>
                        <span class="tnum r q3">{r.when}</span>
                    </div>
                }).collect_view().into_any()
            }}
        </div>
    }
}

/// The hit-rate gauge (the Slint `Gauge`): a 240 degree arc in a 160 x 132 box, with ticks every 60 degrees.
fn gauge(frac: f64) -> impl IntoView {
    let (cx, cy, r) = (80.0f64, 74.0f64, 58.0f64);
    let at = |deg: f64, rad: f64| {
        let a = deg.to_radians();
        (cx + rad * a.sin(), cy - rad * a.cos())
    };
    let f = frac.clamp(0.0, 1.0);
    let deg = -120.0 + 240.0 * f;
    let (x0, y0) = at(-120.0, r);
    let (xe, ye) = at(120.0, r);
    let (x1, y1) = at(deg, r);
    let ticks: String = [-120.0, -60.0, 0.0, 60.0, 120.0]
        .iter()
        .map(|d| {
            let (a, b) = at(*d, 67.0);
            let (c, e) = at(*d, 71.0);
            format!("M{a:.2} {b:.2}L{c:.2} {e:.2}")
        })
        .collect();
    let track = format!("M{x0:.2} {y0:.2}A58 58 0 1 1 {xe:.2} {ye:.2}");
    let value = (f > 0.0005).then(|| {
        format!(
            "M{x0:.2} {y0:.2}A58 58 0 {} 1 {x1:.2} {y1:.2}",
            if deg + 120.0 > 180.0 { 1 } else { 0 }
        )
    });
    view! {
        <svg class="gauge" viewBox="0 0 160 132" aria-hidden="true">
            <defs>
                <linearGradient id="gauge-cr" x1="0" y1="0" x2="1" y2="0">
                    <stop offset="0" stop-color="color-mix(in srgb, var(--chart-cache-read) 45%, var(--paper))"/>
                    <stop offset="1" stop-color="var(--chart-cache-read)"/>
                </linearGradient>
            </defs>
            <path d=track fill="none" stroke="var(--track)" stroke-width="10" stroke-linecap="round"/>
            <path d=ticks fill="none" stroke="var(--rule-strong)" stroke-width="1.25" stroke-linecap="round"/>
            {value.map(|d| view! { <path class="gauge-val" d=d fill="none" stroke="url(#gauge-cr)" stroke-width="10" stroke-linecap="round"/> })}
            <text x=format!("{x0:.1}") y=format!("{:.1}", y0 + 22.0) text-anchor="middle" class="ax-t">"0%"</text>
            <text x=format!("{xe:.1}") y=format!("{:.1}", ye + 22.0) text-anchor="middle" class="ax-t">"100%"</text>
        </svg>
    }
}

#[component]
pub fn TokensCacheCard() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let tokens = ax.part(|v| v.usage.tokens.clone());
    let cache = ax.part(|v| v.usage.cache.clone());
    view! {
        <section class="plate ax-card tokcache-card" style="--i:7">
            <CardHead title="Tokens and cache" sub=Signal::derive(move || tokens.with(|t| t.sub.clone()))/>
            <div class="ax-body tc-body">
                <div class="tok-rows">
                    {move || tokens.get().rows.into_iter().map(|t| {
                        let c = key_color(&t.key);
                        view! {
                            <div class="tk-row">
                                <span class="tk-name"><Swatch color=c.clone()/>{t.label}</span>
                                <span class="tk-bars">
                                    <span class="tk-bar"><i style=format!("width:{:.1}%;background:color-mix(in srgb, {c} 42%, transparent)", t.wt.clamp(0.0, 100.0))></i></span>
                                    <span class="tk-bar"><i style=format!("width:{:.1}%;background:{c}", t.wc.clamp(0.0, 100.0))></i></span>
                                </span>
                                <span class="tk-vals tnum">
                                    <span><b data-tip=t.tok_tip>{t.tok}</b>{(!t.tok_share.is_empty()).then(|| view! { <span class="q3">{format!("{}%", t.tok_share)}</span> })}</span>
                                    <span><b>{t.cost}</b>{(!t.cost_share.is_empty()).then(|| view! { <span class="q3">{format!("{}%", t.cost_share)}</span> })}</span>
                                </span>
                            </div>
                        }
                    }).collect_view()}
                </div>
                <div class="cache-eff">
                    <p class="ax-minor">"Cache efficiency"</p>
                    <div class="ce-grid">
                        <div class="ce-gauge">
                            {move || gauge(cache.with(|c| c.read_frac))}
                            <div class="ce-hit"><b class="tnum" class:na=move || !cache.with(|c| c.has_hit)>{move || cache.with(|c| if c.has_hit { format!("{}%", c.hit_text) } else { c.hit_text.clone() })}</b><span>"hit rate"</span></div>
                        </div>
                        <div class="ce-facts">
                            <div><span class="q2">"Estimated savings"</span>
                                <b class="tnum ce-big" class:na=move || !cache.with(|c| c.has_save)>{move || cache.with(|c| c.save_text.clone())}</b>
                                <span class="q3">"Cache reads priced at each model's cache-read rate instead of its input rate"</span></div>
                            <div><span class="q2">"Estimated cache cost"</span>
                                <b class="tnum ce-big" class:na=move || !cache.with(|c| c.has_ccost)>{move || cache.with(|c| c.ccost_text.clone())}</b>
                                <span class="q3">{move || cache.with(|c| if c.ccost_share.is_empty() { String::new() } else { format!("{}% of the estimated cost for this range", c.ccost_share) })}</span></div>
                        </div>
                    </div>
                    <p class="ax-minor">"Cache reads vs writes"</p>
                    <div class="rw-bar">
                        {move || cache.with(|c| {
                            let total = c.reads.w + c.writes.w;
                            let ww = if total > 0.0 && c.writes.w > 0.0 { (c.writes.w / total * 100.0).max(1.0) } else { 0.0 };
                            let rw = if total > 0.0 && c.reads.w > 0.0 { 100.0 - ww } else { 0.0 };
                            view! {
                                <i style=format!("width:{rw:.2}%;background:{}", key_color("cr"))></i>
                                <i style=format!("width:{ww:.2}%;background:{}", key_color("cw"))></i>
                            }
                        })}
                    </div>
                    <div class="rw-rows">
                        <div class="rw-row rw-head"><span></span><span class="cap r">"Tokens"</span><span class="cap r">"Share"</span><span class="cap r">"Est. cost"</span></div>
                        {move || cache.with(|c| [("cr", "Reads", c.reads.clone()), ("cw", "Writes", c.writes.clone())]).into_iter().map(|(k, name, side)| view! {
                            <div class="rw-row">
                                <span class="rw-name"><Swatch color=key_color(k)/><span>{name}</span>{(!side.note.is_empty()).then(|| view! { <span class="q3">{side.note.clone()}</span> })}</span>
                                <b class="tnum r" data-tip=side.tip.clone()>{side.tok.clone()}</b>
                                <span class="tnum r q2">{if side.share == "Unavailable" || side.share.is_empty() { side.share.clone() } else { format!("{}%", side.share) }}</span>
                                <b class="tnum r">{side.cost.clone()}</b>
                            </div>
                        }).collect_view()}
                    </div>
                    <p class="q3 ax-fine">{move || cache.with(|c| c.note.clone())}</p>
                </div>
            </div>
        </section>
    }
}
