//! Offline snapshot reader parity checks.
use aac_snapshot::reader::{Options, filter_snapshot};
use serde_json::json;
#[test]
fn filters_aliases_hidden_accounts_and_expired_paces() {
    let now = 1_800_000_000_000;
    let data = json!({"accounts":[
        {"provider":"codex","label":"Work","sampledAt":"2020-01-01T00:00:00Z","windows":[{"resetAt":"2020-01-02T00:00:00Z","burn":{"pace":1}}]},
        {"provider":"codex","hidden":true}, {"provider":"claude"}],
        "signIn":[{"provider":"codex"},{"provider":"claude"}]});
    let opts = Options::parse(&["--provider".into(), "openai".into()])
        .expect("valid offline reader fixture");
    let out = filter_snapshot(data, &opts, now);
    assert_eq!(
        out["accounts"]
            .as_array()
            .expect("valid offline reader fixture")
            .len(),
        1
    );
    assert_eq!(
        out["signIn"]
            .as_array()
            .expect("valid offline reader fixture")
            .len(),
        1
    );
    assert_eq!(out["accounts"][0]["windows"][0]["resetPassed"], true);
    assert!(out["accounts"][0]["windows"][0]["burn"].is_null());
}
#[test]
fn reading_at_reset_keeps_pace_and_unknown_time_removes_it() {
    let opts = Options::parse(&[]).expect("valid offline reader fixture");
    let data = json!({"accounts":[{"sampledAt":"2027-01-15T08:00:00Z","windows":[{"resetAt":"2027-01-15T08:00:00Z","burn":{"pace":5}}]}]});
    let now = aac_snapshot::js::date_parse_str("2027-01-15T08:00:01Z")
        .expect("valid offline reader fixture");
    let out = filter_snapshot(data, &opts, now);
    assert_eq!(out["accounts"][0]["windows"][0]["burn"]["pace"], 5);
    let out = filter_snapshot(
        json!({"accounts":[{"windows":[{"burn":{"pace":5}}]}]}),
        &opts,
        now,
    );
    assert!(out["accounts"][0]["windows"][0]["burn"].is_null());
}
#[test]
fn python_oracle_text_and_json() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reader-corpus.json"))
            .expect("valid offline reader fixture");
    let folder = tempfile::Builder::new()
        .prefix("aac-reader-")
        .tempdir()
        .expect("valid offline reader fixture")
        .keep();
    let now = aac_snapshot::js::date_parse_str("2027-01-15T08:00:00Z")
        .expect("valid offline reader fixture");
    for (i, c) in cases
        .as_array()
        .expect("valid offline reader fixture")
        .iter()
        .enumerate()
    {
        let folder = folder.join(i.to_string());
        std::fs::create_dir(&folder).expect("valid offline reader fixture");
        for (key, name) in [
            ("history", "history.json"),
            ("signins", "claude-signins.json"),
        ] {
            if !c[key].is_null() {
                std::fs::write(
                    folder.join(name),
                    serde_json::to_vec(&c[key]).expect("valid offline reader fixture"),
                )
                .expect("valid offline reader fixture");
            }
        }
        std::fs::write(
            folder.join("usage.json"),
            serde_json::to_vec(&c["snapshot"]).expect("valid offline reader fixture"),
        )
        .expect("valid offline reader fixture");
        let mut args = c["args"]
            .as_array()
            .expect("valid offline reader fixture")
            .iter()
            .map(|s| s.as_str().expect("valid offline reader fixture").to_owned())
            .collect::<Vec<_>>();
        args.extend(["--dir".into(), folder.display().to_string()]);
        let opts = Options::parse(&args).expect("valid offline reader fixture");
        let actual = aac_snapshot::reader::ascii(
            &aac_snapshot::reader::run(&opts, now).expect("valid offline reader fixture"),
        )
        .replace(&folder.display().to_string(), "/snapshot")
        .replace("/snapshot\\usage.json", "/snapshot/usage.json");
        if opts.json {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&actual)
                    .expect("valid offline reader fixture"),
                serde_json::from_str::<serde_json::Value>(
                    c["expected"]
                        .as_str()
                        .expect("valid offline reader fixture")
                )
                .expect("valid offline reader fixture"),
                "case {i}"
            );
        } else {
            assert_eq!(
                actual,
                c["expected"]
                    .as_str()
                    .expect("valid offline reader fixture"),
                "case {i}"
            );
        }
    }
}
