//! G16b: the owner's answer store in Rust, ported whole from `src/web-server/status/answers.ts`
//! (`StatusAnswers`): the append-only journal `<store>/answers.jsonl` (records, sends, delivery
//! steps, `void` lines), its fold (`load`), the writer lock and ownership generations
//! (`writer`, [`crate::writer_lock`]), pending completions and unlanded void lines
//! (`flushCompletions`), `recordStep`, `submitSet` (replay by client send id, draft ids, the
//! per-entry checks against the card on disk) and `retryPending`. Share writes, summaries and
//! rings are G16's [`crate::answer_delivery`]; signing is [`crate::answer_signing`].
//!
//! The journal is a multi-process protocol: whichever process holds `answers.lock` (a TypeScript
//! dashboard or this engine) is the only one that appends, and every other reads it again when
//! it changes. This engine writes byte-identical lines, so either side reads the other's journal.
//!
//! What stays with the caller, as the TypeScript route keeps it: the request admission (body
//! shape, entry ids), and the note's secret check, which arrives as `note_secrets` (exactly one
//! secret filter, the TypeScript one, until the cut moves it; see README-gap-G16b).
//!
//! The TypeScript class is asynchronous and keeps object identity across reloads; this engine is
//! synchronous (one caller at a time, under the host's lock) and keeps the same state by record
//! id: a reload keeps this process's own delivery steps (the further-along reading of each) and
//! its own records the reload does not show yet, exactly as `writer()` reconciles them.
use crate::answer_delivery::{
    self as delivery, DOORBELL_OFF_DETAIL, Guard, MAX_DELIVERY_ATTEMPTS, NO_MAIL_DETAIL, RingResult,
    SHARE_UNAVAILABLE_DETAIL, SUPERSEDED_DETAIL, SentRecord, ShareOutcome,
};
use crate::answer_signing::{
    AnswerKey, canonical_json, load_or_create_answer_key, sign_answer, sign_document, verify_signed_answer,
};
use crate::writer_lock::{LockError, hold_writer_lock};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The signed answer schema.
pub const ANSWER_SCHEMA: &str = "agent-reports/answer@1";
/// The journal's file name in the store folder.
pub const JOURNAL_FILE: &str = "answers.jsonl";

const MAX_REMEMBERED_SENDS: usize = 200;
const REMEMBER_SEND_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
const MIN_REMEMBERED_ENTRIES: usize = 1000;
const REMEMBER_ENTRY_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
const ANSWER_NOTE_MAX: usize = 4000;
const CLOSING_KINDS: [&str; 3] = ["answer", "note", "done"];
const RECORD_KINDS: [&str; 4] = ["answer", "note", "object", "done"];

const SHARE_DONE: &str = "Signed copy written to the share.";
const SHARE_FAILED: &str = "The signed copy could not be written to the share.";
const SUMMARY_DONE: &str = "Signed send summary written to the share.";
const SUMMARY_FAILED: &str = "The signed send summary could not be written to the share.";

/// How long each step may take (`DEFAULT_TIMING`).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// One signed file (with the public keys) reaching the share.
    pub share_ms: u64,
    /// One fresh read of a card.
    pub read_ms: u64,
    /// One whole send.
    pub send_budget_ms: u64,
    /// One retry pass.
    pub retry_budget_ms: u64,
    /// One ring.
    pub ring_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            share_ms: 10_000,
            read_ms: 5_000,
            send_budget_ms: 30_000,
            retry_budget_ms: 45_000,
            ring_ms: 40_000,
        }
    }
}

/// A fresh read of a card (`readCard`).
pub enum CardRead {
    /// The card as parsed (the projected doc) and the sha256 of its file bytes.
    Card {
        /// The parsed card document.
        doc: Json,
        /// The file's sha256, lowercase hex.
        sha256: String,
    },
    /// The share says there is no such card (or it does not pass the reader's rules).
    Absent,
    /// The share could not say.
    Failed,
    /// The read outlived its limit (the send's budget is spent).
    TimedOut,
}

/// Faults a test host injects into journal writes (the TypeScript tests patch `fs` the same way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault<'a> {
    /// The single write of a line of this `type` (and delivery `state`, when one) is refused.
    Write(&'a str, Option<&'a str>),
    /// The write lands, then its fsync fails.
    Fsync(&'a str, Option<&'a str>),
    /// A `void` line's write is refused.
    Void,
}

/// What the engine needs from its host: the card reader, the doorbell, the clock and ids.
pub trait AnswerHost: Send {
    /// `readCard`, within `limit`.
    fn read_card(&mut self, card_id: &str, limit: Duration) -> CardRead;
    /// Whether a doorbell was supplied (then `AAC_STATUS_DOORBELL=off` does not apply).
    fn has_doorbell(&self) -> bool {
        false
    }
    /// One ring (`pmMailDoorbell` by default).
    fn ring(&mut self, to: &str, subject: &str, body: &str, timeout: Duration) -> RingResult {
        let bin = delivery::find_pm_mail(std::env::var_os("PATH").as_deref());
        delivery::pm_mail_ring(&bin, to, subject, body, timeout.min(Duration::from_secs(30)))
    }
    /// Milliseconds since the epoch (`deps.now`).
    fn now(&mut self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as f64)
            .unwrap_or(0.0)
    }
    /// `randomUUID()`.
    fn uuid(&mut self) -> String {
        random_uuid()
    }
    /// A journal line id: `randomBytes(8)` in hex.
    fn line_id(&mut self) -> String {
        random_hex(8)
    }
    /// The trusted public key (`readPublicKey`), by default the store's own key file.
    fn trusted_public_key(&mut self, store_dir: &Path) -> Option<String> {
        read_answer_public_key(store_dir)
    }
    /// A change the page shows (a hint).
    fn on_change(&mut self) {}
    /// Test faults; never in production.
    fn fault(&mut self, _fault: Fault<'_>) -> bool {
        false
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    use rand_core::RngCore;
    let mut bytes = vec![0u8; n];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes
}

fn random_hex(n: usize) -> String {
    random_bytes(n).iter().map(|b| format!("{b:02x}")).collect()
}

/// `crypto.randomUUID()`: a version 4 UUID, lowercase.
pub fn random_uuid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// `readAnswerPublicKey`: the store key's public SPKI PEM, or None.
pub fn read_answer_public_key(store_dir: &Path) -> Option<String> {
    use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePublicKey};
    let pem = fs::read_to_string(store_dir.join("answer-key.pem")).ok()?;
    let key = ed25519_dalek::SigningKey::from_pkcs8_pem(&pem).ok()?;
    key.verifying_key().to_public_key_pem(pem_rfc7468::LineEnding::LF).ok()
}

// ---- JSON as JavaScript writes and reads it ------------------------------------------------

/// `JSON.stringify(value)` (compact): Node's string escapes and ECMAScript number text.
pub fn js_stringify(value: &Json) -> String {
    let mut out = String::new();
    write_js(value, &mut out);
    out
}

fn write_js(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&aac_num::ecma::json(n.as_f64().unwrap_or(f64::NAN)));
            }
        }
        Json::String(s) => write_js_string(s, out),
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_js(item, out);
            }
            out.push(']');
        }
        Json::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_js_string(key, out);
                out.push(':');
                write_js(item, out);
            }
            out.push('}');
        }
    }
}

fn write_js_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: Option<&Json>) -> bool {
    match value {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(Json::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Json::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn str_of<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Json::as_str)
}

/// `Date.parse`, NaN when it does not parse.
fn date_parse(text: Option<&str>) -> f64 {
    text.and_then(aac_num::ecma::parse_instant)
        .map_or(f64::NAN, |ms| ms as f64)
}

/// `new Date(ms).toISOString()`.
fn iso(ms: f64) -> String {
    aac_num::ecma::to_iso_string(ms as i64).unwrap_or_default()
}

/// `contentHash`: sha256 hex of the canonical JSON.
pub fn content_hash(value: &Json) -> String {
    Sha256::digest(canonical_json(value).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `cardVersion`.
pub fn card_version(id: &str, seq: &Json, sha256: &str) -> String {
    format!("{id}\n{}\n{sha256}", number_text(seq))
}

fn number_text(value: &Json) -> String {
    match value {
        Json::Number(_) => js_stringify(value),
        Json::String(s) => s.clone(),
        other => js_stringify(other),
    }
}

fn same_number(a: Option<&Json>, b: Option<&Json>) -> bool {
    match (a.and_then(Json::as_f64), b.and_then(Json::as_f64)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

// ---- entries -------------------------------------------------------------------------------

/// The fixed refusal sentences (`REFUSALS`).
pub fn refusal_text(code: &str) -> &'static str {
    match code {
        "card_changed" => "This card changed or is no longer open. Reload it and answer again.",
        "invalid_entry" => "This entry is not a valid answer.",
        "invalid_choice" => "The choice does not match this card\u{2019}s options.",
        "empty_entry" => "Choose an option or write a note.",
        "invalid_note" => "The note must be at most 4000 characters of plain text.",
        "note_secret" => {
            "The note looks like it holds a secret or personal detail (a key, token, password or email address). Notes are written to the shared folder; take it out and send again."
        }
        "invalid_via" => "Only an unchanged recommendation can be sent as recommended.",
        "invalid_kind" => "This kind of answer does not fit this card.",
        "words_required" => "An objection needs your words in the note.",
        "sample_card" => "This is a format sample; no answer is wanted.",
        "judgement_anchored" => {
            "This judgement card carries a recommendation, so it cannot be answered until the agent removes it."
        }
        "answer_not_saved" => "The answer could not be saved. Nothing was sent.",
        "answer_save_unclear" => {
            "The answer may not have been saved. Don\u{2019}t send it again yet; check again in a minute."
        }
        "store_busy" => {
            "Another AI Account Center dashboard is recording answers from this folder. Nothing was sent. Close it, then send again."
        }
        "share_unavailable" => {
            "The reports share did not answer in time, so this card could not be checked. Nothing was recorded for it; try again."
        }
        _ => "This entry is not a valid answer.",
    }
}

/// `refused(cardId, code, kind?)`.
pub fn refused(card_id: Option<&str>, code: &str, kind: Option<&str>) -> Json {
    let text = refusal_text(code);
    json!({
        "cardId": card_id,
        "outcome": if code == "card_changed" { "card_changed" } else { "refused" },
        "code": code,
        "error": match kind { Some(kind) => format!("{text} It looks like {kind}."), None => text.to_owned() },
    })
}

fn recorded_result(card_id: Option<&str>) -> Json {
    json!({"cardId": card_id, "outcome": "recorded", "code": null, "error": null})
}

/// `isCardId`.
pub fn is_card_id(value: &str) -> bool {
    crate::validate::is_card_id(Some(&Json::from(value)))
}

fn named_card_id(raw: &Json) -> Option<String> {
    raw.get("cardId")
        .and_then(Json::as_str)
        .filter(|id| is_card_id(id))
        .map(str::to_owned)
}

/// One entry of an answer set (`AnswerEntry`).
#[derive(Clone, Debug)]
pub struct AnswerEntry {
    /// The card.
    pub card_id: String,
    /// Its version.
    pub card_seq: Json,
    /// Its file's sha256.
    pub card_sha256: String,
    /// The option keys.
    pub choice: Vec<String>,
    /// The raw note, when sent.
    pub note: Option<String>,
    /// `deck` or `recommended`.
    pub via: String,
    /// An explicit kind.
    pub kind: Option<String>,
    /// The draft's id.
    pub entry_id: Option<String>,
}

/// `parseAnswerEntry`: the strict shape, or None.
pub fn parse_answer_entry(raw: &Json) -> Option<AnswerEntry> {
    let rec = raw.as_object()?;
    const KEYS: [&str; 8] = [
        "cardId",
        "cardSeq",
        "cardSha256",
        "choice",
        "note",
        "via",
        "kind",
        "entryId",
    ];
    if !rec.keys().all(|k| KEYS.contains(&k.as_str())) {
        return None;
    }
    let entry_id = match rec.get("entryId") {
        None => None,
        Some(Json::String(id))
            if id.len() == 32 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
        {
            Some(id.clone())
        }
        Some(_) => return None,
    };
    let kind = match rec.get("kind") {
        None => None,
        Some(Json::String(k)) if RECORD_KINDS.contains(&k.as_str()) => Some(k.clone()),
        Some(_) => return None,
    };
    let card_id = rec.get("cardId")?.as_str().filter(|id| is_card_id(id))?.to_owned();
    let seq = rec.get("cardSeq")?;
    let seq_value = seq.as_f64()?;
    if !seq.is_number() || seq_value.fract() != 0.0 || seq_value < 0.0 || seq_value > 9_007_199_254_740_991.0 {
        return None;
    }
    let sha = rec.get("cardSha256")?.as_str()?;
    if sha.len() != 64 || !sha.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return None;
    }
    let choice = rec.get("choice")?.as_array()?;
    if choice.len() > 4 {
        return None;
    }
    let mut keys = Vec::with_capacity(choice.len());
    for key in choice {
        let key = key.as_str()?;
        // `key.length` counts UTF-16 code units.
        let units = key.encode_utf16().count();
        if units == 0 || units > 32 {
            return None;
        }
        keys.push(key.to_owned());
    }
    let note = match rec.get("note") {
        None => None,
        Some(Json::String(n)) => Some(n.clone()),
        Some(_) => return None,
    };
    let via = rec
        .get("via")?
        .as_str()
        .filter(|v| *v == "deck" || *v == "recommended")?
        .to_owned();
    Some(AnswerEntry {
        card_id,
        card_seq: seq.clone(),
        card_sha256: sha.to_owned(),
        choice: keys,
        note,
        via,
        kind,
        entry_id,
    })
}

fn js_trim_char(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `normalizeNote`: trimmed, or None when it is not acceptable. (A lone surrogate cannot reach a
/// Rust string; the admission refuses it first.)
pub fn normalize_note(note: Option<&str>) -> Option<String> {
    let Some(note) = note else {
        return Some(String::new());
    };
    let trimmed = note.trim_matches(js_trim_char);
    if trimmed
        .chars()
        .any(|c| matches!(c as u32, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f))
    {
        return None;
    }
    if trimmed.chars().count() > ANSWER_NOTE_MAX {
        return None;
    }
    Some(trimmed.to_owned())
}

/// `validChoice`.
pub fn valid_choice(card: &Json, choice: &[Json]) -> bool {
    let keys: Vec<&Json> = card
        .get("options")
        .and_then(Json::as_array)
        .map(|options| options.iter().filter_map(|o| o.get("key")).collect())
        .unwrap_or_default();
    for (i, key) in choice.iter().enumerate() {
        if choice[..i].contains(key) {
            return false;
        }
    }
    if !choice.iter().all(|key| keys.contains(&key)) {
        return false;
    }
    truthy(card.get("multiple")) || choice.len() <= 1
}

/// `entryKind`: the record kind, or the refusal code.
pub fn entry_kind(card: &Json, entry: &AnswerEntry, note: &str) -> &'static str {
    let card_kind = str_of(card, "kind").unwrap_or("question");
    if card_kind == "sample" {
        return "sample_card";
    }
    let recommended = truthy(card.get("recommendation"));
    if card_kind == "judgement" && recommended {
        return "judgement_anchored";
    }
    let kind = entry
        .kind
        .as_deref()
        .unwrap_or(if entry.choice.is_empty() { "note" } else { "answer" });
    if kind == "answer" {
        return if (card_kind == "question" || card_kind == "judgement") && !entry.choice.is_empty() {
            "answer"
        } else {
            "invalid_kind"
        };
    }
    if !entry.choice.is_empty() {
        return "invalid_kind";
    }
    match kind {
        "note" => {
            if note.is_empty() {
                "empty_entry"
            } else {
                "note"
            }
        }
        "object" => {
            if card_kind != "objection" {
                "invalid_kind"
            } else if note.is_empty() {
                "words_required"
            } else {
                "object"
            }
        }
        _ => {
            if card_kind == "action" {
                "done"
            } else {
                "invalid_kind"
            }
        }
    }
}

/// `draftHash`.
fn draft_hash(entry: &AnswerEntry, note: &str) -> String {
    let kind = entry
        .kind
        .as_deref()
        .unwrap_or(if entry.choice.is_empty() { "note" } else { "answer" });
    let mut value = json!({
        "cardId": entry.card_id,
        "choice": entry.choice,
        "note": note,
        "via": entry.via,
        "kind": entry.kind,
    });
    if kind == "answer" || kind == "done" {
        value["cardSeq"] = entry.card_seq.clone();
        value["cardSha256"] = Json::from(entry.card_sha256.clone());
    }
    content_hash(&value)
}

// ---- history -------------------------------------------------------------------------------

/// One delivery step (`StepState`).
#[derive(Clone, Debug, PartialEq)]
pub struct StepState {
    /// `pending`, `done`, `failed` or `skipped`.
    pub state: String,
    /// When it was recorded.
    pub at: Option<String>,
    /// Its words.
    pub detail: Option<String>,
    /// Attempts so far.
    pub attempts: u64,
}

impl StepState {
    fn pending() -> Self {
        Self {
            state: "pending".into(),
            at: None,
            detail: None,
            attempts: 0,
        }
    }
    fn with_state(state: &str) -> Self {
        Self {
            state: state.into(),
            ..Self::pending()
        }
    }
    /// `{state, at, detail, attempts}`.
    pub fn to_json(&self) -> Json {
        json!({"state": self.state, "at": self.at, "detail": self.detail, "attempts": self.attempts})
    }
    fn retryable(&self) -> bool {
        (self.state == "pending" || self.state == "failed") && self.attempts < MAX_DELIVERY_ATTEMPTS
    }
}

/// `furthest`: done, else the one with more attempts.
fn furthest(a: &StepState, b: &StepState) -> StepState {
    if a.state == "done" {
        return a.clone();
    }
    if b.state == "done" {
        return b.clone();
    }
    if a.attempts >= b.attempts { a.clone() } else { b.clone() }
}

fn step_from(record: &Json, before: &StepState) -> StepState {
    let state = match str_of(record, "state") {
        Some(s @ ("done" | "failed" | "skipped")) => s,
        _ => "failed",
    };
    StepState {
        state: state.into(),
        at: str_of(record, "at").map(str::to_owned),
        detail: str_of(record, "detail").map(str::to_owned),
        attempts: before.attempts + 1,
    }
}

/// One record (`AnswerHistoryEntry`).
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// The record id.
    pub id: String,
    /// The send it came in.
    pub send_id: Option<String>,
    /// A verified share copy without local history.
    pub shared_only: bool,
    /// The signed answer (with `kind`).
    pub answer: Json,
    /// The card version answered.
    pub card: Json,
    /// The share step.
    pub share: StepState,
    /// The doorbell step.
    pub doorbell: StepState,
}

impl HistoryEntry {
    /// The entry as the TypeScript history object serializes.
    pub fn to_json(&self) -> Json {
        let mut map = Map::new();
        map.insert("id".into(), Json::from(self.id.clone()));
        map.insert("sendId".into(), self.send_id.clone().map_or(Json::Null, Json::from));
        if self.shared_only {
            map.insert("sharedOnly".into(), Json::Bool(true));
        }
        map.insert("answer".into(), self.answer.clone());
        map.insert("card".into(), self.card.clone());
        map.insert("share".into(), self.share.to_json());
        map.insert("doorbell".into(), self.doorbell.to_json());
        Json::Object(map)
    }
    fn kind(&self) -> &str {
        str_of(&self.answer, "kind").unwrap_or("")
    }
    fn closing(&self) -> bool {
        CLOSING_KINDS.contains(&self.kind())
    }
}

/// One send (`SendRecord`).
#[derive(Clone, Debug)]
pub struct SendRecord {
    /// The server's send id.
    pub send_id: String,
    /// The send's durable time.
    pub at: String,
    /// The device word.
    pub device: String,
    /// Its records.
    pub entry_ids: Vec<String>,
    /// The signed summary's step.
    pub summary: StepState,
}

impl SendRecord {
    /// As the TypeScript send object serializes.
    pub fn to_json(&self) -> Json {
        json!({
            "sendId": self.send_id,
            "at": self.at,
            "device": self.device,
            "entryIds": self.entry_ids,
            "summary": self.summary.to_json(),
        })
    }
}

/// `summaryWarning`.
pub fn summary_warning(summary: Option<&StepState>) -> Option<String> {
    let summary = summary?;
    if summary.state == "done" {
        return None;
    }
    if summary.state == "skipped" {
        return summary.detail.clone();
    }
    if summary.state == "pending" || summary.attempts == 0 {
        return Some("The signed send summary is not on the share yet.".into());
    }
    Some(if summary.attempts >= MAX_DELIVERY_ATTEMPTS {
        format!(
            "The signed send summary could not be written to the share after {} attempts; no more retries.",
            summary.attempts
        )
    } else {
        format!(
            "The signed send summary could not be written to the share yet (attempt {} of {MAX_DELIVERY_ATTEMPTS}); retrying.",
            summary.attempts
        )
    })
}

fn attempt_of(step: &StepState) -> String {
    if step.attempts > 0 {
        format!(" (attempt {} of {MAX_DELIVERY_ATTEMPTS})", step.attempts)
    } else {
        String::new()
    }
}

/// `deliveryView`: the page's delivery summary for one record.
pub fn delivery_view(entry: &HistoryEntry, sends: &IndexMap<String, SendRecord>) -> Json {
    let (share, doorbell) = (&entry.share, &entry.doorbell);
    let summary = entry.send_id.as_ref().and_then(|id| sends.get(id)).map(|s| &s.summary);
    let view = |state: &str, at: Option<&String>, detail: Option<String>| {
        json!({
            "share": share.to_json(),
            "doorbell": doorbell.to_json(),
            "summary": summary.map(StepState::to_json),
            "summaryWarning": summary_warning(summary),
            "state": state,
            "at": at,
            "detail": detail,
        })
    };
    if entry.shared_only {
        return view("published", None, Some("The signed answer is on the share.".into()));
    }
    if share.state == "skipped" {
        return view("failed", share.at.as_ref(), share.detail.clone());
    }
    if share.state == "failed" {
        let detail = if share.attempts >= MAX_DELIVERY_ATTEMPTS {
            format!(
                "The signed copy could not be written to the share after {} attempts; no more retries.",
                share.attempts
            )
        } else if share.detail.as_deref() == Some(SHARE_UNAVAILABLE_DETAIL) {
            format!(
                "The reports share is not available, so the signed copy is not written yet{}; retrying.",
                attempt_of(share)
            )
        } else {
            format!(
                "The signed copy could not be written to the share yet{}; retrying.",
                attempt_of(share)
            )
        };
        return view("failed", share.at.as_ref(), Some(detail));
    }
    if doorbell.state == "failed" {
        let detail = if doorbell.attempts >= MAX_DELIVERY_ATTEMPTS {
            format!(
                "The asker could not be rung after {} attempts; no more retries. The signed answer is on the share.",
                doorbell.attempts
            )
        } else {
            format!(
                "The asker could not be rung yet{}; retrying. The signed answer is on the share.",
                attempt_of(doorbell)
            )
        };
        return view("failed", doorbell.at.as_ref(), Some(detail));
    }
    if share.state == "done" && doorbell.state == "skipped" && doorbell.detail.as_deref() == Some(NO_MAIL_DETAIL) {
        return view("rang", doorbell.at.as_ref(), Some(NO_MAIL_DETAIL.into()));
    }
    if doorbell.state == "skipped" {
        return view("failed", doorbell.at.as_ref(), doorbell.detail.clone());
    }
    if share.state == "done" && doorbell.state == "done" {
        return view("rang", doorbell.at.as_ref(), doorbell.detail.clone());
    }
    view("pending", None, Some("Delivering.".into()))
}

// ---- the journal ---------------------------------------------------------------------------

/// Why a store operation failed.
#[derive(Debug)]
pub enum Fail {
    /// `WriterBusyError`.
    Busy(String),
    /// A refused record whose void line was refused too (`AppendUnclearError`).
    Unclear(String),
    /// Any other failure.
    Other(String),
}

impl From<io::Error> for Fail {
    fn from(error: io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<LockError> for Fail {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Busy(holder) => Self::Busy(holder),
            LockError::Io(error) => Self::Other(error.to_string()),
        }
    }
}

type R<T> = Result<T, Fail>;

struct Replay {
    at: f64,
    results: Vec<Json>,
    entries_hash: Option<String>,
    partial: bool,
}

struct Draft {
    at: f64,
    hash: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepName {
    Share,
    Doorbell,
    Summary,
}

impl StepName {
    fn word(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::Doorbell => "doorbell",
            Self::Summary => "summary",
        }
    }
}

struct Pending {
    line: Json,
    target: String,
    step: StepName,
    value: StepState,
}

/// `startBudget`: time left on the wall clock.
pub struct Budget {
    ends: Instant,
    spent: bool,
}

impl Budget {
    /// A budget of `ms`.
    pub fn start(ms: u64) -> Self {
        Self {
            ends: Instant::now() + Duration::from_millis(ms),
            spent: false,
        }
    }
    /// Milliseconds left.
    pub fn left(&self) -> u64 {
        if self.spent {
            return 0;
        }
        self.ends.saturating_duration_since(Instant::now()).as_millis() as u64
    }
    fn spend(&mut self) {
        self.spent = true;
    }
}

fn open_append(file: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(file)
}

fn create_dirs_private(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// The submit's results and the records it made, whose ring the caller runs next
/// ([`AnswersEngine::ring_groups`]), as `submitSet` does once its queue turn ends.
pub struct Submitted {
    /// One result per entry, in order.
    pub results: Vec<Json>,
    /// The ids recorded, for the ring.
    pub recorded: Vec<String>,
}

/// One ring pass's claimed groups.
pub struct RingClaim {
    /// One per (send, asker), in first-appearance order.
    pub groups: Vec<RingGroupClaim>,
}

/// One claimed group.
pub struct RingGroupClaim {
    /// The pm-mail name.
    pub to: String,
    /// The send, or None for a record from before sends.
    pub send_id: Option<String>,
    /// The claimed records.
    pub ids: Vec<String>,
}

/// One ring, ready to go out.
pub struct Ring {
    /// The pm-mail name.
    pub to: String,
    /// The subject.
    pub subject: String,
    /// The body.
    pub body: String,
    /// The ring's limit.
    pub timeout: Duration,
    /// The records it rings.
    pub ids: Vec<String>,
}

/// What [`AnswersEngine::prepare_ring`] found.
pub enum Prepared {
    /// Ring this.
    Ring(Ring),
    /// Already rung: the next group.
    Nothing,
    /// The store is another's: the pass ends.
    NotOwner,
}

/// The answer store (`StatusAnswers`).
pub struct AnswersEngine {
    store_dir: PathBuf,
    reports_root: PathBuf,
    timing: Timing,
    host: Box<dyn AnswerHost>,
    entries: Option<Vec<HistoryEntry>>,
    held_entries: IndexMap<String, HistoryEntry>,
    written: HashSet<String>,
    sharing: HashSet<String>,
    own_sends: HashSet<String>,
    summarizing: HashSet<String>,
    signed_as_stored: std::collections::HashMap<String, Json>,
    voided_ids: HashSet<String>,
    unvoided: IndexSet<String>,
    shared_answers: IndexMap<String, HistoryEntry>,
    history_unavailable: bool,
    loaded_from: Option<String>,
    held_sends: IndexMap<String, SendRecord>,
    pending: IndexMap<String, Pending>,
    sends: IndexMap<String, SendRecord>,
    key: Option<AnswerKey>,
    retrying: bool,
    owner: bool,
    epoch: u64,
    send_epoch: Option<u64>,
    ringing: HashSet<String>,
    replays: IndexMap<String, Replay>,
    drafts: IndexMap<String, Draft>,
    known_key_id: Option<String>,
}

impl AnswersEngine {
    /// A store over `<store_dir>/answers.jsonl` delivering to `<reports_root>/v1`.
    pub fn new(store_dir: PathBuf, reports_root: PathBuf, timing: Timing, host: Box<dyn AnswerHost>) -> Self {
        Self {
            store_dir,
            reports_root,
            timing,
            host,
            entries: None,
            held_entries: IndexMap::new(),
            written: HashSet::new(),
            sharing: HashSet::new(),
            own_sends: HashSet::new(),
            summarizing: HashSet::new(),
            signed_as_stored: std::collections::HashMap::new(),
            voided_ids: HashSet::new(),
            unvoided: IndexSet::new(),
            shared_answers: IndexMap::new(),
            history_unavailable: false,
            loaded_from: None,
            held_sends: IndexMap::new(),
            pending: IndexMap::new(),
            sends: IndexMap::new(),
            key: None,
            retrying: false,
            owner: false,
            epoch: 0,
            send_epoch: None,
            ringing: HashSet::new(),
            replays: IndexMap::new(),
            drafts: IndexMap::new(),
            known_key_id: None,
        }
    }

    /// The host, for tests that steer it.
    pub fn host_mut(&mut self) -> &mut dyn AnswerHost {
        self.host.as_mut()
    }

    /// Whether this engine holds the store's writer lock (as of its last confirmation).
    pub fn is_owner(&self) -> bool {
        self.owner
    }

    fn store_file(&self) -> PathBuf {
        self.store_dir.join(JOURNAL_FILE)
    }

    fn now(&mut self) -> f64 {
        self.host.now()
    }

    fn notify(&mut self) {
        self.host.on_change();
    }

    // ---- reading ----

    /// `list()`: every record, oldest first.
    pub fn list(&mut self) -> R<&[HistoryEntry]> {
        self.ensure_list()?;
        Ok(self.entries.as_deref().unwrap_or(&[]))
    }

    /// `history(limit)`: newest first.
    pub fn history(&mut self, limit: usize) -> R<Vec<HistoryEntry>> {
        Ok(self.list()?.iter().rev().take(limit).cloned().collect())
    }

    /// `sendList()`: the sends, oldest first.
    pub fn send_list(&mut self) -> R<Vec<SendRecord>> {
        self.ensure_list()?;
        Ok(self.sends.values().cloned().collect())
    }

    /// The sends by id, for [`delivery_view`].
    pub fn sends(&self) -> &IndexMap<String, SendRecord> {
        &self.sends
    }

    fn ensure_list(&mut self) -> R<()> {
        if self.history_unavailable {
            match self.writer() {
                Ok(()) => return self.ensure_list(),
                Err(Fail::Busy(_)) => {}
                Err(error) => return Err(error),
            }
        }
        let from = if self.owner && self.entries.is_some() {
            self.loaded_from.clone()
        } else {
            self.journal_stamp()
        };
        if self.entries.is_some() && (self.owner || (from.is_some() && from == self.loaded_from)) {
            return Ok(());
        }
        if self.entries.is_some() {
            self.hold_own();
            self.entries = None;
        }
        let text = match fs::read(self.store_file()) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                self.history_unavailable = true;
                return Err(error.into());
            }
        };
        let held: Vec<(String, SendRecord)> = self
            .sends
            .iter()
            .filter(|(id, _)| self.holds_send(id) && !self.held_sends.contains_key(*id))
            .map(|(id, send)| (id.clone(), send.clone()))
            .collect();
        self.held_sends.extend(held);
        self.sends.clear();
        self.replays.clear();
        self.drafts.clear();
        let loaded = self.load(&text);
        self.entries = Some(loaded);
        self.loaded_from = from;
        self.history_unavailable = false;
        Ok(())
    }

    fn holds_entry(&self, id: &str) -> bool {
        self.written.contains(id)
            || self.sharing.contains(id)
            || self.ringing.contains(id)
            || self.pending.contains_key(&format!("{id}:share"))
            || self.pending.contains_key(&format!("{id}:doorbell"))
    }

    fn holds_send(&self, id: &str) -> bool {
        self.own_sends.contains(id)
            || self.summarizing.contains(id)
            || self.pending.contains_key(&format!("{id}:summary"))
    }

    fn hold_own(&mut self) {
        let Some(entries) = self.entries.as_ref() else {
            return;
        };
        let keep: Vec<HistoryEntry> = entries
            .iter()
            .filter(|e| self.holds_entry(&e.id) && !self.held_entries.contains_key(&e.id))
            .cloned()
            .collect();
        for entry in keep {
            self.held_entries.insert(entry.id.clone(), entry);
        }
    }

    fn journal_stamp(&self) -> Option<String> {
        match fs::metadata(self.store_file()) {
            Ok(meta) => {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                #[cfg(unix)]
                let ino = {
                    use std::os::unix::fs::MetadataExt;
                    meta.ino()
                };
                #[cfg(not(unix))]
                let ino = 0u64;
                Some(format!("{}:{mtime}:{ino}", meta.len()))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Some("absent".into()),
            Err(_) => None,
        }
    }

    /// `load`: folds an already-read journal (sends, replays and drafts must be cleared first).
    fn load(&mut self, text: &str) -> Vec<HistoryEntry> {
        let mut entries: Vec<HistoryEntry> = Vec::new();
        let mut records: Vec<Json> = Vec::new();
        let mut voided: HashSet<String> = HashSet::new();
        struct Request {
            hash: String,
            slots: Vec<Option<Json>>,
        }
        let mut requests: std::collections::HashMap<String, Request> = std::collections::HashMap::new();
        for line in text.split('\n') {
            if line.trim_matches(js_trim_char).is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Json>(line) else {
                continue;
            };
            if !record.is_object() {
                continue;
            }
            if str_of(&record, "type") == Some("void") {
                if let Some(id) = str_of(&record, "line") {
                    voided.insert(id.to_owned());
                    continue;
                }
            }
            records.push(record);
        }
        voided.extend(self.unvoided.iter().cloned());
        let mut voided_ids = HashSet::new();
        for record in &records {
            if str_of(record, "line").is_some_and(|l| voided.contains(l)) {
                if let (Some(id), Some("answer")) = (str_of(record, "id"), str_of(record, "type")) {
                    voided_ids.insert(id.to_owned());
                }
                continue;
            }
            let kind = str_of(record, "type");
            if kind == Some("answer")
                && str_of(record, "id").is_some()
                && truthy(record.get("answer"))
                && truthy(record.get("card"))
            {
                let id = str_of(record, "id").unwrap_or_default().to_owned();
                let stored = record["answer"].clone();
                let answer = if truthy(stored.get("kind")) {
                    stored
                } else {
                    self.signed_as_stored.insert(id.clone(), stored.clone());
                    let mut map = stored.as_object().cloned().unwrap_or_default();
                    map.insert("kind".into(), Json::from("answer"));
                    Json::Object(map)
                };
                let entry = HistoryEntry {
                    id: id.clone(),
                    send_id: str_of(record, "sendId").map(str::to_owned),
                    shared_only: false,
                    answer,
                    card: record["card"].clone(),
                    share: StepState::pending(),
                    doorbell: StepState::pending(),
                };
                if let Some(send_id) = entry.send_id.clone() {
                    match self.sends.get_mut(&send_id) {
                        Some(send) => send.entry_ids.push(id.clone()),
                        None => {
                            let at = str_of(&entry.answer, "answeredAt").unwrap_or_default().to_owned();
                            let device = str_of(&entry.answer, "device").unwrap_or_default().to_owned();
                            self.sends.insert(
                                send_id.clone(),
                                SendRecord {
                                    send_id,
                                    at,
                                    device,
                                    entry_ids: vec![id.clone()],
                                    summary: StepState::pending(),
                                },
                            );
                        }
                    }
                }
                let at = match record.get("at") {
                    Some(Json::String(at)) => date_parse(Some(at)),
                    _ => 0.0,
                };
                if let (Some(entry_id), Some(hash)) = (str_of(record, "entryId"), str_of(record, "entryHash")) {
                    self.remember_draft(entry_id, at, hash);
                }
                let index = record.get("entryIndex").and_then(Json::as_f64);
                let prior = record.get("priorResults").and_then(Json::as_array);
                if let (Some(client), Some(hash), Some(index), Some(prior)) = (
                    str_of(record, "clientSendId"),
                    str_of(record, "entriesHash"),
                    index,
                    prior,
                ) {
                    if index.fract() == 0.0
                        && index >= 0.0
                        && (prior.len() as f64) <= index
                        && prior.iter().all(is_entry_result)
                    {
                        let index = index as usize;
                        let request = match requests.remove(client) {
                            Some(known) if known.hash == hash => known,
                            _ => Request {
                                hash: hash.to_owned(),
                                slots: Vec::new(),
                            },
                        };
                        let mut request = request;
                        if request.slots.len() <= index {
                            request.slots.resize(index + 1, None);
                        }
                        for (k, result) in prior.iter().enumerate() {
                            let slot = index - prior.len() + k;
                            if request.slots[slot].is_none() {
                                request.slots[slot] = Some(result.clone());
                            }
                        }
                        request.slots[index] = Some(recorded_result(str_of(&entry.answer, "cardId")));
                        let results: Vec<Json> = request.slots.iter().map_while(|s| s.clone()).collect();
                        let hash = request.hash.clone();
                        requests.insert(client.to_owned(), request);
                        self.remember(client, at, results, Some(hash), true);
                    }
                }
                entries.push(entry);
            } else if kind == Some("send") && str_of(record, "sendId").is_some() {
                let send_id = str_of(record, "sendId").unwrap_or_default().to_owned();
                self.sends.insert(
                    send_id.clone(),
                    SendRecord {
                        send_id,
                        at: str_of(record, "at").unwrap_or("").to_owned(),
                        device: str_of(record, "device").unwrap_or("browser").to_owned(),
                        entry_ids: record
                            .get("entryIds")
                            .and_then(Json::as_array)
                            .map(|ids| ids.iter().filter_map(Json::as_str).map(str::to_owned).collect())
                            .unwrap_or_default(),
                        summary: StepState::pending(),
                    },
                );
                if let (Some(client), Some(results)) = (
                    str_of(record, "clientSendId"),
                    record.get("results").and_then(Json::as_array),
                ) {
                    let hash = str_of(record, "entriesHash").map(str::to_owned);
                    let at = match record.get("at") {
                        Some(Json::String(at)) => date_parse(Some(at)),
                        _ => 0.0,
                    };
                    self.remember(client, at, results.clone(), hash.clone(), false);
                    match hash {
                        Some(hash) => {
                            requests.insert(
                                client.to_owned(),
                                Request {
                                    hash,
                                    slots: results.iter().cloned().map(Some).collect(),
                                },
                            );
                        }
                        None => {
                            requests.remove(client);
                        }
                    }
                }
            } else if kind == Some("delivery") && str_of(record, "id").is_some() {
                let id = str_of(record, "id").unwrap_or_default();
                if str_of(record, "step") == Some("summary") {
                    if let Some(send) = self.sends.get_mut(id) {
                        send.summary = step_from(record, &send.summary);
                    }
                    continue;
                }
                let step = match str_of(record, "step") {
                    Some("share") => StepName::Share,
                    Some("doorbell") => StepName::Doorbell,
                    _ => continue,
                };
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    match step {
                        StepName::Share => entry.share = step_from(record, &entry.share),
                        _ => entry.doorbell = step_from(record, &entry.doorbell),
                    }
                }
            }
        }
        self.voided_ids = voided_ids;
        entries
    }

    fn remember_draft(&mut self, entry_id: &str, at: f64, hash: &str) {
        self.drafts.shift_remove(entry_id);
        self.drafts.insert(
            entry_id.to_owned(),
            Draft {
                at,
                hash: hash.to_owned(),
            },
        );
        let now = self.now();
        loop {
            let Some((_, oldest)) = self.drafts.first() else {
                break;
            };
            if self.drafts.len() <= MIN_REMEMBERED_ENTRIES || now - oldest.at < REMEMBER_ENTRY_MS {
                break;
            }
            self.drafts.shift_remove_index(0);
        }
    }

    fn remember(&mut self, client: &str, at: f64, results: Vec<Json>, entries_hash: Option<String>, partial: bool) {
        self.replays.shift_remove(client);
        self.replays.insert(
            client.to_owned(),
            Replay {
                at,
                results,
                entries_hash,
                partial,
            },
        );
        while self.replays.len() > MAX_REMEMBERED_SENDS {
            self.replays.shift_remove_index(0);
        }
    }

    // ---- the writer ----

    /// `writer()`: takes or confirms the store's writer lock; a newly gained store is read again
    /// before anything is judged against it, keeping this process's own delivery state.
    fn writer(&mut self) -> R<()> {
        self.flush_completions();
        let gained = match hold_writer_lock(&self.store_dir) {
            Ok(gained) => gained,
            Err(error) => {
                self.owner = false;
                return Err(error.into());
            }
        };
        if gained || !self.owner {
            let from = self.journal_stamp();
            let text = match fs::read(self.store_file()) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) => {
                    self.owner = false;
                    if error.kind() != io::ErrorKind::NotFound {
                        self.hold_own();
                        self.entries = None;
                        self.history_unavailable = true;
                        return Err(error.into());
                    }
                    String::new()
                }
            };
            self.hold_own();
            let mut held_entries: IndexMap<String, HistoryEntry> = std::mem::take(&mut self.held_entries)
                .into_iter()
                .filter(|(id, _)| self.holds_entry(id))
                .collect();
            let mut held_sends: IndexMap<String, SendRecord> = IndexMap::new();
            for (id, send) in self.sends.iter().chain(self.held_sends.iter()) {
                if self.holds_send(id) {
                    held_sends.insert(id.clone(), send.clone());
                }
            }
            self.entries = None;
            self.sends.clear();
            self.replays.clear();
            self.drafts.clear();
            let fresh = self.load(&text);
            let mut merged = Vec::with_capacity(fresh.len());
            for entry in fresh {
                match held_entries.shift_remove(&entry.id) {
                    Some(mut held) => {
                        held.share = furthest(&held.share, &entry.share);
                        held.doorbell = furthest(&held.doorbell, &entry.doorbell);
                        merged.push(held);
                    }
                    None => merged.push(entry),
                }
            }
            for (id, held) in held_entries {
                if self.written.contains(&id) && !self.voided_ids.contains(&id) {
                    merged.push(held);
                }
            }
            for (id, mut held) in held_sends {
                if let Some(fresh) = self.sends.get(&id) {
                    held.summary = furthest(&held.summary, &fresh.summary);
                    held.entry_ids = fresh.entry_ids.clone();
                } else if !self.own_sends.contains(&id) {
                    continue;
                }
                self.sends.insert(id, held);
            }
            self.entries = Some(merged);
            self.held_entries.clear();
            self.held_sends.clear();
            self.loaded_from = from;
            self.history_unavailable = false;
            self.owner = true;
            self.epoch += 1;
        }
        self.ensure_list()?;
        self.flush_completions();
        if !self.pending.is_empty() {
            return Err(Fail::Other("Delivery completion could not be saved.".into()));
        }
        Ok(())
    }

    fn gain_quietly(&mut self) {
        let _ = self.writer();
    }

    /// `flushCompletions`: lands pending void lines, then pending completion lines. The host calls
    /// it on shutdown, as the TypeScript exit hook does.
    pub fn flush_completions(&mut self) {
        if self.pending.is_empty() && self.unvoided.is_empty() {
            return;
        }
        match hold_writer_lock(&self.store_dir) {
            Ok(true) => self.owner = false,
            Ok(false) => {}
            Err(_) => {
                self.owner = false;
                return;
            }
        }
        for line in self.unvoided.clone() {
            if self.append_void_line(&line).is_ok() {
                self.unvoided.shift_remove(&line);
            }
        }
        let keys: Vec<String> = self.pending.keys().cloned().collect();
        for key in keys {
            let Some(line) = self.pending.get(&key).map(|p| p.line.clone()) else {
                continue;
            };
            if self.append(&line).is_err() {
                continue;
            }
            if let Some(pending) = self.pending.shift_remove(&key) {
                self.set_step(&pending.target, pending.step, pending.value);
            }
        }
    }

    /// Sets one step on this process's object(s) for the record or send `id`.
    fn set_step(&mut self, id: &str, step: StepName, value: StepState) {
        if step == StepName::Summary {
            if let Some(send) = self.sends.get_mut(id) {
                send.summary = value.clone();
            }
            if let Some(send) = self.held_sends.get_mut(id) {
                send.summary = value;
            }
            return;
        }
        let apply = |entry: &mut HistoryEntry| match step {
            StepName::Share => entry.share = value.clone(),
            _ => entry.doorbell = value.clone(),
        };
        if let Some(entry) = self
            .entries
            .as_mut()
            .and_then(|all| all.iter_mut().find(|e| e.id == id))
        {
            apply(entry);
        }
        if let Some(entry) = self.held_entries.get_mut(id) {
            apply(entry);
        }
    }

    /// The step as this process's object holds it (its held copy first).
    fn step_of(&self, id: &str, step: StepName) -> StepState {
        if step == StepName::Summary {
            return self
                .held_sends
                .get(id)
                .or_else(|| self.sends.get(id))
                .map_or_else(StepState::pending, |s| s.summary.clone());
        }
        let entry = self
            .held_entries
            .get(id)
            .or_else(|| self.entries.as_ref().and_then(|all| all.iter().find(|e| e.id == id)));
        entry.map_or_else(StepState::pending, |e| match step {
            StepName::Share => e.share.clone(),
            _ => e.doorbell.clone(),
        })
    }

    fn entry(&self, id: &str) -> Option<&HistoryEntry> {
        self.held_entries
            .get(id)
            .or_else(|| self.entries.as_ref().and_then(|all| all.iter().find(|e| e.id == id)))
    }

    // ---- appending ----

    fn append_void_line(&mut self, line: &str) -> io::Result<()> {
        let mut file = open_append(&self.store_file())?;
        self.append_void(&mut file, line)
    }

    fn append_void(&mut self, file: &mut fs::File, line: &str) -> io::Result<()> {
        if self.host.fault(Fault::Void) {
            return Err(io::Error::other("Injected void line write error"));
        }
        let cancel = format!("\n{}\n", js_stringify(&json!({"type": "void", "line": line})));
        file.write_all(cancel.as_bytes())?;
        file.sync_all()
    }

    /// `appendLine` with the unvoided bookkeeping of `append`.
    fn append(&mut self, record: &Json) -> R<()> {
        let file = self.store_file();
        create_dirs_private(&self.store_dir)?;
        let mut handle = open_append(&file)?;
        let line = self.host.line_id();
        let mut map = record.as_object().cloned().unwrap_or_default();
        map.insert("line".into(), Json::from(line.clone()));
        let data = format!("\n{}\n", js_stringify(&Json::Object(map)));
        let kind = str_of(record, "type").unwrap_or("");
        let state = str_of(record, "state");
        let mut written = 0usize;
        let outcome: io::Result<()> = (|| {
            if self.host.fault(Fault::Write(kind, state)) {
                return Err(io::Error::other("Injected journal write error"));
            }
            written = handle.write(data.as_bytes())?;
            if written != data.len() {
                return Err(io::Error::other("The answer store took part of a record."));
            }
            if self.host.fault(Fault::Fsync(kind, state)) {
                return Err(io::Error::other("Injected journal fsync error"));
            }
            handle.sync_all()
        })();
        if let Err(error) = outcome {
            if written > 0 && self.append_void(&mut handle, &line).is_err() {
                self.unvoided.insert(line.clone());
                return Err(Fail::Unclear(line));
            }
            return Err(error.into());
        }
        Ok(())
    }

    fn record(&mut self, line: &Json) -> bool {
        if self.writer().is_ok() && self.append(line).is_ok() {
            return true;
        }
        if str_of(line, "type") == Some("delivery")
            && str_of(line, "state") == Some("done")
            && hold_writer_lock(&self.store_dir).is_ok()
            && self.append(line).is_ok()
        {
            return true;
        }
        false
    }

    /// `recordStep` for a record's share or doorbell step, and `writeSummaryHeld`'s summary step.
    fn record_step(&mut self, id: &str, step: StepName, state: &str, detail: &str) {
        self.gain_quietly();
        let key = format!("{id}:{}", step.word());
        let before = self.step_of(id, step);
        if state != "done" && (before.state == "done" || self.pending.contains_key(&key)) {
            return;
        }
        let at = iso(self.now());
        let line =
            json!({"type": "delivery", "id": id, "step": step.word(), "state": state, "at": at, "detail": detail});
        let value = StepState {
            state: state.into(),
            at: Some(at),
            detail: Some(detail.into()),
            attempts: before.attempts + 1,
        };
        if self.record(&line) {
            self.set_step(id, step, value);
        } else if state == "done" {
            self.pending.insert(
                key,
                Pending {
                    line,
                    target: id.to_owned(),
                    step,
                    value,
                },
            );
        } else if !self.pending.contains_key(&key) {
            self.set_step(id, step, value);
        }
        if !self.pending.is_empty() {
            self.flush_completions();
        }
        if step != StepName::Summary {
            self.notify();
        }
    }

    // ---- the key ----

    fn answer_key(&mut self) -> R<AnswerKey> {
        if self.key.is_none() {
            self.writer()?;
            self.key = Some(load_or_create_answer_key(&self.store_dir).map_err(Fail::Other)?);
        }
        let key = self.key.clone().ok_or_else(|| Fail::Other("no key".into()))?;
        self.known_key_id = Some(key.key_id.clone());
        Ok(key)
    }

    // ---- shared answers ----

    /// `isAnswered`.
    pub fn is_answered(&mut self, card_id: &str, seq: &Json, sha256: &str) -> R<bool> {
        if self.shared_answers.contains_key(&card_version(card_id, seq, sha256)) {
            return Ok(true);
        }
        Ok(self.list()?.iter().any(|e| {
            e.closing()
                && str_of(&e.answer, "cardId") == Some(card_id)
                && same_number(e.answer.get("cardSeq"), Some(seq))
                && str_of(&e.answer, "cardSha256") == Some(sha256)
        }))
    }

    /// `sharedAnswersFor`: verified share copies for the open card versions given.
    pub fn shared_answers_for(&self, cards: &[(Json, String)]) -> Vec<HistoryEntry> {
        cards
            .iter()
            .filter(|(doc, _)| str_of(doc, "state") == Some("open"))
            .filter_map(|(doc, sha)| self.shared_answers.get(&row_version(doc, sha)).cloned())
            .collect()
    }

    /// `pruneSharedAnswers`.
    pub fn prune_shared_answers(&mut self, cards: &[(Json, String)]) {
        let current: HashSet<String> = cards.iter().map(|(doc, sha)| row_version(doc, sha)).collect();
        self.shared_answers.retain(|version, _| current.contains(version));
    }

    fn read_shared_closing_answer(&self, card: &Json, hash: &str, public_key: &str, file_name: &str) -> Option<Json> {
        let file = self.reports_root.join("v1").join("answers").join(file_name);
        let bytes = match aac_files::read::read(&file, &aac_files::read::ReadPolicy::capped(64 * 1024)) {
            aac_files::read::Read::Bytes(bytes, _) => bytes,
            _ => return None,
        };
        let answer = verify_signed_answer(&bytes, public_key).ok()?;
        let kind = str_of(&answer, "kind").unwrap_or("answer").to_owned();
        let card_id = str_of(card, "id")?;
        let choice = answer.get("choice").and_then(Json::as_array);
        let ok = str_of(&answer, "cardId") == Some(card_id)
            && same_number(answer.get("cardSeq"), card.get("seq"))
            && str_of(&answer, "cardSha256") == Some(hash)
            && CLOSING_KINDS.contains(&kind.as_str())
            && !(file_name != format!("{card_id}.json") && kind != "note")
            && str_of(&answer, "answeredBy") == Some("owner")
            && str_of(&answer, "answeredAt").is_some_and(crate::validate::real_timestamp)
            && choice.is_some_and(|c| valid_choice(card, c))
            && answer.get("note").is_some_and(Json::is_string)
            && answer.get("device").is_some_and(Json::is_string)
            && matches!(str_of(&answer, "via"), Some("deck" | "recommended"));
        if !ok {
            return None;
        }
        let mut map = answer.as_object().cloned().unwrap_or_default();
        map.insert("kind".into(), Json::from(kind));
        Some(Json::Object(map))
    }

    fn shared_note_names(&self) -> Vec<String> {
        fs::read_dir(self.reports_root.join("v1").join("answers"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `findSharedClosingAnswer`: Err(()) when the budget ran out where it must not.
    fn find_shared_closing_answer(
        &self,
        card: &Json,
        sha256: &str,
        public_key: &str,
        budget: &Budget,
        names: &mut Option<Vec<String>>,
        prefer_latest: bool,
    ) -> Result<Option<Json>, ()> {
        let within = |budget: &Budget| if budget.left() == 0 { Err(()) } else { Ok(()) };
        let card_id = str_of(card, "id").unwrap_or_default();
        within(budget)?;
        let mut selected = self.read_shared_closing_answer(card, sha256, public_key, &format!("{card_id}.json"));
        if !prefer_latest && selected.is_some() {
            return Ok(selected);
        }
        let prefix = format!("{card_id}.note-");
        let run = |selected: &mut Option<Json>, names: &mut Option<Vec<String>>| -> Result<Option<Json>, ()> {
            within(budget)?;
            let listed = names.get_or_insert_with(|| self.shared_note_names()).clone();
            for name in listed {
                let Some(rest) = name.strip_prefix(&prefix) else {
                    continue;
                };
                let hex_ok = rest.len() == 17
                    && rest.ends_with(".json")
                    && rest[..12]
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
                if !hex_ok {
                    continue;
                }
                within(budget)?;
                let Some(note) = self.read_shared_closing_answer(card, sha256, public_key, &name) else {
                    continue;
                };
                if !prefer_latest {
                    return Ok(Some(note));
                }
                let better = match selected.as_ref() {
                    None => true,
                    Some(held) => {
                        let order = date_parse(str_of(&note, "answeredAt")) - date_parse(str_of(held, "answeredAt"));
                        let value = |a: &Json| {
                            a.get("signature")
                                .and_then(|s| s.get("value"))
                                .and_then(Json::as_str)
                                .unwrap_or("")
                                .to_owned()
                        };
                        order > 0.0 || (order == 0.0 && value(&note) > value(held))
                    }
                };
                if better {
                    *selected = Some(note);
                }
            }
            Ok(None)
        };
        match run(&mut selected, names) {
            Ok(Some(first)) => Ok(Some(first)),
            Ok(None) => Ok(selected),
            Err(()) if prefer_latest => Ok(selected),
            Err(()) => Err(()),
        }
    }

    /// `scanSharedAnswers`: verified share copies for open card versions with no local closing
    /// record, read outside the request path.
    pub fn scan_shared_answers(&mut self, cards: &[(Json, String)], local_closed: &HashSet<String>) {
        let Some(public_key) = self.host.trusted_public_key(&self.store_dir) else {
            return;
        };
        let budget = Budget::start(self.timing.read_ms);
        let mut names = None;
        for (doc, sha) in cards {
            if budget.left() == 0 {
                break;
            }
            let version = row_version(doc, sha);
            if str_of(doc, "state") != Some("open") || local_closed.contains(&version) {
                continue;
            }
            if let Ok(Some(answer)) = self.find_shared_closing_answer(doc, sha, &public_key, &budget, &mut names, true)
            {
                self.shared_answers.insert(
                    version.clone(),
                    HistoryEntry {
                        id: version,
                        send_id: None,
                        shared_only: true,
                        answer,
                        card: doc.clone(),
                        share: StepState::with_state("done"),
                        doorbell: StepState::with_state("skipped"),
                    },
                );
            }
        }
    }

    // ---- share writes ----

    /// `superseded`: a later closing record to the same card (written responses always deliver).
    fn superseded(&mut self, id: &str) -> bool {
        let Some(entry) = self.entry(id).cloned() else {
            return false;
        };
        if entry.kind() == "note" || !entry.closing() {
            return false;
        }
        let Ok(all) = self.list() else {
            return false;
        };
        let start = all.iter().position(|e| e.id == id).map_or(0, |at| at + 1);
        let card = str_of(&entry.answer, "cardId");
        all[start..]
            .iter()
            .any(|later| later.id != id && later.closing() && str_of(&later.answer, "cardId") == card)
    }

    fn mark_superseded(&mut self, id: &str) {
        self.record_step(id, StepName::Share, "skipped", SUPERSEDED_DETAIL);
        if self.step_of(id, StepName::Doorbell).state != "done" {
            self.record_step(id, StepName::Doorbell, "skipped", SUPERSEDED_DETAIL);
        }
    }

    /// `writeShareFile` with its ownership-generation `proceed`.
    fn write_share_file(
        &mut self,
        name: &str,
        body: &str,
        limit_ms: u64,
        wanted: Option<&str>,
        guard: Guard<'_>,
    ) -> ShareOutcome {
        if self.writer().is_err() {
            return ShareOutcome::Failed;
        }
        let key = match self.answer_key() {
            Ok(key) => key,
            Err(_) => return ShareOutcome::Failed,
        };
        let epoch = self.epoch;
        let deadline = Instant::now() + Duration::from_millis(limit_ms);
        let v1 = self.reports_root.join("v1");
        let cell = std::cell::RefCell::new(&mut *self);
        let proceed = || {
            let mut engine = cell.borrow_mut();
            if engine.writer().is_err() {
                return false;
            }
            Instant::now() < deadline && engine.epoch == epoch && wanted.is_none_or(|id| !engine.superseded(id))
        };
        let outcome = delivery::write_share_file(&v1, name, body, &key, guard, &proceed);
        drop(cell);
        if outcome == ShareOutcome::Dropped && Instant::now() >= deadline {
            // withDeadline rejected: a deadline, not a drop.
            return ShareOutcome::Failed;
        }
        outcome
    }

    fn write_share(&mut self, id: &str, limit_ms: u64) -> bool {
        let fresh = self.sharing.insert(id.to_owned());
        let done = self.write_share_held(id, limit_ms);
        if fresh {
            self.sharing.remove(id);
        }
        done
    }

    fn write_share_held(&mut self, id: &str, limit_ms: u64) -> bool {
        if self.superseded(id) {
            self.mark_superseded(id);
            return false;
        }
        let Some(entry) = self.entry(id).cloned() else {
            return false;
        };
        let stored = self
            .signed_as_stored
            .get(id)
            .cloned()
            .unwrap_or_else(|| entry.answer.clone());
        let body = delivery::file_body(&stored);
        let name = delivery::share_file_name(&entry.answer, &entry.id);
        let card_id = str_of(&entry.answer, "cardId").unwrap_or_default().to_owned();
        let guard = if entry.closing() {
            Guard::RefuseOverNewer {
                card_id: &card_id,
                card_seq: entry.answer.get("cardSeq").and_then(Json::as_f64).unwrap_or(0.0),
            }
        } else {
            Guard::None
        };
        let outcome = self.write_share_file(&name, &body, limit_ms, Some(id), guard);
        match outcome {
            ShareOutcome::Written => {
                self.record_step(id, StepName::Share, "done", SHARE_DONE);
                true
            }
            ShareOutcome::Newer => {
                self.mark_superseded(id);
                false
            }
            ShareOutcome::Dropped if self.superseded(id) => {
                self.mark_superseded(id);
                false
            }
            ShareOutcome::Unavailable => {
                self.record_step(id, StepName::Share, "failed", SHARE_UNAVAILABLE_DETAIL);
                false
            }
            _ => {
                self.record_step(id, StepName::Share, "failed", SHARE_FAILED);
                false
            }
        }
    }

    fn write_summary(&mut self, send_id: &str, limit_ms: u64) -> bool {
        let fresh = self.summarizing.insert(send_id.to_owned());
        let done = self.write_summary_held(send_id, limit_ms);
        if fresh {
            self.summarizing.remove(send_id);
        }
        done
    }

    fn write_summary_held(&mut self, send_id: &str, limit_ms: u64) -> bool {
        let Some(send) = self
            .held_sends
            .get(send_id)
            .or_else(|| self.sends.get(send_id))
            .cloned()
        else {
            return false;
        };
        let answers: Vec<Json> = match self.list() {
            Ok(all) => send
                .entry_ids
                .iter()
                .filter_map(|id| all.iter().find(|e| &e.id == id).map(|e| e.answer.clone()))
                .collect(),
            Err(_) => Vec::new(),
        };
        let at = iso(self.now());
        let (mut state, mut detail) = ("done", SUMMARY_DONE);
        let refs: Vec<&Json> = answers.iter().collect();
        let unsigned = delivery::send_summary(&send.send_id, &send.at, &send.device, &refs);
        let outcome = match self.answer_key() {
            Ok(key) => {
                let body = delivery::file_body(&sign_document(&unsigned, &key));
                self.write_share_file(
                    &format!("batches/{send_id}.json"),
                    &body,
                    limit_ms,
                    None,
                    Guard::KeepPublished { unsigned: &unsigned },
                )
            }
            Err(_) => ShareOutcome::Failed,
        };
        if !matches!(outcome, ShareOutcome::Written | ShareOutcome::Published) {
            state = "failed";
            detail = SUMMARY_FAILED;
        }
        self.gain_quietly();
        let key = format!("{send_id}:summary");
        let before = self.step_of(send_id, StepName::Summary);
        if state != "done" && (before.state == "done" || self.pending.contains_key(&key)) {
            return true;
        }
        let line =
            json!({"type": "delivery", "id": send_id, "step": "summary", "state": state, "at": at, "detail": detail});
        let value = StepState {
            state: state.into(),
            at: Some(at),
            detail: Some(detail.into()),
            attempts: before.attempts + 1,
        };
        if self.record(&line) {
            self.set_step(send_id, StepName::Summary, value);
        } else if state == "done" {
            self.pending.insert(
                key,
                Pending {
                    line,
                    target: send_id.to_owned(),
                    step: StepName::Summary,
                    value,
                },
            );
        } else if !self.pending.contains_key(&key) {
            self.set_step(send_id, StepName::Summary, value);
        }
        if !self.pending.is_empty() {
            self.flush_completions();
        }
        state == "done"
    }

    // ---- rings ----

    fn doorbell_off(&self) -> bool {
        !self.host.has_doorbell() && std::env::var("AAC_STATUS_DOORBELL").is_ok_and(|v| v == "off")
    }

    /// `ringGroups`: one ring per (send, asker) for the given records whose signed copy is on
    /// the share. Every group is claimed before the first ring. A host that must not hold its
    /// lock across a ring uses [`Self::claim_rings`], [`Self::prepare_ring`],
    /// [`Self::finish_ring`] and [`Self::release_rings`] instead, as this does.
    pub fn ring_groups(&mut self, ids: &[String], budget: Option<&Budget>) {
        let claim = self.claim_rings(ids);
        for group in &claim.groups {
            if budget.is_some_and(|b| b.left() == 0) {
                break;
            }
            match self.prepare_ring(group) {
                Prepared::Ring(ring) => {
                    let result = self.host.ring(&ring.to, &ring.subject, &ring.body, ring.timeout);
                    self.finish_ring(&ring, &result);
                }
                Prepared::Nothing => continue,
                Prepared::NotOwner => break,
            }
        }
        self.release_rings(claim);
    }

    /// The ring pass's selection: skips recorded (no pm-mail name, doorbell off), the rest
    /// grouped by (send, asker) and claimed so no other pass rings them meanwhile.
    pub fn claim_rings(&mut self, ids: &[String]) -> RingClaim {
        let mut groups: IndexMap<String, RingGroupClaim> = IndexMap::new();
        for id in ids {
            let Some(entry) = self.entry(id).cloned() else {
                continue;
            };
            if entry.share.state != "done" || self.ringing.contains(id) {
                continue;
            }
            if entry.doorbell.state == "done" || entry.doorbell.state == "skipped" {
                continue;
            }
            let to = entry
                .card
                .get("replyTo")
                .and_then(|r| r.get("pmMail"))
                .and_then(Json::as_str)
                .filter(|to| !to.is_empty() && delivery::is_pm_mail_name(to))
                .map(str::to_owned);
            let Some(to) = to else {
                self.record_step(id, StepName::Doorbell, "skipped", NO_MAIL_DETAIL);
                continue;
            };
            if self.doorbell_off() {
                self.record_step(id, StepName::Doorbell, "skipped", DOORBELL_OFF_DETAIL);
                continue;
            }
            let key = format!("{}\n{to}", entry.send_id.as_deref().unwrap_or(&entry.id));
            groups
                .entry(key)
                .or_insert_with(|| RingGroupClaim {
                    to: to.clone(),
                    send_id: entry.send_id.clone(),
                    ids: Vec::new(),
                })
                .ids
                .push(id.clone());
        }
        let claim = RingClaim {
            groups: groups.into_values().collect(),
        };
        for group in &claim.groups {
            for id in &group.ids {
                self.ringing.insert(id.clone());
            }
        }
        claim
    }

    /// Before one group's ring: the store confirmed (gaining it reconciles the steps) and what
    /// is already rung left out.
    pub fn prepare_ring(&mut self, group: &RingGroupClaim) -> Prepared {
        if self.writer().is_err() {
            return Prepared::NotOwner;
        }
        let live: Vec<HistoryEntry> = group
            .ids
            .iter()
            .filter_map(|id| self.entry(id).cloned())
            .filter(|e| e.doorbell.state != "done" && e.doorbell.state != "skipped")
            .collect();
        if live.is_empty() {
            return Prepared::Nothing;
        }
        let files: Vec<String> = live
            .iter()
            .map(|e| delivery::share_file_name(&e.answer, &e.id))
            .collect();
        let records: Vec<SentRecord<'_>> = live
            .iter()
            .zip(&files)
            .map(|(e, file)| SentRecord {
                answer: &e.answer,
                card: &e.card,
                file,
            })
            .collect();
        let key_id = match self.known_key_id.clone() {
            Some(id) => id,
            None => self.answer_key().map(|k| k.key_id).unwrap_or_default(),
        };
        Prepared::Ring(Ring {
            to: group.to.clone(),
            subject: delivery::ring_subject(&records),
            body: delivery::ring_body(group.send_id.as_deref(), &records, &key_id),
            timeout: Duration::from_millis(self.timing.ring_ms),
            ids: live.iter().map(|e| e.id.clone()).collect(),
        })
    }

    /// After one ring: each of its records' doorbell step.
    pub fn finish_ring(&mut self, ring: &Ring, result: &RingResult) {
        for id in &ring.ids {
            self.record_step(
                id,
                StepName::Doorbell,
                if result.ok { "done" } else { "failed" },
                &result.detail,
            );
        }
    }

    /// The pass is over: its claims go.
    pub fn release_rings(&mut self, claim: RingClaim) {
        for group in claim.groups {
            for id in group.ids {
                self.ringing.remove(&id);
            }
        }
    }

    /// `retryPending`'s first half: the share writes and summaries still owed. None when
    /// another pass runs or another process holds the store (its history is read again).
    /// The caller rings `rings` within the budget, then calls [`Self::end_retry`].
    pub fn begin_retry(&mut self) -> Option<(Budget, Vec<String>)> {
        if self.retrying {
            return None;
        }
        if self.writer().is_err() {
            let _ = self.ensure_list();
            return None;
        }
        self.retrying = true;
        let budget = Budget::start(self.timing.retry_budget_ms);
        let ids: Vec<String> = self
            .list()
            .map(|all| all.iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default();
        for id in &ids {
            let wanted = |engine: &Self| engine.entry(id).is_some_and(|e| e.share.retryable());
            if !wanted(self) {
                continue;
            }
            let limit = self.timing.share_ms.min(budget.left());
            if wanted(self) && limit > 0 {
                self.write_share(id, limit);
            }
        }
        let send_ids: Vec<String> = self
            .send_list()
            .map(|s| s.into_iter().map(|s| s.send_id).collect())
            .unwrap_or_default();
        for send_id in &send_ids {
            let wanted = |engine: &Self| {
                let step = engine.step_of(send_id, StepName::Summary);
                step.state != "done" && step.attempts < MAX_DELIVERY_ATTEMPTS
            };
            if !wanted(self) {
                continue;
            }
            let limit = self.timing.share_ms.min(budget.left());
            if wanted(self) && limit > 0 {
                self.write_summary(send_id, limit);
            }
        }
        let rings: Vec<String> = self
            .list()
            .map(|all| {
                all.iter()
                    .filter(|e| e.doorbell.retryable())
                    .map(|e| e.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        Some((budget, rings))
    }

    /// `retryPending` is over.
    pub fn end_retry(&mut self) {
        self.retrying = false;
    }

    // ---- sends ----

    fn highest_recorded_seq(&mut self, card_id: &str) -> R<f64> {
        Ok(self
            .list()?
            .iter()
            .filter(|e| str_of(&e.answer, "cardId") == Some(card_id))
            .filter_map(|e| e.answer.get("cardSeq").and_then(Json::as_f64))
            .fold(0.0, f64::max))
    }

    /// `openCard`: Ok(Some) open at exactly this version and unanswered, Ok(None) when not,
    /// Err(()) when the share cannot say in time (`no_answer`).
    fn open_card(
        &mut self,
        id: &str,
        seq: &Json,
        sha256: &str,
        budget: &mut Budget,
    ) -> R<Result<Option<(Json, String)>, ()>> {
        if !is_card_id(id) {
            return Ok(Ok(None));
        }
        let left = budget.left();
        if left == 0 {
            return Ok(Err(()));
        }
        let limit = Duration::from_millis(self.timing.read_ms.min(left));
        let (doc, hash) = match self.host.read_card(id, limit) {
            CardRead::Card { doc, sha256 } => (doc, sha256),
            CardRead::Absent => return Ok(Ok(None)),
            CardRead::Failed => return Ok(Err(())),
            CardRead::TimedOut => {
                budget.spend();
                return Ok(Err(()));
            }
        };
        if str_of(&doc, "state") != Some("open") {
            return Ok(Ok(None));
        }
        if !same_number(doc.get("seq"), Some(seq)) || hash != sha256 {
            return Ok(Ok(None));
        }
        if seq.as_f64().unwrap_or(0.0) < self.highest_recorded_seq(id)? {
            return Ok(Ok(None));
        }
        if self.is_answered(id, seq, sha256)? {
            return Ok(Ok(None));
        }
        if let Some(public_key) = self.host.trusted_public_key(&self.store_dir) {
            let read_budget = Budget::start(self.timing.read_ms.min(budget.left()));
            let mut names = None;
            match self.find_shared_closing_answer(&doc, sha256, &public_key, &read_budget, &mut names, false) {
                Ok(Some(_)) => return Ok(Ok(None)),
                Ok(None) => {}
                Err(()) => {
                    budget.spend();
                    return Ok(Err(()));
                }
            }
        }
        Ok(Ok(Some((doc, hash))))
    }

    /// `answerCard`: store, sign and share one record. Ok(None) when it was not stored.
    #[allow(clippy::too_many_arguments)]
    fn answer_card(
        &mut self,
        card: (Json, String),
        choice: &[String],
        note: &str,
        device: &str,
        via: &str,
        kind: &str,
        send_id: &str,
        budget: &Budget,
        recorded: &mut Vec<String>,
        draft: Option<(String, String)>,
        request: Option<Json>,
    ) -> R<Option<String>> {
        let id = self.host.uuid();
        let (doc, sha256) = card;
        let answered_at = iso(self.now());
        let unsigned = json!({
            "schema": ANSWER_SCHEMA,
            "cardId": doc.get("id").cloned().unwrap_or(Json::Null),
            "cardSeq": doc.get("seq").cloned().unwrap_or(Json::Null),
            "cardSha256": sha256,
            "kind": kind,
            "choice": choice,
            "note": note,
            "via": via,
            "answeredAt": answered_at,
            "answeredBy": "owner",
            "device": device,
        });
        let stored: R<Json> = (|| {
            let key = self.answer_key()?;
            let answer = sign_answer(&unsigned, &key);
            self.ensure_list()?;
            self.writer()?;
            if self.send_epoch.is_some_and(|e| e != self.epoch) {
                return Err(Fail::Other("The store changed hands.".into()));
            }
            let mut line = Map::new();
            line.insert("type".into(), Json::from("answer"));
            line.insert("id".into(), Json::from(id.clone()));
            line.insert("sendId".into(), Json::from(send_id));
            line.insert("at".into(), Json::from(answered_at.clone()));
            line.insert("answer".into(), answer.clone());
            line.insert("card".into(), doc.clone());
            if let Some((entry_id, hash)) = &draft {
                line.insert("entryId".into(), Json::from(entry_id.clone()));
                line.insert("entryHash".into(), Json::from(hash.clone()));
            }
            if let Some(Json::Object(request)) = &request {
                line.extend(request.clone());
            }
            self.append(&Json::Object(line))?;
            Ok(answer)
        })();
        let answer = match stored {
            Ok(answer) => answer,
            Err(Fail::Unclear(_)) => return Err(Fail::Unclear(id)),
            Err(_) => return Ok(None),
        };
        self.written.insert(id.clone());
        let entry = HistoryEntry {
            id: id.clone(),
            send_id: Some(send_id.to_owned()),
            shared_only: false,
            answer,
            card: doc,
            share: StepState::pending(),
            doorbell: StepState::pending(),
        };
        self.ensure_list()?;
        if let Some(all) = self.entries.as_mut() {
            all.push(entry);
        }
        self.ringing.insert(id.clone());
        recorded.push(id.clone());
        if let Some((entry_id, hash)) = &draft {
            let now = self.now();
            self.remember_draft(entry_id, now, hash);
        }
        self.notify();
        let left = budget.left();
        if left > 0 {
            self.write_share(&id, self.timing.share_ms.min(left));
        }
        Ok(Some(id))
    }

    /// `recordEntry`: one entry of a set.
    #[allow(clippy::too_many_arguments)]
    fn record_entry(
        &mut self,
        raw: &Json,
        note_secret: Option<&str>,
        device: &str,
        send_id: &str,
        recorded: &mut Vec<String>,
        budget: &mut Budget,
        request: Option<Json>,
    ) -> R<Json> {
        let named = named_card_id(raw);
        let Some(entry) = parse_answer_entry(raw) else {
            return Ok(refused(named.as_deref(), "invalid_entry", None));
        };
        let card_id = Some(entry.card_id.as_str());
        let Some(note) = normalize_note(entry.note.as_deref()) else {
            return Ok(refused(card_id, "invalid_note", None));
        };
        if !note.is_empty() {
            if let Some(kind) = note_secret {
                return Ok(refused(card_id, "note_secret", Some(kind)));
            }
        }
        self.ensure_list()?;
        let hash = entry.entry_id.as_ref().map(|_| draft_hash(&entry, &note));
        if let Some(held) = entry.entry_id.as_ref().and_then(|id| self.drafts.get(id)) {
            if Some(&held.hash) != hash.as_ref() {
                return Ok(refused(card_id, "invalid_entry", None));
            }
            return Ok(recorded_result(card_id));
        }
        let card = match self.open_card(&entry.card_id, &entry.card_seq, &entry.card_sha256, budget)? {
            Err(()) => return Ok(refused(card_id, "share_unavailable", None)),
            Ok(None) => return Ok(refused(card_id, "card_changed", None)),
            Ok(Some(card)) => card,
        };
        let choice: Vec<Json> = entry.choice.iter().cloned().map(Json::from).collect();
        if !valid_choice(&card.0, &choice) {
            return Ok(refused(card_id, "invalid_choice", None));
        }
        let kind = entry_kind(&card.0, &entry, &note);
        if !RECORD_KINDS.contains(&kind) {
            return Ok(refused(card_id, kind, None));
        }
        if entry.via == "recommended" {
            let recommended = card.0.get("recommendation").filter(|r| truthy(Some(r)));
            let key = recommended.and_then(|r| r.get("key"));
            if kind != "answer"
                || recommended.is_none()
                || entry.choice.len() != 1
                || key != Some(&Json::from(entry.choice[0].clone()))
            {
                return Ok(refused(card_id, "invalid_via", None));
            }
        }
        match self.writer() {
            Ok(()) => {}
            Err(Fail::Busy(_)) => return Ok(refused(card_id, "store_busy", None)),
            Err(_) => return Ok(refused(card_id, "answer_not_saved", None)),
        }
        let draft = entry.entry_id.clone().zip(hash);
        match self.answer_card(
            card,
            &entry.choice,
            &note,
            device,
            &entry.via,
            kind,
            send_id,
            budget,
            recorded,
            draft,
            request,
        ) {
            Ok(Some(_)) => Ok(recorded_result(card_id)),
            Ok(None) => Ok(refused(card_id, "answer_not_saved", None)),
            Err(Fail::Unclear(_)) => Ok(refused(card_id, "answer_save_unclear", None)),
            Err(error) => Err(error),
        }
    }

    fn replay_of(&mut self, client: &str, entries_hash: &str) -> R<Option<(Vec<Json>, bool)>> {
        self.ensure_list()?;
        let now = self.now();
        let Some(held) = self.replays.get(client) else {
            return Ok(None);
        };
        if now - held.at >= REMEMBER_SEND_MS {
            return Ok(None);
        }
        Ok(match &held.entries_hash {
            Some(hash) if hash != entries_hash => None,
            _ => Some((held.results.clone(), held.partial)),
        })
    }

    /// `submitSet`: each entry validated and recorded on its own, then the signed send summary.
    /// The caller rings the records next with [`Self::ring_groups`]. `note_secrets[i]` is the
    /// admission's secret finding for entry `i`'s note (the TypeScript filter), if any.
    pub fn submit_set(
        &mut self,
        entries: &[Json],
        device: &str,
        client_send_id: Option<&str>,
        note_secrets: &[Option<String>],
    ) -> R<Submitted> {
        let send_id = self.host.uuid();
        let mut recorded: Vec<String> = Vec::new();
        let entries_hash = client_send_id.map(|_| content_hash(&Json::Array(entries.to_vec())));
        let outcome = self.submit_inner(
            entries,
            device,
            client_send_id,
            note_secrets,
            &send_id,
            &entries_hash,
            &mut recorded,
        );
        self.send_epoch = None;
        for id in &recorded {
            self.ringing.remove(id);
        }
        outcome.map(|results| Submitted { results, recorded })
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_inner(
        &mut self,
        entries: &[Json],
        device: &str,
        client_send_id: Option<&str>,
        note_secrets: &[Option<String>],
        send_id: &str,
        entries_hash: &Option<String>,
        recorded: &mut Vec<String>,
    ) -> R<Vec<Json>> {
        match self.writer() {
            Ok(()) => self.send_epoch = Some(self.epoch),
            Err(error) => {
                let code = if matches!(error, Fail::Busy(_)) {
                    "store_busy"
                } else {
                    "answer_not_saved"
                };
                return Ok(entries
                    .iter()
                    .map(|raw| refused(named_card_id(raw).as_deref(), code, None))
                    .collect());
            }
        }
        let replay = match (client_send_id, entries_hash) {
            (Some(client), Some(hash)) => self.replay_of(client, hash)?,
            _ => None,
        };
        if let Some((results, partial)) = &replay {
            if !partial || results.len() >= entries.len() {
                return Ok(results.iter().take(entries.len()).cloned().collect());
            }
        }
        let mut out: Vec<Json> = replay.map(|(results, _)| results).unwrap_or_default();
        let mut budget = Budget::start(self.timing.send_budget_ms);
        let mut last_stored: isize = out.len() as isize - 1;
        for index in out.len()..entries.len() {
            let raw = &entries[index];
            let before = recorded.len();
            let request = match (client_send_id, entries_hash) {
                (Some(client), Some(hash)) => Some(json!({
                    "clientSendId": client,
                    "entriesHash": hash,
                    "entryIndex": index,
                    "priorResults": out[(last_stored + 1) as usize..].to_vec(),
                })),
                _ => None,
            };
            let secret = note_secrets.get(index).and_then(|s| s.as_deref());
            match self.record_entry(raw, secret, device, send_id, recorded, &mut budget, request) {
                Ok(result) => out.push(result),
                Err(_) => {
                    let card_id = named_card_id(raw);
                    out.push(if recorded.len() > before {
                        recorded_result(card_id.as_deref())
                    } else {
                        refused(card_id.as_deref(), "answer_not_saved", None)
                    });
                }
            }
            if recorded.len() > before {
                last_stored = index as isize;
            }
        }
        if !recorded.is_empty() {
            let at = self
                .entry(&recorded[0])
                .and_then(|e| str_of(&e.answer, "answeredAt"))
                .unwrap_or_default()
                .to_owned();
            let send = SendRecord {
                send_id: send_id.to_owned(),
                at: at.clone(),
                device: device.to_owned(),
                entry_ids: recorded.clone(),
                summary: StepState::pending(),
            };
            let mut line = Map::new();
            line.insert("type".into(), Json::from("send"));
            line.insert("sendId".into(), Json::from(send_id));
            line.insert("at".into(), Json::from(at));
            line.insert("device".into(), Json::from(device));
            line.insert("entryIds".into(), json!(send.entry_ids));
            if let Some(client) = client_send_id {
                line.insert("clientSendId".into(), Json::from(client));
                line.insert("results".into(), Json::Array(out.clone()));
                line.insert(
                    "entriesHash".into(),
                    entries_hash.clone().map_or(Json::Null, Json::from),
                );
            }
            self.record(&Json::Object(line));
            self.own_sends.insert(send_id.to_owned());
            self.sends.insert(send_id.to_owned(), send);
            if let Some(client) = client_send_id {
                let now = self.now();
                self.remember(client, now, out.clone(), entries_hash.clone(), false);
            }
            let left = budget.left();
            if left > 0 {
                self.write_summary(send_id, self.timing.share_ms.min(left));
            }
        }
        Ok(out)
    }

    /// `submitSet` and its ring, for a host that does both in one turn.
    pub fn submit_and_ring(
        &mut self,
        entries: &[Json],
        device: &str,
        client_send_id: Option<&str>,
        note_secrets: &[Option<String>],
    ) -> R<Vec<Json>> {
        let submitted = self.submit_set(entries, device, client_send_id, note_secrets)?;
        if !submitted.recorded.is_empty() {
            self.ring_groups(&submitted.recorded, None);
        }
        Ok(submitted.results)
    }

    /// `retryPending`: every share write, send summary and ring that has not succeeded and has
    /// attempts left. Another process's store: its history is read again if it changed.
    pub fn retry_pending(&mut self) {
        let Some((budget, rings)) = self.begin_retry() else {
            return;
        };
        self.ring_groups(&rings, Some(&budget));
        self.end_retry();
    }
}

fn row_version(doc: &Json, sha256: &str) -> String {
    card_version(
        str_of(doc, "id").unwrap_or_default(),
        doc.get("seq").unwrap_or(&Json::Null),
        sha256,
    )
}

fn is_entry_result(value: &Json) -> bool {
    let nullable = |key: &str| matches!(value.get(key), Some(Json::Null | Json::String(_)));
    value.is_object()
        && nullable("cardId")
        && value.get("outcome").is_some_and(Json::is_string)
        && nullable("code")
        && nullable("error")
}

/// `closingCardVersions` over history entries.
pub fn closing_card_versions(entries: &[HistoryEntry]) -> HashSet<String> {
    entries
        .iter()
        .filter(|e| e.closing())
        .map(|e| {
            card_version(
                str_of(&e.answer, "cardId").unwrap_or_default(),
                e.answer.get("cardSeq").unwrap_or(&Json::Null),
                str_of(&e.answer, "cardSha256").unwrap_or_default(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_stringify_matches_node() {
        let value = json!({"a": "q\"\\\u{1}\u{7f}\u{2028}é", "n": [1, -2, 0.5, 1e21], "b": null});
        assert_eq!(
            js_stringify(&value),
            "{\"a\":\"q\\\"\\\\\\u0001\u{7f}\u{2028}é\",\"n\":[1,-2,0.5,1e+21],\"b\":null}"
        );
    }

    #[test]
    fn uuids_are_version_four() {
        let id = random_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }

    #[test]
    fn notes_trim_and_refuse_controls() {
        assert_eq!(normalize_note(Some("  hi \n")).as_deref(), Some("hi"));
        assert_eq!(normalize_note(None).as_deref(), Some(""));
        assert_eq!(normalize_note(Some("a\u{7}b")), None);
        assert_eq!(normalize_note(Some(&"x".repeat(4001))), None);
    }
}
