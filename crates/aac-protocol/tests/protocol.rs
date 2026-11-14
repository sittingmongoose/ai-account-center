//! Versioned wire envelope compatibility tests.
#![allow(clippy::unwrap_used)]
use aac_core::{Cause, Failure};
use aac_protocol::*;
use serde_json::json;

#[test]
fn request_wire_shape_and_validation() {
    let wire = json!({"protocol":1,"verb":"health","id":"fixture-1","deadlineMs":5000,"args":{}});
    let request: Request = serde_json::from_value(wire.clone()).unwrap();
    request.validate().unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), wire);
    for invalid in [
        json!({"protocol":2}),
        json!({"id":""}),
        json!({"deadlineMs":0}),
        json!({"deadlineMs":600001}),
        json!({"args":null}),
        json!({"verb":"sh -c"}),
    ] {
        let mut value = wire.clone();
        for (key, value2) in invalid.as_object().unwrap() {
            value[key] = value2.clone();
        }
        let request: Request = serde_json::from_value(value).unwrap();
        assert!(request.validate().is_err());
    }
}

#[test]
fn ndjson_events_are_tagged_and_failure_has_no_value() {
    let event = Event::Failure {
        protocol: 1,
        id: "fixture-1".into(),
        failure: Failure::new(Cause::Timeout, "expired"),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "failure");
    assert!(value.get("value").is_none());
    assert_eq!(serde_json::from_value::<Event>(value).unwrap(), event);
    assert!(matches!(
        serde_json::from_str::<Control>("{\"cancel\":true}").unwrap(),
        Control::Cancel { cancel: true }
    ));
}

#[test]
fn control_frames_refuse_mixed_or_unknown_fields() {
    assert!(
        serde_json::from_str::<Control>(r#"{"cancel":true,"input":"private fixture"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<Control>(r#"{"input":"private fixture","unknown":true}"#).is_err()
    );
}
