//! Resolve the scoped Windows desktop profile and preserve its per-target cadence.
use super::{Producer, inventory_complete, pick};
use crate::account_usage::{failover, provider_caches, rows};
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value, json};

#[derive(Clone, Default)]
pub(super) struct Saved {
    next: f64,
    identity: Value,
    primary: Value,
    sample: Option<Value>,
}

impl Saved {
    fn read(root: &std::path::Path, target: &str, email: &str, now: i64) -> Option<Self> {
        let value = provider_caches::usage_observation(root, target)?;
        let primary = &value["primary"];
        let saved = &value["saved"];
        if value["version"] != 1
            || value["key"] != target
            || !super::valid_candidate(primary, "claude")
            || value["identity"] != primary["identity"]
            || !primary["email"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(email))
        {
            return None;
        }
        let raw = &saved["reading"];
        let sample = (saved["host"] == "windows"
            && saved["candidate"] == *primary
            && raw["email"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(email))
            && sample_time(raw).is_some_and(|stamp| stamp <= now)
            && !rows::windows(&raw["windows"], 256).is_empty())
        .then(|| {
            if value["fresh"] == true {
                Some(raw.clone())
            } else {
                cached(Some(raw.clone()))
            }
        })
        .flatten();
        Some(Self {
            next: value["nextAttempt"]
                .as_f64()
                .unwrap_or(0.0)
                .clamp(0.0, now as f64 + 86_400_000.0),
            identity: value["identity"].clone(),
            primary: primary.clone(),
            sample,
        })
    }

    fn write_state(&self, producer: &Producer, target: &str, failure: &Value) {
        let saved = self.sample.as_ref().map(|reading| {
            json!({"candidate":self.primary,"reading":reading,"host":"windows","fallback":false})
        });
        let fresh = self
            .sample
            .as_ref()
            .is_some_and(|sample| sample["status"] == "ok");
        producer.save_target(target, &json!({
            "version":1,"key":target,"identity":self.identity,"primary":self.primary,"saved":saved,
            "nextAttempt":self.next,"fresh":fresh,"freshUntil":if fresh {self.next} else {0.0},
            "primaryFailure":failure,"primarySignIn":[],"observedPlan":null,"observedPlanAt":null,
            "observedPlanPeer":null
        }));
    }
}

fn manifest_profile(profile: &Value) -> Value {
    let mut result = Map::new();
    for field in ["email", "id"] {
        if let Some(value) = profile.get(field) {
            result.insert(field.into(), value.clone());
        }
    }
    for platform in ["mac", "windows"] {
        let Some(launcher) = profile.get(platform) else {
            continue;
        };
        let mut fields = Map::new();
        for field in [
            "launcherName",
            "launcherPath",
            "profilePath",
            "startMenuPath",
            "isDefault",
            "sshHost",
        ] {
            if field == "startMenuPath" && platform == "mac" {
                continue;
            }
            if let Some(value) = launcher.get(field) {
                fields.insert(field.into(), value.clone());
            }
        }
        result.insert(platform.into(), Value::Object(fields));
    }
    Value::Object(result)
}

impl Producer {
    pub(super) async fn claude_rows(
        &self,
        profiles: &[Value],
        refresh: u64,
        now: i64,
    ) -> Vec<Option<Value>> {
        stream::iter(profiles.iter().cloned())
            .map(|profile| async move { self.claude_row(&profile, refresh, now).await })
            .buffered(3)
            .collect()
            .await
    }

    async fn claude_row(&self, profile: &Value, refresh: u64, now: i64) -> Option<Value> {
        let private_profile = manifest_profile(profile);
        let binding = String::from_utf8(aac_views::js_json::write(&private_profile)).ok()?;
        let email = profile["email"].as_str()?;
        let id = profile["id"].as_str();
        let windows = profile.get("windows");
        let alias = windows.and_then(|launcher| launcher["sshHost"].as_str());
        let snapshot = || -> Option<Value> {
            let mut sample = provider_caches::claude_snapshot(
                &self.root,
                id?,
                &aac_core::sha256_hex(binding.as_bytes()),
            )
            .ok()??;
            if sample["profileId"] != id?
                || sample["email"] != email
                || sample_time(&sample).is_none_or(|sampled| sampled > now)
            {
                return None;
            }
            sample["status"] = json!("cached");
            Some(sample)
        };
        let Some(launcher) = windows.filter(|_| alias.is_some() || cfg!(windows)) else {
            return snapshot();
        };
        let path = launcher["profilePath"].as_str();
        let default = launcher["isDefault"] == true
            || path.is_some_and(|path| {
                path.trim_end_matches(['/', '\x5c'])
                    .rsplit(['/', '\x5c'])
                    .next()
                    .is_some_and(|last| last.eq_ignore_ascii_case("claude"))
            });
        let selector = format!(
            "claude-desktop-profile:{}",
            if default {
                "default"
            } else {
                id.unwrap_or("default")
            }
        );
        let mut scoped_profile = json!({"id":id.unwrap_or("default"),"email":email});
        if let Some(path) = path {
            scoped_profile["profilePath"] = json!(path);
        }
        scoped_profile["isDefault"] = json!(default);
        let target = aac_core::sha256_hex(aac_views::js_json::write(&json!([
            self.root, "claude", format!("claude:{}", id.unwrap_or(email)), binding,
            [{"host":"windows","platform":"windows","sshHost":alias,"selector":selector,"claudeProfile":scoped_profile}],
            email.to_lowercase(), null
        ])));
        let mut saved = Saved::read(&self.root, &target, email, now).unwrap_or_default();
        if saved.next > now as f64 {
            return saved.sample.or_else(snapshot);
        }
        let mut args = json!({"version":1,"action":"inventory","provider":"claude","claudeProfile":scoped_profile});
        if alias.is_none() {
            args["ccsDir"] = json!(self.root);
        }
        let inventory = self
            .ask("windows", alias, "failover-inventory", &args)
            .await
            .ok();
        let candidate = pick(&inventory, "claude", &selector).filter(|candidate| {
            candidate["email"]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(email))
        });
        let Some(candidate) = candidate else {
            if inventory_complete(&inventory, &selector) {
                Saved::default().write_state(self, &target, &Value::Null);
                return None;
            }
            return cached(saved.sample).or_else(snapshot);
        };
        if saved.identity != candidate["identity"] {
            saved.sample = None;
            saved.identity = candidate["identity"].clone();
        }
        if saved.primary["revision"] != candidate["revision"]
            && let Some(sample) = &mut saved.sample
        {
            if let Some(fields) = sample.as_object_mut() {
                fields.remove("optionalExtras");
                fields.remove("optionalExtrasContext");
            }
            if let Some(windows) = sample["windows"].as_array_mut() {
                windows.retain(|window| !optional_window(window));
            }
        }
        saved.primary = candidate.clone();
        let key = super::identity_key(&self.root, "claude", &saved.identity);
        let _active = self.lock_identities(&key, None).await;
        let now = super::now_at_least(now);
        let held = failover::persisted(&self.root, &key, now as f64)
            .ok()?
            .unwrap_or_default();
        let mut failure;
        if held.until > now as f64 {
            failure = json!("rate_limited");
            saved.next = held.until;
            saved.sample = cached(saved.sample);
        } else {
            args["action"] = json!("collect");
            args["candidate"] = candidate.clone();
            saved.sample = cached(saved.sample);
            saved.next = now as f64 + 15_000.0;
            saved.write_state(self, &target, &Value::Null);
            let response = self
                .ask("windows", alias, "failover-collect", &args)
                .await
                .ok()
                .unwrap_or_default();
            let observed = aac_core::UtcMs::now()
                .ok()
                .map_or(now as f64, |now| now.get() as f64);
            saved.next = observed + 15_000.0;
            failure = response["failureCode"].clone();
            if super::limited(&response) {
                failure = json!("rate_limited");
                let updated = held
                    .claude_rate_limited(&target, observed, response["retryAfterSeconds"].as_f64())
                    .ok()?;
                saved.next = updated.until;
                super::save_hold(&self.owners, &self.root, &key, observed, updated);
            } else if super::confirmed(&response, candidate)
                && response["failureCode"].is_null()
                && response["sample"]["status"] == "ok"
                && response["sample"]["email"]
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case(email))
                && sample_time(&response["sample"]).is_some_and(|stamp| {
                    stamp <= observed as i64
                        && saved
                            .sample
                            .as_ref()
                            .and_then(sample_time)
                            .is_none_or(|old| stamp >= old)
                })
                && !rows::windows(&response["sample"]["windows"], 256).is_empty()
            {
                let updated = held.verified_success(observed);
                if updated != held {
                    super::save_hold(&self.owners, &self.root, &key, observed, updated);
                }
                let floor =
                    (refresh as f64).max(response["refreshAfterSeconds"].as_f64().unwrap_or(0.0));
                saved.next = observed + failover::claude_delay_ms(&target, floor, 0).ok()?;
                saved.sample = Some(response["sample"].clone());
            }
        }
        saved.write_state(self, &target, &failure);
        let result = saved.sample.clone().or_else(snapshot);
        result
    }
}

/// Read pending lifecycle rows from the same private store as the route owner.
pub(super) fn pending(root: &std::path::Path) -> Option<Vec<Value>> {
    let value = provider_caches::load(root, "claude-pending", None, true).ok()??;
    let profiles = value["profiles"].as_array()?;
    if value["version"] != 1 || profiles.len() > 64 {
        return None;
    }
    let mut ids = std::collections::HashSet::new();
    for item in profiles {
        let id = item["id"].as_str()?;
        if !(2..=32).contains(&id.len())
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !ids.insert(id)
            || rows::stamp(&item["createdAt"]).is_none()
            || (!item["label"].is_null()
                && aac_views::antigravity::display_text(Some(&item["label"]), 96).is_none())
            || ["mac", "windows"].iter().any(|platform| {
                let host = &item[*platform];
                !host["sshHost"].is_string()
                    || !host["launcherName"].is_string()
                    || !host["profilePath"].is_string()
            })
        {
            return None;
        }
    }
    Some(profiles.clone())
}

fn optional_window(window: &Value) -> bool {
    window["key"].as_str().is_some_and(|key| {
        matches!(
            key,
            "prepaid_balance" | "reset_credits_available" | "session_reset_allowance"
        ) || ["available", "saved", "used", "expired"]
            .iter()
            .any(|state| {
                key.strip_prefix("reset_credit_")
                    .and_then(|tail| tail.strip_prefix(state))
                    .and_then(|tail| tail.strip_prefix("_grant_"))
                    .and_then(|tail| tail.parse::<u8>().ok())
                    .is_some_and(|n| (1..=20).contains(&n))
            })
    })
}

fn sample_time(sample: &Value) -> Option<i64> {
    let stamp = rows::stamp(&sample["sampledAt"]).or_else(|| rows::stamp(&sample["fetchedAt"]))?;
    aac_views::js::parse_date(&stamp).ok()?
}

fn cached(sample: Option<Value>) -> Option<Value> {
    let mut sample = sample?;
    sample["status"] = json!("cached");
    Some(sample)
}
