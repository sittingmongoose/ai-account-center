//! The product config writer for admin CLI commands: the TS writer's lockfile protocol
//! (src/config/loader/io-locks.ts), backup naming and atomic 0600 save. The YAML layout is this
//! writer's canonical block style rather than js-yaml's; the parsed settings are what the
//! dashboard reads, every section outside the changed one keeps its value, and retired launcher
//! sections travel as their original raw blocks (appended after the active document). Unlike the
//! TS save, defaults are not materialized into the file: the dashboard merges them on read.

use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LOCK_STALE_MS: u64 = 5000;
const LOCK_RETRIES: usize = 10;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// The config.yaml.lock hold, released on drop exactly as the TS writer's finally block does.
pub struct ConfigLock {
    path: PathBuf,
    token: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(unsafe_code)] // libc::kill with signal 0 is the POSIX liveness probe; no memory is touched.
fn owner_alive(pid: u32, timestamp: u64) -> bool {
    #[cfg(unix)]
    {
        let _ = timestamp;
        // kill(pid, 0): success or EPERM means the process exists.
        let rc = unsafe { libc::kill(pid as i32, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        // No cheap liveness probe without a Windows process handle: a fresh timestamp holds,
        // so a live writer is never displaced; a dead owner's lock expires after the stale window.
        now_ms().saturating_sub(timestamp) <= LOCK_STALE_MS
    }
}

fn try_acquire(dir: &Path) -> Option<ConfigLock> {
    let path = dir.join("config.yaml.lock");
    if !dir.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir).ok()?;
    }
    if path.exists() {
        let content = fs::read_to_string(&path).ok()?;
        let mut lines = content.trim().split('\n');
        let pid = lines.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let timestamp = lines.next().and_then(|v| v.parse::<u64>().ok());
        let live = pid > 0 && owner_alive(pid, timestamp.unwrap_or(0));
        if live {
            return None;
        }
        let stale = timestamp.is_none_or(|ts| now_ms().saturating_sub(ts) > LOCK_STALE_MS);
        if stale || !live {
            fs::remove_file(&path).ok()?;
        }
    }
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let data = format!("{}\n{}\n{}", std::process::id(), now_ms(), token);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).ok()?;
    file.write_all(data.as_bytes()).ok()?;
    Some(ConfigLock { path, token })
}

/// Up to one second of retries, then the writer reports the lock (the TS withConfigWriteLock).
pub fn acquire(dir: &Path) -> Option<ConfigLock> {
    for _ in 0..LOCK_RETRIES {
        if let Some(lock) = try_acquire(dir) {
            return Some(lock);
        }
        std::thread::sleep(LOCK_RETRY_DELAY);
    }
    None
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if let Ok(content) = fs::read_to_string(&self.path) {
            if content.trim().split('\n').nth(2) == Some(self.token.as_str()) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

/// The previous config.yaml bytes beside it (0600, never overwriting), named by the UTC stamp
/// the TS backupConfig uses. None when there is no file yet.
pub fn backup_config(file: &Path, now: SystemTime) -> Result<Option<PathBuf>, String> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let stamp = chrono::DateTime::<chrono::Utc>::from(now).format("%Y%m%dT%H%M%SZ");
    let base = format!("{}.bak-proxy-{stamp}", file.display());
    for attempt in 0..100 {
        let target = if attempt == 0 {
            PathBuf::from(&base)
        } else {
            PathBuf::from(format!("{base}-{}", attempt + 1))
        };
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&target) {
            Ok(mut file) => {
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                drop(file);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
                        .map_err(|e| e.to_string())?;
                }
                return Ok(Some(target));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("No free name for the config.yaml backup".into())
}

/// Home-relative, for messages: never a full path outside the home folder.
pub fn display_path(file: &Path) -> String {
    let text = file.display().to_string();
    if let Some(home) = std::env::home_dir() {
        let home = home.display().to_string();
        let prefix = format!("{home}{}", std::path::MAIN_SEPARATOR);
        if text.starts_with(&prefix) {
            return format!("~{}", &text[home.len()..]);
        }
    }
    text
}

/// config.yaml's dashboard_tls block as the product reads it: an absent file is an empty block;
/// an unreadable or invalid file (isUnifiedConfig: a numeric version >= 1) is an error.
pub fn read_tls_block(file: &Path) -> Result<Value, ()> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(_) => return Err(()),
    };
    let (values, _) = aac_config::split_active(&text).map_err(|_| ())?;
    if !values.is_object() {
        return Err(());
    }
    match values.get("version").and_then(Value::as_f64) {
        Some(version) if version >= 1.0 => {}
        _ => return Err(()),
    }
    Ok(values
        .get("dashboard_tls")
        .filter(|block| block.is_object())
        .cloned()
        .unwrap_or(json!({})))
}

/// Read-modify-write under the product lock: back up the exact bytes being replaced, apply the
/// change to the active settings, and save atomically (0600). Returns the backup path, if any.
pub fn mutate(
    file: &Path,
    change: impl FnOnce(&mut Map<String, Value>),
) -> Result<Option<PathBuf>, String> {
    let dir = file.parent().ok_or("config path has no directory")?;
    let _lock = acquire(dir)
        .ok_or("Config file is locked by another process. Wait a moment and try again.")?;
    let original = match fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let (mut values, retired) = match original {
        Some(text) => aac_config::split_active(&text).map_err(|e| e.to_string())?,
        None => (json!({}), BTreeMap::new()),
    };
    let map = values
        .as_object_mut()
        .ok_or("configuration layer must be an object")?;
    // The product loader requires a numeric version >= 1; a config created here carries the
    // current schema version (UNIFIED_CONFIG_VERSION), as the TS save's defaults do.
    map.entry("version".to_string()).or_insert(json!(14));
    let backup = backup_config(file, SystemTime::now())?;
    change(map);
    let text = render(&values, &retired);
    atomic_write(file, text.as_bytes())?;
    Ok(backup)
}

fn atomic_write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = file.parent().ok_or("config path has no directory")?;
    let tmp = dir.join(format!(".config.yaml.tmp-{}-{}", std::process::id(), now_ms()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&tmp).map_err(|e| e.to_string())?;
    handle.write_all(bytes).map_err(|e| e.to_string())?;
    handle.sync_all().map_err(|e| e.to_string())?;
    drop(handle);
    fs::rename(&tmp, file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(file, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

const NETWORK_COMMENT: &str = "# Dashboard network: trust_local_network lets plain HTTP from trusted_networks carry\n# passwords, keys and sign-in codes (off by default). trusted_networks defaults to\n# 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 and fc00::/7.\n";
const HEADER: &str =
    "# AI Account Center configuration\n# Legacy launcher sections are retained but ignored.\n\n";

fn render(values: &Value, retired: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    if values.get("dashboard_network").is_some() {
        out.push_str(NETWORK_COMMENT);
    }
    out.push_str(HEADER);
    let empty = values.as_object().is_none_or(Map::is_empty);
    if !empty || retired.is_empty() {
        out.push_str(&emit_document(values));
    }
    for raw in retired.values() {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(raw);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn emit_document(value: &Value) -> String {
    match value {
        Value::Object(map) if map.is_empty() => "{}\n".into(),
        Value::Object(map) => {
            let mut out = String::new();
            emit_map(map, 0, &mut out);
            out
        }
        other => format!("{}\n", scalar(other)),
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

fn emit_map(map: &Map<String, Value>, indent: usize, out: &mut String) {
    for (key, value) in map {
        push_indent(out, indent);
        out.push_str(&key_text(key));
        out.push(':');
        emit_value(value, indent, out);
    }
}

fn emit_value(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(map) if map.is_empty() => out.push_str(" {}\n"),
        Value::Object(map) => {
            out.push('\n');
            emit_map(map, indent + 2, out);
        }
        Value::Array(items) if items.is_empty() => out.push_str(" []\n"),
        Value::Array(items) => {
            out.push('\n');
            for item in items {
                push_indent(out, indent + 2);
                out.push('-');
                emit_value(item, indent + 2, out);
            }
        }
        _ => {
            out.push(' ');
            out.push_str(&scalar(value));
            out.push('\n');
        }
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => quote(text),
        _ => "null".into(),
    }
}

/// A JSON string literal is a valid YAML double-quoted scalar (same escapes), for both parsers.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

fn plain_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return false;
    }
    !matches!(
        key.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
    )
}

fn key_text(key: &str) -> String {
    if plain_key(key) {
        key.into()
    } else {
        quote(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_emitted_yaml_parses_back_to_the_same_value() {
        let value = json!({
            "version": 1,
            "dashboard_tls": {
                "trusted_proxy": "lan-https-proxy",
                "trusted_proxy_addresses": ["10.0.0.5", "fd00::1"],
                "public_origin": "https://aac.example.test",
                "https_listener": {"enabled": true, "port": 3443, "cert_path": "~/certs/a.pem"}
            },
            "accounts": {"work": {"email": "a@b.test", "notes": "line\nbreak \"quoted\" \\ slash", "tags": []}},
            "empty_map": {},
            "flag": false,
            "nothing": null,
            "ratio": 0.5,
            "yes": "not-a-bool",
            "1numeric-key": "quoted key"
        });
        let text = render(&value, &BTreeMap::new());
        let parsed: Value = serde_yaml::from_str(&text).expect("valid yaml");
        assert_eq!(parsed, value);
    }

    #[test]
    fn retired_blocks_survive_a_save() {
        let value = json!({"version": 1});
        let mut retired = BTreeMap::new();
        retired.insert(
            "channels".to_string(),
            "channels:\n  custom: [a, b]\n".to_string(),
        );
        let text = render(&value, &retired);
        let parsed: Value = serde_yaml::from_str(&text).expect("valid yaml");
        assert_eq!(parsed["channels"], json!({"custom": ["a", "b"]}));
        assert_eq!(parsed["version"], json!(1));
    }

    #[test]
    fn read_tls_block_matches_the_product_validation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.yaml");
        assert_eq!(read_tls_block(&file).unwrap(), json!({}));
        std::fs::write(&file, "- not a mapping\n").unwrap();
        assert!(read_tls_block(&file).is_err());
        std::fs::write(&file, "agentVerbs: on\n").unwrap();
        assert!(read_tls_block(&file).is_err(), "version is required");
        std::fs::write(&file, "version: 1\ndashboard_tls:\n  trusted_proxy: tailscale-serve\n")
            .unwrap();
        assert_eq!(
            read_tls_block(&file).unwrap(),
            json!({"trusted_proxy": "tailscale-serve"})
        );
    }

    #[test]
    fn mutate_backs_up_and_saves_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.yaml");
        std::fs::write(&file, "version: 1\naccounts: {}\n").unwrap();
        let backup = mutate(&file, |config| {
            config.insert("dashboard_tls".into(), json!({"trusted_proxy": "lan-https-proxy"}));
        })
        .unwrap()
        .expect("backup");
        assert!(backup.display().to_string().contains(".bak-proxy-"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"version: 1\naccounts: {}\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let (values, _) = aac_config::split_active(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(values["accounts"], json!({}));
        assert_eq!(values["dashboard_tls"]["trusted_proxy"], json!("lan-https-proxy"));
        assert!(!dir.path().join("config.yaml.lock").exists(), "lock released");
    }

    #[test]
    fn a_live_lock_blocks_and_a_dead_lock_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("config.yaml.lock");
        // A dead owner with a fresh timestamp: the TS writer takes it over at once.
        std::fs::write(&lock, format!("999999\n{}\nother-token", now_ms())).unwrap();
        let held = acquire(dir.path()).expect("dead owner takeover");
        assert!(lock.exists());
        drop(held);
        assert!(!lock.exists(), "released on drop");
        // A live owner (this process) holds: acquire refuses after its retries.
        std::fs::write(
            &lock,
            format!("{}\n{}\nother-token", std::process::id(), now_ms()),
        )
        .unwrap();
        assert!(acquire(dir.path()).is_none());
        assert!(lock.exists(), "a live owner's lock stays");
    }
}
