//! GET /api/accounts/visibility and /api/accounts/preferences from the existing files, read-only.
//! Ports `readAccountVisibility` (with `readPrivateJsonFile`) and `readDashboardPreferences`.
//! Rust answers only a read it can prove equal to the TS reader, side effects included: every
//! case where TS logs (corrupt or migrated preferences), answers an error (an unsafe or invalid
//! visibility file), or could parse what strict JSON refuses returns `None`, and the bridge
//! serves the TS reader. TS remains both files' only writer.
use serde_json::{Map, Value, json};

type Object = Map<String, Value>;
use crate::private_file::{Capped, read_capped};
use std::{fs, os::unix::fs::MetadataExt, path::Path};

const VISIBILITY_FILE: &str = "account-visibility.json";
const MAX_VISIBILITY_BYTES: u64 = 32 * 1024;
const PREFERENCES_FILE: &str = "dashboard-preferences.json";
const EXTRA_SOURCES_FILE: &str = "dashboard-preferences-extra-sources.json";
const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;
pub(crate) const COMPUTERS: &str = include_str!("../../../web-dashboard/public/computers.json");

/// `DASHBOARD_PROVIDER_IDS` (src/web-server/services/dashboard-provider-table.ts).
const PROVIDERS: &[&str] = &[
    "claude",
    "codex",
    "antigravity",
    "cursor",
    "muse",
    "kimi-code",
    "qwen",
    "zai",
    "opencode-go",
];
/// `USAGE_LOG_TOOLS` (dashboard-preferences.ts).
const USAGE_LOG_TOOLS: &[&str] = &["omp", "muse", "zcode", "claude-code", "codex", "jsonl"];
/// `LEGACY_STORAGE_HOSTS` (dashboard-hosts.ts): an immutable compatibility allowlist.
const LEGACY_STORAGE_HOSTS: &[&str] = &["ubuntu", "mac", "windows", "nas1"];
const MAPPING_KEYS: &[&str] = &["timestamp", "model", "inputTokens", "outputTokens", "cost"];

/// IANA zones Rust admits for the saved display zone. `tests/spec/account-read-parity.ts`
/// requires the running TS `isAccountAnalyticsTimeZone` to accept every one; any other zone
/// (valid or not) is decided by the TS reader, whose answer depends on the engine's zone data.
pub(crate) const ADMITTED_TIME_ZONES: &[&str] = &[
    "UTC",
    "Africa/Cairo",
    "Africa/Johannesburg",
    "Africa/Lagos",
    "Africa/Nairobi",
    "America/Anchorage",
    "America/Argentina/Buenos_Aires",
    "America/Bogota",
    "America/Chicago",
    "America/Denver",
    "America/Halifax",
    "America/Los_Angeles",
    "America/Mexico_City",
    "America/New_York",
    "America/Phoenix",
    "America/Sao_Paulo",
    "America/St_Johns",
    "America/Toronto",
    "America/Vancouver",
    "Asia/Bangkok",
    "Asia/Dubai",
    "Asia/Hong_Kong",
    "Asia/Jakarta",
    "Asia/Jerusalem",
    "Asia/Karachi",
    "Asia/Kathmandu",
    "Asia/Manila",
    "Asia/Seoul",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Taipei",
    "Asia/Tehran",
    "Asia/Tokyo",
    "Atlantic/Reykjavik",
    "Australia/Adelaide",
    "Australia/Brisbane",
    "Australia/Melbourne",
    "Australia/Perth",
    "Australia/Sydney",
    "Europe/Amsterdam",
    "Europe/Athens",
    "Europe/Berlin",
    "Europe/Dublin",
    "Europe/Helsinki",
    "Europe/Istanbul",
    "Europe/Lisbon",
    "Europe/London",
    "Europe/Madrid",
    "Europe/Moscow",
    "Europe/Paris",
    "Europe/Rome",
    "Europe/Stockholm",
    "Europe/Warsaw",
    "Europe/Zurich",
    "Pacific/Auckland",
    "Pacific/Honolulu",
];

/// Whether the private-store folder logs its group-writable line, or refuses (`None`).
/// The boot root is canonical, so it is never the symlinked-folder case (the bridge sends a
/// symlinked or other folder to TS before forwarding).
fn store_folder(root: &Path) -> Option<Vec<Value>> {
    let meta = fs::symlink_metadata(root).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.mode() & 0o002 != 0 {
        return None;
    }
    Some(if meta.mode() & 0o020 != 0 {
        vec![json!("group-writable")]
    } else {
        Vec::new()
    })
}

fn record(value: &Value) -> Option<&Object> {
    value.as_object()
}

/// `^[a-z0-9][a-z0-9-]{0,31}(?::[A-Za-z0-9@._+-]{1,128}){1,2}$` or the console wallet id.
fn public_account_id(value: &Value) -> bool {
    let Some(id) = value.as_str() else {
        return false;
    };
    if let Some(hex) = id.strip_prefix("plan-opencode-go-console-mac-")
        && hex.len() == 12
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return true;
    }
    let mut parts = id.split(':');
    let Some(head) = parts.next() else {
        return false;
    };
    let head_ok = !head.is_empty()
        && head.len() <= 32
        && head
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && head
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let rest: Vec<&str> = parts.collect();
    head_ok
        && (1..=2).contains(&rest.len())
        && rest.iter().all(|part| {
            (1..=128).contains(&part.len())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"@._+-".contains(&b))
        })
}

fn unique(values: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// `parseProviderList` of an already-filtered list.
fn provider_list(values: Vec<Value>) -> Option<Vec<Value>> {
    (values.len() <= 32).then(|| unique(values))
}

/// `parseIdList`.
fn id_list(value: &Value) -> Option<Vec<Value>> {
    let values = value.as_array()?;
    (values.len() <= 128 && values.iter().all(public_account_id)).then(|| unique(values.clone()))
}

fn known_providers(value: &Value) -> Option<Vec<Value>> {
    let values = value.as_array()?;
    if !values.iter().all(|v| v.as_str().is_some()) {
        return None;
    }
    Some(
        values
            .iter()
            .filter(|v| v.as_str().is_some_and(|id| PROVIDERS.contains(&id)))
            .cloned()
            .collect(),
    )
}

/// `parseVisibilityFile`; `None` is its null (the file is unavailable).
fn parse_visibility(value: &Value) -> Option<Value> {
    let file = record(value)?;
    if file.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let allowed = [
        "version",
        "hiddenProviders",
        "hiddenAccountIds",
        "trayHiddenProviders",
        "trayHiddenAccountIds",
    ];
    if !file.keys().all(|key| allowed.contains(&key.as_str())) {
        return None;
    }
    let hidden = provider_list(known_providers(file.get("hiddenProviders")?)?)?;
    let tray = match file.get("trayHiddenProviders") {
        Some(list) => provider_list(known_providers(list)?)?,
        None => Vec::new(),
    };
    let ids = id_list(file.get("hiddenAccountIds")?)?;
    let tray_ids = match file.get("trayHiddenAccountIds") {
        Some(list) => id_list(list)?,
        None => Vec::new(),
    };
    Some(json!({
        "hiddenProviders": hidden,
        "hiddenAccountIds": ids,
        "trayHiddenProviders": tray,
        "trayHiddenAccountIds": tray_ids,
    }))
}

/// The visibility reply: `{visibility, folderWarnings}`, or `None` for the TS reader.
pub(crate) fn visibility(root: &Path) -> Option<Value> {
    let warnings = store_folder(root)?;
    // `readPrivateJsonFile`: absent is empty lists; a symlink, a non-file, an oversized file or
    // one that group/other can read is its `invalid` state (TS answers it).
    let lists = match read_capped(&root.join(VISIBILITY_FILE), MAX_VISIBILITY_BYTES)? {
        Capped::Absent => json!({
            "hiddenProviders": [],
            "hiddenAccountIds": [],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": [],
        }),
        // serde_json (preserve_order) keeps JSON.parse's duplicate-key answer: the last value at
        // the first position. Lone surrogates and invalid UTF-8 fail here and use the TS reader.
        Capped::Bytes { bytes, mode } if mode & 0o077 == 0 => {
            parse_visibility(&serde_json::from_slice(&bytes).ok()?)?
        }
        Capped::Symlink | Capped::TooLarge | Capped::Bytes { .. } => return None,
    };
    Some(json!({ "visibility": lists, "folderWarnings": warnings }))
}

/// Every computer's `machineId`, in table order: TS `MACHINE_IDS`
/// (src/web-server/services/dashboard-hosts.ts) reads the same file.
pub(crate) fn machine_ids() -> Option<Vec<String>> {
    machine_ids_in(COMPUTERS)
}

pub(crate) fn machine_ids_in(table: &str) -> Option<Vec<String>> {
    let table: Value = serde_json::from_str(table).ok()?;
    table
        .as_object()?
        .values()
        .map(|computer| Some(computer.as_object()?.get("machineId")?.as_str()?.to_owned()))
        .collect()
}

/// The computers an extra usage-log folder can sit on, and each one's OS.
fn hosts() -> Option<Vec<(String, String)>> {
    let table: Value = serde_json::from_str(COMPUTERS).ok()?;
    table
        .as_object()?
        .iter()
        .map(|(id, computer)| {
            let os = computer.as_object()?.get("os")?.as_str()?;
            Some((id.to_owned(), os.to_owned()))
        })
        .collect()
}

/// `isAccountAnalyticsTimeZone`: `Some(false)` where TS decides without zone data, `None`
/// where only the engine's zone data could decide.
fn time_zone(value: Option<&Value>) -> Option<bool> {
    let Some(Value::String(zone)) = value else {
        return Some(false);
    };
    let segment = |part: &str, first: bool| {
        let mut bytes = part.bytes();
        let lead = bytes.next();
        lead.is_some_and(|b| {
            if first {
                b.is_ascii_alphabetic()
            } else {
                b.is_ascii_alphanumeric() || b"_+-".contains(&b)
            }
        }) && bytes.all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
    };
    let mut parts = zone.split('/');
    let shaped =
        parts.next().is_some_and(|head| segment(head, true)) && parts.all(|p| segment(p, false));
    if !shaped || zone.len() > 64 {
        return Some(false);
    }
    ADMITTED_TIME_ZONES.contains(&zone.as_str()).then_some(true)
}

fn source_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)*$`
fn field_path(path: &str) -> bool {
    path.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// `isUsageLogSource(value, allowRemoteJsonl)`.
fn usage_log_source(value: &Value, allow_remote_jsonl: bool, hosts: &[(String, String)]) -> bool {
    let Some(source) = record(value) else {
        return false;
    };
    let text = |key: &str| source.get(key).and_then(Value::as_str);
    if !allow_remote_jsonl && text("tool") == Some("jsonl") && text("host") != Some("ubuntu") {
        return false;
    }
    let keys = ["id", "tool", "host", "path", "fieldMapping"];
    if !source.keys().all(|key| keys.contains(&key.as_str())) {
        return false;
    }
    let (Some(id), Some(tool), Some(host), Some(path)) =
        (text("id"), text("tool"), text("host"), text("path"))
    else {
        return false;
    };
    let Some(os) = hosts
        .iter()
        .find(|(name, _)| name == host)
        .map(|(_, os)| os)
    else {
        return false;
    };
    let length = aac_views::spread::utf16_len(path);
    let windows_root = os == "windows" && {
        let b = path.as_bytes();
        b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/')
    };
    if !source_id(id)
        || !USAGE_LOG_TOOLS.contains(&tool)
        || length == 0
        || length > 1024
        || path.contains('\0')
        || !(path.starts_with('/') || windows_root)
        || path.contains("..")
    {
        return false;
    }
    // An absent member is undefined; a JSON null is present (and then refused below).
    let Some(mapping) = source.get("fieldMapping") else {
        return tool != "jsonl";
    };
    let Some(mapping) = record(mapping) else {
        return false;
    };
    let names: Vec<&str> = mapping.keys().map(String::as_str).collect();
    if names.is_empty() || names.len() > MAPPING_KEYS.len() {
        return false;
    }
    if mapping.get("timestamp").and_then(Value::as_str).is_none()
        || mapping.get("model").and_then(Value::as_str).is_none()
    {
        return false;
    }
    names.iter().all(|name| {
        MAPPING_KEYS.contains(name)
            && mapping
                .get(*name)
                .and_then(Value::as_str)
                .is_some_and(field_path)
    })
}

/// `isDashboardPreferences`; `None` when the zone needs the engine's zone data.
fn preferences_valid(prefs: &Object, hosts: &[(String, String)]) -> Option<bool> {
    let keys = ["timeZone", "snapshotCleanup", "usageLogSources"];
    if !prefs.keys().all(|key| keys.contains(&key.as_str())) {
        return Some(false);
    }
    if !time_zone(prefs.get("timeZone"))? {
        return Some(false);
    }
    let Some(cleanup) = prefs.get("snapshotCleanup").and_then(Value::as_object) else {
        return Some(false);
    };
    if !cleanup.keys().all(|key| key == "auto")
        || cleanup.get("auto").and_then(Value::as_bool).is_none()
    {
        return Some(false);
    }
    let Some(sources) = prefs.get("usageLogSources").and_then(Value::as_array) else {
        return Some(false);
    };
    if sources.len() > 64 {
        return Some(false);
    }
    let mut ids: Vec<&str> = Vec::new();
    for source in sources {
        if !usage_log_source(source, false, hosts) {
            return Some(false);
        }
        let id = source.as_object()?.get("id")?.as_str()?;
        if ids.contains(&id) {
            return Some(false);
        }
        ids.push(id);
    }
    Some(true)
}

fn defaults() -> Object {
    let mut defaults = Object::new();
    defaults.insert("timeZone".into(), json!("America/New_York"));
    defaults.insert("snapshotCleanup".into(), json!({ "auto": true }));
    defaults.insert("usageLogSources".into(), json!([]));
    defaults
}

/// `readPrimaryPreferences`, except every logging case (corrupt, migrated) is `None`.
fn primary(root: &Path, hosts: &[(String, String)]) -> Option<Object> {
    // Absent or a symlink: the defaults, unlogged. Oversized (TS logs it), a non-file and every
    // unexpected open error: the TS reader.
    let bytes = match read_capped(&root.join(PREFERENCES_FILE), MAX_PREFERENCES_BYTES)? {
        Capped::Absent | Capped::Symlink => return Some(defaults()),
        Capped::TooLarge => return None,
        Capped::Bytes { bytes, .. } => bytes,
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let prefs = record(&value)?;
    if let Some(sources) = prefs.get("usageLogSources").and_then(Value::as_array) {
        let migrated = sources.iter().any(|source| {
            usage_log_source(source, true, hosts)
                && source
                    .as_object()
                    .and_then(|s| s.get("tool"))
                    .and_then(Value::as_str)
                    == Some("jsonl")
                && source
                    .as_object()
                    .and_then(|s| s.get("host"))
                    .and_then(Value::as_str)
                    != Some("ubuntu")
        });
        if migrated {
            return None;
        }
    }
    if !preferences_valid(prefs, hosts)? {
        return None;
    }
    let cleanup = prefs.get("snapshotCleanup")?.clone();
    Some(
        [
            ("timeZone".to_owned(), prefs.get("timeZone")?.clone()),
            ("snapshotCleanup".to_owned(), cleanup),
            (
                "usageLogSources".to_owned(),
                prefs.get("usageLogSources")?.clone(),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

/// The preferences reply, or `None` for the TS reader.
pub(crate) fn preferences(root: &Path) -> Option<Value> {
    let hosts = hosts()?;
    let primary = primary(root, &hosts)?;
    // TS keeps the primary for an absent, symlinked or oversized companion; a non-file or an
    // unexpected open error is left to the TS reader.
    let bytes = match read_capped(&root.join(EXTRA_SOURCES_FILE), MAX_PREFERENCES_BYTES)? {
        Capped::Absent | Capped::Symlink | Capped::TooLarge => {
            return Some(Value::Object(primary));
        }
        Capped::Bytes { bytes, .. } => bytes,
    };
    // Text JSON.parse accepts but serde_json refuses (lone surrogates, invalid UTF-8): TS decides.
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let Some(extra) = record(&value) else {
        return Some(Value::Object(primary));
    };
    if extra.keys().any(|key| key != "usageLogSources") {
        return Some(Value::Object(primary));
    }
    let mut companion = defaults();
    if let Some(sources) = extra.get("usageLogSources") {
        companion.insert("usageLogSources".into(), sources.clone());
    }
    let legacy = companion
        .get("usageLogSources")
        .and_then(Value::as_array)
        .is_some_and(|sources| {
            sources.iter().any(|source| {
                source
                    .as_object()
                    .and_then(|s| s.get("host"))
                    .and_then(Value::as_str)
                    .is_some_and(|host| LEGACY_STORAGE_HOSTS.contains(&host))
            })
        });
    if !preferences_valid(&companion, &hosts)? || legacy {
        return Some(Value::Object(primary));
    }
    let mut sources = primary.get("usageLogSources")?.as_array()?.clone();
    sources.extend(
        companion
            .get("usageLogSources")?
            .as_array()?
            .iter()
            .cloned(),
    );
    let mut merged = primary.clone();
    // Map::insert keeps an existing key's position, like `{...primary, usageLogSources}`.
    merged.insert("usageLogSources".into(), Value::Array(sources));
    Some(Value::Object(if preferences_valid(&merged, &hosts)? {
        merged
    } else {
        primary
    }))
}

/// One account read for the shared read-route dispatch: the two file reads (no body) or the two
/// compositions over their TS input (`None` asks for the logged TS fallback).
pub(crate) fn answer(path: &str, root: &Path, bytes: &[u8]) -> Option<Value> {
    match path {
        "/api/accounts/visibility" if bytes.is_empty() => visibility(root),
        "/api/accounts/preferences" if bytes.is_empty() => preferences(root),
        "/api/accounts/registry" => {
            let input: Value =
                crate::json_object::from_slice(bytes, &["providers", "rows", "jobs", "trash"])
                    .ok()?;
            aac_views::registry::compose(&input).ok()
        }
        "/api/accounts/analytics" => {
            let envelope = crate::json_object::from_slice(bytes, &["data", "presentation"]).ok()?;
            aac_views::analytics::compose(envelope).ok()
        }
        _ => None,
    }
}
