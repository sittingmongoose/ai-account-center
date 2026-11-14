//! Plan readers. Only an injected client can perform network IO; unproven inputs delegate.
mod parse;
use crate::proof;
use aac_core::{Cause, Failure, UtcMs};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

const CAP: usize = 1_048_576;
const PROVIDERS: [(&str, &str); 4] = [
    ("kimi-code", "Kimi Code"),
    ("qwen", "Qwen Token Plan"),
    ("zai", "Z.ai Coding Plan"),
    ("opencode-go", "OpenCode Go"),
];
// Plan-common's retry policy is declared once, including its invalid-header default.
const RETRY_BOUNDS: (u64, u64, u64) = (30, 86400, 600);
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
        let mut request = request
            .timeout(remaining.min(Duration::from_secs(4)))
            .header("Accept", "application/json")
            .header("User-Agent", "CCS-Accounts/1.0");
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
fn load(path: &Path) -> Option<Value> {
    if fs::metadata(path).is_ok_and(|m| !m.is_file()) {
        proof::reject();
        return None;
    }
    match fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            if file.take((CAP + 1) as u64).read_to_end(&mut bytes).is_err() {
                proof::reject();
                return None;
            }
            proof::json(&bytes, CAP, false)
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            None
        }
        Err(_) => {
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
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
        let Ok(info) = fs::symlink_metadata(&directory) else {
            return Value::Null;
        };
        if !info.is_dir() || info.mode() & 0o077 != 0 {
            return Value::Null;
        }
        let path = directory.join(format!("{provider}-{id}.json"));
        let Ok(file) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        else {
            return Value::Null;
        };
        let Ok(info) = file.metadata() else {
            return Value::Null;
        };
        if !info.is_file() || info.mode() & 0o077 != 0 || info.len() > 4096 {
            return Value::Null;
        }
        let mut bytes = Vec::new();
        if file.take(4097).read_to_end(&mut bytes).is_err() {
            return Value::Null;
        }
        let Some(record) = proof::json(&bytes, 4096, false) else {
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
fn valid_secret(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32768 && !s.contains('\0')
}
fn credential(provider: &str, home: &Path, selector: &Value, isolated: bool) -> Value {
    if selector["kind"] == "aac-key" {
        return key_credential(provider, home, selector);
    }
    // These stores need native decryption/SQLite/safari grammar proof. Delegate before
    // reading an alternative store, so selection can never silently change identity.
    if selector
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|k| k != "discover")
        || home.join(".omp/agent/agent.db").exists()
        || (provider == "qwen"
            && (home
                .join(".ccs/account-usage/qwen-console-session.json")
                .exists()
                || cfg!(target_os = "macos")))
    {
        return proof::fallback();
    }
    let auth = load(&home.join(".local/share/opencode/auth.json"));
    if provider == "qwen"
        && (home.join(".qwen/settings.json").exists()
            || (!isolated && std::env::var("BAILIAN_TOKEN_PLAN_API_KEY").is_ok()))
        && auth
            .as_ref()
            .is_some_and(|v| v.get("alibaba-token-plan").is_some())
    {
        return proof::fallback();
    }
    let aliases: &[&str] = match provider {
        "kimi-code" => &["kimi-for-coding", "kimi-code"],
        "zai" => &["zai", "zai-coding-plan"],
        "qwen" => &["alibaba-token-plan"],
        _ => &["opencode-go"],
    };
    for alias in aliases {
        let item = &auth.as_ref().unwrap_or(&Value::Null)[alias];
        if item["type"] == "api"
            && let Some(secret) = item["key"].as_str().filter(|s| valid_secret(s))
        {
            return json!({"secret":secret,"source":"OpenCode"});
        }
    }
    if provider == "qwen" {
        let settings = load(&home.join(".qwen/settings.json"));
        if let Some(secret) = settings
            .as_ref()
            .and_then(|v| v["env"]["BAILIAN_TOKEN_PLAN_API_KEY"].as_str())
            .filter(|s| valid_secret(s))
        {
            return json!({"secret":secret,"source":"Qwen Code"});
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
                return json!({"secret":s,"source":"User environment"});
            }
        }
    }
    Value::Null
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
                match chrono::DateTime::parse_from_rfc2822(s) {
                    Ok(d) => Some(d.timestamp() as f64 - now),
                    Err(_) => {
                        proof::reject();
                        None
                    }
                }
            }
        })
        .unwrap_or(RETRY_BOUNDS.2 as f64);
    seconds.clamp(RETRY_BOUNDS.0 as f64, RETRY_BOUNDS.1 as f64)
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
    let v = proof::scope(|| proof::json(&response.body, CAP, false).unwrap_or(Value::Null));
    if v == proof::fallback() {
        return Err(v);
    }
    if !v.is_object() {
        return Err(error(
            "error",
            "The usage service is unavailable or returned an invalid response.",
        ));
    }
    Ok(v)
}
pub(crate) async fn run(args: &Value) -> Result<Value, Failure> {
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
    let Some((_, label)) = PROVIDERS.iter().find(|(p, _)| *p == provider) else {
        return Err(Failure::new(Cause::Refused, "unsupported quota provider"));
    };
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
    if home.is_empty() || proof::scope(|| json!(proof::ascii(&home))) == proof::fallback() {
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
    let selected =
        proof::scope(|| credential(provider, Path::new(&home), &args["credential"], isolated));
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
    let Some(secret) = selected["secret"].as_str() else {
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
    };
    result["source"] = json!(format!(
        "{} on {place}",
        selected["source"].as_str().unwrap_or("")
    ));
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
async fn qwen(client: &impl Http, secret: &str, now: i64) -> Value {
    let packed: Value = serde_json::from_str(secret).unwrap_or(Value::Null);
    if proof::scope(|| {
        if !proof::proven(&packed) {
            proof::reject();
        }
        Value::Null
    }) == proof::fallback()
    {
        return proof::fallback();
    }
    if packed.get("baseUrl").is_some_and(|v| !v.is_string()) {
        return error(
            "unavailable",
            "The saved Qwen token-plan region is unsupported.",
        );
    }
    let Some(cookie) = packed["cookie"]
        .as_str()
        .filter(|s| !s.trim_matches(proof::space).is_empty())
    else {
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
    if base == "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1" {
        return proof::fallback();
    }
    if base != "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1" {
        return error(
            "unavailable",
            "The saved Qwen token-plan region is unsupported.",
        );
    }
    let gateway_cookie = packed.get("gatewayCookie").unwrap_or(&packed["cookie"]);
    let Some(gateway_cookie) = gateway_cookie
        .as_str()
        .filter(|s| s.len() <= 32768 && !s.bytes().any(|c| matches!(c, 0 | 10 | 13)))
    else {
        return error("unavailable", "The saved Qwen gateway session is invalid.");
    };
    // Percent-decoding of CSRF is a separate grammar boundary; do not silently omit it.
    if gateway_cookie.split(';').any(|s| {
        s.trim_matches(proof::space).starts_with("csrf=")
            || s.trim_matches(proof::space)
                .starts_with("login_aliyunid_csrf=")
    }) {
        return proof::fallback();
    }
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
    let Some(token) = info["data"]["secToken"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384)
    else {
        return error(
            "needs_sign_in",
            "The saved Qwen console session has expired or is incomplete.",
        );
    };
    let mut headers = vec![
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Origin", "https://home.qwencloud.com"),
        (
            "Referer",
            "https://home.qwencloud.com/analytics/token-plan/individual",
        ),
        ("X-Requested-With", "XMLHttpRequest"),
    ];
    if !gateway_cookie.is_empty() {
        headers.push(("Cookie", gateway_cookie));
    }
    let mut payloads = Vec::new();
    for suffix in ["usage", "addon/list", "subscription", "quota-config"] {
        let endpoint = format!("zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/{suffix}");
        let mut data = json!({"cornerstoneParam":{"domain":"home.qwencloud.com","consoleSite":"QWENCLOUD","console":"ONE_CONSOLE","xsp_lang":"en-US","protocol":"V2","productCode":"p_efm"}});
        if suffix == "subscription" {
            data["commodityCode"] = json!("sfm_tokenplansolo_public_intl");
        }
        if suffix == "addon/list" {
            data["commodityCode"] = json!("sfm_tokenplansoloaddon_public_intl");
            data["status"] = json!(["ACTIVE"]);
            data["pageNum"] = json!(1);
            data["pageSize"] = json!(10);
        }
        let params = json!({"Api":endpoint,"Data":data,"V":"1.0"});
        let query = form(&[
            ("product", "sfm_bailian".into()),
            ("action", "IntlBroadScopeAspnGateway".into()),
            ("api", endpoint),
        ]);
        let body = form(&[
            ("product", "sfm_bailian".into()),
            ("action", "IntlBroadScopeAspnGateway".into()),
            ("region", "ap-southeast-1".into()),
            ("sec_token", token.into()),
            ("params", params.to_string()),
        ]);
        let url = format!("https://cs-data.qwencloud.com/data/api.json?{query}");
        match fetch_body(client, &url, &headers, now, Some(body.as_bytes())).await {
            Ok(v) => payloads.push(v),
            Err(e) if suffix == "usage" || e == proof::fallback() => return e,
            Err(_) => payloads.push(Value::Null),
        }
    }
    proof::scope(|| parse::qwen(&payloads))
}
