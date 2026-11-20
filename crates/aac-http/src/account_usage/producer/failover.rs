//! The producer's failover effects and targets: the same agent transport, dashboard hosts and
//! clock the TS coordinator used, so one `FailoverCoordinator` serves every provider.
use super::claude::manifest_profile;
use super::{Producer, source_host};
use crate::account_usage::coordinator::{
    FailoverCoordinator, FailoverDeps, PrimarySource, UsageFailoverTarget, UsageHost,
};
use crate::agent_transport::Agents;
use crate::collection::Collection;
use aac_core::UtcMs;
use aac_files::registry::Owners;
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

/// The coordinator's transport, host list and clock, all borrowed from the producer's boot.
struct Effects {
    agents: Arc<Agents>,
    collection: Arc<Collection>,
}

impl FailoverDeps for Effects {
    fn transport<'a>(
        &'a self,
        host: &'a UsageHost,
        request: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ()>> + Send + 'a>> {
        Box::pin(async move {
            // The TS coordinator names the helper verb by the request's action.
            let verb = match request.get("action").and_then(Value::as_str) {
                Some("inventory") => "failover-inventory",
                Some("collect") => "failover-collect",
                _ => return Err(()),
            };
            self.collection
                .read_value(
                    self.agents.clone(),
                    &host.platform,
                    host.ssh_host.as_deref(),
                    verb,
                    request,
                )
                .await
                .map_err(|_| ())
        })
    }

    fn hosts<'a>(&'a self) -> Pin<Box<dyn Future<Output = Vec<UsageHost>> + Send + 'a>> {
        Box::pin(async move {
            self.agents
                .usage_hosts()
                .await
                .into_iter()
                .filter(|host| host.agent_available)
                .map(|host| UsageHost {
                    host: host.id,
                    platform: host.platform.to_owned(),
                    ssh_host: host.alias,
                })
                .collect()
        })
    }

    fn now(&self) -> f64 {
        UtcMs::now().map_or(0.0, |now| now.get() as f64)
    }
}

impl Producer {
    /// The producer's one coordinator over its own transport, hosts and file owners.
    pub(crate) fn failover(&self) -> Arc<FailoverCoordinator> {
        self.failover.clone()
    }
}

pub(super) fn coordinator(
    agents: Arc<Agents>,
    collection: Arc<Collection>,
    owners: Owners,
) -> Arc<FailoverCoordinator> {
    Arc::new(FailoverCoordinator::new(
        Arc::new(Effects { agents, collection }),
        owners,
    ))
}

/// The TS `claudeTarget`: the Windows launcher is the quota source, and the profile's own
/// manifest entry is the binding that a Re-check revalidates.
pub(super) fn claude_target(root: &Path, profile: &Value) -> Option<UsageFailoverTarget> {
    let email = profile["email"].as_str()?.to_owned();
    let id = profile["id"].as_str();
    let private = manifest_profile(profile);
    let binding = String::from_utf8(aac_views::js_json::write(&private)).ok()?;
    let mut primary_sources = Vec::new();
    if let Some(launcher) = profile.get("windows")
        && (launcher["sshHost"].is_string() || cfg!(windows))
    {
        let alias = launcher["sshHost"].as_str().map(str::to_owned);
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
        let mut scoped = json!({"id": id.unwrap_or("default"), "email": email});
        if let Some(path) = path {
            scoped["profilePath"] = json!(path);
        }
        scoped["isDefault"] = json!(default);
        primary_sources.push(PrimarySource {
            host: UsageHost {
                host: "windows".into(),
                platform: "windows".into(),
                ssh_host: alias,
            },
            selector,
            claude_profile: Some(scoped),
        });
    }
    let claude_desktop_cadence = !primary_sources.is_empty();
    Some(UsageFailoverTarget {
        account_id: format!("claude:{}", id.unwrap_or(&email)),
        provider: "claude".into(),
        ccs_dir: root.to_path_buf(),
        binding,
        primary_sources,
        claude_desktop_cadence,
        expected_email: Some(email),
        expected_identity: None,
    })
}

/// The TS `failoverTarget` of an additional registry source.
pub(super) fn additional_target(
    root: &Path,
    source: &crate::account_usage::sources::Source,
    legacy: bool,
) -> UsageFailoverTarget {
    let (host, _) = source_host(&source.platform, source.ssh_host.as_deref());
    let selector = match &source.credential {
        crate::account_usage::sources::Credential::Discover => "discover".to_owned(),
        crate::account_usage::sources::Credential::AacKey { key_id, .. } => {
            format!("aac-key:{key_id}")
        }
        crate::account_usage::sources::Credential::BrowserCapsule { capsule_id, .. } => {
            format!("browser-capsule:{capsule_id}")
        }
        crate::account_usage::sources::Credential::ConfigHome { home_id, .. } => {
            format!("config-home:{home_id}")
        }
        crate::account_usage::sources::Credential::AntigravityProfile { profile_id, .. } => {
            format!("antigravity-profile:{profile_id}")
        }
    };
    let credential = if legacy {
        Value::Null
    } else {
        json!(source.credential)
    };
    let binding = String::from_utf8(aac_views::js_json::write(&json!([
        source.id,
        source.provider,
        source.platform,
        source.ssh_host,
        credential
    ])))
    .unwrap_or_default();
    UsageFailoverTarget {
        account_id: source.id.clone(),
        provider: source.provider.clone(),
        ccs_dir: root.to_path_buf(),
        binding,
        primary_sources: vec![PrimarySource {
            host: UsageHost {
                host,
                platform: source.platform.clone(),
                ssh_host: source.ssh_host.clone(),
            },
            selector,
            claude_profile: None,
        }],
        claude_desktop_cadence: false,
        expected_email: None,
        expected_identity: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn claude_target_matches_the_ts_binding_and_selector() {
        let profile = json!({
            "email":"Owner@Example.com","id":"livia","canOpen":true,
            "mac":{"launcherName":"Claude","profilePath":"/Users/x/Applications/Claude.app",
                   "sshHost":"jared-mac","canOpen":true},
            "windows":{"launcherName":"Claude","profilePath":"C:\\Users\\x\\AppData\\Local\\Claude",
                       "isDefault":true,"sshHost":"jared-windows"}
        });
        let target = claude_target(std::path::Path::new("/fixture"), &profile).unwrap();
        assert_eq!(target.account_id, "claude:livia");
        assert_eq!(target.provider, "claude");
        assert!(target.claude_desktop_cadence);
        assert_eq!(target.expected_email.as_deref(), Some("Owner@Example.com"));
        assert_eq!(
            target.binding,
            r#"{"email":"Owner@Example.com","id":"livia","mac":{"launcherName":"Claude","profilePath":"/Users/x/Applications/Claude.app","sshHost":"jared-mac"},"windows":{"launcherName":"Claude","profilePath":"C:\\Users\\x\\AppData\\Local\\Claude","isDefault":true,"sshHost":"jared-windows"}}"#
        );
        let source = &target.primary_sources[0];
        assert_eq!(source.host.host, "windows");
        assert_eq!(source.host.platform, "windows");
        assert_eq!(source.host.ssh_host.as_deref(), Some("jared-windows"));
        assert_eq!(source.selector, "claude-desktop-profile:default");
        assert_eq!(
            source.claude_profile.as_ref().unwrap()["profilePath"],
            "C:\\Users\\x\\AppData\\Local\\Claude"
        );
        assert_eq!(source.claude_profile.as_ref().unwrap()["isDefault"], true);
    }

    #[test]
    fn a_mac_only_profile_has_no_quota_source() {
        let profile = json!({
            "email":"mac@example.com",
            "mac":{"launcherName":"Claude","profilePath":"/Users/x/Applications/Claude.app","canOpen":true}
        });
        let target = claude_target(std::path::Path::new("/fixture"), &profile).unwrap();
        assert!(target.primary_sources.is_empty());
        assert!(!target.claude_desktop_cadence);
        assert_eq!(target.account_id, "claude:mac@example.com");
    }

    #[test]
    fn additional_target_uses_the_registry_row_as_its_binding() {
        let source = crate::account_usage::sources::Source {
            id: "cursor:acct:00000001".into(),
            provider: "cursor".into(),
            platform: "ubuntu".into(),
            ssh_host: None,
            label: Some("Cursor".into()),
            credential: crate::account_usage::sources::Credential::Discover,
            created_at: None,
            created_by: None,
        };
        let target = additional_target(std::path::Path::new("/fixture"), &source, false);
        assert_eq!(target.account_id, "cursor:acct:00000001");
        assert_eq!(
            target.binding,
            r#"["cursor:acct:00000001","cursor","ubuntu",null,{"kind":"discover"}]"#
        );
        let primary = &target.primary_sources[0];
        assert_eq!(primary.host.host, "ubuntu");
        assert_eq!(primary.host.platform, "ubuntu");
        assert!(primary.host.ssh_host.is_none());
        assert_eq!(primary.selector, "discover");
        assert!(!target.claude_desktop_cadence);
        let legacy = additional_target(std::path::Path::new("/fixture"), &source, true);
        assert_eq!(
            legacy.binding,
            r#"["cursor:acct:00000001","cursor","ubuntu",null,null]"#
        );
    }
}
