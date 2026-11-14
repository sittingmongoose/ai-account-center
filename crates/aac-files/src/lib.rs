//! Private store registry, advisory ownership and durable same-directory replacement.
use aac_core::{Cause, Failure, bounded_read};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// Storage classification used for retention and credential separation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Provider credentials; never part of ordinary history.
    Credential,
    /// Owner settings.
    Setting,
    /// Mutable operational state.
    State,
    /// Reconstructible bounded cache.
    Cache,
    /// Permanent historical facts.
    History,
    /// Diagnostic logs.
    Log,
    /// Recovery copies.
    Backup,
}

/// Backup treatment belongs to each store declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupClass {
    /// Excluded, for reconstructible caches only.
    Rebuild,
    /// Include in a consistent settings/state snapshot.
    Snapshot,
    /// Requires the owning engine's consistent backup protocol.
    Engine,
    /// Requires explicit credential backup authorization.
    Sensitive,
}

/// A single relative private file and the process role allowed to write it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSpec {
    /// Relative path under the explicitly supplied private root.
    pub path: PathBuf,
    /// Retention/security category.
    pub class: Class,
    /// Unix permission bits; private files must declare 0600.
    pub mode: u32,
    /// Hard byte cap for reads and replacement writes.
    pub max_size: usize,
    /// Unique writer role during the strangler phase.
    pub owner: String,
    /// Stable schema identifier, including version.
    pub schema: String,
    /// Backup treatment.
    pub backup: BackupClass,
}

/// A registry rejects duplicate declarations and refuses writes by the wrong owner.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    stores: BTreeMap<PathBuf, StoreSpec>,
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !path.to_string_lossy().contains(['\\', ':'])
        && !path.components().any(|c| matches!(c, Component::Normal(name) if name == ".trash" || name.to_string_lossy().ends_with(".lock")))
}

impl Registry {
    /// Register exactly one writer per path; no absolute paths, traversal or weak modes.
    pub fn register(&mut self, spec: StoreSpec) -> Result<(), Failure> {
        if !relative(&spec.path)
            || spec.mode != 0o600
            || spec.max_size == 0
            || spec.max_size == usize::MAX
            || spec.owner.is_empty()
            || spec.schema.is_empty()
            || self.stores.contains_key(&spec.path)
        {
            return Err(Failure::new(
                Cause::Refused,
                "invalid or duplicate store declaration",
            ));
        }
        self.stores.insert(spec.path.clone(), spec);
        Ok(())
    }

    /// Acquire a nonblocking advisory writer lease for a registered path and owner role.
    /// The root and all parents must be private directories, never symlinks.
    pub fn claim(&self, root: &Path, path: &Path, owner: &str) -> Result<OwnedStore, Failure> {
        let spec = self
            .stores
            .get(path)
            .ok_or_else(|| Failure::new(Cause::Refused, "unregistered store"))?;
        if owner != spec.owner {
            return Err(Failure::new(
                Cause::Refused,
                "store belongs to another process role",
            ));
        }
        ensure_directory(root)?;
        let file = root.join(&spec.path);
        let parent = file
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing store parent"))?;
        ensure_directory(parent)?;
        let name = file
            .file_name()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing store name"))?;
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let lock = open_private(&parent.join(lock_name), true)?;
        FileExt::try_lock_exclusive(&lock)
            .map_err(|_| Failure::new(Cause::Busy, "store writer already active"))?;
        Ok(OwnedStore {
            path: file,
            spec: spec.clone(),
            _lock: lock,
        })
    }
}

/// A writer lease held until drop; kernel advisory locks also release after process crashes.
/// Locks serialize cooperating AAC processes; they are not a defence against a hostile owner
/// concurrently replacing parent directories. The private root must remain owner-controlled.
#[derive(Debug)]
pub struct OwnedStore {
    path: PathBuf,
    spec: StoreSpec,
    _lock: File,
}

impl OwnedStore {
    /// Read only a regular file and enforce the declared cap during streaming.
    pub fn read(&self) -> Result<Vec<u8>, Failure> {
        read_bounded(&self.path, self.spec.max_size)
    }

    /// Replace all bytes durably, retaining the old file on pre-rename failure.
    pub fn write(&self, bytes: &[u8]) -> Result<(), Failure> {
        self.write_with(|out| out.write_all(bytes))
    }

    /// Stage bytes through a capped writer, fsync them, rename on the same filesystem, then
    /// fsync the directory on Unix. Closure failure or unwinding removes the staging file.
    /// A post-rename directory-sync failure is reported although the new file is visible.
    pub fn write_with(
        &self,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> Result<(), Failure> {
        reject_special(&self.path, true)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing parent"))?;
        ensure_directory(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        set_private(staged.as_file())?;
        {
            let mut capped = CappedWriter {
                inner: staged.as_file_mut(),
                left: self.spec.max_size,
                exceeded: false,
            };
            let result = write(&mut capped);
            if capped.exceeded {
                return Err(Failure::new(
                    Cause::OutputTooLarge,
                    "store byte cap exceeded",
                ));
            }
            result?;
            capped.flush()?;
        }
        staged.as_file().sync_all()?;
        staged
            .persist(&self.path)
            .map_err(|_| Failure::new(Cause::NotWritable, "atomic replacement failed"))?;
        sync_directory(parent)?;
        Ok(())
    }

    /// Atomically move the store into a unique private trash directory beside it.
    /// Trash stays on the same filesystem and is never purged implicitly.
    pub fn trash(&self) -> Result<PathBuf, Failure> {
        reject_special(&self.path, false)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing parent"))?;
        let trash = parent.join(".trash");
        ensure_directory(&trash)?;
        let slot = tempfile::Builder::new().prefix("aac-").tempdir_in(&trash)?;
        let name = self
            .path
            .file_name()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing name"))?;
        let destination = slot.path().join(name);
        fs::rename(&self.path, &destination)?;
        let directory = slot.keep();
        sync_directory(&directory)?;
        sync_directory(&trash)?;
        sync_directory(parent)?;
        Ok(destination)
    }
}

struct CappedWriter<'a> {
    inner: &'a mut File,
    left: usize,
    exceeded: bool,
}
impl Write for CappedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.left {
            self.exceeded = true;
            return Err(io::Error::other("size cap"));
        }
        let n = self.inner.write(bytes)?;
        self.left -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Read a regular non-symlink file with a streaming byte cap; do not trust its metadata size.
pub fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, Failure> {
    reject_special(path, false)?;
    let file = open_private(path, false)?;
    if !file.metadata()?.is_file() {
        return Err(Failure::new(Cause::Refused, "not a regular file"));
    }
    bounded_read(file, max)
}

fn reject_special(path: &Path, missing_ok: bool) -> Result<(), Failure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Err(error) if missing_ok && error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        _ => Err(Failure::new(
            Cause::Refused,
            "symlink or special file refused",
        )),
    }
}

fn open_private(path: &Path, create: bool) -> Result<File, Failure> {
    reject_special(path, create)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Failure::new(Cause::Refused, "not a regular file"));
    }
    if create {
        set_private(&file)?;
    }
    Ok(file)
}

fn set_private(file: &File) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<(), Failure> {
    // Inspect every existing ancestor before creating anything beneath it.
    let mut ancestors: Vec<_> = path
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    ancestors.reverse();
    for directory in ancestors {
        match fs::symlink_metadata(directory) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(Failure::new(
                    Cause::Refused,
                    "non-directory ancestor refused",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let builder = fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                builder.create(directory)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), Failure> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    // Windows has no portable directory fsync through std. File data is synced before
    // tempfile's atomic replacement; native ACL inheritance stays the administrator's policy.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
