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

/// How far back the loop rule looks. Boards here review slower than once a day — the owner's own
/// loops ran for days — so a 24-hour window read a four-day loop of daily fails as "not
/// looping"; the window now spans several days (PINNED-THREADS.md section 3's "recent"
/// reviews).
const LOOP_WINDOW_MS: i64 = 4 * 24 * 60 * 60 * 1000;

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

/// A review verdict that failed, the spellings the boards and reviewers write. Matched by
/// prefix, like a pass: "FAIL, one Medium" is a fail, never an unclear verdict.
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
    if FAILING_VERDICTS.iter().any(|word| text.starts_with(word)) {
        return Verdict::Fail;
    }
    Verdict::Unknown
}

/// The loop rule's counts (section 3): 3 or more failing reviews in a row, or 4 or more
/// failing rounds since a passing one. A pass starts the count over; an unclear verdict is
/// skipped (never counted, never a reset), so the two measure the same stretch of a board
/// whose reviews are written in order; both are checked anyway.
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
    /// The reader's standalone problems (refusals with no accepted document). Carried into the
    /// projection so a refused report says why instead of just going missing.
    #[serde(default)]
    pub status_problems: Vec<StatusProblemIn>,
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
    #[serde(flatten)]
    pub additions: std::collections::BTreeMap<String, Json>,
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
    /// The catalog label for the raw model id, as the TypeScript side resolved it
    /// (D9); falls back to the raw id when the model is not in the catalog.
    #[serde(default)]
    pub model_label: Option<String>,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedIn {
    #[serde(flatten)]
    pub additions: std::collections::BTreeMap<String, Json>,
    pub kind: String,
    pub what: String,
    #[serde(default)]
    pub since: Option<String>,
    /// The same moment as `since`, in epoch ms (the TypeScript side parses it once); used for
    /// clock words, never recomputed here.
    #[serde(default)]
    pub since_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    #[serde(flatten)]
    pub additions: std::collections::BTreeMap<String, Json>,
    pub writer: String,
    pub written_at_ms: i64,
    pub seq: u64,
    pub project: String,
    pub stream: String,
    #[serde(default)]
    pub project_label: Option<String>,
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
    #[serde(default)]
    pub note: Option<String>,
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
    /// The card kind; a missing kind reads as `question` (D10), as the reader defaults it.
    #[serde(default = "default_card_kind")]
    pub kind: String,
    #[serde(default)]
    pub asked_at_ms: Option<i64>,
}

fn default_card_kind() -> String {
    "question".to_string()
}

/// Whether a card kind counts as a question of the owner's (D10): only question,
/// judgement and action cards do. An objection waits for a look, never an answer,
/// and a sample wants no answer at all.
fn is_question_kind(kind: &str) -> bool {
    matches!(kind, "question" | "judgement" | "action")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusIn {
    #[serde(flatten)]
    pub additions: std::collections::BTreeMap<String, Json>,
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

/// One standalone refusal from the status reader: a `status.json` TypeScript refused with no
/// accepted document to keep. The file and reason are fixed words; the refused content is never
/// shown.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusProblemIn {
    pub file: String,
    pub reason: String,
    pub since_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemLiteIn {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockerIn {
    #[serde(flatten)]
    pub additions: std::collections::BTreeMap<String, Json>,
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
    /// The status@1 refusals with no good document: file and fixed reason, so a refused report
    /// says why instead of just going missing.
    #[serde(default)]
    pub status_problems: Vec<StatusProblemOut>,
    #[serde(default)]
    pub updates: Vec<PinnedUpdate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusProblemOut {
    pub file: String,
    pub reason: String,
    pub since_ms: i64,
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
    #[serde(rename = "reported", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub additions: std::collections::BTreeMap<String, Json>,
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
    /// Null when no task list and no report names a blocker (D8): the page says "No task lists
    /// yet", never an invented "Nothing blocked".
    #[serde(default)]
    pub blocked: Option<BlockedView>,
    pub looping: LoopingView,
    /// Not yet landed or done, blocked and looping ones first; null with no list to show.
    #[serde(default)]
    pub left: Option<Vec<ItemOut>>,
    /// Landed and done, newest first; null with no list to show.
    #[serde(default)]
    pub done: Option<Vec<ItemOut>>,
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
    /// The status@1 refusal behind this update, when there is one: the writer file and the fixed
    /// reason, so the refusal says why. Never the refused content.
    #[serde(default)]
    pub refusal: Option<RefusalView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalView {
    pub file: String,
    pub reason: String,
    pub since_ms: Option<i64>,
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
    /// The looping item's id, when the loop rule found one: the Status headline names the pin
    /// as "<title>, task <id>" (D1).
    #[serde(default)]
    pub task: Option<String>,
    /// The failing rounds behind this "yes" from the loop rule, so the headline can pick the
    /// worst looping pin; 0 when the rule did not say yes.
    #[serde(default)]
    pub rounds: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(rename = "blockedOn", skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
/// An unclear verdict is skipped, not a reset: free wording a reviewer left between two
/// fails ("cannot verify on the phone") must not turn a real loop back into "on track",
/// and it never counts toward one either (section 3 prefers "unsure" to a guess).
/// Reviews count by date, not list order (F11): a list written newest first must read
/// the same as one written oldest first. Undated reviews keep their order at the end.
fn item_loop(item: &ItemIn, now_ms: i64) -> ItemLoop {
    let mut ordered: Vec<&ReviewIn> = item.reviews.iter().collect();
    ordered.sort_by_key(|review| (review.at_ms.is_none(), review.at_ms));
    let mut rounds = 0;
    for review in ordered {
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
            Verdict::Unknown => {}
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
    let known = [
        "todo", "working", "review", "fixing", "blocked", "ready", "landed", "done", "dropped",
    ]
    .contains(&item.state.as_str());
    ItemOut {
        state: Some(item.state.clone()),
        note: item.note.clone(),
        blocked_on: item
            .blocked_on
            .as_ref()
            .filter(|b| b.additions.values().any(|v| !v.is_null()))
            .and_then(|b| serde_json::to_value(b).ok()),
        id: item.id.clone(),
        title: if known {
            item.title.clone()
        } else {
            format!("{} (state not recognised: {})", item.title, item.state)
        },
    }
}

// ---------------------------------------------------------------------------------------------
// The pin match (B8)

/// One `%XX` escape, as percents appear in the ids T3 itself prints.
fn hex_byte(first: u8, second: u8) -> Option<u8> {
    let digit = |value: u8| match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    };
    Some(digit(first)? << 4 | digit(second)?)
}

/// The id forms a report may carry for one T3 thread, normalised so they all compare equal:
/// the plain id, the same id percent-encoded (T3's own run ids spell `mcp:launch:` as
/// `mcp%3Alaunch%3A`), a `t3-thread://` link that ends in it, and the bare tail after the
/// `mcp:launch:` prefix. Normalisation only — a title is never fuzzy-matched.
fn normalised_thread_id(id: &str) -> String {
    // Percent-decode first: the escapes hide the separators the later steps look for.
    let bytes = id.trim().as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let Some(byte) = hex_byte(bytes[at + 1], bytes[at + 2])
        {
            decoded.push(byte);
            at += 3;
            continue;
        }
        decoded.push(bytes[at]);
        at += 1;
    }
    let mut text = String::from_utf8_lossy(&decoded).to_lowercase();
    // A link ends in the id; a bare id has no separator at all.
    if let Some((_, tail)) = text.rsplit_once('/') {
        text = tail.to_string();
    }
    text.strip_prefix("mcp:launch:").unwrap_or(&text).to_string()
}

/// Whether a report's `t3Thread` names this pin's thread (B8: normalised, never a title match).
fn same_thread_id(reported: &str, pin: &str) -> bool {
    let left = normalised_thread_id(reported);
    let right = normalised_thread_id(pin);
    !left.is_empty() && left == right
}

fn lite_out(item: &ItemLiteIn) -> ItemOut {
    ItemOut {
        state: None,
        note: None,
        blocked_on: None,
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
    standalone_refusal: Option<&StatusProblemIn>,
) -> PinnedUpdate {
    let mut additions = std::collections::BTreeMap::new();
    for source in [
        tasks.map(|d| &d.additions),
        thread.map(|d| &d.additions),
        status.map(|d| &d.additions),
    ]
    .into_iter()
    .flatten()
    {
        for key in [
            "title",
            "now",
            "next",
            "stages",
            "deliverables",
            "progress",
            "readmeQuestions",
        ] {
            if let Some(value) = source.get(key) {
                additions.insert(key.to_string(), value.clone());
            }
        }
    }
    // Thread progress is a typed field, so serde does not include it in additions.
    // Preserve the same tasks < thread < status precedence as the additive fields.
    if !status.is_some_and(|d| d.additions.contains_key("progress")) {
        if let Some(progress) = thread.and_then(|d| d.progress.as_ref()) {
            additions.insert("progress".into(), serde_json::to_value(progress).unwrap_or(Json::Null));
        }
    }
    if let Some(blocker) = thread
        .and_then(|d| d.blocked_on.as_ref())
        .filter(|b| !b.additions.is_empty())
    {
        additions.insert("blockedOn".into(), serde_json::to_value(blocker).unwrap_or(Json::Null));
    }
    if let Some(doc) = status.filter(|d| d.blockers.iter().any(|b| !b.additions.is_empty())) {
        additions.insert(
            "blockers".into(),
            serde_json::to_value(&doc.blockers).unwrap_or(Json::Null),
        );
    }
    if let Some(thread) = thread {
        if !thread.state.is_empty() {
            additions.insert("state".into(), Json::String(thread.state.clone()));
        }
        additions.insert("writer".into(), Json::String(thread.writer.clone()));
    }
    let mut update = PinnedUpdate {
        additions,
        id: pin.id.clone(),
        title: pin.title.clone(),
        machine: pin.machine.clone(),
        who: None,
        report_state: "none",
        percent: None,
        percent_note: None,
        now: None,
        blocked: None,
        looping: LoopingView {
            answer: "cantTell",
            text: "Can't tell.".to_string(),
            task: None,
            rounds: 0,
        },
        left: None,
        done: None,
        updated: None,
        report_note: None,
        stale_note: None,
        unreadable_note: None,
        summary: status.map(|doc| doc.summary.clone()),
        refusal: None,
    };

    // ---- Who and Now --------------------------------------------------------------------
    // The who line is the project label, the thread's name, and the catalog model label
    // (D9): "Puppet Master · pm7-build · Opus 5.5 xhigh". The bare stream letter never
    // shows; the board's stream label stands in only when the thread names nothing.
    if let Some(thread) = thread {
        let labelled = |value: &Option<String>| value.clone().filter(|text| !text.is_empty());
        let project_part = tasks
            .and_then(|board| labelled(&board.project_label))
            .or_else(|| labelled(&thread.project));
        let name_part = labelled(&thread.name)
            .or_else(|| labelled(&thread.harness))
            .or_else(|| tasks.and_then(|board| labelled(&board.stream_label)));
        let model_text = thread
            .model_label
            .clone()
            .filter(|text| !text.is_empty())
            .or_else(|| thread.model.clone());
        let parts = [
            project_part,
            name_part,
            match (&model_text, &thread.effort) {
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

    // The status@1 refusal behind this update, when the held copy carries one: the writer file
    // and the fixed reason, so a refusal says why. Never the refused content. A standalone
    // refusal (no good copy) for this pin's writer attaches the same way (F13).
    if let Some(doc) = status.filter(|doc| doc.problem.is_some()) {
        let file = if doc.writer.is_empty() {
            "writers/<unknown>/status.json".to_string()
        } else {
            format!("writers/{}/status.json", doc.writer)
        };
        update.refusal = Some(RefusalView {
            file,
            reason: doc.problem.clone().unwrap_or_default(),
            since_ms: doc.problem_since_ms,
        });
    } else if let Some(problem) = standalone_refusal {
        update.refusal = Some(RefusalView {
            file: problem.file.clone(),
            reason: problem.reason.clone(),
            since_ms: Some(problem.since_ms),
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
    // A thread whose last report says it waits on the owner is never stale: it ended its turn to
    // wait for him, so the longer it waits the more the page must keep saying so (B2), whatever
    // its heartbeat said. The `updated` line above still shows its true age.
    let waiting_on_owner = [
        thread
            .and_then(|doc| doc.blocked_on.as_ref())
            .map(|kind| kind.kind.as_str()),
        tasks.and_then(|board| {
            board
                .items
                .iter()
                .filter(|item| !matches!(item.state.as_str(), "landed" | "done" | "dropped"))
                .find(|item| item.blocked_on.as_ref().is_some_and(|kind| kind.kind == "owner"))
                .map(|_| "owner")
        }),
        status.and_then(|doc| {
            doc.blockers
                .iter()
                .find(|blocker| blocker.waiting_on == "owner")
                .map(|_| "owner")
        }),
        // Waiting through an open question card is waiting too (F4): the thread asked
        // and ended its turn, exactly as a blockedOn says so.
        thread.and_then(|doc| {
            input
                .open_cards
                .iter()
                .find(|card| card.writer == doc.writer && is_question_kind(&card.kind))
                .map(|_| "owner")
        }),
        // A blocked/waiting state with no heartbeat is exempt too (N4): the thread ended
        // its turn waiting, whatever state it ended in. With a ttl set, the state alone
        // does not exempt — the owner signals above must name it.
        thread
            .filter(|doc| doc.ttl_seconds.is_none())
            .filter(|doc| matches!(doc.state.as_str(), "blocked" | "waiting"))
            .map(|_| "owner"),
    ]
    .into_iter()
    .flatten()
    .any(|kind| kind == "owner");
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
    } else if !waiting_on_owner && (age > DAY_MS || (ttl_ms > 0 && age > ttl_ms)) {
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
        // (None, None): no board and no estimate. Null, never an invented sentence — the page
        // says "No task lists yet" (D8).
        (None, None) => {}
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
        update.left = Some(left.into_iter().map(item_out).collect());
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
        update.done = Some(done.into_iter().map(item_out).collect());
    } else if let Some(doc) = status {
        update.left = Some(doc.left.iter().map(lite_out).collect());
        update.done = Some(doc.done.iter().map(lite_out).collect());
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
        update.looping = LoopingView {
            answer: "yes",
            text,
            task: Some(counts.id.clone()),
            rounds: counts.rounds,
        };
    } else if let Some(answer) = own {
        update.looping = match answer.state.as_str() {
            "yes" => LoopingView {
                answer: "yes",
                text: match &answer.text {
                    Some(text) => format!("Yes. {text}"),
                    None => "Yes.".to_string(),
                },
                task: None,
                rounds: 0,
            },
            "no" => LoopingView {
                answer: "no",
                text: "No.".to_string(),
                task: None,
                rounds: 0,
            },
            _ => LoopingView {
                answer: "cantTell",
                text: "Can't tell.".to_string(),
                task: None,
                rounds: 0,
            },
        };
    } else if tasks.is_some() {
        // D8: a board with no review inside the loop window proves nothing about looping —
        // "No." there was an invented all-clear. Only a review in the window, or the thread's
        // own answer, may say no — and only a review that says pass or fail counts: a board
        // whose only reviews are unclear ("cannot verify") says "Can't tell", not "No." (F7).
        let reviewed_in_window = tasks.is_some_and(|board| {
            board.items.iter().any(|item| {
                item.reviews.iter().any(|review| {
                    let in_window = match review.at_ms {
                        Some(at) => input.now_ms - at <= LOOP_WINDOW_MS,
                        None => true,
                    };
                    in_window && classify_verdict(&review.verdict) != Verdict::Unknown
                })
            })
        });
        update.looping = if reviewed_in_window {
            LoopingView {
                answer: "no",
                text: "No.".to_string(),
                task: None,
                rounds: 0,
            }
        } else {
            LoopingView {
                answer: "cantTell",
                text: "Can't tell.".to_string(),
                task: None,
                rounds: 0,
            }
        };
    } else {
        update.looping = LoopingView {
            answer: "cantTell",
            text: "Can't tell.".to_string(),
            task: None,
            rounds: 0,
        };
    }

    // ---- Blocked / waiting on you ----------------------------------------------------------
    let mut waiting: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    // The earliest moment one of the waiting-on-you reports names, for "since <clock>".
    let mut waiting_since: Option<i64> = None;
    let mut note_waiting_since = |kind: &BlockedIn| {
        if kind.kind == "owner"
            && let Some(since) = kind.since_ms
        {
            waiting_since = Some(waiting_since.map_or(since, |held: i64| held.min(since)));
        }
    };
    // Blocked in so many words with no `blockedOn` to name: the state is known, the reason
    // is not, and neither may be dropped — the update says blocked and that the report does
    // not say on what, never "None.".
    let mut unexplained: Vec<String> = Vec::new();
    if let Some(kind) = thread.and_then(|doc| doc.blocked_on.as_ref()) {
        if kind.kind == "owner" {
            note_waiting_since(kind);
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
                    note_waiting_since(kind);
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
    // An open card this thread asked is a question of the owner's, whatever else it waits
    // on — but only question, judgement and action cards count (D10): silence is fine on
    // an objection, and a sample wants no answer.
    if let Some(thread) = thread {
        for card in &input.open_cards {
            if card.writer == thread.writer && is_question_kind(&card.kind) {
                waiting.push(card.title.clone());
            }
        }
    }
    waiting.dedup();
    blocked.dedup();
    let mut parts: Vec<String> = Vec::new();
    if !waiting.is_empty() {
        // "Waiting on you since 6:55 PM: pick the reviewers." — the since is the earliest moment
        // a waiting report names; without one the sentence stays as it was.
        let since = waiting_since
            .map(|at| format!(" since {}", clock_text(at, input.utc_offset_minutes)))
            .unwrap_or_default();
        parts.push(format!("Waiting on you{since}: {}.", waiting.join("; ")));
    }
    if !blocked.is_empty() {
        parts.push(format!("Blocked: {}.", blocked.join("; ")));
    }
    parts.extend(unexplained);
    if !parts.is_empty() {
        update.blocked = Some(BlockedView {
            text: parts.join(" "),
            waiting_on_you: !waiting.is_empty(),
        });
    } else if tasks.is_some() {
        // A board where nothing is blocked may say "None."; without one there is nothing to
        // draw the sentence from, so it stays null (D8).
        update.blocked = Some(BlockedView {
            text: "None.".to_string(),
            waiting_on_you: false,
        });
    }

    update
}

/// The composed updates for every pinned thread, in pin order, with the section head and
/// the machines that could not be read. Pure: same input, same words.
pub fn compose(input: &ComposeInput) -> ComposeOutput {
    let mut updates = Vec::with_capacity(input.pins.len());
    for pin in &input.pins {
        // The newest report of each kind that names this thread, by the normalised id forms
        // a report carries (B8): a pinned thread whose report spells the id differently still
        // gets its update instead of reading as "doesn't report".
        let thread = input
            .threads
            .iter()
            .filter(|doc| doc.t3_thread.as_deref().is_some_and(|id| same_thread_id(id, &pin.id)))
            .max_by_key(|doc| (doc.written_at_ms, doc.seq));
        let status = input
            .statuses
            .iter()
            .filter(|doc| same_thread_id(&doc.t3_thread, &pin.id))
            .max_by_key(|doc| (doc.written_at_ms, doc.seq));
        // The board comes from the report's project and stream; a thread that reports only
        // `status.json` names neither, so the same writer's `tasks.json` is its board (B8).
        let tasks = thread
            .and_then(|thread| thread.project.as_ref().zip(thread.stream.as_ref()))
            .and_then(|(project, stream)| {
                input
                    .tasks
                    .iter()
                    .filter(|board| &board.project == project && &board.stream == stream)
                    .max_by_key(|board| (board.written_at_ms, board.seq))
            })
            .or_else(|| {
                let writer = thread
                    .map(|thread| thread.writer.as_str())
                    .or(status.map(|doc| doc.writer.as_str()));
                writer.and_then(|writer| {
                    input
                        .tasks
                        .iter()
                        .filter(|board| board.writer == writer)
                        .max_by_key(|board| (board.written_at_ms, board.seq))
                })
            });
        // A standalone status refusal (no good copy) for this pin's writer attaches to
        // the pin itself (F13), matched the way B8 matches: the report whose thread
        // names this pin by the normalised id. The panel entry stays, too.
        let standalone_refusal = thread.and_then(|doc| {
            let want = format!("writers/{}/status.json", doc.writer);
            input.status_problems.iter().find(|problem| problem.file == want)
        });
        updates.push(compose_update(pin, input, thread, tasks, status, standalone_refusal));
    }
    let looping = updates.iter().filter(|update| update.looping.answer == "yes").count();
    let waiting = updates
        .iter()
        .filter(|update| update.blocked.as_ref().is_some_and(|blocked| blocked.waiting_on_you))
        .count();
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
        status_problems: input
            .status_problems
            .iter()
            .map(|problem| StatusProblemOut {
                file: problem.file.clone(),
                reason: problem.reason.clone(),
                since_ms: problem.since_ms,
            })
            .collect(),
        updates,
    }
}
