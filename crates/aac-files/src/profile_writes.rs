//! Provider files whose existing transaction lock also coordinates the TypeScript fallback.
use super::{read_bounded, require_absolute, set_private, sync_directory};
use std::fs::{self, File};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime};
use tempfile::NamedTempFile;

/// Upper bound for individual private provider inputs.
pub const MAX_PROFILE_BYTES: usize = 1024 * 1024;

fn refused() -> io::Error {
    io::Error::other("provider file write refused")
}

pub(crate) fn owned_directory(dir: &Path) -> io::Result<()> {
    require_absolute(dir).map_err(|_| refused())?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || super::path::foreign(&meta) || fs::canonicalize(dir)? != dir {
        return Err(refused());
    }
    #[cfg(unix)]
    if meta.mode() & 0o002 != 0 {
        return Err(refused());
    }
    Ok(())
}

/// Open only the named leaf, including a directory on Windows.
pub(crate) fn open_leaf(path: &Path, directory: bool) -> io::Result<File> {
    open_leaf_for_use(path, directory, false)
}

fn open_leaf_for_use(path: &Path, directory: bool, heartbeat: bool) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        let _ = heartbeat;
        options.custom_flags(
            libc::O_NOFOLLOW | libc::O_NONBLOCK | if directory { libc::O_DIRECTORY } else { 0 },
        );
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::GENERIC_READ;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_WRITE_ATTRIBUTES,
        };
        if heartbeat {
            options.access_mode(GENERIC_READ | FILE_WRITE_ATTRIBUTES);
        }
        options.custom_flags(
            FILE_FLAG_OPEN_REPARSE_POINT
                | if directory {
                    FILE_FLAG_BACKUP_SEMANTICS
                } else {
                    0
                },
        );
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if meta.file_type().is_symlink()
        || (if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        })
    {
        return Err(refused());
    }
    Ok(file)
}

/// The volume, file identifier and link count of an open handle.
#[cfg_attr(windows, allow(unsafe_code))]
pub(crate) fn file_key(file: &File) -> io::Result<(u64, u64, u64)> {
    #[cfg(unix)]
    {
        let meta = file.metadata()?;
        Ok((meta.dev(), meta.ino(), meta.nlink()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = Default::default();
        // The open File keeps the handle valid for the full metadata call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            u64::from(info.dwVolumeSerialNumber),
            u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
            u64::from(info.nNumberOfLinks),
        ))
    }
}

fn matches_named(file: &File, path: &Path, directory: bool) -> io::Result<bool> {
    let (volume, id, _) = file_key(file)?;
    let (named_volume, named_id, _) = file_key(&open_leaf(path, directory)?)?;
    Ok(volume == named_volume && id == named_id)
}

fn mode_is(meta: &fs::Metadata, mask: u32, value: u32) -> bool {
    #[cfg(unix)]
    {
        meta.mode() & mask == value
    }
    #[cfg(windows)]
    {
        let _ = (meta, mask, value);
        true
    }
}

/// An inherited descriptor of the exact marker created by the existing lock protocol.
pub struct LegacyLease(File);

/// Exclusively hold an existing Codex lock name through a short native write.
pub struct CodexLease {
    marker: PathBuf,
    lease: LegacyLease,
    stop: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

impl CodexLease {
    /// The activation lock shared by dashboard and CLI login writes.
    pub fn activation(home: &Path) -> io::Result<Self> {
        match super::private_builder().create(home) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        Self::wait(Duration::from_secs(10), Duration::from_millis(100), || {
            Self::try_activation(home)
        })
    }

    /// Prefer an active dashboard transaction to a background renewal.
    pub fn try_activation(home: &Path) -> io::Result<Self> {
        let home = home.canonicalize()?;
        owned_directory(&home)?;
        Self::claim(
            home.join(".ccs-activation.lock"),
            Duration::from_secs(120),
            Duration::from_secs(5),
        )
    }

    /// The proper-lockfile path used for codex-profiles.yaml.
    pub fn registry(root: &Path) -> io::Result<Self> {
        owned_directory(root)?;
        let mut marker = root.as_os_str().to_os_string();
        marker.push(".lock");
        let marker = PathBuf::from(marker);
        Self::wait(Duration::from_secs(2), Duration::from_millis(50), || {
            Self::claim(
                marker.clone(),
                Duration::from_secs(10),
                Duration::from_secs(5),
            )
        })
    }

    fn wait(
        maximum: Duration,
        delay: Duration,
        mut attempt: impl FnMut() -> io::Result<Self>,
    ) -> io::Result<Self> {
        let started = Instant::now();
        loop {
            match attempt() {
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && started.elapsed() < maximum =>
                {
                    std::thread::sleep(delay)
                }
                result => return result,
            }
        }
    }

    fn claim(marker: PathBuf, stale: Duration, update: Duration) -> io::Result<Self> {
        match super::private_builder().create(&marker) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                owned_directory(&marker)?;
                let held = open_leaf(&marker, true)?;
                let modified = held.metadata()?.modified()?;
                if !modified.elapsed().is_ok_and(|age| age > stale)
                    || !matches_named(&held, &marker, true)?
                    || held.metadata()?.modified()? != modified
                {
                    return Err(error);
                }
                fs::remove_dir(&marker)?;
                super::private_builder().create(&marker)?;
            }
            Err(error) => return Err(error),
        }
        let directory = open_leaf_for_use(&marker, true, true)?;
        let lease = LegacyLease::verify(&marker, directory)?;
        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat = watch_lock(&lease.0, Arc::clone(&stop), update);
        Ok(Self {
            marker,
            lease,
            stop,
            heartbeat,
        })
    }

    /// The descriptor checked by each write while this hold lasts.
    pub fn writer(&self) -> &LegacyLease {
        &self.lease
    }
}

impl Drop for CodexLease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.thread().unpark();
            let _ = heartbeat.join();
        }
        if self.lease.check(&self.marker).is_ok() {
            let _ = fs::remove_dir(&self.marker);
        }
    }
}

fn watch_lock(
    file: &File,
    stop: Arc<AtomicBool>,
    update: Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let file = file.try_clone().ok()?;
    std::thread::Builder::new()
        .name("aac-codex-lock".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                std::thread::park_timeout(update);
                if !stop.load(Ordering::Acquire) {
                    let _ = file.set_times(fs::FileTimes::new().set_modified(SystemTime::now()));
                }
            }
        })
        .ok()
}

/// A short maintenance transaction using the provider's existing directory lock.
pub struct AntigravityMaintenanceLease {
    marker: PathBuf,
    lease: LegacyLease,
    holder: File,
}

impl AntigravityMaintenanceLease {
    /// Create the private registry directory for a first dashboard sign-in.
    pub fn initialize(root: &Path) -> io::Result<()> {
        owned_directory(root)?;
        ensure_profile_directory(&root.join("antigravity-profiles"))
    }

    /// Reserve the same name as the compatibility registry. A present marker
    /// belongs to that transaction and must be left for its recovery path.
    pub fn claim(root: &Path, holder_record: &[u8]) -> io::Result<Self> {
        let parent = root.join("antigravity-profiles");
        private_profile_directory(&parent)?;
        if holder_record.is_empty() || holder_record.len() > 512 {
            return Err(refused());
        }
        let marker = parent.join(".transaction-lock");
        if fs::symlink_metadata(&marker).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transaction lock held",
            ));
        }
        let stage = tempfile::Builder::new()
            .prefix(".transaction-lock.new-")
            .tempdir_in(&parent)?;
        #[cfg(unix)]
        fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut holder = options.open(stage.path().join("holder.json"))?;
        set_private(&holder).map_err(|_| refused())?;
        holder.write_all(holder_record)?;
        holder.sync_all()?;
        let directory = open_leaf(stage.path(), true)?;
        // Every legitimate hold has a holder file already staged, so rename
        // cannot replace a competing holder's nonempty directory.
        #[cfg(target_os = "linux")]
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            stage.path(),
            rustix::fs::CWD,
            &marker,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        #[cfg(not(target_os = "linux"))]
        fs::rename(stage.path(), &marker)?;
        let lease = LegacyLease::verify(&marker, directory)?;
        Ok(Self {
            marker,
            lease,
            holder,
        })
    }

    /// The verified descriptor used by the existing provider writer.
    pub fn writer(&self) -> &LegacyLease {
        &self.lease
    }

    /// Whether `.transaction-lock` exists and is held by a live (non-stale) process.
    /// Returns `Ok(false)` when absent or provably stale, `Ok(true)` when live, or `Err`
    /// if the lock entry is unsafe (e.g. symlink or non-directory).
    pub fn lock_held_live(
        root: &Path,
        now_ms: u64,
        stale_after_ms: u64,
        mut is_stale: impl FnMut(&[u8], u64) -> Option<bool>,
    ) -> io::Result<bool> {
        let marker = root.join("antigravity-profiles").join(".transaction-lock");
        let meta = match fs::symlink_metadata(&marker) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(refused());
        }
        Ok(stale_lock_reason(&marker, now_ms, stale_after_ms, &mut is_stale).is_none())
    }

    /// Acquire `.transaction-lock`, reclaiming a provably abandoned lock via a serialized
    /// `.transaction-lock.takeover` claim folder (`registry.ts:acquireLock`).
    /// Returns `(lease, Some(reason))` when a stale lock was recovered.
    pub fn claim_or_recover(
        root: &Path,
        holder_record: &[u8],
        now_ms: u64,
        stale_after_ms: u64,
        mut is_stale: impl FnMut(&[u8], u64) -> Option<bool>,
    ) -> io::Result<(Self, Option<String>)> {
        match Self::claim(root, holder_record) {
            Ok(lease) => return Ok((lease, None)),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err),
        }
        let parent = root.join("antigravity-profiles");
        private_profile_directory(&parent)?;
        let marker = parent.join(".transaction-lock");
        let judged = match open_leaf(&marker, true) {
            Ok(dir) => dir,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "transaction lock held",
                ));
            }
        };
        let Some(reason) = stale_lock_reason(&marker, now_ms, stale_after_ms, &mut is_stale) else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transaction lock held",
            ));
        };
        let claim_path = parent.join(".transaction-lock.takeover");
        // Acquire takeover claim folder (reclaiming an abandoned claim older than stale_after_ms).
        let claim_stage =
            stage_lock_folder(&parent, ".transaction-lock.takeover.new-", holder_record)?;
        let mut claim_placed = place_lock_folder(claim_stage.0.path(), &claim_path);
        if !claim_placed
            && stale_lock_reason(&claim_path, now_ms, stale_after_ms, &mut is_stale).is_some()
            && let Ok(claim_judged) = open_leaf(&claim_path, true)
            && let Some(aside) = move_lock_aside(
                &parent,
                &claim_path,
                &claim_judged,
                ".transaction-lock.takeover.stale-",
            )
        {
            discard_stale_lock_dir(&aside);
            claim_placed = place_lock_folder(claim_stage.0.path(), &claim_path);
        }
        if !claim_placed {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transaction lock held",
            ));
        }
        let claim_dir = open_leaf(&claim_path, true).ok();
        let claim_holder = claim_stage.1;
        let _claim_guard = ClaimDropGuard {
            path: claim_path,
            dir: claim_dir,
            holder: claim_holder,
        };
        // Re-verify the judged lock identity and staleness under the takeover claim.
        if !matches_named(&judged, &marker, true).unwrap_or(false)
            || stale_lock_reason(&marker, now_ms, stale_after_ms, &mut is_stale).is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transaction lock held",
            ));
        }
        let Some(aside) = move_lock_aside(&parent, &marker, &judged, ".transaction-lock.stale-")
        else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "transaction lock held",
            ));
        };
        let lease_result = Self::claim(root, holder_record);
        discard_stale_lock_dir(&aside);
        let lease = lease_result?;
        Ok((lease, Some(reason)))
    }
}

struct ClaimDropGuard {
    path: PathBuf,
    dir: Option<File>,
    holder: File,
}

impl Drop for ClaimDropGuard {
    fn drop(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let file = self.path.join("holder.json");
        if matches_named(dir, &self.path, true).unwrap_or(false)
            && matches_named(&self.holder, &file, false).unwrap_or(false)
        {
            #[cfg(unix)]
            let _ = rustix::fs::unlinkat(dir, "holder.json", rustix::fs::AtFlags::empty());
            #[cfg(windows)]
            let _ = fs::remove_file(&file);
            if matches_named(dir, &self.path, true).unwrap_or(false) {
                let _ = fs::remove_dir(&self.path);
            }
        }
    }
}

fn stage_lock_folder(
    parent: &Path,
    prefix: &str,
    holder_record: &[u8],
) -> io::Result<(tempfile::TempDir, File)> {
    let stage = tempfile::Builder::new().prefix(prefix).tempdir_in(parent)?;
    #[cfg(unix)]
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut holder = options.open(stage.path().join("holder.json"))?;
    set_private(&holder).map_err(|_| refused())?;
    holder.write_all(holder_record)?;
    holder.sync_all()?;
    Ok((stage, holder))
}

fn place_lock_folder(staged: &Path, target: &Path) -> bool {
    if fs::symlink_metadata(target).is_ok() {
        return false;
    }
    #[cfg(target_os = "linux")]
    let res = rustix::fs::renameat_with(
        rustix::fs::CWD,
        staged,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .is_ok();
    #[cfg(not(target_os = "linux"))]
    let res = fs::rename(staged, target).is_ok();
    res
}

fn stale_lock_reason(
    lock: &Path,
    now_ms: u64,
    stale_after_ms: u64,
    is_stale: &mut impl FnMut(&[u8], u64) -> Option<bool>,
) -> Option<String> {
    let holder = lock.join("holder.json");
    if let Ok(meta) = fs::symlink_metadata(&holder)
        && meta.is_file()
        && !meta.file_type().is_symlink()
        && meta.len() <= 512
        && mode_is(&meta, 0o077, 0)
        && !crate::path::foreign(&meta)
        && let Ok(bytes) = read_bounded(&holder, 512)
    {
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| u64::try_from(d.as_millis()).ok())
            .unwrap_or(0);
        if let Some(stale) = is_stale(&bytes, mtime_ms) {
            return stale.then(|| "stale holder".to_owned());
        }
    }
    let meta = fs::symlink_metadata(lock).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_millis()).ok())?;
    (now_ms.saturating_sub(mtime_ms) > stale_after_ms)
        .then(|| format!("lock is older than {stale_after_ms}ms without a holder record"))
}

fn move_lock_aside(parent: &Path, lock: &Path, judged: &File, prefix: &str) -> Option<PathBuf> {
    if !matches_named(judged, lock, true).unwrap_or(false) {
        return None;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let aside = parent.join(format!("{prefix}{}-{nonce}", std::process::id()));
    #[cfg(target_os = "linux")]
    let moved = rustix::fs::renameat_with(
        rustix::fs::CWD,
        lock,
        rustix::fs::CWD,
        &aside,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .is_ok();
    #[cfg(not(target_os = "linux"))]
    let moved = fs::rename(lock, &aside).is_ok();
    moved.then_some(aside)
}

fn discard_stale_lock_dir(aside: &Path) {
    let Ok(entries) = fs::read_dir(aside) else {
        return;
    };
    let names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    if names.len() == 1 && names[0] == "holder.json" {
        let _ = fs::remove_file(aside.join("holder.json"));
    }
    if fs::read_dir(aside).is_ok_and(|mut it| it.next().is_none()) {
        let _ = fs::remove_dir(aside);
    }
}

impl Drop for AntigravityMaintenanceLease {
    fn drop(&mut self) {
        let file = self.marker.join("holder.json");
        if self.lease.check(&self.marker).is_ok()
            && matches_named(&self.holder, &file, false).unwrap_or(false)
        {
            #[cfg(unix)]
            let _ =
                rustix::fs::unlinkat(&self.lease.0, "holder.json", rustix::fs::AtFlags::empty());
            #[cfg(windows)]
            let _ = fs::remove_file(&file);
            if self.lease.check(&self.marker).is_ok() {
                let _ = fs::remove_dir(&self.marker);
            }
        }
    }
}

impl LegacyLease {
    /// Verify a marker name still identifies the caller's open directory.
    pub fn verify(marker: &Path, descriptor: File) -> io::Result<Self> {
        let lease = Self(descriptor);
        lease.check(marker)?;
        Ok(lease)
    }

    fn check(&self, marker: &Path) -> io::Result<()> {
        owned_directory(marker)?;
        if !matches_named(&self.0, marker, true)? {
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

    /// Delete only the credential files belonging to an unpublished profile.
    /// Returns the count of entries that could not be proved or removed.
    pub fn antigravity_discard(&self, root: &Path, profile: &str) -> io::Result<usize> {
        self.check(&root.join("antigravity-profiles/.transaction-lock"))?;
        if !antigravity_profile(profile) {
            return Err(refused());
        }
        let instances = root.join("antigravity-instances");
        private_profile_directory(&instances)?;
        Ok(discard_antigravity_files(&instances.join(profile)))
    }

    /// Retry cleanup of Antigravity profiles absent from the validated registry.
    /// Returns the number removed and the number of unproved entries retained.
    pub fn antigravity_sweep(&self, root: &Path, saved: &[String]) -> io::Result<(usize, usize)> {
        self.check(&root.join("antigravity-profiles/.transaction-lock"))?;
        if saved.iter().any(|id| !antigravity_profile(id)) {
            return Err(refused());
        }
        let instances = root.join("antigravity-instances");
        private_profile_directory(&instances)?;
        let mut removed = 0;
        let mut left = 0;
        for entry in fs::read_dir(&instances)? {
            let entry = entry?;
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|id| antigravity_profile(id) && !saved.iter().any(|saved| saved == id))
            {
                let profile = entry.path();
                left += discard_antigravity_files(&profile);
                if !profile.exists() {
                    removed += 1;
                }
            }
        }
        Ok((removed, left))
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
        let instances = root.join("antigravity-instances");
        let profile = instances.join(profile);
        let parent = profile.join("ubuntu");
        for directory in [&instances, &profile, &parent] {
            ensure_profile_directory(directory)?;
        }
        let file = parent.join(format!("credential-{fingerprint}.bin"));
        match fs::symlink_metadata(&file) {
            Ok(meta)
                if meta.is_file()
                    && !super::path::foreign(&meta)
                    && mode_is(&meta, 0o077, 0)
                    && file_key(&open_leaf(&file, false)?)?.2 == 1 =>
            {
                let existing = read_bounded(&file, MAX_PROFILE_BYTES).map_err(|_| refused())?;
                return if existing == bytes {
                    Ok(())
                } else {
                    Err(refused())
                };
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(refused()),
        }
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

    /// Keep the current generation and the reviewed previous generation. A
    /// repeated save of the current credential retains the newest previous file.
    pub fn antigravity_prune(
        &self,
        root: &Path,
        profile: &str,
        current: &str,
        previous: Option<&str>,
    ) -> io::Result<usize> {
        self.check(&root.join("antigravity-profiles/.transaction-lock"))?;
        if !antigravity_profile(profile)
            || !valid_fingerprint(current)
            || previous.is_some_and(|word| !valid_fingerprint(word))
        {
            return Err(refused());
        }
        let directory = root
            .join("antigravity-instances")
            .join(profile)
            .join("ubuntu");
        private_profile_directory(&directory)?;
        let mut candidates = Vec::new();
        let mut left = 0;
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(fingerprint) = name
                .to_str()
                .and_then(credential_filename)
                .filter(|word| *word != current)
            else {
                continue;
            };
            let path = entry.path();
            if let Ok(meta) = fs::symlink_metadata(&path)
                && credential_metadata(&meta)
            {
                candidates.push((fingerprint.to_owned(), path, meta.modified().ok()));
            } else if previous != Some(fingerprint) {
                left += 1;
            }
        }
        let keep = if previous == Some(current) {
            candidates
                .iter()
                .max_by(|left, right| (&left.2, &left.0).cmp(&(&right.2, &right.0)))
                .map(|entry| entry.0.as_str())
        } else {
            previous
        };
        for (fingerprint, path, _) in &candidates {
            if keep != Some(fingerprint) && fs::remove_file(path).is_err() {
                left += 1;
            }
        }
        let _ = sync_directory(&directory);
        Ok(left)
    }
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

fn valid_fingerprint(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(lower_hex)
}

fn credential_filename(name: &str) -> Option<&str> {
    let fingerprint = name.strip_prefix("credential-")?.strip_suffix(".bin")?;
    valid_fingerprint(fingerprint).then_some(fingerprint)
}

fn credential_metadata(meta: &fs::Metadata) -> bool {
    meta.is_file()
        && mode_is(meta, 0o777, 0o600)
        && !crate::path::foreign(meta)
        && meta.len() <= MAX_PROFILE_BYTES as u64
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
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, &to)?;
            #[cfg(windows)]
            {
                use std::os::windows::fs::{FileTypeExt, symlink_dir, symlink_file};
                if expected.file_type().is_symlink_dir() {
                    symlink_dir(link, &to)?;
                } else {
                    symlink_file(link, &to)?;
                }
            }
        } else if expected.is_dir() {
            owned_directory(&from)?;
            super::private_builder().create(&to)?;
            copy_profile_entries(&from, &to)?;
        } else if expected.is_file() {
            let mut input = open_leaf(&from, false)?;
            #[cfg(unix)]
            {
                let actual = input.metadata()?;
                if !actual.is_file()
                    || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
                {
                    return Err(refused());
                }
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(expected.mode() & 0o777);
            let mut output = options.open(&to)?;
            io::copy(&mut input, &mut output)?;
            #[cfg(unix)]
            output.set_permissions(fs::Permissions::from_mode(expected.mode() & 0o777))?;
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
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn private_profile_directory(directory: &Path) -> io::Result<()> {
    owned_directory(directory)?;
    if !mode_is(&fs::symlink_metadata(directory)?, 0o077, 0) {
        return Err(refused());
    }
    Ok(())
}

fn ensure_profile_directory(directory: &Path) -> io::Result<()> {
    match super::private_builder().create(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            private_profile_directory(directory)
        }
        Err(error) => Err(error),
    }
}

fn discard_antigravity_files(profile: &Path) -> usize {
    let host = profile.join("ubuntu");
    let entries = private_profile_directory(profile)
        .and_then(|()| private_profile_directory(&host))
        .and_then(|()| fs::read_dir(&host));
    let entries = match entries {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return 0,
        Err(_) => return 1,
    };
    let mut left = 0;
    for entry in entries {
        let delete = (|| {
            let entry = entry?;
            let name = entry.file_name();
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if name.to_str().and_then(credential_filename).is_none() || !credential_metadata(&meta)
            {
                return Err(refused());
            }
            fs::remove_file(path)
        })();
        if delete.is_err() {
            left += 1;
        }
    }
    for directory in [host.as_path(), profile] {
        let _ = fs::remove_dir(directory);
    }
    left
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
