//! No-browser plain-HTTPS readers for pasted web sessions:
//! - Qwen Token Plan (`home.qwencloud.com` / `bailian.console.aliyun.com`)
//! - OpenCode Zen console wallet (`opencode.ai/console/api/*`)
//! - Muse Code portal quota (`dev.meta.ai/api/*`)
//!
//! All credentials come from `aac_files::web_session` (0600 on Unix, DPAPI on Windows).
//! Redirects are refused, error response bodies are never read or logged, and every reader
//! accepts an injectable [`HttpTransport`] for offline fixture testing.

pub mod muse;
pub mod opencode;
pub mod qwen;

use aac_core::{Cause, Failure};
use aac_files::web_session::{self, PasteOptions, Surface};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    time::{Duration, Instant},
};

/// Bounded HTTP response metadata and body (error bodies >= 400 are never read).
#[derive(Clone, Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Optional `Retry-After` response header.
    pub retry_after: Option<String>,
    /// Response body bytes (empty when `status >= 400`).
    pub body: Vec<u8>,
}

/// Transport-level failure from a bounded HTTPS request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// Connection, TLS, DNS, or timeout failure.
    Network,
    /// Upstream returned a 3xx redirect (session cookies never follow redirects).
    Redirect,
    /// Response body exceeded the caller's byte cap.
    TooLarge,
}

/// Injectable plain-HTTPS transport for offline fixture tests and production `reqwest` calls.
pub trait HttpTransport: Send + Sync {
    /// Issue one bounded `GET` or `POST` request without following redirects.
    fn send<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: Option<&'a [u8]>,
        cap: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>>;
}

/// Production plain-HTTPS transport backed by `reqwest` (`rustls-tls`, no redirects).
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    deadline: Instant,
    endpoint: Option<String>,
}

impl ReqwestTransport {
    /// Create a production HTTPS-only transport bounded by `budget`.
    pub fn new(budget: Duration) -> Result<Self, HttpError> {
        Self::with_endpoint(budget, None)
    }

    /// Create a transport optionally routed to a loopback fixture endpoint (tests only).
    pub fn with_endpoint(budget: Duration, endpoint: Option<String>) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(endpoint.is_none())
            .build()
            .map_err(|_| HttpError::Network)?;
        Ok(Self {
            client,
            deadline: Instant::now() + budget,
            endpoint,
        })
    }
}

impl HttpTransport for ReqwestTransport {
    fn send<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: Option<&'a [u8]>,
        cap: usize,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>> {
        let method = method.to_owned();
        let url = url.to_owned();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let body = body.map(|b| b.to_vec());
        Box::pin(async move {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpError::Network);
            }
            let destination = self.endpoint.as_deref().unwrap_or(&url);
            let mut request = match method.as_str() {
                "POST" => self.client.post(destination),
                _ => self.client.get(destination),
            }
            .timeout(remaining.min(timeout));
            if self.endpoint.is_some() {
                request = request.header("x-aac-fixture-url", &url);
            }
            for (name, value) in &headers {
                request = request.header(name.as_str(), value.as_str());
            }
            if let Some(bytes) = body {
                request = request.body(bytes);
            }
            let mut response = request.send().await.map_err(|_| HttpError::Network)?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                return Err(HttpError::Redirect);
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut out = Vec::new();
            // Error bodies are deliberately never read, decoded, or logged.
            if status < 400 {
                while let Some(chunk) = response.chunk().await.map_err(|_| HttpError::Network)? {
                    if Instant::now() > self.deadline {
                        return Err(HttpError::Network);
                    }
                    let take = chunk
                        .len()
                        .min(cap.saturating_add(1).saturating_sub(out.len()));
                    out.extend_from_slice(&chunk[..take]);
                    if out.len() > cap {
                        return Err(HttpError::TooLarge);
                    }
                }
            }
            Ok(HttpResponse {
                status,
                retry_after,
                body: out,
            })
        })
    }
}

/// Format Unix milliseconds as RFC 3339 UTC seconds (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn iso_seconds(now_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Format Unix milliseconds as RFC 3339 UTC milliseconds (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
pub fn iso_millis(now_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Parse an ISO-8601 timestamp string (years 2000..=2200) and normalize to UTC milliseconds.
pub fn parse_iso_millis(raw: &str) -> Option<String> {
    if raw.len() > 64 {
        return None;
    }
    let normalized = raw.trim().replace('Z', "+00:00");
    let parsed = chrono::DateTime::parse_from_rfc3339(&normalized).ok()?;
    let utc = parsed.with_timezone(&chrono::Utc);
    let year = chrono::Datelike::year(&utc);
    if !(2000..=2200).contains(&year) {
        return None;
    }
    Some(utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// Parse a `Retry-After` header (seconds or HTTP-date), clamped to `[floor, ceiling]`.
pub fn parse_retry_after(
    value: Option<&str>,
    now_secs: f64,
    floor: f64,
    ceiling: f64,
    default: f64,
) -> f64 {
    let parsed = value
        .filter(|s| s.len() <= 128)
        .and_then(|raw| {
            let trimmed = raw.trim();
            if let Ok(seconds) = trimmed.parse::<f64>()
                && seconds.is_finite()
            {
                return Some(seconds);
            }
            chrono::DateTime::parse_from_rfc2822(trimmed)
                .ok()
                .map(|date| date.timestamp() as f64 - now_secs)
        })
        .filter(|seconds| seconds.is_finite());
    match parsed {
        Some(seconds) => seconds.clamp(floor, ceiling),
        None => default,
    }
}

/// Percent-decode a cookie value (matching Python `urllib.parse.unquote`), rejecting any decoded
/// `\r`, `\n`, or `\0` byte.
pub fn percent_decode_strict(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.bytes().any(|b| matches!(b, 0 | 10 | 13)) {
        return None;
    }
    Some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Current host platform label (`mac`, `windows`, or `ubuntu`).
pub fn host_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(windows) {
        "windows"
    } else {
        "ubuntu"
    }
}

/// Agent verb `web-session`: put/delete/status/collect pasted web sessions on a host.
pub(crate) async fn run_verb(args: &Value) -> Result<Value, Failure> {
    let refused = |msg: &'static str| Failure::new(Cause::Refused, msg);
    let action = args["action"]
        .as_str()
        .ok_or_else(|| refused("missing web-session action"))?;
    let ccs_dir = if let Some(dir) = args.get("ccsDir").and_then(Value::as_str) {
        PathBuf::from(dir)
    } else if let Some(home) = args.get("home").and_then(Value::as_str) {
        PathBuf::from(home).join(".ccs")
    } else {
        std::env::var_os("CCS_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ccs")))
            .ok_or_else(|| refused("cannot resolve CCS directory"))?
    };
    let now_ms = args["nowMs"]
        .as_i64()
        .unwrap_or_else(|| aac_core::UtcMs::now().map_or(0, |t| t.get() as i64));
    let endpoint = args["endpoint"].as_str().map(str::to_owned);

    if action == "status" {
        let map = web_session::summaries(&ccs_dir);
        let mut sessions = serde_json::Map::new();
        for (surface, meta) in map {
            sessions.insert(
                surface.to_owned(),
                meta.map_or(Value::Null, |m| m.public_summary()),
            );
        }
        return Ok(json!({ "ok": true, "sessions": sessions }));
    }

    let surface_word = args["surface"]
        .as_str()
        .ok_or_else(|| refused("missing web-session surface"))?;
    let surface =
        Surface::parse(surface_word).ok_or_else(|| refused("unsupported web-session surface"))?;

    match action {
        "delete" => {
            web_session::delete_as_agent(&ccs_dir, surface)
                .map_err(|_| refused("web session could not be deleted safely"))?;
            Ok(json!({ "ok": true, "removed": true }))
        }
        "put" => {
            let raw = args["session"]
                .as_str()
                .ok_or_else(|| refused("missing pasted session"))?;
            let options = PasteOptions {
                region: args["region"].as_str().map(str::to_owned),
                workspace_id: args["workspaceId"].as_str().map(str::to_owned),
                team_id: args["teamId"].as_str().map(str::to_owned),
                email: args["email"].as_str().map(str::to_owned),
                plan: args["plan"].as_str().map(str::to_owned),
                user_hash: args["userHash"].as_str().map(str::to_owned),
            };
            let parsed = web_session::parse_pasted(surface, raw, &options, &iso_seconds(now_ms))
                .map_err(|_| refused("pasted web session is invalid"))?;
            web_session::save_as_agent(&ccs_dir, surface, &parsed)
                .map_err(|_| refused("web session could not be saved safely"))?;
            Ok(json!({ "ok": true, "session": parsed.meta.public_summary() }))
        }
        "collect" => {
            let stored = web_session::load(&ccs_dir, surface)
                .map_err(|_| refused("web session could not be read safely"))?;
            let Some(stored) = stored else {
                return Ok(json!({ "status": "unavailable", "code": "no_session" }));
            };
            let transport = ReqwestTransport::with_endpoint(Duration::from_secs(20), endpoint)
                .map_err(|_| refused("http transport failed"))?;
            match surface {
                Surface::Qwen => match qwen::collect_stored(&transport, &stored, now_ms).await {
                    Ok(reading) => Ok(qwen::to_account_row(
                        &reading,
                        host_platform(),
                        "Pasted Qwen console session",
                    )),
                    Err(qwen::QwenError::NeedsSignIn(msg)) => {
                        Ok(json!({ "status": "needs_sign_in", "message": msg }))
                    }
                    Err(qwen::QwenError::RateLimited {
                        retry_after_seconds,
                        message,
                    }) => Ok(json!({
                        "status": "error",
                        "failureCode": "rate_limited",
                        "retryAfterSeconds": retry_after_seconds,
                        "message": message
                    })),
                    Err(qwen::QwenError::Unavailable(msg)) => {
                        Ok(json!({ "status": "unavailable", "message": msg }))
                    }
                    Err(qwen::QwenError::ProviderError(msg)) => {
                        Ok(json!({ "status": "error", "message": msg }))
                    }
                },
                Surface::OpencodeWallet => {
                    let Some(cookies) = stored.secret.cookies() else {
                        return Err(refused("invalid opencode session record"));
                    };
                    match opencode::collect(
                        &transport,
                        cookies,
                        stored.meta.workspace_id.as_deref(),
                        now_ms,
                    )
                    .await
                    {
                        Ok(reading) => Ok(opencode::to_account_row(&reading, host_platform())),
                        Err(err) => Ok(opencode::error_json(&err)),
                    }
                }
                Surface::Muse => {
                    let Some(cookies) = stored.secret.cookies() else {
                        return Err(refused("invalid muse session record"));
                    };
                    let binding = muse::MuseBinding {
                        expected_email: stored.meta.email.as_deref(),
                        expected_plan: stored.meta.plan.as_deref(),
                        requested_team: stored.meta.team_id.as_deref(),
                        bound_team: stored.meta.team_id.as_deref(),
                        bound_user_hash: stored.meta.user_hash.as_deref(),
                    };
                    match muse::collect(&transport, cookies, &binding, now_ms).await {
                        Ok(reading) => Ok(muse::to_account_row(&reading, host_platform())),
                        Err(err) => Ok(muse::error_json(&err)),
                    }
                }
            }
        }
        _ => Err(refused("unsupported web-session action")),
    }
}
