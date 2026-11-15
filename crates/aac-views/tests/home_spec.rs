//! D7 Home spec: every case of tests/spec/views/home/cases.json (the dashboard's own JS tests, recorded, plus the
//! synthetic zone and decline cases) built by `aac_views::home::build` from the case's envelope (its inputs and the
//! presentation facts `homeReadInput` computed for them). A built view must be byte-equal to the JS view; a decline
//! must be the one declines.json names for that case, and only those cases may decline.
use aac_status::read_context::Envelope;
use aac_views::home::build;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Table {
    version: u32,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    dashboard: serde_json::Value,
    profiles: serde_json::Value,
    antigravity: serde_json::Value,
    ctx: serde_json::Value,
    /// `homeReadInput(...).presentation` (src/web-server/views/home-context.ts).
    presentation: serde_json::Value,
    /// `JSON.stringify` of the JS view, or null when the JS builder throws for this input.
    expected: Option<String>,
}

fn spec_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/views/home")
}

/// The envelope the front door sends for this case.
fn envelope_of(case: &Case) -> Result<Envelope, String> {
    serde_json::from_value(serde_json::json!({
        "data": {
            "ctx": case.ctx,
            "dashboard": case.dashboard,
            "profiles": case.profiles,
            "antigravity": case.antigravity,
        },
        "presentation": case.presentation,
    }))
    .map_err(|e| format!("{}: envelope {e}", case.name))
}

/// Where two texts first differ, with a little of each around it.
fn first_difference(want: &str, got: &str) -> String {
    let at = want
        .char_indices()
        .zip(got.chars())
        .find(|((_, a), b)| a != b)
        .map(|((i, _), _)| i)
        .unwrap_or(want.len().min(got.len()));
    let from = want[..at]
        .char_indices()
        .rev()
        .nth(80)
        .map_or(0, |(i, _)| i);
    let tail = |s: &str| {
        s.get(from..)
            .unwrap_or("")
            .chars()
            .take(200)
            .collect::<String>()
    };
    format!(
        "at byte {at}\n  js:   {}\n  rust: {}",
        tail(want),
        tail(got)
    )
}

#[test]
fn every_home_case_is_equal_or_declined_as_listed() -> Result<(), String> {
    let read = |name: &str| {
        std::fs::read_to_string(spec_dir().join(name)).map_err(|e| format!("{name}: {e}"))
    };
    let table: Table =
        serde_json::from_str(&read("cases.json")?).map_err(|e| format!("cases.json: {e}"))?;
    assert_eq!(table.version, 1, "cases.json version");
    let declines: BTreeMap<String, String> =
        serde_json::from_str(&read("declines.json")?).map_err(|e| format!("declines.json: {e}"))?;
    let mut failures = Vec::new();
    let mut seen_declines = BTreeMap::new();
    let (mut built, mut declined) = (0, 0);
    let dump = std::env::var_os("HOME_SPEC_DUMP");
    let mut dumped = serde_json::Map::new();
    for case in &table.cases {
        let result = build(envelope_of(case)?);
        match (result, declines.get(&case.name)) {
            (Ok(view), None) => {
                let got = serde_json::to_string(&view).map_err(|e| e.to_string())?;
                if dump.is_some() {
                    dumped.insert(case.name.clone(), got.clone().into());
                }
                match &case.expected {
                    Some(want) if *want == got => built += 1,
                    Some(want) => failures.push(format!(
                        "{}: differs {}",
                        case.name,
                        first_difference(want, &got)
                    )),
                    None => {
                        failures.push(format!("{}: built a view where the JS throws", case.name))
                    }
                }
            }
            (Ok(_), Some(word)) => failures.push(format!(
                "{}: built, but declines.json says {word}",
                case.name
            )),
            (Err(word), Some(want)) if word.0 == want => {
                declined += 1;
                seen_declines.insert(word.0, ());
            }
            (Err(word), want) => failures.push(format!(
                "{}: declined {} (declines.json: {want:?})",
                case.name, word.0
            )),
        }
    }
    if let Some(path) = dump {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&dumped).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    for name in declines.keys() {
        if !table.cases.iter().any(|c| &c.name == name) {
            failures.push(format!("declines.json names {name}, which is not a case"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        table.cases.len(),
        failures.join("\n")
    );
    assert!(table.cases.len() >= 100, "only {} cases", table.cases.len());
    // Most of the table must build: a builder that declines everything would pass the rest of this test.
    assert!(
        built * 4 >= table.cases.len() * 3,
        "only {built} of {} cases built",
        table.cases.len()
    );
    println!(
        "{built} built equal, {declined} declined as listed ({} decline words)",
        seen_declines.len()
    );
    Ok(())
}

/// `HOME_SPEC_FUZZ=<file>` (tests/spec/views/home/fuzz.mjs): any decline is allowed, but a built view must be the JS
/// view byte for byte, and the JS must not have thrown for it. Without the variable this test has nothing to read.
#[test]
fn fuzzed_cases_are_equal_or_declined() -> Result<(), String> {
    let Some(path) = std::env::var_os("HOME_SPEC_FUZZ") else {
        return Ok(());
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path:?}: {e}"))?;
    let table: Table = serde_json::from_str(&text).map_err(|e| format!("fuzz table: {e}"))?;
    let mut failures = Vec::new();
    let mut words: BTreeMap<&str, usize> = BTreeMap::new();
    let mut built = 0;
    for case in &table.cases {
        match build(envelope_of(case)?) {
            Ok(view) => {
                let got = serde_json::to_string(&view).map_err(|e| e.to_string())?;
                match &case.expected {
                    Some(want) if *want == got => built += 1,
                    Some(want) => failures.push(format!(
                        "{}: differs {}",
                        case.name,
                        first_difference(want, &got)
                    )),
                    None => {
                        failures.push(format!("{}: built a view where the JS throws", case.name))
                    }
                }
            }
            Err(word) => *words.entry(word.0).or_default() += 1,
        }
    }
    println!(
        "{built} of {} fuzzed cases built equal; declines {words:?}",
        table.cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} fuzzed cases failed:\n{}",
        failures.len(),
        table.cases.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}
