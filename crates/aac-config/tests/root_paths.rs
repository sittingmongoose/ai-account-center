//! Real settings reads through the same empty/relative override table as the TS resolver.
#![allow(clippy::unwrap_used)]
use aac_config::{Config, LoadOptions};
use serde_json::{Value, json};

#[test]
fn max1_config_paths_match_ts_in_an_isolated_process() {
    if let Some(root) = std::env::var_os("AAC_MAX1_PATH_CHILD") {
        let root = std::path::PathBuf::from(root);
        let seed = root.join("seed.json");
        std::fs::write(&seed, b"{}").unwrap();
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/root-paths.json")).unwrap();
        for case in cases {
            let expected = root.join(case["expected"].as_str().unwrap());
            std::fs::create_dir_all(&expected).unwrap();
            std::fs::write(
                expected.join("account-refresh-settings.json"),
                b"{\"refreshIntervalSeconds\":120}",
            )
            .unwrap();
            let mut options = LoadOptions::new(root.join("default-home"), seed.clone());
            for (key, value) in case["environment"].as_object().unwrap() {
                options.environment.insert(
                    key.clone(),
                    value
                        .as_str()
                        .unwrap()
                        .replace("$ROOT", root.to_str().unwrap()),
                );
            }
            if let Some(cli) = case["cli"].as_str() {
                options.cli = json!({"config_dir":cli.replace("$ROOT",root.to_str().unwrap())});
            }
            assert_eq!(
                aac_files::login_rollback::selected_ccs_root(
                    &options.ccs_home,
                    &options.environment,
                    options.cli["config_dir"].as_str(),
                )
                .unwrap(),
                expected,
                "agent selector differs: {}",
                case["name"]
            );
            let result = Config::load(&options);
            assert!(
                result.is_ok(),
                "TS-compatible root refused: {}",
                case["name"]
            );
            let config = result.unwrap();
            assert_eq!(config.private_dir(), expected, "{}", case["name"]);
            assert_eq!(
                config.values()["account_refresh"]["refreshIntervalSeconds"],
                120
            );
            assert_eq!(
                std::fs::read(expected.join("account-refresh-settings.json")).unwrap(),
                b"{\"refreshIntervalSeconds\":120}"
            );
        }
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("cwd")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "max1_config_paths_match_ts_in_an_isolated_process",
        ])
        .current_dir(root.path().join("cwd"))
        .env("AAC_MAX1_PATH_CHILD", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
