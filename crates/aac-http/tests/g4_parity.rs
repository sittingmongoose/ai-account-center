//! G4 byte parity: replays the TypeScript-generated corpus (g4-fixtures.ts) through the native
//! failover coordinator and compares the returned row, the collect-request hosts, and the
//! cooldown/observation files byte for byte. Runs on the Unix service host (Nas1).

use aac_files::registry::{self, Owners};
use aac_http::account_usage::coordinator::{
    FailoverCoordinator, FailoverDeps, PrimarySource, UsageFailoverTarget, UsageHost,
};
use aac_http::account_usage::coordinator::UsageIdentity;
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
        Box::pin(async move { state.lock().unwrap_or_else(|e| e.into_inner()).hosts.clone() })
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
        expected_email: value.get("expectedEmail").and_then(Value::as_str).map(str::to_owned),
        expected_identity: value.get("expectedIdentity").filter(|v| !v.is_null()).map(identity),
    }
}

fn load_corpus() -> Value {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("g4-corpus.json");
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
            state.inventories.insert(h.clone(), if v.as_str() == Some("error") { Err(()) } else { Ok(v.as_array().cloned().unwrap_or_default()) });
        }
    }
    if let Some(coll) = step["collections"].as_object() {
        for (h, v) in coll {
            state.collections.insert(h.clone(), if v.as_str() == Some("error") { Err(()) } else { Ok(v.clone()) });
        }
    }
    if let Some(sign_in) = step["signInNeeded"].as_object() {
        for (h, v) in sign_in {
            state.sign_in_needed.insert(h.clone(), v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default());
        }
    }
    if let Some(gaps) = step["incomplete"].as_object() {
        for (h, v) in gaps {
            state.incomplete.insert(h.clone(), v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default());
        }
    }
}

fn read_dir_json(dir: &PathBuf) -> (HashMap<String, Value>, Option<(String, Value)>) {
    let mut cooldowns = HashMap::new();
    let mut cache = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
            if name.starts_with("cooldown-") && name.ends_with(".json") {
                cooldowns.insert(name, value);
            } else if name.len() == 69 && name.ends_with(".json") && name[..64].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                cache = Some((name[..64].to_owned(), value));
            }
        }
    }
    (cooldowns, cache)
}

fn assert_json(label: &str, expected: &Value, actual: &Value) {
    let expected_bytes = js_json::write(expected);
    let actual_bytes = js_json::write(actual);
    assert_eq!(
        expected_bytes,
        actual_bytes,
        "{label}\n  expected: {}\n  actual:   {}",
        String::from_utf8_lossy(&expected_bytes),
        String::from_utf8_lossy(&actual_bytes)
    );
}

#[tokio::test]
async fn g4_corpus_matches_the_typescript_coordinator() {
    let corpus = load_corpus();
    let scenarios = corpus.as_array().expect("scenarios");
    let mut checked = 0;
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap_or("?");
        let ccs = tempfile::tempdir().expect("ccs");
        let ccs_dir = ccs.path().to_path_buf();
        let tgt = target(&scenario["target"], ccs_dir.clone());
        let template = scenario["template"].clone();
        let state = Arc::new(Mutex::new(FakeState { hosts: vec![host(&json!({"host":"mac","platform":"mac","sshHost":"fixture-mac"})), host(&json!({"host":"ubuntu","platform":"ubuntu","sshHost":"fixture-ubuntu"})), host(&json!({"host":"windows","platform":"windows","sshHost":"fixture-windows"}))], ..Default::default() }));
        let deps = Arc::new(FakeDeps { state: Arc::clone(&state) });
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
                std::fs::write(dir.join(format!("{key}.muse-peers.json")), js_json::write(&grant)).expect("grant");
            }
            let op = step["op"].as_str().unwrap_or("");
            let tmpl = if step["template"].is_null() { template.clone() } else { step["template"].clone() };
            let expected = &step["expected"];
            if op == "collect" || op == "recover" || op == "peek" {
                let result = if op == "collect" {
                    coordinator.collect(&tgt, &tmpl).await
                } else if op == "recover" {
                    coordinator.recover(&tgt, &tmpl).await
                } else {
                    coordinator.peek(&tgt, &tmpl)
                };
                assert_json(&format!("{name}/{op} row"), &expected["result"], &result);
            } else {
                let value = if op == "holdCooldown" {
                    coordinator.hold_cooldown(&tgt, step["holdUntil"].as_f64().unwrap_or(0.0))
                } else {
                    coordinator.cooldown_until(&tgt)
                };
                assert_eq!(value, expected["result"].as_f64().unwrap_or(f64::NAN), "{name}/{op} value");
            }
            // Collect-request hosts.
            let collected: Vec<String> = state.lock().unwrap_or_else(|e| e.into_inner()).calls.iter()
                .filter(|(_, req)| req.get("action").and_then(Value::as_str) == Some("collect"))
                .map(|(h, _)| h.clone()).collect();
            let expected_hosts: Vec<String> = expected["collectedHosts"].as_array().map(|a| a.iter().filter_map(|h| h.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            assert_eq!(collected, expected_hosts, "{name}/{op} collected hosts");
            // Cooldown + observation files.
            let dir = ccs_dir.join("account-usage-failover");
            let (cooldowns, cache) = read_dir_json(&dir);
            let expected_cooldowns = expected["cooldowns"].as_object().cloned().unwrap_or_default();
            assert_eq!(cooldowns.len(), expected_cooldowns.len(), "{name}/{op} cooldown count");
            for (file, expected_value) in &expected_cooldowns {
                let actual = cooldowns.get(file).unwrap_or_else(|| panic!("{name}/{op} missing cooldown {file}"));
                assert_json(&format!("{name}/{op} cooldown {file}"), expected_value, actual);
            }
            let expected_cache = &expected["cache"];
            if expected_cache.is_null() {
                assert!(cache.is_none(), "{name}/{op} unexpected cache file");
            } else {
                let (key, actual) = cache.as_ref().unwrap_or_else(|| panic!("{name}/{op} missing cache"));
                assert_eq!(key, expected["cacheKey"].as_str().unwrap_or(""), "{name}/{op} cache key");
                assert_json(&format!("{name}/{op} cache"), expected_cache, actual);
            }
            checked += 1;
        }
    }
    assert!(checked >= 30, "expected at least 30 steps, got {checked}");
}
