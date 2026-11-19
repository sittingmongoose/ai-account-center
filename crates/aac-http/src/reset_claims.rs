//! M5 lane K: the reset claims file on the private socket. The TS route keeps every public
//! guard (browser session, origin, no query, the exact 64-hex body), the account resolution and
//! the one provider send. This service owns the claims SQLite while the registry assigns it the
//! `reset-claims` row: it holds that row's writer lease for the life of the boot and runs every
//! transaction that writes the file (the ledger operations and the two ticketed source reads)
//! through `aac_status::reset::service_call`, the native ABI unchanged with the path it owns.
#![cfg_attr(not(unix), allow(dead_code))]
use aac_files::{OwnedStore, registry::Owners};
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};
use std::{path::Path, sync::Mutex};

/// The registry row this module writes.
pub(crate) const ROW: &str = "reset-claims";
/// The largest request: a private snapshot with its grants and windows, or a helper command.
pub(crate) const BODY_CAP: usize = 1024 * 1024;

/// The boot's claim on the claims file, taken on first use and held until the service ends.
/// SQLite serializes this process's connections (each call opens its own, as the native ABI
/// does); the lease keeps every other process role off the file.
#[derive(Default)]
pub(crate) struct ResetClaims(Mutex<Option<OwnedStore>>);

impl ResetClaims {
    /// The claims file's path, claiming the row's lease if this boot does not hold it yet.
    fn path(&self, root: &Path, owners: &Owners) -> Result<std::path::PathBuf, Decline> {
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
    pub(crate) fn apply(
        &self,
        root: &Path,
        owners: &Owners,
        body: &[u8],
    ) -> Result<Value, Decline> {
        let path = self.path(root, owners)?;
        Ok(match aac_status::reset::service_call(&path, body) {
            Ok(value) => json!({"ok": true, "value": sorted(value)}),
            Err(code) => json!({"ok": false, "code": code}),
        })
    }
}

/// The reset ABI has always serialized object keys in sorted order.
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
        // Defaults keep TS the writer: the service declines and never opens the file.
        let defaults = Owners::defaults(rows);
        assert_eq!(
            ResetClaims::default()
                .apply(&root, &defaults, body.as_bytes())
                .unwrap_err(),
            Decline::Unsupported
        );
        assert!(
            !root
                .join("account-usage/reset-claims/claims.sqlite")
                .exists()
        );
        let files = rows
            .iter()
            .map(|r| {
                let word = if r.id == ROW {
                    "rust-service"
                } else {
                    r.owner.word()
                };
                (r.id.clone(), word.to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).unwrap();
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
