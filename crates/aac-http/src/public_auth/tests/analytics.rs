use super::*;
use std::{fs, path::Path};

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
