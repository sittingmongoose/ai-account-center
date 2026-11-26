//! Plan readers. Only an injected client can perform network IO; unproven inputs delegate.
mod parse;
use crate::{files, proof};
use aac_core::{Cause, Failure, UtcMs};
use aac_files::read::{Ancestors, ReadPolicy};
// Plan-common's retry policy, including its invalid-header default, is the table's
// `planRetryAfter`; the plan providers and their labels are its `quota: true` rows.
use aac_providers::{PLAN_RETRY_AFTER, ProviderId};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const CAP: usize = 1_048_576;
struct Response {
    status: u16,
    retry: Option<String>,
    body: Vec<u8>,
}
trait Http {
    async fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Response, ()>;
}
struct Client {
    client: reqwest::Client,
    deadline: Instant,
    endpoint: Option<String>,
}
impl Http for Client {
    async fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Response, ()> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(());
        }
        let destination = self.endpoint.as_deref().unwrap_or(url);
        let request = if let Some(body) = body {
            self.client.post(destination).body(body.to_vec())
        } else {
            self.client.get(destination)
        };
        let mut request = request.timeout(remaining.min(Duration::from_secs(4)));
        if !headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("accept"))
        {
            request = request.header("Accept", "application/json");
        }
        if !headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("user-agent"))
        {
            request = request.header("User-Agent", "CCS-Accounts/1.0");
        }
        if self.endpoint.is_some() {
            request = request.header("x-aac-fixture-url", url);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut response = request.send().await.map_err(|_| ())?;
        let status = response.status().as_u16();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|h| h.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        // Error bodies are deliberately never read, decoded or included in diagnostics.
        if status < 400 {
            while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
                let take = chunk.len().min(CAP + 1 - body.len());
                body.extend_from_slice(&chunk[..take]);
                if body.len() > CAP {
                    break;
                }
            }
        }
        Ok(Response {
            status,
            retry,
            body,
        })
    }
}
fn bounded_json(bytes: &[u8], cap: usize) -> Option<Value> {
    (bytes.len() <= cap)
        .then(|| serde_json::from_slice::<Value>(bytes).ok())
        .flatten()
}
/// Python's load_json: parse errors/absence mean no record; Unicode and fractional values are data,
/// not poison, because credentials and provider payloads are validated before publication.
/// A plan credential document below `home`, through the shared hardened read. Absent reads as
/// absent; a link (final or below the home), FIFO, other non-file, oversized or failed read is
/// unproved and delegates, so Python's own reader decides what that store holds.
fn load(home: &Path, path: &Path) -> Option<Value> {
    let policy = ReadPolicy::capped(CAP as u64).ancestors(Ancestors::NoLinkBelow(home));
    match files::read_with(path, &policy) {
        files::Regular::Missing => None,
        files::Regular::Read(bytes, _) => bounded_json(&bytes, CAP),
        _ => {
            proof::reject();
            None
        }
    }
}
fn key_credential(provider: &str, home: &Path, selector: &Value) -> Value {
    #[cfg(not(unix))]
    {
        let _ = (provider, home, selector);
        proof::fallback()
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if provider == "qwen" {
            return proof::fallback();
        }
        let Some(id) = selector["keyId"].as_str().filter(|s| {
            s.len() == 8
                && s.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        }) else {
            return proof::fallback();
        };
        let directory = home.join(".ccs/account-usage/keys");
        let Ok(info) = std::fs::symlink_metadata(&directory) else {
            return Value::Null;
        };
        if !info.is_dir() || info.mode() & 0o077 != 0 {
            return Value::Null;
        }
        let path = directory.join(format!("{provider}-{id}.json"));
        // Any record that is not one private ordinary file reads as unreadable, as before.
        let private = ReadPolicy::capped(4096).mode(aac_files::read::Mode::Private);
        let files::Regular::Read(bytes, _) = files::read_with(&path, &private) else {
            return Value::Null;
        };
        let Some(record) = bounded_json(&bytes, 4096) else {
            return Value::Null;
        };
        if record["version"].is_boolean() {
            return proof::fallback();
        }
        let Some(obj) = record.as_object() else {
            return Value::Null;
        };
        let Some(secret) = record["secret"].as_str().filter(|s| {
            (8..=512).contains(&s.len()) && s.bytes().all(|c| (0x21..=0x7e).contains(&c))
        }) else {
            return Value::Null;
        };
        if record["version"] != 1
            || record["provider"] != provider
            || record["keyId"] != id
            || obj.keys().any(|k| {
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
        {
            return Value::Null;
        }
        if let Some(f) = record.get("fingerprint")
            && f != &json!(format!("sha256:{}", &aac_core::sha256_hex(secret)[..16]))
        {
            return Value::Null;
        }
        json!({"secret":secret,"source":"Dashboard key"})
    }
}
fn email_address(value: Option<&str>) -> Option<&str> {
    value.filter(|s| {
        if s.len() > 254 || s.matches('@').count() != 1 {
            return false;
        }
        let mut parts = s.split('@');
        let local = parts.next().unwrap_or("");
        let domain = parts.next().unwrap_or("");
        let clean = |x: &str| {
            !x.is_empty()
                && !x
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b < 0x20 || b == b'@')
        };
        clean(local) && clean(domain) && domain.contains('.')
    })
}
fn omp_credentials(provider: &str, home: &Path, out: &mut Vec<Value>) {
    let db = home.join(".omp/agent/agent.db");
    if !db.is_file() {
        return;
    }
    let omp_provider = if provider == "qwen" {
        "alibaba-token-plan"
    } else {
        provider
    };
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        proof::reject();
        return;
    };
    let _ = connection.execute_batch("PRAGMA query_only=ON");
    let mut statement = match connection.prepare("SELECT credential_type,data FROM auth_credentials WHERE provider=? AND disabled_cause IS NULL ORDER BY id DESC LIMIT 20") {
        Ok(statement) => statement, Err(_) => { proof::reject(); return; }
    };
    let rows = statement.query_map([omp_provider], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let Ok(rows) = rows else {
        proof::reject();
        return;
    };
    for row in rows.take(20) {
        let Ok((kind, contents)) = row else {
            proof::reject();
            continue;
        };
        if contents.len() > CAP {
            continue;
        }
        let data = serde_json::from_str::<Value>(&contents).unwrap_or(Value::Null);
        if !data.is_object() {
            proof::reject();
            continue;
        }
        let secret = data["key"]
            .as_str()
            .filter(|s| valid_secret(s))
            .or_else(|| data["access"].as_str().filter(|s| valid_secret(s)));
        if let Some(secret) = secret {
            let email = email_address(data["email"].as_str());
            let expires = if provider == "kimi-code" && kind == "oauth" {
                match &data["expires"] {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.trim().parse::<f64>().ok(),
                    _ => None,
                }
                .filter(|n| n.is_finite() && *n >= 0.)
            } else {
                None
            };
            out.push(json!({"secret":secret,"source":"OMP","email":email,"expires":expires}));
        }
    }
}
fn read_binary_capped(path: &Path, cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > cap as u64 {
        proof::reject();
        return None;
    }
    let mut bytes = Vec::new();
    if file
        .by_ref()
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > cap
    {
        proof::reject();
        return None;
    }
    Some(bytes)
}
#[derive(Clone)]
struct SafariCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    expiry: f64,
    host_only: bool,
}
fn safari_string(record: &[u8], at: usize) -> Result<String, ()> {
    let start = u32::from_le_bytes(
        record
            .get(at..at + 4)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?,
    ) as usize;
    if start < 56 || start >= record.len() {
        return Err(());
    }
    let end = record[start..]
        .iter()
        .position(|b| *b == 0)
        .map(|n| start + n)
        .ok_or(())?;
    if end - start > 8192 {
        return Err(());
    }
    String::from_utf8(record[start..end].to_vec()).map_err(|_| ())
}
fn four(bytes: &[u8]) -> Option<[u8; 4]> {
    bytes.try_into().ok()
}
fn safari_cookies(contents: &[u8], allowed: &[&str], now: f64) -> Vec<SafariCookie> {
    if contents.len() > 16 * CAP || contents.len() < 8 || &contents[..4] != b"cook" {
        return vec![];
    }
    let pages = u32::from_be_bytes(four(&contents[4..8]).unwrap_or_default()) as usize;
    if pages > 4096 || 8 + pages * 4 > contents.len() {
        return vec![];
    }
    let mut pos = 8 + pages * 4;
    let mut result = vec![];
    let mut total = 0usize;
    for page_index in 0..pages {
        let size = u32::from_be_bytes(
            four(&contents[8 + page_index * 4..12 + page_index * 4]).unwrap_or_default(),
        ) as usize;
        if size < 12 || pos + size > contents.len() {
            return vec![];
        }
        let page = &contents[pos..pos + size];
        pos += size;
        if page[..4] != [0, 0, 1, 0] {
            return vec![];
        }
        let count = u32::from_le_bytes(four(&page[4..8]).unwrap_or_default()) as usize;
        total += count;
        let header_end = 12 + count * 4;
        if count > 10000
            || total > 20000
            || header_end > page.len()
            || page[header_end - 4..header_end] != [0, 0, 0, 0]
        {
            return vec![];
        }
        let mut seen = std::collections::BTreeSet::new();
        for index in 0..count {
            let offset =
                u32::from_le_bytes(four(&page[8 + index * 4..12 + index * 4]).unwrap_or_default())
                    as usize;
            if !seen.insert(offset) || offset < header_end || offset + 56 > page.len() {
                return vec![];
            }
            let record_size =
                u32::from_le_bytes(four(&page[offset..offset + 4]).unwrap_or_default()) as usize;
            if record_size < 56 || offset + record_size > page.len() {
                return vec![];
            }
            let record = &page[offset..offset + record_size];
            let parsed = (|| {
                let raw = safari_string(record, 16)?.to_ascii_lowercase();
                let domain = raw.trim_start_matches('.').to_owned();
                if !allowed.contains(&domain.as_str()) {
                    return Err(());
                }
                let expiry =
                    f64::from_le_bytes(record.get(40..48).ok_or(())?.try_into().map_err(|_| ())?);
                if !expiry.is_finite()
                    || expiry < 0.
                    || (expiry != 0. && expiry + 978307200. <= now)
                {
                    return Err(());
                }
                let name = safari_string(record, 20)?;
                let path = safari_string(record, 24)?;
                let value = safari_string(record, 28)?;
                let valid_name = !name.is_empty()
                    && name.len() <= 256
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
                if !valid_name
                    || value.is_empty()
                    || value.bytes().any(|b| matches!(b, b';' | b'\r' | b'\n' | 0))
                {
                    return Err(());
                }
                if !path.starts_with('/')
                    || !["/tool/user/info.json", "/data/api.json", "/cn-beijing"]
                        .iter()
                        .any(|r| {
                            *r == path
                                || r.starts_with(&(path.trim_end_matches('/').to_owned() + "/"))
                        })
                {
                    return Err(());
                }
                Ok(SafariCookie {
                    name,
                    value,
                    domain,
                    path,
                    expiry,
                    host_only: !raw.starts_with('.'),
                })
            })();
            if let Ok(cookie) = parsed {
                result.push(cookie);
            }
        }
    }
    result
}
fn safari_qwen(home: &Path, out: &mut Vec<Value>) {
    let path =
        home.join("Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies");
    let Some(contents) = read_binary_capped(&path, 16 * CAP) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.);
    for (region, allowed) in [
        (
            "intl",
            vec![
                "qwencloud.com",
                "home.qwencloud.com",
                "cs-data.qwencloud.com",
            ],
        ),
        (
            "cn",
            vec![
                "aliyun.com",
                "console.aliyun.com",
                "bailian.console.aliyun.com",
                "bailian-cs.console.aliyun.com",
            ],
        ),
    ] {
        let mut cookies = safari_cookies(&contents, &allowed, now);
        cookies.sort_by(|a, b| {
            b.domain
                .len()
                .cmp(&a.domain.len())
                .then(b.path.len().cmp(&a.path.len()))
                .then(b.expiry.total_cmp(&a.expiry))
        });
        let header_for = |host: &str, request: &str| {
            let mut selected = std::collections::BTreeMap::new();
            for c in &cookies {
                let domain =
                    host == c.domain || (!c.host_only && host.ends_with(&format!(".{}", c.domain)));
                let path = request == c.path
                    || request.starts_with(&(c.path.trim_end_matches('/').to_owned() + "/"));
                if domain && path {
                    selected.entry(c.name.clone()).or_insert(c.value.clone());
                }
            }
            selected
                .into_iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let (console, gateway, request) = if region == "intl" {
            (
                "home.qwencloud.com",
                "cs-data.qwencloud.com",
                "/tool/user/info.json",
            )
        } else {
            (
                "bailian.console.aliyun.com",
                "bailian-cs.console.aliyun.com",
                "/cn-beijing",
            )
        };
        let header = header_for(console, request);
        let gateway_header = header_for(gateway, "/data/api.json");
        if header.is_empty() || header.len() > 32768 || gateway_header.len() > 32768 {
            continue;
        }
        let base = if region == "intl" {
            "ap-southeast-1"
        } else {
            "cn-beijing"
        };
        out.push(json!({"secret":json!({"cookie":header,"gatewayCookie":gateway_header,"baseUrl":format!("https://token-plan.{base}.maas.aliyuncs.com/compatible-mode/v1")}).to_string(),"source":"Safari console","email":null,"expires":null}));
    }
}
fn valid_secret(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32768 && !s.contains('\0')
}
#[cfg(windows)]
fn windows_unprotect(ciphertext: &[u8]) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};
    #[allow(unsafe_code)]
    unsafe {
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        if CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            1,
            &mut output,
        ) == 0
        {
            return None;
        }
        if output.cbData > 131072 || output.pbData.is_null() {
            LocalFree(output.pbData as _);
            return None;
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        String::from_utf8(bytes).ok()
    }
}
#[cfg(not(windows))]
fn windows_unprotect(_ciphertext: &[u8]) -> Option<String> {
    None
}
fn qwen_capsule(home: &Path, selector: &Value) -> Option<(String, String, Option<String>)> {
    let id = selector["capsuleId"].as_str().unwrap_or("default");
    if !(id == "default"
        || (id.len() == 8
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
    {
        return None;
    }
    let name = if id == "default" {
        "qwen-console-session.json".into()
    } else {
        format!("qwen-console-session-{id}.json")
    };
    let path = home.join(".ccs/account-usage").join(name);
    let capsule = load(home, &path)?;
    if !capsule.is_object() || !matches!(capsule["region"].as_str(), Some("intl" | "cn")) {
        proof::reject();
        return None;
    }
    let v2 = capsule["version"] == 2;
    let encoded = capsule[if v2 { "cookiesDPAPI" } else { "cookieDPAPI" }].as_str()?;
    if encoded.is_empty() || encoded.len() > if v2 { 196608 } else { 65536 } {
        proof::reject();
        return None;
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decrypted = windows_unprotect(&bytes);
    if decrypted.is_none() {
        proof::reject();
        return None;
    }
    let decrypted = decrypted?;
    let (cookie, gateway) = if v2 {
        let packed = serde_json::from_str::<Value>(&decrypted).ok()?;
        if !packed.is_object() || packed["gatewayCookie"].as_str().is_none() {
            proof::reject();
            return None;
        }
        (
            packed["consoleCookie"].as_str().map(str::to_owned),
            Some(
                packed["gatewayCookie"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
        )
    } else {
        (Some(decrypted), None)
    };
    let cookie = cookie?;
    let bad = |s: &str| s.len() > 32768 || s.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0));
    if cookie.is_empty() || bad(&cookie) || gateway.as_deref().is_some_and(bad) {
        proof::reject();
        return None;
    }
    Some((
        cookie,
        capsule["region"].as_str().unwrap_or_default().to_owned(),
        gateway,
    ))
}
fn credential_candidates(
    provider: &str,
    home: &Path,
    selector: &Value,
    isolated: bool,
) -> Vec<Value> {
    if selector["kind"] == "aac-key" {
        let one = key_credential(provider, home, selector);
        return if one.is_null() { vec![] } else { vec![one] };
    }
    if selector
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|k| k != "discover")
    {
        if selector["kind"] == "browser-capsule"
            && provider == "qwen"
            && let Some((cookie, region, gateway)) = qwen_capsule(home, selector)
        {
            let base = if region == "intl" {
                "ap-southeast-1"
            } else {
                "cn-beijing"
            };
            let mut packed = json!({"cookie":cookie,"baseUrl":format!("https://token-plan.{base}.maas.aliyuncs.com/compatible-mode/v1")});
            if let Some(gateway) = gateway {
                packed["gatewayCookie"] = json!(gateway);
            }
            return vec![
                json!({"secret":packed.to_string(),"source":"Brave console","email":null,"expires":null}),
            ];
        }
        return vec![];
    }
    let mut candidates = Vec::new();
    if provider == "qwen" {
        safari_qwen(home, &mut candidates);
    }
    let auth = load(home, &home.join(".local/share/opencode/auth.json"));
    let aliases: &[&str] = match provider {
        "kimi-code" => &["kimi-for-coding", "kimi-code"],
        "zai" => &["zai", "zai-coding-plan"],
        "qwen" => &["alibaba-token-plan"],
        _ => &["opencode-go"],
    };
    for alias in aliases {
        let item = &auth.as_ref().unwrap_or(&Value::Null)[alias];
        if item["type"] == "api" && item["key"].as_str().is_some_and(valid_secret) {
            candidates.push(json!({"secret":item["key"].as_str().unwrap_or_default(),"source":"OpenCode","email":null,"expires":null}));
        }
    }
    omp_credentials(provider, home, &mut candidates);
    if provider == "qwen" {
        let settings = load(home, &home.join(".qwen/settings.json"));
        if let Some(secret) = settings
            .as_ref()
            .and_then(|v| v["env"]["BAILIAN_TOKEN_PLAN_API_KEY"].as_str())
            .filter(|s| valid_secret(s))
        {
            candidates
                .push(json!({"secret":secret,"source":"Qwen Code","email":null,"expires":null}));
        }
    }
    if !isolated {
        let names: &[&str] = match provider {
            "kimi-code" => &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
            "zai" => &["ZAI_API_KEY", "ZHIPU_API_KEY"],
            "qwen" => &["BAILIAN_TOKEN_PLAN_API_KEY"],
            _ => &["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"],
        };
        for name in names {
            if let Ok(s) = std::env::var(name)
                && valid_secret(&s)
            {
                candidates.push(
                    json!({"secret":s,"source":"User environment","email":null,"expires":null}),
                );
            }
        }
    }
    if provider == "qwen"
        && let Some((cookie, region, gateway)) = qwen_capsule(home, &json!({"capsuleId":"default"}))
    {
        let base = if region == "intl" {
            "ap-southeast-1"
        } else {
            "cn-beijing"
        };
        let mut token = None;
        if let Some(first) = candidates.first().and_then(|c| c["secret"].as_str()) {
            if let Ok(packed) = serde_json::from_str::<Value>(first) {
                token = packed["token"].as_str().map(str::to_owned);
            } else {
                token = Some(first.to_owned());
            }
        }
        let mut packed = json!({"cookie":cookie,"baseUrl":format!("https://token-plan.{base}.maas.aliyuncs.com/compatible-mode/v1")});
        if let Some(token) = token.filter(|t| valid_secret(t)) {
            packed["token"] = json!(token);
        }
        if let Some(gateway) = gateway {
            packed["gatewayCookie"] = json!(gateway);
        }
        candidates.insert(0,json!({"secret":packed.to_string(),"source":"Brave console","email":null,"expires":null}));
    }
    candidates
}
fn error(status: &str, message: &str) -> Value {
    json!({"status":status,"message":aac_core::scrub(message)})
}
fn retry(value: Option<&str>, now: f64) -> f64 {
    let seconds = value
        .filter(|s| s.len() <= 128)
        .and_then(|s| {
            let s = proof::strip(s);
            if s.len() <= 12 && !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) {
                s.parse::<f64>().ok()
            } else {
                chrono::DateTime::parse_from_rfc2822(s)
                    .ok()
                    .map(|d| d.timestamp() as f64 - now)
            }
        })
        .unwrap_or(PLAN_RETRY_AFTER.default_s as f64);
    seconds.clamp(
        PLAN_RETRY_AFTER.floor_s as f64,
        PLAN_RETRY_AFTER.ceiling_s as f64,
    )
}
async fn fetch(
    client: &impl Http,
    url: &str,
    headers: &[(&str, &str)],
    now: i64,
) -> Result<Value, Value> {
    fetch_body(client, url, headers, now, None).await
}
async fn fetch_body(
    client: &impl Http,
    url: &str,
    headers: &[(&str, &str)],
    now: i64,
    body: Option<&[u8]>,
) -> Result<Value, Value> {
    // A request/body/deadline failure cannot establish Python's socket-timeout result.
    let response = client.get(url, headers, body).await.map_err(|_| {
        error(
            "error",
            "The usage service is unavailable or returned an invalid response.",
        )
    })?;
    match response.status {
        401 => {
            return Err(error(
                "needs_sign_in",
                "The saved credential was rejected by the usage service.",
            ));
        }
        403 if url == "https://opencode.ai/zen/go/v1/usage" => {
            return Err(error(
                "error",
                "OpenCode Go usage access is unavailable; the saved key's sign-in status could not be verified.",
            ));
        }
        403 => {
            return Err(error(
                "needs_sign_in",
                "The saved account cannot access this plan's usage.",
            ));
        }
        429 => {
            let parsed = proof::scope(|| json!(retry(response.retry.as_deref(), now as f64)));
            if parsed == proof::fallback() {
                return Err(parsed);
            }
            return Err(error(
                "error",
                "The usage service asked to slow down; try again later.",
            ));
        }
        400.. => {
            return Err(error(
                "error",
                "The usage service could not complete the request.",
            ));
        }
        300..=399 => {
            return Err(error(
                "error",
                "The usage service could not complete the request.",
            ));
        }
        _ => {}
    }
    if response.body.len() > CAP {
        return Err(error(
            "error",
            "The usage service returned an oversized response.",
        ));
    }
    let v = bounded_json(&response.body, CAP).unwrap_or(Value::Null);
    if !v.is_object() {
        return Err(error(
            "error",
            "The usage service is unavailable or returned an invalid response.",
        ));
    }
    Ok(v)
}
// One publication boundary for every textual DTO value, including future providers.
// Changed/redacted/uncheckable text is dropped, never replaced with provider diagnostics.
fn publish(mut value: Value) -> Value {
    fn check(value: &mut Value) {
        match value {
            Value::String(raw) => {
                let checked = aac_core::scrub(raw);
                *raw = if checked.as_str() == raw
                    && !matches!(checked.as_str(), "[redacted]" | "[detail exceeded limit]")
                {
                    checked.as_str().to_owned()
                } else {
                    "error".into()
                };
            }
            Value::Array(rows) => rows.iter_mut().for_each(check),
            Value::Object(row) => row.values_mut().for_each(check),
            _ => {}
        }
    }
    check(&mut value);
    value
}
pub(crate) async fn run(args: &Value) -> Result<Value, Failure> {
    run_inner(args).await.map(publish)
}
async fn run_inner(args: &Value) -> Result<Value, Failure> {
    let Some(obj) = args.as_object() else {
        return Ok(proof::fallback());
    };
    if obj.keys().any(|k| {
        ![
            "provider",
            "platform",
            "home",
            "credential",
            "account",
            "endpoint",
            "now",
        ]
        .contains(&k.as_str())
    }) {
        return Ok(proof::fallback());
    }
    let provider = args["provider"].as_str().unwrap_or("");
    let Some(row) = aac_providers::provider(provider).filter(|p| p.quota) else {
        return Err(Failure::new(Cause::Refused, "unsupported quota provider"));
    };
    // The plan readers below; a `quota: true` row without one declines to Python.
    if !matches!(
        row.id,
        ProviderId::KimiCode | ProviderId::Qwen | ProviderId::Zai | ProviderId::OpencodeGo
    ) {
        return Ok(proof::fallback());
    }
    let label = row.label;
    let platform = args["platform"]
        .as_str()
        .unwrap_or(if cfg!(target_os = "macos") {
            "mac"
        } else if cfg!(windows) {
            "windows"
        } else {
            "ubuntu"
        });
    let place = match platform {
        "mac" => "Mac",
        "windows" => "Windows",
        "ubuntu" => "Ubuntu",
        _ => return Ok(proof::fallback()),
    };
    let isolated = args.get("home").is_some();
    let home = args["home"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok())
        .unwrap_or_default();
    // Python hashes `str(Path.home())` (or the given home) into the OpenCode session header
    // and joins store paths under it; only a spelling pathlib leaves unchanged is proved.
    if proof::scope(|| json!(files::canonical_home(&home))) == proof::fallback() {
        return Ok(proof::fallback());
    }
    #[allow(unused_mut)]
    let mut now = UtcMs::now()?.get() as i64 / 1000;
    #[allow(unused_mut)]
    let mut endpoint = None;
    if args.get("endpoint").is_some() || args.get("now").is_some() {
        #[cfg(not(feature = "parity-fixtures"))]
        return Ok(proof::fallback());
        #[cfg(feature = "parity-fixtures")]
        {
            if !isolated {
                return Ok(proof::fallback());
            }
            if let Some(raw) = args["endpoint"].as_str() {
                let Ok(url) = reqwest::Url::parse(raw) else {
                    return Ok(proof::fallback());
                };
                if url.scheme() != "http"
                    || url.host_str() != Some("127.0.0.1")
                    || !url.username().is_empty()
                    || url.password().is_some()
                {
                    return Ok(proof::fallback());
                }
                endpoint = Some(raw.to_owned());
            }
            now = args["now"].as_i64().unwrap_or(now);
        }
    }
    if (args.get("account").is_some() && !args["credential"].is_object())
        || (args["credential"]
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k != "discover")
            && args.get("account").is_none())
    {
        return Ok(proof::fallback());
    }
    // Credential discovery does blocking file IO: run it (and its proof scope) on a worker, so the
    // request deadline and cancellation still answer while a store read is stuck.
    let selected = {
        let (provider, home, selector) = (
            provider.to_owned(),
            home.clone(),
            args["credential"].clone(),
        );
        crate::blocking(move || {
            Ok(proof::scope(|| {
                json!(credential_candidates(
                    &provider,
                    Path::new(&home),
                    &selector,
                    isolated
                ))
            }))
        })
        .await?
    };
    if selected == proof::fallback() {
        return Ok(selected);
    }
    let mut result = json!({"id":format!("plan-{provider}-{platform}"),"provider":provider,"providerLabel":label,"label":label,"email":null,"plan":null,"platform":platform,"source":format!("Local credential store on {place}"),"status":"unavailable","message":null,"fetchedAt":null,"sampledAt":null,"isActive":false,"windows":[],"capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}});
    if let Some(account) = args.get("account") {
        let Some(id) = account.as_str() else {
            return Ok(proof::fallback());
        };
        let prefix = format!("{provider}:acct:");
        if id != format!("{provider}:usage")
            && !id.strip_prefix(&prefix).is_some_and(|s| {
                s.len() == 8
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            return Ok(proof::fallback());
        }
        result["id"] = json!(id);
    }
    let candidates = selected.as_array().cloned().unwrap_or_default();
    if candidates.is_empty() {
        result["message"] = json!(if args["credential"]
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k != "discover")
        {
            "The stored credential for this account could not be read."
        } else {
            "No saved credential was found in this computer's existing account stores."
        });
        return Ok(result);
    }
    let selected = if provider == "qwen" {
        candidates
            .iter()
            .find(|item| {
                item["secret"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .is_some_and(|p| p["cookie"].as_str().is_some_and(|c| !c.trim().is_empty()))
            })
            .unwrap_or(&candidates[0])
            .clone()
    } else if provider == "kimi-code" {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.)
            .unwrap_or(0.);
        candidates
            .iter()
            .find(|item| {
                item["expires"]
                    .as_f64()
                    .is_none_or(|e| (if e > 100000000000. { e } else { e * 1000. }) > now_ms)
            })
            .unwrap_or(&candidates[0])
            .clone()
    } else {
        candidates[0].clone()
    };
    let Some(secret) = selected["secret"].as_str() else {
        return Ok(proof::fallback());
    };
    if let Some(expires) = selected["expires"].as_f64() {
        let seconds = if expires > 100000000000. {
            expires / 1000.
        } else {
            expires
        };
        let current = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.);
        if seconds <= current {
            result["source"] = json!(format!(
                "{} on {place}",
                selected["source"].as_str().unwrap_or("")
            ));
            result["email"] = selected["email"].clone();
            result["label"] = selected["email"]
                .as_str()
                .map_or(result["providerLabel"].clone(), |s| json!(s));
            result["status"] = json!("needs_sign_in");
            result["message"] = json!(
                "The saved Kimi Code sign-in has expired; its owning application must renew it."
            );
            result["fetchedAt"] = json!(
                chrono::DateTime::from_timestamp(now, 0)
                    .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            );
            return Ok(result);
        }
    }
    result["source"] = json!(format!(
        "{} on {place}",
        selected["source"].as_str().unwrap_or("")
    ));
    result["email"] = selected["email"].clone();
    if let Some(email) = selected["email"].as_str() {
        result["label"] = json!(email);
    }
    let client = Client {
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Failure::new(Cause::Bug, "HTTP client unavailable"))?,
        deadline: Instant::now() + Duration::from_secs(7),
        endpoint,
    };
    let bearer = format!("Bearer {secret}");
    let installation = aac_core::sha256_hex(home.as_bytes());
    let session = format!("ccs-usage-{}", &installation[..32]);
    let (url, headers) = match provider {
        "kimi-code" => (
            "https://api.kimi.com/coding/v1/usages",
            vec![("Authorization", bearer.as_str())],
        ),
        "zai" => (
            "https://api.z.ai/api/monitor/usage/quota/limit",
            vec![
                ("Authorization", secret),
                ("Content-Type", "application/json"),
            ],
        ),
        _ => (
            "https://opencode.ai/zen/go/v1/usage",
            vec![
                ("Authorization", bearer.as_str()),
                ("x-opencode-session", session.as_str()),
            ],
        ),
    };
    let outcome = if provider == "qwen" {
        qwen(&client, secret, now).await
    } else {
        match fetch(&client, url, &headers, now).await {
            Ok(payload) => proof::scope(|| parse::parse(provider, &payload)),
            Err(e) => e,
        }
    };
    if outcome == proof::fallback() {
        return Ok(outcome);
    }
    let stamp = chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    result["fetchedAt"] = json!(stamp);
    if outcome.get("windows").is_some() {
        result["status"] = json!("ok");
        result["plan"] = outcome["plan"].clone();
        result["windows"] = outcome["windows"].clone();
        result["sampledAt"] = json!(stamp);
        if provider == "zai" {
            // Reset pack shapes are a separate grammar; never manufacture an empty balance.
            match fetch(
                &client,
                "https://api.z.ai/api/biz/customer-package-reset/list?targetType=PERSONAL",
                &[("Authorization", bearer.as_str())],
                now,
            )
            .await
            {
                Ok(payload) => {
                    let packs = proof::scope(|| parse::packs(&payload, now));
                    if packs == proof::fallback() {
                        return Ok(packs);
                    }
                    if let Some(packs) = packs.as_array() {
                        if let Some(windows) = result["windows"].as_array_mut() {
                            windows.extend(packs.iter().cloned());
                        }
                    } else {
                        result["message"] =
                            json!("Account usage is current; reset pack inventory is unavailable.");
                    }
                }
                Err(e) if e == proof::fallback() => return Ok(e),
                Err(_) => {
                    result["message"] =
                        json!("Account usage is current; reset pack inventory is unavailable.")
                }
            }
        }
    } else {
        result["status"] = outcome["status"].clone();
        result["message"] = outcome["message"].clone();
    }
    Ok(result)
}

fn form(pairs: &[(&str, String)]) -> String {
    let mut url =
        reqwest::Url::parse("http://localhost/").unwrap_or_else(|_| unreachable!("constant URL"));
    {
        let mut q = url.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
    }
    url.query().unwrap_or("").to_owned()
}
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
async fn fetch_text(
    client: &impl Http,
    url: &str,
    headers: &[(&str, &str)],
    _now: i64,
) -> Result<String, Value> {
    let response = client
        .get(url, headers, None)
        .await
        .map_err(|_| proof::fallback())?;
    match response.status {
        401 => {
            return Err(error(
                "needs_sign_in",
                "The saved credential was rejected by the usage service.",
            ));
        }
        403 => {
            return Err(error(
                "needs_sign_in",
                "The saved account cannot access this plan's usage.",
            ));
        }
        429 => {
            return Err(error(
                "error",
                "The usage service asked to slow down; try again later.",
            ));
        }
        300..=u16::MAX => {
            return Err(error(
                "error",
                "The usage service could not complete the request.",
            ));
        }
        _ => {}
    }
    if response.body.len() > CAP {
        return Err(error(
            "error",
            "The usage service returned an oversized response.",
        ));
    }
    String::from_utf8(response.body).map_err(|_| {
        error(
            "error",
            "The usage service is unavailable or returned an invalid response.",
        )
    })
}
async fn qwen(client: &impl Http, secret: &str, now: i64) -> Value {
    let packed: Value = serde_json::from_str(secret).unwrap_or(Value::Null);
    let Some(cookie) = packed["cookie"].as_str().filter(|s| !s.trim().is_empty()) else {
        return error(
            "unavailable",
            "The saved Qwen API key has no console session; live token-plan usage requires its existing console cookie.",
        );
    };
    if cookie.len() > 32768 || cookie.bytes().any(|c| matches!(c, 0 | 10 | 13)) {
        return error("unavailable", "The saved Qwen console session is invalid.");
    }
    let base = packed
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or("https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1")
        .trim_end_matches('/');
    let region = match base {
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1" => "intl",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1" => "cn",
        _ => {
            return error(
                "unavailable",
                "The saved Qwen token-plan region is unsupported.",
            );
        }
    };
    let gateway_value = if packed.get("gatewayCookie").is_some() {
        &packed["gatewayCookie"]
    } else {
        &packed["cookie"]
    };
    let Some(gateway_cookie) = gateway_value
        .as_str()
        .filter(|s| s.len() <= 32768 && !s.bytes().any(|c| matches!(c, 0 | 10 | 13)))
    else {
        return error("unavailable", "The saved Qwen gateway session is invalid.");
    };
    let (origin, referer, action, gateway, cloud_region, cornerstone, token) = if region == "intl" {
        let info = match fetch(
            client,
            "https://home.qwencloud.com/tool/user/info.json",
            &[("Cookie", cookie)],
            now,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return e,
        };
        let Some(token) = info["data"]["secToken"].as_str().map(str::to_owned) else {
            return error(
                "needs_sign_in",
                "The saved Qwen console session has expired or is incomplete.",
            );
        };
        (
            "https://home.qwencloud.com",
            "https://home.qwencloud.com/analytics/token-plan/individual",
            "IntlBroadScopeAspnGateway",
            "https://cs-data.qwencloud.com/data/api.json",
            "ap-southeast-1",
            json!({"domain":"home.qwencloud.com","consoleSite":"QWENCLOUD","console":"ONE_CONSOLE","xsp_lang":"en-US","protocol":"V2","productCode":"p_efm"}),
            token,
        )
    } else {
        let referer = "https://bailian.console.aliyun.com/cn-beijing?tab=plan";
        let page = match fetch_text(
            client,
            referer,
            &[("Cookie", cookie), ("Accept", "text/html")],
            now,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => return e,
        };
        let token = page.find("SEC_TOKEN:").and_then(|at| {
            let rest = page[at + 10..].trim_start_matches([' ', '\t', '\r', '\n']);
            rest.strip_prefix('"')
                .and_then(|rest| rest.find('"').map(|end| rest[..end].to_owned()))
        });
        let Some(token) = token else {
            return error(
                "needs_sign_in",
                "The saved Qwen console session has expired or is incomplete.",
            );
        };
        let trace = format!(
            "{:x}",
            aac_core::sha256_hex(format!("{now}:{page}").as_bytes())
                .as_bytes()
                .iter()
                .take(16)
                .fold(0u128, |a, b| a << 8 | *b as u128)
        );
        let trace = format!(
            "{}-{}-{}-{}-{}",
            &trace[0..8],
            &trace[8..12],
            &trace[12..16],
            &trace[16..20],
            &trace[20..32]
        );
        (
            "https://bailian.console.aliyun.com",
            referer,
            "BroadScopeAspnGateway",
            "https://bailian-cs.console.aliyun.com/data/api.json",
            "cn-beijing",
            json!({"feTraceId":trace,"feURL":format!("{referer}#/efm/subscription/token-plan/personal"),"protocol":"V2","console":"ONE_CONSOLE","productCode":"p_efm","switchUserType":3,"domain":"bailian.console.aliyun.com","consoleSite":"BAILIAN_ALIYUN","userNickName":"","userPrincipalName":"","xsp_lang":"zh-CN"}),
            token,
        )
    };
    if token.is_empty() || token.len() > 16384 {
        return error(
            "needs_sign_in",
            "The saved Qwen console session has expired or is incomplete.",
        );
    }
    let mut headers = vec![
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Origin", origin),
        ("Referer", referer),
        ("X-Requested-With", "XMLHttpRequest"),
    ];
    if !gateway_cookie.is_empty() {
        headers.push(("Cookie", gateway_cookie));
    }
    let mut csrf = None;
    for part in gateway_cookie.split(';') {
        let mut pieces = part.trim().splitn(2, '=');
        let name = pieces.next().unwrap_or("");
        let value = pieces.next().unwrap_or("");
        if matches!(name, "login_aliyunid_csrf" | "csrf")
            && !value.bytes().any(|b| matches!(b, b'\r' | b'\n'))
        {
            let decoded = percent_decode(value);
            if decoded.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
                return error("unavailable", "The saved Qwen console session is invalid.");
            }
            csrf = Some(decoded);
            break;
        }
    }
    let csrf_text;
    if let Some(value) = csrf {
        csrf_text = value;
        headers.push(("x-xsrf-token", &csrf_text));
        headers.push(("x-csrf-token", &csrf_text));
    }
    let mut payloads = Vec::new();
    for suffix in ["usage", "addon/list", "subscription", "quota-config"] {
        let endpoint = format!("zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/{suffix}");
        let mut data = json!({"cornerstoneParam":cornerstone});
        if suffix == "subscription" && region == "intl" {
            data["commodityCode"] = json!("sfm_tokenplansolo_public_intl");
        }
        if suffix == "addon/list" {
            data["commodityCode"] = json!(format!(
                "sfm_tokenplansoloaddon_public_{}",
                if region == "intl" { "intl" } else { "cn" }
            ));
            data["status"] = json!(["ACTIVE"]);
            data["pageNum"] = json!(1);
            data["pageSize"] = json!(10);
        }
        let params = json!({"Api":endpoint,"Data":data,"V":"1.0"});
        let query = form(&[
            ("product", "sfm_bailian".into()),
            ("action", action.into()),
            ("api", endpoint),
        ]);
        let body = form(&[
            ("product", "sfm_bailian".into()),
            ("action", action.into()),
            ("region", cloud_region.into()),
            ("sec_token", token.clone()),
            ("params", params.to_string()),
        ]);
        let url = format!("{gateway}?{query}");
        match fetch_body(client, &url, &headers, now, Some(body.as_bytes())).await {
            Ok(v) => payloads.push(v),
            Err(e) if suffix == "usage" || e == proof::fallback() => return e,
            Err(_) => payloads.push(Value::Null),
        }
    }
    proof::scope(|| parse::qwen(&payloads))
}

#[cfg(test)]
mod table_tests {
    //! Plan providers come from the shared table (`quota: true`); a row the readers do not
    //! implement declines to Python, an id the table does not list is refused as before.
    use super::*;

    fn ask(provider: &str) -> Result<Value, Failure> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(run_inner(
            &json!({"provider": provider, "home": "/nonexistent-aac-l4"}),
        ))
    }

    #[test]
    fn synthetic_quota_row_declines_and_unknown_ids_are_refused() {
        assert!(ask("retired-provider").is_err());
        assert!(ask("claude").is_err(), "a provider without quota: true");
        let synthetic = ask("zz-synthetic");
        if aac_providers::SYNTHETIC {
            assert_eq!(synthetic.ok(), Some(proof::fallback()));
        } else {
            assert!(synthetic.is_err());
        }
    }

    #[test]
    fn retry_after_policy_is_the_tables() {
        assert_eq!(retry(None, 0.0), 600.0);
        assert_eq!(retry(Some("5"), 0.0), 30.0);
        assert_eq!(retry(Some("999999"), 0.0), 86400.0);
        assert_eq!(retry(Some("120"), 0.0), 120.0);
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn every_text_field_passes_the_shared_scrub_and_uncheckable_text_is_dropped() {
        let raw = json!({"message":"{\"access_token\":\"fixture-dummy-token\"}","windows":[{"label":"x".repeat(4097)}],"source":"[redacted]","status":"error"});
        let published = publish(raw);
        assert_eq!(published["message"], "error");
        assert_eq!(published["windows"][0]["label"], "error");
        assert_eq!(published["status"], "error");
        assert_eq!(published["source"], "error");
        assert!(!published.to_string().contains("fixture-dummy-token"));
    }
}
