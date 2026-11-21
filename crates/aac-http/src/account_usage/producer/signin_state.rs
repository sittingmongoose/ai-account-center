//! The cached `claude-host` session probe and the TS `withClaudeSignIn` projection.
//!
//! The dashboard said "Sign-in needed on Mac" from a five-minute cached `session` step on the
//! profile's Mac launcher, then filtered the needed platforms to the row's own launchers. The
//! coordinator cannot know that host state, so the producer asks it here and applies the same
//! post-projection the TS dashboard applied after the failover service returned.
use super::Producer;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

const ANSWER_TTL_MS: f64 = 5.0 * 60_000.0;
const FAILURE_TTL_MS: f64 = 60_000.0;
const FORCED_MIN_AGE_MS: f64 = 30_000.0;
const MAX_ENTRIES: usize = 64;

#[derive(Clone)]
pub(super) struct Answer {
    at: f64,
    ttl: f64,
    signed_out: bool,
}

/// The producer's per-profile cache of the Mac launcher's session state.
pub(super) type SignInCache = Arc<Mutex<Vec<(String, Answer)>>>;

#[cfg(test)]
pub(super) type SessionFixture = fn(&str, &Value) -> Value;

impl Producer {
    /// The TS `getClaudeMacSignInState`: `Some(true)` signed out, `Some(false)` signed in,
    /// `None` unknown. A failed check is unknown and never a sign-out.
    pub(super) async fn mac_signed_out(&self, profile: &Value, force: bool) -> Option<bool> {
        let id = profile["id"].as_str()?;
        let mac = &profile["mac"];
        let alias = mac["sshHost"].as_str()?;
        let path = mac["profilePath"]
            .as_str()
            .filter(|path| path.starts_with('/'))?;
        let launcher = json!({
            "launcherName": mac["launcherName"],
            "profilePath": path,
            "sshHost": alias,
        });
        let key = String::from_utf8(aac_views::js_json::write(&json!([
            self.root.to_string_lossy(),
            id,
            launcher
        ])))
        .ok()?;
        let now = aac_core::UtcMs::now().map_or(0.0, |now| now.get() as f64);
        {
            let cache = self.sign_in_state.lock().await;
            if let Some((_, entry)) = cache.iter().find(|(held, _)| held == &key) {
                let age = now - entry.at;
                if age < entry.ttl && !(force && age >= FORCED_MIN_AGE_MS) {
                    return Some(entry.signed_out);
                }
            }
        }
        let reply = self.probe_mac_session(alias, launcher).await;
        let signed_out = reply.as_ref().and_then(value_for_signed_in)?;
        let entry = Answer {
            at: now,
            ttl: if reply.is_some_and(|value| value.get("signedIn").is_none()) {
                FAILURE_TTL_MS
            } else {
                ANSWER_TTL_MS
            },
            signed_out,
        };
        let mut cache = self.sign_in_state.lock().await;
        cache.retain(|(held, _)| held != &key);
        cache.push((key, entry));
        if cache.len() > MAX_ENTRIES {
            cache.remove(0);
        }
        Some(signed_out)
    }
    async fn probe_mac_session(&self, alias: &str, launcher: Value) -> Option<Value> {
        #[cfg(test)]
        if let Some(probe) = self.mac_session_fixture {
            return Some(probe(alias, &launcher));
        }
        crate::claude_host::call(
            &self.agents,
            "mac",
            alias,
            json!({"host": "mac", "op": "session", "launcher": launcher}),
            25_000,
        )
        .await
    }
}

fn value_for_signed_in(value: &Value) -> Option<bool> {
    value.get("signedIn")?.as_bool().map(|signed_in| !signed_in)
}

/// Apply the launcher-specific note after the coordinator has projected the verified login.
pub(super) fn with_mac_sign_in(mut row: Value, mac_signed_out: bool, covered: bool) -> Value {
    if row.get("provider").and_then(Value::as_str) != Some("claude") {
        return row;
    }
    let has_platform = |list: &Value, platform: &str| {
        list.as_array()
            .is_some_and(|list| list.iter().any(|held| held == platform))
    };
    let platforms: Vec<&str> = ["mac", "windows"]
        .into_iter()
        .filter(|platform| {
            has_platform(&row["capabilities"]["claudePlatforms"], platform)
                && (has_platform(&row["signInNeeded"], platform)
                    || (covered && has_platform(&row["claudeDesktopSignedOut"], platform))
                    || (*platform == "mac" && mac_signed_out))
        })
        .collect();
    if let Some(fields) = row.as_object_mut() {
        fields.remove("signInNeeded");
        fields.remove("claudeDesktopSignedOut");
    }
    if platforms.is_empty() {
        return row;
    }
    let hosts = platforms
        .iter()
        .map(|platform| if *platform == "mac" { "Mac" } else { "Windows" })
        .collect::<Vec<_>>()
        .join(" and ");
    if covered {
        row["claudeDesktopSignedOut"] = json!(platforms);
        if row["message"].as_str().is_none_or(str::is_empty) {
            row["message"] = json!(format!(
                "Claude Desktop is signed out on {hosts}. AAC reads this account through its Claude Code login, so no sign-in is needed."
            ));
        }
        return row;
    }
    let live = row.get("status").and_then(Value::as_str) == Some("ok");
    let controls = platforms
        .iter()
        .map(|platform| {
            if *platform == "mac" {
                "Open on Mac"
            } else {
                "Open on Windows"
            }
        })
        .collect::<Vec<_>>()
        .join(" or ");
    let message = if live {
        row.get("message").cloned().unwrap_or(Value::Null)
    } else {
        json!(format!(
            "No working Claude sign-in for this account on {hosts}. To sign in, use {controls} in Details: it opens this account's Claude Desktop, which shows its sign-in window."
        ))
    };
    let empty_windows = row
        .get("windows")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    row["signInNeeded"] = json!(platforms);
    row["message"] = message;
    if empty_windows {
        row["status"] = json!("needs_sign_in");
    }
    row
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn claude_row() -> Value {
        json!({
            "id":"claude:livia","provider":"claude","providerLabel":"Claude","label":"livia",
            "email":"owner@example.com","plan":null,"platform":"mac","source":"Claude desktop on Mac",
            "status":"ok","message":null,"fetchedAt":"2026-01-01T00:00:00.000Z",
            "sampledAt":"2026-01-01T00:00:00.000Z","isActive":false,
            "windows":[{"key":"five_hour","usedPercent":10.0}],
            "capabilities":{"codexProfile":null,"claudeProfileId":"livia","claudePlatforms":["mac","windows"]}
        })
    }

    #[test]
    fn a_signed_out_mac_adds_the_platform_and_keeps_a_live_message() {
        let row = with_mac_sign_in(claude_row(), true, false);
        assert_eq!(row["signInNeeded"], json!(["mac"]));
        assert_eq!(row["message"], Value::Null);
        assert_eq!(row["status"], "ok");
    }

    #[test]
    fn a_row_without_windows_needs_a_sign_in() {
        let mut template = claude_row();
        template["status"] = json!("unavailable");
        template["message"] = json!("Claude desktop usage is temporarily unavailable.");
        template["windows"] = json!([]);
        let row = with_mac_sign_in(template, true, false);
        assert_eq!(row["signInNeeded"], json!(["mac"]));
        assert_eq!(row["status"], "needs_sign_in");
        assert_eq!(
            row["message"],
            "No working Claude sign-in for this account on Mac. To sign in, use Open on Mac in Details: it opens this account's Claude Desktop, which shows its sign-in window."
        );
    }

    #[test]
    fn a_windows_sign_in_prompt_names_the_open_control() {
        let mut template = claude_row();
        template["status"] = json!("unavailable");
        template["windows"] = json!([]);
        template["signInNeeded"] = json!(["windows"]);
        let row = with_mac_sign_in(template, false, false);
        assert_eq!(
            row["message"],
            "No working Claude sign-in for this account on Windows. To sign in, use Open on Windows in Details: it opens this account's Claude Desktop, which shows its sign-in window."
        );
    }

    #[test]
    fn a_platform_the_profile_cannot_open_is_never_named() {
        let mut template = claude_row();
        template["capabilities"]["claudePlatforms"] = json!(["windows"]);
        let row = with_mac_sign_in(template, true, false);
        assert!(row.get("signInNeeded").is_none());
        assert_eq!(row["status"], "ok");
    }

    #[test]
    fn a_coordinator_set_is_kept_in_platform_order() {
        let mut template = claude_row();
        template["signInNeeded"] = json!(["windows"]);
        let row = with_mac_sign_in(template, true, false);
        assert_eq!(row["signInNeeded"], json!(["mac", "windows"]));
        // A live row keeps its own message; the sentence belongs to a row that is not live.
        assert_eq!(row["message"], Value::Null);
        let mut template = claude_row();
        template["signInNeeded"] = json!(["windows"]);
        template["status"] = json!("cached");
        let row = with_mac_sign_in(template, true, false);
        assert_eq!(
            row["message"],
            "No working Claude sign-in for this account on Mac and Windows. To sign in, use Open on Mac or Open on Windows in Details: it opens this account's Claude Desktop, which shows its sign-in window."
        );
        assert_eq!(row["status"], "cached");
    }
}
