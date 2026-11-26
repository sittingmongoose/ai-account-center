//! The Claude desktop rows: one coordinator target per profile, one template per desktop.
//!
//! The TS dashboard built the template with `claudeAccount(profile, platform, history)` and let
//! the failover service read the Windows launcher, so the row here is the same pair of calls:
//! `rows::claude` with no reading is the template (platform, launcher capabilities, history
//! wording), and G4's coordinator does the arbitration, cadence and display projection.
use super::Producer;
use crate::account_usage::{Desktop, rows};
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value};

pub(super) fn manifest_profile(profile: &Value) -> Value {
    let mut result = Map::new();
    for field in ["email", "id"] {
        if let Some(value) = profile.get(field) {
            result.insert(field.into(), value.clone());
        }
    }
    for platform in ["mac", "windows"] {
        let Some(launcher) = profile.get(platform) else {
            continue;
        };
        let mut fields = Map::new();
        for field in [
            "launcherName",
            "launcherPath",
            "profilePath",
            "startMenuPath",
            "isDefault",
            "sshHost",
        ] {
            if field == "startMenuPath" && platform == "mac" {
                continue;
            }
            if let Some(value) = launcher.get(field) {
                fields.insert(field.into(), value.clone());
            }
        }
        result.insert(platform.into(), Value::Object(fields));
    }
    Value::Object(result)
}

impl Producer {
    /// The Mac then Windows row for every profile, in profile order.
    pub(super) async fn claude_rows(
        &self,
        profiles: &[Value],
        private_profiles: &[Value],
        history: &[Option<Value>],
        force: bool,
    ) -> Vec<[Option<Value>; 2]> {
        // Both lists come from the same validated manifest snapshot and keep its row order.
        // Check identity at the join before using a private launcher for a public account.
        stream::iter(
            profiles
                .iter()
                .cloned()
                .zip(private_profiles.iter().cloned())
                .zip(history.iter().cloned()),
        )
        .map(move |((display, private), history)| async move {
            if display["id"] != private["id"] || display["email"] != private["email"] {
                return [None, None];
            }
            self.claude_pair(&display, &private, history.as_ref(), force)
                .await
        })
        // R5b: one profile pair at a time; the helper/agent gates serialize the children.
        .buffered(1)
        .collect()
        .await
    }

    async fn claude_pair(
        &self,
        display: &Value,
        private: &Value,
        history: Option<&Value>,
        force: bool,
    ) -> [Option<Value>; 2] {
        let Some(target) = super::failover::claude_target(&self.root, private) else {
            return [None, None];
        };
        // The TS dashboard probed the Mac launcher before publishing the rows; a profile
        // without a Mac launcher answers unknown and never names Mac.
        let mac_signed_out = self.mac_signed_out(private, force).await.unwrap_or(false);
        let mut out: [Option<Value>; 2] = [None, None];
        for (index, desktop) in [Desktop::Mac, Desktop::Windows].into_iter().enumerate() {
            let Some(template) = rows::claude(display, desktop, None, history) else {
                continue;
            };
            // The first call in a generation reads; the other desktop's call projects the
            // same verified reading from the coordinator's state, as the TS service did for
            // a second dashboard.
            let row = self.failover().collect(&target, &template).await;
            let covered = row.get("claudeDesktopSignedOut").is_some()
                || (mac_signed_out && self.failover().claude_login_covers(&target));
            out[index] = Some(super::signin_state::with_mac_sign_in(
                row,
                mac_signed_out,
                covered,
            ));
        }
        out
    }
}

/// Read pending lifecycle rows from the same private store as the route owner.
pub(super) fn pending(root: &std::path::Path) -> Option<Vec<Value>> {
    use crate::account_usage::{provider_caches, rows};
    let value = provider_caches::load(root, "claude-pending", None, true).ok()??;
    let profiles = value["profiles"].as_array()?;
    if value["version"] != 1 || profiles.len() > 64 {
        return None;
    }
    let mut ids = std::collections::HashSet::new();
    for item in profiles {
        let id = item["id"].as_str()?;
        if !(2..=32).contains(&id.len())
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !ids.insert(id)
            || rows::stamp(&item["createdAt"]).is_none()
            || (!item["label"].is_null()
                && aac_views::antigravity::display_text(Some(&item["label"]), 96).is_none())
            || ["mac", "windows"].iter().any(|platform| {
                let host = &item[*platform];
                !host["sshHost"].is_string()
                    || !host["launcherName"].is_string()
                    || !host["profilePath"].is_string()
            })
        {
            return None;
        }
    }
    Some(profiles.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::collection_cache;
    use aac_files::registry::{Owners, Role};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn owners() -> Owners {
        let rows = aac_files::registry::rows().unwrap();
        let files = rows
            .iter()
            .map(|row| {
                let owner = if row.moves_with.as_deref() == Some("collection") {
                    Role::RustService
                } else {
                    row.owner
                };
                (row.id.clone(), owner.word().to_owned())
            })
            .collect();
        Owners::from_boot(rows, &files).unwrap()
    }

    fn stamp(now: i64) -> String {
        aac_num::ecma::to_iso_string(now).unwrap()
    }

    fn fixture_file(root: &std::path::Path, name: &str, value: Value) {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(root.join(name))
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
    }

    /// Start at the manifest and use an observation already keyed to its private launchers.
    /// A fake Mac probe also verifies that the same profile supplies the private SSH alias.
    #[tokio::test]
    async fn the_claude_manifest_collects_the_saved_windows_quota_and_mac_sign_in() {
        assert_saved_windows_quota_and_mac_sign_in(false).await;
    }

    #[tokio::test]
    async fn a_verified_code_read_covers_both_signed_out_desktops() {
        assert_saved_windows_quota_and_mac_sign_in(true).await;
    }

    async fn assert_saved_windows_quota_and_mac_sign_in(code_login: bool) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fixture_file(&root, "config.yaml", json!({"agentTransport":"off"}));
        fixture_file(
            &root,
            "account-usage-accounts.json",
            json!({"version":2,"accounts":[]}),
        );
        let mut producer = Producer::new(root.clone(), owners()).with_home(None);
        producer.mac_session_fixture = Some(|alias, launcher| {
            assert_eq!(alias, "fixture-mac");
            assert_eq!(
                *launcher,
                json!({"launcherName":"Claude Mac","profilePath":"/Users/fixture/Claude","sshHost":"fixture-mac"})
            );
            json!({"signedIn":false})
        });
        let profile = json!({
            "email":"owner@example.com","id":"livia",
            "mac":{"launcherName":"Claude Mac","launcherPath":"/Applications/Claude.app",
                   "profilePath":"/Users/fixture/Claude","sshHost":"fixture-mac"},
            "windows":{"launcherName":"Claude","profilePath":"C:\\Users\\x\\AppData\\Local\\Claude",
                       "isDefault":true,"sshHost":"fixture-windows"}
        });
        fixture_file(
            &root,
            "claude-desktop-profiles.json",
            json!({"version":1,"profiles":[profile]}),
        );
        let public = crate::profile_reads::desktop_profiles(&root).unwrap();
        assert!(public["profiles"][0]["windows"].get("sshHost").is_none());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let target = super::super::failover::claude_target(root.as_path(), &profile).unwrap();
        let key = producer.failover().key(&target);
        let at = stamp(now - 1_000);
        let mut candidate = json!({
            "locator":"a".repeat(64),"selectors":["claude-desktop-profile:default"],
            "identity":{"kind":"claude-oauth","key":"b".repeat(64)},"revision":"c".repeat(64),
            "email":"owner@example.com","plan":"Max"
        });
        if code_login {
            candidate["selectors"] = json!(["t3"]);
        }
        let reading = json!({
            "status":"ok","email":"owner@example.com","plan":"Max",
            "sampledAt":at,"fetchedAt":at,
            "windows":[{"key":"five_hour","label":"5-hour limit","usedPercent":42.0}]
        });
        let observation = json!({
            "version":1,"key":key,"identity":candidate["identity"].clone(),
            "primary":candidate,"saved":{"reading":reading,"candidate":candidate,"host":"windows","fallback":code_login},
            "nextAttempt":(now + 600_000) as f64,"fresh":true,"freshUntil":(now + 600_000) as f64,
            "primaryFailure":null,"primarySignIn":if code_login { vec!["windows"] } else { vec![] },"observedPlan":null,"observedPlanAt":null,
            "observedPlanPeer":null
        });
        collection_cache::write(
            &producer.owners.clone(),
            &root,
            collection_cache::OBSERVATIONS,
            &key,
            &aac_views::js_json::write(&observation),
        )
        .unwrap();
        let samples = producer.collect(Default::default()).await.unwrap();
        for desktop in [Desktop::Mac, Desktop::Windows] {
            let sample = samples
                .iter()
                .find(|sample| sample.desktop == desktop)
                .unwrap();
            let row = sample.dashboard["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == "claude:livia")
                .unwrap();
            if code_login {
                assert!(row.get("signInNeeded").is_none(), "{row}");
                assert_eq!(row["claudeDesktopSignedOut"], json!(["mac", "windows"]));
                assert!(
                    row["message"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("Claude Desktop is signed out on Mac and Windows.")
                );
            } else {
                assert_eq!(row["source"], "Verified quota on Windows");
                assert_eq!(row["signInNeeded"], json!(["mac"]));
            }
            assert_eq!(row["status"], "ok");
            assert_eq!(row["plan"], "Max");
            assert_eq!(row["windows"][0]["usedPercent"], 42.0);
            assert_eq!(
                row["capabilities"]["claudePlatforms"],
                json!(["mac", "windows"])
            );
            assert!(!row.to_string().contains("fixture-"));
        }
    }
}
