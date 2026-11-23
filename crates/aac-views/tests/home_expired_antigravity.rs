//! M1 (batch 5 review): Home's Antigravity columns are shared across accounts, so one account's
//! extra CLI pool (here Gemini 4 Argon) can be missing from another account. `antigravity-plan#3`
//! in the recorded corpus has exactly that shape; this test makes the second account's only CLI
//! meters passed-reset readings. A passed reset no longer describes the window, so it is not
//! evidence of a live reading: the missing cell must read "Unavailable", never "Not reported"
//! (the JavaScript view's `antigravityHasLiveReading`, web-dashboard/public/antigravity-data.mjs).
use aac_status::read_context::Envelope;
use aac_views::home::build;
use serde_json::{Value, json};

const POOL: &str = "cli-gemini-argon-wtu-weekly";
const ACCOUNT: &str = "antigravity:profile:free";

fn case(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/views/home/cases.json");
    let table: Value = serde_json::from_slice(&std::fs::read(&path).expect("cases.json"))
        .expect("cases.json json");
    table["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|case| case["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("case {name}"))
}

#[test]
fn a_passed_reset_reading_is_not_live_for_an_extra_pool() -> Result<(), String> {
    let case = case("antigravity-plan#3");
    let mut dashboard = case["dashboard"].clone();
    let accounts = dashboard["accounts"]
        .as_array_mut()
        .ok_or("dashboard.accounts")?;
    let account = accounts
        .iter_mut()
        .find(|account| account["id"] == ACCOUNT)
        .ok_or("the second account")?;
    let windows = account["windows"].as_array_mut().ok_or("account windows")?;
    assert!(!windows.is_empty(), "the account needs CLI meters");
    for window in windows {
        window["resetPassed"] = json!(true);
    }
    let envelope: Envelope = serde_json::from_value(json!({
        "data": {
            "ctx": case["ctx"],
            "dashboard": dashboard,
            "profiles": case["profiles"],
            "antigravity": case["antigravity"],
        },
        "presentation": case["presentation"],
    }))
    .map_err(|error| format!("envelope: {error}"))?;
    let view = serde_json::to_value(build(envelope).map_err(|decline| decline.0)?)
        .map_err(|error| error.to_string())?;
    let section = view["sections"]
        .as_array()
        .ok_or("sections")?
        .iter()
        .find(|section| section["id"] == "antigravity")
        .ok_or("the antigravity section")?;
    let at = section["columns"]
        .as_array()
        .ok_or("columns")?
        .iter()
        .position(|column| column["key"] == POOL)
        .ok_or("the extra pool column")?;
    let row = section["rows"]
        .as_array()
        .ok_or("rows")?
        .iter()
        .find(|row| row["id"] == ACCOUNT)
        .ok_or("the second account's row")?;
    let cell = &row["cells"].as_array().ok_or("cells")?[at];
    assert_eq!(
        cell["naText"], "Unavailable",
        "a passed-reset meter is not a live reading: {cell}"
    );
    assert_eq!(cell["hasValue"], false, "{cell}");
    Ok(())
}
