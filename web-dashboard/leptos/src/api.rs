//! The Status view (docs/status-tab.md, view v1) and the calls that read and answer it.
//!
//! The server decides every number, word and state. These types only carry them to the page; a missing value stays
//! `None` and is drawn as missing, never as zero.

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

pub const STATUS_VIEW_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusView {
    pub version: u32,
    pub generated_at: String,
    pub refresh: Refresh,
    pub headline: Headline,
    pub sources: Vec<Source>,
    pub cards: Cards,
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
    pub tone: String,
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
    pub text: String,
    pub strong: bool,
    pub tone: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Source {
    pub id: String,
    pub label: String,
    pub state: String,
    pub as_of: Option<String>,
    pub age_seconds: Option<i64>,
    pub stale_after_seconds: Option<i64>,
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
    pub source: String,
    pub open: Vec<Card>,
    pub answered: Vec<AnsweredCard>,
    pub batches: Vec<Batch>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Card {
    pub id: String,
    /// The short id the owner can quote anywhere ("F-Q3").
    pub label: String,
    pub seq: u64,
    pub card_sha256: String,
    pub project: String,
    pub stream: Option<String>,
    pub asked_by: String,
    pub asker_state: Option<String>,
    pub title: String,
    pub now: String,
    pub now_as_of: Option<String>,
    pub old_answer: Option<OldAnswer>,
    pub what_changed: Option<String>,
    pub options: Vec<CardOption>,
    pub recommendation: Option<Recommendation>,
    pub multiple: bool,
    pub urgency: String,
    pub blocks: Option<String>,
    pub batch: Option<String>,
    pub evidence: Vec<Link>,
    pub replies: Vec<Reply>,
    pub notes: Vec<SentNote>,
    pub asked_at: String,
    pub age_seconds: Option<i64>,
}

impl Card {
    /// A card's identity for drafts and keyed lists: a new version of the question is a new card.
    pub fn version_key(&self) -> String {
        format!("{}#{}#{}", self.id, self.seq, self.card_sha256)
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
    pub text: String,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CardOption {
    pub key: String,
    pub label: String,
    pub gets: String,
    pub costs: String,
    pub image: Option<OptionImage>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OptionImage {
    pub url: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Recommendation {
    pub key: String,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Link {
    pub label: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Reply {
    pub at: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SentNote {
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
    pub kind: String,
    pub choice: Option<Vec<String>>,
    pub note: Option<String>,
    pub via: String,
    pub answered_at: String,
    pub delivery: Delivery,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Delivery {
    pub state: String,
    pub at: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Batch {
    pub id: String,
    pub label: String,
    pub card_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Project {
    pub id: String,
    pub label: String,
    pub counts: Option<Counts>,
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
    pub id: String,
    pub label: String,
    pub owner: String,
    pub goal: String,
    pub board: Option<Board>,
    pub source: String,
    pub as_of: Option<String>,
    pub stale: bool,
    pub counts: Option<Counts>,
    pub completion: Option<Completion>,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Board {
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
    pub id: String,
    pub title: String,
    pub state: String,
    pub owner: Option<String>,
    pub landed: Option<Landed>,
    pub reviews: Vec<Review>,
    pub blocked_on: Option<BlockedOn>,
    pub age_seconds: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Landed {
    pub sha: String,
    pub at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Review {
    pub by: String,
    pub verdict: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockedOn {
    pub kind: String,
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
    pub name: String,
    pub machine: Option<String>,
    pub project: Option<String>,
    pub stream: Option<String>,
    pub state: String,
    pub doing: Option<String>,
    pub blocked_on: Option<BlockedOn>,
    pub progress: Option<Progress>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    pub source: String,
    pub last_seen: Option<String>,
    pub age_seconds: Option<i64>,
    pub stale: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
    pub unit: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub state: String,
    pub as_of: Option<String>,
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    pub disk: Vec<Disk>,
    pub browsers: Option<Slots>,
    pub recordings: Option<Slots>,
    pub gpu_pct: Option<f64>,
    pub warnings: Vec<String>,
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
    pub source: String,
    pub capacity: Vec<Capacity>,
    pub leases: Vec<Lease>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Capacity {
    pub platform: String,
    pub running: Option<u32>,
    pub max: Option<u32>,
    pub reserved: Option<u32>,
    pub over_limit: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lease {
    pub device: String,
    pub platform: String,
    pub holder: String,
    pub since: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Usage {
    pub source: String,
    pub warnings: Vec<UsageWarning>,
    pub accounts: Vec<UsageAccount>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageWarning {
    pub tone: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageAccount {
    pub provider: String,
    pub label: String,
    pub window: String,
    pub used_pct: Option<f64>,
    pub resets_at: Option<String>,
    pub projected_full_at: Option<String>,
    pub runs_out_before_reset: bool,
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

async fn send(method: &str, url: &str, body: Option<String>) -> Result<(u16, String), FetchError> {
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
    pub card_id: String,
    /// "recorded", "card_changed" or "refused".
    pub outcome: String,
    pub code: String,
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
        assert_eq!(view.cards.open.len(), 4);
        assert_eq!(view.cards.batches[0].card_ids.len(), 3);
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
