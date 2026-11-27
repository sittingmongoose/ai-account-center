//! Native Antigravity profile host: live login proof, quota collection and registry reads.
//! `profile_reads.rs` answers `/api/antigravity/profiles(/quotas)` from here.
//!
//! Fixture tests use invented homes; production reads no script and starts no Python worker.

use crate::antigravity_quota::{Cooldowns, QuotaRequest, QuotaService, public_quotas};
use crate::antigravity_runtime::{
    LiveIdentity, RegistryRead, empty_inventory, inventory_from_state, read_registry, sha256_hex,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// One identity read's cache: the four stored paths' identity tuple keys a two-second reuse.
struct IdentityCache {
    key: String,
    identity: LiveIdentity,
    expires: i64,
}

/// The process's mutable Antigravity state.
struct State {
    identity: Option<IdentityCache>,
    service: QuotaService,
    cooldowns: Cooldowns,
}

/// The process's one Antigravity host per CCS root (Accounts and Profiles share it).
fn hosts() -> &'static Mutex<HashMap<PathBuf, Arc<Host>>> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Host>>>> = OnceLock::new();
    HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn host_key(ccs: &Path) -> PathBuf {
    ccs.canonicalize().unwrap_or_else(|_| ccs.to_path_buf())
}

/// The Antigravity profile host for one CCS scope.
pub struct Host {
    ccs: PathBuf,
    home: PathBuf,
    state: Mutex<State>,
}

impl Host {
    /// Native Rust host for one CCS scope and live user home (zero Python workers).
    pub fn native(ccs: &Path, home: &Path) -> Self {
        Self {
            ccs: ccs.to_path_buf(),
            home: home.to_path_buf(),
            state: Mutex::new(State {
                identity: None,
                service: QuotaService::default(),
                cooldowns: Cooldowns::default(),
            }),
        }
    }

    /// The long-lived host for this CCS root: Accounts and Profiles share one quota owner.
    pub fn shared(ccs: &Path) -> Option<Arc<Self>> {
        let key = host_key(ccs);
        {
            let map = hosts().lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(host) = map.get(&key) {
                return Some(Arc::clone(host));
            }
        }
        let host = Arc::new(Self::from_root(ccs)?);
        let mut map = hosts().lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(existing) = map.get(&key) {
            return Some(Arc::clone(existing));
        }
        map.insert(key, Arc::clone(&host));
        Some(host)
    }

    /// The service's own construction: the native Rust host for this CCS scope and home.
    pub fn from_root(ccs: &Path) -> Option<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
        Some(Self::native(ccs, &home))
    }

    /// `readInventoryIdentity`: the live login's own identity, cached by the four stored paths'
    /// identity for two seconds. `None` is the TS safe failure.
    pub fn live_identity(&self, now: i64) -> Option<LiveIdentity> {
        let key = file_identity(&self.home)?;
        {
            let state = self.state.lock().ok()?;
            if let Some(cached) = &state.identity
                && cached.key == key
                && now < cached.expires
            {
                return Some(cached.identity.clone());
            }
        }
        let (credential, _) = self.read_native_credential()?;
        let identity = self.validate_credential(&credential)?;
        if file_identity(&self.home)? != key {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        state.identity = Some(IdentityCache {
            key,
            identity: identity.clone(),
            expires: now + 2000,
        });
        Some(identity)
    }

    /// Read and verify the native credential for a lifecycle operation without cache reuse.
    pub(crate) fn read_native_credential(&self) -> Option<(Vec<u8>, String)> {
        let snap = aac_files::antigravity_store::UbuntuFileStore::new(&self.home)
            .read_current()
            .ok()?;
        let raw = snap.credential.raw().to_vec();
        let rev = snap.credential.revision().to_owned();
        if raw.is_empty() || raw.len() > 16_384 || sha256_hex(&raw) != rev {
            return None;
        }
        Some((raw, rev))
    }

    pub(crate) fn validate_credential(&self, credential: &[u8]) -> Option<LiveIdentity> {
        crate::antigravity_worker::validate_credential_live(&self.home, credential)
    }

    pub(crate) fn install_native_credential(&self, credential: &[u8], expected: &str) -> bool {
        let store = aac_files::antigravity_store::UbuntuFileStore::new(&self.home);
        let Ok(before) = store.read_current() else {
            return false;
        };
        if before.credential.revision() != expected {
            return false;
        }
        let Ok(replacement) =
            aac_files::antigravity_store::PrivateCredential::new(credential.to_vec())
        else {
            return false;
        };
        store
            .install(&replacement, &before)
            .is_ok_and(|r| r.installed.credential.revision() == sha256_hex(credential))
    }

    /// The public inventory for this CCS scope.
    pub fn inventory(&self, now: i64) -> Option<Value> {
        let state = match read_registry(&self.ccs) {
            RegistryRead::Absent => return Some(empty_inventory()),
            RegistryRead::Available(state) => state,
            RegistryRead::Unreadable => return None,
        };
        let live = self.live_identity(now);
        inventory_from_state(&self.ccs, &state, live.as_ref(), now)
    }

    /// The public quotas answer from the runtime's own accounts.
    pub fn quotas(&self, refresh: bool, now: i64) -> Value {
        let Some(accounts) = self.accounts(refresh, now) else {
            return Value::Null;
        };
        public_quotas(&accounts, now, now).unwrap_or(Value::Null)
    }

    pub(crate) fn accounts(&self, refresh: bool, now: i64) -> Option<Vec<Value>> {
        let state = match read_registry(&self.ccs) {
            RegistryRead::Absent => return Some(Vec::new()),
            RegistryRead::Available(state) => state,
            RegistryRead::Unreadable => return None,
        };
        let live = self.live_identity(now);
        let mut guard = self.state.lock().ok()?;
        let guard = &mut *guard;
        let registry = &state;
        let mut collect = |profile_id: &str| -> Option<Value> {
            let record = registry.profile(profile_id)?;
            let bytes = read_credential_bytes(&self.ccs, record)?;
            let request = QuotaRequest {
                profile_id: &record.id,
                email: &record.email,
                identity_key: &record.identity_key,
                credential_revision: &record.credential.fingerprint,
                identity: &record.credential.identity,
                credential_format: "antigravity-consumer-json",
                credential_bytes: &bytes,
            };
            crate::antigravity_worker::collect_quota_live(&self.home, &request)
        };
        let read = crate::antigravity_quota::Read {
            ccs: &self.ccs,
            state: &state,
            live: live.as_ref(),
            now,
        };
        Some(
            guard
                .service
                .get_accounts(&read, &mut collect, &mut guard.cooldowns, refresh),
        )
    }
}

fn read_credential_bytes(
    ccs: &Path,
    record: &crate::antigravity_runtime::ProfileRecord,
) -> Option<Vec<u8>> {
    let file = ccs
        .join("antigravity-instances")
        .join(&record.id)
        .join("ubuntu")
        .join(format!("credential-{}.bin", record.credential.fingerprint));
    let bytes = match aac_files::read::read(
        &file,
        &aac_files::read::ReadPolicy::capped(16_384)
            .leaf(aac_files::read::Leaf::Refuse)
            .owner(aac_files::read::Owner::Mine)
            .mode(aac_files::read::Mode::Private),
    ) {
        aac_files::read::Read::Bytes(bytes, _) => bytes,
        _ => return None,
    };
    (sha256_hex(&bytes) == record.credential.fingerprint).then_some(bytes)
}

/// The four stored paths' identity tuple (`native-credential-transport.ts` `fileIdentity`).
fn file_identity(home: &Path) -> Option<String> {
    let parts = [
        home.to_path_buf(),
        home.join(".gemini"),
        home.join(".gemini/antigravity-cli"),
        home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
    ];
    let mut identities = Vec::with_capacity(parts.len());
    for (index, path) in parts.iter().enumerate() {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let last = index == 3;
        if last {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return None;
            }
            let size = meta.len();
            if size == 0 || size > 16_384 {
                return None;
            }
        } else if !meta.is_dir() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = aac_files::path::uid();
            if meta.uid() != uid {
                return None;
            }
            if last {
                if meta.mode() & 0o777 != 0o600 || meta.nlink() != 1 {
                    return None;
                }
                identities.push(format!(
                    "{}:{}:{}:{}:{}:{}:{}:{}",
                    meta.dev(),
                    meta.ino(),
                    meta.len(),
                    meta.mtime(),
                    meta.mtime_nsec(),
                    meta.mode(),
                    meta.uid(),
                    meta.nlink()
                ));
            } else {
                // native-credential-transport.ts: the parent folders must only not be
                // world-writable (mode & 2); they are not required to be exactly 0700.
                if meta.mode() & 0o002 != 0 {
                    return None;
                }
                identities.push(format!(
                    "{}:{}:{}:{}",
                    meta.dev(),
                    meta.ino(),
                    meta.mode(),
                    meta.uid()
                ));
            }
            continue;
        }
        #[cfg(not(unix))]
        identities.push(format!("{}:{}", meta.is_dir(), meta.len()));
    }
    Some(identities.join("/"))
}
