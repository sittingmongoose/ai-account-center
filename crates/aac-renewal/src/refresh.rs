//! The Codex CLI's refresh-token grant for saved logins
//! (`src/codex-auth/codex-token-refresh.ts`). The endpoint is fixed: no
//! environment variable can send a refresh token elsewhere (tests inject the
//! fetch seam). Results carry enumerated codes only; response bodies and
//! parser errors never leave this module.

use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The fixed refresh endpoint.
pub const CODEX_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
/// Codex's public OAuth client id.
pub const CODEX_REFRESH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Long enough that a slow answer is still received: once OpenAI has rotated
/// the token, a response dropped on timeout leaves the saved login dead.
pub const CODEX_REFRESH_TIMEOUT_MS: u64 = 20_000;
const MAX_RESPONSE_CHARS: usize = 256 * 1024;

/// Codex's permanent refresh failures, plus the OAuth form of a rejected grant.
const DEAD_CODES: [&str; 4] = [
    "refresh_token_expired",
    "refresh_token_reused",
    "refresh_token_invalidated",
    "invalid_grant",
];

/// The rotated tokens. In-memory only: never log, persist or return them past
/// the renewal write.
#[derive(Debug, Clone)]
pub struct CodexRefreshedTokens {
    /// New id token, when the response carried one.
    pub id_token: Option<String>,
    /// New access token (required).
    pub access_token: String,
    /// New refresh token, when the response rotated one.
    pub refresh_token: Option<String>,
}

/// One refresh request's outcome, enumerated.
#[derive(Debug, Clone)]
pub enum CodexTokenRefreshResult {
    /// A usable answer.
    Ok(CodexRefreshedTokens),
    /// OpenAI rejected the grant permanently.
    Dead {
        /// The known dead code, or `unauthorized` for any other 401.
        code: String,
        /// HTTP status.
        http_status: u16,
    },
    /// A retryable transport or server outcome.
    Transient {
        /// timeout / network / rate_limited / server_error / http_error.
        code: String,
        /// HTTP status, when one arrived.
        http_status: Option<u16>,
    },
    /// A success status with an unusable body.
    InvalidResponse {
        /// HTTP status.
        http_status: u16,
    },
}

/// The request the seam receives (the body is the exact JSON the TS sends).
#[derive(Debug, Clone)]
pub struct RefreshCall {
    /// Always `CODEX_REFRESH_URL`.
    pub url: String,
    /// Content-Type and Accept, as the TS init.
    pub headers: Vec<(String, String)>,
    /// The JSON grant body.
    pub body: String,
}

/// The seam's answer: a response, a timeout, or a network failure.
#[derive(Debug, Clone)]
pub enum FetchReply {
    /// A response arrived.
    Response {
        /// HTTP status.
        status: u16,
        /// The body text.
        text: String,
    },
    /// AbortSignal.timeout fired (TimeoutError/AbortError in TS).
    Timeout,
    /// Any other send failure.
    Network,
}

/// The injectable fetch seam.
pub type CodexTokenFetch =
    Arc<dyn Fn(RefreshCall) -> Pin<Box<dyn Future<Output = FetchReply> + Send>> + Send + Sync>;

fn parse_object(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.is_object() { Some(value) } else { None }
}

/// A known permanent code from `error`, `error.code` or `code`; never raw text.
fn dead_code(body: Option<&Value>) -> Option<&'static str> {
    let body = body?;
    let error = body.get("error");
    let candidates = [
        error.and_then(Value::as_str),
        error
            .and_then(Value::as_object)
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str),
        body.get("code").and_then(Value::as_str),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(known) = DEAD_CODES.iter().find(|code| **code == candidate) {
            return Some(known);
        }
    }
    None
}

/// undefined/null -> absent; a non-empty string -> present; anything else -> invalid.
fn optional_token(value: Option<&Value>) -> Result<Option<String>, ()> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(()),
    }
}

/// JS `text.length`: UTF-16 code units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// One refresh request. Never panics; a lost response is transient.
pub async fn refresh_codex_tokens(
    refresh_token: &str,
    fetch: &CodexTokenFetch,
    timeout_ms: Option<u64>,
) -> CodexTokenRefreshResult {
    let mut body = Map::new();
    body.insert(
        "client_id".into(),
        Value::String(CODEX_REFRESH_CLIENT_ID.into()),
    );
    body.insert("grant_type".into(), Value::String("refresh_token".into()));
    body.insert(
        "refresh_token".into(),
        Value::String(refresh_token.to_owned()),
    );
    body.insert("scope".into(), Value::String("openid profile email".into()));
    let call = RefreshCall {
        url: CODEX_REFRESH_URL.into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Accept".into(), "application/json".into()),
        ],
        body: Value::Object(body).to_string(),
    };
    let _ = timeout_ms; // the seam owns its deadline; production applies it below
    let reply = fetch(call).await;
    let (status, text) = match reply {
        FetchReply::Response { status, text } => (status, text),
        FetchReply::Timeout => {
            return CodexTokenRefreshResult::Transient {
                code: "timeout".into(),
                http_status: None,
            };
        }
        FetchReply::Network => {
            return CodexTokenRefreshResult::Transient {
                code: "network".into(),
                http_status: None,
            };
        }
    };
    if (200..300).contains(&status) {
        let body = if utf16_len(&text) > MAX_RESPONSE_CHARS {
            None
        } else {
            parse_object(&text)
        };
        let access = optional_token(body.as_ref().and_then(|b| b.get("access_token")));
        let id = optional_token(body.as_ref().and_then(|b| b.get("id_token")));
        let refreshed = optional_token(body.as_ref().and_then(|b| b.get("refresh_token")));
        let (Ok(Some(access_token)), Ok(id_token), Ok(refresh_token)) = (access, id, refreshed)
        else {
            return CodexTokenRefreshResult::InvalidResponse {
                http_status: status,
            };
        };
        return CodexTokenRefreshResult::Ok(CodexRefreshedTokens {
            id_token,
            access_token,
            refresh_token,
        });
    }
    if status == 400 || status == 401 {
        let body = if utf16_len(&text) > MAX_RESPONSE_CHARS {
            None
        } else {
            parse_object(&text)
        };
        // Codex treats every 401 from this endpoint as permanent; a 400 only
        // with a known code.
        if let Some(code) = dead_code(body.as_ref()) {
            return CodexTokenRefreshResult::Dead {
                code: code.to_owned(),
                http_status: status,
            };
        }
        if status == 401 {
            return CodexTokenRefreshResult::Dead {
                code: "unauthorized".into(),
                http_status: status,
            };
        }
    }
    if status == 429 {
        return CodexTokenRefreshResult::Transient {
            code: "rate_limited".into(),
            http_status: Some(status),
        };
    }
    if status >= 500 {
        return CodexTokenRefreshResult::Transient {
            code: "server_error".into(),
            http_status: Some(status),
        };
    }
    CodexTokenRefreshResult::Transient {
        code: "http_error".into(),
        http_status: Some(status),
    }
}

/// The production fetch: reqwest with redirects refused (a redirect would
/// resend the refresh token to another URL) and the fixed timeout.
pub fn production_fetch(timeout_ms: Option<u64>) -> CodexTokenFetch {
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(CODEX_REFRESH_TIMEOUT_MS));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .ok();
    Arc::new(move |call: RefreshCall| {
        let client = client.clone();
        Box::pin(async move {
            let Some(client) = client else {
                return FetchReply::Network;
            };
            let mut request = client.post(&call.url).body(call.body);
            for (name, value) in &call.headers {
                request = request.header(name.as_str(), value.as_str());
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    match response.text().await {
                        Ok(text) => FetchReply::Response { status, text },
                        Err(error) if error.is_timeout() => FetchReply::Timeout,
                        Err(_) => FetchReply::Network,
                    }
                }
                Err(error) if error.is_timeout() => FetchReply::Timeout,
                Err(_) => FetchReply::Network,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seam(reply: FetchReply) -> CodexTokenFetch {
        Arc::new(move |_call| {
            let reply = reply.clone();
            Box::pin(async move { reply })
        })
    }

    #[tokio::test]
    async fn success_and_invalid_bodies() {
        let ok = serde_json::json!({"access_token": "at", "id_token": "id", "refresh_token": "rt"});
        let result = refresh_codex_tokens(
            "rt-old",
            &seam(FetchReply::Response {
                status: 200,
                text: ok.to_string(),
            }),
            None,
        )
        .await;
        assert!(matches!(result, CodexTokenRefreshResult::Ok(_)));
        let result = refresh_codex_tokens(
            "rt-old",
            &seam(FetchReply::Response {
                status: 200,
                text: "{\"access_token\": ".into(),
            }),
            None,
        )
        .await;
        assert!(matches!(
            result,
            CodexTokenRefreshResult::InvalidResponse { http_status: 200 }
        ));
        // A non-string token field is invalid, an absent one is fine.
        let bad = serde_json::json!({"access_token": "at", "id_token": 5});
        let result = refresh_codex_tokens(
            "rt-old",
            &seam(FetchReply::Response {
                status: 200,
                text: bad.to_string(),
            }),
            None,
        )
        .await;
        assert!(matches!(
            result,
            CodexTokenRefreshResult::InvalidResponse { .. }
        ));
        let minimal = serde_json::json!({"access_token": "at"});
        let result = refresh_codex_tokens(
            "rt-old",
            &seam(FetchReply::Response {
                status: 200,
                text: minimal.to_string(),
            }),
            None,
        )
        .await;
        assert!(matches!(result, CodexTokenRefreshResult::Ok(_)));
    }

    #[tokio::test]
    async fn dead_and_transient_codes() {
        for (status, body, expected) in [
            (
                401,
                "{\"error\":{\"code\":\"refresh_token_reused\"}}",
                "refresh_token_reused",
            ),
            (
                401,
                "{\"error\":\"refresh_token_expired\"}",
                "refresh_token_expired",
            ),
            (400, "{\"error\":\"invalid_grant\"}", "invalid_grant"),
            (401, "{}", "unauthorized"),
        ] {
            let result = refresh_codex_tokens(
                "rt",
                &seam(FetchReply::Response {
                    status,
                    text: body.into(),
                }),
                None,
            )
            .await;
            match result {
                CodexTokenRefreshResult::Dead { code, .. } => assert_eq!(code, expected),
                other => panic!("expected dead, got {other:?}"),
            }
        }
        // A 400 without a known code stays transient.
        let result = refresh_codex_tokens(
            "rt",
            &seam(FetchReply::Response {
                status: 400,
                text: "{}".into(),
            }),
            None,
        )
        .await;
        assert!(
            matches!(result, CodexTokenRefreshResult::Transient { code, .. } if code == "http_error")
        );
        let result = refresh_codex_tokens(
            "rt",
            &seam(FetchReply::Response {
                status: 429,
                text: String::new(),
            }),
            None,
        )
        .await;
        assert!(
            matches!(result, CodexTokenRefreshResult::Transient { code, .. } if code == "rate_limited")
        );
        let result = refresh_codex_tokens(
            "rt",
            &seam(FetchReply::Response {
                status: 503,
                text: "upstream".into(),
            }),
            None,
        )
        .await;
        assert!(
            matches!(result, CodexTokenRefreshResult::Transient { code, .. } if code == "server_error")
        );
        let result = refresh_codex_tokens("rt", &seam(FetchReply::Timeout), None).await;
        assert!(
            matches!(result, CodexTokenRefreshResult::Transient { code, http_status: None } if code == "timeout")
        );
        let result = refresh_codex_tokens("rt", &seam(FetchReply::Network), None).await;
        assert!(
            matches!(result, CodexTokenRefreshResult::Transient { code, .. } if code == "network")
        );
    }
}
