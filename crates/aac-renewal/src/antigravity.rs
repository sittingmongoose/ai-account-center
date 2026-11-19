//! Antigravity live-login renewal (`src/antigravity/live-login-renewal.ts`):
//! Sign in again on the live Antigravity login, in place (switching was removed
//! on 2026-11-13). Called by the lifecycle's importSignIn under the registry
//! lock, after the new credential was verified as the profile's own Google
//! identity. The seams here are what that orchestration supplies.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::LazyLock;

const MAX_CREDENTIAL_BYTES: usize = 16_384;

/// The native fallback credential file under the user's home.
pub const ANTIGRAVITY_NATIVE_TOKEN: &str = ".gemini/antigravity-cli/antigravity-oauth-token";

/// The live native login file path under a home.
pub fn native_token_path(home: &Path) -> PathBuf {
    home.join(ANTIGRAVITY_NATIVE_TOKEN)
}

/// Whether the live native login file exists under `home`; a check that cannot
/// run is an error (the caller treats it as "leave it alone").
pub fn live_login_file_exists(home: &Path) -> Result<bool, std::io::Error> {
    match std::fs::symlink_metadata(native_token_path(home)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Driver-private native data. Never serialized into an HTTP response.
#[derive(Debug, Clone)]
pub struct NativeCredential {
    /// The credential format word.
    pub format: String,
    /// The raw credential bytes.
    pub bytes: Vec<u8>,
}

/// Authoritative provider/runtime identity, not an unverified JWT claim.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    /// The Google account email.
    pub email: String,
    /// The provider subject.
    pub subject: String,
    /// The plan word, when known.
    pub plan: Option<String>,
    /// Verification time, ISO.
    pub verified_at: String,
    /// `provider-userinfo` or `native-runtime`.
    pub source: String,
}

/// The private-storage failure codes the guards throw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateStorageError {
    /// The record is corrupt.
    Corrupt,
}

/// `credentialFingerprint`: SHA-256 hex of the credential bytes.
pub fn credential_fingerprint(credential: &NativeCredential) -> String {
    Sha256::digest(&credential.bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

static EMAIL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[^\s@\x00-\x1f]+@[^\s@\x00-\x1f]+\.[^\s@\x00-\x1f]+$")
        .expect("static pattern")
});

/// `canonicalEmail`: a bounded, control-free address, lowercased.
pub fn canonical_email(email: &str) -> Result<String, PrivateStorageError> {
    if email.encode_utf16().count() > 254 || !EMAIL_RE.is_match(email) {
        return Err(PrivateStorageError::Corrupt);
    }
    Ok(email.to_lowercase())
}

/// `identityKey`: SHA-256 of `["antigravity", canonicalEmail, subject]`.
pub fn identity_key(identity: &VerifiedIdentity) -> Result<String, PrivateStorageError> {
    let email = canonical_email(&identity.email)?;
    let payload = Value::Array(vec![
        Value::String("antigravity".into()),
        Value::String(email),
        Value::String(identity.subject.clone()),
    ]);
    Ok(Sha256::digest(payload.to_string().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn control_free(text: &str) -> bool {
    !text.chars().any(|c| ('\u{0}'..='\u{1f}').contains(&c))
}

/// `checkedIdentity`: the verified identity's shape, with the email canonical
/// and the verification time normalized to ISO.
pub fn checked_identity(
    identity: &VerifiedIdentity,
) -> Result<VerifiedIdentity, PrivateStorageError> {
    let email = canonical_email(&identity.email)?;
    let subject_ok = !identity.subject.is_empty()
        && identity.subject.len() <= 256
        && control_free(&identity.subject);
    let plan_ok = match &identity.plan {
        None => true,
        Some(plan) => plan.len() <= 128 && control_free(plan),
    };
    let verified_ok = crate::jsnum::parse_date_ms(&identity.verified_at).is_some();
    let source_ok = identity.source == "provider-userinfo" || identity.source == "native-runtime";
    if !subject_ok || !plan_ok || !verified_ok || !source_ok {
        return Err(PrivateStorageError::Corrupt);
    }
    Ok(VerifiedIdentity {
        email,
        subject: identity.subject.clone(),
        plan: identity.plan.clone(),
        verified_at: crate::jsnum::iso(
            crate::jsnum::parse_date_ms(&identity.verified_at).unwrap_or_default(),
        )
        .unwrap_or_default(),
        source: identity.source.clone(),
    })
}

/// The live native login, as the lifecycle reaches it.
pub trait LiveLoginAccess: Send + Sync {
    /// Whether the live login file exists; a check that cannot run errors.
    fn exists(&self) -> Result<bool, ()>;
    /// Read the live credential.
    fn read(&self) -> Pin<Box<dyn Future<Output = Result<NativeCredential, ()>> + Send + '_>>;
    /// Verify the credential with Google.
    fn validate(
        &self,
        credential: NativeCredential,
    ) -> Pin<Box<dyn Future<Output = Result<VerifiedIdentity, ()>> + Send + '_>>;
    /// Install only while the live login still has `expected` (atomic exchange).
    fn install(
        &self,
        credential: NativeCredential,
        expected: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>>;
}

/// A saved profile record and its place in the list, as restore puts it back.
#[derive(Debug, Clone)]
pub struct SavedProfileSnapshot {
    /// The record's index in the registry list.
    pub index: usize,
    /// The full profile record (the lifecycle's shape).
    pub record: Value,
}

/// The profile fields the live-renewal guards read.
#[derive(Debug, Clone)]
pub struct RenewalProfileRef {
    /// The profile id.
    pub id: String,
    /// Its identity key.
    pub identity_key: String,
}

/// The registry seams the live-renewal guards use (the lifecycle holds the lock).
pub trait RenewalRegistry: Send + Sync {
    /// The saved credential's fingerprint, when the profile is saved.
    fn saved_revision(&self, profile_id: &str) -> Option<String>;
    /// The saved Ubuntu credential.
    fn read_credential(&self, profile_id: &str) -> Result<NativeCredential, ()>;
    /// Put back the record a failed live install replaced.
    fn restore_profile(&self, snapshot: &SavedProfileSnapshot) -> Result<(), ()>;
}

/// Both credentials hold the same Google grant: the same non-empty refresh
/// token, compared in constant time. Anything unparsable is not the same.
pub fn same_grant(left: &NativeCredential, right: &NativeCredential) -> bool {
    match (grant_digest(left), grant_digest(right)) {
        (Some(a), Some(b)) => constant_time_eq(&a, &b),
        _ => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

fn grant_digest(credential: &NativeCredential) -> Option<Vec<u8>> {
    if credential.bytes.is_empty() || credential.bytes.len() > MAX_CREDENTIAL_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(&credential.bytes);
    let value: Value = serde_json::from_str(&text).ok()?;
    let token = value.as_object()?.get("token")?.as_object()?;
    let refresh = token.get("refresh_token")?.as_str()?;
    if refresh.is_empty() {
        return None;
    }
    Some(Sha256::digest(refresh.as_bytes()).to_vec())
}

/// The live native login's fingerprint when it holds this profile's account,
/// read fresh now, or None to leave it alone: no live login, one that cannot be
/// read, or another account. It is this account when its bytes are the
/// profile's saved credential, when Google verifies it as the profile's
/// identity, or, when Google cannot verify it (an expired or revoked login),
/// when it holds the same Google grant (refresh token) as the saved credential.
pub async fn live_login_to_renew(
    live: &dyn LiveLoginAccess,
    registry: &dyn RenewalRegistry,
    profile: &RenewalProfileRef,
) -> Option<String> {
    let native = match live.exists() {
        Ok(true) => live.read().await.ok()?,
        _ => return None,
    };
    let fingerprint = credential_fingerprint(&native);
    if registry.saved_revision(&profile.id).as_deref() == Some(fingerprint.as_str()) {
        return Some(fingerprint);
    }
    if let Ok(identity) = live.validate(native.clone()).await {
        return match checked_identity(&identity) {
            Ok(checked) => match identity_key(&checked) {
                Ok(key) if key == profile.identity_key => Some(fingerprint),
                _ => None,
            },
            Err(_) => None,
        };
    }
    // Google could not verify it: only the same grant still names this account.
    let saved = registry.read_credential(&profile.id).ok()?;
    if same_grant(&native, &saved) {
        Some(fingerprint)
    } else {
        None
    }
}

/// The live-install outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRenewalOutcome {
    /// Both the profile and the live login hold the new credential.
    Renewed,
    /// The live install failed and the previous profile record went back.
    WriteFailed,
    /// The profile has the new credential, the live login the old one (both
    /// the same Google account); only a failed restore reports this.
    LiveLoginNotUpdated,
}

impl LiveRenewalOutcome {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Renewed => "renewed",
            Self::WriteFailed => "write_failed",
            Self::LiveLoginNotUpdated => "live_login_not_updated",
        }
    }
}

/// Commit a Sign in again that renews the live login too: the saved profile
/// first (`save`), then the live login through the guarded store's atomic
/// exchange. When the install fails, the live login is read again: if it
/// already holds the new credential, both are updated; otherwise the previous
/// profile record goes back (write_failed). Only when that also fails does the
/// sign-in report live_login_not_updated. `save` errors when the profile itself
/// cannot be saved (nothing changed).
pub async fn renew_live_login(
    live: &dyn LiveLoginAccess,
    registry: &dyn RenewalRegistry,
    previous: &SavedProfileSnapshot,
    credential: NativeCredential,
    expected: &str,
    save: impl FnOnce() -> Result<(), ()>,
) -> Result<LiveRenewalOutcome, ()> {
    save()?;
    if live
        .install(credential.clone(), expected.to_owned())
        .await
        .is_ok()
    {
        return Ok(LiveRenewalOutcome::Renewed);
    }
    // Checked below: the exchange may have finished before the failure.
    if let Ok(current) = live.read().await
        && credential_fingerprint(&current) == credential_fingerprint(&credential)
    {
        return Ok(LiveRenewalOutcome::Renewed);
    }
    // Unknown: treated as not installed.
    match registry.restore_profile(previous) {
        Err(()) => Ok(LiveRenewalOutcome::LiveLoginNotUpdated),
        Ok(()) => Ok(LiveRenewalOutcome::WriteFailed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn credential(refresh: &str) -> NativeCredential {
        NativeCredential {
            format: "native".into(),
            bytes: serde_json::json!({"token": {"refresh_token": refresh}})
                .to_string()
                .into_bytes(),
        }
    }

    #[test]
    fn same_grant_rules() {
        assert!(same_grant(&credential("rt-a"), &credential("rt-a")));
        assert!(!same_grant(&credential("rt-a"), &credential("rt-b")));
        let empty = NativeCredential {
            format: "native".into(),
            bytes: Vec::new(),
        };
        assert!(!same_grant(&empty, &empty));
        let broken = NativeCredential {
            format: "native".into(),
            bytes: b"{not json".to_vec(),
        };
        assert!(!same_grant(&broken, &credential("rt-a")));
        let no_refresh = NativeCredential {
            format: "native".into(),
            bytes: serde_json::json!({"token": {}}).to_string().into_bytes(),
        };
        assert!(!same_grant(&no_refresh, &no_refresh));
        let oversized = NativeCredential {
            format: "native".into(),
            bytes: vec![b'x'; MAX_CREDENTIAL_BYTES + 1],
        };
        assert!(!same_grant(&oversized, &oversized));
    }

    #[test]
    fn identity_key_and_checked_identity() {
        let identity = VerifiedIdentity {
            email: "A@Example.com".into(),
            subject: "sub-1".into(),
            plan: None,
            verified_at: "2026-10-08T12:00:00Z".into(),
            source: "provider-userinfo".into(),
        };
        let checked = checked_identity(&identity).expect("checked");
        assert_eq!(checked.email, "a@example.com");
        assert_eq!(checked.verified_at, "2026-10-08T12:00:00.000Z");
        let key = identity_key(&checked).expect("key");
        let expected = Sha256::digest(
            Value::Array(vec![
                Value::String("antigravity".into()),
                Value::String("a@example.com".into()),
                Value::String("sub-1".into()),
            ])
            .to_string()
            .as_bytes(),
        );
        assert_eq!(
            key,
            expected
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let mut bad = identity.clone();
        bad.source = "jwt".into();
        assert!(checked_identity(&bad).is_err());
        let mut bad = identity.clone();
        bad.email = "not-an-email".into();
        assert!(checked_identity(&bad).is_err());
    }

    struct FakeLive {
        credential: Mutex<Option<NativeCredential>>,
        exists: bool,
        validate_ok: Option<VerifiedIdentity>,
        install_ok: bool,
        installed: Mutex<Vec<NativeCredential>>,
    }

    impl LiveLoginAccess for FakeLive {
        fn exists(&self) -> Result<bool, ()> {
            Ok(self.exists)
        }
        fn read(&self) -> Pin<Box<dyn Future<Output = Result<NativeCredential, ()>> + Send + '_>> {
            Box::pin(async move { self.credential.lock().expect("lock").clone().ok_or(()) })
        }
        fn validate(
            &self,
            _credential: NativeCredential,
        ) -> Pin<Box<dyn Future<Output = Result<VerifiedIdentity, ()>> + Send + '_>> {
            let result = self.validate_ok.clone().ok_or(());
            Box::pin(async move { result })
        }
        fn install(
            &self,
            credential: NativeCredential,
            _expected: String,
        ) -> Pin<Box<dyn Future<Output = Result<(), ()>> + Send + '_>> {
            let ok = self.install_ok;
            Box::pin(async move {
                if ok {
                    self.installed
                        .lock()
                        .expect("lock")
                        .push(credential.clone());
                    *self.credential.lock().expect("lock") = Some(credential);
                    Ok(())
                } else {
                    Err(())
                }
            })
        }
    }

    struct FakeRegistry {
        revision: Option<String>,
        saved: Option<NativeCredential>,
        restore_ok: bool,
        restored: Mutex<usize>,
    }

    impl RenewalRegistry for FakeRegistry {
        fn saved_revision(&self, _profile_id: &str) -> Option<String> {
            self.revision.clone()
        }
        fn read_credential(&self, _profile_id: &str) -> Result<NativeCredential, ()> {
            self.saved.clone().ok_or(())
        }
        fn restore_profile(&self, _snapshot: &SavedProfileSnapshot) -> Result<(), ()> {
            *self.restored.lock().expect("lock") += 1;
            if self.restore_ok { Ok(()) } else { Err(()) }
        }
    }

    fn identity() -> VerifiedIdentity {
        VerifiedIdentity {
            email: "a@example.com".into(),
            subject: "sub-1".into(),
            plan: None,
            verified_at: "2026-10-08T12:00:00.000Z".into(),
            source: "provider-userinfo".into(),
        }
    }

    fn profile() -> RenewalProfileRef {
        RenewalProfileRef {
            id: "p1".into(),
            identity_key: identity_key(&identity()).expect("key"),
        }
    }

    #[tokio::test]
    async fn live_login_to_renew_rules() {
        let cred = credential("rt-live");
        let fingerprint = credential_fingerprint(&cred);
        // No live login.
        let live = FakeLive {
            credential: Mutex::new(None),
            exists: false,
            validate_ok: None,
            install_ok: true,
            installed: Mutex::new(Vec::new()),
        };
        let registry = FakeRegistry {
            revision: None,
            saved: None,
            restore_ok: true,
            restored: Mutex::new(0),
        };
        assert!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .is_none()
        );
        // The saved revision itself.
        let live = FakeLive {
            credential: Mutex::new(Some(cred.clone())),
            exists: true,
            validate_ok: None,
            install_ok: true,
            installed: Mutex::new(Vec::new()),
        };
        let registry = FakeRegistry {
            revision: Some(fingerprint.clone()),
            saved: None,
            restore_ok: true,
            restored: Mutex::new(0),
        };
        assert_eq!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .as_deref(),
            Some(fingerprint.as_str())
        );
        // Google verifies the profile's identity.
        let registry = FakeRegistry {
            revision: None,
            saved: None,
            restore_ok: true,
            restored: Mutex::new(0),
        };
        let live = FakeLive {
            credential: Mutex::new(Some(cred.clone())),
            exists: true,
            validate_ok: Some(identity()),
            install_ok: true,
            installed: Mutex::new(Vec::new()),
        };
        assert!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .is_some()
        );
        // Google verifies another account.
        let mut other = identity();
        other.subject = "sub-2".into();
        let live = FakeLive {
            validate_ok: Some(other),
            ..live
        };
        assert!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .is_none()
        );
        // Google cannot verify: the same grant still names the account.
        let live = FakeLive {
            credential: Mutex::new(Some(cred.clone())),
            exists: true,
            validate_ok: None,
            install_ok: true,
            installed: Mutex::new(Vec::new()),
        };
        let registry = FakeRegistry {
            revision: None,
            saved: Some(credential("rt-live")),
            restore_ok: true,
            restored: Mutex::new(0),
        };
        assert!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .is_some()
        );
        let registry = FakeRegistry {
            revision: None,
            saved: Some(credential("rt-other")),
            restore_ok: true,
            restored: Mutex::new(0),
        };
        assert!(
            live_login_to_renew(&live, &registry, &profile())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn renew_live_login_outcomes() {
        let new_cred = credential("rt-new");
        let snapshot = SavedProfileSnapshot {
            index: 0,
            record: serde_json::json!({"id": "p1"}),
        };
        // Happy path.
        let live = FakeLive {
            credential: Mutex::new(Some(credential("rt-old"))),
            exists: true,
            validate_ok: None,
            install_ok: true,
            installed: Mutex::new(Vec::new()),
        };
        let registry = FakeRegistry {
            revision: None,
            saved: None,
            restore_ok: true,
            restored: Mutex::new(0),
        };
        assert_eq!(
            renew_live_login(
                &live,
                &registry,
                &snapshot,
                new_cred.clone(),
                "fp-old",
                || Ok(())
            )
            .await
            .expect("outcome"),
            LiveRenewalOutcome::Renewed
        );
        // Install fails but the exchange finished: still renewed.
        let live = FakeLive {
            credential: Mutex::new(Some(new_cred.clone())),
            exists: true,
            validate_ok: None,
            install_ok: false,
            installed: Mutex::new(Vec::new()),
        };
        assert_eq!(
            renew_live_login(
                &live,
                &registry,
                &snapshot,
                new_cred.clone(),
                "fp-old",
                || Ok(())
            )
            .await
            .expect("outcome"),
            LiveRenewalOutcome::Renewed
        );
        // Install fails, live unchanged: restore -> write_failed.
        let live = FakeLive {
            credential: Mutex::new(Some(credential("rt-old"))),
            exists: true,
            validate_ok: None,
            install_ok: false,
            installed: Mutex::new(Vec::new()),
        };
        assert_eq!(
            renew_live_login(
                &live,
                &registry,
                &snapshot,
                new_cred.clone(),
                "fp-old",
                || Ok(())
            )
            .await
            .expect("outcome"),
            LiveRenewalOutcome::WriteFailed
        );
        assert_eq!(*registry.restored.lock().expect("lock"), 1);
        // Restore also fails: live_login_not_updated.
        let registry = FakeRegistry {
            revision: None,
            saved: None,
            restore_ok: false,
            restored: Mutex::new(0),
        };
        assert_eq!(
            renew_live_login(
                &live,
                &registry,
                &snapshot,
                new_cred.clone(),
                "fp-old",
                || Ok(())
            )
            .await
            .expect("outcome"),
            LiveRenewalOutcome::LiveLoginNotUpdated
        );
        // save fails: nothing changed, the error propagates.
        assert!(
            renew_live_login(&live, &registry, &snapshot, new_cred, "fp-old", || Err(()))
                .await
                .is_err()
        );
    }
}
