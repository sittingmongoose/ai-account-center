//! Public admission for one account refresh. Collection and publication stay with I2.
use super::{DashboardAccess, DashboardPrincipal, Reply, Usage, json_content, object_body};
use crate::{
    account_usage::{producer::has_codex_profile, sources},
    agent_transport::Selector,
    claude_open::manifest::{self, PendingProfile},
};
use aac_files::registry::{self, Owners};
use axum::{extract::Request, http::Method};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::path::Path;

const INTERVAL_MS: i64 = 10_000;
const BODY_CAP: usize = 8 * 1024;

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<&'a str> {
    if *method != Method::POST {
        return None;
    }
    let id = path
        .strip_prefix("/api/accounts/")?
        .strip_suffix("/recheck")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

#[derive(Default)]
pub(super) struct Limit(IndexMap<String, i64>);

impl Limit {
    fn admit(&mut self, id: &str, now: i64) -> Option<i64> {
        if let Some(last) = self.0.get(id) {
            let remaining = INTERVAL_MS.saturating_sub(now.saturating_sub(*last));
            if remaining > 0 {
                return Some(1 + (remaining - 1) / 1000);
            }
        }
        self.0.insert(id.to_owned(), now);
        if self.0.len() > 256 {
            self.0.shift_remove_index(0);
        }
        None
    }
}

fn unknown() -> Reply {
    super::super::error(404, "unknown_account", "That account was not found.")
}

fn invalid_id() -> Reply {
    super::super::error(400, "invalid_account", "That account id is not valid.")
}

fn not_implemented() -> Reply {
    super::super::error(
        409,
        "not_implemented",
        "This action is not available on this server yet.",
    )
}

fn internal() -> Reply {
    super::super::error(
        500,
        "internal_error",
        "The request could not be completed safely.",
    )
}

impl Usage {
    pub(super) async fn recheck(
        &self,
        access: &DashboardAccess,
        raw_id: &str,
        request: Request,
    ) -> Reply {
        use super::super::{error, invalid_body};
        match access.principal {
            DashboardPrincipal::Anonymous => {
                return error(401, "auth_required", "Authentication required");
            }
            DashboardPrincipal::Device { .. } => {
                return error(
                    403,
                    "device_scope",
                    "This action needs a signed-in dashboard session.",
                );
            }
            DashboardPrincipal::Browser { .. } => {}
        }
        if request.uri().query().is_some() {
            return error(
                400,
                "unexpected_query",
                "This request does not take a query string.",
            );
        }
        if !access.origin_present || !access.same_origin {
            return error(
                403,
                "origin_required",
                "This change requires the dashboard origin.",
            );
        }
        if !json_content(&request) {
            return error(
                415,
                "json_required",
                "This change requires application/json.",
            );
        }
        let Some(body) = object_body(request, BODY_CAP).await else {
            return invalid_body();
        };
        let Some(id) = super::super::decode_component(raw_id) else {
            return invalid_id();
        };
        if !crate::account_reads::public_account_id(&Value::String(id.clone())) {
            return invalid_id();
        }

        if !sources::is_additional_account_id(&id) {
            return self.other_recheck(&id, body).await;
        }
        let resolved = match self.producer.lookup_recheck(&id).await {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return unknown(),
            Err(_) => {
                return error(
                    500,
                    "registry_unavailable",
                    "Account list could not be read safely.",
                );
            }
        };
        if !body.is_empty() {
            return invalid_body();
        }
        if let Some(reply) = self.admit_recheck(&id) {
            return reply;
        }
        match self
            .producer
            .recheck_resolved(&self.cache, &self.companions(), resolved)
            .await
        {
            Ok(Some(row)) => {
                self.publish();
                super::super::response(200, json!({"account": row}))
            }
            Ok(None) => unknown(),
            Err(_) => internal(),
        }
    }

    fn admit_recheck(&self, id: &str) -> Option<Reply> {
        let mut limit = self
            .recheck_limit
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let seconds = limit.admit(id, super::super::now())?;
        let mut reply = super::super::response(
            429,
            json!({
                "error": "This account was checked a moment ago. Try again shortly.",
                "code": "rate_limited",
                "retryAfterSeconds": seconds,
            }),
        );
        super::super::set_header(&mut reply, "retry-after", &seconds.to_string());
        Some(reply)
    }

    async fn other_recheck(&self, id: &str, body: serde_json::Map<String, Value>) -> Reply {
        let root = self.root.clone();
        let resolved = id.to_owned();
        let account = tokio::task::spawn_blocking(move || resolve_other(&root, &resolved)).await;
        let Ok(Some(account)) = account else {
            return unknown();
        };
        let Other::Claude { label, pending } = account else {
            return not_implemented();
        };
        let platform = body.get("platform").and_then(Value::as_str);
        let email = body.get("email").filter(|email| !email.is_null());
        if !body
            .keys()
            .all(|key| matches!(key.as_str(), "email" | "platform"))
            || !matches!(platform, Some("mac" | "windows"))
            || email.is_some_and(|email| !email.as_str().is_some_and(valid_claude_email))
        {
            return super::super::invalid_body();
        }
        if let Some(reply) = self.admit_recheck(id) {
            return reply;
        }
        if std::env::var("CCS_CLAUDE_HOST_LIFECYCLE").as_deref() != Ok("on") {
            return not_implemented();
        }
        let (promoted, email) = if let Some(profile) = pending {
            match self
                .recheck_pending(
                    *profile,
                    platform.unwrap_or_default(),
                    email.and_then(Value::as_str),
                )
                .await
            {
                Ok(result) => result,
                Err(reply) => return reply,
            }
        } else {
            (true, None)
        };
        super::super::response(
            200,
            json!({
                "account": {
                    "id": id, "provider": "claude", "label": label, "email": email,
                    "status": if promoted { "ok" } else { "needs_sign_in" },
                    "lifecycle": {
                        "state": if promoted { "ready" } else { "pending_sign_in" },
                        "jobId": null,
                    },
                },
            }),
        )
    }

    async fn recheck_pending(
        &self,
        profile: PendingProfile,
        platform: &str,
        email: Option<&str>,
    ) -> Result<(bool, Option<String>), Reply> {
        let launcher = if platform == "mac" {
            &profile.mac
        } else {
            &profile.windows
        };
        let reply = self
            .agents
            .call_value(
                "claude-host",
                Selector::usage(platform, launcher.ssh_host.as_deref()),
                json!({
                    "host":platform,
                    "op":"session",
                    "launcher":{
                        "launcherName":launcher.launcher_name,
                        "launcherPath":launcher.launcher_path,
                        "profilePath":launcher.profile_path,
                        "startMenuPath":launcher.start_menu_path,
                        "sshHost":launcher.ssh_host,
                    },
                }),
                25_000,
                30_000,
                65_536,
            )
            .await;
        let Some(signed_in) = reply.and_then(|reply| reply["signedIn"].as_bool()) else {
            return Err(super::super::response(
                502,
                json!({
                    "error":"A computer could not be reached. Nothing was changed.",
                    "code":"host_unreachable",
                    "host":platform,
                }),
            ));
        };
        if !signed_in {
            return Ok((false, profile.expected_email));
        }
        let email = email
            .or(profile.expected_email.as_deref())
            .filter(|email| valid_claude_email(email))
            .map(str::to_owned);
        let Some(email) = email else {
            return Ok((false, None));
        };
        let result_email = email.clone();
        let root = self.root.clone();
        let owners = self.owners.clone();
        let saved =
            tokio::task::spawn_blocking(move || promote(&root, &owners, &profile, &email)).await;
        if !matches!(saved, Ok(Ok(()))) {
            return Err(super::super::error(
                500,
                "write_failed",
                "The change could not be saved safely.",
            ));
        }
        self.cache.request_refresh();
        self.events
            .publish(super::DashboardEvent::AccountsChanged {});
        Ok((true, Some(result_email)))
    }
}

enum Other {
    Claude {
        label: String,
        pending: Option<Box<PendingProfile>>,
    },
    Unsupported,
}

pub(super) fn other_account_exists(root: &Path, id: &str) -> bool {
    resolve_other(root, id).is_some()
}

fn resolve_other(root: &Path, id: &str) -> Option<Other> {
    if id.starts_with("plan-opencode-go-console-") {
        return Some(Other::Unsupported);
    }
    if let Some(name) = id.strip_prefix("codex:") {
        return has_codex_profile(root, name).then_some(Other::Unsupported);
    }
    if let Some(name) = id.strip_prefix("antigravity:profile:") {
        return crate::antigravity_runtime::read_state(root)
            .is_some_and(|state| state.profile(name).is_some())
            .then_some(Other::Unsupported);
    }
    let name = id.strip_prefix("claude:")?;
    use aac_files::read::{Ancestors, Read, ReadPolicy};
    let inventory = root.join("claude-desktop-profiles.json");
    let policy = ReadPolicy::capped(1024 * 1024).ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(&inventory, &policy) {
        Read::Bytes(bytes, _) => {
            let document: Value = serde_json::from_slice(&bytes).ok()?;
            if let Some(entry) = manifest::manifest_rows(&document)?
                .iter()
                .find(|entry| entry["id"] == name)
            {
                let label = entry["email"].as_str().unwrap_or(name).to_owned();
                return Some(Other::Claude {
                    label,
                    pending: None,
                });
            }
        }
        Read::Absent => {}
        _ => return None,
    }
    let profile = manifest::read_pending_profiles(root)
        .ok()?
        .into_iter()
        .find(|profile| profile.id == name)?;
    Some(Other::Claude {
        label: profile.label.clone().unwrap_or_else(|| profile.id.clone()),
        pending: Some(Box::new(profile)),
    })
}

fn promote(root: &Path, owners: &Owners, expected: &PendingProfile, email: &str) -> Result<(), ()> {
    use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
    let rows = registry::rows().map_err(|_| ())?;
    // The same lock order as the TS writer: pending first, then the inventory.
    let pending = owners
        .claim(rows, "claude-pending", root, None)
        .map_err(|_| ())?;
    let mut pending_doc: Value =
        serde_json::from_slice(&pending.read().map_err(|_| ())?).map_err(|_| ())?;
    let profiles = manifest::parse_pending(&pending_doc).ok_or(())?;
    if !profiles.iter().any(|candidate| candidate == expected) {
        return Err(());
    }
    let pending_rows = pending_doc["profiles"].as_array_mut().ok_or(())?;
    let index = pending_rows
        .iter()
        .position(|row| row["id"] == expected.id)
        .ok_or(())?;
    let entry = pending_rows.remove(index);
    let inventory = owners
        .claim(rows, "claude-desktop-profiles", root, None)
        .map_err(|_| ())?;
    let policy = ReadPolicy::capped(1024 * 1024)
        .ancestors(Ancestors::NoLinkBelow(root))
        .mode(Mode::Private)
        .owner(Owner::Mine);
    let previous = match aac_files::read::read(inventory.path(), &policy) {
        Read::Absent => br#"{"version":1,"profiles":[]}"#.to_vec(),
        Read::Bytes(bytes, _) => bytes,
        _ => return Err(()),
    };
    let mut doc: Value = serde_json::from_slice(&previous).map_err(|_| ())?;
    let rows = doc
        .get_mut("profiles")
        .and_then(Value::as_array_mut)
        .ok_or(())?;
    if rows
        .iter()
        .any(|row| !row.is_object() || row["id"] == expected.id)
        || doc["version"] != 1
    {
        return Err(());
    }
    doc["profiles"].as_array_mut().ok_or(())?.push(json!({
        "id":expected.id, "email":email, "mac":entry["mac"], "windows":entry["windows"],
    }));
    let inventory_next = serde_json::to_vec(&doc).map_err(|_| ())?;
    let pending_next = serde_json::to_vec(&pending_doc).map_err(|_| ())?;
    inventory.write(&inventory_next).map_err(|_| ())?;
    if pending.write(&pending_next).is_err() {
        if inventory.write(&previous).is_err() {
            tracing::warn!("dashboard.claude_recheck.rollback_failed");
        }
        return Err(());
    }
    Ok(())
}

fn valid_claude_email(email: &str) -> bool {
    static EMAIL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$").expect("valid email pattern")
    });
    email.encode_utf16().count() <= 254 && EMAIL.is_match(email)
}
