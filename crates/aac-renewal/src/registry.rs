//! The saved-profile registry read for renewal
//! (`src/codex-auth/codex-profile-registry.ts` validation,
//! `codex-profile-renewal-planner.ts` `listRegisteredCodexProfiles`,
//! `codex-auth/types.ts` name rules). Read without the registry class, whose
//! constructor sweeps temp files.

use std::path::{Path, PathBuf};

/// The registry schema version the TS writer stamps.
pub const CODEX_PROFILE_SCHEMA_VERSION: &str = "1.0";

/// `getCodexProfileNameError`: null when the name is safe to use as a folder.
pub fn codex_profile_name_error(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Profile name is required.");
    }
    if name == "default" || name == "current" {
        return Some("reserved");
    }
    if name.contains('/') || name.contains('\\') {
        return Some("Profile name must not contain path separators.");
    }
    if name.chars().count() > 64 {
        return Some("Profile name must be 64 characters or fewer.");
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or_default();
    let first_ok = first.is_ascii_lowercase() || first.is_ascii_digit();
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !first_ok || !rest_ok {
        return Some("Profile name must match [a-z0-9][a-z0-9_-]{0,63}.");
    }
    None
}

/// The registry file under the CCS root.
pub fn codex_auth_registry_path(ccs_dir: &Path) -> PathBuf {
    ccs_dir.join("codex-profiles.yaml")
}

/// The saved-profile instances folder under the CCS root.
pub fn codex_instances_dir(ccs_dir: &Path) -> PathBuf {
    ccs_dir.join("codex-instances")
}

fn yaml_to_json(value: serde_yaml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    use serde_yaml::Value as Y;
    match value {
        Y::Null => J::Null,
        Y::Bool(b) => J::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                J::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                J::Number(u.into())
            } else {
                serde_json::Number::from_f64(n.as_f64().unwrap_or(f64::NAN))
                    .map(J::Number)
                    .unwrap_or(J::Null)
            }
        }
        Y::String(s) => J::String(s),
        Y::Sequence(seq) => J::Array(seq.into_iter().map(yaml_to_json).collect()),
        Y::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                let key = match key {
                    Y::String(s) => s,
                    Y::Number(n) => n.to_string(),
                    Y::Bool(b) => b.to_string(),
                    Y::Null => "null".to_owned(),
                    _ => continue,
                };
                out.insert(key, yaml_to_json(value));
            }
            J::Object(out)
        }
        Y::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

fn is_object(value: &serde_json::Value) -> bool {
    value.is_object()
}

fn valid_string(value: Option<&serde_json::Value>) -> bool {
    matches!(value, Some(serde_json::Value::String(_)))
}

/// `validateCodexProfileRegistryData`: the exact shape the TS reader accepts.
/// Returns the registered profile names in file order.
pub fn validate_codex_profile_registry_data(parsed: &serde_json::Value) -> Result<Vec<String>, ()> {
    if !is_object(parsed) {
        return Err(());
    }
    let root = parsed.as_object().ok_or(())?;
    let profiles = match root.get("profiles") {
        Some(value) if is_object(value) => value.as_object().ok_or(())?,
        _ => return Err(()),
    };
    match root.get("default") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::String(name)) => {
            if codex_profile_name_error(name).is_some() || !profiles.contains_key(name) {
                return Err(());
            }
        }
        Some(_) => return Err(()),
    }
    let mut names = Vec::new();
    for (name, profile) in profiles {
        if codex_profile_name_error(name).is_some() {
            return Err(());
        }
        if !is_object(profile) {
            return Err(());
        }
        let meta = profile.as_object().ok_or(())?;
        if meta.get("type") != Some(&serde_json::Value::String("codex".into())) {
            return Err(());
        }
        if !valid_string(meta.get("created")) {
            return Err(());
        }
        // last_used must be present: a string or null (undefined fails in TS).
        match meta.get("last_used") {
            Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => {}
            _ => return Err(()),
        }
        if let Some(email) = meta.get("email")
            && !email.is_string()
        {
            return Err(());
        }
        if let Some(plan) = meta.get("plan_type")
            && !plan.is_null()
            && !plan.is_string()
        {
            return Err(());
        }
        if let Some(account) = meta.get("account_id")
            && !account.is_string()
        {
            return Err(());
        }
        names.push(name.clone());
    }
    Ok(names)
}

/// `listRegisteredCodexProfiles`: missing file reads as no profiles; any other
/// read or shape failure is the fixed ConfigError (renewal reports it as a
/// failed cycle, never as an empty registry).
pub fn list_registered_codex_profiles(ccs_dir: &Path) -> Result<Vec<String>, ()> {
    let raw = match std::fs::read_to_string(codex_auth_registry_path(ccs_dir)) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(()),
    };
    let parsed: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|_| ())?;
    validate_codex_profile_registry_data(&yaml_to_json(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_rules_match_the_ts_guard() {
        assert!(codex_profile_name_error("gmail").is_none());
        assert!(codex_profile_name_error("a-b_1").is_none());
        for bad in [
            "", "default", "current", "a/b", "a\\b", "Upper", "-lead", "_lead", "a b",
        ] {
            assert!(codex_profile_name_error(bad).is_some(), "{bad}");
        }
        assert!(codex_profile_name_error(&"a".repeat(64)).is_none());
        assert!(codex_profile_name_error(&"a".repeat(65)).is_some());
    }

    #[test]
    fn registry_shape_is_strict() {
        let good = serde_json::json!({
            "version": "1.0",
            "default": "gmail",
            "profiles": {"gmail": {"type": "codex", "created": "2026-10-01T00:00:00Z", "last_used": null}}
        });
        assert_eq!(
            validate_codex_profile_registry_data(&good).expect("valid"),
            vec!["gmail".to_owned()]
        );
        let mut missing_last_used = good.clone();
        missing_last_used["profiles"]["gmail"]
            .as_object_mut()
            .expect("object")
            .remove("last_used");
        assert!(validate_codex_profile_registry_data(&missing_last_used).is_err());
        let mut bad_default = good.clone();
        bad_default["default"] = serde_json::json!("ghost");
        assert!(validate_codex_profile_registry_data(&bad_default).is_err());
        assert!(validate_codex_profile_registry_data(&serde_json::json!([])).is_err());
    }

    #[test]
    fn yaml_registry_reads_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("codex-profiles.yaml"),
            "version: '1.0'\ndefault: null\nprofiles:\n  zeta:\n    type: codex\n    created: '2026-10-01T00:00:00Z'\n    last_used: null\n  alpha:\n    type: codex\n    created: '2026-10-01T00:00:00Z'\n    last_used: null\n",
        )
        .expect("write");
        assert_eq!(
            list_registered_codex_profiles(dir.path()).expect("read"),
            vec!["zeta".to_owned(), "alpha".to_owned()]
        );
        assert!(
            list_registered_codex_profiles(Path::new("/nonexistent-ccs-root"))
                .expect("missing")
                .is_empty()
        );
    }
}
