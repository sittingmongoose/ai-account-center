use super::*;

#[tokio::test]
async fn rejects_callers_selecting_a_path_or_foreign_platform() {
    for invalid in [
        json!({"action":"inventory","platform":"windows","path":"fixture"}),
        json!({"action":"inventory","platform":"nas1"}),
        json!({"action":"inventory","platform":null}),
        json!({"action":"inventory","platform":if cfg!(windows) { "mac" } else { "windows" }}),
    ] {
        assert_eq!(
            run_inventory(&invalid).await.expect_err("refused").cause,
            Cause::Refused
        );
    }
}

#[test]
fn job_ids_cannot_select_a_path() {
    assert!(valid_job("cdc2d248a04f48a894be80bd777d866d"));
    for invalid in ["", "../../fixture", "cDc2d248a04f48a894be80bd777d866d"] {
        assert!(!valid_job(invalid));
    }
}

#[test]
fn version_matches_python_grammar() {
    assert_eq!(
        version(Some("codex-cli 0.50.0-alpha-1")).as_deref(),
        Some("0.50.0-alpha-1")
    );
    assert_eq!(
        version(Some("codex-cli 0.50.0+build.1")).as_deref(),
        Some("0.50.0+build.1")
    );
    assert_eq!(version(Some("0.50.0")).as_deref(), Some("0.50.0"));
    assert_eq!(
        version(Some("0.50.0-alpha.1")).as_deref(),
        Some("0.50.0-alpha.1")
    );
}
