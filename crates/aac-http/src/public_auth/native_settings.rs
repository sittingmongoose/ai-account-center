//! The cut's public settings, preferences and visibility routes.
//!
//! Admission is the TS route's, one for one: a signed-in browser session, no query string,
//! the dashboard origin and `application/json` for every write, the same byte and shape caps,
//! and the same fixed sentences. The owners behind them are the existing ones — the registered
//! settings leaf, the preferences writer and the visibility writer — so a save keeps the
//! registry lease, the transaction intent and the atomic replacement.
use super::{
    DashboardAccess, DashboardEvent, DashboardEvents, DashboardPrincipal, Reply, set_header,
};
use crate::registered;
use aac_files::registry::{self, Owners, Role};
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::Method,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SETTINGS_CAP: usize = 4096;
const VISIBILITY_CAP: usize = 8 * 1024;

/// The three route families this module owns, with the exact methods the cut answers.
pub(super) fn route(method: &Method, path: &str) -> Option<Family> {
    let method = if *method == Method::HEAD {
        &Method::GET
    } else {
        method
    };
    let family = match path.trim_end_matches('/') {
        "/api/accounts/settings" => Family::Settings,
        "/api/accounts/preferences" => Family::Preferences,
        "/api/accounts/visibility" => Family::Visibility,
        _ => return None,
    };
    let allowed = match family {
        Family::Settings => matches!(*method, Method::GET | Method::PUT),
        Family::Preferences | Family::Visibility => {
            matches!(*method, Method::GET | Method::PUT | Method::PATCH)
        }
    };
    allowed.then_some(family)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Settings,
    Preferences,
    Visibility,
}

#[derive(Clone)]
pub(super) struct Settings {
    root: PathBuf,
    events: DashboardEvents,
    owners: Owners,
    limits: crate::limits::Limits,
}

impl Settings {
    pub(super) fn new(root: PathBuf, events: DashboardEvents) -> Self {
        Self {
            root,
            events,
            owners: owners(),
            limits: crate::limits::Limits::new(),
        }
    }

    /// The group's own router for the owner host: the six routes, their admission and the
    /// exact encoding, registered instead of copying an owner. The host merges it before the
    /// dashboard guard, so the handler sees the verified `DashboardAccess`.
    pub(super) fn router(&self, router: axum::Router) -> axum::Router {
        let settings = Arc::new(self.clone());
        router
            .route("/api/accounts/settings", axum::routing::any(owner_route))
            .route("/api/accounts/preferences", axum::routing::any(owner_route))
            .route("/api/accounts/visibility", axum::routing::any(owner_route))
            .layer(axum::Extension(settings))
    }

    pub(super) async fn handle(
        &self,
        access: &DashboardAccess,
        family: Family,
        request: Request,
    ) -> Reply {
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
        let head = request.method() == Method::HEAD;
        match family {
            Family::Settings => self.settings(request, head).await,
            Family::Preferences => self.preferences(request, head).await,
            Family::Visibility => self.visibility(request, head).await,
        }
    }

    async fn settings(&self, request: Request, head: bool) -> Reply {
        if request.uri().query().is_some_and(|query| {
            url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| !key.is_empty())
        }) {
            return no_store(super::plain(400, "Unexpected settings query."));
        }
        if request.method() == Method::GET {
            let root = self.root.clone();
            let reply = match self
                .limits
                .blocking(crate::table::Pool::Reads, move || Ok(settings_read(&root)))
                .await
            {
                Ok(value) => super::response(200, value),
                Err(_) => super::plain(500, "Usage refresh settings could not be read."),
            };
            return no_store(truncate(reply, head));
        }
        let body = match self.write(request, Family::Settings, SETTINGS_CAP).await {
            Ok(body) => body,
            Err(reply) => return reply,
        };
        match settings_write(&self.root, &self.owners, &body) {
            Ok(value) => no_store(super::response(200, value)),
            Err(SettingsError::Shape) => no_store(super::plain(
                400,
                "Choose a whole number of seconds from 30 to 3600.",
            )),
            Err(SettingsError::Save) => no_store(super::plain(
                500,
                "Usage refresh settings could not be saved.",
            )),
        }
    }

    async fn preferences(&self, request: Request, head: bool) -> Reply {
        if request.uri().query().is_some_and(|query| !query.is_empty()) {
            return no_store(super::plain(400, "Unexpected preferences query."));
        }
        if request.method() == Method::GET {
            let root = self.root.clone();
            let reply = match self
                .limits
                .blocking(crate::table::Pool::Reads, move || {
                    Ok(crate::account_reads::preferences(&root)
                        .or_else(|| crate::preferences_write::pending(&root))
                        .unwrap_or_else(|| Value::Object(crate::account_reads::defaults())))
                })
                .await
            {
                Ok(value) => super::response(200, value),
                Err(_) => super::plain(500, "Dashboard preferences could not be read."),
            };
            return no_store(truncate(reply, head));
        }
        let patch = request.method() == Method::PATCH;
        let body = match self
            .write(
                request,
                Family::Preferences,
                crate::preferences_write::INPUT_CAP,
            )
            .await
        {
            Ok(body) => body,
            Err(reply) => return reply,
        };
        let root = self.root.clone();
        let owners = self.owners.clone();
        let saved = self
            .limits
            .blocking(crate::table::Pool::Reads, move || {
                Ok(crate::preferences_write::save_public(
                    &root, &owners, patch, &body,
                ))
            })
            .await;
        match saved {
            Ok(Ok(value)) => no_store(super::response(200, value)),
            Ok(Err(crate::preferences_write::PublicError::Refused(message))) => {
                no_store(super::plain(400, &message))
            }
            Ok(Err(crate::preferences_write::PublicError::Invalid)) => {
                no_store(super::plain(400, "Preferences are not valid."))
            }
            _ => no_store(super::plain(
                500,
                "Dashboard preferences could not be saved.",
            )),
        }
    }

    async fn visibility(&self, request: Request, head: bool) -> Reply {
        if request.uri().query().is_some() {
            return no_store(super::error(
                400,
                "unexpected_query",
                "This request does not take a query string.",
            ));
        }
        if request.method() == Method::GET {
            let root = self.root.clone();
            let reply = match self
                .limits
                .blocking(crate::table::Pool::Reads, move || {
                    Ok(crate::account_reads::visibility(&root))
                })
                .await
            {
                Ok(Some(value)) => super::response(200, value["visibility"].clone()),
                _ => super::error(
                    500,
                    "visibility_unavailable",
                    "Account visibility could not be read safely.",
                ),
            };
            return no_store(truncate(reply, head));
        }
        let patch = request.method() == Method::PATCH;
        let body = match self
            .write(request, Family::Visibility, VISIBILITY_CAP)
            .await
        {
            Ok(body) => body,
            Err(reply) => return reply,
        };
        if !crate::visibility_write::body_valid(patch, &body) {
            return no_store(body_refusal(Family::Visibility));
        }
        let root = self.root.clone();
        let owners = self.owners.clone();
        let saved = self
            .limits
            .blocking(crate::table::Pool::Reads, move || {
                Ok(crate::visibility_write::save(&root, &owners, patch, &body))
            })
            .await;
        let reply = match saved {
            Ok(Ok(value)) if value["state"] == "saved" => {
                super::response(200, value["visibility"].clone())
            }
            // A file that cannot be read back safely cannot take a partial update.
            Ok(Err(aac_views::decline::Decline::Shape)) | Ok(Ok(_)) => super::error(
                500,
                "visibility_unavailable",
                "Account visibility could not be read safely.",
            ),
            Ok(Err(_)) | Err(_) => super::error(
                500,
                "visibility_write_failed",
                "Account visibility could not be saved.",
            ),
        };
        // The change is saved; a failed hint never changes the answer.
        if reply.status() == 200 {
            self.events.publish(DashboardEvent::AccountsChanged {});
        }
        no_store(reply)
    }

    /// The write admission shared by all three families, in the route's own order.
    async fn write(&self, request: Request, family: Family, cap: usize) -> Result<Bytes, Reply> {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let same = request
            .extensions()
            .get::<DashboardAccess>()
            .is_some_and(|access| access.origin_present && access.same_origin);
        if origin.is_empty() || !same {
            return Err(no_store(origin_refusal(family)));
        }
        if !json_content(&request) {
            return Err(no_store(media_refusal(family)));
        }
        match body(request, cap).await {
            Some(body) => Ok(body),
            None => Err(no_store(body_refusal(family))),
        }
    }
}

fn origin_refusal(family: Family) -> Reply {
    match family {
        Family::Settings => super::plain(403, "Settings require the dashboard origin."),
        Family::Preferences => super::plain(403, "Preferences require the dashboard origin."),
        Family::Visibility => super::error(
            403,
            "origin_required",
            "This change requires the dashboard origin.",
        ),
    }
}

fn media_refusal(family: Family) -> Reply {
    match family {
        Family::Settings => super::plain(415, "Settings require application/json."),
        Family::Preferences => super::plain(415, "Preferences require application/json."),
        Family::Visibility => super::error(
            415,
            "json_required",
            "This change requires application/json.",
        ),
    }
}

fn body_refusal(family: Family) -> Reply {
    match family {
        Family::Settings => super::plain(400, "Choose a whole number of seconds from 30 to 3600."),
        Family::Preferences => super::plain(400, "Preferences are not valid."),
        Family::Visibility => super::error(
            400,
            "invalid_body",
            "Send visibility lists with known providers and at most 128 valid ids each.",
        ),
    }
}

/// The writes group's writers: every row that moves with it belongs to the Rust service.
pub(super) fn owners() -> Owners {
    let Ok(rows) = registry::rows() else {
        return Owners::defaults(&[]);
    };
    let files = rows
        .iter()
        .map(|row| {
            let owner = if row.moves_with.as_deref() == Some("writes") {
                Role::RustService
            } else {
                row.owner
            };
            (row.id.clone(), owner.word().to_owned())
        })
        .collect();
    Owners::from_boot(rows, &files).unwrap_or_else(|_| Owners::defaults(&[]))
}

/// `readAccountRefreshSettings`: the documented default answers an absent, linked, special,
/// oversized or unparsable file; only a valid stored interval is published.
fn settings_read(root: &Path) -> Value {
    let default = json!({"refreshIntervalSeconds": 60});
    let Some((file, policy)) = registered::file(root, "account-refresh-settings") else {
        return default;
    };
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy) else {
        return default;
    };
    let Ok(value) = crate::json_object::from_slice::<Value>(&bytes, &["refreshIntervalSeconds"])
    else {
        return default;
    };
    value
        .get("refreshIntervalSeconds")
        .and_then(Value::as_i64)
        .filter(|seconds| (30..=3600).contains(seconds))
        .map(|seconds| json!({"refreshIntervalSeconds": seconds}))
        .unwrap_or(default)
}

enum SettingsError {
    Shape,
    Save,
}

fn settings_write(root: &Path, owners: &Owners, body: &[u8]) -> Result<Value, SettingsError> {
    let value: Value = crate::json_object::from_slice(body, &["refreshIntervalSeconds"])
        .map_err(|_| SettingsError::Shape)?;
    let field = &value["refreshIntervalSeconds"];
    if !field.is_i64() {
        return Err(SettingsError::Shape);
    }
    let seconds = field.as_i64().ok_or(SettingsError::Shape)?;
    if !(30..=3600).contains(&seconds) {
        return Err(SettingsError::Shape);
    }
    let rows = registry::rows().map_err(|_| SettingsError::Save)?;
    let file = owners
        .claim(rows, "account-refresh-settings", root, None)
        .map_err(|_| SettingsError::Save)?;
    let mut bytes = serde_json::to_vec(&json!({"refreshIntervalSeconds": seconds}))
        .map_err(|_| SettingsError::Save)?;
    bytes.push(b'\n');
    file.replace_settings(&bytes)
        .map_err(|_| SettingsError::Save)?;
    file.clean_settings_temporaries();
    Ok(json!({"refreshIntervalSeconds": seconds}))
}

fn json_content(request: &Request) -> bool {
    super::header(request.headers(), "content-type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// One bounded body read; a declared length over the cap refuses before reading.
async fn body(request: Request, cap: usize) -> Option<Bytes> {
    let declared = request
        .headers()
        .get("content-length")
        .and_then(|size| size.to_str().ok())
        .and_then(|size| size.parse::<usize>().ok());
    if declared.is_some_and(|size| size > cap) {
        return None;
    }
    axum::body::to_bytes(request.into_body(), cap).await.ok()
}

fn truncate(mut reply: Reply, head: bool) -> Reply {
    if head {
        *reply.body_mut() = Body::empty();
    }
    reply
}

fn no_store(mut reply: Reply) -> Reply {
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

/// One request the host routed to this group. The method filter is the group's own; anything
/// else answers the cut's API 404 the way the unregistered path did.
async fn owner_route(
    axum::extract::Extension(settings): axum::extract::Extension<Arc<Settings>>,
    request: Request,
) -> Reply {
    let Some(family) = route(request.method(), request.uri().path()) else {
        return super::plain(404, "API endpoint was not found.");
    };
    let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
        return super::service_error();
    };
    settings.handle(&access, family, request).await
}
