//! Credential data arrives on a private pipe. No command echoes input or filesystem errors.
use aac_files::profile_writes::{LegacyLease, LoginInstallError, MAX_PROFILE_BYTES};
use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsFd,
    path::Path,
};

pub(crate) fn commit() -> i32 {
    let args: Vec<_> = std::env::args_os().skip(2).take(7).collect();
    let Some(args) = args
        .iter()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return 74;
    };
    let Ok(fd) = io::stderr().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let descriptor = File::from(fd);
    let mut input = Vec::new();
    if io::stdin()
        .take((MAX_PROFILE_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > MAX_PROFILE_BYTES
    {
        return 74;
    }
    let result = match args.as_slice() {
        ["codex-metadata", root] => {
            let marker = format!("{root}.lock");
            LegacyLease::verify(Path::new(&marker), descriptor)
                .and_then(|lease| lease.codex_metadata(Path::new(root), &input))
        }
        ["codex-move", root, home, previous, next] if input.is_empty() => {
            let home = Path::new(home);
            LegacyLease::verify(&home.join(".ccs-activation.lock"), descriptor)
                .and_then(|lease| lease.codex_move(Path::new(root), home, previous, next))
        }
        ["codex-login", root, home, profile, mode] if matches!(*mode, "saved" | "live") => {
            let home = Path::new(home);
            let Ok(lease) = LegacyLease::verify(&home.join(".ccs-activation.lock"), descriptor)
            else {
                return 75;
            };
            return match lease.codex_login(Path::new(root), home, profile, *mode == "live", &input)
            {
                Ok(()) => 0,
                Err(LoginInstallError::Refused) => 75,
                Err(LoginInstallError::LiveNotUpdated) => 78,
            };
        }
        ["antigravity-credential", root, profile, fingerprint] => {
            let root = Path::new(root);
            LegacyLease::verify(
                &root.join("antigravity-profiles/.transaction-lock"),
                descriptor,
            )
            .and_then(|lease| lease.antigravity_credential(root, profile, fingerprint, &input))
        }
        ["antigravity-index", root, generation] => {
            let root = Path::new(root);
            LegacyLease::verify(
                &root.join("antigravity-profiles/.transaction-lock"),
                descriptor,
            )
            .and_then(|lease| lease.antigravity_index(root, generation, &input))
        }
        _ => return 74,
    };
    if result.is_ok() { 0 } else { 75 }
}
