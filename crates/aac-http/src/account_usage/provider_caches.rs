//! Private provider cache readers used by the native collection and failover paths.
use aac_files::{
    read::{Ancestors, Mode, Owner, Read, ReadPolicy},
    registry,
};
use aac_views::decline::Decline;
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, path::Path};

pub(crate) const WALLET_ACCOUNT: &str = "opencode-wallet-account";

fn hex(s: &str, length: usize) -> bool {
    s.len() == length
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn load(
    root: &Path,
    id: &str,
    name: Option<&str>,
    single_link: bool,
) -> Result<Option<Value>, Decline> {
    let row = registry::row(id).map_err(|_| Decline::Internal)?;
    let file = root.join(row.file(name).map_err(|_| Decline::Shape)?);
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(&file, &policy) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, stat) if !single_link || stat.nlink() == 1 => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| Decline::Shape)
        }
        _ => Err(Decline::Unsupported),
    }
}

/// Read the last Windows Claude sample only for the caller's current manifest hash.
/// The caller still verifies the normalized sample against the admitted profile.
pub fn claude_snapshot(
    root: &Path,
    profile: &str,
    manifest: &str,
) -> Result<Option<Value>, Decline> {
    if !hex(manifest, 64)
        || profile.is_empty()
        || profile.len() > 64
        || !profile.as_bytes()[0].is_ascii_alphanumeric()
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Decline::Shape);
    }
    let value = load(
        root,
        "claude-live-snapshots",
        Some(&format!("{profile}.json")),
        true,
    )?;
    Ok(value
        .filter(|v| v["schemaVersion"] == 1 && v["manifestHash"] == manifest)
        .and_then(|mut v| v.get_mut("sample").map(Value::take))
        .filter(|v| !v.is_null()))
}

/// Read an OpenCode console wallet account pointer bound to the source fingerprint.
pub fn wallet_account(root: &Path, binding_hash: &str) -> Result<Option<String>, Decline> {
    if !hex(binding_hash, 64) {
        return Err(Decline::Shape);
    }
    let Some(value) = load(root, WALLET_ACCOUNT, None, true)? else {
        return Ok(None);
    };
    Ok(wallet_pointer(binding_hash, &value).map(str::to_owned))
}

pub(crate) fn wallet_pointer<'a>(binding_hash: &str, value: &'a Value) -> Option<&'a str> {
    if !hex(binding_hash, 64)
        || value.as_object().is_none_or(|o| o.len() != 3)
        || value["version"] != 1
        || value["binding"] != binding_hash
    {
        return None;
    }
    value["accountId"].as_str().filter(|s| {
        s.strip_prefix("plan-opencode-go-console-mac-")
            .is_some_and(|tail| hex(tail, 12))
    })
}

/// Read the opt-in wallet binding with a configured safe Mac selector.
pub fn wallet_source(root: &Path) -> Result<Option<Value>, Decline> {
    let Some(value) = load(root, "opencode-wallet-source", None, true)? else {
        return Ok(None);
    };
    let alias = value["sshHost"].as_str().filter(|alias| {
        !alias.is_empty()
            && alias.len() <= 128
            && alias.as_bytes()[0].is_ascii_alphanumeric()
            && alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    });
    if value.as_object().is_none_or(|o| o.len() != 3)
        || value["version"] != 1
        || value["platform"] != "mac"
    {
        return Err(Decline::Shape);
    }
    let alias = alias.ok_or(Decline::Shape)?;
    Ok(Some(json!({"version":1,"platform":"mac","sshHost":alias})))
}

fn muse_identity(value: &Value, kinds: &[&str]) -> bool {
    value.as_object().is_some_and(|row| row.len() == 2)
        && value["kind"]
            .as_str()
            .is_some_and(|kind| kinds.contains(&kind))
        && value["key"].as_str().is_some_and(|key| hex(key, 64))
}

/// The current explicit grant to use a native Muse peer for this workspace.
/// Every peer on the grant must validate, and removing the grant revokes the cache.
pub fn bound_muse_peer(
    root: &Path,
    key: &str,
    primary: &Value,
    peer: &Value,
    host: &str,
) -> Result<bool, Decline> {
    if !hex(key, 64)
        || !muse_identity(primary, &["muse-team", "muse-native-credential"])
        || !muse_identity(peer, &["muse-native-credential"])
    {
        return Ok(false);
    }
    let Some(value) = load(
        root,
        "muse-usage-peer-links",
        Some(&format!("{key}.muse-peers.json")),
        true,
    )?
    else {
        return Ok(false);
    };
    let Some(peers) = value["peers"]
        .as_array()
        .filter(|rows| (1..=8).contains(&rows.len()))
    else {
        return Ok(false);
    };
    if value.as_object().is_none_or(|o| o.len() != 4)
        || value["version"] != 1
        || value["key"] != key
        || &value["identity"] != primary
        || peers.iter().any(|p| {
            p.as_object().is_none_or(|o| o.len() != 2)
                || !p["host"]
                    .as_str()
                    .is_some_and(|h| ["ubuntu", "nas1", "mac", "windows", "alexis"].contains(&h))
                || !muse_identity(&p["identity"], &["muse-native-credential"])
        })
    {
        return Ok(false);
    }
    Ok(peers
        .iter()
        .any(|p| p["host"] == host && &p["identity"] == peer))
}

pub(crate) fn counter(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= 9_007_199_254_740_991)
}

fn cursor_row(row: &Value) -> bool {
    let hour = row["h"].as_str().unwrap_or("").as_bytes();
    let hour_ok = hour.len() == 16
        && hour[4] == b'-'
        && hour[7] == b'-'
        && hour[10] == b' '
        && &hour[13..] == b":00"
        && [&hour[..4], &hour[5..7], &hour[8..10], &hour[11..13]]
            .iter()
            .all(|part| part.iter().all(u8::is_ascii_digit));
    hour_ok
        && row["m"].as_str().is_some_and(|m| {
            !m.is_empty() && m.encode_utf16().count() <= 200 && !aac_views::js::has_control(m)
        })
        && ["i", "o", "cr", "cw", "n", "uncosted"]
            .iter()
            .all(|key| counter(&row[key]).is_some())
        && row["cents"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && n >= 0.0)
}

pub(crate) fn cursor_state(value: &Value) -> Option<&str> {
    value.as_str().filter(|state| {
        ["no_key", "key_rejected", "read", "unavailable", "no_reader"].contains(state)
    })
}

pub(crate) fn cursor_rows(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| cursor_row(row))
        .take(100_000)
        .collect()
}

/// Read the saved Cursor account summary. This function only reads the existing cache.
pub fn cursor_account(root: &Path) -> Result<Option<Value>, Decline> {
    let Some(value) = load(root, "cursor-account-observation", None, true)? else {
        return Ok(None);
    };
    let Some(state) = cursor_state(&value["state"]) else {
        return Ok(None);
    };
    let Some(checked) = value["checkedAt"].as_str() else {
        return Ok(None);
    };
    let rows = cursor_rows(&value["rows"]);
    let key = match (
        value["key"]["last4"].as_str(),
        value["key"]["fingerprint"].as_str(),
    ) {
        (Some(last), Some(fingerprint)) => json!({"last4": last, "fingerprint": fingerprint}),
        _ => Value::Null,
    };
    let mut answer = json!({
        "state": state, "checkedAt": checked, "readAt": value["readAt"].as_str(),
        "account": value["account"].as_str(), "key": key,
        "rows": rows, "events": counter(&value["events"]).unwrap_or(0),
        "unmeasured": counter(&value["unmeasured"]).unwrap_or(0),
    });
    if let Some(cause) = value["cause"].as_str() {
        answer["cause"] = json!(cause);
    }
    Ok(Some(answer))
}

pub(crate) fn answer(root: &Path, bytes: &[u8]) -> Result<Value, Decline> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Claude {
        profile_id: String,
        manifest_hash: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Wallet {
        binding_hash: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Source {
        wallet_source: bool,
    }

    let result =
        if let Ok(request) = crate::json_object::from_slice::<Source>(bytes, &["walletSource"]) {
            if !request.wallet_source {
                return Err(Decline::Shape);
            }
            wallet_source(root)?.map(|value| Value::String(value.to_string()))
        } else if let Ok(request) =
            crate::json_object::from_slice::<Claude>(bytes, &["profileId", "manifestHash"])
        {
            claude_snapshot(root, &request.profile_id, &request.manifest_hash)?
        } else {
            let request: Wallet = crate::json_object::from_slice(bytes, &["bindingHash"])
                .map_err(|_| Decline::Shape)?;
            wallet_account(root, &request.binding_hash)?.map(Value::String)
        };
    Ok(json!({"value": result}))
}
