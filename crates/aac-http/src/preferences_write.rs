//! The guarded preferences writer. The TS front door handles Intl and source-path checks.
use aac_files::{
    OwnedStore,
    read::{Read, read},
    registry::{self, Owners},
};
use aac_views::decline::Decline;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const PRIMARY: &str = "dashboard-preferences";
const COMPANION: &str = "dashboard-preferences-extra-sources";
pub(crate) const TRANSACTION: &str = "dashboard-preferences-transaction";
const DOC_CAP: usize = 64 * 1024;
pub(crate) const INPUT_CAP: usize = 512 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Documents {
    primary: String,
    companion: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    previous: Option<[Option<String>; 2]>,
    primary: String,
    companion: Option<String>,
}

fn stored(root: &Path, id: &str) -> Result<Option<Vec<u8>>, Decline> {
    let (file, policy) = crate::registered::file(root, id).ok_or(Decline::Internal)?;
    match read(&file, &policy) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) => Ok(Some(bytes)),
        _ => Err(Decline::Unsupported),
    }
}

fn revision(root: &Path) -> Result<[Option<String>; 2], Decline> {
    let hash = |bytes: Vec<u8>| {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    Ok([
        stored(root, PRIMARY)?.map(hash),
        stored(root, COMPANION)?.map(hash),
    ])
}

fn response(documents: &Documents) -> Result<Value, Decline> {
    if documents.primary.len() > DOC_CAP
        || documents
            .companion
            .as_ref()
            .is_some_and(|text| text.len() > DOC_CAP)
    {
        return Err(Decline::Unsupported);
    }
    let primary = crate::json_object::from_slice::<Value>(
        documents.primary.as_bytes(),
        &["timeZone", "snapshotCleanup", "usageLogSources"],
    )
    .map_err(|_| Decline::Unsupported)?;
    let mut companion = match &documents.companion {
        Some(text) => {
            let mut value =
                crate::json_object::from_slice::<Value>(text.as_bytes(), &["usageLogSources"])
                    .map_err(|_| Decline::Unsupported)?;
            value["usageLogSources"]
                .take()
                .as_array()
                .cloned()
                .ok_or(Decline::Unsupported)?
        }
        None => Vec::new(),
    };
    crate::account_reads::prepared_preferences(primary, &mut companion).ok_or(Decline::Unsupported)
}

fn materialize(
    primary: &OwnedStore,
    companion: &OwnedStore,
    documents: &Documents,
) -> Result<(), Decline> {
    match &documents.companion {
        Some(text) => companion.replace_settings(text.as_bytes()),
        None => companion.remove_settings(),
    }
    .map_err(|_| Decline::Internal)?;
    primary
        .replace_settings(documents.primary.as_bytes())
        .map_err(|_| Decline::Internal)
}

pub(crate) fn save(
    root: &Path,
    owners: &Owners,
    patch: bool,
    bytes: &[u8],
) -> Result<Value, Decline> {
    let request: Request =
        crate::json_object::from_slice(bytes, &["previous", "primary", "companion"])
            .map_err(|_| Decline::Unsupported)?;
    if request.previous.is_some() != patch {
        return Err(Decline::Unsupported);
    }
    let documents = Documents {
        primary: request.primary,
        companion: request.companion,
    };
    let saved = response(&documents)?;
    let rows = registry::rows().map_err(|_| Decline::Internal)?;
    let claim = |id| {
        owners
            .claim(rows, id, root, None)
            .map_err(|_| Decline::Unsupported)
    };
    let primary = claim(PRIMARY)?;
    let companion = claim(COMPANION)?;
    let transaction = claim(TRANSACTION)?;

    // Resume an earlier committed intent before considering another write.
    // The TS reader gives the intent precedence over the materialized files.
    if let Some(pending) = stored(root, TRANSACTION)? {
        let old: Documents = crate::json_object::from_slice(&pending, &["primary", "companion"])
            .map_err(|_| Decline::Unsupported)?;
        response(&old)?;
        materialize(&primary, &companion, &old)?;
        transaction
            .remove_settings()
            .map_err(|_| Decline::Internal)?;
    }
    if let Some(expected) = request.previous
        && revision(root)? != expected
    {
        return Err(Decline::Stale);
    }
    primary
        .can_replace_settings()
        .map_err(|_| Decline::Unsupported)?;
    companion
        .can_replace_settings()
        .map_err(|_| Decline::Unsupported)?;

    // Once the intent has been published, a transport failure is ambiguous.
    // The next writer completes it under the same three leases.
    let mut pending = serde_json::to_vec(&documents).map_err(|_| Decline::Internal)?;
    pending.push(b'\n');
    transaction.write(&pending).map_err(|_| Decline::Internal)?;
    materialize(&primary, &companion, &documents)?;
    transaction
        .remove_settings()
        .map_err(|_| Decline::Internal)?;
    primary.clean_settings_temporaries();
    companion.clean_settings_temporaries();
    transaction.clean_settings_temporaries();
    Ok(saved)
}
