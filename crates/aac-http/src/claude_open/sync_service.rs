//! Port of `claude-history-sync-service.ts`'s orchestration: the optional copy inside one
//! coalesced authenticated Open, its durable marker sequence, the snapshot retention and the
//! "Clean up now" totals. Pre-append skips preserve Open; an uncertain append holds Open.
//! Counts, platform and fixed reason tokens only leave this boundary — never ids, paths,
//! titles or errors.
use super::js::{self, sha, stringify};
use super::manifest::{Profile, list_profiles};
use super::markers::{self, MarkerRefused, PendingMarker};
use super::policy::{ExpectedLauncher, load_policy};
use super::sync::{self, Adapters, AppendPayload, Record, Snapshot};
use serde_json::{Map, Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

const MAX_HELPER_BYTES: usize = 24 * 1024 * 1024;

/// One fixed-helper call over the existing transport (`runClaudeHistoryHelper`'s boundary):
/// the raw response bytes, or an opaque error. Private data lives only in these buffers.
pub trait HelperCall: Send + Sync {
    /// Call the fixed helper on the target computer. `request` is the full validated envelope.
    fn call<'a>(
        &'a self,
        ssh_host: &'a str,
        platform: &'a str,
        profile_id: &'a str,
        request: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + 'a>>;
}

/// Progress callbacks for the Open progress view; counts only, never record contents.
pub trait SyncObserver: Send + Sync {
    /// A bounded batch of the copy is about to be appended.
    fn copying(&self, total_count: u64, confirmed_count: u64);
}

/// A no-op observer (the ordinary Open without progress tracking).
pub struct NoObserver;
impl SyncObserver for NoObserver {
    fn copying(&self, _total_count: u64, _confirmed_count: u64) {}
}

fn skipped(reason: &str) -> Value {
    json!({"status": "skipped", "reason": reason, "createdCount": 0})
}

/// `decodeSnapshot`: the wire rows to exact bytes, with every binding and size guard.
pub fn decode_snapshot(value: Value) -> Result<Snapshot, ()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut fields = value.as_object().ok_or(())?.clone();
    let rows = fields.remove("records").ok_or(())?;
    let rows = rows.as_array().ok_or(())?;
    if rows.len() > 200 {
        return Err(());
    }
    let mut total = 0usize;
    let mut records = Vec::new();
    for row in rows {
        let name = row["name"].as_str().ok_or(())?;
        let base64_text = row["base64"].as_str().ok_or(())?;
        let digest = row["sha256"].as_str().ok_or(())?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || base64_text.len() > 2_666_668
            || !base64_shape(base64_text)
        {
            return Err(());
        }
        let bytes = STANDARD.decode(base64_text).map_err(|_| ())?;
        total += bytes.len();
        if bytes.len() > 2_000_000 || total > 16_000_000 || sha(&bytes) != digest {
            return Err(());
        }
        records.push(Record {
            name: name.to_string(),
            bytes,
            sha256: digest.to_string(),
        });
    }
    Ok(Snapshot { fields, records })
}

/// The service's canonical base64 shape check.
fn base64_shape(text: &str) -> bool {
    let bytes = text.as_bytes();
    let data = |part: &[u8]| {
        part.iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
    };
    // The reference regex splits the padding tail off before the four-char groups.
    let (prefix, tail) = if bytes.ends_with(b"==") || bytes.ends_with(b"=") {
        let n = bytes.len();
        if n < 4 {
            return false;
        }
        (&bytes[..n - 4], &bytes[n - 4..])
    } else {
        (bytes, &[][..])
    };
    if prefix.len() % 4 != 0 || !data(prefix) {
        return false;
    }
    match tail.len() {
        0 => true,
        4 => {
            (data(&tail[..2]) && tail[2] == b'=' && tail[3] == b'=')
                || (data(&tail[..3]) && tail[3] == b'=')
        }
        _ => false,
    }
}

/// `wireSnapshot` for a record list alone (the verify-transcripts request).
fn wire_records(records: &[Record]) -> Value {
    json!({"records": records.iter().map(sync::wire_record).collect::<Vec<_>>()})
}

/// The expected launchers for the policy rebind, from the resolved profile.
fn expected<'a>(profile: &'a Profile, platform: &str) -> Option<ExpectedLauncher<'a>> {
    let launcher = match platform {
        "mac" => profile.mac.as_ref()?,
        _ => profile.windows.as_ref()?,
    };
    Some(ExpectedLauncher {
        ssh_host: launcher.ssh_host.as_deref(),
        profile_path: launcher.profile_path.as_deref(),
        launcher_path: launcher.launcher_path.as_deref(),
    })
}

/// The copy sequence's mutable state: one durable marker per Open, re-armed per batch.
struct Sequence {
    marker: Option<PendingMarker>,
    stop_reason: Option<String>,
}

/// The adapters the core calls, built over the fixed helper transport exactly as the TS
/// service builds its closures.
struct HelperAdapters<'a> {
    ccs_dir: &'a Path,
    profile: &'a Profile,
    profile_id: &'a str,
    platform: &'a str,
    policy: Value,
    helper: &'a dyn HelperCall,
    observer: &'a dyn SyncObserver,
    sequence: std::sync::Mutex<Sequence>,
}

impl HelperAdapters<'_> {
    /// One helper call with the service's envelope, size cap and canonical-receipt checks.
    async fn call(
        &self,
        target: &str,
        mode: &str,
        fields: Map<String, Value>,
    ) -> Result<Value, ()> {
        let launcher = match target {
            "mac" => self.profile.mac.as_ref(),
            _ => self.profile.windows.as_ref(),
        }
        .ok_or(())?;
        let ssh_host = launcher.ssh_host.as_deref().ok_or(())?;
        let mut request = Map::new();
        request.insert("mode".into(), json!(mode));
        request.insert("profileId".into(), json!(self.profile_id));
        request.insert("platform".into(), json!(target));
        request.insert("policy".into(), self.policy.clone());
        request.insert("expectedEmail".into(), json!(self.profile.email));
        request.extend(fields);
        let data = self
            .helper
            .call(ssh_host, target, self.profile_id, Value::Object(request))
            .await?;
        if data.len() > MAX_HELPER_BYTES {
            return Err(());
        }
        let response = js::parse(&data).ok_or(())?;
        // The fixed helper emits canonical ASCII-only append receipts.
        if mode == "append" && data != stringify(&response).as_bytes() {
            return Err(());
        }
        Ok(response)
    }

    fn clean_stop(&self, reason: &str) -> Value {
        if let Ok(mut sequence) = self.sequence.lock() {
            sequence.stop_reason = Some(reason.to_string());
        }
        json!({"status": "refused", "createdCount": 0, "recoveryRequired": false, "ownedFilesRolledBack": true})
    }

    /// The append adapter: policy recheck, marker arm/rearm, progress, append, trusted finish.
    async fn append(&self, payload: AppendPayload<'_>) -> Result<Value, ()> {
        // A changed or removed policy stops before this batch; it never claims an unconfirmed
        // append.
        let current = load_policy(
            self.ccs_dir,
            self.profile_id,
            &self.profile.email,
            expected(self.profile, "mac"),
            expected(self.profile, "windows"),
        );
        let Some(current) = current else {
            return Ok(self.clean_stop("history_policy_changed"));
        };
        if stringify(&current) != stringify(&self.policy) {
            return Ok(self.clean_stop("history_policy_changed"));
        }
        {
            let mut sequence = self.sequence.lock().map_err(|_| ())?;
            let result = match sequence.marker.as_mut() {
                Some(marker) => marker.rearm(),
                None => match PendingMarker::arm(self.ccs_dir, self.profile_id, self.platform) {
                    Ok(marker) => {
                        sequence.marker = Some(marker);
                        Ok(())
                    }
                    Err(reason) => Err(reason),
                },
            };
            if let Err(reason) = result {
                // No remote append was invoked. Existing/unknown markers hold Open; lack of
                // private marker storage alone skips this optional copy.
                if markers::open_held(self.ccs_dir, self.profile_id, self.platform) {
                    return Err(());
                }
                let word = if reason == MarkerRefused::StoreFull {
                    "history_marker_store_full"
                } else {
                    "history_marker_unavailable"
                };
                return Ok(self.clean_stop(word));
            }
            sequence
                .marker
                .as_ref()
                .ok_or(())?
                .assert_bound()
                .map_err(|_| ())?;
        }
        let batch_count = payload.records.len() as u64;
        let total_count = payload.total_count as u64;
        let confirmed_count = payload.confirmed_count as u64;
        if total_count >= batch_count
            && total_count <= 200
            && confirmed_count + batch_count <= total_count
        {
            self.observer.copying(total_count, confirmed_count);
        }
        let mut fields = Map::new();
        fields.insert("expectedTarget".into(), payload.target.wire());
        fields.insert(
            "records".into(),
            wire_records(payload.records)["records"].clone(),
        );
        let result = self.call(self.platform, "append", fields).await?;
        // Only the fixed helper's successful terminal response proves its sole Node mutator
        // finished. Lost/malformed responses retain the durable hold; no automatic retry.
        self.sequence
            .lock()
            .map_err(|_| ())?
            .marker
            .as_mut()
            .ok_or(())?
            .finish(&result)
            .map_err(|_| ())?;
        Ok(result)
    }
}

impl Adapters for HelperAdapters<'_> {
    fn is_target_closed<'a>(
        &'a self,
        _profile_id: &str,
        platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.call(platform, "closed-check", Map::new()).await?;
            Ok(response.is_object() && response["closed"] == json!(true))
        })
    }

    fn read_source<'a>(
        &'a self,
        _profile_id: &str,
        platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Snapshot, ()>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.call(platform, "collect", Map::new()).await?;
            decode_snapshot(response)
        })
    }

    fn read_target<'a>(
        &'a self,
        _profile_id: &str,
        platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Snapshot, ()>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.call(platform, "collect", Map::new()).await?;
            decode_snapshot(response)
        })
    }

    fn verify_transcript_references<'a>(
        &'a self,
        _profile_id: &str,
        platform: &'a str,
        records: &[Record],
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        let fields = wire_records(records);
        Box::pin(async move {
            let mut map = Map::new();
            map.insert("records".into(), fields["records"].clone());
            let response = self.call(platform, "verify-transcripts", map).await?;
            Ok(response.is_object() && response["verified"] == json!(true))
        })
    }

    fn target_protected_state_unchanged<'a>(
        &'a self,
        target: &Snapshot,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        let expected_target = target.wire();
        Box::pin(async move {
            let mut map = Map::new();
            map.insert("expectedTarget".into(), expected_target);
            let response = self.call(self.platform, "protected-check", map).await?;
            Ok(response.is_object() && response["unchanged"] == json!(true))
        })
    }

    fn append_create_only<'a>(
        &'a self,
        payload: AppendPayload<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ()>> + Send + 'a>> {
        Box::pin(self.append(payload))
    }
}

/// `cleanupHistorySnapshots`: retention for one profile on one platform through the pinned
/// helper path. A refused or malformed answer is an error the caller decides about.
pub async fn cleanup_history_snapshots(
    ccs_dir: &Path,
    profile: &Profile,
    platform: &str,
    helper: &dyn HelperCall,
) -> Result<(usize, usize, usize), ()> {
    let id = profile.id.as_deref().ok_or(())?;
    let launcher = match platform {
        "mac" => profile.mac.as_ref(),
        _ => profile.windows.as_ref(),
    }
    .ok_or(())?;
    let ssh_host = launcher.ssh_host.as_deref().ok_or(())?;
    let policy = load_policy(
        ccs_dir,
        id,
        &profile.email,
        expected(profile, "mac"),
        expected(profile, "windows"),
    )
    .ok_or(())?;
    let request = json!({
        "mode": "snapshot-cleanup", "profileId": id, "platform": platform,
        "policy": policy, "expectedEmail": profile.email
    });
    let data = helper.call(ssh_host, platform, id, request).await?;
    if data.len() > MAX_HELPER_BYTES {
        return Err(());
    }
    let response = js::parse(&data).ok_or(())?;
    let counts = |key: &str| -> Option<usize> {
        let names = response.get(key)?.as_array()?;
        if names.len() > 1024
            || names
                .iter()
                .any(|name| !name.as_str().is_some_and(|s| s.len() <= 128))
        {
            return None;
        }
        Some(names.len())
    };
    if !response.is_object() {
        return Err(());
    }
    Ok((
        counts("kept").ok_or(())?,
        counts("deleted").ok_or(())?,
        counts("skipped").ok_or(())?,
    ))
}

/// `cleanupAllHistorySnapshots`: retention for every known profile on each of its platforms.
pub async fn cleanup_all_history_snapshots(
    ccs_dir: &Path,
    helper: &dyn HelperCall,
) -> Result<Value, ()> {
    let mut total = Map::new();
    for key in [
        "profiles", "targets", "kept", "deleted", "skipped", "failed",
    ] {
        total.insert(key.into(), json!(0));
    }
    let profiles = list_profiles(ccs_dir)?;
    for profile in &profiles {
        let Some(id) = &profile.id else {
            continue;
        };
        let _ = id;
        total["profiles"] = json!(total["profiles"].as_u64().unwrap_or(0) + 1);
        for platform in ["mac", "windows"] {
            let present = match platform {
                "mac" => profile.mac.is_some(),
                _ => profile.windows.is_some(),
            };
            if !present {
                continue;
            }
            total["targets"] = json!(total["targets"].as_u64().unwrap_or(0) + 1);
            match cleanup_history_snapshots(ccs_dir, profile, platform, helper).await {
                Ok((kept, deleted, skipped)) => {
                    total["kept"] = json!(total["kept"].as_u64().unwrap_or(0) + kept as u64);
                    total["deleted"] =
                        json!(total["deleted"].as_u64().unwrap_or(0) + deleted as u64);
                    total["skipped"] =
                        json!(total["skipped"].as_u64().unwrap_or(0) + skipped as u64);
                }
                Err(()) => {
                    total["failed"] = json!(total["failed"].as_u64().unwrap_or(0) + 1);
                }
            }
        }
    }
    tracing::info!(target: "aac_http", "claude.history.snapshots_cleaned");
    Ok(Value::Object(total))
}

/// `synchronizeClaudeHistoryBeforeOpen`: called only inside the existing coalesced
/// authenticated Open. This does not open/resume/stop an app or write a transcript.
pub async fn synchronize_before_open(
    ccs_dir: &Path,
    profile: &Profile,
    platform: &str,
    helper: &dyn HelperCall,
    observer: &dyn SyncObserver,
    snapshot_cleanup_auto: bool,
) -> Value {
    let outcome = run_sync(
        ccs_dir,
        profile,
        platform,
        helper,
        observer,
        snapshot_cleanup_auto,
    )
    .await;
    outcome.unwrap_or_else(|| skipped("unavailable"))
}

async fn run_sync(
    ccs_dir: &Path,
    profile: &Profile,
    platform: &str,
    helper: &dyn HelperCall,
    observer: &dyn SyncObserver,
    snapshot_cleanup_auto: bool,
) -> Option<Value> {
    let Some(id) = profile.id.as_deref() else {
        return Some(skipped("history_policy_unverified"));
    };
    if markers::open_held(ccs_dir, id, platform) {
        return Some(json!({
            "status": "refused", "reason": "create_only_transaction_unconfirmed",
            "createdCount": 0, "recoveryRequired": true
        }));
    }
    let Some(policy) = load_policy(
        ccs_dir,
        id,
        &profile.email,
        expected(profile, "mac"),
        expected(profile, "windows"),
    ) else {
        return Some(skipped("history_policy_unverified"));
    };
    let adapters = HelperAdapters {
        ccs_dir,
        profile,
        profile_id: id,
        platform,
        policy: policy.clone(),
        helper,
        observer,
        sequence: std::sync::Mutex::new(Sequence {
            marker: None,
            stop_reason: None,
        }),
    };
    let result = sync::synchronize_before_profile_open(
        id,
        platform,
        &policy,
        &adapters,
        &sync::CopyLimits::default(),
    )
    .await;
    // After each copy that created records, retention runs once for this profile. It never
    // fails the copy or Open; Settings can turn it off, and "Clean up now" runs it on demand.
    if result["createdCount"].as_u64().unwrap_or(0) > 0 && snapshot_cleanup_auto {
        match cleanup_history_snapshots(ccs_dir, profile, platform, helper).await {
            Ok((kept, deleted, skipped)) => {
                tracing::info!(target: "aac_http", platform, kept, deleted, skipped, "claude.history.snapshots_retained");
            }
            Err(()) => {
                tracing::warn!(target: "aac_http", platform, "claude.history.snapshots_retain_failed");
            }
        }
    }
    let stop_reason = adapters
        .sequence
        .lock()
        .ok()
        .and_then(|sequence| sequence.stop_reason.clone());
    if let Some(reason) = stop_reason
        && result["status"] == json!("refused")
        && result["reason"] == json!("create_only_transaction_refused")
        && result["recoveryRequired"] != json!(true)
    {
        let full = reason == "history_marker_store_full";
        if full {
            tracing::warn!(target: "aac_http", reason, platform, "claude.history.marker_store_full");
        } else {
            tracing::info!(target: "aac_http", reason, platform, "claude.history.copy_stopped");
        }
        let mut out = result.as_object().cloned()?;
        out.insert("reason".into(), json!(reason));
        return Some(Value::Object(out));
    }
    if result["status"] == json!("partial") {
        tracing::info!(target: "aac_http", platform, "claude.history.copy_budget_reached");
    }
    // The marker releases on every path (the core's finally).
    if let Ok(mut sequence) = adapters.sequence.lock()
        && let Some(marker) = sequence.marker.as_mut()
    {
        marker.release();
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::base64_shape;

    #[test]
    fn base64_shape_matches_the_reference_regex() {
        assert!(base64_shape(""));
        assert!(base64_shape("QUJD"));
        assert!(base64_shape("QUI="));
        assert!(base64_shape("QQ=="));
        assert!(base64_shape("QUJDRA=="));
        assert!(base64_shape("QUJDREU="));
        assert!(base64_shape("+/+/"));
        assert!(!base64_shape("A"));
        assert!(!base64_shape("AB"));
        assert!(!base64_shape("ABC"));
        assert!(!base64_shape("===="));
        assert!(!base64_shape("QUJD="));
        assert!(!base64_shape("QU*="));
        assert!(!base64_shape("QQ==="));
        assert!(!base64_shape("QUJDRA==="));
    }
}
