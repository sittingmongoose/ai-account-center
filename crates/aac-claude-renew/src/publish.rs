//! `claude-signins.json` and `claude-signins.md` in the usage snapshot folder: the same schema
//! (version 1) the Python runner published, so the dashboard (`aac_renewal::claude`) and
//! `aac-usage signins` read it unchanged.

use crate::HOUR_MS;
use crate::config::Config;
use crate::hostcmd::iso;
use crate::state::State;
use serde_json::{Value, json};
use std::path::Path;

/// The published JSON file name.
pub const PUBLISH_JSON: &str = "claude-signins.json";
/// The published summary file name.
pub const PUBLISH_MD: &str = "claude-signins.md";
/// This renewal's version, as `runnerVersion`.
pub const VERSION: &str = "2.0.0";

fn t(ms: Option<i64>) -> Value {
    ms.map_or(Value::Null, |m| Value::String(iso(m)))
}

/// The published document.
pub fn document(cfg: &Config, st: &State, runner: &str, now: i64) -> Value {
    let skip: std::collections::HashSet<&str> = cfg.skip.iter().map(String::as_str).collect();
    let within = Config::ms(cfg.renew_within_hours);
    let rows: Vec<Value> = st
        .profiles
        .iter()
        .filter(|(key, _)| {
            let profile = key.split_once('/').map_or("", |(_, p)| p);
            !cfg.ignore_profiles.iter().any(|i| i == profile)
        })
        .filter_map(|(key, p)| {
            let (host, profile) = key.split_once('/')?;
            let state = p.state.as_deref().unwrap_or("not_signed_in");
            Some(json!({
                "host": host,
                "profile": profile,
                "email": p.email,
                "state": state,
                "signInEndsAt": t(p.ends_at),
                "logoutAt": t(p.ends_at),
                "renewFrom": t(p.ends_at.map(|e| e - within)),
                "autoRenew": !skip.contains(key.as_str()),
                "lastRenewAt": t(p.last_renew_at),
                "lastResult": p.last_result,
                "nextVerifyAt": t(p.verify.iter().min().copied()),
                "retryAfter": t(p.retry_after.filter(|r| *r > now)),
                "reverts": p.reverts,
                "readAt": t(p.read_at),
                "note": Value::Null,
            }))
        })
        .collect();
    let needs: Vec<Value> = st
        .accounts
        .iter()
        .filter_map(|(email, a)| {
            Some(json!({
                "email": email,
                "reason": a.needs.as_deref()?,
                "how": a.needs_how,
                "since": t(a.needs_since),
            }))
        })
        .collect();
    let sessions: Vec<Value> = st
        .accounts
        .iter()
        .filter(|(_, a)| a.session_checked_at.is_some())
        .map(|(email, a)| {
            json!({
                "email": email,
                "signedIn": a.session_ends_at.is_some_and(|e| e > now),
                "sessionEndsAt": t(a.session_ends_at),
                "checkedAt": t(a.session_checked_at),
                "visitExtends": a.visit_extends,
            })
        })
        .collect();
    let hosts: Vec<Value> = st
        .hosts
        .iter()
        .map(|(name, h)| json!({"host": name, "readAt": t(h.read_at), "error": h.error}))
        .collect();
    json!({
        "version": 1,
        "source": "AI Account Center claude-renew",
        "runnerVersion": VERSION,
        "generatedAt": iso(now),
        "runner": runner,
        "tickSeconds": 600,
        "staleAfterSeconds": 3600,
        "renewWithinHours": cfg.renew_within_hours,
        "needsYou": needs,
        "hosts": hosts,
        "signIns": rows,
        "browserSessions": sessions,
    })
}

fn when(value: &Value, now: i64) -> String {
    let Some(ms) = value.as_str().and_then(crate::hostcmd::parse_iso_ms) else {
        return "unknown".into();
    };
    let hours = (ms - now) / HOUR_MS;
    let date = &iso(ms)[..16];
    if hours >= 0 {
        format!("{date} UTC (in {}d {}h)", hours / 24, hours % 24)
    } else {
        format!("{date} UTC ({}h ago)", -hours)
    }
}

/// The plain-words summary beside the JSON.
pub fn markdown(doc: &Value, now: i64) -> String {
    let mut out = String::from("# Claude sign-ins\n\n");
    let needs = doc["needsYou"].as_array().cloned().unwrap_or_default();
    if needs.is_empty() {
        out.push_str("Nothing needs you.\n\n");
    } else {
        out.push_str("## Needs you\n\n");
        for n in needs {
            out.push_str(&format!(
                "- **{}**: {}\n",
                n["email"].as_str().unwrap_or(""),
                n["how"].as_str().unwrap_or("")
            ));
        }
        out.push('\n');
    }
    out.push_str("| Computer | Profile | Account | State | Ends | Last result |\n| --- | --- | --- | --- | --- | --- |\n");
    for r in doc["signIns"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            r["host"].as_str().unwrap_or(""),
            r["profile"].as_str().unwrap_or(""),
            r["email"].as_str().unwrap_or(""),
            r["state"].as_str().unwrap_or(""),
            when(&r["signInEndsAt"], now),
            r["lastResult"].as_str().unwrap_or("")
        ));
    }
    out.push_str(&format!(
        "\nGenerated {} by AI Account Center claude-renew {VERSION}.\n",
        iso(now)
    ));
    out
}

/// Writes both files into the folder: the summary first, then the JSON, so the JSON's
/// `generatedAt` says the summary beside it is from the same run.
pub fn write(dir: &Path, doc: &Value, now: i64) -> std::io::Result<()> {
    let md = markdown(doc, now);
    write_shared(&dir.join(PUBLISH_MD), md.as_bytes())?;
    let mut json = serde_json::to_string_pretty(doc).map_err(std::io::Error::other)?;
    json.push('\n');
    write_shared(&dir.join(PUBLISH_JSON), json.as_bytes())
}

fn write_shared(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AccountState, ProfileState};

    #[test]
    fn document_has_the_runner_schema_and_no_secrets() {
        let cfg =
            Config::parse(r#"{"hosts":[{"name":"mac","os":"mac","ssh":"m"}],"skip":["mac/old"]}"#)
                .expect("cfg");
        let now = 1_800_000_000_000;
        let mut st = State::default();
        st.profiles.insert(
            "mac/party".into(),
            ProfileState {
                email: Some("p@x.party".into()),
                state: Some("ok".into()),
                ends_at: Some(now + 48 * HOUR_MS),
                last_result: Some("renewed".into()),
                verify: vec![now + 9 * HOUR_MS],
                retry_after: Some(now - 1),
                ..Default::default()
            },
        );
        st.profiles
            .insert("mac/old".into(), ProfileState::default());
        st.accounts.insert(
            "p@x.party".into(),
            AccountState {
                needs: Some("captcha".into()),
                needs_how: Some(
                    "Solve the check in the Chrome window for p@x.party on the Mac.".into(),
                ),
                needs_since: Some(now),
                session_ends_at: Some(now + HOUR_MS),
                session_checked_at: Some(now),
                ..Default::default()
            },
        );
        let doc = document(&cfg, &st, "ubuntu", now);
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["staleAfterSeconds"], 3600);
        assert_eq!(doc["runnerVersion"], VERSION);
        let rows = doc["signIns"].as_array().expect("rows");
        assert_eq!(rows.len(), 2);
        let party = rows
            .iter()
            .find(|r| r["profile"] == "party")
            .expect("party");
        assert_eq!(party["renewFrom"], iso(now + 24 * HOUR_MS));
        assert_eq!(party["nextVerifyAt"], iso(now + 9 * HOUR_MS));
        assert_eq!(party["retryAfter"], Value::Null);
        assert_eq!(party["autoRenew"], true);
        assert_eq!(
            rows.iter().find(|r| r["profile"] == "old").expect("old")["autoRenew"],
            false
        );
        assert_eq!(doc["needsYou"][0]["reason"], "captcha");
        assert_eq!(doc["browserSessions"][0]["signedIn"], true);
        let text = doc.to_string();
        assert!(!text.contains("https://"));
        let md = markdown(&doc, now);
        assert!(md.contains("| mac | party | p@x.party | ok |"));
        assert!(md.contains("## Needs you"));
        let dir = tempfile::tempdir().expect("dir");
        write(dir.path(), &doc, now).expect("write");
        let back: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(PUBLISH_JSON)).expect("read"),
        )
        .expect("json");
        assert_eq!(back, doc);
    }
}
