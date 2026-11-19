//! G16b gate: the Rust answer store (`answers_engine`) replays every script in
//! `tests/spec/answer-journal-vectors.json`, which the real TS `StatusAnswers` produced, and must
//! leave the same journal bytes, the same share files, the same rings, the same folded history
//! and the same delivery views at every check, and give the same per-entry results to every send.
use aac_status::answer_delivery::RingResult;
use aac_status::answers_engine::{AnswerHost, AnswersEngine, CardRead, Fault, Timing, delivery_view, js_stringify};
use aac_status::share::{
    NodeFs,
    reports::{AgentReportsReader, Limits},
};
use serde_json::Value as Json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct State {
    now: f64,
    uuids: u64,
    lines: u64,
    rings: Vec<Json>,
    ring_fail: bool,
    write_once: Option<(String, Option<String>)>,
    write_always: Option<(String, Option<String>)>,
    fsync_once: Option<(String, Option<String>)>,
    void_always: bool,
}

struct Host {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}

fn matches(m: &Option<(String, Option<String>)>, kind: &str, state: Option<&str>) -> bool {
    m.as_ref()
        .is_some_and(|(t, s)| t == kind && (s.is_none() || s.as_deref() == state))
}

impl AnswerHost for Host {
    fn read_card(&mut self, card_id: &str, _limit: Duration) -> CardRead {
        let reader = AgentReportsReader::new(self.root.clone(), Limits::default());
        match reader.read_card_now(&NodeFs, card_id) {
            Ok(Some((doc, sha256))) => CardRead::Card { doc, sha256 },
            Ok(None) => CardRead::Absent,
            Err(_) => CardRead::Failed,
        }
    }
    fn has_doorbell(&self) -> bool {
        true
    }
    fn ring(&mut self, to: &str, subject: &str, body: &str, _timeout: Duration) -> RingResult {
        let mut state = self.state.lock().unwrap();
        state
            .rings
            .push(serde_json::json!({"to": to, "subject": subject, "body": body}));
        if state.ring_fail {
            RingResult {
                ok: false,
                detail: "pm-mail failed.".into(),
            }
        } else {
            RingResult {
                ok: true,
                detail: format!("Rang {to} through pm-mail."),
            }
        }
    }
    fn now(&mut self) -> f64 {
        self.state.lock().unwrap().now
    }
    fn uuid(&mut self) -> String {
        let mut state = self.state.lock().unwrap();
        state.uuids += 1;
        format!("00000000-0000-4000-8000-{:012x}", state.uuids)
    }
    fn line_id(&mut self) -> String {
        let mut state = self.state.lock().unwrap();
        state.lines += 1;
        format!("{:016x}", state.lines)
    }
    fn fault(&mut self, fault: Fault<'_>) -> bool {
        let mut state = self.state.lock().unwrap();
        match fault {
            Fault::Void => state.void_always,
            Fault::Write(kind, s) => {
                if matches(&state.write_once, kind, s) {
                    state.write_once = None;
                    return true;
                }
                matches(&state.write_always, kind, s)
            }
            Fault::Fsync(kind, s) => {
                if matches(&state.fsync_once, kind, s) {
                    state.fsync_once = None;
                    return true;
                }
                false
            }
        }
    }
}

fn share_files(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, String>) {
        if !dir.is_dir() {
            return;
        }
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let full = dir.join(&name);
            let rel = format!("{rel}/{name}");
            if full.is_dir() {
                walk(&full, &rel, out);
            } else {
                out.insert(rel, fs::read_to_string(&full).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(&root.join("v1").join("keys"), "keys", &mut out);
    walk(&root.join("v1").join("answers"), "answers", &mut out);
    out
}

fn match_of(value: &Json) -> Option<(String, Option<String>)> {
    let kind = value.get("type")?.as_str()?.to_owned();
    Some((kind, value.get("state").and_then(Json::as_str).map(str::to_owned)))
}

fn vectors() -> Json {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/answer-journal-vectors.json");
    serde_json::from_slice(&fs::read(file).expect("vectors")).expect("vectors json")
}

fn engine(store: &Path, root: &Path, state: &Arc<Mutex<State>>) -> AnswersEngine {
    AnswersEngine::new(
        store.to_path_buf(),
        root.to_path_buf(),
        Timing::default(),
        Box::new(Host {
            root: root.to_path_buf(),
            state: state.clone(),
        }),
    )
}

fn replay(scenario: &Json) -> usize {
    let name = scenario["name"].as_str().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("reports");
    let store = scratch.path().join("store").join("status");
    fs::create_dir_all(root.join("v1")).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join("answer-key.pem"), scenario["key"].as_str().unwrap()).unwrap();
    let state = Arc::new(Mutex::new(State {
        now: 1_793_982_600_000.0,
        ..State::default()
    }));
    // NOW = 2026-11-06T16:30:00.000Z
    assert_eq!(
        aac_num::ecma::to_iso_string(1_793_982_600_000).as_deref(),
        Some("2026-11-06T16:30:00.000Z")
    );
    let mut answers = engine(&store, &root, &state);
    let mut checks = 0;
    for (step, op) in scenario["ops"].as_array().unwrap().iter().enumerate() {
        let at = format!("{name} op {step} ({})", op["op"]);
        match op["op"].as_str().unwrap() {
            "write" => {
                let file = root.join(op["path"].as_str().unwrap());
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(file, op["text"].as_str().unwrap()).unwrap();
            }
            "remove" => {
                let target = root.join(op["path"].as_str().unwrap());
                if target.is_dir() {
                    fs::remove_dir_all(&target).unwrap();
                } else if target.exists() {
                    fs::remove_file(&target).unwrap();
                }
            }
            "clock" => state.lock().unwrap().now = op["now"].as_f64().unwrap(),
            "ring" => state.lock().unwrap().ring_fail = op["fail"].as_bool().unwrap(),
            "faults" => {
                let faults = &op["faults"];
                let mut s = state.lock().unwrap();
                if let Some(v) = faults.get("writeOnce") {
                    s.write_once = match_of(v);
                }
                if let Some(v) = faults.get("writeAlways") {
                    s.write_always = match_of(v);
                }
                if let Some(v) = faults.get("fsyncOnce") {
                    s.fsync_once = match_of(v);
                }
                if let Some(v) = faults.get("voidAlways") {
                    s.void_always = v.as_bool().unwrap();
                }
            }
            "submit" => {
                let entries = op["entries"].as_array().unwrap();
                let results = answers
                    .submit_and_ring(entries, op["device"].as_str().unwrap(), op["clientSendId"].as_str())
                    .unwrap_or_else(|_| panic!("{at}: submit failed"));
                assert_eq!(
                    js_stringify(&Json::Array(results)),
                    js_stringify(&op["expect"]),
                    "{at}: results"
                );
            }
            "retry" => answers.retry_pending(),
            "reopen" => answers = engine(&store, &root, &state),
            "check" => {
                checks += 1;
                let expect = &op["expect"];
                let journal = fs::read_to_string(store.join("answers.jsonl")).ok();
                assert_eq!(journal.as_deref(), expect["journal"].as_str(), "{at}: journal bytes");
                let files = share_files(&root);
                let want: BTreeMap<String, String> = expect["files"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                    .collect();
                assert_eq!(files, want, "{at}: share files");
                let rings = Json::Array(state.lock().unwrap().rings.clone());
                assert_eq!(js_stringify(&rings), js_stringify(&expect["rings"]), "{at}: rings");
                let entries: Vec<Json> = answers.list().unwrap().iter().map(|e| e.to_json()).collect();
                assert_eq!(
                    js_stringify(&Json::Array(entries)),
                    js_stringify(&expect["entries"]),
                    "{at}: history"
                );
                let sends: Vec<Json> = answers.send_list().unwrap().iter().map(|s| s.to_json()).collect();
                assert_eq!(
                    js_stringify(&Json::Array(sends)),
                    js_stringify(&expect["sends"]),
                    "{at}: sends"
                );
                let list: Vec<_> = answers.list().unwrap().to_vec();
                let views: Vec<Json> = list.iter().map(|e| delivery_view(e, answers.sends())).collect();
                assert_eq!(
                    js_stringify(&Json::Array(views)),
                    js_stringify(&expect["views"]),
                    "{at}: delivery views"
                );
            }
            other => panic!("unknown op {other}"),
        }
    }
    aac_status::writer_lock::release_writer_lock(&store);
    checks
}

#[test]
fn the_rust_store_replays_every_ts_script_byte_for_byte() {
    let vectors = vectors();
    let mut total = 0;
    for scenario in vectors["scenarios"].as_array().unwrap() {
        total += replay(scenario);
    }
    assert!(total >= 20, "{total} checks");
}
