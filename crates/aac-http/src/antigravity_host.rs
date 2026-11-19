//! G6: the installed Antigravity runtime as the service uses it — the live login proof, the quota
//! worker, the two read-only probes and the registry reads, behind one host. `profile_reads.rs`
//! answers `/api/antigravity/profiles(/quotas)` from here when a caller sends no TS input; a host
//! that cannot resolve its trusted paths declines, so the TS fallback stays in charge.
//!
//! Fixture tests use invented homes, fake worker scripts and a fake probe runner; nothing here
//! spawns the real CLI, reads a real credential or reaches a provider.

use crate::antigravity_probes::{
    NativeUpdatePaused, PausedCache, ProblemReader, SocketState, bounded_run, diagnose,
    native_update_paused, read_native_release, socket_state,
};
use crate::antigravity_quota::{Cooldowns, QuotaRequest, QuotaService, Worker, public_quotas};
use crate::antigravity_runtime::{
    LiveIdentity, hash_word, inventory_with_probes, read_state, sha256_hex,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The installed native runtime, from `<ccs>/antigravity-switching/runtime-installation.json`.
struct Installed {
    pub bundle_directory: PathBuf,
    pub socket_path: PathBuf,
}

/// `readInstalledAntigravityRuntime`: the private installation record, or none.
fn read_installed(ccs: &Path, home: &Path) -> Option<Installed> {
    let file = ccs
        .join("antigravity-switching")
        .join("runtime-installation.json");
    let bytes = match aac_files::read::read(
        &file,
        &aac_files::read::ReadPolicy::capped(8192)
            .leaf(aac_files::read::Leaf::Refuse)
            .owner(aac_files::read::Owner::Mine)
            .mode(aac_files::read::Mode::Private),
    ) {
        aac_files::read::Read::Bytes(bytes, _) => bytes,
        _ => return None,
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let runtime_root = home.join(".local/share/ai-account-center/antigravity-runtime/bundles");
    let bundle = value.get("bundleDirectory")?.as_str()?;
    let bundle = PathBuf::from(bundle);
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || bundle.parent() != Some(runtime_root.as_path())
        || !bundle
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(hash_word)
        || value.get("socketPath").and_then(Value::as_str)
            != Some(
                ccs.join("antigravity-runtime/control.sock")
                    .to_string_lossy()
                    .as_ref(),
            )
    {
        return None;
    }
    Some(Installed {
        bundle_directory: bundle,
        socket_path: ccs.join("antigravity-runtime/control.sock"),
    })
}

/// One identity read's cache: the four stored paths' identity tuple keys a two-second reuse.
struct IdentityCache {
    key: String,
    identity: LiveIdentity,
    expires: i64,
}

/// The process's mutable Antigravity state.
struct State {
    identity: Option<IdentityCache>,
    paused: PausedCache,
    problem: Option<ProblemReader>,
    service: QuotaService,
    cooldowns: Cooldowns,
}

/// The probe runner and version reader: production uses the bounded command runner; fixtures pass
/// fakes so no real `systemctl` or CLI ever runs.
/// The trusted installed paths one host reads and runs.
pub(crate) struct Paths<'a> {
    /// The CCS scope (`<ccs>`).
    pub ccs: &'a Path,
    /// The live user's home (the saved CLI login).
    pub home: &'a Path,
    /// The system interpreter.
    pub python: &'a Path,
    /// `scripts/antigravity/quota_worker.py`.
    pub quota: &'a Path,
    /// `scripts/antigravity/native_credential_worker.py`.
    pub native: &'a Path,
    /// `scripts/account-usage`.
    pub collectors: &'a Path,
    /// `scripts/antigravity/runtime_health.py`.
    pub health: &'a Path,
}

type HealthRun = dyn Fn(&str, &[&str], u64) -> Option<String> + Send + Sync;
type HealthRunner = Box<HealthRun>;
type VersionReader = Box<dyn FnMut() -> Option<String> + Send>;

/// The installed runtime host for one CCS scope.
/// The installed runtime host for one CCS scope.
pub struct Host {
    ccs: PathBuf,
    home: PathBuf,
    quota: Worker,
    native: Worker,
    health_script: PathBuf,
    installed: Option<Installed>,
    runner: HealthRunner,
    version: Mutex<VersionReader>,
    state: Mutex<State>,
}

impl Host {
    /// A host over explicit trusted paths. Construction reads no credential and spawns nothing.
    pub(crate) fn new(
        paths: &Paths<'_>,
        runner: HealthRunner,
        version: VersionReader,
    ) -> Option<Self> {
        let quota = Worker::new(paths.python, paths.quota, paths.collectors)?;
        let native = Worker::new(paths.python, paths.native, paths.collectors)?;
        let installed = read_installed(paths.ccs, paths.home);
        let _ = &native;
        Some(Self {
            ccs: paths.ccs.to_path_buf(),
            home: paths.home.to_path_buf(),
            quota,
            native,
            health_script: paths.health.to_path_buf(),
            installed,
            runner,
            version: Mutex::new(version),
            state: Mutex::new(State {
                identity: None,
                paused: PausedCache::default(),
                problem: None,
                service: QuotaService::default(),
                cooldowns: Cooldowns::default(),
            }),
        })
    }

    /// The service's own construction: `$HOME`, `/usr/bin/python3` and the packaged scripts.
    pub fn from_root(ccs: &Path) -> Option<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))?;
        let exe = std::env::current_exe().ok()?;
        let package = exe.parent()?.parent()?.parent()?;
        let scripts = std::env::var_os("AAC_ANTIGRAVITY_SCRIPTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| package.join("scripts"));
        let python = std::env::var_os("AAC_ANTIGRAVITY_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/python3"));
        Self::new(
            &Paths {
                ccs,
                home: &home,
                python: &python,
                quota: &scripts.join("antigravity/quota_worker.py"),
                native: &scripts.join("antigravity/native_credential_worker.py"),
                collectors: &scripts.join("account-usage"),
                health: &scripts.join("antigravity/runtime_health.py"),
            },
            Box::new(bounded_run),
            Box::new(|| None),
        )
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
        let read = self.native.invoke_with(
            &["--home", &self.home.to_string_lossy()],
            &json!({"operation": "read"}),
        )?;
        if read.get("format").and_then(Value::as_str) != Some("antigravity-consumer-json") {
            return None;
        }
        let revision = read.get("revision").and_then(Value::as_str)?;
        let credential = base64_decode(read.get("credentialBase64").and_then(Value::as_str)?)?;
        if credential.is_empty() || credential.len() > 16_384 || sha256_hex(&credential) != revision
        {
            return None;
        }
        let identity = self.quota.validate_credential(&credential)?;
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

    /// The public inventory, with the paused and runtime-service probes attached.
    pub fn inventory(&self, now: i64) -> Value {
        let live = self.live_identity(now);
        if read_state(&self.ccs).is_none() {
            // No saved runtime: the route's empty Ubuntu inventory, with no probes.
            return json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []});
        }
        let paused = self.paused(now);
        let problem = self.problem(now);
        inventory_with_probes(
            &self.ccs,
            live.as_ref(),
            now,
            paused.as_ref(),
            problem.as_ref(),
        )
        .unwrap_or_else(|| json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []}))
    }

    /// The public quotas answer from the runtime's own accounts.
    pub fn quotas(&self, refresh: bool, now: i64) -> Value {
        let live = self.live_identity(now);
        let Some(state) = read_state(&self.ccs) else {
            return public_quotas(&[], now, now).unwrap_or(Value::Null);
        };
        let mut guard = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => return Value::Null,
        };
        let guard = &mut *guard;
        let worker = &self.quota;
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
            worker.collect_quota(&request)
        };
        let read = crate::antigravity_quota::Read {
            ccs: &self.ccs,
            state: &state,
            live: live.as_ref(),
            now,
        };
        let accounts =
            guard
                .service
                .get_accounts(&read, &mut collect, &mut guard.cooldowns, refresh);
        public_quotas(&accounts, now, now).unwrap_or(Value::Null)
    }

    fn paused(&self, _now: i64) -> Option<Value> {
        let release = read_native_release(
            &self
                .health_script
                .parent()
                .map(|dir| dir.join("runtime/release.json"))
                .unwrap_or_default(),
        );
        let mut guard = self.state.lock().ok()?;
        let mut version = self.version.lock().ok()?;
        let paused = native_update_paused(
            &self.home,
            &release,
            &pin_matches,
            &mut **version,
            &mut guard.paused,
        );
        paused.map(|NativeUpdatePaused { installed_version }| {
            json!({"installedVersion": installed_version})
        })
    }

    fn problem(&self, now: i64) -> Option<Value> {
        let installed = self.installed.as_ref()?;
        let mut guard = self.state.lock().ok()?;
        guard.problem.get_or_insert_with(|| {
            ProblemReader::new(&installed.bundle_directory, &installed.socket_path)
        });
        let runner: &HealthRun = &*self.runner;
        let problem = guard.problem.as_mut()?;
        let mut diagnose = |bundle: &Path| {
            diagnose(
                bundle,
                crate::antigravity_probes::SERVICE_PYTHON,
                &self.health_script,
                &*runner,
            )
        };
        problem.problem(now, &mut diagnose)
    }

    /// `probeRuntimeSocket` for the installed control socket.
    pub fn socket_state(&self, timeout_ms: u64) -> SocketState {
        match &self.installed {
            Some(installed) => socket_state(&installed.socket_path, timeout_ms),
            None => SocketState::Stopped,
        }
    }
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
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

/// `verifyOwnedAntigravityNativePin`: the binary's sha256 over one bounded read.
fn pin_matches(binary: &Path, sha256: &str) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(binary) else {
        return false;
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 * 1024 * 1024 {
        return false;
    }
    use sha2::{Digest, Sha256};
    let Ok(mut file) = std::fs::File::open(binary) else {
        return false;
    };
    use std::io::Read;
    let mut digest = Sha256::new();
    let mut chunk = [0u8; 65_536];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => digest.update(&chunk[..n]),
            Err(_) => return false,
        }
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        == sha256
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::antigravity_runtime::LiveIdentity;
    use base64::Engine as _;
    use serde_json::json;

    const CREDENTIAL: &[u8] = b"fixture private credential bytes";

    fn write(path: &Path, bytes: &[u8], mode: u32) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
    }

    fn python() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("AAC_TEST_PYTHON") {
            return Some(PathBuf::from(path));
        }
        let paths = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&paths) {
            for name in ["python3", "python"] {
                for candidate in [directory.join(name), directory.join(format!("{name}.exe"))] {
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let file = dir.join(name);
        write(&file, body.as_bytes(), 0o600);
        file
    }

    /// The identity's own key, computed the way the fixtures' Python worker does.
    fn identity_script(email: &str, subject: &str) -> String {
        format!(
            r#"import base64,hashlib,json,sys
request = json.load(sys.stdin)
credential = base64.b64decode(request["credentialBase64"]) if "credentialBase64" in request else b""
fingerprint = hashlib.sha256(credential).hexdigest()
if request.get("operation") == "identity":
    identity = {{"email":"{email}","subject":"{subject}","plan":"Google AI Pro",
                "verifiedAt":"2026-10-01T17:00:00.000Z","source":"provider-userinfo"}}
    key = hashlib.sha256(json.dumps(["antigravity", identity["email"], identity["subject"]],
                                    separators=(",", ":")).encode()).hexdigest()
    print(json.dumps({{**identity, "credentialRevision": fingerprint, "identityKey": key}}))
else:
    print(json.dumps({{"profileId":"gmail","identityKey":request["identityKey"],
        "credentialRevision":request["credentialRevision"],"email":"{email}","plan":"Google AI Pro",
        "status":"fresh","fetchedAt":"2026-10-01T17:00:00.000Z","sampledAt":"2026-10-01T17:00:00.000Z",
        "windows":[{{"key":"seven_day","label":"Weekly","usedPercent":40,
                     "resetAt":"2026-10-08T17:00:00.000Z","windowMinutes":10080}}],
        "source":"native-consumer","identityVerified":True,"identityValidation":"verified"}}))
"#
        )
    }

    #[test]
    fn the_host_answers_both_routes_from_the_installed_runtime() {
        let Some(python) = python() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let ccs = temp.path().join(".ccs");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&ccs).unwrap();
        // The home folder itself is one of the identity's four paths: owner-only, like a real one.
        std::fs::create_dir_all(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // The saved profile the registry holds, and its credential file.
        let identity_value = json!({
            "email": "gmail@example.com",
            "subject": "fixture-authoritative-subject-gmail",
            "plan": "Google AI Pro",
            "verifiedAt": "2026-10-01T16:40:00.000Z",
            "source": "provider-userinfo",
        });
        let identity = LiveIdentity::from_value(&identity_value).unwrap();
        let revision = sha256_hex(CREDENTIAL);
        write(
            &ccs.join("antigravity-profiles/registry-000000000001.json"),
            json!({
                "version": 1, "revision": 1,
                "profiles": [{
                    "id": "gmail", "email": "gmail@example.com",
                    "identityKey": identity.identity_key(), "plan": "Google AI Pro",
                    "credential": {"fingerprint": revision, "format": "fake-native-json",
                                   "identity": identity_value,
                                   "savedAt": "2026-10-01T16:40:00.000Z"},
                }],
                "active": {"profileId": "gmail", "verifiedAt": "2026-10-01T16:40:00.000Z"},
                "transaction": null,
            })
            .to_string()
            .as_bytes(),
            0o600,
        );
        write(
            &ccs.join("antigravity-instances/gmail/ubuntu")
                .join(format!("credential-{revision}.bin")),
            CREDENTIAL,
            0o600,
        );
        // The live login the native worker reads, and its identity proof.
        write(
            &home.join(".gemini/antigravity-cli/antigravity-oauth-token"),
            CREDENTIAL,
            0o600,
        );
        // `write` fixes the file's own folder; the identity also reads `.gemini`, so make it private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.join(".gemini"), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let native = script(
            temp.path(),
            "native.py",
            &format!(
                r#"import base64,hashlib,json,sys
request = json.load(sys.stdin)
print(json.dumps({{"format":"antigravity-consumer-json",
    "credentialBase64":"{}",
    "revision":"{}"}}))
"#,
                base64::engine::general_purpose::STANDARD.encode(CREDENTIAL),
                revision
            ),
        );
        let quota = script(
            temp.path(),
            "quota.py",
            &identity_script("gmail@example.com", "fixture-authoritative-subject-gmail"),
        );
        let health = script(temp.path(), "health.py", "");
        let collectors = temp.path().join("collectors");
        std::fs::create_dir_all(&collectors).unwrap();
        let host = Host::new(
            &Paths {
                ccs: &ccs,
                home: &home,
                python: &python,
                quota: &quota,
                native: &native,
                collectors: &collectors,
                health: &health,
            },
            Box::new(|file, _args, _timeout| {
                (file != "systemctl").then(|| r#"{"ok":true}"#.to_owned())
            }),
            Box::new(|| None),
        )
        .unwrap();
        let now = crate::antigravity_runtime::js_date("2026-10-01T17:00:00.000Z").unwrap();
        let inventory = host.inventory(now);
        assert_eq!(inventory["profiles"][0]["id"], json!("gmail"));
        assert_eq!(inventory["profiles"][0]["selected"], json!(true));
        assert_eq!(inventory["profiles"][0]["available"], json!(true));
        assert!(inventory.get("runtimeServiceProblem").is_none());
        let quotas = host.quotas(true, now);
        assert_eq!(quotas["accounts"][0]["status"], json!("ok"));
        assert_eq!(quotas["accounts"][0]["isActive"], json!(true));
        assert_eq!(
            quotas["accounts"][0]["windows"][0]["usedPercent"],
            json!(40)
        );
    }
}
