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
