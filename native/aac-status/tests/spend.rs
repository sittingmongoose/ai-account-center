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
    // The foot's "since" words name the calendar start quota-watch reports.
    assert!(view.periods[1].foot.contains(", since early Friday."));
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
