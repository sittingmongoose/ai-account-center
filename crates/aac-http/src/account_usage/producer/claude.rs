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
        history: &[Option<Value>],
        force: bool,
    ) -> Vec<[Option<Value>; 2]> {
        stream::iter(profiles.iter().cloned().zip(history.iter().cloned()))
            .map(move |(profile, history)| async move {
                self.claude_pair(&profile, history.as_ref(), force).await
            })
            .buffered(3)
            .collect()
            .await
    }

    async fn claude_pair(
        &self,
        profile: &Value,
        history: Option<&Value>,
        force: bool,
    ) -> [Option<Value>; 2] {
        let Some(target) = super::failover::claude_target(&self.root, profile) else {
            return [None, None];
        };
        // The TS dashboard probed the Mac launcher before publishing the rows; a profile
        // without a Mac launcher answers unknown and never names Mac.
        let mac_signed_out = self.mac_signed_out(profile, force).await.unwrap_or(false);
        let mut out: [Option<Value>; 2] = [None, None];
        for (index, desktop) in [Desktop::Mac, Desktop::Windows].into_iter().enumerate() {
            let Some(template) = rows::claude(profile, desktop, None, history) else {
                continue;
            };
            // The first call in a generation reads; the other desktop's call projects the
            // same verified reading from the coordinator's state, as the TS service did for
            // a second dashboard.
            let row = self.failover().collect(&target, &template).await;
            out[index] = Some(super::signin_state::with_mac_sign_in(row, mac_signed_out));
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
    use std::path::Path;
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

    /// A profile whose Windows launcher the coordinator already has a verified reading for:
    /// no transport is reachable, so the row can only come from the coordinator's state.
    #[tokio::test]
    async fn the_claude_row_is_the_coordinator_projection_of_the_saved_reading() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let producer = Producer::new(root.clone(), owners());
        let profile = json!({
            "email":"owner@example.com","id":"livia","canOpen":true,
            "windows":{"launcherName":"Claude","profilePath":"C:\\Users\\x\\AppData\\Local\\Claude",
                       "isDefault":true,"sshHost":"jared-windows"}
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let target = super::super::failover::claude_target(root.as_path(), &profile).unwrap();
        let key = producer.failover().key(&target);
        let at = stamp(now - 1_000);
        let candidate = json!({
            "locator":"a".repeat(64),"selectors":["claude-desktop-profile:default"],
            "identity":{"kind":"claude-oauth","key":"b".repeat(64)},"revision":"c".repeat(64),
            "email":"owner@example.com","plan":"Max"
        });
        let reading = json!({
            "status":"ok","email":"owner@example.com","plan":"Max",
            "sampledAt":at,"fetchedAt":at,
            "windows":[{"key":"five_hour","label":"5-hour limit","usedPercent":42.0}]
        });
        let observation = json!({
            "version":1,"key":key,"identity":candidate["identity"].clone(),
            "primary":candidate,"saved":{"reading":reading,"candidate":candidate,"host":"windows","fallback":false},
            "nextAttempt":(now + 600_000) as f64,"fresh":true,"freshUntil":(now + 600_000) as f64,
            "primaryFailure":null,"primarySignIn":[],"observedPlan":null,"observedPlanAt":null,
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
        let rows = producer.claude_rows(&[profile], &[None], false).await;
        let windows = rows[0][1].as_ref().unwrap();
        assert_eq!(windows["source"], "Verified quota on Windows");
        assert_eq!(windows["status"], "ok");
        assert_eq!(windows["plan"], "Max");
        assert_eq!(windows["windows"][0]["usedPercent"], 42.0);
        assert_eq!(windows["platform"], "windows");
        assert_eq!(
            windows["capabilities"]["claudePlatforms"],
            json!(["windows"])
        );
        // The Mac view keeps its own platform and names where the reading came from, as the
        // TS dashboard's mac row did for a Windows-collected profile.
        let mac = rows[0][0].as_ref().unwrap();
        assert_eq!(mac["platform"], "mac");
        assert_eq!(mac["host"], "windows");
        assert_eq!(mac["source"], "Verified quota on Windows");
    }
}
