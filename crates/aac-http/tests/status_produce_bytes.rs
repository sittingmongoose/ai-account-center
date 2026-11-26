//! G16c: the six fed Status generations, byte for byte against the front door's.
//!
//! The TypeScript vectors (`tests/spec/status-produce-vectors.json`, written by
//! `tests/spec/status-produce-vectors.ts`) hold the real `answerStatusFed` pushes for one fixture
//! input. This build has `serde_json`'s `preserve_order` (the service's own feature set), so the
//! source texts can be compared as bytes: the member order, every number and every string is
//! Node's. The facts are compared as JSON for the three maps (an instant outside the display
//! zone's proved window keeps no date word here, while the reference's `Intl` writes one for any
//! instant); their epochs and ranks are the whole set.

#![allow(clippy::unwrap_used)]

use aac_status::answers_engine::{HistoryEntry, StepState};
use aac_status::produce::answers_feed::AnswersFeed;
use aac_status::produce::facts::ZoneFacts;
use aac_status::produce::sources::{self, Readings};
use aac_status::produce::usage::UsageInput;
use serde_json::Value as Json;

fn vectors() -> Json {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/status-produce-vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("status-produce-vectors.json"))
        .expect("vectors parse")
}

fn entry(value: &Json) -> HistoryEntry {
    let state = |value: &Json| StepState {
        state: value["state"].as_str().unwrap_or("").to_owned(),
        at: value["at"].as_str().map(str::to_owned),
        detail: value["detail"].as_str().map(str::to_owned),
        attempts: value["attempts"].as_u64().unwrap_or(0),
    };
    HistoryEntry {
        id: value["id"].as_str().unwrap_or("").to_owned(),
        send_id: value["sendId"].as_str().map(str::to_owned),
        shared_only: value["sharedOnly"] == true,
        answer: value["answer"].clone(),
        card: value["card"].clone(),
        share: state(&value["share"]),
        doorbell: state(&value["doorbell"]),
    }
}

#[test]
fn the_six_generations_are_the_front_doors_bytes() {
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
        answers: input["answers"]
            .as_array()
            .unwrap()
            .iter()
            .map(entry)
            .collect(),
        sends: Vec::new(),
        problem: input["answerHistoryProblem"].as_str().map(str::to_owned),
    };
    let usage = (!input["usage"].is_null()).then(|| UsageInput::parse(&input["usage"]).unwrap());
    let built = sources::build(&Readings {
        reports: sources::Reports::Wire(&input["reports"]),
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
        let expected = recorded[source]["data"].as_str().unwrap();
        assert_eq!(text, expected, "{source}");
    }
    let rust_facts: Json = serde_json::from_str(&built.facts).unwrap();
    let ts_facts: Json =
        serde_json::from_str(recorded["status-facts"]["facts"].as_str().unwrap()).unwrap();
    assert_eq!(
        aac_status::share::js_text(&rust_facts["epochs"]),
        aac_status::share::js_text(&ts_facts["epochs"])
    );
    assert_eq!(
        aac_status::share::js_text(&rust_facts["order"]),
        aac_status::share::js_text(&ts_facts["order"])
    );
    let rust_dates = rust_facts["dates"].as_object().unwrap();
    let ts_dates = ts_facts["dates"].as_object().unwrap();
    assert!(!rust_dates.is_empty());
    for (at, date) in rust_dates {
        assert_eq!(Some(date), ts_dates.get(at), "date {at}");
    }
}
