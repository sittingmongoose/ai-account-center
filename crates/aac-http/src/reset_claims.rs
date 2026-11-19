//! Exclusive writer lease and native ledger dispatch shared by the public and private listeners.
use aac_files::{OwnedStore, registry::Owners};
use aac_views::decline::Decline;
#[cfg(any(unix, test))]
use serde_json::json;
use serde_json::{Map, Value};
use std::{path::Path, sync::Mutex};

/// The registry row this module writes.
pub(crate) const ROW: &str = "reset-claims";
/// The largest request: a private snapshot with its grants and windows, or a helper command.
#[cfg(unix)]
pub(crate) const BODY_CAP: usize = 1024 * 1024;

/// The public listener owns this row regardless of the remaining worker's feature flags.
pub(crate) fn public_owners(rows: &[aac_files::registry::Row]) -> Result<Owners, Decline> {
    Owners::from_boot(
        rows,
        &rows
            .iter()
            .map(|row| {
                let owner = if row.id == ROW {
                    "rust-service"
                } else {
                    row.owner.word()
                };
                (row.id.clone(), owner.to_owned())
            })
            .collect(),
    )
    .map_err(|_| Decline::Internal)
}

/// The boot's claim on the claims file, taken on first use and held until the service ends.
/// SQLite serializes this process's connections (each call opens its own, as the native ABI
/// does); the lease keeps every other process role off the file.
#[derive(Default)]
pub(crate) struct ResetClaims(Mutex<Option<OwnedStore>>);

impl ResetClaims {
    /// The claims file's path, claiming the row's lease if this boot does not hold it yet.
    pub(crate) fn path(&self, root: &Path, owners: &Owners) -> Result<std::path::PathBuf, Decline> {
        let mut held = self.0.lock().map_err(|_| Decline::Internal)?;
        if held.is_none() {
            let rows = aac_files::registry::rows().map_err(|_| Decline::Internal)?;
            // Another role's row, or another process's live lease: the TS route answers its
            // own storage-unavailable reply, as it does when the native file cannot open.
            *held = Some(
                owners
                    .claim(rows, ROW, root, None)
                    .map_err(|_| Decline::Unsupported)?,
            );
        }
        held.as_ref()
            .map(|store| store.path().to_path_buf())
            .ok_or(Decline::Internal)
    }

    /// One call. A refusal is `{"ok":false,"code":<fixed word>}` so the front door raises the
    /// same `<code>: ...` error the native ABI raises; an answer is `{"ok":true,"value":...}`
    /// with its object keys in the native module's order (sorted).
    #[cfg(any(unix, test))]
    pub(crate) fn apply(
        &self,
        root: &Path,
        owners: &Owners,
        body: &[u8],
    ) -> Result<Value, Decline> {
        let path = self.path(root, owners)?;
        // G11: the account resolution and the provider send run here too; their reads write
        // this same file through the native capture transactions.
        let answer = if source_op(body) {
            crate::reset_source_route::call(
                &crate::reset_source_route::SourceContext::from_env(root, None),
                &path,
                body,
            )
        } else {
            aac_status::reset::service_call(&path, body)
        };
        Ok(match answer {
            Ok(value) => json!({"ok": true, "value": sorted(value)}),
            Err(code) => json!({"ok": false, "code": code}),
        })
    }
}

/// `sourceRead`/`sourceSend` (G11): resolved and sent by the service, not the front door.
#[cfg(any(unix, test))]
fn source_op(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| {
        matches!(
            v.get("op").and_then(Value::as_str),
            Some("sourceRead" | "sourceSend")
        )
    })
}

/// The native module builds without `preserve_order`, so its objects serialize with sorted keys;
/// this service builds with it. Sorting here keeps the answers byte-equal.
pub(crate) fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect::<Map<String, Value>>(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn answers_sort_keys_like_the_native_module() {
        let value = sorted(json!({"b": 1, "a": {"d": [{"z": 1, "y": 2}], "c": null}}));
        assert_eq!(
            value.to_string(),
            r#"{"a":{"c":null,"d":[{"y":2,"z":1}]},"b":1}"#
        );
    }

    #[test]
    fn only_the_assigned_row_is_claimed_and_the_lease_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let rows = aac_files::registry::rows().unwrap();
        let body =
            json!({"op":"pending","account":"codex:fixture","nowMs":1,"entropy":"0".repeat(64)})
                .to_string();
        let owners = public_owners(rows).unwrap();
        let claims = ResetClaims::default();
        let answer = claims.apply(&root, &owners, body.as_bytes()).unwrap();
        assert_eq!(answer["ok"], true);
        assert!(
            root.join("account-usage/reset-claims/claims.sqlite")
                .exists()
        );
        // A second claimant (another boot) is refused while this one holds the lease.
        assert_eq!(
            ResetClaims::default()
                .apply(&root, &owners, body.as_bytes())
                .unwrap_err(),
            Decline::Unsupported
        );
        // A refusal carries the native fixed word.
        let refused = claims.apply(&root, &owners, b"{}").unwrap();
        assert_eq!(refused, json!({"ok": false, "code": "shape"}));
    }
}
