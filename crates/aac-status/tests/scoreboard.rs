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
fn a_run_naming_a_model_the_catalog_does_not_list_is_flagged_in_the_foot() {
    // The golden's example/example-1 row: named in the foot with its runs. A catalog that knows
    // every model in the runs flags nothing (B11).
    let view = by_model(&golden_view()).unwrap();
    for panel in &view.views {
        let words: Vec<&str> = panel.foot.iter().map(|run| run.text.as_str()).collect();
        let at = words
            .iter()
            .position(|text| *text == "Models in runs but not in the catalog:")
            .unwrap_or_else(|| panic!("the unknown model was not flagged: {words:?}"));
        assert_eq!(words[at + 1], " example/example-1 (1 run).");
    }
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if let Some(models) = map.get_mut("models").map(|value| value.as_array_mut().unwrap())
            && let Some(sonnet) = models
                .iter()
                .find(|model| model.get("model").and_then(|m| m.as_str()) == Some("anthropic/claude-sonnet-5-5"))
                .cloned()
        {
            let mut known = sonnet;
            known["model"] = json!("example/example-1");
            known["label"] = json!("Example One");
            models.push(known);
        }
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let text = serde_json::to_string(&by_model(&parsed).unwrap()).unwrap();
    assert!(
        !text.contains("Models in runs but not in the catalog"),
        "a known model was flagged: {text}"
    );
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
    assert_eq!(view.band_summary, "7 models with runs · 92 runs read at 4:30 PM");
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
fn the_rounds_each_item_took_to_pass_reach_the_cell_and_the_report() {
    // The golden's Opus worknode-build quality carries firstTry: the cell and the report say how
    // many items needed no fix round (B6).
    let parsed: ViewIn = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    let view = by_model(&parsed).expect("the golden view composes");
    let mut cell_subs = Vec::new();
    let mut report_subs = Vec::new();
    for panel in &view.views {
        for model in &panel.models {
            if model.label == "Opus 5.5" && model.effort.as_deref() == Some("xhigh") {
                if let Some(cell) = model.cells.get("worknode-build").and_then(|cell| cell.as_ref()) {
                    cell_subs.push(cell.sub.clone());
                }
                for report in &model.report {
                    if report.job == "worknode-build" {
                        for measure in &report.measures {
                            if measure.label == "Passed review" {
                                report_subs.push(measure.sub.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        cell_subs,
        vec![
            vec![
                "7 runs".to_string(),
                "passed review 6 of 6, 5 on the first try".to_string()
            ],
            vec![
                "7 runs".to_string(),
                "passed review 6 of 6, 5 on the first try".to_string()
            ],
        ]
    );
    assert_eq!(report_subs, vec!["6 of 6, 5 on the first try".to_string(); 2]);
}

#[test]
fn a_view_without_first_try_keeps_the_plain_passed_sentence() {
    // An older view that carries no firstTry adds no clause, never a 0 (B6).
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if let Some(Value::Object(quality)) = map.get_mut("quality") {
            quality.remove("firstTry");
        }
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let view = by_model(&parsed).expect("a view without firstTry composes");
    let text = serde_json::to_string(&view).unwrap();
    assert!(
        !text.contains("on the first try"),
        "a first-try clause was invented: {text}"
    );
    assert!(
        text.contains("passed review 6 of 6"),
        "the plain sentence must stay: {text}"
    );
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
fn the_pick_uses_the_same_process_as_the_cell_and_the_sheet() {
    // D3 on real data's shape: one model, two processes. The older process (rank 1,
    // recommended) and the newer one (rank 2, fine) must not both surface: the pick, the
    // cell and the sheet all draw on the chosen process, so the pick says the only model
    // tried and never compares the model with itself. A speed that formats to zero time
    // reads "–", and one model reads "1 model", never "1 models".
    let misses = |value: f64| json!({ "value": value, "n": 12, "low": 0.01, "high": 0.03, "of": 19, "per": 1075 });
    let bench = |version: &str| {
        json!({
            "model": "x/one", "label": "One", "runtime": "rt", "effort": "e",
            "process": { "id": "p", "version": version },
            "n": { "observed": 0, "benchmark": 12 },
            "basis": "benchmark", "ranked": true,
            "why": "",
            "observed": { "n": 0, "reviews": 0, "ranked": false, "why": "" },
            "benchmarks": [{
                "n": 12, "ranked": true, "why": "",
                "measures": {
                    "silent-misses": misses(0.02),
                    "speed": { "value": 0.02, "n": 12, "low": 0.01, "high": 0.04 }
                }
            }]
        })
    };
    let mut older = bench("1");
    older["verdict"] = json!("recommended");
    older["rank"] = json!(1);
    let mut newer = bench("2");
    newer["verdict"] = json!("fine");
    newer["rank"] = json!(2);
    let small: Value = json!({
        "catalog": {
            "minRuns": 5,
            "models": [{ "model": "x/one", "label": "One" }],
            "taskTypes": [{ "id": "plan", "label": "Planning wizard" }]
        },
        "uses": [
            {
                "id": "planning",
                "label": "Planning",
                "taskTypes": [{
                    "id": "plan", "label": "Planning wizard", "unit": "word",
                    "rows": [older, newer]
                }]
            },
            { "id": "building", "label": "Building", "taskTypes": [] },
            { "id": "pm-users", "label": "PM users", "taskTypes": [] }
        ],
        "sources": []
    });
    let parsed: ViewIn = serde_json::from_value(small).unwrap();
    let view = by_model(&parsed).unwrap();
    assert_eq!(view.band_summary, "1 model with runs");
    let pipeline = &view.views[0];
    let meta: Vec<&str> = pipeline.meta.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(meta, vec!["1", " model with runs · a verdict needs 5 runs of that job"]);

    // The pick names the only model tried, with the chosen process's verdict — never
    // the older process's "Benchmark only" comparison of the model with itself.
    let why = pipeline.picks[0]
        .why
        .iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(why, "One · e is the only model tried (12 runs, Fine).");

    // The cell and the sheet agree with the pick (the same process, verdict fine), and
    // the speed reads unknown, never "0.0 s".
    let model = &pipeline.models[0];
    let cell = model.cells.get("plan").unwrap().as_ref().unwrap();
    assert_eq!(cell.verdict, "fine");
    let times: Vec<&str> = model.report[0]
        .measures
        .iter()
        .filter(|m| m.label == "Time per word")
        .map(|m| m.value.as_str())
        .collect();
    assert_eq!(times, vec!["–"]);
    let text = serde_json::to_string(&view).unwrap();
    assert!(!text.contains("0.0 s"), "a zero time was printed: {text}");
    assert!(!text.contains("1 models"), "the model count was not singular: {text}");
}

#[test]
fn a_demoted_row_with_reviews_names_how_many_in_the_cell() {
    // M3: a demoted row whose reviews were read says how many, in the bar's own count —
    // the reviewed items when a quality pass rate exists, else the review runs. "None
    // reviewed yet" stays only for none at all.
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    for use_in in doc["uses"].as_array_mut().unwrap() {
        let use_id = use_in["id"].as_str().unwrap_or("").to_string();
        for task in use_in["taskTypes"].as_array_mut().unwrap() {
            if task["id"] != "worknode-build" {
                continue;
            }
            for row in task["rows"].as_array_mut().unwrap() {
                let haiku = row["model"].as_str() == Some("anthropic/claude-haiku-5-5");
                // The pipeline view's Haiku: 3 review runs read, no decided items (null per).
                if haiku && use_id == "building" {
                    row["observed"]["reviews"] = json!(3);
                }
                // The pm-users view's Haiku: 3 review runs over 2 decided items.
                if haiku && use_id == "pm-users" {
                    row["observed"]["reviews"] = json!(3);
                    row["observed"]["measures"]["quality"] = json!({
                        "value": 1, "n": 5, "low": 0.5, "high": 1, "of": 2, "per": 2
                    });
                }
            }
        }
    }
    let view = by_model(&serde_json::from_value(doc).unwrap()).unwrap();
    let haiku_sub = |panel: &aac_status::scoreboard::ViewOut| {
        panel
            .models
            .iter()
            .find(|m| m.key == "anthropic/claude-haiku-5-5|claude-code|")
            .unwrap()
            .cells
            .get("worknode-build")
            .and_then(|cell| cell.as_ref())
            .map(|cell| cell.sub.clone())
    };
    // The bar's count: zero decided items where none are decided (review runs don't
    // count), the items where they exist (2).
    assert_eq!(haiku_sub(&view.views[0]).unwrap(), vec!["5 runs, 0 reviewed, needs 5"]);
    assert_eq!(haiku_sub(&view.views[1]).unwrap(), vec!["5 runs, 2 reviewed, needs 5"]);
    // The golden's own Haiku (no review runs at all) reads none (L-new-4).
    let golden = by_model(&golden_view()).unwrap();
    assert_eq!(haiku_sub(&golden.views[0]).unwrap(), vec!["5 runs, none reviewed yet"]);
}

#[test]
fn unknown_model_runs_count_every_runtime_and_effort() {
    // L1: one unknown model run at two efforts in one task type is two rows. The foot
    // counts both; the old key (model without its effort) skipped the second.
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    for use_in in doc["uses"].as_array_mut().unwrap() {
        for task in use_in["taskTypes"].as_array_mut().unwrap() {
            if task["id"] != "build" {
                continue;
            }
            let rows = task["rows"].as_array_mut().unwrap();
            let at = rows.iter().position(|row| row["model"] == "example/example-1").unwrap();
            let mut clone = rows[at].clone();
            clone["effort"] = json!("max");
            rows.push(clone);
        }
    }
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let view = by_model(&parsed).unwrap();
    for panel in &view.views {
        let foot: Vec<&str> = panel.foot.iter().map(|run| run.text.as_str()).collect();
        assert!(foot.contains(&" example/example-1 (2 runs)."), "foot was: {foot:?}");
    }
}

#[test]
fn speed_note_bounds_never_print_zero_times() {
    // L5: a bound that formats to a zero time reads "–"; when both bounds read "–" the
    // ranged note drops to the plain Over sentence.
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    for use_in in doc["uses"].as_array_mut().unwrap() {
        let use_id = use_in["id"].as_str().unwrap_or("").to_string();
        for task in use_in["taskTypes"].as_array_mut().unwrap() {
            let task_id = task["id"].as_str().unwrap_or("").to_string();
            for row in task["rows"].as_array_mut().unwrap() {
                let opus = row["model"].as_str() == Some("anthropic/claude-opus-5-5")
                    && row["effort"].as_str() == Some("xhigh")
                    && row["observed"].is_object();
                if !opus {
                    continue;
                }
                // Scout's ranged note: both bounds sub-centisecond. The sheet now stands
                // on the ranked benchmark column (the observed column is below the bar).
                if use_id == "planning" && task_id == "scout" {
                    let speed = &mut row["observed"]["measures"]["speed"];
                    speed["low"] = json!(0.01);
                    speed["high"] = json!(0.04);
                    for bench in row["benchmarks"].as_array_mut().unwrap() {
                        let ranked = bench["ranked"].as_bool() == Some(true);
                        if ranked {
                            bench["measures"]["speed"]["low"] = json!(0.01);
                            bench["measures"]["speed"]["high"] = json!(0.04);
                        }
                    }
                }
                // WorkNode build's per-task note: a sub-second fastest.
                if use_id == "building" && task_id == "worknode-build" {
                    row["observed"]["measures"]["speed"]["perTask"]["low"] = json!(0.1);
                }
            }
        }
    }
    let view = by_model(&serde_json::from_value(doc).unwrap()).unwrap();
    let opus = view.views[0]
        .models
        .iter()
        .find(|m| m.key == "anthropic/claude-opus-5-5|claude-code|xhigh")
        .unwrap();
    let note = |job: &str| {
        opus.report
            .iter()
            .find(|r| r.job == job)
            .unwrap()
            .measures
            .iter()
            .find(|m| m.label.starts_with("Time per"))
            .map(|m| m.note.clone())
    };
    assert_eq!(note("scout").as_deref(), Some("Over 5 runs."));
    assert_eq!(
        note("worknode-build").as_deref(),
        Some("Fastest –, slowest 27 min, over 7 runs.")
    );
}

#[test]
fn a_count_of_one_reads_singular() {
    // L-new-2/L-new-5: "has 1 runs", "1 blind reviews" and "With 1 reviews" all read
    // singular for one. (The golden's one-review rate note and one-review head left
    // the page with the mechanical column, so those halves are pinned on edits.)
    let view = by_model(&golden_view()).unwrap();
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains("GLM 5.3 · max has 1 run"), "{text}");
    assert!(!text.contains("has 1 runs"), "{text}");
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if map.get("model").and_then(|m| m.as_str()) == Some("anthropic/claude-haiku-5-5")
            && let Some(observed) = map.get_mut("observed")
            && let Some(reviews) = observed.get_mut("reviews")
        {
            *reviews = json!(1);
        }
        // One review behind GLM's rate note.
        if map.get("model").and_then(|m| m.as_str()) == Some("zai/glm-5.3")
            && let Some(observed) = map.get_mut("observed")
            && let Some(quality) = observed.get_mut("measures").and_then(|m| m.get_mut("quality"))
            && let Some(per) = quality.get_mut("per")
        {
            *per = json!(1);
        }
    });
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let edited = serde_json::to_string(&by_model(&parsed).unwrap()).unwrap();
    assert!(edited.contains("1 blind review"), "{edited}");
    assert!(!edited.contains("1 blind reviews"), "{edited}");
    assert!(edited.contains("With 1 review the true rate"), "{edited}");
    assert!(!edited.contains("With 1 reviews"), "{edited}");
}

#[test]
fn a_cell_one_run_short_reads_singular() {
    // sb2-R1-N1: a cell one run short of the bar says "1 run", not "1 runs". The
    // golden's Haiku has only this job, so its head and sheet head count one run too.
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    for use_in in doc["uses"].as_array_mut().unwrap() {
        for task in use_in["taskTypes"].as_array_mut().unwrap() {
            if task["id"] != "worknode-build" {
                continue;
            }
            for row in task["rows"].as_array_mut().unwrap() {
                if row["model"].as_str() == Some("anthropic/claude-haiku-5-5") {
                    row["n"]["observed"] = json!(1);
                    row["observed"]["n"] = json!(1);
                }
            }
        }
    }
    let view = by_model(&serde_json::from_value(doc).unwrap()).unwrap();
    for panel in &view.views {
        let haiku = panel
            .models
            .iter()
            .find(|m| m.key == "anthropic/claude-haiku-5-5|claude-code|")
            .unwrap();
        let cell = haiku
            .cells
            .get("worknode-build")
            .and_then(|cell| cell.as_ref())
            .unwrap();
        assert_eq!(cell.verdict, "not-enough-data");
        assert_eq!(cell.word, "1 run");
        assert_eq!(cell.sub, vec!["needs 5".to_string()]);
    }
    // No count phrase anywhere in the answer calls one run plural ("11 runs" is fine).
    let text = serde_json::to_string(&view).unwrap();
    for plural in ["1 runs", "1 real runs", "1 benchmark runs"] {
        for (at, _) in text.match_indices(plural) {
            let before = text[..at].chars().next_back();
            assert!(
                !before.is_some_and(|c| c.is_ascii_digit()),
                "one run read plural at byte {at}: {text}"
            );
        }
    }
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

// ---------------------------------------------------------------------------------------------
// Scoreboard v2: grouping and sort keys over the same rows as the matrix.

fn identity() -> Value {
    json!({
        "families": [
            { "id": "openai", "label": "OpenAI · GPT, Codex", "short": "GPT", "order": 2 },
            { "id": "anthropic", "label": "Anthropic · Claude", "short": "Claude", "order": 1 },
            { "id": "zai", "label": "Z.ai · GLM", "short": "GLM", "order": 3 }
        ],
        "models": {
            "anthropic/claude-opus-5-5": { "family": "anthropic", "order": 0 },
            "anthropic/claude-sonnet-5-5": { "family": "anthropic", "order": 1 },
            "anthropic/claude-haiku-5-5": { "family": "anthropic", "order": 2 },
            "openai/gpt-6.1-sol": { "family": "openai", "order": 4 },
            "zai/glm-5.3": { "family": "zai", "order": 8 }
        },
        "efforts": ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultrathink"]
    })
}

fn with_identity() -> ViewIn {
    let mut view: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    view["identity"] = identity();
    serde_json::from_value(view).unwrap()
}

#[test]
fn every_matrix_row_has_its_family_order_and_sort_keys() {
    let view = with_identity();
    let groups = aac_status::scoreboard::group_keys(&view).unwrap();
    let words = by_model(&view).unwrap();
    // The identity never changes the words: the golden pair still reproduces with it present.
    let expected: Value = serde_json::from_str(&fixture("scoreboard-by-model.json")).unwrap();
    assert_same_json(&serde_json::to_value(&words).unwrap(), &expected);
    let ids: Vec<&str> = groups.families.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, vec!["anthropic", "openai", "zai", "none"]);
    for section in &words.views {
        let keys = &groups.views[&section.id];
        assert_eq!(keys.len(), section.models.len(), "{}", section.id);
        for model in &section.models {
            let k = &keys[&model.key];
            let verdicts = model
                .cells
                .values()
                .flatten()
                .filter(|c| c.verdict != "not-enough-data")
                .count() as i64;
            assert_eq!(k.verdicts.iter().sum::<i64>(), verdicts, "{}", model.key);
            assert_eq!(k.runs, model.runs, "{}", model.key);
            if let (Some(passed), Some(decided)) = (k.passed, k.decided) {
                assert!(passed <= decided && decided > 0);
            }
        }
    }
    let pipeline = &groups.views["pipeline"];
    let opus_xhigh = &pipeline["anthropic/claude-opus-5-5|claude-code|xhigh"];
    let opus_high = &pipeline["anthropic/claude-opus-5-5|claude-code|high"];
    let haiku = &pipeline["anthropic/claude-haiku-5-5|claude-code|"];
    assert_eq!(opus_xhigh.family, "anthropic");
    assert!(opus_high.effort_order < opus_xhigh.effort_order);
    assert_eq!(haiku.effort_order, 100, "no effort sorts after every level");
    assert_eq!(pipeline["zai/glm-5.3|zcode|max"].family, "zai");
    assert!(pipeline["openai/gpt-6.1-sol|codex|high"].model_order > opus_xhigh.model_order);
}

#[test]
fn without_identity_every_row_is_unplaced_and_the_call_still_answers() {
    let groups = aac_status::scoreboard::group_keys(&golden_view()).unwrap();
    assert_eq!(groups.families.len(), 1);
    assert!(
        groups.views["pipeline"]
            .values()
            .all(|k| k.family == "none" && k.model_order == 10_000)
    );
    let answer = ops::dispatch("composeGroups", &fixture("scoreboard-view.json")).unwrap();
    assert!(answer.contains("\"views\""));
}

/// The foot's runs joined as the page draws them (one after another).
fn foot_texts(view: &ViewIn) -> Vec<String> {
    by_model(view)
        .unwrap()
        .views
        .iter()
        .map(|panel| panel.foot.iter().map(|run| run.text.as_str()).collect::<String>())
        .collect()
}

#[test]
fn the_foot_never_runs_two_sentences_together() {
    // Review of 6baa2f370: "...Muse Wonder 1.0.Models in runs but not in the catalog:".
    for text in foot_texts(&golden_view()) {
        assert!(
            text.contains("Muse Wonder 1.0. Models in runs but not in the catalog: example/example-1 (1 run)."),
            "{text}"
        );
        assert!(
            !text
                .as_bytes()
                .windows(2)
                .any(|w| w[0] == b'.' && w[1].is_ascii_uppercase()),
            "a sentence runs into the next: {text}"
        );
        assert!(!text.ends_with(' '), "{text:?}");
    }
    // With nothing outside the catalog the note ends at its last sentence, with no stray space.
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    edit_objects(&mut doc, &mut |map| {
        if let Some(models) = map.get_mut("models").map(|value| value.as_array_mut().unwrap())
            && let Some(sonnet) = models
                .iter()
                .find(|model| model.get("model").and_then(|m| m.as_str()) == Some("anthropic/claude-sonnet-5-5"))
                .cloned()
        {
            let mut known = sonnet;
            known["model"] = json!("example/example-1");
            known["label"] = json!("Example One");
            models.push(known);
        }
    });
    for text in foot_texts(&serde_json::from_value(doc).unwrap()) {
        assert!(text.ends_with("Muse Wonder 1.0."), "{text:?}");
    }
}

#[test]
fn why_only_fine_names_the_only_ranked_survivor() {
    // A sibling with real runs is excluded for a non-count reason; the Fine sentence must
    // not claim no other model has minRuns real runs.
    let ranked = json!({
        "model": "x/one", "label": "One", "runtime": "rt", "effort": "e",
        "process": null,
        "n": { "observed": 5, "benchmark": 0 },
        "basis": "observed", "ranked": true, "rank": 1, "verdict": "fine",
        "why": "The only ranked row here, so nothing to compare it with (n = 5).",
        "observed": {
            "n": 5, "reviews": 0, "ranked": true,
            "why": "The only ranked row here, so nothing to compare it with (n = 5).",
            "measures": {
                "quality": { "value": 1, "n": 5, "low": 0.5, "high": 1, "of": 5, "per": 5 }
            }
        },
        "benchmarks": []
    });
    let sibling = json!({
        "model": "x/two", "label": "Two", "runtime": "rt", "effort": "e",
        "process": null,
        "n": { "observed": 8, "benchmark": 0 },
        "basis": "observed", "ranked": false, "verdict": "avoid",
        "why": "Avoided for a non-count reason.",
        "observed": {
            "n": 8, "reviews": 0, "ranked": false, "why": "Avoided for a non-count reason.",
            "measures": {}
        },
        "benchmarks": []
    });
    let small: Value = json!({
        "catalog": {
            "minRuns": 5,
            "models": [
                { "model": "x/one", "label": "One" },
                { "model": "x/two", "label": "Two" }
            ],
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
                    "rows": [ranked, sibling]
                }]
            },
            { "id": "pm-users", "label": "PM users", "taskTypes": [] }
        ],
        "sources": []
    });
    let parsed: ViewIn = serde_json::from_value(small).unwrap();
    let view = by_model(&parsed).unwrap();
    let one = view.views[0]
        .models
        .iter()
        .find(|model| model.label == "One")
        .expect("ranked Fine row");
    let compare: String = one.report[0].compare.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        compare,
        "Why only Fine: This is the only model with enough real WorkNode build runs to rank here, so there is nothing to compare it with."
    );
    assert!(
        !compare.contains("no other model has"),
        "the old count claim must not stand: {compare}"
    );
}

#[test]
fn why_only_fine_names_benchmark_runs_when_that_is_the_ranked_basis() {
    let ranked = json!({
        "model": "x/one", "label": "One", "runtime": "rt", "effort": "e",
        "process": null,
        "n": { "observed": 0, "benchmark": 5 },
        "basis": "benchmark", "ranked": true, "rank": 1, "verdict": "fine",
        "why": "The only ranked row here, so nothing to compare it with (n = 5).",
        "observed": { "n": 0, "reviews": 0, "ranked": false, "why": "", "measures": {} },
        "benchmarks": [{
            "n": 5, "reviews": 0, "ranked": true,
            "why": "The only ranked row here, so nothing to compare it with (n = 5).",
            "measures": {}
        }]
    });
    let sibling = json!({
        "model": "x/two", "label": "Two", "runtime": "rt", "effort": "e",
        "process": null,
        "n": { "observed": 8, "benchmark": 0 },
        "basis": "observed", "ranked": false, "verdict": "avoid",
        "why": "Avoided: not enough reviews.",
        "observed": {
            "n": 8, "reviews": 0, "ranked": false, "why": "Avoided: not enough reviews.",
            "measures": {}
        },
        "benchmarks": []
    });
    let small: Value = json!({
        "catalog": {
            "minRuns": 5,
            "models": [
                { "model": "x/one", "label": "One" },
                { "model": "x/two", "label": "Two" }
            ],
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
                    "rows": [ranked, sibling]
                }]
            },
            { "id": "pm-users", "label": "PM users", "taskTypes": [] }
        ],
        "sources": []
    });
    let parsed: ViewIn = serde_json::from_value(small).unwrap();
    let view = by_model(&parsed).unwrap();
    let one = view.views[0]
        .models
        .iter()
        .find(|model| model.label == "One")
        .expect("ranked Fine row");
    let compare: String = one.report[0].compare.iter().map(|run| run.text.as_str()).collect();
    assert_eq!(
        compare,
        "Why only Fine: This is the only model with enough benchmark WorkNode build runs to rank here, so there is nothing to compare it with."
    );
    assert!(
        !compare.contains("enough real "),
        "a benchmark-only row must not claim real runs: {compare}"
    );

#[test]
fn untraced_damaged_source_leaves_all_rows_unmarked() {
    let doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    assert_eq!(doc["sources"][0]["state"], json!("unreadable"));
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let actual = serde_json::to_value(by_model(&parsed).unwrap()).unwrap();
    for view in actual["views"].as_array().unwrap() {
        for model in view["models"].as_array().unwrap() {
            assert!(
                model.get("sourceState").is_none(),
                "untraced page-level unreadable source must not label row {}: {model}",
                model["key"]
            );
            assert!(model.get("asOf").is_none());
            assert!(model.get("ageSeconds").is_none());
            for cell in model["cells"].as_object().unwrap().values() {
                if cell.is_object() {
                    assert!(cell.get("sourceState").is_none());
                }
            }
            for report in model["report"].as_array().unwrap() {
                assert!(report.get("sourceState").is_none());
            }
        }
    }
}

#[test]
fn traced_failing_row_carries_source_state_only_to_that_model_and_job() {
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    let mut marked = 0usize;
    for use_item in doc["uses"].as_array_mut().unwrap() {
        for task in use_item["taskTypes"].as_array_mut().unwrap() {
            if task["id"] != "worknode-build" {
                continue;
            }
            for row in task["rows"].as_array_mut().unwrap() {
                if row["model"] == "anthropic/claude-opus-5-5" && row["effort"] == "xhigh" {
                    row["sourceState"] = json!("unreadable");
                    row["asOf"] = json!("2026-11-06T16:25:00.000Z");
                    row["ageSeconds"] = json!(300);
                    marked += 1;
                }
            }
        }
    }
    assert!(marked > 0, "fixture has an Opus 5.5 xhigh worknode-build row");
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let actual = serde_json::to_value(by_model(&parsed).unwrap()).unwrap();
    let models = actual["views"][0]["models"].as_array().unwrap();
    let opus = models
        .iter()
        .find(|m| m["key"] == "anthropic/claude-opus-5-5|claude-code|xhigh")
        .expect("Opus 5.5 xhigh row");
    assert_eq!(opus["cells"]["worknode-build"]["verdict"], json!("recommended"));
    assert_eq!(opus["cells"]["worknode-build"]["word"], json!("Recommended"));
    assert_eq!(opus["sourceState"], json!("unreadable"));
    assert_eq!(opus["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(opus["ageSeconds"], json!(300));
    assert_eq!(opus["cells"]["worknode-build"]["sourceState"], json!("unreadable"));
    assert_eq!(opus["cells"]["worknode-build"]["ageSeconds"], json!(300));
    assert!(
        opus["cells"]["scout"].get("sourceState").is_none(),
        "unaffected job cell on the same model must stay unmarked"
    );
    let build_report = opus["report"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["job"] == "worknode-build")
        .expect("worknode-build report");
    assert_eq!(build_report["sourceState"], json!("unreadable"));
    assert_eq!(build_report["ageSeconds"], json!(300));
    for other_report in opus["report"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["job"] != "worknode-build")
    {
        assert!(
            other_report.get("sourceState").is_none(),
            "unaffected job report must stay unmarked: {other_report}"
        );
    }
    for other in models
        .iter()
        .filter(|m| m["key"] != "anthropic/claude-opus-5-5|claude-code|xhigh")
    {
        assert!(
            other.get("sourceState").is_none(),
            "unrelated model row {} must stay unmarked: {other}",
            other["key"]
        );
    }
}

#[test]
fn unknown_and_ok_row_states_are_omitted_and_missing_age_is_never_invented() {
    let mut doc: Value = serde_json::from_str(&fixture("scoreboard-view.json")).unwrap();
    for use_item in doc["uses"].as_array_mut().unwrap() {
        for task in use_item["taskTypes"].as_array_mut().unwrap() {
            if task["id"] != "worknode-build" {
                continue;
            }
            for row in task["rows"].as_array_mut().unwrap() {
                if row["model"] == "anthropic/claude-opus-5-5" && row["effort"] == "xhigh" {
                    row["sourceState"] = json!("unreadable");
                    row["asOf"] = Value::Null;
                    row["ageSeconds"] = Value::Null;
                } else if row["model"] == "openai/gpt-6.1-sol" && row["effort"] == "high" {
                    row["sourceState"] = json!("ok");
                    row["asOf"] = json!("2026-11-06T16:30:00.000Z");
                    row["ageSeconds"] = json!(0);
                } else if row["model"] == "zai/glm-5.3" && row["effort"] == "max" {
                    row["sourceState"] = json!("mystery");
                    row["asOf"] = json!("2026-11-06T16:20:00.000Z");
                    row["ageSeconds"] = json!(600);
                }
            }
        }
    }
    let parsed: ViewIn = serde_json::from_value(doc).unwrap();
    let actual = serde_json::to_value(by_model(&parsed).unwrap()).unwrap();
    let models = actual["views"][0]["models"].as_array().unwrap();
    let opus = models
        .iter()
        .find(|m| m["key"] == "anthropic/claude-opus-5-5|claude-code|xhigh")
        .unwrap();
    assert_eq!(opus["sourceState"], json!("unreadable"));
    assert!(opus.get("asOf").is_none(), "missing failure time must not be invented");
    assert!(
        opus.get("ageSeconds").is_none(),
        "missing failure age must not be invented"
    );
    let sol = models
        .iter()
        .find(|m| m["key"] == "openai/gpt-6.1-sol|codex|high")
        .unwrap();
    assert!(sol.get("sourceState").is_none(), "ok state must not be emitted");
    let glm = models.iter().find(|m| m["key"] == "zai/glm-5.3|zcode|max").unwrap();
    assert!(
        glm.get("sourceState").is_none(),
        "unknown state must not become Unavailable"
    );
}

#[test]
fn read_scoreboard_traces_failed_run_file_only_to_its_row_using_failure_since() {
    use aac_status::read_context::{Date, Presentation};
    use aac_status::read_scoreboard::compose_fed;
    use std::sync::Arc;

    let fail_at: i64 = 1_793_982_300_000; // 2026-11-06T16:25:00.000Z
    let now: i64 = 1_793_982_600_000; // 2026-11-06T16:30:00.000Z (5m later; scan is fresh)
    let make_run = |id: &str, model_id: &str, harness: &str, effort: &str| {
        json!({
            "schema": "agent-reports/run@1",
            "id": id,
            "writer": "vm.lead",
            "seq": 1,
            "writtenAt": "2026-11-06T15:00:00.000Z",
            "source": "observed",
            "project": "example",
            "stream": "S",
            "task": { "id": id, "type": "worknode-build", "title": id, "size": { "tasks": 1 } },
            "model": { "provider": null, "id": model_id, "harness": harness, "effort": effort, "account": null },
            "startedAt": "2026-11-06T14:50:00.000Z",
            "endedAt": "2026-11-06T15:00:00.000Z",
            "wallSeconds": 600,
            "outcome": "done",
            "flags": [],
            "evidence": [],
            "review": null,
            "process": null,
            "benchmark": null,
            "imported": null
        })
    };
    let opus_doc = make_run("vm.lead--WN-1", "anthropic/claude-opus-5-5", "claude-code", "xhigh");
    let sol_doc = make_run("vm.lead--WN-2", "openai/gpt-6.1-sol", "codex", "high");
    let opus_file = "writers/vm.lead/runs/2026-11-06/vm.lead--WN-1.json";
    let sol_file = "writers/vm.lead/runs/2026-11-06/vm.lead--WN-2.json";
    let data = json!({
        "now": now,
        "timeZone": "UTC",
        "root": "/fixture",
        "catalog": {
            "state": "ok",
            "scannedAt": now,
            "missingFiles": [],
            "problems": [],
            "minRuns": 5,
            "models": [
                { "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "family": "Claude", "runtimes": ["claude-code"], "efforts": ["xhigh"], "prices": null, "notes": null },
                { "model": "openai/gpt-6.1-sol", "label": "Sol 6.1", "family": "GPT-6", "runtimes": ["codex"], "efforts": ["high"], "prices": null, "notes": null }
            ],
            "taskTypes": [
                { "id": "worknode-build", "label": "WorkNode build", "description": null, "unit": "task", "measures": ["reliability"], "byProcess": false }
            ],
            "measures": [
                { "id": "reliability", "label": "Reliability", "description": "Done", "unit": "share", "better": "higher", "kind": "outcome" }
            ],
            "uses": [
                { "id": "pipeline", "label": "Our pipeline", "description": null, "taskTypes": ["worknode-build"], "weights": { "reliability": 3 } }
            ],
            "benchmarks": []
        },
        "runs": {
            "correctionsResolved": true,
            "state": "ok",
            "scannedAt": now,
            "runs": [
                { "file": opus_file, "firstSeenAt": fail_at - 60_000, "sha256": "a", "doc": opus_doc },
                { "file": sol_file, "firstSeenAt": fail_at - 60_000, "sha256": "b", "doc": sol_doc }
            ],
            "writers": 1,
            "skipped": { "runFiles": 0, "writerFolders": 0 },
            "problems": [
                { "file": opus_file, "reason": "could not be read", "since": fail_at },
                { "file": "writers/vm.lead/runs/2026-11-06/vm.lead--damaged.json", "reason": "contains NUL bytes from a damaged write", "since": now }
            ]
        },
        "docs": [opus_doc, sol_doc],
        "joinReviews": [],
        "seedFailures": []
    });
    let facts: Presentation = serde_json::from_value(json!({
        "dates": {
            fail_at.to_string(): { "iso": "2026-11-06T16:25:00.000Z", "clock": "4:25 PM", "day": "Nov 6" },
            now.to_string(): { "iso": "2026-11-06T16:30:00.000Z", "clock": "4:30 PM", "day": "Nov 6" }
        },
        "epochs": {
            "2026-11-06T15:00:00.000Z": 1_793_977_200_000_i64
        },
        "order": {
            "anthropic/claude-opus-5-5|claude-code|xhigh": 0,
            "openai/gpt-6.1-sol|codex|high": 1,
            "vm.lead--WN-1": 2,
            "vm.lead--WN-2": 3
        }
    }))
    .unwrap();
    let date = Date::of_now(now, "4:30 PM".into(), "Nov 6".into()).unwrap();
    let view = compose_fed(data, Arc::new(facts), (now, date)).expect("compose_fed view");
    // Sources shows scan age 0; only the traced Opus row carries failure age 300s (from fail_at).
    assert_eq!(view["sources"][0]["state"], json!("unreadable"));
    assert_eq!(view["sources"][0]["ageSeconds"].as_f64(), Some(0.0));
    let rows = view["uses"][0]["taskTypes"][0]["rows"].as_array().unwrap();
    let opus = rows.iter().find(|r| r["model"] == "anthropic/claude-opus-5-5").unwrap();
    let sol = rows.iter().find(|r| r["model"] == "openai/gpt-6.1-sol").unwrap();
    assert_eq!(opus["sourceState"], json!("unreadable"));
    assert_eq!(opus["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(opus["ageSeconds"].as_f64(), Some(300.0));
    assert!(
        sol.get("sourceState").is_none(),
        "healthy row must stay unmarked: {sol}"
    );
    assert!(sol.get("asOf").is_none());
    assert!(sol.get("ageSeconds").is_none());
}

// ---------------------------------------------------------------------------------------------
// M1 (2026-11-22): a ranked verdict discloses the failed inputs its ranking used.

const M1_NOW: i64 = 1_793_982_600_000; // 2026-11-06T16:30:00.000Z
const M1_FAILED: i64 = 1_793_982_300_000; // 2026-11-06T16:25:00.000Z
const M1_EARLIER: i64 = 1_793_982_000_000; // 2026-11-06T16:20:00.000Z
const M1_HOUR: i64 = 1_793_985_900_000; // 2026-11-06T17:25:00.000Z (one hour after M1_FAILED)

/// One observed build run on Opus and its blind review by Sol, with the catalog and scan state a
/// caller sets: enough to rank one row (minRuns 1, one decisive review). `filtered` keeps only the
/// build run in the view and joins the review from outside it; `now` is the request's clock.
fn m1_envelope(
    catalog: Value,
    problems: Value,
    filtered: bool,
    now: i64,
) -> (Value, aac_status::read_context::Presentation) {
    let run = |id: &str, writer: &str, kind: &str, model: &str, ended: &str, review: Value| {
        json!({
            "schema": "agent-reports/run@1", "id": id, "writer": writer, "seq": 1,
            "writtenAt": ended, "source": "observed", "project": "example", "stream": "S",
            "task": { "id": id, "type": kind, "title": id, "size": { "tasks": 1 } },
            "model": { "provider": null, "id": model, "harness": "claude-code", "effort": "xhigh", "account": null },
            "startedAt": null, "endedAt": ended, "wallSeconds": 600, "outcome": "done",
            "flags": [], "evidence": [], "review": review, "process": null, "benchmark": null,
            "imported": null
        })
    };
    let target = run(
        "vm.lead--WN-1",
        "vm.lead",
        "worknode-build",
        "anthropic/claude-opus-5-5",
        "2026-11-06T15:00:00.000Z",
        json!(null),
    );
    let review = run(
        "mac.reviews--R1",
        "mac.reviews",
        "review",
        "openai/gpt-6.1-sol",
        "2026-11-06T15:30:00.000Z",
        json!({"of": "vm.lead--WN-1", "verdict": "pass", "findings": {"high": 0, "medium": 0, "low": 0}, "overturned": false, "disputed": false, "by": null}),
    );
    let target_file = "writers/vm.lead/runs/2026-11-06/vm.lead--WN-1.json";
    let review_file = "writers/mac.reviews/runs/2026-11-06/mac.reviews--R1.json";
    let data = json!({
        "timeZone": "UTC",
        "root": "/fixture",
        "catalog": {
            "state": catalog["state"], "scannedAt": catalog["scannedAt"],
            "failedSince": catalog["failedSince"],
            "missingFiles": [], "problems": catalog["problems"], "minRuns": 1,
            "models": [
                { "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "family": "Claude", "runtimes": ["claude-code"], "efforts": ["xhigh"], "prices": null, "notes": null },
                { "model": "openai/gpt-6.1-sol", "label": "Sol 6.1", "family": "GPT-6", "runtimes": ["claude-code"], "efforts": ["xhigh"], "prices": null, "notes": null }
            ],
            "taskTypes": [
                { "id": "worknode-build", "label": "WorkNode build", "description": null, "unit": "task", "measures": ["reliability"], "byProcess": false },
                { "id": "review", "label": "Review", "description": null, "unit": "task", "measures": ["reliability"], "byProcess": false }
            ],
            "measures": [
                { "id": "reliability", "label": "Reliability", "description": "Done", "unit": "share", "better": "higher", "kind": "outcome" }
            ],
            "uses": [
                { "id": "pipeline", "label": "Our pipeline", "description": null, "taskTypes": ["worknode-build"], "weights": { "reliability": 3 } }
            ],
            "benchmarks": []
        },
        "now": now,
        "runs": {
            "correctionsResolved": true, "state": "ok", "scannedAt": now,
            "runs": if filtered {
                vec![json!({ "file": target_file, "firstSeenAt": M1_EARLIER, "sha256": "a", "doc": target.clone() })]
            } else {
                vec![
                    json!({ "file": target_file, "firstSeenAt": M1_EARLIER, "sha256": "a", "doc": target.clone() }),
                    json!({ "file": review_file, "firstSeenAt": M1_EARLIER, "sha256": "b", "doc": review.clone() })
                ]
            },
            "writers": 2, "skipped": { "runFiles": 0, "writerFolders": 0 }, "problems": problems
        },
        "docs": if filtered { vec![target] } else { vec![target, review.clone()] },
        "joinReviews": if filtered { vec![review] } else { Vec::new() },
        "joinReviewFiles": if filtered { vec![json!(review_file)] } else { Vec::new() },
        "seedFailures": []
    });
    let facts: aac_status::read_context::Presentation = serde_json::from_value(json!({
        "dates": {
            M1_NOW.to_string(): { "iso": "2026-11-06T16:30:00.000Z", "clock": "4:30 PM", "day": "Nov 6" },
            M1_HOUR.to_string(): { "iso": "2026-11-06T17:25:00.000Z", "clock": "5:25 PM", "day": "Nov 6" },
            M1_FAILED.to_string(): { "iso": "2026-11-06T16:25:00.000Z", "clock": "4:25 PM", "day": "Nov 6" },
            M1_EARLIER.to_string(): { "iso": "2026-11-06T16:20:00.000Z", "clock": "4:20 PM", "day": "Nov 6" }
        },
        "epochs": {
            "2026-11-06T15:00:00.000Z": 1_793_977_200_000_i64,
            "2026-11-06T15:30:00.000Z": 1_793_979_000_000_i64,
            "2026-11-06T16:30:00.000Z": M1_NOW,
            "2026-11-06T16:25:00.000Z": M1_FAILED,
            "2026-11-06T16:20:00.000Z": M1_EARLIER
        },
        "order": {
            "anthropic/claude-opus-5-5|claude-code|xhigh": 0,
            "openai/gpt-6.1-sol|claude-code|xhigh": 1,
            "vm.lead--WN-1": 2,
            "mac.reviews--R1": 3
        }
    }))
    .unwrap();
    (data, facts)
}

fn m1_view(data: Value, facts: aac_status::read_context::Presentation) -> Value {
    aac_status::read_scoreboard::compose(aac_status::read_context::Envelope {
        data,
        presentation: facts,
    })
    .expect("view composes")
}

fn m1_row(view: &Value) -> &Value {
    let rows = view["uses"][0]["taskTypes"][0]["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["model"] == "anthropic/claude-opus-5-5")
        .expect("the Opus row");
    assert_eq!(
        row["ranked"],
        json!(true),
        "the row ranks; it is what carries the verdict"
    );
    assert_eq!(row["verdict"], json!("fine"));
    row
}

#[test]
fn a_failed_catalog_listing_is_disclosed_on_every_ranked_verdict() {
    let (data, facts) = m1_envelope(
        json!({"state": "error", "scannedAt": M1_FAILED, "problems": []}),
        json!([]),
        false,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    assert_eq!(view["sources"][1]["state"], json!("error"));
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("error"),
        "the retained catalog's failure: {row}"
    );
    assert_eq!(row["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(row["ageSeconds"].as_f64(), Some(300.0));
}

#[test]
fn a_failed_retained_review_file_is_disclosed_on_the_row_it_reviewed() {
    let (data, facts) = m1_envelope(
        json!({"state": "ok", "scannedAt": M1_NOW, "problems": []}),
        json!([
            { "file": "writers/mac.reviews/runs/2026-11-06/mac.reviews--R1.json", "reason": "contains NUL bytes from a damaged write", "since": M1_FAILED },
            { "file": "writers/mac.reviews/runs/2026-11-06/mac.reviews--never.json", "reason": "contains NUL bytes from a damaged write", "since": M1_EARLIER }
        ]),
        false,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("unreadable"),
        "the review still ranked it: {row}"
    );
    assert_eq!(row["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(
        row["ageSeconds"].as_f64(),
        Some(300.0),
        "a never-read damaged file adds no attribution (the earlier 16:20 problem must not win)"
    );
}

/// 2026-11-23 A: the WorkNode build filter joins its review from outside the filter, and that
/// joined review's file could not be read this pass. The verdict used the review, so it carries
/// the review source's failure (the reviewer's first scenario).
#[test]
fn a_joined_review_outside_the_filter_discloses_its_failed_file() {
    let (data, facts) = m1_envelope(
        json!({"state": "ok", "scannedAt": M1_NOW, "problems": []}),
        json!([
            { "file": "writers/mac.reviews/runs/2026-11-06/mac.reviews--R1.json", "reason": "contains NUL bytes from a damaged write", "since": M1_FAILED }
        ]),
        true,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("unreadable"),
        "the joined review still ranked it: {row}"
    );
    assert_eq!(row["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(row["ageSeconds"].as_f64(), Some(300.0));
}

/// 2026-11-23 A, second scenario: the review's dated folder could not be listed (the file name
/// itself is fine), and the verdict that used the review still carries the failure.
#[test]
fn a_failed_review_dated_folder_discloses_the_verdict_that_used_it() {
    let (data, facts) = m1_envelope(
        json!({"state": "ok", "scannedAt": M1_NOW, "problems": []}),
        json!([
            { "file": "writers/mac.reviews/runs/2026-11-06", "reason": "folder could not be listed", "since": M1_FAILED }
        ]),
        false,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("unreadable"),
        "the folder holds the review that ranked it: {row}"
    );
    assert_eq!(row["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(row["ageSeconds"].as_f64(), Some(300.0));
}

/// SB3 (2026-11-23 A): the Store now names a writer's `runs` folder it could not list. The review
/// under it is retained and still joins the filtered view, so the verdict that used it carries the
/// review source's failure (the folder is an ancestor of the review file).
#[test]
fn a_failed_writer_runs_folder_discloses_the_verdict_that_used_its_retained_review() {
    let (data, facts) = m1_envelope(
        json!({"state": "ok", "scannedAt": M1_NOW, "problems": []}),
        json!([
            { "file": "writers/mac.reviews/runs", "reason": "folder could not be listed", "since": M1_FAILED }
        ]),
        true,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("unreadable"),
        "the retained review still ranked it: {row}"
    );
    assert_eq!(row["asOf"], json!("2026-11-06T16:25:00.000Z"));
    assert_eq!(row["ageSeconds"].as_f64(), Some(300.0));
}

/// SB3 low (2026-11-23 A): a refused review file that still joins from its earlier good copy is
/// scored, so it is not counted as unscored; a refused file with no copy under the view or its
/// joined reviews is. The parity case 9 shape: one unreadable, not two.
#[test]
fn a_retained_joined_review_with_an_earlier_copy_is_not_counted_unreadable() {
    let (data, facts) = m1_envelope(
        json!({"state": "ok", "scannedAt": M1_NOW, "problems": []}),
        json!([
            { "file": "writers/mac.reviews/runs/2026-11-06/mac.reviews--R1.json", "reason": "contains NUL bytes from a damaged write", "since": M1_FAILED },
            { "file": "writers/mac.reviews/runs/2026-11-06/mac.reviews--never.json", "reason": "contains NUL bytes from a damaged write", "since": M1_FAILED }
        ]),
        true,
        M1_NOW,
    );
    let view = m1_view(data, facts);
    assert_eq!(
        view["unscored"]["unreadable"],
        json!(1),
        "the retained join review has an earlier good copy: {}",
        view["unscored"]
    );
    assert_eq!(
        view["sources"][0]["state"],
        json!("unreadable"),
        "the file-level failure stays in the runs Source: {}",
        view["sources"][0]
    );
    let row = m1_row(&view);
    assert_eq!(
        row["sourceState"],
        json!("unreadable"),
        "the verdict warning stays: {row}"
    );
}

/// 2026-11-23 B: a catalog listing that has failed for an hour ages the verdict by that hour, not
/// from the latest failed scan's `scannedAt`.
#[test]
fn a_continuing_catalog_failure_ages_the_verdict_from_its_first_failure() {
    let (data, facts) = m1_envelope(
        json!({"state": "error", "scannedAt": M1_HOUR, "failedSince": M1_FAILED, "problems": []}),
        json!([]),
        false,
        M1_HOUR,
    );
    let view = m1_view(data, facts);
    assert_eq!(view["sources"][1]["state"], json!("error"));
    let row = m1_row(&view);
    assert_eq!(row["sourceState"], json!("error"));
    assert_eq!(
        row["asOf"],
        json!("2026-11-06T16:25:00.000Z"),
        "the first failed listing, not the latest scan"
    );
    assert_eq!(row["ageSeconds"].as_f64(), Some(3600.0));
}
