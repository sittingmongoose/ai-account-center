//! One file policy (L2): path admission ([`path`]), the hardened read ([`read`]), the private-file
//! registry shared with TS ([`registry`]) and durable, leased same-folder replacement
//! ([`OwnedStore`]). Every Rust reader and writer of a file outside the store goes through here;
//! `scripts/foundation/one-file-policy.mjs` refuses a raw open, link flag or rename elsewhere.
use aac_core::{Cause, Failure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub mod deployed_build;
#[cfg(unix)]
pub mod history_snapshots;
#[cfg(unix)]
pub mod key_sweep;
#[cfg(unix)]
pub mod lifecycle_queue;
pub mod codex_identity;
pub mod codex_registry;
pub mod path;
#[cfg(unix)]
pub mod presence;
#[cfg(unix)]
pub mod profile_staging;
#[cfg(unix)]
pub mod profile_writes;
pub mod read;
pub mod registry;
pub mod share;

use read::{Read, ReadPolicy};

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

/// How a writer makes and checks the folders above its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Parents {
    /// The root is canonical; every folder from `/` down is a real folder, never a link; a
    /// missing folder is made 0700. The target must be a regular file or missing.
    Private,
    /// The rule of the Python helper that is still the reference for this file: missing parents
    /// are made at the umask default and followed as Python's `mkdir(parents=True)` does; the
    /// file's own folder is made 0700, must be a real folder this user owns, and loses any group
    /// or other bit. The target is replaced as `os.replace` does (anything but a folder).
    Reference,
}

/// Where a writer's kernel lease is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lease {
    /// `<name>.lock` beside the file.
    File,
    /// The file's folder itself (`flock` on its descriptor), for folders whose listing another
    /// implementation reads and counts, where a lock file would change the answer.
    Folder,
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

fn require_absolute(path: &Path) -> Result<(), Failure> {
    if path::node::absolute(path)?.as_os_str() != path.as_os_str() {
        return Err(Failure::new(Cause::Refused, "store root must be canonical"));
    }
    Ok(())
}

fn relative(path: &Path) -> bool {
    use std::path::Component;
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
    /// The root must be absolute and lexically canonical. Existing ancestors must be real
    /// directories, never symlinks; new directories are 0700 on Unix. Existing owner/mode
    /// validation is the caller's responsibility until the private-root hardening follow-up.
    pub fn claim(&self, root: &Path, path: &Path, owner: &str) -> Result<OwnedStore, Failure> {
        require_absolute(root)?;
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
        OwnedStore::open(
            root,
            &spec.path,
            spec.max_size,
            Parents::Private,
            Lease::File,
        )
    }
}

/// A writer lease held until drop; kernel advisory locks also release after process crashes.
/// Locks serialize cooperating AAC processes; they are not a defence against a hostile owner
/// concurrently replacing parent directories. The private root must remain owner-controlled.
#[derive(Debug)]
pub struct OwnedStore {
    path: PathBuf,
    max_size: usize,
    parents: Parents,
    /// The declared replacement mode (0644 for share rows, otherwise 0600).
    mode: u32,
    _lock: StoreLock,
}

#[derive(Debug)]
enum StoreLock {
    Kernel { _file: File },
    Config { _guard: ConfigLock },
}

#[derive(Debug)]
struct ConfigLock {
    file: PathBuf,
    claim: Vec<u8>,
}

impl ConfigLock {
    fn take(parent: &Path) -> Result<Self, Failure> {
        let file = parent.join("config.yaml.lock");
        // The TypeScript config writer checks the PID and owner token in this exact format.
        // Publish complete contents atomically, so it never mistakes a new claim for a stale one.
        let mut pending = tempfile::Builder::new()
            .prefix(".config-claim-")
            .tempfile_in(parent)?;
        set_private(pending.as_file())?;
        let token = pending
            .path()
            .file_name()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing config claim name"))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Failure::new(Cause::Refused, "invalid config claim clock"))?
            .as_millis();
        let claim =
            format!("{}\n{now}\n{}", std::process::id(), token.to_string_lossy()).into_bytes();
        pending.as_file_mut().write_all(&claim)?;
        pending.as_file().sync_all()?;
        pending
            .persist_noclobber(&file)
            .map_err(|_| Failure::new(Cause::Busy, "config writer already active"))?;
        Ok(Self { file, claim })
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        if read_bounded(&self.file, 1024).is_ok_and(|bytes| bytes == self.claim) {
            let _ = fs::remove_file(&self.file);
        }
    }
}

/// Outcome of a failed Status journal append.
#[cfg(unix)]
#[derive(Debug)]
pub enum AnswerAppendError {
    /// No record was written or its compensating void is durable.
    Refused,
    /// A record may have reached disk without a durable void.
    Unclear,
}

impl OwnedStore {
    /// Acquire the existing cross-language config writer lock for one read-modify-write.
    /// A pre-existing claim is left for the TypeScript lock's existing recovery path.
    pub fn claim_dashboard_config(root: &Path) -> Result<Self, Failure> {
        require_absolute(root)?;
        ensure_directory(root)?;
        let path = root.join("config.yaml");
        reject_special(&path, true)?;
        Ok(Self {
            path,
            max_size: 4 * 1024 * 1024,
            parents: Parents::Private,
            mode: 0o600,
            _lock: StoreLock::Config {
                _guard: ConfigLock::take(root)?,
            },
        })
    }

    /// Remove a private store's leaf under its writer lease. A symlink is unlinked as a name,
    /// never followed, and a directory is refused by the operating system.
    pub fn remove(&self) -> Result<(), Failure> {
        require_absolute(&self.path)?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing store parent"))?;
        folders(parent, self.parents)?;
        match fs::remove_file(&self.path) {
            Ok(()) => sync_directory(parent),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Make the row's folders under `root` by its rule, then take its lease.
    fn open(
        root: &Path,
        relative: &Path,
        max_size: usize,
        parents: Parents,
        lease: Lease,
    ) -> Result<Self, Failure> {
        let file = match parents {
            Parents::Private => {
                require_absolute(root)?;
                ensure_directory(root)?;
                path::node::resolve(root, relative)?
            }
            Parents::Reference => {
                if !root.is_absolute() {
                    return Err(Failure::new(Cause::Refused, "store root must be absolute"));
                }
                root.join(relative)
            }
        };
        let parent = file
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing store parent"))?;
        folders(parent, parents)?;
        let lock = match lease {
            Lease::File => {
                let name = file
                    .file_name()
                    .ok_or_else(|| Failure::new(Cause::Refused, "missing store name"))?;
                let mut lock_name = name.to_os_string();
                lock_name.push(".lock");
                open_lock(&parent.join(lock_name))?
            }
            Lease::Folder => open_folder(parent)?,
        };
        FileExt::try_lock_exclusive(&lock)
            .map_err(|_| Failure::new(Cause::Busy, "store writer already active"))?;
        Ok(OwnedStore {
            path: file,
            max_size,
            parents,
            mode: 0o600,
            _lock: StoreLock::Kernel { _file: lock },
        })
    }

    /// Accept a lease descriptor from the front door for a registered private file.
    #[cfg(unix)]
    fn inherit(
        root: &Path,
        row: &registry::Row,
        name: Option<&str>,
        lock: File,
    ) -> Result<Self, Failure> {
        use std::os::unix::fs::MetadataExt;

        require_absolute(root)?;
        let file = path::node::resolve(root, Path::new(&row.file(name)?))?;
        let lock_path = match row.lease {
            Lease::File => {
                let mut lock_path = file.as_os_str().to_os_string();
                lock_path.push(".lock");
                PathBuf::from(lock_path)
            }
            Lease::Folder => file
                .parent()
                .ok_or_else(|| Failure::new(Cause::Refused, "missing store parent"))?
                .to_path_buf(),
        };
        require_absolute(&lock_path)?;
        let current = fs::symlink_metadata(lock_path)?;
        let held = lock.metadata()?;
        let same_kind = match row.lease {
            Lease::File => current.is_file() && held.is_file(),
            Lease::Folder => current.is_dir() && held.is_dir(),
        };
        if !same_kind
            || held.uid() != path::uid()
            || held.mode() & 0o077 != 0
            || held.dev() != current.dev()
            || held.ino() != current.ino()
        {
            return Err(Failure::new(Cause::Refused, "wrong store writer lease"));
        }
        FileExt::try_lock_exclusive(&lock)
            .map_err(|_| Failure::new(Cause::Busy, "store writer already active"))?;
        let max_size = usize::try_from(row.max_bytes)
            .map_err(|_| Failure::new(Cause::Refused, "invalid byte cap"))?;
        Ok(Self {
            path: file,
            max_size,
            parents: row.parents,
            mode: if row.mode == "0644" { 0o644 } else { 0o600 },
            _lock: StoreLock::Kernel { _file: lock },
        })
    }

    /// The store's resolved path. For a store with its own engine (the reset claims SQLite),
    /// the lease is the one-writer proof and the engine opens this path itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The row's declared mode for the replacement (R4 share rows write 0644, as their TS
    /// writer did). Only the registry's two modes are admitted; anything else keeps 0600.
    pub fn with_mode(mut self, mode: u32) -> Self {
        if mode == 0o644 {
            self.mode = mode;
        }
        self
    }

    /// Read only a regular file and enforce the declared cap during streaming.
    pub fn read(&self) -> Result<Vec<u8>, Failure> {
        if self.parents == Parents::Private {
            require_absolute(&self.path)?;
        }
        read_bounded(&self.path, self.max_size)
    }

    /// Replace all bytes durably, retaining the old file on pre-rename failure.
    pub fn write(&self, bytes: &[u8]) -> Result<(), Failure> {
        self.write_with(|out| out.write_all(bytes))
    }

    /// Replace an owner-only cache, refusing hard links and loose existing modes.
    #[cfg(unix)]
    pub fn write_private_cache(&self, bytes: &[u8]) -> Result<(), Failure> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let owner = fs::symlink_metadata(parent)?;
        if !private_cache_metadata(&owner, true) {
            return Err(Failure::new(
                Cause::Refused,
                "cache directory is not private",
            ));
        }
        match fs::symlink_metadata(&self.path) {
            Ok(meta)
                if private_cache_metadata(&meta, false) && meta.len() <= self.max_size as u64 => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(Failure::new(Cause::Refused, "cache file is not private")),
        }
        self.write(bytes)
    }

    /// Clear a single owner-only cache while holding its writer lease.
    #[cfg(unix)]
    pub fn clear_private_cache(&self) -> Result<(), Failure> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if !private_cache_metadata(&fs::symlink_metadata(parent)?, true) {
            return Err(Failure::new(
                Cause::Refused,
                "cache directory is not private",
            ));
        }
        match fs::symlink_metadata(&self.path) {
            Ok(meta)
                if private_cache_metadata(&meta, false) && meta.len() <= self.max_size as u64 =>
            {
                fs::remove_file(&self.path)?;
                sync_directory(parent)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            _ => Err(Failure::new(Cause::Refused, "cache file is not private")),
        }
    }

    /// Keep at most `count` validated cache records, including the file just written.
    /// The writer holds its folder lease. Unknown or changed entries are left in place.
    #[cfg(unix)]
    pub fn prune_private_cache(
        &self,
        count: usize,
        matches_name: impl Fn(&str) -> bool,
        matches_record: impl Fn(&str, &[u8]) -> bool,
    ) {
        use std::os::unix::fs::MetadataExt;
        let Some(parent) = self.path.parent() else {
            return;
        };
        let StoreLock::Kernel { _file: lock } = &self._lock else {
            return;
        };
        let Ok(folder) = lock.metadata() else {
            return;
        };
        let same = |a: &fs::Metadata, b: &fs::Metadata| a.dev() == b.dev() && a.ino() == b.ino();
        let unchanged = || {
            fs::symlink_metadata(parent)
                .is_ok_and(|meta| private_cache_metadata(&meta, true) && same(&meta, &folder))
        };
        if count == 0 || !unchanged() {
            return;
        }
        let Ok(entries) = fs::read_dir(parent) else {
            return;
        };
        let entries = entries.take(4097).collect::<Result<Vec<_>, _>>();
        let Ok(entries) = entries else { return };
        if entries.len() <= count || entries.len() > 4096 {
            return;
        }
        let entries = entries
            .into_iter()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                matches_name(&name).then(|| (entry.path(), name))
            })
            .collect::<Vec<_>>();
        if entries.len() <= count {
            return;
        }
        let policy = ReadPolicy::capped(self.max_size as u64)
            .mode(read::Mode::Private)
            .owner(read::Owner::Mine);
        let mut records = Vec::new();
        for (path, name) in entries {
            let Ok(stat) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !private_cache_metadata(&stat, false) {
                continue;
            }
            let Read::Bytes(bytes, _) = read::read(&path, &policy) else {
                continue;
            };
            if matches_record(&name, &bytes) {
                records.push((path, stat));
            }
        }
        records.sort_unstable_by(|(a, am), (b, bm)| {
            (a == &self.path, am.modified().ok(), a).cmp(&(b == &self.path, bm.modified().ok(), b))
        });
        let excess = records.len().saturating_sub(count);
        for (path, before) in records.into_iter().take(excess) {
            if !unchanged() {
                return;
            }
            if fs::symlink_metadata(&path).is_ok_and(|after| {
                private_cache_metadata(&after, false)
                    && same(&before, &after)
                    && before.len() == after.len()
                    && before.modified().ok() == after.modified().ok()
            }) {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Stage bytes through a capped writer in a new file beside the target (`O_EXCL`, a
    /// random `.<name>.<random>.tmp` name) at the row's mode (0600, or 0644 for a share row),
    /// fsync them, rename on the same filesystem, then fsync the folder on Unix. Closure
    /// failure or unwinding removes the staging file. A post-rename folder-sync failure is
    /// reported although the new file is visible.
    pub fn write_with(
        &self,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> Result<(), Failure> {
        self.write_staged(write, &mut |_| Ok(()), false)
    }

    /// Replace a settings leaf without opening it. Renaming over a symlink replaces the
    /// link itself. Parent and lease checks still apply, and a directory cannot be replaced.
    /// As with the legacy settings writers, syncing the directory after rename is best effort.
    pub fn replace_settings(&self, bytes: &[u8]) -> Result<(), Failure> {
        self.write_staged(|out| out.write_all(bytes), &mut |_| Ok(()), true)
    }

    /// Append a Status journal record under the registry writer lease. The cap applies to
    /// an individual record, since existing history stays in the append-only file.
    #[cfg(unix)]
    pub fn append_answer(&self, bytes: &[u8], cancel: &[u8]) -> Result<(), AnswerAppendError> {
        use std::os::unix::fs::OpenOptionsExt;
        if bytes.len() > self.max_size
            || cancel.len() > self.max_size
            || require_absolute(&self.path).is_err()
            || reject_special(&self.path, true).is_err()
        {
            return Err(AnswerAppendError::Refused);
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)
            .map_err(|_| AnswerAppendError::Refused)?;
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(AnswerAppendError::Refused);
        }
        // Each record starts with a newline, so a torn earlier write cannot swallow it.
        match file.write(bytes) {
            Ok(len) if len == bytes.len() && file.sync_all().is_ok() => Ok(()),
            Ok(0) | Err(_) => Err(AnswerAppendError::Refused),
            Ok(_)
                if !cancel.is_empty()
                    && file.write_all(cancel).is_ok()
                    && file.sync_all().is_ok() =>
            {
                Err(AnswerAppendError::Refused)
            }
            Ok(_) => Err(AnswerAppendError::Unclear),
        }
    }

    /// Confirm that a settings leaf is absent or can be replaced without following it.
    pub fn can_replace_settings(&self) -> Result<(), Failure> {
        if self.parents == Parents::Private {
            require_absolute(&self.path)?;
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => Err(Failure::new(
                Cause::Refused,
                "settings leaf cannot be replaced",
            )),
        }
    }

    /// Remove this settings leaf without following it. An absent leaf is equivalent.
    pub fn remove_settings(&self) -> Result<(), Failure> {
        self.can_replace_settings()?;
        match fs::remove_file(&self.path) {
            Ok(()) => {
                if let Some(parent) = self.path.parent() {
                    let _ = sync_directory(parent);
                }
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Remove legacy settings staging files beside this store after a successful replacement.
    /// The writer lease is still held. Unlink never follows a symlink.
    pub fn clean_settings_temporaries(&self) {
        let Some((parent, name)) = self.path.parent().zip(self.path.file_name()) else {
            return;
        };
        let prefix = format!("{}.", name.to_string_lossy());
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let entry_name = entry.file_name();
                let text = entry_name.to_string_lossy();
                if text.starts_with(&prefix) && text.ends_with(".tmp") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }

    /// [`OwnedStore::write_with`] with a hook after each step (the test seam for a write
    /// interrupted mid-way).
    fn write_staged(
        &self,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
        step: &mut dyn FnMut(Step) -> io::Result<()>,
        replace_settings_leaf: bool,
    ) -> Result<(), Failure> {
        if self.parents == Parents::Private {
            require_absolute(&self.path)?;
            if !replace_settings_leaf {
                reject_special(&self.path, true)?;
            }
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing parent"))?;
        folders(parent, self.parents)?;
        let name = self
            .path
            .file_name()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing name"))?;
        let mut staged = tempfile::Builder::new()
            .prefix(&format!(".{}.", name.to_string_lossy()))
            .suffix(".tmp")
            .tempfile_in(parent)?;
        set_mode(staged.as_file(), self.mode)?;
        step(Step::Staged)?;
        {
            let mut capped = CappedWriter {
                inner: staged.as_file_mut(),
                left: self.max_size,
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
        step(Step::Written)?;
        staged.as_file().sync_all()?;
        step(Step::Synced)?;
        staged
            .persist(&self.path)
            .map_err(|_| Failure::new(Cause::NotWritable, "atomic replacement failed"))?;
        step(Step::Renamed)?;
        let synced = sync_directory(parent);
        if !replace_settings_leaf {
            synced?;
        }
        Ok(())
    }

    /// Atomically move the store into a unique private trash directory beside it.
    /// Trash stays on the same filesystem and is never purged implicitly.
    pub fn trash(&self) -> Result<PathBuf, Failure> {
        require_absolute(&self.path)?;
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

/// The steps of one replacement, for the interrupted-write tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Staged,
    Written,
    Synced,
    Renamed,
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

/// Read a regular non-symlink file with a byte cap, through [`read::read`]; the failure keeps
/// the classification callers already report.
pub fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, Failure> {
    match read::read(path, &ReadPolicy::capped(max as u64)) {
        Read::Bytes(bytes, _) => Ok(bytes),
        Read::Absent => Err(io::Error::from(io::ErrorKind::NotFound).into()),
        Read::Unreadable(kind) | Read::ChainUnreadable(kind) => Err(io::Error::from(kind).into()),
        Read::TooLarge => Err(Failure::new(Cause::OutputTooLarge, "byte cap exceeded")),
        Read::Link | Read::Directory | Read::NotRegular => Err(Failure::new(
            Cause::Refused,
            "symlink or special file refused",
        )),
        Read::Unproved | Read::Foreign | Read::BadMode => Err(Failure::new(
            Cause::Refused,
            "file changed while it was opened",
        )),
    }
}

#[cfg(unix)]
fn private_cache_metadata(meta: &fs::Metadata, directory: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o077 == 0
        && meta.uid() == path::uid()
        && if directory {
            meta.is_dir()
        } else {
            meta.is_file() && meta.nlink() == 1
        }
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

/// Open (or create, 0600) a writer's `<name>.lock` file without following a link.
fn open_lock(path: &Path) -> Result<File, Failure> {
    reject_special(path, true)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
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
    set_private(&file)?;
    Ok(file)
}

/// The folder itself as a lease descriptor: a real folder, never a link.
fn open_folder(path: &Path) -> Result<File, Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let folder = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        if !folder.metadata()?.is_dir() {
            return Err(Failure::new(Cause::Refused, "not a folder"));
        }
        Ok(folder)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(Failure::new(Cause::Refused, "folder leases need Unix"))
    }
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

/// The staged file at the row's declared mode (0600 private, 0644 share). Windows keeps its
/// native ACL inheritance, as the private writer does.
fn set_mode(file: &File, mode: u32) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (file, mode);
    Ok(())
}

fn folders(parent: &Path, parents: Parents) -> Result<(), Failure> {
    match parents {
        Parents::Private => ensure_directory(parent),
        Parents::Reference => reference_folder(parent),
    }
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
                private_builder().create(directory)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn private_builder() -> fs::DirBuilder {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
}

/// Python's `Path.mkdir(mode=0o700, parents=True, exist_ok=True)` and the helper's check: the
/// parents at the umask default, the folder itself 0700, a real folder of this user, tightened.
fn reference_folder(folder: &Path) -> Result<(), Failure> {
    let refused = || Failure::new(Cause::Refused, "store folder refused");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        fs::create_dir_all(folder.parent().ok_or_else(refused)?)?;
        match private_builder().create(folder) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            result => result?,
        }
        let meta = fs::symlink_metadata(folder)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != path::uid() {
            return Err(refused());
        }
        if meta.mode() & 0o077 != 0 {
            fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = folder;
        Err(refused())
    }
}

fn sync_directory(path: &Path) -> Result<(), Failure> {
    #[cfg(unix)]
    open_folder(path)?.sync_all()?;
    // Windows has no portable directory fsync through std. File data is synced before
    // tempfile's atomic replacement; native ACL inheritance stays the administrator's policy.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Interrupted replacements: whatever step fails, the committed file stays whole and no
    //! staging file is left; after the rename, the new file is whole and the folder is synced.
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn store(root: &Path, parents: Parents, lease: Lease) -> OwnedStore {
        OwnedStore::open(root, Path::new("d/state.json"), 64, parents, lease).unwrap()
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_write_interrupted_before_its_rename_keeps_the_committed_file() {
        for (parents, lease, lock) in [
            (Parents::Private, Lease::File, true),
            (Parents::Reference, Lease::Folder, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let store = store(&root, parents, lease);
            store.write(b"committed").unwrap();
            for at in [Step::Staged, Step::Written, Step::Synced] {
                let result = store.write_staged(
                    |out| out.write_all(b"replacement"),
                    &mut |now| {
                        if now == at {
                            Err(io::Error::other("interrupted"))
                        } else {
                            Ok(())
                        }
                    },
                    false,
                );
                assert!(result.is_err(), "{at:?}");
                assert_eq!(store.read().unwrap(), b"committed", "{at:?}");
                let mut expected = vec!["state.json".to_owned()];
                if lock {
                    expected.push("state.json.lock".to_owned());
                }
                assert_eq!(names(&root.join("d")), expected, "{at:?}");
            }
            // After the rename the new file is whole, and the error says the sync did not run.
            let result = store.write_staged(
                |out| out.write_all(b"renamed"),
                &mut |now| {
                    if now == Step::Renamed {
                        Err(io::Error::other("interrupted"))
                    } else {
                        Ok(())
                    }
                },
                false,
            );
            assert!(result.is_err());
            assert_eq!(store.read().unwrap(), b"renamed");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reference_folders_follow_the_python_helper() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home/.ccs");
        let file = Path::new("account-usage/keys/zai-12345678.json");
        let store = OwnedStore::open(&root, file, 4096, Parents::Reference, Lease::Folder).unwrap();
        store.write(b"{}").unwrap();
        let keys = root.join("account-usage/keys");
        assert_eq!(fs::metadata(&keys).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(keys.join("zai-12345678.json")).unwrap().mode() & 0o777,
            0o600
        );
        // The parents keep the umask default, as `mkdir(parents=True)` makes them.
        let parent = fs::metadata(root.join("account-usage")).unwrap().mode() & 0o777;
        let probe = dir.path().join("probe");
        fs::create_dir(&probe).unwrap();
        assert_eq!(parent, fs::metadata(&probe).unwrap().mode() & 0o777);
        // No lock file joins the listing; the folder lease refuses a second writer.
        assert_eq!(names(&keys), vec!["zai-12345678.json".to_owned()]);
        let busy = OwnedStore::open(&root, file, 4096, Parents::Reference, Lease::Folder);
        assert_eq!(busy.unwrap_err().cause, Cause::Busy);
        drop(store);
        // A loose folder is tightened; a linked one is refused.
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o755)).unwrap();
        drop(OwnedStore::open(&root, file, 4096, Parents::Reference, Lease::Folder).unwrap());
        assert_eq!(fs::metadata(&keys).unwrap().mode() & 0o777, 0o700);
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(keys.join("zai-12345678.json")).unwrap();
        fs::remove_dir(&keys).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &keys).unwrap();
        assert!(reference_folder(&keys).is_err());
        assert!(OwnedStore::open(&root, file, 4096, Parents::Reference, Lease::Folder).is_err());
        // Refused before anything touches the link's target: no file, no mode change.
        assert!(names(&elsewhere).is_empty());
        assert_eq!(fs::metadata(&elsewhere).unwrap().mode() & 0o777, 0o755);
    }
}
