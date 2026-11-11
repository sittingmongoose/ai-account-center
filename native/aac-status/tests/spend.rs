//! The spend-by-stream composer's words: the golden pair from the Leptos page builder's
//! contract, then every unknown-and-unavailable case the contract names, so none of them
//! produces a guessed figure. No share, no clock: every test passes `nowMs` itself.

use aac_status::ops;
use aac_status::spend::{SpendInput, UNAVAILABLE_SENTENCE, compose};
use serde_json::{Value, json};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// JSON equality that treats 1 and 1.0 as the same number, as the page's JSON does.
fn assert_same_json(actual: &Value, expected: &Value) {
    fn same(actual: &Value, expected: &Value) -> bool {
        match (actual, expected) {
            (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => x == y,
                _ => a == b,
            },
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
            }
            (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y)),
            _ => actual == expected,
        }
    }
    if !same(actual, expected) {
        panic!(
            "golden mismatch\nexpected: {expected}\nactual:   {actual}\nfirst difference: {}",
            first_difference(expected, actual)
        );
    }
}

fn first_difference(expected: &Value, actual: &Value) -> String {
    fn walk(path: &str, expected: &Value, actual: &Value, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        match (expected, actual) {
            (Value::Object(a), Value::Object(b)) => {
                for (key, value) in a {
                    match b.get(key) {
                        Some(other) => walk(&format!("{path}.{key}"), value, other, out),
                        None => {
                            *out = Some(format!("{path}.{key}: missing in actual"));
                            return;
                        }
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                for (at, (x, y)) in a.iter().zip(b).enumerate() {
                    walk(&format!("{path}[{at}]"), x, y, out);
                    if out.is_some() {
                        return;
                    }
                }
            }
            _ if expected != actual => *out = Some(format!("{path}: expected {expected}, actual {actual}")),
            _ => {}
        }
    }
    let mut out = None;
    walk("$", expected, actual, &mut out);
    out.unwrap_or_else(|| "values differ structurally".to_string())
}

fn golden_input() -> SpendInput {
    serde_json::from_str(&fixture("spend-input.json")).unwrap()
}

#[test]
fn the_golden_pair_reproduces_exactly() {
    let input = golden_input();
    let expected: Value = serde_json::from_str(&fixture("spend-by-stream.json")).unwrap();
    let actual = serde_json::to_value(compose(&input)).unwrap();
    assert_same_json(&actual, &expected);
}

#[test]
fn the_golden_pair_through_the_dispatch_too() {
    let answer = ops::dispatch("composeSpend", &fixture("spend-input.json")).unwrap();
    let expected: Value = serde_json::from_str(&fixture("spend-by-stream.json")).unwrap();
    assert_same_json(&serde_json::from_str::<Value>(&answer).unwrap(), &expected);
}

// One period, one stream helpers: the edges below speak over a small view, not the golden.

fn view_of(spend: Value) -> SpendInput {
    let mut merged = json!({
        "nowMs": 1_794_247_320_000i64,
        "utcOffsetMinutes": -300
    });
    merged.as_object_mut().unwrap().insert("spend".to_string(), spend);
    serde_json::from_value(merged).unwrap()
}

fn spend_doc(periods: Value, machines: Value) -> Value {
    json!({
        "asOf": null, "methodId": null, "method": null,
        "periods": periods,
        "budgets": [], "unattributed": [], "machinesNotScanned": machines
    })
}

fn week(streams: Value) -> Value {
    json!({
        "id": "week", "label": "This week", "start": "2026-11-06T09:00:00.000Z", "end": null,
        "coverage": null, "streams": streams
    })
}

fn stream(stream: &str, label: &str, bucket: bool, rows: Value) -> Value {
    json!({ "stream": stream, "label": label, "bucket": bucket, "rows": rows })
}

fn row(account: &str, meter: &str, value: Value) -> Value {
    let unit = if meter == "credits_balance" {
        "credits"
    } else {
        "percent"
    };
    json!({ "account": account, "meter": meter, "meterLabel": meter, "unit": unit, "value": value, "valueText": "", "estimate": true })
}

#[test]
fn no_periods_is_the_unavailable_shape() {
    let view = compose(&view_of(spend_doc(json!([]), json!([]))));
    assert_eq!(view.state, "unavailable");
    assert_eq!(view.unavailable.as_deref(), Some(UNAVAILABLE_SENTENCE));
    assert_eq!(view.default_period, None);
    assert_eq!(view.as_of, None);
    assert_eq!(view.as_of_text, None);
    assert!(view.periods.is_empty());
    // The exact shape, as the route serves it.
    assert_same_json(
        &serde_json::to_value(&view).unwrap(),
        &json!({
            "state": "unavailable",
            "unavailable": UNAVAILABLE_SENTENCE,
            "defaultPeriod": null,
            "asOf": null,
            "asOfText": null,
            "periods": []
        }),
    );
}

#[test]
fn a_null_value_reads_not_read_never_zero() {
    // The two rows share a column: the read part shows, prefixed as a lower bound.
    let mixed = json!([stream(
        "only",
        "only",
        false,
        json!([
            row("claude:me", "seven_day", Value::Null),
            row("claude:me", "seven_day", json!(12.0))
        ])
    )]);
    let view = compose(&view_of(spend_doc(json!([week(mixed)]), json!([]))));
    assert_eq!(view.periods[0].rows[0].cells[0].as_ref().unwrap().text, "≥12%");
    assert_eq!(view.periods[0].rows[0].cells[0].as_ref().unwrap().share, Some(0.12));

    // A stream whose only row is unread says so, with a null share, and no stream ranks.
    let unread = json!([stream(
        "only",
        "only",
        false,
        json!([row("claude:me", "seven_day", Value::Null)])
    )]);
    let view = compose(&view_of(spend_doc(json!([week(unread)]), json!([]))));
    let cell = view.periods[0].rows[0].cells[0].as_ref().unwrap();
    assert_eq!(cell.share, None);
    assert_eq!(cell.text, "not read");
    assert_eq!(
        view.periods[0].lead[0].text,
        "quota-watch has not tied any use to a stream this week."
    );
}

#[test]
fn a_bucket_with_a_null_part_keeps_the_read_parts_with_a_lower_bound() {
    let bucket_stream = json!([stream(
        "(no stream)",
        "(no stream)",
        true,
        json!([
            row("claude:me", "seven_day", Value::Null),
            row("claude:me", "seven_day", Value::Null),
            row("claude:me", "five_hour", json!(210.0))
        ])
    )]);
    let view = compose(&view_of(spend_doc(json!([week(bucket_stream)]), json!([]))));
    let bucket = &view.periods[0].rows[0];
    assert!(bucket.bucket);
    assert_eq!(bucket.stream, "Not tied to a stream");
    assert_eq!(bucket.label, "sessions no stream rule claims");
    // All parts of the weekly column unread: "not read", never a guessed 0.
    assert_eq!(bucket.cells[0].as_ref().unwrap().text, "not read");
    // The 5-hour part reads: 210 points is 2.1 full windows.
    assert_eq!(bucket.cells[1].as_ref().unwrap().text, "2.1×");
    assert_eq!(view.periods[0].more.as_deref(), Some("Use not tied to a stream"));
}

#[test]
fn one_machine_misses_its_own_sessions() {
    let machines = json!([
        { "name": "nas1", "thisPass": true, "error": null, "runsMissed": 6, "runsInWindow": 37 },
        { "name": "mac", "thisPass": false, "error": null, "runsMissed": 1, "runsInWindow": 37 }
    ]);
    let one_stream = json!([stream(
        "only",
        "only",
        false,
        json!([row("claude:me", "seven_day", json!(12.0))])
    )]);
    let one = compose(&view_of(spend_doc(
        json!([week(one_stream.clone())]),
        json!([{ "name": "nas1", "thisPass": true, "error": null, "runsMissed": 6, "runsInWindow": 37 }]),
    )));
    assert!(
        one.periods[0]
            .foot
            .contains("Nas1 missed 6 of 37 scans, so some of its sessions are missing."),
        "foot was: {}",
        one.periods[0].foot
    );
    let two = compose(&view_of(spend_doc(json!([week(one_stream.clone())]), machines)));
    assert!(
        two.periods[0]
            .foot
            .contains("Nas1 missed 6 of 37 scans and the Mac 1, so some of their sessions are missing."),
        "foot was: {}",
        two.periods[0].foot
    );
    // A machine with no missed runs is not named at all.
    let none = compose(&view_of(spend_doc(
        json!([week(one_stream)]),
        json!([{ "name": "nas1", "thisPass": true, "error": null, "runsMissed": 0, "runsInWindow": 37 }]),
    )));
    assert!(!none.periods[0].foot.contains("Nas1 missed"));
}

#[test]
fn a_five_hour_cell_under_one_is_a_percent_and_a_weekly_cell_above_one_stays_one() {
    let streams = json!([
        stream(
            "under",
            "under",
            false,
            json!([row("claude:me", "five_hour", json!(80.0))])
        ),
        stream(
            "over",
            "over",
            false,
            json!([row("claude:team", "seven_day", json!(104.0))])
        ),
        stream(
            "windows",
            "windows",
            false,
            json!([row("claude:gmail", "five_hour", json!(324.8))])
        )
    ]);
    let view = compose(&view_of(spend_doc(json!([week(streams)]), json!([]))));
    let rows = &view.periods[0].rows;
    // Columns: Claude weekly first, then the 5-hour ones by account (team, gmail, me);
    // rows by peak share: windows (3.2), over (104%), under (80%).
    assert_eq!(rows[2].cells[2].as_ref().unwrap().text, "80%"); // under 1.0: a percent
    assert_eq!(rows[1].cells[0].as_ref().unwrap().text, "104%"); // over 1.0 weekly: still a percent
    assert_eq!(rows[0].cells[1].as_ref().unwrap().text, "3.2×"); // at or above 1.0: windows
    assert!(
        view.periods[0]
            .key
            .contains(&"3.2× means 3.2 full 5-hour windows.".to_string())
    );
}

#[test]
fn an_unlabelled_stream_has_no_sub_label_and_a_marked_lone_thread_keeps_its_phrase() {
    // D2a: a stream the reports never labelled has `label` equal to its id, and gets no
    // sub-label at all — the page draws nothing for it. A label the input genuinely
    // carries, including the phrase for a lone thread, is kept verbatim.
    let streams = json!([
        stream("E", "E", false, json!([row("claude:me", "seven_day", json!(20.0))])),
        stream(
            "solo",
            "a thread with no stream",
            false,
            json!([row("claude:me", "seven_day", json!(10.0))])
        )
    ]);
    let view = compose(&view_of(spend_doc(json!([week(streams)]), json!([]))));
    let rows = &view.periods[0].rows;
    // Rows by peak share: E (20%) before the lone thread (10%).
    assert_eq!(rows[0].stream, "E");
    assert_eq!(rows[0].label, "");
    assert_eq!(rows[1].stream, "solo");
    assert_eq!(rows[1].label, "a thread with no stream");
}

#[test]
fn one_non_bucket_row_gives_no_next_sentence() {
    let only = json!([stream(
        "only",
        "only",
        false,
        json!([row("claude:me", "seven_day", json!(12.0))])
    )]);
    let view = compose(&view_of(spend_doc(json!([week(only)]), json!([]))));
    let lead: Vec<&str> = view.periods[0].lead.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        lead,
        vec![
            "This week ",
            "only",
            " used the most: about ",
            "12%",
            " of Claude me’s weekly limit",
            "."
        ]
    );
    assert!(view.periods[0].more.is_none());
}

#[test]
fn the_week_is_the_default_period_and_the_clock_reads_the_dashboard_zone() {
    let view = compose(&golden_input());
    assert_eq!(view.default_period.as_deref(), Some("week"));
    assert_eq!(view.as_of.as_deref(), Some("2026-11-08T18:02:00.000Z"));
    assert_eq!(view.as_of_text.as_deref(), Some("Read 1:02 PM"));
    // The week foot names how much was watched. It does not name the period start:
    // that timestamp is the calendar boundary, not the first reading.
    let week = view.periods.iter().find(|period| period.id == "week").unwrap();
    assert!(week.foot.contains("quota-watch has watched 36% of this week."));
    assert!(!week.foot.contains("since"), "foot was: {}", week.foot);
}

#[test]
fn unknown_fields_are_ignored_and_a_missing_required_field_refuses() {
    // Unknown fields anywhere change nothing.
    let mut padded: Value = serde_json::from_str(&fixture("spend-input.json")).unwrap();
    padded
        .as_object_mut()
        .unwrap()
        .insert("tomorrow".to_string(), json!({ "a": [1] }));
    padded["spend"]["novelty"] = json!("unknown");
    padded["spend"]["periods"][0]["streams"][0]["rows"][0]["novelty"] = json!(true);
    let answer = ops::dispatch("composeSpend", &padded.to_string()).unwrap();
    let expected: Value = serde_json::from_str(&fixture("spend-by-stream.json")).unwrap();
    assert_same_json(&serde_json::from_str::<Value>(&answer).unwrap(), &expected);

    // A missing required field (a row without its meter, a spend without its budgets)
    // rejects the call: the route then answers the unavailable shape, never a guess.
    let mut no_meter: Value = serde_json::from_str(&fixture("spend-input.json")).unwrap();
    no_meter["spend"]["periods"][0]["streams"][0]["rows"][0]
        .as_object_mut()
        .unwrap()
        .remove("meter");
    assert!(ops::dispatch("composeSpend", &no_meter.to_string()).is_err());
    let mut no_budgets: Value = serde_json::from_str(&fixture("spend-input.json")).unwrap();
    no_budgets["spend"].as_object_mut().unwrap().remove("budgets");
    assert!(ops::dispatch("composeSpend", &no_budgets.to_string()).is_err());
    assert!(ops::dispatch("composeSpend", "not json").is_err());
}

// The reviewer's repro inputs (2026-11-09 spend/byModel recheck). Each one failed on
// 7c54292b6 and is locked here.

fn repro_input(name: &str) -> SpendInput {
    serde_json::from_str(&fixture(&format!("repro/{name}"))).unwrap()
}

fn assert_unavailable(view: &aac_status::spend::SpendByStream) {
    assert_eq!(view.state, "unavailable");
    assert_eq!(view.unavailable.as_deref(), Some(UNAVAILABLE_SENTENCE));
    assert!(view.periods.is_empty());
    assert_eq!(view.default_period, None);
    assert_eq!(view.as_of, None);
    assert_eq!(view.as_of_text, None);
}

#[test]
fn a_non_percent_meter_is_unknown_not_zero_percent() {
    // repro/credits-meter.json: 30 credits on weekly_credits, no percentage. A1.
    let raw = fixture("repro/credits-meter.json");
    let view = compose(&repro_input("credits-meter.json"));
    assert_unavailable(&view);
    let text = serde_json::to_string(&view).unwrap();
    assert!(!text.contains("0.0%"), "{text}");
    assert!(!text.contains("\"share\":0"), "{text}");
    let answer = ops::dispatch("composeSpend", &raw).unwrap();
    let parsed: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(parsed["state"], "unavailable");
}

#[test]
fn a_finite_value_past_the_safe_integer_range_is_unavailable() {
    // repro/large-finite.json: a percent cell of 1e30. A4. No saturated i64.
    let view = compose(&repro_input("large-finite.json"));
    assert_unavailable(&view);
    let text = serde_json::to_string(&view).unwrap();
    assert!(!text.contains("9223372036854775807"), "{text}");

    // Two in-range percents whose sum is past 2^53 are the same refusal.
    let summed = json!([stream(
        "only",
        "only",
        false,
        json!([
            row("claude:me", "seven_day", json!(6_000_000_000_000_000.0)),
            row("claude:me", "seven_day", json!(6_000_000_000_000_000.0))
        ])
    )]);
    let summed = compose(&view_of(spend_doc(json!([week(summed)]), json!([]))));
    assert_unavailable(&summed);
}

#[test]
fn equal_shares_tie_by_unicode_code_point() {
    // repro/collation.json: Zebra and alpha used the same share. Code points put 'Z'
    // before 'a'. The old reference's localeCompare put alpha first.
    let view = compose(&repro_input("collation.json"));
    let rows = &view.periods[0].rows;
    assert_eq!(rows[0].stream, "Zebra");
    assert_eq!(rows[1].stream, "alpha");
    let named: Vec<&str> = view.periods[0]
        .lead
        .iter()
        .filter(|run| run.strong == Some(true))
        .map(|run| run.text.as_str())
        .collect();
    assert_eq!(named.first().copied(), Some("Zebra"));
    assert!(named.contains(&"alpha"));

    // The same tie past the BMP. U+E000 is one UTF-16 unit; U+10000 is a surrogate
    // pair, so JavaScript `<` puts U+10000 first. Code points put U+E000 first. R3.
    let supplementary = compose(&repro_input("unicode-codepoints.json"));
    let streams: Vec<&str> = supplementary.periods[0]
        .rows
        .iter()
        .map(|row| row.stream.as_str())
        .collect();
    assert_eq!(streams, vec!["\u{E000}", "\u{10000}"]);
    let named: Vec<&str> = supplementary.periods[0]
        .lead
        .iter()
        .filter(|run| run.strong == Some(true))
        .map(|run| run.text.as_str())
        .collect();
    // The lead names the stream ids. U+E000 leads; U+10000 is next.
    assert_eq!(named.first().copied(), Some("\u{E000}"));
    assert!(named.contains(&"\u{10000}"));
}

#[test]
fn a_minute_precision_iso_time_is_a_real_clock_reading() {
    // repro/minute-iso.json's week start is 2026-11-02T00:00Z. The old parser required
    // seconds and left the clock blank. Seconds count as zero, in Z and offset forms. A6.
    let raw: Value = serde_json::from_str(&fixture("repro/minute-iso.json")).unwrap();
    assert_eq!(raw["spend"]["periods"][1]["start"], "2026-11-02T00:00Z");
    for (as_of, expected) in [
        ("2026-11-02T00:00Z", "Read 7:00 PM"),
        ("2026-11-02T00:00-05:00", "Read 12:00 AM"),
        ("2026-11-02T00:00+00:00", "Read 7:00 PM"),
    ] {
        let mut doc = raw.clone();
        doc["spend"]["asOf"] = json!(as_of);
        let view = compose(&serde_json::from_value(doc).unwrap());
        assert_eq!(view.state, "ok", "{as_of}");
        assert_eq!(view.as_of_text.as_deref(), Some(expected), "{as_of}");
    }
}

#[test]
fn the_coverage_sentence_does_not_name_a_calendar_start() {
    // The same minute-iso week, with the seconds the old parser needed so it would
    // attach "since Sunday evening". That start is quota-watch's calendar boundary,
    // not its first reading, so the sentence names only the share watched.
    let mut doc: Value = serde_json::from_str(&fixture("repro/minute-iso.json")).unwrap();
    let week = doc["spend"]["periods"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|period| period["id"] == "week")
        .unwrap();
    week["start"] = json!("2026-11-02T00:00:00Z");
    let view = compose(&serde_json::from_value(doc).unwrap());
    let foot = &view.periods.iter().find(|period| period.id == "week").unwrap().foot;
    assert!(foot.contains("quota-watch has watched 36% of this week."), "{foot}");
    assert!(!foot.contains("since"), "{foot}");
}
