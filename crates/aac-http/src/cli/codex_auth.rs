//! `ai-account-center codex-auth ...` after CUT1 = A: create/login/show/list/status/remove are
//! managed in the dashboard's Accounts page and print a pointer (exit 1); import-default is a
//! byte-for-byte port of src/codex-auth/commands/import-default-command.ts (non-destructive
//! migration of the legacy ~/.codex/auth.json, C3 torn-write protection, the same refusals,
//! exit codes and summary). Help, version and the retired switching words match the TS router.

use serde_json::{Map, Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const USAGE: &str = "ai-account-center codex-auth import-default <name> [--with-history] [--force] [--force-while-running]";
const SCHEMA_VERSION: &str = "1.0";
const MAX_READ_RETRIES: usize = 3;
const RETRY_DELAY: Duration = Duration::from_millis(100);
const LOCK_STALE: Duration = Duration::from_secs(10);
const LOCK_RETRIES: usize = 40;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);

pub fn help() -> &'static str {
    "AI Account Center Codex Login Management

Usage
  ai-account-center codex-auth <command> [options]
  ccs codex-auth <command> [options]

Commands
  import-default <name>  Save the existing ~/.codex/auth.json as a named login

Managed in the dashboard's Accounts page (the CLI points there):
  Add (create), Sign in again (login), the saved-login list (show/list/status), Remove

Examples
  ai-account-center codex-auth import-default work

Options
  --force                Overwrite a saved login (import-default)
  --with-history         Include history.jsonl and sessions when importing (default: off)
  --force-while-running  Allow import while Codex is running

Notes
  Saved auth files remain in the existing account configuration directory.
  The dashboard reads each saved login's usage and limits and keeps it signed in.
  AI Account Center no longer switches the shared ~/.codex login (activate, use and
  switch are retired); your agents choose their own accounts.
"
}

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

/// exitWithError: `[X] message` on stderr and the typed exit code.
fn exit_with_error(message: &str, code: i32) -> i32 {
    err(&format!("[X] {message}\n"));
    code
}

fn retired_switching(command: &str) -> i32 {
    err(&format!(
        "[X] codex-auth {command} is retired in AI Account Center.\n"
    ));
    err("    Account switching was removed; AI Account Center only reads usage and limits.\n");
    err("    No CODEX_HOME exports or persistent profile changes were made.\n");
    1
}

fn retired_workflow(command: &str) -> i32 {
    err(&format!(
        "[X] \"{command}\" is an unsupported command or retired upstream CCS workflow.\n"
    ));
    err(
        "    AI Account Center manages accounts and usage; profile/model CLI launches are retired.\n",
    );
    err("    Use ai-account-center dashboard, codex-auth, bar, or --help.\n");
    1
}

fn accounts_pointer(subcommand: &str) -> i32 {
    let affordance = match subcommand {
        "create" => "Add",
        "login" => "Sign in again",
        "remove" => "Remove",
        _ => "the saved-login list",
    };
    err(&format!(
        "[X] \"codex-auth {subcommand}\" is managed in the dashboard's Accounts page now ({affordance}).\n"
    ));
    err("    Open ai-account-center dashboard, then Accounts.\n");
    1
}

pub fn run(args: &[String], ccs: &Path) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        out(help());
        return 0;
    };
    match first.as_str() {
        "help" | "--help" | "-h" => {
            out(help());
            0
        }
        "--version" | "-v" => {
            out(&format!(
                "AI Account Center codex-auth {}\n",
                env!("AAC_VERSION")
            ));
            0
        }
        "use" | "switch" | "activate" => retired_switching(first),
        "create" | "login" | "show" | "list" | "status" | "remove" => accounts_pointer(first),
        "import-default" => import_default(rest, ccs),
        other => retired_workflow(&format!("codex-auth {other}")),
    }
}

// ── profile names ────────────────────────────────────────────────────────────

fn name_error(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Profile name is required.".into());
    }
    if matches!(name, "default" | "current") {
        return Some(format!("Profile name \"{name}\" is reserved."));
    }
    if name.contains('/') || name.contains('\\') {
        return Some("Profile name must not contain path separators.".into());
    }
    if name.len() > 64 {
        return Some("Profile name must be 64 characters or fewer.".into());
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !ok {
        return Some("Profile name must match [a-z0-9][a-z0-9_-]{0,63}.".into());
    }
    None
}

// ── arg parsing (commands/types.ts) ──────────────────────────────────────────

struct ParsedArgs {
    profile_name: Option<String>,
    force: bool,
    unknown_flags: Vec<String>,
    seen_force: bool,
    extra_positionals: Vec<String>,
}

fn parse_args(args: &[String]) -> ParsedArgs {
    let mut parsed = ParsedArgs {
        profile_name: None,
        force: false,
        unknown_flags: Vec::new(),
        seen_force: false,
        extra_positionals: Vec::new(),
    };
    let mut positional: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--yes" | "-y" | "--json" => parsed.unknown_flags.push(arg.clone()),
            "--force" => {
                parsed.seen_force = true;
                parsed.force = true;
            }
            "--shell" => {
                parsed.unknown_flags.push(arg.clone());
                index += 1; // consume the shell value like the TS parser
            }
            _ if arg.starts_with("--shell=") => parsed.unknown_flags.push(arg.clone()),
            _ if arg.starts_with('-') && arg != "--" => parsed.unknown_flags.push(arg.clone()),
            "--" => {}
            _ => positional.push(arg.clone()),
        }
        index += 1;
    }
    if let Some(first) = positional.first() {
        parsed.profile_name = Some(first.clone());
    }
    if positional.len() > 1 {
        parsed.extra_positionals = positional[1..].to_vec();
    }
    parsed
}

/// rejectUnsupportedOptions for import-default: --force, --with-history and
/// --force-while-running are allowed; the other shared flags are not.
fn reject_unsupported(parsed: &ParsedArgs) -> Option<i32> {
    let mut unsupported: Vec<String> = Vec::new();
    for flag in &parsed.unknown_flags {
        if flag == "--with-history" || flag == "--force-while-running" {
            continue;
        }
        if !unsupported.contains(flag) {
            unsupported.push(flag.clone());
        }
    }
    if unsupported.is_empty() && parsed.extra_positionals.is_empty() {
        return None;
    }
    err(&format!("Usage: {USAGE}\n"));
    let mut details: Vec<String> = Vec::new();
    if !unsupported.is_empty() {
        details.push(format!("Unknown options: {}", unsupported.join(", ")));
    }
    if !parsed.extra_positionals.is_empty() {
        details.push(format!(
            "Unexpected arguments: {}",
            parsed
                .extra_positionals
                .iter()
                .map(|arg| format!("\"{arg}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(exit_with_error(&details.join("; "), 1))
}

// ── JWT identity (decode-id-token.ts) ────────────────────────────────────────

fn base64url_decode(segment: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let standard = segment.replace('-', "+").replace('_', "/");
    let padded = format!("{standard}{}", "=".repeat((4 - standard.len() % 4) % 4));
    base64::engine::general_purpose::STANDARD
        .decode(padded)
        .ok()
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() % 4 != 1
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn json_segment(segment: &str) -> Option<Value> {
    let bytes = base64url_decode(segment)?;
    let text = String::from_utf8(bytes).ok()?;
    serde_json::from_str(&text).ok()
}

/// The unverified JWT payload, or None when the token is not a well-formed JWT.
fn decode_jwt_payload(id_token: &str) -> Option<Value> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|part| valid_segment(part)) {
        return None;
    }
    let header = json_segment(parts[0])?;
    if !header.is_object() {
        return None;
    }
    let payload = json_segment(parts[1])?;
    if !payload.is_object() {
        return None;
    }
    Some(payload)
}

#[derive(Default)]
struct Identity {
    email: Option<String>,
    plan_type: Option<String>,
    account_id: Option<String>,
}

fn decode_id_token(id_token: &str) -> Identity {
    let Some(payload) = decode_jwt_payload(id_token) else {
        return Identity::default();
    };
    let auth = payload.get("https://api.openai.com/auth");
    let profile = payload.get("https://api.openai.com/profile");
    let email = payload
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| profile.and_then(|p| p.get("email")).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let claim_string = |claim: Option<&Value>, key: &str| {
        claim
            .and_then(|c| c.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    Identity {
        email,
        plan_type: claim_string(auth, "chatgpt_plan_type"),
        account_id: claim_string(auth, "chatgpt_account_id"),
    }
}

// ── registry (codex-profile-registry.ts) ─────────────────────────────────────

fn registry_path(ccs: &Path) -> PathBuf {
    ccs.join("codex-profiles.yaml")
}

fn empty_registry() -> Value {
    json!({ "version": SCHEMA_VERSION, "default": null, "profiles": {} })
}

fn validate_registry(parsed: &Value) -> Result<Value, String> {
    let Some(root) = parsed.as_object() else {
        return Err("registry YAML root is not an object".into());
    };
    let Some(profiles) = root.get("profiles").and_then(Value::as_object) else {
        return Err("registry YAML is missing an object profiles map".into());
    };
    let default = match root.get("default") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(name)) => {
            if let Some(error) = name_error(name) {
                return Err(format!(
                    "registry YAML contains invalid profile name \"{name}\": {error}"
                ));
            }
            json!(name)
        }
        Some(_) => return Err("registry YAML default must be a string or null".into()),
    };
    let mut validated = Map::new();
    for (name, profile) in profiles {
        if let Some(error) = name_error(name) {
            return Err(format!(
                "registry YAML contains invalid profile name \"{name}\": {error}"
            ));
        }
        let Some(meta) = profile.as_object() else {
            return Err(format!(
                "registry YAML profile \"{name}\" must be an object"
            ));
        };
        if meta.get("type").and_then(Value::as_str) != Some("codex") {
            return Err(format!(
                "registry YAML profile \"{name}\" must have type \"codex\""
            ));
        }
        if !meta.get("created").is_some_and(Value::is_string) {
            return Err(format!(
                "registry YAML profile \"{name}\" must have a string created timestamp"
            ));
        }
        match meta.get("last_used") {
            Some(Value::Null) | Some(Value::String(_)) => {}
            _ => {
                return Err(format!(
                    "registry YAML profile \"{name}\" must have a string or null last_used timestamp"
                ));
            }
        }
        if let Some(email) = meta.get("email")
            && !email.is_string()
        {
            return Err(format!(
                "registry YAML profile \"{name}\" email must be a string"
            ));
        }
        if let Some(plan) = meta.get("plan_type")
            && !plan.is_string()
            && !plan.is_null()
        {
            return Err(format!(
                "registry YAML profile \"{name}\" plan_type must be a string or null"
            ));
        }
        if let Some(account) = meta.get("account_id")
            && !account.is_string()
        {
            return Err(format!(
                "registry YAML profile \"{name}\" account_id must be a string"
            ));
        }
        validated.insert(name.clone(), profile.clone());
    }
    if let Some(default) = default.as_str()
        && !validated.contains_key(default)
    {
        return Err("registry YAML default profile is missing from profiles map".into());
    }
    let version = root
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or(SCHEMA_VERSION);
    Ok(json!({ "version": version, "default": default, "profiles": validated }))
}

fn read_registry(file: &Path) -> Result<Value, String> {
    if !file.exists() {
        return Ok(empty_registry());
    }
    let raw = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let parsed: Value =
        serde_yaml::from_str(&raw).map_err(|_| "registry YAML could not be parsed".to_string())?;
    validate_registry(&parsed)
}

/// proper-lockfile's directory lock: `<dir>.lock` mkdir, stale by mtime after ten seconds.
struct RegistryLock {
    path: PathBuf,
}

fn acquire_registry_lock(dir: &Path) -> Result<RegistryLock, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // proper-lockfile locks the directory with a sibling `<dir>.lock`, not a child.
    let path = dir.with_file_name(format!(
        "{}.lock",
        dir.file_name().and_then(|n| n.to_str()).unwrap_or("dir")
    ));
    for attempt in 0..=LOCK_RETRIES {
        match fs::create_dir(&path) {
            Ok(()) => return Ok(RegistryLock { path }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .map(|mtime| {
                        SystemTime::now()
                            .duration_since(mtime)
                            .map(|age| age > LOCK_STALE)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if stale {
                    let _ = fs::remove_dir(&path);
                    continue;
                }
                if attempt == LOCK_RETRIES {
                    return Err(
                        "Failed to acquire codex profile registry lock: Lock file is already being held"
                            .to_string(),
                    );
                }
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Failed to acquire codex profile registry lock: unknown lock error".into())
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn write_registry(file: &Path, data: &Value) -> Result<(), String> {
    let dir = file.parent().ok_or("registry path has no directory")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = crate::cli::config_write::emit_yaml(data);
    let tmp = file.with_file_name(format!(
        "{}.tmp.{}.{}",
        file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("registry"),
        std::process::id(),
        random_suffix()
    ));
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&tmp).map_err(|e| e.to_string())?;
        handle
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
        drop(handle);
        fs::rename(&tmp, file).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|msg| format!("Failed to write codex profile registry: {msg}"))
}

fn random_suffix() -> String {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Shared random tmp suffix (the bar installer and version pin use the same spelling).
pub(crate) fn random_suffix_public() -> String {
    random_suffix()
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn has_profile(ccs: &Path, name: &str) -> bool {
    if name_error(name).is_some() {
        return false;
    }
    read_registry(&registry_path(ccs))
        .ok()
        .and_then(|data| data.get("profiles").and_then(Value::as_object).cloned())
        .is_some_and(|profiles| profiles.contains_key(name))
}

// ── paths and shared config (codex-profile-paths.ts, codex-config-symlink.ts) ─

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn resolve_profile_dir(ccs: &Path, name: &str) -> Result<PathBuf, String> {
    if let Some(error) = name_error(name) {
        return Err(error);
    }
    let instances = ccs.join("codex-instances");
    let instances = std::path::absolute(&instances).map_err(|e| e.to_string())?;
    let profile = std::path::absolute(instances.join(name)).map_err(|e| e.to_string())?;
    if profile != instances && profile.starts_with(&instances) {
        return Ok(profile);
    }
    Err("Profile directory resolved outside codex-instances.".into())
}

fn ensure_shared_config_symlink(profile_dir: &Path) -> Result<(), String> {
    let target = home_dir().join(".codex").join("config.toml");
    let link = profile_dir.join("config.toml");
    fs::create_dir_all(profile_dir).map_err(|e| e.to_string())?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if !target.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&target).map_err(|e| e.to_string())?;
    }
    let existing = fs::symlink_metadata(&link).ok();
    if let Some(stat) = &existing {
        if stat.file_type().is_symlink() {
            let current = fs::read_link(&link).map_err(|e| e.to_string())?;
            if current == target {
                return Ok(());
            }
            fs::remove_file(&link).map_err(|e| e.to_string())?;
        } else if regular_copy_unmodified(&link, &target) {
            err(&format!(
                "[!] codex-auth: overwriting regular file at {} with symlink to shared config.toml\n",
                link.display()
            ));
            fs::remove_file(&link).map_err(|e| e.to_string())?;
        } else {
            err(&format!(
                "[!] codex-auth: preserving existing regular config.toml at {}; use ai-account-center codex-auth create <name> --force to refresh it.\n",
                link.display()
            ));
            return Ok(());
        }
    }
    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(&target, &link) {
            Ok(()) => Ok(()),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EPERM) | Some(libc::EACCES) | Some(libc::ENOSYS)
                ) =>
            {
                symlink_copy_fallback(&target, &link)
            }
            Err(error) => Err(error.to_string()),
        }
    }
    #[cfg(not(unix))]
    {
        symlink_copy_fallback(&target, &link)
    }
}

fn regular_copy_unmodified(link: &Path, target: &Path) -> bool {
    match (fs::read_to_string(link), fs::read_to_string(target)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn symlink_copy_fallback(target: &Path, link: &Path) -> Result<(), String> {
    let tmp = link.with_file_name(format!(
        ".config.toml.tmp-{}-{}-{}",
        std::process::id(),
        now_ms(),
        random_suffix()
    ));
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&tmp).map_err(|e| e.to_string())?;
        let bytes = fs::read(target).map_err(|e| e.to_string())?;
        handle.write_all(&bytes).map_err(|e| e.to_string())?;
        drop(handle);
        fs::rename(&tmp, link).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
        return result;
    }
    err(&format!(
        "[!] codex-auth: symlink unavailable; copied shared config.toml to {}. Config edits won't propagate automatically.\n",
        link.display()
    ));
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── torn-write-safe source read (C3) ─────────────────────────────────────────

fn read_auth_json_safe(source: &Path) -> Result<Value, String> {
    let mut last = String::from("unknown");
    for attempt in 0..MAX_READ_RETRIES {
        let result = (|| -> Result<Value, String> {
            let raw = fs::read_to_string(source).map_err(|e| e.to_string())?;
            let parsed: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            if let Some(kind) = parsed.get("type").and_then(Value::as_str)
                && matches!(kind, "codex" | "anthropic" | "gemini")
            {
                return Err(format!(
                    "CLIPROXY_FORMAT: Source is a CLIProxy auth file (type=\"{kind}\"); use `ccs cliproxy ...` instead."
                ));
            }
            if let Some(id_token) = parsed
                .get("tokens")
                .and_then(|tokens| tokens.get("id_token"))
                .and_then(Value::as_str)
                && !id_token.is_empty()
                && decode_jwt_payload(id_token).is_none()
            {
                return Err("TORN_JWT: id_token payload is not parseable".into());
            }
            Ok(parsed)
        })();
        match result {
            Ok(value) => return Ok(value),
            Err(message) if message.starts_with("CLIPROXY_FORMAT:") => return Err(message),
            Err(message) => {
                last = message;
                if attempt < MAX_READ_RETRIES - 1 {
                    std::thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
    Err(format!(
        "Failed to read a valid auth.json after {MAX_READ_RETRIES} attempts: {last}"
    ))
}

// ── Codex-running detection (ps table, best-effort) ──────────────────────────

fn executable_basename(token: &str) -> String {
    let trimmed = token.trim_matches(|c| c == '"' || c == '\'');
    let normalized = trimmed.replace('\\', "/");
    Path::new(&normalized)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn is_codex_executable_name(name: &str) -> bool {
    matches!(name, "codex" | "codex.exe" | "codex.cmd" | "codex.ps1")
}

fn first_command_token(command: &str) -> String {
    let trimmed = command.trim();
    if let Some(quoted) = trimmed.strip_prefix('"')
        && let Some(end) = quoted.find('"')
    {
        return quoted[..end].to_string();
    }
    trimmed.split_whitespace().next().unwrap_or("").to_string()
}

fn likely_codex_command(command: &str) -> bool {
    let executable = first_command_token(command);
    if is_codex_executable_name(&executable_basename(&executable)) {
        return true;
    }
    let normalized = command.replace('\\', "/").to_lowercase();
    if normalized.contains("/@openai/codex/") {
        return true;
    }
    command.split_whitespace().any(|token| {
        let name = executable_basename(token);
        name == "codex.js" || is_codex_executable_name(&name)
    })
}

#[allow(unsafe_code)] // libc::getuid reads the caller's uid; no memory is touched.
fn select_codex_pid(stdout: &str) -> Option<String> {
    let own_pid = std::process::id().to_string();
    #[cfg(unix)]
    let current_uid = Some(unsafe { libc::getuid() }.to_string());
    #[cfg(not(unix))]
    let current_uid: Option<String> = None;
    for line in stdout.split(['\r', '\n']) {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(uid)) = (fields.next(), fields.next()) else {
            continue;
        };
        let command = fields.collect::<Vec<_>>().join(" ");
        if command.is_empty() || pid == own_pid {
            continue;
        }
        if !pid.bytes().all(|b| b.is_ascii_digit()) || !uid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Some(current) = &current_uid
            && uid != current
        {
            continue;
        }
        if likely_codex_command(&command) {
            return Some(pid.to_string());
        }
    }
    None
}

fn detect_codex_running() -> Option<String> {
    let mut child = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "uid=", "-o", "command="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let stdout = child.stdout.take()?;
                use std::io::Read;
                let mut text = String::new();
                stdout.take(1024 * 1024).read_to_string(&mut text).ok()?;
                if text.trim().is_empty() {
                    return None;
                }
                return select_codex_pid(&text);
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

// ── atomic copy helpers ──────────────────────────────────────────────────────

fn atomic_write_file(dest: &Path, content: &str) -> Result<(), String> {
    let tmp = dest.with_file_name(format!(
        "{}.tmp.{}.{}",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id(),
        random_suffix()
    ));
    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut handle = options.open(&tmp).map_err(|e| e.to_string())?;
        handle
            .write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
        handle.sync_all().map_err(|e| e.to_string())?;
        drop(handle);
        fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn copy_if_present(src: &Path, dest: &Path) -> Result<bool, String> {
    if !src.exists() {
        return Ok(false);
    }
    let content = fs::read_to_string(src).map_err(|e| e.to_string())?;
    atomic_write_file(dest, &content)?;
    Ok(true)
}

fn copy_dir_if_present(src: &Path, dest: &Path) -> Result<i64, String> {
    if !src.exists() {
        return Ok(-1);
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    let mut count = 0i64;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let stat = fs::symlink_metadata(&from).map_err(|e| e.to_string())?;
        if stat.is_dir() {
            count += copy_dir_if_present(&from, &to)?;
        } else if stat.is_file() {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    Ok(count)
}

// ── the command ──────────────────────────────────────────────────────────────

fn import_default(args: &[String], ccs: &Path) -> i32 {
    clean_orphan_tmp_files(ccs);
    let parsed = parse_args(args);
    if let Some(code) = reject_unsupported(&parsed) {
        return code;
    }
    let Some(name) = parsed.profile_name.clone() else {
        out(&format!("Usage: {USAGE}\n"));
        return exit_with_error("Profile name required", 7);
    };
    if let Some(error) = name_error(&name) {
        return exit_with_error(&error, 7);
    }
    let legacy_home = std::env::var_os("LEGACY_CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex"));
    let legacy_auth = legacy_home.join("auth.json");
    if !legacy_auth.exists() {
        out(&format!(
            "  Use `ai-account-center codex-auth login {name}` to authenticate a new profile instead.\n"
        ));
        return exit_with_error("No legacy auth.json", 7);
    }
    if has_profile(ccs, &name) && !parsed.force {
        out("  Use --force to overwrite (a .bak-<ts> backup will be created).\n");
        return exit_with_error("Profile already exists", 7);
    }
    let force_while_running = args.iter().any(|arg| arg == "--force-while-running");
    if let Some(pid) = detect_codex_running() {
        if !force_while_running {
            err(&format!(
                "[!] Codex appears to be running (PID {pid}). A token refresh may be in flight.\n"
            ));
            err(
                "    Quit Codex first, then re-run import-default. Or pass --force-while-running to proceed anyway.\n",
            );
            return exit_with_error("Codex is running", 7);
        }
        err(
            "[!] Proceeding with Codex running (--force-while-running). Be aware a refresh may race.\n",
        );
    }
    let auth_data = match read_auth_json_safe(&legacy_auth) {
        Ok(value) => value,
        Err(message) if message.starts_with("CLIPROXY_FORMAT:") => {
            return exit_with_error(&message, 7);
        }
        Err(message) => return exit_with_error(&message, 1),
    };
    let profile_dir = match resolve_profile_dir(ccs, &name) {
        Ok(dir) => dir,
        Err(message) => return exit_with_error(&message, 7),
    };
    if let Err(error) = fs::create_dir_all(&profile_dir) {
        return exit_with_error(&error.to_string(), 1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&profile_dir, fs::Permissions::from_mode(0o700)) {
            return exit_with_error(&error.to_string(), 1);
        }
    }
    let dest_auth = profile_dir.join("auth.json");
    if parsed.force && dest_auth.exists() {
        let bak = dest_auth.with_file_name(format!("auth.json.bak-{}", now_ms()));
        if let Err(error) = fs::copy(&dest_auth, &bak) {
            return exit_with_error(&error.to_string(), 1);
        }
        err(&format!(
            "[i] Backed up existing auth.json to {}\n",
            bak.file_name().and_then(|n| n.to_str()).unwrap_or("")
        ));
    }
    let pretty = serde_json::to_string_pretty(&auth_data).unwrap_or_default();
    if let Err(error) = atomic_write_file(&dest_auth, &pretty) {
        return exit_with_error(&error.to_string(), 1);
    }
    let with_history = args.iter().any(|arg| arg == "--with-history");
    let (history_status, sessions_status) = if with_history {
        let history = match copy_if_present(
            &legacy_home.join("history.jsonl"),
            &profile_dir.join("history.jsonl"),
        ) {
            Ok(true) => "copied".to_string(),
            Ok(false) => "not present".to_string(),
            Err(error) => return exit_with_error(&error, 1),
        };
        let sessions =
            match copy_dir_if_present(&legacy_home.join("sessions"), &profile_dir.join("sessions"))
            {
                Ok(count) if count >= 0 => format!("copied {count} files"),
                Ok(_) => "not present".to_string(),
                Err(error) => return exit_with_error(&error, 1),
            };
        (history, sessions)
    } else {
        ("not requested".to_string(), "not requested".to_string())
    };
    if let Err(error) = ensure_shared_config_symlink(&profile_dir) {
        return exit_with_error(
            &format!("Failed to prepare profile config.toml: {error}"),
            2,
        );
    }
    let id_token = auth_data
        .get("tokens")
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str);
    let identity = id_token.map(decode_id_token).unwrap_or_default();
    let email_display = identity.email.as_deref().unwrap_or("(unknown)");

    // Register under the directory lock (read-modify-write, atomic save).
    let registry_file = registry_path(ccs);
    let dir = registry_file.parent().unwrap_or(ccs);
    let _lock = match acquire_registry_lock(dir) {
        Ok(lock) => lock,
        Err(message) => return exit_with_error(&message, 1),
    };
    let mut data = match read_registry(&registry_file) {
        Ok(data) => data,
        Err(message) => {
            return exit_with_error(
                &format!(
                    "Codex profile registry at {} could not be read safely: {message}. Refusing to rewrite it.",
                    registry_display_path(ccs)
                ),
                1,
            );
        }
    };
    let stamp = now_iso();
    let profiles = data
        .get_mut("profiles")
        .and_then(Value::as_object_mut)
        .expect("validated registry");
    if profiles.contains_key(&name) {
        // updateProfile spreads the partial: an absent email/account_id clears the key,
        // plan_type is always written (null when the token carries none).
        let existing = profiles
            .get_mut(&name)
            .and_then(Value::as_object_mut)
            .expect("profile");
        existing.insert("last_used".into(), json!(stamp));
        match &identity.email {
            Some(email) => {
                existing.insert("email".into(), json!(email));
            }
            None => {
                existing.remove("email");
            }
        }
        existing.insert(
            "plan_type".into(),
            identity
                .plan_type
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        match &identity.account_id {
            Some(account) => {
                existing.insert("account_id".into(), json!(account));
            }
            None => {
                existing.remove("account_id");
            }
        }
    } else {
        let mut meta = Map::new();
        meta.insert("type".into(), json!("codex"));
        meta.insert("created".into(), json!(stamp));
        meta.insert("last_used".into(), json!(stamp));
        if let Some(email) = &identity.email {
            meta.insert("email".into(), json!(email));
        }
        meta.insert(
            "plan_type".into(),
            identity
                .plan_type
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        if let Some(account) = &identity.account_id {
            meta.insert("account_id".into(), json!(account));
        }
        profiles.insert(name.clone(), Value::Object(meta));
    }
    if let Err(message) = write_registry(&registry_file, &data) {
        return exit_with_error(&message, 1);
    }
    drop(_lock);

    out(&format!(
        "[OK] Imported legacy {} -> profile '{name}'\n",
        legacy_auth.display()
    ));
    out(&format!("  Email   : {email_display}\n"));
    out(&format!("  History : {history_status}\n"));
    out(&format!("  Sessions: {sessions_status}\n"));
    out(&format!(
        "  Next    : ai-account-center codex-auth activate {name}\n"
    ));
    0
}

/// registryDisplayPath: the source-labelled spelling the TS error messages use.
fn registry_display_path(ccs: &Path) -> String {
    if std::env::var_os("CCS_DIR").is_some_and(|v| !v.is_empty()) {
        return "$CCS_DIR/codex-profiles.yaml".into();
    }
    if std::env::var_os("CCS_HOME").is_some_and(|v| !v.is_empty()) {
        return "$CCS_HOME/.ccs/codex-profiles.yaml".into();
    }
    if ccs == home_dir().join(".ccs") {
        return if cfg!(windows) {
            "%USERPROFILE%\\.ccs\\codex-profiles.yaml".into()
        } else {
            "~/.ccs/codex-profiles.yaml".into()
        };
    }
    "codex-profiles.yaml".into()
}

/// The registry constructor's best-effort orphan tmp cleanup (older than one hour).
fn clean_orphan_tmp_files(ccs: &Path) {
    let Ok(entries) = fs::read_dir(ccs) else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("codex-profiles.yaml.tmp.") {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && meta.modified().map(|m| m < cutoff).unwrap_or(false)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn b64url(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn token(payload: &Value) -> String {
        format!(
            "{}.{}.sig",
            b64url(br#"{"alg":"none"}"#),
            b64url(payload.to_string().as_bytes())
        )
    }

    #[test]
    fn profile_names_follow_the_reference_rules() {
        assert_eq!(name_error("").as_deref(), Some("Profile name is required."));
        assert_eq!(
            name_error("default").as_deref(),
            Some("Profile name \"default\" is reserved.")
        );
        assert_eq!(
            name_error("a/b").as_deref(),
            Some("Profile name must not contain path separators.")
        );
        assert_eq!(
            name_error(&"a".repeat(65)).as_deref(),
            Some("Profile name must be 64 characters or fewer.")
        );
        assert!(name_error("Work").is_some());
        assert!(name_error("-lead").is_some());
        assert!(name_error("work-1_a").is_none());
    }

    #[test]
    fn jwt_identity_reads_the_openai_claims() {
        let payload = json!({
            "email": "top@example.test",
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "plus",
                "chatgpt_account_id": "acct-1"
            }
        });
        let identity = decode_id_token(&token(&payload));
        assert_eq!(identity.email.as_deref(), Some("top@example.test"));
        assert_eq!(identity.plan_type.as_deref(), Some("plus"));
        assert_eq!(identity.account_id.as_deref(), Some("acct-1"));
        let profile_only = json!({"https://api.openai.com/profile": {"email": "p@example.test"}});
        assert_eq!(
            decode_id_token(&token(&profile_only)).email.as_deref(),
            Some("p@example.test")
        );
        assert!(decode_id_token("not-a-jwt").email.is_none());
        assert!(decode_id_token("a.b.c").email.is_none());
        assert!(decode_jwt_payload(&token(&json!({"email":"x"}))).is_some());
        let torn = format!("{}.{}.sig", b64url(br#"{"alg":"none"}"#), "!!!!");
        assert!(decode_jwt_payload(&torn).is_none());
    }

    #[test]
    fn ps_selection_matches_the_reference_tokens() {
        let own = std::process::id();
        let uid = current_uid_for_test();
        let lines = format!(
            "{own} {uid} /usr/bin/codex\n\
             101 {uid} /usr/bin/codex\n\
             102 0 /usr/bin/codex\n\
             103 {uid} node /home/u/.npm/_npx/x/node_modules/@openai/codex/bin/codex.js\n\
             104 {uid} /usr/bin/python3 server.py\n"
        );
        assert_eq!(select_codex_pid(&lines).as_deref(), Some("101"));
        let quoted = format!("201 {uid} \"/opt/Codex CLI/codex\" --flag\n");
        assert_eq!(select_codex_pid(&quoted).as_deref(), Some("201"));
        let none = format!("301 {uid} /usr/bin/vim file\n");
        assert!(select_codex_pid(&none).is_none());
    }

    fn current_uid_for_test() -> u32 {
        #[cfg(unix)]
        {
            #[allow(unsafe_code)]
            unsafe {
                libc::getuid()
            }
        }
        #[cfg(not(unix))]
        {
            0
        }
    }

    #[test]
    fn arg_rejection_matches_the_reference() {
        let parsed = parse_args(&["work".into(), "--wat".into()]);
        assert!(reject_unsupported(&parsed).is_some());
        let parsed = parse_args(&["work".into(), "extra".into()]);
        assert!(reject_unsupported(&parsed).is_some());
        let parsed = parse_args(&["work".into(), "--force".into()]);
        assert!(reject_unsupported(&parsed).is_none());
        assert!(parsed.force);
        let parsed = parse_args(&["--yes".into(), "work".into()]);
        assert!(reject_unsupported(&parsed).is_some());
    }

    #[test]
    fn registry_validation_and_roundtrip() {
        assert!(validate_registry(&json!([])).is_err());
        assert!(validate_registry(&json!({"profiles": {}})).is_ok());
        let bad =
            json!({"profiles": {"work": {"type": "claude", "created": "x", "last_used": null}}});
        assert!(validate_registry(&bad).is_err());
        let good = json!({
            "version": "1.0",
            "default": null,
            "profiles": {"work": {"type": "codex", "created": "2026-01-01T00:00:00.000Z", "last_used": null}}
        });
        let validated = validate_registry(&good).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("codex-profiles.yaml");
        write_registry(&file, &validated).unwrap();
        let read = read_registry(&file).unwrap();
        assert_eq!(read, validated);
    }

    fn legacy_fixture(legacy: &Path, payload: &Value) {
        fs::create_dir_all(legacy).unwrap();
        let auth = json!({"tokens": {"id_token": token(payload), "access_token": "a"} });
        fs::write(
            legacy.join("auth.json"),
            serde_json::to_string_pretty(&auth).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[allow(unsafe_code)] // Test-only env isolation under ENV_LOCK; restored before return.
    fn import_default_end_to_end() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ccs = dir.path().join("ccs");
        let home = dir.path().join("home");
        let legacy = dir.path().join("legacy");
        fs::create_dir_all(&ccs).unwrap();
        fs::create_dir_all(&home).unwrap();
        legacy_fixture(&legacy, &json!({"email": "work@example.test"}));
        fs::write(legacy.join("history.jsonl"), "{\"q\":1}\n").unwrap();
        fs::create_dir_all(legacy.join("sessions/sub")).unwrap();
        fs::write(legacy.join("sessions/a.json"), "{}").unwrap();
        fs::write(legacy.join("sessions/sub/b.json"), "{}").unwrap();
        let old_home = std::env::var_os("HOME");
        let old_legacy = std::env::var_os("LEGACY_CODEX_HOME");
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("LEGACY_CODEX_HOME", &legacy);
        }
        let code = import_default(
            &[
                "work".into(),
                "--with-history".into(),
                "--force-while-running".into(),
            ],
            &ccs,
        );
        assert_eq!(code, 0);
        let profile = ccs.join("codex-instances/work");
        assert!(profile.join("auth.json").is_file());
        assert_eq!(
            fs::read_to_string(profile.join("history.jsonl")).unwrap(),
            "{\"q\":1}\n"
        );
        assert!(profile.join("sessions/sub/b.json").is_file());
        let link = fs::read_link(profile.join("config.toml")).unwrap();
        assert_eq!(link, home.join(".codex/config.toml"));
        let registry = read_registry(&registry_path(&ccs)).unwrap();
        assert_eq!(
            registry["profiles"]["work"]["email"],
            json!("work@example.test")
        );
        assert_eq!(registry["profiles"]["work"]["type"], json!("codex"));
        // A second import without --force refuses; with --force it backs up and updates.
        assert_eq!(
            import_default(&["work".into(), "--force-while-running".into()], &ccs),
            7
        );
        assert_eq!(
            import_default(
                &[
                    "work".into(),
                    "--force".into(),
                    "--force-while-running".into()
                ],
                &ccs
            ),
            0
        );
        let backups: Vec<_> = fs::read_dir(&profile)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("auth.json.bak-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        // CLIProxy format refuses without retry.
        fs::write(legacy.join("auth.json"), r#"{"type":"codex"}"#).unwrap();
        assert_eq!(
            import_default(&["other".into(), "--force-while-running".into()], &ccs),
            7
        );
        // Missing legacy auth refuses with the login hint.
        fs::remove_file(legacy.join("auth.json")).unwrap();
        assert_eq!(
            import_default(&["other".into(), "--force-while-running".into()], &ccs),
            7
        );
        unsafe {
            match old_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match old_legacy {
                Some(v) => std::env::set_var("LEGACY_CODEX_HOME", v),
                None => std::env::remove_var("LEGACY_CODEX_HOME"),
            }
        }
    }
}
