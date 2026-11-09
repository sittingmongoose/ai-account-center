//! The spend-by-stream composer: the `spendByStream` field of `GET /api/status`
//! (CONTRACT-spend-bymodel.md section 1), every word decided here. It is a line-for-line
//! port of the Leptos page builder's reference (`reference/spend-reference.mjs`, golden
//! pair `spend-input.json` -> `spend-by-stream.json`); where this comment and the golden
//! output disagree, the golden output wins.
//!
//! A pure function over the `SpendView` the TypeScript server already builds
//! (`buildSpend` in src/web-server/status/status-view-quota.ts) plus `nowMs`,
//! `utcOffsetMinutes` and `accountOrder`. Nothing is recomputed from raw reports, and an
//! unknown stays unknown: a share that was not read is `{ share: null, "not read" }`,
//! never a guessed figure. With no accepted quota report (`periods` empty) the whole
//! field is the contract's unavailable shape.
//!
//! The period `start` the foot's "since" clause names is quota-watch's calendar start of
//! the period (qw_core.py `period_bounds`: local midnight for the day, local Monday for
//! the week), not the first reading; the reference and the golden output keep the clause,
//! so this port keeps it too.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// The fixed sentence for every way the field can be unavailable (no report, no module,
/// input this port refuses): the contract's unavailable shape, never a guessed figure.
pub const UNAVAILABLE_SENTENCE: &str =
    "Spend by stream is unavailable: quota-watch has not written a report this server can read.";

// ---------------------------------------------------------------------------------------------
// Input (the SpendView the TypeScript side builds, plus the clock facts)

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendInput {
    pub now_ms: i64,
    /// The dashboard time zone's UTC offset in minutes at `now_ms`, for clock times.
    pub utc_offset_minutes: i64,
    /// The SpendView's accounts by first appearance in the agent-usage snapshot.
    #[serde(default)]
    pub account_order: Vec<String>,
    pub spend: SpendIn,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendIn {
    /// Key always present; null when no report was ever read.
    pub as_of: Option<String>,
    pub method_id: Option<String>,
    pub method: Option<String>,
    pub periods: Vec<PeriodIn>,
    pub budgets: Vec<BudgetIn>,
    /// Only how many there are is used; the rows themselves stay the quota panel's.
    pub unattributed: Vec<Json>,
    pub machines_not_scanned: Vec<MachineIn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodIn {
    pub id: String,
    /// SpendView's own label, e.g. "Today", "This week", "Last 6 hours".
    pub label: String,
    pub start: Option<String>,
    pub coverage: Option<f64>,
    pub streams: Vec<StreamIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamIn {
    pub stream: String,
    pub label: String,
    pub bucket: bool,
    pub rows: Vec<RowIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RowIn {
    pub account: String,
    pub meter: String,
    pub unit: String,
    /// Rounded by `buildSpend`; null when that account meter was not read.
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetIn {
    pub stream_label: String,
    pub over_limit: bool,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineIn {
    pub name: String,
    pub runs_missed: Option<i64>,
    pub runs_in_window: Option<i64>,
}

// ---------------------------------------------------------------------------------------------
// Output (the view the route passes through as the `spendByStream` field)

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendByStream {
    pub state: &'static str,
    pub unavailable: Option<String>,
    pub default_period: Option<String>,
    pub as_of: Option<String>,
    pub as_of_text: Option<String>,
    pub periods: Vec<PeriodOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodOut {
    pub id: String,
    pub label: String,
    pub lead: Vec<Run>,
    pub summary: Vec<Run>,
    pub columns: Vec<ColumnOut>,
    pub credits: Option<CreditsHead>,
    pub rows: Vec<RowOut>,
    pub key: Vec<String>,
    pub foot: String,
    pub more: Option<String>,
}

/// A run of the sentence type `/api/status` already uses. Only the fields the reference
/// sets are ever set (`strong` for a bolded figure, `tone` for a coloured word).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Run {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strong: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<&'static str>,
}

fn run(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        strong: None,
        tone: None,
    }
}

fn strong(text: impl Into<String>) -> Run {
    Run {
        text: text.into(),
        strong: Some(true),
        tone: None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnOut {
    pub id: String,
    pub account: String,
    pub meter: String,
    pub label: String,
    pub account_label: String,
    pub short: String,
    /// The part before the colon; the page's chart hue only.
    pub hue: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsHead {
    pub label: &'static str,
    pub account_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowOut {
    pub stream: String,
    pub label: String,
    pub bucket: bool,
    /// One per column, same order; null where the stream used nothing of that limit.
    pub cells: Vec<Option<Cell>>,
    pub credits: Vec<CreditsCell>,
    /// Phone bars: column indexes, in order.
    pub block_order: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cell {
    pub share: Option<f64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsCell {
    pub account: String,
    pub account_label: String,
    pub text: String,
}

// ---------------------------------------------------------------------------------------------
// Words

fn provider(account: &str) -> &str {
    account.split(':').next().unwrap_or(account)
}

fn account_label(account: &str) -> &str {
    match account.find(':') {
        Some(at) => &account[at + 1..],
        None => "",
    }
}

fn prov(account: &str) -> String {
    let id = provider(account);
    match id {
        "claude" => "Claude".to_string(),
        "codex" => "Codex".to_string(),
        other => capitalize(other),
    }
}

fn is_credits(meter: &str) -> bool {
    meter == "credits_balance"
}

fn meter_word(meter: &str) -> String {
    match meter {
        "seven_day" => "weekly".to_string(),
        "five_hour" => "5-hour".to_string(),
        "seven_day_opus" => "Opus weekly".to_string(),
        "seven_day_sonnet" => "Sonnet weekly".to_string(),
        other => other.replace('_', " "),
    }
}

fn is_weekly(meter: &str) -> bool {
    meter.starts_with("seven_day")
}

fn is_five(meter: &str) -> bool {
    meter == "five_hour"
}

/// JavaScript's Math.round for the non-negative figures this view uses: half rounds up.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// JavaScript's `toFixed(places)` for non-negative values: the integer n whose n/10^places
/// is closest to the value (ties to the larger n), with the decimals kept ("0.0", not "0").
/// The comparison divides, as the ECMAScript spec does — multiplying instead turns 4.85
/// into "4.9" where every JavaScript engine prints "4.8".
fn to_fixed(value: f64, places: u32) -> String {
    let scale = 10f64.powi(places as i32);
    let low = (value * scale).floor();
    let below = (low / scale - value).abs();
    let above = ((low + 1.0) / scale - value).abs();
    let units = (if above <= below { low + 1.0 } else { low }) as i64;
    let whole = units / 10i64.pow(places);
    let part = units % 10i64.pow(places);
    format!("{whole}.{part:0places$}", places = places as usize)
}

fn pct_text(share: f64) -> String {
    let p = share * 100.0;
    if p < 1.0 {
        format!("{}%", to_fixed(p, 1))
    } else {
        format!("{}%", js_round(p) as i64)
    }
}

fn cell_of(share: Option<f64>, meter: &str) -> Cell {
    match share {
        None => Cell {
            share: None,
            text: "not read".to_string(),
        },
        Some(share) if is_five(meter) && share >= 1.0 => Cell {
            share: Some(share),
            text: format!("{}×", to_fixed(share, 1)),
        },
        Some(share) => Cell {
            share: Some(share),
            text: pct_text(share),
        },
    }
}

/// A whole number with thousands commas, as `toLocaleString('en-US')` writes it.
fn thousands(value: i64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn credits_text(value: f64) -> String {
    thousands(js_round(value) as i64)
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The machine labels a sentence uses, as `pinned.rs::machine_label` writes them.
fn machine_name(machine: &str) -> String {
    match machine {
        "vm" => "the VM".to_string(),
        "mac" => "the Mac".to_string(),
        "windows" => "Windows".to_string(),
        "nas1" => "Nas1".to_string(),
        other => other.to_string(),
    }
}

// The dashboard-zone clock: epoch math over the UTC offset the caller passed, so no
// time-zone database is needed and the words match the TypeScript clock exactly.

/// Milliseconds since the epoch from an ISO-8601 moment; None when it is not one (then no
/// clock time is invented; the field stays null).
fn parse_iso(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let year: i64 = text[0..4].parse().ok()?;
    let month: i64 = text[5..7].parse().ok()?;
    let day: i64 = text[8..10].parse().ok()?;
    let hour: i64 = text[11..13].parse().ok()?;
    let minute: i64 = text[14..16].parse().ok()?;
    let second: i64 = text[17..19].parse().ok()?;
    let mut rest = &text[19..];
    let mut milli = 0i64;
    if rest.starts_with('.') {
        let digits: String = rest[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let scaled = format!("{digits:0<3}");
        milli = scaled[..3].parse().unwrap_or(0);
        rest = &rest[1 + digits.len()..];
    }
    let offset_minutes = if rest.starts_with('Z') || rest.is_empty() {
        0
    } else {
        let digits = rest.strip_prefix(['+', '-'])?;
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let hh: i64 = digits.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let mm: i64 = if digits.len() >= 5 {
            digits[3..5].parse().unwrap_or(0)
        } else {
            0
        };
        sign * (hh * 60 + mm)
    };
    // Days from the civil calendar (Howard Hinnant's days_from_civil), proleptic Gregorian:
    // months count March..February so a year's leap day lands at its end.
    let (y, m) = (
        if month <= 2 { year - 1 } else { year },
        if month <= 2 { month + 9 } else { month - 3 },
    );
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let epoch_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    Some(epoch_seconds * 1000 + milli)
}

/// (year, month, day) of a moment shifted into the dashboard's zone.
fn civil(shifted_ms: i64) -> (i64, i64, i64) {
    let days = shifted_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "1:12 PM" in the dashboard's zone, from the UTC offset the caller passed.
fn clock_text(epoch_ms: i64, offset_minutes: i64) -> String {
    let shifted = epoch_ms + offset_minutes * 60_000;
    let minute = shifted.div_euclid(60_000) % 60;
    let hour = shifted.div_euclid(3_600_000) % 24;
    let (hour, half) = match hour {
        0 => (12, "AM"),
        12 => (12, "PM"),
        hour if hour < 12 => (hour, "AM"),
        hour => (hour - 12, "PM"),
    };
    format!("{hour}:{minute:02} {half}")
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// The weekday of a moment shifted into the dashboard's zone (0 is Sunday).
fn weekday(shifted_ms: i64) -> usize {
    let days = shifted_ms.div_euclid(86_400_000);
    (days + 4).rem_euclid(7) as usize
}

/// "1:12 PM" the same local day, "early Friday" / "Friday afternoon" / "Friday evening" on
/// an earlier one: how the foot names the moment quota-watch's watching began.
fn since_phrase(iso: &str, now_ms: i64, offset_minutes: i64) -> Option<String> {
    let epoch = parse_iso(iso)?;
    let shifted = epoch + offset_minutes * 60_000;
    let now_shifted = now_ms + offset_minutes * 60_000;
    if civil(shifted) == civil(now_shifted) {
        return Some(clock_text(epoch, offset_minutes));
    }
    let hour = shifted.div_euclid(3_600_000) % 24;
    let day = DAYS[weekday(shifted)];
    Some(if hour < 12 {
        format!("early {day}")
    } else if hour < 18 {
        format!("{day} afternoon")
    } else {
        format!("{day} evening")
    })
}

// ---------------------------------------------------------------------------------------------
// The composer

/// One period's view: the table, its order and its sentences. A port of the reference's
/// `periodView`, decision for decision.
fn period_view(period: &PeriodIn, input: &SpendInput, spend: &SpendIn) -> PeriodOut {
    let noun = match period.id.as_str() {
        "day" => "day",
        "week" => "week",
        _ => "window",
    };
    let lower_label = period.label.to_lowercase();
    let (phrase, lower) = match period.id.as_str() {
        "day" => ("Today".to_string(), "today".to_string()),
        "week" => ("This week".to_string(), "this week".to_string()),
        _ => (format!("In the {lower_label}"), format!("in the {lower_label}")),
    };

    // Columns: every account limit a row uses. Claude weekly, Claude 5-hour, then other
    // providers, each by the account order the input gives (else by account id).
    let mut keys: Vec<RowIn> = Vec::new();
    let mut seen_keys: Vec<String> = Vec::new();
    for stream in &period.streams {
        for row in &stream.rows {
            if is_credits(&row.meter) {
                continue;
            }
            let id = format!("{}|{}", row.account, row.meter);
            if !seen_keys.contains(&id) {
                seen_keys.push(id);
                keys.push(row.clone());
            }
        }
    }
    let account_rank = |account: &str| -> i64 {
        input
            .account_order
            .iter()
            .position(|id| id == account)
            .map(|at| at as i64)
            .unwrap_or(-1)
    };
    let base_order = |row: &RowIn| {
        (
            if provider(&row.account) == "claude" { 0 } else { 1 },
            if is_five(&row.meter) { 1 } else { 0 },
        )
    };
    keys.sort_by(|a, b| {
        base_order(a)
            .cmp(&base_order(b))
            .then_with(|| account_rank(&a.account).cmp(&account_rank(&b.account)))
            .then_with(|| base_order(a).cmp(&base_order(b))) // the reference's `cmp` fallback
            .then_with(|| a.account.cmp(&b.account))
    });
    let columns: Vec<ColumnOut> = keys
        .iter()
        .map(|row| {
            let two_on_account = keys.iter().filter(|k| k.account == row.account).count() > 1;
            ColumnOut {
                id: format!("{}|{}", row.account, row.meter),
                account: row.account.clone(),
                meter: row.meter.clone(),
                label: format!("{} {}", prov(&row.account), meter_word(&row.meter)),
                account_label: account_label(&row.account).to_string(),
                short: format!(
                    "{} {}{}",
                    prov(&row.account),
                    account_label(&row.account),
                    if two_on_account {
                        if is_five(&row.meter) { ", 5-h" } else { ", wk" }
                    } else {
                        ""
                    }
                ),
                hue: provider(&row.account).to_string(),
            }
        })
        .collect();
    let mut credits_accounts: Vec<&str> = Vec::new();
    for stream in &period.streams {
        for row in &stream.rows {
            if is_credits(&row.meter) && !credits_accounts.contains(&row.account.as_str()) {
                credits_accounts.push(&row.account);
            }
        }
    }
    let has_credits = !credits_accounts.is_empty();

    // A share of one account's limit; `percent` rows only, never a credits figure.
    let share_of = |row: &RowIn| -> Option<f64> {
        if row.unit == "percent" {
            row.value.map(|value| value / 100.0)
        } else {
            None
        }
    };
    // The cells of one row (one stream) or of every bucket merged: added within one column
    // only, never across accounts or units. A cell where any part was not read keeps the
    // parts it has and says so with a leading `>=`; all parts missing reads "not read".
    let build_cells = |streams: &[&StreamIn]| -> Vec<Option<Cell>> {
        columns
            .iter()
            .map(|col| {
                let mut total = 0.0;
                let mut partial = false;
                let mut seen = false;
                for stream in streams {
                    for row in &stream.rows {
                        if format!("{}|{}", row.account, row.meter) != col.id {
                            continue;
                        }
                        seen = true;
                        match row.value {
                            None => partial = true,
                            Some(_) => total += share_of(row).unwrap_or(0.0),
                        }
                    }
                }
                if !seen {
                    return None;
                }
                if partial && total == 0.0 {
                    return Some(Cell {
                        share: None,
                        text: "not read".to_string(),
                    });
                }
                let cell = cell_of(Some(total), &col.meter);
                Some(if partial {
                    Cell {
                        share: Some(total),
                        text: format!("≥{}", cell.text),
                    }
                } else {
                    cell
                })
            })
            .collect()
    };
    // Credits: per account, whole number with thousands comma; never added across accounts.
    let credits_of = |streams: &[&StreamIn]| -> Vec<CreditsCell> {
        let mut per: Vec<(String, f64)> = Vec::new();
        for stream in streams {
            for row in &stream.rows {
                if is_credits(&row.meter) && row.value.is_some() {
                    match per.iter_mut().find(|(account, _)| account == &row.account) {
                        Some(entry) => entry.1 += row.value.unwrap_or(0.0),
                        None => per.push((row.account.clone(), row.value.unwrap_or(0.0))),
                    }
                }
            }
        }
        per.sort_by(|a, b| a.0.cmp(&b.0));
        per.into_iter()
            .map(|(account, value)| CreditsCell {
                account_label: account_label(&account).to_string(),
                text: credits_text(value),
                account,
            })
            .collect()
    };
    // The largest single share of a weekly limit; a row with no weekly cell falls back to
    // its largest share of any cell.
    let peak = |cells: &[Option<Cell>]| -> Option<(usize, f64)> {
        let mut list: Vec<(usize, f64)> = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            if let Some(cell) = cell
                && is_weekly(&columns[i].meter)
                && let Some(share) = cell.share
            {
                list.push((i, share));
            }
        }
        if list.is_empty() {
            for (i, cell) in cells.iter().enumerate() {
                if let Some(cell) = cell
                    && let Some(share) = cell.share
                {
                    list.push((i, share));
                }
            }
        }
        list.sort_by(|a, b| b.1.total_cmp(&a.1));
        list.into_iter().next()
    };
    // Phone bars: weekly cells by share, each followed at once by its account's 5-hour
    // cell, then anything left.
    let block_order = |cells: &[Option<Cell>]| -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut weekly: Vec<usize> = Vec::new();
        for (i, cell) in cells.iter().enumerate() {
            if cell.is_some() && !is_five(&columns[i].meter) {
                weekly.push(i);
            }
        }
        weekly.sort_by(|a, b| {
            let a_share = cells[*a].as_ref().and_then(|c| c.share).unwrap_or(-1.0);
            let b_share = cells[*b].as_ref().and_then(|c| c.share).unwrap_or(-1.0);
            b_share.total_cmp(&a_share)
        });
        for i in weekly {
            out.push(i);
            let five = columns
                .iter()
                .enumerate()
                .position(|(j, col)| cells[j].is_some() && is_five(&col.meter) && col.account == columns[i].account);
            if let Some(five) = five {
                out.push(five);
            }
        }
        for (i, cell) in cells.iter().enumerate() {
            if cell.is_some() && !out.contains(&i) {
                out.push(i);
            }
        }
        out
    };

    // Rows: the real streams by their peak share (ties by stream id), the bucket row last.
    struct RowWork {
        stream: String,
        label: String,
        bucket: bool,
        cells: Vec<Option<Cell>>,
        credits: Vec<CreditsCell>,
        block_order: Vec<usize>,
        peak: Option<(usize, f64)>,
    }
    let real: Vec<&StreamIn> = period.streams.iter().filter(|s| !s.bucket).collect();
    let mut rows: Vec<RowWork> = real
        .iter()
        .map(|stream| {
            let cells = build_cells(&[stream]);
            RowWork {
                label: if stream.label == stream.stream {
                    "a thread with no stream".to_string()
                } else {
                    stream.label.clone()
                },
                peak: peak(&cells),
                block_order: block_order(&cells),
                credits: credits_of(&[stream]),
                cells,
                stream: stream.stream.clone(),
                bucket: false,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        let a_peak = a.peak.as_ref().map(|p| p.1).unwrap_or(-1.0);
        let b_peak = b.peak.as_ref().map(|p| p.1).unwrap_or(-1.0);
        b_peak.total_cmp(&a_peak).then_with(|| a.stream.cmp(&b.stream))
    });
    let buckets: Vec<&StreamIn> = period.streams.iter().filter(|s| s.bucket).collect();
    if !buckets.is_empty() {
        let cells = build_cells(&buckets);
        rows.push(RowWork {
            stream: "Not tied to a stream".to_string(),
            label: "sessions no stream rule claims".to_string(),
            bucket: true,
            block_order: block_order(&cells),
            credits: credits_of(&buckets),
            cells,
            peak: None,
        });
    }

    // The sentences. The lead answers the question; the summary is the folded phone line.
    let of_account = |col: &ColumnOut, tail: &str| format!("{} {}’s {}", prov(&col.account), col.account_label, tail);
    let mut lead: Vec<Run> = Vec::new();
    let mut summary: Vec<Run> = Vec::new();
    let mut ranked: Vec<&RowWork> = rows.iter().filter(|row| !row.bucket && row.peak.is_some()).collect();
    ranked.sort_by(|a, b| {
        let a_peak = a.peak.as_ref().map(|p| p.1).unwrap_or(-1.0);
        let b_peak = b.peak.as_ref().map(|p| p.1).unwrap_or(-1.0);
        b_peak.total_cmp(&a_peak).then_with(|| a.stream.cmp(&b.stream))
    });
    if ranked.is_empty() {
        lead.push(run(format!("quota-watch has not tied any use to a stream {lower}.")));
        summary.push(run(format!("No use tied to a stream {lower}.")));
    } else {
        let first = ranked[0];
        let (peak_i, peak_share) = first.peak.unwrap();
        let col = &columns[peak_i];
        lead.push(run(format!("{phrase} ")));
        lead.push(strong(first.stream.clone()));
        lead.push(run(" used the most: about "));
        lead.push(strong(pct_text(peak_share)));
        lead.push(run(format!(
            " of {}",
            of_account(
                col,
                &format!("{} limit", if is_five(&col.meter) { "5-hour" } else { "weekly" })
            )
        )));
        summary.push(strong(first.stream.clone()));
        summary.push(run(format!(
            " used the most {lower}: about {} of {}.",
            pct_text(peak_share),
            of_account(
                col,
                &format!("{} limit", if is_five(&col.meter) { "5-hour" } else { "weekly" })
            )
        )));
        let five = columns
            .iter()
            .enumerate()
            .position(|(j, c)| first.cells[j].is_some() && is_five(&c.meter) && c.account == col.account);
        if let Some(five) = five
            && first.cells[five]
                .as_ref()
                .and_then(|c| c.share)
                .is_some_and(|share| share >= 1.0)
        {
            let share = first.cells[five].as_ref().unwrap().share.unwrap();
            lead.push(run(", plus "));
            lead.push(strong(to_fixed(share, 1)));
            lead.push(run(" full 5-hour windows on the same account"));
        }
        lead.push(run("."));
        let next: Vec<&RowWork> = ranked.iter().skip(1).take(2).copied().collect();
        let tail = |c: &ColumnOut| of_account(c, if is_five(&c.meter) { "5-hour window" } else { "week" });
        if next.len() == 1 {
            let (i, share) = next[0].peak.unwrap();
            lead.push(run(" Next is "));
            lead.push(strong(next[0].stream.clone()));
            lead.push(run(", at about "));
            lead.push(strong(pct_text(share)));
            lead.push(run(format!(" of {}.", tail(&columns[i]))));
        } else if next.len() == 2 {
            let (ai, ashare) = next[0].peak.unwrap();
            let (bi, bshare) = next[1].peak.unwrap();
            let (ca, cb) = (&columns[ai], &columns[bi]);
            if ca.id == cb.id {
                lead.push(run(" Next are "));
                lead.push(strong(next[0].stream.clone()));
                lead.push(run(" and "));
                lead.push(strong(next[1].stream.clone()));
                lead.push(run(", at about "));
                lead.push(strong(pct_text(ashare)));
                lead.push(run(" and "));
                lead.push(strong(pct_text(bshare)));
                lead.push(run(format!(" of {}.", tail(ca))));
            } else {
                lead.push(run(" Next are "));
                lead.push(strong(next[0].stream.clone()));
                lead.push(run(", at about "));
                lead.push(strong(pct_text(ashare)));
                lead.push(run(format!(" of {}, and ", tail(ca))));
                lead.push(strong(next[1].stream.clone()));
                lead.push(run(", at about "));
                lead.push(strong(pct_text(bshare)));
                lead.push(run(format!(" of {}.", tail(cb))));
            }
        }
    }

    let any_five = columns.iter().enumerate().any(|(i, col)| {
        is_five(&col.meter)
            && rows
                .iter()
                .any(|row| row.cells[i].as_ref().is_some_and(|c| c.share.is_some_and(|s| s >= 1.0)))
    });
    let mut key = vec![format!(
        "Each figure is the share of that account’s limit the stream used {lower}."
    )];
    if any_five {
        key.push("3.2× means 3.2 full 5-hour windows.".to_string());
    }
    if has_credits {
        key.push("Credits are never added across accounts.".to_string());
    }

    let mut foot: Vec<String> = Vec::new();
    foot.push(if spend.method_id.as_deref() == Some("token-share-at-list-price") {
        "Estimates: each account’s measured use is split between threads by their share of its tokens.".to_string()
    } else {
        format!(
            "Estimates: {}",
            spend
                .method
                .clone()
                .unwrap_or_else(|| "quota-watch splits measured use between threads.".to_string())
        )
    });
    match period.coverage {
        None => foot.push(format!("How much of the {noun} quota-watch watched is not known.")),
        Some(coverage) if coverage < 1.0 => {
            let mut sentence = format!(
                "quota-watch has watched {}% of {}",
                js_round(coverage * 100.0) as i64,
                if period.id == "day" {
                    "today".to_string()
                } else {
                    format!("this {noun}")
                }
            );
            if period.id != "day"
                && let Some(start) = period.start.as_deref()
                && let Some(since) = since_phrase(start, input.now_ms, input.utc_offset_minutes)
            {
                sentence.push_str(&format!(", since {since}"));
            }
            sentence.push('.');
            foot.push(sentence);
        }
        _ => {}
    }
    let missed: Vec<&MachineIn> = spend
        .machines_not_scanned
        .iter()
        .filter(|m| m.runs_missed.unwrap_or(0) > 0)
        .collect();
    if !missed.is_empty() {
        let parts: Vec<String> = missed
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if i == 0 {
                    format!(
                        "{} missed {} of {} scans",
                        capitalize(&machine_name(&m.name)),
                        fmt_opt(m.runs_missed),
                        fmt_opt(m.runs_in_window)
                    )
                } else {
                    format!("{} {}", machine_name(&m.name), fmt_opt(m.runs_missed))
                }
            })
            .collect();
        foot.push(format!(
            "{}, so some of {} sessions are missing.",
            parts.join(" and "),
            if missed.len() == 1 { "its" } else { "their" }
        ));
    }
    if !spend.unattributed.is_empty() {
        foot.push(format!(
            "{} could not be split between streams.",
            if spend.unattributed.len() == 1 {
                "One amount".to_string()
            } else {
                format!("{} amounts", spend.unattributed.len())
            }
        ));
    }
    if spend.budgets.is_empty() {
        foot.push("No stream budgets are set.".to_string());
    } else {
        let over: Vec<&BudgetIn> = spend.budgets.iter().filter(|b| b.over_limit).collect();
        if over.is_empty() {
            foot.push(format!(
                "{} stream {} set; none is over.",
                spend.budgets.len(),
                if spend.budgets.len() == 1 {
                    "budget is"
                } else {
                    "budgets are"
                }
            ));
        } else {
            let sentences: Vec<String> = over
                .iter()
                .map(|b| format!("{} is over its budget: {}", b.stream_label, b.text))
                .collect();
            foot.push(sentences.join(" "));
        }
    }

    let non_bucket = rows.iter().filter(|row| !row.bucket).count();
    let more_count = non_bucket.saturating_sub(4);
    let has_bucket = rows.iter().any(|row| row.bucket);
    let more = if more_count > 0 || has_bucket {
        let mut words = String::new();
        if more_count > 0 {
            words.push_str(&format!(
                "{} more {}",
                more_count,
                if more_count == 1 { "stream" } else { "streams" }
            ));
        }
        if more_count > 0 && has_bucket {
            words.push_str(", and ");
        }
        if has_bucket {
            words.push_str("use not tied to a stream");
        }
        Some(capitalize(&words))
    } else {
        None
    };

    PeriodOut {
        id: period.id.clone(),
        label: period.label.clone(),
        lead,
        summary,
        columns,
        credits: if has_credits {
            Some(CreditsHead {
                label: "Codex credits",
                account_label: "by account",
            })
        } else {
            None
        },
        rows: rows
            .into_iter()
            .map(|row| RowOut {
                stream: row.stream,
                label: row.label,
                bucket: row.bucket,
                cells: row.cells,
                credits: row.credits,
                block_order: row.block_order,
            })
            .collect(),
        key,
        foot: foot.join(" "),
        more,
    }
}

/// How a null count prints in the reference's sentences (JavaScript's `null`).
fn fmt_opt(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string())
}

/// The composed `spendByStream` field: every period's table and sentences, or the
/// contract's unavailable shape when no accepted quota report has periods. Pure: the same
/// input gives the same words.
pub fn compose(input: &SpendInput) -> SpendByStream {
    if input.spend.periods.is_empty() {
        return SpendByStream {
            state: "unavailable",
            unavailable: Some(UNAVAILABLE_SENTENCE.to_string()),
            default_period: None,
            as_of: None,
            as_of_text: None,
            periods: Vec::new(),
        };
    }
    let default_period = if input.spend.periods.iter().any(|p| p.id == "week") {
        "week".to_string()
    } else {
        input.spend.periods[0].id.clone()
    };
    let as_of_text = input.spend.as_of.as_deref().and_then(|as_of| {
        parse_iso(as_of).map(|epoch| format!("Read {}", clock_text(epoch, input.utc_offset_minutes)))
    });
    SpendByStream {
        state: "ok",
        unavailable: None,
        default_period: Some(default_period),
        as_of: input.spend.as_of.clone(),
        as_of_text,
        periods: input
            .spend
            .periods
            .iter()
            .map(|period| period_view(period, input, &input.spend))
            .collect(),
    }
}
