//! The usage record contract, `aac.usage-report.v1` (docs/store.md section 12): what a reader of one
//! tool's logs on one computer hands the store. Shared by the store and the log readers, so the
//! types are plain serde structs with camelCase names on the wire.
//!
//! A report is sent for every read, also a failed one: a failed read is a state and no cells, never
//! zero rows. A cell is the absolute totals of one UTC hour for one session, route, model, effort and
//! account, as the reader counts them now; the store replaces cells, it never adds a report to what
//! it holds, so reading the same logs again never double counts.

use crate::error::Result;
use crate::fail;
use serde::{Deserialize, Serialize};

pub const REPORT_SCHEMA: &str = "aac.usage-report.v1";
pub const STATES: [&str; 10] = [
    "ok",
    "no_usage",
    "partial",
    "cached",
    "scanning",
    "not_installed",
    "installed_no_logs",
    "unreadable",
    "no_reader",
    "scan_failed",
];
/// States a report may carry cells with.
pub const STATES_WITH_CELLS: [&str; 2] = ["ok", "partial"];
/// States a complete report may have.
pub const STATES_COMPLETE: [&str; 2] = ["ok", "no_usage"];
pub const ORIGINS: [&str; 6] = ["default", "env", "tool_config", "t3_instance", "ccs_instance", "extra"];
pub const KINDS: [&str; 3] = ["live", "backfill", "import"];
pub const MAX_CELLS: usize = 200_000;
pub const MAX_SESSIONS: usize = 20_000;
pub const MAX_ROOTS: usize = 64;
pub const MAX_PARTS: usize = 32;
pub const HOUR_MS: u64 = 3_600_000;
/// 2000-01-01T00:00:00Z: nothing older is a usage log of these tools.
pub const EARLIEST_MS: u64 = 946_684_800_000;
/// A record dated more than this after the store's clock is not written: a clock running ahead or a
/// bad log line would otherwise move the 72-hour settle line past now and freeze every live write.
pub const FUTURE_SLACK_MS: u64 = 5 * 60_000;
/// Cell quality flags (`quality` on the wire), kept with the cell for good.
pub const QUALITY_FLAGS: [&str; 1] = ["outputIncludesReasoningTwice"];
/// Bit of `QUALITY_FLAGS[0]`: the output count holds the reasoning tokens twice (the retired Codex
/// cache, docs/store.md 12.8). An upper bound, never split; input and cached input are exact.
pub const Q_OUTPUT_INCLUDES_REASONING_TWICE: u32 = 1;
const MAX_TOKENS: u64 = 1_000_000_000_000_000;
const MAX_USD: f64 = 1.0e9;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    #[serde(default)]
    pub roots: Option<u64>,
    #[serde(default)]
    pub files_found: Option<u64>,
    #[serde(default)]
    pub files_read: Option<u64>,
    #[serde(default)]
    pub files_failed: Option<u64>,
    #[serde(default)]
    pub newest_mtime: Option<String>,
    /// Records the reader left out because they were dated more than `FUTURE_SLACK_MS` after the
    /// time it read them. The store adds the cells it leaves out for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub future_dated: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    /// First 16 hex of sha256("aac-root-v1:" + tool + ":" + origin + ":" + path with home as ~).
    pub root: String,
    pub origin: String,
    pub state: String,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub counts: Option<Counts>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    /// UTC hour start, `2026-11-13T14:00:00Z`.
    pub hour: String,
    /// `sha256("aac-session-v1:" + tool + ":" + raw session id)`, first 16 hex; null when the reader
    /// cannot tie the row to a session.
    #[serde(default)]
    pub session_key: Option<String>,
    /// The full 64-hex hash of the same string, when the reader still had the raw id.
    #[serde(default)]
    pub session_hash: Option<String>,
    /// The logged route, lower case; empty when the log names none.
    #[serde(default)]
    pub route: String,
    /// The model as logged. Canonical id, family and catalog hash are resolved at query time.
    pub model_raw: String,
    /// Reserved: reasoning effort and how it is known (DATA-ARCHITECTURE identitySource words).
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub effort_source: Option<String>,
    /// The dashboard account identity (`accountAnalyticsIdentity`); never guessed.
    #[serde(default)]
    pub account_key: Option<String>,
    #[serde(default)]
    pub account_source: Option<String>,
    pub input_tokens: u64,
    /// Includes reasoning output.
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// Reserved: the reasoning part of `output_tokens` when the log reports it.
    #[serde(default)]
    pub reasoning_output_tokens: Option<u64>,
    /// The cost the tool itself logged for these events (OMP `usage.cost.total`); null when the log
    /// records none. Reported, never estimated.
    #[serde(default)]
    pub logged_cost_usd: Option<f64>,
    /// Computed by the server, not the reader: the API-equivalent estimate (the logged cost where
    /// there is one, the rest at list price) and its part priced at the unknown-model fallback
    /// rate. Null when not computed; the store prices such cells when it answers.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_cost_usd: Option<f64>,
    /// Usage events in the cell; null when the reader cannot split its count.
    #[serde(default)]
    pub events: Option<u64>,
    /// Known flaws of the counts, from `QUALITY_FLAGS`; kept with the cell for good.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<String>,
}

impl Cell {
    /// The cell's quality flags as bits; None when one is unknown.
    pub fn quality_bits(&self) -> Option<u32> {
        let mut bits = 0;
        for flag in &self.quality {
            let i = QUALITY_FLAGS.iter().position(|f| f == flag)?;
            bits |= 1 << i;
        }
        Some(bits)
    }
}

/// Quality bits back to their flag words.
pub fn quality_words(bits: u32) -> Vec<String> {
    QUALITY_FLAGS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, f)| f.to_string())
        .collect()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cwd {
    pub key: String,
    /// Home written as `~`.
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionPart {
    #[serde(default)]
    pub route: String,
    pub model_raw: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub logged_cost_usd: Option<f64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub fallback_cost_usd: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_key: String,
    #[serde(default)]
    pub session_hash: Option<String>,
    pub first: String,
    pub last: String,
    /// The session's totals per route and model (at most 32).
    #[serde(default)]
    pub parts: Vec<SessionPart>,
    #[serde(default)]
    pub target: Option<String>,
    /// Reserved evidence: the working folder (an inferred join only). Threads, tasks, runs,
    /// projects and streams arrive as `aac.session-binding.v1` records, never on a session row.
    #[serde(default)]
    pub cwd: Option<Cwd>,
    /// Known flaws of the parts' counts, from `QUALITY_FLAGS`; kept with the session for good.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<String>,
}

impl Session {
    pub fn quality_bits(&self) -> Option<u32> {
        let mut bits = 0;
        for flag in &self.quality {
            bits |= 1 << QUALITY_FLAGS.iter().position(|f| f == flag)?;
        }
        Some(bits)
    }
}

/// Reserved (section 1.4a of the design): per-message records. Refused until the store keeps them.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub at: String,
    pub key: String,
    #[serde(default)]
    pub session_key: Option<String>,
    #[serde(default)]
    pub route: String,
    pub model_raw: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub schema: String,
    pub computer: String,
    pub tool: String,
    pub read_at: String,
    pub state: String,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub cover_from: Option<String>,
    #[serde(default)]
    pub cause: Option<String>,
    #[serde(default)]
    pub counts: Option<Counts>,
    #[serde(default)]
    pub roots: Vec<Root>,
    #[serde(default)]
    pub cells: Vec<Cell>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub messages: Vec<Message>,
    /// How the store treats it: `live` (default), `backfill` (a deep read: insert and raise only)
    /// or `import` (an old cache file: insert and raise only).
    #[serde(default)]
    pub kind: Option<String>,
    /// Sender's optimisation: UTC day numbers whose cells it left out because they did not change
    /// since its last accepted report. The store leaves those days alone.
    #[serde(default)]
    pub same_days: Vec<u32>,
}

/// `YYYY-MM-DDTHH:MM:SS[.fff]Z` to epoch milliseconds; anything else is None.
pub fn parse_iso(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<u64> {
        let s = b.get(from..from + len)?;
        if !s.iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        std::str::from_utf8(s).ok()?.parse().ok()
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 2)?,
        num(8, 2)?,
        num(11, 2)?,
        num(14, 2)?,
        num(17, 2)?,
    );
    let mut ms = 0;
    let rest = &b[19..b.len() - 1];
    if !rest.is_empty() {
        if rest[0] != b'.' || rest.len() < 2 || rest.len() > 10 || !rest[1..].iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let frac = std::str::from_utf8(&rest[1..]).ok()?;
        let padded = format!("{frac:0<3}");
        ms = padded[..3].parse().ok()?;
    }
    if !(1..=12).contains(&mo) || d == 0 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(y as i64, mo as u32, d as u32);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400_000 + h * HOUR_MS + mi * 60_000 + s * 1000 + ms)
}

fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Lower-case identifier: `[a-z0-9][a-z0-9_-]{0,31}`.
pub fn ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

/// A fixed cause word: `[a-z0-9_:.-]{1,64}`.
pub fn cause_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_:.-".contains(&c))
}

pub fn hex16_ok(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn hex64_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A persisted label: bounded, no control characters, nothing shaped like a credential.
pub fn label_ok(s: &str, max: usize) -> bool {
    // the shared AAC secret policy (docs/secret-policy.md): no credential and no email address
    !s.is_empty()
        && s.len() <= max
        && !s.chars().any(|c| c.is_control())
        && crate::secret_policy::find_secret(s).is_none()
}

fn route_ok(s: &str) -> bool {
    s.is_empty() || (s.len() <= 64 && label_ok(s, 64) && s == s.to_lowercase())
}

fn usd_ok(v: f64) -> bool {
    v.is_finite() && (0.0..=MAX_USD).contains(&v)
}

fn opt_usd_ok(v: Option<f64>) -> bool {
    v.map(usd_ok).unwrap_or(true)
}

/// USD to whole nano-dollars (exact comparisons, no float noise in the store).
pub fn nano(usd: f64) -> u64 {
    (usd * 1e9).round().max(0.0) as u64
}

fn counts_ok(c: &Counts) -> bool {
    [c.roots, c.files_found, c.files_read, c.files_failed, c.future_dated]
        .iter()
        .all(|v| v.map(|n| n <= 100_000_000).unwrap_or(true))
        && c.newest_mtime
            .as_deref()
            .map(|t| parse_iso(t).is_some())
            .unwrap_or(true)
}

/// A report checked whole, with its times as milliseconds.
pub struct Checked<'a> {
    pub report: &'a Report,
    /// The read time, never later than the store's clock.
    pub read_at_ms: u64,
    pub cover_from_ms: Option<u64>,
    pub kind: &'static str,
    /// Each cell's hour start in ms, in the order of `report.cells`; None for a cell dated more
    /// than `FUTURE_SLACK_MS` after the store's clock, which is not written.
    pub cell_hours: Vec<Option<u64>>,
    /// Each cell's quality bits, in the same order.
    pub cell_quality: Vec<u32>,
    /// Each session's first and last ms, the last no later than the clock plus the slack; None
    /// for a session that starts after that.
    pub session_spans: Vec<Option<(u64, u64)>>,
    /// The reader's future-dated count plus the records of the cells left out here.
    pub future_dated: u64,
}

impl Report {
    /// Checks the whole report against the store's clock; any failure refuses all of it.
    pub fn check(&self) -> Result<Checked<'_>> {
        self.check_at(crate::engine::now_ms())
    }

    /// Checks the whole report against `now_ms`: the read time is clamped to it, and cells and
    /// sessions dated more than `FUTURE_SLACK_MS` after it are left out and counted.
    pub fn check_at(&self, now_ms: u64) -> Result<Checked<'_>> {
        let limit = now_ms.saturating_add(FUTURE_SLACK_MS);
        if self.schema != REPORT_SCHEMA {
            fail!("invalid", "the report schema is not {REPORT_SCHEMA}");
        }
        if !ident_ok(&self.computer) || !ident_ok(&self.tool) {
            fail!("invalid", "computer and tool are lower-case identifiers");
        }
        let Some(sent_at_ms) = parse_iso(&self.read_at) else {
            fail!("invalid", "readAt is not an ISO UTC time");
        };
        let read_at_ms = sent_at_ms.min(now_ms);
        if read_at_ms < EARLIEST_MS {
            fail!("invalid", "readAt is too old");
        }
        if !STATES.contains(&self.state.as_str()) {
            fail!("invalid", "unknown state {}", self.state);
        }
        if self.complete && !STATES_COMPLETE.contains(&self.state.as_str()) {
            fail!("invalid", "only an ok or no_usage read can be complete");
        }
        let kind = match self.kind.as_deref().unwrap_or("live") {
            "live" => "live",
            "backfill" => "backfill",
            "import" => "import",
            _ => fail!("invalid", "unknown report kind"),
        };
        if self.cause.as_deref().map(|c| !cause_ok(c)).unwrap_or(false) {
            fail!("invalid", "cause is a fixed word");
        }
        if self.counts.as_ref().map(|c| !counts_ok(c)).unwrap_or(false) {
            fail!("invalid", "counts are out of range");
        }
        if self.roots.len() > MAX_ROOTS {
            fail!("invalid", "too many roots");
        }
        for r in &self.roots {
            if !hex16_ok(&r.root)
                || !ORIGINS.contains(&r.origin.as_str())
                || !STATES.contains(&r.state.as_str())
                || r.cause.as_deref().map(|c| !cause_ok(c)).unwrap_or(false)
                || r.counts.as_ref().map(|c| !counts_ok(c)).unwrap_or(false)
            {
                fail!("invalid", "a root entry is malformed");
            }
        }
        if !self.messages.is_empty() {
            fail!(
                "not_supported",
                "per-message records are reserved; the store does not keep them yet"
            );
        }
        if !self.cells.is_empty() && !STATES_WITH_CELLS.contains(&self.state.as_str()) {
            fail!("invalid", "a {} read carries no cells", self.state);
        }
        if self.cells.len() > MAX_CELLS || self.sessions.len() > MAX_SESSIONS {
            fail!(
                "too_large",
                "a report holds at most {MAX_CELLS} cells and {MAX_SESSIONS} sessions"
            );
        }
        let cover_from_ms = match &self.cover_from {
            Some(t) => match parse_iso(t) {
                // a read time clamped to now pulls a later coverFrom back to its hour
                Some(ms) if ms % HOUR_MS == 0 && ms <= sent_at_ms => Some(ms.min(read_at_ms - read_at_ms % HOUR_MS)),
                _ => fail!("invalid", "coverFrom is not an hour start at or before readAt"),
            },
            None => None,
        };
        if self.complete && cover_from_ms.is_none() {
            fail!("invalid", "a complete read names the hours it covers (coverFrom)");
        }
        let mut future_dated = self.counts.as_ref().and_then(|c| c.future_dated).unwrap_or(0);
        let mut cell_hours = Vec::with_capacity(self.cells.len());
        let mut cell_quality = Vec::with_capacity(self.cells.len());
        for c in &self.cells {
            let Some(h) = parse_iso(&c.hour).filter(|h| h % HOUR_MS == 0) else {
                fail!("invalid", "a cell hour is not a UTC hour start");
            };
            // a cell after the store's clock is left out below; one after its own read is refused
            if h < EARLIEST_MS || (h <= limit && h > sent_at_ms + HOUR_MS) {
                fail!("invalid", "a cell hour is outside the read");
            }
            let Some(quality) = c.quality_bits().filter(|_| c.quality.len() <= QUALITY_FLAGS.len()) else {
                fail!("invalid", "a cell names an unknown quality flag");
            };
            if quality & Q_OUTPUT_INCLUDES_REASONING_TWICE != 0 && c.reasoning_output_tokens.is_some() {
                fail!("invalid", "output that counts reasoning twice has no reasoning split");
            }
            if c.session_key.as_deref().map(|s| !hex16_ok(s)).unwrap_or(false)
                || c.session_hash
                    .as_deref()
                    .map(|h| !hex64_ok(h) || Some(&h[..16]) != c.session_key.as_deref())
                    .unwrap_or(false)
                || !route_ok(&c.route)
                || !label_ok(&c.model_raw, 200)
                || c.effort.as_deref().map(|e| !ident_ok(e)).unwrap_or(false)
                || c.effort_source.as_deref().map(|e| !ident_ok(e)).unwrap_or(false)
                || c.account_key.as_deref().map(|a| !label_ok(a, 128)).unwrap_or(false)
                || c.account_source.as_deref().map(|e| !ident_ok(e)).unwrap_or(false)
            {
                fail!(
                    "invalid",
                    "a cell names a malformed session, route, model, effort or account"
                );
            }
            let tokens = [
                c.input_tokens,
                c.output_tokens,
                c.cache_creation_tokens,
                c.cache_read_tokens,
            ];
            if tokens.iter().any(|t| *t > MAX_TOKENS)
                || !opt_usd_ok(c.cost_usd)
                || !opt_usd_ok(c.fallback_cost_usd)
                || !opt_usd_ok(c.logged_cost_usd)
                || c.fallback_cost_usd.unwrap_or(0.0) > c.cost_usd.unwrap_or(f64::MAX) + 1e-9
                || c.reasoning_output_tokens.map(|r| r > c.output_tokens).unwrap_or(false)
                || c.events.map(|e| e > MAX_TOKENS).unwrap_or(false)
            {
                fail!("invalid", "a cell's counts or costs are out of range");
            }
            cell_quality.push(quality);
            if h > limit {
                // every record of the cell is dated after the slack: left out, and counted
                future_dated = future_dated.saturating_add(c.events.unwrap_or(1).max(1));
                cell_hours.push(None);
            } else {
                cell_hours.push(Some(h));
            }
        }
        let mut session_spans = Vec::with_capacity(self.sessions.len());
        for s in &self.sessions {
            let (Some(first), Some(last)) = (parse_iso(&s.first), parse_iso(&s.last)) else {
                fail!("invalid", "a session's first and last are ISO UTC times");
            };
            if !hex16_ok(&s.session_key)
                || s.session_hash
                    .as_deref()
                    .map(|h| !hex64_ok(h) || h[..16] != s.session_key)
                    .unwrap_or(false)
                || first > last
                || first < EARLIEST_MS
                || s.parts.len() > MAX_PARTS
                || s.quality.len() > QUALITY_FLAGS.len()
                || s.quality_bits().is_none()
            {
                fail!("invalid", "a session is malformed");
            }
            for p in &s.parts {
                if !route_ok(&p.route)
                    || !label_ok(&p.model_raw, 200)
                    || [
                        p.input_tokens,
                        p.output_tokens,
                        p.cache_creation_tokens,
                        p.cache_read_tokens,
                    ]
                    .iter()
                    .any(|t| *t > MAX_TOKENS)
                    || !opt_usd_ok(p.cost_usd)
                    || !opt_usd_ok(p.fallback_cost_usd)
                    || !opt_usd_ok(p.logged_cost_usd)
                {
                    fail!("invalid", "a session part is malformed");
                }
            }
            if s.target.as_deref().map(|t| !label_ok(t, 32)).unwrap_or(false)
                || s.cwd
                    .as_ref()
                    .map(|c| !hex16_ok(&c.key) || !label_ok(&c.path, 512))
                    .unwrap_or(false)
            {
                fail!("invalid", "a session's target or folder is malformed");
            }
            session_spans.push(if first > limit {
                None
            } else {
                Some((first, last.min(limit)))
            });
        }
        if self.same_days.len() > 4000 {
            fail!("invalid", "too many unchanged days");
        }
        Ok(Checked {
            report: self,
            read_at_ms,
            cover_from_ms,
            kind,
            cell_hours,
            cell_quality,
            session_spans,
            future_dated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_shared_secret_policy() {
        for ok in [
            "claude-opus-5-5",
            "gpt-5.6-sol",
            "claude-3-5-sonnet-v2@20241022",
            "anthropic/claude-sonnet-4.5",
            "codex",
            "~/projects/aac",
        ] {
            assert!(label_ok(ok, 200), "{ok}");
        }
        for bad in [
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "someone@example.com",
            "https://user:hunter2@internal/api",
        ] {
            assert!(!label_ok(bad, 200), "{bad}");
            assert!(crate::secret_policy::find_secret(bad).is_some(), "{bad}");
        }
        assert!(!label_ok("", 10) && !label_ok("a\nb", 10) && !label_ok("abcdef", 5));
    }

    #[test]
    fn iso_round_trip() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-11-13T14:00:00Z"), Some(1_794_578_400_000));
        assert_eq!(parse_iso("2026-11-13T14:00:00.250Z"), Some(1_794_578_400_250));
        assert_eq!(parse_iso("2026-02-29T00:00:00Z"), None);
        assert!(parse_iso("2024-02-29T00:00:00Z").is_some());
        assert_eq!(parse_iso("2026-11-13 14:00"), None);
        assert_eq!(parse_iso("2026-11-13T14:00:00+01:00"), None);
    }
}
