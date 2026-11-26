//! Claude Desktop and Claude Code saved logins, read in memory as the signed-in OS user
//! (`claude_usage.py`: profile paths, `decrypt_cache`, `eligible_entries`, Code logins).
//! Windows unwraps the profile's os_crypt key with DPAPI (no UI) and opens the AES-GCM token
//! cache; macOS reads `Claude Safe Storage` from the login Keychain with interaction disabled.
//! Nothing here refreshes, writes or prints a credential.
use super::Unavailable;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const MAX_CAPSULE_BYTES: usize = 65536;
pub(crate) const UUID: &str =
    r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
static UUID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\A{UUID}\z")).expect("uuid"));
static PROFILE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9_-]{0,63}\z").expect("id"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[^\s@]+@[^\s@]+\.[^\s@]+\z").expect("email"));
static CACHE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\Aacct:({UUID})\|({UUID}):({UUID}):https://api\.anthropic\.com:([A-Za-z0-9_: -]+)\z"
    ))
    .expect("cache key")
});
static SCOPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9_:-]+\z").expect("scope"));

pub(crate) fn is_uuid(v: &str) -> bool {
    UUID_RE.is_match(v)
}
pub(crate) fn valid_profile_id(v: &Value) -> bool {
    v.as_str().is_some_and(|s| PROFILE_ID.is_match(s))
}
/// `resolve_expected_email`: the manifest email this call is bound to.
pub(crate) fn expected_email(v: &Value) -> Option<String> {
    let candidate = match v {
        Value::Null => std::env::var("CCS_CLAUDE_PROFILE_EMAIL").ok()?,
        Value::String(s) => s.clone(),
        _ => return None,
    };
    (candidate.chars().count() <= 254 && EMAIL.is_match(&candidate)).then_some(candidate)
}
pub(crate) fn token_ok(token: &str) -> bool {
    !token.is_empty() && token.chars().count() <= 32768 && !token.chars().any(|c| (c as u32) < 32)
}

/// `json.dumps(value, sort_keys=..., separators=(",", ":"))` with `ensure_ascii`.
pub(crate) fn py_dumps(value: &Value, sort: bool) -> String {
    let mut out = String::new();
    dump(value, sort, &mut out);
    out
}
fn dump(value: &Value, sort: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string());
            } else {
                out.push_str(&aac_num::py::json_dumps(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => {
            out.push('"');
            for unit in s.encode_utf16() {
                match unit {
                    0x22 => out.push_str("\\\""),
                    0x5c => out.push_str("\\\\"),
                    0x0a => out.push_str("\\n"),
                    0x0d => out.push_str("\\r"),
                    0x09 => out.push_str("\\t"),
                    0x08 => out.push_str("\\b"),
                    0x0c => out.push_str("\\f"),
                    0x20..=0x7e => out.push(unit as u8 as char),
                    _ => out.push_str(&format!("\\u{unit:04x}")),
                }
            }
            out.push('"');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump(v, sort, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            let mut keys: Vec<&String> = o.keys().collect();
            if sort {
                keys.sort();
            }
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                dump(&Value::String(k.clone()), sort, out);
                out.push(':');
                dump(&o[k], sort, out);
            }
            out.push('}');
        }
    }
}
pub(crate) fn sha256(text: &str) -> String {
    aac_core::sha256_hex(text.as_bytes())
}
/// `reset_login_binding(account, cache)`.
pub(crate) fn login_binding(account: &Value, cache: &Value) -> String {
    sha256(&py_dumps(&json!([account, cache]), true))
}

/// `read_json`: one object of at most 1 MiB (UTF-8, optional BOM). Missing is unavailable.
pub(crate) fn read_json(path: &Path) -> Result<Map<String, Value>, Unavailable> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| Unavailable::new("unavailable"))?;
    let mut bytes = vec![];
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unavailable::new("unavailable"))?;
    if bytes.len() > MAX_BYTES {
        return Err(Unavailable::new("unavailable"));
    }
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|_| Unavailable::new("unavailable"))?;
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => Ok(o),
        _ => Err(Unavailable::new("unavailable")),
    }
}
fn decode_base64(v: Option<&Value>) -> Result<Vec<u8>, Unavailable> {
    use base64::Engine;
    let s = v
        .and_then(Value::as_str)
        .filter(|s| s.len() <= MAX_BYTES)
        .ok_or(Unavailable::new("unavailable"))?;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| Unavailable::new("unavailable"))
}

// ---- profile paths ---------------------------------------------------------------------------

fn norm_case(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}
fn windows_legacy(home: &Path) -> PathBuf {
    home.join("AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")
}
/// `resolve_windows_profile_root`: the Store default's relocation, never another profile.
pub(crate) fn resolve_windows_profile_root(profile: &Path, home: &Path) -> PathBuf {
    let current = home.join("AppData/Local/Claude-Data/claude_pzs8sxrjxfjjc");
    if norm_case(&crate::files::lexical(profile))
        != norm_case(&crate::files::lexical(&windows_legacy(home)))
    {
        return profile.to_path_buf();
    }
    match std::fs::symlink_metadata(&current) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(20) => {
            profile.to_path_buf()
        }
        _ => current,
    }
}
pub(crate) fn windows_profile_path(id: &str, home: &Path, is_default: bool) -> PathBuf {
    if is_default {
        resolve_windows_profile_root(&windows_legacy(home), home)
    } else {
        home.join("AppData")
            .join("Roaming")
            .join(format!("Claude-{id}"))
    }
}
fn reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}
fn plain_path(p: PathBuf) -> PathBuf {
    let text = p.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}
/// `_canonical_profile_directory`: no share, link or reparse point on the way; a missing
/// directory stays as written (it can still be signed out), anything else unreadable fails.
pub(crate) fn canonical_profile_directory(value: &Path) -> Result<PathBuf, Unavailable> {
    let text = value.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err(Unavailable::new("unavailable"));
    }
    if !value.is_absolute() {
        return Ok(value.to_path_buf());
    }
    let root = crate::files::lexical(value);
    let mut chain: Vec<&Path> = root.ancestors().collect();
    chain.reverse();
    for dir in chain {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() && !reparse(&meta) => {}
            Ok(_) => return Err(Unavailable::new("unavailable")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound || e.raw_os_error() == Some(20) => {
                return Ok(value.to_path_buf());
            }
            Err(_) => return Err(Unavailable::new("unavailable")),
        }
    }
    std::fs::canonicalize(&root)
        .map(plain_path)
        .map_err(|_| Unavailable::new("unavailable"))
}
fn mac_profile_path(id: &str, home: &Path, is_default: bool) -> PathBuf {
    home.join("Library/Application Support")
        .join(if is_default {
            "Claude".to_owned()
        } else {
            format!("Claude-{id}")
        })
}
fn profile_dir_ok(dir: &str) -> bool {
    !dir.is_empty() && dir.chars().count() <= 4096 && !dir.contains('\0')
}
/// `resolve_profile_path(profile_id, profile_dir, home, is_default)` for this host's platform.
pub(crate) fn resolve_profile_path(
    id: &Value,
    dir: &Value,
    home: &Path,
    is_default: bool,
) -> Result<PathBuf, Unavailable> {
    let unavailable = || Unavailable::new("unavailable");
    if cfg!(target_os = "macos") {
        if !dir.is_null() {
            let d = dir
                .as_str()
                .filter(|d| profile_dir_ok(d))
                .ok_or_else(unavailable)?;
            if !Path::new(d).is_absolute() {
                return Err(unavailable());
            }
            return canonical_profile_directory(Path::new(d));
        }
        let id = id
            .as_str()
            .filter(|_| valid_profile_id(id))
            .ok_or_else(unavailable)?;
        return canonical_profile_directory(&mac_profile_path(id, home, is_default));
    }
    if !dir.is_null() {
        let d = dir
            .as_str()
            .filter(|d| profile_dir_ok(d))
            .ok_or_else(unavailable)?;
        if !windows_absolute(d) {
            return Err(unavailable());
        }
        let root = canonical_profile_directory(Path::new(d))?;
        let resolved = resolve_windows_profile_root(&root, home);
        return if resolved != root {
            canonical_profile_directory(&resolved)
        } else {
            Ok(root)
        };
    }
    let id = id
        .as_str()
        .filter(|_| valid_profile_id(id))
        .ok_or_else(unavailable)?;
    canonical_profile_directory(&windows_profile_path(id, home, is_default))
}
/// `ntpath.isabs`: a drive with a separator, or a rooted share.
fn windows_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
        || p.starts_with("\\\\")
        || p.starts_with("//")
        || (cfg!(not(windows)) && p.starts_with('/'))
}

// ---- crypto ----------------------------------------------------------------------------------

/// AES-256-GCM (`AESGCM(key).decrypt(nonce, ciphertext+tag, None)`).
pub(crate) fn aes_gcm_open(key: &[u8], nonce: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut buffer = sealed.to_vec();
    let plain = key.open_in_place(nonce, Aad::empty(), &mut buffer).ok()?;
    Some(plain.to_vec())
}
/// Electron safeStorage on macOS: PBKDF2-HMAC-SHA1(secret, "saltysalt", 1003) to 16 bytes.
pub(crate) fn safe_storage_key(secret: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        std::num::NonZeroU32::new(1003).expect("iterations"),
        b"saltysalt",
        secret,
        &mut key,
    );
    key
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn unprotect(blob: &[u8], expected: Option<usize>) -> Option<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(blob.len()).ok()?,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input points at `blob` for the call; output is allocated by the API and freed below.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return None;
    }
    let len = output.cbData as usize;
    // SAFETY: the API returned `len` readable bytes at pbData.
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, len) }.to_vec();
    // SAFETY: pbData was allocated by CryptUnprotectData with LocalAlloc.
    unsafe { LocalFree(output.pbData as _) };
    (expected.is_none_or(|n| n == len) && (1..=MAX_CAPSULE_BYTES).contains(&len)).then_some(bytes)
}
#[cfg(not(windows))]
pub(crate) fn unprotect(_: &[u8], _: Option<usize>) -> Option<Vec<u8>> {
    None
}

/// `profile_key`: the Local State os_crypt key, unwrapped as the same OS user.
fn profile_key(root: &Path) -> Result<Vec<u8>, Unavailable> {
    let state = read_json(&root.join("Local State"))?;
    let wrapped = decode_base64(
        state
            .get("os_crypt")
            .and_then(Value::as_object)
            .ok_or(Unavailable::new("unavailable"))?
            .get("encrypted_key"),
    )?;
    if !wrapped.starts_with(b"DPAPI") || wrapped.len() <= 5 {
        return Err(Unavailable::new("unavailable"));
    }
    unprotect(&wrapped[5..], Some(32)).ok_or(Unavailable::new("unavailable"))
}

/// `decrypt_cache`: the profile's last account and its decrypted OAuth token cache.
pub(crate) fn decrypt_cache(root: &Path) -> Result<(String, Map<String, Value>), Unavailable> {
    let config = read_json(&root.join("config.json"))?;
    let account = config
        .get("lastKnownAccountUuid")
        .and_then(Value::as_str)
        .filter(|a| is_uuid(a))
        .ok_or(Unavailable::new("needs_sign_in"))?
        .to_owned();
    let blob = decode_base64(config.get("oauth:tokenCacheV2"))?;
    let decoded = if cfg!(target_os = "macos") {
        if !blob.starts_with(b"v10") || blob.len() < 19 || (blob.len() - 3) % 16 != 0 {
            return Err(Unavailable::new("unavailable"));
        }
        let secret = super::keychain::read("Claude Safe Storage", "Claude Key", false)
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(Unavailable::new("unavailable"))?;
        let key = safe_storage_key(&secret);
        super::keychain::aes128_cbc_decrypt(&key, &[b' '; 16], &blob[3..])
            .ok_or(Unavailable::new("unavailable"))?
    } else {
        let key = profile_key(root)?;
        if !blob.starts_with(b"v10") || blob.len() < 31 {
            return Err(Unavailable::new("unavailable"));
        }
        aes_gcm_open(&key, &blob[3..15], &blob[15..]).ok_or(Unavailable::new("unavailable"))?
    };
    if decoded.len() > MAX_BYTES {
        return Err(Unavailable::new("unavailable"));
    }
    match serde_json::from_slice::<Value>(&decoded) {
        Ok(Value::Object(cache)) if cache.len() <= 128 => Ok((account, cache)),
        _ => Err(Unavailable::new("unavailable")),
    }
}

/// One usable saved OAuth login: expiry (ms), [account, client, organization], token.
#[derive(Clone)]
pub(crate) struct Entry {
    pub expires: f64,
    pub identities: [String; 3],
    pub token: String,
}
/// `eligible_entries(account, cache, allow_expired, organization)`, newest four first.
pub(crate) fn eligible_entries(
    account: &str,
    cache: &Map<String, Value>,
    allow_expired: bool,
    organization: Option<&str>,
    now_ms: f64,
) -> Vec<Entry> {
    let mut found = vec![];
    for (key, entry) in cache {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if key.chars().count() > 4096 {
            continue;
        }
        let Some(m) = CACHE_KEY.captures(key) else {
            continue;
        };
        let identities = [m[1].to_owned(), m[2].to_owned(), m[3].to_owned()];
        let scopes: Vec<&str> = m[4].split(' ').collect();
        let token = entry.get("token").and_then(Value::as_str);
        let expires = entry
            .get("expiresAt")
            .and_then(super::normalize::number)
            .and_then(serde_json::Number::as_f64);
        let (Some(token), Some(expires)) = (token, expires) else {
            continue;
        };
        if identities[0] != account
            || !scopes.contains(&"user:profile")
            || !scopes.iter().all(|s| SCOPE.is_match(s))
            || !token_ok(token)
            || (!allow_expired && expires <= now_ms + 30000.0)
            || organization.is_some_and(|o| identities[2] != o)
        {
            continue;
        }
        found.push(Entry {
            expires,
            identities,
            token: token.to_owned(),
        });
    }
    found.sort_by(|a, b| b.expires.total_cmp(&a.expires));
    found.truncate(4);
    found
}

// ---- Claude Code logins ---------------------------------------------------------------------

const MAX_CODE_LOGIN_ROOTS: usize = 32;
pub(crate) fn regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && !reparse(&m))
}
/// `_code_login_roots`: ~/.claude, then saved instances, then T3's account folders.
pub(crate) fn code_login_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.join(".claude")];
    for base in [home.join(".ccs").join("instances"), home.join(".claude-t3")] {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .take(256)
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| PROFILE_ID.is_match(n))
            .collect();
        names.sort();
        roots.extend(names.into_iter().map(|n| base.join(n)));
    }
    roots.truncate(MAX_CODE_LOGIN_ROOTS);
    roots
}
/// `_mac_code_keychain_service`.
pub(crate) fn code_keychain_service(root: &Path, home: &Path) -> String {
    use unicode_normalization::UnicodeNormalization;
    if root == home.join(".claude") {
        return "Claude Code-credentials".into();
    }
    let text: String = root.to_string_lossy().nfc().collect();
    format!("Claude Code-credentials-{}", &sha256(&text)[..8])
}
fn code_login_oauth(root: &Path, home: &Path) -> Result<Option<Value>, Unavailable> {
    if cfg!(target_os = "macos") {
        match super::keychain::read(
            &code_keychain_service(root, home),
            &super::keychain::account(),
            true,
        ) {
            Ok(raw) if !raw.is_empty() => {
                let text = std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw));
                return Ok(text
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(t).ok())
                    .and_then(|v| v.get("claudeAiOauth").cloned()));
            }
            Err(code) if code != super::keychain::NOT_FOUND => return Ok(None),
            _ => {}
        }
    }
    let credentials = root.join(".credentials.json");
    if !regular_file(&credentials) {
        return Ok(None);
    }
    Ok(read_json(&credentials)?.get("claudeAiOauth").cloned())
}
/// One saved Claude Code login for the expected email.
#[derive(Clone)]
pub(crate) struct CodeLogin {
    pub account: String,
    pub organization: String,
    pub token: String,
    pub binding: String,
}
/// `code_reset_logins`: unexpired Code logins whose saved email is the expected one.
pub(crate) fn code_reset_logins(expected: &Value, home: &Path, now_ms: f64) -> Vec<CodeLogin> {
    let Some(expected) = expected_email(expected) else {
        return vec![];
    };
    if !cfg!(any(windows, target_os = "macos")) {
        return vec![];
    }
    let want = super::normalize::py_strip(&expected).to_lowercase();
    let mut found = vec![];
    for root in code_login_roots(home) {
        let metadata = if root == home.join(".claude") {
            home.join(".claude.json")
        } else {
            root.join(".claude.json")
        };
        if !regular_file(&metadata) {
            continue;
        }
        let Ok(saved) = read_json(&metadata) else {
            continue;
        };
        let Ok(Some(oauth)) = code_login_oauth(&root, home) else {
            continue;
        };
        let (Some(saved), Some(o)) = (
            saved.get("oauthAccount").and_then(Value::as_object),
            oauth.as_object(),
        ) else {
            continue;
        };
        let text =
            |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).map(str::to_owned);
        let (Some(account), Some(organization), Some(address), Some(token)) = (
            text(saved, "accountUuid"),
            text(saved, "organizationUuid"),
            text(saved, "emailAddress"),
            text(o, "accessToken"),
        ) else {
            continue;
        };
        let expires = o
            .get("expiresAt")
            .and_then(super::normalize::number)
            .and_then(serde_json::Number::as_f64);
        let scopes_ok = o
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|s| s.contains(&json!("user:profile")));
        if super::normalize::py_strip(&address).to_lowercase() != want
            || !is_uuid(&account)
            || !is_uuid(&organization)
            || !token_ok(&token)
            || expires.is_none_or(|e| e <= now_ms + 30000.0)
            || !scopes_ok
        {
            continue;
        }
        let binding = login_binding(
            &json!(account),
            &json!({"claude-code": [root.to_string_lossy(), organization, oauth]}),
        );
        found.push(CodeLogin {
            account,
            organization,
            token,
            binding,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn py_dumps_matches_json_dumps_sorted_ascii() {
        let v = json!({"b":[1,2.5,"é\n\u{1f600}"],"a":{"z":null,"y":true}});
        assert_eq!(
            py_dumps(&v, true),
            r#"{"a":{"y":true,"z":null},"b":[1,2.5,"\u00e9\n\ud83d\ude00"]}"#
        );
    }

    #[test]
    fn eligible_entries_need_profile_scope_and_a_live_expiry() {
        let a = "11111111-1111-1111-1111-111111111111";
        let c = "22222222-2222-2222-2222-222222222222";
        let o = "33333333-3333-3333-3333-333333333333";
        let key = |scopes: &str| format!("acct:{a}|{c}:{o}:https://api.anthropic.com:{scopes}");
        let cache = json!({
            key("user:profile user:inference"): {"token":"t1","expiresAt": 2_000_000},
            key("user:inference"): {"token":"t2","expiresAt": 3_000_000},
            key("user:profile"): {"token":"t3","expiresAt": 1_000_010},
            format!("x{}", key("user:profile")): {"token":"t4","expiresAt": 9_000_000},
        });
        let cache = cache.as_object().unwrap();
        let found = eligible_entries(a, cache, false, None, 1_000_000.0);
        assert_eq!(
            found.iter().map(|e| e.token.as_str()).collect::<Vec<_>>(),
            ["t1"]
        );
        let all = eligible_entries(a, cache, true, None, 1_000_000.0);
        assert_eq!(
            all.iter().map(|e| e.token.as_str()).collect::<Vec<_>>(),
            ["t1", "t3"]
        );
        assert!(eligible_entries(a, cache, true, Some(c), 0.0).is_empty());
    }

    #[test]
    fn aes_gcm_and_safe_storage_key_are_the_electron_primitives() {
        use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        let sealing = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).unwrap());
        let mut data = b"{\"a\":1}".to_vec();
        sealing
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut data)
            .unwrap();
        assert_eq!(aes_gcm_open(&key, &nonce, &data).unwrap(), b"{\"a\":1}");
        data[0] ^= 1;
        assert!(aes_gcm_open(&key, &nonce, &data).is_none());
        // hashlib.pbkdf2_hmac("sha1", b"secret", b"saltysalt", 1003, dklen=16).hex()
        let hex: String = safe_storage_key(b"secret")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, PBKDF2_SECRET);
    }
    const PBKDF2_SECRET: &str = include_str!("../../tests/fixtures/claude-pbkdf2.txt");

    /// A synthetic Windows profile: DPAPI-wrapped os_crypt key, AES-GCM token cache.
    #[cfg(windows)]
    #[allow(unsafe_code)]
    #[test]
    fn windows_profile_cache_decrypts_as_the_same_user() {
        use base64::Engine;
        use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::Cryptography::{
                CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
            },
        };
        let key = [5u8; 32];
        let input = CRYPT_INTEGER_BLOB {
            cbData: 32,
            pbData: key.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: test-only DPAPI call with valid blobs; output freed below.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        };
        assert_ne!(ok, 0);
        // SAFETY: the API returned cbData bytes at pbData.
        let wrapped =
            unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        // SAFETY: allocated by CryptProtectData.
        unsafe { LocalFree(out.pbData as _) };
        let b64 = base64::engine::general_purpose::STANDARD;
        let account = "11111111-1111-1111-1111-111111111111";
        let entry_key = format!(
            "acct:{account}|22222222-2222-2222-2222-222222222222:33333333-3333-3333-3333-333333333333:https://api.anthropic.com:user:profile"
        );
        let mut cache = serde_json::to_vec(
            &json!({entry_key: {"token":"t","expiresAt": 9_000_000_000_000_i64}}),
        )
        .unwrap();
        let nonce = [1u8; 12];
        LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key).unwrap())
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut cache,
            )
            .unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&cache);
        let dir = tempfile::tempdir().unwrap();
        let mut dpapi = b"DPAPI".to_vec();
        dpapi.extend_from_slice(&wrapped);
        std::fs::write(
            dir.path().join("Local State"),
            json!({"os_crypt":{"encrypted_key": b64.encode(dpapi)}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            json!({"lastKnownAccountUuid": account, "oauth:tokenCacheV2": b64.encode(blob)})
                .to_string(),
        )
        .unwrap();
        let (found, cache) = decrypt_cache(dir.path()).unwrap();
        assert_eq!(found, account);
        let entries = eligible_entries(&found, &cache, false, None, 0.0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].token, "t");
    }
}
