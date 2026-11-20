//! Saved-login renewal planner (`src/codex-auth/codex-renewal-planner.ts`).
//! `collect_codex_renewal_snapshot` reads local files and running-process homes
//! (no network, no writes); `plan_codex_profile_renewal` is pure. Snapshots hold
//! credentials in memory only; plan entries carry safe fields.
//!
//! Rules, in order: no login / unsafe file; no account binding; the live
//! login's account (every saved copy) or an unreadable live login; a recorded
//! rejection of this file version; no family markers; process scan failure; a
//! Codex process using the profile folder; any other login that shares or may
//! share the family; recorded backoff; then due or fresh.

use crate::family::{
    CodexFamilyMatch, CodexLoginFingerprint, CodexLoginSource, CodexLoginSourceKind,
    FamilyComparison, SourceState, compare_login_family, find_home_codex_auth_files,
    find_profile_auth_files, fingerprint_codex_login, parse_auth_json, read_cliproxy_codex_sources,
    read_codex_login_source, same_account_or_unknown, scan_processes,
};
use crate::jsnum::{iso, parse_date_ms};
use crate::procscan::{CodexProcessScan, ScanProcesses, home_dir, normalize};
use crate::registry::{codex_instances_dir, list_registered_codex_profiles};
use crate::status_store::{
    CodexRenewalRecord, CodexRenewalRecords, codex_auth_file_version, codex_renewal_status_path,
    read_codex_renewal_records, record,
};
use crate::types::{CODEX_RENEWAL_MESSAGES, CodexRenewalReason, is_dead_reason, is_retry_reason};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The registered-names seam (defaults to the registry read).
pub type ListProfiles = Arc<dyn Fn() -> Result<Vec<String>, ()> + Send + Sync>;

/// Renew once 4 days or less of the 10-day access token remain.
pub const CODEX_RENEWAL_LEAD_MS: i64 = 4 * DAY_MS;
/// Without a decodable expiry: 6 days after the last refresh.
pub const CODEX_RENEWAL_FALLBACK_AGE_MS: i64 = 6 * DAY_MS;
/// Upper bound for a manual early renewal lead (the access token lasts 10 days).
pub const CODEX_RENEWAL_MAX_LEAD_MS: i64 = 10 * DAY_MS;
const MAX_PROFILE_AUTH_BYTES: u64 = 1024 * 1024;

/// Where to look. Tests inject every path; production uses the boot's homes.
#[derive(Clone, Default)]
pub struct CodexRenewalEnvironment {
    /// The CCS root (the scoped config dir in TS).
    pub ccs_dir: PathBuf,
    /// The shared native login (`~/.codex`).
    pub codex_home: Option<PathBuf>,
    /// Scanned for `.codex*` homes and their child homes.
    pub home_dir: Option<PathBuf>,
    /// Extra homes to read as `extra` sources.
    pub extra_codex_homes: Vec<PathBuf>,
    /// None means the platform scan (Linux only); a seam can inject or fail it.
    pub scan_processes: Option<ScanProcesses>,
    /// Registered names; defaults to the registry read.
    pub list_profiles: Option<ListProfiles>,
    /// Manual early-renewal lead, clamped between 4 and 10 days.
    pub renew_within_ms: Option<f64>,
}

/// The saved login's read state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileState {
    /// A bounded regular file with a renewal token.
    Ok,
    /// Absent, or no renewal token.
    Missing,
    /// A link, oversized, unreadable or not a JSON object.
    Unsafe,
}

/// Private: holds the saved credentials in memory.
#[derive(Debug, Clone)]
pub struct CodexRenewalProfileSnapshot {
    /// The registered profile name.
    pub name: String,
    /// Its folder under codex-instances.
    pub profile_dir: PathBuf,
    /// The auth.json path.
    pub auth_path: PathBuf,
    /// Realpath of the file, when resolvable.
    pub real_path: Option<PathBuf>,
    /// Realpath of the folder, when resolvable.
    pub real_dir: Option<PathBuf>,
    /// Read state.
    pub state: ProfileState,
    /// The exact bytes read, for the compare-and-swap.
    pub content: Option<Vec<u8>>,
    /// The parsed file.
    pub parsed: Option<Value>,
    /// Its fingerprint.
    pub fingerprint: Option<CodexLoginFingerprint>,
    /// Stat identity of the file.
    pub version: Option<String>,
}

/// Private: holds credentials in memory.
#[derive(Debug, Clone)]
pub struct CodexRenewalSnapshot {
    /// The resolved shared login home.
    pub codex_home: PathBuf,
    /// The resolved scanned home folder.
    pub home_dir: PathBuf,
    /// The live login source, when present.
    pub live: Option<CodexLoginSource>,
    /// The profile snapshots, in registry order.
    pub profiles: Vec<CodexRenewalProfileSnapshot>,
    /// Every login source, in collection order.
    pub sources: Vec<CodexLoginSource>,
    /// The running-process scan.
    pub process_scan: CodexProcessScan,
    /// The recorded history.
    pub records: CodexRenewalRecords,
    /// The clamped renewal lead, ms.
    pub renew_within_ms: i64,
}

/// The planner's decision word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexRenewalDecision {
    /// Not due yet.
    Fresh,
    /// Renew this cycle.
    Due,
    /// A guard skips it.
    Skip,
    /// OpenAI rejected this file version.
    Dead,
    /// A recorded backoff still applies.
    Backoff,
}

impl CodexRenewalDecision {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Due => "due",
            Self::Skip => "skip",
            Self::Dead => "dead",
            Self::Backoff => "backoff",
        }
    }
}

/// One profile's plan: safe fields only.
#[derive(Debug, Clone)]
pub struct CodexRenewalPlanEntry {
    /// Profile name.
    pub name: String,
    /// Decision word.
    pub decision: CodexRenewalDecision,
    /// Outcome code.
    pub reason: CodexRenewalReason,
    /// Fixed message.
    pub message: &'static str,
    /// Access expiry, ISO.
    pub access_expires_at: Option<String>,
    /// When renewal becomes due, ISO.
    pub due_at: Option<String>,
    /// Last success on any version, ISO.
    pub last_renewed_at: Option<String>,
    /// Last attempt on any version, ISO.
    pub last_attempt_at: Option<String>,
    /// Last outcome code on any version.
    pub last_outcome: Option<String>,
    /// Backoff/fresh retry time, ISO.
    pub next_attempt_at: Option<String>,
}

impl CodexRenewalPlanEntry {
    /// The entry in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), Value::String(self.name.clone()));
        out.insert(
            "decision".into(),
            Value::String(self.decision.word().into()),
        );
        out.insert("reason".into(), Value::String(self.reason.code().into()));
        out.insert("message".into(), Value::String(self.message.into()));
        insert_opt(&mut out, "accessExpiresAt", &self.access_expires_at);
        insert_opt(&mut out, "dueAt", &self.due_at);
        insert_opt(&mut out, "lastRenewedAt", &self.last_renewed_at);
        insert_opt(&mut out, "lastAttemptAt", &self.last_attempt_at);
        insert_opt(&mut out, "lastOutcome", &self.last_outcome);
        insert_opt(&mut out, "nextAttemptAt", &self.next_attempt_at);
        Value::Object(out)
    }
}

/// Insert an optional string as null (the TS DTOs spell nulls explicitly).
pub fn insert_opt(out: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    out.insert(
        key.into(),
        value.clone().map(Value::String).unwrap_or(Value::Null),
    );
}

/// One shared-family hit: path, kind and the markers that matched.
#[derive(Debug, Clone)]
pub struct SharedWith {
    /// The other login's path.
    pub path: PathBuf,
    /// Where it was found.
    pub kind: CodexLoginSourceKind,
    /// Match labels, in comparison order.
    pub matched_by: Vec<CodexFamilyMatch>,
}

/// One login that could not be checked.
#[derive(Debug, Clone)]
pub struct Uncheckable {
    /// The other login's path.
    pub path: PathBuf,
    /// Where it was found.
    pub kind: CodexLoginSourceKind,
}

/// The guard details behind a plan (paths and labels, never credentials).
#[derive(Debug, Clone, Default)]
pub struct CodexRenewalGuards {
    /// The live login belongs to this account.
    pub live: bool,
    /// A running Codex uses this profile folder.
    pub in_use: bool,
    /// Other logins that share the family.
    pub shared_with: Vec<SharedWith>,
    /// Other logins that could not be ruled out.
    pub uncheckable: Vec<Uncheckable>,
}

impl CodexRenewalGuards {
    /// The guards in the TS key order (dry run adds `sharesFamily`).
    pub fn to_json(&self, shares_family: bool) -> Value {
        let mut out = Map::new();
        out.insert("live".into(), Value::Bool(self.live));
        out.insert("inUse".into(), Value::Bool(self.in_use));
        let shared: Vec<Value> = self
            .shared_with
            .iter()
            .map(|hit| {
                let mut entry = Map::new();
                entry.insert("path".into(), Value::String(path_text(&hit.path)));
                entry.insert("kind".into(), Value::String(hit.kind.word().into()));
                entry.insert(
                    "matchedBy".into(),
                    Value::Array(
                        hit.matched_by
                            .iter()
                            .map(|m| Value::String(m.word().into()))
                            .collect(),
                    ),
                );
                Value::Object(entry)
            })
            .collect();
        out.insert("sharedWith".into(), Value::Array(shared));
        let uncheckable: Vec<Value> = self
            .uncheckable
            .iter()
            .map(|hit| {
                let mut entry = Map::new();
                entry.insert("path".into(), Value::String(path_text(&hit.path)));
                entry.insert("kind".into(), Value::String(hit.kind.word().into()));
                Value::Object(entry)
            })
            .collect();
        out.insert("uncheckable".into(), Value::Array(uncheckable));
        if shares_family {
            out.insert(
                "sharesFamily".into(),
                Value::Bool(!self.shared_with.is_empty()),
            );
        }
        Value::Object(out)
    }
}

/// Paths spell with forward slashes in parity output (the harness normalizes
/// the sandbox root the same way on both sides).
pub fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn real_path_of(file: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(file).ok()
}

/// `readCodexRenewalProfile`: the saved login, refusing links, non-regular and
/// oversized files.
pub fn read_codex_renewal_profile(ccs_dir: &Path, name: &str) -> CodexRenewalProfileSnapshot {
    let profile_dir = normalize(&codex_instances_dir(ccs_dir)).join(name);
    let auth_path = profile_dir.join("auth.json");
    let base = CodexRenewalProfileSnapshot {
        name: name.to_owned(),
        real_path: real_path_of(&auth_path),
        real_dir: real_path_of(&profile_dir),
        state: ProfileState::Missing,
        content: None,
        parsed: None,
        fingerprint: None,
        version: codex_auth_file_version(&auth_path),
        profile_dir,
        auth_path,
    };
    let stat = match std::fs::symlink_metadata(&base.auth_path) {
        Ok(stat) => stat,
        Err(error) => {
            let state = match error.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
                    ProfileState::Missing
                }
                _ => ProfileState::Unsafe,
            };
            return CodexRenewalProfileSnapshot { state, ..base };
        }
    };
    if !stat.is_file() || stat.len() > MAX_PROFILE_AUTH_BYTES {
        return CodexRenewalProfileSnapshot {
            state: ProfileState::Unsafe,
            ..base
        };
    }
    let content = match std::fs::read(&base.auth_path) {
        Ok(content) => content,
        Err(error) => {
            let state = match error.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
                    ProfileState::Missing
                }
                _ => ProfileState::Unsafe,
            };
            return CodexRenewalProfileSnapshot { state, ..base };
        }
    };
    let parsed = parse_auth_json(&content);
    let Some(parsed) = parsed.filter(Value::is_object) else {
        return CodexRenewalProfileSnapshot {
            state: ProfileState::Unsafe,
            ..base
        };
    };
    let fingerprint = fingerprint_codex_login(&parsed);
    let tokens = parsed.get("tokens").filter(|t| t.is_object());
    let refresh_is_string = tokens
        .and_then(|t| t.get("refresh_token"))
        .is_some_and(Value::is_string);
    if fingerprint
        .as_ref()
        .and_then(|f| f.refresh_digest.as_ref())
        .is_none()
        || tokens.is_none()
        || !refresh_is_string
    {
        return CodexRenewalProfileSnapshot {
            state: ProfileState::Missing,
            ..base
        };
    }
    CodexRenewalProfileSnapshot {
        state: ProfileState::Ok,
        content: Some(content),
        parsed: Some(parsed),
        fingerprint,
        ..base
    }
}

fn collect_sources(
    env: &CodexRenewalEnvironment,
    codex_home: &Path,
    home_dir: &Path,
    process_scan: &CodexProcessScan,
) -> (Option<CodexLoginSource>, Vec<CodexLoginSource>) {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut sources: Vec<CodexLoginSource> = Vec::new();
    let mut add = |file: PathBuf, kind: CodexLoginSourceKind| -> Option<CodexLoginSource> {
        let resolved = normalize(&file);
        if seen.contains(&resolved) {
            return None;
        }
        seen.push(resolved.clone());
        let source = read_codex_login_source(&resolved, kind);
        if let Some(source) = &source {
            sources.push(source.clone());
        }
        source
    };
    let live = add(codex_home.join("auth.json"), CodexLoginSourceKind::Live);
    for file in find_profile_auth_files(&normalize(&codex_instances_dir(&env.ccs_dir))) {
        add(file, CodexLoginSourceKind::Profile);
    }
    for home in &env.extra_codex_homes {
        add(home.join("auth.json"), CodexLoginSourceKind::Extra);
    }
    for file in find_home_codex_auth_files(home_dir) {
        add(file, CodexLoginSourceKind::CodexHome);
    }
    for process_home in &process_scan.homes {
        add(
            process_home.home.join("auth.json"),
            CodexLoginSourceKind::ProcessHome,
        );
    }
    for source in read_cliproxy_codex_sources(&env.ccs_dir) {
        let resolved = normalize(&source.path);
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved);
        sources.push(source);
    }
    (live, sources)
}

fn renewal_lead(value: Option<f64>) -> i64 {
    match value {
        Some(value) if value.is_finite() => {
            (value.max(CODEX_RENEWAL_LEAD_MS as f64)).min(CODEX_RENEWAL_MAX_LEAD_MS as f64) as i64
        }
        _ => CODEX_RENEWAL_LEAD_MS,
    }
}

/// Read-only snapshot. `names` limits the profile snapshots; every saved login
/// is still a source. A registry read failure propagates (the cycle reports it).
pub fn collect_codex_renewal_snapshot(
    env: &CodexRenewalEnvironment,
    names: Option<&[String]>,
) -> Result<CodexRenewalSnapshot, ()> {
    let codex_home = normalize(
        &env.codex_home
            .clone()
            .unwrap_or_else(|| home_dir().join(".codex")),
    );
    let home_dir = normalize(&env.home_dir.clone().unwrap_or_else(home_dir));
    let process_scan = scan_processes(env.scan_processes.as_ref());
    let (live, sources) = collect_sources(env, &codex_home, &home_dir, &process_scan);
    let profile_names: Vec<String> = match names {
        Some(names) => names.to_vec(),
        None => match &env.list_profiles {
            Some(seam) => seam()?,
            None => list_registered_codex_profiles(&env.ccs_dir)?,
        },
    };
    Ok(CodexRenewalSnapshot {
        codex_home,
        home_dir,
        live,
        profiles: profile_names
            .iter()
            .map(|name| read_codex_renewal_profile(&env.ccs_dir, name))
            .collect(),
        sources,
        process_scan,
        records: read_codex_renewal_records(&codex_renewal_status_path(&env.ccs_dir)),
        renew_within_ms: renewal_lead(env.renew_within_ms),
    })
}

/// `evaluateCodexRenewalGuards`.
pub fn evaluate_codex_renewal_guards(
    snapshot: &CodexRenewalSnapshot,
    profile: &CodexRenewalProfileSnapshot,
) -> CodexRenewalGuards {
    let mut guards = CodexRenewalGuards::default();
    let Some(fingerprint) = &profile.fingerprint else {
        // The live/inUse checks still run for a fingerprint-less profile in TS?
        // No: TS computes live and inUse before the early return.
        return guards_with_live_in_use(snapshot, profile, guards);
    };
    guards = guards_with_live_in_use(snapshot, profile, guards);
    for source in &snapshot.sources {
        if source.path == profile.auth_path {
            continue;
        }
        match compare_login_family(fingerprint, profile.real_path.as_deref(), source) {
            FamilyComparison::Unverifiable => guards.uncheckable.push(Uncheckable {
                path: source.path.clone(),
                kind: source.kind,
            }),
            FamilyComparison::Matches(matched_by) => guards.shared_with.push(SharedWith {
                path: source.path.clone(),
                kind: source.kind,
                matched_by,
            }),
            FamilyComparison::Unrelated => {}
        }
    }
    guards
}

fn guards_with_live_in_use(
    snapshot: &CodexRenewalSnapshot,
    profile: &CodexRenewalProfileSnapshot,
    mut guards: CodexRenewalGuards,
) -> CodexRenewalGuards {
    let live_fingerprint = snapshot
        .live
        .as_ref()
        .filter(|live| live.state == SourceState::Ok)
        .and_then(|live| live.fingerprint.as_ref());
    if let (Some(fingerprint), Some(live)) = (&profile.fingerprint, live_fingerprint) {
        guards.live = same_account_or_unknown(fingerprint, live);
    }
    guards.in_use = snapshot.process_scan.homes.iter().any(|process_home| {
        process_home.home == profile.profile_dir
            || process_home
                .real_home
                .as_ref()
                .is_some_and(|real| Some(real) == profile.real_dir.as_ref())
    });
    guards
}

fn current_record<'a>(
    snapshot: &'a CodexRenewalSnapshot,
    profile: &CodexRenewalProfileSnapshot,
) -> Option<&'a CodexRenewalRecord> {
    let record = record(&snapshot.records, &profile.name)?;
    match &profile.version {
        Some(version) if &record.auth_version == version => Some(record),
        _ => None,
    }
}

/// Pure decision for one profile from a snapshot.
pub fn plan_codex_profile_renewal(
    snapshot: &CodexRenewalSnapshot,
    profile: &CodexRenewalProfileSnapshot,
    now: i64,
) -> (CodexRenewalPlanEntry, CodexRenewalGuards) {
    let guards = evaluate_codex_renewal_guards(snapshot, profile);
    let history = record(&snapshot.records, &profile.name);
    let current = current_record(snapshot, profile);
    let fingerprint = profile.fingerprint.as_ref();
    let access_expires_at = fingerprint.and_then(|f| f.access_expires_at);
    let last_refresh_at = fingerprint.and_then(|f| f.last_refresh_at);
    let due_at: Option<i64> = if let Some(expiry) = access_expires_at {
        Some(expiry - snapshot.renew_within_ms)
    } else if let Some(last_refresh) = last_refresh_at {
        Some(
            last_refresh + CODEX_RENEWAL_FALLBACK_AGE_MS
                - (snapshot.renew_within_ms - CODEX_RENEWAL_LEAD_MS),
        )
    } else if fingerprint.is_some() {
        Some(now)
    } else {
        None
    };
    let entry = |decision: CodexRenewalDecision,
                 reason: CodexRenewalReason,
                 next_attempt_at: Option<String>|
     -> (CodexRenewalPlanEntry, CodexRenewalGuards) {
        (
            CodexRenewalPlanEntry {
                name: profile.name.clone(),
                decision,
                reason,
                message: reason.message(),
                access_expires_at: access_expires_at.and_then(iso),
                due_at: due_at.and_then(iso),
                last_renewed_at: history.and_then(|h| h.last_success_at.clone()),
                last_attempt_at: history.map(|h| h.last_attempt_at.clone()),
                last_outcome: history.map(|h| h.outcome.clone()),
                next_attempt_at,
            },
            guards.clone(),
        )
    };

    if profile.state == ProfileState::Missing {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::NoLogin,
            None,
        );
    }
    if profile.state == ProfileState::Unsafe || fingerprint.is_none() {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::UnsafeFile,
            None,
        );
    }
    let fingerprint = fingerprint.expect("checked above");
    if fingerprint.binding.is_none() {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::Unverifiable,
            None,
        );
    }
    if let Some(live) = &snapshot.live {
        if live.state == SourceState::Unreadable {
            return entry(
                CodexRenewalDecision::Skip,
                CodexRenewalReason::LiveUnverifiable,
                None,
            );
        }
        if live.state == SourceState::Ok {
            let live_fp = live.fingerprint.as_ref();
            if live_fp.and_then(|f| f.binding.as_ref()).is_none()
                && live_fp.and_then(|f| f.email.as_ref()).is_none()
            {
                // A live token login without a readable account could be any saved profile.
                return entry(
                    CodexRenewalDecision::Skip,
                    CodexRenewalReason::LiveUnverifiable,
                    None,
                );
            }
        }
    }
    if guards.live {
        return entry(CodexRenewalDecision::Skip, CodexRenewalReason::Live, None);
    }
    if let Some(record) = current
        && let Some(reason) = record.reason()
        && is_dead_reason(reason)
    {
        return entry(CodexRenewalDecision::Dead, reason, None);
    }
    if fingerprint.session_ids.is_empty() && fingerprint.auth_times.is_empty() {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::Unverifiable,
            None,
        );
    }
    if snapshot.process_scan.state == crate::procscan::ScanState::Failed {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::ProcessScanFailed,
            None,
        );
    }
    if guards.in_use {
        return entry(CodexRenewalDecision::Skip, CodexRenewalReason::InUse, None);
    }
    if !guards.shared_with.is_empty() {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::SharedFamily,
            None,
        );
    }
    if !guards.uncheckable.is_empty() {
        return entry(
            CodexRenewalDecision::Skip,
            CodexRenewalReason::FamilyUnverifiable,
            None,
        );
    }
    if let Some(record) = current
        && let Some(reason) = record.reason()
        && is_retry_reason(reason)
        && crate::status_store::backoff_active(record, now)
    {
        return entry(
            CodexRenewalDecision::Backoff,
            reason,
            record.next_attempt_at.clone(),
        );
    }
    if let Some(due) = due_at
        && now >= due
    {
        return entry(CodexRenewalDecision::Due, CodexRenewalReason::Due, None);
    }
    entry(
        CodexRenewalDecision::Fresh,
        CodexRenewalReason::Fresh,
        due_at.and_then(iso),
    )
}

/// Plan every profile in the snapshot.
pub fn plan_codex_profile_renewals(
    snapshot: &CodexRenewalSnapshot,
    now: i64,
) -> Vec<CodexRenewalPlanEntry> {
    snapshot
        .profiles
        .iter()
        .map(|profile| plan_codex_profile_renewal(snapshot, profile, now).0)
        .collect()
}

/// The fixed message for a wire code (parity exports).
pub fn message_for(code: &str) -> Option<&'static str> {
    CODEX_RENEWAL_MESSAGES
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, message)| *message)
}

/// Whether a recorded outcome is a backoff that still applies (helper for tests).
pub fn record_backoff(record: &CodexRenewalRecord, now: i64) -> bool {
    record
        .next_attempt_at
        .as_deref()
        .and_then(parse_date_ms)
        .is_some_and(|retry| retry > now)
}
