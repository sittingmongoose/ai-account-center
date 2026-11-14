//! Store-root identity must remain fixed across cwd changes and independent leases.
#![allow(clippy::unwrap_used)]
use aac_core::Cause;
use aac_files::*;
use std::path::Path;
fn registry() -> Registry {
    let mut registry = Registry::default();
    registry
        .register(StoreSpec {
            path: "state.json".into(),
            class: Class::State,
            mode: 0o600,
            max_size: 100,
            owner: "writer".into(),
            schema: "fixture.v1".into(),
            backup: BackupClass::Snapshot,
        })
        .unwrap();
    registry
}
#[test]
fn max1_relative_root_cannot_redirect_a_held_writer() {
    if let Some(root) = std::env::var_os("AAC_MAX1_STORE_CHILD") {
        let root = std::path::PathBuf::from(root);
        let registry = registry();
        let attempt = registry.claim(Path::new("private"), Path::new("state.json"), "writer");
        let rejected = attempt.is_err();
        let first = match attempt {
            Ok(store) => store,
            Err(failure) => {
                assert_eq!(failure.cause, Cause::Refused);
                assert!(
                    !root.join("a/private").exists(),
                    "relative root created directories before refusal"
                );
                registry
                    .claim(&root.join("a/private"), Path::new("state.json"), "writer")
                    .unwrap()
            }
        };
        first.write(b"original A").unwrap();
        let second = registry
            .claim(&root.join("b/private"), Path::new("state.json"), "writer")
            .unwrap();
        second.write(b"original B").unwrap();
        std::env::set_current_dir(root.join("b")).unwrap();
        first.write(b"updated A").unwrap();
        assert_eq!(
            second.read().unwrap(),
            b"original B",
            "A lease overwrote separately locked B"
        );
        assert_eq!(first.read().unwrap(), b"updated A");
        assert!(rejected, "relative roots must be refused");
        assert!(
            matches!(registry.claim(&root.join("a/../b/private"),Path::new("state.json"),"writer"),Err(error) if error.cause==Cause::Refused)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("a")).unwrap();
    std::fs::create_dir(root.path().join("b")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "max1_relative_root_cannot_redirect_a_held_writer",
        ])
        .current_dir(root.path().join("a"))
        .env("AAC_MAX1_STORE_CHILD", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
