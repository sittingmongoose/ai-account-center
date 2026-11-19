//! The native boot publishes its own dashboard without an HTTP feed producer.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]
use aac_http::{
    Boot, NativeAccountUsage,
    account_usage::{AccountUsageCache, Desktop, producer::CompanionInputs},
    serve_with_native_account_usage,
};
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
    sync::Arc,
    time::Duration,
};
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
    let measured_source = json!({
        "id":"zai:usage","provider":"zai","platform":"ubuntu","label":"Measured fixture",
        "credential":{"kind":"discover"},
    });
    private_file(
        &root,
        "account-usage-accounts.json",
        &json!({"version":2,"accounts":[
            {"id":"cursor:usage","provider":"cursor","platform":"ubuntu","label":"Fixture",
             "credential":{"kind":"discover"}},
            measured_source
        ]}),
    );
    // Seed an existing v1 observation with the same source key the TS coordinator
    // writes. Its current sample is eligible without launching any provider reader.
    let binding = json!([
        "zai:usage",
        "zai",
        "ubuntu",
        null,
        measured_source["credential"]
    ])
    .to_string();
    let key = aac_core::sha256_hex(aac_views::js_json::write(&json!([
        root,"zai","zai:usage",binding,
        [{"host":"ubuntu","platform":"ubuntu","sshHost":null,"selector":"discover"}],
        null,null
    ])));
    let identity = json!({"kind":"zai-credential","key":"b".repeat(64)});
    let candidate = json!({
        "locator":"c".repeat(64),"revision":"d".repeat(64),"selectors":["discover"],
        "identity":identity,"email":null,"plan":"Fixture"
    });
    let observation = root.join("account-usage-failover");
    fs::create_dir(&observation).unwrap();
    fs::set_permissions(&observation, fs::Permissions::from_mode(0o700)).unwrap();
    let now = aac_core::UtcMs::now().unwrap().get() as i64;
    let sampled_at = aac_num::ecma::to_iso_string(now - 2000).unwrap();
    let reading = json!({
        "status":"ok","email":null,"plan":"Fixture","fetchedAt":sampled_at,"sampledAt":sampled_at,
        "windows":[{"key":"hourly","label":"Hourly","usedPercent":25,"remainingPercent":75}]
    });
    private_file(
        &observation,
        &format!("{key}.json"),
        &json!({"version":1,"key":key,"identity":identity,"primary":candidate,
            "saved":{"candidate":candidate,"host":"nas1","fallback":true,"reading":reading},
            "nextAttempt":now+60_000,"fresh":true,"freshUntil":now+60_000,
            "primaryFailure":null,"primarySignIn":[],"observedPlan":null,"observedPlanAt":null,
            "observedPlanPeer":null
        }),
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
        dashboard["accounts"][0],
        json!({
            "id":"cursor:usage","provider":"cursor","providerLabel":"Cursor","label":"Fixture",
            "email":null,"plan":null,"platform":"ubuntu","source":"Account on Ubuntu",
            "status":"unavailable","message":"Saved account usage is unavailable on this computer.",
            "fetchedAt":null,"sampledAt":null,"isActive":false,"windows":[],
            "capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]},
            "lifecycle":{"state":"ready","jobId":null},"hidden":true,"trayHidden":false
        })
    );
    let measured = &dashboard["accounts"][1];
    assert_eq!(measured["status"], "ok");
    assert_eq!(measured["source"], "Verified quota on Nas1 (fallback)");
    assert_eq!(measured["host"], "nas1");
    assert_eq!(measured["windows"][0]["usedPercent"], 25);
    let observations = cache
        .store_observations(&sampled_at, now, i64::MIN)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].sampled_at_ms, now - 2000);
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
    let feed = json!({
        "dashboard":cache.generation(Desktop::Windows, true).unwrap(),
        "platform":"windows","secure":true,"now":0,"updatedAt":"2026-01-01T00:00:00.000Z"
    });
    let socket = folder.join("http.sock");
    let (accounts, put) = tokio::task::spawn_blocking(move || {
        let exchange = |method, path, principal: Value, extra: String| {
            let mut pipe = UnixStream::connect(&socket).unwrap();
            pipe.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            write!(pipe, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nx-aac-boot-secret: {}\r\nx-aac-principal: {principal}\r\n{extra}\r\n", "d".repeat(64)).unwrap();
            let mut response = String::new();
            pipe.read_to_string(&mut response).unwrap();
            response
        };
        (
            exchange(
                "GET", "/api/accounts/dashboard",
                json!({"kind":"session","id":"fixture-admin","role":"admin"}),
                format!("x-aac-feed: {feed}\r\n"),
            ),
            exchange(
                "PUT", "/internal/feed/home-dashboard-windows-1",
                json!({"kind":"session","id":"front-door","role":"service"}),
                "Content-Length: 0\r\n".into(),
            )
        )
    }).await.unwrap();
    let (headers, body) = accounts.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
    assert_eq!(serde_json::from_str::<Value>(body).unwrap(), secure);
    assert!(put.starts_with("HTTP/1.1 503"), "{put}");
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}
