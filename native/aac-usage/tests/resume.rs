//! DESIGN 16.1: a compressed Codex log that is one zstd frame of several GB is read across calls,
//! each resuming at the decoded line offset the last one reached, with nothing counted twice.
//! The cuts come from a test-only line budget, so they fall at fixed points.

use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const SESSION: &str = "01a19f7c-b191-7a42-9bd9-c0b3dd7f3f33";
const NAME: &str = "rollout-2026-11-03T03-38-31-01a19f7c-b191-7a42-9bd9-c0b3dd7f3f33.jsonl.zst";

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

fn request() -> Request {
    protocol::parse_request(
        json!({"kinds": ["codex"], "minDateMs": 1_790_000_000_000u64, "deadlineMs": 60_000})
            .to_string()
            .as_bytes(),
    )
    .unwrap()
}

/// One Codex session: its header, then `turns` token counts (cumulative), each after a filler line
/// of `filler` bytes standing in for the conversation, ten seconds apart.
fn lines(turns: u64, filler: usize, model: &str) -> impl Iterator<Item = String> + '_ {
    let head = vec![
        json!({"timestamp": "2026-11-03T03:38:31Z", "type": "session_meta",
               "payload": {"id": SESSION, "model_provider": "openai", "cwd": "/home/u/proj"}})
        .to_string(),
        json!({"timestamp": "2026-11-03T03:38:31Z", "type": "turn_context", "payload": {"model": model}}).to_string(),
    ];
    let pad = "x".repeat(filler);
    head.into_iter().chain((1..=turns).flat_map(move |turn| {
        let at = 1_793_677_111_000i64 + (turn as i64) * 10_000;
        let stamp = chrono_like(at);
        [
            json!({"timestamp": stamp, "type": "response_item", "payload": {"type": "message", "content": pad}})
                .to_string(),
            json!({"timestamp": stamp, "type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": turn * 1000, "cached_input_tokens": turn * 900,
                                      "output_tokens": turn * 10, "reasoning_output_tokens": 0},
                "last_token_usage": {"input_tokens": 1000, "cached_input_tokens": 900,
                                     "output_tokens": 10, "reasoning_output_tokens": 0}}}})
            .to_string(),
        ]
    }))
}

/// ISO 8601 UTC with milliseconds, without a date crate.
fn chrono_like(epoch_ms: i64) -> String {
    let secs = epoch_ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        epoch_ms.rem_euclid(1000)
    )
}

/// A single-frame zstd log, as Codex writes them; `pledged` puts the decoded size in its header.
fn write_log(home: &Path, turns: u64, filler: usize, model: &str, pledged: bool) -> (PathBuf, u64) {
    let path = home.join(".codex/sessions/2026/11/03").join(NAME);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let decoded: u64 = lines(turns, filler, model).map(|l| l.len() as u64 + 1).sum();
    let mut encoder = zstd::stream::write::Encoder::new(fs::File::create(&path).unwrap(), 1).unwrap();
    if pledged {
        encoder.set_pledged_src_size(Some(decoded)).unwrap();
    }
    for line in lines(turns, filler, model) {
        encoder.write_all(line.as_bytes()).unwrap();
        encoder.write_all(b"\n").unwrap();
    }
    encoder.finish().unwrap();
    (path, decoded)
}

#[derive(Default, Debug, PartialEq)]
struct Totals {
    hours: BTreeMap<(String, String), [u64; 5]>,
    sessions: [u64; 5],
}

fn add(totals: &mut Totals, answer: &Value) {
    for row in answer["rows"].as_array().unwrap() {
        let cell = totals
            .hours
            .entry((row["h"].as_str().unwrap().into(), row["m"].as_str().unwrap().into()))
            .or_default();
        for (i, key) in ["i", "o", "cr", "cw", "n"].iter().enumerate() {
            cell[i] += row[*key].as_u64().unwrap();
        }
    }
    for row in answer["srows"].as_array().unwrap() {
        for (i, key) in ["i", "o", "cr", "cw", "n"].iter().enumerate() {
            totals.sessions[i] += row[*key].as_u64().unwrap();
        }
    }
}

struct Calls {
    totals: Totals,
    calls: usize,
    unfinished: Vec<Value>,
    prints: Map<String, Value>,
}

/// Call the scanner until the log is read to its end, merging as the server does: an answer whose
/// print has `readFrom` adds to the file's rows, any other replaces them.
fn read_in_calls(home: &Path, line_budget: Option<u64>, budget: f64, mut prints: Map<String, Value>) -> Calls {
    let mut totals = Totals::default();
    let mut unfinished = Vec::new();
    for calls in 1..=100_000 {
        let mut ask = request();
        ask.fingerprints = json!({ "codex": prints }).as_object().unwrap().clone();
        ask.line_budget = line_budget;
        ask.budget = budget;
        let answer = protocol::scan(&ask, home, &env(home));
        // The saved place carries no identifier: the raw session id never leaves the computer.
        assert!(!answer.to_string().contains(SESSION));
        let fresh = answer["kinds"]["codex"]["fingerprints"].as_object().unwrap().clone();
        for (key, print) in &fresh {
            match print.get("readFrom").and_then(Value::as_u64) {
                Some(from) => assert_eq!(
                    prints.get(key).and_then(|p| p["readThrough"].as_u64()),
                    Some(from),
                    "a resumed answer starts where the last one stopped"
                ),
                None => totals = Totals::default(),
            }
            prints.insert(key.clone(), print.clone());
        }
        add(&mut totals, &answer);
        if let Some(list) = answer["kinds"]["codex"]["unfinished"].as_array() {
            unfinished.extend(list.iter().cloned());
        }
        if !fresh.is_empty() && fresh.values().all(|p| p.get("readThrough").is_none()) {
            return Calls {
                totals,
                calls,
                unfinished,
                prints,
            };
        }
    }
    panic!("the log was never read to its end");
}

#[test]
fn a_single_frame_log_is_read_across_calls_without_counting_twice() {
    let home = tempfile::tempdir().unwrap();
    let (_, decoded) = write_log(home.path(), 600, 2_000, "gpt-6.1-sol", true);
    let whole = read_in_calls(home.path(), None, 60.0, Map::new());
    assert_eq!(whole.calls, 1);
    assert_eq!(whole.totals.sessions[4], 600);

    let parts = read_in_calls(home.path(), Some(97), 60.0, Map::new());
    assert!(parts.calls > 10, "cut into many calls: {}", parts.calls);
    assert_eq!(parts.totals, whole.totals);

    // While part-read, the log is named without its conversation id, with how far it got.
    let first = &parts.unfinished[0];
    assert_eq!(first["name"], "rollout-2026-11-03T03-38-31.jsonl.zst");
    assert_eq!(first["decodedBytes"], decoded);
    assert_eq!(first["reason"], "deadline");
    let throughs: Vec<u64> = parts
        .unfinished
        .iter()
        .map(|u| u["readThroughBytes"].as_u64().unwrap())
        .collect();
    assert!(throughs.windows(2).all(|w| w[0] < w[1]), "each call reads further");
    assert!(!parts.unfinished.iter().any(|u| u.to_string().contains(SESSION)));
}

#[test]
fn a_plain_log_resumes_by_seeking() {
    let home = tempfile::tempdir().unwrap();
    let path = home
        .path()
        .join(".codex/sessions/2026/11/03")
        .join(NAME.trim_end_matches(".zst"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines(300, 500, "gpt-6.1-sol").map(|l| l + "\n").collect();
    fs::write(&path, text).unwrap();
    let whole = read_in_calls(home.path(), None, 60.0, Map::new());
    let parts = read_in_calls(home.path(), Some(41), 60.0, Map::new());
    assert!(parts.calls > 10);
    assert_eq!(parts.totals, whole.totals);
}

#[test]
fn a_log_that_changed_after_a_partial_read_is_read_again_whole() {
    let home = tempfile::tempdir().unwrap();
    write_log(home.path(), 200, 1_000, "gpt-6.1-sol", true);
    let mut ask = request();
    ask.line_budget = Some(50);
    let answer = protocol::scan(&ask, home.path(), &env(home.path()));
    let prints = answer["kinds"]["codex"]["fingerprints"].as_object().unwrap().clone();
    assert!(prints.values().any(|p| p.get("readThrough").is_some()));
    // Rewritten (a different model, so a different size and head): the old place means nothing.
    write_log(home.path(), 210, 1_000, "gpt-6.2-sol", true);
    let mut again = request();
    again.fingerprints = json!({ "codex": prints }).as_object().unwrap().clone();
    let answer = protocol::scan(&again, home.path(), &env(home.path()));
    let fresh = answer["kinds"]["codex"]["fingerprints"].as_object().unwrap();
    assert!(
        fresh
            .values()
            .all(|p| p.get("readFrom").is_none() && p.get("readThrough").is_none())
    );
    assert!(
        answer["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["m"] == "gpt-6.2-sol")
    );
}

#[test]
fn a_saved_place_past_the_end_reads_the_log_whole() {
    let home = tempfile::tempdir().unwrap();
    write_log(home.path(), 100, 500, "gpt-6.1-sol", true);
    let whole = read_in_calls(home.path(), None, 60.0, Map::new());
    // A print that matches the file but claims a place past its decoded end.
    let mut prints = whole.prints.clone();
    for print in prints.values_mut() {
        print["readThrough"] = json!(1u64 << 40);
        print["readState"] = json!({});
    }
    let again = read_in_calls(home.path(), None, 60.0, prints);
    assert_eq!(again.totals, whole.totals);
}

#[test]
fn a_saved_place_for_another_session_reads_the_log_whole() {
    let home = tempfile::tempdir().unwrap();
    write_log(home.path(), 120, 500, "gpt-6.1-sol", true);
    let mut ask = request();
    ask.line_budget = Some(60);
    let answer = protocol::scan(&ask, home.path(), &env(home.path()));
    let mut prints = answer["kinds"]["codex"]["fingerprints"].as_object().unwrap().clone();
    for print in prints.values_mut() {
        assert!(print["readState"]["sessionDigest"].is_string());
        print["readState"]["sessionDigest"] = json!("0000000000000000");
    }
    let whole = read_in_calls(home.path(), None, 60.0, Map::new());
    let again = read_in_calls(home.path(), None, 60.0, prints);
    assert_eq!(again.calls, 1);
    assert_eq!(again.totals, whole.totals);
}

#[test]
fn display_names_drop_conversation_ids() {
    assert_eq!(
        aac_usage::util::display_name(Path::new(NAME)),
        "rollout-2026-11-03T03-38-31.jsonl.zst"
    );
    assert_eq!(
        aac_usage::util::display_name(Path::new(
            "2026-09-26T05-39-12-615Z_01a0dc39-8667-7224-b0cb-6f3aaeabc153.jsonl"
        )),
        "2026-09-26T05-39-12-615Z.jsonl"
    );
}

/// Several GB decoded in one frame: read whole with no size cap, then cut every 4,001 lines so each
/// call decodes and discards up to 3 GB before reading on. `cargo test --release -- --ignored`
/// (Nas1: 1.7 s whole, 15 calls in 3.4 s cut).
#[test]
#[ignore]
fn a_multi_gigabyte_single_frame_log_is_read_to_its_end() {
    let home = tempfile::tempdir().unwrap();
    // 30,000 turns of 100 KB filler: about 3 GB decoded, a few MB on disk.
    let (path, decoded) = write_log(home.path(), 30_000, 100_000, "gpt-6.1-sol", false);
    assert!(decoded > 3_000_000_000, "decoded {decoded}");
    assert!(fs::metadata(&path).unwrap().len() < 200_000_000);
    // Read whole, with no size cap.
    let started = std::time::Instant::now();
    let whole = read_in_calls(home.path(), None, 60.0, Map::new());
    assert_eq!(whole.totals.sessions[4], 30_000);
    assert_eq!(whole.totals.sessions[0], 30_000 * 100);
    eprintln!(
        "whole: {} call(s), {:.1} s, {decoded} bytes decoded",
        whole.calls,
        started.elapsed().as_secs_f64()
    );
    // Cut every 4,001 lines: each call decodes and discards a prefix of up to 3 GB, then reads on.
    let started = std::time::Instant::now();
    let parts = read_in_calls(home.path(), Some(4_001), 60.0, Map::new());
    assert!(parts.calls > 10);
    assert_eq!(parts.totals, whole.totals);
    let deepest = parts
        .unfinished
        .iter()
        .filter_map(|u| u["readThroughBytes"].as_u64())
        .max()
        .unwrap();
    assert!(deepest > 2_500_000_000, "resumed past 2.5 GB: {deepest}");
    eprintln!(
        "resumed: {} calls, {:.1} s, deepest place {deepest}",
        parts.calls,
        started.elapsed().as_secs_f64()
    );
}
