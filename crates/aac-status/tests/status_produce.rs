//! G16c parity: the Status feed's producers against the TypeScript vectors
//! (`tests/spec/status-produce-vectors.ts`, regenerated from the real TS code).
//!
//! The parsers (browser-check, Simlock, host probes, the busy percent), the answered-cards
//! merge and the six sources' content are compared with the recorded answers. The sources are
//! compared as JSON here (this crate's maps are key-sorted without `preserve_order`); the byte
//! comparison runs in `crates/aac-http/tests/status_produce_bytes.rs`, where the service build
//! keeps Node's key order. The facts' key order follows first-seen order on both sides; their
//! values are compared.

use aac_status::answers_engine::{HistoryEntry, StepState};
use aac_status::produce::answers_feed::{AnswersFeed, merge_shared};
use aac_status::produce::browsers::{self, BrowserMachine};
use aac_status::produce::facts::ZoneFacts;
use aac_status::produce::machines::{self, HostOs};
use aac_status::produce::simlock;
use aac_status::produce::sources::{self, Readings};
use aac_status::produce::usage::UsageInput;
use serde_json::{Value as Json, json};

fn vectors() -> Json {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/status-produce-vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("status-produce-vectors.json")).expect("vectors parse")
}

/// Both sides as `JSON.stringify` writes them: a JSON number that is a whole value writes the
/// same digits whether it was read as an integer or a binary64 (`aac_status::share::js_text`).
fn assert_same(label: &str, rust: Json, ts: &Json) {
    assert_eq!(
        aac_status::share::js_text(&rust),
        aac_status::share::js_text(ts),
        "{label}"
    );
}

fn entry(value: &Json) -> HistoryEntry {
    let state = |value: &Json| StepState {
        state: value["state"].as_str().unwrap_or("").to_owned(),
        at: value["at"].as_str().map(str::to_owned),
        detail: value["detail"].as_str().map(str::to_owned),
        attempts: u64::try_from(value["attempts"].as_u64().unwrap_or(0)).unwrap_or(0),
    };
    HistoryEntry {
        id: value["id"].as_str().unwrap_or("").to_owned(),
        send_id: value["sendId"].as_str().map(str::to_owned),
        shared_only: value["sharedOnly"] == json!(true),
        answer: value["answer"].clone(),
        card: value["card"].clone(),
        share: state(&value["share"]),
        doorbell: state(&value["doorbell"]),
    }
}

#[test]
fn browser_rows_parse_and_merge_like_the_collector() {
    let vectors = vectors();
    let case = &vectors["browsers"];
    let now = case["now"].as_i64().unwrap();
    let rows: Vec<BrowserMachine> = browsers::parse_slots(&case["slots"]).expect("slots parse");
    assert_same("rows", serde_json::to_value(&rows).unwrap(), &case["rows"]);
    let before = browsers::merge_rows(None, rows.clone(), now);
    assert_same("before", serde_json::to_value(&before).unwrap(), &case["before"]);
    let failed = browsers::parse_slots(&case["failed"]).expect("failed slots parse");
    let merged = browsers::merge_rows(Some(&before), failed, now + 1_000);
    assert_same("merged", serde_json::to_value(&merged).unwrap(), &case["merged"]);
    let noted = browsers::parse_slots(&case["noted"]).expect("noted slots parse");
    let noted = browsers::merge_rows(Some(&before), noted, now + 2_000);
    assert_same("problems", serde_json::to_value(&noted).unwrap(), &case["problems"]);
    assert_same(
        "problem list",
        serde_json::to_value(browsers::problems(&noted)).unwrap(),
        &json!([{ "item": "Ubuntu VM", "reason": "backend busy", "since": now + 2_000 }]),
    );
}

#[test]
fn simlock_reads_capacity_and_leases_like_the_collector() {
    let vectors = vectors();
    let status = simlock::parse_status(&vectors["simlock"]["input"]).expect("simlock parses");
    assert_same(
        "simlock",
        serde_json::to_value(&status).unwrap(),
        &vectors["simlock"]["expected"],
    );
}

#[test]
fn host_probes_and_the_busy_percent_match() {
    let vectors = vectors();
    for probe in vectors["machines"]["probes"].as_array().unwrap() {
        let host = HostOs::of(probe["host"].as_str().unwrap()).unwrap();
        let sample = machines::parse_probe(host, probe["stdout"].as_str().unwrap());
        assert_same(
            probe["host"].as_str().unwrap(),
            serde_json::to_value(&sample).unwrap(),
            &probe["expected"],
        );
    }
    for case in vectors["machines"]["busy"].as_array().unwrap() {
        let times = |value: &Json| {
            value.is_object().then(|| machines::CpuTimes {
                busy: value["busy"].as_i64().unwrap(),
                total: value["total"].as_i64().unwrap(),
            })
        };
        assert_eq!(
            machines::busy_between(times(&case["before"]), times(&case["after"])),
            case["expected"].as_i64()
        );
    }
}

#[test]
fn shared_answer_copies_fill_a_version_the_journal_has_not_closed() {
    let vectors = vectors();
    let local: Vec<HistoryEntry> = vectors["answersMerge"]["local"]
        .as_array()
        .unwrap()
        .iter()
        .map(entry)
        .collect();
    let shared: Vec<HistoryEntry> = vectors["answersMerge"]["shared"]
        .as_array()
        .unwrap()
        .iter()
        .map(entry)
        .collect();
    let merged = merge_shared(&local, &shared);
    let ids: Vec<&str> = merged.iter().map(|entry| entry.id.as_str()).collect();
    let expected: Vec<&str> = vectors["answersMerge"]["merged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, expected);
}

#[test]
fn the_six_sources_carry_the_same_content_as_the_front_door() {
    let vectors = vectors();
    let input = &vectors["input"];
    let zone = &vectors["zone"];
    let facts = ZoneFacts {
        from: zone["from"].as_i64().unwrap(),
        to: zone["to"].as_i64().unwrap(),
        offsets: zone["offsets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| [row[0].as_i64().unwrap(), row[1].as_i64().unwrap()])
            .collect(),
    };
    let zone = facts.zone().expect("the fixture zone proves");
    let now = vectors["now"].as_i64().unwrap();
    let answers = AnswersFeed {
        answers: input["answers"].as_array().unwrap().iter().map(entry).collect(),
        sends: Vec::new(),
        problem: input["answerHistoryProblem"].as_str().map(str::to_owned),
    };
    let usage = (!input["usage"].is_null()).then(|| UsageInput::parse(&input["usage"]).unwrap());
    let built = sources::build(&Readings {
        reports: &input["reports"],
        quota: &input["quota"],
        pinned: &input["pinned"],
        registry: &input["registry"],
        machines: &input["machines"],
        browsers: &input["browsers"],
        simlock: &input["simlock"],
        usage: usage.as_ref(),
        answers: &answers,
        time_zone: vectors["timeZone"].as_str().unwrap(),
        now,
        offset_minutes: zone.offset_minutes(now).expect("offset at now"),
        zone: &zone,
    })
    .expect("the fixture builds");
    let recorded = &vectors["sources"];
    for (source, text) in [
        ("status-reports", &built.reports),
        ("status-quota", &built.quota),
        ("status-pinned", &built.pinned),
        ("status-collected", &built.collected),
        ("status-answers", &built.answers),
        ("status-facts", &built.facts_data),
    ] {
        let rust: Json = serde_json::from_str(text).unwrap();
        let ts: Json = serde_json::from_str(recorded[source]["data"].as_str().unwrap()).unwrap();
        assert_same(source, rust, &ts);
    }
    let rust_facts: Json = serde_json::from_str(&built.facts).unwrap();
    let ts_facts: Json = serde_json::from_str(recorded["status-facts"]["facts"].as_str().unwrap()).unwrap();
    // The epochs and ranks are the whole set. Dates are a subset: an instant outside the
    // display zone's proved window keeps no word here (the reference's `Intl` writes one for
    // any instant).
    assert_same("epochs", rust_facts["epochs"].clone(), &ts_facts["epochs"]);
    assert_same("order", rust_facts["order"].clone(), &ts_facts["order"]);
    let rust_dates = rust_facts["dates"].as_object().unwrap();
    let ts_dates = ts_facts["dates"].as_object().unwrap();
    assert!(!rust_dates.is_empty());
    for (at, date) in rust_dates {
        assert_eq!(Some(date), ts_dates.get(at), "date {at}");
    }
}
