//! Settings: `<CCS dir>/claude-renew/config.json`, private to the server (0600). Host names, ssh
//! aliases and emails live only there.
//!
//! ```json
//! {
//!   "enabled": true,
//!   "hosts": [
//!     {"name": "mac", "os": "mac", "ssh": "test-mac"},
//!     {"name": "ubuntu", "os": "linux"},
//!     {"name": "windows", "os": "windows", "ssh": "test-windows"}
//!   ],
//!   "browserHost": "mac",
//!   "publishDir": "/mnt/Cursor/aac-usage"
//! }
//! ```
//!
//! A host without `ssh` is this computer. Use the AI Account Center computer names so the
//! dashboard matches the rows. Under the dashboard, the host named as the computer the dashboard
//! runs on is read in place even when it has `ssh` (so one file serves the VM and a dashboard
//! elsewhere, `{"name": "ubuntu", "os": "linux", "ssh": "ubuntu-vm"}`), and a dashboard that runs
//! on no covered computer leaves out every host without `ssh` ([`Here`]).

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A computer's operating system.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// Linux (Ubuntu, Nas1, Odroid, 1070ti).
    Linux,
    /// macOS: logins run in the desktop session for the Keychain.
    Mac,
    /// Windows: logins run through `cmd` under a terminal.
    Windows,
}

/// One covered computer.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    /// The AI Account Center computer name (`mac`, `ubuntu`, `windows`, `nas1` ...).
    pub name: String,
    /// Its operating system.
    pub os: Os,
    /// Its ssh alias or `user@host`; absent for this computer.
    #[serde(default)]
    pub ssh: Option<String>,
    /// The path to `claude` there, when it is not found on the usual paths.
    #[serde(default)]
    pub claude: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_renew() -> f64 {
    24.0
}
fn default_captcha() -> f64 {
    12.0
}
fn default_verify() -> f64 {
    9.0
}
fn default_visit() -> f64 {
    24.0
}
fn default_browser_host() -> String {
    "mac".into()
}
fn default_profiles() -> String {
    "~/.ccs/claude-signin/chrome-plain".into()
}
fn default_agent_desktop() -> String {
    "/opt/homebrew/bin/agent-desktop".into()
}

/// The renewal settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Whether the server runs renewals on its schedule (manual `renew` works either way).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The covered computers.
    pub hosts: Vec<Host>,
    /// The computer with Google Chrome, the accessibility grant and the person (a Mac).
    #[serde(default = "default_browser_host")]
    pub browser_host: String,
    /// Renew a sign-in once its end is this close.
    #[serde(default = "default_renew")]
    pub renew_within_hours: f64,
    /// After a check only a person may pass went unsolved, wait this long before reopening it.
    #[serde(default = "default_captcha")]
    pub captcha_backoff_hours: f64,
    /// Read a renewed sign-in again this long later (an old process may write the old one back).
    #[serde(default = "default_verify")]
    pub verify_after_hours: f64,
    /// Open claude.ai in each account's profile this often so its browser session keeps sliding.
    #[serde(default = "default_visit")]
    pub visit_every_hours: f64,
    /// Where `claude-signins.json` and `.md` go (the usage snapshot folder).
    #[serde(default)]
    pub publish_dir: Option<PathBuf>,
    /// `host/profile` pairs never renewed automatically (still tracked).
    #[serde(default)]
    pub skip: Vec<String>,
    /// Profile folder names under `~/.claude-t3` to leave alone (retired accounts).
    #[serde(default)]
    pub ignore_profiles: Vec<String>,
    /// The plain Chrome profiles' parent folder on the browser computer (one folder per email).
    #[serde(default = "default_profiles")]
    pub profiles_dir: String,
    /// `agent-desktop` on the browser computer.
    #[serde(default = "default_agent_desktop")]
    pub agent_desktop: String,
}

/// Why settings were refused.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The file could not be read.
    Unreadable,
    /// It is not the expected JSON.
    Shape(String),
    /// A value is unsafe to use in a command.
    Unsafe(&'static str),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Unreadable => write!(f, "settings file unreadable"),
            ConfigError::Shape(why) => write!(f, "settings file not valid: {why}"),
            ConfigError::Unsafe(what) => write!(f, "settings value refused: {what}"),
        }
    }
}

/// A name that is safe in paths, keys and shell words.
pub fn safe_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// An email that is safe as a folder name and a shell word.
pub fn safe_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && value.len() <= 200
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-@".contains(&b))
        && !domain.starts_with('.')
        && !value.starts_with('-')
}

fn safe_path_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 400
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/~ :\\".contains(&b))
}

impl Config {
    /// Parses and checks settings text.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let cfg: Config =
            serde_json::from_str(text).map_err(|e| ConfigError::Shape(e.to_string()))?;
        cfg.check()?;
        Ok(cfg)
    }

    /// Reads and checks the settings file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|_| ConfigError::Unreadable)?;
        Config::parse(&text)
    }

    fn check(&self) -> Result<(), ConfigError> {
        if self.hosts.is_empty() {
            return Err(ConfigError::Unsafe("no hosts"));
        }
        let mut seen = std::collections::HashSet::new();
        for host in &self.hosts {
            if !safe_name(&host.name) || !seen.insert(host.name.as_str()) {
                return Err(ConfigError::Unsafe("host name"));
            }
            if let Some(ssh) = &host.ssh
                && (ssh.starts_with('-')
                    || !(1..=200).contains(&ssh.len())
                    || !ssh
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-@:".contains(&b)))
            {
                return Err(ConfigError::Unsafe("ssh alias"));
            }
            if let Some(claude) = &host.claude
                && !safe_path_text(claude)
            {
                return Err(ConfigError::Unsafe("claude path"));
            }
        }
        if self.host(&self.browser_host).is_none() {
            return Err(ConfigError::Unsafe("browserHost is not a configured host"));
        }
        if !safe_path_text(&self.profiles_dir) || !safe_path_text(&self.agent_desktop) {
            return Err(ConfigError::Unsafe("browser computer path"));
        }
        for hours in [
            self.renew_within_hours,
            self.captcha_backoff_hours,
            self.verify_after_hours,
            self.visit_every_hours,
        ] {
            if !(0.0..=24.0 * 30.0).contains(&hours) {
                return Err(ConfigError::Unsafe("hours out of range"));
            }
        }
        if self.ignore_profiles.iter().any(|p| !safe_name(p)) {
            return Err(ConfigError::Unsafe("ignoreProfiles"));
        }
        Ok(())
    }

    /// The host with this name.
    pub fn host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Hours as milliseconds.
    pub fn ms(hours: f64) -> i64 {
        (hours * crate::HOUR_MS as f64) as i64
    }
}

/// Where the server that runs renewal is, as the dashboard decides it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Here {
    /// As the settings say: a host without `ssh` is this computer (the standalone command).
    #[default]
    AsConfigured,
    /// The server runs on this AI Account Center computer: that host is read in place, even
    /// when the settings give it an `ssh` alias.
    Computer(String),
    /// The server runs on no covered computer: a host is reached only over its `ssh` alias.
    Elsewhere,
}

impl Config {
    /// Places the hosts for a server at `here`. Returns the hosts left out: without an `ssh`
    /// alias, they would otherwise be read on a computer that is not theirs.
    pub fn place(&mut self, here: &Here) -> Vec<String> {
        let mut left_out = Vec::new();
        match here {
            Here::AsConfigured => {}
            Here::Computer(name) => {
                for host in &mut self.hosts {
                    if host.name == *name {
                        host.ssh = None;
                    } else if host.ssh.is_none() {
                        left_out.push(host.name.clone());
                    }
                }
            }
            Here::Elsewhere => {
                left_out.extend(
                    self.hosts
                        .iter()
                        .filter(|host| host.ssh.is_none())
                        .map(|host| host.name.clone()),
                );
            }
        }
        self.hosts.retain(|host| !left_out.contains(&host.name));
        left_out
    }
}

/// `<CCS dir>/claude-renew`, where the settings and the state live.
pub fn state_dir(ccs_dir: &Path) -> PathBuf {
    ccs_dir.join("claude-renew")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"hosts":[{"name":"mac","os":"mac","ssh":"test-mac"},{"name":"ubuntu","os":"linux"},
        {"name":"windows","os":"windows","ssh":"test-windows"}],"publishDir":"/tmp/x"}"#;

    #[test]
    fn parses_defaults() {
        let cfg = Config::parse(GOOD).expect("good settings");
        assert!(cfg.enabled);
        assert_eq!(cfg.browser_host, "mac");
        assert_eq!(cfg.renew_within_hours, 24.0);
        assert_eq!(cfg.captcha_backoff_hours, 12.0);
        assert_eq!(cfg.host("ubuntu").and_then(|h| h.ssh.clone()), None);
        assert_eq!(cfg.host("windows").map(|h| h.os), Some(Os::Windows));
    }

    /// One settings file for both: the VM renews itself in place on the VM, and a dashboard
    /// elsewhere reaches the VM over `ubuntu-vm`.
    #[test]
    fn renewal_reaches_the_vm_over_ssh_when_the_dashboard_runs_elsewhere() {
        const VM: &str = r#"{"hosts":[{"name":"mac","os":"mac","ssh":"test-mac"},
            {"name":"ubuntu","os":"linux","ssh":"ubuntu-vm"},{"name":"stray","os":"linux"}],"publishDir":"/tmp/x"}"#;
        let ssh = |cfg: &Config, name: &str| cfg.host(name).and_then(|h| h.ssh.clone());
        let mut on_vm = Config::parse(VM).expect("parse");
        assert_eq!(
            on_vm.place(&Here::Computer("ubuntu".into())),
            vec!["stray".to_owned()]
        );
        assert_eq!(ssh(&on_vm, "ubuntu"), None, "the VM renews itself in place");
        assert_eq!(ssh(&on_vm, "mac").as_deref(), Some("test-mac"));
        let mut elsewhere = Config::parse(VM).expect("parse");
        assert_eq!(elsewhere.place(&Here::Elsewhere), vec!["stray".to_owned()]);
        assert_eq!(ssh(&elsewhere, "ubuntu").as_deref(), Some("ubuntu-vm"));
        assert!(elsewhere.host("stray").is_none());
        let argv = crate::remote::SshRemote::plain().argv(
            elsewhere.host("ubuntu").expect("vm"),
            &crate::remote::Script::Sh("true".into()),
            false,
        );
        assert_eq!(argv[0], "ssh");
        let dashdash = argv.iter().position(|a| a == "--").expect("--");
        assert_eq!(argv[dashdash + 1], "ubuntu-vm");
        let mut standalone = Config::parse(VM).expect("parse");
        assert!(standalone.place(&Here::AsConfigured).is_empty());
        assert_eq!(standalone.hosts.len(), 3);
    }

    #[test]
    fn refuses_unsafe_values() {
        for bad in [
            r#"{"hosts":[{"name":"mac","os":"mac","ssh":"-oProxyCommand=x"}]}"#,
            r#"{"hosts":[{"name":"m;ac","os":"mac"}]}"#,
            r#"{"hosts":[{"name":"mac","os":"mac"},{"name":"mac","os":"linux"}]}"#,
            r#"{"hosts":[{"name":"ubuntu","os":"linux"}]}"#,
            r#"{"hosts":[{"name":"mac","os":"mac","claude":"$(rm -rf /)"}]}"#,
            r#"{"hosts":[]}"#,
        ] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn emails_and_names() {
        assert!(safe_email("owner@example.party"));
        assert!(safe_email("a.b+c@me.com"));
        assert!(!safe_email("a b@me.com"));
        assert!(!safe_email("a@b"));
        assert!(!safe_email("x@y.com;rm"));
        assert!(safe_name("party"));
        assert!(!safe_name("../x"));
    }
}
