//! Reading the sign-ins of the computer the server runs on from the profile files, without
//! starting the Claude CLI.
//!
//! `claude auth status --json` is a Node process of up to ~100 MiB, started once per profile on
//! every run; on the dashboard's own computer it was the largest helper next to the server. On a
//! Linux host that is this computer, the scan reads the same facts from the files that command
//! reads itself (Claude Code 2.1):
//!
//! | Fact | File and field |
//! | --- | --- |
//! | Signed in through claude.ai | `<config>/.credentials.json` `claudeAiOauth.accessToken` (presence only) with the `user:inference` scope |
//! | When the sign-in ends | `claudeAiOauth.refreshTokenExpiresAt`, kept only with a refresh token (presence only), a plausible time and `expiresAt` no later than three days after it |
//! | Account email | `oauthAccount.emailAddress` in `<config>/.claude.json` (`~/.claude.json` for `~/.claude`) |
//! | Token-only profile | `env.CLAUDE_CODE_OAUTH_TOKEN` in `<config>/settings.json` (presence only) |
//!
//! Token values are never kept: the credential fields are read as present, empty or missing.
//!
//! Anything the files cannot settle goes to the CLI exactly as before ([`Native::Cli`]), for the
//! whole computer: a missing, unreadable, linked or damaged credentials or `.claude.json` file; a
//! legacy `<config>/.config.json`; an API key (`primaryApiKey`); settings that touch sign-in
//! (`apiKeyHelper`, login, gateway or provider keys, sign-in environment names) in the profile's
//! `settings.json` or `remote-settings.json` or in the server folder's project settings; managed
//! settings (`/etc/claude-code`); Anthropic CLI profiles (`~/.config/anthropic`); any
//! `ANTHROPIC_*` or `CLAUDE_*` variable in the server's environment other than the three the CLI
//! scan sets or clears; a token without the inference scope; and a sign-in whose end has passed
//! (the CLI's ended-login rules decide that one).
//!
//! The Mac keeps the CLI because its sign-ins live in the Keychain; Windows keeps it because
//! managed policy can live in the `HKLM` registry key, which this crate cannot read without a
//! new dependency or a child process. Other computers are read over ssh with the CLI.

use crate::config::{Host, Os};
use crate::hostcmd::{Scan, clean_email, safe_claude, safe_dir};
use crate::plan::{Reading, SignIn};
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The earliest sign-in end the CLI accepts (`Date.UTC(2025, 0, 1)`).
const EARLIEST_END_MS: i64 = 1_735_689_600_000;
/// The latest the CLI accepts: ten years (of 365 days) from now.
const LATEST_END_AHEAD_MS: i64 = 315_360_000_000;
/// An access token ending this long after the sign-in hides the sign-in's end.
const TOKEN_PAST_END_MS: f64 = 3.0 * 86_400_000.0;
/// Larger files are not read natively.
const FILE_CAP: u64 = 16 * 1024 * 1024;

/// What this computer looks like to the native read: its folders and the names of its
/// environment variables (never their values).
#[derive(Clone, Debug)]
pub struct LocalRead {
    /// `$HOME`.
    pub home: PathBuf,
    /// The server's working folder: the CLI scan started there and read its project settings.
    pub cwd: Option<PathBuf>,
    /// The managed settings folder (`/etc/claude-code`).
    pub managed_dir: PathBuf,
    /// `$XDG_CONFIG_HOME`, when set.
    pub config_home: Option<PathBuf>,
    /// The names of the server's environment variables.
    pub env_names: Vec<String>,
    /// `$PATH`, where `command -v claude` looked.
    pub path: Option<OsString>,
}

/// The outcome of a native scan of this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Native {
    /// Every profile was read from its files.
    Scan(Scan),
    /// No `claude` here (the CLI scan's `AACNOCLAUDE`).
    NoClaude,
    /// Read this computer with the CLI; the word says why.
    Cli(&'static str),
}

/// One profile read from its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Profile {
    /// The state, email and end `claude auth status --json` reports for these files.
    Read(SignIn, Option<String>, Option<i64>),
    /// The files cannot settle it; the word says why.
    Cli(&'static str),
}

impl LocalRead {
    /// This process's computer, or `None` where the native read does not apply (not Linux, or
    /// no `$HOME`).
    pub fn from_process() -> Option<Self> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
        Some(LocalRead {
            home: PathBuf::from(home),
            cwd: std::env::current_dir().ok(),
            managed_dir: PathBuf::from("/etc/claude-code"),
            config_home: std::env::var_os("XDG_CONFIG_HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from),
            env_names: std::env::vars_os()
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect(),
            path: std::env::var_os("PATH"),
        })
    }

    /// Why this computer as a whole must be read with the CLI, if it must.
    pub fn host_blocker(&self) -> Option<&'static str> {
        if self.env_names.iter().any(|n| server_env_blocks(n)) {
            return Some("auth_environment");
        }
        if exists(&self.managed_dir) {
            return Some("managed_settings");
        }
        let anthropic = [
            Some(self.home.join(".config").join("anthropic")),
            self.config_home.as_ref().map(|c| c.join("anthropic")),
        ];
        if anthropic.iter().flatten().any(|p| exists(p)) {
            return Some("anthropic_profiles");
        }
        if let Some(cwd) = &self.cwd {
            for name in ["settings.json", "settings.local.json"] {
                match settings(&cwd.join(".claude").join(name), false) {
                    Settings::Absent | Settings::Plain => {}
                    Settings::TokenOnly => return Some("project_settings_auth"),
                    Settings::Blocked(why) => return Some(why),
                }
            }
        }
        None
    }

    /// Reads every profile of this Linux computer as the CLI scan would list and read them.
    pub fn scan(&self, host: &Host, ignore: &[String], now: i64) -> Native {
        if host.os != Os::Linux {
            return Native::Cli("not_linux");
        }
        if let Some(why) = self.host_blocker() {
            return Native::Cli(why);
        }
        let Some(home) = self.home.to_str() else {
            return Native::Cli("home_not_text");
        };
        let Some(claude) = self.find_claude(host) else {
            return Native::NoClaude;
        };
        let mut rows: Vec<(String, String, bool)> = vec![];
        let base = self.home.join(".claude-t3");
        if base.is_dir() {
            let Ok(entries) = std::fs::read_dir(&base) else {
                return Native::Cli("profiles_unreadable");
            };
            let mut names: Vec<String> = vec![];
            for entry in entries {
                let Ok(entry) = entry else {
                    return Native::Cli("profiles_unreadable");
                };
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let dir = base.join(&name);
                if !dir.is_dir() || !dir.join(".claude.json").is_file() {
                    continue;
                }
                if ignore
                    .iter()
                    .any(|i| crate::config::safe_name(i) && *i == name)
                {
                    continue;
                }
                names.push(name);
            }
            names.sort();
            for name in names {
                let dir = format!("{home}/.claude-t3/{name}");
                rows.push((name, dir, false));
            }
        }
        if self.home.join(".claude").is_dir() {
            rows.push(("default".into(), format!("{home}/.claude"), true));
        }
        let mut scan = Scan {
            claude: safe_claude(&claude).then_some(claude),
            readings: vec![],
        };
        for (name, dir, default) in rows {
            if !crate::config::safe_name(&name) || !safe_dir(&dir) {
                continue;
            }
            let (state, email, ends_at) = match self.read_profile(Path::new(&dir), default, now) {
                Profile::Read(state, email, ends) => (state, email, ends),
                Profile::Cli(why) => return Native::Cli(why),
            };
            if default && state == SignIn::NotSignedIn {
                continue;
            }
            scan.readings.push(Reading {
                host: host.name.clone(),
                profile: name,
                dir,
                default,
                email,
                state,
                ends_at,
            });
        }
        Native::Scan(scan)
    }

    /// Reads one profile's files (`dir` is its config folder; `default` for `~/.claude`).
    pub fn read_profile(&self, dir: &Path, default: bool, now: i64) -> Profile {
        if exists(&dir.join(".config.json")) {
            return Profile::Cli("legacy_config");
        }
        let global = if default {
            self.home.join(".claude.json")
        } else {
            dir.join(".claude.json")
        };
        let config: GlobalConfig = match read_json(&global) {
            Ok(c) => c,
            Err(Unread::Missing) => return Profile::Cli("config_missing"),
            Err(Unread::Unreadable) => return Profile::Cli("config_unreadable"),
            Err(Unread::Damaged) => return Profile::Cli("config_damaged"),
        };
        if config.primary_api_key.is_some() {
            return Profile::Cli("api_key");
        }
        let user = settings(&dir.join("settings.json"), true);
        let remote = settings(&dir.join("remote-settings.json"), false);
        // Only the profile's own settings may hold a token (`settings(_, false)` blocks one).
        for s in [&user, &remote] {
            if let Settings::Blocked(why) = s {
                return Profile::Cli(why);
            }
        }
        if user == Settings::TokenOnly {
            // The settings' token comes first: `oauth_token`, no end and no email.
            return Profile::Read(SignIn::NoEndDate, None, None);
        }
        let creds_path = dir.join(".credentials.json");
        match std::fs::symlink_metadata(&creds_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Profile::Cli("credentials_link");
            }
            _ => {}
        }
        let creds: Credentials = match read_json(&creds_path) {
            Ok(c) => c,
            Err(Unread::Missing) => return Profile::Cli("credentials_missing"),
            Err(Unread::Unreadable) => return Profile::Cli("credentials_unreadable"),
            Err(Unread::Damaged) => return Profile::Cli("credentials_damaged"),
        };
        let Some(oauth) = creds.oauth else {
            return Profile::Read(SignIn::NotSignedIn, None, None);
        };
        match oauth.access {
            Presence::Absent | Presence::Empty => {
                return Profile::Read(SignIn::NotSignedIn, None, None);
            }
            Presence::Other => return Profile::Cli("credentials_shape"),
            Presence::Set => {}
        }
        let inference = oauth
            .scopes
            .as_ref()
            .and_then(|s| s.as_array())
            .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(INFERENCE)));
        if !inference {
            return Profile::Cli("scopes");
        }
        let email = config
            .oauth_account
            .as_ref()
            .and_then(|a| a.email.as_ref())
            .and_then(serde_json::Value::as_str)
            .and_then(clean_email);
        let ends = match oauth.refresh {
            Presence::Set => login_end(&oauth, now),
            Presence::Absent | Presence::Empty => None,
            Presence::Other => return Profile::Cli("credentials_shape"),
        };
        match ends {
            Some(end) if end <= now => Profile::Cli("sign_in_ended"),
            Some(end) => Profile::Read(SignIn::Ok, email, Some(end)),
            None => Profile::Read(SignIn::NoEndDate, email, None),
        }
    }

    /// The `claude` the CLI scan would have found: the configured path, `command -v claude`,
    /// then the usual install paths.
    pub fn find_claude(&self, host: &Host) -> Option<String> {
        if let Some(configured) = host.claude.as_deref().filter(|c| safe_claude(c)) {
            return Some(configured.to_owned());
        }
        if let Some(path) = &self.path {
            for dir in std::env::split_paths(path) {
                if dir.as_os_str().is_empty() {
                    continue;
                }
                let candidate = dir.join("claude");
                if candidate.is_file() && executable(&candidate) {
                    return candidate.to_str().map(str::to_owned);
                }
            }
        }
        let home = self.home.to_str()?;
        [
            format!("{home}/.local/bin/claude"),
            "/opt/homebrew/bin/claude".to_owned(),
            "/usr/local/bin/claude".to_owned(),
            format!("{home}/.claude/local/claude"),
        ]
        .into_iter()
        .find(|p| executable(Path::new(p)))
    }
}

/// The scope a claude.ai token needs.
const INFERENCE: &str = "user:inference";

/// `refreshTokenExpiresAt` as the CLI reports it (`loginExpiresAt`), or `None`.
fn login_end(oauth: &Oauth, now: i64) -> Option<i64> {
    let end = oauth.ends_at.as_ref()?.as_f64()?;
    if !end.is_finite()
        || end < EARLIEST_END_MS as f64
        || end > now.saturating_add(LATEST_END_AHEAD_MS) as f64
    {
        return None;
    }
    if let Some(token_end) = oauth
        .expires_at
        .as_ref()
        .and_then(serde_json::Value::as_f64)
        && token_end > end + TOKEN_PAST_END_MS
    {
        return None;
    }
    // `new Date(t)` drops the fraction toward zero.
    Some(end.trunc() as i64)
}

/// A server environment name that changes how the CLI signs in.
fn server_env_blocks(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    // The CLI scan sets or clears these three itself.
    if matches!(
        upper.as_str(),
        "CLAUDE_CONFIG_DIR" | "CLAUDE_CODE_OAUTH_TOKEN" | "ANTHROPIC_API_KEY"
    ) {
        return false;
    }
    upper.starts_with("ANTHROPIC_") || upper.starts_with("CLAUDE_")
}

/// A settings `env` name that changes how the CLI signs in.
fn settings_env_blocks(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("ANTHROPIC_")
        || upper.starts_with("CLAUDE_CODE_USE_")
        || upper.starts_with("CLAUDE_CODE_REMOTE")
        || upper.starts_with("CLAUDE_BG_")
        || [
            "TOKEN",
            "API_KEY",
            "APIKEY",
            "OAUTH",
            "AUTH",
            "GATEWAY",
            "SECURESTORAGE",
            "CONFIG_DIR",
            "BEDROCK",
            "VERTEX",
            "FOUNDRY",
            "SUBSCRIPTION",
            "BASE_URL",
            "CREDENTIAL",
        ]
        .iter()
        .any(|w| upper.contains(w))
}

/// A top-level settings key that changes how the CLI signs in.
fn settings_key_blocks(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "apikey",
        "auth",
        "login",
        "token",
        "credential",
        "oauth",
        "gateway",
        "provider",
        "hybrid",
        "organization",
        "helper",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

#[derive(Debug, PartialEq, Eq)]
enum Settings {
    Absent,
    Plain,
    /// Only `env.CLAUDE_CODE_OAUTH_TOKEN` (non-empty) touches sign-in.
    TokenOnly,
    Blocked(&'static str),
}

fn settings(path: &Path, user: bool) -> Settings {
    let map: serde_json::Map<String, serde_json::Value> = match read_json(path) {
        Ok(m) => m,
        Err(Unread::Missing) => return Settings::Absent,
        Err(Unread::Unreadable) => return Settings::Blocked("settings_unreadable"),
        Err(Unread::Damaged) => return Settings::Blocked("settings_damaged"),
    };
    let mut token = false;
    for (key, value) in &map {
        if key == "env" {
            let Some(env) = value.as_object() else {
                return Settings::Blocked("settings_damaged");
            };
            for (name, value) in env {
                if name == "CLAUDE_CODE_OAUTH_TOKEN"
                    && user
                    && value.as_str().is_some_and(|v| !v.is_empty())
                {
                    token = true;
                } else if settings_env_blocks(name) {
                    return Settings::Blocked("settings_auth");
                }
            }
        } else if settings_key_blocks(key) {
            return Settings::Blocked("settings_auth");
        }
    }
    if token {
        Settings::TokenOnly
    } else {
        Settings::Plain
    }
}

enum Unread {
    Missing,
    Unreadable,
    Damaged,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Unread> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Unread::Missing),
        Err(_) => return Err(Unread::Unreadable),
    };
    if !meta.is_file() || meta.len() > FILE_CAP {
        return Err(Unread::Unreadable);
    }
    let bytes = std::fs::read(path).map_err(|_| Unread::Unreadable)?;
    serde_json::from_slice(&bytes).map_err(|_| Unread::Damaged)
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_: &Path) -> bool {
    false
}

/// The fields of `.claude.json` the status command reads.
#[derive(Deserialize)]
struct GlobalConfig {
    #[serde(rename = "oauthAccount", default)]
    oauth_account: Option<Account>,
    #[serde(rename = "primaryApiKey", default)]
    primary_api_key: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Account {
    #[serde(rename = "emailAddress", default)]
    email: Option<serde_json::Value>,
}

/// The fields of `.credentials.json` the status command reads; tokens only as presence.
#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth", default)]
    oauth: Option<Oauth>,
}

#[derive(Deserialize)]
struct Oauth {
    #[serde(rename = "accessToken", default)]
    access: Presence,
    #[serde(rename = "refreshToken", default)]
    refresh: Presence,
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<serde_json::Value>,
    #[serde(rename = "refreshTokenExpiresAt", default)]
    ends_at: Option<serde_json::Value>,
    #[serde(default)]
    scopes: Option<serde_json::Value>,
}

/// Whether a secret field holds something, without keeping it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Presence {
    #[default]
    Absent,
    Empty,
    Set,
    /// Not a string.
    Other,
}

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Presence;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Presence, E> {
                Ok(if v.is_empty() {
                    Presence::Empty
                } else {
                    Presence::Set
                })
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Presence, E> {
                Ok(Presence::Absent)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Presence, E> {
                Ok(Presence::Absent)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Presence, E> {
                Ok(Presence::Other)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Presence, E> {
                Ok(Presence::Other)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Presence, E> {
                Ok(Presence::Other)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Presence, E> {
                Ok(Presence::Other)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Presence, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Presence::Other)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Presence, A::Error> {
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Presence::Other)
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(all(test, unix))]
mod tests;
