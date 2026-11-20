//! T3 management facade backed only by published dashboard-cache rows.
mod projection;

use super::{
    Dashboard, PublicPeer, Reply, decode_component, error, header, response,
    routes::limits::{Limits, limited},
    set_header,
    storage::{is_hex, now, sha},
};
use aac_files::read::{Ancestors, Mode, Read, ReadPolicy};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};
use projection::{Account, CLAUDE, CODEX, CREDITS};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

const MOUNT: &str = "/v0/management";
const KEY_FILE: &str = "auth/usage-hub-key.json";
const CACHE_LIMIT: u64 = 4 * 1024 * 1024;
pub(super) const NATIVE_SNAPSHOT: &str = "native-hub.json";

#[cfg(unix)]
pub(crate) fn publish_native_snapshot(directory: &std::path::Path, dashboards: Vec<Value>) {
    let dashboards = dashboards
        .into_iter()
        .filter_map(|mut dashboard| {
            let accounts = dashboard["accounts"].as_array_mut()?;
            accounts.retain(|row| matches!(row["provider"].as_str(), Some("codex" | "claude")));
            for row in &mut *accounts {
                if let Some(fields) = row.as_object_mut() {
                    fields.retain(|key, _| {
                        matches!(
                            key.as_str(),
                            "id" | "provider"
                                | "label"
                                | "email"
                                | "plan"
                                | "sampledAt"
                                | "fetchedAt"
                                | "status"
                                | "lifecycle"
                                | "windows"
                        )
                    });
                    if let Some(lifecycle) =
                        fields.get_mut("lifecycle").and_then(Value::as_object_mut)
                    {
                        lifecycle.retain(|key, _| key == "state");
                    }
                    if let Some(windows) = fields.get_mut("windows").and_then(Value::as_array_mut) {
                        for window in windows {
                            if let Some(window) = window.as_object_mut() {
                                window.retain(|key, _| {
                                    matches!(
                                        key.as_str(),
                                        "key"
                                            | "label"
                                            | "usedPercent"
                                            | "resetPassed"
                                            | "kind"
                                            | "resetAt"
                                            | "windowMinutes"
                                    )
                                });
                            }
                        }
                    }
                }
            }
            Some(json!({"accounts": accounts}))
        })
        .collect::<Vec<_>>();
    if !dashboards.is_empty()
        && let Ok(text) = serde_json::to_string(&dashboards)
        && text.len() as u64 <= CACHE_LIMIT
        && aac_files::share::write_atomically(directory, NATIVE_SNAPSHOT, &text, 0o600).is_err()
    {
        tracing::warn!("dashboard.hub_snapshot.unpublished");
    }
}

#[derive(Clone, Default)]
/// Latest projection of the Mac and Windows dashboard cache, shared with the public router.
pub struct HubCache(Arc<RwLock<Arc<Vec<Account>>>>);

impl HubCache {
    /// Replaces the cached T3 answers from the existing dashboard snapshots in Mac, Windows order.
    /// Producers supply the public dashboard DTOs after applying their usual reset-time pass.
    pub fn publish_dashboard_cache(&self, dashboards: &[Value]) {
        let next = Arc::new(projection::project(dashboards));
        if let Ok(mut cache) = self.0.write() {
            *cache = next;
        }
    }

    fn accounts(&self) -> Arc<Vec<Account>> {
        self.0
            .read()
            .map_or_else(|_| Arc::default(), |accounts| accounts.clone())
    }
}

struct SnapshotFeed {
    path: PathBuf,
    native: Option<PathBuf>,
    next_poll: Mutex<Option<Instant>>,
}

impl SnapshotFeed {
    fn refresh(&self, cache: &HubCache) {
        let Ok(mut next_poll) = self.next_poll.lock() else {
            return;
        };
        let now = Instant::now();
        if next_poll.is_some_and(|next| now < next) {
            return;
        }
        *next_poll = Some(now + Duration::from_secs(5));
        let dashboards = self
            .native
            .as_deref()
            .and_then(Self::read)
            .or_else(|| Self::read(&self.path));
        if let Some(dashboards) = dashboards {
            cache.publish_dashboard_cache(&dashboards);
        }
    }

    fn read(path: &std::path::Path) -> Option<Vec<Value>> {
        let parent = path.parent()?;
        let policy = ReadPolicy::capped(CACHE_LIMIT)
            .ancestors(Ancestors::NoLinkBelow(parent))
            .mode(Mode::Private)
            .owner(aac_files::read::Owner::Mine);
        let Read::Bytes(bytes, _) = aac_files::read::read(path, &policy) else {
            return None;
        };
        let dashboards = match serde_json::from_slice::<Value>(&bytes).ok()? {
            Value::Array(dashboards) => dashboards,
            value => vec![value],
        };
        dashboards
            .iter()
            .all(|dashboard| dashboard["accounts"].is_array())
            .then_some(dashboards)
    }
}

#[derive(Default)]
struct Rate {
    requests: Limits,
    failures: Limits,
}

#[derive(Clone)]
pub(super) struct Hub {
    dashboard: Dashboard,
    cache: HubCache,
    feed: Option<Arc<SnapshotFeed>>,
    rate: Arc<Mutex<Rate>>,
}

impl Hub {
    pub(super) fn new(
        dashboard: Dashboard,
        cache: HubCache,
        snapshot: Option<PathBuf>,
        cut: bool,
    ) -> Self {
        let feed = snapshot.filter(|path| path.is_absolute()).map(|path| {
            Arc::new(SnapshotFeed {
                native: (cut && cfg!(unix)).then(|| path.with_file_name(NATIVE_SNAPSHOT)),
                path,
                next_poll: Mutex::new(None),
            })
        });
        Self {
            dashboard,
            cache,
            feed,
            rate: Arc::default(),
        }
    }

    pub(super) fn owns(path: &str) -> bool {
        let path = path.to_ascii_lowercase();
        path == MOUNT
            || path
                .strip_prefix(MOUNT)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub(super) async fn handle(self, request: Request) -> Reply {
        let (mut parts, body) = request.into_parts();
        let head = parts.method == Method::HEAD;
        let json_content = header(&parts.headers, "content-type")
            .and_then(|value| value.split(';').next())
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
        let value = if json_content {
            let Ok(Ok(bytes)) =
                tokio::time::timeout(Duration::from_secs(10), to_bytes(body, 100 * 1024)).await
            else {
                return response(
                    413,
                    json!({"error":"Request body is too large.","code":"body_too_large"}),
                );
            };
            if bytes.is_empty() {
                Value::Null
            } else {
                match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) if value.is_object() || value.is_array() => value,
                    _ => {
                        return response(
                            400,
                            json!({"error":"Invalid JSON in request body","code":"invalid_json"}),
                        );
                    }
                }
            }
        } else {
            Value::Null
        };
        if !parts.headers.contains_key("host")
            && let Some(authority) = parts.uri.authority()
            && let Ok(host) = authority.as_str().parse()
        {
            parts.headers.insert("host", host);
        }
        let Some(peer) = super::peer(&parts.extensions) else {
            return internal();
        };
        let Ok(permit) = self.dashboard.permits.clone().try_acquire_owned() else {
            return error(503, "auth_busy", "The dashboard is busy. Try again later.");
        };
        let mut reply = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            self.answer(&parts, peer, &value)
        })
        .await
        .unwrap_or_else(|_| internal());
        if head {
            *reply.body_mut() = Body::empty();
        }
        reply
    }

    fn answer(&self, parts: &axum::http::request::Parts, peer: PublicPeer, value: &Value) -> Reply {
        let (connection, origin) = {
            let Ok(state) = self.dashboard.runtime.lock() else {
                return internal();
            };
            let Ok(config) = state.load_config() else {
                return internal();
            };
            (
                config
                    .network
                    .connection(&parts.headers, peer.address.ip(), peer.tls),
                config.network.origin,
            )
        };
        if connection.lan_proxy {
            return error(
                404,
                "not_found",
                "The usage hub does not answer through a reverse proxy.",
            );
        }
        let at = now();
        let client = connection.client_key();
        let (request_budget, failure_budget) = {
            let Ok(mut rate) = self.rate.lock() else {
                return internal();
            };
            let budget = rate.requests.check(client.clone(), at, 120, 60_000);
            if budget.blocked() {
                return no_store(limited(
                    &budget,
                    &budget,
                    "Too many usage hub requests. Try again later.",
                ));
            }
            rate.requests.spend(&budget);
            let failed = rate.failures.check(client, at, 10, 900_000);
            if failed.blocked() {
                return no_store(limited(
                    &budget,
                    &failed,
                    "Too many wrong usage hub keys. Try again later.",
                ));
            }
            rate.failures.spend(&failed);
            (budget, failed)
        };
        let reply = if parts.uri.query().is_some_and(|query| {
            query.contains("aacu_")
                || decode_component(query).is_some_and(|query| query.contains("aacu_"))
        }) {
            error(
                400,
                "key_in_query",
                "Send the management key only in a request header.",
            )
        } else {
            self.authenticate(parts, value, connection.secure, origin)
        };
        if reply.status() != StatusCode::UNAUTHORIZED
            && let Ok(mut rate) = self.rate.lock()
        {
            rate.failures.refund(&failure_budget);
        }
        no_store(request_budget.headers(reply))
    }

    fn authenticate(
        &self,
        parts: &axum::http::request::Parts,
        value: &Value,
        secure: bool,
        origin: Option<String>,
    ) -> Reply {
        let key = {
            let Ok(state) = self.dashboard.runtime.lock() else {
                return internal();
            };
            state.files.read_json(KEY_FILE, 4096)
        };
        let key = match key {
            Ok(Some(key)) => key,
            Ok(None) => {
                return error(404, "usage_hub_off", "The usage hub is off.");
            }
            Err(_) => return unreadable(),
        };
        let Some(hash) = key["keySha256"]
            .as_str()
            .filter(|hash| is_hex(hash, 64) && !hash.bytes().any(|byte| byte.is_ascii_uppercase()))
        else {
            return unreadable();
        };
        if key["version"] != 1
            || !key["createdAt"]
                .as_str()
                .is_some_and(|date| chrono::DateTime::parse_from_rfc3339(date).is_ok())
        {
            return unreadable();
        }
        if !secure {
            return response(
                403,
                json!({
                    "error":"Use loopback, HTTPS or a trusted local network address for the usage hub (dashboard_network.trust_local_network).",
                    "code":"secure_transport_required", "secureOrigin":origin,
                }),
            );
        }
        let Some(presented) = presented_key(&parts.headers).filter(|key| !key.is_empty()) else {
            return error(
                401,
                "missing_management_key",
                "A management key is required.",
            );
        };
        if presented.len() > 512 || !bool::from(sha(presented).as_bytes().ct_eq(hash.as_bytes())) {
            return error(
                401,
                "invalid_management_key",
                "The management key is not valid.",
            );
        }
        let path = parts.uri.path().get(MOUNT.len()..).unwrap_or_default();
        let path = path.strip_suffix('/').unwrap_or(path);
        match (&parts.method, path) {
            (&Method::GET | &Method::HEAD, "/auth-files") => {
                let files = self
                    .accounts()
                    .iter()
                    .map(|account| account.file.clone())
                    .collect::<Vec<_>>();
                response(200, json!({"files":files}))
            }
            (&Method::POST, "/api-call") => self.api_call(value),
            (&Method::POST, "/reset-quota") => unsupported(
                "AI Account Center does not reset quotas for T3. It only reports usage.",
            ),
            _ => error(
                404,
                "not_found",
                "AI Account Center offers only auth-files and api-call usage reads here.",
            ),
        }
    }

    fn accounts(&self) -> Arc<Vec<Account>> {
        if let Some(feed) = &self.feed {
            feed.refresh(&self.cache);
        }
        self.cache.accounts()
    }

    fn api_call(&self, body: &Value) -> Reply {
        if !body.is_object() {
            return error(
                400,
                "invalid_request",
                "Send the api-call as a JSON object.",
            );
        }
        let Some(index) = body["auth_index"]
            .as_str()
            .filter(|index| !index.is_empty() && index.encode_utf16().count() <= 128)
        else {
            return error(
                400,
                "invalid_request",
                "auth_index must be a string from auth-files.",
            );
        };
        if body.get("method").is_some_and(|method| {
            !method
                .as_str()
                .is_some_and(|method| method.eq_ignore_ascii_case("GET"))
        }) || body.get("data").is_some()
        {
            return unsupported(
                "AI Account Center answers only GET usage reads. It is not a proxy and changes nothing.",
            );
        }
        let url = body["url"].as_str();
        if url.is_some_and(|url| url.starts_with(CREDITS)) {
            return unsupported("AI Account Center does not offer Codex reset credits to T3.");
        }
        if !matches!(url, Some(CODEX | CLAUDE)) {
            return unsupported(
                "AI Account Center answers only the Codex and Claude usage reads T3 makes. It is not a proxy.",
            );
        }
        let accounts = self.accounts();
        let Some(account) = accounts
            .iter()
            .find(|account| account.file["auth_index"] == index)
        else {
            return error(
                404,
                "unknown_auth_index",
                "No account has this auth_index. List auth-files.",
            );
        };
        let expected = if account.file["provider"] == "codex" {
            CODEX
        } else {
            CLAUDE
        };
        if url != Some(expected) {
            return unsupported("This usage read does not match the account provider.");
        }
        let (status, body) = account.usage.as_ref().map_or_else(|| {
            (503, json!({"error":{"type":"no_cached_reading","message":account.file["status_message"]}}).to_string())
        }, |usage| (200, usage.to_string()));
        response(
            200,
            json!({"status_code":status,"header":{"Content-Type":["application/json"]},"body":body}),
        )
    }
}

fn presented_key(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key("authorization") {
        let mut values = headers.get_all("authorization").iter();
        let mut words = values.next()?.to_str().ok()?.split_whitespace();
        let result = match (words.next(), words.next(), words.next()) {
            (Some(scheme), Some(key), None) if scheme.eq_ignore_ascii_case("bearer") => Some(key),
            _ => None,
        };
        return result.filter(|_| values.next().is_none());
    }
    let mut values = headers.get_all("x-management-key").iter();
    let value = values.next()?.to_str().ok()?.trim();
    values.next().is_none().then_some(value)
}

fn no_store(mut reply: Reply) -> Reply {
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn unsupported(message: &str) -> Reply {
    error(403, "unsupported_api_call", message)
}

fn unreadable() -> Reply {
    error(
        503,
        "usage_hub_key_unreadable",
        "The usage hub key cannot be read safely.",
    )
}

fn internal() -> Reply {
    error(
        500,
        "internal_error",
        "The usage hub could not answer safely.",
    )
}
