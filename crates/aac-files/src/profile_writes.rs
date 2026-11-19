//! Provider files whose existing transaction lock also coordinates the TypeScript fallback.
use super::{read_bounded, require_absolute, set_private, sync_directory};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::Path;
use tempfile::NamedTempFile;

/// Upper bound for individual private provider inputs.
pub const MAX_PROFILE_BYTES: usize = 1024 * 1024;

fn refused() -> io::Error {
    io::Error::other("provider file write refused")
}

pub(crate) fn owned_directory(dir: &Path) -> io::Result<()> {
    require_absolute(dir).map_err(|_| refused())?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir()
        || meta.uid() != super::path::uid()
        || meta.mode() & 0o002 != 0
        || fs::canonicalize(dir)? != dir
    {
        return Err(refused());
    }
    Ok(())
}

/// An inherited descriptor of the exact marker created by the existing lock protocol.
pub struct LegacyLease(File);

impl LegacyLease {
    /// Verify a marker name still identifies the caller's open directory.
    pub fn verify(marker: &Path, descriptor: File) -> io::Result<Self> {
        let lease = Self(descriptor);
        lease.check(marker)?;
        Ok(lease)
    }

    fn check(&self, marker: &Path) -> io::Result<()> {
        owned_directory(marker)?;
        let held = self.0.metadata()?;
        let named = fs::symlink_metadata(marker)?;
        if !held.is_dir() || held.dev() != named.dev() || held.ino() != named.ino() {
            return Err(refused());
        }
        Ok(())
    }

    /// Publish a validated YAML registry while its proper-lockfile directory is held.
    pub fn codex_metadata(&self, root: &Path, bytes: &[u8]) -> io::Result<()> {
        owned_directory(root)?;
        let mut marker = root.as_os_str().to_os_string();
        marker.push(".lock");
        self.check(Path::new(&marker))?;
        replace(stage(root, bytes)?, &root.join("codex-profiles.yaml"))
    }

    /// Move a profile or staging directory to a vacant name under the activation lock.
    pub fn codex_move(&self, root: &Path, home: &Path, old: &str, new: &str) -> io::Result<()> {
        self.check(&home.join(".ccs-activation.lock"))?;
        if !codex_location(old) || !codex_location(new) {
            return Err(refused());
        }
        let parent = root.join("codex-instances");
        owned_directory(&parent)?;
        let previous = parent.join(old);
        let next = parent.join(new);
        owned_directory(&previous)?;
        match fs::symlink_metadata(&next) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => return Err(refused()),
            Err(error) => return Err(error),
        }
        fs::rename(previous, next)?;
        let _ = sync_directory(&parent);
        Ok(())
    }

    /// Preserve one staged removal under the same activation lease. The backup
    /// must be vacant and must name the same profile as the staged directory.
    pub fn codex_preserve(
        &self,
        root: &Path,
        home: &Path,
        staged: &str,
        backup: &str,
    ) -> io::Result<()> {
        self.check(&home.join(".ccs-activation.lock"))?;
        let profile = codex_removal_name(staged, ".deleting.").ok_or_else(refused)?;
        if codex_removal_name(backup, ".preserved.") != Some(profile) {
            return Err(refused());
        }
        let parent = root.join("codex-instances");
        let source = parent.join(staged);
        let target = parent.join(backup);
        owned_directory(&source)?;
        super::private_builder().create(&target)?;
        match copy_profile_entries(&source, &target) {
            Ok(()) => {
                let _ = sync_directory(&parent);
                Ok(())
            }
            Err(error) => {
                let _ = fs::remove_dir_all(target);
                Err(error)
            }
        }
    }

    /// Discard a staged removal or a preservation copy. The live profile
    /// name and sign-in staging area are outside this operation's scope.
    pub fn codex_discard(&self, root: &Path, home: &Path, name: &str) -> io::Result<()> {
        self.check(&home.join(".ccs-activation.lock"))?;
        if codex_removal_name(name, ".deleting.").is_none()
            && codex_removal_name(name, ".preserved.").is_none()
        {
            return Err(refused());
        }
        let parent = root.join("codex-instances");
        owned_directory(&parent)?;
        let target = parent.join(name);
        match owned_directory(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        fs::remove_dir_all(target)?;
        let _ = sync_directory(&parent);
        Ok(())
    }

    /// Replace an attested saved login and, when required, the same live login.
    pub fn codex_login(
        &self,
        root: &Path,
        home: &Path,
        profile: &str,
        live: bool,
        bytes: &[u8],
    ) -> Result<(), LoginInstallError> {
        self.check(&home.join(".ccs-activation.lock"))?;
        if !codex_profile(profile) {
            return Err(LoginInstallError::Refused);
        }
        let profile = root.join("codex-instances").join(profile).join("auth.json");
        let live = live.then(|| home.join("auth.json"));
        install_login(&profile, live.as_deref(), bytes, replace)
    }

    /// Save a new immutable Antigravity credential generation under its transaction lock.
    pub fn antigravity_credential(
        &self,
        root: &Path,
        profile: &str,
        fingerprint: &str,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.check(&root.join("antigravity-profiles/.transaction-lock"))?;
        if !antigravity_profile(profile)
            || fingerprint.len() != 64
            || !fingerprint.bytes().all(lower_hex)
        {
            return Err(refused());
        }
        let parent = root
            .join("antigravity-instances")
            .join(profile)
            .join("ubuntu");
        let file = parent.join(format!("credential-{fingerprint}.bin"));
        publish(&parent, &file, bytes)
    }

    /// Publish a validated Antigravity state revision without ever replacing a revision.
    pub fn antigravity_index(&self, root: &Path, generation: &str, bytes: &[u8]) -> io::Result<()> {
        let parent = root.join("antigravity-profiles");
        self.check(&parent.join(".transaction-lock"))?;
        if generation.len() != 12 || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(refused());
        }
        let file = parent.join(format!("registry-{generation}.json"));
        publish(&parent, &file, bytes)
    }
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn codex_profile(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.len() <= 64
        && !matches!(name, "default" | "current")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn codex_location(name: &str) -> bool {
    if let Some(id) = name.strip_prefix(".staging-job_") {
        return id.len() == 16 && id.bytes().all(lower_hex);
    }
    if let Some((profile, suffix)) = name
        .split_once(".deleting.")
        .or_else(|| name.split_once(".preserved."))
    {
        return codex_profile(profile)
            && suffix.split_once('.').is_some_and(|(pid, tag)| {
                !pid.is_empty()
                    && pid.bytes().all(|b| b.is_ascii_digit())
                    && !tag.is_empty()
                    && tag
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            });
    }
    codex_profile(name)
}

fn codex_removal_name<'a>(name: &'a str, divider: &str) -> Option<&'a str> {
    let (profile, _) = name.split_once(divider)?;
    codex_location(name).then_some(profile)
}

fn copy_profile_entries(source: &Path, target: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let expected = fs::symlink_metadata(&from)?;
        if expected.file_type().is_symlink() {
            let link = fs::read_link(&from)?;
            // Match the legacy copy's absolute link destination, without following it.
            let link = crate::path::node::resolve(source, &link).map_err(|_| refused())?;
            symlink(link, &to)?;
        } else if expected.is_dir() {
            owned_directory(&from)?;
            super::private_builder().create(&to)?;
            copy_profile_entries(&from, &to)?;
        } else if expected.is_file() {
            let mut input = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&from)?;
            let actual = input.metadata()?;
            if !actual.is_file() || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
            {
                return Err(refused());
            }
            let mode = actual.mode() & 0o777;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&to)?;
            io::copy(&mut input, &mut output)?;
            output.set_permissions(fs::Permissions::from_mode(mode))?;
            output.sync_all()?;
        } else {
            return Err(refused());
        }
    }
    sync_directory(target).map_err(|_| refused())
}

fn antigravity_profile(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name.len() <= 48
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn stage(parent: &Path, bytes: &[u8]) -> io::Result<NamedTempFile> {
    if bytes.is_empty() || bytes.len() > MAX_PROFILE_BYTES {
        return Err(refused());
    }
    owned_directory(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix(".aac-profile-")
        .tempfile_in(parent)?;
    set_private(file.as_file()).map_err(|_| refused())?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}

fn replace(file: NamedTempFile, target: &Path) -> io::Result<()> {
    file.persist(target).map_err(|error| error.error)?;
    let _ = sync_directory(target.parent().ok_or_else(refused)?);
    Ok(())
}

fn publish(parent: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    stage(parent, bytes)?
        .persist_noclobber(target)
        .map_err(|error| error.error)?;
    sync_directory(parent).map_err(|_| refused())?;
    Ok(())
}

/// The saved-file and live-file result keeps the existing public job error mapping.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginInstallError {
    /// Neither login was changed or the saved login was restored.
    Refused,
    /// The saved login changed and could not be restored after a failed live install.
    LiveNotUpdated,
}

impl From<io::Error> for LoginInstallError {
    fn from(_: io::Error) -> Self {
        Self::Refused
    }
}

fn install_login(
    profile: &Path,
    live: Option<&Path>,
    bytes: &[u8],
    mut rename: impl FnMut(NamedTempFile, &Path) -> io::Result<()>,
) -> Result<(), LoginInstallError> {
    let auth_folder = profile.parent().ok_or_else(refused)?;
    let Some(live) = live else {
        return rename(stage(auth_folder, bytes)?, profile).map_err(Into::into);
    };
    let live_folder = live.parent().ok_or_else(refused)?;
    owned_directory(auth_folder)?;
    owned_directory(live_folder)?;
    let previous = match fs::symlink_metadata(profile) {
        Ok(meta) if meta.is_file() => {
            Some(read_bounded(profile, MAX_PROFILE_BYTES).map_err(|_| refused())?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        _ => return Err(LoginInstallError::Refused),
    };
    // Prepare every write, including rollback, before publishing the saved login.
    let saved = stage(auth_folder, bytes)?;
    let actual = stage(live_folder, bytes)?;
    let restore = previous
        .as_deref()
        .map(|old| stage(auth_folder, old))
        .transpose()?;
    rename(saved, profile)?;
    if rename(actual, live).is_ok() {
        return Ok(());
    }
    let restored = match restore {
        Some(file) => rename(file, profile),
        None => fs::remove_file(profile),
    };
    if restored.is_err() {
        return Err(LoginInstallError::LiveNotUpdated);
    }
    let _ = sync_directory(auth_folder);
    Err(LoginInstallError::Refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_failure_rolls_back_or_reports_the_partial_commit()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let saved = root.path().join("saved");
        let live = root.path().join("live");
        for fail_restore in [false, true] {
            let old = [0x11_u8; 8];
            let next = [0x22_u8; 8];
            fs::write(&saved, old)?;
            fs::write(&live, old)?;
            let mut attempts = 0;
            let outcome = install_login(&saved, Some(&live), &next, |staged, target| {
                attempts += 1;
                if target == live || (fail_restore && attempts == 3) {
                    return Err(refused());
                }
                replace(staged, target)
            });
            let (expected, kept) = if fail_restore {
                (LoginInstallError::LiveNotUpdated, next)
            } else {
                (LoginInstallError::Refused, old)
            };
            assert_eq!(outcome, Err(expected));
            assert!(fs::read(&saved)? == kept);
            assert!(fs::read(&live)? == old);
            assert_eq!(fs::read_dir(root.path())?.count(), 2);
        }
        Ok(())
    }
}
