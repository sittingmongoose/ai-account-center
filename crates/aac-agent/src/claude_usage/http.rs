//! The fixed Anthropic OAuth routes: no redirects, an aggregate deadline, bounded bodies, and
//! no provider body text past this boundary (`claude_usage.Client`, `failover_core.Client`).
use super::desktop::MAX_BYTES;
use serde_json::Value;
use std::time::{Duration, Instant};

const ORIGIN: &str = "https://api.anthropic.com";
pub(crate) const PROFILE: &str = "/api/oauth/profile";
pub(crate) const USAGE: &str = "/api/oauth/usage?cedar_ember=1";

/// Why a request produced no JSON object: an HTTP status, or none for transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HttpError {
    pub status: Option<u16>,
    pub retry_after: Option<i64>,
    /// The body was too large or not one JSON object.
    pub invalid: bool,
}
impl HttpError {
    fn transport() -> Self {
        Self {
            status: None,
            retry_after: None,
            invalid: false,
        }
    }
}

/// `retry_after_seconds`: delta seconds or an HTTP date, bounded to a day.
pub(crate) fn retry_after(value: Option<&str>, now_ms: i64) -> Option<i64> {
    let value = value.filter(|v| v.len() <= 128)?;
    let trimmed = super::normalize::py_strip(value);
    let seconds = if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let digits = trimmed.trim_start_matches('0');
        if digits.len() > 6 {
            86400
        } else {
            digits.parse::<i64>().unwrap_or(0)
        }
    } else {
        let at = chrono::DateTime::parse_from_rfc2822(value)
            .ok()?
            .timestamp_millis();
        ((at - now_ms) as f64 / 1000.0).ceil() as i64
    };
    Some(seconds.clamp(0, 86400))
}

#[cfg(test)]
thread_local! {
    /// Test-only origin for the fixed routes (a local server on this thread).
    pub(crate) static TEST_ORIGIN: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) struct Client {
    client: reqwest::Client,
    deadline: Instant,
    per_request: Duration,
    origin: String,
}
impl Client {
    pub(crate) fn new(total: Duration, per_request: Duration) -> Option<Self> {
        Some(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()?,
            deadline: Instant::now() + total,
            per_request,
            #[cfg(test)]
            origin: TEST_ORIGIN
                .with(|o| o.borrow().clone())
                .unwrap_or_else(|| ORIGIN.into()),
            #[cfg(not(test))]
            origin: ORIGIN.into(),
        })
    }
    fn remaining(&self) -> Option<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }
    fn allowed(route: &str) -> bool {
        route == PROFILE || route == USAGE || route == "/api/oauth/usage"
    }
    async fn body(&self, mut response: reqwest::Response) -> Result<Vec<u8>, HttpError> {
        let mut bytes = Vec::new();
        loop {
            if Instant::now() >= self.deadline {
                return Err(HttpError::transport());
            }
            let Some(chunk) = response.chunk().await.map_err(|_| HttpError::transport())? else {
                return Ok(bytes);
            };
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err(HttpError {
                    status: Some(200),
                    retry_after: None,
                    invalid: true,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    /// GET one allowed route with the read-only OAuth headers.
    pub(crate) async fn get(&self, route: &str, token: &str) -> Result<Value, HttpError> {
        let Some(remaining) = self.remaining().filter(|_| Self::allowed(route)) else {
            return Err(HttpError::transport());
        };
        let response = self
            .client
            .get(format!("{}{route}", self.origin))
            .timeout(remaining.min(self.per_request))
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "CCS-ReadOnly-Usage/1.0")
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|_| HttpError::transport())?;
        let status = response.status().as_u16();
        let retry = retry_after(
            response
                .headers()
                .get("Retry-After")
                .and_then(|h| h.to_str().ok()),
            aac_core::UtcMs::now().map(|t| t.get() as i64).unwrap_or(0),
        );
        if !(200..300).contains(&status) {
            return Err(HttpError {
                status: Some(status),
                retry_after: retry,
                invalid: false,
            });
        }
        let bytes = self.body(response).await?;
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => Ok(v),
            _ => Err(HttpError {
                status: Some(status),
                retry_after: None,
                invalid: true,
            }),
        }
    }
    /// The one reset claim POST. `Ok((status, body))` for any HTTP answer.
    pub(crate) async fn claim(
        &self,
        organization: &str,
        token: &str,
        body: Vec<u8>,
        user_agent: &str,
    ) -> Result<(u16, Option<Value>, Option<i64>), ()> {
        if !super::desktop::is_uuid(organization) {
            return Err(());
        }
        let response = self
            .client
            .post(format!(
                "{}/api/organizations/{organization}/reset_rate_limits",
                self.origin
            ))
            .timeout(Duration::from_secs(8))
            .header("Authorization", format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", user_agent)
            .body(body)
            .send()
            .await
            .map_err(|_| ())?;
        let status = response.status().as_u16();
        let retry = retry_after(
            response
                .headers()
                .get("Retry-After")
                .and_then(|h| h.to_str().ok()),
            aac_core::UtcMs::now().map(|t| t.get() as i64).unwrap_or(0),
        );
        if !(200..300).contains(&status) {
            return Ok((status, None, retry));
        }
        let mut response = response;
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Ok((status, None, None));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((status, serde_json::from_slice(&bytes).ok(), None))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn retry_after_is_bounded() {
        assert_eq!(retry_after(Some("30"), 0), Some(30));
        assert_eq!(retry_after(Some(" 999999999 "), 0), Some(86400));
        assert_eq!(retry_after(Some("soon"), 0), None);
        assert_eq!(retry_after(None, 0), None);
        let at = chrono::DateTime::parse_from_rfc2822("Thu, 26 Nov 2026 20:00:00 GMT")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            retry_after(Some("Thu, 26 Nov 2026 20:00:00 GMT"), at - 90_500),
            Some(91)
        );
        assert_eq!(
            retry_after(Some("Thu, 26 Nov 2026 20:00:00 GMT"), at + 5),
            Some(0)
        );
    }
}
