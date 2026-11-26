#![cfg(test)]

use super::*;
use std::{fs, path::Path};

const ANALYTICS: &str = "/api/accounts/analytics";
const CAPTURE: &str = "/api/accounts/analytics/capture";
const CACHE: &str = "cache/analytics-remote-v1/mac.json";

fn body(reply: &str, status: u16) -> Value {
    let (headers, data) = reply.split_once("\r\n\r\n").unwrap();
    let status_line = headers.lines().next().unwrap();
    assert!(
        status_line.starts_with(&format!("HTTP/1.1 {status} ")),
        "{status_line}: {data}"
    );
    serde_json::from_str(data).unwrap()
}

async fn headers(addr: SocketAddr) -> String {
    let cookie = login(addr).await;
    format!("Cookie: {cookie}\r\nOrigin: http://{addr}\r\nContent-Type: application/json\r\n")
}

fn cache(root: &Path) -> PathBuf {
    let file = root.join(CACHE);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, br#"{"sessions":{"s1":{"cwd":"/fixture"}}}"#).unwrap();
    file
}

#[tokio::test]
async fn analytics_refuses_without_a_store_owner() {
    let (home, _auth, addr, handle) = server_with_analytics(true, None).await;
    let headers = headers(addr).await;
    let reply = request(addr, ANALYTICS, "GET", &headers, "").await;
    let value = body(&reply, 503);
    assert_eq!(value["code"], "analytics_unavailable");
    assert_eq!(
        value["error"],
        "Account analytics could not be read safely. Try again."
    );
    assert!(reply.contains("\r\nretry-after: 1\r\n"));
    assert!(!home.path().join("quota-history-v1.json").exists());
    handle.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn analytics_capture_reports_failed_replacement_and_retries_while_off() {
    use std::os::unix::fs::PermissionsExt;

    let (home, _auth, addr, handle) = server_with_analytics(true, None).await;
    let file = cache(home.path());
    let directory = file.parent().unwrap();
    let permissions = fs::metadata(directory).unwrap().permissions();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o500)).unwrap();
    let headers = headers(addr).await;
    let put = || request(addr, CAPTURE, "PUT", &headers, r#"{"cwd":false}"#);
    let refused = put().await;
    fs::set_permissions(directory, permissions).unwrap();

    assert_eq!(body(&refused, 500)["code"], "save_failed");
    assert_eq!(
        body(&request(addr, CAPTURE, "GET", &headers, "").await, 200),
        json!({"cwd":false})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&file).unwrap()).unwrap()["sessions"]["s1"]["cwd"],
        "/fixture"
    );
    assert_eq!(
        body(&put().await, 200),
        json!({"cwd":false,"cleared":{"folders":1,"caches":0}})
    );
    assert!(serde_json::from_slice::<Value>(&fs::read(&file).unwrap()).unwrap()["sessions"]["s1"]["cwd"].is_null());
    handle.abort();
}

#[tokio::test]
async fn analytics_capture_opposite_writes_wait_for_the_file_lease_and_keep_their_receipts() {
    let (home, _auth, addr, handle) = server_with_analytics(true, None).await;
    let root = home.path().canonicalize().unwrap();
    let file = cache(&root);
    let rows = aac_files::registry::rows().unwrap();
    let owners = aac_files::registry::Owners::from_boot(
        rows,
        &rows
            .iter()
            .map(|row| {
                let owner = if row.id == "analytics-capture" {
                    "rust-service"
                } else {
                    row.owner.word()
                };
                (row.id.clone(), owner.to_owned())
            })
            .collect(),
    )
    .unwrap();
    let held = owners
        .claim(rows, "analytics-capture", &root, None)
        .unwrap();
    let (off_headers, on_headers) = tokio::join!(headers(addr), headers(addr));
    let off = request(addr, CAPTURE, "PUT", &off_headers, r#"{"cwd":false}"#);
    let on = request(addr, CAPTURE, "PUT", &on_headers, r#"{"cwd":true}"#);
    tokio::pin!(off, on);
    tokio::select! {
        _ = &mut off => panic!("off write replied while the lease was held"),
        _ = &mut on => panic!("on write replied while the lease was held"),
        () = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    drop(held);
    let (off, on) = tokio::join!(off, on);
    assert_eq!(
        body(&off, 200),
        json!({"cwd":false,"cleared":{"folders":1,"caches":0}})
    );
    assert_eq!(body(&on, 200), json!({"cwd":true}));
    assert!(
        serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap()["sessions"]["s1"]["cwd"]
            .is_null()
    );
    handle.abort();
}

fn real_analytics_owner(root: &Path, populate: bool) -> (analytics_routes::Hop, i64) {
    let store = root.join("store");
    let engine =
        Arc::new(aac_store::ops::open_engine(store.to_str().unwrap(), None, None).unwrap());
    let hour = 3_600_000;
    let now = chrono::Utc::now().timestamp_millis().div_euclid(hour) * hour;
    let day = 24 * hour;
    let dashboard = json!({"dashboards":[{"platform":"mac","dashboard":{"accounts":[{
        "id":"codex:fixture", "provider":"codex", "label":"Fixture", "status":"ok",
        "windows":[{"key":"five-hour","label":"5 hours","kind":"rate_limit","usedPercent":22,"windowMinutes":300}]
    }]}}]});
    if populate {
        let provider = aac_core::ProviderId::new("codex").unwrap();
        let identity = aac_core::AccountKey::derive(&provider, "codex:fixture", None).unwrap();
        let observation = |sample: i64, used: u64| {
            json!({
                "account":{"identity":identity.as_str(),"provider":"codex","account_id":"codex:fixture",
                    "platform":"mac","source":"fixture","status":"ok","is_active":false},
                "sampled_at_ms":sample,"observed_at_ms":sample,
                "windows":[{"identity":"[\"five-hour\",\"rate_limit\",null]",
                    "key":"five-hour","label":"5 hours","kind":"rate_limit",
                    "used_pct":used,"remaining_pct":100 - used,"window_minutes":300}]
            })
        };
        let recorded = aac_store::ops::call(
            &engine,
            "record",
            &json!({"observations":[observation(now - 100 * day, 20), observation(now - 3 * day, 22)]}).to_string(),
        ).unwrap();
        assert_eq!(recorded["sampled"], 2);
        let ingested = aac_store::ops::call(&engine, "activityIngest", &json!({"reports":[{
            "schema":"aac.usage-report.v1", "computer":"mac", "tool":"codex",
            "readAt":aac_store::usage::iso(now), "state":"ok", "complete":true,
            "coverFrom":aac_store::usage::iso(now - 30 * day),
            "cells":[{"hour":aac_store::usage::iso(now - 3 * day),"sessionKey":"0123456789abcdef",
                "route":"","modelRaw":"gpt-fixture","inputTokens":100,"outputTokens":0,
                "cacheCreationTokens":0,"cacheReadTokens":0,"costUsd":0,"fallbackCostUsd":0,"events":1}]
        }]}).to_string()).unwrap();
        assert_eq!(ingested["put"], 1);
    }
    let root = root.to_owned();
    let hop: analytics_routes::Hop = Arc::new(move |uri, _principal, _headers| {
        let engine = Arc::clone(&engine);
        let result = crate::analytics_route::compose(
            engine,
            &root,
            Some(&dashboard),
            &crate::analytics_live::Live::default(),
            uri.split_once('?').map(|(_, query)| query),
            now,
        );
        let answer =
            crate::analytics_route::encode(result, uri.split_once('?').map(|(_, query)| query));
        Box::pin(async move {
            answer.ok().map(|(status, body)| owner_host::ServiceReply {
                status,
                headers: axum::http::HeaderMap::new(),
                body: axum::body::Bytes::from(body),
            })
        })
    });
    (hop, now)
}

#[tokio::test]
async fn analytics_named_account_reads_hourly_and_daily_ranges_from_the_real_store() {
    let owner_home = tempfile::tempdir().unwrap();
    let (hop, now) = real_analytics_owner(owner_home.path(), true);
    let (_home, _auth, addr, handle) = server_with_analytics(true, Some(hop)).await;
    let headers = headers(addr).await;
    let from = aac_store::usage::iso(now - 100 * 86_400_000);
    let to = aac_store::usage::iso(now);
    let custom = format!("custom&from={}&to={}", &from[..10], &to[..10]);
    for (preset, range) in [("30d", "30d"), ("custom", custom.as_str())] {
        let path = format!(
            "{ANALYTICS}?range={range}&provider=codex&account=codex%3Afixture&refresh=false&tz=UTC"
        );
        let raw = request(addr, &path, "GET", &headers, "").await;
        assert!(raw.starts_with("HTTP/1.1 200"), "{preset} range failed");
        let reply = body(&raw, 200);
        assert_eq!(reply["filters"]["account"], "codex:fixture");
        assert_eq!(reply["accounts"][0]["id"], "codex:fixture");
        assert_eq!(reply["history"]["sampleCount"], 1);
        assert_eq!(reply["activity"]["status"], "unavailable");
        assert_eq!(reply["range"]["preset"], preset);
    }
    handle.abort();
}

#[tokio::test]
async fn analytics_admits_canonical_linked_and_case_variant_zones_and_refuses_unknown_zones() {
    let owner_home = tempfile::tempdir().unwrap();
    let (hop, _) = real_analytics_owner(owner_home.path(), false);
    let (_home, _auth, addr, handle) = server_with_analytics(true, Some(hop)).await;
    let headers = headers(addr).await;
    let cases = [
        ("America/New_York", "America/New_York"),
        ("GMT", "UTC"),
        ("US/Eastern", "America/New_York"),
        ("america/new_york", "America/New_York"),
    ];
    for (input, expected) in cases {
        let path = format!("{ANALYTICS}?range=7d&tz={input}");
        let reply = request(addr, &path, "GET", &headers, "").await;
        assert_eq!(body(&reply, 200)["range"]["tz"], expected, "{input}");
    }
    let invalid = request(
        addr,
        &format!("{ANALYTICS}?tz=Bogus/Zone"),
        "GET",
        &headers,
        "",
    )
    .await;
    assert_eq!(body(&invalid, 400)["code"], "invalid_tz");
    let path = format!("{ANALYTICS}?range=custom&from=2000-01-01&to=2000-01-02");
    assert_eq!(
        body(&request(addr, &path, "GET", &headers, "").await, 400)["code"],
        "invalid_range"
    );
    let head = request(addr, ANALYTICS, "HEAD", &headers, "").await;
    assert!(head.starts_with("HTTP/1.1 200"));
    assert!(head.contains("cache-control: no-store"));
    assert!(head.contains("etag: W/"));
    assert_eq!(head.split_once("\r\n\r\n").unwrap().1, "");
    handle.abort();
}
