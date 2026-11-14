//! Read-only configuration snapshots. Callers supply the fixture or private root explicitly;
//! this crate never discovers a user's home, changes process environment or writes a file.
use aac_core::{Cause, ComputerId, Failure};
use aac_files::read_bounded;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Retired CCS sections are opaque text; they are never part of the effective model.
pub const RETIRED_SECTIONS: &[&str] = &[
    "cliproxy",
    "proxy",
    "websearch",
    "global_env",
    "continuity",
    "copilot",
    "cursor",
    "cliproxy_server",
    "quota_management",
    "thinking",
    "runtime",
    "channels",
    "browser",
    "image_analysis",
];

/// Legacy settings files and their effective section names and read caps. State, caches,
/// provider credentials and retired switching settings are intentionally not config inputs.
pub const SETTINGS_FILES: &[(&str, &str, usize)] = &[
    (
        "dashboard-preferences.json",
        "dashboard_preferences",
        1024 * 1024,
    ),
    (
        "dashboard-preferences-extra-sources.json",
        "dashboard_preferences_extra_sources",
        1024 * 1024,
    ),
    ("account-refresh-settings.json", "account_refresh", 4096),
    ("account-visibility.json", "account_visibility", 32 * 1024),
    (
        "account-usage-sources.json",
        "account_usage_sources",
        1024 * 1024,
    ),
    (
        "account-usage-accounts.json",
        "account_usage_accounts",
        1024 * 1024,
    ),
    ("agent-usage-settings.json", "agent_usage", 1024 * 1024),
];

/// Environment overrides understood by this read-only foundation. Unlisted keys are ignored;
/// new subsystem-specific settings are added as their owners migrate, rather than guessed.
pub const ENVIRONMENT: &[(&str, &str)] = &[
    ("CCS_DASHBOARD_PORT", "dashboard.port"),
    ("CCS_DASHBOARD_HOST", "dashboard.host"),
    ("CCS_SESSION_SECRET", "dashboard_auth.session_secret"),
    (
        "CCS_DASHBOARD_PASSWORD_HASH",
        "dashboard_auth.password_hash",
    ),
    ("AAC_STORE", "store.enabled"),
    ("AAC_STORE_MODULE", "store.module"),
];

/// Supported host OS vocabulary, including the existing `mac` spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    /// Linux host.
    Linux,
    /// macOS host; `macos` is accepted as a configuration alias.
    #[serde(alias = "macos")]
    Mac,
    /// Windows host.
    Windows,
}

/// Transport configuration without side effects. SSH endpoint validation occurs in transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportConfig {
    /// Spawn the same agent locally for a plain-machine installation.
    Local,
    /// Reach a configured SSH peer with pinned host keys and an explicit identity file.
    Ssh {
        /// Hostname or IP, never a command fragment.
        host: String,
        /// Explicit login user, separate from the host.
        user: Option<String>,
        /// Explicit SSH port, defaulting to 22.
        port: Option<u16>,
        /// Private-key file; transport uses IdentitiesOnly=yes.
        key: PathBuf,
    },
}

/// A configured computer; compatibility fields stay opaque and retain their spellings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComputerConfig {
    /// Stable historical computer id.
    pub id: ComputerId,
    /// Owner-visible label.
    pub label: String,
    /// Host OS.
    pub os: OperatingSystem,
    /// Same protocol through either child process or SSH.
    pub transport: TransportConfig,
    /// Explicit host responsibilities; no server-OS special case is inferred.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Existing machineId, legacy, names and other metadata, kept for compatibility.
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

/// Inputs are injected so tests cannot accidentally discover a real private home.
#[derive(Clone, Debug)]
pub struct LoadOptions {
    /// Explicit home; legacy CCS_HOME appends `.ccs` to this path.
    pub ccs_home: PathBuf,
    /// Injected environment, including CCS_HOME/CCS_DIR if supplied by the caller.
    pub environment: BTreeMap<String, String>,
    /// Package's existing computers.json; never embed owner-specific SSH aliases in Rust.
    pub computers_seed: PathBuf,
    /// Final object overrides corresponding to parsed CLI flags.
    pub cli: Value,
    /// Lowest-precedence defaults.
    pub defaults: Value,
}

impl LoadOptions {
    /// Start with the existing refresh interval and no implicit computer or environment access.
    pub fn new(ccs_home: PathBuf, computers_seed: PathBuf) -> Self {
        Self {
            ccs_home,
            computers_seed,
            environment: BTreeMap::new(),
            cli: json!({}),
            defaults: json!({"account_refresh": {"refreshIntervalSeconds": 60}}),
        }
    }
}

/// An immutable effective snapshot plus the original YAML for a future lossless writer.
/// Debug output is deliberately omitted: settings may contain dashboard authentication data.
#[derive(Clone)]
pub struct Config {
    values: Value,
    computers: Vec<ComputerConfig>,
    original_yaml: String,
    retired: BTreeMap<String, String>,
    private_dir: PathBuf,
}

impl Config {
    /// Load defaults < YAML < legacy setting files < environment < CLI. Reads are bounded;
    /// corrupt present files fail explicitly, and a missing optional file stays absent.
    pub fn load(options: &LoadOptions) -> Result<Self, Failure> {
        let mut private_dir = options
            .environment
            .get("CCS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                options
                    .environment
                    .get("CCS_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| options.ccs_home.clone())
                    .join(".ccs")
            });
        if let Some(dir) = options.cli.get("config_dir").and_then(Value::as_str) {
            private_dir = PathBuf::from(dir);
        }
        if !private_dir.is_absolute() {
            return Err(Failure::new(
                Cause::Refused,
                "configuration root must be absolute",
            ));
        }
        let original_yaml = match optional_read(&private_dir.join("config.yaml"), 4 * 1024 * 1024)?
        {
            Some(bytes) => String::from_utf8(bytes)
                .map_err(|_| Failure::new(Cause::Damaged, "config YAML is not UTF-8"))?,
            None => String::new(),
        };
        let (active_yaml, retired) = separate_retired(&original_yaml);
        let yaml: Value = if active_yaml.trim().is_empty() {
            json!({})
        } else {
            serde_yaml::from_str(&active_yaml)
                .map_err(|_| Failure::new(Cause::Damaged, "invalid active YAML configuration"))?
        };
        let mut values = options.defaults.clone();
        require_object(&values)?;
        merge(&mut values, yaml)?;
        for (file, section, cap) in SETTINGS_FILES {
            if let Some(bytes) = optional_read(&private_dir.join(file), *cap)? {
                let setting: Value = serde_json::from_slice(&bytes)
                    .map_err(|_| Failure::new(Cause::Damaged, "invalid settings JSON"))?;
                require_object(&setting)?;
                values[*section] = setting;
            }
        }
        for (env, key) in ENVIRONMENT {
            if let Some(value) = options.environment.get(*env) {
                let value = match *env {
                    "CCS_DASHBOARD_PORT" => {
                        let port: u16 = value
                            .parse()
                            .map_err(|_| Failure::new(Cause::Refused, "invalid dashboard port"))?;
                        if port == 0 {
                            return Err(Failure::new(Cause::Refused, "invalid dashboard port"));
                        }
                        json!(port)
                    }
                    "AAC_STORE" => json!(value.trim().to_lowercase() != "off"),
                    _ => json!(value),
                };
                set_path(&mut values, key, value)?;
            }
        }
        merge(&mut values, options.cli.clone())?;
        let computer_input = if let Some(explicit) = values.get("computers") {
            explicit.clone()
        } else {
            let seed = optional_read(&private_dir.join("computers.json"), 1024 * 1024)?
                .map(Ok)
                .unwrap_or_else(|| read_bounded(&options.computers_seed, 1024 * 1024))?;
            serde_json::from_slice(&seed)
                .map_err(|_| Failure::new(Cause::Damaged, "invalid computer seed"))?
        };
        let computers = read_computers(computer_input)?;
        for section in RETIRED_SECTIONS {
            if let Some(object) = values.as_object_mut() {
                object.remove(*section);
            }
        }
        Ok(Self {
            values,
            computers,
            original_yaml,
            retired,
            private_dir,
        })
    }

    /// Borrow the effective configuration; callers must never log the whole value.
    pub fn values(&self) -> &Value {
        &self.values
    }
    /// Borrow validated computers, seeded without changing any stored computer id.
    pub fn computers(&self) -> &[ComputerConfig] {
        &self.computers
    }
    /// Return the exact source bytes, including opaque retired sections and comments.
    pub fn original_yaml(&self) -> &str {
        &self.original_yaml
    }
    /// Borrow the retired top-level blocks without parsing or applying their values.
    pub fn retired_sections(&self) -> &BTreeMap<String, String> {
        &self.retired
    }
    /// Return the resolved CCS_DIR/CCS_HOME root used for this snapshot.
    pub fn private_dir(&self) -> &Path {
        &self.private_dir
    }
}

fn optional_read(path: &Path, max: usize) -> Result<Option<Vec<u8>>, Failure> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(_) => read_bounded(path, max).map(Some),
    }
}
fn require_object(value: &Value) -> Result<(), Failure> {
    if value.is_object() {
        Ok(())
    } else {
        Err(Failure::new(
            Cause::Damaged,
            "configuration layer must be an object",
        ))
    }
}
fn merge(base: &mut Value, overlay: Value) -> Result<(), Failure> {
    require_object(&overlay)?;
    merge_value(base, overlay);
    Ok(())
}
fn merge_value(base: &mut Value, overlay: Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            if let Some(previous) = base.get_mut(key) {
                merge_value(previous, value.clone());
            } else {
                base.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overlay;
    }
}
fn set_path(values: &mut Value, path: &str, value: Value) -> Result<(), Failure> {
    let mut slot = values;
    let parts: Vec<_> = path.split('.').collect();
    for key in &parts[..parts.len().saturating_sub(1)] {
        if slot.get(*key).is_none() {
            slot[*key] = json!({});
        }
        if !slot[*key].is_object() {
            return Err(Failure::new(
                Cause::Damaged,
                "override crosses a scalar section",
            ));
        }
        slot = &mut slot[*key];
    }
    if let Some(key) = parts.last() {
        slot[*key] = value;
    }
    Ok(())
}

fn separate_retired(source: &str) -> (String, BTreeMap<String, String>) {
    let mut active = String::new();
    let mut retired = BTreeMap::<String, String>::new();
    let mut section: Option<String> = None;
    for line in source.split_inclusive('\n') {
        if !line.starts_with(char::is_whitespace)
            && !line.starts_with('#')
            && !line.trim().is_empty()
        {
            section = line.split_once(':').and_then(|(key, _)| {
                let key = key.trim().trim_matches(['\'', '"']);
                RETIRED_SECTIONS.contains(&key).then(|| key.to_owned())
            });
        }
        if let Some(key) = &section {
            retired.entry(key.clone()).or_default().push_str(line);
        } else {
            active.push_str(line);
        }
    }
    (active, retired)
}

fn read_computers(input: Value) -> Result<Vec<ComputerConfig>, Failure> {
    let rows = if let Some(rows) = input.as_array() {
        rows.clone()
    } else if let Some(map) = input.as_object() {
        map.iter()
            .map(|(id, fields)| {
                let mut row = fields.clone();
                row["id"] = json!(id);
                if row.get("transport").is_none() {
                    row["transport"] = if let Some(alias) =
                        row.get("sshAlias").and_then(Value::as_str)
                    {
                        // Legacy seeds retain the alias but do not invent an identity key. An explicit
                        // endpoint key is required before transport can use this migrated definition.
                        json!({"kind": "ssh", "host": alias, "user": null, "port": null, "key": ""})
                    } else {
                        json!({"kind": "local"})
                    };
                }
                row
            })
            .collect()
    } else {
        return Err(Failure::new(
            Cause::Damaged,
            "computers must be an object or array",
        ));
    };
    let mut ids = std::collections::BTreeSet::new();
    let mut computers = Vec::new();
    for row in rows {
        let computer: ComputerConfig = serde_json::from_value(row)
            .map_err(|_| Failure::new(Cause::Damaged, "invalid computer definition"))?;
        if computer.label.is_empty()
            || computer.label.len() > 160
            || computer.label.chars().any(char::is_control)
            || !ids.insert(computer.id.clone())
        {
            return Err(Failure::new(
                Cause::Damaged,
                "invalid or duplicate computer id",
            ));
        }
        computers.push(computer);
    }
    Ok(computers)
}
