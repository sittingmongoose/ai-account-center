#![cfg(test)]
//! The in-agent app run: its lock, cancel and deadline, the U13 row whitelist and
//! the token scrub (Python `scrub`'s `_SECRET_PATTERNS`, minimal).
use super::*;
use serde_json::json;

fn job<'a>(root: &'a Path, platform: Platform, cancelled: bool) -> Job<'a> {
    Job {
        host: crate::apps::updater::tests::host(root, platform),
        state: root,
        platform,
        shared: Arc::new(Shared::default()),
        deadline: Instant::now() + Duration::from_secs(60),
        t3_row: None,
        cancelled: Arc::new(move || cancelled),
    }
}

#[test]
fn scrub_redacts_token_like_strings_and_keeps_ordinary_words() {
    for (input, expected) in [
        (
            "Authorization: Bearer abc.def-123",
            "Authorization: <redacted>",
        ),
        ("bearer abcdefgh12345678", "bearer <redacted>"),
        ("api_key=sk-live-123 rest", "api_key=<redacted>"),
        (
            "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig",
            "token <redacted>",
        ),
        ("ghp_abcdefghijklmnop1234", "<redacted>"),
        ("AKIAABCDEFGHIJKLMNOP", "<redacted>"),
        (
            "https://user:pass@example.com/path?x=1",
            "https://example.com/path",
        ),
        ("mail someone@example.com now", "mail <email> now"),
        ("0123456789abcdef0123456789abcdef", "<redacted>"),
        ("~/.local/bin/codex", "~/.local/bin/codex"),
        ("restart_context", "restart_context"),
    ] {
        assert_eq!(scrub(input), expected, "{input}");
    }
    assert_eq!(scrub(&"a".repeat(5000)), "<redacted><redacted>");
}

#[test]
fn sanitize_keeps_only_wire_fields_and_scrubs_diagnostics() {
    let row = json!({
        "appId": "omp", "platform": "ubuntu", "status": "failed", "previousVersion": "1",
        "version": "1", "manager": "native", "messageCode": "update_failed",
        "updateAttempted": true, "restartedProcesses": 0, "quotaReset": "5 PM",
        "output": "secret installer output",
        "diagnostics": {"cause": "unknown", "stderr": "leak",
                        "owner": {"path": "~/x/ghp_abcdefghijklmnop1234"}}
    });
    let clean = sanitize(row);
    assert!(clean.get("output").is_none());
    assert_eq!(clean["quotaReset"], "5 PM");
    assert!(clean["diagnostics"].get("stderr").is_none());
    assert_eq!(clean["diagnostics"]["owner"]["path"], "~/x/<redacted>");
}

#[tokio::test]
async fn a_held_app_lock_reports_busy_and_runs_nothing() {
    let root = tempfile::tempdir().expect("root");
    let _held = execution_lock(root.path(), "agent-worker-omp.lock").expect("lock");
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = run_app("omp", job(root.path(), Platform::Ubuntu, false), phases).await
    else {
        panic!("a row");
    };
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("failed"), Some("busy"))
    );
}

#[tokio::test]
async fn a_cancelled_job_skips_the_app_and_removes_its_stage() {
    let root = tempfile::tempdir().expect("root");
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = run_app("omp", job(root.path(), Platform::Ubuntu, true), phases).await
    else {
        panic!("a row");
    };
    assert_eq!(row["status"], "skipped");
    assert_eq!(row["messageCode"], "skipped_cancelled");
    assert!(!root.path().join("agent-staging/omp").exists());
}

#[tokio::test]
async fn an_absent_cli_is_not_installed_through_the_rust_updater() {
    let root = tempfile::tempdir().expect("root");
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = run_app("omp", job(root.path(), Platform::Ubuntu, false), phases).await
    else {
        panic!("a row");
    };
    assert_eq!(row["status"], "not_installed");
    assert_eq!(row["diagnostics"]["cause"], "none");
}

#[tokio::test]
async fn a_damaged_stage_is_never_removed_and_reports_readiness_unknown() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir_all(root.path().join("agent-staging/omp")).expect("stage");
    std::fs::write(root.path().join("agent-staging/omp/keep"), b"x").expect("file");
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = run_app("omp", job(root.path(), Platform::Ubuntu, false), phases).await
    else {
        panic!("a row");
    };
    assert_eq!(row["messageCode"], "readiness_unknown");
    assert!(root.path().join("agent-staging/omp/keep").exists());
}
