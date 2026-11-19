//! Additional-provider registry admission for the native collector.
use aac_files::read::{Ancestors, Mode, Owner, Read};
use aac_providers::{PROVIDERS, ProviderId};
use aac_views::decline::Decline;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Each source names an existing credential by an opaque selector.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Credential {
    /// Discover the migrated account with the provider's ordinary reader.
    Discover,
    /// AAC-owned key slot.
    AacKey {
        /// Opaque eight-hex slot id.
        key_id: String,
    },
    /// Qwen console session captured by the browser bridge.
    BrowserCapsule {
        /// Default capsule or an opaque eight-hex id.
        capsule_id: String,
    },
    /// Scoped config home.
    ConfigHome {
        /// Opaque eight-hex id.
        home_id: String,
    },
    /// Antigravity runtime profile.
    AntigravityProfile {
        /// Profile name from the Antigravity registry.
        profile_id: String,
    },
}

/// The source selector passed to the account readers.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    /// Stable account id.
    pub id: String,
    /// Provider id from the common table.
    pub provider: String,
    /// Helper OS platform, independent of the selected desktop.
    pub platform: String,
    /// Configured SSH alias. None means local.
    pub ssh_host: Option<String>,
    /// Label the owner chose.
    pub label: Option<String>,
    /// Credential selector.
    pub credential: Credential,
    /// Original creation stamp.
    pub created_at: Option<String>,
    /// Origin of the registry entry.
    pub created_by: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    accounts: Vec<Source>,
}

/// A proven absent or ready registry. An unreadable or unproved registry is a decline.
pub enum Sources {
    /// There is no v2 file. The caller can resolve v1.
    Legacy,
    /// Valid v2 data, including an explicitly empty account list.
    V2(Vec<Source>),
}

fn hex8(s: &str) -> bool {
    s.len() == 8
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn alias(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn additional(word: &str) -> Option<ProviderId> {
    PROVIDERS
        .iter()
        .find(|p| {
            p.dashboard
                && !matches!(p.id, ProviderId::Codex | ProviderId::Claude)
                && p.id.as_str() == word
        })
        .map(|p| p.id)
}

impl Source {
    fn check(&self, provider: ProviderId) -> Result<Option<(&'static str, &str)>, Decline> {
        let discover = format!("{}:usage", self.provider);
        let prefix = format!("{}:acct:", self.provider);
        if (self.id != discover && !self.id.strip_prefix(&prefix).is_some_and(hex8))
            || !["mac", "windows", "ubuntu"].contains(&self.platform.as_str())
            || self.ssh_host.as_deref().is_some_and(|s| !alias(s))
            || self.label.as_deref().is_some_and(|s| {
                !s.is_ascii()
                    || s.len() > 48
                    || s.trim().is_empty()
                    || s.bytes().any(|b| b.is_ascii_control())
            })
            || self
                .created_by
                .as_deref()
                .is_some_and(|s| !["migration", "dashboard"].contains(&s))
        {
            return Err(Decline::Shape);
        }
        if let Some(stamp) = &self.created_at
            && (!stamp.contains('T')
                || !stamp.as_bytes().get(16).is_some_and(|b| *b == b':')
                || aac_views::js::parse_date(stamp)
                    .map_err(|_| Decline::Date)?
                    .is_none())
        {
            return Err(Decline::Date);
        }
        match &self.credential {
            Credential::Discover
                if self.id == discover && self.created_by.as_deref() != Some("dashboard") =>
            {
                Ok((provider == ProviderId::Qwen).then_some(("capsule", "default")))
            }
            Credential::AacKey { key_id } if provider.row().key_store && hex8(key_id) => {
                Ok(Some(("key", key_id)))
            }
            Credential::BrowserCapsule { capsule_id }
                if provider == ProviderId::Qwen
                    && (capsule_id == "default" || hex8(capsule_id)) =>
            {
                Ok(Some(("capsule", capsule_id)))
            }
            Credential::ConfigHome { home_id }
                if matches!(provider, ProviderId::Muse | ProviderId::KimiCode) && hex8(home_id) =>
            {
                Ok(Some(("home", home_id)))
            }
            Credential::AntigravityProfile { profile_id }
                if provider == ProviderId::Antigravity
                    && (1..=48).contains(&profile_id.len())
                    && profile_id.as_bytes()[0].is_ascii_lowercase()
                    && profile_id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)
                    }) =>
            {
                Ok(Some(("profile", profile_id)))
            }
            _ => Err(Decline::Shape),
        }
    }
}

/// Parse the bounded v2 file. Unsupported labels or date spellings leave its TS reader in charge.
pub fn parse_registry(bytes: &[u8]) -> Result<Vec<Source>, Decline> {
    let registry: Registry = crate::json_object::from_slice(bytes, &["version", "accounts"])
        .map_err(|_| Decline::Shape)?;
    if registry.version != 2 || registry.accounts.len() > 64 {
        return Err(Decline::Shape);
    }
    let mut ids = BTreeSet::new();
    let mut references = BTreeSet::new();
    let mut per_provider = BTreeMap::new();
    for source in &registry.accounts {
        let provider = additional(&source.provider).ok_or(Decline::Shape)?;
        let count = per_provider.entry(source.provider.as_str()).or_insert(0);
        *count += 1;
        if *count > 16
            || !ids.insert(source.id.as_str())
            || source
                .check(provider)?
                .is_some_and(|reference| !references.insert((source.provider.as_str(), reference)))
        {
            return Err(Decline::Shape);
        }
    }
    Ok(registry.accounts)
}

/// Read the active registry under the boot root through the shared file policy. A damaged
/// v2 file declines and never turns into a legacy or an empty registry.
pub fn read_registry(root: &Path) -> Result<Sources, Decline> {
    let (file, policy) =
        crate::registered::file(root, "account-registry").ok_or(Decline::Internal)?;
    let policy = policy
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(&file, &policy) {
        Read::Absent => Ok(Sources::Legacy),
        Read::Bytes(bytes, _) => parse_registry(&bytes).map(Sources::V2),
        _ => Err(Decline::Unsupported),
    }
}

/// Legacy providers use the same identifiers and default source as the TS manifest.
pub(super) fn legacy(root: &Path) -> Vec<Source> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Override {
        provider: String,
        platform: String,
        ssh_host: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Manifest {
        version: u32,
        sources: Vec<Override>,
    }
    let file = root.join("account-usage-sources.json");
    let config: Option<Manifest> = aac_files::read_bounded(&file, 32 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let order = [
        "antigravity",
        "muse",
        "cursor",
        "kimi-code",
        "qwen",
        "zai",
        "opencode-go",
    ];
    let mut overrides = BTreeMap::new();
    if let Some(config) = config.filter(|c| c.version == 1 && c.sources.len() <= order.len()) {
        for row in config.sources {
            if !order.contains(&row.provider.as_str())
                || !["mac", "windows", "ubuntu"].contains(&row.platform.as_str())
                || row.ssh_host.as_deref().is_some_and(|s| !alias(s))
                || overrides.insert(row.provider.clone(), row).is_some()
            {
                overrides.clear();
                break;
            }
        }
    }
    order
        .into_iter()
        .map(|provider| {
            let entry = overrides.remove(provider);
            Source {
                id: format!("{provider}:usage"),
                provider: provider.into(),
                platform: entry
                    .as_ref()
                    .map_or_else(|| "ubuntu".into(), |row| row.platform.clone()),
                ssh_host: entry.and_then(|row| row.ssh_host),
                label: None,
                credential: Credential::Discover,
                created_at: None,
                created_by: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_qwen_capsule_cannot_be_reused_by_a_second_registry_row() {
        let migrated = json!({"id":"qwen:usage","provider":"qwen","platform":"ubuntu","credential":{"kind":"discover"}});
        let capsule = json!({"id":"qwen:acct:1234abcd","provider":"qwen","platform":"mac","credential":{"kind":"browser-capsule","capsuleId":"default"}});
        let bytes =
            |rows| serde_json::to_vec(&json!({"version":2,"accounts":rows})).expect("fixture");
        assert_eq!(
            parse_registry(&bytes(vec![migrated.clone()]))
                .expect("migration")
                .len(),
            1
        );
        assert_eq!(
            parse_registry(&bytes(vec![migrated, capsule])),
            Err(Decline::Shape)
        );
    }
}
