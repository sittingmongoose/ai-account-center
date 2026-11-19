//! Port of the pinned copy core `scripts/claude-history/history-index-sync.cjs`: policy and
//! snapshot validation, the neutral projection, the missing-record plan and the bounded
//! microbatch sequence. The Rust service runs it in place of `require()`ing the core; the
//! TypeScript path keeps the core until the cut. Reason tokens, result shapes and field order
//! are the core's, byte for byte.
use super::js::{self, safe_integer, sha, stringify, utf16_len, uuid};
use serde_json::{Map, Value, json};
use std::pin::Pin;

/// The IDs the pinned copy core supports; the server keeps no copy of this list.
pub const PROFILES: [&str; 4] = ["platyr", "gmail", "party", "me"];
/// The core's `DIRECTIONS`: source platform first, target second.
pub const DIRECTIONS: [(&str, &str, &str); 2] =
    [("platyr", "mac", "windows"), ("gmail", "windows", "mac")];
/// The core's `MICROBATCH_RECORDS`.
pub const MICROBATCH_RECORDS: usize = 1;
/// The core's `REGISTRY_LIMIT`.
pub const REGISTRY_LIMIT: usize = 200;
/// The core's `OPEN_COPY_BUDGET_MS`.
pub const OPEN_COPY_BUDGET_MS: u64 = 45_000;
/// The core's `MAX_BATCHES_PER_OPEN`.
pub const MAX_BATCHES_PER_OPEN: u64 = 50;
const PROJECT: &str = "/mnt/Cursor/PuppetMaster";
pub(crate) const NATIVE: [(&str, &str, &str); 2] = [
    (
        "mac",
        "2.19675.0",
        "102d49173311cfcba1bfa62abc5ea986797f5b97ec16caa3784ee65cd19774ac",
    ),
    (
        "windows",
        "2.19675.0.0",
        "492242eb2c352f60a886031275b7a37fd2fc791946be98b8c8fdd4bbfc41ded6",
    ),
];
const SAFE_REASONS: [&str; 12] = [
    "registry_invalid",
    "record_binding_invalid",
    "record_invalid",
    "record_identity_invalid",
    "duplicate_identity",
    "identity_mismatch",
    "snapshot_changed",
    "endpoint_unverified",
    "identity_collision",
    "target_opened",
    "target_state_changed",
    "history_policy_unverified",
];

/// The core's `Refused`: a fixed reason token, never a raw error.
#[derive(Debug)]
pub struct Refused(pub &'static str);

type Checked<T> = Result<T, Refused>;

fn reject(reason: &'static str) -> Refused {
    Refused(reason)
}

/// One wire record after `decodeSnapshot`: its name, exact bytes and pinned digest.
#[derive(Clone, Debug)]
pub struct Record {
    /// The registry file name (`local_<uuid>.json`).
    pub name: String,
    /// The exact descriptor bytes.
    pub bytes: Vec<u8>,
    /// Lowercase hex SHA-256 of `bytes`.
    pub sha256: String,
}

/// A decoded private snapshot: the helper's fields plus decoded records.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Every snapshot field except `records`, in wire order.
    pub fields: Map<String, Value>,
    /// The decoded records, in wire order.
    pub records: Vec<Record>,
}

impl Snapshot {
    /// One field of the snapshot envelope.
    pub fn get(&self, key: &str) -> &Value {
        self.fields.get(key).unwrap_or(&Value::Null)
    }

    /// The wire form `wireSnapshot` sends: records as `{name, base64, sha256}`.
    pub fn wire(&self) -> Value {
        let mut out = Value::Object(self.fields.clone());
        out["records"] = Value::Array(self.records.iter().map(wire_record).collect());
        out
    }
}

/// `wireSnapshot`'s row form.
pub fn wire_record(record: &Record) -> Value {
    use base64::{Engine, engine::general_purpose::STANDARD};
    json!({"name": record.name, "base64": STANDARD.encode(&record.bytes), "sha256": sha(&record.bytes)})
}

/// The core's `snapshotRevision`: SHA-256 of the compact JSON of `[name, sha(bytes)]` pairs
/// sorted by name. Valid names are ASCII with fixed hyphen positions, so the core's
/// `localeCompare` order and byte order agree on every record that passes `decodeRecords`.
pub fn snapshot_revision(records: &[Record]) -> String {
    let mut pairs: Vec<Value> = records
        .iter()
        .map(|record| json!([record.name, sha(&record.bytes)]))
        .collect();
    pairs.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    sha(stringify(&Value::Array(pairs)).as_bytes())
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

fn text<'a>(value: &'a Value) -> Option<&'a str> {
    value.as_str()
}

/// The core's `validatePolicy`.
pub fn validate_policy(policy: &Value) -> Checked<()> {
    let bad = || reject("history_policy_unverified");
    if !is_object(policy)
        || policy["version"] != json!(1)
        || policy["enabled"] != json!(true)
        || !matches!(text(&policy["sourcePlatform"]), Some("mac" | "windows"))
        || !text(&policy["identity"]["accountUuid"]).is_some_and(uuid)
        || !text(&policy["identity"]["organizationUuid"]).is_some_and(uuid)
        || text(&policy["project"]["cwd"]) != Some(PROJECT)
        || text(&policy["project"]["originCwd"]) != Some(PROJECT)
    {
        return Err(bad());
    }
    let Some(root) = text(&policy["project"]["transcriptRoot"]) else {
        return Err(bad());
    };
    // /^\/[^\\\x00-\x1f\x7f]+\/\.claude\/projects$/ plus the segment rules.
    const SUFFIX: &str = "/.claude/projects";
    let middle = root
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix(SUFFIX))
        .filter(|middle| !middle.is_empty());
    let Some(middle) = middle else {
        return Err(bad());
    };
    if middle.contains('\\')
        || middle.bytes().any(|b| b < 0x20 || b == 0x7f)
        || root.contains("//")
        || root.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(bad());
    }
    for platform in ["mac", "windows"] {
        let endpoint = &policy["ssh"][platform];
        let Some(alias) = text(&endpoint["alias"]) else {
            return Err(bad());
        };
        let Some(hostname) = text(&endpoint["hostname"]) else {
            return Err(bad());
        };
        let Some(username) = text(&endpoint["username"]) else {
            return Err(bad());
        };
        if !shape(alias, 128, b"._-", false)
            || !shape(hostname, 255, b".:-", false)
            || !shape(username, 128, b"_.-", true)
        {
            return Err(bad());
        }
        let port = endpoint["port"].as_f64();
        if !port.is_some_and(|p| {
            safe_integer(&endpoint["port"]) && (1.0..=65535.0).contains(&p)
        }) {
            return Err(bad());
        }
    }
    let (a, b) = (&policy["ssh"]["mac"], &policy["ssh"]["windows"]);
    if a["hostname"] != b["hostname"] || a["username"] != b["username"] || a["port"] != b["port"] {
        return Err(bad());
    }
    Ok(())
}

/// The endpoint shapes: an alphanumeric first byte, then `extra` punctuation, bounded length.
fn shape(raw: &str, max_tail: usize, extra: &[u8], underscore_first: bool) -> bool {
    let bytes = raw.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    let first_ok = first.is_ascii_alphanumeric() || (underscore_first && *first == b'_');
    first_ok
        && bytes.len() <= max_tail + 1
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(b))
}

/// The core's `decodeRecords`: decoded records to descriptor values. The service wrapper has
/// already turned the wire base64 into exact bytes (its `decodeSnapshot`); the core re-checks
/// the binding, the size bounds and the identity rules on those bytes.
pub fn decode_records(records: &[Record]) -> Checked<Vec<Value>> {
    if records.len() > 200 {
        return Err(reject("registry_invalid"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut cli_ids = std::collections::HashSet::new();
    let mut total = 0usize;
    let mut decoded = Vec::new();
    for record in records {
        let name = record.name.as_str();
        let bytes = &record.bytes;
        let digest = record.sha256.as_str();
        if !name_shape(name)
            || bytes.len() > 2_000_000
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || sha(bytes) != digest
        {
            return Err(reject("record_binding_invalid"));
        }
        total += bytes.len();
        if total > 16_000_000 {
            return Err(reject("registry_invalid"));
        }
        let stripped = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let Some(value) = js::parse(stripped) else {
            return Err(reject("record_invalid"));
        };
        let session_id = text(&value["sessionId"]).unwrap_or_default();
        let cli_session_id = text(&value["cliSessionId"]).unwrap_or_default();
        if !is_object(&value)
            || !session_id.starts_with("local_")
            || !uuid(&session_id[6..])
            || !uuid(cli_session_id)
            || name != format!("{session_id}.json")
        {
            return Err(reject("record_identity_invalid"));
        }
        if !ids.insert(session_id.to_string()) || !cli_ids.insert(cli_session_id.to_string()) {
            return Err(reject("duplicate_identity"));
        }
        decoded.push(value);
    }
    Ok(decoded)
}

/// The core's record name shape `/^local_[0-9a-f-]+\.json$/`.
fn name_shape(name: &str) -> bool {
    name.strip_prefix("local_")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|middle| {
            !middle.is_empty()
                && middle
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
        })
}

/// The core's `validateSnapshot`: identity, records, stability, revision and endpoint binding.
pub fn validate_snapshot(
    snapshot: &Snapshot,
    profile_id: &str,
    platform: &str,
    policy: &Value,
) -> Checked<Vec<Value>> {
    let identity = &policy["identity"];
    let account = text(&identity["accountUuid"]).unwrap_or_default();
    let org = text(&identity["organizationUuid"]).unwrap_or_default();
    if text(snapshot.get("profileId")) != Some(profile_id)
        || text(snapshot.get("platform")) != Some(platform)
        || text(&snapshot.get("identity")["accountSha256"])
            != Some(sha(account.to_ascii_lowercase().as_bytes()).as_str())
        || text(&snapshot.get("identity")["orgSha256"])
            != Some(sha(org.to_ascii_lowercase().as_bytes()).as_str())
    {
        return Err(reject("identity_mismatch"));
    }
    let rows = decode_records(&snapshot.records)?;
    if snapshot.get("snapshotStable") != &json!(true)
        || text(snapshot.get("revision")) != Some(snapshot_revision(&snapshot.records).as_str())
    {
        return Err(reject("snapshot_changed"));
    }
    let endpoint = &policy["ssh"][platform];
    let expected = [
        (
            "hostnameSha256",
            sha(text(&endpoint["hostname"]).unwrap_or_default().as_bytes()),
        ),
        (
            "usernameSha256",
            sha(text(&endpoint["username"]).unwrap_or_default().as_bytes()),
        ),
        (
            "portSha256",
            sha(stringify(&js::normalize(endpoint["port"].clone())).as_bytes()),
        ),
    ];
    for (key, value) in expected {
        if text(&snapshot.get("endpoint")[key]) != Some(value.as_str()) {
            return Err(reject("endpoint_unverified"));
        }
    }
    Ok(rows)
}

/// The core's `nativeGuardVerified`.
pub fn native_guard_verified(snapshot: &Snapshot, platform: &str) -> bool {
    let Some((_, version, digest)) = NATIVE.iter().find(|(name, _, _)| *name == platform) else {
        return false;
    };
    let guard = snapshot.get("nativeGuard");
    text(&guard["version"]) == Some(*version)
        && text(&guard["managerSha256"]) == Some(*digest)
        && guard["warmGuardVerified"] == json!(true)
        && guard["autoResumeGuardVerified"] == json!(true)
}

/// The neutral projection's outcome: a descriptor or a fixed skip reason.
pub enum Neutral {
    /// The projected descriptor object.
    Descriptor(Value),
    /// A fixed skip reason token.
    Reason(&'static str),
}

/// The core's `neutral`: project one source descriptor onto the target platform.
pub fn neutral(source: &Value, source_platform: &str, target_platform: &str, policy: &Value) -> Neutral {
    let not_verified = Neutral::Reason("project_or_backend_not_verified");
    if text(&source["cwd"]) != text(&policy["project"]["cwd"])
        || text(&source["originCwd"]) != text(&policy["project"]["originCwd"])
        || text(&source["sshConfig"]["sshHost"]) != text(&policy["ssh"][source_platform]["alias"])
    {
        return not_verified;
    }
    if !source["wslConfig"].is_null() && source["wslConfig"] != json!(false) {
        return not_verified;
    }
    if is_object(&source["backend"]) && text(&source["backend"]["kind"]) != Some("ssh") {
        return not_verified;
    }
    let Some(reference) = text(&source["sshRemoteTranscriptPath"]) else {
        return Neutral::Reason("transcript_reference_invalid");
    };
    let root = text(&policy["project"]["transcriptRoot"]).unwrap_or_default();
    let prefix = format!("{root}/");
    let Some(relative) = reference.strip_prefix(&prefix) else {
        return Neutral::Reason("transcript_reference_invalid");
    };
    let cli_session_id = text(&source["cliSessionId"]).unwrap_or_default();
    if reference.contains('\\')
        || reference.contains("//")
        || relative.split('/').count() != 2
        || reference.split('/').any(|part| matches!(part, "." | ".."))
        || reference.split('/').next_back() != Some(&format!("{cli_session_id}.jsonl"))
    {
        return Neutral::Reason("transcript_reference_invalid");
    }
    let title = text(&source["title"]).unwrap_or_default();
    if !js::non_negative_number(&source["createdAt"])
        || !js::non_negative_number(&source["lastActivityAt"])
        || !(1..=1000).contains(&utf16_len(title))
        || title.bytes().any(|b| b < 0x20 || b == 0x7f)
        || !source["isArchived"].is_boolean()
    {
        return Neutral::Reason("display_metadata_invalid");
    }
    let mut descriptor = Map::new();
    for key in ["sessionId", "cliSessionId"] {
        descriptor.insert(key.into(), source[key].clone());
    }
    descriptor.insert("cwd".into(), policy["project"]["cwd"].clone());
    descriptor.insert("originCwd".into(), policy["project"]["originCwd"].clone());
    descriptor.insert("createdAt".into(), js::normalize(source["createdAt"].clone()));
    descriptor.insert(
        "lastActivityAt".into(),
        js::normalize(source["lastActivityAt"].clone()),
    );
    descriptor.insert("lastFocusedAt".into(), json!(0));
    descriptor.insert("title".into(), source["title"].clone());
    descriptor.insert("isArchived".into(), source["isArchived"].clone());
    descriptor.insert("permissionMode".into(), json!("default"));
    descriptor.insert("importedFrom".into(), json!("local-1p-code"));
    descriptor.insert("resumeConfirmed".into(), json!(false));
    descriptor.insert("remoteControlAutoEligible".into(), json!(false));
    descriptor.insert(
        "sshConfig".into(),
        json!({"sshHost": policy["ssh"][target_platform]["alias"]}),
    );
    descriptor.insert("sshRemoteTranscriptPath".into(), json!(reference));
    if let Some(model) = text(&source["model"]) {
        if model_shape(model) {
            descriptor.insert("model".into(), json!(model));
        }
    }
    if matches!(
        text(&source["effort"]),
        Some("low" | "medium" | "high" | "max" | "xhigh")
    ) {
        descriptor.insert("effort".into(), source["effort"].clone());
    }
    if safe_integer(&source["completedTurns"])
        && source["completedTurns"]
            .as_f64()
            .is_some_and(|n| (0.0..=1_000_000.0).contains(&n))
    {
        descriptor.insert(
            "completedTurns".into(),
            js::normalize(source["completedTurns"].clone()),
        );
    }
    Neutral::Descriptor(Value::Object(descriptor))
}

/// `/^claude-[a-zA-Z0-9._-]{1,100}$/`.
fn model_shape(model: &str) -> bool {
    model
        .strip_prefix("claude-")
        .is_some_and(|rest| {
            (1..=100).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

/// The core's plan: missing records, already-present count and skip-reason counts.
pub struct Plan {
    /// Records to append, in source order.
    pub records: Vec<Record>,
    /// Source rows whose immutable binding already matches the target.
    pub already_present_count: usize,
    /// Skip reasons in first-seen order, with counts.
    pub skipped: Map<String, Value>,
}

/// The core's `planMissing`.
pub fn plan_missing(
    source_rows: &[Value],
    target_rows: &[Value],
    source_platform: &str,
    target_platform: &str,
    policy: &Value,
) -> Checked<Plan> {
    let mut records = Vec::new();
    let mut skipped: Map<String, Value> = Map::new();
    let mut already_present_count = 0usize;
    for source in source_rows {
        let candidate = match neutral(source, source_platform, target_platform, policy) {
            Neutral::Descriptor(value) => value,
            Neutral::Reason(reason) => {
                let count = skipped.get(reason).and_then(Value::as_u64).unwrap_or(0);
                skipped.insert(reason.into(), json!(count + 1));
                continue;
            }
        };
        let matches: Vec<&Value> = target_rows
            .iter()
            .filter(|row| {
                row["sessionId"] == candidate["sessionId"]
                    || row["cliSessionId"] == candidate["cliSessionId"]
            })
            .collect();
        if !matches.is_empty() {
            let existing = matches[0];
            if matches.len() != 1
                || existing["sessionId"] != candidate["sessionId"]
                || existing["cliSessionId"] != candidate["cliSessionId"]
                || existing["cwd"] != candidate["cwd"]
                || existing["originCwd"] != candidate["originCwd"]
                || existing["sshConfig"]["sshHost"] != candidate["sshConfig"]["sshHost"]
                || existing["sshRemoteTranscriptPath"] != candidate["sshRemoteTranscriptPath"]
            {
                return Err(reject("identity_collision"));
            }
            already_present_count += 1;
            continue;
        }
        let bytes = format!("{}\n", stringify(&candidate)).into_bytes();
        records.push(Record {
            name: format!("{}.json", candidate["sessionId"].as_str().unwrap_or_default()),
            sha256: sha(&bytes),
            bytes,
        });
    }
    Ok(Plan {
        records,
        already_present_count,
        skipped,
    })
}

/// The per-Open copy bound. Callers may only tighten it; an unreadable clock is exhausted.
pub struct CopyLimits {
    /// Milliseconds of copy budget, at most [`OPEN_COPY_BUDGET_MS`].
    pub budget_ms: u64,
    /// Batches per Open, at most [`MAX_BATCHES_PER_OPEN`].
    pub max_batches: u64,
    /// The clock in milliseconds; `None` reads as an exhausted budget.
    pub clock: Box<dyn Fn() -> Option<u64> + Send + Sync>,
}

impl Default for CopyLimits {
    fn default() -> Self {
        Self {
            budget_ms: OPEN_COPY_BUDGET_MS,
            max_batches: MAX_BATCHES_PER_OPEN,
            clock: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64)
            }),
        }
    }
}

impl CopyLimits {
    /// The core's `copyLimits`: positive safe integers only, capped at the core's bounds.
    pub fn from_value(limits: &Value) -> Self {
        let positive = |value: &Value| {
            value
                .as_f64()
                .filter(|n| safe_integer(value) && *n > 0.0)
                .map(|n| n as u64)
        };
        let budget_ms = positive(&limits["budgetMs"])
            .map(|n| n.min(OPEN_COPY_BUDGET_MS))
            .unwrap_or(OPEN_COPY_BUDGET_MS);
        let max_batches = positive(&limits["maxBatches"])
            .map(|n| n.min(MAX_BATCHES_PER_OPEN))
            .unwrap_or(MAX_BATCHES_PER_OPEN);
        Self {
            budget_ms,
            max_batches,
            clock: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as u64)
            }),
        }
    }
}

/// The append payload the core hands the adapter: whole-plan counts for progress only.
pub struct AppendPayload<'a> {
    /// The profile being copied.
    pub profile_id: &'a str,
    /// The target platform.
    pub target_platform: &'a str,
    /// The current authorized target baseline.
    pub target: &'a Snapshot,
    /// This microbatch's records.
    pub records: &'a [Record],
    /// The whole plan's record count.
    pub total_count: usize,
    /// Records confirmed by earlier batches of this Open.
    pub confirmed_count: usize,
}

type AdapterFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, ()>> + Send + 'a>>;

/// The six adapters the core calls; the service builds them over the fixed helper transport.
pub trait Adapters: Send + Sync {
    /// Whether the target profile is closed.
    fn is_target_closed<'a>(&'a self, profile_id: &str, platform: &'a str) -> AdapterFut<'a, bool>;
    /// Read and decode the source snapshot.
    fn read_source<'a>(&'a self, profile_id: &str, platform: &'a str) -> AdapterFut<'a, Snapshot>;
    /// Read and decode the target snapshot.
    fn read_target<'a>(&'a self, profile_id: &str, platform: &'a str) -> AdapterFut<'a, Snapshot>;
    /// Stat-only transcript reference verification on the source computer.
    fn verify_transcript_references<'a>(
        &'a self,
        profile_id: &str,
        platform: &'a str,
        records: &[Record],
    ) -> AdapterFut<'a, bool>;
    /// Whether the target's protected state still matches the baseline.
    fn target_protected_state_unchanged<'a>(
        &'a self,
        target: &Snapshot,
    ) -> AdapterFut<'a, bool>;
    /// One create-only append transaction; a lost receipt is an error, never a retry.
    fn append_create_only<'a>(&'a self, payload: AppendPayload<'a>) -> AdapterFut<'a, Value>;
}

fn skipped(reason: &str) -> Value {
    json!({"status": "skipped", "reason": reason, "createdCount": 0})
}

fn public_counts(plan: &Plan) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("alreadyPresentCount".into(), json!(plan.already_present_count));
    out.insert("skippedReasonCounts".into(), Value::Object(plan.skipped.clone()));
    out
}

/// The core's `synchronizeBeforeProfileOpen`. Never panics on adapter errors: only fixed reason
/// tokens leave this boundary.
pub async fn synchronize_before_profile_open(
    profile_id: &str,
    target_platform: &str,
    policy: &Value,
    adapters: &dyn Adapters,
    limits: &CopyLimits,
) -> Value {
    if !PROFILES.contains(&profile_id) || !matches!(target_platform, "mac" | "windows") {
        return skipped("unsupported_profile_or_platform");
    }
    let Some(direction) = DIRECTIONS.iter().find(|(id, _, _)| *id == profile_id) else {
        return skipped("unsupported_direction");
    };
    let source_platform = direction.1;
    if direction.2 != target_platform {
        return skipped("unsupported_direction");
    }
    let started_at = (limits.clock)();
    let mut confirmed_count = 0usize;
    let mut batches_run = 0u64;
    let result = run_sequence(
        profile_id,
        target_platform,
        source_platform,
        policy,
        adapters,
        limits,
        started_at,
        &mut confirmed_count,
        &mut batches_run,
    )
    .await;
    match result {
        Ok(value) => value,
        Err(Refused(reason)) => {
            let reason = if SAFE_REASONS.contains(&reason) {
                reason
            } else {
                "unavailable"
            };
            if confirmed_count > 0 {
                json!({"status": "refused", "reason": reason, "createdCount": confirmed_count, "recoveryRequired": false})
            } else {
                skipped(reason)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_sequence(
    profile_id: &str,
    target_platform: &str,
    source_platform: &str,
    policy: &Value,
    adapters: &dyn Adapters,
    limits: &CopyLimits,
    started_at: Option<u64>,
    confirmed_count: &mut usize,
    batches_run: &mut u64,
) -> Checked<Value> {
    validate_policy(policy)?;
    if text(&policy["sourcePlatform"]) != Some(source_platform) {
        return Ok(skipped("history_policy_unverified"));
    }
    if adapters
        .is_target_closed(profile_id, target_platform)
        .await
        .map_err(|_| reject("unavailable"))?
        != true
    {
        return Ok(skipped("profile_already_open_or_unknown"));
    }
    let target = adapters
        .read_target(profile_id, target_platform)
        .await
        .map_err(|_| reject("unavailable"))?;
    let target_rows = validate_snapshot(&target, profile_id, target_platform, policy)?;
    if !native_guard_verified(&target, target_platform) {
        return Ok(skipped("native_guard_unverified"));
    }
    if target.get("noPendingInput") != &json!(true)
        || target.get("noScheduledWork") != &json!(true)
        || target.get("protectedSnapshotStable") != &json!(true)
    {
        return Ok(skipped("destination_work_or_state_unknown"));
    }
    let source = adapters
        .read_source(profile_id, source_platform)
        .await
        .map_err(|_| reject("unavailable"))?;
    let source_rows = validate_snapshot(&source, profile_id, source_platform, policy)?;
    if !native_guard_verified(&source, source_platform) {
        return Ok(skipped("native_guard_unverified"));
    }
    let plan = plan_missing(&source_rows, &target_rows, source_platform, target_platform, policy)?;
    if plan.records.is_empty() {
        let mut out = Map::new();
        out.insert("status".into(), json!("unchanged"));
        out.insert("createdCount".into(), json!(0));
        out.extend(public_counts(&plan));
        return Ok(Value::Object(out));
    }
    if adapters
        .verify_transcript_references(profile_id, source_platform, &plan.records)
        .await
        .map_err(|_| reject("unavailable"))?
        != true
    {
        return Ok(skipped("transcript_metadata_unverified"));
    }
    let source_again = adapters
        .read_source(profile_id, source_platform)
        .await
        .map_err(|_| reject("unavailable"))?;
    validate_snapshot(&source_again, profile_id, source_platform, policy)?;
    if !native_guard_verified(&source_again, source_platform) {
        return Ok(skipped("native_guard_unverified"));
    }
    if source_again.get("revision") != source.get("revision") {
        return Ok(skipped("source_changed"));
    }
    let mut batch_target = target.clone();
    if target.records.len() + plan.records.len() > REGISTRY_LIMIT {
        return Ok(skipped("registry_invalid"));
    }
    let total_count = plan.records.len();
    let mut authorized: Vec<Record> = target.records.clone();
    let mut offset = 0usize;
    while offset < total_count {
        let batch = &plan.records[offset..(offset + MICROBATCH_RECORDS).min(total_count)];
        // The core's guard(): closed state and protected baseline before every batch.
        if adapters
            .is_target_closed(profile_id, target_platform)
            .await
            .map_err(|_| reject("unavailable"))?
            != true
        {
            return Err(reject("target_opened"));
        }
        if adapters
            .target_protected_state_unchanged(&batch_target)
            .await
            .map_err(|_| reject("unavailable"))?
            != true
        {
            return Err(reject("target_state_changed"));
        }
        let result = adapters
            .append_create_only(AppendPayload {
                profile_id,
                target_platform,
                target: &batch_target,
                records: batch,
                total_count,
                confirmed_count: *confirmed_count,
            })
            .await;
        let Ok(result) = result else {
            // A lost append receipt is not proof that the remote writer did nothing.
            return Ok(json!({
                "status": "refused", "reason": "create_only_transaction_unconfirmed",
                "createdCount": *confirmed_count, "recoveryRequired": true
            }));
        };
        let created_ok = result["status"] == json!("created_metadata")
            && result["createdCount"].as_u64() == Some(batch.len() as u64)
            && result["protectedBytesUnchanged"] == json!(true);
        if !created_ok {
            let recovery = result["recoveryRequired"] == json!(true)
                || result["status"] == json!("refused_replacement_preserved")
                || (result["status"] != json!("refused_rolled_back")
                    && !(result["status"] == json!("refused")
                        && result["ownedFilesRolledBack"] == json!(true)
                        && result["recoveryRequired"] == json!(false)));
            let before = result["createdCountBeforeRefusal"]
                .as_u64()
                .filter(|n| *n <= batch.len() as u64)
                .unwrap_or(0);
            return Ok(json!({
                "status": "refused", "reason": "create_only_transaction_refused",
                "createdCount": *confirmed_count, "recoveryRequired": recovery,
                "createdCountBeforeRefusal": before
            }));
        }
        *confirmed_count += batch.len();
        *batches_run += 1;
        for record in batch {
            authorized.push(Record {
                name: record.name.clone(),
                sha256: sha(&record.bytes),
                bytes: record.bytes.clone(),
            });
        }
        if *confirmed_count == total_count {
            break;
        }
        // Only between batches: the last receipt finished the marker, so a stop holds nothing.
        let elapsed_ok = started_at
            .zip((limits.clock)())
            .is_some_and(|(start, now)| now.saturating_sub(start) < limits.budget_ms);
        if *batches_run >= limits.max_batches || !elapsed_ok {
            let mut out = Map::new();
            out.insert("status".into(), json!("partial"));
            out.insert("reason".into(), json!("open_copy_budget_reached"));
            out.insert("createdCount".into(), json!(*confirmed_count));
            out.insert("remainingCount".into(), json!(total_count - *confirmed_count));
            out.insert("recoveryRequired".into(), json!(false));
            out.extend(public_counts(&plan));
            out.insert("originalIdsPreserved".into(), json!(true));
            out.insert("explicitNativeConfirmationRequired".into(), json!(true));
            out.insert("resumedSessions".into(), json!(0));
            out.insert("copiedTranscripts".into(), json!(0));
            return Ok(Value::Object(out));
        }
        let current = adapters
            .read_target(profile_id, target_platform)
            .await
            .map_err(|_| reject("unavailable"))?;
        validate_snapshot(&current, profile_id, target_platform, policy)?;
        if !native_guard_verified(&current, target_platform) {
            return Err(reject("target_state_changed"));
        }
        if current.get("noPendingInput") != &json!(true)
            || current.get("noScheduledWork") != &json!(true)
            || current.get("protectedSnapshotStable") != &json!(true)
            || stringify(&js::normalize(current.get("protectedSnapshot").clone()))
                != stringify(&js::normalize(target.get("protectedSnapshot").clone()))
        {
            return Err(reject("target_state_changed"));
        }
        if current.records.len() != authorized.len() {
            return Err(reject("target_state_changed"));
        }
        for row in &current.records {
            let allowed = authorized.iter().find(|record| record.name == row.name);
            if !allowed.is_some_and(|record| record.bytes == row.bytes) {
                return Err(reject("target_state_changed"));
            }
        }
        batch_target = current;
        offset += MICROBATCH_RECORDS;
    }
    let mut out = Map::new();
    out.insert("status".into(), json!("synchronized"));
    out.insert("createdCount".into(), json!(*confirmed_count));
    out.extend(public_counts(&plan));
    out.insert("originalIdsPreserved".into(), json!(true));
    out.insert("explicitNativeConfirmationRequired".into(), json!(true));
    out.insert("resumedSessions".into(), json!(0));
    out.insert("copiedTranscripts".into(), json!(0));
    Ok(Value::Object(out))
}
