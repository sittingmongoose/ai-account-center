//! Exercise staged Codex removal under the activation lease.
#![cfg(unix)]

use aac_files::profile_writes::LegacyLease;
use std::{fs, os::unix::fs::symlink};

#[test]
fn preserves_only_the_staged_profile_under_its_activation_lease() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("codex");
    let marker = home.join(".ccs-activation.lock");
    fs::create_dir_all(&marker)?;
    let instances = root.join("codex-instances");
    let staged = "profile.deleting.123.test";
    let preserved = "profile.preserved.123.test";
    let source = instances.join(staged);
    let backup = instances.join(preserved);
    fs::create_dir_all(source.join("nested"))?;
    fs::write(source.join("nested/config.txt"), "fixture configuration")?;
    symlink("../not-in-profile", source.join("external-link"))?;
    let lease = LegacyLease::verify(&marker, fs::File::open(&marker)?)?;

    assert!(
        lease
            .codex_preserve(&root, &home, staged, "another.preserved.123.test")
            .is_err()
    );
    lease.codex_preserve(&root, &home, staged, preserved)?;
    assert_eq!(
        fs::read_to_string(backup.join("nested/config.txt"))?,
        "fixture configuration"
    );
    assert_eq!(
        fs::read_link(backup.join("external-link"))?,
        instances.join("not-in-profile")
    );
    assert!(
        lease
            .codex_preserve(&root, &home, staged, preserved)
            .is_err()
    );
    assert!(lease.codex_discard(&root, &home, "profile").is_err());
    lease.codex_discard(&root, &home, staged)?;
    assert!(!source.exists());
    assert!(backup.is_dir());
    Ok(())
}
