use super::policy::{Connection, Network};
use aac_core::Failure;
use aac_files::read::{Ancestors, Mode, Read, ReadPolicy};
use aac_files::{OwnedStore, registry};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD},
};
use chrono::{DateTime, SecondsFormat, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const DAY: i64 = 86_400_000;
pub(super) const DAYS: [i64; 5] = [1, 7, 30, 90, 365];
const SECRET: &str = ".session-secret";
const CREDENTIAL: &str = "auth/credential.json";
const BAR: &str = "bar/.auth-token";
const CONFIG: &str = "config.yaml";
const SESSIONS: &str = "auth/sessions.json";
const EPOCH: &str = "auth/state.json";
const DEVICES: &str = "auth/devices.json";
const SETUP: &str = "auth/setup-code";
const SMALL: u64 = 262_144;
const SESSION_CAP: usize = 500;

#[derive(Clone, Copy, Debug)]
pub(super) struct StoreError;

impl From<Failure> for StoreError {
    fn from(_: Failure) -> Self {
        Self
    }
}

pub(super) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or(0)
}

pub(super) fn iso(time: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(time)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub(super) fn timestamp(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
}

pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

pub(super) fn sha(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

pub(super) fn random<const N: usize>() -> Result<[u8; N], StoreError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| StoreError)?;
    Ok(bytes)
}

pub(super) fn token() -> Result<String, StoreError> {
    Ok(format!("aacd_{}", URL_SAFE_NO_PAD.encode(random::<32>()?)))
}

pub(super) fn mac(secret: &str, text: &str) -> Result<[u8; 32], StoreError> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).map_err(|_| StoreError)?;
    mac.update(text.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

pub(super) fn same(left: &str, right: &str) -> bool {
    use subtle::ConstantTimeEq;
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub(super) struct Files {
    root: PathBuf,
    entries: BTreeMap<&'static str, OwnedStore>,
}

impl Files {
    pub(super) fn claim(root: PathBuf) -> Result<Self, StoreError> {
        let rows = registry::rows()?;
        let names = [
            ("auth-sessions", SESSIONS),
            ("auth-state", EPOCH),
            ("auth-devices", DEVICES),
            ("auth-setup-code", SETUP),
            ("auth-credential", CREDENTIAL),
            ("auth-session-secret", SECRET),
            ("bar-auth-token", BAR),
        ];
        let owners = registry::Owners::from_boot(
            rows,
            &rows
                .iter()
                .map(|row| {
                    let owner = if names.iter().any(|(id, _)| *id == row.id) {
                        "rust-service"
                    } else {
                        row.owner.word()
                    };
                    (row.id.clone(), owner.to_owned())
                })
                .collect(),
        )?;
        let mut entries = BTreeMap::new();
        for (id, path) in names {
            entries.insert(path, owners.claim(rows, id, &root, None)?);
        }
        let files = Self { root, entries };
        // The TS store keeps an exclusive process marker while it owns persisted sessions.
        // A public start over the same root must follow a clean TS shutdown.
        if files.bytes("auth/sessions.owner", 4096, true)?.is_some() {
            return Err(StoreError);
        }
        Ok(files)
    }

    fn bytes(&self, name: &str, cap: u64, private: bool) -> Result<Option<Vec<u8>>, StoreError> {
        let mut policy = ReadPolicy::capped(cap).ancestors(Ancestors::NoLinkBelow(&self.root));
        if private {
            policy = policy.mode(Mode::Private);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let path = self.root.join(name);
                let folder = path.parent().ok_or(StoreError)?;
                let metadata = std::fs::symlink_metadata(folder).map_err(|_| StoreError)?;
                if !metadata.is_dir() || metadata.permissions().mode() & 0o022 != 0 {
                    return Err(StoreError);
                }
            }
        }
        match aac_files::read::read(&self.root.join(name), &policy) {
            Read::Bytes(bytes, _) => Ok(Some(bytes)),
            Read::Absent => Ok(None),
            _ => Err(StoreError),
        }
    }

    pub(super) fn write(&self, file: &'static str, bytes: &[u8]) -> Result<(), StoreError> {
        self.entries.get(file).ok_or(StoreError)?.write(bytes)?;
        Ok(())
    }

    pub(super) fn read_json(&self, name: &str, cap: u64) -> Result<Option<Value>, StoreError> {
        self.bytes(name, cap, true)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| StoreError))
            .transpose()
    }

    pub(super) fn write_json(&self, name: &'static str, value: &Value) -> Result<(), StoreError> {
        let mut text = serde_json::to_string_pretty(value).map_err(|_| StoreError)?;
        text.push('\n');
        self.write(name, text.as_bytes())
    }

    pub(super) fn devices(&self) -> Result<Option<Value>, StoreError> {
        self.read_json(DEVICES, SMALL)
    }
    pub(super) fn save_devices(&self, value: &Value) -> Result<(), StoreError> {
        self.write_json(DEVICES, value)
    }

    pub(super) fn config(&self) -> Result<Value, StoreError> {
        let Some(bytes) = self.bytes(CONFIG, 4 * 1024 * 1024, false)? else {
            return Ok(json!({}));
        };
        let value: Value = serde_yaml::from_slice(&bytes).map_err(|_| StoreError)?;
        match value {
            Value::Null => Ok(json!({})),
            Value::Object(_) => Ok(value),
            _ => Err(StoreError),
        }
    }

    pub(super) fn config_writer(&self) -> Result<OwnedStore, StoreError> {
        Ok(OwnedStore::claim_dashboard_config(&self.root)?)
    }

    pub(super) fn save_config(writer: &OwnedStore, value: &Value) -> Result<(), StoreError> {
        let text = serde_yaml::to_string(value).map_err(|_| StoreError)?;
        writer.write(text.as_bytes())?;
        Ok(())
    }

    pub(super) fn epoch(&self) -> Result<i64, StoreError> {
        let Some(state) = self.read_json(EPOCH, SMALL)? else {
            return Ok(0);
        };
        if state["version"] != 1 {
            return Err(StoreError);
        }
        state["sessionEpoch"]
            .as_i64()
            .filter(|epoch| epoch.unsigned_abs() <= 9_007_199_254_740_991)
            .ok_or(StoreError)
    }

    pub(super) fn ensure_epoch(&self) -> Result<i64, StoreError> {
        if let Ok(epoch) = self.epoch() {
            return Ok(epoch);
        }
        self.save_epoch((now() / 1000).max(1))
    }

    pub(super) fn bump_epoch(&self) -> Result<i64, StoreError> {
        match self.epoch() {
            Ok(epoch) if epoch < 9_007_199_254_740_991 => self.save_epoch(epoch + 1),
            _ => self.save_epoch((now() / 1000).max(1)),
        }
    }

    fn save_epoch(&self, epoch: i64) -> Result<i64, StoreError> {
        self.write_json(EPOCH, &json!({"version":1,"sessionEpoch":epoch}))?;
        Ok(epoch)
    }

    pub(super) fn sync_credential(&self, config: &Config) -> Result<(), StoreError> {
        if !config.configured() {
            return Ok(());
        }
        let expected = config.stamp();
        match self.read_json(CREDENTIAL, SMALL) {
            Ok(None) => (),
            Ok(Some(value)) if value["version"] == 1 && value["stamp"] == expected => return Ok(()),
            _ => {
                self.bump_epoch()?;
            }
        }
        self.save_credential(config)
    }

    pub(super) fn save_credential(&self, config: &Config) -> Result<(), StoreError> {
        self.write_json(CREDENTIAL, &json!({"version":1,"stamp":config.stamp()}))
    }

    pub(super) fn secret(&self, overrides: &Overrides) -> Result<String, StoreError> {
        if let Some(value) = overrides.secret.as_ref().filter(|value| !value.is_empty()) {
            return Ok(value.clone());
        }
        if let Some(value) = self
            .text(SECRET, SMALL)?
            .filter(|value| value.encode_utf16().count() >= 32)
        {
            return Ok(value);
        }
        let secret = hex(&random::<32>()?);
        self.write(SECRET, secret.as_bytes())?;
        Ok(secret)
    }

    pub(super) fn bar_proof(&self, nonce: &str) -> Result<String, StoreError> {
        let secret = match self.text(BAR, 4096)?.filter(|value| is_hex(value, 64)) {
            Some(secret) => secret,
            None => {
                let secret = hex(&random::<32>()?);
                self.write(BAR, format!("{secret}\n").as_bytes())?;
                secret
            }
        };
        Ok(hex(&mac(&secret, nonce)?))
    }

    fn text(&self, path: &str, cap: u64) -> Result<Option<String>, StoreError> {
        self.bytes(path, cap, true)?
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map(|text| text.trim().to_owned())
                    .map_err(|_| StoreError)
            })
            .transpose()
    }

    pub(super) fn setup(&self, config: &Config) -> Result<Option<Setup>, StoreError> {
        if !config.enabled || config.configured() || config.managed_by_env {
            return Ok(None);
        }
        // Exactly 32 allowed characters, so each mask is uniform.
        const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
        let code: String = random::<8>()?
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
            .collect();
        self.write(SETUP, format!("{}-{}\n", &code[..4], &code[4..]).as_bytes())?;
        Ok(Some(Setup {
            code,
            expires: now() + 3_600_000,
        }))
    }

    pub(super) fn consume_setup(&self) -> Result<(), StoreError> {
        self.entries.get(SETUP).ok_or(StoreError)?.remove()?;
        Ok(())
    }
}

pub(super) fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) struct Setup {
    code: String,
    expires: i64,
}

impl Setup {
    pub(super) fn print(&self) {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() {
            println!(
                "Dashboard first-run setup code: {}-{}",
                &self.code[..4],
                &self.code[4..]
            );
        }
        println!("Valid for 60 minutes. Saved in auth/setup-code in the CCS folder (0600).");
    }

    pub(super) fn matches(&self, candidate: &str, now: i64) -> bool {
        now < self.expires && same(&self.code, &candidate.replace('-', "").to_uppercase())
    }
}

#[derive(Default)]
pub(super) struct Overrides {
    enabled: Option<bool>,
    username: Option<String>,
    password: Option<String>,
    secret: Option<String>,
}

impl Overrides {
    pub(super) fn from_process() -> Self {
        let env = |name| std::env::var(name).ok();
        Self {
            enabled: env("CCS_DASHBOARD_AUTH_ENABLED")
                .map(|value| matches!(value.as_str(), "1" | "true")),
            username: env("CCS_DASHBOARD_USERNAME"),
            password: env("CCS_DASHBOARD_PASSWORD_HASH"),
            secret: env("CCS_SESSION_SECRET"),
        }
    }
}

pub(super) struct Config {
    pub(super) doc: Value,
    pub(super) enabled: bool,
    pub(super) username: String,
    pub(super) hash: String,
    pub(super) managed_by_env: bool,
    pub(super) days: i64,
    pub(super) network: Network,
}

impl Config {
    pub(super) fn new(doc: Value, env: &Overrides) -> Self {
        let auth = &doc["dashboard_auth"];
        let days = auth["session_lifetime_days"]
            .as_i64()
            .filter(|days| DAYS.contains(days))
            .or_else(|| {
                let hours = auth["session_timeout_hours"]
                    .as_f64()
                    .filter(|hours| *hours > 0.0 && *hours != 24.0)?;
                DAYS.into_iter().min_by(|a, b| {
                    (*a as f64 * 24.0 - hours)
                        .abs()
                        .total_cmp(&(*b as f64 * 24.0 - hours).abs())
                })
            })
            .unwrap_or(7);
        Self {
            enabled: env.enabled.unwrap_or(auth["enabled"] == true),
            username: env
                .username
                .clone()
                .unwrap_or_else(|| auth["username"].as_str().unwrap_or_default().into()),
            hash: env
                .password
                .clone()
                .unwrap_or_else(|| auth["password_hash"].as_str().unwrap_or_default().into()),
            managed_by_env: env.username.is_some() || env.password.is_some(),
            days,
            network: Network::from_config(&doc["dashboard_network"], &doc["dashboard_tls"]),
            doc,
        }
    }
    pub(super) fn configured(&self) -> bool {
        !self.username.is_empty() && !self.hash.is_empty()
    }
    pub(super) fn source(&self) -> &'static str {
        if self.managed_by_env { "env" } else { "config" }
    }
    pub(super) fn stamp(&self) -> String {
        sha(&format!(
            "aac-dashboard-credential\0{}\0{}\0{}",
            self.enabled, self.username, self.hash
        ))
    }
    pub(super) fn changed_at(&self) -> Option<&str> {
        (!self.managed_by_env).then_some(())?;
        self.doc["dashboard_auth"]["password_changed_at"]
            .as_str()
            .filter(|value| timestamp(value).is_some())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cookie {
    original_max_age: Option<i64>,
    #[serde(default)]
    expires: Option<String>,
    #[serde(default)]
    secure: bool,
    http_only: bool,
    path: String,
    same_site: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Session {
    cookie: Cookie,
    #[serde(default)]
    pub(super) authenticated: bool,
    pub(super) username: Option<String>,
    #[serde(default)]
    pub(super) epoch: i64,
    pub(super) signed_in_over: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idle_lifetime_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(super) revoked: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Session {
    pub(super) fn expires_at(&self) -> Option<&str> {
        self.cookie.expires.as_deref()
    }
    pub(super) fn remembers(&self) -> bool {
        self.cookie.original_max_age.is_some()
    }

    pub(super) fn current(&self, epoch: Result<i64, StoreError>) -> bool {
        self.authenticated && epoch.is_ok_and(|value| value == self.epoch)
    }

    fn roll(&mut self, now: i64, default_idle: i64) -> i64 {
        let expiry = now.saturating_add(
            self.cookie
                .original_max_age
                .or(self.idle_lifetime_ms)
                .unwrap_or(default_idle),
        );
        if self.remembers() {
            self.cookie.expires = iso(expiry);
        }
        expiry
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    key: String,
    data: String,
    expires: i64,
}

pub(super) struct Sessions {
    records: BTreeMap<String, Stored>,
    secret: String,
    last_save: i64,
}

impl Sessions {
    pub(super) fn open(files: &Files, secret: String) -> Self {
        let mut records = BTreeMap::new();
        if let Ok(Some(doc)) = files.read_json(SESSIONS, 4 * 1024 * 1024) {
            if let Some(entries) = doc["sessions"].as_array().filter(|_| doc["version"] == 1) {
                for value in entries.iter().take(SESSION_CAP) {
                    let Ok(record) = serde_json::from_value::<Stored>(value.clone()) else {
                        continue;
                    };
                    if is_hex(&record.key, 64)
                        && record.data.len() <= 8192
                        && record.expires > now()
                    {
                        records.entry(record.key.clone()).or_insert(record);
                    }
                }
            }
        }
        Self {
            records,
            secret,
            last_save: now(),
        }
    }

    pub(super) fn read(
        &self,
        cookie: &str,
        connection: &Connection,
        at: i64,
    ) -> Option<(String, Session)> {
        let signed = cookie
            .split(';')
            .find_map(|part| part.trim().strip_prefix("connect.sid="))?;
        let signed = signed
            .strip_prefix("s%3A")
            .or_else(|| signed.strip_prefix("s:"))?;
        let (id, proof) = signed.split_once('.')?;
        if id.len() > 256 {
            return None;
        }
        let proof = proof.replace("%2B", "+").replace("%2F", "/");
        if !same(&STANDARD_NO_PAD.encode(mac(&self.secret, id).ok()?), &proof) {
            return None;
        }
        let record = self
            .records
            .get(&sha(id))
            .filter(|record| record.expires > at)?;
        let session: Session = serde_json::from_str(&record.data).ok()?;
        if session.authenticated
            && !connection.credential_allowed(session.signed_in_over.as_deref())
        {
            return None;
        }
        Some((id.to_owned(), session))
    }

    fn save(
        &mut self,
        files: &Files,
        sessions: BTreeMap<String, Stored>,
        at: i64,
    ) -> Result<(), StoreError> {
        let mut entries: Vec<_> = sessions
            .into_values()
            .filter(|record| record.expires > at)
            .collect();
        entries.sort_by(|a, b| b.expires.cmp(&a.expires));
        entries.truncate(SESSION_CAP);
        files.write_json(SESSIONS, &json!({"version":1,"sessions":entries}))?;
        self.records = entries
            .into_iter()
            .map(|record| (record.key.clone(), record))
            .collect();
        self.last_save = at;
        Ok(())
    }

    pub(super) fn count_others(
        &self,
        id: Option<&str>,
        epoch: Result<i64, StoreError>,
        at: i64,
    ) -> usize {
        let own = id.map(sha);
        self.records
            .values()
            .filter(|record| record.expires > at && own.as_ref() != Some(&record.key))
            .filter_map(|record| serde_json::from_str::<Session>(&record.data).ok())
            .filter(|session| session.current(epoch))
            .count()
    }

    pub(super) fn start(
        &mut self,
        files: &Files,
        issue: NewSession<'_>,
    ) -> Result<(String, Session), StoreError> {
        let id = URL_SAFE_NO_PAD.encode(random::<24>()?);
        let idle = issue.days * DAY;
        let remember = issue
            .remember
            .unwrap_or_else(|| issue.prior.is_none_or(|(_, old)| old.remembers()));
        let mut session = Session {
            cookie: Cookie {
                original_max_age: remember.then_some(idle),
                expires: None,
                secure: issue.connection.transport.word() == "encrypted",
                http_only: true,
                path: "/".into(),
                same_site: "strict".into(),
            },
            authenticated: true,
            username: Some(issue.username.into()),
            epoch: files.ensure_epoch()?,
            signed_in_over: Some(issue.connection.transport.word().into()),
            idle_lifetime_ms: (!remember).then_some(idle),
            revoked: false,
        };
        let expiry = session.roll(issue.at, idle);
        let mut next = self.records.clone();
        if let Some((old, _)) = issue.prior {
            next.remove(&sha(old));
        }
        insert(&mut next, &id, &session, expiry)?;
        self.save(files, next, issue.at)?;
        Ok((id, session))
    }

    pub(super) fn touch(
        &mut self,
        files: &Files,
        id: &str,
        session: &mut Session,
        at: i64,
        idle: i64,
    ) {
        let expiry = session.roll(at, idle);
        let _ = insert(&mut self.records, id, session, expiry);
        if at.saturating_sub(self.last_save) >= 60_000 {
            let _ = self.save(files, self.records.clone(), at);
        }
    }

    pub(super) fn flush(&mut self, files: &Files) {
        if !self.records.is_empty() {
            let _ = self.save(files, self.records.clone(), now());
        }
    }

    pub(super) fn destroy(&mut self, files: &Files, id: &str, at: i64) -> Result<(), StoreError> {
        let mut next = self.records.clone();
        next.remove(&sha(id));
        self.save(files, next, at)
    }

    pub(super) fn cookie(
        &self,
        id: &str,
        session: &Session,
        connection: &Connection,
    ) -> Option<String> {
        if session.cookie.secure && connection.transport.word() != "encrypted" {
            return None;
        }
        let proof = STANDARD_NO_PAD
            .encode(mac(&self.secret, id).ok()?)
            .replace('+', "%2B")
            .replace('/', "%2F");
        let mut value = format!("connect.sid=s%3A{id}.{proof}; Path=/");
        if let Some(expires) = session
            .expires_at()
            .and_then(timestamp)
            .and_then(DateTime::from_timestamp_millis)
        {
            value.push_str(&format!(
                "; Expires={}",
                expires.format("%a, %d %b %Y %H:%M:%S GMT")
            ));
        }
        value.push_str("; HttpOnly");
        if session.cookie.secure {
            value.push_str("; Secure");
        }
        value.push_str("; SameSite=Strict");
        Some(value)
    }
}

pub(super) struct NewSession<'a> {
    pub(super) prior: Option<&'a (String, Session)>,
    pub(super) username: &'a str,
    pub(super) remember: Option<bool>,
    pub(super) connection: &'a Connection,
    pub(super) days: i64,
    pub(super) at: i64,
}

fn insert(
    records: &mut BTreeMap<String, Stored>,
    id: &str,
    session: &Session,
    expires: i64,
) -> Result<(), StoreError> {
    let key = sha(id);
    let data = serde_json::to_string(session).map_err(|_| StoreError)?;
    records.insert(key.clone(), Stored { key, data, expires });
    Ok(())
}

pub(super) fn root_from_env() -> Option<PathBuf> {
    // This server never discovers the user's real home implicitly.
    let path = std::env::var_os("CCS_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CCS_HOME").map(|home| Path::new(&home).join(".ccs")))?;
    path.is_absolute().then_some(path)
}
