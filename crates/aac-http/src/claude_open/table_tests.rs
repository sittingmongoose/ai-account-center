//! G13 parity against the checked-in goldens: `tests/spec/agent/claude-open-table.json`
//! (generated from the real TS core, progress store, policy parse and account-list rule) and
//! lane F's `claude-host-table.json` Open command vectors. Every comparison is byte-equal on
//! the canonical JSON text; operation ids are random on both sides and normalize to one token.
use super::js::{self, sha, stringify};
use super::manifest::{pending_from_value, profile_from_value, windows_launcher_account_list};
use super::open::{mac_open_command, windows_account_list_script, windows_open_command};
use super::operations::{OpenErrorKind, Operations, Observer};
use super::policy::parse_policy;
use super::sync::{
    self, Adapters, AppendPayload, CopyLimits, Record, Snapshot, synchronize_before_profile_open,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const TABLE: &str = include_str!("../../../../tests/spec/agent/claude-open-table.json");
const HOST_TABLE: &str = include_str!("../../../../tests/spec/agent/claude-host-table.json");

fn table() -> Value {
    serde_json::from_str(TABLE).expect("claude-open-table.json parses")
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn records_from_wire(rows: &Value) -> Vec<Record> {
    rows.as_array()
        .expect("rows array")
        .iter()
        .map(|row| Record {
            name: row["name"].as_str().expect("name").to_string(),
            bytes: unhex(row["bytes"].as_str().expect("bytes")),
            sha256: row["sha256"].as_str().expect("sha256").to_string(),
        })
        .collect()
}

fn decoded_rows(rows: &Value) -> Vec<Value> {
    records_from_wire(rows)
        .into_iter()
        .map(|record| js::parse(&record.bytes).expect("record JSON"))
        .collect()
}

/// Replace every random operation id with one token on both sides of a comparison.
fn normalize_ids(value: Value) -> Value {
    match value {
        Value::String(text) => {
            if text.len() == 27
                && text.starts_with("op_")
                && text[3..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                Value::String("op_ID".into())
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_ids).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| (key, normalize_ids(item)))
                .collect::<Map<String, Value>>(),
        ),
        other => other,
    }
}

fn valid_policy(table: &Value) -> Value {
    table["policy"][0]["value"].clone()
}

#[test]
fn policy_parse_matches_the_ts_reference() {
    let table = table();
    for case in table["policy"].as_array().expect("policy cases") {
        let got = parse_policy(&case["value"]);
        let expected = &case["expected"];
        let name = case["name"].as_str().expect("name");
        match (got, expected.is_null()) {
            (None, true) => {}
            (Some(value), false) => {
                assert_eq!(
                    stringify(&value),
                    stringify(&js::normalize(expected.clone())),
                    "policy case {name}"
                );
            }
            (got, want_null) => panic!("policy case {name}: got {got:?}, null={want_null}"),
        }
    }
}

#[test]
fn neutral_projection_matches_the_ts_reference() {
    let table = table();
    let policy = valid_policy(&table);
    for case in table["neutral"].as_array().expect("neutral cases") {
        let name = case["name"].as_str().expect("name");
        let got = sync::neutral(&case["source"], "windows", "mac", &policy);
        let expected = &case["expected"];
        if let Some(reason) = expected.get("reason").and_then(Value::as_str) {
            assert!(
                matches!(got, sync::Neutral::Reason(r) if r == reason),
                "neutral case {name}: expected reason {reason}"
            );
        } else {
            let sync::Neutral::Descriptor(descriptor) = got else {
                panic!("neutral case {name}: expected a descriptor");
            };
            assert_eq!(
                stringify(&descriptor),
                stringify(&js::normalize(expected["descriptor"].clone())),
                "neutral case {name}"
            );
        }
    }
}

#[test]
fn plan_matches_the_ts_reference() {
    let table = table();
    let policy = valid_policy(&table);
    for case in table["plan"].as_array().expect("plan cases") {
        let name = case["name"].as_str().expect("name");
        let plan = sync::plan_missing(
            &decoded_rows(&case["source"]),
            &decoded_rows(&case["target"]),
            "windows",
            "mac",
            &policy,
        )
        .expect("plan");
        let expected = &case["expected"];
        let records: Vec<Value> = plan
            .records
            .iter()
            .map(|record| json!({"name": record.name, "bytes": hex(&record.bytes)}))
            .collect();
        assert_eq!(
            stringify(&Value::Array(records)),
            stringify(&expected["records"]),
            "plan case {name} records"
        );
        assert_eq!(
            plan.already_present_count as u64,
            expected["alreadyPresentCount"].as_u64().expect("count"),
            "plan case {name} present"
        );
        assert_eq!(
            stringify(&Value::Object(plan.skipped)),
            stringify(&expected["skipped"]),
            "plan case {name} skipped"
        );
    }
}

#[test]
fn snapshot_revision_matches_the_ts_reference() {
    let table = table();
    for case in table["revision"].as_array().expect("revision cases") {
        let records = records_from_wire(&case["rows"]);
        assert_eq!(
            sync::snapshot_revision(&records),
            case["expected"].as_str().expect("revision"),
            "revision case {}",
            case["name"]
        );
    }
}

#[test]
fn decode_records_matches_the_ts_reference() {
    let table = table();
    for case in table["decode"].as_array().expect("decode cases") {
        let name = case["name"].as_str().expect("name");
        let records = records_from_wire(&case["rows"]);
        let got = sync::decode_records(&records);
        let expected = &case["expected"];
        if expected.is_null() {
            assert!(got.is_ok(), "decode case {name}: expected ok");
        } else {
            let reason = expected.as_str().expect("reason");
            assert!(
                matches!(got, Err(sync::Refused(r)) if r == reason),
                "decode case {name}: expected {reason}"
            );
        }
    }
}

// --- copy sequences over the same scripted adapters the generator uses ----------------------
struct SequenceState {
    closed: usize,
    target: usize,
    source: usize,
    protected: usize,
    append: usize,
    appended: Vec<Record>,
    clock: Arc<AtomicU64>,
}

struct TableAdapters {
    case: Value,
    state: Mutex<SequenceState>,
}

fn pick<'a>(list: &'a Value, index: usize) -> Option<&'a Value> {
    list.as_array().and_then(|items| items.get(index))
}

fn snapshot_from_json(value: &Value) -> Snapshot {
    let mut fields = value.as_object().expect("snapshot object").clone();
    let rows = fields.remove("records").expect("records");
    Snapshot {
        fields,
        records: records_from_wire(&rows),
    }
}

fn wire_rows(records: &[Record]) -> Value {
    Value::Array(
        records
            .iter()
            .map(|record| {
                json!({"name": record.name, "bytes": hex(&record.bytes), "sha256": record.sha256})
            })
            .collect(),
    )
}

impl TableAdapters {
    fn grown_target(&self, state: &SequenceState) -> Snapshot {
        let baseline = &self.case["target"][0];
        let mut rows = records_from_wire(&baseline["records"]);
        rows.extend(state.appended.clone());
        let mut fields = baseline.as_object().expect("baseline").clone();
        fields.insert("records".into(), wire_rows(&rows));
        fields.insert(
            "revision".into(),
            json!(sync::snapshot_revision(&rows)),
        );
        Snapshot {
            fields,
            records: rows,
        }
    }
}

impl Adapters for TableAdapters {
    fn is_target_closed<'a>(
        &'a self,
        _profile_id: &str,
        _platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| ())?;
            let index = state.closed;
            state.closed += 1;
            Ok(pick(&self.case["closed"], index)
                .and_then(Value::as_bool)
                .unwrap_or(true))
        })
    }

    fn read_target<'a>(
        &'a self,
        _profile_id: &str,
        _platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Snapshot, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| ())?;
            let index = state.target;
            state.target += 1;
            if let Some(scripted) = pick(&self.case["target"], index) {
                return Ok(snapshot_from_json(scripted));
            }
            Ok(self.grown_target(&state))
        })
    }

    fn read_source<'a>(
        &'a self,
        _profile_id: &str,
        _platform: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Snapshot, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| ())?;
            let index = state.source;
            state.source += 1;
            let scripted = pick(&self.case["source"], index)
                .cloned()
                .unwrap_or_else(|| json!({"records": []}));
            Ok(snapshot_from_json(&scripted))
        })
    }

    fn verify_transcript_references<'a>(
        &'a self,
        _profile_id: &str,
        _platform: &'a str,
        _records: &[Record],
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        let verify = self.case["verify"].as_bool().unwrap_or(true);
        Box::pin(async move { Ok(verify) })
    }

    fn target_protected_state_unchanged<'a>(
        &'a self,
        _target: &Snapshot,
    ) -> Pin<Box<dyn Future<Output = Result<bool, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| ())?;
            let index = state.protected;
            state.protected += 1;
            Ok(pick(&self.case["protected"], index)
                .and_then(Value::as_bool)
                .unwrap_or(true))
        })
    }

    fn append_create_only<'a>(
        &'a self,
        payload: AppendPayload<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().map_err(|_| ())?;
            let index = state.append;
            state.append += 1;
            let step = self.case["limits"]["clockStep"].as_u64().unwrap_or(0);
            state.clock.fetch_add(step, Ordering::SeqCst);
            let entry = pick(&self.case["append"], index)
                .cloned()
                .unwrap_or_else(|| json!({"receipt": {"status": "created_metadata", "createdCount": 1, "protectedBytesUnchanged": true, "writerQuiescent": true}}));
            if entry.get("throw").and_then(Value::as_bool) == Some(true) {
                return Err(());
            }
            let receipt = entry["receipt"].clone();
            if receipt["status"] == json!("created_metadata") {
                for record in payload.records {
                    state.appended.push(Record {
                        name: record.name.clone(),
                        sha256: sha(&record.bytes),
                        bytes: record.bytes.clone(),
                    });
                }
            }
            Ok(receipt)
        })
    }
}

#[tokio::test]
async fn copy_sequences_match_the_ts_reference() {
    let table = table();
    let policy = valid_policy(&table);
    for case in table["sequences"].as_array().expect("sequences") {
        let name = case["name"].as_str().expect("name");
        let clock = Arc::new(AtomicU64::new(0));
        let adapters = TableAdapters {
            case: case.clone(),
            state: Mutex::new(SequenceState {
                closed: 0,
                target: 0,
                source: 0,
                protected: 0,
                append: 0,
                appended: Vec::new(),
                clock: Arc::clone(&clock),
            }),
        };
        let limits = CopyLimits {
            budget_ms: case["limits"]["budgetMs"]
                .as_u64()
                .map(|n| n.min(sync::OPEN_COPY_BUDGET_MS))
                .unwrap_or(sync::OPEN_COPY_BUDGET_MS),
            max_batches: case["limits"]["maxBatches"]
                .as_u64()
                .map(|n| n.min(sync::MAX_BATCHES_PER_OPEN))
                .unwrap_or(sync::MAX_BATCHES_PER_OPEN),
            clock: Box::new(move || Some(clock.load(Ordering::SeqCst))),
        };
        let profile_id = case["profileId"].as_str().unwrap_or("gmail");
        let target_platform = case["targetPlatform"].as_str().unwrap_or("mac");
        let case_policy = if case["policy"].is_null() {
            policy.clone()
        } else {
            case["policy"].clone()
        };
        let result = synchronize_before_profile_open(
            profile_id,
            target_platform,
            &case_policy,
            &adapters,
            &limits,
        )
        .await;
        assert_eq!(
            stringify(&result),
            case["expected"].as_str().expect("expected text"),
            "sequence case {name}"
        );
    }
}

// --- progress store scripts ------------------------------------------------------------------
struct RunningEntry {
    sender: tokio::sync::oneshot::Sender<Result<(), OpenErrorKind>>,
    observer: Observer,
}

fn error_kind(word: &str) -> OpenErrorKind {
    match word {
        "held" => OpenErrorKind::HistoryHeld,
        "notfound" => OpenErrorKind::NotFound,
        "notconfig" => OpenErrorKind::NotConfigured,
        "timeout" => OpenErrorKind::TransportTimeout,
        "transport" => OpenErrorKind::Transport,
        _ => OpenErrorKind::Other,
    }
}

#[tokio::test]
async fn progress_store_matches_the_ts_reference() {
    let table = table();
    for case in table["operations"].as_array().expect("operations cases") {
        let name = case["name"].as_str().expect("name");
        let clock = Arc::new(AtomicU64::new(1_000_000));
        let operations = Operations::with_options(super::operations::Options {
            now: Arc::new({
                let clock = Arc::clone(&clock);
                move || clock.load(Ordering::SeqCst)
            }),
            retain_ms: case["retainMs"].as_u64().unwrap_or(600_000),
        });
        let running: Arc<Mutex<HashMap<String, RunningEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let scope = "/synthetic/.ccs";
        let mut snapshots: Vec<Value> = Vec::new();
        for step in case["steps"].as_array().expect("steps") {
            let profile = step["profile"].as_str().unwrap_or("gmail");
            let platform = step["platform"].as_str().unwrap_or("mac");
            let key = format!("{profile}/{platform}");
            match step["action"].as_str().expect("action") {
                "start" => {
                    let running = Arc::clone(&running);
                    let key = key.clone();
                    operations.start(scope, profile, platform, move |observer| {
                        let (sender, receiver) = tokio::sync::oneshot::channel();
                        running
                            .lock()
                            .expect("running lock")
                            .insert(key, RunningEntry { sender, observer });
                        async move { receiver.await.unwrap_or(Err(OpenErrorKind::Other)) }
                    });
                }
                "copying" => {
                    let guard = running.lock().expect("running lock");
                    if let Some(entry) = guard.get(&key) {
                        entry.observer.copying(
                            step["total"].as_u64().unwrap_or(0),
                            step["confirmed"].as_u64().unwrap_or(0),
                        );
                    }
                }
                "synchronized" => {
                    let guard = running.lock().expect("running lock");
                    if let Some(entry) = guard.get(&key) {
                        entry.observer.synchronized(
                            step["status"].as_str().unwrap_or(""),
                            step["createdCount"].as_u64().unwrap_or(0),
                        );
                    }
                }
                "opening" => {
                    let guard = running.lock().expect("running lock");
                    if let Some(entry) = guard.get(&key) {
                        entry.observer.opening();
                    }
                }
                "finish" => {
                    let entry = running.lock().expect("running lock").remove(&key);
                    if let Some(entry) = entry {
                        let outcome = match step["error"].as_str() {
                            Some(word) => Err(error_kind(word)),
                            None => Ok(()),
                        };
                        let _ = entry.sender.send(outcome);
                    }
                    for _ in 0..4 {
                        tokio::task::yield_now().await;
                    }
                }
                "advance" => {
                    clock.fetch_add(step["ms"].as_u64().unwrap_or(0), Ordering::SeqCst);
                }
                "snapshot" => {
                    tokio::task::yield_now().await;
                    let for_scope: Vec<Value> = operations
                        .for_scope(scope)
                        .into_iter()
                        .map(|(id, operation)| json!([id, operation.to_json()]))
                        .collect();
                    let running_op = operations
                        .running(scope, profile, platform)
                        .map(|operation| operation.to_json())
                        .unwrap_or(Value::Null);
                    snapshots.push(json!({"forScope": for_scope, "running": running_op}));
                }
                other => panic!("unknown step {other}"),
            }
        }
        let got = stringify(&normalize_ids(Value::Array(snapshots)));
        let expected = stringify(&normalize_ids(js::normalize(case["expected"].clone())));
        assert_eq!(got, expected, "operations case {name}");
    }
}

#[test]
fn account_list_matches_the_ts_reference() {
    let table = table();
    for case in table["accountList"].as_array().expect("accountList cases") {
        let name = case["name"].as_str().expect("name");
        let manifest: Vec<_> = case["manifest"]
            .as_array()
            .expect("manifest")
            .iter()
            .map(|row| profile_from_value(row).expect("manifest row"))
            .collect();
        let pending = pending_from_value(&json!({"version": 1, "profiles": case["pending"]}))
            .expect("pending");
        let got = windows_launcher_account_list(&manifest, &pending);
        let expected = &case["expected"];
        if expected.is_null() {
            assert!(got.is_none(), "accountList case {name}: expected null");
        } else {
            assert_eq!(
                stringify(&json!(got.expect("list"))),
                stringify(expected),
                "accountList case {name}"
            );
        }
    }
}

#[test]
fn open_commands_match_lane_f_goldens() {
    let table: Value = serde_json::from_str(HOST_TABLE).expect("claude-host-table.json parses");
    for case in table["open"].as_array().expect("open vectors") {
        let name = case["name"].as_str().expect("name");
        let wants_error = case["expected"].is_object();
        let got = if case["kind"] == json!("mac") {
            mac_open_command(case["launcherPath"].as_str())
        } else {
            let account_list: Option<Vec<String>> = case["accountList"].as_array().map(|items| {
                items
                    .iter()
                    .map(|id| id.as_str().expect("id").to_string())
                    .collect()
            });
            windows_open_command(
                case["profileId"].as_str().expect("profileId"),
                account_list.as_deref(),
            )
        };
        if wants_error {
            assert!(got.is_err(), "open case {name}: expected refusal");
        } else {
            assert_eq!(
                got.expect("command"),
                case["expected"].as_str().expect("command text"),
                "open case {name}"
            );
        }
    }
    for case in table["accountList"].as_array().expect("accountList vectors") {
        let name = case["name"].as_str().expect("name");
        let ids: Vec<String> = case["ids"]
            .as_array()
            .expect("ids")
            .iter()
            .map(|id| id.as_str().expect("id").to_string())
            .collect();
        let got = windows_account_list_script(&ids);
        if case["expected"].is_object() {
            assert!(got.is_err(), "accountList script case {name}: expected refusal");
        } else {
            let expected: Vec<String> = case["expected"]
                .as_array()
                .expect("lines")
                .iter()
                .map(|line| line.as_str().expect("line").to_string())
                .collect();
            assert_eq!(got.expect("script"), expected, "accountList script case {name}");
        }
    }
}
