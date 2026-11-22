//! Select the installed quota, API key and console readers for each configured source.
use super::{Producer, pick, source_host};
use crate::account_usage::coordinator::{Discovered, UsageIdentity};
use crate::account_usage::{
    cursor::Command,
    provider_caches, rows,
    sources::{Credential, Source},
};
use serde_json::{Value, json};

/// One extra account's row with what tells two registry entries of one account apart.
struct Keyed {
    provider: String,
    /// The entry discovers its credential instead of naming its own.
    discovered: bool,
    /// The credential identity its shown reading came from, when known.
    identity: Option<UsageIdentity>,
    row: Value,
}

/// The `aac-key:<id>` selectors other entries of the provider on the same computer already name.
fn bound_selectors(sources: &[Source], source: &Source) -> Vec<String> {
    sources
        .iter()
        .filter(|other| {
            *other != source
                && other.provider == source.provider
                && other.platform == source.platform
                && other.ssh_host == source.ssh_host
        })
        .filter_map(|other| match &other.credential {
            Credential::AacKey { key_id } => Some(format!("aac-key:{key_id}")),
            _ => None,
        })
        .collect()
}

/// The stored identities once the named entries whose key files could not be read are settled:
/// each such entry's last known identity counts as named. None when one has no known identity,
/// so the discover entry waits (no row, no read) rather than show one account twice.
fn settle_unread(
    discovered: Discovered,
    known: impl Fn(&str) -> Option<UsageIdentity>,
) -> Option<Vec<(UsageIdentity, bool)>> {
    let mut found = discovered.found;
    for selector in &discovered.unread {
        let identity = known(selector)?;
        for (stored, bound) in &mut found {
            if *stored == identity {
                *bound = true;
            }
        }
    }
    Some(found)
}

/// A discover entry's rows from the stored identities: one per identity no other entry names,
/// with the id `<entry id>:<digest>` and the label `Key <digest>` (the identity's digest, never
/// the key). None (unreadable) or an
/// empty store keeps the entry as it was; only named identities means no row.
fn pinned_sources(
    source: &Source,
    found: Option<Vec<(UsageIdentity, bool)>>,
) -> Vec<(Source, Option<UsageIdentity>)> {
    let Some(found) = found.filter(|found| !found.is_empty()) else {
        return vec![(source.clone(), None)];
    };
    found
        .into_iter()
        .filter(|(_, bound)| !bound)
        .map(|(identity, _)| {
            let digest = identity.key.get(..8).unwrap_or(&identity.key).to_owned();
            let mut pinned = source.clone();
            pinned.id = format!("{}:{digest}", source.id);
            pinned.label = Some(format!("Key {digest}"));
            (pinned, Some(identity))
        })
        .collect()
}

/// Two registry entries whose readings came from the same credential identity are one account:
/// show it once (never two rows of the same account). The row bound to its own credential stays
/// over a `discover` row, then the first in list order. Rows without a known identity are never
/// merged and nothing is rebound (the TS `oneRowPerIdentity`).
fn one_row_per_identity(rows: Vec<Keyed>) -> Vec<Value> {
    let mut dropped = vec![false; rows.len()];
    for index in 0..rows.len() {
        let Some(identity) = rows[index].identity.as_ref().filter(|_| !dropped[index]) else {
            continue;
        };
        for other in index + 1..rows.len() {
            if dropped[other]
                || rows[other].provider != rows[index].provider
                || rows[other].identity.as_ref() != Some(identity)
            {
                continue;
            }
            if rows[index].discovered && !rows[other].discovered {
                dropped[index] = true;
                break;
            }
            dropped[other] = true;
        }
    }
    rows.into_iter()
        .zip(dropped)
        .filter_map(|(keyed, dropped)| (!dropped).then_some(keyed.row))
        .collect()
}

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
        let sources = if problem.is_none() {
            self.expand_discovery(sources, legacy).await
        } else {
            sources.into_iter().map(|source| (source, None)).collect()
        };
        let read: Vec<(Source, Option<UsageIdentity>, Value)> = stream::iter(sources)
            .filter(|(source, _)| {
                std::future::ready(!antigravity || source.provider != "antigravity")
            })
            .map(|(source, pin)| async move {
                let row = if let Some((status, message)) = problem {
                    let (host, name) = source_host(&source.platform, source.ssh_host.as_deref());
                    let mut row = rows::additional(&source, &host, &name, None);
                    row["status"] = json!(status);
                    row["message"] = json!(message);
                    row
                } else {
                    self.pinned_row(&source, pin.as_ref(), legacy, now, force)
                        .await
                };
                (source, pin, row)
            })
            .buffered(3)
            .collect()
            .await;
        let mut result = if problem.is_none() {
            let failover = self.failover();
            one_row_per_identity(
                read.into_iter()
                    .map(|(source, pin, row)| {
                        let mut target =
                            super::failover::additional_target(&self.root, &source, legacy);
                        target.expected_identity = pin;
                        Keyed {
                            provider: source.provider.clone(),
                            discovered: matches!(source.credential, Credential::Discover),
                            identity: failover.reading_identity(&target),
                            row,
                        }
                    })
                    .collect(),
            )
        } else {
            read.into_iter().map(|(_, _, row)| row).collect()
        };
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
        self.pinned_row(source, None, legacy, now, force).await
    }

    /// Owner, 2026-11-21 (card OCG1): every OpenCode Go key stored on the computer is read,
    /// once (the TS `expandDiscovery`). A `discover` entry stands for each stored key no other
    /// entry of the provider already names (`aac-key:<id>`): one row per such key, pinned to
    /// that key's identity. When every stored key is already named, the entry adds no row and
    /// no read; an unreadable store keeps the entry as it was.
    pub(super) async fn expand_discovery(
        &self,
        sources: Vec<Source>,
        legacy: bool,
    ) -> Vec<(Source, Option<UsageIdentity>)> {
        let mut out = Vec::with_capacity(sources.len());
        for source in &sources {
            if source.provider != "opencode-go"
                || !matches!(source.credential, Credential::Discover)
            {
                out.push((source.clone(), None));
                continue;
            }
            let target = super::failover::additional_target(&self.root, source, legacy);
            let failover = self.failover();
            let found = match failover
                .discovered_identities(&target, &bound_selectors(&sources, source))
                .await
            {
                None => None,
                Some(discovered) => {
                    // A named entry whose key file could not be read stands in with its last known
                    // identity; with none known the entry adds no row and no read this time.
                    let known = |selector: &str| {
                        sources
                            .iter()
                            .find(|other| {
                                *other != source
                                    && other.provider == source.provider
                                    && matches!(&other.credential, Credential::AacKey { key_id } if format!("aac-key:{key_id}") == selector)
                            })
                            .and_then(|other| {
                                failover.reading_identity(&super::failover::additional_target(
                                    &self.root, other, legacy,
                                ))
                            })
                    };
                    match settle_unread(discovered, known) {
                        Some(found) => Some(found),
                        None => continue,
                    }
                }
            };
            out.extend(pinned_sources(source, found));
        }
        out
    }

    /// One source's row, its read pinned to one stored key's identity when it has one.
    pub(super) async fn pinned_row(
        &self,
        source: &Source,
        pin: Option<&UsageIdentity>,
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
        let mut target = super::failover::additional_target(&self.root, source, legacy);
        target.expected_identity = pin.cloned();
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

#[cfg(test)]
mod tests {
    use super::{
        Discovered, Keyed, UsageIdentity, bound_selectors, one_row_per_identity, pinned_sources,
        settle_unread,
    };
    use crate::account_usage::sources::{Credential, Source};
    use serde_json::json;

    fn source(id: &str, credential: Credential) -> Source {
        Source {
            id: id.to_owned(),
            provider: "opencode-go".to_owned(),
            platform: "ubuntu".to_owned(),
            ssh_host: None,
            label: Some("OpenCode Go".to_owned()),
            credential,
            created_at: None,
            created_by: None,
        }
    }

    fn identity(letter: &str) -> UsageIdentity {
        UsageIdentity {
            kind: "opencode-go-credential".to_owned(),
            key: letter.repeat(64),
        }
    }

    fn key(id: &str) -> Credential {
        Credential::AacKey {
            key_id: id.to_owned(),
        }
    }

    #[test]
    fn three_distinct_stored_keys_give_three_rows_each_read_once() {
        let sources = [
            source("opencode-go:usage", Credential::Discover),
            source("opencode-go:acct:52f3251b", key("52f3251b")),
            source("opencode-go:acct:2e034ec2", key("2e034ec2")),
        ];
        assert_eq!(
            bound_selectors(&sources, &sources[0]),
            ["aac-key:52f3251b", "aac-key:2e034ec2"]
        );
        // Two stored keys are the two named entries' keys; the third is read by the discover entry.
        let pinned = pinned_sources(
            &sources[0],
            Some(vec![
                (identity("a"), true),
                (identity("b"), true),
                (identity("c"), false),
            ]),
        );
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].0.id, "opencode-go:usage:cccccccc");
        assert_eq!(pinned[0].0.label.as_deref(), Some("Key cccccccc"));
        assert_eq!(pinned[0].1, Some(identity("c")));
        let rows = one_row_per_identity(
            [
                (pinned[0].0.id.as_str(), true, "c"),
                ("opencode-go:acct:52f3251b", false, "b"),
                ("opencode-go:acct:2e034ec2", false, "a"),
            ]
            .into_iter()
            .map(|(id, discovered, letter)| Keyed {
                provider: "opencode-go".to_owned(),
                discovered,
                identity: Some(identity(letter)),
                row: json!({ "id": id }),
            })
            .collect(),
        );
        assert_eq!(
            ids(rows),
            [
                "opencode-go:usage:cccccccc",
                "opencode-go:acct:52f3251b",
                "opencode-go:acct:2e034ec2"
            ]
        );
    }

    #[test]
    fn a_discovery_entry_alone_gives_one_row_per_stored_key() {
        let alone = source("opencode-go:usage", Credential::Discover);
        let pinned = pinned_sources(
            &alone,
            Some(vec![
                (identity("a"), false),
                (identity("b"), false),
                (identity("c"), false),
            ]),
        );
        let ids: Vec<&str> = pinned.iter().map(|(s, _)| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "opencode-go:usage:aaaaaaaa",
                "opencode-go:usage:bbbbbbbb",
                "opencode-go:usage:cccccccc"
            ]
        );
    }

    #[test]
    fn named_keys_add_no_row_and_an_unreadable_store_keeps_the_entry() {
        let entry = source("opencode-go:usage", Credential::Discover);
        assert!(pinned_sources(&entry, Some(vec![(identity("a"), true)])).is_empty());
        for found in [None, Some(Vec::new())] {
            let kept = pinned_sources(&entry, found);
            assert_eq!(kept.len(), 1);
            assert_eq!(kept[0].0, entry);
            assert_eq!(kept[0].1, None);
        }
    }

    fn keyed(id: &str, provider: &str, discovered: bool, identity: Option<&str>) -> Keyed {
        Keyed {
            provider: provider.to_owned(),
            discovered,
            identity: identity.map(|key| UsageIdentity {
                kind: "opencode-go-key".to_owned(),
                key: key.repeat(64),
            }),
            row: json!({ "id": id }),
        }
    }

    fn ids(rows: Vec<serde_json::Value>) -> Vec<String> {
        rows.iter()
            .map(|row| row["id"].as_str().unwrap_or("").to_owned())
            .collect()
    }

    #[test]
    fn two_entries_of_one_credential_identity_show_one_row() {
        // discover and key 2e034ec2 read the same OpenCode Go account; the Z.ai rows have no
        // identity yet and a different provider never merges with OpenCode Go.
        let rows = one_row_per_identity(vec![
            keyed("opencode-go:usage", "opencode-go", true, Some("a")),
            keyed("opencode-go:acct:52f3251b", "opencode-go", false, Some("b")),
            keyed("opencode-go:acct:2e034ec2", "opencode-go", false, Some("a")),
            keyed("zai:acct:1", "zai", false, None),
            keyed("zai:acct:2", "zai", false, None),
            keyed("kimi-code:acct:1", "kimi-code", false, Some("a")),
        ]);
        assert_eq!(
            ids(rows),
            [
                "opencode-go:acct:52f3251b",
                "opencode-go:acct:2e034ec2",
                "zai:acct:1",
                "zai:acct:2",
                "kimi-code:acct:1"
            ]
        );
    }

    #[test]
    fn two_bound_entries_of_one_identity_keep_the_first() {
        let rows = one_row_per_identity(vec![
            keyed("opencode-go:acct:2e034ec2", "opencode-go", false, Some("a")),
            keyed("opencode-go:usage", "opencode-go", true, Some("a")),
            keyed("opencode-go:acct:52f3251b", "opencode-go", false, Some("a")),
        ]);
        assert_eq!(ids(rows), ["opencode-go:acct:2e034ec2"]);
    }

    #[test]
    fn an_unreadable_named_key_stands_in_with_its_last_identity_or_discovery_waits() {
        let discovered = || Discovered {
            found: vec![
                (identity("a"), false),
                (identity("b"), true),
                (identity("c"), false),
            ],
            unread: vec!["aac-key:2e034ec2".to_owned()],
        };
        let settled = settle_unread(discovered(), |selector| {
            (selector == "aac-key:2e034ec2").then(|| identity("a"))
        })
        .expect("known identity");
        let entry = source("opencode-go:usage", Credential::Discover);
        let ids: Vec<String> = pinned_sources(&entry, Some(settled))
            .into_iter()
            .map(|(s, _)| s.id)
            .collect();
        assert_eq!(ids, ["opencode-go:usage:cccccccc"]);
        assert_eq!(settle_unread(discovered(), |_| None), None);
    }
}
