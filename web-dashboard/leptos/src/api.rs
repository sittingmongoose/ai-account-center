//! The Status view (docs/status-tab.md, view v1) and the calls that read and answer it.
//!
//! The server decides every number, word and state. These types only carry them to the page; a missing value stays
//! `None` and is drawn as missing, never as zero.

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

pub const STATUS_VIEW_VERSION: u32 = 1;

/// Text and lists the server sends as `null` when unknown read as empty, which the page draws as missing. Numbers are
/// left strict: a null count must not turn into a zero.
pub(crate) fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusView {
    pub version: u32,
    #[serde(deserialize_with = "nullable")]
    pub generated_at: String,
    pub refresh: Refresh,
    pub headline: Headline,
    #[serde(deserialize_with = "nullable")]
    pub sources: Vec<Source>,
    pub cards: Cards,
    #[serde(deserialize_with = "nullable")]
    pub projects: Vec<Project>,
    pub threads: Rows<Thread>,
    pub machines: Rows<Machine>,
    pub simlock: Simlock,
    pub usage: Usage,
    /// Estimated spend per work stream from quota-watch; absent from servers that don't read it yet.
    pub spend: Option<Spend>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Refresh {
    pub poll_seconds: u32,
}

impl Default for Refresh {
    fn default() -> Self {
        Self { poll_seconds: 30 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Headline {
    #[serde(deserialize_with = "nullable")]
    pub tone: String,
    #[serde(deserialize_with = "nullable")]
    pub runs: Vec<Run>,
    pub counts: HeadlineCounts,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HeadlineCounts {
    pub open_cards: u32,
    pub blocked_items: u32,
    pub working_threads: u32,
    pub stale_sources: u32,
}

/// A piece of a line with its own weight and tone ("**3 questions** for you").
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    pub strong: bool,
    pub tone: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Source {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub as_of: Option<String>,
    pub age_seconds: Option<i64>,
    pub stale_after_seconds: Option<i64>,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
}

impl Source {
    pub fn is_ok(&self) -> bool {
        self.state == "ok"
    }

    /// How a reading this source supplied to another source's panel is labelled while this source is not `ok`:
    /// "stale, read 6m ago" or "cached, read 1m ago" (the last good reading), never nothing. None while `ok`.
    pub fn reading_words(&self, elapsed_seconds: i64) -> Option<String> {
        if self.is_ok() {
            return None;
        }
        let word = if self.state == "stale" {
            "stale"
        } else {
            "cached"
        };
        Some(
            match crate::fmt::live_age(self.age_seconds, elapsed_seconds) {
                Some(age) if self.as_of.is_some() => {
                    format!("{word}, read {}", crate::fmt::age(age))
                }
                _ => "not read yet".to_string(),
            },
        )
    }

    /// This source's problem, named, for a panel it is not the main source of. None while `ok`.
    pub fn problem(&self) -> Option<String> {
        if self.is_ok() {
            return None;
        }
        let label = if self.label.is_empty() {
            &self.id
        } else {
            &self.label
        };
        Some(if self.detail.is_empty() {
            format!("{label}: {}", self.state)
        } else {
            format!("{label}: {}", self.detail)
        })
    }
}

/// quota-watch's estimates, as the server words them. Every value here is an estimate except rows marked otherwise.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Spend {
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    pub as_of: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub method: String,
    #[serde(deserialize_with = "nullable")]
    pub periods: Vec<SpendPeriod>,
    #[serde(deserialize_with = "nullable")]
    pub budgets: Vec<SpendBudget>,
    #[serde(deserialize_with = "nullable")]
    pub unattributed: Vec<SpendUnattributed>,
    #[serde(deserialize_with = "nullable")]
    pub machines_not_scanned: Vec<SpendMachine>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendPeriod {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub coverage_text: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub streams: Vec<SpendStream>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendStream {
    #[serde(deserialize_with = "nullable")]
    pub stream: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// A bucket such as "(no stream)" rather than a real stream.
    pub bucket: bool,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<SpendRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendRow {
    #[serde(deserialize_with = "nullable")]
    pub account: String,
    #[serde(deserialize_with = "nullable")]
    pub meter_label: String,
    #[serde(deserialize_with = "nullable")]
    pub value_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendBudget {
    #[serde(deserialize_with = "nullable")]
    pub stream_label: String,
    #[serde(deserialize_with = "nullable")]
    pub desc: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    pub over_limit: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendUnattributed {
    #[serde(deserialize_with = "nullable")]
    pub account: String,
    #[serde(deserialize_with = "nullable")]
    pub meter_label: String,
    #[serde(deserialize_with = "nullable")]
    pub amount_text: String,
    #[serde(deserialize_with = "nullable")]
    pub reason: String,
    #[serde(deserialize_with = "nullable")]
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SpendMachine {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    pub error: Option<String>,
    pub runs_missed: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cards {
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    #[serde(deserialize_with = "nullable")]
    pub open: Vec<Card>,
    #[serde(deserialize_with = "nullable")]
    pub answered: Vec<AnsweredCard>,
    pub answer_history_problem: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub batches: Vec<Batch>,
    /// True only when the server's latest scan read every writer folder: then a card missing from `open` has closed.
    /// Anything else (no field, a share error, a capped scan, the first scan after a restart) proves nothing.
    pub complete: Option<bool>,
}

impl Cards {
    /// Whether drafts of cards missing from this list may be dropped.
    pub fn proves_closed(&self) -> bool {
        self.complete == Some(true)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Card {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// "question" (default), "objection" (a call already made; the owner may object) or "action" (owner-only task).
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    /// The short id the owner can quote anywhere ("F-Q3").
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub seq: u64,
    #[serde(deserialize_with = "nullable")]
    pub card_sha256: String,
    #[serde(deserialize_with = "nullable")]
    pub project: String,
    pub stream: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub asked_by: String,
    pub asker_state: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub now: String,
    pub now_as_of: Option<String>,
    pub old_answer: Option<OldAnswer>,
    pub what_changed: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub options: Vec<CardOption>,
    pub recommendation: Option<Recommendation>,
    pub multiple: bool,
    #[serde(deserialize_with = "nullable")]
    pub urgency: String,
    pub blocks: Option<String>,
    pub batch: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub evidence: Vec<Link>,
    /// Optional sections, only what this question needs (comparisons, tables, media...).
    #[serde(deserialize_with = "nullable")]
    pub sections: Vec<Block>,
    pub needed_by: Option<NeededBy>,
    pub exception: Option<Exception>,
    pub reversible: Option<Reversible>,
    /// A badge such as Scout's adopt / adapt / keep ours / reject; the deck can filter on it.
    pub verdict: Option<String>,
    /// A group within the batch, with its own "use the recommendations for this group".
    pub group: Option<String>,
    /// Whether the judges agreed; split cards are where the owner's time goes.
    pub agreement: Option<Agreement>,
    /// What the asker is unsure of.
    pub doubt: Option<String>,
    /// A rule card that settles a whole class of cases.
    pub applies_to: Option<AppliesTo>,
    #[serde(deserialize_with = "nullable")]
    pub changes_canon: Vec<CanonChange>,
    /// Other card ids this one depends on or affects.
    #[serde(deserialize_with = "nullable")]
    pub related: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub replies: Vec<Reply>,
    #[serde(deserialize_with = "nullable")]
    pub notes: Vec<SentNote>,
    #[serde(deserialize_with = "nullable")]
    pub asked_at: String,
    pub age_seconds: Option<i64>,
    /// The report folder that asked (`<machine>.<name>`).
    #[serde(deserialize_with = "nullable")]
    pub writer: String,
    /// Why the card can't be trusted right now (for example its file was damaged after it was read).
    pub problem: Option<String>,
    /// False while there is a problem: the server would refuse an answer, so the page offers none. Absent means true.
    pub answerable: Option<bool>,
}

impl Card {
    /// A card's identity for drafts and keyed lists: a new version of the question is a new card.
    pub fn version_key(&self) -> String {
        format!("{}#{}#{}", self.id, self.seq, self.card_sha256)
    }

    pub fn is_question(&self) -> bool {
        self.kind.is_empty() || self.kind == "question"
    }

    pub fn can_answer(&self) -> bool {
        self.answerable.unwrap_or(true) && self.kind != "sample"
    }

    /// A measurement card: the owner's own judgement, never anchored by a recommendation.
    pub fn is_judgement(&self) -> bool {
        self.kind == "judgement"
    }

    pub fn short(&self) -> String {
        if self.label.is_empty() {
            self.id.rsplit("--").next().unwrap_or(&self.id).to_string()
        } else {
            self.label.clone()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OldAnswer {
    #[serde(deserialize_with = "nullable")]
    pub text: String,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CardOption {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub gets: String,
    #[serde(deserialize_with = "nullable")]
    pub costs: String,
    #[serde(deserialize_with = "nullable")]
    pub media: Vec<Media>,
    #[serde(deserialize_with = "nullable")]
    pub sections: Vec<Block>,
    /// "keep" (ours), "borrow" (from others) or "own" (new), when the asker says.
    pub stance: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Agreement {
    pub same: Option<bool>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppliesTo {
    pub count: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CanonChange {
    #[serde(rename = "ref")]
    #[serde(deserialize_with = "nullable")]
    pub reference: String,
    #[serde(deserialize_with = "nullable")]
    pub what: String,
}

/// A screenshot or a video; `url`/`posterUrl` are served by AAC from the asking writer's own folder.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Media {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub url: String,
    pub poster_url: Option<String>,
    pub caption: Option<String>,
    /// "built" or "mockup".
    pub label: Option<String>,
}

/// An optional section of a card. Fields beyond `type`, `title` and `fold` depend on the type; the page reads the
/// ones its type uses and shows an unknown type as such, never silently drops it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Block {
    #[serde(rename = "type")]
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub title: Option<String>,
    pub fold: bool,
    pub unknown: bool,
    pub body: Option<String>,
    pub tone: Option<String>,
    pub ordered: bool,
    #[serde(deserialize_with = "nullable")]
    pub columns: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    pub items: Vec<serde_json::Value>,
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<serde_json::Value>,
    /// planChange: the document, the unit in it, its words now and the proposed words.
    pub doc: Option<String>,
    pub unit: Option<String>,
    pub now: Option<String>,
    pub proposed: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NeededBy {
    pub at: Option<String>,
    pub if_no_answer: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Exception {
    #[serde(deserialize_with = "nullable")]
    pub rule: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Reversible {
    pub yes: bool,
    pub how: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recommendation {
    #[serde(deserialize_with = "nullable")]
    pub key: String,
    #[serde(deserialize_with = "nullable")]
    pub why: String,
    /// "low", "medium" or "high".
    pub confidence: Option<String>,
    /// What would change the recommendation.
    pub flip: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Link {
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Reply {
    pub at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SentNote {
    /// "note", or "object" (an objection, in the owner's words; the card stays open).
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub note: String,
    pub at: Option<String>,
    pub delivery: Delivery,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AnsweredCard {
    pub card: Card,
    pub answer: Answer,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Answer {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    pub choice: Option<Vec<String>>,
    pub note: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub via: String,
    #[serde(deserialize_with = "nullable")]
    pub answered_at: String,
    /// The kind of device the answer came from ("iPhone", "Mac"...).
    #[serde(deserialize_with = "nullable")]
    pub device: String,
    pub delivery: Delivery,
}

impl Answer {
    /// The answered row's first line: a completed action says Done; an answer names its choices (`labels`); a
    /// legacy record with neither says it was a note.
    pub fn label(&self, labels: &str) -> String {
        if self.kind == "done" {
            "Done".to_string()
        } else if !labels.is_empty() {
            labels.to_string()
        } else {
            "Note only".to_string()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Delivery {
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
    /// The send summary not on the share yet (pending, retrying or given up, with its attempts).
    pub summary_warning: Option<String>,
}

impl Delivery {
    /// Whether the asking agent has been told, for the line under a record.
    pub fn told(&self) -> &'static str {
        match self.state.as_str() {
            "rang" => "the agent was told",
            "failed" => "could not tell the agent",
            _ => "telling the agent",
        }
    }

    /// The same words, only while the agent has not been told: a delivered note or objection says nothing extra.
    pub fn not_told(&self) -> Option<&'static str> {
        (self.state != "rang").then(|| self.told())
    }

    /// What a record's row warns about its delivery, in order: its own failed share write or ring (the server's
    /// words, with the attempt), then its send summary not on the share. Answered rows and the notes and
    /// objections on an open card both show this, so a failure is never hidden behind a written summary.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.state == "failed" {
            out.push(if self.detail.trim().is_empty() {
                "Could not tell the agent.".to_string()
            } else {
                self.detail.clone()
            });
        }
        if let Some(w) = self
            .summary_warning
            .as_deref()
            .filter(|w| !w.trim().is_empty())
        {
            out.push(w.to_string());
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Batch {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub card_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Project {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    pub counts: Option<Counts>,
    #[serde(deserialize_with = "nullable")]
    pub streams: Vec<Stream>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Counts {
    pub total: u32,
    pub landed: u32,
    pub done: u32,
    pub ready: u32,
    pub review: u32,
    pub fixing: u32,
    pub working: u32,
    pub blocked: u32,
    pub todo: u32,
    pub dropped: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stream {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub owner: String,
    #[serde(deserialize_with = "nullable")]
    pub goal: String,
    pub board: Option<Board>,
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    pub as_of: Option<String>,
    pub stale: bool,
    pub counts: Option<Counts>,
    pub completion: Option<Completion>,
    #[serde(deserialize_with = "nullable")]
    pub items: Vec<Item>,
    /// Why this stream's task report could not be read (the last good reading is shown).
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Board {
    #[serde(deserialize_with = "nullable")]
    pub path: String,
    pub age_seconds: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Completion {
    /// Missing stays unknown (never read as 0).
    pub done: Option<u32>,
    pub of: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub title: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub owner: Option<String>,
    pub landed: Option<Landed>,
    #[serde(deserialize_with = "nullable")]
    pub reviews: Vec<Review>,
    pub blocked_on: Option<BlockedOn>,
    pub age_seconds: Option<i64>,
    pub branch: Option<String>,
    /// Why an item was dropped or changed.
    pub why: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Landed {
    #[serde(deserialize_with = "nullable")]
    pub sha: String,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Review {
    #[serde(deserialize_with = "nullable")]
    pub by: String,
    #[serde(deserialize_with = "nullable")]
    pub verdict: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockedOn {
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    #[serde(deserialize_with = "nullable")]
    pub what: String,
    pub since: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rows<T> {
    pub source: String,
    /// The panel's secondary sources (pm-mail in Threads, browsers in Machines).
    #[serde(deserialize_with = "nullable")]
    pub also: Vec<String>,
    pub rows: Vec<T>,
}

impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self {
            source: String::new(),
            also: Vec::new(),
            rows: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Thread {
    #[serde(deserialize_with = "nullable")]
    pub name: String,
    pub machine: Option<String>,
    pub project: Option<String>,
    pub stream: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub doing: Option<String>,
    pub blocked_on: Option<BlockedOn>,
    pub progress: Option<Progress>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    pub last_seen: Option<String>,
    pub age_seconds: Option<i64>,
    pub stale: bool,
    /// Seconds this thread's clock runs ahead of AAC's, when it is enough to matter.
    pub clock_ahead_seconds: Option<i64>,
    pub problem: Option<String>,
    /// A pm-mail row's registry state: while it fails, the row is the last good scan. None on report rows.
    pub registry_source: Option<Source>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Progress {
    /// Missing stays unknown (never read as 0).
    pub done: Option<u32>,
    pub total: Option<u32>,
    #[serde(deserialize_with = "nullable")]
    pub unit: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Machine {
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub as_of: Option<String>,
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    #[serde(deserialize_with = "nullable")]
    pub disk: Vec<Disk>,
    pub browsers: Option<Slots>,
    pub recordings: Option<Slots>,
    pub gpu_pct: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub warnings: Vec<String>,
    pub age_seconds: Option<i64>,
    /// Why a machine is stale or unreachable, and which reading is shown instead.
    pub detail: Option<String>,
    /// Browsers running there now.
    #[serde(deserialize_with = "nullable")]
    pub sessions: Vec<Session>,
    /// browser-check's state for browsers, recordings, GPU and sessions, apart from the machine's own `state`.
    pub slots_source: Option<Source>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Session {
    pub url: Option<String>,
    pub owner: Option<String>,
    pub recording: bool,
    pub age_seconds: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cpu {
    pub cores: Option<u32>,
    pub load1: Option<f64>,
    pub busy_pct: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Memory {
    pub available_gb: Option<f64>,
    pub total_gb: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Disk {
    #[serde(deserialize_with = "nullable")]
    pub mount: String,
    pub free_gb: Option<f64>,
    pub total_gb: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Slots {
    pub running: Option<u32>,
    pub max: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Simlock {
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    #[serde(deserialize_with = "nullable")]
    pub capacity: Vec<Capacity>,
    #[serde(deserialize_with = "nullable")]
    pub leases: Vec<Lease>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Capacity {
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    pub running: Option<u32>,
    pub max: Option<u32>,
    pub reserved: Option<u32>,
    pub over_limit: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lease {
    #[serde(deserialize_with = "nullable")]
    pub device: String,
    #[serde(deserialize_with = "nullable")]
    pub platform: String,
    #[serde(deserialize_with = "nullable")]
    pub holder: String,
    pub since: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Usage {
    #[serde(deserialize_with = "nullable")]
    pub source: String,
    #[serde(deserialize_with = "nullable")]
    pub warnings: Vec<UsageWarning>,
    #[serde(deserialize_with = "nullable")]
    pub accounts: Vec<UsageAccount>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageWarning {
    #[serde(deserialize_with = "nullable")]
    pub tone: String,
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageAccount {
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    #[serde(deserialize_with = "nullable")]
    pub window: String,
    pub used_pct: Option<f64>,
    pub resets_at: Option<String>,
    pub projected_full_at: Option<String>,
    pub runs_out_before_reset: Option<bool>,
    pub at_limit: bool,
    pub stale: bool,
    /// The reset passed after this reading: the number is history.
    pub reset_passed: bool,
}

impl UsageAccount {
    /// The row's state words: a stale reading says so, and one from before a passed reset says it is history.
    pub fn note(&self, state: String) -> String {
        let stale = if self.reset_passed {
            "reading from before its reset"
        } else {
            "stale reading"
        };
        match (self.stale || self.reset_passed, state.is_empty()) {
            (true, true) => stale.to_string(),
            (true, false) => format!("{stale} · {state}"),
            (false, _) => state,
        }
    }
}

/// Why a read failed, in words the page can show.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    SignedOut,
    Offline,
    Server(u16),
    Version(u32),
    Malformed,
}

impl FetchError {
    pub fn sentence(&self) -> String {
        match self {
            FetchError::SignedOut => {
                "Signed out. Sign in on the dashboard to see the status.".into()
            }
            FetchError::Offline => "No connection to AI Account Center.".into(),
            FetchError::Server(code) => format!("The server answered {code}."),
            FetchError::Version(v) => format!(
                "This page reads status view {STATUS_VIEW_VERSION}; the server sent {v}. Reload the page."
            ),
            FetchError::Malformed => "The server's answer could not be read.".into(),
        }
    }
}

/// Reads give up after 15 s. A send may take the server longer (it reads each card fresh and writes to the share) and
/// carries its id, so it gets 120 s, and a retry after that is still recorded once.
const READ_DEADLINE_MS: i32 = 15_000;
const SEND_DEADLINE_MS: i32 = 120_000;

/// Aborts a request when its time is up. Dropping it clears the timer and frees the callback, so a request that
/// answered in time leaves nothing behind.
struct Deadline {
    window: web_sys::Window,
    timer: Option<i32>,
    _abort: Option<wasm_bindgen::closure::Closure<dyn FnMut()>>,
}

impl Deadline {
    fn start(window: &web_sys::Window, init: &RequestInit, ms: i32) -> Self {
        let mut deadline = Deadline {
            window: window.clone(),
            timer: None,
            _abort: None,
        };
        let Ok(controller) = web_sys::AbortController::new() else {
            return deadline;
        };
        init.set_signal(Some(&controller.signal()));
        let abort = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || controller.abort());
        deadline.timer = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                abort.as_ref().unchecked_ref(),
                ms,
            )
            .ok();
        deadline._abort = Some(abort);
        deadline
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Some(timer) = self.timer {
            self.window.clear_timeout_with_handle(timer);
        }
    }
}

pub(crate) async fn send(
    method: &str,
    url: &str,
    body: Option<String>,
) -> Result<(u16, String), FetchError> {
    let window = web_sys::window().ok_or(FetchError::Offline)?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_cache(web_sys::RequestCache::NoStore);
    if let Some(body) = body {
        init.set_body(&JsValue::from_str(&body));
    }
    // A request that never answers must not stall every later read: give up and count it as offline.
    let deadline = Deadline::start(
        &window,
        &init,
        if method == "GET" {
            READ_DEADLINE_MS
        } else {
            SEND_DEADLINE_MS
        },
    );
    let request = Request::new_with_str_and_init(url, &init).map_err(|_| FetchError::Malformed)?;
    if method != "GET" {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(|_| FetchError::Malformed)?;
    }
    let result = async {
        let response: Response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|_| FetchError::Offline)?
            .dyn_into()
            .map_err(|_| FetchError::Malformed)?;
        let status = response.status();
        let text = JsFuture::from(response.text().map_err(|_| FetchError::Malformed)?)
            .await
            .map_err(|_| FetchError::Offline)?
            .as_string()
            .unwrap_or_default();
        Ok((status, text))
    }
    .await;
    drop(deadline);
    result
}

pub async fn fetch_status() -> Result<StatusView, FetchError> {
    let (status, text) = send("GET", "/api/status", None).await?;
    if status == 401 {
        return Err(FetchError::SignedOut);
    }
    if status != 200 {
        return Err(FetchError::Server(status));
    }
    let view: StatusView = serde_json::from_str(&text).map_err(|_| FetchError::Malformed)?;
    if view.version != STATUS_VIEW_VERSION {
        return Err(FetchError::Version(view.version));
    }
    Ok(view)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerEntry {
    /// This entry's dedupe key: the draft's nonce with the entry's content. A retry of the same draft carries it again
    /// and the server records it once, even when the rest of the send has changed.
    pub entry_id: String,
    /// "answer", "note", "object" or "done".
    pub kind: String,
    pub card_id: String,
    pub card_seq: u64,
    pub card_sha256: String,
    pub choice: Vec<String>,
    pub note: String,
    /// "deck", or "recommended" when "Use the recommendations" filled it and the owner left it.
    pub via: String,
}

#[derive(Serialize)]
struct SubmitRequest<'a> {
    #[serde(rename = "sendId")]
    send_id: String,
    answers: &'a [AnswerEntry],
}

/// A send's identity, taken from what is sent (each entry's id included): a retry of the same drafts carries the same
/// id. Entries are deduped on their own `entryId` too, which also covers a retry whose set of entries has changed.
pub fn send_id(answers: &[AnswerEntry]) -> String {
    hex_id(&serde_json::to_string(answers).unwrap_or_default())
}

/// An entry's id: its draft's random nonce plus what it says (its own id field left out). A retry of the same draft
/// is the same entry; a new draft with the same words is a new one.
pub fn entry_id(nonce: &str, entry: &AnswerEntry) -> String {
    // Words stand whatever version of the card they were written on (a note, an objection, which doesn't close the
    // card): an agent's reply between a lost response and the retry must not make them new. A closing choice (answer,
    // done) is bound to the version it was made on, so an unchanged tap on a re-asked question is a new answer.
    let note = entry.kind == "note" || entry.kind == "object";
    let content = AnswerEntry {
        entry_id: String::new(),
        card_seq: if note { 0 } else { entry.card_seq },
        card_sha256: if note {
            String::new()
        } else {
            entry.card_sha256.clone()
        },
        ..entry.clone()
    };
    hex_id(&format!(
        "{nonce}\n{}",
        serde_json::to_string(&content).unwrap_or_default()
    ))
}

/// Two 64-bit FNV-1a hashes with different starting points: 32 hex characters.
fn hex_id(text: &str) -> String {
    let fnv = |basis: u64| {
        text.bytes().fold(basis, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    format!(
        "{:016x}{:016x}",
        fnv(0xcbf2_9ce4_8422_2325),
        fnv(0x6c62_272e_07bb_0142)
    )
}

/// One entry's result as the server reports it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EntryOutcome {
    #[serde(deserialize_with = "nullable")]
    pub card_id: String,
    /// "recorded", "card_changed" or "refused".
    #[serde(deserialize_with = "nullable")]
    pub outcome: String,
    #[serde(deserialize_with = "nullable")]
    pub code: String,
    #[serde(deserialize_with = "nullable")]
    pub error: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SubmitResponse {
    results: Vec<EntryOutcome>,
}

/// How a send went, in the page's words.
#[derive(Clone, Debug, PartialEq)]
pub enum SendOutcome {
    /// Each entry's own result (some may be refused while others are recorded).
    Results(Vec<EntryOutcome>),
    /// The whole request failed: refused (nothing recorded) or unknown (the words say which).
    Failed(String),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ErrorBody {
    error: String,
}

fn send_outcome(status: u16, text: &str) -> SendOutcome {
    if (200..300).contains(&status) {
        return match serde_json::from_str::<SubmitResponse>(text) {
            Ok(r) => SendOutcome::Results(r.results),
            Err(_) => SendOutcome::Failed(
                "The server's answer could not be read. Reload to see what was recorded.".into(),
            ),
        };
    }
    if status == 401 {
        return SendOutcome::Failed(
            "Signed out. Sign in again; your choices are kept on this device.".into(),
        );
    }
    // A server error (or a proxy's 502/504) can come after some answers were recorded: never say nothing was.
    if status >= 500 {
        return SendOutcome::Failed(format!("The server answered {status}. {MAYBE_RECORDED}"));
    }
    let body: ErrorBody = serde_json::from_str(text).unwrap_or_default();
    if body.error.is_empty() {
        SendOutcome::Failed(format!(
            "The server answered {status}. Nothing was recorded; your choices are kept."
        ))
    } else {
        SendOutcome::Failed(format!(
            "{} Nothing was recorded; your choices are kept.",
            body.error
        ))
    }
}

/// When a send may have landed. A retry of the same answers carries the same send id, so it is recorded once.
const MAYBE_RECORDED: &str = "Your answers may have been recorded. Sending the same answers again won't record them \
                              twice; reload to see what was recorded. Your choices are kept.";

pub async fn post_answers(answers: &[AnswerEntry]) -> SendOutcome {
    let body = serde_json::to_string(&SubmitRequest {
        send_id: send_id(answers),
        answers,
    })
    .unwrap_or_default();
    match send("POST", "/api/status/answers", Some(body)).await {
        Ok((status, text)) => send_outcome(status, &text),
        // The answers may have reached the server before the connection dropped: say so, never "nothing was sent".
        Err(FetchError::Offline) => SendOutcome::Failed(format!(
            "The connection dropped or timed out while sending. {MAYBE_RECORDED}"
        )),
        Err(error) => SendOutcome::Failed(format!(
            "{} Nothing was recorded; your choices are kept.",
            error.sentence()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_answer_history_is_explicit_and_recovers() {
        let cards: Cards = serde_json::from_value(serde_json::json!({
            "answered": null,
            "answerHistoryProblem": "The answer history can't be read right now.",
            "complete": false
        }))
        .unwrap();
        assert_eq!(
            cards.answer_history_problem.as_deref(),
            Some("The answer history can't be read right now.")
        );
        assert!(!cards.proves_closed());
        let recovered: Cards = serde_json::from_value(
            serde_json::json!({"answered": [], "answerHistoryProblem": null}),
        )
        .unwrap();
        assert!(recovered.answer_history_problem.is_none());
    }

    fn sent_note(value: serde_json::Value) -> SentNote {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_note_whose_share_write_failed_says_so_though_its_summary_is_written() {
        let n = sent_note(serde_json::json!({
            "kind": "note", "note": "A note.", "at": "2026-11-07T10:00:00.000Z",
            "delivery": {
                "state": "failed", "at": null,
                "detail": "The signed copy could not be written to the share after 20 attempts; no more retries.",
                "share": {"state": "failed", "at": null, "detail": "EACCES", "attempts": 20},
                "doorbell": {"state": "pending", "at": null, "detail": null, "attempts": 0},
                "summary": {"state": "done", "at": null, "detail": null, "attempts": 1},
                "summaryWarning": null
            }
        }));
        assert_eq!(
            n.delivery.warnings(),
            vec![
                "The signed copy could not be written to the share after 20 attempts; no more retries."
                    .to_string()
            ]
        );
        assert_eq!(n.delivery.not_told(), Some("could not tell the agent"));
    }

    #[test]
    fn an_objection_whose_ring_failed_says_so_with_its_attempt() {
        let n = sent_note(serde_json::json!({
            "kind": "object", "note": "Keep the old layout.", "at": null,
            "delivery": {
                "state": "failed", "at": null,
                "detail": "The asker could not be rung yet (attempt 3 of 20); retrying. The signed answer is on the share.",
                "summary": {"state": "done", "at": null, "detail": null, "attempts": 1},
                "summaryWarning": null
            }
        }));
        assert_eq!(n.kind, "object");
        let warnings = n.delivery.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("(attempt 3 of 20)"));
        assert_eq!(n.delivery.not_told(), Some("could not tell the agent"));
    }

    #[test]
    fn a_delivered_note_with_a_failed_summary_shows_only_the_summary() {
        let n = sent_note(serde_json::json!({
            "kind": "note", "note": "A note.",
            "delivery": {
                "state": "rang", "detail": "Rang lead.",
                "summaryWarning": "The signed send summary could not be written to the share yet (attempt 2 of 20); retrying."
            }
        }));
        assert_eq!(
            n.delivery.warnings(),
            vec![
                "The signed send summary could not be written to the share yet (attempt 2 of 20); retrying."
                    .to_string()
            ]
        );
        assert_eq!(n.delivery.not_told(), None);
    }

    #[test]
    fn a_failed_note_and_a_failed_summary_are_both_shown_record_first() {
        let n = sent_note(serde_json::json!({
            "kind": "note", "note": "x",
            "delivery": {"state": "failed", "detail": null, "summaryWarning": "Summary not on the share yet."}
        }));
        assert_eq!(
            n.delivery.warnings(),
            vec![
                "Could not tell the agent.".to_string(),
                "Summary not on the share yet.".to_string()
            ]
        );
    }

    #[test]
    fn a_clean_note_shows_nothing_about_delivery() {
        let n = sent_note(serde_json::json!({
            "kind": "note", "note": "A note.",
            "delivery": {"state": "rang", "detail": "Rang lead.", "summaryWarning": null}
        }));
        assert!(n.delivery.warnings().is_empty());
        assert_eq!(n.delivery.not_told(), None);
        assert_eq!(n.delivery.told(), "the agent was told");
    }

    #[test]
    fn a_pending_note_is_told_without_a_warning() {
        let n = sent_note(serde_json::json!({
            "kind": "note", "note": "A note.",
            "delivery": {"state": "pending", "detail": "Delivering.", "summaryWarning": null}
        }));
        assert!(n.delivery.warnings().is_empty());
        assert_eq!(n.delivery.not_told(), Some("telling the agent"));
    }

    #[test]
    fn a_send_summary_warning_reads_beside_the_ring() {
        let delivery: Delivery = serde_json::from_value(serde_json::json!({
            "state": "rang", "at": null, "detail": "Rang.",
            "summary": {"state": "failed", "at": null, "detail": "x", "attempts": 20},
            "summaryWarning": "The signed send summary could not be written to the share after 20 attempts; no more retries."
        }))
        .unwrap();
        assert_eq!(delivery.state, "rang");
        assert!(
            delivery
                .summary_warning
                .as_deref()
                .is_some_and(|w| w.contains("after 20 attempts"))
        );
        let done: Delivery =
            serde_json::from_value(serde_json::json!({"state": "rang", "summaryWarning": null}))
                .unwrap();
        assert!(done.summary_warning.is_none());
    }

    #[test]
    fn the_fixture_reads_as_view_v1() {
        let text = include_str!("../fixtures/status-view.json");
        let view: StatusView = serde_json::from_str(text).expect("fixture parses");
        assert_eq!(view.version, STATUS_VIEW_VERSION);
        assert_eq!(view.cards.open.len(), 6);
        assert_eq!(view.cards.batches[0].card_ids.len(), 3);
        // Each card kind and the optional parts are read, not dropped.
        let kinds: Vec<&str> = view.cards.open.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(&"objection") && kinds.contains(&"action"));
        let scout = view.cards.open.iter().find(|c| c.label == "S-Q1").unwrap();
        assert_eq!(scout.sections[0].kind, "compare");
        assert_eq!(scout.sections[0].items.len(), 3);
        let chart = &view.cards.open[0];
        assert_eq!(chart.options[1].media[0].kind, "video");
        assert!(chart.options[1].media[0].poster_url.is_some());
        // A missing reading stays missing.
        let windows = view
            .machines
            .rows
            .iter()
            .find(|m| m.id == "windows")
            .unwrap();
        assert_eq!(windows.cpu.as_ref().unwrap().load1, None);
        let c = view.projects[0]
            .streams
            .iter()
            .find(|s| s.id == "C")
            .unwrap();
        assert!(c.completion.is_none());
    }

    #[test]
    fn a_resend_of_the_same_entries_keeps_its_id() {
        let entry = |note: &str| AnswerEntry {
            entry_id: "0".repeat(32),
            kind: "note".into(),
            card_id: "vm.a--c1".into(),
            card_seq: 3,
            card_sha256: "ab".repeat(32),
            choice: vec![],
            note: note.into(),
            via: "deck".into(),
        };
        let first = send_id(&[entry("go")]);
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(first, send_id(&[entry("go")]), "a retry is the same send");
        assert_ne!(first, send_id(&[entry("go now")]), "an edit is a new send");
        let body = serde_json::to_string(&SubmitRequest {
            send_id: first.clone(),
            answers: &[entry("go")],
        })
        .unwrap();
        assert!(body.starts_with(&format!(r#"{{"sendId":"{first}","answers":["#)));
        assert!(body.contains(r#""entryId":"#), "{body}");
        // an entry's id: the same draft and words keep it, another draft (nonce) or other words change it
        let id = entry_id("n1", &entry("go"));
        assert_eq!(id.len(), 32);
        assert_eq!(id, entry_id("n1", &entry("go")));
        assert_ne!(id, entry_id("n2", &entry("go")));
        assert_ne!(id, entry_id("n1", &entry("go now")));
        // a note keeps its id across card versions; an answer doesn't
        let mut later = entry("go");
        later.card_seq = 4;
        later.card_sha256 = "cd".repeat(32);
        assert_eq!(
            entry_id("n1", &later),
            id,
            "the same note on a re-asked card"
        );
        let answer = |seq: u64| AnswerEntry {
            kind: "answer".into(),
            choice: vec!["A".into()],
            card_seq: seq,
            ..entry("")
        };
        assert_ne!(entry_id("n1", &answer(3)), entry_id("n1", &answer(4)));
        let object = |seq: u64| AnswerEntry {
            kind: "object".into(),
            card_seq: seq,
            ..entry("not this way")
        };
        assert_eq!(
            entry_id("n1", &object(3)),
            entry_id("n1", &object(4)),
            "an objection's words stand across versions too"
        );
        let mut carrying = entry("go");
        carrying.entry_id = id.clone();
        assert_eq!(
            entry_id("n1", &carrying),
            id,
            "its own id field is left out"
        );
    }

    #[test]
    fn only_a_complete_card_list_proves_cards_closed() {
        let read = |text: &str| serde_json::from_str::<Cards>(text).unwrap().proves_closed();
        assert!(read(
            r#"{"source":"agent-reports","open":[],"complete":true}"#
        ));
        assert!(!read(
            r#"{"source":"agent-reports","open":[],"complete":false}"#
        ));
        assert!(
            !read(r#"{"source":"agent-reports","open":[]}"#),
            "an older server proves nothing"
        );
        assert!(!read(
            r#"{"source":"agent-reports","open":[],"complete":null}"#
        ));
    }

    #[test]
    fn missing_progress_counts_stay_unknown() {
        let c: Completion = serde_json::from_str(r#"{"done":3}"#).unwrap();
        assert_eq!((c.done, c.of), (Some(3), None));
        let p: Progress = serde_json::from_str(r#"{"unit":"files"}"#).unwrap();
        assert_eq!((p.done, p.total), (None, None));
    }

    /// A view the server branch built from scripts/status-fixture.mjs (--with-kinds --with-problems): the page must read
    /// what the server actually sends, nulls included.
    #[test]
    fn the_server_view_reads() {
        let text = include_str!("../fixtures/server-view.json");
        let view: StatusView = serde_json::from_str(text).expect("server view parses");
        assert_eq!(view.version, STATUS_VIEW_VERSION);
        assert_eq!(view.cards.open.len(), 9);
        assert!(
            view.cards.proves_closed(),
            "the server's whole scan says so"
        );
        let kinds: Vec<&str> = view.cards.open.iter().map(|c| c.kind.as_str()).collect();
        for kind in ["objection", "action", "judgement", "sample"] {
            assert!(kinds.contains(&kind), "{kind}");
        }
        assert!(view.cards.open.iter().all(|c| !c.writer.is_empty()));
        // A sample card is shown but never answered; every other card in this view can be.
        for c in &view.cards.open {
            assert_eq!(c.can_answer(), c.kind != "sample", "{}", c.label);
        }
        // The threads' additions are read, not dropped.
        let card = |label: &str| view.cards.open.iter().find(|c| c.label == label).unwrap();
        let split = card("F-Q4");
        assert_eq!(split.verdict.as_deref(), Some("adapt"));
        assert_eq!(split.group.as_deref(), Some("toasts"));
        assert_eq!(split.agreement.as_ref().and_then(|a| a.same), Some(false));
        assert_eq!(split.related.len(), 1);
        let rule = card("F-Q5");
        assert_eq!(rule.applies_to.as_ref().and_then(|a| a.count), Some(4));
        assert_eq!(rule.changes_canon[0].reference, "PM7 plan > Flags");
        assert!(rule.doubt.is_some());
        let words = card("ST-Q2");
        let types: Vec<&str> = words.sections.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(types, ["quote", "planChange"]);
        assert!(words.sections[1].proposed.is_some());
        assert_eq!(words.notes[0].kind, "note");
        // An option the agent left without gets/costs reads as empty, not as a failed page.
        assert!(
            view.cards
                .open
                .iter()
                .flat_map(|c| &c.options)
                .any(|o| o.gets.is_empty())
        );
        // Image and video links are the server's file URLs.
        let media: Vec<&Media> = view
            .cards
            .open
            .iter()
            .flat_map(|c| &c.options)
            .flat_map(|o| &o.media)
            .collect();
        assert!(
            !media.is_empty()
                && media
                    .iter()
                    .all(|m| m.url.starts_with("/api/status/files/"))
        );
        let answered = &view.cards.answered[0].answer;
        assert!(!answered.device.is_empty() && !answered.delivery.state.is_empty());
        // An unreachable machine says why, and a machine with no reading stays missing.
        let nas1 = view.machines.rows.iter().find(|m| m.id == "nas1").unwrap();
        assert!(
            nas1.detail
                .as_deref()
                .is_some_and(|d| d.contains("refused"))
        );
        assert!(nas1.memory.is_none() && nas1.disk.is_empty());
        let vm = view.machines.rows.iter().find(|m| m.id == "vm").unwrap();
        assert_eq!(vm.sessions.len(), 1);
        // A usage window with no projection is unknown, not "won't run out".
        assert!(
            view.usage
                .accounts
                .iter()
                .any(|a| a.runs_out_before_reset.is_none())
        );
        // Damaged report files are reported by the source, not dropped silently.
        let reports = view
            .sources
            .iter()
            .find(|s| s.id == "agent-reports")
            .unwrap();
        assert_eq!(reports.state, "unreadable");
        // quota-watch's estimates: three periods, budgets and unattributed rows, every value worded by the server.
        let spend = view.spend.as_ref().expect("spend section");
        assert_eq!(spend.source, "quota-watch");
        let ids: Vec<&str> = spend.periods.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["day", "week", "window"]);
        assert!(spend.budgets.iter().any(|b| b.over_limit));
        assert!(!spend.unattributed.is_empty());
        assert!(
            spend
                .periods
                .iter()
                .flat_map(|p| &p.streams)
                .all(|s| s.rows.iter().all(|r| !r.value_text.is_empty()))
        );
    }

    #[test]
    fn send_outcomes_keep_each_entry_and_say_when_nothing_was_recorded() {
        let ok = send_outcome(
            200,
            r#"{"results":[{"cardId":"a","outcome":"recorded"},{"cardId":"b","outcome":"card_changed","code":"card_changed"}]}"#,
        );
        match ok {
            SendOutcome::Results(r) => {
                assert_eq!(r.len(), 2);
                assert_eq!(r[1].outcome, "card_changed");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            send_outcome(200, "not json"),
            SendOutcome::Failed(_)
        ));
        assert_eq!(
            send_outcome(401, ""),
            SendOutcome::Failed(
                "Signed out. Sign in again; your choices are kept on this device.".into()
            )
        );
        assert_eq!(
            send_outcome(400, r#"{"error":"Too many answers."}"#),
            SendOutcome::Failed(
                "Too many answers. Nothing was recorded; your choices are kept.".into()
            )
        );
        // a 5xx may follow recorded answers: never "nothing was recorded"
        for status in [500, 502, 504] {
            match send_outcome(status, r#"{"error":"Boom."}"#) {
                SendOutcome::Failed(words) => {
                    assert!(words.contains("may have been recorded"), "{words}");
                    assert!(!words.contains("Nothing was"), "{words}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// The server's Machines and Threads rows after browser-check and the pm-mail registry failed at 10:01 while the
    /// machine reads and agent reports stayed healthy (the Sol max reproduction on 2e7fa26d).
    fn failed(id: &str, label: &str, detail: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "label": label, "state": "error", "asOf": "2026-11-07T15:00:00.000Z",
            "ageSeconds": 60, "detail": detail
        })
    }

    #[test]
    fn a_failed_browser_check_labels_its_readings_cached_on_a_fresh_machine() {
        let detail = "browser-check failed (since 10:01 AM). Showing the reading from 10:00 AM.";
        let machines: Rows<Machine> = serde_json::from_value(serde_json::json!({
            "source": "machines", "also": ["browsers"],
            "rows": [{
                "id": "vm", "label": "Ubuntu VM", "state": "ok", "ageSeconds": 0, "detail": null,
                "browsers": {"running": 6, "max": 12}, "recordings": {"running": 1, "max": 5}, "gpuPct": 39,
                "slotsSource": failed("browsers", "Browsers and recordings", detail)
            }]
        }))
        .unwrap();
        assert_eq!(machines.also, ["browsers"]);
        let vm = &machines.rows[0];
        assert_eq!(vm.state, "ok");
        assert_eq!(vm.browsers.as_ref().and_then(|b| b.running), Some(6));
        let slots = vm.slots_source.as_ref().unwrap();
        assert_eq!(
            slots.reading_words(0).as_deref(),
            Some("cached, read 1m ago")
        );
        // The age keeps running between refreshes.
        assert_eq!(
            slots.reading_words(300).as_deref(),
            Some("cached, read 6m ago")
        );
        assert_eq!(
            slots.problem().as_deref(),
            Some(format!("Browsers and recordings: {detail}").as_str())
        );
        // A healthy browser-check adds nothing; a stale one says stale.
        let ok: Source = serde_json::from_value(
            serde_json::json!({"id": "browsers", "state": "ok", "ageSeconds": 5, "asOf": "x"}),
        )
        .unwrap();
        assert_eq!((ok.reading_words(0), ok.problem()), (None, None));
        let stale: Source = serde_json::from_value(serde_json::json!({"id": "browsers", "label": "Browsers and recordings", "state": "stale", "ageSeconds": 400, "asOf": "x", "detail": "No new reading since 10:00 AM."})).unwrap();
        assert_eq!(
            stale.reading_words(0).as_deref(),
            Some("stale, read 7m ago")
        );
        let never: Source = serde_json::from_value(serde_json::json!({"id": "browsers", "state": "missing", "asOf": null, "ageSeconds": null, "detail": ""})).unwrap();
        assert_eq!(never.reading_words(0).as_deref(), Some("not read yet"));
        assert_eq!(never.problem().as_deref(), Some("browsers: missing"));
        // A view from before this field reads as unknown, not as healthy-looking data.
        let old: Machine = serde_json::from_value(serde_json::json!({"id": "vm"})).unwrap();
        assert!(old.slots_source.is_none());
    }

    #[test]
    fn a_failed_pm_mail_registry_labels_its_rows_while_reports_stay_healthy() {
        let detail = "The pm-mail registry could not be listed (since 10:01 AM). Showing the reading from 10:00 AM.";
        let threads: Rows<Thread> = serde_json::from_value(serde_json::json!({
            "source": "agent-reports", "also": ["pm-mail"],
            "rows": [
                {"name": "vm.report", "source": "report", "stale": false, "registrySource": null},
                {"name": "registry-worker", "source": "pm-mail", "state": null, "stale": false, "problem": null,
                 "ageSeconds": 60, "registrySource": failed("pm-mail", "pm-mail registry", detail)}
            ]
        }))
        .unwrap();
        assert_eq!(threads.also, ["pm-mail"]);
        assert!(threads.rows[0].registry_source.is_none());
        let reg = threads.rows[1].registry_source.as_ref().unwrap();
        assert_eq!(reg.reading_words(0).as_deref(), Some("cached, read 1m ago"));
        assert_eq!(
            reg.problem().as_deref(),
            Some(format!("pm-mail registry: {detail}").as_str())
        );
    }

    #[test]
    fn a_usage_reading_from_before_its_reset_says_it_is_history() {
        let row = |v: serde_json::Value| -> UsageAccount { serde_json::from_value(v).unwrap() };
        let passed = row(serde_json::json!({
            "label": "Review fixture", "window": "5-hour", "usedPct": 100, "resetsAt": "2026-11-07T15:00:30.000Z",
            "projectedFullAt": null, "runsOutBeforeReset": null, "atLimit": false, "stale": true, "resetPassed": true
        }));
        assert_eq!(
            passed.used_pct,
            Some(100.0),
            "the measured number stays, as history"
        );
        assert!(!passed.at_limit);
        assert_eq!(passed.note(String::new()), "reading from before its reset");
        let stale = row(serde_json::json!({"usedPct": 40, "stale": true}));
        assert_eq!(stale.note(String::new()), "stale reading");
        assert_eq!(
            stale.note("full 4:00 PM".into()),
            "stale reading · full 4:00 PM"
        );
        let current = row(
            serde_json::json!({"usedPct": 100, "atLimit": true, "stale": false, "resetPassed": false}),
        );
        assert_eq!(current.note("at its limit".into()), "at its limit");
    }

    #[test]
    fn a_completed_action_reads_done_with_or_without_a_note() {
        let answer = |v: serde_json::Value| -> Answer { serde_json::from_value(v).unwrap() };
        let bare = answer(serde_json::json!({"kind": "done", "choice": [], "note": ""}));
        assert_eq!(bare.label(""), "Done");
        let noted = answer(
            serde_json::json!({"kind": "done", "choice": [], "note": "Moved the NAS share."}),
        );
        assert_eq!(noted.label(""), "Done");
        let chosen = answer(serde_json::json!({"kind": "answer", "choice": ["b"], "note": ""}));
        assert_eq!(chosen.label("b · Keep it"), "b · Keep it");
        // A legacy record with no kind and no choice was a note.
        let legacy = answer(serde_json::json!({"choice": null, "note": "Just a thought."}));
        assert_eq!(legacy.label(""), "Note only");
    }
}
