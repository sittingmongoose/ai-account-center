//! The Scoreboard by-model composer's words: the golden pair from the Leptos page
//! builder's contract, then the cases the contract names (no effort, an unknown runtime,
//! a job with nothing but not-enough-data rows) and the schema-drift refusals.

use aac_status::ops;
use aac_status::scoreboard::{ByModel, ViewIn, by_model};
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

fn golden_view() -> ViewIn {
    serde_json::from_str(&fixture("scoreboard-view.json")).unwrap()
}

#[test]
fn the_golden_pair_reproduces_exactly() {
    let expected: Value = serde_json::from_str(&fixture("scoreboard-by-model.json")).unwrap();
    let actual = serde_json::to_value(by_model(&golden_view()).unwrap()).unwrap();
    assert_same_json(&actual, &expected);
}

#[test]
fn the_golden_pair_through_the_dispatch_too() {
    let answer = ops::dispatch("composeByModel", &fixture("scoreboard-view.json")).unwrap();
    let expected: Value = serde_json::from_str(&fixture("scoreboard-by-model.json")).unwrap();
    assert_same_json(&serde_json::from_str::<Value>(&answer).unwrap(), &expected);
}

#[test]
fn a_model_with_no_effort_reads_effort_unknown_and_names_without_the_dot() {
    let view = by_model(&golden_view()).unwrap();
    let haiku = view.views[0]
        .models
        .iter()
        .find(|model| model.label == "Haiku 5.5")
        .expect("the golden view has Haiku 5.5");
    assert_eq!(haiku.effort, None);
    assert_eq!(haiku.effort_text, "effort unknown");
    // A pick's why for such a model names it without the " · effort" dot; the golden's
    // Sonnet foot line does the same.
    let foot: Vec<&str> = view.views[0].foot.iter().map(|run| run.text.as_str()).collect();
    assert!(
        foot.iter().any(|text| text.contains("(Sonnet 5.5 has 2 runs)")),
        "foot was: {foot:?}"
    );
}

#[test]
fn an_unknown_runtime_keeps_its_id_and_gets_no_mark() {
    // The golden's example/example-1 row runs under OMP, which the maps know; an unknown
    // one must pass through unnamed. Build a one-row view to say it precisely.
    let small: Value = json!({
        "catalog": {
            "minRuns": 5,
            "models": [{ "model": "x/one", "label": "One" }],
            "taskTypes": [{ "id": "worknode-build", "label": "WorkNode build" }]
        },
        "uses": [
            { "id": "planning", "label": "Planning", "taskTypes": [] },
            {
                "id": "building",
                "label": "Building",
                "taskTypes": [{
                    "id": "worknode-build",
                    "label": "WorkNode build",
                    "unit": "task",
                    "rows": [{
                        "model": "x/one", "label": "One", "runtime": "new-runtime", "effort": null,
                        "process": null,
                        "n": { "observed": 6, "benchmark": 0 },
                        "basis": "observed", "ranked": true, "rank": 1, "verdict": "fine",
                        "why": "Fine row",
                        "observed": {
                            "n": 6, "reviews": 2, "ranked": true, "why": "",
                            "measures": {
                                "quality": { "value": 1, "n": 6, "low": 0.5, "high": 1, "of": 6, "per": 6 }
                            }
                        },
                        "benchmarks": []
                    }]
                }]
            },
            { "id": "pm-users", "label": "PM users", "taskTypes": [] }
        ],
        "sources": []
    });
    let parsed: ViewIn = serde_json::from_value(small).unwrap();
    let view = by_model(&parsed).unwrap();
    let model = &view.views[0].models[0];
    assert_eq!(model.runtime, "new-runtime");
    assert_eq!(model.mark, None);
    assert_eq!(model.head, "new-runtime · ");
    // A mark the maps do know still comes through on the golden's own rows.
    let golden_view = by_model(&golden_view()).unwrap();
    let glm = golden_view.views[0]
        .models
        .iter()
        .find(|model| model.label == "GLM 5.3")
        .unwrap();
    assert_eq!(glm.mark, Some("zai"));
    assert_eq!(glm.runtime, "zcode");
}

#[test]
fn a_job_with_only_not_enough_data_rows_has_no_column_and_is_named_in_the_foot() {
    let view = by_model(&golden_view()).unwrap();
    let pipeline = &view.views[0];
    let job_ids: Vec<&str> = pipeline.jobs.iter().map(|job| job.id.as_str()).collect();
    assert!(!job_ids.contains(&"build"), "Build has only not-enough-data rows");
    assert!(!job_ids.contains(&"worknode-create"));
    let foot: Vec<&str> = pipeline.foot.iter().map(|run| run.text.as_str()).collect();
    assert!(foot.iter().any(|text| text.contains("Build (Sonnet 5.5 has 2 runs)")));
    assert!(foot.iter().any(|text| text.contains("Research report")));
    // The models' cell maps still carry the shown jobs only, null for a job not run.
    let sol = pipeline
        .models
        .iter()
        .find(|m| m.key == "openai/gpt-6.1-sol|codex|high")
        .unwrap();
    assert!(sol.cells.get("plan").unwrap().is_none());
    assert!(sol.cells["worknode-build"].is_some());
}

#[test]
fn unknown_fields_are_ignored_and_a_missing_required_field_refuses() {
    let mut padded: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    padded.as_object_mut().unwrap().insert("novelty".into(), json!(1));
    padded["uses"][0]["taskTypes"][0]["rows"][0]["novelty"] = json!("unknown");
    padded["uses"][0]["taskTypes"][0]["rows"][0]["observed"]["novelty"] = json!([1, 2]);
    let answer = ops::dispatch("composeByModel", &padded.to_string()).unwrap();
    let expected: Value = serde_json::from_str(&fixture("scoreboard-by-model.json")).unwrap();
    assert_same_json(&serde_json::from_str::<Value>(&answer).unwrap(), &expected);

    // A missing required field (a row without its verdict, a view without its catalog)
    // rejects the call: the route then serves no byModel field, never a guess.
    let mut no_verdict: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    no_verdict["uses"][0]["taskTypes"][0]["rows"][0]
        .as_object_mut()
        .unwrap()
        .remove("verdict");
    assert!(ops::dispatch("composeByModel", &no_verdict.to_string()).is_err());
    let mut no_catalog: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    no_catalog.as_object_mut().unwrap().remove("catalog");
    assert!(ops::dispatch("composeByModel", &no_catalog.to_string()).is_err());
    assert!(ops::dispatch("composeByModel", "not json").is_err());
}

#[test]
fn the_read_note_comes_from_the_runs_source_detail() {
    let view = by_model(&golden_view()).unwrap();
    let texts: Vec<&str> = view.read_note.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "Read from 92 runs at 4:30 PM: 68 real work, 24 benchmark.",
            " One run file could not be read (a damaged write) and is left out."
        ]
    );
    assert_eq!(view.band_summary, "8 models with runs · 92 runs read at 4:30 PM");
    assert_eq!(view.default_view, "pipeline");
    assert_eq!(view.read_link.label, "What was read");
    assert_eq!(view.read_link.anchor, "sources");
    assert!(view.intro.starts_with(
        "Which model to use for which job. Each row is one model at one effort. A verdict needs at least 5 runs"
    ));
}

// The reviewer's repro inputs (2026-11-09 spend/byModel recheck), plus the two refusals
// the review accepted and asked to lock.

fn facts_kept(view: &ByModel) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for panel in &view.views {
        for model in &panel.models {
            for report in &model.report {
                for measure in &report.measures {
                    if measure.label == "Facts kept" {
                        out.push((model.label.clone(), measure.value.clone(), measure.note.clone()));
                    }
                }
                for run in &report.compare {
                    if run.text.contains("kept 0%") {
                        out.push((model.label.clone(), "compare".to_string(), run.text.clone()));
                    }
                }
            }
        }
    }
    out
}

fn edit_objects(value: &mut Value, visit: &mut dyn FnMut(&mut serde_json::Map<String, Value>)) {
    match value {
        Value::Array(items) => {
            for child in items {
                edit_objects(child, visit);
            }
        }
        Value::Object(map) => {
            visit(map);
            let names: Vec<String> = map.keys().cloned().collect();
            for name in names {
                if let Some(child) = map.get_mut(&name) {
                    edit_objects(child, visit);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[test]
fn a_null_scout_value_is_omitted_not_printed_as_zero() {
    // repro/scout-null.json: facts-kept.value is null, the bounds are still numbers. A2.
    let parsed: ViewIn = serde_json::from_str(&fixture("repro/scout-null.json")).unwrap();
    let view = by_model(&parsed).expect("a null scout measure is left out, not a refusal");
    let facts = facts_kept(&view);
    assert!(facts.is_empty(), "a null facts-kept was printed: {facts:?}");
    let drift = view.views.iter().any(|panel| {
        panel.models.iter().any(|model| {
            model.report.iter().any(|report| {
                report
                    .measures
                    .iter()
                    .any(|m| m.label == "Verdicts changed on a re-check")
            })
        })
    });
    assert!(drift, "a known verdict-drift measure must stay");
}

#[test]
fn a_null_scout_bound_is_omitted_not_printed_as_zero() {
    // Same repro, with a known value and a null low bound: the note must not say 0%.
    let mut doc: Value = serde_json::from_str(&fixture("repro/scout-null.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if let Some(Value::Object(facts)) = map.get_mut("facts-kept") {
            facts.insert("value".to_string(), json!(0.93));
            facts.insert("low".to_string(), Value::Null);
        }
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let view = by_model(&parsed).expect("a null scout bound is left out, not a refusal");
    let facts = facts_kept(&view);
    assert!(facts.is_empty(), "a null facts-kept bound was printed: {facts:?}");
}

#[test]
fn an_avoid_row_without_cheating_says_how_many_runs() {
    // repro/avoid-no-cheating.json: GLM 5.3 is Avoid on quality, with no cheating measure. A3.
    let parsed: ViewIn = serde_json::from_str(&fixture("repro/avoid-no-cheating.json")).unwrap();
    let view = by_model(&parsed).expect("a missing cheating measure is not a refusal of byModel");
    let mut subs = Vec::new();
    for panel in &view.views {
        for model in &panel.models {
            if model.label == "GLM 5.3" && model.effort.as_deref() == Some("max") {
                let cell = model
                    .cells
                    .get("worknode-build")
                    .and_then(|cell| cell.as_ref())
                    .unwrap();
                assert_eq!(cell.verdict, "avoid");
                assert_eq!(cell.word, "Avoid");
                subs.push(cell.sub.clone());
            }
        }
    }
    assert_eq!(subs, vec![vec!["5 runs".to_string()], vec!["5 runs".to_string()]]);
}

#[test]
fn a_scout_row_missing_its_facts_kept_measure_is_still_refused() {
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        map.remove("facts-kept");
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let err = by_model(&parsed).unwrap_err();
    assert_eq!(err.code, "shape");
    assert!(
        err.to_string().contains("a scout row with no facts-kept measure"),
        "{err}"
    );
}

#[test]
fn a_per_task_speed_with_no_value_is_still_refused() {
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if let Some(Value::Object(per_task)) = map.get_mut("perTask") {
            per_task.insert("value".to_string(), Value::Null);
        }
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let err = by_model(&parsed).unwrap_err();
    assert_eq!(err.code, "shape");
    assert!(err.to_string().contains("a per-task speed with no value"), "{err}");
}

#[test]
fn unknown_speed_and_cost_are_not_called_the_same() {
    // The review fixture builder's review runs, with wallSeconds and cost set to null.
    // Both models stay Recommended. Unknown speed and unknown cost are not a tie. R1.
    let parsed: ViewIn = serde_json::from_str(&fixture("repro/unknown-speed-cost.json")).unwrap();
    let view = by_model(&parsed).expect("unknown speed and cost still compose");
    let text = serde_json::to_string(&view).unwrap();
    assert!(
        !text.contains("at the same speed and cost"),
        "an unknown speed and cost were called the same"
    );
    assert!(text.contains("is also Recommended here, with 15 runs."));
    assert!(text.contains("is also Recommended here, with 8 runs."));
}
