//! Own-host additional-provider failover discovery and collection (Muse, Cursor, Kimi Code,
//! Qwen, Z.ai, OpenCode Go), ported from `scripts/account-usage/failover_additional.py`.
pub(crate) mod cursor;
pub(crate) mod desktop;
pub(crate) mod muse;
pub(crate) mod plans;
pub(crate) mod sqlite;

use aac_core::sha256_hex;
use aac_providers::ProviderId;
use desktop::{
    Credential, ReadTracker, canonical_ascii_json, config_home, credential_from_omp, cursor_root,
    email as desktop_email, env_get, jwt_claims, normalize_muse, now_unix_secs, platform_label,
    read_json_file, safe_text, secret as desktop_secret, utc_now_iso_secs,
};
use muse::{CAPSULE_NAME, MuseError, ReqwestPortalTransport};
use plans::{
    MAX_BYTES, ReqwestPlanTransport, aac_key_credential, email_address, env_var_names, fetch_qwen,
    opencode_aliases, parse_go, parse_kimi, parse_qwen, parse_zai, parse_zai_reset_packs,
    plan_account, plan_string, qwen_console_capsule, qwen_credential, qwen_gateway_cookie,
    safari_qwen_candidates,
};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    io::ErrorKind,
    path::Path,
    pin::Pin,
    sync::LazyLock,
};

const MAX_CANDIDATES: usize = 128;

static HEX8_SLOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A([a-f0-9]{8})\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static QWEN_CAPSULE_SLOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\Aqwen-console-session-([a-f0-9]{8})\.json\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});

/// Result of one failover adapter's read-only local inventory pass.
#[derive(Debug, Clone, PartialEq)]
pub struct InventoryResult {
    /// Candidate objects (six public fields plus private `_`-prefixed adapter fields).
    pub candidates: Vec<Value>,
    /// Sorted, bounded selectors whose store could not be read (`["*"]` when unattributed).
    pub incomplete_selectors: Vec<String>,
    /// Total counted read failures observed during this pass.
    pub read_failures: usize,
}

impl InventoryResult {
    /// Empty inventory with zero read failures.
    pub fn empty() -> Self {
        Self {
            candidates: Vec::new(),
            incomplete_selectors: Vec::new(),
            read_failures: 0,
        }
    }

    /// Encode into the failover dispatcher's inventory object shape.
    pub fn into_inventory_value(self) -> Value {
        let mut out = json!({ "candidates": self.candidates });
        if !self.incomplete_selectors.is_empty() {
            out["incompleteSelectors"] = json!(self.incomplete_selectors);
        }
        out
    }
}

/// Structured HTTP response returned by a [`PlanTransport`].
#[derive(Debug, Clone)]
pub enum PlanHttpBody {
    /// Decoded JSON object payload.
    Json(Value),
    /// Raw UTF-8 text payload (used for CN Qwen console HTML page).
    Text(String),
}

/// Structured collection error matching Python `plan_common.CollectionError`.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionError {
    /// Public status (`"needs_sign_in"`, `"unavailable"`, `"error"`).
    pub status: &'static str,
    /// Fixed, scrubbed message.
    pub message: &'static str,
    /// Optional structured failover failure code.
    pub failure_code: Option<&'static str>,
    /// Optional bounded retry-after seconds.
    pub retry_after: Option<f64>,
}

/// Injectable HTTP transport for additional-provider quota endpoints.
pub trait PlanTransport: Send + Sync {
    /// Issue a bounded GET or POST request.
    fn request<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a [u8]>,
        text: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PlanHttpBody, CollectionError>> + Send + 'a>>;
}

/// Structured Muse portal error matching Python `muse_console.MuseError`.
#[derive(Debug, Clone, PartialEq)]
pub struct PortalError {
    /// Fixed error code from `muse_console.ERROR_MESSAGES`.
    pub code: &'static str,
    /// Optional bounded retry-after seconds.
    pub retry_after: Option<f64>,
}

/// Injectable HTTP transport for Muse portal endpoints (`https://dev.meta.ai`).
pub trait PortalTransport: Send + Sync {
    /// Issue a GET request against one allowed Muse portal route.
    fn get<'a>(
        &'a self,
        route: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, PortalError>> + Send + 'a>>;
}

/// Optional DPAPI unprotect callback for Windows capsules/keys and cross-platform tests.
pub type DpapiUnprotect = dyn Fn(&[u8], Option<&[u8]>) -> Option<String> + Send + Sync;

/// Optional overrides for deterministic offline tests; production uses `Default`.
#[derive(Default)]
pub struct AdditionalEnv<'a> {
    /// Override current UTC timestamp in Unix seconds.
    pub now_secs: Option<f64>,
    /// Override process environment variables (e.g. `XDG_CONFIG_HOME`, API keys).
    pub env_vars: Option<&'a BTreeMap<String, String>>,
    /// Override Windows DPAPI decryption.
    pub dpapi: Option<&'a DpapiUnprotect>,
    /// Override key-file format (`Some(true)` forces `.json` plaintext keys).
    pub force_plaintext_keys: Option<bool>,
    /// Override HTTP transport for Muse native, Cursor, Kimi, Z.ai, OpenCode Go, Qwen.
    pub plan_transport: Option<&'a dyn PlanTransport>,
    /// Override HTTP transport for Muse portal (`https://dev.meta.ai`).
    pub portal_transport: Option<&'a dyn PortalTransport>,
}

/// Stateful adapter entrypoint aligned with `failover::Adapter`.
#[derive(Debug, Default)]
pub struct AdditionalAdapter;

impl AdditionalAdapter {
    /// Create a new additional-provider failover adapter.
    pub fn new() -> Self {
        Self
    }

    /// Run read-only local inventory and return the dispatcher's inventory object.
    #[allow(clippy::result_unit_err)]
    pub fn inventory_for(
        &mut self,
        provider: &str,
        platform: &str,
        home: &Path,
        ccs: &Path,
    ) -> Result<Value, ()> {
        Ok(inventory(provider, home, platform, Some(ccs)).into_inventory_value())
    }

    /// Collect one previously inventoried private candidate.
    pub async fn collect_for(
        &mut self,
        candidate: &Value,
        platform: &str,
        home: &Path,
        ccs: &Path,
    ) -> Value {
        collect(candidate, home, platform, Some(ccs)).await
    }
}

/// Whether `failover_additional` owns this provider id.
pub fn handles(provider: &str) -> bool {
    parse_additional_provider(provider).is_some()
}

fn parse_additional_provider(provider: &str) -> Option<ProviderId> {
    match ProviderId::parse(provider)? {
        id @ (ProviderId::Muse
        | ProviderId::Cursor
        | ProviderId::KimiCode
        | ProviderId::Qwen
        | ProviderId::Zai
        | ProviderId::OpencodeGo) => Some(id),
        _ => None,
    }
}

fn hash_field(kind: &str, value: &Value) -> String {
    let packed = canonical_ascii_json(value);
    sha256_hex(format!("aac-failover-v1:{kind}:{packed}").as_bytes())
}

fn slots(directory: &Path, pattern: &Regex, tracker: &mut ReadTracker) -> Vec<String> {
    match fs::symlink_metadata(directory) {
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Vec::new();
        }
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Vec::new();
            }
        }
    }
    let iter = match fs::read_dir(directory) {
        Ok(it) => it,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut entries = Vec::new();
    for entry in iter {
        match entry {
            Ok(e) => entries.push(e),
            Err(_) => {
                tracker.record_failure();
                return Vec::new();
            }
        }
    }
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for entry in entries.into_iter().take(512) {
        let is_link = fs::symlink_metadata(entry.path())
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(true);
        if is_link {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(cap) = pattern.captures(&name)
            && let Some(group) = cap.get(1)
        {
            out.push(group.as_str().to_owned());
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    out
}

fn cookie_header_pairs(value: &str) -> Result<Vec<[String; 2]>, ()> {
    if value.is_empty() || value.chars().count() > 32_768 {
        return Err(());
    }
    let mut pairs = Vec::new();
    let mut names = BTreeSet::new();
    for fragment in value.split(';') {
        let (name, content) = fragment.trim().split_once('=').ok_or(())?;
        if name.is_empty()
            || !names.insert(name.to_owned())
            || name
                .chars()
                .chain(content.chars())
                .any(|c| (c as u32) < 32 || (c as u32) == 127)
        {
            return Err(());
        }
        pairs.push([name.to_owned(), content.to_owned()]);
    }
    pairs.sort();
    Ok(pairs)
}

enum MuseContextRead {
    Absent,
    Verified(Value),
    Unreadable,
}

fn muse_context(
    ccs_dir: &Path,
    credential: &Credential,
    now_secs: Option<f64>,
    tracker: &mut ReadTracker,
) -> MuseContextRead {
    let Some(address) = credential
        .email
        .as_deref()
        .and_then(|e| desktop_email(&Value::String(e.to_owned())))
    else {
        return MuseContextRead::Absent;
    };
    let capsule = ccs_dir.join("account-usage").join(CAPSULE_NAME);
    let present = match fs::symlink_metadata(&capsule) {
        Ok(_) => true,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => false,
        Err(_) => true,
    };
    match muse::read_capsule(&ccs_dir.join("account-usage"), true, now_secs) {
        Ok((cookies, team, bound_email, plan)) => {
            if !bound_email.eq_ignore_ascii_case(&address) {
                return MuseContextRead::Absent;
            }
            MuseContextRead::Verified(json!({
                "email": address.to_ascii_lowercase(),
                "team": team,
                "plan": plan,
                "session": muse::session_values(&cookies)
            }))
        }
        Err(_) => {
            if !present {
                MuseContextRead::Absent
            } else {
                tracker.record_failure();
                MuseContextRead::Unreadable
            }
        }
    }
}

enum StoredCredential {
    Desktop(Credential),
    Plan(Value),
}

fn build_candidate(
    provider_id: ProviderId,
    locator: &str,
    selectors: Vec<String>,
    credential: StoredCredential,
    muse_ctx: Option<Value>,
) -> Option<Value> {
    let provider = provider_id.as_str();
    let (access, address, plan, cred_val) = match credential {
        StoredCredential::Desktop(c) => {
            let addr = c
                .email
                .as_deref()
                .and_then(|e| desktop_email(&Value::String(e.to_owned())));
            let pl = c
                .plan
                .as_deref()
                .and_then(|p| safe_text(&Value::String(p.to_owned()), 48));
            let val = c.to_private_value();
            (c.access, addr, pl, val)
        }
        StoredCredential::Plan(v) => {
            let secret = v.get("secret")?.as_str()?.to_owned();
            let addr = v.get("email").and_then(desktop_email);
            (secret, addr, None, v)
        }
    };
    if access.is_empty() || access.chars().count() > MAX_BYTES {
        return None;
    }
    let mut material = Value::String(access.clone());
    let (kind, identity_val): (String, Value) = match provider_id {
        ProviderId::Muse => {
            if let Some(ref ctx) = muse_ctx {
                let id_arr = json!([ctx["email"], ctx["team"]]);
                material = json!([access, ctx]);
                ("muse-team".to_owned(), id_arr)
            } else {
                ("muse-native-credential".to_owned(), Value::String(access))
            }
        }
        ProviderId::Cursor => {
            let claims = jwt_claims(&access);
            let sub = claims.get("sub").and_then(Value::as_str).filter(|s| {
                !s.is_empty()
                    && s.chars().count() <= 256
                    && !s.chars().any(|c| (c as u32) <= 0x20 || (c as u32) == 0x7f)
            });
            if let Some(s) = sub {
                ("cursor-subject".to_owned(), Value::String(s.to_owned()))
            } else {
                ("cursor-credential".to_owned(), Value::String(access))
            }
        }
        ProviderId::Qwen => {
            let (cookie, region) = qwen_credential(&access).ok()?;
            let gateway = qwen_gateway_cookie(&access, &cookie).ok()?;
            let cookie_pairs = cookie_header_pairs(&cookie).ok()?;
            let gateway_pairs = cookie_header_pairs(&gateway).ok()?;
            material = json!([region, cookie_pairs, gateway_pairs]);
            ("qwen-session".to_owned(), material.clone())
        }
        _ => (format!("{provider}-credential"), Value::String(access)),
    };
    let mut out = json!({
        "locator": hash_field(&format!("{provider}:locator"), &Value::String(locator.to_owned())),
        "selectors": selectors,
        "identity": {
            "kind": kind,
            "key": hash_field(&kind, &identity_val),
        },
        "revision": hash_field(&format!("{provider}:revision"), &material),
        "email": address,
        "plan": plan,
        "_provider": provider,
        "_credential": cred_val,
    });
    if provider_id == ProviderId::Muse {
        out["_muse_context"] = muse_ctx.unwrap_or(Value::Null);
    }
    Some(out)
}

fn desktop_candidates(
    provider_id: ProviderId,
    home: &Path,
    platform: &str,
    ccs_dir: &Path,
    env: &AdditionalEnv<'_>,
    tracker: &mut ReadTracker,
) -> Vec<(String, Vec<String>, StoredCredential)> {
    let mut found = Vec::new();
    let discover = vec!["discover".to_owned()];
    if provider_id == ProviderId::Muse {
        let mut paths = vec![(
            config_home(home, env.env_vars).join("muse/auth.json"),
            "native:muse".to_owned(),
            discover.clone(),
        )];
        let slot_list = tracker.attribute(&["config-home:*".to_owned()], |t| {
            slots(&ccs_dir.join("muse-homes"), &HEX8_SLOT_RE, t)
        });
        for slot in slot_list {
            let sel = format!("config-home:{slot}");
            paths.push((
                ccs_dir
                    .join("muse-homes")
                    .join(&slot)
                    .join("muse/auth.json"),
                sel.clone(),
                vec![sel],
            ));
        }
        for (path, locator, selectors) in paths {
            let doc = tracker.attribute(&selectors, |t| read_json_file(&path, t));
            if let Some(meta) = doc
                .as_ref()
                .and_then(|d| d.get("providers"))
                .and_then(|p| p.get("meta"))
                .and_then(Value::as_object)
                && let Some(token) = meta
                    .get("access_token")
                    .and_then(desktop_secret)
                    .filter(|s| s.starts_with("dca:"))
            {
                let addr = meta.get("user_email").and_then(desktop_email);
                found.push((
                    locator,
                    selectors,
                    StoredCredential::Desktop(Credential {
                        access: token,
                        source: "Muse CLI".to_owned(),
                        refresh: String::new(),
                        expires: None,
                        email: addr,
                        project: None,
                        plan: None,
                        google_client: None,
                    }),
                ));
            }
        }
    } else {
        let root = cursor_root(home, platform, env.env_vars);
        let fields = tracker.attribute(&discover, |t| {
            sqlite::cursor_vscdb_rows(&root.join("User/globalStorage/state.vscdb"), t)
        });
        let auth = tracker
            .attribute(&discover, |t| read_json_file(&root.join("auth.json"), t))
            .unwrap_or_else(|| json!({}));
        let sources = [
            (
                "cursor:sqlite",
                fields.get("cursorAuth/accessToken").cloned(),
                fields.get("cursorAuth/cachedEmail").cloned(),
                fields.get("cursorAuth/stripeMembershipType").cloned(),
            ),
            (
                "cursor:auth-json",
                auth.get("accessToken").cloned(),
                None,
                None,
            ),
        ];
        for (locator, token_v, email_v, plan_v) in sources {
            if let Some(token) = token_v.as_ref().and_then(desktop_secret) {
                let claims = jwt_claims(&token);
                let addr = claims
                    .get("email")
                    .and_then(desktop_email)
                    .or_else(|| email_v.as_ref().and_then(desktop_email));
                let pl = plan_v.as_ref().and_then(|v| safe_text(v, 160));
                found.push((
                    locator.to_owned(),
                    discover.clone(),
                    StoredCredential::Desktop(Credential {
                        access: token,
                        source: "Cursor desktop".to_owned(),
                        refresh: String::new(),
                        expires: None,
                        email: addr,
                        project: None,
                        plan: pl,
                        google_client: None,
                    }),
                ));
            }
        }
    }
    let omp_provider = if provider_id == ProviderId::Muse {
        "muse-code"
    } else {
        provider_id.as_str()
    };
    let omp_rows = tracker.attribute(&discover, |t| {
        sqlite::omp_additional_rows(home, omp_provider, MAX_CANDIDATES, t)
    });
    for row in omp_rows {
        if row.credential_type.as_deref() != Some("oauth") {
            continue;
        }
        if let Some(cred) = credential_from_omp(provider_id.as_str(), &row.data) {
            found.push((
                format!("omp:{}", row.row_id),
                discover.clone(),
                StoredCredential::Desktop(cred),
            ));
        }
    }
    found
}

fn plan_candidates(
    provider_id: ProviderId,
    home: &Path,
    ccs_dir: &Path,
    env: &AdditionalEnv<'_>,
    tracker: &mut ReadTracker,
) -> Vec<(String, Vec<String>, StoredCredential)> {
    let provider = provider_id.as_str();
    let discover = vec!["discover".to_owned()];
    let mut found: Vec<(String, Vec<String>, StoredCredential)> = Vec::new();

    let opencode = tracker
        .attribute(&discover, |t| {
            read_json_file(&home.join(".local/share/opencode/auth.json"), t)
        })
        .unwrap_or_else(|| json!({}));
    for alias in opencode_aliases(provider_id) {
        if let Some(item) = opencode.get(*alias).and_then(Value::as_object)
            && item.get("type").and_then(Value::as_str) == Some("api")
            && let Some(key) = item.get("key").and_then(plan_string)
        {
            found.push((
                format!("opencode:{alias}"),
                discover.clone(),
                StoredCredential::Plan(json!({
                    "secret": key,
                    "source": "OpenCode",
                    "email": null,
                    "expires": null
                })),
            ));
        }
    }

    let omp_provider = if provider_id == ProviderId::Qwen {
        "alibaba-token-plan"
    } else {
        provider
    };
    let omp_rows = tracker.attribute(&discover, |t| {
        sqlite::omp_additional_rows(home, omp_provider, MAX_CANDIDATES, t)
    });
    for row in omp_rows {
        let Some(kind) = row
            .credential_type
            .as_deref()
            .filter(|k| matches!(*k, "api_key" | "oauth"))
        else {
            continue;
        };
        let secret = row
            .data
            .get("key")
            .and_then(plan_string)
            .or_else(|| row.data.get("access").and_then(plan_string));
        let Some(secret) = secret else {
            continue;
        };
        let expires = if provider_id == ProviderId::KimiCode && kind == "oauth" {
            row.data.get("expires").and_then(plans::api_number)
        } else {
            None
        };
        found.push((
            format!("omp:{}", row.row_id),
            discover.clone(),
            StoredCredential::Plan(json!({
                "secret": secret,
                "source": "OMP",
                "email": row.data.get("email").and_then(email_address),
                "expires": expires
            })),
        ));
    }

    for name in env_var_names(provider_id) {
        if let Some(val) = env_get(name, env.env_vars)
            && let Some(secret) = plan_string(&Value::String(val))
        {
            found.push((
                format!("environment:{name}"),
                discover.clone(),
                StoredCredential::Plan(json!({
                    "secret": secret,
                    "source": "User environment",
                    "email": null,
                    "expires": null
                })),
            ));
        }
    }

    if provider_id.row().key_store
        && let Ok(pat) = Regex::new(&format!(
            r"\A{}-([a-f0-9]{{8}})\.(?:json|dpapi)\z",
            regex::escape(provider)
        ))
    {
        let raw_slots = tracker.attribute(&["aac-key:*".to_owned()], |t| {
            slots(&ccs_dir.join("account-usage/keys"), &pat, t)
        });
        let unique_slots: BTreeSet<String> = raw_slots.into_iter().collect();
        for slot in unique_slots {
            let sel = format!("aac-key:{slot}");
            let item = tracker.attribute(std::slice::from_ref(&sel), |t| {
                aac_key_credential(
                    provider,
                    &slot,
                    home,
                    Some(ccs_dir),
                    env.force_plaintext_keys,
                    env.dpapi,
                    t,
                )
            });
            if let Some(cred) = item {
                found.push((sel.clone(), vec![sel], StoredCredential::Plan(cred)));
            }
        }
    }

    if provider_id == ProviderId::Qwen {
        let mut browser: Vec<(String, Vec<String>, StoredCredential)> = Vec::new();
        let safari_items =
            tracker.attribute(&discover, |t| safari_qwen_candidates(home, env.now_secs, t));
        for item in safari_items {
            if let Some(s) = item.get("secret").and_then(Value::as_str)
                && let Ok((_, region)) = qwen_credential(s)
            {
                browser.push((
                    format!("safari:{region}"),
                    discover.clone(),
                    StoredCredential::Plan(item),
                ));
            }
        }
        let mut cap_slots = vec!["default".to_owned()];
        cap_slots.extend(tracker.attribute(&["browser-capsule:*".to_owned()], |t| {
            slots(&ccs_dir.join("account-usage"), &QWEN_CAPSULE_SLOT_RE, t)
        }));
        for slot in cap_slots {
            let mut selectors = vec![format!("browser-capsule:{slot}")];
            if slot == "default" {
                selectors.push("discover".to_owned());
            }
            let item = tracker.attribute(&selectors, |t| {
                qwen_console_capsule(home, &slot, Some(ccs_dir), env.dpapi, t)
            });
            if let Some(cred) = item {
                browser.insert(
                    0,
                    (
                        format!("browser-capsule:{slot}"),
                        selectors,
                        StoredCredential::Plan(cred),
                    ),
                );
            }
        }
        browser.extend(found);
        found = browser;
    }

    if provider_id == ProviderId::KimiCode {
        let now = now_unix_secs(env.now_secs);
        found.sort_by_key(|(_, _, cred)| {
            let exp = match cred {
                StoredCredential::Plan(v) => v.get("expires").and_then(Value::as_f64),
                StoredCredential::Desktop(c) => c.expires,
            };
            exp.is_some_and(|v| (if v > 100_000_000_000.0 { v / 1000.0 } else { v }) <= now)
        });
    }

    found
}

/// Read-only local credential inventory for additional providers.
pub fn inventory(
    provider: &str,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
) -> InventoryResult {
    inventory_with(provider, home, platform, ccs_dir, &AdditionalEnv::default())
}

/// Read-only local credential inventory with explicit test/clock overrides.
pub fn inventory_with(
    provider: &str,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
    env: &AdditionalEnv<'_>,
) -> InventoryResult {
    let Some(provider_id) = parse_additional_provider(provider) else {
        return InventoryResult::empty();
    };
    if platform_label(platform).is_none() {
        return InventoryResult::empty();
    }
    let ccs = ccs_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".ccs"));
    let mut tracker = ReadTracker::default();
    let found = if matches!(provider_id, ProviderId::Muse | ProviderId::Cursor) {
        desktop_candidates(provider_id, home, platform, &ccs, env, &mut tracker)
    } else {
        plan_candidates(provider_id, home, &ccs, env, &mut tracker)
    };
    let mut candidates = Vec::new();
    for (locator, selectors, cred) in found.into_iter().take(MAX_CANDIDATES) {
        let muse_ctx = if provider_id == ProviderId::Muse {
            let StoredCredential::Desktop(ref c) = cred else {
                continue;
            };
            match tracker.attribute(&selectors, |t| muse_context(&ccs, c, env.now_secs, t)) {
                MuseContextRead::Absent => None,
                MuseContextRead::Verified(ctx) => Some(ctx),
                MuseContextRead::Unreadable => continue,
            }
        } else {
            None
        };
        if let Some(item) = build_candidate(provider_id, &locator, selectors, cred, muse_ctx) {
            candidates.push(item);
        }
    }
    InventoryResult {
        candidates,
        incomplete_selectors: tracker.incomplete_selectors(),
        read_failures: tracker.total_failures(),
    }
}

fn failure_value(code: &str, retry: Option<f64>) -> Value {
    let mut out = json!({
        "sample": null,
        "failureCode": code,
    });
    if let Some(r) = retry.filter(|r| r.is_finite() && *r > 0.0 && *r <= 86_400.0) {
        out["retryAfterSeconds"] = json!(r);
    }
    out
}

fn account_mismatch_err() -> CollectionError {
    CollectionError {
        status: "unavailable",
        message: "The saved account identity could not be verified.",
        failure_code: Some("account_mismatch"),
        retry_after: None,
    }
}

async fn muse_identity(
    credential: &Credential,
    client: &dyn PlanTransport,
) -> Result<(Value, String, Option<String>), CollectionError> {
    let headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("x-api-version".to_owned(), "1.0.0".to_owned()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    let payload = match client
        .request(desktop::MUSE_URL, &headers, Some(b"{}"), false)
        .await?
    {
        PlanHttpBody::Json(v) => v,
        PlanHttpBody::Text(_) => {
            return Err(CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            });
        }
    };
    let Some(address) = payload.get("user_email").and_then(desktop_email) else {
        return Err(account_mismatch_err());
    };
    if let Some(ref expected) = credential.email
        && !address.eq_ignore_ascii_case(expected)
    {
        return Err(account_mismatch_err());
    }
    if payload.get("is_subs_active") == Some(&Value::Bool(false)) {
        return Err(CollectionError {
            status: "unavailable",
            message: "The subscription is inactive.",
            failure_code: Some("inactive_subscription"),
            retry_after: None,
        });
    }
    let plan = payload
        .get("subs_tier_name")
        .and_then(|v| safe_text(v, 160));
    Ok((payload, address, plan))
}

async fn collect_muse(
    credential: &Credential,
    result: &mut Value,
    ccs_dir: &Path,
    client: &dyn PlanTransport,
    context: Option<&Value>,
    env: &AdditionalEnv<'_>,
) -> Result<Option<Value>, CollectionError> {
    let mut tracker = ReadTracker::default();
    let current = match muse_context(ccs_dir, credential, env.now_secs, &mut tracker) {
        MuseContextRead::Unreadable => return Ok(Some(failure_value("unavailable", None))),
        MuseContextRead::Absent => None,
        MuseContextRead::Verified(ctx) => Some(ctx),
    };
    if current.as_ref() != context {
        return Ok(Some(failure_value("source_changed", None)));
    }
    let root = ccs_dir.join("account-usage");
    let Some(ctx) = context else {
        let mut native_sample = result.clone();
        let platform = native_sample
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("ubuntu")
            .to_owned();
        let out = muse::native_collect(
            &root,
            &credential.access,
            credential.email.as_deref(),
            &platform,
            env.now_secs,
            || async move {
                match muse_identity(credential, client).await {
                    Ok((payload, address, plan)) => {
                        let windows = normalize_muse(&payload);
                        native_sample["email"] = json!(address);
                        native_sample["plan"] = json!(plan);
                        native_sample["windows"] = Value::Array(windows);
                        if native_sample["windows"]
                            .as_array()
                            .is_none_or(Vec::is_empty)
                        {
                            let mut fail = failure_value("no_browser_cookie", None);
                            fail["_verifiedPlan"] = json!(plan);
                            return fail;
                        }
                        let now_iso = utc_now_iso_secs(env.now_secs);
                        native_sample["fetchedAt"] = json!(now_iso);
                        native_sample["sampledAt"] = json!(now_iso);
                        json!({ "sample": native_sample, "failureCode": null })
                    }
                    Err(err) => {
                        let code = err
                            .failure_code
                            .unwrap_or(if err.status == "needs_sign_in" {
                                "needs_sign_in"
                            } else {
                                "provider_error"
                            });
                        failure_value(code, err.retry_after)
                    }
                }
            },
        )
        .await;
        return Ok(Some(out));
    };

    let (cookies, team, address_bound, bound_plan) =
        match muse::read_capsule(&root, false, env.now_secs) {
            Ok(tuple) => tuple,
            Err(err) => return Ok(Some(failure_value(err.code, err.retry_after))),
        };
    if !credential
        .email
        .as_deref()
        .is_some_and(|e| e.eq_ignore_ascii_case(&address_bound))
    {
        return Err(account_mismatch_err());
    }
    let ctx_team = ctx.get("team").and_then(Value::as_str).unwrap_or("");
    let ctx_email = ctx.get("email").and_then(Value::as_str).unwrap_or("");
    let ctx_session = ctx.get("session").cloned().unwrap_or(Value::Null);
    if team != ctx_team
        || !address_bound.eq_ignore_ascii_case(ctx_email)
        || json!(muse::session_values(&cookies)) != ctx_session
    {
        return Ok(Some(failure_value("source_changed", None)));
    }
    let owned_portal;
    let portal_ref: &dyn PortalTransport = if let Some(p) = env.portal_transport {
        p
    } else {
        owned_portal = match ReqwestPortalTransport::new(&cookies, env.now_secs) {
            Ok(p) => p,
            Err(err) => return Ok(Some(failure_value(err.code, err.retry_after))),
        };
        &owned_portal
    };
    let bound_email_for_closure = address_bound.clone();
    let sample = match muse::quota_sample(
        &root,
        &cookies,
        &address_bound,
        bound_plan,
        Some(&team),
        &credential.access,
        portal_ref,
        env.now_secs,
        Some(&team),
        Some(|| async move {
            match muse_identity(credential, client).await {
                Ok((_, addr, plan)) => {
                    if !addr.eq_ignore_ascii_case(&bound_email_for_closure) {
                        return Err(MuseError::new("account_mismatch"));
                    }
                    Ok(plan.unwrap_or_default())
                }
                Err(err) => {
                    let code = err
                        .failure_code
                        .unwrap_or(if err.status == "needs_sign_in" {
                            "needs_sign_in"
                        } else {
                            "provider_error"
                        });
                    Err(MuseError::with_retry(code, err.retry_after))
                }
            }
        }),
    )
    .await
    {
        Ok(s) => s,
        Err(err) => {
            let mut fail = failure_value(err.code, err.retry_after);
            if let Some(pl) = err
                .plan
                .as_deref()
                .and_then(|p| safe_text(&Value::String(p.to_owned()), 160))
                && muse::valid_plan_timestamp(err.plan_at.as_deref(), env.now_secs)
            {
                fail["observedPlan"] = json!(pl);
                fail["observedPlanAt"] = json!(err.plan_at);
            }
            return Ok(Some(fail));
        }
    };

    let retained = sample
        .get("retained")
        .or_else(|| sample.get("cached"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    result["email"] = json!(address_bound);
    result["plan"] = sample["quota"]["tier"].clone();
    result["windows"] = Value::Array(normalize_muse(&json!({
        "subscription_quota": sample["quota"]
    })));
    result["status"] = json!(if retained { "cached" } else { "ok" });
    result["sampledAt"] = sample["sampledAt"].clone();
    result["message"] = sample["message"].clone();

    let mut meta = Map::new();
    for key in [
        "failureCode",
        "retryAfterSeconds",
        "refreshAfterSeconds",
        "observedPlan",
        "observedPlanAt",
    ] {
        if let Some(v) = sample.get(key) {
            meta.insert(key.to_owned(), v.clone());
        }
    }
    Ok(Some(Value::Object(meta)))
}

async fn collect_plan(
    provider_id: ProviderId,
    credential: &Value,
    result: &mut Value,
    home: &Path,
    client: &dyn PlanTransport,
    now_secs: Option<f64>,
) -> Result<Option<Value>, CollectionError> {
    if let Some(expires) = credential.get("expires").and_then(Value::as_f64) {
        let exp_s = if expires > 100_000_000_000.0 {
            expires / 1000.0
        } else {
            expires
        };
        if exp_s <= now_unix_secs(now_secs) {
            return Ok(Some(failure_value("needs_sign_in", None)));
        }
    }
    let Some(secret) = credential.get("secret").and_then(Value::as_str) else {
        return Ok(Some(failure_value("invalid_request", None)));
    };
    let get_json = |url: &'static str, headers: Vec<(String, String)>| async move {
        match client.request(url, &headers, None, false).await? {
            PlanHttpBody::Json(v) => Ok(v),
            PlanHttpBody::Text(_) => Err(CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            }),
        }
    };
    let (plan, mut windows) = match provider_id {
        ProviderId::KimiCode => {
            let payload = get_json(
                "https://api.kimi.com/coding/v1/usages",
                vec![("Authorization".into(), format!("Bearer {secret}"))],
            )
            .await?;
            parse_kimi(&payload)?
        }
        ProviderId::Zai => {
            let payload = get_json(
                "https://api.z.ai/api/monitor/usage/quota/limit",
                vec![
                    ("Authorization".into(), secret.to_owned()),
                    ("Content-Type".into(), "application/json".into()),
                ],
            )
            .await?;
            let (plan, mut windows) = parse_zai(&payload)?;
            match get_json(
                "https://api.z.ai/api/biz/customer-package-reset/list?targetType=PERSONAL",
                vec![("Authorization".into(), format!("Bearer {secret}"))],
            )
            .await
            {
                Ok(packs_payload) => match parse_zai_reset_packs(&packs_payload, now_secs) {
                    Ok(packs) => windows.extend(packs),
                    Err(_) => {
                        result["message"] =
                            json!("Account usage is current; reset pack inventory is unavailable.");
                    }
                },
                Err(error) => {
                    result["message"] =
                        json!("Account usage is current; reset pack inventory is unavailable.");
                    if error.failure_code == Some("rate_limited") {
                        result["plan"] = json!(plan);
                        result["windows"] = Value::Array(windows);
                        return Ok(Some(json!({
                            "failureCode": "rate_limited",
                            "retryAfterSeconds": error.retry_after.unwrap_or(600.0)
                        })));
                    }
                }
            }
            (plan, windows)
        }
        ProviderId::OpencodeGo => {
            let installation = &sha256_hex(home.to_string_lossy().as_bytes())[..32];
            let payload = get_json(
                "https://opencode.ai/zen/go/v1/usage",
                vec![
                    ("Authorization".into(), format!("Bearer {secret}")),
                    (
                        "x-opencode-session".into(),
                        format!("ccs-usage-{installation}"),
                    ),
                ],
            )
            .await?;
            parse_go(&payload)?
        }
        ProviderId::Qwen => {
            let (usage, sub, cfg, addon) = fetch_qwen(client, secret).await?;
            parse_qwen(&usage, sub.as_ref(), cfg.as_ref(), addon.as_ref())?
        }
        _ => return Ok(Some(failure_value("invalid_request", None))),
    };
    result["plan"] = json!(plan);
    result["windows"] = Value::Array(std::mem::take(&mut windows));
    Ok(None)
}

/// Collect usage for one freshly inventoried private candidate.
pub async fn collect(
    candidate: &Value,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
) -> Value {
    collect_with(
        candidate,
        home,
        platform,
        ccs_dir,
        &AdditionalEnv::default(),
    )
    .await
}

/// Collect usage for one private candidate with explicit test/transport overrides.
pub async fn collect_with(
    candidate: &Value,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
    env: &AdditionalEnv<'_>,
) -> Value {
    let Some(provider_str) = candidate.get("_provider").and_then(Value::as_str) else {
        return failure_value("invalid_request", None);
    };
    let Some(provider_id) = parse_additional_provider(provider_str) else {
        return failure_value("invalid_request", None);
    };
    let Some(host_label) = platform_label(platform) else {
        return failure_value("invalid_request", None);
    };
    let Some(cred_val) = candidate.get("_credential").filter(|v| !v.is_null()) else {
        return failure_value("invalid_request", None);
    };
    let ccs = ccs_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".ccs"));
    let is_desktop = matches!(provider_id, ProviderId::Muse | ProviderId::Cursor);
    let mut result = if is_desktop {
        desktop::account(provider_str, platform)
    } else {
        plan_account(provider_str, platform)
    };
    result["status"] = json!("ok");
    result["message"] = Value::Null;
    result["email"] = candidate.get("email").cloned().unwrap_or(Value::Null);
    result["plan"] = candidate.get("plan").cloned().unwrap_or(Value::Null);
    let source = cred_val
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("Local credential store");
    result["source"] = json!(format!("{source} on {host_label}"));

    let owned_client;
    let client: &dyn PlanTransport = if let Some(c) = env.plan_transport {
        c
    } else {
        owned_client =
            match ReqwestPlanTransport::new(if is_desktop { 16 } else { 7 }, env.now_secs) {
                Ok(c) => c,
                Err(err) => {
                    let code = err.failure_code.unwrap_or("provider_error");
                    return failure_value(code, err.retry_after);
                }
            };
        &owned_client
    };

    let outcome: Result<Option<Value>, CollectionError> = match provider_id {
        ProviderId::Muse => {
            let Some(cred) = Credential::from_private_value(cred_val) else {
                return failure_value("invalid_request", None);
            };
            let ctx = candidate.get("_muse_context").filter(|v| !v.is_null());
            collect_muse(&cred, &mut result, &ccs, client, ctx, env).await
        }
        ProviderId::Cursor => {
            let Some(cred) = Credential::from_private_value(cred_val) else {
                return failure_value("invalid_request", None);
            };
            cursor::collect(&cred, &mut result, client, env.now_secs)
                .await
                .map(|m| Some(Value::Object(m)))
        }
        _ => {
            collect_plan(
                provider_id,
                cred_val,
                &mut result,
                home,
                client,
                env.now_secs,
            )
            .await
        }
    };

    match outcome {
        Ok(failure) => {
            if let Some(ref f) = failure
                && f.as_object().is_some_and(|o| o.contains_key("sample"))
            {
                return f.clone();
            }
            if result["windows"].as_array().is_none_or(Vec::is_empty) {
                let mut fail = failure_value("invalid_response", None);
                if provider_id == ProviderId::Cursor
                    && let Some(obj) = fail.as_object_mut()
                {
                    obj.extend(cursor::observation(&result, None));
                }
                return fail;
            }
            let now_iso = utc_now_iso_secs(env.now_secs);
            result["fetchedAt"] = json!(now_iso);
            if result["sampledAt"].is_null() {
                result["sampledAt"] = result["fetchedAt"].clone();
            }
            let mut out = Map::new();
            out.insert("sample".into(), result);
            out.insert("failureCode".into(), Value::Null);
            if let Some(Value::Object(extra)) = failure {
                out.extend(extra);
            }
            Value::Object(out)
        }
        Err(error) => {
            let code = error
                .failure_code
                .unwrap_or(if error.status == "needs_sign_in" {
                    "needs_sign_in"
                } else {
                    "provider_error"
                });
            let mut fail = failure_value(code, error.retry_after);
            if provider_id == ProviderId::Cursor
                && let Some(obj) = fail.as_object_mut()
            {
                obj.extend(cursor::observation(&result, Some(code)));
            }
            fail
        }
    }
}

/// Compute `plan_common.retry_after_seconds` against `now_secs`.
pub fn plan_retry_after_seconds(value: Option<&str>, now_secs: f64) -> f64 {
    plans::retry_after_seconds(value, now_secs)
}

/// Compute `cursor_usage.normalize_shared` directly for unit tests.
pub fn cursor_normalize_shared(payload: &Value, plan_info: &Value, plan: &str) -> Vec<Value> {
    cursor::normalize_shared(payload, plan_info, plan)
}
