//! Read-only configuration precedence and rollback-text fixtures.
#![allow(clippy::unwrap_used)]
use aac_config::*;
use aac_core::Cause;
use serde_json::json;

fn options(root: &std::path::Path) -> LoadOptions {
    let seed = root.join("seed.json");
    std::fs::write(&seed, serde_json::to_vec(&json!({
        "node-a": {"label":"Local fixture", "os":"linux", "sshAlias":null, "legacy":true, "machineId":"fixture-a"},
        "node-b": {"label":"Remote fixture", "os":"mac", "sshAlias":"fixture-host", "legacy":false}
    })).unwrap()).unwrap();
    LoadOptions::new(root.to_path_buf(), seed)
}

#[test]
fn precedence_and_private_root_match_legacy_ccs_home() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path());
    let private = root.path().join(".ccs");
    std::fs::create_dir(&private).unwrap();
    std::fs::write(private.join("config.yaml"), "dashboard:\n  port: 1000\n  host: yaml-host\naccount_refresh:\n  refreshIntervalSeconds: 45\n").unwrap();
    std::fs::write(
        private.join("account-refresh-settings.json"),
        "{\"refreshIntervalSeconds\":120}",
    )
    .unwrap();
    opts.environment
        .insert("CCS_DASHBOARD_USERNAME".into(), "env-user".into());
    opts.environment
        .insert("CCS_HOME".into(), root.path().to_string_lossy().into());
    opts.cli = json!({"dashboard":{"port":3000},"dashboard_auth":{"username":"flag-user"}});
    let config = Config::load(&opts).unwrap();
    assert_eq!(config.private_dir(), private);
    assert_eq!(config.values()["dashboard"]["port"], 3000);
    assert_eq!(config.values()["dashboard_auth"]["username"], "flag-user");
    assert_eq!(config.values()["dashboard"]["host"], "yaml-host");
    assert_eq!(
        config.values()["account_refresh"]["refreshIntervalSeconds"],
        120
    );
    opts.cli = json!({});
    assert_eq!(
        Config::load(&opts).unwrap().values()["dashboard_auth"]["username"],
        "env-user"
    );
    opts.environment.remove("CCS_DASHBOARD_USERNAME");
    assert_eq!(
        Config::load(&opts).unwrap().values()["dashboard"]["port"],
        1000
    );
    std::fs::remove_file(private.join("account-refresh-settings.json")).unwrap();
    assert_eq!(
        Config::load(&opts).unwrap().values()["account_refresh"]["refreshIntervalSeconds"],
        45
    );
}

#[test]
fn defaults_and_seed_are_read_only_and_keep_legacy_metadata() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path());
    let config = Config::load(&opts).unwrap();
    assert!(!root.path().join(".ccs").exists());
    assert_eq!(
        config.values()["account_refresh"]["refreshIntervalSeconds"],
        60
    );
    let node = config
        .computers()
        .iter()
        .find(|c| c.id.as_str() == "node-a")
        .unwrap();
    assert_eq!(node.metadata["legacy"], true);
    assert_eq!(node.metadata["machineId"], "fixture-a");
    assert_eq!(node.transport, TransportConfig::Local);
    let remote = config
        .computers()
        .iter()
        .find(|c| c.id.as_str() == "node-b")
        .unwrap();
    assert!(
        matches!(&remote.transport, TransportConfig::Ssh { host, key, .. } if host == "fixture-host" && key.as_os_str().is_empty())
    );
}

#[test]
fn retired_sections_roundtrip_verbatim_without_interpretation() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path());
    let private = root.path().join(".ccs");
    std::fs::create_dir(&private).unwrap();
    let mut original = String::from("---\nversion: 14\n");
    for section in RETIRED_SECTIONS {
        original.push_str(&format!("'{section}': &{section}\n  # Preserve comments and recursive legacy YAML.\n  self: *{section}\n  literal: |\n    ---\n    ...\n"));
    }
    original.push_str("dashboard:\n  port: 3000\n...\n");
    std::fs::write(private.join("config.yaml"), &original).unwrap();
    let config = Config::load(&opts).unwrap();
    assert_eq!(config.original_yaml().as_bytes(), original.as_bytes());
    assert_eq!(
        std::fs::read(private.join("config.yaml")).unwrap(),
        original.as_bytes()
    );
    for section in RETIRED_SECTIONS {
        assert!(config.values().get(*section).is_none());
        assert!(config.retired_sections()[*section].starts_with(&format!("'{section}':")));
    }
    // The read-only round trip is the original source, not a YAML serialization that loses comments.
    std::fs::write(root.path().join("roundtrip.yaml"), config.original_yaml()).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("roundtrip.yaml")).unwrap(),
        original.as_bytes()
    );
}

#[test]
fn ccs_dir_and_cli_directory_take_precedence_without_home_mutation() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path());
    let direct = root.path().join("direct");
    opts.environment
        .insert("CCS_DIR".into(), direct.to_string_lossy().into());
    assert_eq!(Config::load(&opts).unwrap().private_dir(), direct);
    let cli = root.path().join("flag");
    opts.cli = json!({"config_dir":cli});
    assert_eq!(Config::load(&opts).unwrap().private_dir(), cli);
    assert!(!cli.exists());
}

#[test]
fn configured_computers_override_seed_and_duplicates_fail() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path());
    let computer = json!({"id":"custom", "label":"Custom fixture", "os":"windows", "transport":{"kind":"local"}, "roles":["pins"]});
    opts.cli = json!({"computers":[computer.clone()]});
    assert_eq!(
        Config::load(&opts).unwrap().computers()[0].id.as_str(),
        "custom"
    );
    opts.cli = json!({"computers":[computer.clone(),computer]});
    assert!(Config::load(&opts).is_err());
}

#[test]
fn corrupt_oversized_and_symlink_settings_are_honest_failures() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path());
    let private = root.path().join(".ccs");
    std::fs::create_dir(&private).unwrap();
    let file = private.join("account-refresh-settings.json");
    std::fs::write(&file, "{broken").unwrap();
    assert!(matches!(Config::load(&opts), Err(error) if error.cause == Cause::Damaged));
    std::fs::write(&file, " ".repeat(4097)).unwrap();
    assert!(matches!(Config::load(&opts), Err(error) if error.cause == Cause::OutputTooLarge));
    #[cfg(unix)]
    {
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&opts.computers_seed, &file).unwrap();
        assert!(Config::load(&opts).is_err());
    }
}

#[test]
fn existing_settings_filenames_and_auth_environment_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let mut opts = options(root.path());
    let private = root.path().join(".ccs");
    std::fs::create_dir(&private).unwrap();
    for (file, _, _) in SETTINGS_FILES {
        std::fs::write(
            private.join(file),
            serde_json::to_vec(&json!({"origin":file})).unwrap(),
        )
        .unwrap();
    }
    opts.environment
        .insert("CCS_DASHBOARD_AUTH_ENABLED".into(), "1".into());
    let config = Config::load(&opts).unwrap();
    assert_eq!(config.values()["dashboard_auth"]["enabled"], true);
    assert_eq!(
        config.values()["agent_usage_snapshot"]["origin"],
        "agent-usage-snapshot.json"
    );
    for (file, section, _) in SETTINGS_FILES {
        assert_eq!(config.values()[*section]["origin"], *file);
    }
    opts.environment
        .insert("CCS_DASHBOARD_AUTH_ENABLED".into(), "false".into());
    assert_eq!(
        Config::load(&opts).unwrap().values()["dashboard_auth"]["enabled"],
        false
    );
}

#[test]
fn regression_m2_non_object_computers_are_damaged_without_panicking() {
    for value in [
        json!(42),
        json!(true),
        json!("scalar"),
        json!([]),
        json!(null),
    ] {
        for layer in ["cli", "seed", "yaml"] {
            let root = tempfile::tempdir().unwrap();
            let mut opts = options(root.path());
            let computers = json!({"node-a":value});
            match layer {
                "cli" => opts.cli = json!({"computers":computers}),
                "seed" => std::fs::write(
                    &opts.computers_seed,
                    serde_json::to_vec(&computers).unwrap(),
                )
                .unwrap(),
                _ => {
                    std::fs::create_dir(root.path().join(".ccs")).unwrap();
                    std::fs::write(
                        root.path().join(".ccs/config.yaml"),
                        serde_yaml::to_string(&json!({"computers":computers})).unwrap(),
                    )
                    .unwrap();
                }
            }
            let result = std::panic::catch_unwind(|| Config::load(&opts));
            assert!(
                matches!(result, Ok(Err(error)) if error.cause == Cause::Damaged),
                "malformed {layer} computer entry must return Damaged"
            );
        }
    }
}

#[test]
fn regression_m3_indented_and_flow_retired_sections_stay_opaque() {
    for style in ["indented-block", "indented-flow-values", "flow-root"] {
        let root = tempfile::tempdir().unwrap();
        let opts = options(root.path());
        let mut original = String::new();
        if style == "flow-root" {
            original.push_str("{ dashboard: {port: 3000}, unicode: '\u{03bb}', ");
            for section in RETIRED_SECTIONS {
                original.push_str(&format!(
                    "'{section}': &{section} {{self: *{section}, unicode: '\u{03bb}'}}, "
                ));
            }
            original.push_str("active_literal: 'retired: not a root key' }\n");
        } else {
            original.push_str("  # Root indentation is valid YAML.\n  dashboard:\n    port: 3000\n  unicode: '\u{03bb}'\n");
            for section in RETIRED_SECTIONS {
                if style == "indented-block" {
                    original.push_str(&format!("  '{section}': &{section}\n    self: *{section}\n    text: |\n      ---\n      ...\n"));
                } else {
                    original.push_str(&format!(
                        "  '{section}': &{section} {{self: *{section}, text: 'literal # , :'}}\n"
                    ));
                }
            }
            original.push_str("  active_literal: 'retired: not a root key'\n");
        }
        std::fs::create_dir(root.path().join(".ccs")).unwrap();
        let path = root.path().join(".ccs/config.yaml");
        std::fs::write(&path, &original).unwrap();
        let result = Config::load(&opts);
        assert!(
            result.is_ok(),
            "{style} must load without expanding retired aliases"
        );
        let config = result.unwrap();
        assert_eq!(config.values()["dashboard"]["port"], 3000);
        assert_eq!(config.values()["unicode"], "\u{03bb}");
        assert_eq!(config.values()["active_literal"], "retired: not a root key");
        assert_eq!(config.original_yaml(), original);
        assert_eq!(std::fs::read(path).unwrap(), original.as_bytes());
        for section in RETIRED_SECTIONS {
            assert!(config.values().get(*section).is_none());
            assert!(config.retired_sections()[*section].contains(&format!("'{section}':")));
        }
    }
}
