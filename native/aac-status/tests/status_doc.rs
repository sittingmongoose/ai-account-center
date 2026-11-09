//! The `status@1` validator against hand-written documents and the packaged examples.

use aac_status::status_doc::{LoopingState, parse_status_file};
use serde_json::{Value, json};

const WRITER: &str = "vm.example";

fn valid() -> Value {
    json!({
        "schema": "agent-reports/status@1",
        "writer": WRITER,
        "seq": 18,
        "writtenAt": "2026-11-08T16:00:00Z",
        "t3Thread": "00000000-0000-4000-8000-000000000001",
        "summary": "Windows 10 sweep for stream F is on batch 9. One question waits on the owner.",
        "percent": 70,
        "done": [{ "id": "F-08", "title": "Sticker block landed" }],
        "left": [{ "id": "F-07", "title": "Tray icon sharp at 125% scale" }],
        "blockers": [{ "text": "Pick the reviewers for batch B09", "waitingOn": "owner" }],
        "looping": { "state": "yes", "text": "F-07: 5 fix rounds, same finding" },
    })
}

fn parse(doc: &Value) -> Result<aac_status::status_doc::ParsedStatus, String> {
    parse_status_file(doc.to_string().as_bytes(), WRITER)
}

fn refused(doc: &Value) -> String {
    parse(doc).unwrap_err()
}

#[test]
fn a_full_document_projects_every_field() {
    let parsed = parse(&valid()).unwrap();
    let doc = &parsed.doc;
    assert_eq!(doc.writer, WRITER);
    assert_eq!(doc.seq, 18);
    assert_eq!(doc.written_at, "2026-11-08T16:00:00Z");
    assert_eq!(doc.t3_thread, "00000000-0000-4000-8000-000000000001");
    assert_eq!(doc.percent, Some(70));
    assert_eq!(doc.done[0].id, "F-08");
    assert_eq!(doc.left[0].title, "Tray icon sharp at 125% scale");
    assert_eq!(doc.blockers[0].waiting_on, "owner");
    let looping = doc.looping.as_ref().unwrap();
    assert_eq!(looping.state, LoopingState::Yes);
    assert_eq!(looping.text.as_deref(), Some("F-07: 5 fix rounds, same finding"));
    assert_eq!(parsed.sha256.len(), 64);
    assert!(parsed.sha256.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn unknown_fields_are_ignored_never_passed_through() {
    let mut doc = valid();
    doc["surprise"] = json!({ "nested": true });
    let parsed = parse(&doc).unwrap();
    let shown = serde_json::to_value(&parsed.doc).unwrap();
    assert!(shown.get("surprise").is_none());
}

#[test]
fn the_writer_must_match_its_folder_and_the_schema_must_match() {
    assert_eq!(
        parse_status_file(valid().to_string().as_bytes(), "vm.other").unwrap_err(),
        "writer does not match its folder"
    );
    let mut doc = valid();
    doc["schema"] = json!("agent-reports/status@2");
    assert_eq!(refused(&doc), "schema is missing or not valid");
    let mut doc = valid();
    doc["seq"] = json!(-1);
    assert_eq!(refused(&doc), "seq is missing or not valid");
    let mut doc = valid();
    doc["seq"] = json!(1.5);
    assert_eq!(refused(&doc), "seq is missing or not valid");
}

#[test]
fn written_at_must_be_a_real_calendar_moment() {
    for bad in [
        "2026-02-30T10:00:00Z",      // February never has 30 days
        "2026-02-29T10:00:00Z",      // 2026 is not a leap year
        "2026-13-01T10:00:00Z",      // no month 13
        "2026-11-08T24:00:00Z",      // no hour 24
        "2026-11-08T10:00:60Z",      // no leap second
        "2026-11-08T10:00:00+15:00", // offset past +14:00
        "2026-11-08 10:00:00Z",      // not the ISO shape
        "2026-11-08T10:00:00",       // no offset
    ] {
        let mut doc = valid();
        doc["writtenAt"] = json!(bad);
        assert_eq!(refused(&doc), "writtenAt is missing or not valid", "case {bad}");
    }
    for good in [
        "2024-02-29T10:00:00Z",
        "2026-11-08T10:00Z",
        "2026-11-08T10:00:00.250-05:00",
    ] {
        let mut doc = valid();
        doc["writtenAt"] = json!(good);
        assert!(parse(&doc).is_ok(), "case {good}");
    }
}

#[test]
fn percent_is_an_integer_from_0_to_100_or_null() {
    for bad in [json!(101), json!(-1), json!(70.5), json!("70")] {
        let mut doc = valid();
        doc["percent"] = bad.clone();
        assert_eq!(refused(&doc), "percent is missing or not valid", "case {bad}");
    }
    for (value, want) in [(json!(0), Some(0)), (json!(100), Some(100)), (Value::Null, None)] {
        let mut doc = valid();
        doc["percent"] = value;
        assert_eq!(parse(&doc).unwrap().doc.percent, want);
    }
    let mut doc = valid();
    doc.as_object_mut().unwrap().remove("percent");
    assert_eq!(parse(&doc).unwrap().doc.percent, None);
}

#[test]
fn looping_states_and_optional_pieces() {
    let mut doc = valid();
    doc["looping"] = json!({ "state": "maybe" });
    assert_eq!(refused(&doc), "looping.state is missing or not valid");

    let mut doc = valid();
    doc["looping"] = json!({ "state": "unsure", "text": "" });
    let looping = parse(&doc).unwrap().doc.looping.unwrap();
    assert_eq!(looping.state, LoopingState::Unsure);
    assert_eq!(looping.text, None);

    let mut doc = valid();
    doc.as_object_mut().unwrap().remove("looping");
    doc.as_object_mut().unwrap().remove("done");
    doc.as_object_mut().unwrap().remove("blockers");
    let parsed = parse(&doc).unwrap().doc;
    assert_eq!(parsed.looping, None);
    assert!(parsed.done.is_empty() && parsed.blockers.is_empty());
}

#[test]
fn lists_refuse_repeated_ids_and_bad_blockers() {
    let mut doc = valid();
    doc["done"] = json!([{ "id": "F-08", "title": "One" }, { "id": "F-08", "title": "Two" }]);
    assert_eq!(refused(&doc), "done repeat an id");

    let mut doc = valid();
    doc["blockers"] = json!([{ "text": "Waiting", "waitingOn": "the owner" }]);
    assert_eq!(refused(&doc), "blockers[0].waitingOn is missing or not valid");

    let mut doc = valid();
    doc["left"] = json!([{ "id": "F-07" }]);
    assert_eq!(refused(&doc), "left[0].title is missing or not valid");
}

#[test]
fn control_characters_and_oversize_text_are_refused() {
    let mut doc = valid();
    doc["summary"] = json!("Bad\u{7}bell");
    assert_eq!(refused(&doc), "summary is missing or not valid");

    let mut doc = valid();
    doc["summary"] = json!("x".repeat(501));
    assert_eq!(refused(&doc), "summary is missing or not valid");
}

#[test]
fn damaged_bytes_are_refused_with_the_shared_reasons() {
    assert_eq!(parse_status_file(b"", WRITER).unwrap_err(), "empty");
    assert_eq!(
        parse_status_file(b"{\"a\":\x00}", WRITER).unwrap_err(),
        "contains NUL bytes from a damaged write"
    );
    assert_eq!(parse_status_file(&[0xff, 0xfe], WRITER).unwrap_err(), "not UTF-8 text");
    assert_eq!(parse_status_file(b"not json", WRITER).unwrap_err(), "not valid JSON");
    assert_eq!(
        parse_status_file(&vec![b'a'; 64 * 1024 + 1], WRITER).unwrap_err(),
        "larger than the 64 KB cap"
    );
}

#[test]
fn anything_that_looks_like_a_secret_is_refused_whole() {
    // Token-shaped fixtures are joined at runtime so this source file holds none whole.
    let api_key = ["sk", "ant", "abcdefghijklmnopqrstuvwxyz0123456789"].join("-");
    let bearer_word = ["Bear", "er"].join("");
    let bearer = format!("Authorization: {bearer_word} abcdefghijklmnopqrstuvwxyz0123456789");
    let jwt = [
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        "signaturepart",
    ]
    .join(".");
    let bearer_kind = ["a ", "bear", "er token"].join("");
    let cases: Vec<(Value, String)> = vec![
        (
            json!({ "summary": format!("Use {api_key} for now.") }),
            "an API key".to_string(),
        ),
        (
            json!({ "summary": "Mail agent@example.com about it." }),
            "an email address".to_string(),
        ),
        (
            json!({ "note": { "password": "hunter22" } }),
            "a credential field".to_string(),
        ),
        (json!({ "note": bearer }), bearer_kind),
        (json!({ "note": jwt }), "a JSON web token".to_string()),
        (
            json!({ "note": "-----BEGIN RSA PRIVATE KEY-----" }),
            "a private key".to_string(),
        ),
    ];
    for (patch, kind) in cases {
        let mut doc = valid();
        for (key, value) in patch.as_object().unwrap() {
            doc[key] = value.clone();
        }
        assert_eq!(refused(&doc), format!("refused: it contains what looks like {kind}"));
    }
}

#[test]
fn a_non_ascii_letter_on_either_side_of_a_token_still_leaves_its_boundary() {
    // The TypeScript reader's `\b` is ASCII: a non-ASCII letter is not a word character to
    // it, so a token directly after one keeps its leading boundary (and before one, its
    // trailing). Every boundary here is ASCII the same way, so each family is refused with
    // a CJK letter on either side of the token. Token-shaped fixtures are joined at runtime
    // so this source file holds none whole.
    let cjk = "\u{5bc6}"; // a CJK letter a summary might carry
    let jwt = [
        "eyJhbGciOiJIUzI1NiJ5dCI6IkpXVCJ9",
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
        "signaturepart",
    ]
    .join(".");
    let families: Vec<(&str, String)> = vec![
        ("an API key", ["sk", "ant", &"a".repeat(24)].join("-")),
        ("an API key", ["sk", &"a".repeat(24)].join("-")),
        ("a GitHub token", format!("{}{}", ["gh", "p_"].join(""), "b".repeat(30))),
        ("a GitHub token", format!("{}{}", "github_pat_", "b".repeat(30))),
        ("a Slack token", format!("{}{}", "xoxa-", "c".repeat(12))),
        ("an AWS key", format!("{}{}", "AKIA", "0DEF".repeat(4))),
        ("a GitLab token", format!("{}{}", "glpat-", "e".repeat(22))),
        ("an npm token", format!("{}{}", "npm_", "f".repeat(36))),
        ("a Hugging Face token", format!("{}{}", "hf_", "g".repeat(32))),
        ("a Google API key", format!("{}{}", "AIza", "h".repeat(35))),
        ("a JSON web token", jwt),
        (
            "a bearer token",
            format!("{}{}", ["Bear", "er "].join(""), "i".repeat(24)),
        ),
        ("an email address", "agent@example.co".to_string()),
    ];
    for (kind, token) in families {
        for summary in [format!("{cjk}{token}"), format!("{token}{cjk}")] {
            let mut doc = valid();
            doc["summary"] = json!(summary);
            assert_eq!(
                parse(&doc).unwrap_err(),
                format!("refused: it contains what looks like {kind}"),
                "token family {kind} with a non-ASCII letter around it"
            );
        }
    }
}

#[test]
fn an_escaped_secret_is_still_a_secret() {
    // The file as written hides the token behind a \\u escape; the decoded walk sees it.
    let bytes = br#"{"schema":"agent-reports/status@1","writer":"vm.example","seq":1,"writtenAt":"2026-11-08T16:00:00Z","t3Thread":"t-1","summary":"key is \u0073k-ant-abcdefghijklmnopqrstuvwxyz0123456789 ok"}"#;
    assert_eq!(
        parse_status_file(bytes, WRITER).unwrap_err(),
        "refused: it contains what looks like an API key"
    );
    // And JSON text inside a string is walked too.
    let mut doc = valid();
    doc["summary"] = json!("Working.");
    doc["note"] = json!("{\"password\": \"hunter22\"}");
    assert_eq!(refused(&doc), "refused: it contains what looks like a credential field");
}

#[test]
fn the_packaged_examples_agree_with_the_validator() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agent-reports/examples");
    let good = std::fs::read(examples.join("status.valid.json")).unwrap();
    assert!(parse_status_file(&good, WRITER).is_ok());
    for name in [
        "status.invalid-bad-looping-state.json",
        "status.invalid-missing-summary.json",
    ] {
        let bytes = std::fs::read(examples.join(name)).unwrap();
        assert!(parse_status_file(&bytes, WRITER).is_err(), "{name} must be refused");
    }
}
