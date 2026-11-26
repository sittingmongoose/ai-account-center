//! Read-only failover discovery. Only six reconstructed public fields leave this module.
//! Unproved stores, grammar and IO outcomes delegate the entire inventory to Python.
mod claude;
mod core;
pub(crate) mod dispatch;
use crate::{files, proof};
use aac_core::Failure;
use aac_files::read::{Ancestors, Mode, Owner, ReadPolicy};
use base64::Engine;
pub(crate) use dispatch::{Adapter, Context};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const CORE_CAP: usize = 1_048_576;
const DESKTOP_CAP: usize = 65_536;

fn matches(pattern: &str, value: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(value))
}
/// Python's `_scope`: an absent value is the default, which passes the same rule as an explicit
/// one (Python's `Path.home()` and `os.path.abspath` normalize both); `None` delegates.
fn scope(value: Option<&Value>, default: &str) -> Option<PathBuf> {
    let s = match value {
        None | Some(Value::Null) => default,
        Some(Value::String(s)) => s.as_str(),
        _ => {
            proof::reject();
            return None;
        }
    };
    if !s.is_empty()
        && s.len() <= 4096
        && s.bytes().all(|c| (0x20..=0x7e).contains(&c))
        && !files::double_root(s)
        && Path::new(s).is_absolute()
    {
        Some(files::lexical(Path::new(s)))
    } else {
        proof::reject();
        None
    }
}
fn missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}
// A symlink is outside this subset, even when Python can safely resolve it. Inspect each
// component before any open/enumeration so an unsupported store cannot redirect a read.
fn metadata(path: &Path) -> Option<fs::Metadata> {
    if files::linked_within(root_of(path), path) {
        proof::reject();
        return None;
    }
    match fs::symlink_metadata(path) {
        Ok(m) => Some(m),
        Err(e) if missing(&e) => None,
        Err(_) => {
            proof::reject();
            None
        }
    }
}
/// The root this path starts from ("/" on Unix), so a link anywhere on it is seen.
fn root_of(path: &Path) -> &Path {
    path.ancestors().last().unwrap_or(path)
}
fn exists(path: &Path) -> bool {
    metadata(path).is_some()
}
// Restrict input to serde's canonical string/number spellings plus ordinary JSON
// whitespace. Comparing a compact raw document also detects duplicate keys at every
// depth; serde Value alone would discard them before Python's strict main sees them.
fn document(bytes: &[u8], cap: usize, bom: bool) -> Option<Value> {
    let value = proof::json(bytes, cap, bom)?;
    let raw = if bom {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    } else {
        bytes
    };
    let mut compact = Vec::new();
    let (mut quoted, mut escaped) = (false, false);
    for &byte in raw {
        if quoted {
            compact.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
            compact.push(byte);
        } else if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            compact.push(byte);
        }
    }
    if serde_json::to_vec(&value).is_ok_and(|b| b == compact) {
        Some(value)
    } else {
        proof::reject();
        None
    }
}
pub(crate) fn request_proven(bytes: &[u8]) -> bool {
    bytes.len() <= 64 * 1024 && document(bytes, 64 * 1024, false).is_some()
}
/// The reason a request frame that fails the strict input check is refused with.
const UNPROVEN: &str = "request-not-proven";
/// What survives of a raw request frame that failed `request_proven`: the marker and the
/// action, when it is one of the two this verb serves.
pub(crate) fn unproven_args(args: &Value) -> Value {
    let action = args["action"]
        .as_str()
        .filter(|a| ["inventory", "collect"].contains(a));
    json!({"unprovenRawRequest": true, "action": action})
}
/// The honest answer to a frame that failed the strict input check: an unread inventory or an
/// unavailable collection, with the reason. It never delegates to Python.
fn unproven(args: &Value) -> Value {
    if args["action"] == "collect" {
        let mut out = dispatch::failure("unavailable", &Value::Null, &Value::Null);
        out["reason"] = json!(UNPROVEN);
        out
    } else {
        json!({"version":1,"action":"inventory","candidates":[],"failed":true,"reason":UNPROVEN})
    }
}
fn read(path: &Path, cap: usize, bom: bool) -> Option<Value> {
    read_with_policy(path, cap, bom, false)
}
fn read_with_policy(path: &Path, cap: usize, bom: bool, private: bool) -> Option<Value> {
    // Python's `_read_present`: one ordinary file of this user, of at most `cap` bytes, with no
    // link anywhere on its path; a private record also has no group or other bit. Absent reads
    // as absent; everything else is outside this subset and delegates.
    let policy = ReadPolicy::capped(cap as u64)
        .ancestors(Ancestors::NoLinkBelow(root_of(path)))
        .owner(Owner::Mine)
        .mode(if private { Mode::Private } else { Mode::Any });
    let bytes = match files::read_with(path, &policy) {
        files::Regular::Read(bytes, _) => bytes,
        files::Regular::Missing => return None,
        _ => {
            proof::reject();
            return None;
        }
    };
    let value = document(&bytes, cap, bom)?;
    if !value.is_object() {
        proof::reject();
        return None;
    }
    Some(value)
}
fn children(path: &Path, dirs_only: bool, cap: usize) -> Vec<PathBuf> {
    let Some(info) = metadata(path) else {
        return vec![];
    };
    if !info.is_dir() {
        proof::reject();
        return vec![];
    }
    // Count every entry before filtering, matching Python's raw listing bound.
    let Ok(iter) = aac_files::read::entries(path, cap) else {
        proof::reject();
        return vec![];
    };
    let mut result = Vec::new();
    for entry in iter {
        // An entry error or one entry past the bound.
        let Ok(entry) = entry else {
            proof::reject();
            return vec![];
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            proof::reject();
            return vec![];
        };
        if !proof::ascii(&name) {
            return vec![];
        }
        result.push(entry.path());
    }
    result.sort();
    if dirs_only {
        result.retain(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            files::safe_name(name) && metadata(p).is_some_and(|m| m.is_dir())
        });
        if result.len() > 64 {
            proof::reject();
            result.clear();
        }
    }
    result
}
// Python's sort_keys + ensure_ascii on the proved ASCII/integer subset.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(obj) => {
            let sorted: BTreeMap<_, _> =
                obj.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
            serde_json::to_value(sorted).unwrap_or(Value::Null)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
fn digest(value: &Value) -> String {
    aac_core::sha256_hex(serde_json::to_vec(&canonical(value)).unwrap_or_default())
}
fn additional_hash(kind: &str, value: &Value) -> String {
    let packed = serde_json::to_string(&canonical(value)).unwrap_or_default();
    aac_core::sha256_hex(format!("aac-failover-v1:{kind}:{packed}"))
}
fn desktop_text(value: &Value, cap: usize) -> Option<String> {
    let raw = value.as_str()?;
    if raw.len() > cap {
        return None;
    }
    let text = proof::strip(raw);
    if text.is_empty()
        || text.bytes().any(|c| c < 32 || c == 127)
        || matches(
            r"(?i)(bearer[\t\n\x0b\x0c\r ]|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})",
            text,
        )
    {
        return None;
    }
    Some(text.into())
}
fn public_text(value: &Value, cap: usize) -> Option<String> {
    let raw = value.as_str()?;
    if raw.len() > cap
        || raw.bytes().any(|c| c < 32 || c == 127)
        || matches(
            r"(?i)(bearer[\t\n\x0b\x0c\r ]|sk[-_]|dca:|eyJ[A-Za-z0-9_-]{8}|access_token|refresh_token|https?://)",
            raw,
        )
    {
        return None;
    }
    let text = proof::strip(raw);
    (!text.is_empty()).then(|| text.into())
}
fn email(value: &Value, public: bool) -> Option<String> {
    let text = if public {
        public_text(value, 254)
    } else {
        desktop_text(value, 254)
    }?;
    matches(
        r"\A[^\t\n\x0b\x0c\r @]+@[^\t\n\x0b\x0c\r @]+\.[^\t\n\x0b\x0c\r @]+\z",
        &text,
    )
    .then_some(text)
}
fn secret(value: &Value) -> Option<&str> {
    let raw = value.as_str()?;
    if raw.len() > 16384 {
        return None;
    }
    let text = proof::strip(raw);
    (!text.is_empty() && text.bytes().all(|c| c > 32 && c != 127)).then_some(text)
}
fn id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && s.bytes().all(|c| c > 32 && c != 127))
}
fn uuid(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| {
            matches(
                r"\A[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\z",
                s,
            )
        })
        .map(str::to_ascii_lowercase)
}
fn jwt(value: &Value) -> Value {
    let Some(text) = value.as_str().filter(|s| s.len() <= 32768) else {
        return json!({});
    };
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || p.len() % 4 == 1
                || !p
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        })
    {
        return json!({});
    }
    let mut decoded = Vec::new();
    for part in &parts[..2] {
        let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part) else {
            // Python also accepts noncanonical pad bits; that grammar is delegated.
            proof::reject();
            return json!({});
        };
        let Some(value) = proof::json(&bytes, CORE_CAP, false) else {
            return json!({});
        };
        if !value.is_object() {
            return json!({});
        }
        decoded.push(value);
    }
    decoded.pop().unwrap_or_else(|| json!({}))
}
fn roots(home: &Path, ccs: &Path, provider: &str) -> Vec<(PathBuf, Vec<String>)> {
    let mut result = vec![(
        home.join(if provider == "codex" {
            ".codex"
        } else {
            ".claude"
        }),
        vec!["discover".into(), format!("{provider}-profile:default")],
    )];
    for root in children(
        &ccs.join(if provider == "codex" {
            "codex-instances"
        } else {
            "instances"
        }),
        true,
        1024,
    ) {
        let name = root.file_name().unwrap_or_default().to_string_lossy();
        result.push((root.clone(), vec![format!("{provider}-profile:{name}")]));
    }
    let mut t3 = children(
        &home.join(if provider == "codex" {
            ".codex-t3"
        } else {
            ".claude-t3"
        }),
        true,
        1024,
    );
    if provider == "codex" {
        t3.extend(
            children(&home.join(".t3/userdata/providers/codex"), true, 1024)
                .into_iter()
                .map(|p| p.join("shadow")),
        );
    }
    let settings =
        read(&home.join(".t3/userdata/settings.json"), CORE_CAP, true).unwrap_or(Value::Null);
    if let Some(instances) = settings.get("providerInstances").filter(|v| !v.is_null()) {
        if let Some(instances) = instances.as_object() {
            if instances.len() > 64 {
                proof::reject();
            }
            for item in instances.values().take(64) {
                if item.is_null() {
                    continue;
                }
                if !item.is_object() {
                    proof::reject();
                    continue;
                }
                if item["driver"]
                    != (if provider == "codex" {
                        "codex"
                    } else {
                        "claudeAgent"
                    })
                {
                    continue;
                }
                if item
                    .get("config")
                    .is_some_and(|v| !v.is_null() && !v.is_object())
                {
                    proof::reject();
                    continue;
                }
                for field in if provider == "codex" {
                    &["homePath", "shadowHomePath"][..]
                } else {
                    &["homePath"][..]
                } {
                    let value = &item["config"][field];
                    if value.is_null() {
                        continue;
                    }
                    let Some(raw) = value
                        .as_str()
                        .filter(|s| s.len() <= 1024 && !s.contains('\0'))
                    else {
                        proof::reject();
                        continue;
                    };
                    let text = proof::strip(raw);
                    if text.is_empty() {
                        continue;
                    }
                    // Python's pathlib keeps a leading "//" as its own root and refuses the
                    // home; folding it into "/" would turn that refusal into a candidate.
                    if files::double_root(text) {
                        continue;
                    }
                    let path = if text == "~" {
                        home.to_path_buf()
                    } else if text.starts_with("~/") || text.starts_with("~\\") {
                        home.join(&text[2..])
                    } else {
                        PathBuf::from(text)
                    };
                    // An expanded "~" can only produce "//" from a home the scope already refused.
                    let path = files::lexical(&path);
                    if path.is_absolute() && (path.starts_with(home) || path.starts_with(ccs)) {
                        t3.push(path);
                    } else {
                        proof::reject();
                    }
                }
            }
        } else {
            proof::reject();
        }
    }
    result.extend(t3.into_iter().map(|p| (p, vec!["t3".into()])));
    if result.len() > 256 {
        proof::reject();
        result.truncate(256);
    }
    result
}
fn core_candidates(provider: &str, home: &Path, ccs: &Path) -> Vec<Value> {
    let mut unique: Vec<Value> = vec![];
    for (root, selectors) in roots(home, ccs, provider) {
        if !root.starts_with(home) && !root.starts_with(ccs) {
            proof::reject();
            continue;
        }
        let (storage, binding, revision, address, plan, private) = if provider == "codex" {
            let storage = root.join("auth.json");
            let raw = read(&storage, CORE_CAP, true).unwrap_or(Value::Null);
            let tokens = &raw["tokens"];
            let claims = jwt(&tokens["id_token"]);
            let auth = &claims["https://api.openai.com/auth"];
            let Some(account) = id(&auth["chatgpt_account_id"]) else {
                continue;
            };
            let address_raw = if py_truth(&claims["email"]) {
                &claims["email"]
            } else {
                &claims["https://api.openai.com/profile"]["email"]
            };
            let Some(address) = email(address_raw, false) else {
                continue;
            };
            if !tokens["account_id"].is_null() && tokens["account_id"] != account {
                continue;
            }
            let fields = [
                &auth["chatgpt_user_id"],
                &auth["user_id"],
                &claims["sub"],
                &claims["iss"],
            ];
            if fields.iter().any(|v| !v.is_null() && id(v).is_none()) {
                continue;
            }
            if py_truth(fields[0]) && py_truth(fields[1]) && fields[0] != fields[1] {
                continue;
            }
            let user = if py_truth(fields[0]) {
                fields[0]
            } else {
                fields[1]
            };
            let binding = json!([
                account,
                user,
                claims["sub"],
                claims["iss"],
                if py_truth(user) || (py_truth(&claims["sub"]) && py_truth(&claims["iss"])) {
                    Value::Null
                } else {
                    json!(address)
                }
            ]);
            let access = jwt(&tokens["access_token"]);
            let access_auth = &access["https://api.openai.com/auth"];
            if [
                ("chatgpt_account_id", &auth["chatgpt_account_id"]),
                ("chatgpt_user_id", user),
                ("user_id", user),
            ]
            .iter()
            .any(|(key, expected)| {
                access_auth
                    .get(key)
                    .is_some_and(|v| py_truth(expected) && v != *expected)
            }) || [("sub", &claims["sub"]), ("iss", &claims["iss"])]
                .iter()
                .any(|(key, expected)| {
                    access
                        .get(key)
                        .is_some_and(|v| py_truth(expected) && v != *expected)
                })
            {
                continue;
            }
            if secret(&tokens["access_token"]).is_none() {
                continue;
            }
            let plan = desktop_text(&auth["chatgpt_plan_type"], 64);
            (
                storage,
                binding.clone(),
                json!([tokens, binding]),
                address,
                plan,
                json!({"_token":secret(&tokens["access_token"]),"_account":account}),
            )
        } else {
            let metadata_path = if root == home.join(".claude") {
                home.join(".claude.json")
            } else {
                root.join(".claude.json")
            };
            let raw = read(&metadata_path, CORE_CAP, true).unwrap_or(Value::Null);
            let account = &raw["oauthAccount"];
            let (Some(account_id), Some(org_id), Some(address)) = (
                uuid(&account["accountUuid"]),
                uuid(&account["organizationUuid"]),
                email(&account["emailAddress"], false),
            ) else {
                continue;
            };
            let storage = root.join(".credentials.json");
            let credentials = read(&storage, CORE_CAP, true).unwrap_or(Value::Null);
            let oauth = &credentials["claudeAiOauth"];
            if secret(&oauth["accessToken"]).is_none() {
                continue;
            }
            let plan = desktop_text(&oauth["subscriptionType"], 64);
            (
                storage,
                json!([account_id, org_id]),
                json!([oauth, account_id, org_id, address]),
                address,
                plan,
                json!({"_token":secret(&oauth["accessToken"]),"_account":account_id,"_organization":org_id,
                    "_expires":oauth["expiresAt"].as_f64().filter(|n| n.is_finite()).and(oauth.get("expiresAt").cloned())}),
            )
        };
        let mut item = json!({"locator":digest(&json!([provider, storage.to_string_lossy().replace('\\', "/")])), "selectors":selectors,
            "identity":{"kind": if provider == "codex" { "codex-principal-v1" } else { "claude-account-org-v1" }, "key":digest(&json!([provider, binding]))},
            "revision":digest(&revision), "email":address, "plan":plan});
        item.as_object_mut()
            .expect("candidate")
            .extend(private.as_object().expect("private").clone());
        if let Some(old) = unique.iter_mut().find(|v| v["locator"] == item["locator"]) {
            let mut selectors: Vec<_> = old["selectors"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(item["selectors"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            selectors.sort();
            selectors.dedup();
            old["selectors"] = json!(selectors);
        } else if unique.len() < 64 {
            unique.push(item);
        }
    }
    unique
}
fn py_truth(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
fn additional_candidate(
    provider: &str,
    locator: &str,
    selectors: &[String],
    access: &str,
    address: Option<String>,
) -> Value {
    let kind = if provider == "muse" {
        "muse-native-credential".into()
    } else {
        format!("{provider}-credential")
    };
    json!({"locator":additional_hash(&format!("{provider}:locator"), &json!(locator)), "selectors":selectors,
        "identity":{"kind":kind,"key":additional_hash(&kind, &json!(access))}, "revision":additional_hash(&format!("{provider}:revision"), &json!(access)), "email":address, "plan":null})
}
// Qwen binds the two cookie sessions, not an inference key beside them.
fn plan_candidate(provider: &str, locator: &str, access: &str) -> Option<Value> {
    if provider != "qwen" {
        return Some(additional_candidate(
            provider,
            locator,
            &["discover".into()],
            access,
            None,
        ));
    }
    let packed = proof::json(access.as_bytes(), 32768, false)?;
    let cookie = packed["cookie"].as_str()?;
    if proof::strip(cookie).is_empty()
        || cookie.len() > 32768
        || cookie.bytes().any(|b| matches!(b, 0 | 10 | 13))
    {
        return None;
    }
    let base = packed.get("baseUrl").unwrap_or(&Value::Null);
    let base = if packed.get("baseUrl").is_none() {
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
    } else {
        base.as_str()?
    };
    let region = match base.trim_end_matches('/') {
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1" => "intl",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1" => "cn",
        _ => return None,
    };
    let gateway = match packed.get("gatewayCookie") {
        Some(v) => v.as_str()?,
        None => cookie,
    };
    if gateway.len() > 32768 || gateway.bytes().any(|b| matches!(b, 0 | 10 | 13)) {
        return None;
    }
    fn pairs(raw: &str) -> Option<Vec<(String, String)>> {
        if raw.is_empty() || raw.len() > 32768 {
            return None;
        }
        let mut pairs = BTreeMap::new();
        for fragment in raw.split(';') {
            let (name, content) = proof::strip(fragment).split_once('=')?;
            if name.is_empty()
                || name
                    .bytes()
                    .chain(content.bytes())
                    .any(|b| b < 32 || b == 127)
                || pairs.insert(name.into(), content.into()).is_some()
            {
                return None;
            }
        }
        Some(pairs.into_iter().collect())
    }
    let material = json!([region, pairs(cookie)?, pairs(gateway)?]);
    Some(
        json!({"locator":additional_hash("qwen:locator", &json!(locator)), "selectors":["discover"],
        "identity":{"kind":"qwen-session", "key":additional_hash("qwen-session", &material)},
        "revision":additional_hash("qwen:revision", &material), "email":null,"plan":null}),
    )
}
fn keys(provider: &str, ccs: &Path) -> Vec<Value> {
    let directory = ccs.join("account-usage/keys");
    let entries = children(&directory, false, 512);
    // Python's `_slots`: the sorted listing's matching non-link names, cut to 128 before the
    // json/dpapi twins collapse into one slot each.
    let mut slots: Vec<String> = entries
        .iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let rest = name.strip_prefix(&format!("{provider}-"))?;
            let id = rest
                .strip_suffix(".json")
                .or_else(|| rest.strip_suffix(".dpapi"))?;
            if !matches(r"\A[a-f0-9]{8}\z", id) {
                return None;
            }
            match fs::symlink_metadata(p) {
                Ok(m) if m.file_type().is_symlink() => None,
                Ok(_) => Some(id.into()),
                Err(_) => {
                    proof::reject();
                    None
                }
            }
        })
        .collect();
    slots.truncate(128);
    slots.sort();
    slots.dedup();
    let mut found = Vec::new();
    for slot in slots {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata(&directory).is_none_or(|m| m.mode() & 0o077 != 0) {
                proof::reject();
                continue;
            }
        }
        let path = directory.join(format!("{provider}-{slot}.json"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata(&path).is_some_and(|m| m.mode() & 0o077 != 0) {
                proof::reject();
                continue;
            }
        }
        let Some(record) = read_with_policy(&path, 4096, false, true) else {
            if exists(&directory.join(format!("{provider}-{slot}.dpapi"))) {
                proof::reject();
            }
            continue;
        };
        let Some(access) = record["secret"]
            .as_str()
            .filter(|s| (8..=512).contains(&s.len()) && s.bytes().all(|c| (33..=126).contains(&c)))
        else {
            proof::reject();
            continue;
        };
        if record["version"] != 1
            || record["provider"] != provider
            || record["keyId"] != slot
            || record.as_object().is_none_or(|o| {
                o.keys().any(|k| {
                    ![
                        "version",
                        "provider",
                        "keyId",
                        "secret",
                        "fingerprint",
                        "last4",
                        "createdAt",
                    ]
                    .contains(&k.as_str())
                })
            })
            || record.get("fingerprint").is_some_and(|v| {
                v != &json!(format!("sha256:{}", &aac_core::sha256_hex(access)[..16]))
            })
        {
            proof::reject();
            continue;
        }
        let locator = format!("aac-key:{slot}");
        found.push(additional_candidate(
            provider,
            &locator,
            std::slice::from_ref(&locator),
            access,
            None,
        ));
    }
    found
}
fn additional(provider: &str, home: &Path, ccs: &Path) -> Vec<Value> {
    if exists(&home.join(".omp/agent/agent.db")) {
        return proof_rejected();
    }
    if provider == "cursor" {
        return proof_rejected();
    }
    if provider == "muse" {
        if exists(&ccs.join("account-usage/muse-console-session.json")) {
            return proof_rejected();
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        if !proof::ascii(&config.to_string_lossy()) {
            return vec![];
        }
        let mut paths = vec![(
            config.join("muse/auth.json"),
            "native:muse".into(),
            vec!["discover".into()],
        )];
        for slot in children(&ccs.join("muse-homes"), false, 512) {
            let name = slot.file_name().unwrap_or_default().to_string_lossy();
            if matches(r"\A[a-f0-9]{8}\z", &name) {
                let locator = format!("config-home:{name}");
                paths.push((slot.join("muse/auth.json"), locator.clone(), vec![locator]));
            }
        }
        let mut result = Vec::new();
        for (path, locator, selectors) in paths.into_iter().take(129) {
            let raw = read(&path, DESKTOP_CAP, true).unwrap_or(Value::Null);
            let meta = &raw["providers"]["meta"];
            if let Some(access) = secret(&meta["access_token"]).filter(|s| s.starts_with("dca:")) {
                result.push(additional_candidate(
                    provider,
                    &locator,
                    &selectors,
                    access,
                    email(&meta["user_email"], false),
                ));
            }
        }
        result.truncate(128);
        return result;
    }
    if provider == "qwen" {
        // Python reads Safari's cookie store for Qwen on every platform; that parser is not
        // ported, so any Safari cookie file (or an unreadable path to it) falls back.
        let safari = home
            .join("Library/Containers/com.apple.Safari/Data/Library")
            .join("Cookies/Cookies.binarycookies");
        match std::fs::symlink_metadata(&safari) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            _ => return proof_rejected(),
        }
        // Native browser extraction and private capsules have their own policy/format.
        for path in children(&ccs.join("account-usage"), false, 512) {
            if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n == "qwen-console-session.json"
                    || matches(r"\Aqwen-console-session-[a-f0-9]{8}\.json\z", n)
            }) {
                return proof_rejected();
            }
        }
    }
    let aliases: &[&str] = match provider {
        "kimi-code" => &["kimi-for-coding", "kimi-code"],
        "zai" => &["zai", "zai-coding-plan"],
        "qwen" => &["alibaba-token-plan"],
        "opencode-go" => &["opencode-go"],
        // A table row with no reader here (DESIGN-M3-COMPLETE 2.4): decline, never another's.
        _ => return proof_rejected(),
    };
    let auth = read(
        &home.join(".local/share/opencode/auth.json"),
        DESKTOP_CAP,
        true,
    )
    .unwrap_or(Value::Null);
    let mut result = Vec::new();
    for alias in aliases {
        let item = &auth[alias];
        if item["type"] == "api"
            && let Some(access) = item["key"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 32768 && !s.contains('\0'))
            && let Some(candidate) = plan_candidate(provider, &format!("opencode:{alias}"), access)
        {
            result.push(candidate);
        }
    }
    {
        let names: &[&str] = match provider {
            "kimi-code" => &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
            "zai" => &["ZAI_API_KEY", "ZHIPU_API_KEY"],
            "qwen" => &["BAILIAN_TOKEN_PLAN_API_KEY"],
            _ => &["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"],
        };
        for name in names {
            if let Ok(access) = std::env::var(name)
                && !access.is_empty()
                && access.len() <= 32768
                && !access.contains('\0')
            {
                if !proof::ascii(&access) {
                    continue;
                }
                if let Some(candidate) =
                    plan_candidate(provider, &format!("environment:{name}"), &access)
                {
                    result.push(candidate);
                }
            } else if std::env::var_os(name).is_some_and(|v| v.to_str().is_none()) {
                proof::reject();
            }
        }
    }
    if provider != "qwen" {
        result.extend(keys(provider, ccs));
    }
    result.truncate(128);
    result
}
fn proof_rejected() -> Vec<Value> {
    proof::reject();
    vec![]
}
fn public_candidate(value: Value) -> Option<Value> {
    let provider = match value["identity"]["kind"].as_str()? {
        "codex-principal-v1" => "codex",
        "claude-account-org-v1" => "claude",
        kind => kind,
    };
    // Existing additional inventories have provider-specific identity kinds.
    let provider = provider.strip_suffix("-credential").unwrap_or(provider);
    dispatch::candidate(
        &value,
        if provider == "qwen-session" {
            "qwen"
        } else if provider == "muse-native" {
            "muse"
        } else {
            provider
        },
        false,
    )
}

fn collect(args: &Value) -> Value {
    let Some(obj) = args.as_object().filter(|o| {
        o.keys().all(|k| {
            [
                "version",
                "action",
                "provider",
                "homeDir",
                "ccsDir",
                "claudeProfile",
            ]
            .contains(&k.as_str())
        })
    }) else {
        return proof::fallback();
    };
    if !proof::proven(args) || args["version"] != 1 || args["action"] != "inventory" {
        return proof::fallback();
    }
    // Every provider row has a failover inventory (config/tables/providers.json); an id the
    // table does not list delegates, and so does a row with no reader below.
    let Some(provider) = args["provider"]
        .as_str()
        .filter(|p| aac_providers::provider(p).is_some())
    else {
        return proof::fallback();
    };
    // No native keychain/DPAPI/platform-variant store has been qualified here.
    if !cfg!(target_os = "linux") || obj.contains_key("claudeProfile") {
        return proof::fallback();
    }
    // The production request omits homeDir: the environment's home passes the explicit rule.
    let default = std::env::var_os("HOME").unwrap_or_default();
    let Some(default) = default.to_str() else {
        return proof::fallback();
    };
    let Some(home) = scope(obj.get("homeDir"), default).filter(|p| p.is_absolute()) else {
        return proof::fallback();
    };
    let ccs_default = home.join(".ccs");
    let Some(ccs) = scope(obj.get("ccsDir"), &ccs_default.to_string_lossy()) else {
        return proof::fallback();
    };
    if !proof::ascii(&home.to_string_lossy()) || !proof::ascii(&ccs.to_string_lossy()) {
        return proof::fallback();
    }
    let values = if ["codex", "claude"].contains(&provider) {
        core_candidates(provider, &home, &ccs)
    } else if ["antigravity", "opencode-wallet"].contains(&provider) {
        return proof::fallback();
    } else {
        additional(provider, &home, &ccs)
    };
    let candidates: Vec<_> = values
        .into_iter()
        .take(128)
        .filter_map(public_candidate)
        .collect();
    let result = json!({"version":1,"action":"inventory","candidates":candidates});
    if serde_json::to_vec(&result).is_ok_and(|b| b.len() <= 512 * 1024) {
        result
    } else {
        proof::fallback()
    }
}
/// Integration hook for U1's Claude and U3's provider adapters. Called on the agent's
/// blocking worker; credentials remain in the adapter's private inventory rows.
pub(crate) fn run_with_adapter(args: &Value, mut adapter: impl Adapter) -> Value {
    let Ok(clock) = aac_core::UtcMs::now() else {
        return dispatch::failure("unavailable", &Value::Null, &Value::Null);
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return dispatch::failure("unavailable", &Value::Null, &Value::Null);
    };
    runtime.block_on(dispatch::execute(args, &mut adapter, clock.get() as i64))
}
pub(crate) fn run(args: &Value) -> Result<Value, Failure> {
    if args["unprovenRawRequest"] == true {
        return Ok(unproven(args));
    }
    // Claude inventory and collection are native on every platform (U1): never Python.
    if args["provider"] == "claude" {
        return Ok(run_with_adapter(args, claude::Claude));
    }
    if args["action"] == "collect" {
        return Ok(core::run(args));
    }
    Ok(proof::scope(|| collect(args)))
}

#[cfg(test)]
mod table_tests {
    //! Failover's providers are the shared table's rows. A row with no reader here declines to
    //! Python: it never falls through to another provider's credentials (DESIGN-M3-COMPLETE 2.4).
    use super::*;

    fn inventory(provider: &str, home: &Path) -> Value {
        run(&json!({
            "version": 1,
            "action": "inventory",
            "provider": provider,
            "homeDir": home.to_string_lossy(),
            "ccsDir": home.join(".ccs").to_string_lossy(),
        }))
        .expect("inventory")
    }

    #[test]
    fn synthetic_failover_row_declines_instead_of_reading_opencode_go() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let home = tempfile::tempdir().expect("home");
        let store = home.path().join(".local/share/opencode");
        fs::create_dir_all(&store).expect("store");
        fs::create_dir_all(home.path().join(".ccs")).expect("ccs");
        fs::write(
            store.join("auth.json"),
            r#"{"opencode-go":{"type":"api","key":"sk-fixture-opencode-go"}}"#,
        )
        .expect("auth");
        let control = inventory("opencode-go", home.path());
        assert!(
            control["candidates"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "the fixture holds an OpenCode Go key: {control}"
        );
        assert_eq!(inventory("zz-synthetic", home.path()), proof::fallback());
        assert_eq!(
            inventory("retired-provider", home.path()),
            proof::fallback()
        );
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;
    #[test]
    fn stale_collect_request_is_source_changed_without_provider_io() {
        let home = tempfile::tempdir().expect("home");
        let candidate = json!({"locator":"a".repeat(64),"identity":{"kind":"codex-principal-v1","key":"b".repeat(64)},"revision":"c".repeat(64),"email":null,"plan":null,"selectors":["discover"]});
        let args = json!({"version":1,"action":"collect","provider":"codex","homeDir":home.path(),"ccsDir":home.path().join(".ccs"),"candidate":candidate});
        assert_eq!(run(&args).expect("run")["failureCode"], "source_changed");
    }
}

#[cfg(test)]
mod inventory_parity_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn core_inventory_matches_python_and_never_publishes_private_rows() {
        let home = tempfile::tempdir().expect("synthetic home");
        let claims = json!({"email":"native@example.test","https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account","chatgpt_user_id":"fixture-user","chatgpt_plan_type":"plus"},"sub":"fixture-subject","iss":"fixture-issuer"});
        let encode = |value: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(value).expect("JWT"))
        };
        let id_token = format!(
            "{}.{}.c2ln",
            encode(&json!({"alg":"none"})),
            encode(&claims)
        );
        let auth = json!({"tokens":{"id_token":id_token,"access_token":"private-fixture-access","account_id":"fixture-account"}});
        for root in [
            home.path().join(".codex"),
            home.path().join(".ccs/codex-instances/saved"),
            home.path().join(".codex-t3/native"),
        ] {
            fs::create_dir_all(&root).expect("root");
            fs::write(
                root.join("auth.json"),
                serde_json::to_vec(&auth).expect("auth"),
            )
            .expect("write");
        }
        let request = json!({"version":1,"action":"inventory","provider":"codex","homeDir":home.path(),"ccsDir":home.path().join(".ccs")});
        let native = run(&request).expect("native");
        let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/account-usage");
        let mut child=std::process::Command::new("python3").arg("-c").arg("import sys,json;sys.path.insert(0,sys.argv[1]);import failover_usage;print(json.dumps(failover_usage.run(json.load(sys.stdin))))").arg(scripts).env("HOME",home.path()).env("CCS_HOME",home.path().join(".ccs")).env("PYTHONDONTWRITEBYTECODE","1").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().expect("Python oracle");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(&serde_json::to_vec(&request).expect("request"))
            .expect("input");
        let output = child.wait_with_output().expect("oracle");
        assert!(output.status.success());
        let expected: Value = serde_json::from_slice(&output.stdout).expect("public oracle");
        assert_eq!(native, expected);
        assert_eq!(
            native["candidates"].as_array().expect("candidates").len(),
            3
        );
        assert!(!native.to_string().contains("private-fixture-access"));
    }
}
