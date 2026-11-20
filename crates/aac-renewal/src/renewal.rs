//! Keep idle saved Codex logins alive (`src/codex-auth/codex-profile-renewal.ts`
//! and `codex-renewal-dry-run.ts`): renew each inactive saved profile before its
//! access token expires, with the Codex CLI's refresh grant, and write the
//! rotated tokens back into that same profile only.
//!
//! Refresh tokens rotate and a replayed one can revoke its whole family, so
//! every doubt skips. Per profile, under the activation lock taken without
//! waiting: re-read the profile, re-plan it with every guard, refresh with a
//! 20 s timeout, check the new tokens' account, and replace the file only if it
//! still holds the bytes read under the lock. Tokens never reach logs, the
//! status file, errors or return values.

use crate::atomic::replace_file_atomically;
use crate::family::{AuthBytes, read_auth_bytes};
use crate::jsnum::{iso, js_round, parse_date_ms};
use crate::lock::try_acquire_codex_activation_lock;
use crate::planner::{
    CodexRenewalDecision, CodexRenewalEnvironment, CodexRenewalPlanEntry,
    collect_codex_renewal_snapshot, insert_opt, path_text, plan_codex_profile_renewal,
};
use crate::procscan::{home_dir, normalize};
use crate::refresh::{
    CodexRefreshedTokens, CodexTokenFetch, CodexTokenRefreshResult, production_fetch,
    refresh_codex_tokens,
};
use crate::registry::{
    codex_instances_dir, codex_profile_name_error, list_registered_codex_profiles,
};
use crate::status_store::{
    CodexRenewalAttemptOutcome, CodexRenewalRecord, codex_auth_file_version,
    codex_renewal_status_path, next_codex_renewal_record, record, set_record,
    update_codex_renewal_records,
};
use crate::tokens::{check_refreshed_identity, family_markers_kept, merge_refreshed_tokens};
use crate::types::{
    CodexRenewalReason, CodexRenewalState, codex_renewal_state, is_codex_profile_renewal_enabled,
};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

const DEFAULT_GAP_MS: (i64, i64) = (3_000, 12_000);

/// The outcome trail sink: level, event, message, context (no secrets).
pub type CodexRenewalLogSink = Arc<dyn Fn(&str, &str, &str, &Value) + Send + Sync>;

/// The pause between two profiles of one cycle.
pub type RenewalSleep = Arc<dyn Fn(i64) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Everything one cycle or status read needs; every seam is injectable.
#[derive(Clone)]
pub struct CodexProfileRenewalOptions {
    /// Where to look (CCS root, homes, process scan, registry seam).
    pub env: CodexRenewalEnvironment,
    /// Test seam only; the refresh URL is fixed. None uses the production fetch.
    pub fetch: Option<CodexTokenFetch>,
    /// Refresh deadline override.
    pub timeout_ms: Option<u64>,
    /// The environment for the disable switch; None reads the process env.
    pub env_map: Option<HashMap<String, String>>,
    /// Epoch ms clock.
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Jitter source in [0, 1).
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// The inter-profile pause.
    pub sleep: RenewalSleep,
    /// Pause range override (min, max) ms.
    pub gap_ms: Option<(i64, i64)>,
    /// Outcome trail; None logs through tracing.
    pub log: Option<CodexRenewalLogSink>,
    /// Checked before each profile; a refresh in flight is never interrupted.
    pub should_continue: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// The profile being renewed right now.
    pub on_renewing: Option<OnRenewing>,
}

/// The profile-being-renewed callback.
pub type OnRenewing = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// The system epoch-ms clock.
pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A jitter source in [0, 1) from OS entropy (jitter only, never security).
pub fn system_random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

impl Default for CodexProfileRenewalOptions {
    fn default() -> Self {
        Self {
            env: CodexRenewalEnvironment::default(),
            fetch: None,
            timeout_ms: None,
            env_map: None,
            now: Arc::new(system_now_ms),
            random: Arc::new(system_random),
            sleep: Arc::new(|ms| {
                Box::pin(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms.max(0) as u64)).await;
                })
            }),
            gap_ms: None,
            log: None,
            should_continue: None,
            on_renewing: None,
        }
    }
}

/// The disable switch as this call sees it (process env when not injected).
pub fn renewal_enabled(options: &CodexProfileRenewalOptions) -> bool {
    match &options.env_map {
        Some(map) => is_codex_profile_renewal_enabled(map),
        None => {
            let mut map = HashMap::new();
            if let Ok(value) = std::env::var("CCS_CODEX_RENEWAL") {
                map.insert("CCS_CODEX_RENEWAL".to_owned(), value);
            }
            is_codex_profile_renewal_enabled(&map)
        }
    }
}

/// One attempt's DTO: safe fields only.
#[derive(Debug, Clone)]
pub struct CodexRenewalAttempt {
    /// Profile name.
    pub name: String,
    /// State word.
    pub state: CodexRenewalState,
    /// Outcome code.
    pub reason: CodexRenewalReason,
    /// Fixed message.
    pub message: &'static str,
    /// True only when a refresh request was sent.
    pub attempted: bool,
    /// Attempt start, ISO.
    pub at: String,
    /// Backoff or busy retry, ISO.
    pub next_attempt_at: Option<String>,
    /// Access expiry the attempt saw, ISO.
    pub access_expires_at: Option<String>,
}

impl CodexRenewalAttempt {
    /// The attempt in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), Value::String(self.name.clone()));
        out.insert("state".into(), Value::String(self.state.word().into()));
        out.insert("reason".into(), Value::String(self.reason.code().into()));
        out.insert("message".into(), Value::String(self.message.into()));
        out.insert("attempted".into(), Value::Bool(self.attempted));
        out.insert("at".into(), Value::String(self.at.clone()));
        insert_opt(&mut out, "nextAttemptAt", &self.next_attempt_at);
        insert_opt(&mut out, "accessExpiresAt", &self.access_expires_at);
        Value::Object(out)
    }
}

/// One cycle: every plan, the attempts, and the earliest retry for the scheduler.
#[derive(Debug, Clone)]
pub struct CodexRenewalCycle {
    /// Cycle start, ISO.
    pub started_at: String,
    /// Cycle end, ISO.
    pub finished_at: String,
    /// The disable switch at cycle start.
    pub enabled: bool,
    /// Every profile's plan.
    pub plans: Vec<CodexRenewalPlanEntry>,
    /// The due profiles' attempts, in order.
    pub attempts: Vec<CodexRenewalAttempt>,
    /// Earliest backoff or busy retry, for the scheduler.
    pub retry_at: Option<String>,
}

impl CodexRenewalCycle {
    /// The cycle in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("startedAt".into(), Value::String(self.started_at.clone()));
        out.insert("finishedAt".into(), Value::String(self.finished_at.clone()));
        out.insert("enabled".into(), Value::Bool(self.enabled));
        out.insert(
            "plans".into(),
            Value::Array(self.plans.iter().map(|p| p.to_json()).collect()),
        );
        out.insert(
            "attempts".into(),
            Value::Array(self.attempts.iter().map(|a| a.to_json()).collect()),
        );
        insert_opt(&mut out, "retryAt", &self.retry_at);
        Value::Object(out)
    }
}

/// Dashboard status per saved profile, from local files only.
#[derive(Debug, Clone)]
pub struct CodexProfileRenewalProfileStatus {
    /// Profile name.
    pub name: String,
    /// State word.
    pub state: CodexRenewalState,
    /// Outcome code.
    pub reason: CodexRenewalReason,
    /// Fixed message.
    pub message: &'static str,
    /// Last success, ISO.
    pub last_renewed_at: Option<String>,
    /// Last attempt, ISO.
    pub last_attempt_at: Option<String>,
    /// Last outcome code.
    pub last_outcome: Option<String>,
    /// Next retry, ISO (null while disabled).
    pub next_attempt_at: Option<String>,
    /// Access expiry, ISO.
    pub access_expires_at: Option<String>,
}

impl CodexProfileRenewalProfileStatus {
    /// The profile status in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), Value::String(self.name.clone()));
        out.insert("state".into(), Value::String(self.state.word().into()));
        out.insert("reason".into(), Value::String(self.reason.code().into()));
        out.insert("message".into(), Value::String(self.message.into()));
        insert_opt(&mut out, "lastRenewedAt", &self.last_renewed_at);
        insert_opt(&mut out, "lastAttemptAt", &self.last_attempt_at);
        insert_opt(&mut out, "lastOutcome", &self.last_outcome);
        insert_opt(&mut out, "nextAttemptAt", &self.next_attempt_at);
        insert_opt(&mut out, "accessExpiresAt", &self.access_expires_at);
        Value::Object(out)
    }
}

/// The status read's DTO.
#[derive(Debug, Clone)]
pub struct CodexProfileRenewalStatus {
    /// The disable switch.
    pub enabled: bool,
    /// Read time, ISO.
    pub checked_at: String,
    /// Per-profile status.
    pub profiles: Vec<CodexProfileRenewalProfileStatus>,
}

impl CodexProfileRenewalStatus {
    /// The status in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("enabled".into(), Value::Bool(self.enabled));
        out.insert("checkedAt".into(), Value::String(self.checked_at.clone()));
        out.insert(
            "profiles".into(),
            Value::Array(self.profiles.iter().map(|p| p.to_json()).collect()),
        );
        Value::Object(out)
    }
}

fn write_log(
    options: &CodexProfileRenewalOptions,
    attempt: &CodexRenewalAttempt,
    family_markers_kept_flag: Option<bool>,
) {
    let level = if attempt.state == CodexRenewalState::Failed
        || attempt.state == CodexRenewalState::Retrying
    {
        "warn"
    } else {
        "info"
    };
    let mut context = Map::new();
    context.insert("profile".into(), Value::String(attempt.name.clone()));
    context.insert(
        "outcome".into(),
        Value::String(attempt.reason.code().into()),
    );
    context.insert("attempted".into(), Value::Bool(attempt.attempted));
    context.insert("at".into(), Value::String(attempt.at.clone()));
    if let Some(value) = &attempt.next_attempt_at {
        context.insert("nextAttemptAt".into(), Value::String(value.clone()));
    }
    if let Some(value) = &attempt.access_expires_at {
        context.insert("accessExpiresAt".into(), Value::String(value.clone()));
    }
    if let Some(kept) = family_markers_kept_flag {
        context.insert("familyMarkersKept".into(), Value::Bool(kept));
    }
    let context = Value::Object(context);
    // The outcome trail must never break renewal.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &options.log {
        Some(sink) => sink(level, "codex.renewal", attempt.message, &context),
        None if level == "warn" => {
            tracing::warn!(target: "aac_renewal", event = "codex.renewal", "{}", attempt.message);
        }
        None => {
            tracing::info!(target: "aac_renewal", event = "codex.renewal", "{}", attempt.message);
        }
    }));
    let _ = result;
}

fn skipped(reason: CodexRenewalReason) -> CodexRenewalAttemptOutcome {
    CodexRenewalAttemptOutcome {
        reason,
        attempted: false,
        version: None,
        access_expires_at: None,
        family_markers_kept: None,
    }
}

fn registered_names(options: &CodexProfileRenewalOptions) -> Result<Vec<String>, ()> {
    match &options.env.list_profiles {
        Some(seam) => seam(),
        None => list_registered_codex_profiles(&options.env.ccs_dir),
    }
}

async fn renew_under_lock(
    name: &str,
    options: &CodexProfileRenewalOptions,
    codex_home: &std::path::Path,
    now: i64,
) -> Result<CodexRenewalAttemptOutcome, ()> {
    let names = registered_names(options)?;
    if !names.iter().any(|n| n == name) {
        return Ok(skipped(CodexRenewalReason::NoLogin));
    }
    let mut env = options.env.clone();
    env.codex_home = Some(codex_home.to_path_buf());
    let snapshot = collect_codex_renewal_snapshot(&env, Some(&[name.to_owned()]))?;
    let profile = &snapshot.profiles[0];
    let (entry, _) = plan_codex_profile_renewal(&snapshot, profile, now);
    let access_before = profile
        .fingerprint
        .as_ref()
        .and_then(|f| f.access_expires_at);
    if entry.decision != CodexRenewalDecision::Due {
        return Ok(CodexRenewalAttemptOutcome {
            reason: entry.reason,
            attempted: false,
            version: profile.version.clone(),
            access_expires_at: access_before,
            family_markers_kept: None,
        });
    }
    let refresh_token = profile
        .parsed
        .as_ref()
        .and_then(|p| p.get("tokens"))
        .and_then(|t| t.get("refresh_token"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let fetch = options
        .fetch
        .clone()
        .unwrap_or_else(|| production_fetch(options.timeout_ms));
    let result = refresh_codex_tokens(&refresh_token, &fetch, options.timeout_ms).await;
    let failed = |reason: CodexRenewalReason| CodexRenewalAttemptOutcome {
        reason,
        attempted: true,
        version: profile.version.clone(),
        access_expires_at: access_before,
        family_markers_kept: None,
    };
    let tokens: CodexRefreshedTokens = match result {
        CodexTokenRefreshResult::Ok(tokens) => tokens,
        CodexTokenRefreshResult::Dead { .. } => return Ok(failed(CodexRenewalReason::Dead)),
        CodexTokenRefreshResult::Transient { .. } => {
            return Ok(failed(CodexRenewalReason::Transient));
        }
        CodexTokenRefreshResult::InvalidResponse { .. } => {
            return Ok(failed(CodexRenewalReason::InvalidResponse));
        }
    };
    let (identity_reason, identity_fingerprint) = check_refreshed_identity(profile, &tokens, now);
    let Some(after) = identity_fingerprint.filter(|_| identity_reason.is_none()) else {
        return Ok(failed(
            identity_reason.unwrap_or(CodexRenewalReason::InvalidResponse),
        ));
    };
    // Compare-and-swap: only the bytes read under this lock may be replaced.
    let current = read_auth_bytes(&profile.auth_path);
    let regular = std::fs::symlink_metadata(&profile.auth_path)
        .map(|m| m.is_file())
        .unwrap_or(false);
    let unchanged = match (&current, &profile.content) {
        (AuthBytes::Bytes(bytes), Some(content)) => bytes == content,
        _ => false,
    };
    if !regular || !unchanged {
        return Ok(failed(CodexRenewalReason::Changed));
    }
    let merged = merge_refreshed_tokens(
        profile.parsed.as_ref().unwrap_or(&Value::Null),
        &tokens,
        now,
    );
    if replace_file_atomically(&profile.auth_path, &merged).is_err() {
        return Ok(failed(CodexRenewalReason::WriteFailed));
    }
    // The TS path invalidates its dashboard profiles cache here; the Rust
    // profile reads answer from disk per request, so there is nothing to drop.
    let before = profile.fingerprint.as_ref().expect("due profile has one");
    Ok(CodexRenewalAttemptOutcome {
        reason: CodexRenewalReason::Renewed,
        attempted: true,
        version: codex_auth_file_version(&profile.auth_path),
        access_expires_at: after.access_expires_at,
        family_markers_kept: Some(family_markers_kept(before, &after)),
    })
}

/// Renew one saved profile if it is due and every guard passes. Never waits
/// for the activation lock; never panics for provider or file outcomes.
pub async fn renew_codex_profile(
    name: &str,
    options: &CodexProfileRenewalOptions,
) -> CodexRenewalAttempt {
    let now = &options.now;
    let random = &options.random;
    let started_at = now();
    let result = |outcome: &CodexRenewalAttemptOutcome, record: Option<&CodexRenewalRecord>| {
        let attempt = CodexRenewalAttempt {
            name: name.to_owned(),
            state: codex_renewal_state(outcome.reason),
            reason: outcome.reason,
            message: outcome.reason.message(),
            attempted: outcome.attempted,
            at: iso(started_at).unwrap_or_default(),
            next_attempt_at: record.and_then(|r| r.next_attempt_at.clone()),
            access_expires_at: outcome.access_expires_at.and_then(iso),
        };
        write_log(options, &attempt, outcome.family_markers_kept);
        attempt
    };
    if !renewal_enabled(options) {
        return result(&skipped(CodexRenewalReason::Disabled), None);
    }
    if codex_profile_name_error(name).is_some() {
        return result(&skipped(CodexRenewalReason::NoLogin), None);
    }
    let codex_home = normalize(
        &options
            .env
            .codex_home
            .clone()
            .unwrap_or_else(|| home_dir().join(".codex")),
    );
    let lock = crate::status_store::create_private_dir(&codex_home)
        .ok()
        .and_then(|_| try_acquire_codex_activation_lock(&codex_home).ok());
    let outcome = match lock {
        None => {
            let version = codex_auth_file_version(
                &normalize(&codex_instances_dir(&options.env.ccs_dir))
                    .join(name)
                    .join("auth.json"),
            );
            CodexRenewalAttemptOutcome {
                version,
                ..skipped(CodexRenewalReason::ActivationBusy)
            }
        }
        Some(lock) => {
            let outcome = renew_under_lock(name, options, &codex_home, now())
                .await
                .unwrap_or_else(|_| skipped(CodexRenewalReason::Transient));
            drop(lock);
            outcome
        }
    };
    if !outcome.attempted && outcome.reason != CodexRenewalReason::ActivationBusy {
        return result(&outcome, None);
    }
    let mut written: Option<CodexRenewalRecord> = None;
    // History is advisory; the outcome is still logged and returned.
    let update = (|| -> Result<(), ()> {
        let keep = registered_names(options)?;
        let at = started_at;
        let jitter = random();
        let outcome_ref = &outcome;
        let written_slot = &mut written;
        update_codex_renewal_records(
            &codex_renewal_status_path(&options.env.ccs_dir),
            Some(&keep),
            |records| {
                let next =
                    next_codex_renewal_record(record(records, name), outcome_ref, at, jitter);
                *written_slot = Some(next.clone());
                set_record(records, name, next);
            },
        )
    })();
    let _ = update;
    result(&outcome, written.as_ref())
}

fn earliest_retry(
    plans: &[CodexRenewalPlanEntry],
    attempts: &[CodexRenewalAttempt],
) -> Option<String> {
    let times: Vec<i64> = plans
        .iter()
        .filter(|plan| plan.decision == CodexRenewalDecision::Backoff)
        .map(|plan| plan.next_attempt_at.clone())
        .chain(attempts.iter().map(|a| a.next_attempt_at.clone()))
        .flatten()
        .filter_map(|value| parse_date_ms(&value))
        .collect();
    times.iter().min().copied().and_then(iso)
}

/// One cycle: plan every profile, then renew the due ones sequentially.
/// A registry read failure propagates (the service reports a failed cycle).
pub async fn renew_due_codex_profiles(
    options: &CodexProfileRenewalOptions,
) -> Result<CodexRenewalCycle, ()> {
    let now = &options.now;
    let random = &options.random;
    let sleep = &options.sleep;
    let started_at = iso(now()).unwrap_or_default();
    let enabled = renewal_enabled(options);
    if !enabled {
        return Ok(CodexRenewalCycle {
            started_at: started_at.clone(),
            finished_at: started_at,
            enabled,
            plans: Vec::new(),
            attempts: Vec::new(),
            retry_at: None,
        });
    }
    let snapshot = collect_codex_renewal_snapshot(&options.env, None)?;
    let plans: Vec<CodexRenewalPlanEntry> = snapshot
        .profiles
        .iter()
        .map(|profile| plan_codex_profile_renewal(&snapshot, profile, now()).0)
        .collect();
    let mut attempts: Vec<CodexRenewalAttempt> = Vec::new();
    let gap = options.gap_ms.unwrap_or(DEFAULT_GAP_MS);
    // One line per profile per cycle: renew_codex_profile logs the due ones.
    for plan in &plans {
        if plan.decision == CodexRenewalDecision::Due {
            continue;
        }
        let attempt = CodexRenewalAttempt {
            name: plan.name.clone(),
            state: codex_renewal_state(plan.reason),
            reason: plan.reason,
            message: plan.message,
            attempted: false,
            at: started_at.clone(),
            next_attempt_at: plan.next_attempt_at.clone(),
            access_expires_at: plan.access_expires_at.clone(),
        };
        write_log(options, &attempt, None);
    }
    for plan in &plans {
        if plan.decision != CodexRenewalDecision::Due {
            continue;
        }
        if options
            .should_continue
            .as_ref()
            .is_some_and(|check| !check())
        {
            break;
        }
        if !attempts.is_empty() {
            let pause = js_round(gap.0 as f64 + (gap.1 - gap.0) as f64 * random());
            sleep(pause).await;
            if options
                .should_continue
                .as_ref()
                .is_some_and(|check| !check())
            {
                break;
            }
        }
        if let Some(on_renewing) = &options.on_renewing {
            on_renewing(Some(&plan.name));
        }
        let attempt = renew_codex_profile(&plan.name, options).await;
        attempts.push(attempt);
        if let Some(on_renewing) = &options.on_renewing {
            on_renewing(None);
        }
    }
    let retry_at = earliest_retry(&plans, &attempts);
    Ok(CodexRenewalCycle {
        started_at,
        finished_at: iso(now()).unwrap_or_default(),
        enabled,
        plans,
        attempts,
        retry_at,
    })
}

/// Dashboard status per saved profile, from local files only.
pub fn get_codex_profile_renewal_status(
    options: &CodexProfileRenewalOptions,
) -> Result<CodexProfileRenewalStatus, ()> {
    let now = (options.now)();
    let enabled = renewal_enabled(options);
    let snapshot = collect_codex_renewal_snapshot(&options.env, None)?;
    let profiles = snapshot
        .profiles
        .iter()
        .map(|profile| {
            let (plan, _) = plan_codex_profile_renewal(&snapshot, profile, now);
            let reason = if !enabled && plan.decision != CodexRenewalDecision::Dead {
                CodexRenewalReason::Disabled
            } else {
                plan.reason
            };
            CodexProfileRenewalProfileStatus {
                name: plan.name.clone(),
                state: codex_renewal_state(reason),
                reason,
                message: reason.message(),
                last_renewed_at: plan.last_renewed_at.clone(),
                last_attempt_at: plan.last_attempt_at.clone(),
                last_outcome: plan.last_outcome.clone(),
                next_attempt_at: if enabled {
                    plan.next_attempt_at.clone()
                } else {
                    None
                },
                access_expires_at: plan.access_expires_at.clone(),
            }
        })
        .collect();
    Ok(CodexProfileRenewalStatus {
        enabled,
        checked_at: iso(now).unwrap_or_default(),
        profiles,
    })
}

/// One dry-run profile: the plan plus guard details and same-account names.
#[derive(Debug, Clone)]
pub struct CodexRenewalDryRunProfile {
    /// The plan entry.
    pub entry: CodexRenewalPlanEntry,
    /// Whether the saved login has session ids to compare.
    pub has_session_id: bool,
    /// Whether it has sign-in times to compare.
    pub has_auth_time: bool,
    /// Guard details (paths and labels).
    pub guards: crate::planner::CodexRenewalGuards,
    /// Other saved profiles of the same account.
    pub same_account_profiles: Vec<String>,
}

/// The read-only preview (`dryRunCodexProfileRenewal`): no network, no writes,
/// no lock. Token text and digests never appear in the result.
#[derive(Debug, Clone)]
pub struct CodexRenewalDryRun {
    /// Read time, ISO.
    pub generated_at: String,
    /// The disable switch.
    pub enabled_by_environment: bool,
    /// The CCS root.
    pub ccs_dir: std::path::PathBuf,
    /// The instances folder.
    pub instances_dir: std::path::PathBuf,
    /// The shared login home.
    pub codex_home: std::path::PathBuf,
    /// The scanned home folder.
    pub home_dir: std::path::PathBuf,
    /// The process scan state word.
    pub process_scan: &'static str,
    /// Running Codex homes.
    pub process_homes: Vec<std::path::PathBuf>,
    /// Every source's path, kind and state.
    pub sources: Vec<(std::path::PathBuf, &'static str, &'static str)>,
    /// Per-profile previews.
    pub profiles: Vec<CodexRenewalDryRunProfile>,
}

impl CodexRenewalDryRun {
    /// The dry run in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert(
            "generatedAt".into(),
            Value::String(self.generated_at.clone()),
        );
        out.insert(
            "enabledByEnvironment".into(),
            Value::Bool(self.enabled_by_environment),
        );
        out.insert("ccsDir".into(), Value::String(path_text(&self.ccs_dir)));
        out.insert(
            "instancesDir".into(),
            Value::String(path_text(&self.instances_dir)),
        );
        out.insert(
            "codexHome".into(),
            Value::String(path_text(&self.codex_home)),
        );
        out.insert("homeDir".into(), Value::String(path_text(&self.home_dir)));
        out.insert(
            "processScan".into(),
            Value::String(self.process_scan.into()),
        );
        out.insert(
            "processHomes".into(),
            Value::Array(
                self.process_homes
                    .iter()
                    .map(|h| Value::String(path_text(h)))
                    .collect(),
            ),
        );
        out.insert(
            "sources".into(),
            Value::Array(
                self.sources
                    .iter()
                    .map(|(path, kind, state)| {
                        let mut entry = Map::new();
                        entry.insert("path".into(), Value::String(path_text(path)));
                        entry.insert("kind".into(), Value::String((*kind).into()));
                        entry.insert("state".into(), Value::String((*state).into()));
                        Value::Object(entry)
                    })
                    .collect(),
            ),
        );
        out.insert(
            "profiles".into(),
            Value::Array(
                self.profiles
                    .iter()
                    .map(|profile| {
                        let entry = profile.entry.to_json();
                        let obj = entry.as_object().cloned().unwrap_or_default();
                        // TS dry-run profile order: name, decision, state, reason,
                        // message, accessExpiresAt, dueAt, lastRenewedAt, lastOutcome,
                        // nextAttemptAt, hasSessionId, hasAuthTime, guards, sameAccountProfiles.
                        let mut ordered = Map::new();
                        for key in ["name", "decision"] {
                            if let Some(v) = obj.get(key) {
                                ordered.insert(key.into(), v.clone());
                            }
                        }
                        ordered.insert(
                            "state".into(),
                            Value::String(codex_renewal_state(profile.entry.reason).word().into()),
                        );
                        for key in [
                            "reason",
                            "message",
                            "accessExpiresAt",
                            "dueAt",
                            "lastRenewedAt",
                        ] {
                            if let Some(v) = obj.get(key) {
                                ordered.insert(key.into(), v.clone());
                            }
                        }
                        ordered.insert(
                            "lastOutcome".into(),
                            obj.get("lastOutcome").cloned().unwrap_or(Value::Null),
                        );
                        ordered.insert(
                            "nextAttemptAt".into(),
                            obj.get("nextAttemptAt").cloned().unwrap_or(Value::Null),
                        );
                        ordered.insert("hasSessionId".into(), Value::Bool(profile.has_session_id));
                        ordered.insert("hasAuthTime".into(), Value::Bool(profile.has_auth_time));
                        ordered.insert("guards".into(), profile.guards.to_json(true));
                        ordered.insert(
                            "sameAccountProfiles".into(),
                            Value::Array(
                                profile
                                    .same_account_profiles
                                    .iter()
                                    .map(|n| Value::String(n.clone()))
                                    .collect(),
                            ),
                        );
                        Value::Object(ordered)
                    })
                    .collect(),
            ),
        );
        Value::Object(out)
    }
}

/// Read-only preview of saved-login renewal: what each guard would decide and
/// which other login files share or may share a profile's family.
pub fn dry_run_codex_profile_renewal(
    options: &CodexProfileRenewalOptions,
) -> Result<CodexRenewalDryRun, ()> {
    let now = (options.now)();
    let snapshot = collect_codex_renewal_snapshot(&options.env, None)?;
    let profiles = snapshot
        .profiles
        .iter()
        .map(|profile| {
            let (entry, guards) = plan_codex_profile_renewal(&snapshot, profile, now);
            let fingerprint = profile.fingerprint.as_ref();
            let same_account_profiles = snapshot
                .profiles
                .iter()
                .filter(|other| {
                    other.name != profile.name
                        && fingerprint.is_some_and(|f| f.binding.is_some())
                        && other
                            .fingerprint
                            .as_ref()
                            .is_some_and(|f| f.binding.is_some())
                        && fingerprint
                            .zip(other.fingerprint.as_ref())
                            .is_some_and(|(a, b)| crate::family::same_account_or_unknown(a, b))
                })
                .map(|other| other.name.clone())
                .collect();
            CodexRenewalDryRunProfile {
                entry,
                has_session_id: fingerprint.is_some_and(|f| !f.session_ids.is_empty()),
                has_auth_time: fingerprint.is_some_and(|f| !f.auth_times.is_empty()),
                guards,
                same_account_profiles,
            }
        })
        .collect();
    Ok(CodexRenewalDryRun {
        generated_at: iso(now).unwrap_or_default(),
        enabled_by_environment: renewal_enabled(options),
        ccs_dir: normalize(&options.env.ccs_dir),
        instances_dir: normalize(&codex_instances_dir(&options.env.ccs_dir)),
        codex_home: snapshot.codex_home.clone(),
        home_dir: snapshot.home_dir.clone(),
        process_scan: snapshot.process_scan.state.word(),
        process_homes: snapshot
            .process_scan
            .homes
            .iter()
            .map(|h| h.home.clone())
            .collect(),
        sources: snapshot
            .sources
            .iter()
            .map(|s| (s.path.clone(), s.kind.word(), s.state.word()))
            .collect(),
        profiles,
    })
}
