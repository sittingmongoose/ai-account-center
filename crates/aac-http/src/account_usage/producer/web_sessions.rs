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
#[derive(Default)]
pub(crate) struct WebSessionState {
    entries: Mutex<HashMap<Surface, SurfaceCache>>,
    endpoint: Mutex<Option<String>>,
}

impl WebSessionState {
    /// Clear cached state and cooldown for one surface (called after PUT or DELETE).
    pub(crate) fn clear(&self, surface: Surface) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&surface);
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

    fn snapshot(&self, surface: Surface, fingerprint: &str) -> SurfaceCache {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.get(&surface) {
            Some(entry) if entry.fingerprint == fingerprint => entry.clone(),
            _ => SurfaceCache {
                fingerprint: fingerprint.to_owned(),
                ..Default::default()
            },
        }
    }

    fn record_ok(&self, surface: Surface, fingerprint: &str, now_ms: i64, row: Value) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(
            surface,
            SurfaceCache {
                fingerprint: fingerprint.to_owned(),
                checked_at_ms: now_ms,
                cooldown_until_ms: 0,
                last_good: Some(row),
            },
        );
    }

    fn record_cooldown(&self, surface: Surface, fingerprint: &str, until_ms: i64) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = guard.entry(surface).or_default();
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

/// Collect Qwen Token Plan usage in-process when a pasted web session exists in `root`.
/// Returns `None` when no pasted Qwen session is stored (leaving legacy/failover unchanged).
pub(super) async fn collect_qwen(
    root: &Path,
    state: &WebSessionState,
    source: &Source,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let root_buf = root.to_path_buf();
    let stored = tokio::task::spawn_blocking(move || web_session::load(&root_buf, Surface::Qwen))
        .await
        .ok()?
        .ok()??;
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::Qwen, &fp);
    let now_iso = iso_seconds(now_ms);
    let platform = host_platform();

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
            state.record_ok(Surface::Qwen, &fp, now_ms, row.clone());
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
            state.record_cooldown(Surface::Qwen, &fp, until);
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

/// Collect OpenCode Zen console wallet usage in-process when a pasted web session exists in `root`.
/// Returns `None` when no pasted OpenCode wallet session is stored.
pub(super) async fn collect_opencode_wallet(
    root: &Path,
    state: &WebSessionState,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let root_buf = root.to_path_buf();
    let stored =
        tokio::task::spawn_blocking(move || web_session::load(&root_buf, Surface::OpencodeWallet))
            .await
            .ok()?
            .ok()??;
    let cookies = stored.secret.cookies()?.to_vec();
    let workspace = stored.meta.workspace_id.clone();
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::OpencodeWallet, &fp);
    let now_iso = iso_seconds(now_ms);
    let platform = host_platform();
    let fallback_id = workspace
        .as_deref()
        .map(opencode::wallet_account_id)
        .unwrap_or_else(|| {
            let suffix = &fp.strip_prefix("sha256:").unwrap_or(&fp);
            let short = suffix.get(..12).unwrap_or("000000000000");
            format!("plan-opencode-go-console-web-{short}")
        });

    if !force && now_ms < cached.cooldown_until_ms {
        return Some(match cached.last_good {
            Some(prev) => with_cached_status(
                prev,
                "cached",
                &now_iso,
                "OpenCode console rate limited the check; showing the last cached reading.",
            ),
            None => opencode_error_row(
                &fallback_id,
                platform,
                "error",
                "OpenCode console rate limited the check; try again later.",
                &now_iso,
            ),
        });
    }

    let Some(transport) = build_transport(state) else {
        return Some(match cached.last_good {
            Some(prev) => with_cached_status(
                prev,
                "cached",
                &now_iso,
                "HTTP client unavailable; showing the last cached wallet reading.",
            ),
            None => opencode_error_row(
                &fallback_id,
                platform,
                "error",
                "HTTP client unavailable for OpenCode wallet check.",
                &now_iso,
            ),
        });
    };

    match opencode::collect(&transport, &cookies, workspace.as_deref(), now_ms).await {
        Ok(reading) => {
            let row = opencode::to_account_row(&reading, platform);
            state.record_ok(Surface::OpencodeWallet, &fp, now_ms, row.clone());
            Some(row)
        }
        Err(OpenCodeError::NeedsSignIn) => {
            let msg = "The pasted OpenCode console session has expired. Paste a fresh session.";
            Some(match cached.last_good {
                Some(prev) => with_cached_status(prev, "needs_sign_in", &now_iso, msg),
                None => opencode_error_row(&fallback_id, platform, "needs_sign_in", msg, &now_iso),
            })
        }
        Err(OpenCodeError::RateLimited {
            retry_after_seconds,
        }) => {
            let secs = retry_after_seconds.unwrap_or(60.0).clamp(30.0, 86_400.0);
            state.record_cooldown(
                Surface::OpencodeWallet,
                &fp,
                now_ms.saturating_add((secs * 1000.0) as i64),
            );
            let msg = "OpenCode console rate limited the check; showing the last cached reading.";
            Some(match cached.last_good {
                Some(prev) => with_cached_status(prev, "cached", &now_iso, msg),
                None => opencode_error_row(
                    &fallback_id,
                    platform,
                    "error",
                    "OpenCode console rate limited the check; try again later.",
                    &now_iso,
                ),
            })
        }
        Err(OpenCodeError::WorkspaceMismatch | OpenCodeError::ChooseWorkspace(_)) => {
            let msg = "The saved OpenCode workspace is no longer valid for this session. Paste a fresh session.";
            Some(match cached.last_good {
                Some(prev) => with_cached_status(prev, "needs_sign_in", &now_iso, msg),
                None => opencode_error_row(&fallback_id, platform, "needs_sign_in", msg, &now_iso),
            })
        }
        Err(OpenCodeError::WalletNotPrepaid) => {
            let msg = "The selected OpenCode workspace is not a prepaid Zen wallet.";
            Some(match cached.last_good {
                Some(prev) => with_cached_status(prev, "cached", &now_iso, msg),
                None => opencode_error_row(&fallback_id, platform, "unavailable", msg, &now_iso),
            })
        }
        Err(
            OpenCodeError::InvalidResponse
            | OpenCodeError::ProviderError
            | OpenCodeError::NetworkError,
        ) => Some(match cached.last_good {
            Some(prev) => with_cached_status(
                prev,
                "cached",
                &now_iso,
                "Showing the last cached wallet reading; live check failed.",
            ),
            None => opencode_error_row(
                &fallback_id,
                platform,
                "error",
                "OpenCode console wallet could not be read.",
                &now_iso,
            ),
        }),
    }
}

fn opencode_error_row(
    account_id: &str,
    platform: &str,
    status: &str,
    message: &str,
    fetched_at: &str,
) -> Value {
    json!({
        "id": account_id,
        "provider": "opencode-go",
        "providerLabel": "OpenCode Go",
        "label": "OpenCode console wallet",
        "email": null,
        "plan": null,
        "platform": platform,
        "source": "Pasted OpenCode console session",
        "status": status,
        "message": aac_core::scrub(message),
        "fetchedAt": fetched_at,
        "sampledAt": null,
        "isActive": false,
        "windows": [],
        "capabilities": rows::capabilities(),
    })
}

/// Collect Muse Code portal quota in-process when a pasted web session exists in `root`.
/// Returns `None` when no pasted Muse session is stored (leaving failover unchanged).
pub(super) async fn collect_muse(
    root: &Path,
    home: Option<&Path>,
    state: &WebSessionState,
    source: &Source,
    now_ms: i64,
    force: bool,
) -> Option<Value> {
    let root_buf = root.to_path_buf();
    let stored = tokio::task::spawn_blocking(move || web_session::load(&root_buf, Surface::Muse))
        .await
        .ok()?
        .ok()??;
    let cookies = stored.secret.cookies()?.to_vec();
    let fp = stored.meta.fingerprint.clone();
    let cached = state.snapshot(Surface::Muse, &fp);
    let now_iso = iso_seconds(now_ms);
    let platform = host_platform();

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
            state.record_ok(Surface::Muse, &fp, now_ms, row.clone());
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
    async fn opencode_wallet_pasted_session_collects_and_retains_balance_on_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let mut parsed = parse_pasted(
            Surface::OpencodeWallet,
            "auth=fixture-auth-val",
            &PasteOptions {
                workspace_id: Some("wrk_fixture123".into()),
                ..Default::default()
            },
            "2026-11-26T12:00:00Z",
        )
        .unwrap();
        parsed.meta.workspace_id = Some("wrk_fixture123".into());
        save_as_agent(dir.path(), Surface::OpencodeWallet, &parsed).unwrap();

        let expired_flag = Arc::new(AtomicUsize::new(0));
        let flag_clone = expired_flag.clone();
        let (endpoint, server) = spawn_fixture(move |url, _| {
            if flag_clone.load(Ordering::SeqCst) == 1 {
                return (401, vec![], String::new());
            }
            if url.ends_with("/orgs") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"[{"id":"wrk_fixture123","name":"Main"}]"#.into(),
                );
            }
            if url.ends_with("/billing/status") {
                return (
                    200,
                    vec![("content-type", "application/json")],
                    r#"{"mode":"pay-as-you-go","billingMode":"prepaid","balanceMicroCents":"1250000000"}"#.into(),
                );
            }
            (200, vec![("content-type", "application/json")], "{}".into())
        })
        .await;

        let state = WebSessionState::default();
        state.set_endpoint(Some(endpoint));
        let t0 = 1_795_608_000_000_i64;

        let ok = collect_opencode_wallet(dir.path(), &state, t0, false)
            .await
            .unwrap();
        assert_eq!(ok["status"], "ok");
        assert_eq!(ok["id"], opencode::wallet_account_id("wrk_fixture123"));
        assert_eq!(ok["windows"][0]["remaining"], 12.5);

        expired_flag.store(1, Ordering::SeqCst);
        let expired = collect_opencode_wallet(dir.path(), &state, t0 + 60_000, true)
            .await
            .unwrap();
        assert_eq!(expired["status"], "needs_sign_in");
        assert_eq!(expired["windows"][0]["remaining"], 12.5);
        assert!(expired["sampledAt"].is_string());

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
}
