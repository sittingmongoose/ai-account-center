use aac_status::ops;
use serde_json::Value;

fn answer(method: &str, args: &str) -> Value {
    serde_json::from_str(&ops::dispatch(method, args).unwrap()).unwrap()
}

#[test]
fn unknown_method_is_refused_with_a_fixed_code() {
    let error = ops::dispatch("nope", "{}").unwrap_err();
    assert_eq!(error.code, "unknown_method");
}

#[test]
fn remote_pin_answers_round_trip_through_the_dispatch() {
    let ok = answer(
        "parseRemotePins",
        r#"{ "machine": "mac", "json": "[{\"thread_id\":\"t\",\"title\":\"T\",\"pinnedAt\":\"x\",\"pinnedType\":\"text\",\"pinOrderKey\":\"k\",\"orderType\":\"text\",\"rootType\":\"object\",\"lineageType\":null,\"parentType\":null}]" }"#,
    );
    assert_eq!(ok["ok"], serde_json::json!(true));
    assert_eq!(ok["pinned"][0]["id"], "t");

    let refused = answer("parseRemotePins", r#"{ "machine": "mac", "json": "not json" }"#);
    assert_eq!(refused["ok"], serde_json::json!(false));
    assert_eq!(refused["reason"], "shape");

    let wrong_machine = ops::dispatch("parseRemotePins", r#"{ "machine": "vm", "json": "[]" }"#);
    assert_eq!(wrong_machine.unwrap_err().code, "invalid");
}

#[test]
fn merged_pins_come_out_tagged_and_ordered() {
    let merged = answer(
        "mergePins",
        r#"{ "lists": [
              { "machine": "mac", "pinned": [
                  { "id": "b", "title": "B", "pinnedAt": "x", "pinOrderKey": "b" } ] },
              { "machine": "vm", "pinned": [
                  { "id": "a", "title": "A", "pinnedAt": "x", "pinOrderKey": "a" } ] }
        ] }"#,
    );
    assert_eq!(merged["pinned"][0]["id"], "a");
    assert_eq!(merged["pinned"][0]["machine"], "vm");
    assert_eq!(merged["pinned"][1]["machine"], "mac");

    let unknown = ops::dispatch("mergePins", r#"{ "lists": [ { "machine": "laptop", "pinned": [] } ] }"#);
    assert_eq!(unknown.unwrap_err().code, "invalid");
}

#[test]
fn compose_answers_with_the_updates_and_the_head() {
    let answer = answer(
        "compose",
        r#"{ "nowMs": 1794116000000, "utcOffsetMinutes": 0,
             "pins": [ { "machine": "vm", "id": "t1", "title": "Thread t1",
                         "pinnedAt": "x", "pinOrderKey": "a" } ],
             "machineProblems": [ { "machine": "mac", "code": "did_not_answer" } ] }"#,
    );
    assert_eq!(answer["head"], serde_json::json!("1 pinned in T3"));
    assert_eq!(answer["updates"][0]["reportState"], "none");
    assert_eq!(
        answer["updates"][0]["reportNote"],
        serde_json::json!("This thread doesn't report its status yet.")
    );
    assert_eq!(
        answer["machineProblems"][0]["text"],
        serde_json::json!("Pinned threads unavailable on the Mac: it did not answer.")
    );

    // Input this crate did not build is a rejected call, never a guess.
    let bad = ops::dispatch("compose", r#"{ "nowMs": "soon" }"#);
    assert_eq!(bad.unwrap_err().code, "invalid");
}
