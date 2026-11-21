//! G4 byte parity: replays the TypeScript-generated corpus (g4-fixtures.ts) through the native
//! failover coordinator and compares the returned row, the collect-request hosts, and the
//! cooldown/observation files byte for byte. Runs on the Unix service host (Nas1).

use aac_files::registry::{self, Owners};
use aac_http::account_usage::coordinator::UsageIdentity;
use aac_http::account_usage::coordinator::{
    FailoverCoordinator, FailoverDeps, PrimarySource, UsageFailoverTarget, UsageHost,
};
use aac_views::js_json;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FakeState {
    now: f64,
    hosts: Vec<UsageHost>,
    inventories: HashMap<String, Result<Vec<Value>, ()>>,
    collections: HashMap<String, Result<Value, ()>>,
    sign_in_needed: HashMap<String, Vec<String>>,
    incomplete: HashMap<String, Vec<String>>,
    calls: Vec<(String, Value)>,
}

struct FakeDeps {
    state: Arc<Mutex<FakeState>>,
}

impl FailoverDeps for FakeDeps {
    fn transport<'a>(
        &'a self,
        host: &'a UsageHost,
        request: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ()>> + Send + 'a>> {
        let state = Arc::clone(&self.state);
        let host = host.clone();
        let request = request.clone();
        Box::pin(async move {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.calls.push((host.host.clone(), request.clone()));
            if request.get("action").and_then(Value::as_str) == Some("inventory") {
                let mut out = json!({"version":1,"action":"inventory","candidates":[]});
                match s.inventories.get(&host.host) {
                    Some(Ok(cands)) => out["candidates"] = Value::Array(cands.clone()),
                    Some(Err(_)) => return Err(()),
                    None => {}
                }
                if let Some(sign_in) = s.sign_in_needed.get(&host.host) {
                    out["signInNeededSelectors"] = json!(sign_in);
                }
                if let Some(gaps) = s.incomplete.get(&host.host) {
                    out["incompleteSelectors"] = json!(gaps);
                }
                Ok(out)
            } else {
                let mut out = json!({"version":1,"action":"collect","candidate":request.get("candidate").cloned().unwrap_or(Value::Null),"sample":Value::Null,"failureCode":"unavailable"});
                match s.collections.get(&host.host) {
                    Some(Ok(coll)) => {
                        if let Some(obj) = coll.as_object() {
                            for (k, v) in obj {
                                out[k] = v.clone();
                            }
                        }
                        Ok(out)
                    }
                    Some(Err(_)) => Err(()),
                    None => Ok(out),
                }
            }
        })
    }

    fn hosts<'a>(&'a self) -> Pin<Box<dyn Future<Output = Vec<UsageHost>> + Send + 'a>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hosts
                .clone()
        })
    }

    fn now(&self) -> f64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).now
    }
}

fn collection_owners() -> Owners {
    let rows = registry::rows().expect("registry");
    let mut files = BTreeMap::new();
    for row in rows {
        let owner = if row.moves_with.as_deref() == Some("collection") {
            "rust-service"
        } else {
            row.owner.word()
        };
        files.insert(row.id.clone(), owner.to_owned());
    }
    Owners::from_boot(rows, &files).expect("owners")
}

fn host(value: &Value) -> UsageHost {
    UsageHost {
        host: value["host"].as_str().unwrap_or("").to_owned(),
        platform: value["platform"].as_str().unwrap_or("").to_owned(),
        ssh_host: value["sshHost"].as_str().map(str::to_owned),
    }
}

fn identity(value: &Value) -> UsageIdentity {
    UsageIdentity {
        kind: value["kind"].as_str().unwrap_or("").to_owned(),
        key: value["key"].as_str().unwrap_or("").to_owned(),
    }
}

fn target(value: &Value, ccs_dir: PathBuf) -> UsageFailoverTarget {
    let primary_sources = value["primarySources"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| PrimarySource {
                    host: host(s),
                    selector: s["selector"].as_str().unwrap_or("").to_owned(),
                    claude_profile: s.get("claudeProfile").filter(|v| !v.is_null()).cloned(),
                })
                .collect()
        })
        .unwrap_or_default();
    UsageFailoverTarget {
        account_id: value["accountId"].as_str().unwrap_or("").to_owned(),
        provider: value["provider"].as_str().unwrap_or("").to_owned(),
        ccs_dir,
        binding: value["binding"].as_str().unwrap_or("").to_owned(),
        primary_sources,
        claude_desktop_cadence: value["claudeDesktopCadence"] == json!(true),
        expected_email: value
            .get("expectedEmail")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expected_identity: value
            .get("expectedIdentity")
            .filter(|v| !v.is_null())
            .map(identity),
    }
}

fn load_corpus() -> Value {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("g4-corpus.json");
    let text = std::fs::read_to_string(file).expect("corpus");
    serde_json::from_str(&text).expect("corpus parse")
}

fn set_step(state: &mut FakeState, step: &Value) {
    state.now = step["now"].as_f64().unwrap_or(0.0);
    state.inventories.clear();
    state.collections.clear();
    state.sign_in_needed.clear();
    state.incomplete.clear();
    if let Some(hosts) = step["hosts"].as_array() {
        state.hosts = hosts.iter().map(host).collect();
    }
    if let Some(inv) = step["inventories"].as_object() {
        for (h, v) in inv {
            state.inventories.insert(
                h.clone(),
                if v.as_str() == Some("error") {
                    Err(())
                } else {
                    Ok(v.as_array().cloned().unwrap_or_default())
                },
            );
        }
    }
    if let Some(coll) = step["collections"].as_object() {
        for (h, v) in coll {
            state.collections.insert(
                h.clone(),
                if v.as_str() == Some("error") {
                    Err(())
                } else {
                    Ok(v.clone())
                },
            );
        }
    }
    if let Some(sign_in) = step["signInNeeded"].as_object() {
        for (h, v) in sign_in {
            state.sign_in_needed.insert(
                h.clone(),
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            );
        }
    }
    if let Some(gaps) = step["incomplete"].as_object() {
        for (h, v) in gaps {
            state.incomplete.insert(
                h.clone(),
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            );
        }
    }
}

fn read_dir_json(dir: &PathBuf) -> (HashMap<String, Value>, Option<(String, Value)>) {
    let mut cooldowns = HashMap::new();
    let mut cache = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if name.starts_with("cooldown-") && name.ends_with(".json") {
                cooldowns.insert(name, value);
            } else if name.len() == 69
                && name.ends_with(".json")
                && name[..64]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                cache = Some((name[..64].to_owned(), value));
            }
        }
    }
    (cooldowns, cache)
}

/// Compare as JSON.stringify bytes; a mismatch is recorded so one run reports every diff.
fn check_json(diffs: &mut Vec<String>, label: &str, expected: &Value, actual: &Value) {
    let expected_bytes = js_json::write(expected);
    let actual_bytes = js_json::write(actual);
    if expected_bytes != actual_bytes {
        diffs.push(format!(
            "{label}\n  expected: {}\n  actual:   {}",
            String::from_utf8_lossy(&expected_bytes),
            String::from_utf8_lossy(&actual_bytes)
        ));
    }
}

fn check_eq<T: std::fmt::Debug + PartialEq>(
    diffs: &mut Vec<String>,
    label: &str,
    expected: T,
    actual: T,
) {
    if expected != actual {
        diffs.push(format!(
            "{label}\n  expected: {expected:?}\n  actual:   {actual:?}"
        ));
    }
}

#[tokio::test]
async fn g4_corpus_matches_the_typescript_coordinator() {
    let corpus = load_corpus();
    let scenarios = corpus.as_array().expect("scenarios");
    let mut checked = 0;
    let mut diffs: Vec<String> = Vec::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap_or("?");
        let ccs_dir = std::env::temp_dir().join("aac-g4-parity").join(name);
        let _ = std::fs::remove_dir_all(&ccs_dir);
        std::fs::create_dir_all(&ccs_dir).expect("ccs");
        let tgt = target(&scenario["target"], ccs_dir.clone());
        let template = scenario["template"].clone();
        let state = Arc::new(Mutex::new(FakeState {
            hosts: vec![
                host(&json!({"host":"mac","platform":"mac","sshHost":"fixture-mac"})),
                host(&json!({"host":"ubuntu","platform":"ubuntu","sshHost":"fixture-ubuntu"})),
                host(&json!({"host":"windows","platform":"windows","sshHost":"fixture-windows"})),
            ],
            ..Default::default()
        }));
        let deps = Arc::new(FakeDeps {
            state: Arc::clone(&state),
        });
        let owners = collection_owners();
        let mut coordinator = FailoverCoordinator::new(deps.clone(), owners.clone());
        for step in scenario["steps"].as_array().expect("steps") {
            if step["restart"] == json!(true) {
                coordinator = FailoverCoordinator::new(deps.clone(), owners.clone());
            }
            {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                set_step(&mut s, step);
                s.calls.clear();
            }
            if let Some(peers) = step["musePeers"].as_array() {
                let key = coordinator.key(&tgt);
                let grant = json!({"version":1,"key":key,"identity":{"kind":"muse-team","key":"a".repeat(64)},"peers":peers});
                let dir = ccs_dir.join("account-usage-failover");
                std::fs::create_dir_all(&dir).expect("dir");
                let path = dir.join(format!("{key}.muse-peers.json"));
                std::fs::write(&path, js_json::write(&grant)).expect("grant");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                        .expect("grant mode");
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                        .expect("dir mode");
                }
            }
            let op = step["op"].as_str().unwrap_or("");
            let tmpl = if step["template"].is_null() {
                template.clone()
            } else {
                step["template"].clone()
            };
            let expected = &step["expected"];
            let label = format!("{name}/{op}@{}", step["now"].as_f64().unwrap_or(0.0));
            if op == "collect" || op == "recover" || op == "peek" {
                let result = if op == "collect" {
                    coordinator.collect(&tgt, &tmpl).await
                } else if op == "recover" {
                    coordinator.recover(&tgt, &tmpl).await
                } else {
                    coordinator.peek(&tgt, &tmpl)
                };
                check_json(
                    &mut diffs,
                    &format!("{label} row"),
                    &expected["result"],
                    &result,
                );
            } else {
                let value = if op == "holdCooldown" {
                    coordinator.hold_cooldown(&tgt, step["holdUntil"].as_f64().unwrap_or(0.0))
                } else {
                    coordinator.cooldown_until(&tgt)
                };
                let want = expected["result"].as_f64().unwrap_or(f64::NAN);
                if !(value == want || (value.is_nan() && want.is_nan())) {
                    diffs.push(format!(
                        "{label} value\n  expected: {want}\n  actual:   {value}"
                    ));
                }
            }
            // Collect-request hosts.
            let collected: Vec<String> = state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .calls
                .iter()
                .filter(|(_, req)| req.get("action").and_then(Value::as_str) == Some("collect"))
                .map(|(h, _)| h.clone())
                .collect();
            let expected_hosts: Vec<String> = expected["collectedHosts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|h| h.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            check_eq(
                &mut diffs,
                &format!("{label} collected hosts"),
                expected_hosts,
                collected,
            );
            // Cooldown + observation files.
            let dir = ccs_dir.join("account-usage-failover");
            let (cooldowns, cache) = read_dir_json(&dir);
            let expected_cooldowns = expected["cooldowns"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut actual_names: Vec<&String> = cooldowns.keys().collect();
            let mut expected_names: Vec<&String> = expected_cooldowns.keys().collect();
            actual_names.sort();
            expected_names.sort();
            check_eq(
                &mut diffs,
                &format!("{label} cooldown files"),
                expected_names,
                actual_names,
            );
            for (file, expected_value) in &expected_cooldowns {
                match cooldowns.get(file) {
                    Some(actual) => check_json(
                        &mut diffs,
                        &format!("{label} cooldown {file}"),
                        expected_value,
                        actual,
                    ),
                    None => diffs.push(format!("{label} missing cooldown {file}")),
                }
            }
            let expected_cache = &expected["cache"];
            if expected_cache.is_null() {
                if cache.is_some() {
                    diffs.push(format!("{label} unexpected cache file"));
                }
            } else if let Some((key, actual)) = cache.as_ref() {
                check_eq(
                    &mut diffs,
                    &format!("{label} cache key"),
                    expected["cacheKey"].as_str().unwrap_or("").to_owned(),
                    key.clone(),
                );
                check_json(
                    &mut diffs,
                    &format!("{label} cache"),
                    expected_cache,
                    actual,
                );
            } else {
                diffs.push(format!("{label} missing cache"));
            }
            checked += 1;
        }
    }
    assert!(checked >= 30, "expected at least 30 steps, got {checked}");
    assert!(
        diffs.is_empty(),
        "{} of {checked} steps differ from the TypeScript coordinator:\n\n{}",
        diffs.len(),
        diffs.join("\n\n")
    );
}

/// Card OCG1: the discover entry lists every stored key's identity once, marking those another
/// entry already names, from the inventory alone (no collect request).
#[tokio::test]
async fn discovered_identities_list_each_stored_key_once() {
    let dir = tempfile::tempdir().expect("account root");
    let target = target(
        &json!({
            "accountId":"opencode-go:usage", "provider":"opencode-go", "binding":"fixture",
            "primarySources":[{"host":"ubuntu","platform":"ubuntu","selector":"discover"}]
        }),
        dir.path().to_owned(),
    );
    let stored = |locator: &str, selector: &str, key: &str| {
        json!({
            "locator":locator.repeat(64), "selectors":[selector], "revision":"2".repeat(64),
            "identity":{"kind":"opencode-go-credential","key":key.repeat(64)}
        })
    };
    let state = Arc::new(Mutex::new(FakeState::default()));
    set_step(
        &mut state.lock().expect("fixture state"),
        &json!({
            "now":1_791_979_200_000_i64, "hosts":[{"host":"ubuntu","platform":"ubuntu","sshHost":null}],
            "inventories":{"ubuntu":[
                stored("1", "discover", "a"), stored("3", "discover", "b"),
                stored("5", "discover", "c"), stored("7", "discover", "a"),
                stored("9", "aac-key:2e034ec2", "a")
            ]}
        }),
    );
    let coordinator = FailoverCoordinator::new(
        Arc::new(FakeDeps {
            state: state.clone(),
        }),
        collection_owners(),
    );
    let found = coordinator
        .discovered_identities(&target, &["aac-key:2e034ec2".to_owned()])
        .await
        .expect("readable stores");
    let id = |key: &str| identity(&json!({"kind":"opencode-go-credential","key":key.repeat(64)}));
    assert_eq!(
        found.found,
        vec![(id("a"), true), (id("b"), false), (id("c"), false)]
    );
    assert!(found.unread.is_empty());
    // A named key slot the helper could not read is reported, never treated as free; an
    // unreadable store gives no answer at all.
    {
        let mut fixture = state.lock().expect("fixture state");
        fixture.now += 31_000.0;
        fixture
            .incomplete
            .insert("ubuntu".to_owned(), vec!["aac-key:2e034ec2".to_owned()]);
    }
    let partial = coordinator
        .discovered_identities(
            &target,
            &["aac-key:2e034ec2".to_owned(), "aac-key:52f3251b".to_owned()],
        )
        .await
        .expect("readable stores");
    assert_eq!(partial.unread, ["aac-key:2e034ec2"]);
    {
        let mut fixture = state.lock().expect("fixture state");
        fixture.now += 31_000.0;
        fixture
            .incomplete
            .insert("ubuntu".to_owned(), vec!["*".to_owned()]);
    }
    assert_eq!(coordinator.discovered_identities(&target, &[]).await, None);
    assert!(
        state
            .lock()
            .expect("fixture state")
            .calls
            .iter()
            .all(|(_, request)| request["action"] == "inventory")
    );
}

/// The duplicate-row guard reads which credential identity a row's reading came from. The
/// accessor names the verified reading's identity and never starts a read.
#[tokio::test]
async fn reading_identity_names_the_shown_reading_without_a_read() {
    let dir = tempfile::tempdir().expect("account root");
    let target = target(
        &json!({
            "accountId":"opencode-go:acct:2e034ec2", "provider":"opencode-go", "binding":"fixture",
            "primarySources":[{"host":"ubuntu","platform":"ubuntu","selector":"aac-key:2e034ec2"}]
        }),
        dir.path().to_owned(),
    );
    let template = json!({
        "id":"opencode-go:acct:2e034ec2", "provider":"opencode-go", "platform":"ubuntu",
        "status":"unavailable", "email":null, "windows":[]
    });
    let candidate = json!({
        "locator":"1".repeat(64), "selectors":["aac-key:2e034ec2"], "revision":"2".repeat(64),
        "identity":{"kind":"opencode-go-key","key":"a".repeat(64)}
    });
    let now = 1_791_979_200_000_i64;
    let state = Arc::new(Mutex::new(FakeState::default()));
    set_step(
        &mut state.lock().expect("fixture state"),
        &json!({
            "now":now, "hosts":[{"host":"ubuntu","platform":"ubuntu","sshHost":null}],
            "inventories":{"ubuntu":[candidate]},
            "collections":{"ubuntu":{"failureCode":null,"sample":{
                "status":"ok", "sampledAt":aac_num::ecma::to_iso_string(now),
                "fetchedAt":aac_num::ecma::to_iso_string(now),
                "windows":[{"key":"weekly","label":"Weekly","usedPercent":16}]
            }}}
        }),
    );
    let coordinator = FailoverCoordinator::new(
        Arc::new(FakeDeps {
            state: state.clone(),
        }),
        collection_owners(),
    );
    assert_eq!(coordinator.reading_identity(&target), None);
    assert!(state.lock().expect("fixture state").calls.is_empty());
    let row = coordinator.collect(&target, &template).await;
    assert_eq!(row["status"], "ok", "{row}");
    let calls = state.lock().expect("fixture state").calls.len();
    assert_eq!(
        coordinator.reading_identity(&target),
        Some(identity(
            &json!({"kind":"opencode-go-key","key":"a".repeat(64)})
        ))
    );
    assert_eq!(state.lock().expect("fixture state").calls.len(), calls);
}

/// N3: the first sample after a restart holds no state in memory, but the saved reading on
/// disk still names the identity. The peek reads it without a provider call, without creating
/// state and without writing anything, so an unreadable named key file never hides the
/// unnamed key's row for that sample.
#[tokio::test]
async fn reading_identity_after_restart_peeks_the_saved_reading_without_a_read() {
    let dir = tempfile::tempdir().expect("account root");
    let target = target(
        &json!({
            "accountId":"opencode-go:acct:2e034ec2", "provider":"opencode-go", "binding":"fixture",
            "primarySources":[{"host":"ubuntu","platform":"ubuntu","selector":"aac-key:2e034ec2"}]
        }),
        dir.path().to_owned(),
    );
    let template = json!({
        "id":"opencode-go:acct:2e034ec2", "provider":"opencode-go", "platform":"ubuntu",
        "status":"unavailable", "email":null, "windows":[]
    });
    let candidate = json!({
        "locator":"1".repeat(64), "selectors":["aac-key:2e034ec2"], "revision":"2".repeat(64),
        "identity":{"kind":"opencode-go-key","key":"a".repeat(64)}
    });
    let now = 1_791_979_200_000_i64;
    let state = Arc::new(Mutex::new(FakeState::default()));
    set_step(
        &mut state.lock().expect("fixture state"),
        &json!({
            "now":now, "hosts":[{"host":"ubuntu","platform":"ubuntu","sshHost":null}],
            "inventories":{"ubuntu":[candidate]},
            "collections":{"ubuntu":{"failureCode":null,"sample":{
                "status":"ok", "sampledAt":aac_num::ecma::to_iso_string(now),
                "fetchedAt":aac_num::ecma::to_iso_string(now),
                "windows":[{"key":"weekly","label":"Weekly","usedPercent":16}]
            }}}
        }),
    );
    let coordinator = || {
        FailoverCoordinator::new(
            Arc::new(FakeDeps {
                state: state.clone(),
            }),
            collection_owners(),
        )
    };
    let row = coordinator().collect(&target, &template).await;
    assert_eq!(row["status"], "ok", "{row}");
    let failover_dir = dir.path().join("account-usage-failover");
    let before = read_dir_json(&failover_dir);
    // Restart: a fresh coordinator has no state in memory; the disk cache still names the key.
    let restarted = coordinator();
    let calls = state.lock().expect("fixture state").calls.len();
    assert_eq!(
        restarted.reading_identity(&target),
        Some(identity(
            &json!({"kind":"opencode-go-key","key":"a".repeat(64)})
        ))
    );
    assert_eq!(state.lock().expect("fixture state").calls.len(), calls);
    assert_eq!(read_dir_json(&failover_dir), before);
    // A reading saved for one identity never answers a target pinned to another.
    let mut other = target.clone();
    other.expected_identity = Some(identity(
        &json!({"kind":"opencode-go-key","key":"b".repeat(64)}),
    ));
    assert_eq!(restarted.reading_identity(&other), None);
}

#[tokio::test]
async fn claude_code_covers_a_signed_out_desktop_across_restart() {
    let dir = tempfile::tempdir().expect("account root");
    let fixture = |selector: &str, locator: &str| {
        json!({
            "locator":locator.repeat(64), "selectors":[selector], "revision":"e".repeat(64),
            "identity":{"kind":"claude-account-org-v1","key":"c".repeat(64)},
            "email":"same@example.test"
        })
    };
    let desktop = fixture("claude-desktop-profile:personal", "6");
    let code = fixture("t3", "5");
    let target = target(
        &json!({
            "accountId":"claude:personal", "provider":"claude", "binding":"fixture",
            "expectedEmail":"same@example.test", "claudeDesktopCadence":true,
            "primarySources":[{"host":"windows","platform":"windows","selector":"claude-desktop-profile:personal"}]
        }),
        dir.path().to_owned(),
    );
    let template = json!({
        "id":"claude:personal", "provider":"claude", "platform":"windows",
        "status":"unavailable", "email":"same@example.test", "windows":[]
    });
    let state = Arc::new(Mutex::new(FakeState::default()));
    let coordinator = || {
        FailoverCoordinator::new(
            Arc::new(FakeDeps {
                state: state.clone(),
            }),
            collection_owners(),
        )
    };

    // Each case gets a fresh account root. Rejected Code must clear existing coverage.
    for (name, mut peers, starts_signed_out) in [
        ("bootstrap", vec![code.clone()], true),
        ("after-desktop", vec![code.clone()], false),
        (
            "ambiguous",
            vec![code.clone(), {
                let mut candidate = fixture("t3", "7");
                candidate["identity"]["key"] = json!("d".repeat(64));
                candidate
            }],
            true,
        ),
        (
            "other-email",
            vec![{
                let mut candidate = code.clone();
                candidate["email"] = json!("other@example.test");
                candidate
            }],
            true,
        ),
    ] {
        let mut target = target.clone();
        target.account_id = format!("claude:{name}");
        let coordinator_instance = coordinator();
        let step = |minutes: i64, candidates: &[Value], signed_out: bool, reject: bool| {
            let now = 1_791_979_200_000 + minutes * 60_000;
            let sample = json!({
                "status":"ok", "email":"same@example.test",
                "sampledAt":aac_num::ecma::to_iso_string(now), "fetchedAt":aac_num::ecma::to_iso_string(now),
                "windows":[{"key":"five_hour","label":"5-hour limit","usedPercent":42}]
            });
            let mut state = state.lock().expect("fixture state");
            state.calls.clear();
            set_step(
                &mut state,
                &json!({
                    "now":now, "inventories":{"windows":candidates},
                    "signInNeeded":{"windows":if signed_out { vec!["claude-desktop-profile:personal"] } else { vec![] }},
                    "collections":{"windows":if reject {
                        json!({"sample":null,"failureCode":"needs_sign_in"})
                    } else {
                        json!({"sample":sample,"failureCode":null})
                    }}
                }),
            );
        };
        if !starts_signed_out {
            peers.insert(0, desktop.clone());
            step(0, &peers, false, false);
            let row = coordinator_instance.collect(&target, &template).await;
            assert_eq!(row["status"], "ok", "{name}");
            assert!(row.get("signInNeeded").is_none(), "{name}");
            peers.remove(0);
        }
        step(10, &peers, true, false);
        let row = coordinator_instance.collect(&target, &template).await;
        let collected: Vec<Value> = state
            .lock()
            .expect("fixture state")
            .calls
            .iter()
            .filter(|(_, request)| request["action"] == "collect")
            .map(|(_, request)| request["candidate"]["locator"].clone())
            .collect();
        if matches!(name, "ambiguous" | "other-email") {
            assert!(collected.is_empty(), "{name}");
            assert_eq!(row["signInNeeded"], json!(["windows"]), "{name}");
            assert!(row.get("claudeDesktopSignedOut").is_none(), "{name}");
            continue;
        }
        assert_eq!(row["status"], "ok", "{name}: {row}");
        assert_eq!(
            row["source"], "Verified quota on Windows (fallback)",
            "{name}"
        );
        assert_eq!(collected, vec![code["locator"].clone()], "{name}");
        for result in [row, coordinator().peek(&target, &template)] {
            assert!(result.get("signInNeeded").is_none(), "{name}: {result}");
            assert_eq!(
                result["claudeDesktopSignedOut"],
                json!(["windows"]),
                "{name}"
            );
        }
        step(20, &peers, true, true);
        let rejected = coordinator_instance.collect(&target, &template).await;
        assert_eq!(rejected["signInNeeded"], json!(["windows"]), "{name}");
        assert!(rejected.get("claudeDesktopSignedOut").is_none(), "{name}");
    }
}
