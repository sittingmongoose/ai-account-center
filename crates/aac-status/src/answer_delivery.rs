//! G16: the Status answer delivery in Rust, ported from `src/web-server/status/answers.ts`
//! (`ringGroups`, `writeShareFile`, `writeAtomic`, `refuseOverNewer`, `keepPublishedSummary`,
//! `writeShareHeld`, `writeSummaryHeld`, `retryPending`) and `answer-delivery.ts` (the ring text,
//! the send summary and the pm-mail doorbell). Signing is `answer_signing` (lane C).
//!
//! The engine holds no journal: it plans and performs the share writes, the summaries and the
//! rings for the entries it is given, and names each step's outcome in the TS words so the
//! journal's owner records it exactly as `recordStep` does. A share write re-checks `proceed`
//! (deadline, ownership, still wanted) between every step and just before its rename, as the TS
//! writer does, so a write that outlives its deadline or its writer never publishes.
use crate::answer_signing::{AnswerKey, canonical_json, sign_document, verify_signed_document};
use serde_json::{Map, Value as Json, json};
use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// The doorbell's pm-mail sender name.
pub const DOORBELL_SENDER: &str = "aac-answers";
/// The signed send summary's schema.
pub const SEND_SCHEMA: &str = "agent-reports/send@1";
/// The signed answer's schema.
pub const ANSWER_SCHEMA: &str = "agent-reports/answer@1";
/// A card with no pm-mail name: delivery is complete without a ring.
pub const NO_MAIL_DETAIL: &str = "No mail name; the agent reads its answers file.";
/// `AAC_STATUS_DOORBELL=off` on this server.
pub const DOORBELL_OFF_DETAIL: &str = "The pm-mail doorbell is turned off on this server.";
/// A newer closing answer to the same card reached the share first.
pub const SUPERSEDED_DETAIL: &str =
    "Not delivered: a newer answer to this card replaced this one before it reached the share.";
/// The share's `v1` folder is not there.
pub const SHARE_UNAVAILABLE_DETAIL: &str = "The reports share is not available.";
/// Attempts per step before delivery gives up.
pub const MAX_DELIVERY_ATTEMPTS: u64 = 20;

const SHARE_DONE: &str = "Signed copy written to the share.";
const SHARE_FAILED: &str = "The signed copy could not be written to the share.";
const SUMMARY_DONE: &str = "Signed send summary written to the share.";
const SUMMARY_FAILED: &str = "The signed send summary could not be written to the share.";
const CLOSING_KINDS: [&str; 3] = ["answer", "note", "done"];

// ---- text ----------------------------------------------------------------------------------

/// ECMAScript `\s` (WhiteSpace and LineTerminator).
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `oneLine`: whitespace and control runs become one space; the ends are trimmed.
pub fn one_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut run = false;
    for c in value.chars() {
        let u = c as u32;
        if js_space(c) || u <= 0x1f || (0x7f..=0x9f).contains(&u) {
            if !run {
                out.push(' ');
                run = true;
            }
        } else {
            out.push(c);
            run = false;
        }
    }
    out.trim_matches(js_space).to_owned()
}

/// One record of a send, as the ring and the summary need it.
#[derive(Clone, Copy, Debug)]
pub struct SentRecord<'a> {
    /// The signed answer as stored.
    pub answer: &'a Json,
    /// The card version answered.
    pub card: &'a Json,
    /// The record's signed file under `<reports>/v1/answers/`.
    pub file: &'a str,
}

fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    value.get(key).and_then(Json::as_str).unwrap_or("")
}

/// A JSON number as JavaScript writes it in a template string.
fn js_number(value: Option<&Json>) -> String {
    match value {
        Some(Json::Number(n)) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 => (f as i64).to_string(),
            (None, Some(f)) => f.to_string(),
            _ => "NaN".to_owned(),
        },
        Some(Json::String(s)) => s.clone(),
        Some(Json::Bool(b)) => b.to_string(),
        Some(Json::Null) => "null".to_owned(),
        _ => "undefined".to_owned(),
    }
}

fn batches_of(records: &[SentRecord<'_>]) -> Vec<String> {
    let set: BTreeSet<String> = records
        .iter()
        .filter_map(|r| r.card.get("batch").and_then(Json::as_str))
        .filter(|batch| !batch.is_empty())
        .map(one_line)
        .collect();
    // JavaScript's default sort compares UTF-16 code units; for these names that is code point order.
    let mut out: Vec<String> = set.into_iter().collect();
    out.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    out
}

fn count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "answer" } else { "answers" })
}

/// `ringSubject`.
pub fn ring_subject(records: &[SentRecord<'_>]) -> String {
    let batches = batches_of(records);
    if batches.is_empty() {
        format!("{} from the owner", count(records.len()))
    } else {
        format!("{} from the owner ({})", count(records.len()), batches.join(", "))
    }
}

fn kind_words(kind: &str) -> String {
    match kind {
        "answer" => "answer".to_owned(),
        "note" => {
            "written response (closes this card; the owner may be asking a question or proposing another solution)"
                .to_owned()
        }
        "object" => "objection (the card stays open; reply on the card or withdraw it)".to_owned(),
        "done" => "done".to_owned(),
        other => other.to_owned(),
    }
}

fn choice_text(record: &SentRecord<'_>) -> String {
    let choice: Vec<&str> = record
        .answer
        .get("choice")
        .and_then(Json::as_array)
        .map(|c| c.iter().filter_map(Json::as_str).collect())
        .unwrap_or_default();
    if choice.is_empty() {
        return "none".to_owned();
    }
    let options = record.card.get("options").and_then(Json::as_array);
    choice
        .iter()
        .map(|key| {
            let label = options
                .and_then(|o| o.iter().find(|opt| opt.get("key").and_then(Json::as_str) == Some(*key)))
                .and_then(|opt| opt.get("label").and_then(Json::as_str))
                .unwrap_or("unknown option");
            format!("{} ({})", one_line(key), one_line(label))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ringBody`: the ring for one asker.
pub fn ring_body(send_id: Option<&str>, records: &[SentRecord<'_>], key_id: &str) -> String {
    let batches = batches_of(records);
    let mut lines = vec![format!(
        "The owner sent {} on the AI Account Center Status tab.",
        count(records.len())
    )];
    if !batches.is_empty() {
        lines.push(format!("Batch: {}", batches.join(", ")));
    }
    if let Some(id) = send_id {
        lines.push(format!("Send: {id}"));
    }
    lines.push(format!(
        "Cards: {}",
        records
            .iter()
            .map(|r| one_line(text(r.answer, "cardId")))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    lines.push(String::new());
    for record in records {
        let answer = record.answer;
        let note = text(answer, "note");
        let words = if note.is_empty() {
            "(none)".to_owned()
        } else {
            note.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\n    ")
        };
        lines.push(format!(
            "Card {} (seq {})",
            one_line(text(answer, "cardId")),
            js_number(answer.get("cardSeq"))
        ));
        lines.push(format!("  Kind: {}", kind_words(text(answer, "kind"))));
        lines.push(format!("  Choice: {}", choice_text(record)));
        lines.push(format!("  Words: {words}"));
        lines.push(format!("  Time (UTC): {}", text(answer, "answeredAt")));
        lines.push(format!("  Card sha256: {}", text(answer, "cardSha256")));
        lines.push(format!(
            "  Signed file: agent-reports/v1/answers/{}",
            one_line(record.file)
        ));
        lines.push(String::new());
    }
    let summary = send_id
        .map(|id| format!(", with the whole send in agent-reports/v1/answers/batches/{id}.json"))
        .unwrap_or_default();
    lines.push(format!(
        "The signed files are the authority{summary} (key agent-reports/v1/keys/{key_id}.pub). This mail is only a doorbell."
    ));
    lines.join("\n")
}

/// `sendSummary`: the unsigned summary, one entry per record in the order sent.
pub fn send_summary(send_id: &str, at: &str, device: &str, answers: &[&Json]) -> Json {
    let entries: Vec<Json> = answers
        .iter()
        .map(|a| {
            json!({
                "cardId": a.get("cardId").cloned().unwrap_or(Json::Null),
                "seq": a.get("cardSeq").cloned().unwrap_or(Json::Null),
                "cardSha256": a.get("cardSha256").cloned().unwrap_or(Json::Null),
                "kind": a.get("kind").cloned().unwrap_or(Json::Null),
                "choice": a.get("choice").cloned().unwrap_or(Json::Null),
                "note": a.get("note").cloned().unwrap_or(Json::Null),
            })
        })
        .collect();
    json!({
        "schema": SEND_SCHEMA,
        "sendId": send_id,
        "at": at,
        "answeredBy": "owner",
        "device": device,
        "entries": entries,
    })
}

/// `shareFileName`: answers and dones replace `<card>.json`; notes and objections keep their own.
pub fn share_file_name(answer: &Json, record_id: &str) -> String {
    let hex: String = record_id.chars().filter(|c| *c != '-').take(12).collect();
    let card = text(answer, "cardId");
    match text(answer, "kind") {
        "note" => format!("{card}.note-{hex}.json"),
        "object" => format!("{card}.object-{hex}.json"),
        _ => format!("{card}.json"),
    }
}

/// `JSON.stringify(value, null, 2) + '\n'`: a share file's bytes.
pub fn file_body(value: &Json) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_default();
    text.push('\n');
    text
}

/// `isPmMailName`.
pub fn is_pm_mail_name(value: &str) -> bool {
    let b = value.as_bytes();
    value != "all"
        && !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

// ---- entries and the ring plan -------------------------------------------------------------

/// One delivery step's state, as the journal holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// `pending`, `done`, `failed` or `skipped`.
    pub state: String,
    /// Attempts so far.
    pub attempts: u64,
}

impl Step {
    /// From the journal's `{state, attempts}` object.
    pub fn from_json(value: &Json) -> Self {
        Self {
            state: text(value, "state").to_owned(),
            attempts: value.get("attempts").and_then(Json::as_u64).unwrap_or(0),
        }
    }
    fn retryable(&self) -> bool {
        (self.state == "pending" || self.state == "failed") && self.attempts < MAX_DELIVERY_ATTEMPTS
    }
}

/// One journal entry as delivery needs it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The record id.
    pub id: String,
    /// The send it came in.
    pub send_id: Option<String>,
    /// The signed answer as stored.
    pub answer: Json,
    /// The card version answered.
    pub card: Json,
    /// The share step.
    pub share: Step,
    /// The doorbell step.
    pub doorbell: Step,
}

/// One ring: an asker and the records of one send that belong to them.
#[derive(Clone, Debug, PartialEq)]
pub struct RingGroup {
    /// The pm-mail name.
    pub to: String,
    /// The send, or null for a record from before sends.
    pub send_id: Option<String>,
    /// The entry ids, in journal order.
    pub ids: Vec<String>,
}

/// What a ring pass does before any ring: the steps it skips (id, detail) and the groups it rings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RingPlan {
    /// Doorbell steps recorded `skipped` with this detail.
    pub skipped: Vec<(String, String)>,
    /// One ring per (send, asker), in first-appearance order.
    pub groups: Vec<RingGroup>,
}

/// `ringGroups`' selection: shared, not ringing now, not rung or skipped; no pm-mail name or the
/// doorbell turned off is a skip; the rest group by send (or record) and asker.
pub fn ring_plan(entries: &[Entry], ringing: &HashSet<String>, doorbell_off: bool) -> RingPlan {
    let mut plan = RingPlan::default();
    for entry in entries {
        if entry.share.state != "done" || ringing.contains(&entry.id) {
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
            .filter(|name| is_pm_mail_name(name));
        let Some(to) = to else {
            plan.skipped.push((entry.id.clone(), NO_MAIL_DETAIL.to_owned()));
            continue;
        };
        if doorbell_off {
            plan.skipped.push((entry.id.clone(), DOORBELL_OFF_DETAIL.to_owned()));
            continue;
        }
        let key_send = entry.send_id.clone().unwrap_or_else(|| entry.id.clone());
        match plan
            .groups
            .iter_mut()
            .find(|g| g.to == to && g.send_id.clone().unwrap_or_else(|| g.ids[0].clone()) == key_send)
        {
            Some(group) => group.ids.push(entry.id.clone()),
            None => plan.groups.push(RingGroup {
                to: to.to_owned(),
                send_id: entry.send_id.clone(),
                ids: vec![entry.id.clone()],
            }),
        }
    }
    plan
}

/// The ring's subject and body for one group.
pub fn ring_text(group: &RingGroup, entries: &[Entry], key_id: &str) -> (String, String) {
    let files: Vec<(String, &Entry)> = group
        .ids
        .iter()
        .filter_map(|id| entries.iter().find(|e| &e.id == id))
        .map(|e| (share_file_name(&e.answer, &e.id), e))
        .collect();
    let records: Vec<SentRecord<'_>> = files
        .iter()
        .map(|(file, e)| SentRecord {
            answer: &e.answer,
            card: &e.card,
            file,
        })
        .collect();
    (
        ring_subject(&records),
        ring_body(group.send_id.as_deref(), &records, key_id),
    )
}

/// What one retry pass works on (`retryPending`): shares and summaries still owed and under the
/// attempt cap, then the rings still owed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetryPlan {
    /// Entries whose signed copy is retried.
    pub shares: Vec<String>,
    /// Sends whose summary is retried.
    pub summaries: Vec<String>,
    /// Entries offered to the ring pass.
    pub rings: Vec<String>,
}

/// One send as delivery needs it.
#[derive(Clone, Debug)]
pub struct Send {
    /// The send id.
    pub send_id: String,
    /// When it was sent.
    pub at: String,
    /// The device that sent it.
    pub device: String,
    /// Its records, in the order sent.
    pub entry_ids: Vec<String>,
    /// The summary step.
    pub summary: Step,
}

/// `retryPending`'s selection.
pub fn retry_plan(entries: &[Entry], sends: &[Send]) -> RetryPlan {
    RetryPlan {
        shares: entries
            .iter()
            .filter(|e| e.share.retryable())
            .map(|e| e.id.clone())
            .collect(),
        summaries: sends
            .iter()
            .filter(|s| s.summary.state != "done" && s.summary.attempts < MAX_DELIVERY_ATTEMPTS)
            .map(|s| s.send_id.clone())
            .collect(),
        rings: entries
            .iter()
            .filter(|e| e.doorbell.retryable())
            .map(|e| e.id.clone())
            .collect(),
    }
}

/// `superseded`: a later closing record to the same card (written responses always deliver).
pub fn superseded(entries: &[Entry], id: &str) -> bool {
    let Some(at) = entries.iter().position(|e| e.id == id) else {
        return false;
    };
    let entry = &entries[at];
    let kind = text(&entry.answer, "kind");
    if kind == "note" || !CLOSING_KINDS.contains(&kind) {
        return false;
    }
    let card = text(&entry.answer, "cardId");
    entries[at + 1..].iter().any(|later| {
        later.id != entry.id
            && CLOSING_KINDS.contains(&text(&later.answer, "kind"))
            && text(&later.answer, "cardId") == card
    })
}

// ---- the doorbell --------------------------------------------------------------------------

/// A ring's result, in the words the journal records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingResult {
    /// Rang.
    pub ok: bool,
    /// The step detail.
    pub detail: String,
}

/// `findExecutable('pm-mail', ['/mnt/Cursor/Inbox/pm-mail'])` over `PATH` (plus the fallbacks).
pub fn find_pm_mail(path: Option<&std::ffi::OsStr>) -> PathBuf {
    let mut candidates: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).map(|dir| dir.join("pm-mail")).collect())
        .unwrap_or_default();
    candidates.push(PathBuf::from("/mnt/Cursor/Inbox/pm-mail"));
    candidates
        .into_iter()
        .find(|candidate| executable(candidate))
        .unwrap_or_else(|| PathBuf::from("pm-mail"))
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `pmMailDoorbell`: `pm-mail --as aac-answers send <to> <subject>` with the body on stdin, a
/// timeout and a 64 KiB output bound; the session and thread bindings never reach the child.
pub fn pm_mail_ring(bin: &Path, to: &str, subject: &str, body: &str, timeout: Duration) -> RingResult {
    let failure = |detail: &str| RingResult {
        ok: false,
        detail: detail.to_owned(),
    };
    let mut command = Command::new(bin);
    command
        .args(["--as", DOORBELL_SENDER, "send", to, subject])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("T3CODE_THREAD_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return failure("pm-mail is not installed here.");
        }
        Err(_) => return failure("pm-mail failed."),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let body = body.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&body);
        });
    }
    // Drain both pipes (bounded) so a chatty child cannot block on a full pipe.
    let drain = |pipe: Option<Box<dyn std::io::Read + std::marker::Send>>| {
        pipe.map(|mut p| {
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = (&mut p).take(64 * 1024 + 1).read_to_end(&mut sink);
            })
        })
    };
    let _out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + std::marker::Send>),
    );
    let _err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + std::marker::Send>),
    );
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    RingResult {
                        ok: true,
                        detail: format!("Rang {to} through pm-mail."),
                    }
                } else {
                    failure("pm-mail failed.")
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return failure("pm-mail did not answer in time.");
            }
            Err(_) => return failure("pm-mail failed."),
        }
    }
}

// ---- share writes --------------------------------------------------------------------------

/// How a share write ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareOutcome {
    /// Renamed into place.
    Written,
    /// A newer closing answer of ours to the card is there (`ShareHasNewerError`).
    Newer,
    /// This send's summary is already published (`SummaryPublishedError`).
    Published,
    /// `proceed` said no (deadline, ownership or no longer wanted): nothing was published.
    Dropped,
    /// The share's `v1` is not a folder (`ShareUnavailableError`).
    Unavailable,
    /// Any other failure; retried later.
    Failed,
}

/// The last guard before the rename.
pub enum Guard<'a> {
    /// None.
    None,
    /// `refuseOverNewer` for a closing record of this card at this seq.
    RefuseOverNewer {
        /// The card id.
        card_id: &'a str,
        /// The record's card seq.
        card_seq: f64,
    },
    /// `keepPublishedSummary` for this unsigned summary.
    KeepPublished {
        /// The unsigned summary.
        unsigned: &'a Json,
    },
}

/// `readRegularFile` outcomes the guards branch on.
enum ShareRead {
    Bytes(Vec<u8>),
    Absent,
    NotPlain,
    Denied,
    Other,
}

fn read_regular(path: &Path, cap: u64) -> ShareRead {
    use aac_files::read::{Read, ReadPolicy, read};
    match read(path, &ReadPolicy::capped(cap)) {
        Read::Bytes(bytes, _) => ShareRead::Bytes(bytes),
        Read::Absent => ShareRead::Absent,
        Read::Link | Read::NotRegular => ShareRead::NotPlain,
        // The TS reader returns the first cap + 1 bytes: never this signer's file nor equal to
        // the body, so a guard treats it as foreign content.
        Read::TooLarge => ShareRead::Bytes(Vec::new()),
        Read::Unreadable(std::io::ErrorKind::PermissionDenied) => ShareRead::Denied,
        _ => ShareRead::Other,
    }
}

/// `mkdirBelow`: the folders from `v1` down to `dir`, one level at a time, never `v1` itself.
fn mkdir_below(v1: &Path, dir: &Path, proceed: &dyn Fn() -> bool) -> Result<(), ShareOutcome> {
    let relative = dir.strip_prefix(v1).map_err(|_| ShareOutcome::Failed)?;
    let mut current = v1.to_path_buf();
    for part in relative.components() {
        current.push(part);
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ShareOutcome::Failed),
        }
        if !proceed() {
            return Err(ShareOutcome::Dropped);
        }
    }
    Ok(())
}

fn temp_suffix() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 6];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `writeAtomic`: a `.<name>.tmp-<hex>` file in the same folder (0644, exclusive), written and
/// synced, the guard, one last `proceed`, then the rename; the temp file goes on any failure.
fn write_atomic(
    v1: &Path,
    file: &Path,
    body: &[u8],
    proceed: &dyn Fn() -> bool,
    before_rename: &dyn Fn(&Path) -> Result<(), ShareOutcome>,
) -> Result<(), ShareOutcome> {
    let check = || if proceed() { Ok(()) } else { Err(ShareOutcome::Dropped) };
    let parent = file.parent().ok_or(ShareOutcome::Failed)?;
    mkdir_below(v1, parent, proceed)?;
    check()?;
    let name = file.file_name().ok_or(ShareOutcome::Failed)?.to_string_lossy();
    let temp = parent.join(format!(".{name}.tmp-{}", temp_suffix()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut handle = options.open(&temp).map_err(|_| ShareOutcome::Failed)?;
        check()?;
        handle.write_all(body).map_err(|_| ShareOutcome::Failed)?;
        check()?;
        handle.sync_all().map_err(|_| ShareOutcome::Failed)?;
        check()?;
        drop(handle);
        check()?;
        before_rename(file)?;
        // No gap between the last ownership check and publication.
        check()?;
        fs::rename(&temp, file).map_err(|_| ShareOutcome::Failed)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn refuse_over_newer(file: &Path, card_id: &str, card_seq: f64, key: &AnswerKey) -> Result<(), ShareOutcome> {
    let bytes = match read_regular(file, 64 * 1024) {
        ShareRead::Bytes(bytes) => bytes,
        ShareRead::Absent | ShareRead::NotPlain | ShareRead::Denied => return Ok(()),
        ShareRead::Other => return Err(ShareOutcome::Failed),
    };
    let Ok(held) = verify_signed_document(&bytes, &key.public_key_pem, ANSWER_SCHEMA) else {
        return Ok(());
    };
    let kind = held.get("kind").and_then(Json::as_str).unwrap_or("answer");
    let seq = held.get("cardSeq").and_then(Json::as_f64);
    if held.get("cardId").and_then(Json::as_str) == Some(card_id)
        && CLOSING_KINDS.contains(&kind)
        && seq.is_some_and(|s| s > card_seq)
    {
        return Err(ShareOutcome::Newer);
    }
    Ok(())
}

fn keep_published_summary(file: &Path, body: &str, unsigned: &Json, key: &AnswerKey) -> Result<(), ShareOutcome> {
    let bytes = match read_regular(file, body.len() as u64 + 64 * 1024) {
        ShareRead::Bytes(bytes) => bytes,
        ShareRead::NotPlain => return Ok(()),
        ShareRead::Absent => {
            // Absent only once a fresh listing of its folder does not show it.
            let name = file.file_name().map(|n| n.to_os_string());
            let listed = file
                .parent()
                .and_then(|dir| fs::read_dir(dir).ok())
                .map(|entries| entries.flatten().any(|e| Some(e.file_name()) == name));
            return match listed {
                Some(false) => Ok(()),
                _ => Err(ShareOutcome::Failed),
            };
        }
        ShareRead::Denied | ShareRead::Other => return Err(ShareOutcome::Failed),
    };
    if bytes == body.as_bytes() {
        return Err(ShareOutcome::Published);
    }
    let Ok(held) = verify_signed_document(&bytes, &key.public_key_pem, SEND_SCHEMA) else {
        return Ok(());
    };
    if !held.get("at").is_some_and(Json::is_string) {
        return Ok(());
    }
    let strip = |value: &Json, keys: &[&str]| -> Json {
        let mut map: Map<String, Json> = value.as_object().cloned().unwrap_or_default();
        for key in keys {
            map.remove(*key);
        }
        Json::Object(map)
    };
    if canonical_json(&strip(&held, &["signature", "at"])) == canonical_json(&strip(unsigned, &["at"])) {
        return Err(ShareOutcome::Published);
    }
    Ok(())
}

/// `writeShareFile`: both public key files made current, then `answers/<name>` written atomically
/// behind `guard`. `v1` must already be a folder.
pub fn write_share_file(
    v1: &Path,
    name: &str,
    body: &str,
    key: &AnswerKey,
    guard: Guard<'_>,
    proceed: &dyn Fn() -> bool,
) -> ShareOutcome {
    if !proceed() {
        return ShareOutcome::Dropped;
    }
    if !fs::metadata(v1).is_ok_and(|m| m.is_dir()) {
        return ShareOutcome::Unavailable;
    }
    if !proceed() {
        return ShareOutcome::Dropped;
    }
    let run = || -> Result<(), ShareOutcome> {
        // Verifiers try keys/<keyId>.pub first, so a rotation keeps old answers checkable.
        for file in [
            v1.join("keys").join("aac-answers.pub"),
            v1.join("keys").join(format!("{}.pub", key.key_id)),
        ] {
            let current = match read_regular(&file, 64 * 1024) {
                ShareRead::Bytes(bytes) => String::from_utf8(bytes).unwrap_or_default(),
                _ => String::new(),
            };
            if !proceed() {
                return Err(ShareOutcome::Dropped);
            }
            if current != key.public_key_pem {
                write_atomic(v1, &file, key.public_key_pem.as_bytes(), proceed, &|_| Ok(()))?;
            }
            if !proceed() {
                return Err(ShareOutcome::Dropped);
            }
        }
        let target = v1.join("answers").join(name);
        write_atomic(v1, &target, body.as_bytes(), proceed, &|file| match &guard {
            Guard::None => Ok(()),
            Guard::RefuseOverNewer { card_id, card_seq } => refuse_over_newer(file, card_id, *card_seq, key),
            Guard::KeepPublished { unsigned } => keep_published_summary(file, body, unsigned, key),
        })
    };
    match run() {
        Ok(()) => ShareOutcome::Written,
        Err(outcome) => outcome,
    }
}

/// A step to record, in the journal's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRecord {
    /// `share`, `doorbell` or `summary`.
    pub step: &'static str,
    /// `done`, `failed` or `skipped`.
    pub state: &'static str,
    /// The detail.
    pub detail: String,
}

/// `writeShareHeld`: the steps a share write records. A superseded record (before the write, by
/// the guard, or dropped while a newer one arrived) skips its share and its owed ring.
pub fn share_steps(outcome: &ShareOutcome, superseded_now: bool, doorbell_done: bool) -> Vec<StepRecord> {
    let skipped = || {
        let mut steps = vec![StepRecord {
            step: "share",
            state: "skipped",
            detail: SUPERSEDED_DETAIL.to_owned(),
        }];
        if !doorbell_done {
            steps.push(StepRecord {
                step: "doorbell",
                state: "skipped",
                detail: SUPERSEDED_DETAIL.to_owned(),
            });
        }
        steps
    };
    match outcome {
        ShareOutcome::Written => vec![StepRecord {
            step: "share",
            state: "done",
            detail: SHARE_DONE.to_owned(),
        }],
        ShareOutcome::Newer => skipped(),
        ShareOutcome::Dropped if superseded_now => skipped(),
        ShareOutcome::Unavailable => vec![StepRecord {
            step: "share",
            state: "failed",
            detail: SHARE_UNAVAILABLE_DETAIL.to_owned(),
        }],
        _ => vec![StepRecord {
            step: "share",
            state: "failed",
            detail: SHARE_FAILED.to_owned(),
        }],
    }
}

/// `writeSummaryHeld`'s step: a published or written summary is done.
pub fn summary_step(outcome: &ShareOutcome) -> StepRecord {
    match outcome {
        ShareOutcome::Written | ShareOutcome::Published => StepRecord {
            step: "summary",
            state: "done",
            detail: SUMMARY_DONE.to_owned(),
        },
        _ => StepRecord {
            step: "summary",
            state: "failed",
            detail: SUMMARY_FAILED.to_owned(),
        },
    }
}

/// One record's signed copy (`writeShareHeld`): skipped when superseded, else written behind the
/// newer-answer guard for closing kinds.
pub fn deliver_share(
    v1: &Path,
    entries: &[Entry],
    id: &str,
    key: &AnswerKey,
    proceed: &dyn Fn() -> bool,
) -> Vec<StepRecord> {
    let Some(entry) = entries.iter().find(|e| e.id == id) else {
        return Vec::new();
    };
    let doorbell_done = entry.doorbell.state == "done";
    if superseded(entries, id) {
        return share_steps(&ShareOutcome::Newer, true, doorbell_done);
    }
    let kind = text(&entry.answer, "kind");
    let guard = if CLOSING_KINDS.contains(&kind) {
        Guard::RefuseOverNewer {
            card_id: text(&entry.answer, "cardId"),
            card_seq: entry.answer.get("cardSeq").and_then(Json::as_f64).unwrap_or(0.0),
        }
    } else {
        Guard::None
    };
    let outcome = write_share_file(
        v1,
        &share_file_name(&entry.answer, &entry.id),
        &file_body(&entry.answer),
        key,
        guard,
        &|| proceed() && !superseded(entries, id),
    );
    share_steps(&outcome, superseded(entries, id), doorbell_done)
}

/// One send's signed summary (`writeSummaryHeld`).
pub fn deliver_summary(
    v1: &Path,
    entries: &[Entry],
    send: &Send,
    key: &AnswerKey,
    proceed: &dyn Fn() -> bool,
) -> StepRecord {
    let answers: Vec<&Json> = send
        .entry_ids
        .iter()
        .filter_map(|id| entries.iter().find(|e| &e.id == id).map(|e| &e.answer))
        .collect();
    let unsigned = send_summary(&send.send_id, &send.at, &send.device, &answers);
    let body = file_body(&sign_document(&unsigned, key));
    let outcome = write_share_file(
        v1,
        &format!("batches/{}.json", send.send_id),
        &body,
        key,
        Guard::KeepPublished { unsigned: &unsigned },
        proceed,
    );
    summary_step(&outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_whitespace_and_controls() {
        assert_eq!(one_line("  tabs\tand\nnew\r\nlines  "), "tabs and new lines");
        assert_eq!(one_line("ctl\u{0}\u{1f}\u{7f}\u{85}x"), "ctl x");
        assert_eq!(one_line("nbsp\u{a0}and\u{2028}ls"), "nbsp and ls");
    }

    #[test]
    fn pm_mail_names_follow_the_schema_rule() {
        for ok in ["lead", "w-2", "a_b", "0x"] {
            assert!(is_pm_mail_name(ok), "{ok}");
        }
        for bad in ["", "all", "Lead", "-x", "a b", &"a".repeat(65)] {
            assert!(!is_pm_mail_name(bad), "{bad}");
        }
    }
}
