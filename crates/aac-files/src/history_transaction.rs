//! Create-only Claude metadata transactions; every mutation rechecks the bound roots and guards.
use crate::profile_writes::{file_key, open_leaf};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
fn refused() -> io::Error {
    io::Error::other("history storage refused")
}
/// Volume, inode and link count of a no-follow handle.
pub type Identity = (u64, u64, u64);
/// Resolve a directory identity after refusing every linked ancestor.
pub fn directory(path: &Path) -> io::Result<Identity> {
    if !path.is_absolute() {
        return Err(refused());
    }
    for p in path.ancestors() {
        let m = fs::symlink_metadata(p)?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(refused());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return Err(refused());
            }
        }
    }
    let k = file_key(&open_leaf(path, true)?)?;
    if k.0 == 0 || k.1 == 0 {
        return Err(refused());
    }
    Ok(k)
}
/// The identity of a no-follow regular file, with every ancestor checked.
pub fn identity(path: &Path) -> io::Result<Identity> {
    directory(path.parent().ok_or_else(refused)?)?;
    let k = file_key(&open_leaf(path, false)?)?;
    if k.0 == 0 || k.1 == 0 {
        return Err(refused());
    }
    Ok(k)
}
/// Bounded no-follow read, with identity checked after the read.
pub fn read(path: &Path, owned: bool) -> io::Result<(Vec<u8>, Identity)> {
    directory(path.parent().ok_or_else(refused)?)?;
    let mut f = open_leaf(path, false)?;
    let k = file_key(&f)?;
    if k.0 == 0 || k.1 == 0 || !(k.2 == 1 || owned && k.2 == 2) || f.metadata()?.len() > 2_000_000 {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut f)
        .take(2_000_001)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2_000_000 || file_key(&open_leaf(path, false)?)? != k {
        return Err(refused());
    }
    Ok((bytes, k))
}
/// Caller-owned application and storage guards, re-evaluated immediately before mutations.
pub trait Guard {
    /// Confirm the bound profile has stayed closed and its protected state is unchanged.
    fn closed(&mut self) -> bool;
    /// Confirm owner-only storage on Windows. Unix permissions are also checked locally.
    fn private(&mut self, path: &Path, directory: bool) -> bool;
    /// Test seam for real filesystem races; production implementations leave this empty.
    fn phase(&mut self, _phase: &str, _path: &Path) {}
}
/// One validated, immutable record or protected file.
pub struct Record {
    /// Leaf file name only.
    pub name: String,
    /// Exact bytes; absence is permitted only for protected files.
    pub bytes: Option<Vec<u8>>,
}
struct Owned {
    path: PathBuf,
    key: Identity,
    bytes: Vec<u8>,
    complete: bool,
}
fn same(a: Identity, b: Identity) -> bool {
    a.0 == b.0 && a.1 == b.1
}
fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn unique() -> io::Result<String> {
    let mut b = [0; 16];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
fn leaf(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 240
        && !matches!(s, "." | "..")
        && !s
            .chars()
            .any(|c| c < ' ' || c == '\u{7f}' || c == '/' || c == '\\')
}
fn record_name(s: &str) -> bool {
    s.strip_prefix("local_")
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|id| {
            id.len() == 36
                && id.bytes().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == b'-'
                    } else {
                        c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
                    }
                })
        })
}
struct State<'a> {
    profile: &'a Path,
    registry: &'a Path,
    pk: Identity,
    rk: Identity,
    snapshot: Option<(PathBuf, Identity)>,
    protected: &'a [Record],
    windows: bool,
    guard: &'a mut dyn Guard,
}
impl State<'_> {
    fn roots(&self) -> io::Result<()> {
        if !same(directory(self.profile)?, self.pk) || !same(directory(self.registry)?, self.rk) {
            return Err(refused());
        }
        if let Some((p, k)) = &self.snapshot
            && !same(directory(p)?, *k)
        {
            return Err(refused());
        }
        Ok(())
    }
    fn private(&mut self, p: &Path, dir: bool) -> io::Result<()> {
        self.roots()?;
        if !self.guard.private(p, dir) {
            return Err(refused());
        }
        #[cfg(unix)]
        {
            if !self.windows {
                let m = fs::symlink_metadata(p)?;
                let expected = if dir { 0o700 } else { 0o600 };
                if m.file_type().is_symlink() || m.permissions().mode() & 0o777 != expected {
                    return Err(refused());
                }
            }
        }
        self.roots()
    }
    fn check(&mut self) -> io::Result<()> {
        self.roots()?;
        if !self.guard.closed() {
            return Err(refused());
        }
        self.roots()?;
        if self.windows {
            self.private(self.profile, true)?;
            self.private(self.registry, true)?;
        }
        if let Some((p, _)) = &self.snapshot {
            self.private(&p.clone(), true)?;
        }
        for r in self.protected {
            let raw = match read(&self.profile.join(&r.name), false) {
                Ok((v, _)) => Some(v),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            if raw != r.bytes {
                return Err(refused());
            }
        }
        Ok(())
    }
    fn before(&mut self, phase: &str, p: &Path) -> io::Result<()> {
        self.guard.phase(phase, p);
        self.check()
    }
    fn exclusive(&mut self, p: &Path, bytes: &[u8], owned: &mut Vec<Owned>) -> io::Result<()> {
        self.before("exclusiveCreate", p)?;
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            opts.custom_flags(0x00200000);
        }
        let mut file = opts.open(p)?;
        let key = file_key(&file)?;
        let index = owned.len();
        owned.push(Owned {
            path: p.into(),
            key,
            bytes: bytes.into(),
            complete: false,
        });
        #[cfg(unix)]
        {
            self.before("privateFileMode", p)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        self.private(p, false)?;
        if !same(file_key(&open_leaf(p, false)?)?, key) {
            return Err(refused());
        }
        let mut offset = 0;
        while offset < bytes.len() {
            self.before("writePrivateContent", p)?;
            self.private(p, false)?;
            if !same(file_key(&open_leaf(p, false)?)?, key) {
                return Err(refused());
            }
            let n = file.write(&bytes[offset..])?;
            if n == 0 {
                return Err(refused());
            }
            offset += n;
        }
        self.before("flushPrivateContent", p)?;
        file.sync_all()?;
        let (raw, current) = read(p, false)?;
        if !same(current, key) || raw != bytes {
            return Err(refused());
        }
        owned[index].complete = true;
        Ok(())
    }
    fn remove(&mut self, o: &Owned, phase: &str) -> (bool, bool) {
        let check = |state: &Self| -> io::Result<bool> {
            state.roots()?;
            match read(&o.path, true) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
                Ok((b, k)) if o.complete && same(k, o.key) && b == o.bytes => Ok(true),
                _ => Err(refused()),
            }
        };
        match check(self) {
            Ok(false) => (false, true),
            Err(_) => (false, false),
            Ok(true) => {
                if self.before(phase, &o.path).is_err() || check(self).ok() != Some(true) {
                    return (false, false);
                }
                match fs::remove_file(&o.path) {
                    Ok(()) => (true, true),
                    Err(_) => (false, false),
                }
            }
        }
    }
}
fn default_receipt() -> Value {
    json!({"schemaVersion":2,"status":"refused","createdCount":0,"recoveryRequired":true,"ownedFilesRolledBack":false,"snapshotBindingVerified":false,"privateSnapshotsPreserved":false,"profileApplyEndpointImplemented":false,"vendorResumeImplemented":false,"distributedLeaseImplemented":false})
}
/// Append records without replacing existing names. Receipts never claim resume or distributed leases.
pub fn append(
    profile: &Path,
    registry: &Path,
    records: &[Record],
    protected: &[Record],
    windows: bool,
    guard: &mut dyn Guard,
) -> Value {
    let setup = (|| -> io::Result<(Identity, Identity)> {
        if profile == registry
            || std::path::absolute(profile)?.as_os_str() != profile.as_os_str()
            || std::path::absolute(registry)?.as_os_str() != registry.as_os_str()
            || !registry.starts_with(profile)
            || records.is_empty()
            || records.len() > 200
        {
            return Err(refused());
        }
        let mut names = std::collections::HashSet::new();
        for r in records {
            if !record_name(&r.name)
                || !names.insert(&r.name)
                || r.bytes.as_ref().is_none_or(|b| b.len() > 2_000_000)
            {
                return Err(refused());
            }
        }
        names.clear();
        for r in protected {
            if !leaf(&r.name)
                || r.name.starts_with(".history-index-")
                || !names.insert(&r.name)
                || r.bytes.as_ref().is_some_and(|b| b.len() > 2_000_000)
            {
                return Err(refused());
            }
        }
        Ok((directory(profile)?, directory(registry)?))
    })();
    let Ok((pk, rk)) = setup else {
        return default_receipt();
    };
    let mut state = State {
        profile,
        registry,
        pk,
        rk,
        snapshot: None,
        protected,
        windows,
        guard,
    };
    let mut made = Vec::new();
    let mut stages = Vec::new();
    let mut snapshots = Vec::new();
    let result = (|| -> io::Result<()> {
        state.check()?;
        for r in records {
            match fs::symlink_metadata(registry.join(&r.name)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                _ => return Err(refused()),
            }
        }
        let folder = profile.join(format!(".history-index-snapshot-{}", unique()?));
        state.before("createSnapshot", &folder)?;
        // Only Unix narrows the mode; elsewhere the builder needs no `mut` (clippy -D warnings).
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder.create(&folder)?;
        state.snapshot = Some((folder.clone(), directory(&folder)?));
        state.private(&folder, true)?;
        let mut manifest = vec![];
        for (i, r) in protected.iter().enumerate() {
            if let Some(b) = &r.bytes {
                state.exclusive(
                    &folder.join(format!("protected-{i}.bin")),
                    b,
                    &mut snapshots,
                )?;
            }
            manifest.push(json!({"index":i,"present":r.bytes.is_some(),"sha256":r.bytes.as_ref().map(|b|hex(b))}));
        }
        state.exclusive(
            &folder.join("snapshot-manifest.json"),
            &serde_json::to_vec(&manifest)?,
            &mut snapshots,
        )?;
        for r in records {
            let bytes = r.bytes.as_deref().ok_or_else(refused)?;
            let stage = registry.join(format!(".history-index-stage-{}", unique()?));
            state.exclusive(&stage, bytes, &mut stages)?;
            state.guard.phase("staged", &stage);
            state.check()?;
            let key = stages.last().ok_or_else(refused)?.key;
            let dest = registry.join(&r.name);
            state.before("linkCreateOnly", &dest)?;
            state.private(&stage, false)?;
            let (raw, k) = read(&stage, false)?;
            if raw != bytes || !same(k, key) {
                return Err(refused());
            }
            fs::hard_link(&stage, &dest)?;
            made.push(Owned {
                path: dest.clone(),
                key,
                bytes: bytes.into(),
                complete: true,
            });
            state.guard.phase("created", &dest);
            state.check()?;
            state.private(&dest, false)?;
            let (raw, k) = read(&dest, true)?;
            if raw != bytes || !same(k, key) {
                return Err(refused());
            }
            let o = stages.last().ok_or_else(refused)?;
            if !state.remove(o, "cleanupOwnedStage").1 {
                return Err(refused());
            }
            stages.pop();
        }
        state.check()?;
        for o in &made {
            state.private(&o.path, false)?;
            let (raw, k) = read(&o.path, false)?;
            if raw != o.bytes || !same(k, o.key) {
                return Err(refused());
            }
        }
        Ok(())
    })();
    if result.is_ok() {
        return json!({"schemaVersion":2,"status":"created_metadata","createdCount":made.len(),"protectedBytesUnchanged":true,"recoveryRequired":false,"ownedFilesRolledBack":false,"snapshotBindingVerified":true,"privateSnapshotsPreserved":true,"profileApplyEndpointImplemented":false,"vendorResumeImplemented":false,"distributedLeaseImplemented":false});
    }
    let mut safe = state.roots().is_ok();
    if safe {
        for o in made.iter().chain(stages.iter()).rev() {
            if !state.remove(o, "rollbackOwnedFile").1 {
                safe = false;
            }
        }
    }
    let mut bound = state.snapshot.is_some() && state.roots().is_ok();
    let private = if let Some((p, _)) = &state.snapshot {
        state.private(&p.clone(), true).is_ok()
    } else {
        false
    };
    bound = bound && state.roots().is_ok();
    json!({"schemaVersion":2,"status":"refused","createdCount":0,"createdCountBeforeRefusal":made.len(),"recoveryRequired":!safe||state.snapshot.is_some()&&(!bound||!private),"ownedFilesRolledBack":safe,"snapshotBindingVerified":bound,"privateSnapshotsPreserved":bound&&private,"profileApplyEndpointImplemented":false,"vendorResumeImplemented":false,"distributedLeaseImplemented":false})
}

/// Retain the three newest owned history snapshots; foreign entries are reported and preserved.
pub fn cleanup(root: &Path) -> io::Result<Value> {
    let root_key = directory(root)?;
    let mut snapshots = Vec::new();
    let mut skipped = Vec::new();
    for entry in fs::read_dir(root)?.take(1000) {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name
            .strip_prefix(".history-index-snapshot-")
            .is_some_and(|s| {
                s.len() == 32
                    && s.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        match directory(&entry.path()) {
            Ok(key) => snapshots.push((name, key, metadata.modified()?)),
            Err(_) => skipped.push(name),
        }
    }
    snapshots.sort_by_key(|a| std::cmp::Reverse(a.2));
    let mut kept = vec![];
    let mut deleted = vec![];
    for (i, (name, key, _)) in snapshots.into_iter().enumerate() {
        if i < 3 {
            kept.push(name);
            continue;
        }
        if deleted.len() >= 200 {
            skipped.push(name);
            continue;
        }
        let dir = root.join(&name);
        let result = (|| -> io::Result<()> {
            if !same(directory(root)?, root_key) || !same(directory(&dir)?, key) {
                return Err(refused());
            }
            let entries = fs::read_dir(&dir)?
                .take(65)
                .collect::<Result<Vec<_>, _>>()?;
            if entries.len() > 64 {
                return Err(refused());
            }
            let mut children = vec![];
            let mut manifest = false;
            for entry in entries {
                let n = entry.file_name().into_string().map_err(|_| refused())?;
                if n == "snapshot-manifest.json" {
                    manifest = true;
                } else if !n
                    .strip_prefix("protected-")
                    .and_then(|n| n.strip_suffix(".bin"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err(refused());
                }
                let k = identity(&entry.path())?;
                if k.2 != 1 {
                    return Err(refused());
                }
                children.push((entry.path(), k));
            }
            if !manifest {
                return Err(refused());
            }
            for (path, k) in children {
                if !same(directory(root)?, root_key)
                    || !same(directory(&dir)?, key)
                    || identity(&path)? != k
                {
                    return Err(refused());
                }
                fs::remove_file(path)?;
            }
            if !same(directory(root)?, root_key) || !same(directory(&dir)?, key) {
                return Err(refused());
            }
            fs::remove_dir(&dir)
        })();
        if result.is_ok() {
            deleted.push(name);
        } else {
            skipped.push(name);
        }
    }
    skipped.sort();
    Ok(json!({"kept":kept,"deleted":deleted,"skipped":skipped}))
}
