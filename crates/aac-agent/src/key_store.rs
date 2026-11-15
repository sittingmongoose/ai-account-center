//! AAC-owned Unix key writes. No secret or filesystem diagnostic is published.
use crate::proof;
use aac_core::{Cause, Failure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

fn refused() -> Failure {
    Failure::new(Cause::Refused, "key store could not be written safely")
}

pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    if !proof::proven(args)
        || args.as_object().is_none_or(|m| {
            m.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "action" | "provider" | "keyId" | "secret" | "home" | "nowMs"
                )
            })
        })
    {
        return Ok(proof::fallback());
    }
    // DPAPI requires native Windows qualification. Delegate before touching any path.
    if cfg!(windows) {
        return Ok(proof::fallback());
    }
    let action = args["action"].as_str().ok_or_else(refused)?;
    let provider = args["provider"].as_str().ok_or_else(refused)?;
    let id = args["keyId"].as_str().ok_or_else(refused)?;
    if !matches!(action, "put" | "delete")
        || !aac_providers::provider(provider).is_some_and(|p| p.key_store)
        || id.len() != 8
        || !id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(refused());
    }
    let home = if let Some(home) = args.get("home") {
        PathBuf::from(home.as_str().ok_or_else(refused)?)
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(refused)?
    };
    if !home.is_absolute() || !home.to_str().is_some_and(proof::ascii) {
        return Ok(proof::fallback());
    }
    let name = format!("{provider}-{id}.json");
    let target = home.join(".ccs/account-usage/keys").join(&name);
    if action == "delete" {
        match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(meta) if !meta.is_dir() => fs::remove_file(target).map_err(|_| refused())?,
            _ => return Err(refused()),
        }
        return Ok(json!({"ok":true}));
    }
    let raw = args["secret"].as_str().ok_or_else(refused)?;
    if raw.len() > 4096 {
        return Err(refused());
    }
    let secret = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(raw);
    if !(8..=512).contains(&secret.len()) || !secret.bytes().all(|c| (0x21..=0x7e).contains(&c)) {
        return Err(refused());
    }
    let now = if let Some(now) = args.get("nowMs") {
        now.as_i64()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .ok_or_else(refused)?
    } else {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| refused())?
            .as_millis();
        chrono::DateTime::from_timestamp_millis(i64::try_from(millis).map_err(|_| refused())?)
            .ok_or_else(refused)?
    };
    let digest: String = Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let fingerprint = format!("sha256:{}", &digest[..16]);
    let last4 = &secret[secret.len() - 4..];
    let record = json!({"version":1,"provider":provider,"keyId":id,"secret":secret,
        "fingerprint":fingerprint,"last4":last4,"createdAt":now.format("%Y-%m-%dT%H:%M:%SZ").to_string()});
    let mut payload = serde_json::to_vec(&record).map_err(|_| refused())?;
    if payload.len() > 4096 {
        return Err(refused());
    }
    payload.push(b'\n');
    #[cfg(unix)]
    {
        // The registry row `account-keys` (config/private-files.json): `key_store.py`'s folder
        // rule (parents at the umask, `keys` 0700, a real folder of this user, tightened), a
        // lease on that folder (a lock file would join the listing Python counts), then one
        // 0600 file staged with O_EXCL beside the target, fsynced, renamed and the folder synced.
        let store =
            aac_files::registry::claim_as_agent("account-keys", &home.join(".ccs"), Some(&name))
                .map_err(|_| refused())?;
        store.write(&payload).map_err(|_| refused())?;
        Ok(json!({"ok":true,"fingerprint":fingerprint,"last4":last4}))
    }
    #[cfg(not(unix))]
    {
        Ok(proof::fallback())
    }
}
