//! Native sign-in prepare/install callbacks for the compatibility front door. The TypeScript
//! owner holds its locks and admits the request; these actions carry the provider logic that
//! used to run in TypeScript. Each answer is `ok` (use it), `decline` (TypeScript answers) or
//! `refused` (a definitive provider refusal). Credential bytes never leave the private pipe;
//! identity answers carry display claims and the private binding only, plus a byte digest.
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read};
use std::path::Path;

const INPUT_MAX: u64 = 256 * 1024;

/// The Codex environment keys the CLI may see, in `codex-account-lifecycle.ts` PASSED_ENV order.
const CODEX_PASSED_ENV: [&str; 15] = [
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "HTTPS_PROXY", "HTTP_PROXY",
    "NO_PROXY", "https_proxy", "http_proxy", "no_proxy", "SSL_CERT_FILE", "SSL_CERT_DIR",
];

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", rename_all_fields = "camelCase", deny_unknown_fields)]
enum Request {
    /// Build the Codex device-auth command after creating its private staging folder.
    CodexPrepare {
        root: String,
        staging: String,
        shared: String,
        cli: String,
        env: Value,
    },
    /// Decode a local Codex login file's identity and digest its bytes.
    CodexLogin {
        path: String,
        complete: bool,
    },
    /// Whether a saved profile holds the live native login.
    CodexSavedLive { native: String, profile: String },
    /// Whether a fresh login is a saved profile's own account and person.
    CodexSameIdentity {
        saved: Option<IdentityInput>,
        meta_email: Option<String>,
        meta_account: Option<String>,
        fresh: IdentityInput,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdentityInput {
    email: String,
    account_id: Option<String>,
    plan: Option<String>,
    binding: Option<BindingInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingInput {
    account_id: String,
    email: String,
    user_id: Option<String>,
    subject: Option<String>,
    issuer: Option<String>,
}

impl From<BindingInput> for aac_files::codex_identity::Binding {
    fn from(value: BindingInput) -> Self {
        Self {
            account_id: value.account_id,
            email: value.email,
            user_id: value.user_id,
            subject: value.subject,
            issuer: value.issuer,
        }
    }
}

impl From<IdentityInput> for aac_files::codex_identity::Identity {
    fn from(value: IdentityInput) -> Self {
        Self {
            email: value.email,
            account_id: value.account_id,
            plan: value.plan,
            binding: value.binding.map(Into::into),
        }
    }
}

fn identity_json(identity: &aac_files::codex_identity::Identity) -> Value {
    let binding = identity.binding.as_ref().map(|binding| {
        let mut map = serde_json::Map::new();
        map.insert("accountId".to_owned(), json!(binding.account_id));
        map.insert("email".to_owned(), json!(binding.email));
        if let Some(user) = &binding.user_id {
            map.insert("userId".to_owned(), json!(user));
        }
        if let Some(subject) = &binding.subject {
            map.insert("subject".to_owned(), json!(subject));
        }
        if let Some(issuer) = &binding.issuer {
            map.insert("issuer".to_owned(), json!(issuer));
        }
        Value::Object(map)
    });
    json!({
        "email": identity.email,
        "accountId": identity.account_id,
        "plan": identity.plan,
        "binding": binding,
    })
}

/// Run one callback request from stdin and print its JSON answer.
pub(crate) fn run() -> i32 {
    let mut bytes = Vec::new();
    if io::stdin().take(INPUT_MAX + 1).read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > INPUT_MAX
    {
        return 74;
    }
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return 74;
    };
    let Some(answer) = evaluate(request) else {
        return 74;
    };
    println!("{answer}");
    0
}
fn decline() -> Option<Value> {
    Some(json!({"status": "decline"}))
}

fn evaluate(request: Request) -> Option<Value> {
    match request {
        Request::CodexPrepare { root, staging, shared, cli, env } => {
            codex_prepare(Path::new(&root), &staging, Path::new(&shared), &cli, &env)
        }
        Request::CodexLogin { path, complete } => {
            let (identity, bytes) = aac_files::codex_identity::read_login(Path::new(&path), complete)?;
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(&bytes);
            let sha256: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            Some(json!({"status": "ok", "identity": identity_json(&identity), "sha256": sha256}))
        }
        Request::CodexSavedLive { native, profile } => {
            let live = aac_files::codex_identity::saved_login_is_live_checked(
                Path::new(&native),
                Path::new(&profile),
            )?;
            Some(json!({"status": "ok", "live": live}))
        }
        Request::CodexSameIdentity { saved, meta_email, meta_account, fresh } => {
            let saved: Option<aac_files::codex_identity::Identity> = saved.map(Into::into);
            let fresh: aac_files::codex_identity::Identity = fresh.into();
            let same = aac_files::codex_identity::same_codex_identity(
                saved.as_ref(),
                meta_email.as_deref(),
                meta_account.as_deref(),
                &fresh,
            );
            Some(json!({"status": "ok", "same": same}))
        }
    }
}

fn codex_prepare(root: &Path, staging: &str, shared: &Path, cli: &str, env: &Value) -> Option<Value> {
    if cli.is_empty() {
        return Some(json!({"status": "refused", "code": "tool_missing"}));
    }
    // The native staging layout is the canonical codex-instances folder under the root.
    if aac_files::profile_staging::prepare(root, staging, shared).is_err() {
        return decline();
    }
    let codex_home = root.join("codex-instances").join(staging);
    let mut command_env = serde_json::Map::new();
    for key in CODEX_PASSED_ENV {
        if let Some(value) = env.get(key).and_then(|value| value.as_str()) {
            command_env.insert(key.to_owned(), json!(value));
        }
    }
    command_env.insert("TERM".to_owned(), json!("dumb"));
    command_env.insert("NO_COLOR".to_owned(), json!("1"));
    command_env.insert("CODEX_HOME".to_owned(), json!(codex_home.to_str()?));
    Some(json!({
        "status": "ok",
        "command": {
            "file": cli,
            "args": ["login", "--device-auth"],
            "env": Value::Object(command_env),
            "pty": true,
        }
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn prepare_builds_the_device_auth_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("codex-instances")).unwrap();
        let shared = root.join("config.toml");
        std::fs::write(&shared, "model = \"fixture\"\n").unwrap();
        let env = json!({"PATH": "/usr/bin", "HOME": "/home/x", "AAC_PRIVATE": "never"});
        let answer = codex_prepare(root, ".staging-job_0123456789abcdef", &shared, "/fake/codex", &env).unwrap();
        assert_eq!(answer["status"], "ok");
        let command = &answer["command"];
        assert_eq!(command["file"], "/fake/codex");
        assert_eq!(command["args"], json!(["login", "--device-auth"]));
        assert_eq!(command["pty"], true);
        assert_eq!(command["env"]["TERM"], "dumb");
        assert_eq!(command["env"]["NO_COLOR"], "1");
        assert_eq!(command["env"]["PATH"], "/usr/bin");
        assert!(command["env"].get("AAC_PRIVATE").is_none());
        assert!(command["env"]["CODEX_HOME"].as_str().unwrap().ends_with(".staging-job_0123456789abcdef"));
        assert!(root.join("codex-instances/.staging-job_0123456789abcdef/config.toml").exists());
    }

    #[test]
    fn prepare_refuses_without_a_cli() {
        let dir = tempfile::tempdir().unwrap();
        let answer = codex_prepare(dir.path(), ".staging-job_0123456789abcdef", Path::new("/none"), "", &json!({})).unwrap();
        assert_eq!(answer["status"], "refused");
        assert_eq!(answer["code"], "tool_missing");
    }
}
