//! Bounded HTTP transport and conversions matching `scripts/account-usage/desktop_helpers.py`.
use crate::desktop_usage::{
    Credential, DesktopTransport, HTTP_TIMEOUT_SECS, MAX_RESPONSE_BYTES, UsageError,
};
use crate::failover_additional::desktop;
use serde_json::Value;
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) fn to_internal_cred(c: &Credential) -> desktop::Credential {
    desktop::Credential {
        access: c.access.clone(),
        source: c.source.clone(),
        refresh: c.refresh.clone(),
        expires: c.expires,
        email: c.email.clone(),
        project: c.project.clone(),
        plan: c.plan.clone(),
        google_client: c.google_client.clone(),
    }
}

pub(crate) fn from_internal_cred(c: desktop::Credential) -> Credential {
    Credential {
        access: c.access,
        source: c.source,
        refresh: c.refresh,
        expires: c.expires,
        email: c.email,
        project: c.project,
        plan: c.plan,
        google_client: c.google_client,
    }
}

pub(crate) fn from_internal_err(e: desktop::UsageError) -> UsageError {
    UsageError {
        status: e.status,
        message: e.message,
        failure_code: e.failure_code,
        retry_after: e.retry_after,
    }
}

fn encode_form_body(body: &Value) -> Result<String, UsageError> {
    let Some(obj) = body.as_object() else {
        return Err(UsageError::new(
            "error",
            "The usage service returned an unsupported response.",
        ));
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in obj {
        let text = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => {
                return Err(UsageError::new(
                    "error",
                    "The usage service returned an unsupported response.",
                ));
            }
        };
        serializer.append_pair(k, &text);
    }
    Ok(serializer.finish())
}

/// Production `reqwest`-backed `DesktopTransport` matching `desktop_helpers.request_json`.
pub struct ReqwestDesktopTransport {
    client: reqwest::Client,
    now_secs: Option<f64>,
}

impl ReqwestDesktopTransport {
    /// Create a new production desktop HTTP transport with redirects disabled.
    pub fn new(now_secs: Option<f64>) -> Result<Self, UsageError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                UsageError::new(
                    "error",
                    "The usage service could not be reached within its time limit.",
                )
            })?;
        Ok(Self { client, now_secs })
    }
}

impl DesktopTransport for ReqwestDesktopTransport {
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a Value>,
        form: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, UsageError>> + Send + 'a>> {
        Box::pin(async move {
            let mut has_ua = false;
            let mut req = if body.is_some() {
                self.client.post(url)
            } else {
                self.client.get(url)
            };
            req = req.timeout(Duration::from_secs(HTTP_TIMEOUT_SECS));
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("user-agent") {
                    has_ua = true;
                }
                req = req.header(k.as_str(), v.as_str());
            }
            req = req.header("Accept", "application/json");
            if !has_ua {
                req = req.header("User-Agent", "CCS-Account-Usage/1.0");
            }
            if let Some(payload) = body {
                if form {
                    let encoded = encode_form_body(payload)?;
                    req = req
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(encoded);
                } else {
                    let encoded = serde_json::to_string(payload).map_err(|_| {
                        UsageError::new(
                            "error",
                            "The usage service returned an unsupported response.",
                        )
                    })?;
                    req = req.header("Content-Type", "application/json").body(encoded);
                }
            }
            let mut resp = req.send().await.map_err(|_| {
                UsageError::new(
                    "error",
                    "The usage service could not be reached within its time limit.",
                )
            })?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                return Err(UsageError::new(
                    "error",
                    "The usage service returned an unexpected redirect.",
                ));
            }
            if status == 429 {
                let hdr_val = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| Value::String(s.to_owned()))
                    .unwrap_or(Value::Null);
                let retry = desktop::desktop_retry_after_seconds(&hdr_val, self.now_secs);
                return Err(UsageError {
                    status: "error",
                    message: "The usage service is limiting requests. Refresh resumes after a cooldown.",
                    failure_code: Some("rate_limited"),
                    retry_after: Some(retry),
                });
            }
            if status == 401 || status == 403 {
                return Err(UsageError::new(
                    "needs_sign_in",
                    "The saved account could not authenticate with the usage service.",
                ));
            }
            if status == 400 {
                // `desktop_helpers.is_invalid_grant`: read at most 4096 bytes.
                let mut err_body = Vec::new();
                while let Ok(Some(chunk)) = resp.chunk().await {
                    let remaining = 4096usize.saturating_sub(err_body.len());
                    if remaining == 0 {
                        break;
                    }
                    let take = chunk.len().min(remaining);
                    err_body.extend_from_slice(&chunk[..take]);
                    if err_body.len() >= 4096 {
                        break;
                    }
                }
                if desktop::is_invalid_grant(&err_body) {
                    return Err(UsageError::new(
                        "needs_sign_in",
                        "The saved account's sign-in has expired or was revoked.",
                    ));
                }
                return Err(UsageError::new(
                    "error",
                    "The usage service is temporarily unavailable.",
                ));
            }
            if !(200..300).contains(&status) {
                return Err(UsageError::new(
                    "error",
                    "The usage service is temporarily unavailable.",
                ));
            }
            let mut raw = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| {
                UsageError::new(
                    "error",
                    "The usage service could not be reached within its time limit.",
                )
            })? {
                let take = chunk
                    .len()
                    .min((MAX_RESPONSE_BYTES + 1).saturating_sub(raw.len()));
                raw.extend_from_slice(&chunk[..take]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return Err(UsageError::new(
                        "error",
                        "The usage service response exceeded its size limit.",
                    ));
                }
            }
            let val: Value = serde_json::from_slice(&raw).map_err(|_| {
                UsageError::new(
                    "error",
                    "The usage service returned an unsupported response.",
                )
            })?;
            if !val.is_object() {
                return Err(UsageError::new(
                    "error",
                    "The usage service returned an unsupported response.",
                ));
            }
            Ok(val)
        })
    }
}
