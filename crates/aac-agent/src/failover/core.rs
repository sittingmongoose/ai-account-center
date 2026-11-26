//! Core quota collection: fixed endpoints, bounded reads, and no refresh or credential writes.
use super::{dispatch, *};
use std::time::{Duration, Instant};
const CODEX: &str = "https://chatgpt.com/backend-api/wham/usage";
const PROFILE: &str = "https://api.anthropic.com/api/oauth/profile";
const USAGE: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1";
fn now() -> i64 {
    aac_core::UtcMs::now().map(|v| v.get() as i64).unwrap_or(0)
}

struct Native;
impl Adapter for Native {
    fn inventory(&mut self, c: &Context) -> Result<Value, ()> {
        if c.claude_profile.is_some() {
            return Err(());
        }
        let result = proof::scope(
            || json!({"candidates":super::core_candidates(&c.provider,&c.home,&c.ccs)}),
        );
        if result == proof::fallback() {
            Err(())
        } else {
            Ok(result)
        }
    }
    async fn collect(&mut self, candidate: &Value, c: &Context) -> Value {
        if c.provider != "codex" {
            return dispatch::failure("unavailable", &Value::Null, &Value::Null);
        }
        let Ok(client) = Client::new() else {
            return dispatch::failure("unavailable", &Value::Null, &Value::Null);
        };
        match client
            .get(
                CODEX,
                candidate["_token"].as_str().unwrap_or(""),
                candidate["_account"].as_str(),
            )
            .await
        {
            Err(error) => error,
            Ok(payload) => {
                let windows = codex_windows(&payload, now());
                if windows.is_empty() {
                    return dispatch::failure("invalid_response", &Value::Null, &Value::Null);
                }
                let plan = desktop_text(alias(&payload, "plan_type", "planType"), 64)
                    .or_else(|| candidate["plan"].as_str().map(str::to_owned));
                let stamp = chrono::DateTime::from_timestamp_millis(now())
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                json!({"sample":{"provider":"codex","platform":c.platform,"email":candidate["email"],"plan":plan,"status":"ok","sampledAt":stamp,"fetchedAt":stamp,"windows":windows},"failureCode":null})
            }
        }
    }
}
pub(super) fn run(request: &Value) -> Value {
    // Claude routes to its own adapter before this point (failover::claude); providers
    // without a native collector here keep their existing route.
    if request["provider"] != "codex" {
        return proof::fallback();
    }
    if request["version"].as_u64() != Some(1)
        || request.as_object().is_none_or(|o| {
            o.keys().any(|k| {
                ![
                    "version",
                    "action",
                    "provider",
                    "candidate",
                    "homeDir",
                    "ccsDir",
                    "claudeProfile",
                ]
                .contains(&k.as_str())
            })
        })
        || request.get("claudeProfile").is_some()
        || dispatch::candidate(&request["candidate"], "codex", true).is_none()
    {
        return dispatch::failure("invalid_response", &Value::Null, &Value::Null);
    }
    let mut inventory = request.clone();
    let Some(obj) = inventory.as_object_mut() else {
        return proof::fallback();
    };
    obj.remove("candidate");
    obj.insert("action".into(), json!("inventory"));
    if super::run(&inventory).ok() == Some(proof::fallback()) {
        return proof::fallback();
    }
    super::run_with_adapter(request, Native)
}
struct Client {
    client: reqwest::Client,
    deadline: Instant,
    #[cfg(test)]
    endpoint: Option<String>,
}
impl Client {
    fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            deadline: Instant::now() + Duration::from_secs(12),
            #[cfg(test)]
            endpoint: None,
        })
    }
    async fn get(&self, url: &str, token: &str, account: Option<&str>) -> Result<Value, Value> {
        let error = |code, retry: &Value| dispatch::failure(code, &Value::Null, retry);
        if ![CODEX, PROFILE, USAGE].contains(&url) {
            return Err(error("unavailable", &Value::Null));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(error("unavailable", &Value::Null));
        }
        #[cfg(test)]
        let destination = self.endpoint.as_deref().unwrap_or(url);
        #[cfg(not(test))]
        let destination = url;
        let mut request = self
            .client
            .get(destination)
            .timeout(remaining.min(Duration::from_secs(5)))
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json");
        if url == CODEX {
            request = request
                .header("ChatGPT-Account-Id", account.unwrap_or(""))
                .header(
                    "User-Agent",
                    "codex_cli_rs/0.76.0 (Debian 13.0.0; x86_64) WindowsTerminal",
                );
        } else {
            request = request
                .header("anthropic-beta", "oauth-2025-04-20")
                .header("User-Agent", "CCS-ReadOnly-Usage/1.0");
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| error("unavailable", &Value::Null))?;
        let status = response.status().as_u16();
        if status >= 300 {
            let retry = retry_after(
                response
                    .headers()
                    .get("Retry-After")
                    .and_then(|h| h.to_str().ok()),
                now(),
            );
            return Err(error(
                match status {
                    429 => "rate_limited",
                    401 => "needs_sign_in",
                    _ => "unavailable",
                },
                &json!(retry),
            ));
        }
        let mut bytes = Vec::new();
        loop {
            if Instant::now() >= self.deadline {
                return Err(error("unavailable", &Value::Null));
            }
            let chunk = response
                .chunk()
                .await
                .map_err(|_| error("unavailable", &Value::Null))?;
            let Some(chunk) = chunk else {
                break;
            };
            if bytes.len() + chunk.len() > CORE_CAP {
                return Err(error("invalid_response", &Value::Null));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error("unavailable", &Value::Null))?;
        if !value.is_object() {
            return Err(error("invalid_response", &Value::Null));
        }
        Ok(value)
    }
}
fn retry_after(raw: Option<&str>, now: i64) -> Option<i64> {
    let raw = raw.filter(|s| s.len() <= 128)?;
    let seconds = if raw.trim().bytes().all(|b| b.is_ascii_digit()) && !raw.trim().is_empty() {
        {
            let digits = raw.trim().trim_start_matches('0');
            if digits.len() > 5 {
                86400
            } else {
                digits.parse::<i64>().unwrap_or(0).min(86400)
            }
        }
    } else {
        let at = chrono::DateTime::parse_from_rfc2822(raw)
            .ok()?
            .timestamp_millis();
        ((at - now) as f64 / 1000.0).ceil() as i64
    };
    Some(seconds.clamp(0, 86400))
}
fn alias<'a>(value: &'a Value, snake: &str, camel: &str) -> &'a Value {
    value.get(snake).unwrap_or(&value[camel])
}
fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}
fn reset(v: &Value) -> Value {
    let t = if let Some(n) = number(v) {
        let seconds = if n > 100_000_000_000.0 { n / 1000.0 } else { n };
        aac_num::py::fromtimestamp(seconds)
            .and_then(|(sec, micro)| chrono::DateTime::from_timestamp(sec, micro * 1000))
    } else {
        v.as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    use chrono::Datelike;
    t.filter(|t| (2000..=2100).contains(&t.year()))
        .map(|t| json!(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

fn quota(key: &str, label: &str, kind: &str) -> Value {
    json!({"key":key,"label":label,"kind":kind,"usedPercent":null,"remainingPercent":null,"resetAt":null,"windowMinutes":null,"used":null,"limit":null,"unit":null})
}
fn group(value: &Value, prefix: &str, title: &str, now: i64, windows: &mut Vec<Value>) {
    for (position, camel, expected) in [
        ("primary_window", "primaryWindow", 18000.0),
        ("secondary_window", "secondaryWindow", 604800.0),
    ] {
        let raw = alias(value, position, camel);
        let Some(used) = number(alias(raw, "used_percent", "usedPercent")).filter(|n| *n >= 0.0)
        else {
            continue;
        };
        let seconds = number(alias(raw, "limit_window_seconds", "limitWindowSeconds"))
            .filter(|n| *n > 0.0)
            .unwrap_or(expected);
        let (cadence, label) = if seconds == 18000.0 {
            ("five_hour", "5-hour usage")
        } else if seconds == 604800.0 {
            ("seven_day", "Weekly usage")
        } else {
            (position, "Usage")
        };
        let key = format!("{prefix}{cadence}");
        if windows.iter().any(|w| w["key"] == key) {
            continue;
        }
        let label = if prefix.is_empty() {
            label.into()
        } else {
            format!("{title} - {label}")
        };
        let mut w = quota(&key, &label, "rate_limit");
        let used = aac_num::py::round(used, 4).unwrap_or(used);
        w["usedPercent"] = json!(used);
        w["remainingPercent"] = json!(aac_num::py::round(100.0 - used, 4).unwrap_or(0.0).max(0.0));
        if seconds / 60.0 <= 525600.0 {
            w["windowMinutes"] = json!(seconds / 60.0);
        }
        let raw_reset = number(alias(raw, "reset_at", "resetAt"));
        let instant = match raw_reset {
            Some(n) if n > 0.0 => Some(n),
            None => number(alias(raw, "reset_after_seconds", "resetAfterSeconds"))
                .filter(|n| *n >= 0.0)
                .map(|n| now as f64 / 1000.0 + n),
            _ => None,
        };
        w["resetAt"] = reset(&json!(instant));
        windows.push(w);
    }
}
pub(super) fn codex_windows(payload: &Value, now: i64) -> Vec<Value> {
    let mut windows = vec![];
    group(
        alias(payload, "rate_limit", "rateLimit"),
        "",
        "Usage",
        now,
        &mut windows,
    );
    group(
        alias(payload, "code_review_rate_limit", "codeReviewRateLimit"),
        "code_review_",
        "Code review",
        now,
        &mut windows,
    );
    if let Some(additional) =
        alias(payload, "additional_rate_limits", "additionalRateLimits").as_array()
    {
        for (i, value) in additional.iter().take(16).enumerate() {
            let title = desktop_text(alias(value, "limit_name", "limitName"), 64)
                .unwrap_or_else(|| "Additional usage".into());
            group(
                alias(value, "rate_limit", "rateLimit"),
                &format!("additional_{i}_"),
                &title,
                now,
                &mut windows,
            );
        }
    }
    let credits = &payload["credits"];
    let balance = number(&credits["balance"]).or_else(|| {
        credits["balance"]
            .as_str()
            .filter(|s| matches(r"\A\d+(?:\.\d+)?\z", s))
            .and_then(|s| s.parse().ok())
    });
    if balance.is_some_and(|n| n >= 0.0) || credits["unlimited"].is_boolean() {
        let mut w = quota("credits_balance", "Extra usage credits", "balance");
        if let Some(n) = balance {
            w["remaining"] = json!(if n >= 0.0 { Some(n) } else { None });
        }
        w["unit"] = json!("credits");
        for k in ["unlimited", "has_credits"] {
            if credits[k].is_boolean() {
                w[if k == "has_credits" { "enabled" } else { k }] = credits[k].clone();
            }
        }
        windows.push(w);
    }
    if let Some(n) = number(&payload["rate_limit_reset_credits"]["available_count"])
        .filter(|n| *n >= 0.0 && n.fract() == 0.0)
    {
        let mut w = quota("banked_resets", "Banked resets", "balance");
        w["remaining"] = json!(n);
        w["unit"] = json!("resets");
        windows.push(w);
    }
    let monthly = &payload["spend_control"]["individual_limit"];
    let used = number(&monthly["used"]);
    let limit = number(&monthly["limit"]);
    let remaining = number(&monthly["remaining_percent"]).filter(|n| (0.0..=100.0).contains(n));
    if used.is_some() || limit.is_some() || remaining.is_some() {
        let mut w = quota("monthly_credit_limit", "Monthly credit limit", "spend");
        w["used"] = json!(used.filter(|n| *n >= 0.0));
        w["limit"] = json!(limit.filter(|n| *n >= 0.0));
        w["remainingPercent"] = json!(remaining);
        w["usedPercent"] =
            json!(remaining.map(|n| aac_num::py::round(100.0 - n, 4).unwrap_or(0.0).max(0.0)));
        w["resetAt"] = reset(&monthly["reset_at"]);
        w["unit"] = json!("credits");
        if let (Some(used), Some(limit)) = (used, limit) {
            w["remaining"] = json!((limit - used).max(0.0));
        }
        windows.push(w);
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;
    fn numeric(v: Value) -> Value {
        match v {
            Value::Number(n) => json!(n.as_f64()),
            Value::Array(a) => json!(a.into_iter().map(numeric).collect::<Vec<_>>()),
            Value::Object(o) => {
                Value::Object(o.into_iter().map(|(k, v)| (k, numeric(v))).collect())
            }
            v => v,
        }
    }
    #[test]
    fn python_codex_window_oracle() {
        let cases: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/failover-codex.json"))
                .expect("oracle");
        for (i, c) in cases.as_array().expect("cases").iter().enumerate() {
            assert_eq!(
                numeric(json!(codex_windows(
                    &c["payload"],
                    c["now"].as_i64().expect("clock") * 1000
                ))),
                numeric(c["expected"].clone()),
                "case {i}: {c}"
            );
        }
    }
    #[test]
    fn bounded_retry_after() {
        assert_eq!(retry_after(Some("999999999999999999999"), 0), Some(86400));
        assert_eq!(retry_after(Some("bad"), 0), None);
        assert_eq!(
            retry_after(Some("Thu, 26 Nov 2026 20:00:00 GMT"), 1795723199500),
            Some(1)
        );
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{Read, Write};
    fn server(response: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("address");
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout");
            let mut input = vec![0; 8192];
            let size = stream.read(&mut input).expect("request");
            stream.write_all(response.as_bytes()).expect("response");
            String::from_utf8_lossy(&input[..size]).into_owned()
        });
        (format!("http://{addr}/"), worker)
    }
    #[tokio::test]
    async fn rate_limit_does_not_read_or_forward_error_body() {
        let (endpoint, worker) = server(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 123\r\nContent-Length: 999999\r\nConnection: close\r\n\r\nprivate-provider-body",
        );
        let mut client = Client::new().expect("client");
        client.endpoint = Some(endpoint);
        let out = client
            .get(CODEX, "synthetic-fixture-token", Some("fixture-account"))
            .await
            .expect_err("429");
        assert_eq!(out["failureCode"], "rate_limited");
        assert_eq!(out["retryAfterSeconds"], 123);
        assert!(!out.to_string().contains("private-provider-body"));
        let request = worker.join().expect("server");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("chatgpt-account-id: fixture-account")
        );
    }
    #[tokio::test]
    async fn redirect_is_not_followed() {
        let (endpoint, worker) = server(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/credential-sink\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let mut client = Client::new().expect("client");
        client.endpoint = Some(endpoint);
        let out = client
            .get(CODEX, "synthetic-fixture-token", Some("fixture-account"))
            .await
            .expect_err("redirect");
        assert_eq!(out["failureCode"], "unavailable");
        worker.join().expect("server");
    }
    #[tokio::test]
    async fn endpoint_allowlist_and_deadline_prevent_io() {
        let client = Client::new().expect("client");
        assert!(
            client
                .get("https://untrusted.invalid/", "fixture", None)
                .await
                .is_err()
        );
        let mut client = client;
        client.deadline = Instant::now() - Duration::from_secs(1);
        assert!(client.get(CODEX, "fixture", None).await.is_err());
    }
}
