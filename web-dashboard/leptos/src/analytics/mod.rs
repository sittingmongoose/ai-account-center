//! Analytics, drawn natively in the app frame (PLAN step 7). The frame reads its data itself: `public/frame-analytics.mjs`
//! keeps the page state, the reads and the timers and words every number with the same builders as bridge.js
//! (public/analytics-*.mjs, `ANALYTICS_VIEW_VERSION` 7). It sends the views (`feed.rs`: `analytics`, `analytics-motion`,
//! `analytics-head`, `analytics-loading`) and this page lays them out and sends the page's action kinds back
//! (`analytics-range`, `analytics-focus` ...). The charts that are laid out in pixels (the trend, daily cost and the
//! focus chart) report their boxes (`analytics-layout`), as the Slint page did. No Slint dashboard is loaded for it.

pub mod feed;
mod head;
mod models;
mod quota;
mod stats;
mod time;
mod trend;
pub mod view;

use crate::ui::charts::*;
use crate::ui::*;
use feed::Feed;
use leptos::prelude::*;
use std::sync::Arc;
use view::*;

/// What every card reads: the parsed view and the controller's way back.
#[derive(Clone, Copy)]
pub struct Ax {
    pub feed: Feed,
    pub view: Memo<Option<Arc<AnalyticsView>>>,
    /// The page has had a useful answer (numbers, or a settled failure): sticky until the view is cleared.
    pub ready: Memo<bool>,
    pub load: Memo<LoadState>,
    /// The header: the view's, then each `analytics-head` between refreshes.
    pub head: RwSignal<Head>,
}

impl Ax {
    /// Runs one of the page's action kinds (nothing is done until the controller has loaded).
    pub fn act(&self, kind: &str, value: &str) {
        self.feed.act(kind, value);
    }

    /// A part of the view, memoized so a card redraws only when its own part changed.
    pub fn part<T: Clone + Default + PartialEq + Send + Sync + 'static>(
        &self,
        pick: impl Fn(&AnalyticsView) -> T + Send + Sync + 'static,
    ) -> Memo<T> {
        let view = self.view;
        Memo::new(move |_| view.with(|v| v.as_deref().map(&pick).unwrap_or_default()))
    }
}

fn start(feed: Feed) -> Ax {
    let view = Memo::new(move |prev: Option<&Option<Arc<AnalyticsView>>>| {
        let json = feed.analytics.get();
        if json.is_empty() {
            return None;
        }
        // An unreadable push keeps what was on screen.
        parse(&json)
            .map(Arc::new)
            .or_else(|| prev.cloned().flatten())
    });
    let ready = Memo::new(move |prev: Option<&bool>| {
        view.with(|v| match v {
            None => false,
            Some(v) => prev.copied().unwrap_or(false) || v.usage.ready,
        })
    });
    let load = Memo::new(move |_| {
        let json = feed.loading.get();
        serde_json::from_str::<LoadState>(&json).unwrap_or_default()
    });
    let head = RwSignal::new(Head::default());
    Effect::new(move |_| {
        if let Some(h) = view.with(|v| v.as_ref().map(|v| v.usage.head.clone())) {
            head.set(h);
        }
    });
    Effect::new(move |_| {
        let json = feed.head.get();
        if let Ok(h) = serde_json::from_str::<Head>(&json) {
            head.set(h);
        }
    });
    Ax {
        feed,
        view,
        ready,
        load,
        head,
    }
}

/// A part this short is kept on one line; a longer one wraps inside itself.
const WHOLE_PART: usize = 40;

/// Text runs split into the parts between " · ": a line breaks between parts, never inside a short one, so no word
/// is left alone on a line ("639M" / "cache"). Strong runs bold; warn and crit tones coloured (calm is plain ink).
pub fn runs(list: Vec<Run>) -> impl IntoView {
    let mut groups: Vec<Vec<Run>> = vec![Vec::new()];
    for r in list {
        for (i, piece) in r.text.split(" · ").enumerate() {
            if i > 0 {
                groups.push(Vec::new());
            }
            if !piece.is_empty() {
                let last = groups.len() - 1;
                groups[last].push(Run {
                    text: piece.to_string(),
                    ..r.clone()
                });
            }
        }
    }
    let count = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(|(i, group)| {
            let whole = group.iter().map(|r| r.text.chars().count()).sum::<usize>() <= WHOLE_PART;
            let pieces = group
                .into_iter()
                .map(|r| {
                    let tone = match r.tone.as_str() {
                        "warn" | "crit" | "good" | "acc" => Some(r.tone.clone()),
                        "over" => Some("crit".to_string()),
                        _ => None,
                    };
                    if r.strong {
                        view! { <b data-tone=tone>{r.text}</b> }.into_any()
                    } else {
                        view! { <span data-tone=tone>{r.text}</span> }.into_any()
                    }
                })
                .collect_view();
            view! {
                <span class:nw=whole>{pieces}</span>
                {(i + 1 < count).then(|| " · ")}
            }
        })
        .collect_view()
}

/// Plain text split the same way (subtitles, notes).
pub fn parts(text: &str) -> impl IntoView + use<> {
    runs(vec![Run {
        text: text.to_string(),
        strong: false,
        tone: String::new(),
    }])
}

/// A card's head: title, subtitle (and an optional note) at the left, its tools at the right.
#[component]
pub fn CardHead(
    #[prop(into)] title: Signal<String>,
    #[prop(optional, into)] sub: Signal<String>,
    #[prop(optional, into)] note: Signal<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <div class="ax-head">
            <div class="ax-titles">
                <h2>{move || title.get()}</h2>
                <p class="ax-sub" hidden=move || sub.with(|s| s.is_empty())>{move || sub.with(|s| parts(s))}</p>
                <p class="ax-note" hidden=move || note.with(|s| s.is_empty())>{move || note.get()}</p>
            </div>
            {children.map(|c| view! { <div class="ax-tools">{c()}</div> })}
        </div>
    }
}

/// The Lucide icon a scope line names.
pub fn line_icon(name: &str) -> &'static str {
    match name {
        "terminal" => "terminal",
        "wallet" => "wallet",
        "users" => "users",
        "layers" => "layers",
        "clock" => "clock",
        _ => "info",
    }
}

#[component]
pub fn AnalyticsPage() -> impl IntoView {
    let feed = expect_context::<Feed>();
    let ax = start(feed);
    provide_context(ax);
    models::provide_detail();
    let error = Memo::new(move |_| ax.load.with(|l| l.error.clone()));
    view! {
        <TipLayer>
            <div class="page ax-page" aria-busy=move || ax.load.with(|l| l.loading).to_string()>
                <head::Header/>
                <p class="banner" role="status" hidden=move || error.with(|e| e.is_empty())>{move || error.get()}</p>
                // a failed first read is explained by the line above; the placeholders are for a read under way
                <Show when=move || ax.ready.get() fallback=move || error.with(|e| e.is_empty()).then(|| view! { <LoadingCards/> })>
                    <Kpis/>
                    <trend::TrendCard/>
                    <div class="ax-grid">
                        <models::CbmCard/>
                        <models::DonutCard/>
                        <stats::SessionsCard/>
                        <stats::RecentCard/>
                        <stats::TokensCacheCard/>
                        <time::HeatCard/>
                        <time::DailyCard/>
                    </div>
                    <quota::QuotaCard/>
                    <quota::AgendaCard/>
                    <models::DetailPopover/>
                </Show>
            </div>
        </TipLayer>
    }
}

/// Before the first useful answer: one row per computer saying how far its scan is, over calm placeholders.
#[component]
fn LoadingCards() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let hosts = ax.part(|v| v.usage.loading.hosts.clone());
    let rows = move || {
        let list = hosts.get();
        if list.is_empty() {
            ["Ubuntu", "Mac", "Windows", "Nas1"]
                .into_iter()
                .map(|n| LoadHost {
                    name: n.into(),
                    detail: "queued".into(),
                })
                .collect()
        } else {
            list
        }
    };
    view! {
        <section class="plate ax-card ax-loading">
            <CardHead title="Reading usage logs"
                sub="Reading usage logs on Ubuntu, Mac, Windows and Nas1. The page appears as soon as the first numbers land."/>
            <div class="ax-body">
                {move || rows().into_iter().map(|h| view! {
                    <div class="ld-host"><b>{h.name}</b><span class="tnum q3">{h.detail}</span></div>
                    <div class="skel"></div>
                }).collect_view()}
            </div>
        </section>
        <section class="plate ax-card ax-loading">
            <div class="ax-body"><div class="skel tall"></div><div class="skel"></div></div>
        </section>
    }
}

// ---------------------------------------------------------------- KPIs and tokens by provider

#[component]
fn Kpis() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let kpis = ax.part(|v| v.usage.kpis.clone());
    let apport_tip = ax.part(|v| v.usage.apport_tip.clone());
    let providers = ax.part(|v| v.usage.providers.clone());
    view! {
        <div class="kpis">
            {move || kpis.get().into_iter().map(|k| {
                let swatch = match k.swatch.as_str() {
                    "cost" => Some(view! { <Swatch color=key_color("cost") shape="line"/> }.into_any()),
                    "cache" => Some(view! { <Swatch color=key_color("cw") shape="half" color2=key_color("cr")/> }.into_any()),
                    "in" | "out" => Some(view! { <Swatch color=key_color(&k.swatch)/> }.into_any()),
                    _ => None,
                };
                let tip = apport_tip.get_untracked();
                view! {
                    <section class="plate kpi" data-key=k.key.clone()>
                        <div class="kpi-label">
                            {swatch}<span>{k.label}</span>
                            {k.apport.then(|| view! { <span class="info-dot" data-tip=tip tabindex="0" aria-label="How this is split"><Icon name="info"/></span> })}
                        </div>
                        <div class="kpi-val tnum" class:na=!k.has data-tip=(!k.tip.is_empty()).then_some(k.tip.clone())>{k.text}</div>
                        <div class="kpi-sub tnum">{runs(k.sub)}</div>
                    </section>
                }
            }).collect_view()}
        </div>
        <Show when=move || providers.with(|p| p.shown && !p.items.is_empty())>
            <p class="prov-line">
                <span class="cap">{move || providers.with(|p| p.label.clone())}</span>
                {move || {
                    let (items, note) = providers.with(|p| (p.items.clone(), p.note.clone()));
                    let n = items.len();
                    // the note stays on the last item's line
                    items.into_iter().enumerate().map(|(i, t)| view! {
                        <span class="prov-item" data-tip=t.tip>
                            {(!t.mark.is_empty()).then(|| view! { <ProviderMark provider=t.mark.clone() size=16/> })}
                            <span class="q2">{t.label}</span>
                            <b class="tnum" class:quiet=t.quiet>{t.value}</b>
                            {(i + 1 == n && !note.is_empty()).then(|| view! { <span class="q3 prov-note">{note.clone()}</span> })}
                        </span>
                    }).collect_view()
                }}
            </p>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/analytics-view.json"
        ))
        .expect("fixtures/analytics-view.json (node web-dashboard/leptos/dev/analytics-view.mjs)")
    }

    #[test]
    fn the_fixture_view_reads_whole() {
        let v = parse(&fixture()).expect("version 7 view");
        assert_eq!(v.version, ANALYTICS_VIEW_VERSION);
        assert!(v.usage.ready && v.usage.available);
        assert_eq!(v.usage.kpis.len(), 5);
        assert!(v.usage.trend.pw > 0.0 && !v.usage.trend.paths.total.is_empty());
        assert!(v.usage.trend.lut.len() as f64 >= (v.usage.trend.pw / 2.0).floor());
        assert!(!v.usage.cbm.rows.is_empty() && !v.usage.donut.segs.is_empty());
        assert_eq!(v.usage.heat.cells.len(), 7 * 24);
        assert!(!v.quota.groups.is_empty() && !v.agenda.a.is_empty());
        assert!(!v.usage.calendar.cells.is_empty());
    }

    #[test]
    fn another_version_or_garbage_is_not_drawn() {
        assert!(parse("").is_none());
        assert!(parse("{\"version\":6}").is_none());
        assert!(parse("not json").is_none());
        // nulls where text and lists are expected read as empty, never fail the view
        let v = parse("{\"version\":7,\"usage\":{\"kpis\":null,\"head\":{\"read\":null}},\"agenda\":{\"a\":null}}")
            .expect("nulls tolerated");
        assert!(v.usage.kpis.is_empty() && v.usage.head.read.is_empty() && v.agenda.a.is_empty());
    }

    #[test]
    fn scope_icons_fall_back_to_info() {
        assert_eq!(line_icon("wallet"), "wallet");
        assert_eq!(line_icon("anything"), "info");
    }
}
