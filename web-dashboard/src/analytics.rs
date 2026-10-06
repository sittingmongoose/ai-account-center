//! Analytics view model (version 7, public/analytics-data.mjs `analyticsSlintModel`): the header, the KPI
//! row, the usage charts (trend, cost by model, donut, sessions, token breakdown, cache efficiency,
//! heatmap, daily cost), the custom range calendar, the quota history with its focus charts and the
//! resets agenda. Everything lands in the `AxData` global (ui/pages/analytics/ax-data.slint).
//!
//! Lists whose rows animate when a value changes (KPIs, model bars, donut arcs, stats, token bars,
//! heatmap cells, daily bars, quota groups and rows) are persistent models updated in place, so a
//! changed reading counts up, grows or morphs from its previous value instead of re-mounting.
use crate::sync::{Nested, sync_rows};
use crate::{
    AnalyticsHeadView, AxAgendaRow, AxBar, AxBarRow, AxBucket, AxCache, AxCalendar, AxCardText,
    AxDaily, AxData, AxDay, AxDonut, AxDonutLeg, AxDonutSeg, AxDot, AxFocus, AxFocusLegend, AxHeat,
    AxHeatCell, AxIncluded, AxKpi, AxLabel, AxLegendItem, AxLoadHost, AxModelRow, AxModelType,
    AxPickItem, AxProvItem, AxProvLine, AxProvSummary, AxQuotaGroup, AxQuotaRow, AxScopeLine,
    AxSessRecent, AxSessRow, AxShape, AxSrcCell, AxSrcRow, AxStat, AxStop, AxStopRow, AxTick,
    AxTokRow, AxTrend, AxTrendPaths, AxXTick, AxYTick, Dashboard, RunView,
};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use wasm_bindgen::JsValue;

/// The analytics view-model version this build understands (public/analytics-data.mjs ANALYTICS_VIEW_VERSION).
pub const ANALYTICS_VIEW_VERSION: u64 = 7;

// ---------------------------------------------------------------- JSON access (camelCase keys)
static NULL: Value = Value::Null;
fn g<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}
fn s(v: &Value, k: &str) -> SharedString {
    g(v, k).as_str().unwrap_or("").into()
}
fn f(v: &Value, k: &str) -> f32 {
    g(v, k)
        .as_f64()
        .filter(|x| x.is_finite())
        .map(|x| x as f32)
        .unwrap_or(0.)
}
fn b(v: &Value, k: &str) -> bool {
    g(v, k).as_bool().unwrap_or(false)
}
fn i(v: &Value, k: &str) -> i32 {
    g(v, k)
        .as_f64()
        .filter(|x| x.is_finite())
        .map(|x| x.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32)
        .unwrap_or(0)
}
fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    g(v, k).as_array().map(|a| a.as_slice()).unwrap_or(&[])
}
fn rows_of<T: Clone + 'static>(v: &Value, k: &str, each: impl Fn(&Value) -> T) -> Vec<T> {
    arr(v, k).iter().map(each).collect()
}
fn list<T: Clone + 'static>(v: &Value, k: &str, each: impl Fn(&Value) -> T) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows_of(v, k, each)))
}
fn ints(v: &Value, k: &str) -> Vec<i32> {
    rows_of(v, k, |x| x.as_f64().map(|n| n as i32).unwrap_or(-1))
}
fn strings(v: &Value, k: &str) -> Vec<SharedString> {
    rows_of(v, k, |x| x.as_str().unwrap_or("").into())
}

/// Make `model` equal to `rows` position by position (cells and bars have no ids; their count is stable).
fn sync_by_index<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if index < model.row_count() {
            if model.row_data(index).as_ref() != Some(&row) {
                model.set_row_data(index, row);
            }
        } else {
            model.push(row);
        }
    }
    while model.row_count() > count {
        model.remove(model.row_count() - 1);
    }
}

// ---------------------------------------------------------------- conversions
fn run(v: &Value) -> RunView {
    RunView {
        text: s(v, "text"),
        strong: b(v, "strong"),
        tone: s(v, "tone"),
    }
}
fn kpi(v: &Value) -> AxKpi {
    AxKpi {
        key: s(v, "key"),
        label: s(v, "label"),
        swatch: s(v, "swatch"),
        num: f(v, "num"),
        has: b(v, "has"),
        fmt: s(v, "fmt"),
        text: s(v, "text"),
        tip: s(v, "tip"),
        sub: list(v, "sub", run),
        apport: b(v, "apport"),
    }
}
fn trend(v: &Value, generation: i32) -> AxTrend {
    AxTrend {
        w: f(v, "w"),
        h: f(v, "h"),
        pw: f(v, "pw"),
        ph: f(v, "ph"),
        sub: s(v, "sub"),
        split: b(v, "split"),
        cache: b(v, "cache"),
        cost_shown: b(v, "costShown"),
        empty: b(v, "empty"),
        empty_text: s(v, "emptyText"),
        minor: s(v, "minor"),
        tail_x: g(v, "tailX").as_f64().map(|x| x as f32).unwrap_or(-1.),
        tail_label: s(v, "tailLabel"),
        hatch: s(v, "hatch"),
        clip_note: s(v, "clipNote"),
        generation,
    }
}
fn trend_paths(v: &Value) -> AxTrendPaths {
    AxTrendPaths {
        total: s(v, "total"),
        total_line: s(v, "totalLine"),
        band0: s(v, "band0"),
        band1: s(v, "band1"),
        band2: s(v, "band2"),
        band3: s(v, "band3"),
        line0: s(v, "line0"),
        line1: s(v, "line1"),
        line2: s(v, "line2"),
        line3: s(v, "line3"),
        cost: s(v, "cost"),
    }
}
fn bucket(v: &Value) -> AxBucket {
    AxBucket {
        x: f(v, "x"),
        y_tok: f(v, "yTok"),
        y_cost: f(v, "yCost"),
        time: s(v, "time"),
        vin: s(v, "vin"),
        vout: s(v, "vout"),
        vcw: s(v, "vcw"),
        vcr: s(v, "vcr"),
        all: s(v, "all"),
        cost: s(v, "cost"),
        tin: s(v, "tin"),
        tout: s(v, "tout"),
        tcw: s(v, "tcw"),
        tcr: s(v, "tcr"),
        tall: s(v, "tall"),
        cr_dim: b(v, "crDim"),
        by_provider: s(v, "byProvider"),
        foot: s(v, "foot"),
        foot2: s(v, "foot2"),
    }
}
fn model_row(v: &Value) -> AxModelRow {
    AxModelRow {
        key: s(v, "key"),
        name: s(v, "name"),
        provider: s(v, "provider"),
        idx: i(v, "idx"),
        w: f(v, "w"),
        fin: f(v, "fin"),
        fout: f(v, "fout"),
        fcw: f(v, "fcw"),
        fcr: f(v, "fcr"),
        tip_in: s(v, "tipIn"),
        tip_out: s(v, "tipOut"),
        tip_cw: s(v, "tipCw"),
        tip_cr: s(v, "tipCr"),
        tok: s(v, "tok"),
        tok_tip: s(v, "tokTip"),
        cost: s(v, "cost"),
        cost_na: b(v, "costNa"),
        partial: b(v, "partial"),
        share: s(v, "share"),
        sub: s(v, "sub"),
        usage: s(v, "usage"),
        types: list(v, "types", |t| AxModelType {
            key: s(t, "key"),
            label: s(t, "label"),
            tok: s(t, "tok"),
            tip: s(t, "tip"),
            cost: s(t, "cost"),
            w: f(t, "w"),
            none: b(t, "none"),
        }),
        io: s(v, "io"),
        io_note: s(v, "ioNote"),
        io_quiet: s(v, "ioQuiet"),
        rate: s(v, "rate"),
    }
}
fn card_text(v: &Value) -> AxCardText {
    AxCardText {
        sub: s(v, "sub"),
        note: s(v, "note"),
        foot: s(v, "foot"),
        empty: s(v, "empty"),
    }
}
fn donut_seg(v: &Value) -> AxDonutSeg {
    AxDonutSeg {
        key: s(v, "key"),
        name: s(v, "name"),
        provider: s(v, "provider"),
        mix: f(v, "mix"),
        other: b(v, "other"),
        model: s(v, "model"),
        tip: s(v, "tip"),
        a0: f(v, "a0"),
        a1: f(v, "a1"),
        share: s(v, "share"),
        value: s(v, "value"),
        value_tip: s(v, "valueTip"),
        label: b(v, "label"),
        idx: i(v, "idx"),
    }
}
fn donut_leg(v: &Value) -> AxDonutLeg {
    AxDonutLeg {
        seg: donut_seg(g(v, "seg")),
        kind: s(v, "kind"),
        open: b(v, "open"),
        arc: g(v, "arc").as_f64().map(|n| n as i32).unwrap_or(-1),
    }
}
fn stat(v: &Value) -> AxStat {
    AxStat {
        key: s(v, "key"),
        label: s(v, "label"),
        num: f(v, "num"),
        has: b(v, "has"),
        fmt: s(v, "fmt"),
        text: s(v, "text"),
    }
}
fn sess_recent(v: &Value) -> AxSessRecent {
    AxSessRecent {
        tool: s(v, "tool"),
        models: s(v, "models"),
        tokens: s(v, "tokens"),
        cost: s(v, "cost"),
        when: s(v, "when"),
        tip: s(v, "tip"),
    }
}
fn sess_row(v: &Value) -> AxSessRow {
    AxSessRow {
        provider: s(v, "provider"),
        label: s(v, "label"),
        sessions: s(v, "sessions"),
        per: s(v, "per"),
        events: s(v, "events"),
        events_tip: s(v, "eventsTip"),
    }
}
fn cache(v: &Value) -> AxCache {
    let r = g(v, "reads");
    let w = g(v, "writes");
    AxCache {
        sub: s(v, "sub"),
        hit: f(v, "hit"),
        has_hit: b(v, "hasHit"),
        hit_text: s(v, "hitText"),
        save: f(v, "save"),
        has_save: b(v, "hasSave"),
        save_text: s(v, "saveText"),
        ccost: f(v, "ccost"),
        has_ccost: b(v, "hasCcost"),
        ccost_text: s(v, "ccostText"),
        ccost_share: s(v, "ccostShare"),
        r_tok: s(r, "tok"),
        r_tip: s(r, "tip"),
        r_share: s(r, "share"),
        r_cost: s(r, "cost"),
        w_tok: s(w, "tok"),
        w_tip: s(w, "tip"),
        w_share: s(w, "share"),
        w_cost: s(w, "cost"),
        w_note: s(w, "note"),
        read_frac: f(v, "readFrac"),
        reads: f(r, "w"),
        writes: f(w, "w"),
    }
}
fn shape(v: &Value) -> AxShape {
    AxShape {
        d: s(v, "d"),
        stroke: s(v, "stroke"),
        fill: s(v, "fill"),
        sw: f(v, "sw"),
        op: f(v, "op"),
        shade: i(v, "shade"),
        round: b(v, "round"),
        layer: i(v, "layer"),
    }
}
fn label(v: &Value) -> AxLabel {
    AxLabel {
        x: f(v, "x"),
        y: f(v, "y"),
        w: f(v, "w"),
        text: s(v, "text"),
        color: s(v, "color"),
        shade: i(v, "shade"),
        plate: b(v, "plate"),
        px: f(v, "px"),
        py: f(v, "py"),
        pw: f(v, "pw"),
        ph: f(v, "ph"),
    }
}
fn focus(v: &Value) -> AxFocus {
    AxFocus {
        id: s(v, "id"),
        provider: s(v, "provider"),
        w: f(v, "w"),
        h: f(v, "h"),
        comparable: b(v, "comparable"),
        compare: b(v, "compare"),
        cmp_label: s(v, "cmpLabel"),
        empty: s(v, "empty"),
        foot: s(v, "foot"),
        shapes: list(v, "shapes", shape),
        labels: list(v, "labels", label),
        dots: list(v, "dots", |d| AxDot {
            x: f(d, "x"),
            y: f(d, "y"),
            r: f(d, "r"),
            shade: i(d, "shade"),
            fill: s(d, "fill"),
        }),
        legend: list(v, "legend", |l| AxFocusLegend {
            name: s(l, "name"),
            kind: s(l, "kind"),
            shade: i(l, "shade"),
            dash: i(l, "dash"),
            width: f(l, "width"),
        }),
        stops: list(v, "stops", |st| AxStop {
            x: f(st, "x"),
            time: s(st, "time"),
            proj: b(st, "proj"),
            rows: list(st, "rows", |r| AxStopRow {
                name: s(r, "name"),
                value: s(r, "value"),
                shade: i(r, "shade"),
            }),
            dot_y: g(st, "dotY").as_f64().map(|x| x as f32).unwrap_or(-1.),
        }),
        lut: ModelRc::new(VecModel::from(ints(v, "lut"))),
        plot_l: f(g(v, "plot"), "l"),
        plot_t: f(g(v, "plot"), "t"),
        plot_w: f(g(v, "plot"), "w"),
        plot_h: f(g(v, "plot"), "h"),
    }
}
fn quota_row(v: &Value, focus_of: &dyn Fn(&str) -> Option<AxFocus>) -> AxQuotaRow {
    let id = s(v, "id");
    AxQuotaRow {
        focus: focus_of(id.as_str()).unwrap_or_default(),
        id,
        provider: s(v, "provider"),
        label: s(v, "label"),
        active: b(v, "active"),
        plan: s(v, "plan"),
        win: s(v, "win"),
        has_value: b(v, "hasValue"),
        value: f(v, "value"),
        value_text: s(v, "valueText"),
        sev: i(v, "sev"),
        next: s(v, "next"),
        next_tip: s(v, "nextTip"),
        open: b(v, "open"),
        calm_line: s(v, "calmLine"),
        warn_line: s(v, "warnLine"),
        crit_line: s(v, "critLine"),
        over_line: s(v, "overLine"),
        calm_fill: s(v, "calmFill"),
        warn_fill: s(v, "warnFill"),
        crit_fill: s(v, "critFill"),
        over_fill: s(v, "overFill"),
        limit: s(v, "limit"),
        na: s(v, "na"),
        points: i(v, "points"),
    }
}
fn agenda_row(v: &Value) -> AxAgendaRow {
    AxAgendaRow {
        kind: s(v, "kind"),
        day: s(v, "day"),
        hint: s(v, "hint"),
        time: s(v, "time"),
        rel: s(v, "rel"),
        provider: s(v, "provider"),
        who: s(v, "who"),
        what: s(v, "what"),
        tip: s(v, "tip"),
        now: list(v, "now", run),
        i: i(v, "i"),
    }
}

// ---------------------------------------------------------------- persistent models
pub struct AnalyticsModels {
    kpis: Rc<VecModel<AxKpi>>,
    models: Rc<VecModel<AxModelRow>>,
    donut: Rc<VecModel<AxDonutSeg>>,
    stats: Rc<VecModel<AxStat>>,
    tokens: Rc<VecModel<AxTokRow>>,
    heat: Rc<VecModel<AxHeatCell>>,
    bars: Rc<VecModel<AxBar>>,
    groups: Rc<VecModel<AxQuotaGroup>>,
    group_rows: Nested<AxQuotaRow>,
    agenda_a: Rc<VecModel<AxAgendaRow>>,
    agenda_b: Rc<VecModel<AxAgendaRow>>,
    scope: Rc<VecModel<AxScopeLine>>,
    included_hosts: Rc<VecModel<SharedString>>,
    included_rows: Rc<VecModel<AxSrcRow>>,
    load_hosts: Rc<VecModel<AxLoadHost>>,
    src_cells: Nested<AxSrcCell>,
    prov_lines: Rc<VecModel<AxProvLine>>,
    prov_items: Nested<AxProvItem>,
    picker_items: Rc<VecModel<AxPickItem>>,
    trend_y: Rc<VecModel<AxYTick>>,
    trend_x: Rc<VecModel<AxXTick>>,
    trend_legend: Rc<VecModel<AxLegendItem>>,
    trend_buckets: Rc<VecModel<AxBucket>>,
    trend_lut: Rc<VecModel<i32>>,
    donut_lut: Rc<VecModel<i32>>,
    donut_legend: Rc<VecModel<AxDonutLeg>>,
    sess_rows: Rc<VecModel<AxSessRow>>,
    sess_recent: Rc<VecModel<AxSessRecent>>,
    sess_recent_more: Rc<VecModel<AxSessRecent>>,
    daily_y: Rc<VecModel<AxYTick>>,
    heat_hours: Rc<VecModel<SharedString>>,
    days: Rc<VecModel<AxDay>>,
    quota_ticks: Rc<VecModel<AxTick>>,
    trend_gen: i32,
    trend_key: String,
}

impl Default for AnalyticsModels {
    fn default() -> Self {
        Self {
            kpis: Rc::new(VecModel::default()),
            models: Rc::new(VecModel::default()),
            donut: Rc::new(VecModel::default()),
            stats: Rc::new(VecModel::default()),
            tokens: Rc::new(VecModel::default()),
            heat: Rc::new(VecModel::default()),
            bars: Rc::new(VecModel::default()),
            groups: Rc::new(VecModel::default()),
            group_rows: Nested::default(),
            agenda_a: Rc::new(VecModel::default()),
            agenda_b: Rc::new(VecModel::default()),
            scope: Rc::new(VecModel::default()),
            included_hosts: Rc::new(VecModel::default()),
            included_rows: Rc::new(VecModel::default()),
            load_hosts: Rc::new(VecModel::default()),
            src_cells: Nested::default(),
            prov_lines: Rc::new(VecModel::default()),
            prov_items: Nested::default(),
            picker_items: Rc::new(VecModel::default()),
            trend_y: Rc::new(VecModel::default()),
            trend_x: Rc::new(VecModel::default()),
            trend_legend: Rc::new(VecModel::default()),
            trend_buckets: Rc::new(VecModel::default()),
            trend_lut: Rc::new(VecModel::default()),
            donut_lut: Rc::new(VecModel::default()),
            donut_legend: Rc::new(VecModel::default()),
            sess_rows: Rc::new(VecModel::default()),
            sess_recent: Rc::new(VecModel::default()),
            sess_recent_more: Rc::new(VecModel::default()),
            daily_y: Rc::new(VecModel::default()),
            heat_hours: Rc::new(VecModel::default()),
            days: Rc::new(VecModel::default()),
            quota_ticks: Rc::new(VecModel::default()),
            trend_gen: 0,
            trend_key: String::new(),
        }
    }
}

pub fn bind(ui: &Dashboard, m: &AnalyticsModels) {
    let ax = ui.global::<AxData>();
    ax.set_kpis(ModelRc::from(m.kpis.clone()));
    ax.set_models(ModelRc::from(m.models.clone()));
    ax.set_donut_segs(ModelRc::from(m.donut.clone()));
    ax.set_sess_stats(ModelRc::from(m.stats.clone()));
    ax.set_tokens(ModelRc::from(m.tokens.clone()));
    ax.set_heat_cells(ModelRc::from(m.heat.clone()));
    ax.set_daily_bars(ModelRc::from(m.bars.clone()));
    ax.set_quota_groups(ModelRc::from(m.groups.clone()));
    ax.set_agenda_a(ModelRc::from(m.agenda_a.clone()));
    ax.set_agenda_b(ModelRc::from(m.agenda_b.clone()));
    ax.set_scope(ModelRc::from(m.scope.clone()));
    ax.set_included_hosts(ModelRc::from(m.included_hosts.clone()));
    ax.set_included_rows(ModelRc::from(m.included_rows.clone()));
    ax.set_load_hosts(ModelRc::from(m.load_hosts.clone()));
    ax.set_prov_lines(ModelRc::from(m.prov_lines.clone()));
    ax.set_picker_items(ModelRc::from(m.picker_items.clone()));
    ax.set_trend_y(ModelRc::from(m.trend_y.clone()));
    ax.set_trend_x(ModelRc::from(m.trend_x.clone()));
    ax.set_trend_legend(ModelRc::from(m.trend_legend.clone()));
    ax.set_trend_buckets(ModelRc::from(m.trend_buckets.clone()));
    ax.set_trend_lut(ModelRc::from(m.trend_lut.clone()));
    ax.set_donut_lut(ModelRc::from(m.donut_lut.clone()));
    ax.set_donut_legend(ModelRc::from(m.donut_legend.clone()));
    ax.set_sess_rows(ModelRc::from(m.sess_rows.clone()));
    ax.set_sess_recent(ModelRc::from(m.sess_recent.clone()));
    ax.set_sess_recent_more(ModelRc::from(m.sess_recent_more.clone()));
    ax.set_daily_y(ModelRc::from(m.daily_y.clone()));
    ax.set_heat_hours(ModelRc::from(m.heat_hours.clone()));
    ax.set_days(ModelRc::from(m.days.clone()));
    ax.set_quota_ticks(ModelRc::from(m.quota_ticks.clone()));
}

fn head(v: &Value, previous: AnalyticsHeadView) -> AnalyticsHeadView {
    AnalyticsHeadView {
        loading: previous.loading,
        updating: b(v, "updating"),
        error: previous.error,
        scope: s(v, "scope"),
        read: s(v, "read"),
        read_tip: s(v, "readTip"),
        update_note: s(v, "updateNote"),
        date: s(v, "date"),
        custom: b(v, "custom"),
    }
}

/// The header alone (the "read 2m ago" line ticks between refreshes).
pub fn set_head(ui: &Dashboard, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid analytics header"))?;
    let ax = ui.global::<AxData>();
    ax.set_head(head(&v, ax.get_head()));
    Ok(())
}

/// One frame of the trend morph: only the eleven path strings change.
pub fn set_trend_paths(ui: &Dashboard, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid trend paths"))?;
    ui.global::<AxData>().set_trend_paths(trend_paths(&v));
    Ok(())
}

pub fn set_loading(ui: &Dashboard, loading: bool, error: &str) {
    let ax = ui.global::<AxData>();
    let mut h = ax.get_head();
    h.loading = loading;
    h.error = error.into();
    ax.set_head(h);
}

pub fn set_analytics(ui: &Dashboard, m: &mut AnalyticsModels, json: &str) -> Result<(), JsValue> {
    let v: Value =
        serde_json::from_str(json).map_err(|_| JsValue::from_str("Invalid analytics view data"))?;
    if g(&v, "version").as_u64() != Some(ANALYTICS_VIEW_VERSION) {
        return Err(JsValue::from_str(
            "Unsupported analytics view-model version",
        ));
    }
    let ax = ui.global::<AxData>();
    let state = g(&v, "state");
    ax.set_range(s(state, "range"));
    ax.set_prov(s(state, "prov"));
    ax.set_split(b(state, "split"));
    ax.set_cache_on(b(state, "cache"));
    ax.set_donut_mode(s(state, "donut"));
    ax.set_heat_mode(s(state, "heat"));
    ax.set_cbm_sort(if s(state, "cbmSort") == "tokens" {
        "tokens".into()
    } else {
        "cost".into()
    });

    let usage = g(&v, "usage");
    ax.set_head(head(g(usage, "head"), ax.get_head()));
    ax.set_available(b(usage, "available"));
    ax.set_status_note(s(usage, "statusNote"));
    ax.set_apport_tip(s(usage, "apportTip"));
    sync_by_index(
        &m.scope,
        rows_of(usage, "scope", |l| AxScopeLine {
            icon: s(l, "icon"),
            text: s(l, "text"),
        }),
    );
    // included usage: where the numbers come from (activity.sources), never a division of them
    let inc = g(usage, "included");
    ax.set_included(AxIncluded {
        shown: b(inc, "shown"),
        label: s(inc, "label"),
        line: s(inc, "line"),
        foot: s(inc, "foot"),
    });
    sync_by_index(&m.included_hosts, strings(inc, "hosts"));
    let mut live_src = Vec::new();
    let mut src_rows = Vec::new();
    for (n, r) in arr(inc, "rows").iter().enumerate() {
        let tool = s(r, "tool");
        // the tool names are not guaranteed unique, so the position keeps the owners apart
        let owner = format!("{n}#{tool}");
        live_src.push(owner.clone());
        let cells = m.src_cells.sync(
            &owner,
            rows_of(r, "cells", |c| AxSrcCell {
                id: s(c, "id"),
                text: s(c, "text"),
                tone: s(c, "tone"),
                tip: s(c, "tip"),
            }),
            // keyed by the cell's host, never by its text: a state flip ("Scanning…" to
            // "Read 1m ago") updates the cell in place instead of re-creating it
            |c: &AxSrcCell| c.id.clone(),
        );
        src_rows.push(AxSrcRow { tool, cells });
    }
    sync_rows(&m.included_rows, src_rows, |r: &AxSrcRow| r.tool.clone());
    m.src_cells.retain(&live_src);
    // the held loading screen's per-host scan progress
    let loading = g(usage, "loading");
    sync_by_index(
        &m.load_hosts,
        rows_of(loading, "hosts", |h| AxLoadHost {
            name: s(h, "name"),
            detail: s(h, "detail"),
        }),
    );
    sync_rows(
        &m.kpis,
        arr(usage, "kpis").iter().map(kpi).collect(),
        |k: &AxKpi| k.key.clone(),
    );
    // tokens by provider: a summary, packed into lines by the view model; never a section
    let provs = g(usage, "providers");
    ax.set_prov_summary(AxProvSummary {
        shown: b(provs, "shown"),
        label: s(provs, "label"),
        note: s(provs, "note"),
    });
    let mut live_lines = Vec::new();
    let mut prov_lines = Vec::new();
    for (n, l) in arr(provs, "lines").iter().enumerate() {
        // the packed lines have no id of their own, so their position owns the items model
        let owner = n.to_string();
        live_lines.push(owner.clone());
        let items = m.prov_items.sync(
            &owner,
            rows_of(l, "items", |t| AxProvItem {
                key: s(t, "key"),
                label: s(t, "label"),
                mark: s(t, "mark"),
                value: s(t, "value"),
                tip: s(t, "tip"),
                quiet: b(t, "quiet"),
            }),
            |t: &AxProvItem| t.key.clone(),
        );
        prov_lines.push(AxProvLine {
            items,
            first: b(l, "first"),
            last: b(l, "last"),
        });
    }
    sync_by_index(&m.prov_lines, prov_lines);
    m.prov_items.retain(&live_lines);
    // the provider picker: All, then every provider with usage in the range
    let picker = g(usage, "picker");
    sync_rows(
        &m.picker_items,
        rows_of(picker, "items", |p| AxPickItem {
            value: s(p, "value"),
            label: s(p, "label"),
            mark: s(p, "mark"),
            tokens: s(p, "tokens"),
            off: b(p, "off"),
        }),
        |p: &AxPickItem| p.value.clone(),
    );
    ax.set_picker_label(s(picker, "label"));
    ax.set_picker_mark(s(picker, "mark"));

    // trend: the axes cross-fade when the geometry changes (gen); bridge.js may animate the paths
    let t = g(usage, "trend");
    let key = format!(
        "{}|{}|{}|{}",
        g(t, "yTicks"),
        g(t, "xTicks"),
        g(t, "pw"),
        g(t, "ph")
    );
    if key != m.trend_key {
        m.trend_key = key;
        m.trend_gen += 1;
    }
    ax.set_trend(trend(t, m.trend_gen));
    if let Some(paths) = t.get("paths") {
        ax.set_trend_paths(trend_paths(paths));
    }
    sync_by_index(
        &m.trend_y,
        rows_of(t, "yTicks", |y| AxYTick {
            y: f(y, "y"),
            left: s(y, "left"),
            right: s(y, "right"),
            base: b(y, "base"),
        }),
    );
    sync_by_index(
        &m.trend_x,
        rows_of(t, "xTicks", |x| AxXTick {
            x: f(x, "x"),
            label: s(x, "label"),
            major: b(x, "major"),
        }),
    );
    sync_rows(
        &m.trend_legend,
        rows_of(t, "legend", |l| AxLegendItem {
            key: s(l, "key"),
            label: s(l, "label"),
            note: s(l, "note"),
        }),
        |l: &AxLegendItem| l.key.clone(),
    );
    sync_by_index(&m.trend_buckets, rows_of(t, "buckets", bucket));
    sync_by_index(&m.trend_lut, ints(t, "lut"));

    // cost by model and the donut
    let cbm = g(usage, "cbm");
    ax.set_cbm(card_text(cbm));
    sync_rows(
        &m.models,
        arr(cbm, "rows").iter().map(model_row).collect(),
        |r: &AxModelRow| r.key.clone(),
    );
    let donut = g(usage, "donut");
    ax.set_donut(AxDonut {
        sub: s(donut, "sub"),
        note: s(donut, "note"),
        mode: s(donut, "mode"),
        centre: s(donut, "centre"),
        centre_label: s(donut, "centreLabel"),
        unit: s(donut, "unit"),
        empty: b(donut, "empty"),
        empty_text: s(donut, "emptyText"),
    });
    sync_rows(
        &m.donut,
        arr(donut, "segs").iter().map(donut_seg).collect(),
        |r: &AxDonutSeg| r.key.clone(),
    );
    sync_by_index(&m.donut_lut, ints(donut, "lut"));
    sync_rows(
        &m.donut_legend,
        rows_of(donut, "legend", donut_leg),
        |l: &AxDonutLeg| l.seg.key.clone(),
    );

    // sessions, tokens, cache
    let sessions = g(usage, "sessions");
    ax.set_sessions(card_text(sessions));
    ax.set_sess_more_sub(s(sessions, "moreSub"));
    sync_rows(
        &m.stats,
        arr(sessions, "stats").iter().map(stat).collect(),
        |r: &AxStat| r.key.clone(),
    );
    // the per-provider rows, keyed by provider, then one continued table across two boxes: the five
    // most recent sessions, then the next twenty
    sync_rows(
        &m.sess_rows,
        rows_of(sessions, "rows", sess_row),
        |r: &AxSessRow| r.provider.clone(),
    );
    sync_by_index(&m.sess_recent, rows_of(sessions, "recent", sess_recent));
    sync_by_index(
        &m.sess_recent_more,
        rows_of(sessions, "recentMore", sess_recent),
    );
    let tokens = g(usage, "tokens");
    ax.set_tokens_sub(s(tokens, "sub"));
    sync_rows(
        &m.tokens,
        arr(tokens, "rows")
            .iter()
            .map(|r| AxTokRow {
                key: s(r, "key"),
                label: s(r, "label"),
                wt: f(r, "wt"),
                wc: f(r, "wc"),
                tok: s(r, "tok"),
                tok_tip: s(r, "tokTip"),
                tok_share: s(r, "tokShare"),
                cost: s(r, "cost"),
                cost_share: s(r, "costShare"),
            })
            .collect(),
        |r: &AxTokRow| r.key.clone(),
    );
    ax.set_cache(cache(g(usage, "cache")));

    // heatmap and daily cost
    let heat = g(usage, "heat");
    ax.set_heat(AxHeat {
        mode: s(heat, "mode"),
        sub: s(heat, "sub"),
        busiest: s(heat, "busiest"),
        dash: s(heat, "dash"),
    });
    sync_by_index(&m.heat_hours, strings(heat, "hours"));
    sync_by_index(
        &m.heat,
        arr(heat, "cells")
            .iter()
            .map(|c| AxHeatCell {
                state: i(c, "state"),
                k: f(c, "k"),
                tip: s(c, "tip"),
            })
            .collect(),
    );
    let daily = g(usage, "daily");
    ax.set_daily(AxDaily {
        title: s(daily, "title"),
        sub: s(daily, "sub"),
        empty: b(daily, "empty"),
        empty_text: s(daily, "emptyText"),
        show_claude: b(daily, "showClaude"),
        show_codex: b(daily, "showCodex"),
        show_other: b(daily, "showOther"),
        claude_label: s(daily, "claudeLabel"),
        codex_label: s(daily, "codexLabel"),
        other_label: s(daily, "otherLabel"),
        other_hue: s(daily, "otherHue"),
    });
    sync_by_index(
        &m.daily_y,
        rows_of(daily, "yTicks", |y| AxYTick {
            y: f(y, "y"),
            left: s(y, "label"),
            right: SharedString::default(),
            base: b(y, "base"),
        }),
    );
    sync_by_index(
        &m.bars,
        arr(daily, "bars")
            .iter()
            .map(|r| AxBar {
                h: f(r, "h"),
                cl: f(r, "cl"),
                cx: f(r, "cx"),
                label: s(r, "label"),
                time: s(r, "time"),
                rows: list(r, "rows", |x| AxBarRow {
                    p: s(x, "p"),
                    label: s(x, "label"),
                    value: s(x, "value"),
                }),
                total: s(r, "total"),
                foot: s(r, "foot"),
            })
            .collect(),
    );

    // custom range calendar
    let cal = g(usage, "calendar");
    ax.set_calendar(AxCalendar {
        lo: i(cal, "lo"),
        hi: i(cal, "hi"),
        first_n: i(cal, "firstN"),
        last_n: i(cal, "lastN"),
        note: s(cal, "note"),
    });
    sync_by_index(
        &m.days,
        rows_of(cal, "cells", |d| AxDay {
            day: s(d, "day"),
            n: i(d, "n"),
            label: s(d, "label"),
            month: s(d, "month"),
            off: b(d, "off"),
            today: b(d, "today"),
            long: s(d, "long"),
        }),
    );

    // quota history and the focus charts of open rows
    let quota = g(&v, "quota");
    ax.set_quota_sub(s(quota, "sub"));
    sync_by_index(
        &m.quota_ticks,
        rows_of(quota, "ticks", |t| AxTick {
            pos: f(t, "pos"),
            label: s(t, "label"),
        }),
    );
    let focus_list = arr(quota, "focus");
    let focus_of = |id: &str| {
        focus_list
            .iter()
            .find(|x| g(x, "id").as_str() == Some(id))
            .map(focus)
    };
    let mut live = Vec::new();
    let mut groups = Vec::new();
    for group in arr(quota, "groups") {
        let provider = s(group, "provider");
        live.push(provider.to_string());
        let rows = m.group_rows.sync(
            provider.as_str(),
            arr(group, "rows")
                .iter()
                .map(|r| quota_row(r, &focus_of))
                .collect(),
            |r: &AxQuotaRow| r.id.clone(),
        );
        groups.push(AxQuotaGroup {
            provider,
            label: s(group, "label"),
            count: i(group, "count"),
            meta: s(group, "meta"),
            collapsed: b(group, "collapsed"),
            rows,
        });
    }
    sync_rows(&m.groups, groups, |gr: &AxQuotaGroup| gr.provider.clone());
    m.group_rows.retain(&live);

    // resets and expiries: in place, so the agenda redraws only when its rows change
    let agenda = g(&v, "agenda");
    ax.set_agenda_empty(b(agenda, "empty"));
    sync_by_index(
        &m.agenda_a,
        arr(agenda, "a").iter().map(agenda_row).collect(),
    );
    sync_by_index(
        &m.agenda_b,
        arr(agenda, "b").iter().map(agenda_row).collect(),
    );
    // The page arms only on an answer the view model calls useful (numbers to show, or a settled
    // failure the header explains): a cold answer that is still converging stays behind the
    // loading screen instead of drawing "Unavailable" cards that later pop into numbers.
    if b(usage, "ready") {
        ax.set_ready(true);
    }
    Ok(())
}
