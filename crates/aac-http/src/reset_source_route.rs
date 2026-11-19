//! M5 lane K, gap G11: the reset source in the private service. It replaces the TS
//! `reset-source-adapter.ts`: account resolution (the saved Codex profile, the Claude launcher
//! inventory), the ticketed reads and the one provider send. Every rule is ported from the TS
//! adapter and its helpers (`codex-auth-dashboard-service.ts`, `claude-desktop-profile-service.ts`,
//! `usage-failover-transport.ts`, `ssh-policy.ts`, `dashboard-hosts.ts`) and the transactions and
//! host programs are the existing native ones (`aac_status::reset_source`, `reset_claude_source`).
//!
//! Send rules, exactly as the adapter: a Codex failure is an error (the route records the attempt
//! ambiguous). A Claude failure before the host command is dispatched proves nothing was sent and
//! answers `not-sent`; after that point every failure is an error, never a retry.
use aac_status::reset_claude_source::HelperCommand;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    ffi::{CString, OsStr, OsString},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

type Res<T> = Result<T, &'static str>;

/// `usageHelperProcessTimeoutMs('inventory')`: the claim runs under the inventory request.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(12);
const BOOTSTRAP: &str = "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));sys.stdin=io.StringIO(json.dumps(_p['request']));exec(compile(_p['helperSource'],'managed-usage','exec'))";
const SSH_ISOLATION: [&str; 13] = [
    "BatchMode=yes",
    "ConnectTimeout=5",
    "ConnectionAttempts=1",
    "StrictHostKeyChecking=yes",
    "VerifyHostKeyDNS=no",
    "UpdateHostKeys=no",
    "ForwardAgent=no",
    "ForwardX11=no",
    "GlobalKnownHostsFile=none",
    "PermitLocalCommand=no",
    "ClearAllForwardings=yes",
    "ControlMaster=no",
    "ControlPath=none",
];
const CAPABILITY_OPTIONS: [&str; 2] = ["KnownHostsCommand=none", "SetEnv=AAC_SSH_POLICY=1"];
const PROBE_FAILURE_TTL: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the source reads from its process; tests build it directly.
pub(crate) struct SourceContext {
    /// The canonical private root the service owns.
    pub ccs: PathBuf,
    /// The package root (`AAC_AGENT_PACKAGE_ROOT`), for `scripts/account-usage/claude_usage.py`.
    pub package: PathBuf,
    /// `CCS_HOME ?? HOME`, the default known-hosts pin's home.
    pub home: OsString,
    /// `AAC_AGENT_IDENTITY_FILE`.
    pub identity_file: Option<String>,
    /// `AAC_AGENT_KNOWN_HOSTS_FILE`.
    pub known_hosts_file: Option<String>,
    /// `os.hostname()`.
    pub hostname: String,
    /// The PATH the SSH client is found on and runs with.
    pub path: OsString,
}

impl SourceContext {
    pub(crate) fn from_env(ccs: &Path) -> Res<Self> {
        let package = std::env::var_os("AAC_AGENT_PACKAGE_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or("credential-host-unavailable")?;
        Ok(Self {
            ccs: ccs.to_path_buf(),
            package,
            home: std::env::var_os("CCS_HOME")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap_or_default(),
            identity_file: std::env::var("AAC_AGENT_IDENTITY_FILE").ok(),
            known_hosts_file: std::env::var("AAC_AGENT_KNOWN_HOSTS_FILE").ok(),
            hostname: hostname(),
            path: std::env::var_os("PATH").unwrap_or_default(),
        })
    }
}

fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer outlives the call and its length is passed; the result is truncated
    // to the first NUL below.
    #[allow(unsafe_code)]
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if status != 0 {
        return String::new();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

// ---- resolution ------------------------------------------------------------------------------

/// A resolved reset account.
pub(crate) enum Target {
    Codex {
        home: String,
        account: String,
        profile: String,
    },
    Claude {
        profile: String,
        email: String,
        profile_dir: Option<String>,
        is_default: bool,
        ssh_host: String,
    },
}

pub(crate) fn resolve(ctx: &SourceContext, account: &str) -> Res<Target> {
    if let Some(name) = account.strip_prefix("codex:") {
        let (home, account) = codex_profile(&ctx.ccs, name)?;
        return Ok(Target::Codex {
            home,
            account,
            profile: name.to_owned(),
        });
    }
    if !account.starts_with("claude:") {
        return Err("provider-read-only");
    }
    claude_profile(&ctx.ccs, account)
}

/// `codex-profiles.yaml` and the profile's `auth.json`, as `getCodexAuthProfilesSummary` reads
/// them for the adapter: a registered profile with a decodable `chatgpt_account_id`.
fn codex_profile(ccs: &Path, name: &str) -> Res<(String, String)> {
    let unavailable = "credential-host-unavailable";
    let registry = match std::fs::read(ccs.join("codex-profiles.yaml")) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("registry-unavailable"),
    };
    let profiles = match registry {
        None => Map::new(),
        Some(bytes) => codex_registry_profiles(&bytes).ok_or("registry-unavailable")?,
    };
    if !profiles.contains_key(name) {
        return Err(unavailable);
    }
    let home = ccs.join("codex-instances").join(name);
    let account = codex_account_id(&home.join("auth.json")).ok_or(unavailable)?;
    Ok((home.to_str().ok_or(unavailable)?.to_owned(), account))
}

/// `validateCodexProfileRegistryData`: the profiles map of a valid registry, or None.
fn codex_registry_profiles(bytes: &[u8]) -> Option<Map<String, Value>> {
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    let yaml: serde_yaml::Value = serde_yaml::from_slice(bytes).ok()?;
    let data: Value = serde_json::to_value(yaml).ok()?;
    let data = data.as_object()?;
    let profiles = data.get("profiles")?.as_object()?;
    let default = data.get("default");
    match default {
        None | Some(Value::Null) => {}
        Some(Value::String(name)) if valid_codex_name(name) && profiles.contains_key(name) => {}
        Some(_) => return None,
    }
    for (name, meta) in profiles {
        let meta = meta.as_object()?;
        let optional_string = |key: &str| meta.get(key).is_none_or(Value::is_string);
        if !valid_codex_name(name)
            || meta.get("type") != Some(&json!("codex"))
            || !meta.get("created").is_some_and(Value::is_string)
            || !meta
                .get("last_used")
                .is_some_and(|v| v.is_null() || v.is_string())
            || !optional_string("email")
            || !meta
                .get("plan_type")
                .is_none_or(|v| v.is_null() || v.is_string())
            || !optional_string("account_id")
        {
            return None;
        }
    }
    Some(profiles.clone())
}

/// `isValidCodexProfileName`.
fn valid_codex_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !name.is_empty()
        && name.len() <= 64
        && name != "default"
        && name != "current"
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// `decodeAccountIdentity(...).account_id`: the unverified id_token payload's
/// `https://api.openai.com/auth.chatgpt_account_id`, when the token is a well-formed JWT.
fn codex_account_id(auth: &Path) -> Option<String> {
    let raw = std::fs::read(auth).ok()?;
    let parsed: Value = serde_json::from_slice(&raw).ok()?;
    let token = parsed.get("tokens")?.get("id_token")?.as_str()?;
    let parts: Vec<&str> = token.split('.').collect();
    let segment = |part: &str| {
        !part.is_empty()
            && part.len() % 4 != 1
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if parts.len() != 3 || !parts.iter().all(|p| segment(p)) {
        return None;
    }
    // Node's Buffer decoder accepts non-canonical trailing bits; so does this one.
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let decode = |part: &str| -> Option<Value> {
        use base64::Engine;
        let bytes = engine.decode(part).ok()?;
        serde_json::from_str(&String::from_utf8_lossy(&bytes)).ok()
    };
    decode(parts[0])?.as_object()?;
    let payload = decode(parts[1])?;
    payload
        .as_object()?
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_owned)
}

/// `listClaudeDesktopProfiles` + the adapter's lookup: the launcher inventory's profile named by
/// `claude:<id ?? email>`, whose Windows launcher has an SSH alias (the service runs on Unix).
fn claude_profile(ccs: &Path, account: &str) -> Res<Target> {
    let invalid = "inventory-invalid";
    let unavailable = "credential-host-unavailable";
    let text = match std::fs::read(ccs.join("claude-desktop-profiles.json")) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(unavailable),
        Err(_) => return Err(invalid),
    };
    if js_length(&text) > 1024 * 1024 {
        return Err(invalid);
    }
    let manifest: Value = serde_json::from_str(&text).map_err(|_| invalid)?;
    let profiles = manifest
        .as_object()
        .filter(|m| m.get("version").and_then(Value::as_f64) == Some(1.0))
        .and_then(|m| m.get("profiles"))
        .and_then(Value::as_array)
        .filter(|p| p.len() <= 1000)
        .ok_or(invalid)?;
    let mut ids = std::collections::HashSet::new();
    let mut found = None;
    for profile in profiles {
        let parsed = read_profile(profile).ok_or(invalid)?;
        if let Some(id) = &parsed.id
            && !ids.insert(id.clone())
        {
            return Err(invalid);
        }
        let key = format!("claude:{}", parsed.id.as_deref().unwrap_or(&parsed.email));
        if found.is_none() && key == account {
            found = Some(parsed);
        }
    }
    let profile = found.ok_or(unavailable)?;
    let windows = profile.windows.ok_or(unavailable)?;
    let ssh_host = windows.ssh_host.clone().ok_or(unavailable)?;
    let is_default = windows.is_default == Some(true)
        || windows
            .profile_path
            .as_deref()
            .is_some_and(default_claude_data_folder);
    Ok(Target::Claude {
        profile: profile.id.unwrap_or_else(|| "default".to_owned()),
        email: profile.email,
        profile_dir: windows.profile_path,
        is_default,
        ssh_host,
    })
}

struct ClaudeProfile {
    id: Option<String>,
    email: String,
    windows: Option<Launcher>,
}
struct Launcher {
    profile_path: Option<String>,
    is_default: Option<bool>,
    ssh_host: Option<String>,
}

/// JavaScript `string.length`: UTF-16 code units.
fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}
/// JavaScript `\s`: Unicode white space plus the BOM.
fn js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}
/// `readText`.
fn read_text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    (text.trim_matches(js_space).chars().next().is_some()
        && js_length(text) <= max
        && !text.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}'))
    .then(|| text.to_owned())
}
fn read_launcher(value: &Value, windows: bool) -> Option<Launcher> {
    let value = value.as_object()?;
    read_text(value.get("launcherName"), 255)?;
    let mut profile_path = None;
    for field in ["launcherPath", "profilePath"] {
        if let Some(v) = value.get(field) {
            let text = read_text(Some(v), 4096)?;
            if field == "profilePath" {
                profile_path = Some(text);
            }
        }
    }
    if windows && let Some(v) = value.get("startMenuPath") {
        read_text(Some(v), 4096)?;
    }
    let is_default = match value.get("isDefault") {
        None => None,
        Some(v) => Some(v.as_bool()?),
    };
    let ssh_host = match value.get("sshHost") {
        None => None,
        Some(v) => {
            let host = read_text(Some(v), 128)?;
            let b = host.as_bytes();
            if !(b[0].is_ascii_alphanumeric()
                && b.iter()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c)))
            {
                return None;
            }
            Some(host)
        }
    };
    Some(Launcher {
        profile_path,
        is_default,
        ssh_host,
    })
}
fn read_profile(value: &Value) -> Option<ClaudeProfile> {
    let value = value.as_object()?;
    let email = read_text(value.get("email"), 254)?;
    if !email_shape(&email) {
        return None;
    }
    let id = match value.get("id") {
        None => None,
        Some(v) => {
            let id = read_text(Some(v), 64)?;
            let b = id.as_bytes();
            if !(b[0].is_ascii_alphanumeric()
                && b.iter()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(c)))
            {
                return None;
            }
            Some(id)
        }
    };
    let mac = match value.get("mac") {
        None => false,
        Some(v) => {
            read_launcher(v, false)?;
            true
        }
    };
    let windows = match value.get("windows") {
        None => None,
        Some(v) => Some(read_launcher(v, true)?),
    };
    if !mac && windows.is_none() {
        return None;
    }
    Some(ClaudeProfile { id, email, windows })
}
/// `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`.
fn email_shape(email: &str) -> bool {
    if email.chars().any(js_space) {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let chars: Vec<char> = domain.chars().collect();
    !local.is_empty()
        && chars
            .iter()
            .enumerate()
            .any(|(i, c)| *c == '.' && i >= 1 && i + 1 < chars.len())
}
/// `isDefaultClaudeDataFolder`.
fn default_claude_data_folder(path: &str) -> bool {
    path.trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|last| last.to_lowercase() == "claude")
}

/// `localComputer()`: the computer table's id for this host name on this OS.
pub(crate) fn local_computer(hostname: &str) -> String {
    let kind = if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let table: Value = serde_json::from_str(crate::account_reads::COMPUTERS).unwrap_or(Value::Null);
    let Some(rows) = table.as_object() else {
        return "ubuntu".to_owned();
    };
    let lower = hostname.to_lowercase();
    let names = |row: &Value| -> Vec<String> {
        row["names"]
            .as_array()
            .map(|n| {
                n.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default()
    };
    let exact = rows.iter().find(|(id, row)| {
        let mut candidates = vec![id.to_lowercase()];
        if let Some(alias) = row["sshAlias"].as_str() {
            candidates.push(alias.to_lowercase());
        }
        candidates.extend(names(row));
        candidates.contains(&lower)
    });
    let known = exact.or_else(|| {
        let mut ordered: Vec<(&String, &Value)> = rows.iter().collect();
        ordered.sort_by_key(|(_, row)| row["legacy"].as_bool() == Some(true));
        ordered
            .into_iter()
            .find(|(_, row)| names(row).iter().any(|n| lower.contains(n.as_str())))
    });
    match known {
        Some((id, row)) if row["os"].as_str() == Some(kind) => id.clone(),
        _ if kind == "linux" => "ubuntu".to_owned(),
        _ => kind.to_owned(),
    }
}

// ---- SSH policy (ssh-policy.ts) ----------------------------------------------------------------

struct Probe {
    identity: String,
    result: Result<Vec<String>, ()>,
    at: Instant,
}
static PROBES: Mutex<Option<HashMap<PathBuf, Probe>>> = Mutex::new(None);

/// The child environment of an SSH run: `sshEnvironment()` with the context's PATH.
fn ssh_environment(command: &mut Command, path: &OsStr) {
    command.env_clear();
    for name in ["HOME", "USER", "LOGNAME", "LANG", "SSH_AUTH_SOCK"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("PATH", path);
}

fn executable_identity(candidate: &Path) -> Res<(PathBuf, String)> {
    let file = std::fs::canonicalize(candidate).map_err(|_| "ssh-unavailable")?;
    let meta = std::fs::metadata(&file).map_err(|_| "ssh-unavailable")?;
    if !meta.is_file() {
        return Err("ssh-unavailable");
    }
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}.{}\0{}.{}",
        file.display(),
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    );
    Ok((file, identity))
}

fn probe(file: &Path, path: &OsStr) -> Result<Vec<String>, ()> {
    let mut args = Vec::new();
    for option in CAPABILITY_OPTIONS {
        let mut command = Command::new(file);
        ssh_environment(&mut command, path);
        let mut child = command
            .args(["-G", "-F", "none", "-o", option, "--", "localhost"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ())?;
        let deadline = Instant::now() + PROBE_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(());
                }
            }
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            use std::io::Read;
            let _ = pipe.by_ref().take(128 * 1024).read_to_string(&mut stderr);
        }
        if status.success() {
            args.extend(["-o".to_owned(), option.to_owned()]);
        } else if stderr.to_lowercase().contains("bad configuration option:") {
            tracing::warn!(
                option = option.split('=').next(),
                "ssh capability unsupported; compatibility downgrade"
            );
        } else {
            return Err(());
        }
    }
    Ok(args)
}

/// `capabilities()`: the PATH candidate and the capability options probed for its current file.
fn capabilities(path: &OsStr) -> Res<(PathBuf, Vec<String>)> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let executable = std::env::split_paths(path)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
            .join("ssh")
        })
        .find(|candidate| {
            CString::new(candidate.as_os_str().as_bytes()).is_ok_and(|c| {
                // SAFETY: a valid NUL-terminated path for the duration of the call.
                #[allow(unsafe_code)]
                let ok = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
                ok
            })
        })
        .ok_or("ssh-unavailable")?;
    for _ in 0..3 {
        let (file, identity) = executable_identity(&executable)?;
        let mut probes = PROBES.lock().map_err(|_| "ssh-unavailable")?;
        let probes = probes.get_or_insert_with(HashMap::new);
        let stale = probes.get(&file).is_none_or(|entry| {
            entry.identity != identity
                || (entry.result.is_err() && entry.at.elapsed() >= PROBE_FAILURE_TTL)
        });
        if stale {
            let result = probe(&file, path);
            probes.insert(
                file.clone(),
                Probe {
                    identity: identity.clone(),
                    result,
                    at: Instant::now(),
                },
            );
        }
        let result = probes
            .get(&file)
            .map(|entry| entry.result.clone())
            .ok_or("ssh-unavailable")?
            .map_err(|()| "ssh-unavailable")?;
        if executable_identity(&executable).ok().map(|(_, id)| id) == Some(identity) {
            return Ok((executable, result));
        }
    }
    Err("ssh-unavailable")
}

/// `sshLaunch('ssh', args)`: the probed file, refused when the argv was built for another one.
fn ssh_launch(path: &OsStr, args: &[String]) -> Res<PathBuf> {
    let (file, current) = capabilities(path)?;
    if CAPABILITY_OPTIONS
        .iter()
        .any(|option| current.iter().any(|a| a == option) != args.iter().any(|a| a == option))
    {
        return Err("ssh-unavailable");
    }
    Ok(file)
}

/// `path.join(home, rest)` on POSIX: joined, then normalized.
fn posix_join(home: &OsStr, rest: &str) -> PathBuf {
    let joined = Path::new(home).join(rest);
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() && !joined.is_absolute() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `sshConnectionOptions(alias)`.
fn connection_options(ctx: &SourceContext, alias: &str) -> Res<Vec<String>> {
    let refused = "ssh-policy-refused";
    let b = alias.as_bytes();
    if b.is_empty()
        || b.len() > 128
        || !b[0].is_ascii_alphanumeric()
        || !b
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(c))
    {
        return Err(refused);
    }
    let config = raw_config(&ctx.ccs);
    let definition = config.as_ref().and_then(|c| {
        let alias = json!(alias);
        let matches = |id: &str, row: &Value| {
            truthy(row)
                && (id == alias
                    || row.get("id") == Some(&alias)
                    || row.get("sshAlias") == Some(&alias))
        };
        match c.get("computers") {
            Some(Value::Object(rows)) => rows
                .iter()
                .find(|(id, row)| matches(id, row))
                .map(|(_, r)| r.clone()),
            Some(Value::Array(rows)) => rows
                .iter()
                .enumerate()
                .find(|(i, row)| matches(&i.to_string(), row))
                .map(|(_, r)| r.clone()),
            _ => None,
        }
    });
    let explicit = definition
        .as_ref()
        .and_then(|d| d.get("transport"))
        .filter(|t| truthy(t));
    let settings = config.as_ref().and_then(|c| c.get("agentTransportSsh"));
    // `explicit?.key || env || config.identityFile`: the first truthy one.
    let identity = [
        explicit.and_then(|t| t.get("key")).cloned(),
        ctx.identity_file.clone().map(Value::String),
        settings.and_then(|s| s.get("identityFile")).cloned(),
    ]
    .into_iter()
    .flatten()
    .find(truthy);
    // `env ?? config.knownHostsFile ?? join(home, '.ssh/known_hosts')`: the first non-nullish one.
    let pin = match (
        &ctx.known_hosts_file,
        settings.and_then(|s| s.get("knownHostsFile")),
    ) {
        (Some(file), _) => Value::String(file.clone()),
        (None, Some(value)) if !value.is_null() => value.clone(),
        _ => Value::String(
            posix_join(&ctx.home, ".ssh/known_hosts")
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let option_path = |value: &Value| -> Res<String> {
        let file = value.as_str().ok_or(refused)?;
        let expanded = match file.strip_prefix("~/") {
            Some(rest) => posix_join(&ctx.home, rest).to_string_lossy().into_owned(),
            None => file.to_owned(),
        };
        if !expanded.starts_with('/') || expanded.contains(['\r', '\n', '%', '"', '\\']) {
            return Err(refused);
        }
        Ok(expanded)
    };
    let mut args = vec![
        "-o".to_owned(),
        format!("UserKnownHostsFile=\"{}\"", option_path(&pin)?),
    ];
    if let Some(host) = explicit.and_then(|t| t.get("host")).filter(|h| truthy(h)) {
        let host = host.as_str().ok_or(refused)?;
        if host.starts_with('-')
            || !host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".:_-".contains(&c))
        {
            return Err(refused);
        }
        args.extend(["-o".to_owned(), format!("Hostname={host}")]);
    }
    if let Some(user) = explicit.and_then(|t| t.get("user")).filter(|u| truthy(u)) {
        let user = user.as_str().ok_or(refused)?;
        if user.is_empty()
            || !user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(refused);
        }
        args.extend(["-o".to_owned(), format!("User={user}")]);
    }
    if let Some(port) = explicit.and_then(|t| t.get("port")) {
        let port = port
            .as_f64()
            .filter(|p| p.fract() == 0.0 && (1.0..=65535.0).contains(p))
            .ok_or(refused)?;
        args.extend(["-o".to_owned(), format!("Port={}", port as u32)]);
    }
    if let Some(identity) = identity {
        args.extend([
            "-i".to_owned(),
            option_path(&identity)?,
            "-o".to_owned(),
            "IdentitiesOnly=yes".to_owned(),
        ]);
    }
    Ok(args)
}

/// JavaScript truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `loadUnifiedConfig()`: the raw YAML document, or null when absent or unreadable.
fn raw_config(ccs: &Path) -> Option<Value> {
    let bytes = std::fs::read(ccs.join("config.yaml")).ok()?;
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&bytes).ok()?;
    serde_json::to_value(yaml).ok().filter(Value::is_object)
}

/// `sshArgv(alias, command, {interval: 5, countMax: 3})`.
fn ssh_argv(ctx: &SourceContext, alias: &str, command: &str) -> Res<Vec<String>> {
    let connection = connection_options(ctx, alias)?;
    let (_, capabilities) = capabilities(&ctx.path)?;
    let mut argv = vec!["-T".to_owned()];
    for value in SSH_ISOLATION {
        argv.extend(["-o".to_owned(), value.to_owned()]);
    }
    argv.extend(capabilities);
    argv.extend(connection);
    argv.extend([
        "-o".to_owned(),
        "ServerAliveInterval=5".to_owned(),
        "-o".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        "--".to_owned(),
        alias.to_owned(),
        command.to_owned(),
    ]);
    Ok(argv)
}

/// `usageHelperCommand('windows')`.
pub(crate) fn windows_helper_command() -> String {
    use base64::Engine;
    let script = [
        "$ErrorActionPreference = 'Stop'".to_owned(),
        "$ProgressPreference = 'SilentlyContinue'".to_owned(),
        "$env:PYTHONIOENCODING = 'utf-8'".to_owned(),
        "$env:PYTHONUTF8 = '1'".to_owned(),
        "$venv = [IO.Path]::Combine($HOME, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe')".to_owned(),
        "$python = if (Test-Path -LiteralPath $venv -PathType Leaf) { $venv } else { (Get-Command python3.exe,python.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source }".to_owned(),
        "if (-not $python) { exit 1 }".to_owned(),
        format!("& $python -c '{}'", BOOTSTRAP.replace('\'', "''")),
        "exit $LASTEXITCODE".to_owned(),
    ]
    .join("; ");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16)
    )
}

/// The adapter's `source`: the packaged Claude helper wrapped so it runs `reset_host(request)`.
fn claude_helper_source(ctx: &SourceContext) -> Res<String> {
    let script = std::fs::read_to_string(ctx.package.join("scripts/account-usage/claude_usage.py"))
        .map_err(|_| "credential-host-unavailable")?;
    let literal = serde_json::to_string(&serde_json::to_string(&script).map_err(|_| "shape")?)
        .map_err(|_| "shape")?;
    Ok([
        "import json,sys".to_owned(),
        "scope={\"__name__\":\"aac_private_reset\"}".to_owned(),
        format!("exec(compile(json.loads({literal}),\"managed-reset\",\"exec\"),scope)"),
        "request=json.load(sys.stdin)".to_owned(),
        "print(json.dumps(scope[\"reset_host\"](request),allow_nan=False))".to_owned(),
    ]
    .join("\n"))
}

fn claude_input(target: &Target, write: Option<&Value>) -> Value {
    let Target::Claude {
        profile,
        email,
        profile_dir,
        is_default,
        ssh_host,
    } = target
    else {
        return Value::Null;
    };
    json!({
        "action": if write.is_some() { "claim" } else { "read" },
        "profile": profile,
        "expectedEmail": email,
        "profileDir": profile_dir,
        "isDefault": is_default,
        "host": format!("windows:{ssh_host}"),
        "write": write.cloned().unwrap_or(Value::Null),
    })
}

/// `privateUsageInvocation` + `sshLaunch`: the exact command the native helper spawns.
fn claude_command(
    ctx: &SourceContext,
    ssh_host: &str,
    helper_source: String,
    input: Value,
) -> Res<HelperCommand> {
    let args = ssh_argv(ctx, ssh_host, &windows_helper_command())?;
    let executable = ssh_launch(&ctx.path, &args)?;
    Ok(HelperCommand {
        executable: executable.to_str().ok_or("ssh-unavailable")?.to_owned(),
        args,
        payload: json!({"helperSource": helper_source, "request": input}),
    })
}

// ---- operations ------------------------------------------------------------------------------

/// `sourceRead` and `sourceSend` on the claims file at `db`.
pub(crate) fn call(ctx: &SourceContext, db: &Path, body: &[u8]) -> Res<Value> {
    let input: Map<String, Value> = serde_json::from_slice(body).map_err(|_| "shape")?;
    match input.get("op").and_then(Value::as_str) {
        Some("sourceRead") if input.len() == 2 => {
            let account = input
                .get("routeAccount")
                .and_then(Value::as_str)
                .ok_or("shape")?;
            read(ctx, db, account)
        }
        Some("sourceSend") if input.len() == 2 => {
            let write = input
                .get("write")
                .filter(|w| w.is_object())
                .ok_or("shape")?;
            send(ctx, write)
        }
        _ => Err("shape"),
    }
}

/// A ticketed read through the native capture transactions on the claims file.
fn read(ctx: &SourceContext, db: &Path, account: &str) -> Res<Value> {
    let target = resolve(ctx, account)?;
    let body = match &target {
        Target::Codex {
            home,
            account: expected,
            profile,
        } => json!({
            "op": "sourceCodexRead",
            "routeAccount": account,
            "home": home,
            "expectedAccount": expected,
            "profile": profile,
            "host": local_computer(&ctx.hostname),
        }),
        Target::Claude { ssh_host, .. } => {
            let source = claude_helper_source(ctx)?;
            let command = claude_command(ctx, ssh_host, source, claude_input(&target, None))?;
            json!({
                "op": "sourceClaudeRead",
                "routeAccount": account,
                "command": {"executable": command.executable, "args": command.args, "payload": command.payload},
            })
        }
    };
    aac_status::reset::service_call(db, body.to_string().as_bytes())
}

/// The one provider send for an attempt the ledger began. Never retried here.
fn send(ctx: &SourceContext, write: &Value) -> Res<Value> {
    let account = write
        .get("routeAccount")
        .and_then(Value::as_str)
        .ok_or("shape")?;
    if account.starts_with("claude:") {
        // Everything before the host command is dispatched proves nothing was sent.
        let prepared = (|| -> Res<(Target, String)> {
            let target = resolve(ctx, account)?;
            let source = claude_helper_source(ctx)?;
            Ok((target, source))
        })();
        let Ok((target, source)) = prepared else {
            return Ok(json!({"outcome": "not-sent"}));
        };
        let Target::Claude { ssh_host, .. } = &target else {
            return Ok(json!({"outcome": "not-sent"}));
        };
        let input = claude_input(&target, Some(write));
        // Dispatched from here: a failure may follow a spend, so it is an error (ambiguous).
        let command =
            claude_command(ctx, ssh_host, source, input.clone()).map_err(|_| "host-unconfirmed")?;
        return aac_status::reset_claude_source::run(&command, input, CLAIM_TIMEOUT)
            .map_err(|_| "host-unconfirmed");
    }
    let Target::Codex {
        home,
        account: expected,
        profile,
    } = resolve(ctx, account)?
    else {
        return Err("provider-read-only");
    };
    aac_status::reset_source::dispatch(
        &json!({
            "home": home,
            "expectedAccount": expected,
            "profile": profile,
            "host": local_computer(&ctx.hostname),
            "write": write,
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn default_folder_email_and_name_rules_match_the_ts_helpers() {
        for (path, expected) in [
            ("C:\\Users\\x\\AppData\\Roaming\\Claude", true),
            ("C:\\Users\\x\\AppData\\Roaming\\Claude\\\\", true),
            ("/Users/x/Library/Application Support/claude/", true),
            ("C:\\Profiles\\Claude-work", false),
            ("", false),
        ] {
            assert_eq!(default_claude_data_folder(path), expected, "{path}");
        }
        for (email, ok) in [
            ("a@b.c", true),
            ("fixture@example.com", true),
            ("a@b.", false),
            ("a@.b", false),
            ("a@@b.c", false),
            ("a b@c.d", false),
            ("@b.c", false),
            ("a@b..c", true),
        ] {
            assert_eq!(email_shape(email), ok, "{email}");
        }
        assert!(valid_codex_name("work-2"));
        for bad in [
            "",
            "default",
            "current",
            "Work",
            "_x",
            "-x",
            "a/b",
            &"a".repeat(65),
        ] {
            assert!(!valid_codex_name(bad), "{bad}");
        }
    }

    #[test]
    fn local_computer_follows_the_shared_table() {
        assert_eq!(local_computer("ubuntuserver"), "ubuntu");
        assert_eq!(local_computer("ubuntu-agent-nas1"), "nas1");
        assert_eq!(local_computer("somebox"), "ubuntu");
        // A Windows name on a Linux host is not that computer.
        assert_eq!(local_computer("AlexisGamingPC"), "ubuntu");
    }

    #[test]
    fn the_windows_helper_command_is_the_encoded_bootstrap() {
        use base64::Engine;
        let command = windows_helper_command();
        let b64 = command
            .strip_prefix("powershell.exe -NoProfile -NonInteractive -EncodedCommand ")
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.starts_with("$ErrorActionPreference = 'Stop'; "));
        assert!(script.contains("_p=json.loads(sys.stdin.buffer.read().decode(''utf-8''))"));
        assert!(script.ends_with("; exit $LASTEXITCODE"));
    }

    #[test]
    fn registry_validation_follows_validate_codex_profile_registry_data() {
        let ok = b"version: '1'\ndefault: work\nprofiles:\n  work:\n    type: codex\n    created: '2026-11-01T00:00:00Z'\n    last_used: null\n";
        assert!(codex_registry_profiles(ok).unwrap().contains_key("work"));
        for bad in [
            &b"profiles: []\n"[..],
            b"default: missing\nprofiles: {}\n",
            b"profiles:\n  work:\n    type: codex\n    created: x\n",
            b"profiles:\n  Work:\n    type: codex\n    created: x\n    last_used: null\n",
            b"profiles:\n  work:\n    type: claude\n    created: x\n    last_used: null\n",
            b"[1, 2]\n",
        ] {
            assert!(
                codex_registry_profiles(bad).is_none(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
