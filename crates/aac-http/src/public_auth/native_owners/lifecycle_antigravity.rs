//! Guarded writes to the shared Antigravity saved-profile registry.
use super::{
    NativeOwners, lifecycle_codex::RemoveError, lifecycle_maintenance,
    lifecycle_registry::antigravity_preflight, lifecycle_remove,
};
use crate::{
    antigravity_host::Host,
    antigravity_renewal::{self, Effect, Outcome},
    antigravity_runtime::{self as runtime, LiveIdentity, ProfileRecord, RegistryState},
    public_auth::storage,
    signin_callbacks,
};
use aac_core::signin::ErrorCode;
use aac_files::{profile_staging, profile_writes::AntigravityMaintenanceLease, read};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const CREDENTIAL: &str = ".gemini/antigravity-cli/antigravity-oauth-token";
const FORMAT: &str = "antigravity-consumer-json";
pub(super) const ORIGIN: &str = "https://accounts.google.com";
const BUS: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;
const DRIVER: &str = include_str!("../../../../../scripts/antigravity/signin_driver.py");

pub(super) struct Stage {
    root: PathBuf,
    directory: String,
    marker: String,
    device: u64,
    inode: u64,
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = profile_staging::cleanup_antigravity(&self.root, &self.directory);
        let _ = profile_staging::remove_antigravity_marker(
            &self.root,
            &self.marker,
            self.device,
            self.inode,
        );
    }
}

impl Stage {
    pub(super) fn prepare(
        root: PathBuf,
        job: &str,
        profile: &str,
    ) -> Result<(Self, Value), ErrorCode> {
        let home = std::env::home_dir().ok_or(ErrorCode::WriteFailed)?;
        if let Some(reason) = antigravity_preflight(Some(&home)) {
            return Err(if reason == "tool_missing" {
                ErrorCode::ToolMissing
            } else {
                ErrorCode::WriteFailed
            });
        }
        let directory = format!(
            ".staging-{}",
            job.strip_prefix("job_").ok_or(ErrorCode::WriteFailed)?
        );
        let marker = format!("{profile}.running");
        let parent = root.join("antigravity-signin");
        let path = parent.join(&marker);
        let temporary = format!(
            ".marker-{}",
            storage::hex(&storage::random::<8>().map_err(|_| ErrorCode::WriteFailed)?)
        );
        let holder = lifecycle_maintenance::holder_record().ok_or(ErrorCode::WriteFailed)?;
        profile_staging::antigravity_root(&root).map_err(|_| ErrorCode::WriteFailed)?;
        if let Ok((device, inode)) = profile_staging::marker_identity(&path)
            && !lifecycle_maintenance::antigravity_signin_running(&root, profile)
        {
            let _ = profile_staging::remove_antigravity_marker(&root, &marker, device, inode);
        }
        if !profile_staging::claim_antigravity_marker(&root, &marker, &temporary, holder.as_bytes())
            .map_err(|_| ErrorCode::WriteFailed)?
        {
            return Err(ErrorCode::WriteFailed);
        }
        let (device, inode) =
            profile_staging::marker_identity(&path).map_err(|_| ErrorCode::WriteFailed)?;
        let stage = Self {
            root,
            directory,
            marker,
            device,
            inode,
        };
        profile_staging::prepare_antigravity(&stage.root, &stage.directory, BUS.as_bytes())
            .map_err(|_| ErrorCode::WriteFailed)?;
        let args = stage.command(&home).ok_or(ErrorCode::WriteFailed)?;
        Ok((stage, args))
    }

    fn staging_home(&self) -> PathBuf {
        self.root
            .join("antigravity-signin")
            .join(&self.directory)
            .join("home")
    }

    fn command(&self, home: &Path) -> Option<Value> {
        let stage = self.staging_home();
        let parent = stage.parent()?;
        let leaf = "/sys/kernel/security/apparmor/.access";
        let runtime_dir = format!("/run/user/{}", crate::antigravity_probes::current_uid()?);
        let env = Value::Object(
            std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, json!(value.into_string().ok()?)))
                })
                .collect(),
        );
        let sandbox = signin_callbacks::antigravity_sandbox(
            parent.join("session-bus.conf").to_str()?,
            stage.join(".gemini").to_str()?,
            parent.join("browser-stubs").to_str()?,
            home.to_str()?,
            home.join(".local/bin/agy").to_str()?,
            read::stat(Path::new(leaf))
                .is_ok_and(|stat| stat.file)
                .then_some(leaf),
            read::stat(Path::new(&runtime_dir))
                .is_ok_and(|stat| stat.dir)
                .then_some(runtime_dir.as_str()),
            &env,
        )?;
        let mut args = vec![
            json!("-c"),
            json!(DRIVER.trim_end()),
            json!(stage.join(CREDENTIAL).to_str()?),
            json!(ORIGIN),
            json!("--"),
            json!("/usr/bin/bwrap"),
        ];
        args.extend_from_slice(sandbox["sandboxArgs"].as_array()?);
        Some(json!({"file":"/usr/bin/python3","args":args,"env":sandbox["env"],"pty":false}))
    }

    fn credential(&self) -> Option<Vec<u8>> {
        profile_staging::antigravity_token(&self.root, &self.directory)
    }
}

pub(super) fn current(root: &Path) -> Result<(Value, Option<RegistryState>), ErrorCode> {
    if let Some((document, parsed, _)) = runtime::read_document(root) {
        return Ok((document, Some(parsed)));
    }
    match runtime::newest_generation(&root.join("antigravity-profiles")) {
        Ok(None) => Ok((
            json!({"version":1,"revision":0,"profiles":[],"active":null,"transaction":null}),
            None,
        )),
        _ => Err(ErrorCode::WriteFailed),
    }
}

pub(super) fn install(
    native: &NativeOwners,
    stage: &Stage,
    profile: &str,
    add: bool,
    stopped: impl Fn() -> bool,
) -> Result<LiveIdentity, ErrorCode> {
    let root = native.agents.root();
    let host = native
        .lifecycle_usage
        .as_ref()
        .and_then(|usage| usage.antigravity.as_ref())
        .ok_or(ErrorCode::WriteFailed)?;
    let bytes = stage.credential().ok_or(ErrorCode::WriteFailed)?;
    let holder = lifecycle_maintenance::holder_record().ok_or(ErrorCode::WriteFailed)?;
    AntigravityMaintenanceLease::initialize(root).map_err(|_| ErrorCode::WriteFailed)?;
    let lease = AntigravityMaintenanceLease::claim(root, holder.as_bytes())
        .map_err(|_| ErrorCode::WriteFailed)?;
    let (previous, parsed) = current(root)?;
    if !previous["transaction"].is_null() || stopped() {
        return Err(ErrorCode::WriteFailed);
    }
    let profiles = parsed
        .as_ref()
        .map_or(&[][..], |parsed| parsed.profiles.as_slice());
    let existing = profiles.iter().find(|record| record.id == profile);
    if add && (existing.is_some() || profiles.len() >= 16) || !add && existing.is_none() {
        return Err(ErrorCode::WriteFailed);
    }
    let identity = host
        .validate_credential(&bytes)
        .ok_or(ErrorCode::WriteFailed)?;
    let now = storage::now();
    if identity
        .age_ms(now)
        .is_none_or(|age| !(-60_000..=300_000).contains(&age))
    {
        return Err(ErrorCode::WriteFailed);
    }
    let key = identity.identity_key();
    if existing.is_some_and(|profile| profile.identity_key != key) {
        return Err(ErrorCode::IdentityMismatch);
    }
    if profiles.iter().any(|record| {
        record.id != profile && (record.identity_key == key || record.email == identity.email)
    }) {
        return Err(ErrorCode::DuplicateIdentity);
    }
    let live = existing.and_then(|record| live_to_renew(root, host, record));
    if stopped() {
        return Err(ErrorCode::Timeout);
    }
    let started = storage::iso(now).ok_or(ErrorCode::WriteFailed)?;
    let fingerprint = runtime::sha256_hex(&bytes);
    let mut next = previous.clone();
    let rows = next["profiles"]
        .as_array_mut()
        .ok_or(ErrorCode::WriteFailed)?;
    rows.retain(|row| row["id"].as_str() != Some(profile));
    rows.push(json!({
        "id":profile,"email":identity.email,"identityKey":key,"plan":identity.plan,
        "credential":{"fingerprint":fingerprint,"format":FORMAT,
            "identity":{"email":identity.email,"subject":identity.subject,"plan":identity.plan,
                "verifiedAt":identity.verified_at,"source":identity.source},
            "savedAt":started}
    }));
    let save = || -> Result<bool, ErrorCode> {
        lease
            .writer()
            .antigravity_credential(root, profile, &fingerprint, &bytes)
            .map_err(|_| ErrorCode::WriteFailed)?;
        if stopped() {
            return Err(ErrorCode::Timeout);
        }
        publish(root, &lease, next.clone()).ok_or(ErrorCode::WriteFailed)?;
        let previous = existing.map(|record| record.credential.fingerprint.as_str());
        if let Ok(left) = lease
            .writer()
            .antigravity_prune(root, profile, &fingerprint, previous)
            && left > 0
        {
            eprintln!("accounts.antigravity.credentials.left_in_place count={left}");
        }
        Ok(true)
    };
    if let Some(expected) = live {
        let outcome =
            tokio::runtime::Handle::current().block_on(antigravity_renewal::run(|step| {
                std::future::ready(match step {
                    Effect::Save => save(),
                    Effect::Install => Ok(host.install_native_credential(&bytes, &expected)),
                    Effect::Verify => Ok(host
                        .read_native_credential()
                        .is_some_and(|(_, value)| value == fingerprint)),
                    Effect::Restore => {
                        if existing
                            .and_then(|record| runtime::read_credential_bytes(root, record))
                            .is_none()
                        {
                            return std::future::ready(Ok(false));
                        }
                        let mut restore = previous.clone();
                        restore["revision"] =
                            json!(next["revision"].as_i64().unwrap_or_default() + 1);
                        Ok(publish(root, &lease, restore).is_some())
                    }
                })
            }))?;
        match outcome {
            Outcome::Renewed => {}
            Outcome::Restored => return Err(ErrorCode::WriteFailed),
            Outcome::ProfileOnly => return Err(ErrorCode::LiveLoginNotUpdated),
        }
    } else {
        save()?;
    }
    Ok(identity)
}

fn live_to_renew(root: &Path, host: &Host, profile: &ProfileRecord) -> Option<String> {
    let (bytes, fingerprint) = host.read_native_credential()?;
    if fingerprint == profile.credential.fingerprint {
        return Some(fingerprint);
    }
    if let Some(identity) = host.validate_credential(&bytes) {
        return (identity.identity_key() == profile.identity_key).then_some(fingerprint);
    }
    let saved = runtime::read_credential_bytes(root, profile)?;
    storage::same(&grant_digest(&bytes)?, &grant_digest(&saved)?).then_some(fingerprint)
}

fn grant_digest(bytes: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(bytes).ok()?;
    let token = json["token"]["refresh_token"].as_str()?;
    (!token.is_empty()).then(|| storage::sha(token))
}

pub(super) fn review(native: &NativeOwners, name: &str) -> Result<String, RemoveError> {
    let root = native.agents.root();
    let (document, state, _) = document(root, name)?;
    if exists(&root.join("antigravity-profiles/.transaction-lock"))? {
        return Err(RemoveError::Busy);
    }
    guard(native, &document, &state, name)?;
    Ok(fingerprint(&document))
}

fn document(root: &Path, name: &str) -> Result<(Value, RegistryState, i64), RemoveError> {
    if !runtime::profile_id_ok(name) {
        return Err(RemoveError::Missing);
    }
    let (document, state, generation) = runtime::read_document(root).ok_or(RemoveError::Missing)?;
    if state.profile(name).is_none() {
        return Err(RemoveError::Missing);
    }
    Ok((document, state, generation))
}

fn exists(path: &Path) -> Result<bool, RemoveError> {
    read::entry_exists(path).map_err(|_| RemoveError::Failed)
}

fn guard(
    native: &NativeOwners,
    document: &Value,
    state: &RegistryState,
    name: &str,
) -> Result<(), RemoveError> {
    if !document["transaction"].is_null() {
        return Err(RemoveError::Busy);
    }
    if state.active_for(name).is_some() {
        return Err(RemoveError::Active);
    }
    let profile = state.profile(name).ok_or(RemoveError::Missing)?;
    let home = std::env::home_dir().ok_or(RemoveError::Failed)?;
    if exists(&home.join(".gemini/antigravity-cli/antigravity-oauth-token"))? {
        let host = native
            .lifecycle_usage
            .as_ref()
            .and_then(|usage| usage.antigravity.as_ref())
            .ok_or(RemoveError::Failed)?;
        let (credential, fingerprint) = host.read_native_credential().ok_or(RemoveError::Failed)?;
        if fingerprint == profile.credential.fingerprint {
            return Err(RemoveError::Active);
        }
        let identity = host
            .validate_credential(&credential)
            .ok_or(RemoveError::Failed)?;
        if identity.identity_key() == profile.identity_key {
            return Err(RemoveError::Active);
        }
    }
    let root = native.agents.root();
    if lifecycle_remove::running(native, &format!("antigravity:profile:{name}"))
        || lifecycle_maintenance::antigravity_signin_running(root, name)
    {
        return Err(RemoveError::Running);
    }
    Ok(())
}

fn fingerprint(document: &Value) -> String {
    storage::sha(&document.to_string())
}

pub(super) fn remove(native: &NativeOwners, name: &str, reviewed: &str) -> Result<(), RemoveError> {
    let root = native.agents.root();
    let record = lifecycle_maintenance::holder_record().ok_or(RemoveError::Failed)?;
    let lease = AntigravityMaintenanceLease::claim(root, record.as_bytes())
        .map_err(|_| RemoveError::Busy)?;
    let (mut document, state, _) = document(root, name)?;
    guard(native, &document, &state, name)?;
    if reviewed != fingerprint(&document) {
        return Err(RemoveError::Stale);
    }
    let profiles = document["profiles"]
        .as_array_mut()
        .ok_or(RemoveError::Failed)?;
    profiles.retain(|profile| profile["id"].as_str() != Some(name));
    publish(root, &lease, document).ok_or(RemoveError::Failed)?;
    let left = lease.writer().antigravity_discard(root, name).unwrap_or(1);
    if left > 0 {
        eprintln!("accounts.remove.left_in_place provider=antigravity count={left}");
    }
    Ok(())
}

fn publish(root: &Path, lease: &AntigravityMaintenanceLease, mut document: Value) -> Option<()> {
    let generation = document["revision"].as_i64()?.checked_add(1)?;
    document["revision"] = json!(generation);
    runtime::validate_state(&document)?;
    lease
        .writer()
        .antigravity_index(
            root,
            &format!("{generation:012}"),
            &aac_views::js_json::write(&document),
        )
        .ok()
}
