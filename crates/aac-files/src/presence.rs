//! Exclusive private presence markers used by older processes that use O_EXCL.
use super::*;

/// An exclusive create-new marker. Its owner removes only the inode it created.
pub struct Presence {
    path: PathBuf,
    file: File,
}

/// A regular marker's contents and the identity used for any later removal.
pub struct Observed {
    /// The marker's bounded bytes. An unreadable or oversized marker is opaque.
    pub bytes: Option<Vec<u8>>,
    /// Timestamp used for the legacy writer's short initial publication window.
    pub modified: Option<std::time::SystemTime>,
    metadata: fs::Metadata,
}

fn same_file(first: &fs::Metadata, later: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        first.dev() == later.dev() && first.ino() == later.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (first, later);
        false
    }
}

/// Read a regular marker, or report its opaque metadata for bounded stale-marker handling.
pub fn observe(path: &Path) -> Result<Option<Observed>, Failure> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(Failure::new(Cause::Refused, "invalid presence marker")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let bytes = match read::read(path, &ReadPolicy::capped(512)) {
        Read::Bytes(bytes, observed) if same_file(&metadata, &observed) => Some(bytes),
        _ => None,
    };
    Ok(Some(Observed {
        bytes,
        modified: metadata.modified().ok(),
        metadata,
    }))
}

/// Remove a stale marker only while it still names the same observed inode.
pub fn remove_observed(path: &Path, observed: &Observed) -> Result<(), Failure> {
    remove_if_same(path, &observed.metadata)
}

fn remove_if_same(path: &Path, expected: &fs::Metadata) -> Result<(), Failure> {
    if path
        .symlink_metadata()
        .is_ok_and(|actual| actual.is_file() && same_file(expected, &actual))
    {
        fs::remove_file(path)?;
        Ok(())
    } else {
        Err(Failure::new(Cause::Busy, "presence marker changed"))
    }
}

impl Presence {
    /// Atomically publish a marker in an existing private directory.
    pub fn claim(path: &Path, bytes: &[u8]) -> Result<Self, Failure> {
        require_absolute(path)?;
        if bytes.len() > 512 {
            return Err(Failure::new(Cause::OutputTooLarge, "presence marker cap"));
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                Failure::new(Cause::Busy, "presence marker already held")
            } else {
                err.into()
            }
        })?;
        let mut lease = Self {
            path: path.into(),
            file,
        };
        lease.file.write_all(bytes)?;
        lease.file.sync_all()?;
        Ok(lease)
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        if let Ok(metadata) = self.file.metadata() {
            let _ = remove_if_same(&self.path, &metadata);
        }
    }
}
