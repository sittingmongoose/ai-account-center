//! Open a configured Cursor account on its Mac, using the boot's existing agent pool.
use super::recheck;
use crate::{
    account_reads::public_account_id,
    account_usage::{producer, sources},
    agent_transport::{Agents, MutationReply, Selector},
    public_auth::{Reply, error, lifecycle, response},
};
use axum::{extract::Request, http::Method};
use serde_json::{Value, json};
use std::sync::Arc;

const OPEN_COMMAND: &str = r#"console_user=$(/usr/bin/stat -f '%Su' /dev/console)
test "$console_user" != root && test "$console_user" != loginwindow || exit 1
console_uid=$(/usr/bin/id -u "$console_user") || exit 1
current_uid=$(/usr/bin/id -u) || exit 1
test "$console_uid" = "$current_uid" || exit 1
exec /usr/bin/open -a Cursor"#;

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<&'a str> {
    if !matches!(*method, Method::POST | Method::OPTIONS) {
        return None;
    }
    let id = path.strip_prefix("/api/accounts/")?.strip_suffix("/open")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

pub(super) async fn handle(agents: &Arc<Agents>, raw_id: &str, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return lifecycle::options("POST", "4");
    }
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let Some(platform) = body
        .get("platform")
        .and_then(Value::as_str)
        .filter(|platform| matches!(*platform, "mac" | "windows") && body.len() == 1)
    else {
        return lifecycle::invalid_action_body();
    };

    let Some(id) = super::super::decode_component(raw_id) else {
        return super::super::plain(400, "The request could not be processed.");
    };
    if !public_account_id(&Value::String(id.clone())) {
        return error(400, "invalid_account", "That account id is not valid.");
    }
    let root = agents.root().to_path_buf();
    if !sources::is_additional_account_id(&id) {
        let known = tokio::task::spawn_blocking(move || recheck::other_account_exists(&root, &id))
            .await
            .unwrap_or_default();
        return if known { not_configured() } else { unknown() };
    }
    let source = match producer::recheck::lookup(root, &id).await {
        Ok(Some(account)) => account.source,
        Ok(None) => return unknown(),
        Err(_) => {
            return error(
                500,
                "registry_unavailable",
                "Account list could not be read safely.",
            );
        }
    };
    let Some(alias) = source
        .ssh_host
        .filter(|_| source.provider == "cursor" && source.platform == "mac" && platform == "mac")
    else {
        return not_configured();
    };
    let reply = if launch(agents, &alias).await {
        response(200, json!({"opened":true}))
    } else {
        response(
            502,
            json!({"error":"A computer could not be reached. Nothing was changed.","code":"host_unreachable","host":"mac"}),
        )
    };
    super::no_store(reply)
}

fn unknown() -> Reply {
    error(404, "unknown_account", "That account was not found.")
}

fn not_configured() -> Reply {
    error(
        409,
        "not_configured",
        "That app has no launcher on that computer.",
    )
}

async fn launch(agents: &Arc<Agents>, alias: &str) -> bool {
    let selector = Selector::usage("mac", Some(alias));
    match agents
        .call_mutation("cursor-open", &selector, json!({}), 30_000)
        .await
    {
        MutationReply::Result(value) => return value["opened"] == true,
        MutationReply::NotDispatched => {}
        MutationReply::Uncertain => return false,
    }
    agents
        .reference(
            selector,
            vec!["/bin/sh".into(), "-c".into(), OPEN_COMMAND.into()],
            Vec::new(),
            60_000,
            64 * 1024,
        )
        .await
        .is_ok_and(|output| output.code == Some(0))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::public_auth::{DashboardAccess, DashboardPrincipal};
    use axum::body::{Body, to_bytes};

    #[tokio::test]
    async fn validates_the_json_shape_and_resolves_the_account_before_opening() {
        let root = tempfile::tempdir().unwrap();
        let agents = Arc::new(Agents::with_default(root.path().to_owned(), true));
        for (id, body, origin, expected) in [
            (
                "cursor:usage",
                r#"{"platform":"mac"}"#,
                false,
                "origin_required",
            ),
            ("malformed", "{}", true, "invalid_body"),
            (
                "malformed",
                r#"{"platform":"mac"}"#,
                true,
                "invalid_account",
            ),
            (
                "zai:acct:00000001",
                r#"{"platform":"mac"}"#,
                true,
                "unknown_account",
            ),
            (
                "cursor:usage",
                r#"{"platform":"mac"}"#,
                true,
                "not_configured",
            ),
        ] {
            let mut request = Request::builder()
                .method(Method::POST)
                .uri(format!("/api/accounts/{id}/open"))
                .header("content-type", "application/json")
                .header("content-length", body.len())
                .body(Body::from(body))
                .unwrap();
            request.extensions_mut().insert(DashboardAccess {
                auth_enabled: true,
                principal: DashboardPrincipal::Browser {
                    username: "fixture".into(),
                },
                secure_transport: true,
                same_origin: true,
                origin_present: origin,
                local_access: true,
                session_id: Some("fixture-session".into()),
                credential_transport: "direct-loopback",
                direct_loopback: true,
                lan_proxy: false,
                client_address: None,
            });
            let reply = handle(&agents, id, request).await;
            assert_eq!(reply.headers()["cache-control"], "no-store");
            let bytes = to_bytes(reply.into_body(), 4096).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["code"],
                expected
            );
        }
    }
}
