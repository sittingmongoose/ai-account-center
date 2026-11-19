//! `GET /api/views/home` on the owner host: the page's one read, from this boot's own owners.
//!
//! The TypeScript handler answered one request with three reads — the Accounts dashboard before its per-response
//! pass, this process's Claude profile manifest and the Antigravity runtime's inventory — then composed the page's
//! view from them. Every owner now exists here: the Accounts generation is the one this boot publishes (the shared
//! `AccountUsageCache` the dashboard and tray read), the profile manifest is `profile_reads`' capped read, and the
//! inventory is the runtime host's own `inventory`. The view is the same code the private service ran
//! (`home_producer` → `home_fed`), so the bytes are `home@3`'s.
//!
//! Admission, guards and encoding are the TS route's, one for one: a signed-in browser session (a device gets
//! `device_scope`), the platform/refresh query contract, `x-aac-home-context` decoded and validated (an unreadable
//! context only means the answer has no view), `cache-control: no-store` from the query check onward, and
//! `{"error":"Account usage could not be read safely."}` when the dashboard has no generation. When the producer or
//! the fed view declines, the legacy shape answers with `view: null` — the page builds its own, as the TS fallback
//! did — from the same reads.

use super::owner_host::OwnerHost;
use super::{DashboardAccess, DashboardPrincipal, Reply, header, service_error, set_header};
use crate::account_usage::AccountUsageCache;
use crate::feed::Feeds;
use crate::home_producer::{HomeProducer, Inputs};
use aac_views::home::fed;
use axum::{
    Router,
    extract::{Extension, Request},
    http::Method,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

/// The page's context header, as `home-feed.ts` names it.
const CONTEXT_HEADER: &str = "x-aac-home-context";
/// The one sentence every unreadable dashboard read answers.
const UNREADABLE: &str = "Account usage could not be read safely.";
const INVALID_QUERY: &str = "Select a valid desktop platform and refresh value.";

/// This group's state: the boot's owners and the one Home producer over them.
pub(super) struct Home {
    host: Arc<OwnerHost>,
    root: PathBuf,
    feeds: Feeds,
    producer: HomeProducer,
    antigravity: Option<crate::antigravity_host::Host>,
}

impl Home {
    pub(super) fn new(host: Arc<OwnerHost>, root: PathBuf) -> Self {
        let antigravity = crate::antigravity_host::Host::from_root(&root);
        Self {
            host,
            root,
            feeds: Feeds::new(),
            producer: HomeProducer::default(),
            antigravity,
        }
    }

    /// The group's router for the owner host. One path; the handler keeps the method check so an
    /// unlisted method answers the cut's API 404 rather than a 405 no route of the cut produces.
    pub(super) fn router(self: &Arc<Self>, router: Router) -> Router {
        router
            .route("/api/views/home", axum::routing::any(home_route))
            .layer(Extension(self.clone()))
    }
}

async fn home_route(Extension(home): Extension<Arc<Home>>, request: Request) -> Reply {
    let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
        return service_error();
    };
    home.get(&access, request).await
}

/// `mac`/`windows` and `true`/`false`, and no other query member, exactly as the TS route checks.
fn query(request: &Request) -> Option<(&'static str, bool)> {
    let mut platform = "mac";
    let mut refresh = false;
    let mut seen_platform = false;
    let mut seen_refresh = false;
    let query = request.uri().query().unwrap_or_default();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "platform" if !seen_platform => {
                platform = match value.as_ref() {
                    "mac" => "mac",
                    "windows" => "windows",
                    _ => return None,
                };
                seen_platform = true;
            }
            "refresh" if !seen_refresh => {
                refresh = match value.as_ref() {
                    "true" => true,
                    "false" => false,
                    _ => return None,
                };
                seen_refresh = true;
            }
            // An empty member (`?`) is what Express's query parser drops; a named one declines.
            "" | "platform" | "refresh" => return None,
            _ => return None,
        }
    }
    Some((platform, refresh))
}

impl Home {
    async fn get(&self, access: &DashboardAccess, request: Request) -> Reply {
        match access.principal {
            DashboardPrincipal::Anonymous => {
                return super::error(401, "auth_required", "Authentication required");
            }
            DashboardPrincipal::Device { .. } => {
                return super::error(
                    403,
                    "device_scope",
                    "This action needs a signed-in dashboard session.",
                );
            }
            DashboardPrincipal::Browser { .. } => {}
        }
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return super::plain(404, "API endpoint was not found.");
        }
        let Some((platform, refresh)) = query(&request) else {
            return super::plain(400, INVALID_QUERY);
        };
        let head = request.method() == Method::HEAD;
        let secure = access.secure_transport;
        let Some(cache) = self
            .host
            .public_owners()
            .and_then(|owners| owners.usage_cache())
        else {
            return set_no_store(super::plain(500, UNREADABLE));
        };
        if refresh {
            cache.request_refresh();
        }
        // The page's context is passed through as the front door wrote it; the fed answer decodes
        // and validates it, and an unreadable one leaves the answer without a view.
        let page = header(request.headers(), CONTEXT_HEADER).map(str::to_owned);
        let reply = self
            .read(&cache, platform, secure, page.as_deref())
            .await
            .unwrap_or_else(|| set_no_store(super::plain(500, UNREADABLE)));
        truncate(reply, head)
    }

    /// The answer: the producer's three generations, the fed view, or the legacy shape with no view.
    /// `None` is an unreadable dashboard (the caller answers the 500).
    async fn read(
        &self,
        cache: &AccountUsageCache,
        platform: &'static str,
        secure: bool,
        page: Option<&str>,
    ) -> Option<Reply> {
        // A cold boot has no generation yet: ask for one and wait, as the TS read collected on
        // demand, so the page never answers "unreadable" while the first pass is running.
        let desktop = match platform {
            "windows" => crate::account_usage::Desktop::Windows,
            _ => crate::account_usage::Desktop::Mac,
        };
        if cache.generation(desktop, secure).is_none() {
            cache.request_refresh();
            if !cache
                .await_generation(desktop, secure, std::time::Duration::from_secs(12))
                .await
            {
                return None;
            }
        }
        let generation = cache
            .feeds
            .latest(&crate::account_usage::source(platform, secure))
            .ok()?;
        let dashboard: Value = serde_json::from_str(generation.data.get()).ok()?;
        let updated = generation
            .id
            .parse::<i64>()
            .ok()
            .and_then(|id| aac_num::ecma::to_iso_string(id / 1000))?;
        let now = aac_core::UtcMs::now().ok()?.get() as i64;
        let prepared = crate::account_usage::prepared(&generation).ok()?;
        let dashboard_bytes = prepared.render(&updated, now);
        // No installed runtime is the route's empty Ubuntu inventory, exactly as the TS reader
        // answers without a runtime to ask.
        let antigravity = self
            .antigravity
            .as_ref()
            .map(|host| host.inventory(now))
            .unwrap_or_else(|| json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []}));
        let inputs = Inputs {
            platform: platform.into(),
            secure,
            dashboard,
            // No native Open runs in this boot (the Open route stays with the worker until its
            // group lands): the page's own progress pairs are absent, as a fresh TS process had them.
            opens: json!([]),
            antigravity,
        };
        let produced = self.producer.produce(&self.feeds, &inputs).ok();
        if let Some(produced) = produced
            && let Some(feed) = feed_header(platform, secure, now, &updated, &produced)
            && let Some(answer) = self.fed(&feed, page).await
        {
            return Some(answer);
        }
        // The legacy shape: the same reads, no view (`home-read-parity`'s fed path declined).
        Some(super::json_bytes(
            200,
            legacy(&dashboard_bytes, &inputs, &self.root)?,
        ))
    }

    async fn fed(&self, feed: &str, page: Option<&str>) -> Option<Reply> {
        let feeds = self.feeds.clone();
        let root = self.root.clone();
        let feed = feed.to_owned();
        let page = page.map(str::to_owned);
        let answer = tokio::task::spawn_blocking(move || {
            crate::home_fed::answer(&feeds, &root, &feed, page.as_deref())
        })
        .await
        .ok()?
        .ok()?;
        // The service's own reply carries the `home@3` view and the ETag identity; Home answers
        // JSON with the same no-store the TS route set.
        let mut reply = super::json_bytes(200, answer.bytes);
        set_header(&mut reply, "cache-control", "no-store");
        Some(reply)
    }
}

/// The fed request's per-request facts, as `home-feed.ts` writes them.
fn feed_header(
    platform: &str,
    secure: bool,
    now: i64,
    updated_at: &str,
    produced: &Value,
) -> Option<String> {
    let mut feed = serde_json::Map::new();
    feed.insert("platform".into(), json!(platform));
    feed.insert("secure".into(), json!(secure));
    feed.insert("now".into(), json!(now));
    feed.insert("updatedAt".into(), json!(updated_at));
    for key in ["dashboard", "opens", "antigravity"] {
        feed.insert(key.into(), produced.get(key)?.clone());
    }
    String::from_utf8(aac_views::js_json::write(&Value::Object(feed))).ok()
}

/// The TS legacy answer's shape from the same reads: the rendered dashboard, the profile rows, the
/// composed inventory and `view: null` (the page builds its own view).
fn legacy(dashboard: &[u8], inputs: &Inputs, root: &std::path::Path) -> Option<Vec<u8>> {
    let manifest = crate::profile_reads::desktop_manifest(root)?;
    let composed = crate::profile_reads::compose_manifest(manifest.as_deref())?;
    let opens = match fed::prepare_opens(b"[]") {
        Ok(opens) => opens,
        Err(_) => return None,
    };
    let profiles = match fed::profiles_with_opens(&composed, &opens) {
        Ok(profiles) => profiles,
        Err(_) => return None,
    };
    let profiles_text = aac_views::js_json::write(&profiles);
    let envelope = match crate::home_producer::antigravity_envelope(&inputs.antigravity) {
        Ok(envelope) => envelope,
        Err(_) => return None,
    };
    let antigravity = match fed::prepare_antigravity(&envelope) {
        Ok(antigravity) => antigravity,
        Err(_) => return None,
    };
    let antigravity_text = aac_views::js_json::write(&antigravity);
    let mut out =
        Vec::with_capacity(dashboard.len() + profiles_text.len() + antigravity_text.len() + 96);
    out.extend_from_slice(br#"{"schemaVersion":1,"dashboard":"#);
    out.extend_from_slice(dashboard);
    out.extend_from_slice(br#","profiles":"#);
    out.extend_from_slice(&profiles_text);
    out.extend_from_slice(br#","antigravity":"#);
    out.extend_from_slice(&antigravity_text);
    out.extend_from_slice(br#","view":null}"#);
    Some(out)
}

fn set_no_store(mut reply: Reply) -> Reply {
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn truncate(mut reply: Reply, head: bool) -> Reply {
    if head {
        *reply.body_mut() = axum::body::Body::empty();
    }
    reply
}
