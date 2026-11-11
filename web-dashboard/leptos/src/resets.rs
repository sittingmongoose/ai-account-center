//! Resets and balances in Details: the account's rate-limit resets as one fact group (caps head, the answer as one
//! sentence, a State / Refills / Expires table with Use reset on each row AAC can use), the confirm dialog, the result
//! in place, and the "Reset claims" history. The server contract is docs/reset-claims.md.
//!
//! The owner picks the exact grant: each row is one grant, and Use reset names that grant's opaque key and nothing
//! else. Opening the dialog calls only `prepare` (a fresh read; nothing is sent to the provider). Only "Use this
//! reset" sends the claim, with the one-shot confirmation prepare returned, inside its 120-second life. The dialog
//! names the account by the label prepare read fresh, never a cached card. After a claim the group reads the
//! inventory again: what it shows comes from the server's verification read, never from what was hoped. An
//! unconfirmed claim blocks every Use reset on the account until Check again settles it.
//!
//! Rows from the usage reading (no grant keys) show while the inventory loads, or when it can't be read, and can't
//! be used. Z.ai cards are listed with "Use in Z.ai"; Qwen says its plan has none.

use crate::api::{FetchError, send};
use crate::dash::nullable;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};

// ---- the wire (reset-claim-routes.ts) ----------------------------------------------------------------------------

/// GET `/api/accounts/:id/reset-claims`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Inventory {
    #[serde(deserialize_with = "nullable")]
    pub status: String,
    #[serde(deserialize_with = "nullable")]
    pub reason: String,
    pub observed_at_ms: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub grants: Vec<Grant>,
    #[serde(deserialize_with = "nullable")]
    pub history: Vec<HistoryEvent>,
    pub pending_attempt: Option<String>,
    pub canonical_account_key: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Grant {
    #[serde(deserialize_with = "nullable")]
    pub grant_key: String,
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub refills_text: String,
    pub provider_words: Option<String>,
    pub resets_left: u32,
    pub expires_at_ms: Option<f64>,
    pub claimable: bool,
    pub claim_blocked_reason: Option<String>,
}

/// POST `.../prepare`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Prepared {
    pub account: AccountLabel,
    #[serde(deserialize_with = "nullable")]
    pub confirmation_token: String,
    pub expires_at_ms: Option<f64>,
    pub grant: Grant,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AccountLabel {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

/// POST root and `.../check`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ClaimResult {
    #[serde(deserialize_with = "nullable")]
    pub attempt_key: String,
    #[serde(deserialize_with = "nullable")]
    pub outcome: String,
    #[serde(deserialize_with = "nullable")]
    pub verification: String,
    pub provider_success: bool,
    #[serde(deserialize_with = "nullable")]
    pub windows: Vec<Refilled>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Refilled {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    pub used_percent: Option<f64>,
}

/// A 409 (or 403) the route explains: an allowlisted native code, and whether the provider write was reached.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Refusal {
    #[serde(deserialize_with = "nullable")]
    pub code: String,
    pub reason: Option<String>,
    pub sent: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HistoryEvent {
    pub at_ms: f64,
    #[serde(deserialize_with = "nullable")]
    pub event: String,
    #[serde(deserialize_with = "nullable")]
    pub outcome: String,
    pub details: HistoryDetails,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HistoryDetails {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub idempotency_hash: String,
}

/// A reset row the usage reading lists (`resetRows` from view-model.mjs): no key, so it can't be used.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadingRow {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub refills: String,
    /// ready, saved, used or expired
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub left: u32,
    pub count: u32,
    pub expires_at_ms: Option<f64>,
}

// ---- the view logic (pure: unit tested) ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Ready,
    NotYet,
    Used,
    Expired,
}

impl Phase {
    pub fn word(self) -> &'static str {
        match self {
            Phase::Ready => "Ready",
            Phase::NotYet => "Not yet",
            Phase::Used => "Used",
            Phase::Expired => "Expired",
        }
    }
    pub fn spent(self) -> bool {
        matches!(self, Phase::Used | Phase::Expired)
    }
}

/// One table row: one grant (or, from the reading, one grant or card kind).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Stable within one list: the grant key, else the reading's key.
    pub key: String,
    /// The opaque key Use reset names; None for a row from the reading.
    pub grant_key: Option<String>,
    pub phase: Phase,
    /// What it refills, in a few words ("Weekly limit"); for an unknown kind, the provider's own words.
    pub refills: String,
    /// The second line under it: the full sentence, or why AAC can't use it.
    pub note: String,
    pub unknown: bool,
    /// Uses this row still has (one grant may hold more than one).
    pub left: u32,
    /// How many resets the row stands for (a used row counts what was used).
    pub count: u32,
    pub expires_at_ms: Option<f64>,
    pub claimable: bool,
}

pub const UNKNOWN_NOTE: &str =
    "AAC doesn't know what this refills yet; it can't be used from here.";

/// The few words for a known kind, as the Refills column says them.
pub fn refill_words(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "session" => "5-hour limit",
        "weekly" => "Weekly limit",
        "full" => "5-hour and weekly limits",
        "fable" => "Fable weekly usage",
        "banked" => "Codex limits that are used up",
        _ => return None,
    })
}

/// The reset's own name, for the dialog and the history.
pub fn kind_name(kind: &str) -> &'static str {
    match kind {
        "session" => "5-hour reset",
        "weekly" => "Weekly reset",
        "full" => "Full reset",
        "fable" => "Fable reset",
        "banked" => "Banked reset",
        _ => "Other reset",
    }
}

/// Why AAC can't use a reset that is still there, in one sentence.
pub fn why_not(reason: &str, provider: &str) -> String {
    match reason {
        "unknown-grant-kind" => UNKNOWN_NOTE.into(),
        "provider-read-only" if provider == "zai" => "Z.ai cards are used in Z.ai; AAC only lists them.".into(),
        "provider-read-only" => "AAC can't use resets for this provider.".into(),
        "exact-grant-unavailable" | "unsupported-grant-id" => {
            "The provider didn't name this exact reset, so AAC can't pick it safely.".into()
        }
        "credential-host-unavailable" if provider == "claude" => {
            "AAC can use resets only through a Windows sign-in for now; a Mac-only sign-in is view-only.".into()
        }
        "credential-host-unavailable" => "AAC can't find this login's saved profile on this computer.".into(),
        "identity-unconfirmed" | "identity-changed" => {
            "AAC couldn't confirm which account this is, so it won't use this reset.".into()
        }
        "grant-unavailable" => "The provider says this reset can't be used yet.".into(),
        "refill-windows-unconfirmed" if provider == "codex" => {
            "None of your Codex limits is used up, so there is nothing to refill yet.".into()
        }
        "refill-windows-unconfirmed" => "AAC can't confirm which limit this refills right now.".into(),
        "signed-out" => "This login is signed out.".into(),
        "cooldown" => "The provider asked AAC to wait before another reset.".into(),
        "rate-limited" => "Too many reset tries in a short time; wait a minute.".into(),
        "check-again-required" => "A reset claim on this account is still unconfirmed.".into(),
        _ => "AAC can't use this reset from here right now.".into(),
    }
}

/// The inventory as table rows, in the order that matters.
pub fn rows_from_inventory(inv: &Inventory, provider: &str, now_ms: f64) -> Vec<Row> {
    let mut rows: Vec<Row> = inv
        .grants
        .iter()
        .map(|g| {
            let unknown = g.kind == "unknown" || refill_words(&g.kind).is_none();
            let reason = g.claim_blocked_reason.clone().unwrap_or_default();
            let phase = if g.resets_left == 0 {
                Phase::Used
            } else if g.expires_at_ms.is_some_and(|e| e <= now_ms) {
                Phase::Expired
            } else if reason == "grant-unavailable" {
                Phase::NotYet
            } else {
                Phase::Ready
            };
            let refills = if unknown {
                match g
                    .provider_words
                    .as_deref()
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                {
                    Some(words) => format!("\u{201c}{words}\u{201d}"),
                    None => "A reset of another kind".into(),
                }
            } else {
                refill_words(&g.kind).unwrap_or_default().into()
            };
            let claimable =
                g.claimable && phase == Phase::Ready && !unknown && !g.grant_key.is_empty();
            let note = if unknown {
                UNKNOWN_NOTE.into()
            } else if phase.spent() || claimable {
                g.refills_text.clone()
            } else {
                why_not(&reason, provider)
            };
            Row {
                key: g.grant_key.clone(),
                grant_key: (!g.grant_key.is_empty()).then(|| g.grant_key.clone()),
                phase,
                refills,
                note,
                unknown,
                left: g.resets_left,
                count: g.resets_left.max(1),
                expires_at_ms: g.expires_at_ms,
                claimable,
            }
        })
        .collect();
    sort_rows(&mut rows);
    rows
}

/// The reading's rows: never usable from here.
pub fn rows_from_reading(rows: &[ReadingRow], now_ms: f64) -> Vec<Row> {
    let mut out: Vec<Row> = rows
        .iter()
        .map(|r| {
            let phase = match r.state.as_str() {
                "used" => Phase::Used,
                "expired" => Phase::Expired,
                _ if r.left == 0 => Phase::Used,
                _ if r.expires_at_ms.is_some_and(|e| e <= now_ms) => Phase::Expired,
                "saved" => Phase::NotYet,
                _ => Phase::Ready,
            };
            Row {
                key: r.key.clone(),
                grant_key: None,
                phase,
                refills: if r.refills.is_empty() {
                    "Not reported".into()
                } else {
                    r.refills.clone()
                },
                note: String::new(),
                unknown: false,
                left: r.left,
                count: r.count.max(r.left).max(1),
                expires_at_ms: r.expires_at_ms,
                claimable: false,
            }
        })
        .collect();
    sort_rows(&mut out);
    out
}

/// Usable first, soonest expiry first; then not yet, unknown kinds, and used or expired rows at the bottom.
pub fn sort_rows(rows: &mut [Row]) {
    let rank = |r: &Row| match r.phase {
        Phase::Ready if !r.unknown => 0,
        Phase::NotYet if !r.unknown => 1,
        Phase::Ready | Phase::NotYet => 2,
        Phase::Used | Phase::Expired => 3,
    };
    rows.sort_by(|a, b| {
        rank(a).cmp(&rank(b)).then_with(|| {
            let (x, y) = (
                a.expires_at_ms.unwrap_or(f64::MAX),
                b.expires_at_ms.unwrap_or(f64::MAX),
            );
            if rank(a) == 3 {
                y.total_cmp(&x)
            } else {
                x.total_cmp(&y)
            }
        })
    });
}

/// What the counts add up to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counts {
    pub ready: u32,
    pub not_yet: u32,
    pub unknown: u32,
    pub used: u32,
}

pub fn counts(rows: &[Row]) -> Counts {
    let mut c = Counts::default();
    for r in rows {
        match r.phase {
            Phase::Ready | Phase::NotYet if r.unknown => c.unknown += r.left,
            Phase::Ready => c.ready += r.left,
            Phase::NotYet => c.not_yet += r.left,
            Phase::Used => c.used += r.count,
            Phase::Expired => {}
        }
    }
    c
}

fn plural(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The group's answer as one sentence: the bold lead, then the rest. `day` words a time as "Dec 10".
pub fn summary(rows: &[Row], mode: &str, day: &dyn Fn(f64) -> String) -> (String, String) {
    if mode == "qwen" {
        return (
            "No resets on this plan.".into(),
            "Qwen's only top-up is a Credit Pack, bought in the Qwen console.".into(),
        );
    }
    let (one, many) = if mode == "zai" {
        ("reset card", "reset cards")
    } else {
        ("rate-limit reset", "rate-limit resets")
    };
    let c = counts(rows);
    let lead = if c.ready > 0 {
        format!("{} ready.", plural(c.ready, one, many))
    } else if c.not_yet > 0 {
        format!("{} saved, not usable yet.", plural(c.not_yet, one, many))
    } else if c.unknown > 0 {
        format!("No {one} AAC can use.")
    } else if !rows.is_empty() {
        format!("No {many} left.")
    } else {
        format!("No {many} listed.")
    };
    let mut rest: Vec<String> = vec![];
    let soonest = rows
        .iter()
        .filter(|r| r.phase == Phase::Ready && !r.unknown)
        .filter_map(|r| r.expires_at_ms)
        .min_by(f64::total_cmp);
    if let Some(at) = soonest {
        rest.push(if c.ready == 1 {
            format!("It expires {}.", day(at))
        } else {
            format!("The first expires {}.", day(at))
        });
    }
    if c.ready > 0 && c.not_yet > 0 {
        rest.push(format!(
            "{} more {} not usable yet.",
            c.not_yet,
            if c.not_yet == 1 { "is" } else { "are" }
        ));
    }
    if c.unknown > 0 {
        rest.push(format!(
            "{} of a kind AAC doesn't know yet, so it can't use {}.",
            plural(c.unknown, "more is", "more are"),
            if c.unknown == 1 { "it" } else { "them" }
        ));
    }
    if mode == "zai" && c.ready + c.not_yet > 0 {
        rest.push("Use them in Z.ai; AAC lists them but can't use them from here.".into());
    }
    (lead, rest.join(" "))
}

/// The head's end: "2 ready, 1 used" (the ready count bold); empty without rows.
pub fn head_end(rows: &[Row]) -> Option<(u32, String)> {
    if rows.is_empty() {
        return None;
    }
    let c = counts(rows);
    let mut tail = vec![];
    if c.not_yet > 0 {
        tail.push(format!("{} not yet", c.not_yet));
    }
    if c.used > 0 {
        tail.push(format!("{} used", c.used));
    }
    Some((
        c.ready,
        if tail.is_empty() {
            " ready".into()
        } else {
            format!(" ready, {}", tail.join(", "))
        },
    ))
}

/// "in 29 days", "in 5 h", "in 40 min"; empty for a time that has passed.
pub fn until_words(ms: f64) -> String {
    if !ms.is_finite() || ms <= 0.0 {
        return String::new();
    }
    let minutes = (ms / 60_000.0).floor() as i64;
    let hours = minutes / 60;
    let days = hours / 24;
    if days >= 2 {
        format!("in {days} days")
    } else if hours >= 1 {
        format!("in {hours} h")
    } else {
        format!("in {} min", minutes.max(1))
    }
}

/// A limit window by the name the meters use.
pub fn window_name(id: &str, provider: &str) -> String {
    match (provider, id) {
        (_, "five_hour") => "5-hour".into(),
        (_, "seven_day") => "Weekly".into(),
        (_, "seven_day_overage_included") => "Fable included usage".into(),
        ("codex", "primary") => "5-hour".into(),
        ("codex", "secondary") => "Weekly".into(),
        (_, other) => other.replace('_', " "),
    }
}

/// How a result reads in place, and what it allows next.
#[derive(Clone, Debug, PartialEq)]
pub struct Said {
    pub text: String,
    /// Shown as a problem (crit) rather than plain.
    pub bad: bool,
    /// Check again is offered (a pending claim).
    pub check: bool,
}

pub fn result_said(r: &ClaimResult, provider: &str) -> Said {
    let plain = |text: &str| Said {
        text: text.into(),
        bad: false,
        check: false,
    };
    let bad = |text: &str| Said {
        text: text.into(),
        bad: true,
        check: false,
    };
    match r.outcome.as_str() {
        "claimed" => {
            let parts: Vec<String> = r
                .windows
                .iter()
                .filter_map(|w| {
                    w.used_percent
                        .filter(|p| p.is_finite())
                        .map(|p| format!("{} is back to {}% used", window_name(&w.id, provider), p.round() as i64))
                })
                .collect();
            if parts.is_empty() {
                plain("Reset applied and confirmed.")
            } else {
                plain(&format!("Reset applied: {}.", parts.join(", ")))
            }
        }
        "claimed-unconfirmed" if r.provider_success => Said {
            text: "Reset was applied, but AAC could not confirm the new limits. Check again.".into(),
            bad: true,
            check: true,
        },
        "claimed-unconfirmed" => Said {
            text: "AAC sent the reset but got no clear answer, so it may have been used. Check again before using another."
                .into(),
            bad: true,
            check: true,
        },
        "nothing-to-reset" => plain("Nothing to reset right now. Nothing was spent."),
        "no-credit" => bad("That reset is no longer there. Nothing was spent."),
        "already-used" => bad("That reset was already used."),
        "cooldown" => bad("The provider asked AAC to wait before another reset. Nothing was spent."),
        "rate-limited" => bad("Too many reset tries in a short time. Nothing was spent; wait a minute."),
        "signed-out" => bad("The provider says this login is signed out. Nothing was spent."),
        "identity-changed" => bad("The signed-in account changed. Nothing was spent."),
        _ => bad("The provider couldn't take the reset right now. Nothing was spent."),
    }
}

/// A refused prepare or claim.
pub fn refusal_said(r: &Refusal, provider: &str) -> Said {
    let bad = |text: String| Said {
        text,
        bad: true,
        check: false,
    };
    if r.code == "browser-session-required" {
        return bad(
            "Using a reset needs a signed-in dashboard session. Paired apps can only view resets."
                .into(),
        );
    }
    match r.reason.as_deref() {
        Some("confirmation-invalid") => bad("This confirmation expired, open it again.".into()),
        Some("grant-changed") => bad("The resets changed since this list was read. Nothing was sent; look again.".into()),
        Some("stale-inventory") => bad("AAC's fresh read came back too old. Nothing was sent; try again.".into()),
        Some("check-again-required") => Said {
            text: "A reset claim on this account is still unconfirmed. Check it before using another.".into(),
            bad: true,
            check: true,
        },
        Some(reason) => bad(format!("{} Nothing was sent.", why_not(reason, provider))),
        None if r.sent => Said {
            text: "AAC may have sent the reset but couldn't record the answer. Check again before using another."
                .into(),
            bad: true,
            check: true,
        },
        None => bad("AAC couldn't use this reset right now. Nothing was sent.".into()),
    }
}

/// One claim in the history: when it started, which kind, how it ended (newest first).
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryRow {
    pub at_ms: f64,
    pub kind: String,
    pub result: &'static str,
}

pub fn history_word(outcome: &str) -> &'static str {
    match outcome {
        "claimed" => "Used, confirmed",
        "claimed-unconfirmed" => "Unconfirmed",
        "nothing-to-reset" => "Nothing to reset",
        "no-credit" => "No reset left",
        "already-used" => "Already used",
        "cooldown" => "Asked to wait",
        "rate-limited" => "Too many tries",
        "signed-out" => "Signed out",
        "identity-changed" => "Account changed",
        _ => "Provider unavailable",
    }
}

/// Each claim attempt once, with its latest verification. Prepares that were never used are left out.
pub fn history_rows(events: &[HistoryEvent]) -> Vec<HistoryRow> {
    let mut rows: Vec<(String, HistoryRow)> = vec![];
    for e in events {
        if e.event != "attempt" && e.event != "verification" {
            continue;
        }
        let id = e.details.idempotency_hash.clone();
        match rows.iter_mut().find(|(k, _)| !id.is_empty() && *k == id) {
            Some((_, row)) => {
                if e.event == "verification" {
                    row.result = history_word(&e.outcome);
                }
            }
            None => rows.push((
                id,
                HistoryRow {
                    at_ms: e.at_ms,
                    kind: kind_name(&e.details.kind).into(),
                    result: history_word(&e.outcome),
                },
            )),
        }
    }
    let mut out: Vec<HistoryRow> = rows.into_iter().map(|(_, r)| r).collect();
    out.sort_by(|a, b| b.at_ms.total_cmp(&a.at_ms));
    out
}

/// The client's own deadline for a confirmation: the server's 120 s counted from when prepare was sent (the server
/// starts later, so this is never later than its own), less the time a fresh read before the claim may take.
pub const CONFIRM_LIFE_MS: f64 = 120_000.0;
pub const CONFIRM_MARGIN_MS: f64 = 10_000.0;

pub fn confirm_deadline(
    sent_at_ms: f64,
    server_expires_at_ms: Option<f64>,
    server_now_guess_ms: f64,
) -> f64 {
    let local = sent_at_ms + CONFIRM_LIFE_MS - CONFIRM_MARGIN_MS;
    // A server whose clock agrees with ours may say it ends sooner; never later.
    match server_expires_at_ms {
        Some(at) if (at - server_now_guess_ms).abs() <= CONFIRM_LIFE_MS => {
            local.min(at - CONFIRM_MARGIN_MS)
        }
        _ => local,
    }
}

/// "1:05" left on the confirmation.
pub fn countdown(ms_left: f64) -> String {
    let s = (ms_left.max(0.0) / 1000.0).ceil() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

// ---- the lock on spending while a claim is unresolved ------------------------------------------------------------

/// Whether a claim on the shown account may still be unresolved. While it may be, every Use reset is disabled and only
/// Check again (a read) is offered. It latches on any answer that may mean a send happened (an unconfirmed result, a
/// lost or unreadable answer, a refusal after the write was reached) and is cleared only by a definite result, or by
/// an inventory read that started late enough to prove nothing is unresolved. An older read never clears it, and a
/// read never brings back the same attempt once it was settled. A different named attempt still blocks spending.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unresolved {
    /// The unresolved attempt's key, once an answer or a read names it (Check again needs it).
    pub key: Option<String>,
    latched: bool,
    /// A read that started before this (device clock) can't clear the latch: a claim this page sent might still
    /// reach the server's reservation after that read looked, or had reached it before it answered.
    proof_after_ms: f64,
    /// Settled attempt identities, retained so delayed inventory/check responses cannot revive them.
    settled: Vec<String>,
}

/// What an answer means for the lock.
#[derive(Clone, Debug, PartialEq)]
pub enum Lock {
    Keep,
    Latch {
        key: Option<String>,
        proof_after_ms: f64,
    },
    Settle {
        key: String,
    },
}

impl Unresolved {
    pub fn blocks(&self) -> bool {
        self.latched
    }

    pub fn apply(&mut self, lock: Lock) {
        match lock {
            Lock::Keep => {}
            Lock::Latch {
                key,
                proof_after_ms,
            } => {
                if key.as_ref().is_some_and(|k| self.settled.contains(k)) {
                    return;
                }
                self.latched = true;
                if let Some(k) = key.filter(|k| !k.is_empty()) {
                    self.key = Some(k);
                }
                self.proof_after_ms = self.proof_after_ms.max(proof_after_ms);
            }
            Lock::Settle { key } => {
                if key.is_empty() {
                    return;
                }
                if self.key.as_ref() == Some(&key) {
                    self.key = None;
                    self.latched = false;
                    self.proof_after_ms = 0.0;
                }
                if !self.settled.contains(&key) {
                    self.settled.push(key);
                }
            }
        }
    }

    /// An inventory read that started at `started_ms` named this pending attempt, or none. True when it cleared the
    /// latch (the server shows nothing unresolved, and the read is new enough to prove it).
    pub fn read(&mut self, pending: Option<String>, started_ms: f64) -> bool {
        match pending.filter(|k| !k.is_empty()) {
            Some(ref k) if self.settled.contains(k) => false,
            Some(k) => {
                self.apply(Lock::Latch {
                    key: Some(k),
                    proof_after_ms: started_ms,
                });
                false
            }
            None if self.latched && started_ms >= self.proof_after_ms => {
                if let Some(key) = self.key.clone() {
                    self.apply(Lock::Settle { key });
                } else {
                    self.latched = false;
                    self.proof_after_ms = 0.0;
                }
                true
            }
            None => false,
        }
    }
}

/// The lock after a claim's answer. `sent_at_ms` is when the claim left; a claim whose answer was lost could still
/// reserve the account until its confirmation can no longer be used, at most 120 s after it was sent.
pub fn claim_lock(answer: &Answer<ClaimResult>, sent_at_ms: f64, now_ms: f64) -> Lock {
    let unknown = Lock::Latch {
        key: None,
        proof_after_ms: sent_at_ms + CONFIRM_LIFE_MS,
    };
    match answer {
        Answer::Ok(r) if r.outcome == "claimed-unconfirmed" => Lock::Latch {
            key: Some(r.attempt_key.clone()),
            proof_after_ms: now_ms,
        },
        Answer::Ok(r) if r.outcome.is_empty() => unknown,
        Answer::Ok(r) if !r.attempt_key.is_empty() => Lock::Settle {
            key: r.attempt_key.clone(),
        },
        Answer::Ok(_) => unknown,
        // the write was reached, or another attempt already holds the account: a fresh read names its key
        Answer::Refused(r) if r.sent || r.reason.as_deref() == Some("check-again-required") => {
            Lock::Latch {
                key: None,
                proof_after_ms: now_ms,
            }
        }
        Answer::Refused(_) => Lock::Keep,
        Answer::Lost | Answer::Failed(_) => unknown,
    }
}

/// The lock after Check again: only a definite result settles it.
pub fn check_lock(answer: &Answer<ClaimResult>, now_ms: f64) -> Lock {
    match answer {
        Answer::Ok(r) if r.outcome == "claimed-unconfirmed" => Lock::Latch {
            key: Some(r.attempt_key.clone()),
            proof_after_ms: now_ms,
        },
        Answer::Ok(r) if !r.outcome.is_empty() && !r.attempt_key.is_empty() => Lock::Settle {
            key: r.attempt_key.clone(),
        },
        _ => Lock::Keep,
    }
}

/// The lock after prepare: the server says another claim on the account is still unresolved.
pub fn prepare_lock(answer: &Answer<Prepared>, now_ms: f64) -> Lock {
    match answer {
        Answer::Refused(r) if r.reason.as_deref() == Some("check-again-required") => Lock::Latch {
            key: None,
            proof_after_ms: now_ms,
        },
        _ => Lock::Keep,
    }
}

/// Whether a row's Use reset can be pressed.
pub fn use_enabled(claimable: bool, lock: &Unresolved, dialog: &Dialog) -> bool {
    claimable && !lock.blocks() && *dialog == Dialog::Closed
}

const SETTLED_ELSEWHERE: &str =
    "The unconfirmed claim has been settled; Reset claims shows how it ended.";

// ---- the server calls -------------------------------------------------------------------------------------------

fn base(account: &str) -> String {
    format!(
        "/api/accounts/{}/reset-claims",
        String::from(js_sys::encode_uri_component(account))
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareBody<'a> {
    grant_key: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaimBody<'a> {
    grant_key: &'a str,
    confirmation_token: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckBody<'a> {
    attempt_key: &'a str,
}

/// What a call came back with.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer<T> {
    Ok(T),
    Refused(Refusal),
    /// No answer (dropped connection or timeout): for a claim, it may have been sent.
    Lost,
    Failed(String),
}

fn answer<T: for<'de> Deserialize<'de>>(result: Result<(u16, String), FetchError>) -> Answer<T> {
    match result {
        Ok((200, text)) => match serde_json::from_str::<T>(&text) {
            Ok(v) => Answer::Ok(v),
            Err(_) => Answer::Failed("The server's answer could not be read.".into()),
        },
        Ok((401, _)) => Answer::Failed("Signed out. Sign in again to use resets.".into()),
        Ok((403 | 409, text)) => Answer::Refused(serde_json::from_str(&text).unwrap_or_default()),
        Ok((status, _)) => Answer::Failed(format!("The server answered {status}.")),
        Err(FetchError::Offline) => Answer::Lost,
        Err(e) => Answer::Failed(e.sentence()),
    }
}

pub async fn load(account: &str) -> Answer<Inventory> {
    answer(send("GET", &base(account), None).await)
}

pub async fn prepare(account: &str, grant_key: &str) -> Answer<Prepared> {
    let body = serde_json::to_string(&PrepareBody { grant_key }).unwrap_or_default();
    answer(send("POST", &format!("{}/prepare", base(account)), Some(body)).await)
}

pub async fn claim(account: &str, grant_key: &str, token: &str) -> Answer<ClaimResult> {
    let body = serde_json::to_string(&ClaimBody {
        grant_key,
        confirmation_token: token,
    })
    .unwrap_or_default();
    answer(send("POST", &base(account), Some(body)).await)
}

pub async fn check(account: &str, attempt_key: &str) -> Answer<ClaimResult> {
    let body = serde_json::to_string(&CheckBody { attempt_key }).unwrap_or_default();
    answer(send("POST", &format!("{}/check", base(account)), Some(body)).await)
}

// ---- dates (the device's own time zone) ---------------------------------------------------------------------------

fn date_with(ms: f64, time: bool) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    if date.get_time().is_nan() {
        return String::new();
    }
    let options = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    };
    set("month", "short");
    set("day", "numeric");
    if time {
        set("hour", "numeric");
        set("minute", "2-digit");
    }
    date.to_locale_string("en-US", &options).into()
}

/// "Dec 10, 5:00 PM".
pub fn when(ms: f64) -> String {
    date_with(ms, true)
}

/// "Dec 10".
pub fn day(ms: f64) -> String {
    date_with(ms, false)
}

// ---- the state one open Details keeps ----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Idle,
    Loading,
    Ready(Inventory),
    Failed(String),
}

/// The dialog: asking prepare, then the confirmation (or its end), then sending.
#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Closed,
    Opening {
        grant_key: String,
    },
    Open {
        grant_key: String,
        prepared: Prepared,
        deadline_ms: f64,
        ready: u32,
    },
    Sending {
        grant_key: String,
        prepared: Prepared,
        ready: u32,
    },
}

#[derive(Clone, Copy)]
pub struct ResetCtx {
    pub account: RwSignal<String>,
    pub provider: RwSignal<String>,
    pub load: RwSignal<Load>,
    pub dialog: RwSignal<Dialog>,
    /// What the last prepare, claim or check came to, shown in place under the table. (A fresh read mints new grant
    /// keys, so the line belongs to the group rather than to a row.)
    pub said: RwSignal<Option<Said>>,
    /// The shown account's lock: while it blocks, every Use reset waits on Check again.
    pub unresolved: RwSignal<Unresolved>,
    pub checking: RwSignal<bool>,
    /// The last good inventory per account, with when its read started (a reopen within a minute reuses it).
    cache: StoredValue<Vec<(String, f64, Inventory)>>,
    /// Every account's lock, kept while Details shows another account or is closed.
    locks: StoredValue<Vec<(String, Unresolved)>>,
    aliases: StoredValue<Vec<(String, String)>>,
    results: StoredValue<Vec<(String, Said)>>,
}

const REUSE_MS: f64 = 60_000.0;
const ACCOUNT_CHANGED: &str = "The account for this saved profile changed. Reset claims now shows the new account. Earlier claim results stay with the previous account.";

impl ResetCtx {
    pub fn new() -> Self {
        ResetCtx {
            account: RwSignal::new(String::new()),
            provider: RwSignal::new(String::new()),
            load: RwSignal::new(Load::Idle),
            dialog: RwSignal::new(Dialog::Closed),
            said: RwSignal::new(None),
            unresolved: RwSignal::new(Unresolved::default()),
            checking: RwSignal::new(false),
            cache: StoredValue::new(vec![]),
            locks: StoredValue::new(vec![]),
            aliases: StoredValue::new(vec![]),
            results: StoredValue::new(vec![]),
        }
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog
            .try_with_untracked(|d| *d != Dialog::Closed)
            .unwrap_or(false)
    }

    /// Details shows another account (or closes: ""). Whatever was open for the last one goes.
    pub fn show(&self, account: &str, provider: &str, fetch: bool) {
        if self.account.get_untracked() == account {
            return;
        }
        self.account.set(account.to_string());
        self.provider.set(provider.to_string());
        self.dialog.set(Dialog::Closed);
        self.said.set(self.said_of(account));
        let lock = self.lock_of(account);
        let blocked = lock.blocks();
        self.unresolved.set(lock);
        self.load.set(Load::Idle);
        if fetch && !account.is_empty() {
            let now = js_sys::Date::now();
            // an account with a claim still unresolved is always read fresh
            let cached = self.cache.with_value(|c| {
                c.iter()
                    .find(|(a, at, _)| a == account && now - at < REUSE_MS && !blocked)
                    .map(|(_, at, i)| (*at, i.clone()))
            });
            match cached {
                Some((started, inv)) => self.take(account, inv, started),
                None => self.refresh(),
            }
        }
    }

    fn lock_key(&self, account: &str) -> String {
        self.aliases
            .with_value(|a| {
                a.iter()
                    .find(|(route, _)| route == account)
                    .map(|(_, key)| key.clone())
            })
            .unwrap_or_else(|| account.to_string())
    }
    fn bind_alias(&self, account: &str, canonical: &str) {
        let known = self
            .aliases
            .with_value(|a| a.iter().any(|(route, _)| route == account));
        let previous_key = self.lock_key(account);
        if known && previous_key == canonical {
            return;
        }
        let previous = self.lock_of(account);
        let previous_result = self.said_of(account);
        self.aliases.update_value(|a| {
            a.retain(|(route, _)| route != account);
            a.push((account.into(), canonical.into()));
        });
        // Only first-time resolution moves route-local state. A replacement login owns
        // a different lock/result; the original canonical account keeps its own state.
        if !known {
            self.update_lock(canonical, |u| {
                for key in previous.settled {
                    u.apply(Lock::Settle { key });
                }
                if previous.latched {
                    u.apply(Lock::Latch {
                        key: previous.key,
                        proof_after_ms: previous.proof_after_ms,
                    });
                }
            });
            if let Some(said) = previous_result {
                self.say_for(canonical, said);
            }
            if previous_key != canonical {
                self.locks
                    .update_value(|l| l.retain(|(key, _)| key != &previous_key));
                self.results
                    .update_value(|r| r.retain(|(key, _)| key != &previous_key));
            }
        }
        if self.account.get_untracked() == account {
            self.unresolved.set(self.lock_of(canonical));
            self.said.set(self.said_of(canonical));
            if known {
                self.dialog.set(Dialog::Closed);
                self.said.set(Some(Said {
                    text: ACCOUNT_CHANGED.into(),
                    bad: false,
                    check: false,
                }));
                self.cache
                    .update_value(|c| c.retain(|(route, _, _)| route != account));
            }
        }
    }
    fn lock_of(&self, account: &str) -> Unresolved {
        let account = self.lock_key(account);
        self.locks
            .try_with_value(|l| {
                l.iter()
                    .find(|(a, _)| a == &account)
                    .map(|(_, u)| u.clone())
            })
            .flatten()
            .unwrap_or_default()
    }

    /// Changes one account's lock, shown or not (an answer may arrive after Details moved on).
    fn update_lock<R>(&self, account: &str, f: impl FnOnce(&mut Unresolved) -> R) -> R {
        let mut lock = self.lock_of(account);
        let key = self.lock_key(account);
        let out = f(&mut lock);
        let _ = self.locks.try_update_value(|l| {
            l.retain(|(a, _)| a != &key);
            if lock != Unresolved::default() {
                l.push((key.clone(), lock.clone()));
            }
        });
        if self
            .account
            .try_get_untracked()
            .is_some_and(|shown| self.lock_key(&shown) == key)
        {
            let _ = self.unresolved.try_set(lock);
        }
        out
    }

    fn read_lock(&self, account: &str, pending: Option<String>, started_ms: f64) {
        let cleared = self.update_lock(account, |u| u.read(pending, started_ms));
        if cleared && self.said_of(account).is_some_and(|s| s.check) {
            let said = Said {
                text: SETTLED_ELSEWHERE.into(),
                bad: false,
                check: false,
            };
            let key = self.remember_said(account, &said);
            // Reconcile the account's saved result even while a changed-login notice is visible.
            if self
                .account
                .try_get_untracked()
                .is_some_and(|shown| self.lock_key(&shown) == key)
                && self
                    .said
                    .try_with_untracked(|s| s.as_ref().is_some_and(|s| s.check))
                    == Some(true)
            {
                let _ = self.said.try_set(Some(said));
            }
        }
    }

    fn take(&self, account: &str, inv: Inventory, started_ms: f64) {
        if let Some(key) = &inv.canonical_account_key {
            self.bind_alias(account, key);
        }
        self.read_lock(account, inv.pending_attempt.clone(), started_ms);
        self.load.set(Load::Ready(inv));
    }

    /// Reads the inventory again (fresh at the server). Keeps the rows on screen while it reads.
    pub fn refresh(&self) {
        let me = *self;
        let account = self.account.get_untracked();
        if account.is_empty() {
            return;
        }
        if !matches!(self.load.get_untracked(), Load::Ready(_)) {
            self.load.set(Load::Loading);
        }
        let started = js_sys::Date::now();
        spawn_local(async move {
            let got = load(&account).await;
            if let Answer::Ok(inv) = &got {
                // the lock learns from every read, even one for an account no longer shown
                let _ = me.cache.try_update_value(|c| {
                    c.retain(|(a, _, _)| *a != account);
                    c.push((account.clone(), started, inv.clone()));
                });
                if me.account.try_get_untracked().as_deref() != Some(account.as_str()) {
                    if let Some(key) = &inv.canonical_account_key {
                        me.bind_alias(&account, key);
                    }
                    me.read_lock(&account, inv.pending_attempt.clone(), started);
                    return;
                }
            }
            if me.account.try_get_untracked().as_deref() != Some(account.as_str()) {
                return;
            }
            match got {
                Answer::Ok(inv) => me.take(&account, inv, started),
                Answer::Refused(r) if r.code == "browser-session-required" => {
                    me.load.set(Load::Failed(
                        "Resets can be used only from a signed-in dashboard session.".into(),
                    ))
                }
                Answer::Refused(_) | Answer::Lost => me.load.set(Load::Failed(
                    "AAC couldn't read which resets it can use right now.".into(),
                )),
                Answer::Failed(text) => me.load.set(Load::Failed(text)),
            }
        });
    }

    fn said_of(&self, account: &str) -> Option<Said> {
        let key = self.lock_key(account);
        self.results.with_value(|r| {
            r.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, said)| said.clone())
        })
    }

    fn remember_said(&self, account: &str, said: &Said) -> String {
        let key = self.lock_key(account);
        let _ = self.results.try_update_value(|r| {
            r.retain(|(k, _)| *k != key);
            r.push((key.clone(), said.clone()));
        });
        key
    }

    fn say_for(&self, account: &str, said: Said) {
        let key = self.remember_said(account, &said);
        if self
            .account
            .try_get_untracked()
            .is_some_and(|shown| self.lock_key(&shown) == key)
        {
            let _ = self.said.try_set(Some(said));
        }
    }

    #[cfg(test)]
    fn say(&self, said: Said) {
        self.say_for(&self.account.get_untracked(), said);
    }

    /// Use reset: asks prepare only. Nothing goes to the provider until Use this reset.
    pub fn open(&self, grant_key: String, ready: u32) {
        if self.unresolved.with_untracked(Unresolved::blocks) || self.dialog_open() {
            return;
        }
        let me = *self;
        let account = self.account.get_untracked();
        let identity = self.lock_key(&account);
        let provider = self.provider.get_untracked();
        self.said.set(None);
        self.dialog.set(Dialog::Opening {
            grant_key: grant_key.clone(),
        });
        let sent_at = js_sys::Date::now();
        spawn_local(async move {
            let got = prepare(&account, &grant_key).await;
            me.update_lock(&identity, |u| {
                u.apply(prepare_lock(&got, js_sys::Date::now()))
            });
            let still = me.account.try_get_untracked().as_deref() == Some(account.as_str())
                && me.lock_key(&account) == me.lock_key(&identity)
                && me.dialog.try_with_untracked(
                    |d| matches!(d, Dialog::Opening { grant_key: k } if *k == grant_key),
                ) == Some(true);
            if !still {
                return;
            }
            match got {
                Answer::Ok(prepared)
                    if !prepared.confirmation_token.is_empty()
                        && !prepared.account.label.is_empty() =>
                {
                    let deadline_ms =
                        confirm_deadline(sent_at, prepared.expires_at_ms, js_sys::Date::now());
                    me.dialog.set(Dialog::Open {
                        grant_key,
                        prepared,
                        deadline_ms,
                        ready,
                    });
                }
                Answer::Ok(_) => {
                    me.dialog.set(Dialog::Closed);
                    me.say_for(
                        &identity,
                        Said {
                            text: "The server's answer could not be read. Nothing was sent.".into(),
                            bad: true,
                            check: false,
                        },
                    );
                }
                Answer::Refused(r) => {
                    me.dialog.set(Dialog::Closed);
                    let said = refusal_said(&r, &provider);
                    if said.check || r.reason.as_deref() == Some("grant-changed") {
                        me.refresh();
                    }
                    me.say_for(&identity, said);
                }
                Answer::Lost => {
                    me.dialog.set(Dialog::Closed);
                    me.say_for(
                        &identity,
                        Said {
                            text: "No connection to AI Account Center. Nothing was sent.".into(),
                            bad: true,
                            check: false,
                        },
                    );
                }
                Answer::Failed(text) => {
                    me.dialog.set(Dialog::Closed);
                    me.say_for(
                        &identity,
                        Said {
                            text: format!("{text} Nothing was sent."),
                            bad: true,
                            check: false,
                        },
                    );
                }
            }
        });
    }

    /// Cancel, Escape or a click beside the dialog. Not while the claim is on its way.
    pub fn close(&self) {
        if self
            .dialog
            .with_untracked(|d| matches!(d, Dialog::Sending { .. }))
        {
            return;
        }
        self.dialog.set(Dialog::Closed);
    }

    /// Use this reset: the one claim, with the confirmation prepare gave, if it is still inside its life.
    pub fn confirm(&self) {
        let Dialog::Open {
            grant_key,
            prepared,
            deadline_ms,
            ready,
        } = self.dialog.get_untracked()
        else {
            return;
        };
        if js_sys::Date::now() >= deadline_ms || self.unresolved.with_untracked(Unresolved::blocks)
        {
            return;
        }
        let me = *self;
        let account = self.account.get_untracked();
        let identity = self.lock_key(&account);
        let provider = self.provider.get_untracked();
        self.dialog.set(Dialog::Sending {
            grant_key: grant_key.clone(),
            prepared: prepared.clone(),
            ready,
        });
        let _ = self
            .cache
            .try_update_value(|c| c.retain(|(a, _, _)| *a != account));
        let sent_at = js_sys::Date::now();
        spawn_local(async move {
            let got = claim(&account, &grant_key, &prepared.confirmation_token).await;
            // latched first, for this account even if Details has moved on: a possible send blocks every Use reset
            me.update_lock(&identity, |u| {
                u.apply(claim_lock(&got, sent_at, js_sys::Date::now()))
            });
            match got {
                Answer::Ok(result) => me.say_for(&identity, result_said(&result, &provider)),
                Answer::Refused(r) => me.say_for(&identity, refusal_said(&r, &provider)),
                // The claim may have reached the provider: the fresh read below names the pending attempt.
                Answer::Lost => me.say_for(&identity, Said {
                    text: "The connection dropped while sending, so the reset may have been used. AAC is reading the account again."
                        .into(),
                    bad: true,
                    check: true,
                }),
                Answer::Failed(text) => me.say_for(&identity, Said { text: format!("{text} The reset may have been used; check again."), bad: true, check: true }),
            }
            if me.account.try_get_untracked().as_deref() != Some(account.as_str())
                || me.lock_key(&account) != me.lock_key(&identity)
            {
                return;
            }
            let _ = me.dialog.try_set(Dialog::Closed);
            me.refresh();
        });
    }

    /// Check again: a read only. It never sends another claim.
    pub fn check_again(&self) {
        if self.checking.get_untracked() {
            return;
        }
        let Some(attempt) = self.unresolved.with_untracked(|u| u.key.clone()) else {
            // No key yet (a lost answer): the fresh read names it when there is one.
            self.refresh();
            return;
        };
        let me = *self;
        let account = self.account.get_untracked();
        let identity = self.lock_key(&account);
        let provider = self.provider.get_untracked();
        self.checking.set(true);
        spawn_local(async move {
            let got = match check(&account, &attempt).await {
                Answer::Ok(r) if r.attempt_key != attempt => Answer::Failed(
                    "AAC couldn't match this result to the claim being checked.".into(),
                ),
                got => got,
            };
            me.update_lock(&identity, |u| {
                u.apply(check_lock(&got, js_sys::Date::now()))
            });
            match got {
                Answer::Ok(result) => me.say_for(&identity, result_said(&result, &provider)),
                Answer::Refused(r) => me.say_for(&identity, refusal_said(&r, &provider)),
                Answer::Lost => me.say_for(
                    &identity,
                    Said {
                        text: "No connection to AI Account Center. The claim is still unconfirmed."
                            .into(),
                        bad: true,
                        check: true,
                    },
                ),
                Answer::Failed(text) => me.say_for(
                    &identity,
                    Said {
                        text,
                        bad: true,
                        check: true,
                    },
                ),
            }
            if me.account.try_get_untracked().as_deref() != Some(account.as_str())
                || me.lock_key(&account) != me.lock_key(&identity)
            {
                return;
            }
            me.checking.set(false);
            me.refresh();
        });
    }
}

impl Default for ResetCtx {
    fn default() -> Self {
        Self::new()
    }
}

// ---- the views ----------------------------------------------------------------------------------------------------

/// The "Resets and balances" fact group.
#[component]
pub fn ResetsGroup(
    rc: ResetCtx,
    mode: String,
    reading_rows: Vec<ReadingRow>,
    balances: Vec<crate::details::AmountView>,
) -> impl IntoView {
    let mode_c = mode.clone();
    let no_reading_rows = reading_rows.is_empty();
    let rows = Memo::new(move |_| {
        let now = js_sys::Date::now();
        let provider = rc.provider.get();
        rc.load.with(|l| match l {
            Load::Ready(inv) if inv.status == "ok" || inv.status == "unsupported" => {
                rows_from_inventory(inv, &provider, now)
            }
            _ => rows_from_reading(&reading_rows, now),
        })
    });
    let has_rows = move || rows.with(|r| !r.is_empty());
    let mode_s = StoredValue::new(mode.clone());
    let summary_v = move || {
        let (lead, rest) = rows.with(|r| summary(r, &mode_s.get_value(), &day));
        view! { <p class="fg-sum"><b>{lead}</b>{(!rest.is_empty()).then(|| format!(" {rest}"))}</p> }
    };
    let head = move || {
        rows.with(|r| head_end(r))
            .map(|(ready, tail)| view! { <span class="end"><b>{ready}</b>{tail}</span> })
    };
    // Why there are no buttons, when the rows came from the reading.
    let why = move || {
        let mode = mode_s.get_value();
        let provider = rc.provider.get();
        if mode == "zai" {
            return rows.with(|r| !r.is_empty()).then(|| {
                (
                    "Z.ai cards are used in Z.ai; AAC only lists them.".to_string(),
                    false,
                )
            });
        }
        if mode != "claim" || !has_rows() {
            return None;
        }
        rc.load.with(|l| match l {
            Load::Loading | Load::Idle => Some((
                "Checking which of these AAC can use\u{2026}".to_string(),
                false,
            )),
            Load::Failed(text) => Some((
                format!("{text} They can't be used from here until it can."),
                true,
            )),
            Load::Ready(inv) if inv.status != "ok" => Some((
                format!(
                    "{} They can't be used from here.",
                    why_not(
                        if inv.reason.is_empty() {
                            "unavailable"
                        } else {
                            &inv.reason
                        },
                        &provider
                    )
                ),
                true,
            )),
            Load::Ready(_) => None,
        })
    };
    let pending_line = move || {
        let said = rc.said.get();
        let pending = rc.unresolved.with(Unresolved::blocks);
        let said = said.or_else(|| {
            pending.then(|| Said {
                text: "A reset claim on this account is still unconfirmed. AAC won't use another until it confirms this one."
                    .into(),
                bad: true,
                check: true,
            })
        });
        said.map(|s| {
            let show_check = s.check || pending;
            view! {
                <div class="fg-line fg-said" class:w-crit=s.bad role="status" data-probe="reset-result">
                    <span>{s.text.clone()}</span>
                    {show_check.then(|| view! {
                        <button type="button" class="btn sm" data-probe="resets-check"
                            disabled=move || rc.checking.get()
                            on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); rc.check_again(); }>
                            {move || if rc.checking.get() { "Checking\u{2026}" } else { "Check again" }}
                        </button>
                    })}
                </div>
            }
        })
    };
    let ready_total = Memo::new(move |_| rows.with(|r| counts(r).ready));
    let n_bal = balances.len();
    // An account with balances but no resets of any kind (Cursor credits, say) gets a plain balances group.
    let resets = !mode.is_empty() || !no_reading_rows;
    let title = if resets {
        "Resets and balances"
    } else {
        "Balances, packs and credits"
    };
    view! {
        <section class="fg" data-probe="resets">
            <div class="fg-cap"><h3 class="cap">{title}</h3>{resets.then_some(head)}</div>
            {resets.then_some(summary_v)}
            {move || (mode_c != "qwen" && has_rows()).then(|| view! {
                <table class="ft resets-table">
                    <thead><tr><th class="cap">"State"</th><th class="cap">"Refills"</th><th class="cap r">"Expires"</th></tr></thead>
                    <tbody>
                        {move || rows.get().into_iter().map(|row| view! { <ResetRow rc=rc row=row ready_total=ready_total/> }).collect_view()}
                    </tbody>
                </table>
            })}
            {move || why().map(|(text, bad)| view! { <p class="fg-line" class:w-crit=bad>{text}</p> })}
            {pending_line}
            {(mode == "zai").then(|| view! {
                <p class="fg-line"><a class="btn sm ghost" href="https://z.ai/manage-apikey/subscription" target="_blank" rel="noopener noreferrer">"Use in Z.ai \u{2197}"</a></p>
            })}
            {(n_bal > 0).then(|| view! {
                <dl class="fp num">
                    {balances.into_iter().map(|a| view! {
                        <dt>{a.label.clone()}{(!a.sub.is_empty()).then(|| view! { <small>{a.sub.clone()}</small> })}</dt>
                        <dd class:w-crit=a.spent>{a.value.clone()}{(!a.unit.is_empty()).then(|| view! { " "<span>{a.unit.clone()}</span> })}</dd>
                    }).collect_view()}
                </dl>
            })}
        </section>
    }
}

#[component]
fn ResetRow(rc: ResetCtx, row: Row, ready_total: Memo<u32>) -> impl IntoView {
    let spent = row.phase.spent();
    let now = js_sys::Date::now();
    let expires = row.expires_at_ms.map(when).filter(|s| !s.is_empty());
    let until = row
        .expires_at_ms
        .filter(|_| !spent)
        .map(|at| until_words(at - now))
        .filter(|s| !s.is_empty());
    let grant_key = row.grant_key.clone().filter(|_| row.claimable);
    let probe = format!(
        "reset-row:{}",
        if row.unknown {
            "unknown"
        } else {
            row.phase.word()
        }
    );
    let left = (row.left > 1).then(|| format!("{} left", row.left));
    view! {
        <tr class:spent=spent class:unknown=row.unknown data-probe=probe>
            <td class="w">{row.phase.word()}{left.map(|t| view! { <small>{t}</small> })}</td>
            <td>{row.refills.clone()}{(!row.note.is_empty()).then(|| view! { <small>{row.note.clone()}</small> })}</td>
            <td class="r nw">
                {match expires { Some(t) => view! { <span>{t}</span> }.into_any(), None => view! { <span class="q3">"No expiry given"</span> }.into_any() }}
                {until.map(|t| view! { <small>{t}</small> })}
                {grant_key.map(|gk| {
                    let gk2 = gk.clone();
                    view! {
                        <button type="button" class="btn sm ft-act" data-probe="reset-use"
                            disabled=move || rc.unresolved.with(|u| rc.dialog.with(|d| !use_enabled(true, u, d)))
                            on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); rc.open(gk.clone(), ready_total.get_untracked()); }>
                            {move || if rc.dialog.with(|d| matches!(d, Dialog::Opening { grant_key } if *grant_key == gk2)) { "Opening\u{2026}" } else { "Use reset" }}
                        </button>
                    }
                })}
            </td>
        </tr>
    }
}

/// The confirmation, over the panel: the account as prepare read it, the exact grant, what it refills, its expiry,
/// the cost. Cancel and Escape close it; nothing is sent until Use this reset.
#[component]
pub fn ResetDialog(rc: ResetCtx) -> impl IntoView {
    let now = RwSignal::new(js_sys::Date::now());
    let tick = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
        let _ = now.try_set(js_sys::Date::now());
    });
    let timer = window()
        .set_interval_with_callback_and_timeout_and_arguments_0(
            wasm_bindgen::JsCast::unchecked_ref(tick.as_ref()),
            500,
        )
        .ok();
    tick.forget();
    on_cleanup(move || {
        if let Some(id) = timer {
            window().clear_interval_with_handle(id);
        }
    });
    let keys = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" && rc.dialog_open() {
            e.stop_propagation();
            rc.close();
        }
    });
    on_cleanup(move || keys.remove());
    let open = Memo::new(move |_| {
        rc.dialog
            .with(|d| matches!(d, Dialog::Open { .. } | Dialog::Sending { .. }))
    });
    Effect::new(move |_| {
        if open.get() {
            let focus = wasm_bindgen::closure::Closure::once_into_js(|| {
                if let Some(el) = document()
                    .query_selector(".reset-dialog .btn.primary, .reset-dialog .btn")
                    .ok()
                    .flatten()
                    .and_then(|e| wasm_bindgen::JsCast::dyn_into::<web_sys::HtmlElement>(e).ok())
                {
                    let _ = el.focus();
                }
            });
            let _ = window().request_animation_frame(wasm_bindgen::JsCast::unchecked_ref(&focus));
        }
    });
    move || {
        let (grant_key, prepared, deadline, ready, sending) = match rc.dialog.get() {
            Dialog::Open {
                grant_key,
                prepared,
                deadline_ms,
                ready,
            } => (grant_key, prepared, deadline_ms, ready, false),
            Dialog::Sending {
                grant_key,
                prepared,
                ready,
            } => (grant_key, prepared, f64::MAX, ready, true),
            _ => return None,
        };
        let g = prepared.grant.clone();
        let name = if g.label.is_empty() {
            kind_name(&g.kind).to_string()
        } else {
            g.label.clone()
        };
        let expires = g.expires_at_ms.map(|at| {
            let rel = until_words(at - js_sys::Date::now());
            if rel.is_empty() {
                when(at)
            } else {
                format!("{}, {rel}", when(at))
            }
        });
        let have = ready.max(g.resets_left).max(1);
        let cost = format!(
            "You have {}; this spends one and can't be undone.",
            plural(have, "reset", "resets")
        );
        let expired = move || !sending && now.get() >= deadline;
        let reopen = grant_key.clone();
        Some(view! {
            <div class="ascrim reset-scrim" on:click=move |_| rc.close()>
                <div class="adialog reset-dialog" role="dialog" aria-modal="true" aria-labelledby="rdialog-title" data-probe="reset-dialog"
                    on:click=|ev| ev.stop_propagation()>
                    <h2 id="rdialog-title">{format!("Use one {name}?")}</h2>
                    <dl class="fp top">
                        <dt>"Account"</dt><dd data-probe="reset-dialog-account">{prepared.account.label.clone()}</dd>
                        <dt>"Reset"</dt><dd>{name.clone()}</dd>
                        <dt>"Refills"</dt><dd>{g.refills_text.clone()}</dd>
                        <dt>"Expires"</dt><dd>{expires.unwrap_or_else(|| "No expiry given".into())}</dd>
                    </dl>
                    <p class="a-warn">{cost}</p>
                    {move || if sending {
                        view! { <p class="a-status" role="status">"Using the reset and reading the account again\u{2026}"</p> }.into_any()
                    } else if expired() {
                        view! { <p class="a-status w-crit" role="status" data-probe="reset-dialog-expired">"This confirmation expired, open it again."</p> }.into_any()
                    } else {
                        view! { <p class="a-exp tnum">{move || format!("This confirmation is good for {}.", countdown(deadline - now.get()))}</p> }.into_any()
                    }}
                    <div class="a-btns">
                        <button class="btn" aria-disabled=sending.to_string() data-probe="reset-dialog-cancel"
                            on:click=move |_| rc.close()>"Cancel"</button>
                        {move || if expired() {
                            let k = reopen.clone();
                            view! {
                                <button class="btn primary" data-probe="reset-dialog-reopen"
                                    on:click=move |_| { rc.dialog.set(Dialog::Closed); rc.open(k.clone(), ready); }>"Open it again"</button>
                            }.into_any()
                        } else {
                            view! {
                                <button class="btn primary" aria-disabled=sending.to_string() data-probe="reset-dialog-use"
                                    on:click=move |_| { if !sending { rc.confirm(); } }>"Use this reset"</button>
                            }.into_any()
                        }}
                    </div>
                </div>
            </div>
        })
    }
}

/// The "Reset claims" fact group: every claim on this account, newest first, kept forever.
#[component]
pub fn ResetHistory(rc: ResetCtx) -> impl IntoView {
    let all = RwSignal::new(false);
    let rows = Memo::new(move |_| {
        rc.load.with(|l| match l {
            Load::Ready(inv) => history_rows(&inv.history),
            _ => vec![],
        })
    });
    move || {
        let list = rows.get();
        if list.is_empty() {
            return None;
        }
        let n = list.len();
        let last = list[0].clone();
        let shown: Vec<HistoryRow> = if all.get() {
            list
        } else {
            list.into_iter().take(6).collect()
        };
        Some(view! {
            <section class="fg" data-probe="reset-history">
                <div class="fg-cap"><h3 class="cap">"Reset claims"</h3><span class="end"><b>{n}</b>{if n == 1 { " claim" } else { " claims" }}</span></div>
                <p class="fg-sum"><b>{format!("Last: {} on {}.", last.kind, day(last.at_ms))}</b>{format!(" {}. Every claim on this account is kept.", last.result)}</p>
                <table class="ft">
                    <thead><tr><th class="cap">"When"</th><th class="cap">"Reset"</th><th class="cap r">"Result"</th></tr></thead>
                    <tbody>
                        {shown.into_iter().map(|h| view! {
                            <tr><td class="nw">{when(h.at_ms)}</td><td>{h.kind.clone()}</td><td class="r">{h.result}</td></tr>
                        }).collect_view()}
                    </tbody>
                </table>
                {(n > 6).then(|| view! {
                    <p class="fg-line">
                        {move || (!all.get()).then(|| format!("and {} more", n - 6))}
                        <button type="button" class="btn sm ghost" on:click=move |e: web_sys::MouseEvent| { e.stop_propagation(); all.update(|v| *v = !*v); }>
                            {move || if all.get() { "Show fewer" } else { "Show" }}
                        </button>
                    </p>
                })}
            </section>
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_700_000_000_000.0;
    const DAY: f64 = 86_400_000.0;

    fn fake_day(ms: f64) -> String {
        format!("day+{}", ((ms - NOW) / DAY).round() as i64)
    }

    fn inv(json: &str) -> Inventory {
        serde_json::from_str(json).unwrap()
    }

    fn grant(
        kind: &str,
        left: u32,
        days: Option<f64>,
        claimable: bool,
        reason: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "grantKey": format!("{kind}{left}{}", days.unwrap_or(-1.0)), "kind": kind, "label": kind_name(kind),
            "refillsText": "Refills your weekly allowance.", "providerWords": null, "resetsLeft": left,
            "expiresAtMs": days.map(|d| NOW + d * DAY), "claimable": claimable, "claimBlockedReason": reason
        })
    }

    fn inventory(grants: Vec<serde_json::Value>) -> Inventory {
        inv(
            &serde_json::json!({"status":"ok","grants":grants,"history":[],"pendingAttempt":null})
                .to_string(),
        )
    }

    #[test]
    fn claimable_rows_need_a_key_a_known_kind_and_the_server_saying_so() {
        let i = inventory(vec![
            grant("weekly", 1, Some(29.0), true, None),
            grant(
                "weekly",
                1,
                Some(34.0),
                false,
                Some("credential-host-unavailable"),
            ),
            grant("fable", 0, Some(-3.0), false, Some("grant-unavailable")),
            grant("session", 1, Some(2.0), false, Some("grant-unavailable")),
        ]);
        let rows = rows_from_inventory(&i, "claude", NOW);
        let claimable: Vec<bool> = rows.iter().map(|r| r.claimable).collect();
        assert_eq!(claimable, vec![true, false, false, false]);
        // the blocked ready grant says why, in words, not the code
        assert_eq!(rows[1].phase, Phase::Ready);
        assert!(
            rows[1].note.contains("Mac-only sign-in is view-only"),
            "{}",
            rows[1].note
        );
        // a provider-unusable grant is "Not yet", the used one sinks to the bottom
        assert_eq!(rows[2].phase, Phase::NotYet);
        assert_eq!(rows[3].phase, Phase::Used);
        assert!(rows[3].phase.spent());
        // a grant the server says is claimable is still refused here once it has expired
        let late = inventory(vec![grant("weekly", 1, Some(-0.5), true, None)]);
        let r = rows_from_inventory(&late, "claude", NOW);
        assert_eq!((r[0].phase, r[0].claimable), (Phase::Expired, false));
        // and a row with no key can never be used
        let mut nokey = inventory(vec![grant("weekly", 1, Some(3.0), true, None)]);
        nokey.grants[0].grant_key.clear();
        assert!(!rows_from_inventory(&nokey, "claude", NOW)[0].claimable);
    }

    #[test]
    fn unknown_kinds_show_the_providers_words_and_cannot_be_used() {
        let mut g = grant("unknown", 1, Some(5.0), true, Some("unknown-grant-kind"));
        g["label"] = "Other reset".into();
        g["providerWords"] = "seven_day_opus".into();
        let mut unnamed = grant("unknown", 2, None, false, Some("unknown-grant-kind"));
        unnamed["grantKey"] = "x".into();
        let rows = rows_from_inventory(
            &inventory(vec![g, unnamed, grant("weekly", 1, Some(9.0), true, None)]),
            "claude",
            NOW,
        );
        assert_eq!(rows[0].refills, "Weekly limit");
        assert!(rows[1].unknown && rows[2].unknown);
        assert_eq!(rows[1].refills, "\u{201c}seven_day_opus\u{201d}");
        assert_eq!(rows[2].refills, "A reset of another kind");
        for r in &rows[1..] {
            assert!(!r.claimable);
            assert_eq!(
                r.note,
                "AAC doesn't know what this refills yet; it can't be used from here."
            );
        }
        // never added to a known kind's count
        let c = counts(&rows);
        assert_eq!((c.ready, c.unknown), (1, 3));
        let (lead, rest) = summary(&rows, "claim", &fake_day);
        assert_eq!(lead, "1 rate-limit reset ready.");
        assert_eq!(
            rest,
            "It expires day+9. 3 more are of a kind AAC doesn't know yet, so it can't use them."
        );
    }

    #[test]
    fn state_words_are_plain() {
        assert_eq!(Phase::Ready.word(), "Ready");
        assert_eq!(Phase::NotYet.word(), "Not yet");
        assert_eq!(Phase::Used.word(), "Used");
        assert_eq!(Phase::Expired.word(), "Expired");
        let reading = vec![
            ReadingRow {
                key: "a".into(),
                refills: "Weekly, Fable included usage".into(),
                state: "ready".into(),
                left: 1,
                count: 1,
                expires_at_ms: Some(NOW + 29.0 * DAY),
            },
            ReadingRow {
                key: "b".into(),
                refills: "".into(),
                state: "saved".into(),
                left: 1,
                count: 1,
                expires_at_ms: None,
            },
            ReadingRow {
                key: "c".into(),
                refills: "Fable included usage".into(),
                state: "used".into(),
                left: 0,
                count: 1,
                expires_at_ms: Some(NOW + 100.0 * DAY),
            },
            ReadingRow {
                key: "d".into(),
                refills: "Weekly".into(),
                state: "ready".into(),
                left: 1,
                count: 1,
                expires_at_ms: Some(NOW - 1.0),
            },
        ];
        let rows = rows_from_reading(&reading, NOW);
        let words: Vec<&str> = rows.iter().map(|r| r.phase.word()).collect();
        assert_eq!(words, vec!["Ready", "Not yet", "Used", "Expired"]);
        assert_eq!(rows[1].refills, "Not reported");
        assert!(rows.iter().all(|r| !r.claimable && r.grant_key.is_none()));
    }

    #[test]
    fn the_summary_sentence_answers_first() {
        let two = rows_from_inventory(
            &inventory(vec![
                grant("weekly", 1, Some(34.0), true, None),
                grant("weekly", 1, Some(29.0), true, None),
                grant("fable", 0, Some(100.0), false, None),
            ]),
            "claude",
            NOW,
        );
        assert_eq!(
            summary(&two, "claim", &fake_day),
            (
                "2 rate-limit resets ready.".into(),
                "The first expires day+29.".into()
            )
        );
        assert_eq!(head_end(&two), Some((2, " ready, 1 used".into())));
        // soonest first, used last
        assert_eq!(two[0].expires_at_ms, Some(NOW + 29.0 * DAY));
        assert!(two[2].phase.spent());

        let none_left = rows_from_inventory(
            &inventory(vec![grant("weekly", 0, None, false, None)]),
            "claude",
            NOW,
        );
        assert_eq!(
            summary(&none_left, "claim", &fake_day).0,
            "No rate-limit resets left."
        );
        assert_eq!(
            summary(&[], "claim", &fake_day).0,
            "No rate-limit resets listed."
        );
        let saved = rows_from_inventory(
            &inventory(vec![grant(
                "session",
                1,
                None,
                false,
                Some("grant-unavailable"),
            )]),
            "claude",
            NOW,
        );
        assert_eq!(
            summary(&saved, "claim", &fake_day),
            (
                "1 rate-limit reset saved, not usable yet.".into(),
                String::new()
            )
        );
        assert_eq!(
            summary(&[], "qwen", &fake_day),
            (
                "No resets on this plan.".into(),
                "Qwen's only top-up is a Credit Pack, bought in the Qwen console.".into()
            )
        );
        let cards = rows_from_reading(
            &[ReadingRow {
                key: "z".into(),
                refills: "5-hour quota".into(),
                state: "ready".into(),
                left: 3,
                count: 3,
                expires_at_ms: Some(NOW + 2.0 * DAY),
            }],
            NOW,
        );
        assert_eq!(
            summary(&cards, "zai", &fake_day),
            ("3 reset cards ready.".into(), "The first expires day+2. Use them in Z.ai; AAC lists them but can't use them from here.".into())
        );
    }

    #[test]
    fn results_say_what_the_verification_read_saw() {
        let ok: ClaimResult = serde_json::from_str(
            r#"{"attemptKey":"a","outcome":"claimed","verification":"confirmed","providerSuccess":true,"windows":[{"id":"seven_day","usedPercent":0.4}]}"#,
        )
        .unwrap();
        assert_eq!(
            result_said(&ok, "claude").text,
            "Reset applied: Weekly is back to 0% used."
        );
        let unconfirmed = ClaimResult {
            outcome: "claimed-unconfirmed".into(),
            provider_success: true,
            ..Default::default()
        };
        let s = result_said(&unconfirmed, "claude");
        assert_eq!(
            s.text,
            "Reset was applied, but AAC could not confirm the new limits. Check again."
        );
        assert!(s.check && s.bad);
        let lost = ClaimResult {
            outcome: "claimed-unconfirmed".into(),
            ..Default::default()
        };
        assert!(result_said(&lost, "codex").check);
        let nothing = ClaimResult {
            outcome: "nothing-to-reset".into(),
            ..Default::default()
        };
        assert!(
            result_said(&nothing, "codex")
                .text
                .starts_with("Nothing to reset right now")
        );
        let used = ClaimResult {
            outcome: "already-used".into(),
            ..Default::default()
        };
        assert_eq!(
            result_said(&used, "claude").text,
            "That reset was already used."
        );
        assert!(!result_said(&used, "claude").check);
        assert_eq!(window_name("secondary", "codex"), "Weekly");
    }

    #[test]
    fn refusals_word_the_allowlisted_code_and_a_lost_write_blocks_another() {
        let r = |reason: Option<&str>, sent: bool| Refusal {
            code: "reset-state-unavailable".into(),
            reason: reason.map(Into::into),
            sent,
        };
        assert_eq!(
            refusal_said(&r(Some("confirmation-invalid"), false), "claude").text,
            "This confirmation expired, open it again."
        );
        assert!(refusal_said(&r(Some("check-again-required"), false), "claude").check);
        assert!(refusal_said(&r(None, true), "claude").check);
        assert!(!refusal_said(&r(None, false), "claude").check);
        assert!(
            refusal_said(&r(Some("cooldown"), false), "codex")
                .text
                .ends_with("Nothing was sent.")
        );
        let device = Refusal {
            code: "browser-session-required".into(),
            ..Default::default()
        };
        assert!(
            refusal_said(&device, "claude")
                .text
                .contains("Paired apps can only view resets")
        );
    }

    #[test]
    fn the_confirmation_ends_no_later_than_the_server_says() {
        let sent = NOW;
        assert_eq!(confirm_deadline(sent, None, NOW), NOW + 110_000.0);
        // the server's own end, when its clock agrees, can only shorten it
        assert_eq!(
            confirm_deadline(sent, Some(NOW + 60_000.0), NOW),
            NOW + 50_000.0
        );
        assert_eq!(
            confirm_deadline(sent, Some(NOW + 500_000.0), NOW),
            NOW + 110_000.0
        );
        assert_eq!(countdown(65_100.0), "1:06");
        assert_eq!(countdown(-5.0), "0:00");
        assert_eq!(until_words(29.2 * DAY), "in 29 days");
        assert_eq!(until_words(5.5 * 3_600_000.0), "in 5 h");
        assert_eq!(until_words(30_000.0), "in 1 min");
        assert_eq!(until_words(-1.0), "");
    }

    #[test]
    fn history_lists_each_claim_once_with_its_latest_result() {
        let events: Vec<HistoryEvent> = serde_json::from_str(
            r#"[
              {"atMs":1,"event":"prepare","outcome":"prepared","details":{"kind":"weekly"}},
              {"atMs":2,"event":"attempt","outcome":"claimed-unconfirmed","details":{"kind":"weekly","idempotencyHash":"h1"}},
              {"atMs":3,"event":"verification","outcome":"claimed-unconfirmed","details":{"kind":"weekly","idempotencyHash":"h1"}},
              {"atMs":9,"event":"verification","outcome":"claimed","details":{"kind":"weekly","idempotencyHash":"h1"}},
              {"atMs":20,"event":"attempt","outcome":"claimed-unconfirmed","details":{"kind":"banked","idempotencyHash":"h2"}}
            ]"#,
        )
        .unwrap();
        let rows = history_rows(&events);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].kind.as_str(), rows[0].result),
            ("Banked reset", "Unconfirmed")
        );
        assert_eq!(
            (rows[1].kind.as_str(), rows[1].result, rows[1].at_ms),
            ("Weekly reset", "Used, confirmed", 2.0)
        );
    }

    fn unconfirmed(key: &str) -> Answer<ClaimResult> {
        Answer::Ok(ClaimResult {
            attempt_key: key.into(),
            outcome: "claimed-unconfirmed".into(),
            verification: "unconfirmed".into(),
            provider_success: true,
            windows: vec![],
        })
    }

    fn settled(outcome: &str) -> Answer<ClaimResult> {
        Answer::Ok(ClaimResult {
            attempt_key: "k".into(),
            outcome: outcome.into(),
            verification: "confirmed".into(),
            ..Default::default()
        })
    }

    /// Review U1: every answer that may mean a send happened blocks every Use reset at once, before any read names
    /// the attempt, and a read that started too early (or before the answer) can't unblock it.
    #[test]
    fn an_ambiguous_claim_answer_blocks_every_use_reset_until_it_is_settled() {
        let rows = rows_from_inventory(
            &inventory(vec![
                grant("weekly", 1, Some(9.0), true, None),
                grant("weekly", 1, Some(19.0), true, None),
            ]),
            "claude",
            NOW,
        );
        let usable = |u: &Unresolved| {
            rows.iter()
                .filter(|r| use_enabled(r.claimable, u, &Dialog::Closed))
                .count()
        };
        let sent = NOW;
        let answered = NOW + 2_000.0;
        let ambiguous = [
            Answer::Lost,
            Answer::Failed("The server answered 502.".into()),
            Answer::Refused(Refusal {
                code: "reset-state-unavailable".into(),
                reason: None,
                sent: true,
            }),
            Answer::Refused(Refusal {
                code: "reset-state-unavailable".into(),
                reason: Some("check-again-required".into()),
                sent: false,
            }),
            Answer::Ok(ClaimResult::default()),
            unconfirmed("k1"),
        ];
        for answer in ambiguous {
            let mut u = Unresolved::default();
            assert_eq!(usable(&u), 2);
            u.apply(claim_lock(&answer, sent, answered));
            assert_eq!(usable(&u), 0, "blocked at once after {answer:?}");
            // the automatic read, started before the answer arrived, saw no pending attempt: still blocked
            assert!(!u.read(None, answered - 1.0), "{answer:?}");
            assert_eq!(
                usable(&u),
                0,
                "an older empty read can't unblock after {answer:?}"
            );
        }

        // A lost answer: even a read started after it proves nothing while the claim could still reserve the account.
        let mut lost = Unresolved::default();
        lost.apply(claim_lock(&Answer::Lost, sent, answered));
        assert!(!lost.read(None, answered + 1_000.0));
        assert!(lost.blocks());
        // the read names the attempt; Check again settles it, and Use reset comes back
        lost.read(Some("k2".into()), answered + 2_000.0);
        assert_eq!(lost.key.as_deref(), Some("k2"));
        let mut confirmed = match settled("claimed") {
            Answer::Ok(r) => r,
            _ => unreachable!(),
        };
        confirmed.attempt_key = "k2".into();
        lost.apply(check_lock(&Answer::Ok(confirmed), answered + 3_000.0));
        assert_eq!(usable(&lost), 2);
        // a read that started before that settle can't bring the settled key back
        assert!(!lost.read(Some("k2".into()), answered + 2_500.0));
        assert_eq!(usable(&lost), 2);
        // and once the confirmation can no longer be used, a fresh empty read clears a lost answer by itself
        let mut late = Unresolved::default();
        late.apply(claim_lock(&Answer::Lost, sent, answered));
        assert!(late.read(None, sent + CONFIRM_LIFE_MS));
        assert_eq!(usable(&late), 2);

        // A known key is never replaced by an older empty read; a newer one shows it was settled elsewhere.
        let mut known = Unresolved::default();
        known.apply(claim_lock(&unconfirmed("k3"), sent, answered));
        assert!(!known.read(None, answered - 500.0));
        assert_eq!(known.key.as_deref(), Some("k3"));
        assert!(known.blocks());
        known.apply(check_lock(&unconfirmed("k3"), answered + 100.0));
        assert!(known.blocks(), "a check that is still unconfirmed keeps it");
        assert!(known.read(None, answered + 200.0));
        assert!(!known.blocks() && known.key.is_none());

        // Settled answers and nothing-sent refusals don't latch.
        for answer in [
            settled("claimed"),
            settled("already-used"),
            settled("nothing-to-reset"),
            Answer::Refused(Refusal {
                code: "reset-state-unavailable".into(),
                reason: Some("confirmation-invalid".into()),
                sent: false,
            }),
        ] {
            let mut u = Unresolved::default();
            u.apply(claim_lock(&answer, sent, answered));
            assert_eq!(usable(&u), 2, "{answer:?}");
        }
        // Check again that is lost or refused keeps the lock; prepare told of another unresolved claim latches it.
        let mut u = Unresolved::default();
        u.apply(claim_lock(&unconfirmed("k4"), sent, answered));
        u.apply(check_lock(&Answer::Lost, answered + 1.0));
        u.apply(check_lock(&Answer::Failed("x".into()), answered + 2.0));
        assert!(u.blocks());
        let mut p = Unresolved::default();
        p.apply(prepare_lock(
            &Answer::Refused(Refusal {
                code: "reset-state-unavailable".into(),
                reason: Some("check-again-required".into()),
                sent: false,
            }),
            answered,
        ));
        assert_eq!(usable(&p), 0);
        // and an open dialog blocks the rows too
        assert!(!use_enabled(
            true,
            &Unresolved::default(),
            &Dialog::Opening {
                grant_key: "g".into()
            }
        ));
    }

    #[test]
    fn canonical_aliases_share_the_spend_lock_and_pending_recovery() {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            let canonical = |pending: serde_json::Value| {
                inv(&serde_json::json!({
                    "status":"ok", "canonicalAccountKey":"canonical", "pendingAttempt":pending
                })
                .to_string())
            };
            rc.show("a", "codex", false);
            rc.take("a", canonical(serde_json::Value::Null), 1.0);
            rc.show("b", "codex", false);
            rc.take("b", canonical(serde_json::Value::Null), 2.0);
            rc.update_lock("a", |u| {
                u.apply(Lock::Latch {
                    key: Some("attempt".into()),
                    proof_after_ms: 10.0,
                })
            });
            assert!(rc.unresolved.get_untracked().blocks());
            assert!(!use_enabled(
                true,
                &rc.unresolved.get_untracked(),
                &Dialog::Closed
            ));
            rc.take("b", canonical(serde_json::json!("attempt")), 11.0);
            assert!(rc.lock_of("a").blocks());
            rc.show("a", "codex", false);
            assert!(rc.unresolved.get_untracked().blocks());
            rc.take("a", canonical(serde_json::Value::Null), 12.0);
            assert!(!rc.lock_of("b").blocks());
        });
    }

    #[test]
    fn nulls_and_missing_fields_read_safely() {
        let i = inv(
            r#"{"status":"unavailable","reason":null,"grants":null,"history":null,"pendingAttempt":null}"#,
        );
        assert!(i.grants.is_empty() && i.pending_attempt.is_none());
        let p: Prepared = serde_json::from_str(r#"{"account":{"label":"a@example.test"},"confirmationToken":"t","expiresAtMs":5,"grant":{"kind":"weekly"},"costText":"x"}"#).unwrap();
        assert_eq!(p.account.label, "a@example.test");
    }
}

#[cfg(test)]
#[path = "reset_attempt_tests.rs"]
mod attempt_tests;

#[cfg(test)]
#[path = "reset_identity_tests.rs"]
mod identity_tests;
