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
        ["codex-preserve", root, home, staged, backup] if input.is_empty() => {
            let home = Path::new(home);
            LegacyLease::verify(&home.join(".ccs-activation.lock"), descriptor)
                .and_then(|lease| lease.codex_preserve(Path::new(root), home, staged, backup))
        }
        ["codex-discard", root, home, target] if input.is_empty() => {
            let home = Path::new(home);
            LegacyLease::verify(&home.join(".ccs-activation.lock"), descriptor)
                .and_then(|lease| lease.codex_discard(Path::new(root), home, target))
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

pub(crate) fn antigravity_maintenance() -> i32 {
    #[derive(serde::Deserialize)]
    #[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
    enum Request {
        Discard { profile: String },
        Sweep { saved: Vec<String> },
    }
    let mut args = std::env::args_os().skip(2);
    let (Some(root), None) = (args.next(), args.next()) else {
        return 74;
    };
    let root = Path::new(&root);
    let Ok(descriptor) = io::stderr().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let Ok(lease) = LegacyLease::verify(
        &root.join("antigravity-profiles/.transaction-lock"),
        File::from(descriptor),
    ) else {
        return 74;
    };
    let mut bytes = Vec::new();
    if io::stdin().take(16_385).read_to_end(&mut bytes).is_err() || bytes.len() > 16_384 {
        return 74;
    }
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return 74;
    };
    let result = match request {
        Request::Discard { profile } => lease
            .antigravity_discard(root, &profile)
            .map(|left| (0, left)),
        Request::Sweep { saved } => lease.antigravity_sweep(root, &saved),
    };
    match result {
        Ok((removed, left)) => {
            println!("{removed} {left}");
            0
        }
        Err(_) => 75,
    }
}

pub(crate) fn staging(antigravity: bool) -> i32 {
    let args: Vec<_> = std::env::args_os().skip(2).take(6).collect();
    let Some(args) = args
        .iter()
        .map(|arg| arg.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return 74;
    };
    use aac_files::profile_staging;
    let outcome = match (antigravity, args.as_slice()) {
        (true, ["marker-claim", root, name, temp]) => {
            let mut holder = Vec::new();
            if io::stdin().take(513).read_to_end(&mut holder).is_err() {
                return 74;
            }
            profile_staging::claim_antigravity_marker(Path::new(root), name, temp, &holder)
                .map(usize::from)
        }
        (true, ["marker-remove", root, name, dev, ino]) => match (dev.parse(), ino.parse()) {
            (Ok(dev), Ok(ino)) => {
                profile_staging::remove_antigravity_marker(Path::new(root), name, dev, ino)
                    .map(usize::from)
            }
            _ => return 74,
        },
        (true, ["prepare", root, name]) => {
            let mut bus = Vec::new();
            if io::stdin().take(4097).read_to_end(&mut bus).is_err() {
                return 74;
            }
            profile_staging::prepare_antigravity(Path::new(root), name, &bus).map(|()| 0)
        }
        (true, ["cleanup", root, name]) => {
            profile_staging::cleanup_antigravity(Path::new(root), name).map(|()| 0)
        }
        (true, ["sweep", root, now]) => match now.parse() {
            Ok(now) => profile_staging::sweep_antigravity(Path::new(root), now),
            Err(_) => return 74,
        },
        (false, ["prepare", root, name, shared]) => {
            aac_files::profile_staging::prepare(Path::new(root), name, Path::new(shared))
                .map(|()| 0)
        }
        (false, ["cleanup", root, name]) => {
            aac_files::profile_staging::cleanup(Path::new(root), name).map(|()| 0)
        }
        (false, ["sweep", root, now]) => match now.parse() {
            Ok(now) => aac_files::profile_staging::sweep(Path::new(root), now),
            Err(_) => return 74,
        },
        _ => return 74,
    };
    match outcome {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(_) => 75,
    }
}
