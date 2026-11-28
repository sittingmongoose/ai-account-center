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
    ffi::{OsStr, OsString},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod zai;

type Res<T> = Result<T, &'static str>;

/// One private or package file through the shared hardened read (L2): never a link leaf, at
/// most `cap` bytes. `Ok(None)` is a missing file; any other refusal is an error.
fn read_file(path: &Path, cap: u64) -> Result<Option<Vec<u8>>, ()> {
    use aac_files::read::{Read, ReadPolicy, read};
    let mut outcome = Read::Unproved;
    // A file replaced between its check and its open is read again (as the auth reader does).
    for _ in 0..3 {
        outcome = read(path, &ReadPolicy::capped(cap));
        if !matches!(outcome, Read::Unproved) {
            break;
        }
    }
    match outcome {
        Read::Bytes(bytes, _) => Ok(Some(bytes)),
        Read::Absent => Ok(None),
        _ => Err(()),
    }
}

/// `usageHelperProcessTimeoutMs('inventory')`: the claim runs under the inventory request.
const CLAIM_TIMEOUT: Duration = Duration::from_secs(12);
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
    /// The package root (`AAC_AGENT_PACKAGE_ROOT`), for the pinned agent catalog.
    pub package: Option<PathBuf>,
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
    log: Option<Arc<aac_log::Log>>,
}

impl SourceContext {
    pub(crate) fn from_env(ccs: &Path, package_root: Option<PathBuf>) -> Self {
        let package = std::env::var_os("AAC_AGENT_PACKAGE_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or(package_root);
        Self {
            ccs: ccs.to_path_buf(),
            package,
            home: std::env::var_os("CCS_HOME")
                .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }))
                .unwrap_or_default(),
            identity_file: std::env::var("AAC_AGENT_IDENTITY_FILE").ok(),
            known_hosts_file: std::env::var("AAC_AGENT_KNOWN_HOSTS_FILE").ok(),
            hostname: aac_files::process::hostname().unwrap_or_default(),
            path: std::env::var_os("PATH").unwrap_or_default(),
            log: None,
        }
    }
    pub(crate) fn with_log(mut self, log: Arc<aac_log::Log>) -> Self {
        self.log = Some(log);
        self
    }
}

// ---- resolution ------------------------------------------------------------------------------

/// Which Claude credential host platform a target routes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaudePlatform {
    Windows,
    Mac,
}

impl ClaudePlatform {
    fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Mac => "mac",
        }
    }

    fn local_supported(self) -> bool {
        match self {
            Self::Windows => cfg!(windows),
            Self::Mac => cfg!(target_os = "macos"),
        }
    }
}

fn claude_host_id(platform: ClaudePlatform, ssh_host: Option<&str>) -> String {
    format!("{}:{}", platform.as_str(), ssh_host.unwrap_or("local"))
}

/// A resolved reset account.
pub(crate) enum Target {
    Zai {
        key_id: String,
        key_path: PathBuf,
        label: String,
    },
    Codex {
        home: String,
        account: String,
        profile: String,
    },
    Claude {
        platform: ClaudePlatform,
        profile: String,
        email: String,
        profile_dir: Option<String>,
        is_default: bool,
        ssh_host: Option<String>,
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
    if account.starts_with("zai:") {
        return zai::resolve(ctx, account);
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
    match aac_files::login_rollback::read_switch(ccs) {
        Ok(Some(switch)) if switch.codex.is_some() => {
            // The quarantine retires every saved Codex copy while step 3 is on. Until reset
            // reads/writes are rebound to the pinned T3 login, refuse honestly and specifically.
            return Err("step3-reset-unavailable");
        }
        Ok(_) => {}
        Err(_) => return Err(unavailable),
    }
    let registry = read_file(&ccs.join("codex-profiles.yaml"), 4 * 1024 * 1024)
        .map_err(|()| "registry-unavailable")?;
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
    let raw = read_file(auth, 1024 * 1024).ok()??;
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

fn claude_target_from_launcher(
    platform: ClaudePlatform,
    profile_id: Option<&str>,
    email: &str,
    launcher: Launcher,
) -> Option<Target> {
    if launcher.ssh_host.is_none() && !platform.local_supported() {
        return None;
    }
    let is_default = launcher.is_default == Some(true)
        || launcher
            .profile_path
            .as_deref()
            .is_some_and(default_claude_data_folder);
    Some(Target::Claude {
        platform,
        profile: profile_id.unwrap_or("default").to_owned(),
        email: email.to_owned(),
        profile_dir: launcher.profile_path,
        is_default,
        ssh_host: launcher.ssh_host,
    })
}

/// `listClaudeDesktopProfiles` + the adapter's lookup: runnable Claude credential-host targets for
/// `claude:<id ?? email>`, preferring Windows first and falling back to Mac Keychain hosts.
fn claude_targets(ccs: &Path, account: &str) -> Res<Vec<Target>> {
    let invalid = "inventory-invalid";
    let unavailable = "credential-host-unavailable";
    // Over 4 MiB is over the TS limit of 1 Mi UTF-16 units whatever the characters are.
    let text = match read_file(&ccs.join("claude-desktop-profiles.json"), 4 * 1024 * 1024) {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(None) => return Err(unavailable),
        Err(()) => return Err(invalid),
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
    let mut targets = Vec::new();
    if let Some(windows) = profile.windows
        && let Some(target) = claude_target_from_launcher(
            ClaudePlatform::Windows,
            profile.id.as_deref(),
            &profile.email,
            windows,
        )
    {
        targets.push(target);
    }
    if let Some(mac) = profile.mac
        && let Some(target) = claude_target_from_launcher(
            ClaudePlatform::Mac,
            profile.id.as_deref(),
            &profile.email,
            mac,
        )
    {
        targets.push(target);
    }
    if targets.is_empty() {
        return Err(unavailable);
    }
    Ok(targets)
}

fn claude_profile(ccs: &Path, account: &str) -> Res<Target> {
    claude_targets(ccs, account)?
        .into_iter()
        .next()
        .ok_or("credential-host-unavailable")
}

/// Pin a confirmed Claude write to the exact `identity.host` committed in the ledger.
fn resolve_claude_for_write(ccs: &Path, account: &str, write: &Value) -> Res<Target> {
    let targets = claude_targets(ccs, account)?;
    if let Some(expected_host) = write
        .get("identity")
        .and_then(|identity| identity.get("host"))
        .and_then(Value::as_str)
    {
        return targets
            .into_iter()
            .find(|target| match target {
                Target::Claude {
                    platform, ssh_host, ..
                } => claude_host_id(*platform, ssh_host.as_deref()) == expected_host,
                _ => false,
            })
            .ok_or("credential-host-unavailable");
    }
    targets
        .into_iter()
        .next()
        .ok_or("credential-host-unavailable")
}

struct ClaudeProfile {
    id: Option<String>,
    email: String,
    mac: Option<Launcher>,
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
        None => None,
        Some(v) => Some(read_launcher(v, false)?),
    };
    let windows = match value.get("windows") {
        None => None,
        Some(v) => Some(read_launcher(v, true)?),
    };
    if mac.is_none() && windows.is_none() {
        return None;
    }
    Some(ClaudeProfile {
        id,
        email,
        mac,
        windows,
    })
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
    let kind = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };
    let Some(rows) = aac_status::computers::registry().as_object() else {
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
    let mut names = vec!["HOME", "USER", "LOGNAME", "LANG", "SSH_AUTH_SOCK"];
    if cfg!(windows) {
        names.extend(["SYSTEMROOT", "USERPROFILE", "WINDIR"]);
    }
    for name in names {
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
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
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
    #[cfg(not(unix))]
    let identity = format!(
        "{}\0{}\0{:?}\0{:?}",
        file.display(),
        meta.len(),
        meta.created(),
        meta.modified()
    );
    Ok((file, identity))
}

fn probe(file: &Path, ctx: &SourceContext) -> Result<Vec<String>, ()> {
    let mut args = Vec::new();
    for option in CAPABILITY_OPTIONS {
        let mut command = Command::new(file);
        ssh_environment(&mut command, &ctx.path);
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
            let word = option.split('=').next();
            let message = "ssh capability unsupported; compatibility downgrade";
            if let Some(log) = &ctx.log {
                log.write(
                    "rust-service",
                    aac_log::Level::Warn,
                    module_path!(),
                    message,
                    &Map::from_iter([("option".to_owned(), json!(word))]),
                    &aac_log::Extra::default(),
                );
            } else {
                tracing::warn!(option = word, "{message}");
            }
        } else {
            return Err(());
        }
    }
    Ok(args)
}

fn ssh_executable(candidate: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        CString::new(candidate.as_os_str().as_bytes()).is_ok_and(|path| {
            // SAFETY: the path is NUL-terminated and remains alive for the call.
            #[allow(unsafe_code)]
            let ok = unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0;
            ok
        })
    }
    #[cfg(not(unix))]
    candidate.is_file()
}

/// `capabilities()`: the PATH candidate and the capability options probed for its current file.
fn capabilities(ctx: &SourceContext) -> Res<(PathBuf, Vec<String>)> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let executable = std::env::split_paths(&ctx.path)
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            }
            .join(if cfg!(windows) { "ssh.exe" } else { "ssh" })
        })
        .find(|candidate| ssh_executable(candidate))
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
            let result = probe(&file, ctx);
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
fn ssh_launch(ctx: &SourceContext, args: &[String]) -> Res<PathBuf> {
    let (file, current) = capabilities(ctx)?;
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
        let absolute = Path::new(&expanded).is_absolute();
        let expanded = if cfg!(windows) {
            expanded.replace('\\', "/")
        } else {
            expanded
        };
        if !absolute || expanded.contains(['\r', '\n', '%', '"', '\\']) {
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
    let bytes = read_file(&ccs.join("config.yaml"), 4 * 1024 * 1024).ok()??;
    let yaml: serde_yaml::Value = serde_yaml::from_slice(&bytes).ok()?;
    serde_json::to_value(yaml).ok().filter(Value::is_object)
}

/// `sshArgv(alias, command, {interval: 5, countMax: 3})`.
fn ssh_argv(
    ctx: &SourceContext,
    alias: &str,
    command: &str,
    keepalive_count: u8,
) -> Res<Vec<String>> {
    let connection = connection_options(ctx, alias)?;
    let (_, capabilities) = capabilities(ctx)?;
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
        format!("ServerAliveCountMax={keepalive_count}"),
        "--".to_owned(),
        alias.to_owned(),
        command.to_owned(),
    ]);
    Ok(argv)
}

/// The pinned agent artifact a Claude credential host runs (`config/agent-artifacts.json`).
#[derive(Clone, Debug)]
pub(crate) struct ClaudeAgent {
    sha256: String,
    file: PathBuf,
}

/// The catalog entry for `platform`, as the agent transport picks it.
fn claude_agent(ctx: &SourceContext, platform: ClaudePlatform) -> Res<ClaudeAgent> {
    let package = ctx
        .package
        .as_deref()
        .ok_or("credential-host-unavailable")?;
    let catalog = std::env::var_os("AAC_AGENT_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| package.join("config/agent-artifacts.json"));
    let bytes = read_file(&catalog, 65536)
        .ok()
        .flatten()
        .ok_or("credential-host-unavailable")?;
    let catalog: Value =
        serde_json::from_slice(&bytes).map_err(|_| "credential-host-unavailable")?;
    let artifacts = catalog["artifacts"]
        .as_array()
        .filter(|a| a.len() <= 16)
        .ok_or("credential-host-unavailable")?;
    artifacts
        .iter()
        .find(|a| match platform {
            ClaudePlatform::Windows => a["target"] == "x86_64-pc-windows-msvc",
            ClaudePlatform::Mac => {
                a["target"] == "aarch64-apple-darwin" || a["target"] == "x86_64-apple-darwin"
            }
        })
        .and_then(|a| {
            let sha256 = a["sha256"].as_str()?;
            let file = a["file"].as_str()?;
            (sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
            .then(|| ClaudeAgent {
                sha256: sha256.to_owned(),
                file: package.join(file),
            })
        })
        .ok_or("credential-host-unavailable")
}

/// Remote Windows command: the shipped agent, only at its pinned hash, in reset-host mode.
pub(crate) fn windows_agent_command(sha256: &str) -> String {
    use base64::Engine;
    let script = [
        "$ErrorActionPreference = 'Stop'".to_owned(),
        "$ProgressPreference = 'SilentlyContinue'".to_owned(),
        "$f = Join-Path $env:LOCALAPPDATA 'ai-account-center\\bin\\aac-agent.exe'".to_owned(),
        format!("if (!(Test-Path -LiteralPath $f -PathType Leaf) -or (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash.ToLowerInvariant() -ne '{sha256}') {{ exit 87 }}"),
        "& $f --claude-reset".to_owned(),
        "exit $LASTEXITCODE".to_owned(),
    ]
    .join("; ");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        base64::engine::general_purpose::STANDARD.encode(utf16)
    )
}

/// Remote macOS command (any POSIX login shell): the shipped agent at its pinned hash.
pub(crate) fn mac_agent_command(sha256: &str) -> String {
    format!(
        r#"f="$HOME/.cache/ai-account-center/bin/aac-agent"; [ -f "$f" ] && [ ! -L "$f" ] && [ "$(/usr/bin/shasum -a 256 "$f" | cut -d ' ' -f 1)" = '{sha256}' ] || exit 87; exec "$f" --claude-reset"#
    )
}

fn claude_input(target: &Target, write: Option<&Value>) -> Value {
    let Target::Claude {
        platform,
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
        "host": claude_host_id(*platform, ssh_host.as_deref()),
        "write": write.cloned().unwrap_or(Value::Null),
    })
}

/// `privateUsageInvocation` + `sshLaunch`: the exact command the reset transaction spawns.
/// The payload carries only the non-secret request; the host agent reads it on stdin.
fn claude_command(
    ctx: &SourceContext,
    platform: ClaudePlatform,
    ssh_host: Option<&str>,
    agent: &ClaudeAgent,
    input: Value,
) -> Res<HelperCommand> {
    let (executable, args) = if let Some(ssh_host) = ssh_host {
        let remote = match platform {
            ClaudePlatform::Windows => windows_agent_command(&agent.sha256),
            ClaudePlatform::Mac => mac_agent_command(&agent.sha256),
        };
        let args = ssh_argv(ctx, ssh_host, &remote, 3)?;
        let executable = ssh_launch(ctx, &args)?;
        (
            executable.to_str().ok_or("ssh-unavailable")?.to_owned(),
            args,
        )
    } else {
        (
            agent
                .file
                .to_str()
                .ok_or("credential-host-unavailable")?
                .to_owned(),
            vec!["--claude-reset".to_owned()],
        )
    };
    Ok(HelperCommand {
        executable,
        args,
        payload: json!({"request": input}),
    })
}

// ---- operations ------------------------------------------------------------------------------

/// `sourceRead` and `sourceSend` on the claims file at `db`.
#[cfg(any(unix, test))]
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
            send(ctx, db, write)
        }
        _ => Err("shape"),
    }
}

fn read_claude_target(
    ctx: &SourceContext,
    db: &Path,
    account: &str,
    target: &Target,
) -> Res<Value> {
    let Target::Claude {
        platform, ssh_host, ..
    } = target
    else {
        return Err("credential-host-unavailable");
    };
    let agent = claude_agent(ctx, *platform)?;
    let command = claude_command(
        ctx,
        *platform,
        ssh_host.as_deref(),
        &agent,
        claude_input(target, None),
    )?;
    let body = json!({
        "op": "sourceClaudeRead",
        "routeAccount": account,
        "command": {"executable": command.executable, "args": command.args, "payload": command.payload},
    });
    aac_status::reset::service_call(db, body.to_string().as_bytes())
}

fn claude_read_fallback_allowed(err: &str) -> bool {
    matches!(
        err,
        "credential-host-unavailable"
            | "ssh-unavailable"
            | "ssh-policy-refused"
            | "signed-out"
            | "no-matching-code-login"
    )
}

fn more_specific_claude_error(first: &'static str, next: &'static str) -> &'static str {
    let is_login_state = |code: &str| matches!(code, "signed-out" | "no-matching-code-login");
    if is_login_state(first) && !is_login_state(next) {
        first
    } else {
        next
    }
}

/// A ticketed read through the native capture transactions on the claims file.
pub(crate) fn read(ctx: &SourceContext, db: &Path, account: &str) -> Res<Value> {
    if account.starts_with("claude:") {
        let targets = claude_targets(&ctx.ccs, account)?;
        let mut last_err: Option<&'static str> = None;
        for target in targets {
            match read_claude_target(ctx, db, account, &target) {
                Ok(snapshot) => return Ok(snapshot),
                Err(err) => {
                    if !claude_read_fallback_allowed(err) {
                        return Err(err);
                    }
                    last_err = Some(match last_err {
                        None => err,
                        Some(prev) => more_specific_claude_error(prev, err),
                    });
                }
            }
        }
        return Err(last_err.unwrap_or("credential-host-unavailable"));
    }
    let target = resolve(ctx, account)?;
    let body = match &target {
        Target::Zai { .. } => return zai::read(ctx, db, account, &target),
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
        Target::Claude { .. } => return Err("credential-host-unavailable"),
    };
    aac_status::reset::service_call(db, body.to_string().as_bytes())
}

/// The one provider send for an attempt the ledger began. Never retried here.
pub(crate) fn send(ctx: &SourceContext, db: &Path, write: &Value) -> Res<Value> {
    let account = write
        .get("routeAccount")
        .and_then(Value::as_str)
        .ok_or("shape")?;
    if account.starts_with("zai:") {
        return Ok(zai::send(ctx, db, account, write));
    }
    if account.starts_with("claude:") {
        // Everything before the host command is dispatched proves nothing was sent.
        let prepared = (|| -> Res<(Target, ClaudeAgent)> {
            let target = resolve_claude_for_write(&ctx.ccs, account, write)?;
            let Target::Claude { platform, .. } = &target else {
                return Err("credential-host-unavailable");
            };
            let agent = claude_agent(ctx, *platform)?;
            Ok((target, agent))
        })();
        let Ok((target, agent)) = prepared else {
            return Ok(json!({"outcome": "not-sent"}));
        };
        let Target::Claude {
            platform, ssh_host, ..
        } = &target
        else {
            return Ok(json!({"outcome": "not-sent"}));
        };
        let input = claude_input(&target, Some(write));
        // Dispatched from here: a failure may follow a spend, so it is an error (ambiguous).
        let command = claude_command(ctx, *platform, ssh_host.as_deref(), &agent, input.clone())
            .map_err(|_| "host-unconfirmed")?;
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

    #[cfg(target_os = "linux")]
    #[test]
    fn local_computer_follows_the_shared_table() {
        assert_eq!(local_computer("ubuntuserver"), "ubuntu");
        assert_eq!(local_computer("ubuntu-agent-nas1"), "nas1");
        assert_eq!(local_computer("somebox"), "ubuntu");
        // A Windows name on a Linux host is not that computer.
        assert_eq!(local_computer("AlexisGamingPC"), "ubuntu");
    }

    #[test]
    fn unresolved_sources_keep_their_confirmed_send_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let ctx = SourceContext::from_env(root.path(), None);
        let write = |account: &str| json!({"routeAccount": account});
        assert_eq!(
            send(&ctx, root.path(), &write("claude:missing")).unwrap(),
            json!({"outcome":"not-sent"})
        );
        assert_eq!(
            send(&ctx, root.path(), &write("codex:missing")).unwrap_err(),
            "credential-host-unavailable"
        );
        assert_eq!(
            send(&ctx, root.path(), &write("cursor:any")).unwrap_err(),
            "provider-read-only"
        );
    }

    #[test]
    fn the_windows_reset_command_runs_only_the_pinned_agent() {
        use base64::Engine;
        let command = windows_agent_command(&"a".repeat(64));
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
        assert!(script.contains(&format!("-ne '{}') {{ exit 87 }}", "a".repeat(64))));
        assert!(script.contains("& $f --claude-reset"));
        assert!(!script.contains("python"));
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
    #[test]
    fn mac_only_claude_profile_resolves_for_reset_claims() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("claude-desktop-profiles.json"),
            json!({
                "version": 1,
                "profiles": [
                    {
                        "id": "mac-only",
                        "email": "fixture@example.com",
                        "mac": {
                            "launcherName": "Claude (mac-only)",
                            "profilePath": "/Users/fixture/Library/Application Support/Claude-mac-only",
                            "sshHost": "fixture-mac"
                        }
                    },
                    {
                        "id": "mac-default",
                        "email": "default@example.com",
                        "mac": {
                            "launcherName": "Claude",
                            "profilePath": "/Users/fixture/Library/Application Support/Claude",
                            "sshHost": "fixture-mac"
                        }
                    },
                    {
                        "id": "mac-no-ssh",
                        "email": "local@example.com",
                        "mac": {
                            "launcherName": "Claude",
                            "profilePath": "/Users/fixture/Library/Application Support/Claude"
                        }
                    }
                ]
            })
            .to_string(),
        )
        .unwrap();
        let ctx = SourceContext::from_env(root.path(), None);
        let target = resolve(&ctx, "claude:mac-only").expect("mac-only profile must resolve");
        let input = claude_input(&target, None);
        assert_eq!(input["host"], "mac:fixture-mac");
        assert_eq!(input["profile"], "mac-only");
        assert_eq!(input["expectedEmail"], "fixture@example.com");
        assert_eq!(
            input["profileDir"],
            "/Users/fixture/Library/Application Support/Claude-mac-only"
        );
        assert_eq!(input["isDefault"], false);

        let default_target = resolve(&ctx, "claude:mac-default").unwrap();
        assert_eq!(claude_input(&default_target, None)["isDefault"], true);

        if cfg!(target_os = "macos") {
            let local_target = resolve(&ctx, "claude:mac-no-ssh").unwrap();
            assert_eq!(claude_input(&local_target, None)["host"], "mac:local");
        } else {
            assert_eq!(
                resolve(&ctx, "claude:mac-no-ssh").err(),
                Some("credential-host-unavailable")
            );
        }

        let cmd = mac_agent_command(&"b".repeat(64));
        assert!(cmd.contains(&format!("= '{}' ] || exit 87", "b".repeat(64))));
        assert!(cmd.ends_with(r#"exec "$f" --claude-reset"#));
        assert!(!cmd.contains("python"));
    }

    #[cfg(unix)]
    fn write_fake_ssh(bin_dir: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = bin_dir.join("ssh");
        std::fs::write(&path, format!("#!/usr/bin/python3\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn test_context(root: &Path, bin_dir: &Path) -> SourceContext {
        let pkg = root.join("pkg");
        std::fs::create_dir_all(pkg.join("config")).unwrap();
        std::fs::write(
            pkg.join("config/agent-artifacts.json"),
            json!({"artifacts":[
                {"target":"x86_64-pc-windows-msvc","version":"0.1.0","sha256":"a".repeat(64),"file":"dist/agent/x86_64-pc-windows-msvc/aac-agent.exe"},
                {"target":"aarch64-apple-darwin","version":"0.1.0","sha256":"b".repeat(64),"file":"dist/agent/aarch64-apple-darwin/aac-agent"}
            ]})
            .to_string(),
        )
        .unwrap();
        let ssh_dir = root.join(".ssh");
        std::fs::create_dir_all(&ssh_dir).unwrap();
        std::fs::write(ssh_dir.join("known_hosts"), "").unwrap();
        SourceContext {
            ccs: root.to_path_buf(),
            package: Some(pkg),
            home: root.as_os_str().to_os_string(),
            identity_file: None,
            known_hosts_file: Some(ssh_dir.join("known_hosts").to_string_lossy().into_owned()),
            hostname: "ubuntuserver".to_owned(),
            path: bin_dir.as_os_str().to_os_string(),
            log: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn dual_host_read_falls_back_to_mac_and_send_pins_to_confirmed_host() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let log_file = root.path().join("calls.jsonl");
        let win_mode = root.path().join("win-mode");
        std::fs::write(&win_mode, "down").unwrap();

        std::fs::write(
            root.path().join("claude-desktop-profiles.json"),
            json!({
                "version": 1,
                "profiles": [{
                    "id": "dual",
                    "email": "fixture@example.com",
                    "windows": {
                        "launcherName": "Claude (dual)",
                        "profilePath": "C:\\Users\\fixture\\AppData\\Roaming\\Claude-dual",
                        "sshHost": "fixture-win"
                    },
                    "mac": {
                        "launcherName": "Claude (dual)",
                        "profilePath": "/Users/fixture/Library/Application Support/Claude-dual",
                        "sshHost": "fixture-mac"
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();

        let fake_ssh = format!(
            r#"import json, sys
from pathlib import Path
argv = sys.argv[1:]
if '-G' in argv:
    sys.exit(0)
sep = argv.index('--')
alias = argv[sep + 1]
remote_cmd = argv[sep + 2]
payload = json.loads(sys.stdin.buffer.read().decode('utf-8'))
req = payload['request']
with Path({log:?}).open('a', encoding='utf-8') as f:
    f.write(json.dumps({{'alias': alias, 'action': req.get('action'), 'host': req.get('host'), 'keys': sorted(payload), 'remote_cmd': remote_cmd}}) + '\n')
if alias == 'fixture-win':
    mode = Path({win_mode:?}).read_text().strip()
    if mode == 'down':
        sys.exit(255)
    if mode == 'rate-limited':
        if req.get('action') == 'capture':
            print(json.dumps({{'account': '11111111-1111-1111-1111-111111111111', 'organization': '33333333-3333-3333-3333-333333333333', 'savedLoginBinding': 'a' * 64, 'observedAtMs': 1700000000000}}))
        else:
            print(json.dumps({{'unavailable': True, 'reason': 'rate-limited'}}))
        sys.exit(0)
action = req.get('action')
if action == 'capture':
    print(json.dumps({{
        'account': '11111111-1111-1111-1111-111111111111',
        'organization': '33333333-3333-3333-3333-333333333333',
        'savedLoginBinding': 'b' * 64,
        'observedAtMs': 1700000000000,
    }}))
elif action == 'read':
    print(json.dumps({{
        'identity': {{
            'provider': 'claude',
            'account': '11111111-1111-1111-1111-111111111111',
            'organization': '33333333-3333-3333-3333-333333333333',
            'profile': 'dual',
            'host': req['host'],
        }},
        'observedAtMs': 1700000000000,
        'savedLoginBinding': 'b' * 64,
        'raw': {{
            'eligible': True,
            'grants': [{{'id': 'mac_grant', 'resets_left': 1, 'resets_total': 1, 'paused': False, 'usable_now': True, 'clears': ['five_hour']}}]
        }},
        'expiries': [None],
        'windows': {{'five_hour': 90}},
        'sourceBinding': 'c' * 64,
        'accountLabel': 'fixture@example.com',
    }}))
elif action == 'claim':
    print(json.dumps({{'outcome': 'reset'}}))
"#,
            log = log_file.to_string_lossy(),
            win_mode = win_mode.to_string_lossy(),
        );
        write_fake_ssh(&bin, &fake_ssh);

        let ctx = test_context(root.path(), &bin);
        let db = root.path().join("claims.sqlite");

        // 1. Windows unreachable -> read falls back to Mac cleanly.
        let snap = read(&ctx, &db, "claude:dual").expect("read must fall back to mac");
        assert_eq!(snap["identity"]["host"], "mac:fixture-mac");
        let calls: Vec<Value> = std::fs::read_to_string(&log_file)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0]["alias"], "fixture-win");
        assert_eq!(calls[0]["keys"], json!(["request"]));
        assert!(
            calls[0]["remote_cmd"]
                .as_str()
                .unwrap()
                .starts_with("powershell.exe ")
        );
        assert_eq!(calls[1]["alias"], "fixture-mac");
        assert_eq!(calls[1]["action"], "capture");
        assert_eq!(calls[1]["keys"], json!(["request"]));
        assert_eq!(calls[1]["remote_cmd"], mac_agent_command(&"b".repeat(64)));
        assert_eq!(calls[2]["alias"], "fixture-mac");
        assert_eq!(calls[2]["action"], "read");

        // 2. Windows rate-limited -> must NOT silently fall back to Mac.
        std::fs::write(&win_mode, "rate-limited").unwrap();
        std::fs::remove_file(&log_file).unwrap();
        assert_eq!(read(&ctx, &db, "claude:dual").err(), Some("rate-limited"));
        let rl_calls: Vec<Value> = std::fs::read_to_string(&log_file)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(rl_calls.iter().all(|c| c["alias"] == "fixture-win"));

        // 3. send() with identity.host == "mac:fixture-mac" routes ONLY to fixture-mac.
        std::fs::remove_file(&log_file).unwrap();
        let write_mac = json!({
            "routeAccount": "claude:dual",
            "identity": {
                "provider": "claude",
                "account": "11111111-1111-1111-1111-111111111111",
                "organization": "33333333-3333-3333-3333-333333333333",
                "profile": "dual",
                "host": "mac:fixture-mac"
            },
            "sourceBinding": "c".repeat(64),
            "method": "claude.reset_rate_limits",
            "body": {"program": "cedar_ember", "grant_id": "mac_grant", "request_id": "req-1"}
        });
        assert_eq!(
            send(&ctx, &db, &write_mac).unwrap(),
            json!({"outcome": "reset"})
        );
        let send_calls: Vec<Value> = std::fs::read_to_string(&log_file)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(send_calls.len(), 1);
        assert_eq!(send_calls[0]["alias"], "fixture-mac");
        assert_eq!(send_calls[0]["action"], "claim");

        // 4. send() pinned to a missing host refuses as not-sent without spawning ssh.
        std::fs::remove_file(&log_file).unwrap();
        let mut wrong_host = write_mac.clone();
        wrong_host["identity"]["host"] = json!("mac:other-mac");
        assert_eq!(
            send(&ctx, &db, &wrong_host).unwrap(),
            json!({"outcome": "not-sent"})
        );
        assert!(!log_file.exists());
    }
    #[test]
    fn step3_quarantine_refuses_the_retired_saved_copy_specifically() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let ccs = root.path().to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let instance = ccs.join("codex-instances/work");
        std::fs::create_dir_all(&instance).unwrap();
        let account = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"https://api.openai.com/auth.chatgpt_account_id":account}).to_string());
        std::fs::write(
            instance.join("auth.json"),
            json!({"tokens":{"id_token":format!("header.{payload}.signature")}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            ccs.join("codex-profiles.yaml"),
            b"version: '1'\ndefault: work\nprofiles:\n  work:\n    type: codex\n    created: '2026-11-01T00:00:00Z'\n    last_used: null\n",
        )
        .unwrap();
        let places = aac_files::login_rollback::Places {
            home: root.path().to_path_buf(),
            ccs: ccs.clone(),
        };
        let pin = aac_files::login_rollback::CodexPin {
            identity: "a".repeat(64),
            email: "fixture@example.com".into(),
        };
        let switch = aac_files::login_rollback::Switch {
            version: 1,
            since: "2026-11-27T18:00:00Z".into(),
            codex: Some(std::collections::BTreeMap::from([("work".to_owned(), pin)])),
            claude_renewal_host: None,
        };
        aac_files::login_rollback::quarantine(
            &places,
            &[aac_files::login_rollback::Planned {
                kind: aac_files::login_rollback::Kind::CodexSaved,
                profile: "work".into(),
            }],
            Some(&switch),
            "2026-11-27T18:00:00Z",
            "restore",
        )
        .unwrap();
        assert!(
            !instance.join("auth.json").exists(),
            "the saved copy was quarantined"
        );
        let ctx = SourceContext::from_env(root.path(), None);
        assert_eq!(
            resolve(&ctx, "codex:work").err(),
            Some("step3-reset-unavailable")
        );
    }
}
