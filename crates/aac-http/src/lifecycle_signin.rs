//! Bind a guarded lifecycle launch to the dashboard's local Lane G journal.
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    job_id: String,
    front_door_pid: u32,
    provider: String,
    kind: String,
    mode: String,
    account_id: Option<String>,
    profile_name: Option<String>,
    platform: String,
    allowed_origins: Vec<String>,
    timeout_ms: u64,
    command: Command,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    file: String,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
    pty: bool,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn job_id(id: &str) -> bool {
    id.strip_prefix("job_").is_some_and(|part| {
        part.len() == 16
            && part
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Build a Lane G start request after lifecycle admission and provider preparation.
/// The command always starts on the dashboard host. Muse's prepared command
/// reaches the configured Mac itself. Job metadata still says Mac.
pub fn agent_start(ccs: &Path, input: &str) -> Option<(u64, Value)> {
    let start: Start = serde_json::from_str(input).ok()?;
    let provider_matches = matches!(
        (
            start.provider.as_str(),
            start.kind.as_str(),
            start.platform.as_str(),
            start.mode.as_str()
        ),
        ("codex", "device-code", "ubuntu", "add" | "signin-again")
            | (
                "antigravity",
                "supervised-cli",
                "ubuntu",
                "add" | "signin-again"
            )
            | ("muse", "device-code", "mac", "signin-again")
    );
    let account_matches = match (start.mode.as_str(), &start.account_id) {
        ("add", None) => true,
        ("signin-again", Some(id)) => {
            identifier(id)
                && id
                    .split_once(':')
                    .is_some_and(|(provider, _)| provider == start.provider)
        }
        _ => false,
    };
    if !ccs.is_absolute()
        || !provider_matches
        || !account_matches
        || !job_id(&start.job_id)
        || start.front_door_pid == 0
        || !start.profile_name.as_deref().is_some_and(identifier)
        || !(1..=900_000).contains(&start.timeout_ms)
        || start.allowed_origins.is_empty()
        || start.allowed_origins.len() > 8
        || !Path::new(&start.command.file).is_absolute()
        || start.command.args.len() > 64
        || start.command.env.len() > 64
    {
        return None;
    }
    Some((
        start.timeout_ms + 60_000,
        json!({
            "action": "start",
            "jobId": start.job_id,
            "frontDoorPid": start.front_door_pid,
            "ccs": ccs.to_str()?,
            "provider": start.provider,
            "kind": start.kind,
            "mode": start.mode,
            "accountId": start.account_id,
            "profileName": start.profile_name,
            "platform": start.platform,
            "allowedOrigins": start.allowed_origins,
            "timeoutMs": start.timeout_ms,
            "command": {
                "file": start.command.file,
                "args": start.command.args,
                "env": start.command.env,
                "pty": start.command.pty,
            }
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_uses_one_boot_scoped_journal_even_for_a_muse_job() {
        let mut request = json!({
            "jobId":"job_0123456789abcdef","frontDoorPid":42,"provider":"muse",
            "kind":"device-code","mode":"signin-again","accountId":"muse:usage",
            "profileName":"muse:usage","platform":"mac",
            "allowedOrigins":["https://auth.meta.com"],"timeoutMs":300000,
            "command":{"file":"/fixture/ssh","args":[],"env":{},"pty":false}
        });
        let root = Path::new("/fixture/ccs");
        let (deadline, wire) =
            agent_start(root, &request.to_string()).expect("valid prepared flow");
        assert_eq!(deadline, 360_000);
        assert_eq!(wire["ccs"], "/fixture/ccs");
        assert_eq!(wire["action"], "start");
        assert_eq!(wire["platform"], "mac");
        request["ccs"] = json!("/another/journal");
        assert!(agent_start(root, &request.to_string()).is_none());
    }
}
