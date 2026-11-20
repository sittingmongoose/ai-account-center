//! Exclusive private presence markers used by older processes that use O_EXCL.
use super::*;

/// An exclusive create-new marker. Its owner removes only the inode it created.
pub struct Presence {
    #[cfg(not(windows))]
    path: PathBuf,
    file: File,
}

/// A regular marker's contents and the identity used for any later removal.
pub struct Observed {
    /// The marker's bounded bytes. An unreadable or oversized marker is opaque.
    pub bytes: Option<Vec<u8>>,
    /// Timestamp used for the legacy writer's short initial publication window.
    pub modified: Option<std::time::SystemTime>,
    #[cfg(not(windows))]
    metadata: fs::Metadata,
    #[cfg(windows)]
    key: Option<(u64, u64)>,
    #[cfg(windows)]
    _witness: Option<File>,
}

#[cfg(not(windows))]
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
    #[cfg(not(windows))]
    {
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
    #[cfg(windows)]
    {
        use std::io::Read as IoRead;
        let mut observed = Observed {
            bytes: None,
            modified: metadata.modified().ok(),
            key: None,
            _witness: None,
        };
        if let Ok(file) = crate::profile_writes::open_leaf(path, false) {
            observed.key = marker_key(&file);
            observed.modified = file.metadata().and_then(|meta| meta.modified()).ok();
            let mut bytes = Vec::new();
            if observed.key.is_some()
                && (&file).take(513).read_to_end(&mut bytes).is_ok()
                && bytes.len() <= 512
            {
                observed.bytes = Some(bytes);
            }
            observed._witness = Some(file);
        }
        Ok(Some(observed))
    }
}

/// Remove a stale marker only while it still names the same observed inode.
pub fn remove_observed(path: &Path, observed: &Observed) -> Result<(), Failure> {
    #[cfg(not(windows))]
    {
        remove_if_same(path, &observed.metadata)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::GENERIC_READ;
        use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_OPEN_REPARSE_POINT};
        let file = fs::OpenOptions::new()
            .access_mode(GENERIC_READ | DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if observed.key.is_none()
            || marker_key(&file) != observed.key
            || !file.metadata()?.is_file()
        {
            return Err(Failure::new(Cause::Busy, "presence marker changed"));
        }
        delete_marker_handle(&file).map_err(Into::into)
    }
}

#[cfg(not(windows))]
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

#[cfg(windows)]
fn marker_key(file: &File) -> Option<(u64, u64)> {
    let (volume, index, links) = crate::profile_writes::file_key(file).ok()?;
    (links == 1).then_some((volume, index))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn delete_marker_handle(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // The handle owns the checked file and the buffer has the layout required by this call.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&disposition).cast(),
            std::mem::size_of_val(&disposition) as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Foundation::GENERIC_WRITE;
            use windows_sys::Win32::Storage::FileSystem::DELETE;
            options.access_mode(GENERIC_WRITE | DELETE);
        }
        let file = options.open(path).map_err(|err| {
            if err.kind() == io::ErrorKind::AlreadyExists {
                Failure::new(Cause::Busy, "presence marker already held")
            } else {
                err.into()
            }
        })?;
        let mut lease = Self {
            #[cfg(not(windows))]
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
        #[cfg(not(windows))]
        if let Ok(metadata) = self.file.metadata() {
            let _ = remove_if_same(&self.path, &metadata);
        }
        #[cfg(windows)]
        let _ = delete_marker_handle(&self.file);
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn a_stale_windows_observation_cannot_remove_a_new_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker");
        let parked = dir.path().join("parked");
        fs::write(&path, b"old").unwrap();
        let observed = observe(&path).unwrap().unwrap();
        assert_eq!(observed.bytes.as_deref(), Some(b"old".as_slice()));

        fs::rename(&path, &parked).unwrap();
        let replacement = Presence::claim(&path, b"new").unwrap();
        assert!(remove_observed(&path, &observed).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");

        remove_observed(&parked, &observed).unwrap();
        drop(observed);
        assert!(!parked.exists());
        drop(replacement);
        assert!(!path.exists());
    }
}
