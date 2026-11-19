//! Select a Mac-only profile's desktop-owned, read-only usage history.
use super::Producer;
use crate::agent_transport::Selector;
use aac_transport::CallOptions;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{ffi::OsString, time::Duration};

const CAP: usize = 1024 * 1024;
const MISSING: &str = "CCS_DESKTOP_USAGE_MISSING";
const READ: &str = r#"
history="$1/plan-usage-history.json"
if [ ! -f "$history" ]; then printf '%s' CCS_DESKTOP_USAGE_MISSING; exit 0; fi
bytes=$(/usr/bin/stat -f '%z' "$history") || exit 1
test "$bytes" -le 1048576 || exit 1
exec /bin/cat "$history"
"#;

impl Producer {
    pub(super) async fn mac_history_rows(
        &self,
        profiles: &[Value],
        now: i64,
    ) -> Vec<Option<Value>> {
        stream::iter(profiles.to_vec())
            .map(|profile| async move { self.mac_history(&profile, now).await })
            .buffered(3)
            .collect()
            .await
    }

    async fn mac_history(&self, profile: &Value, now: i64) -> Option<Value> {
        if profile.get("windows").is_some() {
            return None;
        }
        let launcher = &profile["mac"];
        let mut entry = json!({"email":profile["email"],"read":"unconfigured"});
        if let Some(id) = profile.get("id") {
            entry["id"] = id.clone();
        }
        if let (Some(alias), Some(path)) = (
            launcher["sshHost"].as_str(),
            launcher["profilePath"].as_str(),
        ) && path.starts_with('/')
            && path.encode_utf16().count() <= 4096
            && !path.chars().any(|c| c.is_ascii_control())
        {
            entry["read"] = json!("failed");
            if let Some(body) = self.read_history(alias, path).await {
                entry["read"] = json!("read");
                entry["history"] = json!(body);
            }
        }
        let fetched = aac_num::ecma::to_iso_string(now)?;
        let result = aac_views::claude_desktop::usage(&json!({
            "platform":"mac","fetchedAt":fetched,"profiles":[entry]
        }))
        .ok()?;
        result["profiles"].as_array()?.first().cloned()
    }

    async fn read_history(&self, alias: &str, path: &str) -> Option<Option<String>> {
        let options = CallOptions::new(Duration::from_secs(15), CAP);
        let selector = Selector::usage("mac", Some(alias));
        let _permit = self.agents.permit()?;
        let client = self.agents.client(&selector, options.deadline).await?;
        let args = ["/bin/sh", "-c", READ, "aac-desktop-history", path].map(OsString::from);
        let read = client.computer().execute(&args, &[], &options).await.ok()?;
        if read.code != Some(0) {
            return None;
        }
        let text = String::from_utf8(read.stdout).ok()?;
        Some((text.trim() != MISSING).then_some(text))
    }
}
