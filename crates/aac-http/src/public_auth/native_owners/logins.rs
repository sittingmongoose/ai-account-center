//! GET /api/accounts/logins (Central Logins step 1): every account x computer x tool with its kind, end, renewer and
//! health, read-only. It reads only what AAC already holds: the dashboard cache's account rows and the Claude
//! renewal runner's published report. It never collects, refreshes or writes, and its view type
//! (`aac_api::logins`) has no field for a secret.
use super::NativeOwners;
use crate::public_auth::{Method, Reply, Request, error, json_bytes, lifecycle, now, storage};
use aac_renewal::claude::ClaudeSignInRenewalReader;
use serde_json::Value;

pub(super) const PATH: &str = "/api/accounts/logins";

pub(super) fn route(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) && path == PATH
}

/// The view's bytes from the account rows and the renewal report (`ClaudeSignInRenewal::to_json`).
pub(super) fn render(accounts: &[Value], renewal: Option<&Value>, at: i64) -> Vec<u8> {
    serde_json::to_vec(&aac_views::logins::compose(accounts, renewal, at)).unwrap_or_default()
}

pub(super) async fn handle(native: &NativeOwners, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return lifecycle::options("GET, HEAD", "9");
    }
    if let Err(reply) = lifecycle::admit(&request) {
        return reply;
    }
    if request.uri().query().is_some() {
        return lifecycle::unexpected_query();
    }
    let Some(usage) = &native.usage else {
        return unavailable();
    };
    let cache = usage.cache.clone();
    let root = usage.root.clone();
    let result = tokio::task::spawn_blocking(move || {
        let at = now();
        let stamp = storage::iso(at)?;
        // Cache only: a missing generation lists no accounts rather than starting a collection.
        let accounts = cache.merged_accounts(&stamp, at).unwrap_or_default();
        let renewal = ClaudeSignInRenewalReader::new(root, Default::default())
            .read()
            .map(|report| report.to_json());
        Some(render(&accounts, renewal.as_ref(), at))
    })
    .await;
    let Ok(Some(bytes)) = result else {
        return unavailable();
    };
    super::no_store(json_bytes(200, bytes))
}

fn unavailable() -> Reply {
    error(
        500,
        "internal_error",
        "The request could not be completed safely.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_route_reads_only() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(route(&method, PATH));
        }
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!route(&method, PATH));
        }
        assert!(!route(&Method::GET, "/api/accounts/logins/x"));
    }

    /// The bytes the route answers with carry no secret any input held: only the allowlisted row fields.
    #[test]
    fn the_reply_never_carries_a_secret_field() {
        let token = "sk-ant-oat01-ROUTESECRET";
        let renewal = json!({"runner": "mac", "generatedAt": "2026-11-26T18:50:30.000Z", "stale": false,
            "needsYou": [], "signIns": [{"host": "mac", "profile": "me", "email": "a@example.com",
            "state": "ok", "accessToken": token, "refreshToken": token, "configDir": "/Users/u/.claude"}]});
        let accounts = [
            json!({"id": "codex:1", "provider": "codex", "email": "b@example.com",
            "host": "ubuntu", "status": "ok", "tokens": {"access_token": token}, "authPath": "/home/u/.codex",
            "cookie": "ROUTESECRET", "apiKey": token, "label": token}),
        ];
        let bytes = render(&accounts, Some(&renewal), 1_795_000_000_000);
        let text = String::from_utf8(bytes).expect("test value");
        for leak in [
            "ROUTESECRET",
            "sk-ant",
            "token",
            "Token",
            "cookie",
            "apiKey",
            "/Users/",
            "/home/",
            ".codex",
            ".claude",
        ] {
            assert!(!text.contains(leak), "{leak} leaked: {text}");
        }
        let view: Value = serde_json::from_str(&text).expect("test value");
        assert_eq!(view["rows"].as_array().map(Vec::len), Some(2));
    }
}
