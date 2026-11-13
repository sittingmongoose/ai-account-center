//! Status concept C v2 (owner-approved, S1): pinned threads as a roster and a sheet.
//!
//! The roster lists every reporting pin under its state word; the sheet shows one thread with the same five facts in
//! the same places (For you, Waiting on, Now, Next, Updated), the thread's own stages, the things to open with their
//! versions, its tasks grouped by state, and its problem line. Wide, the sheet sits beside the roster; narrow, each
//! pin is its own plate and the sheet rises from the bottom.
//!
//! Everything here is read from what the reports said (`reported`, validated by the server in report-v2.ts) and the
//! composer's fields. A fact a report did not give says so ("Not reported"); nothing is filled in. A report written
//! before these fields existed is drawn with the composer's fields alone.

use crate::api::*;
use serde::Deserialize;
use serde_json::Value;

// ---- What a report said (optional, additive) ----------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stage {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub by: Option<String>,
    pub round: Option<u32>,
    pub of: Option<u32>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportedMedia {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub poster: Option<String>,
    pub caption: Option<String>,
    pub label: Option<String>,
    pub seconds: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OlderVersion {
    #[serde(deserialize_with = "nullable")]
    pub version: String,
    pub updated_at: Option<String>,
    pub path: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Deliverable {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub state: Option<String>,
    #[serde(rename = "for")]
    pub for_whom: Option<String>,
    pub want: Option<String>,
    pub main: Option<bool>,
    #[serde(deserialize_with = "nullable")]
    pub version: String,
    pub updated_at: Option<String>,
    pub supersedes: Option<String>,
    pub path: Option<String>,
    pub url: Option<String>,
    pub note: Option<String>,
    pub unlocks: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub media: Vec<ReportedMedia>,
    #[serde(deserialize_with = "nullable")]
    pub versions: Vec<OlderVersion>,
    #[serde(deserialize_with = "nullable")]
    pub cards: Vec<String>,
    pub ready_at: Option<String>,
}

impl Deliverable {
    pub fn is_ready_for_owner(&self) -> bool {
        self.for_whom.as_deref() == Some("owner") && self.state.as_deref() == Some("ready")
    }
}

/// A blocker as a report gives it: thread@1 `blockedOn` (`kind`, `what`) or status@1 `blockers` (`waitingOn`,
/// `text`), with v2's optional `short`, `since` and `until`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Blocker {
    pub kind: Option<String>,
    pub waiting_on: Option<String>,
    pub what: Option<String>,
    pub text: Option<String>,
    pub short: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

impl Blocker {
    pub fn who(&self) -> &str {
        self.kind
            .as_deref()
            .or(self.waiting_on.as_deref())
            .unwrap_or("")
    }
    pub fn words(&self) -> String {
        self.short
            .clone()
            .or_else(|| self.what.clone())
            .or_else(|| self.text.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Progress {
    pub done: Option<u32>,
    pub total: Option<u32>,
    pub unit: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reported {
    pub title: Option<String>,
    pub now: Option<String>,
    pub next: Option<String>,
    pub stages: Vec<Stage>,
    pub deliverables: Vec<Deliverable>,
    pub readme_questions: bool,
    pub progress: Option<Progress>,
    /// thread@1 `state`, when the server passes it on.
    pub state: Option<String>,
    /// The matched report folder, when the server passes it on.
    pub writer: Option<String>,
    pub blockers: Vec<Blocker>,
    pub blocked_on: Option<Blocker>,
}

fn field<T: for<'de> Deserialize<'de>>(v: &Value, key: &str) -> Option<T> {
    v.get(key)
        .filter(|x| !x.is_null())
        .and_then(|x| serde_json::from_value(x.clone()).ok())
}

/// One list, read row by row: a row this page can't read is left out, never the whole list.
fn rows<T: for<'de> Deserialize<'de>>(v: &Value, key: &str) -> Vec<T> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| serde_json::from_value(x.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn text_of(v: &Value, key: &str) -> Option<String> {
    field::<String>(v, key).filter(|s| !s.trim().is_empty())
}

pub fn reported(u: &PinnedUpdate) -> Reported {
    let Some(v) = u.reported.as_ref() else {
        return Reported::default();
    };
    Reported {
        title: text_of(v, "title"),
        now: text_of(v, "now"),
        next: text_of(v, "next"),
        stages: rows(v, "stages"),
        deliverables: rows(v, "deliverables"),
        readme_questions: field::<bool>(v, "readmeQuestions").unwrap_or(false),
        progress: field(v, "progress"),
        state: text_of(v, "state"),
        writer: text_of(v, "writer"),
        blockers: rows(v, "blockers"),
        blocked_on: field(v, "blockedOn"),
    }
}

// ---- The state word -----------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Word {
    ReadyForYou,
    WaitingOnYou,
    Waiting,
    Working,
    Idle,
    /// A fresh report that does not say whether it works, idles or is done (the server doesn't pass thread state).
    Reporting,
    Done,
    Quiet,
    NotReporting,
}

impl Word {
    pub fn text(self) -> &'static str {
        match self {
            Word::ReadyForYou => "Ready for you",
            Word::WaitingOnYou => "Waiting on you",
            Word::Waiting => "Waiting",
            Word::Working => "Working",
            Word::Idle => "Idle",
            Word::Reporting => "Reporting",
            Word::Done => "Done",
            Word::Quiet => "Quiet",
            Word::NotReporting => "Not reporting",
        }
    }
    pub fn tone(self) -> Option<&'static str> {
        match self {
            Word::ReadyForYou | Word::WaitingOnYou => Some("warn"),
            Word::Done => Some("good"),
            Word::Quiet | Word::NotReporting => Some("mute"),
            _ => None,
        }
    }
    /// The roster group the word sits under.
    pub fn group(self) -> &'static str {
        match self {
            Word::ReadyForYou | Word::WaitingOnYou => "Needs you",
            Word::Waiting => "Waiting",
            Word::Working => "Working",
            Word::Idle => "Idle",
            Word::Reporting => "Reporting",
            Word::Done => "Done",
            Word::Quiet => "Quiet",
            Word::NotReporting => "Not reporting",
        }
    }
}

/// The open cards that belong to a thread: those its deliverables name, and, when the server says which report
/// folder the pin is, every open card that folder asked.
pub fn open_cards_for<'a>(r: &Reported, open: &'a [Card]) -> Vec<&'a Card> {
    open.iter()
        .filter(|c| {
            r.writer.as_deref().is_some_and(|w| w == c.writer)
                || r.deliverables.iter().any(|d| d.cards.contains(&c.id))
        })
        .collect()
}

fn blocked_known(u: &PinnedUpdate) -> bool {
    let t = u.blocked.text.trim();
    !t.is_empty() && t != "None." && t != "None"
}

pub fn state_word(u: &PinnedUpdate, r: &Reported, cards: usize) -> Word {
    match u.report_state.as_str() {
        "none" => return Word::NotReporting,
        "stale" => return Word::Quiet,
        _ => {}
    }
    if r.deliverables.iter().any(Deliverable::is_ready_for_owner) {
        return Word::ReadyForYou;
    }
    let owner_blocker = r.blockers.iter().any(|b| b.who() == "owner")
        || r.blocked_on.as_ref().is_some_and(|b| b.who() == "owner");
    if cards > 0 || u.blocked.waiting_on_you || owner_blocker {
        return Word::WaitingOnYou;
    }
    if blocked_known(u)
        || !r.blockers.is_empty()
        || r.blocked_on.is_some()
        || matches!(r.state.as_deref(), Some("waiting" | "blocked"))
    {
        return Word::Waiting;
    }
    match r.state.as_deref() {
        Some("working") => Word::Working,
        Some("idle") => Word::Idle,
        Some("done") => Word::Done,
        _ => Word::Reporting,
    }
}

/// Whether a stage the owner acts in waits now (amber, and the thread counts under Needs you).
pub fn owner_stage_waiting(r: &Reported) -> bool {
    r.stages
        .iter()
        .any(|s| s.by.as_deref() == Some("owner") && s.state == "waiting")
}

// ---- Words ---------------------------------------------------------------------------------------------------------

/// At most `max` characters, cut at a whole word with an ellipsis.
pub fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let at = head
        .rfind(' ')
        .filter(|&i| i > max / 2)
        .unwrap_or(head.len());
    format!(
        "{}…",
        head[..at].trim_end_matches(|c: char| c == ',' || c == ';' || c == ':' || c == ' ')
    )
}

/// The first sentence of a text, cut at `max`.
pub fn first_sentence(text: &str, max: usize) -> String {
    let t = text.trim();
    let end = t.find(". ").map(|i| i + 1).unwrap_or(t.len());
    cut(&t[..end], max)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "Reviews, round 2" and "3 of 6": the first stage that is not done, counted without skipped ones.
pub fn stage_line(stages: &[Stage]) -> Option<(String, usize, usize)> {
    let shown: Vec<&Stage> = stages.iter().filter(|s| s.state != "skipped").collect();
    if shown.len() < 2 {
        return None;
    }
    let at = shown
        .iter()
        .position(|s| s.state == "now")
        .or_else(|| shown.iter().position(|s| s.state != "done"))?;
    let s = shown[at];
    let mut text = s.label.clone();
    if let Some(round) = s.round {
        text.push_str(&match s.of {
            Some(of) => format!(", round {round} of {of}"),
            None => format!(", round {round}"),
        });
    }
    Some((text, at + 1, shown.len()))
}

/// The tone a stage segment is drawn in: grey done, blue now, amber the owner's step that waits, empty to come.
pub fn stage_tone(s: &Stage) -> &'static str {
    match (s.state.as_str(), s.by.as_deref()) {
        ("done", _) => "done",
        ("now", _) => "now",
        ("waiting", Some("owner")) => "you",
        ("waiting", _) => "wait",
        _ => "next",
    }
}

pub fn stage_state_words(s: &Stage) -> &'static str {
    match (s.state.as_str(), s.by.as_deref()) {
        ("done", _) => "done",
        ("now", _) => "now",
        ("waiting", Some("owner")) => "waits on you",
        ("waiting", _) => "waiting",
        _ => "to come",
    }
}

pub fn who_words(kind: &str) -> &'static str {
    match kind {
        "owner" => "You",
        "agent" => "An agent",
        "review" => "A review",
        "lock" => "The landing lock",
        "resource" => "A machine",
        "quota" => "Quota",
        _ => "Something else",
    }
}

pub fn kind_words(kind: &str) -> &'static str {
    match kind {
        "concept" => "Concept",
        "report" => "Report",
        "document" => "Document",
        "build" => "Build",
        "decisions" => "Decisions",
        "video" | "recording" => "Video",
        "screenshots" => "Screenshots",
        _ => "Other",
    }
}

/// The model part of the composer's who line ("Project · name · Model effort"), only when all three parts are there.
pub fn model_of(who: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = who?.split(" · ").collect();
    (parts.len() == 3).then(|| parts[2].to_string())
}

// ---- Things to open ------------------------------------------------------------------------------------------------

/// The share-relative path the read-only deliverable route serves, when `path` is under a recognised share root and
/// in a folder the route allows. Paths with dot or empty segments are never sent (the route would refuse them).
pub fn share_relative(path: &str, allow_media: bool) -> Option<String> {
    let p = path.replace('\\', "/");
    let rest = ["/mnt/Cursor/", "/Volumes/Cursor/", "//TRUENAS/Cursor/"]
        .iter()
        .find_map(|prefix| p.strip_prefix(prefix))?;
    if rest
        .split('/')
        .any(|seg| seg.is_empty() || seg.starts_with('.'))
        || rest.contains(':')
        || rest.contains('%')
    {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').collect();
    let ok = (parts[0] == "share" && parts.len() > 1)
        || (allow_media
            && parts.len() > 5
            && parts[0] == "agent-reports"
            && parts[1] == "v1"
            && parts[2] == "writers"
            && parts[4] == "media");
    ok.then(|| rest.to_string())
}

pub fn route_href(relative: &str) -> String {
    format!("/api/status/deliverable?path={}", js_encode(relative))
}

/// encodeURIComponent, without needing a browser (native tests).
fn js_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// How a deliverable opens: through the route, a link, or not here (Copy path).
#[derive(Clone, Debug, PartialEq)]
pub enum Opener {
    Route(String),
    Link(String),
    CopyPath(String),
    Nothing,
}

pub fn opener(path: Option<&str>, url: Option<&str>) -> Opener {
    if let Some(u) = url.filter(|u| u.starts_with("https://")) {
        return Opener::Link(u.to_string());
    }
    match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => match share_relative(p, false) {
            Some(rel) => Opener::Route(route_href(&rel)),
            None => Opener::CopyPath(p.to_string()),
        },
        None => Opener::Nothing,
    }
}

/// The order things to open are listed in: `main` first, then those for the owner, then the newest.
pub fn ordered(list: &[Deliverable]) -> Vec<Deliverable> {
    let mut out = list.to_vec();
    out.sort_by(|a, b| {
        let key = |d: &Deliverable| {
            (
                !d.main.unwrap_or(false),
                d.for_whom.as_deref() != Some("owner"),
            )
        };
        key(a)
            .cmp(&key(b))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    // At most one is the main one: the first that says so.
    let mut seen = false;
    for d in &mut out {
        if d.main == Some(true) {
            if seen {
                d.main = Some(false);
            }
            seen = true;
        }
    }
    out
}

/// What this browser remembers opening: `(version, when in ms)` per thread and deliverable.
pub type Opened = Vec<(String, f64)>;

#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    New,
    NewSince(String),
    Opened(f64),
}

pub fn mark(version: &str, opened: &Opened) -> Mark {
    if let Some((_, at)) = opened.iter().find(|(v, _)| v == version) {
        return Mark::Opened(*at);
    }
    match opened
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    {
        Some((v, _)) => Mark::NewSince(v.clone()),
        None => Mark::New,
    }
}

// ---- Tasks ---------------------------------------------------------------------------------------------------------

pub const GROUPS: [&str; 7] = [
    "Waits on you",
    "Blocked",
    "In work",
    "State not recognised",
    "To do",
    "In review",
    "Done",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    /// The raw state of an item whose state is not one of the nine.
    pub odd_state: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskGroup {
    pub label: &'static str,
    pub warn: bool,
    pub rows: Vec<TaskRow>,
}

impl TaskGroup {
    /// More than 6 rows fold, and a Done group of more than 3.
    pub fn folds(&self) -> bool {
        self.rows.len() > 6 || (self.label == "Done" && self.rows.len() > 3)
    }
}

const KNOWN: [&str; 9] = [
    "todo", "working", "review", "fixing", "blocked", "ready", "landed", "done", "dropped",
];

/// "Title (state not recognised: queued)", the composer's words for an unknown state, split back into title and state.
pub fn split_odd(title: &str) -> (String, Option<String>) {
    if let Some(open) = title.rfind(" (state not recognised: ") {
        if title.ends_with(')') {
            let state = &title[open + " (state not recognised: ".len()..title.len() - 1];
            return (title[..open].to_string(), Some(state.to_string()));
        }
    }
    (title.to_string(), None)
}

fn group_of(state: &str, owner_block: bool, blocked: bool) -> &'static str {
    if owner_block {
        return "Waits on you";
    }
    if state == "blocked" || blocked {
        return "Blocked";
    }
    match state {
        "working" | "fixing" => "In work",
        "todo" => "To do",
        "review" | "ready" => "In review",
        "done" | "landed" => "Done",
        _ => "State not recognised",
    }
}

/// The board behind a pin, found among the task lists the view carries: the one stream whose items (dropped ones left
/// out) are exactly the pin's left and done items, by id and title. Two streams that match, or none, give no board.
pub fn board_for<'a>(u: &PinnedUpdate, projects: &'a [Project]) -> Option<&'a Stream> {
    let pin: Vec<(&str, String)> = u
        .left
        .iter()
        .chain(u.done.iter())
        .map(|i| (i.id.as_str(), split_odd(&i.title).0))
        .collect();
    if pin.is_empty() {
        return None;
    }
    let mut hits = projects.iter().flat_map(|p| p.streams.iter()).filter(|s| {
        let items: Vec<&Item> = s.items.iter().filter(|i| i.state != "dropped").collect();
        items.len() == pin.len()
            && pin
                .iter()
                .all(|(id, title)| items.iter().any(|i| i.id == *id && i.title == *title))
    });
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

fn blocked_note(b: &BlockedOn) -> String {
    let who = who_words(&b.kind);
    if b.what.is_empty() {
        who.to_string()
    } else {
        format!("{who}: {}", b.what)
    }
}

/// The task groups, in the fixed order, without empty groups. With the board's item states each item goes to its
/// state's group; without them only what the pin itself says is used: Left (blocked ones first) and Done, with an
/// unknown state still in its own group.
pub fn task_groups(u: &PinnedUpdate, board: Option<&Stream>) -> Vec<TaskGroup> {
    let mut groups: Vec<TaskGroup> = Vec::new();
    let mut push =
        |label: &'static str, row: TaskRow| match groups.iter_mut().find(|g| g.label == label) {
            Some(g) => g.rows.push(row),
            None => groups.push(TaskGroup {
                label,
                warn: matches!(label, "Waits on you" | "State not recognised"),
                rows: vec![row],
            }),
        };
    let all_stated = !u.left.is_empty() || !u.done.is_empty();
    if all_stated
        && u.left
            .iter()
            .chain(u.done.iter())
            .all(|i| i.state.is_some())
    {
        // The server passed each item's own state: no board needed.
        for item in u.left.iter().chain(u.done.iter()) {
            let state = item.state.clone().unwrap_or_default();
            if state == "dropped" {
                continue;
            }
            let (title, _) = split_odd(&item.title);
            push(
                group_of(&state, false, false),
                TaskRow {
                    id: item.id.clone(),
                    title: cut(&title, 60),
                    note: item.note.clone().filter(|n| !n.is_empty()),
                    odd_state: (!KNOWN.contains(&state.as_str())).then_some(state),
                },
            );
        }
    } else if let Some(board) = board {
        let by_id = |id: &str| board.items.iter().find(|i| i.id == id);
        for item in u.left.iter().chain(u.done.iter()) {
            let Some(it) = by_id(&item.id) else { continue };
            let owner = it.blocked_on.as_ref().is_some_and(|b| b.kind == "owner");
            let label = group_of(&it.state, owner, it.blocked_on.is_some());
            let odd = (!KNOWN.contains(&it.state.as_str())).then(|| it.state.clone());
            let note = item
                .note
                .clone()
                .filter(|n| !n.is_empty())
                .or_else(|| it.blocked_on.as_ref().filter(|_| !owner).map(blocked_note));
            push(
                label,
                TaskRow {
                    id: item.id.clone(),
                    title: cut(&it.title, 60),
                    note,
                    odd_state: odd,
                },
            );
        }
    } else {
        for item in &u.left {
            let (title, odd) = split_odd(&item.title);
            let label = if odd.is_some() {
                "State not recognised"
            } else {
                "Left"
            };
            push(
                label,
                TaskRow {
                    id: item.id.clone(),
                    title: cut(&title, 60),
                    note: item.note.clone().filter(|n| !n.is_empty()),
                    odd_state: odd,
                },
            );
        }
        for item in &u.done {
            push(
                "Done",
                TaskRow {
                    id: item.id.clone(),
                    title: cut(&split_odd(&item.title).0, 60),
                    note: item.note.clone().filter(|n| !n.is_empty()),
                    odd_state: None,
                },
            );
        }
    }
    let order = |l: &str| {
        GROUPS
            .iter()
            .position(|g| *g == l)
            .unwrap_or(if l == "Left" { 2 } else { 9 })
    };
    groups.sort_by_key(|g| order(g.label));
    groups
}

// ---- Waits, ages and boards (2026-11-13) ----------------------------------------------------------------------------

/// A wait in its report's own words: the short form when it gave one.
pub fn wait_words(w: &PinnedWait) -> String {
    w.short
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| w.text.clone())
}

/// What the owner is waited on for, each once: from the composer's structured waits, or, from a server that does
/// not send them, the words after the head of its waiting sentence (only the waiting-on-you part of it).
pub fn owner_waits(u: &PinnedUpdate) -> Vec<String> {
    if u.waits.is_empty() {
        if !u.blocked.waiting_on_you {
            return Vec::new();
        }
        let text = u.blocked.text.trim();
        let rest = text.split_once(": ").map(|(_, r)| r).unwrap_or(text);
        // An older sentence goes on with "Blocked: ..." for everyone else's waits; those are not the owner's.
        let mine = rest.split(" Blocked: ").next().unwrap_or(rest);
        return vec![mine.trim().to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    for w in u.waits.iter().filter(|w| w.on == "owner") {
        let words = wait_words(w);
        if !words.trim().is_empty() && !out.iter().any(|held| held.trim() == words.trim()) {
            out.push(words);
        }
    }
    out
}

/// Everyone else the thread waits on (an agent, a review, a machine, the landing lock, quota), whether or not it also
/// waits on the owner. Empty from a server that does not send structured waits.
pub fn other_waits(u: &PinnedUpdate) -> Vec<&PinnedWait> {
    u.waits.iter().filter(|w| w.on != "owner").collect()
}

/// A wait's head word for the Waiting-on list: who it waits on, with the board item when it is a task's.
pub fn other_wait_head(w: &PinnedWait) -> String {
    let who = if w.on == "unknown" {
        "Blocked"
    } else {
        who_words(&w.on)
    };
    match w.task.as_deref().filter(|t| !t.is_empty()) {
        Some(task) => format!("{who} ({task})"),
        None => who.to_string(),
    }
}

/// "just now", "4 min", "1 h 3 min", "2 d".
pub fn short_age(seconds: i64) -> String {
    let s = seconds.max(0);
    if s < 45 {
        "just now".into()
    } else if s < 3_600 {
        format!("{} min", ((s + 30) / 60).max(1))
    } else if s < 86_400 {
        let (h, m) = (s / 3_600, (s % 3_600) / 60);
        if m == 0 || h >= 6 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    } else {
        format!("{} d", s / 86_400)
    }
}

/// "4 min ago", or "just now".
pub fn ago(seconds: i64) -> String {
    match short_age(seconds) {
        n if n == "just now" => n,
        n => format!("{n} ago"),
    }
}

/// How much older the human update may be than the heartbeat before the page says both: two writes of one
/// `agent-report update` land seconds apart.
pub const TWO_AGES_AFTER_SECONDS: i64 = 600;

/// The human update's age and the heartbeat's, when they differ enough to say both: a hook keeps rewriting
/// thread.json (and its carried doing line) while the summary, done and left lists stay as old as status.json.
pub fn two_ages(u: &PinnedUpdate) -> Option<(i64, i64)> {
    let human = u.status_updated.as_ref()?.age_seconds?;
    let beat = u.heartbeat.as_ref()?.age_seconds?;
    (human - beat > TWO_AGES_AFTER_SECONDS).then_some((human, beat))
}

/// A human update's age: hours up to two days ("35 h" says more than "1 d" about a summary that old), else as
/// `short_age`.
pub fn update_age(seconds: i64) -> String {
    if (3_600..172_800).contains(&seconds) {
        format!("{} h", seconds / 3_600)
    } else {
        short_age(seconds)
    }
}

/// "35 h ago", or "just now".
pub fn update_ago(seconds: i64) -> String {
    match update_age(seconds) {
        n if n == "just now" => n,
        n => format!("{n} ago"),
    }
}

/// "updated 35 h ago · active 2 min ago": the human update first, so a fresh heartbeat never reads as fresh words.
pub fn two_ages_words(human: i64, beat: i64) -> String {
    format!(
        "updated {} · active {}",
        update_ago(human),
        update_ago(beat)
    )
}

/// The roster row's age: the human update's when a heartbeat is newer (so the row never reads fresher than its
/// words), else the newest report's. The flag says the words are older than six hours behind a live heartbeat.
pub fn roster_age(u: &PinnedUpdate) -> Option<(String, bool)> {
    match two_ages(u) {
        Some((human, _)) => Some((update_age(human), human > 6 * 3_600)),
        None => u
            .updated
            .as_ref()
            .and_then(|x| x.age_seconds)
            .map(|a| (short_age(a), false)),
    }
}

/// "active 24 min", "active 45 h", "active just now": how long ago the thread last reported anything, the heartbeat
/// included. In the words' own units (`update_age`), so "46 h" never sits over "active 1 d" or "1 h" over
/// "active 1 h 4 min" and reads as the older of the two.
pub fn active_words(seconds: i64) -> String {
    format!("active {}", update_age(seconds))
}

/// The roster row's activity age, shown under the words' age when a heartbeat is newer than the words. Before wave 8
/// the corner gave the newest report's age ("24 min"), so a working thread read as alive; wave 8 put the summary's
/// age there ("2 h") and dropped that. Both show now, each named, so neither stands for the other (2026-11-13
/// regression).
pub fn roster_active(u: &PinnedUpdate) -> Option<String> {
    // Only when it reads differently: "46 h" over "active 46 h" would add a line and say nothing.
    two_ages(u)
        .filter(|(human, beat)| update_age(*human) != update_age(*beat))
        .map(|(_, beat)| active_words(beat))
}

/// A loop's lines, short (roster) and full (sheet). On another writer's board the short line names the board as
/// shared and still carries the task and its rounds: the board's owner and stream are in the full line, and a 72-
/// character cut of "On the shared P board kept by vm.x, ..." used to stop before the task (2026-11-13 regression).
pub fn loop_lines(u: &PinnedUpdate) -> Option<(String, String)> {
    if u.looping.answer != "yes" {
        return None;
    }
    let text = u.looping.text.trim();
    let text = text.strip_prefix("Yes.").unwrap_or(text).trim();
    let full = if text.is_empty() {
        "Looping.".to_string()
    } else {
        format!("Looping: {text}")
    };
    let shared = u.board.as_ref().is_some_and(|b| b.shared);
    let task = u
        .looping
        .task
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let short = match (shared, task, u.looping.rounds) {
        (true, Some(task), Some(rounds)) => format!(
            "Looping (shared board): {task} has gone back for fixes {} without a passing review.",
            plural(rounds as usize, "time", "times")
        ),
        (true, Some(task), None) => format!("Looping (shared board): {task}."),
        _ => full.clone(),
    };
    Some((cut(&short, 72), full))
}

/// The quiet sheet's last words: the summary when there is one, else the last doing line with its age.
pub fn quiet_last(u: &PinnedUpdate, elapsed: i64) -> Option<String> {
    if let Some(s) = u.summary.as_ref().filter(|s| !s.trim().is_empty()) {
        return Some(format!("The last one said: {}", s.trim()));
    }
    let now = u.now.as_ref().map(|n| n.trim()).filter(|n| !n.is_empty())?;
    let age = u
        .heartbeat
        .as_ref()
        .and_then(|h| h.age_seconds)
        .or_else(|| u.updated.as_ref().and_then(|x| x.age_seconds));
    Some(match age {
        Some(a) => format!("Its last doing line, {}: {now}", ago(a + elapsed)),
        None => format!("Its last doing line: {now}"),
    })
}

/// "Shared Projects board, kept by vm.projects-lead": said where a pin's tasks and loop are another writer's.
pub fn shared_board_words(u: &PinnedUpdate) -> Option<String> {
    let b = u.board.as_ref().filter(|b| b.shared)?;
    let stream = b
        .stream_label
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| b.stream.clone());
    Some(format!("Shared {stream} board, kept by {}", b.writer))
}

/// "1 thing to open", "4 things to open".
pub fn things(n: usize) -> String {
    plural(n, "thing to open", "things to open")
}

pub fn questions(n: usize) -> String {
    plural(n, "question", "questions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn update(reported: Value) -> PinnedUpdate {
        PinnedUpdate {
            report_state: "fresh".into(),
            reported: Some(reported),
            ..Default::default()
        }
    }

    #[test]
    fn a_report_without_v2_fields_reads_as_nothing_reported() {
        let u = PinnedUpdate {
            report_state: "fresh".into(),
            ..Default::default()
        };
        let r = reported(&u);
        assert_eq!(r, Reported::default());
        // no state from the server: never guessed as working
        assert_eq!(state_word(&u, &r, 0), Word::Reporting);
    }

    #[test]
    fn one_bad_row_drops_only_that_row() {
        let u = update(json!({
            "stages": [{"label": "Study", "state": "done"}, {"label": 5}, {"label": "Land", "state": "next"}],
            "deliverables": [{"id": "a", "name": "A", "kind": "concept", "version": "v1"}, "nonsense"]
        }));
        let r = reported(&u);
        assert_eq!(r.stages.len(), 2);
        assert_eq!(r.deliverables.len(), 1);
    }

    #[test]
    fn state_words_follow_the_rules_order() {
        let ready = update(
            json!({"deliverables": [{"id": "a", "name": "A", "kind": "concept", "version": "v1", "for": "owner", "state": "ready"}], "state": "working"}),
        );
        assert_eq!(state_word(&ready, &reported(&ready), 0), Word::ReadyForYou);
        let card = update(json!({"state": "working"}));
        assert_eq!(state_word(&card, &reported(&card), 2), Word::WaitingOnYou);
        let mut waiting = update(json!({"state": "working"}));
        waiting.blocked.text = "Waiting on an agent: B lands".into();
        assert_eq!(state_word(&waiting, &reported(&waiting), 0), Word::Waiting);
        for (s, w) in [
            ("working", Word::Working),
            ("idle", Word::Idle),
            ("done", Word::Done),
        ] {
            let u = update(json!({ "state": s }));
            assert_eq!(state_word(&u, &reported(&u), 0), w);
        }
        let mut stale = update(json!({}));
        stale.report_state = "stale".into();
        assert_eq!(state_word(&stale, &reported(&stale), 3), Word::Quiet);
    }

    #[test]
    fn the_stage_line_counts_without_skipped_steps() {
        let stages: Vec<Stage> = serde_json::from_value(json!([
            {"label": "Study", "state": "done"}, {"label": "Old", "state": "skipped"},
            {"label": "Build", "state": "done"}, {"label": "Reviews", "state": "now", "round": 2},
            {"label": "Your look", "state": "waiting", "by": "owner"}, {"label": "Max reviews", "state": "next"},
            {"label": "Land", "state": "next"}
        ]))
        .unwrap();
        assert_eq!(
            stage_line(&stages),
            Some(("Reviews, round 2".to_string(), 3, 6))
        );
        assert_eq!(stage_tone(&stages[4]), "you");
        assert!(stage_line(&stages[..1]).is_none());
    }

    #[test]
    fn only_share_paths_open_through_the_route() {
        assert_eq!(
            share_relative("/mnt/Cursor/share/aac/round2/concept c2.html", false).as_deref(),
            Some("share/aac/round2/concept c2.html")
        );
        assert_eq!(
            share_relative("\\\\TRUENAS\\Cursor\\share\\x.md", false).as_deref(),
            Some("share/x.md")
        );
        assert!(share_relative("/Volumes/Cursor/share/../etc/passwd", false).is_none());
        assert!(share_relative("/home/me/state.json", false).is_none());
        assert!(
            share_relative(
                "/mnt/Cursor/agent-reports/v1/writers/vm.a/media/x.png",
                false
            )
            .is_none()
        );
        assert!(
            share_relative(
                "/mnt/Cursor/agent-reports/v1/writers/vm.a/media/x.png",
                true
            )
            .is_some()
        );
        assert_eq!(
            route_href("share/a b/c.html"),
            "/api/status/deliverable?path=share%2Fa%20b%2Fc.html"
        );
        assert_eq!(
            opener(Some("/home/me/lead-state.md"), None),
            Opener::CopyPath("/home/me/lead-state.md".into())
        );
        assert!(matches!(
            opener(None, Some("https://x.test/a")),
            Opener::Link(_)
        ));
        assert_eq!(opener(None, Some("javascript:alert(1)")), Opener::Nothing);
    }

    #[test]
    fn marks_say_new_new_since_or_opened() {
        assert_eq!(mark("v2", &vec![]), Mark::New);
        assert_eq!(
            mark("v2", &vec![("v1".into(), 5.0)]),
            Mark::NewSince("v1".into())
        );
        assert_eq!(
            mark("v2", &vec![("v1".into(), 5.0), ("v2".into(), 9.0)]),
            Mark::Opened(9.0)
        );
    }

    #[test]
    fn one_unknown_task_state_marks_one_row_and_the_list_still_shows() {
        let u = PinnedUpdate {
            left: vec![
                PinnedItem {
                    id: "a".into(),
                    title: "Sweep".into(),
                    ..Default::default()
                },
                PinnedItem {
                    id: "b".into(),
                    title: "Second review (state not recognised: queued)".into(),
                    ..Default::default()
                },
            ],
            done: vec![PinnedItem {
                id: "c".into(),
                title: "Study".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let plain = task_groups(&u, None);
        assert_eq!(
            plain
                .iter()
                .map(|g| (g.label, g.rows.len()))
                .collect::<Vec<_>>(),
            vec![("Left", 1), ("State not recognised", 1), ("Done", 1)]
        );
        assert_eq!(plain[1].rows[0].odd_state.as_deref(), Some("queued"));
        // with the board's states each item goes to its own group
        let stream = Stream {
            items: vec![
                Item {
                    id: "a".into(),
                    title: "Sweep".into(),
                    state: "working".into(),
                    ..Default::default()
                },
                Item {
                    id: "b".into(),
                    title: "Second review".into(),
                    state: "queued".into(),
                    ..Default::default()
                },
                Item {
                    id: "c".into(),
                    title: "Study".into(),
                    state: "done".into(),
                    ..Default::default()
                },
                Item {
                    id: "d".into(),
                    title: "Gone".into(),
                    state: "dropped".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let projects = vec![Project {
            streams: vec![stream],
            ..Default::default()
        }];
        let board = board_for(&u, &projects);
        assert!(board.is_some());
        let grouped = task_groups(&u, board);
        assert_eq!(
            grouped.iter().map(|g| g.label).collect::<Vec<_>>(),
            vec!["In work", "State not recognised", "Done"]
        );
        // two boards that both match say nothing about states
        let twice = vec![projects[0].clone(), projects[0].clone()];
        assert!(board_for(&u, &twice).is_none());
    }

    #[test]
    fn the_servers_own_item_states_group_without_a_board() {
        let item = |id: &str, state: &str| PinnedItem {
            id: id.into(),
            title: format!("Task {id}"),
            state: Some(state.into()),
            ..Default::default()
        };
        let u = PinnedUpdate {
            left: vec![
                item("a", "working"),
                item("b", "queued"),
                item("c", "review"),
                item("d", "todo"),
            ],
            done: vec![item("e", "landed")],
            ..Default::default()
        };
        let groups = task_groups(&u, None);
        assert_eq!(
            groups.iter().map(|g| g.label).collect::<Vec<_>>(),
            vec![
                "In work",
                "State not recognised",
                "To do",
                "In review",
                "Done"
            ]
        );
        assert_eq!(groups[1].rows[0].odd_state.as_deref(), Some("queued"));
        // the writer the server matched finds that thread's open cards
        let r = Reported {
            writer: Some("vm.a".into()),
            ..Default::default()
        };
        let cards = vec![
            Card {
                id: "1".into(),
                writer: "vm.a".into(),
                ..Default::default()
            },
            Card {
                id: "2".into(),
                writer: "vm.b".into(),
                ..Default::default()
            },
        ];
        assert_eq!(open_cards_for(&r, &cards).len(), 1);
    }

    #[test]
    fn big_groups_fold() {
        let g = TaskGroup {
            label: "In review",
            warn: false,
            rows: vec![TaskRow::default(); 7],
        };
        assert!(g.folds());
        let d = TaskGroup {
            label: "Done",
            warn: false,
            rows: vec![TaskRow::default(); 4],
        };
        assert!(d.folds());
        let s = TaskGroup {
            label: "To do",
            warn: false,
            rows: vec![TaskRow::default(); 6],
        };
        assert!(!s.folds());
    }

    #[test]
    fn cuts_at_a_whole_word() {
        assert_eq!(cut("short", 60), "short");
        let long = "Review of today's Settings: what is wrong, the duplicates and the dead buttons";
        let c = cut(long, 60);
        assert!(c.chars().count() <= 61 && c.ends_with('…') && !c.contains("dupli…"));
        assert_eq!(first_sentence("One. Two.", 140), "One.");
    }

    #[test]
    fn main_comes_first_and_only_once() {
        let list: Vec<Deliverable> = serde_json::from_value(json!([
            {"id": "a", "name": "A", "kind": "report", "version": "v1", "updatedAt": "2026-11-11T10:00:00Z"},
            {"id": "b", "name": "B", "kind": "concept", "version": "v1", "main": true},
            {"id": "c", "name": "C", "kind": "concept", "version": "v1", "main": true, "for": "owner"}
        ]))
        .unwrap();
        let o = ordered(&list);
        assert_eq!(
            o.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
        assert_eq!(o.iter().filter(|d| d.main == Some(true)).count(), 1);
    }

    // ---- 2026-11-13: waits beside an owner wait, two ages, the quiet doing line, shared boards ----

    /// The settings-managers shape, as the composer now sends it: one owner wait (from the thread and its status,
    /// already once), a second owner wait on a task, and waits on a machine, a review and an agent.
    fn mixed_waits() -> PinnedUpdate {
        serde_json::from_value(json!({
            "id": "t1", "title": "Settings managers", "machine": "vm", "reportState": "fresh",
            "blocked": { "text": "Waiting on you: Open the preview and answer card SM-0; Answers to WL1 to WL5. Blocked: Browsers reserved (waiting on resource).", "waitingOnYou": true },
            "looping": { "answer": "no", "text": "No." },
            "waits": [
                { "on": "owner", "text": "Open the preview and answer card SM-0", "from": "thread", "since": "2026-11-11T16:54:51Z" },
                { "on": "resource", "text": "Browsers on the VM and Nas1 are reserved.", "task": "SWEEP", "from": "tasks", "since": "2026-11-11T18:21:07Z" },
                { "on": "owner", "text": "Answers to WL1 to WL5.", "task": "WORK-LANES", "from": "tasks" },
                { "on": "review", "text": "Second reviewer pass.", "task": "PAGE-FRAME", "from": "tasks" },
                { "on": "agent", "text": "The fix worker's rebuild.", "short": "Rebuild", "task": "GLM-FIXES", "from": "tasks" },
                { "on": "unknown", "text": "Blocked; the report does not say on what.", "task": "LAND", "from": "tasks" },
                "a row this page cannot read"
            ],
            "statusUpdated": { "text": "Updated 1 day ago", "at": "2026-11-11T18:21:12Z", "ageSeconds": 126000 },
            "heartbeat": { "text": "Active 2 min ago", "at": "2026-11-13T05:30:00Z", "ageSeconds": 120 },
            "now": "The review said READY; the press test waits for the browsers.",
            "summary": "The review said READY; the press test waits for the browsers."
        }))
        .unwrap()
    }

    #[test]
    fn waits_on_others_show_beside_an_owner_wait() {
        let u = mixed_waits();
        // the unreadable row is left out, the rest kept
        assert_eq!(u.waits.len(), 6);
        assert_eq!(
            owner_waits(&u),
            [
                "Open the preview and answer card SM-0",
                "Answers to WL1 to WL5."
            ]
        );
        let others: Vec<(String, String)> = other_waits(&u)
            .into_iter()
            .map(|w| (other_wait_head(w), wait_words(w)))
            .collect();
        assert_eq!(
            others,
            [
                (
                    "A machine (SWEEP)".to_string(),
                    "Browsers on the VM and Nas1 are reserved.".to_string()
                ),
                (
                    "A review (PAGE-FRAME)".into(),
                    "Second reviewer pass.".into()
                ),
                ("An agent (GLM-FIXES)".into(), "Rebuild".into()),
                (
                    "Blocked (LAND)".into(),
                    "Blocked; the report does not say on what.".into()
                ),
            ]
        );
    }

    #[test]
    fn an_older_server_sentence_keeps_only_the_owner_part_for_you() {
        let mut u = mixed_waits();
        u.waits.clear();
        assert_eq!(
            owner_waits(&u),
            ["Open the preview and answer card SM-0; Answers to WL1 to WL5."]
        );
        assert!(other_waits(&u).is_empty());
    }

    #[test]
    fn an_old_summary_keeps_its_own_age_beside_a_fresh_heartbeat() {
        let u = mixed_waits();
        assert_eq!(two_ages(&u), Some((126_000, 120)));
        assert_eq!(
            two_ages_words(126_000, 120),
            "updated 35 h ago · active 2 min ago"
        );
        assert_eq!(
            two_ages_words(35 * 3600, 30),
            "updated 35 h ago · active just now"
        );
        // Both in hours under two days, so an old heartbeat never reads as "1 d" beside "36 h".
        assert_eq!(
            two_ages_words(36 * 3600, 35 * 3600),
            "updated 36 h ago · active 35 h ago"
        );
        // one update writes both files: no second age
        let mut same = u.clone();
        same.heartbeat.as_mut().unwrap().age_seconds = Some(126_000 - 30);
        assert_eq!(two_ages(&same), None);
        // The roster corner says the summary's age, not the heartbeat's, and the Now line says it the same way.
        assert_eq!(roster_age(&u), Some(("35 h".to_string(), true)));
        assert_eq!(format!("as of {}", update_ago(126_000)), "as of 35 h ago");
        assert_eq!(update_age(3 * 86_400), "3 d");
        let mut thread_only = u;
        thread_only.status_updated = None;
        assert_eq!(two_ages(&thread_only), None);
        assert_eq!(roster_age(&thread_only), None);
    }

    #[test]
    fn a_quiet_thread_without_a_summary_shows_its_last_doing_line_with_its_age() {
        let mut u: PinnedUpdate = serde_json::from_value(json!({
            "id": "scout", "title": "Scout", "machine": "vm", "reportState": "stale",
            "staleNote": "No status update for 2 days. If it is finished, unpin it in T3.",
            "now": "Reading the wonderer's notes for batch 3.",
            "heartbeat": { "text": "Active 2 days ago", "at": "2026-11-11T05:00:00Z", "ageSeconds": 172800 },
            "updated": { "text": "Updated 2 days ago", "at": "2026-11-11T05:00:00Z", "ageSeconds": 172800 }
        }))
        .unwrap();
        assert_eq!(
            quiet_last(&u, 0).as_deref(),
            Some("Its last doing line, 2 d ago: Reading the wonderer's notes for batch 3.")
        );
        u.summary = Some("Batch 2 done.".into());
        assert_eq!(
            quiet_last(&u, 0).as_deref(),
            Some("The last one said: Batch 2 done.")
        );
        u.summary = None;
        u.now = None;
        assert_eq!(quiet_last(&u, 0), None);
    }

    #[test]
    fn a_shared_stream_board_names_its_owner() {
        let mut u: PinnedUpdate = serde_json::from_value(json!({
            "id": "split", "title": "Split v3", "machine": "vm", "reportState": "fresh",
            "board": { "writer": "vm.projects-lead", "project": "puppet-master", "stream": "P", "streamLabel": "Projects", "shared": true }
        }))
        .unwrap();
        assert_eq!(
            shared_board_words(&u).as_deref(),
            Some("Shared Projects board, kept by vm.projects-lead")
        );
        u.board.as_mut().unwrap().shared = false;
        assert_eq!(shared_board_words(&u), None);
    }

    // ---- 2026-11-13 regression: what wave 8 stopped showing on the owner's real pins ----

    /// pm7-build's real shape: the hook rewrote thread.json 20 min ago, the status (and its words) is 2 h old.
    #[test]
    fn the_roster_corner_keeps_the_active_age_beside_an_older_summary() {
        let u: PinnedUpdate = serde_json::from_value(json!({
            "id": "pm7", "title": "Build PMConcept7", "machine": "vm", "reportState": "fresh",
            "updated": { "text": "Updated 20 min ago", "at": "2026-11-13T13:32:25Z", "ageSeconds": 1200 },
            "statusUpdated": { "text": "Updated 2 hours ago", "at": "2026-11-13T11:45:55Z", "ageSeconds": 7620 },
            "heartbeat": { "text": "Active 20 min ago", "at": "2026-11-13T13:32:25Z", "ageSeconds": 1200 }
        }))
        .unwrap();
        // The words' age stays (a summary never reads as current), and the activity the old corner gave is back.
        assert_eq!(roster_age(&u), Some(("2 h".to_string(), false)));
        assert_eq!(roster_active(&u).as_deref(), Some("active 20 min"));
        assert_eq!(active_words(30), "active just now");
        // The same units as the words' age: never "46 h" over "active 1 d", or "1 h" over "active 1 h 4 min".
        assert_eq!(active_words(45 * 3600 + 120), "active 45 h");
        assert_eq!(active_words(3840), "active 1 h");
        assert_eq!(update_age(83 * 60), "1 h");
        // Both ages in the same hour (onboarding's real 45.8 h and 45.1 h): no second line that repeats the first.
        let mut close = u.clone();
        close.status_updated.as_mut().unwrap().age_seconds = Some(165_000);
        close.heartbeat.as_mut().unwrap().age_seconds = Some(162_400);
        assert_eq!(roster_age(&close), Some(("45 h".to_string(), true)));
        assert_eq!(roster_active(&close), None);
        // One update writes both files: one age, as before wave 8, and no second line.
        let mut same = u.clone();
        same.status_updated.as_mut().unwrap().age_seconds = Some(1260);
        assert_eq!(roster_active(&same), None);
        assert_eq!(roster_age(&same), Some(("20 min".to_string(), false)));
    }

    /// claude-split-v3's real shape: a worker on claude-projects-refresh's shared P board, task P-BATCH2 four rounds.
    #[test]
    fn a_shared_board_loop_keeps_its_task_and_rounds_in_the_roster_line() {
        let mut u: PinnedUpdate = serde_json::from_value(json!({
            "id": "split", "title": "Improve Project Management Page Readability", "machine": "vm", "reportState": "fresh",
            "looping": { "answer": "yes", "task": "P-BATCH2", "rounds": 4,
                "text": "Yes. On the shared P board kept by vm.claude-projects-refresh, P-BATCH2 has gone back for fixes 4 times without a passing review." },
            "board": { "writer": "vm.claude-projects-refresh", "project": "puppet-master", "stream": "P", "shared": true }
        }))
        .unwrap();
        let (short, full) = loop_lines(&u).unwrap();
        assert_eq!(
            short,
            "Looping (shared board): P-BATCH2 has gone back for fixes 4 times…"
        );
        assert!(short.chars().count() <= 73, "{short}");
        assert_eq!(
            full,
            "Looping: On the shared P board kept by vm.claude-projects-refresh, P-BATCH2 has gone back for fixes 4 times without a passing review."
        );
        // The thread's own board: the line is cut from the composer's words, as before.
        u.board.as_mut().unwrap().shared = false;
        u.looping.text =
            "Yes. P-BATCH2 has gone back for fixes 4 times without a passing review.".into();
        assert_eq!(
            loop_lines(&u).unwrap().0,
            "Looping: P-BATCH2 has gone back for fixes 4 times without a passing…"
        );
        // An older server sends no task: the full words, cut.
        u.board.as_mut().unwrap().shared = true;
        u.looping.task = None;
        assert!(loop_lines(&u).unwrap().0.starts_with("Looping: P-BATCH2"));
        u.looping.answer = "no".into();
        assert_eq!(loop_lines(&u), None);
    }
}
