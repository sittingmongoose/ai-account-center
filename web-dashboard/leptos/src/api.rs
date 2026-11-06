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
    #[serde(deserialize_with = "nullable")]
    pub batches: Vec<Batch>,
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

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Delivery {
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    pub at: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub detail: String,
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
    pub done: u32,
    pub of: u32,
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
    pub rows: Vec<T>,
}

impl<T> Default for Rows<T> {
    fn default() -> Self {
        Self {
            source: String::new(),
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
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
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

pub(crate) async fn send(
    method: &str,
    url: &str,
    body: Option<String>,
) -> Result<(u16, String), FetchError> {
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(RequestCredentials::SameOrigin);
    init.set_cache(web_sys::RequestCache::NoStore);
    if let Some(body) = body {
        init.set_body(&JsValue::from_str(&body));
    }
    let request = Request::new_with_str_and_init(url, &init).map_err(|_| FetchError::Malformed)?;
    if method != "GET" {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(|_| FetchError::Malformed)?;
    }
    let window = web_sys::window().ok_or(FetchError::Offline)?;
    let response: Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| FetchError::Offline)?
        .dyn_into()
        .map_err(|_| FetchError::Malformed)?;
    let status = response.status();
    let text = JsFuture::from(response.text().map_err(|_| FetchError::Malformed)?)
        .await
        .map_err(|_| FetchError::Malformed)?
        .as_string()
        .unwrap_or_default();
    Ok((status, text))
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
    answers: &'a [AnswerEntry],
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
    /// The whole request failed; nothing was recorded.
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
    let body: ErrorBody = serde_json::from_str(text).unwrap_or_default();
    if body.error.is_empty() {
        SendOutcome::Failed(format!(
            "The server answered {status}. Nothing was sent; your choices are kept."
        ))
    } else {
        SendOutcome::Failed(format!(
            "{} Nothing was sent; your choices are kept.",
            body.error
        ))
    }
}

pub async fn post_answers(answers: &[AnswerEntry]) -> SendOutcome {
    let body = serde_json::to_string(&SubmitRequest { answers }).unwrap_or_default();
    match send("POST", "/api/status/answers", Some(body)).await {
        Ok((status, text)) => send_outcome(status, &text),
        Err(error) => SendOutcome::Failed(format!(
            "{} Nothing was sent; your choices are kept.",
            error.sentence()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A view the server branch built from scripts/status-fixture.mjs (--with-kinds --with-problems): the page must read
    /// what the server actually sends, nulls included.
    #[test]
    fn the_server_view_reads() {
        let text = include_str!("../fixtures/server-view.json");
        let view: StatusView = serde_json::from_str(text).expect("server view parses");
        assert_eq!(view.version, STATUS_VIEW_VERSION);
        assert_eq!(view.cards.open.len(), 9);
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
    }

    #[test]
    fn send_outcomes_keep_each_entry_and_say_when_nothing_was_sent() {
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
                "Too many answers. Nothing was sent; your choices are kept.".into()
            )
        );
    }
}
