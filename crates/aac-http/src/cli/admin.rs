//! The admin families CUT1 first dropped and G19's Settings pointer no longer covers:
//! `usage-hub` (T3 management key), `usage-snapshot` (agent usage folder) and `cursor-key`
//! (AAC's Cursor account key). Ported from src/commands/usage-hub-command.ts,
//! usage-snapshot-command.ts and cursor-key-command.ts; stdout/stderr and exit codes match.
//!
//! The Cursor key itself is stored by the packaged `aac-cursor-account` reader (a Rust binary),
//! exactly as the TS command hands stdin to it; this module only resolves and runs it.

use base64::Engine;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const PRINT_FLAGS: &[&str] = &["--stdout", "--print-once"];
const HUB_KEY_FILE: &str = "auth/usage-hub-key.json";
const HUB_KEY_MAX: u64 = 4096;
const SNAPSHOT_SETTINGS_FILE: &str = "agent-usage-snapshot.json";
const SNAPSHOT_SETTINGS_MAX: u64 = 64 * 1024;
const SNAPSHOT_JSON: &str = "usage.json";
const MAX_LOCATIONS: usize = 8;
const MAX_WHERE_LENGTH: usize = 60;
const MAX_DIRECTORY_LENGTH: usize = 1024;
const MAX_STDIN: usize = 4096;

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn iso_from_ms(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn iso_parseable(text: &str) -> bool {
    DateTime::parse_from_rfc3339(text).is_ok()
}

/// Node's `path.isAbsolute` on this platform (win32 also accepts a rooted `/x` or `\\x`).
fn absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    if cfg!(windows) {
        value.starts_with('/')
            || value.starts_with('\\')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'/' || bytes[2] == b'\\'))
    } else {
        value.starts_with('/')
    }
}

/// `path.resolve` for the CLI's accepted absolute paths: lexically normalized, no `..`.
fn resolved(value: &str) -> String {
    let path = Path::new(value);
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result.display().to_string()
}

fn no_control(text: &str) -> bool {
    !text.chars().any(|ch| (ch as u32) < 0x20 || ch == '\u{7f}')
}

fn plain_text(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.trim() == text && no_control(text)
}

/// The private JSON reader: a regular, non-symlink file under the size cap.
fn read_private(file: &Path, max: u64) -> Option<Value> {
    let meta = fs::symlink_metadata(file).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > max {
        return None;
    }
    let bytes = fs::read(file).ok()?;
    if bytes.len() as u64 > max {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Atomic 0600 replacement: random temporary name created exclusively, fsync, rename.
fn write_private(file: &Path, value: &Value) -> Result<(), ()> {
    let directory = file.parent().ok_or(())?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|_| ())?;
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|_| ())?;
    let name = file.file_name().and_then(|name| name.to_str()).ok_or(())?;
    let temporary = directory.join(format!(".{name}.{}.tmp", hex(&random)));
    let body = format!("{}\n", serde_json::to_string_pretty(value).map_err(|_| ())?);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> io::Result<()> {
        let mut handle = options.open(&temporary)?;
        handle.write_all(body.as_bytes())?;
        handle.sync_all()?;
        drop(handle);
        fs::rename(&temporary, file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(file, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|_| ())
}

// ---------------------------------------------------------------------------
// usage-hub
// ---------------------------------------------------------------------------

const HUB_HELP: &[&str] = &[
    "T3 usage hub (read-only CLIProxyAPI-compatible usage for T3 Code)",
    "",
    "Usage: ai-account-center dashboard usage-hub <command>",
    "",
    "Commands:",
    "  status              Show whether the hub is on and the URL for T3",
    "  generate --stdout   Create the management key and write it once to stdout",
    "  rotate --stdout     Replace the key; the old key stops working at once",
    "  off                 Remove the key and turn the hub off",
    "",
    "The key is shown only once, on stdout, and only with --stdout (or --print-once).",
    "Pipe it straight into T3; AI Account Center keeps only its SHA-256.",
    "",
    "In T3: Settings > Providers > Usage providers > Add hub, with the dashboard",
    "address and no path, for example http://127.0.0.1:3000 on this computer.",
    "",
];

fn hub_help() -> String {
    HUB_HELP.join("\n")
}

struct HubRecord {
    key_sha256: String,
    created_at: String,
}

enum HubState {
    Off,
    Invalid,
    On(HubRecord),
}

fn hub_key_path(ccs: &Path) -> PathBuf {
    ccs.join(HUB_KEY_FILE)
}

fn parse_hub_record(value: &Value) -> Option<HubRecord> {
    let record = value.as_object()?;
    if record.get("version") != Some(&json!(1)) {
        return None;
    }
    let hash = record.get("keySha256")?.as_str()?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let created = record.get("createdAt")?.as_str()?;
    if !iso_parseable(created) {
        return None;
    }
    Some(HubRecord {
        key_sha256: hash.to_string(),
        created_at: created.to_string(),
    })
}

fn hub_state(ccs: &Path) -> HubState {
    let file = hub_key_path(ccs);
    if !file.exists() {
        return HubState::Off;
    }
    match read_private(&file, HUB_KEY_MAX) {
        Some(value) => match parse_hub_record(&value) {
            Some(record) => HubState::On(record),
            None => HubState::Invalid,
        },
        None => HubState::Invalid,
    }
}

fn new_hub_key() -> String {
    let mut bytes = [0u8; 32];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "aacu_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

fn write_hub_key(ccs: &Path, replace: bool) -> Result<String, bool> {
    if let HubState::On(_) = hub_state(ccs)
        && !replace
    {
        return Err(false);
    }
    let key = new_hub_key();
    let record = json!({
        "version": 1,
        "keySha256": hex(&Sha256::digest(key.as_bytes())),
        "createdAt": iso_now(),
    });
    write_private(&hub_key_path(ccs), &record).map_err(|_| true)?;
    Ok(key)
}

fn remove_hub_key(ccs: &Path) -> Result<bool, ()> {
    match fs::remove_file(hub_key_path(ccs)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(()),
    }
}

/// `dashboard_network` from config.yaml, best effort: unreadable or invalid means off.
fn network_settings(ccs: &Path) -> (bool, Vec<String>) {
    let defaults = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];
    let Ok(text) = fs::read_to_string(ccs.join("config.yaml")) else {
        return (
            false,
            defaults.iter().map(|entry| (*entry).to_string()).collect(),
        );
    };
    let Ok((values, _)) = aac_config::split_active(&text) else {
        return (
            false,
            defaults.iter().map(|entry| (*entry).to_string()).collect(),
        );
    };
    let block = values
        .get("dashboard_network")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let trust = block.get("trust_local_network") == Some(&json!(true));
    let list = block.get("trusted_networks");
    let entries: Vec<String> = match list {
        None => defaults.iter().map(|entry| (*entry).to_string()).collect(),
        Some(value) => value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(canonical_network)
                    .collect()
            })
            .unwrap_or_default(),
    };
    (trust, entries)
}

/// A trusted range as the product accepts it: not wider than /8 (IPv4) or /48 (IPv6), the
/// unique-local fc00::/7 family excepted, never loopback-only; host bits cleared.
fn canonical_network(text: &str) -> Option<String> {
    let network = text.trim().parse::<ipnet::IpNet>().ok()?;
    let prefix = network.prefix_len();
    let network = network.trunc();
    match network {
        ipnet::IpNet::V4(value) => {
            let bytes = value.addr().octets();
            if prefix < 8 || (prefix >= 8 && bytes[0] == 127) {
                return None;
            }
            Some(network.to_string())
        }
        ipnet::IpNet::V6(value) => {
            let bytes = value.addr().octets();
            if prefix >= 7 && (bytes[0] & 0xfe) == 0xfc {
                return Some(network.to_string());
            }
            if prefix < 48 || (prefix == 128 && value.addr().is_loopback()) {
                return None;
            }
            Some(network.to_string())
        }
    }
}

fn private_addresses() -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let Ok(items) = if_addrs::get_if_addrs() else {
        return result;
    };
    for item in items {
        let std::net::IpAddr::V4(address) = item.ip() else {
            continue;
        };
        if address.is_loopback() || address.is_unspecified() {
            continue;
        }
        let octets = address.octets();
        let private = octets[0] == 10
            || (octets[0] == 192 && octets[1] == 168)
            || (octets[0] == 172 && (16..=31).contains(&octets[1]));
        if private && !result.contains(&address.to_string()) {
            result.push(address.to_string());
        }
    }
    result
}

fn hub_status(ccs: &Path) -> i32 {
    match hub_state(ccs) {
        HubState::Off => out(
            "Usage hub: off (no key). Turn it on: ai-account-center dashboard usage-hub generate --stdout\n",
        ),
        HubState::Invalid => out(
            "Usage hub: off. The key file cannot be read safely; replace it: ai-account-center dashboard usage-hub rotate --stdout\n",
        ),
        HubState::On(record) => out(&format!(
            "Usage hub: on (key fingerprint {}, created {})\n",
            &record.key_sha256[..8],
            record.created_at
        )),
    }
    let (trust, networks) = network_settings(ccs);
    out("URL for T3 (no path; T3 adds /v0/management/...):\n");
    out("  this computer:  http://127.0.0.1:<dashboard port>  (3000 for the installed service)\n");
    if trust {
        for address in private_addresses() {
            out(&format!(
                "  other computers: http://{address}:<dashboard port>\n"
            ));
        }
        let listed = if networks.is_empty() {
            "no ranges".to_string()
        } else {
            networks.join(", ")
        };
        out(&format!(
            "LAN access: allowed for peers in {listed} (trusted local network is on).\n"
        ));
    } else {
        out(
            "LAN access: refused (loopback and HTTPS only). Turn on the trusted local network in the dashboard Settings, from this computer, to allow other computers.\n",
        );
    }
    0
}

pub fn usage_hub(args: &[String], ccs: &Path) -> i32 {
    let Some(first) = args.first() else {
        out(&hub_help());
        return 0;
    };
    if matches!(first.as_str(), "help" | "--help" | "-h") {
        out(&hub_help());
        return 0;
    }
    if !matches!(first.as_str(), "status" | "generate" | "rotate" | "off") {
        err(&format!(
            "[X] Unknown usage-hub command: {first}\n{}",
            hub_help()
        ));
        return 1;
    }
    let command = first.as_str();
    let allowed: &[&str] = if matches!(command, "generate" | "rotate") {
        PRINT_FLAGS
    } else {
        &[]
    };
    let unexpected: Vec<&str> = args[1..]
        .iter()
        .map(String::as_str)
        .filter(|arg| !allowed.contains(arg))
        .collect();
    if !unexpected.is_empty() {
        err(&format!(
            "[X] Unexpected arguments for usage-hub {command}: {}\n",
            unexpected.join(" ")
        ));
        return 1;
    }
    if command == "status" {
        return hub_status(ccs);
    }
    if command == "off" {
        return match remove_hub_key(ccs) {
            Ok(true) => {
                err("[OK] Usage hub turned off. Its key no longer works.\n");
                0
            }
            Ok(false) => {
                err("[i] The usage hub was already off.\n");
                0
            }
            Err(()) => {
                err("[X] The usage hub key could not be removed.\n");
                1
            }
        };
    }
    if !args[1..]
        .iter()
        .any(|arg| PRINT_FLAGS.contains(&arg.as_str()))
    {
        err(&format!(
            "[X] The key is shown only once. Run: ai-account-center dashboard usage-hub {command} --stdout\n"
        ));
        return 1;
    }
    match write_hub_key(ccs, command == "rotate") {
        Ok(key) => {
            out(&format!("{key}\n"));
            err(if command == "rotate" {
                "[OK] Usage hub key replaced. The old key no longer works; update T3 with the new one.\n"
            } else {
                "[OK] Usage hub key created. It was written to stdout once and is not stored.\n"
            });
            0
        }
        Err(false) => {
            err(
                "[X] A usage hub key already exists. Replace it with: ai-account-center dashboard usage-hub rotate --stdout\n",
            );
            1
        }
        Err(true) => {
            err("[X] The usage hub key could not be saved safely.\n");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// usage-snapshot
// ---------------------------------------------------------------------------

const SNAPSHOT_HELP: &[&str] = &[
    "Agent usage snapshot (account usage for agents, in a shared folder)",
    "",
    "Usage: ai-account-center dashboard usage-snapshot <command>",
    "",
    "Commands:",
    "  status          Show the folder, its locations and how old its snapshot is",
    "  set <folder> [--location <where>=<path>]...",
    "                  Publish usage.json, usage.md and history.json into <folder>",
    "  off             Stop publishing",
    "",
    "The dashboard rewrites the files after every refresh (every minute by default)",
    "from its own readings. Agents read them with the aac-usage command in that folder.",
    "Each --location names where a group of computers finds the folder, such as",
    "--location \"the Mac=/Volumes/share/aac-usage\"; aac-usage rules lists them.",
    "Without --location, set keeps the locations already saved.",
    "",
];

const SNAPSHOT_SET_USAGE: &str = "[X] Usage: ai-account-center dashboard usage-snapshot set <folder> [--location <where>=<path>]...\n";

fn snapshot_help() -> String {
    SNAPSHOT_HELP.join("\n")
}

#[derive(Clone)]
struct Location {
    where_: String,
    path: String,
}

struct SnapshotSettings {
    directory: String,
    locations: Vec<Location>,
}

fn snapshot_settings_path(ccs: &Path) -> PathBuf {
    ccs.join(SNAPSHOT_SETTINGS_FILE)
}

fn valid_directory(value: &str) -> bool {
    value.len() > 1
        && value.len() <= MAX_DIRECTORY_LENGTH
        && no_control(value)
        && absolute(value)
        && !value.split(['\\', '/']).any(|part| part == "..")
}

fn parse_locations(value: Option<&Value>) -> Vec<Location> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    if items.len() > MAX_LOCATIONS {
        return Vec::new();
    }
    items
        .iter()
        .filter_map(|item| {
            let entry = item.as_object()?;
            let where_ = entry.get("where")?.as_str()?;
            let path = entry.get("path")?.as_str()?;
            (plain_text(where_, MAX_WHERE_LENGTH) && plain_text(path, MAX_DIRECTORY_LENGTH)).then(
                || Location {
                    where_: where_.to_string(),
                    path: path.to_string(),
                },
            )
        })
        .collect()
}

fn read_snapshot_settings(ccs: &Path) -> Option<SnapshotSettings> {
    let value = read_private(&snapshot_settings_path(ccs), SNAPSHOT_SETTINGS_MAX)?;
    let record = value.as_object()?;
    if record.get("version") != Some(&json!(1)) {
        return None;
    }
    let directory = record.get("directory")?.as_str()?;
    if !valid_directory(directory) {
        return None;
    }
    Some(SnapshotSettings {
        directory: directory.to_string(),
        locations: parse_locations(record.get("locations")),
    })
}

fn write_snapshot_settings(ccs: &Path, value: Option<&SnapshotSettings>) -> Result<(), ()> {
    let file = snapshot_settings_path(ccs);
    let Some(settings) = value else {
        return match fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(()),
        };
    };
    let mut record = Map::new();
    record.insert("version".into(), json!(1));
    record.insert("directory".into(), json!(settings.directory));
    if !settings.locations.is_empty() {
        record.insert(
            "locations".into(),
            Value::Array(
                settings
                    .locations
                    .iter()
                    .map(|entry| json!({"where": entry.where_, "path": entry.path}))
                    .collect(),
            ),
        );
    }
    let value = Value::Object(record);
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&value).map_err(|_| ())?
    );
    if body.len() > SNAPSHOT_SETTINGS_MAX as usize {
        return Err(());
    }
    write_private(&file, &value)
}

fn span(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "now".into();
    }
    let days = (seconds / 86_400.0).floor() as i64;
    let hours = ((seconds % 86_400.0) / 3600.0).floor() as i64;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as i64;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{}s", seconds.floor() as i64)
    }
}

fn snapshot_status(ccs: &Path) -> i32 {
    let Some(settings) = read_snapshot_settings(ccs) else {
        out(
            "Usage snapshot: off. Turn it on: ai-account-center dashboard usage-snapshot set <folder>\n",
        );
        return 0;
    };
    out(&format!(
        "Usage snapshot: on, folder {}\n",
        settings.directory
    ));
    for location in &settings.locations {
        out(&format!("  {}: {}\n", location.where_, location.path));
    }
    match fs::metadata(Path::new(&settings.directory).join(SNAPSHOT_JSON)) {
        Ok(meta) => {
            let modified = meta.modified().ok();
            let ms = modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as i64)
                .unwrap_or(0);
            let age = SystemTime::now()
                .duration_since(modified.unwrap_or(UNIX_EPOCH))
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0);
            out(&format!(
                "Last written {} ({} ago).\n",
                iso_from_ms(ms),
                span(age)
            ));
        }
        Err(_) => {
            out("No snapshot written there yet; the dashboard writes one after its next refresh.\n")
        }
    }
    0
}

fn parse_set_args(args: &[String]) -> Option<(String, Option<Vec<String>>)> {
    let mut folder: Option<String> = None;
    let mut locations: Option<Vec<String>> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let value = if arg == "--location" {
            index += 1;
            args.get(index).cloned()
        } else if let Some(rest) = arg.strip_prefix("--location=") {
            Some(rest.to_string())
        } else if arg.starts_with("--") || folder.is_some() {
            return None;
        } else {
            folder = Some(arg.clone());
            index += 1;
            continue;
        };
        locations.get_or_insert_with(Vec::new).push(value?);
        index += 1;
    }
    Some((folder?, locations))
}

fn parse_location(value: &str) -> Option<Location> {
    let split = value.find('=')?;
    if split == 0 {
        return None;
    }
    let where_ = value[..split].trim();
    let path = value[split + 1..].trim();
    (plain_text(where_, MAX_WHERE_LENGTH) && plain_text(path, MAX_DIRECTORY_LENGTH)).then(|| {
        Location {
            where_: where_.to_string(),
            path: path.to_string(),
        }
    })
}

fn snapshot_set(args: &[String], ccs: &Path) -> i32 {
    let Some((folder, locations)) = parse_set_args(args) else {
        err(SNAPSHOT_SET_USAGE);
        return 1;
    };
    let resolved = resolved(&folder);
    if !valid_directory(&folder) {
        err("[X] Give an absolute folder path without \"..\".\n");
        return 1;
    }
    let locations = match locations {
        Some(values) => {
            let parsed: Option<Vec<Location>> =
                values.iter().map(|value| parse_location(value)).collect();
            match parsed {
                Some(list) if list.len() <= MAX_LOCATIONS => list,
                _ => {
                    err(
                        "[X] Each --location is <where>=<path>, such as \"the Mac=/Volumes/share/aac-usage\" (at most 8).\n",
                    );
                    return 1;
                }
            }
        }
        None => read_snapshot_settings(ccs)
            .map(|settings| settings.locations)
            .unwrap_or_default(),
    };
    let target = Path::new(&resolved);
    match fs::metadata(target) {
        Ok(meta) if !meta.is_dir() => {
            err(&format!("[X] {resolved} is not a folder.\n"));
            return 1;
        }
        Ok(_) => {}
        Err(_) => {
            let created = {
                #[cfg_attr(not(unix), allow(unused_mut))]
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o755);
                }
                builder.create(target)
            };
            if created.is_err() {
                err(&format!(
                    "[X] {resolved} could not be used. Its parent folder must exist and be writable.\n"
                ));
                return 1;
            }
        }
    }
    let settings = SnapshotSettings {
        directory: resolved.clone(),
        locations: locations.clone(),
    };
    if write_snapshot_settings(ccs, Some(&settings)).is_err() {
        err(&format!(
            "[X] {resolved} could not be used. Its parent folder must exist and be writable.\n"
        ));
        return 1;
    }
    err(&format!(
        "[OK] Usage snapshot on: the dashboard writes into {resolved} after its next refresh.\n"
    ));
    if !locations.is_empty() {
        let listed: Vec<String> = locations
            .iter()
            .map(|entry| format!("{} {}", entry.where_, entry.path))
            .collect();
        err(&format!("[i] Locations: {}\n", listed.join("; ")));
    }
    0
}

pub fn usage_snapshot(args: &[String], ccs: &Path) -> i32 {
    let Some(first) = args.first() else {
        out(&snapshot_help());
        return 0;
    };
    if matches!(first.as_str(), "help" | "--help" | "-h") {
        out(&snapshot_help());
        return 0;
    }
    if !matches!(first.as_str(), "status" | "set" | "off") {
        err(&format!(
            "[X] Unknown usage-snapshot command: {first}\n{}",
            snapshot_help()
        ));
        return 1;
    }
    if first == "set" {
        return snapshot_set(&args[1..], ccs);
    }
    if args.len() > 1 {
        err(&format!(
            "[X] Unexpected arguments for usage-snapshot {first}: {}\n",
            args[1..].join(" ")
        ));
        return 1;
    }
    if first == "status" {
        return snapshot_status(ccs);
    }
    match write_snapshot_settings(ccs, None) {
        Ok(()) => {
            err("[OK] Usage snapshot off. Files already written stay until you remove them.\n");
            0
        }
        Err(()) => {
            err("[X] The usage snapshot setting could not be removed.\n");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// cursor-key
// ---------------------------------------------------------------------------

const CURSOR_HELP: &[&str] = &[
    "Cursor account key (whole-account Cursor usage in Analytics)",
    "",
    "Usage: ai-account-center dashboard cursor-key <command>",
    "",
    "Commands:",
    "  set      Paste the API key made for AAC on the Cursor dashboard, then Enter (stdin;",
    "           typed input is not shown; piped input reads to the end)",
    "  status   Show whether a key is kept, with its last four characters",
    "  remove   Forget the key",
    "",
    "The key is never an argument and never printed.",
    "",
];

fn cursor_help() -> String {
    CURSOR_HELP.join("\n")
}

fn reader_target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// The packaged reader next to the native bin (`dist/native/aac-cursor-account`), digest-checked.
fn cursor_reader() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let directory = exe.parent()?.join("../native/aac-cursor-account");
    let manifest = read_private(&directory.join("manifest.json"), 1024 * 1024)?;
    let object = manifest.as_object()?;
    if object.get("name")?.as_str()? != "aac-cursor-account" {
        return None;
    }
    let entry = object.get("targets")?.get(reader_target())?;
    let file = entry.get("file")?.as_str()?;
    if file.contains("..") {
        return None;
    }
    let expected = entry.get("sha256")?.as_str()?;
    if expected.len() != 64 {
        return None;
    }
    let binary = directory.join(file);
    let bytes = fs::read(&binary).ok()?;
    (hex(&Sha256::digest(&bytes)) == expected).then_some(binary)
}

fn run_reader(args: &[&str], input: &str, ccs: &Path) -> Result<Value, &'static str> {
    let Some(reader) = cursor_reader() else {
        return Err("no_reader");
    };
    let mut child = Command::new(reader)
        .args(args)
        .arg("--ccs-dir")
        .arg(ccs)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "reader_failed")?;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let output = child.wait_with_output().map_err(|_| "reader_failed")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    match serde_json::from_str::<Value>(line) {
        Ok(value) if value.is_object() => Ok(value),
        _ if output.status.success() => Err("bad_output"),
        _ => Err("reader_failed"),
    }
}

fn read_stdin_piped() -> Result<String, ()> {
    let mut text = String::new();
    let mut stdin = io::stdin().lock();
    let mut buffer = [0u8; 1024];
    loop {
        match stdin.read(&mut buffer) {
            Ok(0) => return Ok(text),
            Ok(count) => {
                text.push_str(&String::from_utf8_lossy(&buffer[..count]));
                if text.len() > MAX_STDIN {
                    return Err(());
                }
            }
            Err(_) => return Err(()),
        }
    }
}

/// A terminal hides the key: echo off on Windows, raw-ish termios on Unix; restored on exit.
#[allow(unsafe_code)]
fn read_stdin_hidden() -> Result<String, ()> {
    let mut line = String::new();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleMode(handle: *mut core::ffi::c_void, mode: *mut u32) -> i32;
            fn SetConsoleMode(handle: *mut core::ffi::c_void, mode: u32) -> i32;
        }
        let handle = io::stdin().as_raw_handle();
        let mut mode = 0u32;
        let ok = unsafe { GetConsoleMode(handle, &mut mode) } != 0;
        if ok {
            unsafe {
                SetConsoleMode(handle, mode & !0x0004);
            }
        }
        err("Paste the key, then Enter (it is not shown): ");
        let _ = io::stdout().flush();
        let result = io::stdin().read_line(&mut line);
        if ok {
            unsafe {
                SetConsoleMode(handle, mode);
            }
        }
        result.map_err(|_| ())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = io::stdin().as_raw_fd();
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::tcgetattr(fd, &mut original) } == 0;
        if ok {
            let mut hidden = original;
            hidden.c_lflag &= !(libc::ECHO | libc::ICANON);
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &hidden);
            }
        }
        err("Paste the key, then Enter (it is not shown): ");
        let _ = io::stdout().flush();
        let result = io::stdin().read_line(&mut line);
        if ok {
            unsafe {
                libc::tcsetattr(fd, libc::TCSANOW, &original);
            }
        }
        err("\n");
        result.map_err(|_| ())?;
    }
    Ok(line)
}

fn read_secret() -> Result<String, ()> {
    if io::stdin().is_terminal() {
        read_stdin_hidden()
    } else {
        read_stdin_piped()
    }
}

fn cursor_key(args: &[String], ccs: &Path) -> i32 {
    let Some(command) = args.first() else {
        out(&cursor_help());
        return 0;
    };
    let help_word = matches!(command.as_str(), "help" | "--help" | "-h");
    if help_word || args.len() > 1 {
        out(&cursor_help());
        return if help_word { 0 } else { 2 };
    }
    let command = command.as_str();
    if !matches!(command, "set" | "status" | "remove") {
        out(&cursor_help());
        return 2;
    }
    match command {
        "set" => {
            let secret = match read_secret() {
                Ok(text) => text.trim().to_string(),
                Err(()) => {
                    err("[X] The key is longer than any Cursor key; nothing was kept.\n");
                    return 1;
                }
            };
            if secret.is_empty() {
                err("[X] No key on stdin. Paste the key, then Enter (or pipe it in).\n");
                return 2;
            }
            match run_reader(&["key", "put"], &format!("{secret}\n"), ccs) {
                Ok(answer) => {
                    if answer.get("ok") != Some(&json!(true)) {
                        err(
                            if answer.get("error").and_then(Value::as_str) == Some("invalid_key") {
                                "[X] That is not a Cursor key (8 to 512 characters, no spaces); nothing was kept.\n"
                            } else {
                                "[X] The key could not be kept safely; nothing was kept.\n"
                            },
                        );
                        return 1;
                    }
                    let last4 = answer
                        .get("last4")
                        .map(Value::to_string)
                        .unwrap_or_default();
                    let last4 = last4.trim_matches('"');
                    out(&format!(
                        "[OK] Cursor key kept (ending {last4}). Analytics reads the account at its next refresh.\n"
                    ));
                    0
                }
                Err("no_reader") => {
                    err("[X] The Cursor account reader is not built for this computer.\n");
                    1
                }
                Err(_) => {
                    err("[X] The Cursor account reader did not answer.\n");
                    1
                }
            }
        }
        "status" => match run_reader(&["key", "status"], "", ccs) {
            Ok(answer) => {
                let last4 = answer
                    .get("last4")
                    .map(Value::to_string)
                    .unwrap_or_default();
                let last4 = last4.trim_matches('"');
                if answer.get("present") == Some(&json!(true)) {
                    out(&format!("Cursor key kept (ending {last4}).\n"));
                } else if answer.get("error").is_some() {
                    out(
                        "A Cursor key file is there but is not safe to use (permissions or format).\n",
                    );
                } else {
                    out("No Cursor key kept.\n");
                }
                0
            }
            Err("no_reader") => {
                err("[X] The Cursor account reader is not built for this computer.\n");
                1
            }
            Err(_) => {
                err("[X] The Cursor account reader did not answer.\n");
                1
            }
        },
        "remove" => match run_reader(&["key", "delete"], "", ccs) {
            Ok(answer) => {
                let removed = answer.get("removed").and_then(Value::as_f64).unwrap_or(0.0);
                out(if removed > 0.0 {
                    "[OK] Cursor key forgotten.\n"
                } else {
                    "No Cursor key was kept.\n"
                });
                0
            }
            Err("no_reader") => {
                err("[X] The Cursor account reader is not built for this computer.\n");
                1
            }
            Err(_) => {
                err("[X] The Cursor account reader did not answer.\n");
                1
            }
        },
        _ => 2,
    }
}

pub fn cursor_key_command(args: &[String], ccs: &Path) -> i32 {
    cursor_key(args, ccs)
}

// The key is never printed; only this module's own tests read the stored hash.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_key_shape_and_hash() {
        let key = new_hub_key();
        assert!(key.starts_with("aacu_"));
        assert_eq!(key.len(), 5 + 43);
        assert_eq!(hex(&Sha256::digest(key.as_bytes())).len(), 64);
    }

    #[test]
    fn snapshot_directory_rules() {
        assert!(valid_directory("/tmp/aac-usage"));
        assert!(!valid_directory("relative"));
        assert!(!valid_directory("/tmp/../x"));
        assert!(!valid_directory("/tmp/\u{1}"));
    }

    #[test]
    fn span_matches_the_reader_words() {
        assert_eq!(span(0.0), "now");
        assert_eq!(span(59.9), "59s");
        assert_eq!(span(3_600.0), "1h 0m");
        assert_eq!(span(90_000.0), "1d 1h");
    }
}
