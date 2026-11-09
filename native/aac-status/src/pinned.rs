//! The pinned-update composer: the status update each pinned thread would give if its owner
//! asked "status?", every word decided here (PINNED-THREADS.md section 1).
//!
//! It is a pure function over three kinds of input, all already validated by their own
//! readers: the merged pins (this crate, `pins::merge_pins`), the `agent-reports` views the
//! TypeScript Status reader builds (`thread@1` matched by `agent.t3Thread`, `tasks.json`
//! found by the thread's project and stream, the open cards), and the `agent-reports/status@1`
//! documents read from the share. Nothing is recomputed from raw runs, nothing is read from
//! disk here, and every unknown is said in words ("No task list.", "Can't tell.") — a
//! missing piece is never invented as a zero or a no.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// How far back the loop rule looks (PINNED-THREADS.md section 3: "in the last 24 hours").
const LOOP_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;

/// Past this age without a report a pinned thread says it stopped reporting, whatever its
/// heartbeat said (section 1: "past its time-to-live, or a day").
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// A review verdict that passed, by any spelling the boards show: the published schema's
/// `pass` and its sibling words, plus every wording that starts with `pass` (`PASS WITH
/// FOLLOW-UPS`, `PASS WITH NOTES`, `PASS WITH NONBLOCKING FOLLOW-UPS`), so a passing review
/// with follow-ups named never reads as a failing round.
const PASSING_VERDICTS: [&str; 8] = [
    "pass", "passed", "approve", "approved", "approval", "lgtm", "clean", "ship",
];

/// A review verdict that failed, the spellings the boards and reviewers write.
const FAILING_VERDICTS: [&str; 6] = ["fail", "failed", "block", "blocked", "changes requested", "rejected"];

/// How one review verdict reads for the loop rule (section 3): it passed, it failed, or it
/// is unclear. Free text a reviewer may write is classified trimmed and case-insensitive;
/// every other wording is unclear, and an unclear verdict never counts toward a loop.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    Pass,
    Fail,
    Unknown,
}

fn classify_verdict(verdict: &str) -> Verdict {
    let text = verdict.trim().to_lowercase();
    if text.starts_with("pass") || PASSING_VERDICTS.contains(&text.as_str()) {
        return Verdict::Pass;
    }
    if FAILING_VERDICTS.contains(&text.as_str()) {
        return Verdict::Fail;
    }
    Verdict::Unknown
}

/// The loop rule's counts (section 3): 3 or more failing reviews in a row, or 4 or more
/// failing rounds since a passing one. A pass (or an unclear verdict, which never counts as
/// a round) starts the count over, so the two measure the same stretch of a board whose
/// reviews are written in order; both are checked anyway.
const LOOP_ROW_MIN: usize = 3;
const LOOP_ROUNDS_MIN: usize = 4;

/// The fixed labels a machine gets in a sentence, as the Status page names them.
fn machine_label(machine: &str) -> String {
    match machine {
        "vm" => "the VM".to_string(),
        "mac" => "the Mac".to_string(),
        "windows" => "Windows".to_string(),
        "nas1" => "Nas1".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Input (what the TypeScript side sends; every field already validated by its own reader)

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeInput {
    pub now_ms: i64,
    /// The dashboard time zone's UTC offset in minutes at `now_ms`, for clock times.
    pub utc_offset_minutes: i64,
    #[serde(default)]
    pub pins: Vec<PinIn>,
    #[serde(default)]
    pub machine_problems: Vec<MachineProblemIn>,
    #[serde(default)]
    pub threads: Vec<ThreadIn>,
    #[serde(default)]
    pub tasks: Vec<TasksIn>,
    #[serde(default)]
    pub open_cards: Vec<CardIn>,
    #[serde(default)]
    pub statuses: Vec<StatusIn>,
}

/// One merged pin (`pins::merge_pins` output, in merged order).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinIn {
    pub machine: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub pinned_at: Json,
    #[serde(default)]
    pub pin_order_key: Json,
}

/// Why one machine's pins are absent this scan: a fixed code, put in words by this crate.
/// Codes: `did_not_answer` (the ssh read or sqlite failed), `shape` (the answer was not the
/// shape T3's state file has), `missing` / `io` / `limit` (this computer's own T3 database).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineProblemIn {
    pub machine: String,
    pub code: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIn {
    pub writer: String,
    pub written_at: String,
    pub written_at_ms: i64,
    pub seq: u64,
    #[serde(default)]
    pub t3_thread: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub doing: Option<String>,
    #[serde(default)]
    pub blocked_on: Option<BlockedIn>,
    #[serde(default)]
    pub progress: Option<ProgressIn>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
    /// The file's current problem; the doc above is the last good copy. `problem_since_ms`
    /// is when that problem was first seen.
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub problem_since_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedIn {
    pub kind: String,
    pub what: String,
    #[serde(default)]
    pub since: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressIn {
    pub done: u64,
    pub total: u64,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksIn {
    pub writer: String,
    pub written_at_ms: i64,
    pub seq: u64,
    pub project: String,
    pub stream: String,
    #[serde(default)]
    pub stream_label: Option<String>,
    /// The board file's current problem; the items above are its last good copy.
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub items: Vec<ItemIn>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemIn {
    pub id: String,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub reviews: Vec<ReviewIn>,
    #[serde(default)]
    pub landed: Option<LandedIn>,
    #[serde(default)]
    pub blocked_on: Option<BlockedIn>,
    #[serde(default)]
    pub updated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewIn {
    #[serde(default)]
    pub at_ms: Option<i64>,
    #[serde(default)]
    pub verdict: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandedIn {
    #[serde(default)]
    pub at_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardIn {
    pub writer: String,
    pub id: String,
    pub seq: u64,
    pub title: String,
    #[serde(default)]
    pub asked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusIn {
    pub writer: String,
    pub written_at: String,
    pub written_at_ms: i64,
    pub seq: u64,
    pub t3_thread: String,
    pub summary: String,
    #[serde(default)]
    pub percent: Option<u64>,
    #[serde(default)]
    pub done: Vec<ItemLiteIn>,
    #[serde(default)]
    pub left: Vec<ItemLiteIn>,
    #[serde(default)]
    pub blockers: Vec<BlockerIn>,
    #[serde(default)]
    pub looping: Option<LoopingIn>,
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(default)]
    pub problem_since_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemLiteIn {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockerIn {
    pub text: String,
    pub waiting_on: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoopingIn {
    pub state: String,
    #[serde(default)]
    pub text: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Output (what the route passes through as the view's `pinned` field)

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeOutput {
    pub as_of_ms: i64,
    /// The section head's count line, e.g. "5 pinned in T3 · 1 looping · 2 waiting on you".
    pub head: String,
    pub counts: PinnedCounts,
    #[serde(default)]
    pub machine_problems: Vec<MachineProblemOut>,
    #[serde(default)]
    pub updates: Vec<PinnedUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedCounts {
    pub pinned: usize,
    pub looping: usize,
    pub waiting_on_you: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineProblemOut {
    pub machine: String,
    pub label: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedUpdate {
    pub id: String,
    pub title: String,
    pub machine: String,
    /// "Puppet Master · pm7-build · Opus 5.5 xhigh", or null with no report to build it from.
    #[serde(default)]
    pub who: Option<String>,
    /// fresh | stale | unreadable | none (section 1's report cases).
    pub report_state: &'static str,
    /// "58%" with the counts behind it, or null when no percent can be said.
    #[serde(default)]
    pub percent: Option<PercentView>,
    /// The sentence beside the percent: the estimate fallback, the disagreement, "No task list."
    #[serde(default)]
    pub percent_note: Option<String>,
    /// What it is doing now (`thread@1.doing`, else the status document's summary).
    #[serde(default)]
    pub now: Option<String>,
    pub blocked: BlockedView,
    pub looping: LoopingView,
    /// Not yet landed or done, blocked and looping ones first.
    #[serde(default)]
    pub left: Vec<ItemOut>,
    /// Landed and done, newest first.
    #[serde(default)]
    pub done: Vec<ItemOut>,
    /// "Updated 6 min ago" (clock time in `at` for hover and tap).
    #[serde(default)]
    pub updated: Option<UpdatedView>,
    /// "This thread doesn't report its status yet." (report_state none).
    #[serde(default)]
    pub report_note: Option<String>,
    /// "No status update for 25 days. If it is finished, unpin it in T3." (report_state stale).
    #[serde(default)]
    pub stale_note: Option<String>,
    /// "Could not be read since 1:12 PM." (report_state unreadable: the update shown is the
    /// last good copy).
    #[serde(default)]
    pub unreadable_note: Option<String>,
    /// The status document's own summary, when there is one.
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PercentView {
    pub text: String,
    #[serde(default)]
    pub done: Option<u64>,
    #[serde(default)]
    pub of: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedView {
    pub text: String,
    pub waiting_on_you: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopingView {
    /// yes | no | cantTell.
    pub answer: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemOut {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatedView {
    pub text: String,
    pub at: String,
    pub age_seconds: i64,
}

// ---------------------------------------------------------------------------------------------
// Words

fn plural(count: i64, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// "6 min ago" (age words): just now, minutes, hours, days.
fn age_words(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes < 1 {
        return "just now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{} ago", plural(hours, "hour"));
    }
    format!("{} ago", plural(minutes / 1440, "day"))
}

/// A whole-number duration in words for the stopped-reporting note: minutes, hours, days.
fn for_words(ms: i64) -> String {
    let minutes = (ms / 60_000).max(1);
    if minutes < 60 {
        return plural(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return plural(hours, "hour");
    }
    plural(minutes / 1440, "day")
}

/// "1:12 PM" in the dashboard's zone, from the UTC offset the caller passed.
fn clock_text(epoch_ms: i64, offset_minutes: i64) -> String {
    let shifted = epoch_ms + offset_minutes * 60_000;
    let minute = (shifted / 60_000 % 60 + 60) % 60;
    let raw_hour = (shifted / 3_600_000 % 24 + 24) % 24;
    let (hour, half) = match raw_hour {
        0 => (12, "AM"),
        12 => (12, "PM"),
        hour if hour < 12 => (hour, "AM"),
        hour => (hour - 12, "PM"),
    };
    format!("{hour}:{minute:02} {half}")
}

/// The machine-problem sentence for one unavailability code, by name.
fn machine_problem_text(machine: &str, code: &str) -> String {
    let label = machine_label(machine);
    match code {
        "did_not_answer" => format!("Pinned threads unavailable on {label}: it did not answer."),
        "shape" => format!("Pinned threads unavailable on {label}: T3's state file changed shape."),
        "missing" => format!("Pinned threads unavailable on {label}: T3's state file is missing."),
        "io" => format!("Pinned threads unavailable on {label}: T3's state file could not be read."),
        "limit" => {
            format!("Pinned threads unavailable on {label}: its pin list is past this reader's limits.")
        }
        _ => format!("Pinned threads unavailable on {label}: its pins could not be read."),
    }
}

// ---------------------------------------------------------------------------------------------
// The loop rule (section 3)

#[derive(Debug, Clone, PartialEq)]
struct ItemLoop {
    id: String,
    /// Failing reviews since the last passing one; a pass starts the count over.
    rounds: usize,
}

/// The loop count for one item, over the reviews inside the loop window. A review the
/// item's board did not date counts too — an undatable fix round must not hide a loop.
fn item_loop(item: &ItemIn, now_ms: i64) -> ItemLoop {
    let mut rounds = 0;
    for review in &item.reviews {
        let in_window = match review.at_ms {
            Some(at) => now_ms - at <= LOOP_WINDOW_MS,
            None => true,
        };
        if !in_window {
            continue;
        }
        match classify_verdict(&review.verdict) {
            Verdict::Pass => rounds = 0,
            Verdict::Fail => rounds += 1,
            // An unclear verdict is neither: it breaks the row of consecutive failures, and
            // a loop is never guessed across it (section 3 prefers "unsure" to a guess).
            Verdict::Unknown => rounds = 0,
        }
    }
    ItemLoop {
        id: item.id.clone(),
        rounds,
    }
}

/// The worst looping item of a board, by either loop rule. Finished items (landed, done,
/// dropped) are not looping: their rounds are over.
fn worst_loop(items: &[ItemIn], now_ms: i64) -> Option<ItemLoop> {
    items
        .iter()
        .filter(|item| !matches!(item.state.as_str(), "landed" | "done" | "dropped"))
        .map(|item| item_loop(item, now_ms))
        .filter(|counts| counts.rounds >= LOOP_ROUNDS_MIN || counts.rounds >= LOOP_ROW_MIN)
        .max_by_key(|counts| counts.rounds)
}

fn item_out(item: &ItemIn) -> ItemOut {
    ItemOut {
        id: item.id.clone(),
        title: item.title.clone(),
    }
}

fn lite_out(item: &ItemLiteIn) -> ItemOut {
    ItemOut {
        id: item.id.clone(),
        title: item.title.clone(),
    }
}

// ---------------------------------------------------------------------------------------------
// The composer

fn compose_update(
    pin: &PinIn,
    input: &ComposeInput,
    thread: Option<&ThreadIn>,
    tasks: Option<&TasksIn>,
    status: Option<&StatusIn>,
) -> PinnedUpdate {
    let mut update = PinnedUpdate {
        id: pin.id.clone(),
        title: pin.title.clone(),
        machine: pin.machine.clone(),
        who: None,
        report_state: "none",
        percent: None,
        percent_note: None,
        now: None,
        blocked: BlockedView {
            text: "None.".to_string(),
            waiting_on_you: false,
        },
        looping: LoopingView {
            answer: "cantTell",
            text: "Can't tell.".to_string(),
        },
        left: Vec::new(),
        done: Vec::new(),
        updated: None,
        report_note: None,
        stale_note: None,
        unreadable_note: None,
        summary: status.map(|doc| doc.summary.clone()),
    };

    // ---- Who and Now --------------------------------------------------------------------
    if let Some(thread) = thread {
        let parts = [
            thread.name.clone().or_else(|| thread.harness.clone()),
            thread.stream.clone(),
            match (&thread.model, &thread.effort) {
                (Some(model), Some(effort)) => Some(format!("{model} {effort}")),
                (Some(model), None) => Some(model.clone()),
                (None, Some(effort)) => Some(effort.clone()),
                (None, None) => None,
            },
        ];
        let named: Vec<String> = parts.into_iter().flatten().collect();
        if !named.is_empty() {
            update.who = Some(named.join(" · "));
        }
        update.now = thread.doing.clone().or_else(|| status.map(|doc| doc.summary.clone()));
    } else if let Some(doc) = status {
        update.now = Some(doc.summary.clone());
    }

    // ---- The report cases (section 1's table) -------------------------------------------
    let last_report_ms = [thread.map(|doc| doc.written_at_ms), status.map(|doc| doc.written_at_ms)]
        .into_iter()
        .flatten()
        .max();
    if thread.is_none() && status.is_none() {
        // No report of either kind names this thread. Nothing else can be said without
        // guessing, so the update says exactly that and stops.
        update.report_state = "none";
        update.report_note = Some("This thread doesn't report its status yet.".to_string());
        update.percent_note = Some("No task list.".to_string());
        update.blocked = BlockedView {
            text: "Not reported.".to_string(),
            waiting_on_you: false,
        };
        return update;
    }

    let age = input.now_ms - last_report_ms.unwrap_or(input.now_ms);
    if let Some(at) = last_report_ms {
        // The newest report of the two kinds, its own written-at text.
        let written_at = if status.is_some_and(|doc| doc.written_at_ms == at) {
            status.map(|doc| doc.written_at.clone())
        } else {
            thread.map(|doc| doc.written_at.clone())
        }
        .unwrap_or_default();
        update.updated = Some(UpdatedView {
            text: format!("Updated {words}", words = age_words(age)),
            at: written_at,
            age_seconds: age / 1000,
        });
    }

    // Unreadable beats fresh and stale: the copy shown is the last good one, greyed.
    let unreadable_since = [
        thread
            .filter(|doc| doc.problem.is_some())
            .and_then(|doc| doc.problem_since_ms),
        status
            .filter(|doc| doc.problem.is_some())
            .and_then(|doc| doc.problem_since_ms),
    ]
    .into_iter()
    .flatten()
    .max();
    let ttl_ms = thread
        .and_then(|doc| doc.ttl_seconds)
        .map(|seconds| seconds * 1000)
        .unwrap_or(0);
    if let Some(since) = unreadable_since {
        update.report_state = "unreadable";
        update.unreadable_note = Some(format!(
            "Could not be read since {}.",
            clock_text(since, input.utc_offset_minutes)
        ));
    } else if thread.map(|doc| doc.problem.is_some()).unwrap_or(false)
        || status.map(|doc| doc.problem.is_some()).unwrap_or(false)
    {
        // A problem with no first-seen clock yet (this scan just found it): say it in words,
        // never invent a moment.
        update.report_state = "unreadable";
        update.unreadable_note = Some("Could not be read since the last scan.".to_string());
    } else if age > DAY_MS || (ttl_ms > 0 && age > ttl_ms) {
        update.report_state = "stale";
        update.stale_note = Some(format!(
            "No status update for {}. If it is finished, unpin it in T3.",
            for_words(age)
        ));
    } else {
        update.report_state = "fresh";
    }

    // ---- Percent: the task list first, the thread's own estimate beside it ----------------
    let board_counts = tasks.map(|board| {
        let of = board.items.iter().filter(|item| item.state != "dropped").count() as u64;
        let done = board
            .items
            .iter()
            .filter(|item| matches!(item.state.as_str(), "landed" | "done"))
            .count() as u64;
        (done, of)
    });
    let estimate = status.and_then(|doc| doc.percent);
    match (board_counts, estimate) {
        (Some((done, of)), _) if of > 0 => {
            let percent = done * 100 / of;
            update.percent = Some(PercentView {
                text: format!("{percent}%"),
                done: Some(done),
                of: Some(of),
            });
            if let Some(estimate) = estimate
                && estimate.abs_diff(percent) > 10
            {
                update.percent_note = Some(format!(
                    "Its task list says {done} of {of} ({percent}%); the thread estimates {estimate}%."
                ));
            }
            if update.percent_note.is_none() && tasks.is_some_and(|board| board.problem.is_some()) {
                update.percent_note =
                    Some("Its task list could not be read; these counts are its last good copy.".to_string());
            }
        }
        (Some(_), Some(estimate)) => {
            // A task list whose items are all dropped says nothing; only the estimate does.
            update.percent = Some(PercentView {
                text: format!("{estimate}%"),
                done: None,
                of: None,
            });
            update.percent_note = Some(format!("About {estimate}%, by its own estimate."));
        }
        (Some(_), None) => {
            update.percent_note = Some("Its task list is empty.".to_string());
        }
        (None, Some(estimate)) => {
            update.percent = Some(PercentView {
                text: format!("{estimate}%"),
                done: None,
                of: None,
            });
            update.percent_note = Some(format!("About {estimate}%, by its own estimate."));
        }
        (None, None) => {
            update.percent_note = Some("No task list.".to_string());
        }
    }

    // ---- Left and done -------------------------------------------------------------------
    if let Some(board) = tasks {
        let loop_ids: Vec<String> = worst_loop(&board.items, input.now_ms)
            .map(|counts| vec![counts.id])
            .unwrap_or_default();
        let mut left: Vec<&ItemIn> = board
            .items
            .iter()
            .filter(|item| !matches!(item.state.as_str(), "landed" | "done" | "dropped"))
            .collect();
        // Blocked and looping ones first; the board's own order within each group.
        left.sort_by_key(|item| {
            let blocked = item.state == "blocked" || item.blocked_on.is_some();
            let looping = loop_ids.contains(&item.id);
            !(blocked || looping)
        });
        update.left = left.into_iter().map(item_out).collect();
        let mut done: Vec<&ItemIn> = board
            .items
            .iter()
            .filter(|item| matches!(item.state.as_str(), "landed" | "done"))
            .collect();
        done.sort_by_key(|item| {
            std::cmp::Reverse(
                item.landed
                    .as_ref()
                    .and_then(|landed| landed.at_ms)
                    .or(item.updated_at_ms)
                    .unwrap_or(0),
            )
        });
        update.done = done.into_iter().map(item_out).collect();
    } else if let Some(doc) = status {
        update.left = doc.left.iter().map(lite_out).collect();
        update.done = doc.done.iter().map(lite_out).collect();
    }

    // ---- Looping (section 3) --------------------------------------------------------------
    let own = status.and_then(|doc| doc.looping.as_ref());
    let rule = tasks.and_then(|board| worst_loop(&board.items, input.now_ms));
    if let Some(counts) = &rule {
        let mut text = format!(
            "Yes. {} has gone back for fixes {} without a passing review.",
            counts.id,
            plural(counts.rounds as i64, "time")
        );
        if let Some(finding) = own.and_then(|answer| answer.text.as_ref()) {
            text.push(' ');
            text.push_str(finding);
        }
        update.looping = LoopingView { answer: "yes", text };
    } else if let Some(answer) = own {
        update.looping = match answer.state.as_str() {
            "yes" => LoopingView {
                answer: "yes",
                text: match &answer.text {
                    Some(text) => format!("Yes. {text}"),
                    None => "Yes.".to_string(),
                },
            },
            "no" => LoopingView {
                answer: "no",
                text: "No.".to_string(),
            },
            _ => LoopingView {
                answer: "cantTell",
                text: "Can't tell.".to_string(),
            },
        };
    } else if tasks.is_some() {
        update.looping = LoopingView {
            answer: "no",
            text: "No.".to_string(),
        };
    } else {
        update.looping = LoopingView {
            answer: "cantTell",
            text: "Can't tell.".to_string(),
        };
    }

    // ---- Blocked / waiting on you ----------------------------------------------------------
    let mut waiting: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    // Blocked in so many words with no `blockedOn` to name: the state is known, the reason
    // is not, and neither may be dropped — the update says blocked and that the report does
    // not say on what, never "None.".
    let mut unexplained: Vec<String> = Vec::new();
    if let Some(kind) = thread.and_then(|doc| doc.blocked_on.as_ref()) {
        if kind.kind == "owner" {
            waiting.push(kind.what.clone());
        } else {
            blocked.push(format!("{} (waiting on {})", kind.what, kind.kind));
        }
    } else if thread.is_some_and(|doc| doc.state == "blocked") {
        unexplained.push("Blocked; the report does not say on what.".to_string());
    }
    if let Some(board) = tasks {
        for item in &board.items {
            if matches!(item.state.as_str(), "landed" | "done" | "dropped") {
                continue;
            }
            // An item that names a blocker is a blocked item, whatever its state column says.
            if let Some(kind) = &item.blocked_on {
                if kind.kind == "owner" {
                    waiting.push(kind.what.clone());
                } else {
                    blocked.push(format!("{} (waiting on {})", kind.what, kind.kind));
                }
            } else if item.state == "blocked" {
                // Name the item; the reason was not reported to name.
                unexplained.push(format!("Blocked on {}; the report does not say on what.", item.id));
            }
        }
    }
    if let Some(doc) = status {
        for blocker in &doc.blockers {
            if blocker.waiting_on == "owner" {
                waiting.push(blocker.text.clone());
            } else {
                blocked.push(format!("{} (waiting on {})", blocker.text, blocker.waiting_on));
            }
        }
    }
    // An open card this thread asked is a question of the owner's, whatever else it waits on.
    if let Some(thread) = thread {
        for card in &input.open_cards {
            if card.writer == thread.writer {
                waiting.push(card.title.clone());
            }
        }
    }
    waiting.dedup();
    blocked.dedup();
    let mut parts: Vec<String> = Vec::new();
    if !waiting.is_empty() {
        parts.push(format!("Waiting on you: {}.", waiting.join("; ")));
    }
    if !blocked.is_empty() {
        parts.push(format!("Blocked: {}.", blocked.join("; ")));
    }
    parts.extend(unexplained);
    if !parts.is_empty() {
        update.blocked = BlockedView {
            text: parts.join(" "),
            waiting_on_you: !waiting.is_empty(),
        };
    }

    update
}

/// The composed updates for every pinned thread, in pin order, with the section head and
/// the machines that could not be read. Pure: same input, same words.
pub fn compose(input: &ComposeInput) -> ComposeOutput {
    let mut updates = Vec::with_capacity(input.pins.len());
    for pin in &input.pins {
        // The newest report of each kind that names this thread.
        let thread = input
            .threads
            .iter()
            .filter(|doc| doc.t3_thread.as_deref() == Some(pin.id.as_str()))
            .max_by_key(|doc| (doc.written_at_ms, doc.seq));
        let status = input
            .statuses
            .iter()
            .filter(|doc| doc.t3_thread == pin.id)
            .max_by_key(|doc| (doc.written_at_ms, doc.seq));
        let tasks = thread
            .and_then(|thread| thread.project.as_ref().zip(thread.stream.as_ref()))
            .and_then(|(project, stream)| {
                input
                    .tasks
                    .iter()
                    .filter(|board| &board.project == project && &board.stream == stream)
                    .max_by_key(|board| (board.written_at_ms, board.seq))
            });
        updates.push(compose_update(pin, input, thread, tasks, status));
    }
    let looping = updates.iter().filter(|update| update.looping.answer == "yes").count();
    let waiting = updates.iter().filter(|update| update.blocked.waiting_on_you).count();
    let mut head = format!("{} pinned in T3", input.pins.len());
    if looping > 0 {
        head.push_str(&format!(" · {looping} looping"));
    }
    if waiting > 0 {
        head.push_str(&format!(" · {waiting} waiting on you"));
    }
    ComposeOutput {
        as_of_ms: input.now_ms,
        head,
        counts: PinnedCounts {
            pinned: input.pins.len(),
            looping,
            waiting_on_you: waiting,
        },
        machine_problems: input
            .machine_problems
            .iter()
            .map(|problem| MachineProblemOut {
                machine: problem.machine.clone(),
                label: machine_label(&problem.machine),
                text: machine_problem_text(&problem.machine, &problem.code),
            })
            .collect(),
        updates,
    }
}
