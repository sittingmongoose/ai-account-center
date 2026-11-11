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
    let at = head.rfind(' ').filter(|&i| i > max / 2).unwrap_or(head.len());
    format!(
        "{}…",
        head[..at].trim_end_matches(|c: char| c == ',' || c == ';' || c == ':' || c == ' ')
    )
}

/// The first sentence of a text, cut at `max`.
pub fn first_sentence(text: &str, max: usize) -> String {
    let t = text.trim();
    let end = t
        .find(". ")
        .map(|i| i + 1)
        .unwrap_or(t.len());
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
    format!(
        "/api/status/deliverable?path={}",
        js_encode(relative)
    )
}

/// encodeURIComponent, without needing a browser (native tests).
fn js_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
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
    let mut hits = projects
        .iter()
        .flat_map(|p| p.streams.iter())
        .filter(|s| {
            let items: Vec<&Item> = s.items.iter().filter(|i| i.state != "dropped").collect();
            items.len() == pin.len()
                && pin.iter().all(|(id, title)| {
                    items
                        .iter()
                        .any(|i| i.id == *id && i.title == *title)
                })
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
    let mut push = |label: &'static str, row: TaskRow| {
        match groups.iter_mut().find(|g| g.label == label) {
            Some(g) => g.rows.push(row),
            None => groups.push(TaskGroup {
                label,
                warn: matches!(label, "Waits on you" | "State not recognised"),
                rows: vec![row],
            }),
        }
    };
    let all_stated = !u.left.is_empty() || !u.done.is_empty();
    if all_stated && u.left.iter().chain(u.done.iter()).all(|i| i.state.is_some()) {
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
            let note = item.note.clone().filter(|n| !n.is_empty()).or_else(|| {
                it.blocked_on
                    .as_ref()
                    .filter(|_| !owner)
                    .map(blocked_note)
            });
            push(
                label,
                TaskRow {
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
        let ready = update(json!({"deliverables": [{"id": "a", "name": "A", "kind": "concept", "version": "v1", "for": "owner", "state": "ready"}], "state": "working"}));
        assert_eq!(state_word(&ready, &reported(&ready), 0), Word::ReadyForYou);
        let card = update(json!({"state": "working"}));
        assert_eq!(state_word(&card, &reported(&card), 2), Word::WaitingOnYou);
        let mut waiting = update(json!({"state": "working"}));
        waiting.blocked.text = "Waiting on an agent: B lands".into();
        assert_eq!(state_word(&waiting, &reported(&waiting), 0), Word::Waiting);
        for (s, w) in [("working", Word::Working), ("idle", Word::Idle), ("done", Word::Done)] {
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
        assert!(share_relative("/mnt/Cursor/agent-reports/v1/writers/vm.a/media/x.png", false).is_none());
        assert!(share_relative("/mnt/Cursor/agent-reports/v1/writers/vm.a/media/x.png", true).is_some());
        assert_eq!(
            route_href("share/a b/c.html"),
            "/api/status/deliverable?path=share%2Fa%20b%2Fc.html"
        );
        assert_eq!(
            opener(Some("/home/me/lead-state.md"), None),
            Opener::CopyPath("/home/me/lead-state.md".into())
        );
        assert!(matches!(opener(None, Some("https://x.test/a")), Opener::Link(_)));
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
                PinnedItem { id: "a".into(), title: "Sweep".into(), ..Default::default() },
                PinnedItem { id: "b".into(), title: "Second review (state not recognised: queued)".into(), ..Default::default() },
            ],
            done: vec![PinnedItem { id: "c".into(), title: "Study".into(), ..Default::default() }],
            ..Default::default()
        };
        let plain = task_groups(&u, None);
        assert_eq!(
            plain.iter().map(|g| (g.label, g.rows.len())).collect::<Vec<_>>(),
            vec![("Left", 1), ("State not recognised", 1), ("Done", 1)]
        );
        assert_eq!(plain[1].rows[0].odd_state.as_deref(), Some("queued"));
        // with the board's states each item goes to its own group
        let stream = Stream {
            items: vec![
                Item { id: "a".into(), title: "Sweep".into(), state: "working".into(), ..Default::default() },
                Item { id: "b".into(), title: "Second review".into(), state: "queued".into(), ..Default::default() },
                Item { id: "c".into(), title: "Study".into(), state: "done".into(), ..Default::default() },
                Item { id: "d".into(), title: "Gone".into(), state: "dropped".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let projects = vec![Project { streams: vec![stream], ..Default::default() }];
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
    fn big_groups_fold() {
        let g = TaskGroup { label: "In review", warn: false, rows: vec![TaskRow::default(); 7] };
        assert!(g.folds());
        let d = TaskGroup { label: "Done", warn: false, rows: vec![TaskRow::default(); 4] };
        assert!(d.folds());
        let s = TaskGroup { label: "To do", warn: false, rows: vec![TaskRow::default(); 6] };
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
        assert_eq!(o.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(o.iter().filter(|d| d.main == Some(true)).count(), 1);
    }
}
