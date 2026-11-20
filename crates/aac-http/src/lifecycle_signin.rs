//! Bind a guarded lifecycle launch to the dashboard's local Lane G journal.
use base64::{Engine, engine::general_purpose::STANDARD};
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

fn ascii(value: &str, cap: usize) -> bool {
    !value.is_empty() && value.len() <= cap && value.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

impl Command {
    fn bind(mut self, provider: &str) -> Option<Self> {
        // The dashboard prepares the fixed Antigravity driver as a multiline
        // Python argument. G admits printable argv only. Preserve the program
        // and its argv without putting a staged credential on the wire.
        if provider == "antigravity"
            && self.file == "/usr/bin/python3"
            && self.args.first().is_some_and(|arg| arg == "-c")
            && self
                .args
                .get(1)
                .is_some_and(|program| program.contains('\n'))
        {
            let program = STANDARD.encode(&self.args[1]);
            let chunks: Vec<_> = program
                .as_bytes()
                .chunks(3500)
                .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                .collect();
            let mut args = vec![
                "-c".into(),
                "import base64,sys; n=int(sys.argv[1]); body=base64.b64decode(''.join(sys.argv[2:2+n])); sys.argv=['-c',*sys.argv[2+n:]]; exec(compile(body,'<string>','exec'))".into(),
                chunks.len().to_string(),
            ];
            args.extend(chunks);
            args.extend(self.args.drain(2..));
            self.args = args;
        }
        (Path::new(&self.file).is_absolute()
            && ascii(&self.file, 1024)
            && self.args.len() <= 64
            && self.args.iter().all(|arg| ascii(arg, 4096))
            && self.env.len() <= 64
            && self
                .env
                .iter()
                .all(|(key, value)| ascii(key, 128) && ascii(value, 8192)))
        .then_some(self)
    }
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
    let root = ccs.to_str()?;
    if !ccs.is_absolute()
        || !ascii(root, 1024)
        || !provider_matches
        || !account_matches
        || !aac_core::signin::valid_job_id(&start.job_id)
        || start.front_door_pid == 0
        || !start.profile_name.as_deref().is_some_and(identifier)
        || !(100..=900_000).contains(&start.timeout_ms)
        || start.allowed_origins.is_empty()
        || start.allowed_origins.len() > 8
        || !start.allowed_origins.iter().all(|origin| {
            ascii(origin, 256)
                && origin
                    .strip_prefix("https://")
                    .is_some_and(|host| !host.contains('/'))
        })
    {
        return None;
    }
    let command = start.command.bind(&start.provider)?;
    Some((
        start.timeout_ms + 60_000,
        json!({
            "action": "start",
            "jobId": start.job_id,
            "frontDoorPid": start.front_door_pid,
            "ccs": root,
            "provider": start.provider,
            "kind": start.kind,
            "mode": start.mode,
            "accountId": start.account_id,
            "profileName": start.profile_name,
            "platform": start.platform,
            "allowedOrigins": start.allowed_origins,
            "timeoutMs": start.timeout_ms,
            "command": {
                "file": command.file,
                "args": command.args,
                "env": command.env,
                "pty": command.pty,
            }
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn antigravity_driver_reaches_the_agent_as_printable_argv_without_altering_the_program() {
        let program = "import sys\nassert sys.argv[1] == 'fixture'\n";
        let command = Command {
            file: "/usr/bin/python3".into(),
            args: vec!["-c".into(), program.into(), "fixture".into()],
            env: Default::default(),
            pty: false,
        }
        .bind("antigravity")
        .expect("driver fits the agent argv");
        assert!(command.args.iter().all(|arg| ascii(arg, 4096)));
        assert_eq!(command.args[2], "1");
        assert_eq!(
            STANDARD.decode(&command.args[3]).expect("encoded driver"),
            program.as_bytes()
        );
        assert_eq!(command.args[4], "fixture");
    }

    #[cfg(windows)]
    #[test]
    fn the_unix_antigravity_driver_is_refused_on_windows() {
        let command = Command {
            file: "/usr/bin/python3".into(),
            args: vec!["-c".into(), "import sys\n".into()],
            env: Default::default(),
            pty: false,
        };
        assert!(command.bind("antigravity").is_none());
    }

    #[test]
    fn lifecycle_uses_one_boot_scoped_journal_even_for_a_muse_job() {
        let (root, ssh) = if cfg!(windows) {
            (r"C:\fixture\ccs", r"C:\fixture\ssh.exe")
        } else {
            ("/fixture/ccs", "/fixture/ssh")
        };
        let mut request = json!({
            "jobId":"job_0123456789abcdef","frontDoorPid":42,"provider":"muse",
            "kind":"device-code","mode":"signin-again","accountId":"muse:usage",
            "profileName":"muse:usage","platform":"mac",
            "allowedOrigins":["https://auth.meta.com"],"timeoutMs":300000,
            "command":{"file":ssh,"args":[],"env":{},"pty":false}
        });
        let (deadline, wire) =
            agent_start(Path::new(root), &request.to_string()).expect("valid prepared flow");
        assert_eq!(deadline, 360_000);
        assert_eq!(wire["ccs"], root);
        assert_eq!(wire["action"], "start");
        assert_eq!(wire["platform"], "mac");
        request["ccs"] = json!("/another/journal");
        assert!(agent_start(Path::new(root), &request.to_string()).is_none());
    }
}
