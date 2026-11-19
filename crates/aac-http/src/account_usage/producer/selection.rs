//! Select the installed quota, API key and console readers for each configured source.
use super::{Producer, pick, source_host};
use crate::account_usage::{
    cursor::Command,
    provider_caches, rows,
    sources::{Credential, Source},
};
use serde_json::{Value, json};

impl Producer {
    pub(super) async fn additional(
        &self,
        mut sources: Vec<Source>,
        legacy: bool,
        problem: Option<(&'static str, &'static str)>,
        antigravity: bool,
        now: i64,
        force: bool,
    ) -> Vec<Value> {
        use futures_util::{StreamExt, stream};
        let order = [
            "antigravity",
            "muse",
            "cursor",
            "kimi-code",
            "qwen",
            "zai",
            "opencode-go",
        ];
        sources.sort_by_key(|source| {
            order
                .iter()
                .position(|id| *id == source.provider)
                .unwrap_or(order.len())
        });
        let mut result: Vec<_> = stream::iter(sources)
            .filter(|source| std::future::ready(!antigravity || source.provider != "antigravity"))
            .map(|source| async move {
                if let Some((status, message)) = problem {
                    let (host, name) = source_host(&source.platform, source.ssh_host.as_deref());
                    let mut row = rows::additional(&source, &host, &name, None);
                    row["status"] = json!(status);
                    row["message"] = json!(message);
                    row
                } else {
                    self.additional_row(&source, legacy, now, force).await
                }
            })
            .buffered(3)
            .collect()
            .await;
        if problem.is_none()
            && let Some(wallet) = self.wallet(now).await
        {
            result.push(wallet);
        }
        result
    }

    pub(super) async fn additional_row(
        &self,
        source: &Source,
        legacy: bool,
        now: i64,
        force: bool,
    ) -> Value {
        let (host, name) = source_host(&source.platform, source.ssh_host.as_deref());
        let template = || rows::additional(source, &host, &name, None);
        if source.provider == "cursor"
            && let Some(row) = self.cursor_quota(now, template(), force).await
        {
            return row;
        }
        let supported = matches!(
            source.credential,
            Credential::Discover
                | Credential::AacKey { .. }
                | Credential::BrowserCapsule { .. }
                | Credential::ConfigHome { .. }
        );
        if !supported {
            return template();
        }
        // The coordinator owns the read, the peer fallback and the display projection; the
        // template is the same `rows::additional` row TS handed the failover service.
        let target = super::failover::additional_target(&self.root, source, legacy);
        self.failover().collect(&target, &template()).await
    }

    async fn wallet(&self, now: i64) -> Option<Value> {
        let root = self.root.clone();
        let binding = tokio::task::spawn_blocking(move || provider_caches::wallet_source(&root))
            .await
            .ok()?
            .ok()??;
        let alias = binding["sshHost"].as_str()?;
        let inventory = self
            .ask(
                "mac",
                Some(alias),
                "failover-inventory",
                &json!({"version":1,"action":"inventory","provider":"opencode-wallet"}),
            )
            .await
            .ok();
        let candidate = pick(&inventory, "opencode-wallet", "wallet")?;
        let key = candidate["identity"]["key"].as_str()?;
        let account = format!("plan-opencode-go-console-mac-{}", &key[..12]);
        let hash = aac_core::sha256_hex(aac_views::js_json::write(&binding));
        let pointer = json!({"version":1,"binding":hash,"accountId":account});
        let _ = crate::collection_cache::write(
            &self.owners,
            &self.root,
            provider_caches::WALLET_ACCOUNT,
            &hash,
            &aac_views::js_json::write(&pointer),
        );
        let reading = self
            .read_primary("mac", Some(alias), "opencode-wallet", candidate, now)
            .await?;
        let windows = rows::windows(&reading["windows"], 4);
        if !matches!(reading["status"].as_str(), Some("ok" | "cached"))
            || !windows.iter().any(|w| {
                w["key"] == "zen-balance"
                    && w["kind"] == "balance"
                    && w["unit"] == "USD"
                    && w["remaining"].is_number()
            })
        {
            return None;
        }
        Some(json!({
            "id":account,"provider":"opencode-go","providerLabel":"OpenCode Go","label":"OpenCode console wallet",
            "email":null,"plan":reading["plan"],"platform":"mac","source":"Authenticated OpenCode console workspace on Mac",
            "status":reading["status"],"message":"Console workspace; API-key account identity has not been linked.",
            "fetchedAt":rows::stamp(&reading["fetchedAt"]),"sampledAt":rows::stamp(&reading["sampledAt"]),
            "isActive":false,"windows":windows,"capabilities":rows::capabilities(),
        }))
    }

    async fn cursor_quota(&self, now: i64, mut row: Value, force: bool) -> Option<Value> {
        let status = self
            .cursor
            .command(Command::KeyStatus, self.root.clone())
            .await
            .unwrap_or_default();
        if status["present"] == false && status["error"].is_null() {
            let _ = crate::collection_cache::write(
                &self.owners,
                &self.root,
                provider_caches::CURSOR_QUOTA,
                "no-key",
                b"{\"clear\":true}",
            );
            return None;
        }
        let fingerprint = (status["present"] == true)
            .then(|| status["fingerprint"].as_str())
            .flatten()
            .filter(|value| provider_caches::cursor_fingerprint(value));
        // Missing packaged reader leaves discovery available during the transition.
        if status.is_null() {
            return None;
        }
        let previous = fingerprint.and_then(|fingerprint| {
            provider_caches::cursor_quota(&self.root, fingerprint)
                .ok()
                .flatten()
        });
        if !force
            && let Some(previous) = &previous
            && let Some(checked) = previous["checkedAt"]
                .as_str()
                .and_then(|stamp| aac_views::js::parse_date(stamp).ok().flatten())
            && (0..600_000).contains(&now.saturating_sub(checked))
        {
            let mut cached = previous["account"].clone();
            cached["id"] = row["id"].clone();
            cached["label"] = row["label"].clone();
            return Some(cached);
        }
        let at = aac_num::ecma::to_iso_string(now)?;
        let answer = if fingerprint.is_some() {
            self.cursor
                .command(Command::Quota, self.root.clone())
                .await
                .unwrap_or_default()
        } else {
            Value::Null
        };
        let quota = &answer["quota"];
        let plan = &quota["planUsage"];
        let valid = |field: &str| plan[field].as_f64().filter(|n| n.is_finite() && *n >= 0.0);
        let reset = quota["billingCycleEndMs"]
            .as_i64()
            .and_then(aac_num::ecma::to_iso_string);
        let measure = |key: &str, label: &str, used: f64| {
            json!({
                "key":key,"label":label,"usedPercent":used,"remainingPercent":(100.0-used).max(0.0),
                "resetAt":reset,"windowMinutes":null,"used":null,"limit":null,"unit":null
            })
        };
        row["source"] = json!("Cursor API key");
        row["platform"] = json!(if cfg!(target_os = "macos") {
            "mac"
        } else if cfg!(windows) {
            "windows"
        } else {
            "ubuntu"
        });
        row.as_object_mut()?.remove("host");
        row["fetchedAt"] = json!(at);
        if let (Some(total), Some(auto), true) = (
            valid("totalPercentUsed"),
            valid("autoPercentUsed"),
            answer["state"] == "read",
        ) {
            row["status"] = json!("ok");
            row["message"] = Value::Null;
            row["sampledAt"] = json!(
                answer["readAt"]
                    .as_i64()
                    .and_then(aac_num::ecma::to_iso_string)
                    .unwrap_or_else(|| at.clone())
            );
            row["windows"] = json!([
                measure("plan-reported", "Included usage", total),
                measure("autoPercentUsed", "Cursor models", auto)
            ]);
        } else if answer["state"] == "key_rejected" {
            row["status"] = json!("needs_sign_in");
            row["message"] = json!("The key was rejected.");
        } else if let Some(old) = previous.as_ref().map(|old| &old["account"]).filter(|old| {
            old["sampledAt"].is_string() && old["windows"].as_array().is_some_and(|w| !w.is_empty())
        }) {
            row["status"] = json!("cached");
            row["sampledAt"] = old["sampledAt"].clone();
            row["windows"] = old["windows"].clone();
            row["message"] = json!(format!(
                "Cursor read failed; showing the cached reading from {}.",
                old["sampledAt"].as_str()?
            ));
        } else {
            row["message"] = json!("Cursor quota could not be read.");
        }
        if let Some(fingerprint) = fingerprint {
            let record =
                json!({"version":1,"fingerprint":fingerprint,"checkedAt":at,"account":row});
            let _ = crate::collection_cache::write(
                &self.owners,
                &self.root,
                provider_caches::CURSOR_QUOTA,
                fingerprint.strip_prefix("sha256:")?,
                &aac_views::js_json::write(&record),
            );
        }
        Some(row)
    }
}
