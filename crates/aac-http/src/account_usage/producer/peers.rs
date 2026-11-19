//! Select the same verified account on its primary computer or an admitted peer.
use super::{Producer, identity_key, inventory_complete, now_at_least, pick, source_host};
use crate::account_usage::{
    failover, provider_caches, rows,
    sources::{Credential, Source},
};
use crate::agent_transport::UsageHost;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::collections::HashSet;

fn selector(source: &Source) -> String {
    match &source.credential {
        Credential::Discover => "discover".into(),
        Credential::AacKey { key_id } => format!("aac-key:{key_id}"),
        Credential::BrowserCapsule { capsule_id } => format!("browser-capsule:{capsule_id}"),
        Credential::ConfigHome { home_id } => format!("config-home:{home_id}"),
        Credential::AntigravityProfile { profile_id } => {
            format!("antigravity-profile:{profile_id}")
        }
    }
}

fn measured(reading: &Value, now: i64) -> Option<i64> {
    if !matches!(reading["status"].as_str(), Some("ok" | "cached"))
        || rows::windows(&reading["windows"], 256).is_empty()
    {
        return None;
    }
    let stamp = rows::stamp(&reading["sampledAt"])?;
    aac_views::js::parse_date(&stamp)
        .ok()?
        .filter(|stamp| *stamp <= now)
}

fn linked(
    root: &std::path::Path,
    source: &Source,
    target: &str,
    identity: &Value,
    candidate: &Value,
    host: &str,
) -> bool {
    identity == &candidate["identity"]
        || (source.provider == "muse"
            && provider_caches::bound_muse_peer(
                root,
                target,
                identity,
                &candidate["identity"],
                host,
            ) == Ok(true))
}

impl Producer {
    async fn inventory(
        &self,
        platform: &str,
        alias: Option<&str>,
        provider: &str,
    ) -> Result<Value, &'static str> {
        let mut args = json!({"version":1,"action":"inventory","provider":provider});
        if alias.is_none() {
            args["ccsDir"] = json!(self.root);
        }
        self.ask(platform, alias, "failover-inventory", &args).await
    }

    pub(super) async fn coordinate(
        &self,
        source: &Source,
        legacy: bool,
        now: i64,
    ) -> Result<Option<Value>, ()> {
        let alias = source.ssh_host.as_deref();
        let (primary_host, _) = source_host(&source.platform, alias);
        let selector = selector(source);
        let credential = if legacy {
            Value::Null
        } else {
            json!(source.credential)
        };
        let binding = String::from_utf8(aac_views::js_json::write(&json!([
            source.id,
            source.provider,
            source.platform,
            alias,
            credential
        ])))
        .map_err(|_| ())?;
        let target = aac_core::sha256_hex(aac_views::js_json::write(&json!([
            self.root, source.provider, source.id, binding,
            [{"host":primary_host,"platform":source.platform,"sshHost":alias,"selector":selector}],
            null, null
        ])));
        let mut state = provider_caches::usage_observation(&self.root, &target)
            .filter(|value| value["version"] == 1 && value["key"] == target)
            .unwrap_or_else(|| json!({
                "version":1,"key":target,"identity":null,"primary":null,"saved":null,
                "nextAttempt":0,"fresh":false,"freshUntil":0,"primaryFailure":null,
                "primarySignIn":[],"observedPlan":null,"observedPlanAt":null,"observedPlanPeer":null
            }));
        let saved = &state["saved"];
        if !super::valid_candidate(&saved["candidate"], &source.provider)
            || !state["identity"].is_object()
            || !linked(
                &self.root,
                source,
                &target,
                &state["identity"],
                &saved["candidate"],
                saved["host"].as_str().unwrap_or_default(),
            )
            || measured(&saved["reading"], now).is_none()
        {
            state["saved"] = Value::Null;
        }
        if state["nextAttempt"]
            .as_i64()
            .is_some_and(|at| at > now && at <= now + 86_400_000)
            && state["identity"].is_object()
        {
            return Ok(project(&state, state["fresh"] != true));
        }
        let inventory = self
            .inventory(&source.platform, alias, &source.provider)
            .await;
        let unsupported = inventory.as_ref().err() == Some(&"unsupported");
        let inventory = inventory.ok();
        let now = now_at_least(now);
        let mut candidate = pick(&inventory, &source.provider, &selector).cloned();
        let configured_unreadable = !inventory_complete(&inventory, &selector);
        let configured = (
            UsageHost {
                id: primary_host.clone(),
                platform: platform_word(&source.platform),
                alias: source.ssh_host.clone(),
            },
            inventory,
        );
        let mut hosts = Vec::new();
        let mut selected_host = &configured.0;
        if candidate.is_none() && source.provider == "cursor" {
            hosts = self.peer_inventories(&source.provider, &primary_host).await;
            if let Some((host, selected)) = cursor_session(
                std::iter::once(&configured).chain(hosts.iter()),
                &state["identity"],
                configured_unreadable,
            ) {
                candidate = Some(selected.clone());
                selected_host = host;
            }
        }
        if let Some(candidate) = &candidate {
            if state["identity"] != candidate["identity"] {
                state["saved"] = Value::Null;
                state["identity"] = candidate["identity"].clone();
            }
            state["primary"] = candidate.clone();
        } else if !configured_unreadable {
            state["identity"] = Value::Null;
            state["primary"] = Value::Null;
            state["saved"] = Value::Null;
            self.save_target(&target, &state);
            return Ok(None);
        }
        if !state["identity"].is_object() {
            return if unsupported { Err(()) } else { Ok(None) };
        }
        let identity = state["identity"].clone();
        let primary_key = identity_key(&self.root, &source.provider, &identity);
        state["fresh"] = json!(false);
        state["nextAttempt"] = json!(now + 30_000);
        if gate(&self.root, &primary_key, now).is_none_or(|until| until > now) {
            state["primaryFailure"] = json!("rate_limited");
            return Ok(project(&state, true));
        }
        self.save_target(&target, &state);
        let mut tried = HashSet::new();
        let mut good = false;
        if let Some(candidate) = &candidate {
            tried.insert((
                selected_host.id.clone(),
                candidate["locator"].as_str().unwrap_or_default().to_owned(),
            ));
            good = self
                .try_peer(
                    source,
                    &target,
                    &mut state,
                    selected_host.platform,
                    selected_host.alias.as_deref(),
                    &selected_host.id,
                    candidate,
                    false,
                    now,
                )
                .await;
        }
        if !good && gate(&self.root, &primary_key, now).is_some_and(|until| until <= now) {
            if hosts.is_empty() {
                hosts = self.peer_inventories(&source.provider, &primary_host).await;
            }
            for (host, candidates) in std::iter::once(configured).chain(hosts) {
                for candidate in candidates
                    .as_ref()
                    .map(inventory_candidates)
                    .into_iter()
                    .flatten()
                {
                    if !super::valid_candidate(candidate, &source.provider)
                        || !linked(&self.root, source, &target, &identity, candidate, &host.id)
                        || !tried.insert((
                            host.id.clone(),
                            candidate["locator"].as_str().unwrap_or_default().to_owned(),
                        ))
                    {
                        continue;
                    }
                    if self
                        .try_peer(
                            source,
                            &target,
                            &mut state,
                            host.platform,
                            host.alias.as_deref(),
                            &host.id,
                            candidate,
                            true,
                            now,
                        )
                        .await
                    {
                        good = true;
                        break;
                    }
                }
                if good || gate(&self.root, &primary_key, now).is_none_or(|until| until > now) {
                    break;
                }
            }
        }
        let now = now_at_least(now);
        if let Some(until) = gate(&self.root, &primary_key, now).filter(|until| *until > now) {
            state["nextAttempt"] = json!(until);
            state["primaryFailure"] = json!("rate_limited");
        }
        self.save_target(&target, &state);
        Ok(project(&state, !good))
    }

    async fn peer_inventories(
        &self,
        provider: &str,
        primary: &str,
    ) -> Vec<(UsageHost, Option<Value>)> {
        stream::iter(
            self.agents
                .usage_hosts()
                .await
                .into_iter()
                .filter(|host| host.id != primary),
        )
        .map(|host| async move {
            let inventory = self
                .inventory(host.platform, host.alias.as_deref(), provider)
                .await
                .ok();
            (host, inventory)
        })
        .buffered(3)
        .collect()
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the peer probe retains selected and fallback account bindings"
    )]
    async fn try_peer(
        &self,
        source: &Source,
        target: &str,
        state: &mut Value,
        platform: &str,
        alias: Option<&str>,
        host: &str,
        candidate: &Value,
        fallback: bool,
        now: i64,
    ) -> bool {
        let now = now_at_least(now);
        let primary = identity_key(&self.root, &source.provider, &state["identity"]);
        if gate(&self.root, &primary, now).is_none_or(|until| until > now) {
            return false;
        }
        let peer = identity_key(&self.root, &source.provider, &candidate["identity"]);
        if peer != primary && gate(&self.root, &peer, now).is_none_or(|until| until > now) {
            return false;
        }
        let Some(reply) = self
            .attempt(
                platform,
                alias,
                &source.provider,
                candidate,
                (peer != primary).then_some(primary.as_str()),
                now,
            )
            .await
        else {
            return false;
        };
        let now = now_at_least(now);
        let failure = &reply["failureCode"];
        if !fallback {
            state["primaryFailure"] = failure.clone();
            if matches!(
                failure.as_str(),
                Some("identity_mismatch" | "account_mismatch" | "identity_unavailable")
            ) {
                state["saved"] = Value::Null;
            }
        }
        if !super::confirmed(&reply, candidate)
            || matches!(
                failure.as_str(),
                Some(
                    "needs_sign_in"
                        | "identity_mismatch"
                        | "account_mismatch"
                        | "identity_unavailable"
                        | "source_changed"
                )
            )
        {
            return false;
        }
        let reading = &reply["sample"];
        if !linked(
            &self.root,
            source,
            target,
            &state["identity"],
            candidate,
            host,
        ) {
            return false;
        }
        let Some(sampled) = measured(reading, now) else {
            return false;
        };
        if measured(&state["saved"]["reading"], now).is_some_and(|old| old > sampled) {
            return false;
        }
        let fresh = reading["status"] == "ok";
        state["saved"] =
            json!({"reading":reading,"candidate":candidate,"host":host,"fallback":fallback});
        state["fresh"] = json!(fresh);
        if fresh {
            let refresh = reply["refreshAfterSeconds"]
                .as_u64()
                .unwrap_or(0)
                .min(86400);
            let wait = refresh.max(super::read_refresh(&self.root)) as i64;
            state["nextAttempt"] = json!(now + wait * 1000);
            state["freshUntil"] = state["nextAttempt"].clone();
        }
        fresh
    }

    pub(super) fn save_target(&self, target: &str, state: &Value) {
        let bytes = aac_views::js_json::write(state);
        let _ = crate::collection_cache::write(
            &self.owners,
            &self.root,
            crate::collection_cache::OBSERVATIONS,
            target,
            &bytes,
        );
    }
}

fn cursor_session<'a>(
    hosts: impl IntoIterator<Item = &'a (UsageHost, Option<Value>)>,
    anchor: &Value,
    configured_unreadable: bool,
) -> Option<(&'a UsageHost, &'a Value)> {
    let mut sessions = hosts.into_iter().flat_map(|(host, candidates)| {
        candidates
            .as_ref()
            .map(inventory_candidates)
            .into_iter()
            .flatten()
            .filter(|candidate| {
                super::valid_candidate(candidate, "cursor")
                    && candidate["selectors"]
                        .as_array()
                        .is_some_and(|selectors| selectors.iter().any(|word| word == "discover"))
            })
            .map(move |candidate| (host, candidate))
    });
    let (host, selected) = sessions.next()?;
    let identity = &selected["identity"];
    if (configured_unreadable && anchor.is_object() && anchor != identity)
        || sessions.any(|(_, peer)| &peer["identity"] != identity)
    {
        return None;
    }
    Some((host, selected))
}

fn inventory_candidates(inventory: &Value) -> impl Iterator<Item = &Value> {
    inventory["candidates"]
        .as_array()
        .filter(|rows| {
            inventory["version"] == 1
                && inventory["action"] == "inventory"
                && inventory["failed"] != true
                && rows.len() <= 128
        })
        .into_iter()
        .flatten()
}

fn gate(root: &std::path::Path, key: &str, now: i64) -> Option<i64> {
    Some(
        failover::persisted(root, key, now_at_least(now) as f64)
            .ok()?
            .unwrap_or_default()
            .until
            .ceil() as i64,
    )
}

fn project(state: &Value, cached: bool) -> Option<Value> {
    let saved = &state["saved"];
    let mut reading = saved["reading"].clone();
    let values = reading.as_object_mut()?;
    if let Some(host) = saved["host"].as_str() {
        values.insert(
            "verifiedSource".into(),
            json!({
                "host":host,"fallback":saved["fallback"] == true,
                "linked":saved["candidate"]["identity"] != state["identity"],
            }),
        );
    }
    if cached {
        values.insert("status".into(), json!("cached"));
    }
    if cached {
        let reason = if state["primaryFailure"] == "rate_limited" {
            "rate_limited"
        } else if state["fresh"].is_null() {
            "outcome_unknown"
        } else {
            "read_failed"
        };
        values.insert("cacheReason".into(), json!(reason));
    }
    Some(reading)
}

fn platform_word(platform: &str) -> &'static str {
    match platform {
        "mac" => "mac",
        "windows" => "windows",
        _ => "ubuntu",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor_host(host: &str, key: &str) -> (UsageHost, Option<Value>) {
        let kind = aac_providers::provider("cursor")
            .expect("registered")
            .failover
            .kinds[0];
        (
            UsageHost {
                id: host.into(),
                platform: "mac",
                alias: Some(host.into()),
            },
            Some(json!({
                "version":1,"action":"inventory","candidates":[{
                    "selectors":["discover"],"locator":"a".repeat(64),"revision":"b".repeat(64),
                    "identity":{"kind":kind,"key":key.repeat(64)}
                }]
            })),
        )
    }

    #[test]
    fn cursor_requires_one_peer_identity_and_keeps_an_unreadable_primary_anchor() {
        let peers = [cursor_host("mac", "c"), cursor_host("windows", "d")];
        assert!(cursor_session(&peers, &Value::Null, false).is_none());
        let same = [cursor_host("mac", "c"), cursor_host("windows", "c")];
        let (host, selected) =
            cursor_session(&same, &Value::Null, false).expect("one verified identity");
        assert_eq!(host.id, "mac");
        assert!(cursor_session(&peers[1..], &selected["identity"], true).is_none());
        assert!(cursor_session(&peers[1..], &selected["identity"], false).is_some());
        assert!(cursor_session(&same, &selected["identity"], true).is_some());
    }

    #[test]
    fn muse_grant_is_read_for_each_attempt_and_removal_rejects_the_peer() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = tempfile::tempdir().expect("fixture root");
        let root = dir.path();
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).expect("private fixture");
        let target = "b".repeat(64);
        let folder = root.join("account-usage-failover");
        fs::create_dir(&folder).expect("cache folder");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).expect("private cache");
        let file = folder.join(format!("{target}.muse-peers.json"));
        let primary = json!({"kind":"muse-team","key":"b".repeat(64)});
        let peer = json!({"kind":"muse-native-credential","key":"c".repeat(64)});
        let candidate = json!({"identity":peer});
        let source = Source {
            id: "muse:usage".into(),
            provider: "muse".into(),
            platform: "mac".into(),
            ssh_host: None,
            label: None,
            credential: Credential::Discover,
            created_at: None,
            created_by: None,
        };
        assert!(!linked(
            root, &source, &target, &primary, &candidate, "windows"
        ));
        fs::write(&file, json!({
            "version":1,"key":target,"identity":primary,"peers":[{"host":"windows","identity":peer}]
        }).to_string()).expect("grant fixture");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("private grant");
        assert!(linked(
            root, &source, &target, &primary, &candidate, "windows"
        ));
        assert!(!linked(root, &source, &target, &primary, &candidate, "mac"));
        fs::remove_file(file).expect("revoke fixture grant");
        assert!(!linked(
            root, &source, &target, &primary, &candidate, "windows"
        ));
    }
}
