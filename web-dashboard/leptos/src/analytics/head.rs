//! The Analytics header: the scope line with its read time, "What this page covers" and "Included usage" (which logs
//! on which computers are in the numbers), the range presets, the custom range calendar and the provider picker.
//! Refresh is the frame's own button.

use super::view::*;
use super::{Ax, line_icon};

fn ranges() -> Vec<(String, String)> {
    RANGES
        .iter()
        .map(|(id, l)| (id.to_string(), l.to_string()))
        .collect()
}
use crate::ui::charts::*;
use crate::ui::*;
use leptos::prelude::*;

const RANGES: [(&str, &str); 6] = [
    ("24h", "24H"),
    ("7d", "7D"),
    ("30d", "30D"),
    ("90d", "90D"),
    ("month", "Month"),
    ("all", "All"),
];

#[derive(Clone, Copy, PartialEq)]
enum Panel {
    None,
    Scope,
    Included,
}

#[component]
pub fn Header() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let head = ax.head;
    let included = ax.part(|v| v.usage.included.clone());
    let scope = ax.part(|v| v.usage.scope.clone());
    let state = ax.part(|v| v.state.clone());
    let panel = RwSignal::new(Panel::None);
    let toggle = move |p: Panel| panel.update(|cur| *cur = if *cur == p { Panel::None } else { p });
    let range = Signal::derive(move || {
        let s = state.with(|s| s.range.clone());
        if s.is_empty() { "7d".to_string() } else { s }
    });
    view! {
        <div class="page-head ax-top">
            <div class="ax-lead">
                <h1 class="page-title">"Analytics"</h1>
                <p class="headline ax-scope">
                    <span>{move || head.with(|h| h.scope.trim_end().to_string())}</span>" "
                    <b class="tnum" data-tip=move || head.with(|h| h.read_tip.clone())>{move || head.with(|h| h.read.clone())}</b>
                    // whether it is updating is said by the strip under the header (mod.rs FreshStrip), not here
                </p>
                <div class="ax-discl">
                    <button class="disclosure" class:open=move || panel.get() == Panel::Scope aria-expanded=move || (panel.get() == Panel::Scope).to_string()
                        on:click=move |_| toggle(Panel::Scope)><Icon name="chev-right"/>"What this page covers"</button>
                    <Show when=move || included.with(|i| i.shown)>
                        <button class="disclosure" class:open=move || panel.get() == Panel::Included aria-expanded=move || (panel.get() == Panel::Included).to_string()
                            data-probe="included-usage" on:click=move |_| toggle(Panel::Included)>
                            <Icon name="chev-right"/><span>{move || included.with(|i| i.label.clone())}</span>
                        </button>
                    </Show>
                </div>
            </div>
            <div class="page-tools ax-tools-row">
                <Segmented options=ranges()
                    selected=range label="Range" on_select=Callback::new(move |v: String| ax.act("analytics-range", &v))/>
                <DateButton/>
                <ProviderPicker/>
            </div>
        </div>
        <Show when=move || panel.get() == Panel::Scope>
            <section class="plate ax-panel">
                <h3>"What this page covers"</h3>
                <ul class="scope-lines">
                    {move || scope.get().into_iter().map(|l| view! {
                        <li><Icon name=line_icon(&l.icon)/><span>{l.text}</span></li>
                    }).collect_view()}
                </ul>
            </section>
        </Show>
        <Show when=move || panel.get() == Panel::Included>
            <IncludedPanel included=included/>
        </Show>
    }
}

/// Which tools' logs on which computers are in these numbers, with each source's state and last scan. A grid on
/// wide screens; grouped by tool where the registered computer columns would not fit whole.
#[component]
fn IncludedPanel(included: Memo<Included>) -> impl IntoView {
    let detail = RwSignal::new(String::new());
    let tone = |t: &str| match t {
        "ok" => "q2",
        "cached" => "w-warn",
        "unavailable" => "w-crit",
        "scanning" => "w-acc",
        _ => "q4",
    };
    view! {
        <section class="plate ax-panel inc-panel">
            <h3>"Included usage"</h3>
            <p class="q2">{move || included.with(|i| i.line.clone())}</p>
            <div class="inc-grid" style=move || format!("--cols:{}", included.with(|i| i.hosts.len().max(1)))>
                <span></span>
                {move || included.with(|i| i.hosts.clone()).into_iter().map(|h| view! { <span class="cap">{h}</span> }).collect_view()}
                {move || included.with(|i| i.rows.clone()).into_iter().map(|r| view! {
                    <b class="inc-tool">{r.tool}</b>
                    {r.cells.into_iter().map(|c| {
                        let tip = c.tip.clone();
                        view! {
                            <button class=format!("inc-cell tnum {}", tone(&c.tone)) data-tip=c.tip.clone()
                                on:click=move |_| detail.set(tip.clone()) on:pointerenter={let t = c.tip.clone(); move |_| detail.set(t.clone())}>
                                <span class="inc-host">{c.id.clone()}</span>{c.text}
                            </button>
                        }
                    }).collect_view()}
                }).collect_view()}
            </div>
            <p class="q2 inc-detail">{move || { let d = detail.get(); if d.is_empty() { "Point at or press a source to read its details.".to_string() } else { d } }}</p>
            <p class="q3 inc-foot">{move || included.with(|i| i.foot.clone())}</p>
        </section>
    }
}

/// The month of cell `n` (the grid that shows it), or the last month.
fn month_of(c: &Calendar, n: i32) -> i32 {
    c.cells
        .get(n.max(0) as usize)
        .filter(|_| n >= 0)
        .map(|d| d.m)
        .unwrap_or(c.months.len() as i32 - 1)
        .max(0)
}

/// The custom range: whole local days back to the oldest stored day, one month at a time; a range can
/// start in one month and end in another.
#[component]
fn DateButton() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let cal = ax.part(|v| v.usage.calendar.clone());
    let custom = Memo::new(move |_| ax.head.with(|h| h.custom));
    let date = Memo::new(move |_| ax.head.with(|h| h.date.clone()));
    let open = RwSignal::new(false);
    let anchor = NodeRef::<leptos::html::Button>::new();
    let lo = RwSignal::new(-1i32);
    let hi = RwSignal::new(-1i32);
    let hover = RwSignal::new(-1i32);
    let month = RwSignal::new(0i32);
    // each opening starts from the page's range, on the month of its last day
    Effect::new(move |_| {
        if open.get() {
            let c = cal.get_untracked();
            lo.set(c.lo);
            hi.set(if c.hi >= c.lo { c.hi } else { -1 });
            hover.set(-1);
            month.set(month_of(&c, if c.hi >= 0 { c.hi } else { c.last_n }));
        }
    });
    let month_label = move || {
        cal.with(|c| {
            c.months
                .get(month.get().max(0) as usize)
                .map(|m| m.label.clone())
                .unwrap_or_default()
        })
    };
    let span = move || {
        let (l, h, v) = (lo.get(), hi.get(), hover.get());
        let a = if h < 0 && v >= 0 { l.min(v) } else { l };
        let b = if h >= 0 {
            h
        } else if v >= 0 {
            l.max(v)
        } else {
            l
        };
        (a, b)
    };
    let long = move |n: i32| {
        cal.with(|c| {
            c.cells
                .get(n.max(0) as usize)
                .map(|d| d.long.clone())
                .unwrap_or_default()
        })
    };
    let sentence = move || {
        let (l, h) = (lo.get(), hi.get());
        if l < 0 {
            "Pick a first day".to_string()
        } else if h < 0 {
            format!("{} · pick a last day, or apply one day", long(l))
        } else {
            let n = h - l + 1;
            format!(
                "{} to {} · {n} day{}",
                long(l),
                long(h),
                if n == 1 { "" } else { "s" }
            )
        }
    };
    let pick = move |n: i32| {
        let (l, h) = (lo.get_untracked(), hi.get_untracked());
        if h >= 0 || l < 0 {
            lo.set(n);
            hi.set(-1);
        } else if n < l {
            hi.set(l);
            lo.set(n);
        } else {
            hi.set(n);
        }
        hover.set(-1);
    };
    let apply = move |_| {
        let (l, h) = (lo.get_untracked(), hi.get_untracked());
        if l < 0 {
            return;
        }
        let h = if h >= 0 { h } else { l };
        let days = cal.with_untracked(|c| {
            let d = |n: i32| {
                c.cells
                    .get(n as usize)
                    .map(|d| d.day.clone())
                    .unwrap_or_default()
            };
            (d(l), d(h))
        });
        if !days.0.is_empty() && !days.1.is_empty() {
            ax.act("analytics-custom", &format!("{},{}", days.0, days.1));
        }
        open.set(false);
    };
    let quick = move |l: i32, h: i32| {
        lo.set(l);
        hi.set(h);
        hover.set(-1);
        month.set(month_of(&cal.get_untracked(), h));
    };
    view! {
        <PopButton open=open node_ref=anchor class="date-btn" tip="Pick days within the logs that were read">
            <Icon name="calendar"/><span class="tnum" class:on=move || custom.get()>{move || date.get()}</span>
        </PopButton>
        <Popover open=open anchor=anchor width=340 label="Custom range">
            <div class="cal">
                <h3>"Custom range"</h3>
                <p class="q3 cal-note">{move || cal.with(|c| c.note.clone())}</p>
                <div class="cal-quick">
                    {move || cal.get().quick.into_iter().map(|q| {
                        let (l, h) = (q.lo, q.hi);
                        view! { <button class="btn sm" on:click=move |_| quick(l, h)>{q.label}</button> }
                    }).collect_view()}
                </div>
                <div class="cal-nav">
                    // braced: an unbraced closure with a comparison would end the tag early in `view!`
                    <button class="btn ghost sm icon-only" aria-label="Previous month" disabled={move || month.get() <= 0}
                        on:click={move |_| month.update(|m| *m = (*m - 1).max(0))}><Icon name="chev-left"/></button>
                    <b class="cal-month">{month_label}</b>
                    <button class="btn ghost sm icon-only" aria-label="Next month" disabled={move || month.get() >= cal.with(|c| c.months.len() as i32) - 1}
                        on:click={move |_| { let last = cal.with_untracked(|c| c.months.len() as i32) - 1; month.update(|m| *m = (*m + 1).min(last)) }}><Icon name="chev-right"/></button>
                </div>
                <div class="cal-grid" on:pointerleave=move |_| hover.set(-1)>
                    {["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].into_iter().map(|w| view! { <span class="cap cal-wd">{w}</span> }).collect_view()}
                    {move || { let shown = month.get(); cal.get().cells.into_iter().filter(move |d| d.m == shown).map(|d| {
                        let i = d.n;
                        if d.pad {
                            return view! { <span class="cal-pad"></span> }.into_any();
                        }
                        let off = d.off;
                        let today = d.today;
                        let inside = move || { let (a, b) = span(); !off && lo.get() >= 0 && i >= a && i <= b };
                        let end = move || { let (a, b) = span(); !off && lo.get() >= 0 && (i == a || i == b) };
                        let first = move || span().0 == i;
                        let last = move || span().1 == i;
                        view! {
                            <button class="cal-day tnum" disabled=off class:in=inside class:end=end class:first=first class:last=last class:today=today
                                aria-label=d.long.clone() on:click=move |_| pick(d.n)
                                on:pointerenter=move |_| { if hi.get_untracked() < 0 && !off { hover.set(i) } }>
                                {(!d.month.is_empty()).then(|| view! { <small>{d.month.clone()}</small> })}
                                {d.label}
                            </button>
                        }.into_any()
                    }).collect_view() }}
                </div>
                <div class="cal-foot">
                    <span class="tnum q2">{sentence}</span>
                    <div class="cal-acts">
                        <button class="btn ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                        <button class="btn primary" disabled={move || lo.get() < 0} on:click=apply>"Apply"</button>
                    </div>
                </div>
            </div>
        </Popover>
    }
}

/// The provider picker: All and every provider that served usage in the range, each with its tokens.
#[component]
fn ProviderPicker() -> impl IntoView {
    let ax = expect_context::<Ax>();
    let picker = ax.part(|v| v.usage.picker.clone());
    let prov = ax.part(|v| v.state.prov.clone());
    let open = RwSignal::new(false);
    let anchor = NodeRef::<leptos::html::Button>::new();
    view! {
        <PopButton open=open node_ref=anchor class="prov-btn" tip="Show one provider's usage, by model">
            {move || picker.with(|p| (!p.mark.is_empty()).then(|| view! { <ProviderMark provider=p.mark.clone() size=16/> }))}
            <span data-probe="analytics-provider">{move || picker.with(|p| p.label.clone())}</span>
        </PopButton>
        <Popover open=open anchor=anchor width=320 label="Provider">
            <div class="pick-list">
                <div class="pick-head"><span class="cap">"Provider"</span><span class="cap">"Tokens"</span></div>
                {move || picker.get().items.into_iter().map(|it| {
                    let value = it.value.clone();
                    let v = it.value.clone();
                    let selected = Signal::derive(move || prov.with(|p| *p == v || (p.is_empty() && v == "all")));
                    view! {
                        <button class="pick-row" class:sel=selected class:off=it.off
                            on:click=move |_| { open.set(false); ax.act("analytics-provider", &value); }>
                            <span class="pick-mark">{(!it.mark.is_empty()).then(|| view! { <ProviderMark provider=it.mark.clone() size=16/> })}</span>
                            <span class="pick-label">{it.label}</span>
                            <span class="tnum q3">{it.tokens}</span>
                            <span class="pick-check">{move || selected.get().then(|| view! { <Icon name="check"/> })}</span>
                        </button>
                    }
                }).collect_view()}
            </div>
        </Popover>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ninety_days_sits_between_thirty_days_and_month() {
        let ids: Vec<&str> = RANGES.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["24h", "7d", "30d", "90d", "month", "all"]);
        assert_eq!(RANGES[3].1, "90D");
    }

    #[test]
    fn a_picked_day_opens_its_own_month() {
        let cal: Calendar = serde_json::from_str(
            r#"{"cells":[{"day":"","n":0,"m":0,"pad":true,"off":true},{"day":"2026-08-31","n":1,"m":0},
               {"day":"2026-09-01","n":2,"m":1}],"months":[{"label":"August 2026","first":0},{"label":"September 2026","first":2}],
               "quick":[{"label":"Today","lo":2,"hi":2}],"lo":1,"hi":2,"firstN":1,"lastN":2,"note":""}"#,
        )
        .unwrap();
        assert_eq!(month_of(&cal, 1), 0);
        assert_eq!(month_of(&cal, 2), 1);
        // no picked day: the last month
        assert_eq!(month_of(&cal, -1), 1);
        assert!(cal.cells[0].pad);
        assert_eq!(cal.quick[0].hi, 2);
    }
}
