//! The native boot publishes its own dashboard without an HTTP feed producer.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]
use aac_http::{
    Boot, NativeAccountUsage,
    account_usage::{AccountUsageCache, Desktop, producer::CompanionInputs},
    serve_with_native_account_usage,
};
use serde_json::{Map, Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

fn private_file(root: &Path, name: &str, value: &Value) {
    let file = root.join(name);
    fs::write(&file, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn native_boot_builds_all_selectors_from_private_settings_without_a_feed() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let boot_id = "0123456789abcdef01234567";
    let run = root.join("run");
    let folder = run.join(format!("aac-http-{boot_id}"));
    fs::create_dir_all(&folder).unwrap();
    for path in [&root, &run, &folder] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    private_file(
        &root,
        "account-usage-accounts.json",
        &json!({"version":2,"accounts":[{
            "id":"cursor:usage","provider":"cursor","platform":"ubuntu","label":"Fixture",
            "credential":{"kind":"discover"},
        }]}),
    );
    private_file(
        &root,
        "account-refresh-settings.json",
        &json!({"refreshIntervalSeconds":72}),
    );
    private_file(
        &root,
        "account-visibility.json",
        &json!({
            "version":1,"hiddenProviders":["cursor"],"hiddenAccountIds":[],
            "trayHiddenProviders":[],"trayHiddenAccountIds":[]
        }),
    );
    let registry: Value = serde_json::from_str(aac_files::registry::SOURCE).unwrap();
    let files: Map<String, Value> = registry["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let role = if row["movesWith"] == "collection" {
                json!("rust-service")
            } else {
                row["owner"].clone()
            };
            (row["id"].as_str().unwrap().to_owned(), role)
        })
        .collect();
    let boot = Boot::from_json(
        &serde_json::to_vec(&json!({
            "ccsDir":root,"socketPath":folder.join("http.sock"),
            "secret":"d".repeat(64),"bootId":boot_id,"files":files
        }))
        .unwrap(),
    )
    .unwrap();

    let cache = AccountUsageCache::default();
    let (observed, mut reports) = mpsc::unbounded_channel();
    let (stop, stopped) = oneshot::channel();
    let producer = NativeAccountUsage {
        companions: Arc::new(|| {
            let mut inputs = CompanionInputs::default();
            inputs.lifecycle.mounted = true;
            inputs
        }),
        published: Arc::new(move |cache| {
            observed
                .send(cache.hub_dashboards("2026-01-01T00:00:00.000Z", 0))
                .unwrap();
        }),
    };
    let server = tokio::spawn(serve_with_native_account_usage(
        boot,
        async {
            let _ = stopped.await;
        },
        cache.clone(),
        producer,
    ));
    let dashboards = tokio::time::timeout(Duration::from_secs(5), reports.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dashboards.len(), 2);
    assert_eq!(dashboards[0], dashboards[1]);
    let dashboard = &dashboards[0];
    assert_eq!(dashboard["settings"]["refreshIntervalSeconds"], 72);
    assert_eq!(
        dashboard["accounts"],
        json!([{
            "id":"cursor:usage","provider":"cursor","providerLabel":"Cursor","label":"Fixture",
            "email":null,"plan":null,"platform":"ubuntu","source":"Account on Ubuntu",
            "status":"unavailable","message":"Saved account usage is unavailable on this computer.",
            "fetchedAt":null,"sampledAt":null,"isActive":false,"windows":[],
            "capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]},
            "lifecycle":{"state":"ready","jobId":null},"hidden":true,"trayHidden":false
        }])
    );
    assert_eq!(dashboard["providers"][3]["accountCount"], 1);
    assert_eq!(dashboard["providers"][3]["visible"], false);
    assert_eq!(
        dashboard["providers"][1]["signIn"]["unavailableReason"],
        "secure_transport_required"
    );
    let secure: Value = serde_json::from_slice(
        &cache
            .render(Desktop::Windows, true, "2026-01-01T00:00:00.000Z", 0)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(secure["providers"][1]["signIn"]["available"], true);
    assert!(cache.generation(Desktop::Windows, true).is_some());
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}
