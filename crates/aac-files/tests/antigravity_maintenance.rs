//! Exercise guarded Antigravity orphan and removal cleanup.
#![cfg(unix)]

use aac_files::profile_writes::LegacyLease;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn sweeps_only_owned_unnamed_antigravity_credentials() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let marker = root.join("antigravity-profiles/.transaction-lock");
    let instances = root.join("antigravity-instances");
    let name = format!("credential-{}.bin", "a".repeat(64));
    for dir in [&marker, &instances] {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    for id in ["orphan", "saved", "un_proved"] {
        let profile = instances.join(id);
        let host = profile.join("ubuntu");
        fs::create_dir_all(&host)?;
        for dir in [&profile, &host] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let key = host.join(&name);
        if id == "un_proved" {
            symlink(instances.join("saved/ubuntu").join(&name), key)?;
        } else {
            fs::File::create(&key)?;
            fs::set_permissions(key, fs::Permissions::from_mode(0o600))?;
        }
    }

    let lease = LegacyLease::verify(&marker, fs::File::open(&marker)?)?;
    let (removed, left_in_place) = lease.antigravity_sweep(&root, &["saved".into()])?;
    assert_eq!((removed, left_in_place), (1, 1));
    assert!(!instances.join("orphan").exists());
    assert!(instances.join("un_proved/ubuntu").join(&name).is_symlink());
    assert!(instances.join("saved/ubuntu").join(&name).is_file());
    assert_eq!(lease.antigravity_discard(&root, "un_proved")?, 1);
    Ok(())
}
