//! Codex login preparation and identity-checked install under the existing lock order.
use crate::{public_auth::storage, signin_callbacks};
use aac_core::signin::ErrorCode;
use aac_files::{
    codex_identity::{self, Identity},
    codex_registry::{self, Field, Meta, Registry},
    profile_staging,
    profile_writes::{CodexLease, LoginInstallError},
    read,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(super) fn home() -> Option<PathBuf> {
    Some(std::env::home_dir()?.canonicalize().ok()?.join(".codex"))
}

fn auth(root: &Path, profile: &str) -> PathBuf {
    root.join("codex-instances").join(profile).join("auth.json")
}

pub(super) fn registry(root: &Path) -> Result<Registry, ErrorCode> {
    let file = root.join("codex-profiles.yaml");
    match read::entry_exists(&file) {
        Ok(false) => Ok(Registry::empty()),
        Ok(true) => codex_registry::read(&file).ok_or(ErrorCode::WriteFailed),
        Err(_) => Err(ErrorCode::WriteFailed),
    }
}

pub(super) fn name_in_use(root: &Path, name: &str, registry: &Registry) -> bool {
    registry.has(name)
        || auth(root, name)
            .parent()
            .is_none_or(|folder| read::entry_exists(folder).unwrap_or(true))
}

pub(super) fn prepare(root: &Path, job_id: &str, cli: &Path) -> Result<Value, ErrorCode> {
    let env: Map<String, Value> = std::env::vars_os()
        .filter_map(|(name, value)| {
            Some((
                name.into_string().ok()?,
                Value::String(value.into_string().ok()?),
            ))
        })
        .collect();
    let shared = home().ok_or(ErrorCode::WriteFailed)?.join("config.toml");
    let staging = format!(".staging-{job_id}");
    let prepared = signin_callbacks::codex_prepare(
        root,
        &staging,
        &shared,
        cli.to_str().ok_or(ErrorCode::ToolMissing)?,
        &Value::Object(env),
    )
    .filter(|reply| reply["status"] == "ok")
    .ok_or(ErrorCode::WriteFailed)?;
    Ok(prepared["command"].clone())
}

pub(super) fn cleanup(root: &Path, job_id: &str) {
    let _ = profile_staging::cleanup(root, &format!(".staging-{job_id}"));
}

pub(super) fn install(
    root: &Path,
    job_id: &str,
    name: &str,
    add: bool,
    stamp: &str,
    stopped: impl Fn() -> bool,
) -> Result<Identity, ErrorCode> {
    let staging = format!(".staging-{job_id}");
    let path = auth(root, &staging);
    let (fresh, bytes) = codex_identity::read_login(&path, true).ok_or(ErrorCode::WriteFailed)?;
    let home = home().ok_or(ErrorCode::WriteFailed)?;
    let activation = CodexLease::activation(&home).map_err(|_| ErrorCode::WriteFailed)?;
    let metadata = CodexLease::registry(root).map_err(|_| ErrorCode::WriteFailed)?;
    let mut registry = registry(root)?;
    if add {
        if registry.profiles.len() >= 16 || name_in_use(root, name, &registry) {
            return Err(ErrorCode::WriteFailed);
        }
    } else {
        let saved =
            codex_identity::read_login(&auth(root, name), false).map(|(identity, _)| identity);
        let profile = registry.get(name).ok_or(ErrorCode::WriteFailed)?;
        if !codex_identity::same_codex_identity(
            saved.as_ref(),
            word(profile, "email"),
            word(profile, "account_id"),
            &fresh,
        ) {
            return Err(ErrorCode::IdentityMismatch);
        }
    }
    for (other, meta) in &registry.profiles {
        if other == name {
            continue;
        }
        let saved =
            codex_identity::read_login(&auth(root, other), false).map(|(identity, _)| identity);
        if codex_identity::same_text(
            saved
                .as_ref()
                .map(|identity| identity.email.as_str())
                .or(word(meta, "email")),
            Some(&fresh.email),
        ) {
            return Err(ErrorCode::DuplicateIdentity);
        }
    }
    if stopped() {
        return Err(ErrorCode::Timeout);
    }
    if add {
        let (_, rechecked) =
            codex_identity::read_login(&path, true).ok_or(ErrorCode::WriteFailed)?;
        if rechecked != bytes || stopped() {
            return Err(ErrorCode::WriteFailed);
        }
        registry
            .create(name, stamp, display_fields(&fresh))
            .map_err(|_| ErrorCode::WriteFailed)?;
        let next = codex_registry::emit(&registry).ok_or(ErrorCode::WriteFailed)?;
        activation
            .writer()
            .codex_move(root, &home, &staging, name)
            .map_err(|_| ErrorCode::WriteFailed)?;
        if metadata
            .writer()
            .codex_metadata(root, next.as_bytes())
            .is_err()
        {
            let _ = activation.writer().codex_move(root, &home, name, &staging);
            return Err(ErrorCode::WriteFailed);
        }
        return Ok(fresh);
    }
    let live = home.join("auth.json");
    let live_identity = codex_identity::read_login(&live, false).map(|(identity, _)| identity);
    let saved_live = codex_identity::saved_login_is_live_checked(&live, &auth(root, name))
        .ok_or(ErrorCode::WriteFailed)?;
    let remains_live = live_identity.as_ref().is_some_and(|current| {
        codex_identity::same_codex_identity(Some(current), None, None, &fresh)
    });
    if saved_live && !remains_live {
        return Err(ErrorCode::IdentityMismatch);
    }
    if stopped() {
        return Err(ErrorCode::Timeout);
    }
    activation
        .writer()
        .codex_login(root, &home, name, remains_live, &bytes)
        .map_err(|error| match error {
            LoginInstallError::LiveNotUpdated => ErrorCode::LiveLoginNotUpdated,
            LoginInstallError::Refused => ErrorCode::WriteFailed,
        })?;
    let mut fields = vec![("last_used".into(), Field::Str(stamp.into()))];
    fields.extend(display_fields(&fresh));
    if registry.update(name, fields).is_ok()
        && let Some(next) = codex_registry::emit(&registry)
    {
        let _ = metadata.writer().codex_metadata(root, next.as_bytes());
    }
    Ok(fresh)
}

fn word<'a>(meta: &'a Meta, key: &str) -> Option<&'a str> {
    meta.iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| {
            if let Field::Str(text) = value {
                Some(text.as_str())
            } else {
                None
            }
        })
}

fn display_fields(identity: &Identity) -> Meta {
    let mut fields = vec![
        ("email".into(), Field::Str(identity.email.clone())),
        (
            "plan_type".into(),
            identity.plan.clone().map_or(Field::Null, Field::Str),
        ),
    ];
    if let Some(id) = &identity.account_id {
        fields.push(("account_id".into(), Field::Str(id.clone())));
    }
    fields
}

#[derive(Clone, Copy, Debug)]
pub(super) enum RemoveError {
    Missing,
    Active,
    Default,
    Last,
    Busy,
    Running,
    Stale,
    Failed,
}

pub(super) fn review(root: &Path, name: &str, running: bool) -> Result<String, RemoveError> {
    let home = home().ok_or(RemoveError::Failed)?;
    let current = registry(root).map_err(|_| RemoveError::Failed)?;
    review_current(root, &home, name, &current)?;
    if read::stat(&home.join(".ccs-activation.lock"))
        .is_ok_and(|stat| storage::now() as f64 - stat.mtime_ms <= 120_000.0)
    {
        return Err(RemoveError::Busy);
    }
    if running {
        return Err(RemoveError::Running);
    }
    fingerprint(root, &home, name, &current)
}

fn review_current(
    root: &Path,
    home: &Path,
    name: &str,
    current: &Registry,
) -> Result<(), RemoveError> {
    if !current.has(name) {
        return Err(RemoveError::Missing);
    }
    inactive(&home.join("auth.json"), &auth(root, name))?;
    if current.default.as_deref() == Some(name) && current.profiles.len() > 1 {
        return Err(RemoveError::Default);
    }
    if current.profiles.len() <= 1 {
        return Err(RemoveError::Last);
    }
    Ok(())
}

fn inactive(live: &Path, saved: &Path) -> Result<(), RemoveError> {
    match codex_identity::saved_login_is_live_checked(live, saved) {
        Some(false) => Ok(()),
        Some(true) => Err(RemoveError::Active),
        None => Err(RemoveError::Failed),
    }
}

fn fingerprint(
    root: &Path,
    home: &Path,
    name: &str,
    current: &Registry,
) -> Result<String, RemoveError> {
    let policy = |base| {
        read::ReadPolicy::capped(codex_identity::MAX_AUTH_BYTES as u64)
            .owner(read::Owner::Mine)
            .ancestors(read::Ancestors::NoLinkBelow(base))
    };
    let hash = |path: &Path, policy| match read::read(path, &policy) {
        read::Read::Bytes(bytes, _) => Ok(Some(storage::hex(&Sha256::digest(bytes)))),
        read::Read::Absent => Ok(None),
        _ => Err(RemoveError::Failed),
    };
    let live = hash(&home.join("auth.json"), policy(home))?;
    let saved = hash(&auth(root, name), policy(root))?;
    let registry = codex_registry::emit(current).ok_or(RemoveError::Failed)?;
    Ok(storage::sha(
        &serde_json::json!([registry, live, saved]).to_string(),
    ))
}

pub(super) fn remove(root: &Path, name: &str, reviewed: &str) -> Result<(), RemoveError> {
    let home = home().ok_or(RemoveError::Failed)?;
    remove_in(root, &home, name, reviewed)
}

fn remove_in(root: &Path, home: &Path, name: &str, reviewed: &str) -> Result<(), RemoveError> {
    let activation = CodexLease::activation(home).map_err(|_| RemoveError::Busy)?;
    let metadata = CodexLease::registry(root).map_err(|_| RemoveError::Failed)?;
    let mut current = registry(root).map_err(|_| RemoveError::Failed)?;
    review_current(root, home, name, &current)?;
    if fingerprint(root, home, name, &current)? != reviewed {
        return Err(RemoveError::Stale);
    }
    let original = codex_registry::emit(&current).ok_or(RemoveError::Failed)?;
    current
        .remove(name, false)
        .map_err(|_| RemoveError::Failed)?;
    let next = codex_registry::emit(&current).ok_or(RemoveError::Failed)?;
    let publish = |bytes: &str| metadata.writer().codex_metadata(root, bytes.as_bytes());
    let parent = root.join("codex-instances");
    if !read::entry_exists(&parent.join(name)).map_err(|_| RemoveError::Failed)? {
        return publish(&next).map_err(|_| RemoveError::Failed);
    }
    let tail = storage::hex(&storage::random::<8>().map_err(|_| RemoveError::Failed)?);
    let pid = std::process::id();
    let staged = format!("{name}.deleting.{pid}.{tail}");
    let backup = format!("{name}.preserved.{pid}.{tail}");
    let writer = activation.writer();
    writer
        .codex_move(root, home, name, &staged)
        .map_err(|_| RemoveError::Failed)?;
    let prepare = || {
        writer
            .codex_preserve(root, home, &staged, &backup)
            .map_err(|_| RemoveError::Failed)?;
        inactive(&home.join("auth.json"), &auth(root, &staged))?;
        publish(&next).map_err(|_| RemoveError::Failed)
    };
    if let Err(error) = prepare() {
        if writer.codex_move(root, home, &staged, name).is_ok() {
            let _ = writer.codex_discard(root, home, &backup);
        }
        return Err(error);
    }
    if writer.codex_discard(root, home, &staged).is_err()
        || writer.codex_discard(root, home, &backup).is_err()
    {
        let restore_from = if read::entry_exists(&parent.join(&backup)).unwrap_or_default() {
            &backup
        } else {
            &staged
        };
        if writer.codex_move(root, home, restore_from, name).is_ok() {
            let _ = writer.codex_discard(root, home, &staged);
        }
        let _ = publish(&original);
        return Err(RemoveError::Failed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn lifecycle_remove_compares_the_review_again_under_the_provider_locks() {
        let scope = tempfile::tempdir().unwrap();
        let base = scope.path().canonicalize().unwrap();
        let root = base.join("ccs");
        let home = base.join("native-home");
        let profile = root.join("codex-instances/work");
        let other = root.join("codex-instances/keep");
        for folder in [&home, &profile, &other] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(profile.join("data"), b"work-profile").unwrap();
        std::fs::write(other.join("data"), b"other-profile").unwrap();
        let file = root.join("codex-profiles.yaml");
        let mut current = Registry::empty();
        for name in ["work", "keep"] {
            current
                .create(name, "2026-11-19T17:00:00Z", vec![])
                .unwrap();
        }
        std::fs::write(&file, codex_registry::emit(&current).unwrap()).unwrap();
        let reviewed = fingerprint(&root, &home, "work", &current).unwrap();
        current.default = Some("keep".into());
        std::fs::write(&file, codex_registry::emit(&current).unwrap()).unwrap();
        assert!(matches!(
            remove_in(&root, &home, "work", &reviewed),
            Err(RemoveError::Stale)
        ));
        assert!(profile.is_dir());
        let refreshed = fingerprint(&root, &home, "work", &current).unwrap();
        remove_in(&root, &home, "work", &refreshed).unwrap();
        assert!(!profile.exists());
        assert_eq!(std::fs::read(other.join("data")).unwrap(), b"other-profile");
        let saved = codex_registry::read(&file).unwrap();
        assert_eq!(saved.list(), vec!["keep"]);
        assert_eq!(saved.default.as_deref(), Some("keep"));
    }
}
