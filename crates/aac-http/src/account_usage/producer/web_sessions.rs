//! Local in-process collection for pasted web sessions (Qwen Token Plan, OpenCode Zen wallet,
//! and Muse Code portal quota), with per-surface caching, 429 cooldowns, and honest expiry
//! (`needs_sign_in` retaining the last cached windows).

use crate::account_usage::{rows, sources::Source};
use aac_agent::web_session::{
    ReqwestTransport, host_platform, iso_seconds,
    muse::{self, MuseBinding, MuseError},
    opencode::{self, OpenCodeError},
    qwen::{self, QwenError},
};
use aac_files::web_session::{self, Surface};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Mutex, time::Duration};

#[derive(Clone, Default)]
struct SurfaceCache {
    fingerprint: String,
    checked_at_ms: i64,
    cooldown_until_ms: i64,
    last_good: Option<Value>,
}

/// Shared per-surface cache, refresh budget, and 429 cooldown state held by [`super::Producer`].
/// One OpenCode wallet session per account, so wallet entries are keyed by
/// `(surface, account)`; Qwen and Muse keep one global entry each.
#[derive(Default)]
pub(crate) struct WebSessionState {
    entries: Mutex<HashMap<(Surface, Option<String>), SurfaceCache>>,
    endpoint: Mutex<Option<String>>,
}

type CacheKey = (Surface, Option<String>);

impl WebSessionState {
    /// Clear cached state and cooldown for one surface (called after PUT or DELETE).
    /// `account` is `Some` for one OpenCode Go account's wallet, `None` otherwise.
    pub(crate) fn clear(&self, surface: Surface, account: Option<&str>) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&(surface, account.map(str::to_owned)));
    }

    /// Override the upstream HTTPS target with a loopback fixture URL (tests only).
    #[cfg(test)]
    pub(crate) fn set_endpoint(&self, endpoint: Option<String>) {
        let mut guard = self
            .endpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = endpoint;
    }

    fn endpoint(&self) -> Option<String> {
        self.endpoint
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn snapshot(&self, surface: Surface, account: Option<&str>, fingerprint: &str) -> SurfaceCache {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key: CacheKey = (surface, account.map(str::to_owned));
        match guard.get(&key) {
            Some(entry) if entry.fingerprint == fingerprint => entry.clone(),
            _ => SurfaceCache {
                fingerprint: fingerprint.to_owned(),
                ..Default::default()
            },
        }
    }

    fn record_ok(
        &self,
        surface: Surface,
        account: Option<&str>,
        fingerprint: &str,
        now_ms: i64,
        row: Value,
    ) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key: CacheKey = (surface, account.map(str::to_owned));
        guard.insert(
            key,
            SurfaceCache {
                fingerprint: fingerprint.to_owned(),
                checked_at_ms: now_ms,
                cooldown_until_ms: 0,
                last_good: Some(row),
            },
        );
    }

    fn record_cooldown(
        &self,
        surface: Surface,
        account: Option<&str>,
        fingerprint: &str,
        until_ms: i64,
    ) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key: CacheKey = (surface, account.map(str::to_owned));
        let entry = guard.entry(key).or_default();
        if entry.fingerprint != fingerprint {
            *entry = SurfaceCache {
                fingerprint: fingerprint.to_owned(),
                ..Default::default()
            };
        }
        entry.cooldown_until_ms = entry.cooldown_until_ms.max(until_ms);
    }
}

fn build_transport(state: &WebSessionState) -> Option<ReqwestTransport> {
    ReqwestTransport::with_endpoint(Duration::from_secs(12), state.endpoint()).ok()
}

fn with_cached_status(mut cached: Value, status: &str, fetched_at: &str, message: &str) -> Value {
    cached["status"] = json!(status);
    cached["fetchedAt"] = json!(fetched_at);
    cached["message"] = json!(aac_core::scrub(message));
    cached
}

fn apply_source_identity(mut row: Value, source: &Source) -> Value {
    row["id"] = json!(source.id);
    if let Some(label) = &source.label {
        row["label"] = json!(label);
    }
    row
}

/// Collect Qwen Token Plan usage in-process from the pasted web session in `root`.
/// Missing or unreadable sessions report `needs_sign_in`; no legacy bridge is consulted.
pub(super) async fn collect_qwen(
    root: &Path,
    state: &WebSessionState,
    source: &Source,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let now_iso = iso_seconds(now_ms);
    let platform = host_platform();
    let root_buf = root.to_path_buf();
    let stored = match tokio::task::spawn_blocking(move || {
        web_session::load(&root_buf, Surface::Qwen)
    })
    .await
    {
        Ok(Ok(Some(stored))) => stored,
        _ => {
            return Some(qwen_error_row(
                source,
                platform,
                "needs_sign_in",
                "Paste a Qwen console session to refresh usage.",
                &now_iso,
            ));
        }
    };
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::Qwen, None, &fp);

    if !force && now_ms < cached.cooldown_until_ms {
        if let Some(prev) = cached.last_good {
            return Some(apply_source_identity(
                with_cached_status(
                    prev,
                    "cached",
                    &now_iso,
                    "Qwen rate limited the usage check; showing the last cached reading.",
                ),
                source,
            ));
        }
        return Some(qwen_error_row(
            source,
            platform,
            "error",
            "Qwen rate limited the usage check; try again later.",
            &now_iso,
        ));
    }

    let Some(transport) = build_transport(state) else {
        return Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(
                    prev,
                    "cached",
                    &now_iso,
                    "HTTP client unavailable; showing the last cached Qwen reading.",
                ),
                source,
            ),
            None => qwen_error_row(
                source,
                platform,
                "error",
                "HTTP client unavailable for Qwen usage check.",
                &now_iso,
            ),
        });
    };

    match qwen::collect_stored(&transport, &stored, now_ms).await {
        Ok(reading) => {
            let mut row = qwen::to_account_row(&reading, platform, "Pasted Qwen console session");
            row = apply_source_identity(row, source);
            state.record_ok(Surface::Qwen, None, &fp, now_ms, row.clone());
            Some(row)
        }
        Err(QwenError::NeedsSignIn(message)) => Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(prev, "needs_sign_in", &now_iso, &message),
                source,
            ),
            None => qwen_error_row(source, platform, "needs_sign_in", &message, &now_iso),
        }),
        Err(QwenError::RateLimited {
            retry_after_seconds,
            message,
        }) => {
            let until = now_ms.saturating_add((retry_after_seconds * 1000.0) as i64);
            state.record_cooldown(Surface::Qwen, None, &fp, until);
            Some(match cached.last_good {
                Some(prev) => apply_source_identity(
                    with_cached_status(prev, "cached", &now_iso, &message),
                    source,
                ),
                None => qwen_error_row(source, platform, "error", &message, &now_iso),
            })
        }
        Err(QwenError::Unavailable(message)) => Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(prev, "cached", &now_iso, &message),
                source,
            ),
            None => qwen_error_row(source, platform, "unavailable", &message, &now_iso),
        }),
        Err(QwenError::ProviderError(message)) => Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(
                    prev,
                    "cached",
                    &now_iso,
                    "Showing the last cached Qwen reading; live check failed.",
                ),
                source,
            ),
            None => qwen_error_row(source, platform, "error", &message, &now_iso),
        }),
    }
}

fn qwen_error_row(
    source: &Source,
    platform: &str,
    status: &str,
    message: &str,
    fetched_at: &str,
) -> Value {
    json!({
        "id": source.id,
        "provider": "qwen",
        "providerLabel": "Qwen Token Plan",
        "label": source.label.as_deref().unwrap_or("Qwen Token Plan"),
        "email": null,
        "plan": null,
        "platform": platform,
        "source": "Pasted Qwen console session",
        "status": status,
        "message": aac_core::scrub(message),
        "fetchedAt": fetched_at,
        "sampledAt": null,
        "isActive": false,
        "windows": [],
        "capabilities": rows::capabilities(),
    })
}

/// Collect one OpenCode Go account's Zen prepaid wallet balance window, or `None`.
///
/// There is no standalone wallet row: the window rides inside the account's own
/// card as a `balance` line. An account without a pasted console session gets no
/// line (never an invented `$0`); a stale or failed read with a cached balance
/// shows the cached window, a failed read with none shows an unavailable line
/// (no number), and a session whose workspace cannot be matched to the account
/// shows nothing and logs a plain reason.
pub(super) async fn collect_wallet_window(
    root: &Path,
    state: &WebSessionState,
    source: &Source,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let account = source.id.as_str();
    let root_buf = root.to_path_buf();
    let account_buf = account.to_owned();
    let stored = match tokio::task::spawn_blocking(move || {
        web_session::load_for_account(&root_buf, Surface::OpencodeWallet, &account_buf)
    })
    .await
    {
        Ok(Ok(Some(stored))) => stored,
        _ => return None,
    };
    let workspace = stored.meta.workspace_id.clone();
    let Some(workspace) = workspace else {
        tracing::warn!(
            account,
            "OpenCode wallet session names no workspace; showing no wallet line",
        );
        return None;
    };
    let cookies = stored.secret.cookies()?.to_vec();
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::OpencodeWallet, Some(account), &fp);

    let missing = |reason: &str| -> Option<Value> {
        tracing::warn!(account, reason);
        None
    };
    // A failed read of a pasted session: the last balance, cached, else an
    // unavailable line inside the card (never a guessed number).
    let failed = |reason: &str| -> Option<Value> {
        cached
            .last_good
            .as_ref()
            .and_then(|window| wallet_window(window.clone(), true))
            .or_else(|| {
                tracing::warn!(account, reason);
                Some(unavailable_wallet_window())
            })
    };

    if !force && now_ms < cached.cooldown_until_ms {
        return failed("OpenCode console rate limited the wallet check");
    }

    let Some(transport) = build_transport(state) else {
        return failed("OpenCode console wallet check has no HTTP client");
    };

    match opencode::collect(&transport, &cookies, Some(&workspace), now_ms).await {
        Ok(reading) => {
            let Some(window) = reading
                .windows
                .iter()
                .find(|w| w["key"] == "zen-balance")
                .cloned()
            else {
                return missing("OpenCode console reading carries no wallet balance");
            };
            let mut stored_window = window;
            stored_window["label"] = json!("Wallet");
            stored_window["sampledAt"] = json!(reading.sampled_at);
            state.record_ok(
                Surface::OpencodeWallet,
                Some(account),
                &fp,
                now_ms,
                stored_window.clone(),
            );
            wallet_window(stored_window, false)
        }
        Err(OpenCodeError::NeedsSignIn) => {
            failed("The pasted OpenCode console session has expired; paste a fresh one")
        }
        Err(OpenCodeError::RateLimited {
            retry_after_seconds,
        }) => {
            let secs = retry_after_seconds.unwrap_or(60.0).clamp(30.0, 86_400.0);
            state.record_cooldown(
                Surface::OpencodeWallet,
                Some(account),
                &fp,
                now_ms.saturating_add((secs * 1000.0) as i64),
            );
            failed("OpenCode console rate limited the wallet check")
        }
        Err(OpenCodeError::WorkspaceMismatch | OpenCodeError::ChooseWorkspace(_)) => missing(
            "The saved OpenCode workspace no longer matches this account; showing no wallet line",
        ),
        Err(OpenCodeError::WalletNotPrepaid) => {
            missing("The saved OpenCode workspace is not a prepaid Zen wallet")
        }
        Err(
            OpenCodeError::InvalidResponse
            | OpenCodeError::ProviderError
            | OpenCodeError::NetworkError,
        ) => failed("The OpenCode console wallet could not be read"),
    }
}

/// The `Wallet` line of a card whose pasted session could not be read and has no
/// cached balance: the reading contract's unavailable window, every number null.
fn unavailable_wallet_window() -> Value {
    json!({
        "key": "zen-balance",
        "label": "Wallet",
        "kind": "balance",
        "unit": "USD",
        "usedPercent": null,
        "remainingPercent": null,
        "resetAt": null,
        "windowMinutes": null,
        "used": null,
        "limit": null,
        "remaining": null,
        "status": "unavailable",
    })
}

/// One normalized `Wallet` balance window for an OpenCode Go account card.
/// A stale replay keeps its measured balance with the cached state and its
/// original sample stamp.
fn wallet_window(window: Value, stale: bool) -> Option<Value> {
    let mut window = window;
    if stale {
        window["status"] = json!("cached");
    }
    rows::windows(&json!([window]), 1).pop()
}

/// Collect Muse Code portal quota in-process from the pasted web session in `root`.
/// Missing or unreadable sessions report `needs_sign_in`; no legacy bridge is consulted.
pub(super) async fn collect_muse(
    root: &Path,
    home: Option<&Path>,
    state: &WebSessionState,
    source: &Source,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let now_iso = iso_seconds(now_ms);
    let platform = host_platform();
    let root_buf = root.to_path_buf();
    let stored = match tokio::task::spawn_blocking(move || {
        web_session::load(&root_buf, Surface::Muse)
    })
    .await
    {
        Ok(Ok(Some(stored))) => stored,
        _ => {
            return Some(muse_error_row(
                source,
                None,
                None,
                platform,
                "needs_sign_in",
                "Paste a Muse Code portal session to refresh usage.",
                &now_iso,
            ));
        }
    };
    let cookies = stored.secret.cookies()?.to_vec();
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::Muse, None, &fp);

    if !force && now_ms < cached.cooldown_until_ms {
        return Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(
                    prev,
                    "cached",
                    &now_iso,
                    "Muse Code rate limited the check; showing the last cached reading.",
                ),
                source,
            ),
            None => muse_error_row(
                source,
                stored.meta.email.as_deref(),
                stored.meta.plan.as_deref(),
                platform,
                "error",
                "Muse Code rate limited the check; try again later.",
                &now_iso,
            ),
        });
    }

    let refresh_budget_ms = (muse::REFRESH_SECONDS as i64).saturating_mul(1000);
    if !force
        && let Some(prev) = cached.last_good.as_ref()
        && now_ms.saturating_sub(cached.checked_at_ms) < refresh_budget_ms
    {
        return Some(apply_source_identity(prev.clone(), source));
    }

    let Some(transport) = build_transport(state) else {
        return Some(match cached.last_good {
            Some(prev) => apply_source_identity(
                with_cached_status(
                    prev,
                    "cached",
                    &now_iso,
                    "HTTP client unavailable; showing the last cached Muse reading.",
                ),
                source,
            ),
            None => muse_error_row(
                source,
                stored.meta.email.as_deref(),
                stored.meta.plan.as_deref(),
                platform,
                "error",
                "HTTP client unavailable for Muse Code check.",
                &now_iso,
            ),
        });
    };

    let local_email = if stored.meta.email.is_none() {
        home.and_then(muse::read_local_cli_email)
    } else {
        None
    };
    let expected_email = stored.meta.email.as_deref().or(local_email.as_deref());
    let binding = MuseBinding {
        expected_email,
        expected_plan: stored.meta.plan.as_deref(),
        requested_team: stored.meta.team_id.as_deref(),
        bound_team: stored.meta.team_id.as_deref(),
        bound_user_hash: stored.meta.user_hash.as_deref(),
    };

    match muse::collect(&transport, &cookies, &binding, now_ms).await {
        Ok(reading) => {
            let mut row = muse::to_account_row(&reading, platform);
            row = apply_source_identity(row, source);
            state.record_ok(Surface::Muse, None, &fp, now_ms, row.clone());
            Some(row)
        }
        Err(MuseError::NeedsSignIn) => {
            let msg = "The pasted Muse Code portal session has expired. Paste a fresh session.";
            Some(match cached.last_good {
                Some(prev) => apply_source_identity(
                    with_cached_status(prev, "needs_sign_in", &now_iso, msg),
                    source,
                ),
                None => muse_error_row(
                    source,
                    expected_email,
                    stored.meta.plan.as_deref(),
                    platform,
                    "needs_sign_in",
                    msg,
                    &now_iso,
                ),
            })
        }
        Err(
            MuseError::AccountMismatch
            | MuseError::PlanMismatch
            | MuseError::TeamMismatch
            | MuseError::ChooseTeam(_)
            | MuseError::IdentityUnavailable,
        ) => {
            let msg = "The pasted Muse Code session no longer matches the bound account or team. Paste a fresh session.";
            Some(match cached.last_good {
                Some(prev) => apply_source_identity(
                    with_cached_status(prev, "needs_sign_in", &now_iso, msg),
                    source,
                ),
                None => muse_error_row(
                    source,
                    expected_email,
                    stored.meta.plan.as_deref(),
                    platform,
                    "needs_sign_in",
                    msg,
                    &now_iso,
                ),
            })
        }
        Err(MuseError::RateLimited {
            retry_after_seconds,
        }) => {
            state.record_cooldown(
                Surface::Muse,
                None,
                &fp,
                now_ms.saturating_add((retry_after_seconds * 1000.0) as i64),
            );
            let msg = "Muse Code rate limited the check; showing the last cached reading.";
            Some(match cached.last_good {
                Some(prev) => {
                    apply_source_identity(with_cached_status(prev, "cached", &now_iso, msg), source)
                }
                None => muse_error_row(
                    source,
                    expected_email,
                    stored.meta.plan.as_deref(),
                    platform,
                    "error",
                    "Muse Code rate limited the check; try again later.",
                    &now_iso,
                ),
            })
        }
        Err(MuseError::InvalidResponse | MuseError::ProviderError | MuseError::NetworkError) => {
            Some(match cached.last_good {
                Some(prev) => apply_source_identity(
                    with_cached_status(
                        prev,
                        "cached",
                        &now_iso,
                        "Showing the last cached Muse Code reading; live check failed.",
                    ),
                    source,
                ),
                None => muse_error_row(
                    source,
                    expected_email,
                    stored.meta.plan.as_deref(),
                    platform,
                    "error",
                    "Muse Code portal quota could not be read.",
                    &now_iso,
                ),
            })
        }
    }
}

fn muse_error_row(
    source: &Source,
    email: Option<&str>,
    plan: Option<&str>,
    platform: &str,
    status: &str,
    message: &str,
    fetched_at: &str,
) -> Value {
    json!({
        "id": source.id,
        "provider": "muse",
        "providerLabel": "Muse Code",
        "label": source.label.as_deref().unwrap_or("Muse Code"),
        "email": email,
        "plan": plan,
        "platform": platform,
        "source": "Pasted Muse Code portal session",
        "status": status,
        "message": aac_core::scrub(message),
        "fetchedAt": fetched_at,
        "sampledAt": null,
        "isActive": false,
        "windows": [],
        "capabilities": rows::capabilities(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::account_usage::sources::Credential;
    use aac_files::web_session::{PasteOptions, parse_pasted, save_as_agent};
    use axum::{Router, extract::Request, routing::any};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn dummy_source(provider: &str) -> Source {
        Source {
            id: format!("{provider}:usage"),
            provider: provider.to_owned(),
            platform: "ubuntu".to_owned(),
            ssh_host: None,
            label: None,
            credential: Credential::Discover,
            created_at: None,
            created_by: None,
        }
    }

    async fn spawn_fixture(
        handler: impl Fn(String, usize) -> (u16, Vec<(&'static str, &'static str)>, String)
        + Send
        + Sync
        + 'static,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let count = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(handler);
        let app = Router::new().fallback(any(move |req: Request| {
            let count = count.clone();
            let handler = handler.clone();
            async move {
                let url = req
                    .headers()
                    .get("x-aac-fixture-url")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let n = count.fetch_add(1, Ordering::SeqCst);
                let (status, hdrs, body) = handler(url, n);
                let mut builder = axum::http::Response::builder().status(status);
                for (k, v) in hdrs {
                    builder = builder.header(k, v);
                }
                builder.body(axum::body::Body::from(body)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), task)
    }

    #[tokio::test]
    async fn qwen_pasted_session_collects_caches_on_429_and_retains_windows_on_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse_pasted(
            Surface::Qwen,
            "login_aliyunid_ticket=fixture-ok; login_aliyunid_csrf=csrf%2B1",
            &PasteOptions {
                region: Some("intl".into()),
                ..Default::default()
            },
            "2026-11-26T12:00:00Z",
        )
        .unwrap();
        save_as_agent(dir.path(), Surface::Qwen, &parsed).unwrap();

        let mode = Arc::new(AtomicUsize::new(0));
        let mode_clone = mode.clone();
        let (endpoint, server) = spawn_fixture(move |url, _| {
            let m = mode_clone.load(Ordering::SeqCst);
            if m == 1 {
                return (429, vec![("retry-after", "120")], String::new());
            }
            if m == 2 {
                return (401, vec![], String::new());
            }
            if url.contains("info.json") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"{"data":{"secToken":"sec-tok"}}"#.into(),
                );
            }
            if url.contains("usage") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"{"Data":{"per5HourPercentage":0.42,"per5HourResetTime":1795651200}}"#.into(),
                );
            }
            (200, vec![("content-type", "application/json")], "{}".into())
        })
        .await;

        let state = WebSessionState::default();
        state.set_endpoint(Some(endpoint));
        let source = dummy_source("qwen");
        let t0 = 1_795_608_000_000_i64;

        let ok = collect_qwen(dir.path(), &state, &source, t0, false)
            .await
            .unwrap();
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["windows"][0]["usedPercent"], 42.0);
        assert_eq!(ok["source"], "Pasted Qwen console session");

        // 429 -> cached with previous real windows retained
        mode.store(1, Ordering::SeqCst);
        let limited = collect_qwen(dir.path(), &state, &source, t0 + 10_000, true)
            .await
            .unwrap();
        assert_eq!(limited["status"], "cached");
        assert_eq!(limited["windows"][0]["usedPercent"], 42.0);

        // 401 -> honest needs_sign_in WITH cached windows & sampledAt retained
        mode.store(2, Ordering::SeqCst);
        let expired = collect_qwen(dir.path(), &state, &source, t0 + 200_000, true)
            .await
            .unwrap();
        assert_eq!(expired["status"], "needs_sign_in");
        assert_eq!(expired["windows"][0]["usedPercent"], 42.0);
        assert!(expired["sampledAt"].is_string());

        server.abort();
    }

    #[tokio::test]
    async fn opencode_wallet_window_is_per_account_and_retains_balance_on_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let save_session = |dir: &Path, cookie: &str, account: &str| -> String {
            let mut parsed = parse_pasted(
                Surface::OpencodeWallet,
                cookie,
                &PasteOptions {
                    workspace_id: Some("wrk_fixture123".into()),
                    ..Default::default()
                },
                "2026-11-26T12:00:00Z",
            )
            .unwrap();
            parsed.meta.workspace_id = Some("wrk_fixture123".into());
            parsed.meta.account_id = Some(account.to_owned());
            save_as_agent(dir, Surface::OpencodeWallet, &parsed).unwrap();
            account.to_owned()
        };
        let account_a = save_session(dir.path(), "auth=fixture-auth-a", "opencode-go:usage");
        let source_a = |id: &str| Source {
            id: id.to_owned(),
            provider: "opencode-go".to_owned(),
            platform: "ubuntu".to_owned(),
            ssh_host: None,
            label: None,
            credential: Credential::Discover,
            created_at: None,
            created_by: None,
        };

        // 0: $12.50. 1: $3.25. 2: $0. 3: another workspace. 4: session expired.
        let mode = Arc::new(AtomicUsize::new(0));
        let mode_clone = mode.clone();
        let (endpoint, server) = spawn_fixture(move |url, _| {
            let m = mode_clone.load(Ordering::SeqCst);
            if m == 4 {
                return (401, vec![], String::new());
            }
            if url.ends_with("/orgs") {
                let orgs = if m == 3 {
                    r#"[{"id":"wrk_zzzz999","name":"Other"}]"#
                } else {
                    r#"[{"id":"wrk_fixture123","name":"Main"}]"#
                };
                return (200, vec![("content-type", "application/json")], orgs.into());
            }
            if url.ends_with("/billing/status") {
                let balance = match m {
                    1 => "325000000",
                    2 => "0",
                    _ => "1250000000",
                };
                let body = format!(
                    r#"{{"mode":"pay-as-you-go","billingMode":"prepaid","balanceMicroCents":"{balance}"}}"#
                );
                return (200, vec![("content-type", "application/json")], body);
            }
            (200, vec![("content-type", "application/json")], "{}".into())
        })
        .await;

        let state = WebSessionState::default();
        state.set_endpoint(Some(endpoint));
        let t0 = 1_795_608_000_000_i64;

        // No session for this account: no line, no invented zero.
        assert!(
            collect_wallet_window(
                dir.path(),
                &state,
                &source_a("opencode-go:acct:52f3251b"),
                t0,
                false
            )
            .await
            .is_none()
        );

        let window = collect_wallet_window(dir.path(), &state, &source_a(&account_a), t0, false)
            .await
            .unwrap();
        assert_eq!(window["key"], "zen-balance");
        assert_eq!(window["label"], "Wallet");
        assert_eq!(window["kind"], "balance");
        assert_eq!(window["remaining"], 12.5);
        assert_eq!(window["unit"], "USD");

        // A second account's own session reads its own balance, independently.
        let account_b = save_session(
            dir.path(),
            "auth=fixture-auth-b",
            "opencode-go:acct:52f3251b",
        );
        mode.store(1, Ordering::SeqCst);
        let window_b = collect_wallet_window(dir.path(), &state, &source_a(&account_b), t0, false)
            .await
            .unwrap();
        assert_eq!(window_b["remaining"], 3.25);

        // A read zero is a real reading, shown as-is.
        mode.store(2, Ordering::SeqCst);
        let zero =
            collect_wallet_window(dir.path(), &state, &source_a(&account_a), t0 + 30_000, true)
                .await
                .unwrap();
        assert_eq!(zero["remaining"], 0.0);

        // A session whose workspace the console no longer returns is not matched
        // to the account, even with a cached balance: no line, no guess.
        mode.store(3, Ordering::SeqCst);
        assert!(
            collect_wallet_window(dir.path(), &state, &source_a(&account_a), t0 + 60_000, true)
                .await
                .is_none()
        );

        // Expired session with a cached balance: the stale window, honestly marked.
        mode.store(0, Ordering::SeqCst);
        let fresh = collect_wallet_window(
            dir.path(),
            &state,
            &source_a(&account_a),
            t0 + 120_000,
            true,
        )
        .await
        .unwrap();
        assert_eq!(fresh["remaining"], 12.5);
        mode.store(4, Ordering::SeqCst);
        let stale = collect_wallet_window(
            dir.path(),
            &state,
            &source_a(&account_a),
            t0 + 180_000,
            true,
        )
        .await
        .unwrap();
        assert_eq!(stale["status"], "cached");
        assert_eq!(stale["remaining"], 12.5);
        assert!(stale["sampledAt"].is_string());

        server.abort();
    }

    /// An account with a pasted session whose read fails before any balance was
    /// read keeps an honest Wallet line inside its card: unavailable, no number.
    /// A workspace that no longer matches still shows nothing.
    #[tokio::test]
    async fn opencode_wallet_failed_read_without_cache_shows_an_unavailable_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut parsed = parse_pasted(
            Surface::OpencodeWallet,
            "auth=fixture-auth-fail",
            &PasteOptions {
                workspace_id: Some("wrk_fixture123".into()),
                ..Default::default()
            },
            "2026-11-26T12:00:00Z",
        )
        .unwrap();
        parsed.meta.workspace_id = Some("wrk_fixture123".into());
        parsed.meta.account_id = Some("opencode-go:usage:cdae4b3e".to_owned());
        save_as_agent(dir.path(), Surface::OpencodeWallet, &parsed).unwrap();
        let source = Source {
            id: "opencode-go:usage:cdae4b3e".to_owned(),
            provider: "opencode-go".to_owned(),
            platform: "ubuntu".to_owned(),
            ssh_host: None,
            label: None,
            credential: Credential::Discover,
            created_at: None,
            created_by: None,
        };

        // 0: upstream 500. 1: session expired. 2: rate limited. 3: other workspace.
        let mode = Arc::new(AtomicUsize::new(0));
        let mode_clone = mode.clone();
        let (endpoint, server) =
            spawn_fixture(move |url, _| match mode_clone.load(Ordering::SeqCst) {
                0 => (500, vec![], String::new()),
                1 => (401, vec![], String::new()),
                2 => (429, vec![("retry-after", "120")], String::new()),
                _ if url.ends_with("/orgs") => (
                    200,
                    vec![("content-type", "application/json")],
                    r#"[{"id":"wrk_zzzz999","name":"Other"}]"#.into(),
                ),
                _ => (200, vec![("content-type", "application/json")], "{}".into()),
            })
            .await;
        let t0 = 1_795_608_000_000_i64;
        for (m, label) in [(0, "provider error"), (1, "expired"), (2, "rate limited")] {
            let state = WebSessionState::default();
            state.set_endpoint(Some(endpoint.clone()));
            mode.store(m, Ordering::SeqCst);
            let window = collect_wallet_window(dir.path(), &state, &source, t0, true)
                .await
                .unwrap_or_else(|| panic!("{label}: the card keeps a Wallet line"));
            assert_eq!(window["key"], "zen-balance", "{label}");
            assert_eq!(window["label"], "Wallet", "{label}");
            assert_eq!(window["kind"], "balance", "{label}");
            assert_eq!(window["status"], "unavailable", "{label}");
            assert!(
                window["remaining"].is_null(),
                "{label}: never an invented $0"
            );
            assert!(
                crate::account_usage::contract::analytics_window(&window).is_some(),
                "{label}: the window passes the reading contract"
            );
        }

        let state = WebSessionState::default();
        state.set_endpoint(Some(endpoint));
        mode.store(3, Ordering::SeqCst);
        assert!(
            collect_wallet_window(dir.path(), &state, &source, t0, true)
                .await
                .is_none()
        );
        server.abort();
    }

    #[tokio::test]
    async fn muse_pasted_session_respects_300s_budget_and_retains_windows_on_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let mut parsed = parse_pasted(
            Surface::Muse,
            "llama_dev_sess=fixture-sess-val",
            &PasteOptions::default(),
            "2026-11-26T12:00:00Z",
        )
        .unwrap();
        parsed.meta.email = Some("dev@example.test".into());
        parsed.meta.plan = Some("Pro".into());
        parsed.meta.team_id = Some("42".into());
        parsed.meta.user_hash = Some(aac_core::sha256_hex("muse-user:1001"));
        save_as_agent(dir.path(), Surface::Muse, &parsed).unwrap();

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let mode = Arc::new(AtomicUsize::new(0));
        let mode_clone = mode.clone();
        let (endpoint, server) = spawn_fixture(move |url, _| {
            hits_clone.fetch_add(1, Ordering::SeqCst);
            if mode_clone.load(Ordering::SeqCst) == 1 {
                return (401, vec![], String::new());
            }
            if url.ends_with("/auth/me") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"{"email":"dev@example.test","userId":"1001"}"#.into(),
                );
            }
            if url.ends_with("/portal/teams") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"{"teams":[{"team_id":"42","team_name":"Main"}]}"#.into(),
                );
            }
            (
                200,
                vec![("content-type", "application/json")],
                r#"{"subscription_quota":{"tier":"Pro","as_of":1795607990,"window_weighted_used":25,"window_weighted_limit":100,"window_duration_secs":18000,"weekly_weighted_used":10,"weekly_weighted_limit":200}}"#.into(),
            )
        })
        .await;

        let state = WebSessionState::default();
        state.set_endpoint(Some(endpoint));
        let source = dummy_source("muse");
        let t0 = 1_795_608_000_000_i64;

        let ok = collect_muse(dir.path(), None, &state, &source, t0, false)
            .await
            .unwrap();
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["windows"][0]["usedPercent"], 25.0);
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        // Within 300s budget (non-forced): zero network calls
        let budgeted = collect_muse(dir.path(), None, &state, &source, t0 + 120_000, false)
            .await
            .unwrap();
        assert_eq!(budgeted["status"], "ok");
        assert_eq!(hits.load(Ordering::SeqCst), 3);

        // Forced expiry: honest needs_sign_in retaining cached windows
        mode.store(1, Ordering::SeqCst);
        let expired = collect_muse(dir.path(), None, &state, &source, t0 + 130_000, true)
            .await
            .unwrap();
        assert_eq!(expired["status"], "needs_sign_in");
        assert_eq!(expired["windows"][0]["usedPercent"], 25.0);

        server.abort();
    }

    #[tokio::test]
    async fn retired_bridge_surfaces_require_pasted_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let state = WebSessionState::default();
        let now = 1_795_608_000_000_i64;

        let qwen = collect_qwen(dir.path(), &state, &dummy_source("qwen"), now, false)
            .await
            .expect("Qwen must not fall back to the retired bridge");
        assert_eq!(qwen["status"], "needs_sign_in");
        assert_eq!(qwen["windows"], json!([]));

        // Without a pasted console session, an OpenCode Go account carries no
        // wallet line at all (and never a standalone row).
        assert!(
            collect_wallet_window(dir.path(), &state, &dummy_source("opencode-go"), now, false)
                .await
                .is_none()
        );

        let muse = collect_muse(dir.path(), None, &state, &dummy_source("muse"), now, false)
            .await
            .expect("Muse must not fall back to the retired bridge");
        assert_eq!(muse["status"], "needs_sign_in");
        assert_eq!(muse["windows"], json!([]));
    }
}
