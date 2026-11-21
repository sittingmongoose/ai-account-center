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
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
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
    CodexLogin { path: String, complete: bool },
    /// Whether a saved profile holds the live native login.
    CodexSavedLive { native: String, profile: String },
    /// Whether a fresh login is a saved profile's own account and person.
    CodexSameIdentity {
        saved: Option<IdentityInput>,
        meta_email: Option<String>,
        meta_account: Option<String>,
        fresh: IdentityInput,
    },
    /// The fixed Muse remote login and email commands for a config home.
    MuseCommands { home_id: Option<String> },
    /// Validate the Muse login email and map it to the job result.
    MuseValidateEmail { account_id: String, email: String },
    /// The Antigravity bubblewrap argv and allowlisted environment for a staging folder.
    AntigravitySandbox {
        bus_config: String,
        mask: String,
        stubs: String,
        real_home: String,
        native_binary: String,
        apparmor_leaf: Option<String>,
        runtime_dir: Option<String>,
        env: Value,
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
#[doc(hidden)]
pub fn run() -> i32 {
    let mut bytes = Vec::new();
    if io::stdin()
        .take(INPUT_MAX + 1)
        .read_to_end(&mut bytes)
        .is_err()
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
        Request::CodexPrepare {
            root,
            staging,
            shared,
            cli,
            env,
        } => codex_prepare(Path::new(&root), &staging, Path::new(&shared), &cli, &env),
        Request::CodexLogin { path, complete } => {
            let (identity, bytes) =
                aac_files::codex_identity::read_login(Path::new(&path), complete)?;
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
        Request::CodexSameIdentity {
            saved,
            meta_email,
            meta_account,
            fresh,
        } => {
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
        Request::MuseCommands { home_id } => muse_commands(home_id.as_deref()),
        Request::MuseValidateEmail { account_id, email } => {
            let trimmed = email.trim();
            if !muse_email_valid(trimmed) {
                return Some(json!({"status": "refused", "code": "write_failed"}));
            }
            Some(json!({
                "status": "ok",
                "result": {"accountId": account_id, "email": trimmed, "plan": null},
            }))
        }
        Request::AntigravitySandbox {
            bus_config,
            mask,
            stubs,
            real_home,
            native_binary,
            apparmor_leaf,
            runtime_dir,
            env,
        } => antigravity_sandbox(
            &bus_config,
            &mask,
            &stubs,
            &real_home,
            &native_binary,
            apparmor_leaf.as_deref(),
            runtime_dir.as_deref(),
            &env,
        ),
    }
}

pub(crate) fn codex_prepare(
    root: &Path,
    staging: &str,
    shared: &Path,
    cli: &str,
    env: &Value,
) -> Option<Value> {
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

/// The fixed remote Python that prints only the Muse login email (`EMAIL_PROGRAM`).
const MUSE_EMAIL_PROGRAM: &str = r#"import json,os,sys; p=os.path.join(os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config"), "muse", "auth.json"); v=json.load(open(p)); m=(v.get("providers") or {}).get("meta") or {}; e=m.get("user_email"); sys.stdout.write(e if isinstance(e, str) and e else "")"#;

fn muse_home_id(home_id: Option<&str>) -> Option<&str> {
    match home_id {
        None => Some(""),
        Some(id) => {
            let valid = id.len() == 8
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            valid.then_some(id)
        }
    }
}

/// The fixed Muse remote login and email commands (`remoteLoginCommand`/`remoteEmailCommand`).
pub(crate) fn muse_commands(home_id: Option<&str>) -> Option<Value> {
    let id = muse_home_id(home_id)?;
    let prefix = if id.is_empty() {
        String::new()
    } else {
        format!("env XDG_CONFIG_HOME=\"$HOME/.ccs/muse-homes/{id}\" ")
    };
    let remote_login = format!("{prefix}PATH=\"$HOME/.local/bin:$PATH\" muse login");
    let email_prefix = if id.is_empty() {
        String::new()
    } else {
        format!("XDG_CONFIG_HOME=\"$HOME/.ccs/muse-homes/{id}\" ")
    };
    let remote_email = format!("{email_prefix}/usr/bin/python3 -c '{MUSE_EMAIL_PROGRAM}'");
    Some(json!({"status": "ok", "remoteLogin": remote_login, "remoteEmail": remote_email}))
}

/// The Muse login email rule: one `@`, non-empty whitespace-free local and domain, a dot inside
/// the domain, at most 254 characters (`complete`'s regex and length cap).
pub(crate) fn muse_email_valid(email: &str) -> bool {
    if email.chars().count() > 254 {
        return false;
    }
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    if local.chars().any(char::is_whitespace) || domain.chars().any(char::is_whitespace) {
        return false;
    }
    let chars: Vec<char> = domain.chars().collect();
    chars
        .iter()
        .enumerate()
        .any(|(i, c)| *c == '.' && i > 0 && i < chars.len() - 1)
}

/// The Antigravity CLI environment allowlist (`signin-sandbox.ts` PASSED_ENV), in order.
const AGY_PASSED_ENV: [&str; 19] = [
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TERM",
    "COLORTERM",
    "COLUMNS",
    "LINES",
    "SSH_CLIENT",
    "SSH_CONNECTION",
    "SSH_TTY",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];
const AGY_WORKDIR: &str = "AAC-signin";
const AGY_DBUS_RUN_SESSION: &str = "/usr/bin/dbus-run-session";

/// The exact bubblewrap argv and allowlisted environment (`sandboxArgs`/`sandboxEnvironment`).
/// Declines when the real home is not absolute, so TypeScript answers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn antigravity_sandbox(
    bus_config: &str,
    mask: &str,
    stubs: &str,
    real_home: &str,
    native_binary: &str,
    apparmor_leaf: Option<&str>,
    runtime_dir: Option<&str>,
    env: &Value,
) -> Option<Value> {
    let home = Path::new(real_home);
    if !home.is_absolute() {
        return decline();
    }
    let real_gemini = home.join(".gemini");
    let mut args: Vec<String> = vec![
        "--die-with-parent".into(),
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--ro-bind".into(),
        "/".into(),
        "/".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ];
    if let Some(runtime) = runtime_dir {
        args.push("--tmpfs".into());
        args.push(runtime.to_owned());
    }
    args.push("--ro-bind".into());
    args.push(bus_config.to_owned());
    args.push(bus_config.to_owned());
    args.push("--ro-bind".into());
    args.push(native_binary.to_owned());
    args.push(native_binary.to_owned());
    args.push("--bind".into());
    args.push(mask.to_owned());
    args.push(real_gemini.to_str()?.to_owned());
    if let Some(leaf) = apparmor_leaf {
        args.push("--bind".into());
        args.push(leaf.to_owned());
        args.push(leaf.to_owned());
    }
    args.push("--tmpfs".into());
    args.push(
        real_gemini
            .join("antigravity-cli")
            .join("log")
            .to_str()?
            .to_owned(),
    );
    args.push("--proc".into());
    args.push("/proc".into());
    args.push("--dev".into());
    args.push("/dev".into());
    args.push("--chdir".into());
    args.push(real_gemini.join(AGY_WORKDIR).to_str()?.to_owned());
    args.push(AGY_DBUS_RUN_SESSION.into());
    args.push("--config-file".into());
    args.push(bus_config.to_owned());
    args.push(native_binary.to_owned());

    let mut command_env = serde_json::Map::new();
    for key in AGY_PASSED_ENV {
        if let Some(value) = env.get(key).and_then(|value| value.as_str())
            && !value.bytes().any(|b| b < 0x20)
        {
            command_env.insert(key.to_owned(), json!(value));
        }
    }
    let term = command_env
        .get("TERM")
        .and_then(|value| value.as_str())
        .unwrap_or("xterm-256color")
        .to_owned();
    command_env.insert("HOME".to_owned(), json!(home.to_str()?));
    command_env.insert("PATH".to_owned(), json!(format!("{stubs}:/usr/bin:/bin")));
    command_env.insert(
        "BROWSER".to_owned(),
        json!(Path::new(stubs).join("xdg-open").to_str()?),
    );
    command_env.insert("TERM".to_owned(), json!(term));
    Some(json!({"status": "ok", "sandboxArgs": args, "env": Value::Object(command_env)}))
}

/// One registry mutation under the caller-held directory lease.
#[cfg(unix)]
#[derive(Deserialize)]
#[serde(
    tag = "edit",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum RegistryEdit {
    /// Insert a profile (`createProfile`); `meta` is spread over the base in order.
    Create {
        name: String,
        created: String,
        meta: Vec<(String, FieldValue)>,
    },
    /// Merge a partial update (`updateProfile`).
    Update {
        name: String,
        partial: Vec<(String, FieldValue)>,
    },
}

/// A metadata field value: a string or null.
#[cfg(unix)]
#[derive(Deserialize)]
#[serde(untagged)]
enum FieldValue {
    /// A string scalar.
    Str(String),
    /// A null scalar.
    Null,
}

#[cfg(unix)]
impl From<FieldValue> for aac_files::codex_registry::Field {
    fn from(value: FieldValue) -> Self {
        match value {
            FieldValue::Str(text) => Self::Str(text),
            FieldValue::Null => Self::Null,
        }
    }
}

#[cfg(unix)]
fn apply_edit(
    registry: &mut aac_files::codex_registry::Registry,
    edit: RegistryEdit,
) -> Result<(), &'static str> {
    match edit {
        RegistryEdit::Create {
            name,
            created,
            meta,
        } => {
            let meta = meta
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect();
            registry.create(&name, &created, meta).map_err(|_| "exists")
        }
        RegistryEdit::Update { name, partial } => {
            let partial = partial
                .into_iter()
                .map(|(key, value)| (key, value.into()))
                .collect();
            registry.update(&name, partial).map_err(|_| "not_found")
        }
    }
}

/// Apply a registry mutation while the caller holds the registry directory lease (inherited on
/// stderr). Prints `committed` or `refused`; exit 74 declines so TypeScript answers.
#[cfg(unix)]
pub fn registry_edit() -> i32 {
    use std::os::fd::AsFd;
    let Some(root) = std::env::args_os()
        .nth(2)
        .and_then(|arg| arg.to_str().map(str::to_owned))
    else {
        return 74;
    };
    let root = Path::new(&root);
    let Ok(fd) = io::stderr().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let marker = format!("{}.lock", root.display());
    let Ok(lease) =
        aac_files::profile_writes::LegacyLease::verify(Path::new(&marker), std::fs::File::from(fd))
    else {
        return 74;
    };
    let mut bytes = Vec::new();
    if io::stdin()
        .take(INPUT_MAX + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > INPUT_MAX
    {
        return 74;
    }
    let Ok(edit) = serde_json::from_slice::<RegistryEdit>(&bytes) else {
        return 74;
    };
    let Some(mut registry) = aac_files::codex_registry::read(&root.join("codex-profiles.yaml"))
    else {
        return 74;
    };
    if let Err(code) = apply_edit(&mut registry, edit) {
        println!("{}", json!({"status": "refused", "code": code}));
        return 0;
    }
    let Some(emitted) = aac_files::codex_registry::emit(&registry) else {
        return 74;
    };
    match lease.codex_metadata(root, emitted.as_bytes()) {
        Ok(()) => {
            println!("{}", json!({"status": "committed"}));
            0
        }
        Err(_) => 75,
    }
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
        let answer = codex_prepare(
            root,
            ".staging-job_0123456789abcdef",
            &shared,
            "/fake/codex",
            &env,
        )
        .unwrap();
        assert_eq!(answer["status"], "ok");
        let command = &answer["command"];
        assert_eq!(command["file"], "/fake/codex");
        assert_eq!(command["args"], json!(["login", "--device-auth"]));
        assert_eq!(command["pty"], true);
        assert_eq!(command["env"]["TERM"], "dumb");
        assert_eq!(command["env"]["NO_COLOR"], "1");
        assert_eq!(command["env"]["PATH"], "/usr/bin");
        assert!(command["env"].get("AAC_PRIVATE").is_none());
        assert!(
            command["env"]["CODEX_HOME"]
                .as_str()
                .unwrap()
                .ends_with(".staging-job_0123456789abcdef")
        );
        assert!(
            root.join("codex-instances/.staging-job_0123456789abcdef/config.toml")
                .exists()
        );
    }

    #[test]
    fn prepare_refuses_without_a_cli() {
        let dir = tempfile::tempdir().unwrap();
        let answer = codex_prepare(
            dir.path(),
            ".staging-job_0123456789abcdef",
            Path::new("/none"),
            "",
            &json!({}),
        )
        .unwrap();
        assert_eq!(answer["status"], "refused");
        assert_eq!(answer["code"], "tool_missing");
    }

    #[test]
    fn muse_commands_match_the_fixed_remote_strings() {
        let answer = muse_commands(None).unwrap();
        assert_eq!(
            answer["remoteLogin"],
            "PATH=\"$HOME/.local/bin:$PATH\" muse login"
        );
        let email = answer["remoteEmail"].as_str().unwrap();
        assert!(email.starts_with("/usr/bin/python3 -c '"));
        assert!(email.contains("user_email"));
        let home = muse_commands(Some("9f2c41d0")).unwrap();
        assert_eq!(
            home["remoteLogin"],
            "env XDG_CONFIG_HOME=\"$HOME/.ccs/muse-homes/9f2c41d0\" PATH=\"$HOME/.local/bin:$PATH\" muse login"
        );
        assert!(
            home["remoteEmail"]
                .as_str()
                .unwrap()
                .starts_with("XDG_CONFIG_HOME=\"$HOME/.ccs/muse-homes/9f2c41d0\" /usr/bin/python3")
        );
        assert!(muse_commands(Some("BAD")).is_none());
    }

    #[test]
    fn muse_email_rule() {
        assert!(muse_email_valid("muse-user@example.test"));
        assert!(!muse_email_valid(""));
        assert!(!muse_email_valid("not-an-email"));
        assert!(!muse_email_valid(&"x".repeat(300)));
        assert!(!muse_email_valid("a@b"));
        assert!(!muse_email_valid("a@@b.c"));
        assert!(!muse_email_valid("a b@c.d"));
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_sandbox_builds_bwrap_argv_and_env() {
        let env =
            json!({"USER": "u", "TERM": "dumb", "LANG": "C", "SECRET": "x", "BAD": "a\u{1}b"});
        let answer = antigravity_sandbox(
            "/ccs/s/bus.conf",
            "/ccs/s/home/.gemini",
            "/ccs/s/browser-stubs",
            "/home/u",
            "/home/u/.local/bin/agy",
            Some("/sys/kernel/security/apparmor/.access"),
            Some("/run/user/1000"),
            &env,
        )
        .unwrap();
        assert_eq!(answer["status"], "ok");
        let args: Vec<&str> = answer["sandboxArgs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            &args[..10],
            &[
                "--die-with-parent",
                "--unshare-user",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--ro-bind",
                "/",
                "/",
                "--tmpfs",
                "/tmp"
            ]
        );
        assert!(args.windows(2).any(|w| w == ["--tmpfs", "/run/user/1000"]));
        assert!(
            args.windows(3)
                .any(|w| w == ["--bind", "/ccs/s/home/.gemini", "/home/u/.gemini"])
        );
        assert!(
            args.windows(2)
                .any(|w| w == ["--chdir", "/home/u/.gemini/AAC-signin"])
        );
        assert_eq!(args.last(), Some(&"/home/u/.local/bin/agy"));
        let e = &answer["env"];
        assert_eq!(e["HOME"], "/home/u");
        assert_eq!(e["TERM"], "dumb");
        assert_eq!(e["USER"], "u");
        assert!(e.get("SECRET").is_none());
        assert!(e.get("BAD").is_none());
        assert_eq!(e["PATH"], "/ccs/s/browser-stubs:/usr/bin:/bin");
        assert_eq!(e["BROWSER"], "/ccs/s/browser-stubs/xdg-open");
    }
}
