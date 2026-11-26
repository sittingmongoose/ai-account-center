//! Configuration invariants ported from the retired TypeScript config suites
//! (`dashboard-auth-persistence.test.ts`, `unified-config.test.ts`,
//! `config-dir-override.test.js`, `dashboard-network-config.test.ts`,
//! `dashboard-tls-config.test.ts`).
#![allow(clippy::unwrap_used)]

use aac_config::{Config, LoadOptions, RETIRED_SECTIONS, render_active, split_active};
use serde_json::json;
use std::path::Path;

fn options(seed_dir: &Path, ccs_home: &Path) -> LoadOptions {
    let seed = seed_dir.join("computers.json");
    std::fs::write(
        &seed,
        serde_json::to_vec(&json!({
            "ubuntu": { "label": "Ubuntu VM", "os": "linux", "sshAlias": null },
            "nas1": { "label": "Nas1", "os": "linux", "sshAlias": "nas1" },
            "mac": { "label": "Mac", "os": "mac", "sshAlias": "fixture-mac" },
            "windows": { "label": "Windows", "os": "windows", "sshAlias": "fixture-windows" },
            "peer": { "label": "Peer", "os": "windows", "sshAlias": "fixture-peer" }
        }))
        .unwrap(),
    )
    .unwrap();
    LoadOptions::new(ccs_home.to_path_buf(), seed)
}

#[test]
fn disabled_auth_block_persists_across_unrelated_mutation_and_absent_block_is_not_invented() {
    // 1. Existing disabled dashboard_auth with custom settings survives unrelated active update.
    let original = concat!(
        "version: 2\n",
        "dashboard_auth:\n",
        "  enabled: false\n",
        "  username: saved-admin\n",
        "  password_hash: $2b$10$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234\n",
        "  session_timeout_hours: 168\n",
        "  session_lifetime_days: 7\n",
        "dashboard_network:\n",
        "  trust_local_network: false\n",
        "  trusted_networks:\n",
        "    - 10.0.0.0/8\n",
    );
    let (mut active, _) = split_active(original).unwrap();
    active["dashboard_network"]["trust_local_network"] = json!(true);
    let rendered = render_active(original, &active).unwrap();
    let (reloaded, _) = split_active(&rendered).unwrap();
    assert_eq!(reloaded["dashboard_auth"]["enabled"], false);
    assert_eq!(reloaded["dashboard_auth"]["username"], "saved-admin");
    assert_eq!(reloaded["dashboard_auth"]["session_lifetime_days"], 7);
    assert_eq!(reloaded["dashboard_network"]["trust_local_network"], true);
    assert_eq!(
        reloaded["dashboard_network"]["trusted_networks"],
        json!(["10.0.0.0/8"])
    );

    // 2. Saving a configuration without dashboard_auth does not invent an auth block.
    let minimal = "version: 2\ndashboard_network:\n  trust_local_network: false\n";
    let (mut active2, _) = split_active(minimal).unwrap();
    active2["dashboard_network"]["trust_local_network"] = json!(true);
    let rendered2 = render_active(minimal, &active2).unwrap();
    let (reloaded2, _) = split_active(&rendered2).unwrap();
    assert!(
        reloaded2.get("dashboard_auth").is_none(),
        "must not invent dashboard_auth when absent"
    );
}

#[test]
fn dashboard_tls_and_network_blocks_survive_unrelated_mutations_with_retired_sections() {
    let original = concat!(
        "version: 2\n",
        "dashboard_tls:\n",
        "  public_origin: https://aac.example.test\n",
        "  trusted_proxy: lan-https-proxy\n",
        "  trusted_proxy_addresses:\n",
        "    - 192.168.1.9\n",
        "dashboard_network:\n",
        "  trust_local_network: true\n",
        "  trusted_networks:\n",
        "    - 100.64.0.0/10\n",
        "cliproxy:\n",
        "  # retired comment stays verbatim\n",
        "  port: 8317\n",
    );
    let (mut active, retired) = split_active(original).unwrap();
    assert!(retired.contains_key("cliproxy"));
    assert!(active.get("cliproxy").is_none());

    active["dashboard_auth"] = json!({
        "enabled": true,
        "username": "admin",
        "password_changed_at": "2026-11-26T00:00:00.000Z"
    });
    let rendered = render_active(original, &active).unwrap();
    assert!(rendered.contains("# retired comment stays verbatim"));
    let (reloaded, retired_after) = split_active(&rendered).unwrap();
    assert!(retired_after.contains_key("cliproxy"));
    assert_eq!(
        reloaded["dashboard_tls"]["trusted_proxy"],
        "lan-https-proxy"
    );
    assert_eq!(
        reloaded["dashboard_tls"]["trusted_proxy_addresses"],
        json!(["192.168.1.9"])
    );
    assert_eq!(
        reloaded["dashboard_tls"]["public_origin"],
        "https://aac.example.test"
    );
    assert_eq!(reloaded["dashboard_network"]["trust_local_network"], true);
    assert_eq!(
        reloaded["dashboard_network"]["trusted_networks"],
        json!(["100.64.0.0/10"])
    );
    assert_eq!(
        reloaded["dashboard_auth"]["password_changed_at"],
        "2026-11-26T00:00:00.000Z"
    );
}

#[test]
fn config_dir_precedence_ccs_home_ccs_dir_and_cli_override() {
    let root = tempfile::tempdir().unwrap();
    let default_home = root.path().join("default-home");
    let ccs_home = root.path().join("alt-home");
    let ccs_dir = root.path().join("direct-ccs-dir");
    let cli_dir = root.path().join("cli-config-dir");

    // 1. Default appends .ccs to ccs_home.
    let opts = options(root.path(), &default_home);
    assert_eq!(
        Config::load(&opts).unwrap().private_dir(),
        default_home.join(".ccs")
    );

    // 2. CCS_HOME appends .ccs.
    let mut opts_home = options(root.path(), &default_home);
    opts_home
        .environment
        .insert("CCS_HOME".into(), ccs_home.to_string_lossy().into());
    assert_eq!(
        Config::load(&opts_home).unwrap().private_dir(),
        ccs_home.join(".ccs")
    );

    // 3. CCS_DIR is used directly (no .ccs appended) and beats CCS_HOME.
    let mut opts_dir = opts_home.clone();
    opts_dir
        .environment
        .insert("CCS_DIR".into(), ccs_dir.to_string_lossy().into());
    assert_eq!(Config::load(&opts_dir).unwrap().private_dir(), ccs_dir);

    // 4. CLI config_dir has highest precedence over both CCS_DIR and CCS_HOME.
    let mut opts_cli = opts_dir;
    opts_cli.cli = json!({ "config_dir": cli_dir });
    assert_eq!(Config::load(&opts_cli).unwrap().private_dir(), cli_dir);
    assert!(
        !cli_dir.exists(),
        "read-only Config::load must never create the directory"
    );
}

#[test]
fn partial_and_older_version_configs_load_without_retired_sections() {
    let root = tempfile::tempdir().unwrap();
    let opts = options(root.path(), root.path());
    let private = root.path().join(".ccs");
    std::fs::create_dir(&private).unwrap();

    // Older partial YAML with retired cliproxy section loads cleanly and omits retired sections.
    std::fs::write(
        private.join("config.yaml"),
        "version: 1\ndashboard:\n  port: 3777\ncliproxy:\n  enabled: true\n",
    )
    .unwrap();
    let loaded = Config::load(&opts).unwrap();
    assert_eq!(loaded.values()["version"], 1);
    assert_eq!(loaded.values()["dashboard"]["port"], 3777);
    assert_eq!(
        loaded.values()["account_refresh"]["refreshIntervalSeconds"],
        60
    );
    for section in RETIRED_SECTIONS {
        assert!(
            loaded.values().get(*section).is_none(),
            "retired section {section} must not enter active model"
        );
    }
}
