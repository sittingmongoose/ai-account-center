//! The Analytics view, `ANALYTICS_VIEW_VERSION` 7, exactly as the classic app handed it to Slint (`analyticsSlintModel` in
//! public/analytics-data.mjs, with `usageView`, `quotaView` and `agendaView`). Every word, number and pixel position is
//! decided there; the page only lays it out. Text and lists read through `nullable` (a `null` is empty); geometry
//! numbers default to 0 when missing, and the page never shows a number it was not given as text.

use crate::dash::nullable;
use serde::Deserialize;

pub const ANALYTICS_VIEW_VERSION: u32 = 7;

/// A text run of a sentence (KPI sub-lines, the agenda's "now 27% used").
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Run {
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    pub strong: bool,
    #[serde(deserialize_with = "nullable")]
    pub tone: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AnalyticsView {
    pub version: u32,
    pub state: PageState,
    pub usage: Usage,
    pub quota: Quota,
    pub agenda: Agenda,
}

/// The page state bridge.js keeps (`analyticsPage`): the controls show it, the actions change it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct PageState {
    #[serde(deserialize_with = "nullable")]
    pub range: String,
    #[serde(deserialize_with = "nullable")]
    pub prov: String,
    pub split: bool,
    pub cache: bool,
    #[serde(deserialize_with = "nullable")]
    pub donut: String,
    #[serde(deserialize_with = "nullable")]
    pub heat: String,
    #[serde(deserialize_with = "nullable")]
    pub cbm_sort: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Usage {
    pub available: bool,
    #[serde(deserialize_with = "nullable")]
    pub status_note: String,
    pub cached: bool,
    pub ready: bool,
    pub loading: Loading,
    pub head: Head,
    #[serde(deserialize_with = "nullable")]
    pub scope: Vec<ScopeLine>,
    #[serde(deserialize_with = "nullable")]
    pub kpis: Vec<Kpi>,
    pub providers: Providers,
    pub picker: Picker,
    #[serde(deserialize_with = "nullable")]
    pub apport_tip: String,
    pub trend: Trend,
    pub cbm: Cbm,
    pub donut: Donut,
    pub sessions: Sessions,
    pub tokens: Tokens,
    pub cache: Cache,
    pub included: Included,
    pub heat: Heat,
    pub daily: Daily,
    pub calendar: Calendar,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Loading {
    #[serde(deserialize_with = "nullable")]
    pub hosts: Vec<LoadHost>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoadHost {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
}

/// The header line (also sent alone between refreshes as `analytics-head`, so "read 2m ago" ticks).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Head {
    #[serde(deserialize_with = "nullable")]
    pub scope: String,
    #[serde(deserialize_with = "nullable")]
    pub read: String,
    pub updating: bool,
    #[serde(deserialize_with = "nullable")]
    pub update_note: String,
    #[serde(deserialize_with = "nullable")]
    pub read_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub date: String,
    pub custom: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScopeLine {
    #[serde(deserialize_with = "nullable")]
    pub icon: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Kpi {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub swatch: String,
    pub has: bool,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: Vec<Run>,
    pub apport: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Providers {
    pub shown: bool,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub items: Vec<ProvItem>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProvItem {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub mark: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    pub quiet: bool,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Picker {
    #[serde(deserialize_with = "nullable")]
    pub items: Vec<PickItem>,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub mark: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct PickItem {
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub mark: String,
    #[serde(deserialize_with = "nullable")]
    pub tokens: String,
    pub off: bool,
}

/// The usage trend, laid out by bridge.js in the pixels of the box the page reported (`analytics-layout trend`):
/// `pw` x `ph` is the plot inside the fixed pads (`TREND_PAD`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Trend {
    pub w: f64,
    pub h: f64,
    pub pw: f64,
    pub ph: f64,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    pub cost_shown: bool,
    pub empty: bool,
    #[serde(deserialize_with = "nullable")]
    pub empty_text: String,
    #[serde(deserialize_with = "nullable")]
    pub y_ticks: Vec<YTick>,
    #[serde(deserialize_with = "nullable")]
    pub x_ticks: Vec<XTick>,
    #[serde(deserialize_with = "nullable")]
    pub minor: String,
    pub tail_x: f64,
    #[serde(deserialize_with = "nullable")]
    pub tail_label: String,
    #[serde(deserialize_with = "nullable")]
    pub hatch: String,
    #[serde(deserialize_with = "nullable")]
    pub clip_note: String,
    #[serde(deserialize_with = "nullable")]
    pub buckets: Vec<Bucket>,
    #[serde(deserialize_with = "nullable")]
    pub lut: Vec<i32>,
    #[serde(deserialize_with = "nullable")]
    pub legend: Vec<LegendItem>,
    pub paths: TrendPaths,
    /// Spans no computer was read for (plot-local px): drawn as gaps, never as zero.
    #[serde(deserialize_with = "nullable")]
    pub gaps: Vec<TrendGap>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TrendGap {
    pub x0: f64,
    pub x1: f64,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct YTick {
    pub y: f64,
    #[serde(deserialize_with = "nullable")]
    pub left: String,
    #[serde(deserialize_with = "nullable")]
    pub right: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub base: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct XTick {
    pub x: f64,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub major: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Bucket {
    pub x: f64,
    pub y_tok: f64,
    pub y_cost: f64,
    #[serde(deserialize_with = "nullable")]
    pub time: String,
    #[serde(deserialize_with = "nullable")]
    pub vin: String,
    #[serde(deserialize_with = "nullable")]
    pub vout: String,
    #[serde(deserialize_with = "nullable")]
    pub vcw: String,
    #[serde(deserialize_with = "nullable")]
    pub vcr: String,
    #[serde(deserialize_with = "nullable")]
    pub all: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    pub cr_dim: bool,
    #[serde(deserialize_with = "nullable")]
    pub by_provider: String,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
    #[serde(deserialize_with = "nullable")]
    pub foot2: String,
    /// No computer's logs cover the bucket: every value reads "Not read".
    pub gap: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LegendItem {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
}

/// The eleven trend paths, in plot pixels.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TrendPaths {
    #[serde(deserialize_with = "nullable")]
    pub total: String,
    #[serde(deserialize_with = "nullable")]
    pub total_line: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    #[serde(deserialize_with = "nullable")]
    pub band0: String,
    #[serde(deserialize_with = "nullable")]
    pub band1: String,
    #[serde(deserialize_with = "nullable")]
    pub band2: String,
    #[serde(deserialize_with = "nullable")]
    pub band3: String,
    #[serde(deserialize_with = "nullable")]
    pub line0: String,
    #[serde(deserialize_with = "nullable")]
    pub line1: String,
    #[serde(deserialize_with = "nullable")]
    pub line2: String,
    #[serde(deserialize_with = "nullable")]
    pub line3: String,
}

impl TrendPaths {
    /// The paths in drawing order: total, total line, cost, the four bands, the four band lines.
    pub fn all(&self) -> [&str; 11] {
        [
            &self.total,
            &self.total_line,
            &self.cost,
            &self.band0,
            &self.band1,
            &self.band2,
            &self.band3,
            &self.line0,
            &self.line1,
            &self.line2,
            &self.line3,
        ]
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cbm {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub sort: String,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<ModelRow>,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
    #[serde(deserialize_with = "nullable")]
    pub empty: String,
}

/// One model: the row in Cost by model and its detail.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelRow {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    pub idx: i32,
    /// The bar's length, percent of the widest.
    pub w: f64,
    pub fin: f64,
    pub fout: f64,
    pub fcw: f64,
    pub fcr: f64,
    #[serde(deserialize_with = "nullable")]
    pub tip_in: String,
    #[serde(deserialize_with = "nullable")]
    pub tip_out: String,
    #[serde(deserialize_with = "nullable")]
    pub tip_cw: String,
    #[serde(deserialize_with = "nullable")]
    pub tip_cr: String,
    #[serde(deserialize_with = "nullable")]
    pub tok: String,
    #[serde(deserialize_with = "nullable")]
    pub tok_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    pub cost_na: bool,
    pub partial: bool,
    #[serde(deserialize_with = "nullable")]
    pub share: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub usage: String,
    #[serde(deserialize_with = "nullable")]
    pub types: Vec<TypeRow>,
    #[serde(deserialize_with = "nullable")]
    pub io: String,
    #[serde(deserialize_with = "nullable")]
    pub io_note: String,
    #[serde(deserialize_with = "nullable")]
    pub io_quiet: String,
    #[serde(deserialize_with = "nullable")]
    pub rate: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct TypeRow {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub tok: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    pub w: f64,
    pub none: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Donut {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub mode: String,
    #[serde(deserialize_with = "nullable")]
    pub segs: Vec<Seg>,
    #[serde(deserialize_with = "nullable")]
    pub lut: Vec<i32>,
    #[serde(deserialize_with = "nullable")]
    pub legend: Vec<DonutLegend>,
    #[serde(deserialize_with = "nullable")]
    pub centre: String,
    #[serde(deserialize_with = "nullable")]
    pub centre_label: String,
    #[serde(deserialize_with = "nullable")]
    pub centre_short: String,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
    pub empty: bool,
    #[serde(deserialize_with = "nullable")]
    pub empty_text: String,
}

/// A donut arc: `a0`..`a1` in radians, clockwise from the top.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Seg {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    pub mix: f64,
    pub other: bool,
    #[serde(deserialize_with = "nullable")]
    pub model: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    pub idx: i32,
    pub a0: f64,
    pub a1: f64,
    #[serde(deserialize_with = "nullable")]
    pub share: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    #[serde(deserialize_with = "nullable")]
    pub value_tip: String,
    pub label: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct DonutLegend {
    pub seg: Seg,
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub open: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Sessions {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
    #[serde(deserialize_with = "nullable")]
    pub more_sub: String,
    #[serde(deserialize_with = "nullable")]
    pub stats: Vec<Stat>,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<SessProvider>,
    #[serde(deserialize_with = "nullable")]
    pub recent: Vec<SessRow>,
    #[serde(deserialize_with = "nullable")]
    pub recent_more: Vec<SessRow>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Stat {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub has: bool,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessProvider {
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub sessions: String,
    #[serde(deserialize_with = "nullable")]
    pub per: String,
    #[serde(deserialize_with = "nullable")]
    pub events: String,
    #[serde(deserialize_with = "nullable")]
    pub events_tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessRow {
    #[serde(deserialize_with = "nullable")]
    pub tool: String,
    #[serde(deserialize_with = "nullable")]
    pub models: String,
    #[serde(deserialize_with = "nullable")]
    pub tokens: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    #[serde(deserialize_with = "nullable")]
    pub when: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tokens {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<TokRow>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TokRow {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// Bar lengths in percent: the token share and the cost share.
    pub wt: f64,
    pub wc: f64,
    #[serde(deserialize_with = "nullable")]
    pub tok: String,
    #[serde(deserialize_with = "nullable")]
    pub tok_tip: String,
    #[serde(deserialize_with = "nullable")]
    pub tok_share: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    #[serde(deserialize_with = "nullable")]
    pub cost_share: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Cache {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    pub has_hit: bool,
    #[serde(deserialize_with = "nullable")]
    pub hit_text: String,
    pub has_save: bool,
    #[serde(deserialize_with = "nullable")]
    pub save_text: String,
    pub has_ccost: bool,
    #[serde(deserialize_with = "nullable")]
    pub ccost_text: String,
    #[serde(deserialize_with = "nullable")]
    pub ccost_share: String,
    pub reads: CacheSide,
    pub writes: CacheSide,
    /// 0..1, the gauge's fill.
    pub read_frac: f64,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct CacheSide {
    #[serde(deserialize_with = "nullable")]
    pub tok: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
    #[serde(deserialize_with = "nullable")]
    pub share: String,
    #[serde(deserialize_with = "nullable")]
    pub cost: String,
    pub w: f64,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Included {
    pub shown: bool,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub line: String,
    #[serde(deserialize_with = "nullable")]
    pub hosts: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<IncRow>,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct IncRow {
    #[serde(deserialize_with = "nullable")]
    pub tool: String,
    #[serde(deserialize_with = "nullable")]
    pub cells: Vec<IncCell>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct IncCell {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    #[serde(deserialize_with = "nullable")]
    pub tone: String,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Heat {
    #[serde(deserialize_with = "nullable")]
    pub mode: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    /// 7 x 24, Monday first: state 0 outside the logs, 1 nothing logged, 2 a value at intensity `k`, 3 cost not logged.
    #[serde(deserialize_with = "nullable")]
    pub cells: Vec<HeatCell>,
    #[serde(deserialize_with = "nullable")]
    pub days: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub hours: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub busiest: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct HeatCell {
    pub state: i32,
    pub k: f64,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Daily {
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub y_ticks: Vec<YTick>,
    #[serde(deserialize_with = "nullable")]
    pub bars: Vec<DailyBar>,
    pub empty: bool,
    #[serde(deserialize_with = "nullable")]
    pub empty_text: String,
    pub show_claude: bool,
    pub show_codex: bool,
    pub show_other: bool,
    #[serde(deserialize_with = "nullable")]
    pub claude_label: String,
    #[serde(deserialize_with = "nullable")]
    pub codex_label: String,
    #[serde(deserialize_with = "nullable")]
    pub other_label: String,
    #[serde(deserialize_with = "nullable")]
    pub other_hue: String,
}

/// One day (or hour): `h` is the bar's height in the plot (0..1), `cl` and `cx` the Claude and Codex parts of it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct DailyBar {
    pub h: f64,
    pub cl: f64,
    pub cx: f64,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub time: String,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<DailyRow>,
    #[serde(deserialize_with = "nullable")]
    pub total: String,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
    /// No computer was read for this bar's time: a gap, not a zero.
    pub gap: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct DailyRow {
    #[serde(deserialize_with = "nullable")]
    pub p: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
}

/// The custom range picker: whole local days back to the oldest stored day, one Monday-first grid per
/// month; `n` indexes every cell, `m` is a cell's month, `off` days can't be picked and `pad` cells fill
/// a month's first week.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Calendar {
    #[serde(deserialize_with = "nullable")]
    pub cells: Vec<Day>,
    #[serde(deserialize_with = "nullable")]
    pub months: Vec<CalMonth>,
    #[serde(deserialize_with = "nullable")]
    pub quick: Vec<CalQuick>,
    pub lo: i32,
    pub hi: i32,
    pub first_n: i32,
    pub last_n: i32,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct CalMonth {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub first: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct CalQuick {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub lo: i32,
    pub hi: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Day {
    #[serde(deserialize_with = "nullable")]
    pub day: String,
    pub n: i32,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub month: String,
    pub off: bool,
    pub today: bool,
    #[serde(deserialize_with = "nullable")]
    pub long: String,
    /// The month (index into `Calendar::months`) whose grid shows this cell.
    pub m: i32,
    pub pad: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Quota {
    #[serde(deserialize_with = "nullable")]
    pub sub: String,
    #[serde(deserialize_with = "nullable")]
    pub ticks: Vec<QTick>,
    #[serde(deserialize_with = "nullable")]
    pub groups: Vec<QGroup>,
    #[serde(deserialize_with = "nullable")]
    pub focus: Vec<Focus>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct QTick {
    pub pos: f64,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct QGroup {
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub count: u32,
    #[serde(deserialize_with = "nullable")]
    pub meta: String,
    pub collapsed: bool,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<QRow>,
}

/// One account's main window: a step sparkline in a 600 x 24 viewbox, one path per severity.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct QRow {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub active: bool,
    #[serde(deserialize_with = "nullable")]
    pub plan: String,
    #[serde(deserialize_with = "nullable")]
    pub win: String,
    pub has_value: bool,
    #[serde(deserialize_with = "nullable")]
    pub value_text: String,
    pub sev: i32,
    #[serde(deserialize_with = "nullable")]
    pub next: String,
    #[serde(deserialize_with = "nullable")]
    pub next_tip: String,
    pub open: bool,
    #[serde(deserialize_with = "nullable")]
    pub calm_line: String,
    #[serde(deserialize_with = "nullable")]
    pub warn_line: String,
    #[serde(deserialize_with = "nullable")]
    pub crit_line: String,
    #[serde(deserialize_with = "nullable")]
    pub over_line: String,
    #[serde(deserialize_with = "nullable")]
    pub calm_fill: String,
    #[serde(deserialize_with = "nullable")]
    pub warn_fill: String,
    #[serde(deserialize_with = "nullable")]
    pub crit_fill: String,
    #[serde(deserialize_with = "nullable")]
    pub over_fill: String,
    #[serde(deserialize_with = "nullable")]
    pub limit: String,
    #[serde(deserialize_with = "nullable")]
    pub na: String,
}

/// An open row's focus chart, laid out by bridge.js in the pixels of the reported focus box (`w` x `h`). Shapes and
/// labels name theme colours by key; labels were placed so they never overlap.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Focus {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    pub w: f64,
    pub h: f64,
    pub comparable: bool,
    pub compare: bool,
    #[serde(deserialize_with = "nullable")]
    pub cmp_label: String,
    #[serde(deserialize_with = "nullable")]
    pub empty: String,
    #[serde(deserialize_with = "nullable")]
    pub shapes: Vec<Shape>,
    #[serde(deserialize_with = "nullable")]
    pub labels: Vec<Label>,
    #[serde(deserialize_with = "nullable")]
    pub dots: Vec<Dot>,
    #[serde(deserialize_with = "nullable")]
    pub legend: Vec<FocusLegend>,
    #[serde(deserialize_with = "nullable")]
    pub stops: Vec<Stop>,
    #[serde(deserialize_with = "nullable")]
    pub lut: Vec<i32>,
    #[serde(deserialize_with = "nullable")]
    pub foot: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Shape {
    #[serde(deserialize_with = "nullable")]
    pub d: String,
    #[serde(deserialize_with = "nullable")]
    pub stroke: String,
    #[serde(deserialize_with = "nullable")]
    pub fill: String,
    pub sw: f64,
    pub op: f64,
    pub shade: i32,
    pub round: bool,
    pub layer: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Label {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    #[serde(deserialize_with = "nullable")]
    pub color: String,
    pub shade: i32,
    pub plate: bool,
    pub px: f64,
    pub py: f64,
    pub pw: f64,
    pub ph: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Dot {
    pub x: f64,
    pub y: f64,
    pub shade: i32,
    pub r: f64,
    #[serde(deserialize_with = "nullable")]
    pub fill: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct FocusLegend {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub shade: i32,
    pub dash: i32,
    pub width: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Stop {
    pub x: f64,
    #[serde(deserialize_with = "nullable")]
    pub time: String,
    pub proj: bool,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<StopRow>,
    pub dot_y: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct StopRow {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub value: String,
    pub shade: i32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Agenda {
    pub empty: bool,
    #[serde(deserialize_with = "nullable")]
    pub a: Vec<AgendaRow>,
    #[serde(deserialize_with = "nullable")]
    pub b: Vec<AgendaRow>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgendaRow {
    /// "day" (a day heading) or "line" (a reset or expiry).
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub day: String,
    #[serde(deserialize_with = "nullable")]
    pub hint: String,
    #[serde(deserialize_with = "nullable")]
    pub time: String,
    #[serde(deserialize_with = "nullable")]
    pub rel: String,
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub who: String,
    #[serde(deserialize_with = "nullable")]
    pub what: String,
    #[serde(deserialize_with = "nullable")]
    pub now: Vec<Run>,
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

/// `{"loading": bool, "error": "..."}` (`analytics-loading`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoadState {
    pub loading: bool,
    #[serde(deserialize_with = "nullable")]
    pub error: String,
}

/// Whether the numbers on screen are current (`analytics-fresh`). `stale`: they predate this visit's newest answer
/// (kept in this browser or by the server while it computes the next); `updating`: newer ones are on their way. `note`
/// says it in one short line ("Updating · 3 of 11 tools · data from 2m ago"); nothing is shown when it is empty.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Fresh {
    pub shown: bool,
    pub updating: bool,
    pub stale: bool,
    /// The last read, or the server's last computation, failed: nothing newer is on its way.
    pub failed: bool,
    pub as_of: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    /// The whole sentence behind the short `note` (the strip's tip).
    #[serde(deserialize_with = "nullable")]
    pub tip: String,
}

/// The view, or None for an empty, unreadable or other-version push (the page then shows what it had).
pub fn parse(json: &str) -> Option<AnalyticsView> {
    if json.is_empty() {
        return None;
    }
    let view: AnalyticsView = serde_json::from_str(json).ok()?;
    (view.version == ANALYTICS_VIEW_VERSION).then_some(view)
}
