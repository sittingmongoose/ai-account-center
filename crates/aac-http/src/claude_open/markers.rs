//! Port of the core's durable uncertain-append hold (`pendingMarkerState`, `armPendingMarker`
//! and the marker file protocol in `history-index-sync.cjs`) plus the without-core fallback of
//! `claude-history-hold-fallback.ts`. One Open owns one create-only marker file: pending while
//! a batch's append may be in flight, finished between batches. Finishing its own file never
//! clears another operation; a restart or a removed policy never proves quiescence.
//!
//! The service runs on the Linux dashboard host; the ownership rules are the core's POSIX
//! rules. On a non-POSIX host the core refuses to arm (`ownedPrivate` is false there), so the
//! TypeScript path keeps that behaviour until the cut.
use super::js::{sha, stringify};
use super::sync::PROFILES;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The core's `MARKER_LIMIT`.
pub const MARKER_LIMIT: usize = 256;
const MARKER_DIRECTORY: &str = "claude-history-pending";

/// The core's `Refused` reason tokens for the marker protocol.
#[derive(Debug, PartialEq, Eq)]
pub enum MarkerRefused {
    /// An uncertain append holds Open (`history_append_pending`).
    AppendPending,
    /// Private marker storage is unavailable (`history_marker_unavailable`).
    Unavailable,
    /// The bounded marker store is full (`history_marker_store_full`).
    StoreFull,
    /// The marker folder is absent: the one non-holding read error (internal).
    AbsentFolder,
}

type Checked<T> = Result<T, MarkerRefused>;

fn marker_directory(directory: &Path) -> PathBuf {
    directory.join(MARKER_DIRECTORY)
}

/// The core's `MARKER_NAME`: profile, platform and a 32-hex nonce.
fn marker_name(name: &str) -> Option<(&str, &str, &str)> {
    let (profile, rest) = name.split_once('-')?;
    if !PROFILES.contains(&profile) {
        return None;
    }
    let (platform, rest) = rest.split_once('-')?;
    if !matches!(platform, "mac" | "windows") {
        return None;
    }
    let nonce = rest.strip_suffix(".json")?;
    (nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some((profile, platform, nonce))
}

/// The core's `ownedPrivate` for files and directories: real, private, owned, exact mode.
fn owned_private(meta: &fs::Metadata, directory: bool) -> bool {
    let file_type = meta.file_type();
    let right_kind = if directory {
        file_type.is_dir()
    } else {
        file_type.is_file()
    };
    right_kind
        && !file_type.is_symlink()
        && meta.uid() == process_uid()
        && meta.permissions().mode() & 0o777 == if directory { 0o700 } else { 0o600 }
        && (directory || meta.nlink() == 1)
}

/// The process real uid, the core's `process.getuid()`: read once from `/proc/self/status`
/// (the service runs on Linux; no unsafe call).
pub(crate) fn process_uid() -> u32 {
    static UID: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix("Uid:")
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|real| real.parse().ok())
                })
            })
            .unwrap_or(u32::MAX)
    });
    *UID
}

/// The core's `rootIsPrivate`: the CCS root itself is private and canonically spelled.
fn root_is_private(directory: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(directory) else {
        return false;
    };
    if !owned_private(&meta, true) {
        return false;
    }
    match (fs::canonicalize(directory), directory.canonicalize()) {
        (Ok(real), Ok(resolved)) => real == resolved,
        _ => false,
    }
}

/// The core's `syncDirectory`: fsync the directory inode itself.
fn sync_directory(directory: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(directory)?;
    file.sync_all()
}

/// One read marker: its parsed record, exact bytes and opened inode identity.
struct ReadMarker {
    record: Value,
    bytes: Vec<u8>,
    dev: u64,
    ino: u64,
}

/// The core's `readMarker`: every check fails closed as `history_append_pending`.
fn read_marker(filename: &Path) -> Checked<ReadMarker> {
    let pending = || MarkerRefused::AppendPending;
    let before = fs::symlink_metadata(filename).map_err(|_| pending())?;
    if !owned_private(&before, false) || before.len() > 2048 {
        return Err(pending());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(filename)
        .map_err(|_| pending())?;
    let opened = file.metadata().map_err(|_| pending())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| pending())?;
    let after = fs::symlink_metadata(filename).map_err(|_| pending())?;
    if !owned_private(&opened, false)
        || opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.dev() != after.dev()
        || opened.ino() != after.ino()
        || bytes.len() > 2048
        || opened.len() != after.len()
        || mtime_ms(&opened) != mtime_ms(&after)
        || ctime_ms(&opened) != ctime_ms(&after)
    {
        return Err(pending());
    }
    let Some(record) = super::js::parse(&bytes) else {
        return Err(pending());
    };
    if bytes != stringify(&record).as_bytes() {
        return Err(pending());
    }
    let name = filename
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(pending)?;
    let Some((profile, platform, nonce)) = marker_name(name) else {
        return Err(pending());
    };
    let finished = record["state"] == json!("finished");
    let allowed: &[&str] = if finished {
        &[
            "version",
            "profileId",
            "targetPlatform",
            "nonce",
            "state",
            "createdAt",
            "finishedAt",
            "terminalReceiptSha256",
        ]
    } else {
        &[
            "version",
            "profileId",
            "targetPlatform",
            "nonce",
            "state",
            "createdAt",
        ]
    };
    let Some(map) = record.as_object() else {
        return Err(pending());
    };
    if map.len() != allowed.len()
        || map.keys().any(|key| !allowed.contains(&key.as_str()))
        || record["version"] != json!(1)
        || record["profileId"] != json!(profile)
        || record["targetPlatform"] != json!(platform)
        || record["nonce"] != json!(nonce)
        || !matches!(record["state"].as_str(), Some("pending" | "finished"))
        || !record["createdAt"].as_str().is_some_and(iso_date)
        || (finished
            && (!record["finishedAt"].as_str().is_some_and(iso_date)
                || !record["terminalReceiptSha256"]
                    .as_str()
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))))
    {
        return Err(pending());
    }
    Ok(ReadMarker {
        record,
        bytes,
        dev: opened.dev(),
        ino: opened.ino(),
    })
}

/// Node's `stat.mtimeMs` as the comparison uses it: float milliseconds.
fn mtime_ms(meta: &fs::Metadata) -> f64 {
    meta.mtime() as f64 * 1000.0 + meta.mtime_nsec() as f64 / 1_000_000.0
}

/// Node's `stat.ctimeMs`.
fn ctime_ms(meta: &fs::Metadata) -> f64 {
    meta.ctime() as f64 * 1000.0 + meta.ctime_nsec() as f64 / 1_000_000.0
}

/// `Number.isFinite(Date.parse(value))` for the ISO forms the marker protocol writes and the
/// fixtures pin: date-time with optional fraction and `Z`/offset, or a plain date.
fn iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 10 {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    if !digits(0..4) || bytes[4] != b'-' || !digits(5..7) || bytes[7] != b'-' || !digits(8..10) {
        return false;
    }
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..10].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    if bytes.len() == 10 {
        return true;
    }
    if bytes[10] != b'T' || bytes.len() < 19 {
        return false;
    }
    if !digits(11..13) || bytes[13] != b':' || !digits(14..16) || bytes[16] != b':' || !digits(17..19)
    {
        return false;
    }
    let hour: u32 = value[11..13].parse().unwrap_or(99);
    let minute: u32 = value[14..16].parse().unwrap_or(99);
    let second: u32 = value[17..19].parse().unwrap_or(99);
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut rest = &value[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let end = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        if end == 0 {
            return false;
        }
        rest = &fraction[end..];
    }
    if rest == "Z" {
        return true;
    }
    let offset = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-'));
    match offset {
        Some(offset) => {
            offset.len() == 5
                && offset.as_bytes()[0..2].iter().all(u8::is_ascii_digit)
                && offset.as_bytes()[2] == b':'
                && offset.as_bytes()[3..5].iter().all(u8::is_ascii_digit)
        }
        None => rest.is_empty(),
    }
}

/// The core's `pendingMarkerState` result.
pub struct MarkerState {
    /// Whether an Open for this profile and platform is held.
    pub held: bool,
    /// The marker count when not held.
    pub count: usize,
}

/// The core's `pendingMarkerState`: every unreadable or foreign entry holds safely.
pub fn pending_marker_state(directory: &Path, profile_id: &str, target_platform: &str) -> MarkerState {
    if !PROFILES.contains(&profile_id) || !matches!(target_platform, "mac" | "windows") {
        return MarkerState { held: true, count: 0 };
    }
    let root = marker_directory(directory);
    let result = (|| -> Checked<usize> {
        let info = fs::symlink_metadata(&root).map_err(|e| classify(e, &root))?;
        if !root_is_private(directory) || !owned_private(&info, true) {
            return Err(MarkerRefused::AppendPending);
        }
        let entries = fs::read_dir(&root).map_err(|e| classify(e, &root))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| classify(e, &root))?;
            let Some(name) = entry.file_name().into_string().ok() else {
                return Err(MarkerRefused::AppendPending);
            };
            names.push(name);
        }
        if names.len() > MARKER_LIMIT {
            return Err(MarkerRefused::AppendPending);
        }
        for name in &names {
            let Some((profile, platform, _)) = marker_name(name) else {
                return Err(MarkerRefused::AppendPending);
            };
            let marker = read_marker(&root.join(name))?;
            if marker.record["profileId"] == json!(profile)
                && marker.record["targetPlatform"] == json!(platform)
                && marker.record["state"] == json!("pending")
                && profile == profile_id
                && platform == target_platform
            {
                return Err(MarkerRefused::AppendPending);
            }
        }
        Ok(names.len())
    })();
    match result {
        Ok(count) => MarkerState { held: false, count },
        Err(MarkerRefused::AbsentFolder) => MarkerState { held: false, count: 0 },
        Err(_) => MarkerState { held: true, count: 0 },
    }
}

/// The core's catch rule: an absent marker folder is the one non-holding error.
fn classify(error: io::Error, root: &Path) -> MarkerRefused {
    if error.kind() == io::ErrorKind::NotFound && !root.exists() {
        // The caller turns this into the absent-folder answer.
        return MarkerRefused::AbsentFolder;
    }
    MarkerRefused::AppendPending
}

/// The service wrapper's hold check (`claudeHistoryOpenHeld`): only the core's own IDs can hold.
pub fn open_held(directory: &Path, profile_id: &str, platform: &str) -> bool {
    if !PROFILES.contains(&profile_id) {
        return false;
    }
    pending_marker_state(directory, profile_id, platform).held
}

/// `claudeHistoryOpenHeldWithoutCore`: the marker-name rule when the core cannot be loaded.
/// The Rust core is compiled in, so the service never needs this; the port keeps it for the
/// parity table and the TS fallback's contract.
pub fn open_held_without_core(directory: &Path, profile_id: &str, platform: &str) -> bool {
    let segment_ok = !profile_id.is_empty()
        && profile_id.len() <= 64
        && profile_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !segment_ok {
        return true;
    }
    let names = match fs::read_dir(marker_directory(directory)) {
        Ok(entries) => entries,
        Err(error) => return error.kind() != io::ErrorKind::NotFound,
    };
    let prefix = format!("{profile_id}-{platform}-");
    for entry in names.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if let Some(tail) = name.strip_prefix(&prefix) {
            if tail.len() == 37
                && tail.ends_with(".json")
                && tail[..32].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return true;
            }
        }
    }
    false
}

/// The current time as `new Date().toISOString()`.
fn iso_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    iso_from_ms(ms)
}

/// Milliseconds since the epoch to the ISO form `Date.toISOString` writes.
pub fn iso_from_ms(ms: u64) -> String {
    let days = ms / 86_400_000;
    let time = ms % 86_400_000;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        (time / 60_000) % 60,
        (time / 1000) % 60,
        time % 1000
    )
}

/// Howard Hinnant's civil-from-days algorithm (the inverse of `Date.toISOString`'s calendar).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 16 random bytes as lowercase hex, from the kernel's urandom (never a guessable source).
fn nonce() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// One Open's durable marker: the core's `armPendingMarker` handle.
#[derive(Debug)]
pub struct PendingMarker {
    directory: PathBuf,
    filename: PathBuf,
    file: Option<File>,
    record: Value,
    bytes: Vec<u8>,
    finished_bytes: Option<Vec<u8>>,
    released: bool,
}

impl PendingMarker {
    /// The core's `armPendingMarker`: refuse a held profile, a nonprivate root or a full store
    /// before any file is created; then create this Open's own create-only marker.
    pub fn arm(directory: &Path, profile_id: &str, target_platform: &str) -> Checked<Self> {
        if pending_marker_state(directory, profile_id, target_platform).held {
            return Err(MarkerRefused::AppendPending);
        }
        if !root_is_private(directory) {
            return Err(MarkerRefused::Unavailable);
        }
        let root = marker_directory(directory);
        match fs::create_dir(&root) {
            Ok(()) => {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                    .map_err(|_| MarkerRefused::Unavailable)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(MarkerRefused::Unavailable),
        }
        sync_directory(directory).map_err(|_| MarkerRefused::Unavailable)?;
        let state = pending_marker_state(directory, profile_id, target_platform);
        if state.held {
            return Err(MarkerRefused::AppendPending);
        }
        if state.count >= MARKER_LIMIT {
            return Err(MarkerRefused::StoreFull);
        }
        let nonce = nonce().map_err(|_| MarkerRefused::Unavailable)?;
        let filename = root.join(format!("{profile_id}-{target_platform}-{nonce}.json"));
        let record = json!({
            "version": 1, "profileId": profile_id, "targetPlatform": target_platform,
            "nonce": nonce, "state": "pending", "createdAt": iso_now()
        });
        let bytes = stringify(&record).into_bytes();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&filename)
            .map_err(|_| MarkerRefused::Unavailable)?;
        let marker = Self {
            directory: directory.to_path_buf(),
            filename,
            file: Some(file),
            record,
            bytes: bytes.clone(),
            finished_bytes: None,
            released: false,
        };
        // The core writes, fsyncs the file and the folder, then proves the binding; a failure
        // closes the descriptor (Drop) and leaves any created file to hold Open.
        let result = (|| -> Checked<()> {
            let mut file = marker.file()?;
            file.write_all(&bytes).map_err(|_| MarkerRefused::AppendPending)?;
            file.sync_all().map_err(|_| MarkerRefused::AppendPending)?;
            sync_directory(&root).map_err(|_| MarkerRefused::AppendPending)?;
            marker.assert_bound()
        })();
        match result {
            Ok(()) => Ok(marker),
            Err(reason) => Err(reason),
        }
    }

    fn file(&self) -> Checked<&File> {
        self.file.as_ref().ok_or(MarkerRefused::AppendPending)
    }

    /// The core's `bound()`: this inode still holds exactly this operation's bytes.
    pub fn assert_bound(&self) -> Checked<()> {
        let current = read_marker(&self.filename)?;
        let original = self.file()?.metadata().map_err(|_| MarkerRefused::AppendPending)?;
        let root = marker_directory(&self.directory);
        let root_meta = fs::symlink_metadata(&root).map_err(|_| MarkerRefused::AppendPending)?;
        if !root_is_private(&self.directory)
            || !owned_private(&root_meta, true)
            || current.dev != original.dev()
            || current.ino != original.ino()
            || current.bytes != self.bytes
        {
            return Err(MarkerRefused::AppendPending);
        }
        Ok(())
    }

    /// The core's `finish`: only a trusted terminal receipt finishes this held inode.
    pub fn finish(&mut self, terminal_receipt: &Value) -> Checked<()> {
        let valid_receipt = terminal_receipt.is_object()
            && terminal_receipt["writerQuiescent"] == json!(true)
            && matches!(
                terminal_receipt["status"].as_str(),
                Some("created_metadata" | "refused")
            )
            && terminal_receipt["createdCount"]
                .as_u64()
                .is_some_and(|n| n <= 200);
        if self.released || self.finished_bytes.is_some() || !valid_receipt {
            return Err(MarkerRefused::AppendPending);
        }
        self.assert_bound()?;
        let mut finished_record = self.record.as_object().cloned().unwrap_or_default();
        finished_record.insert("state".into(), json!("finished"));
        finished_record.insert("finishedAt".into(), json!(iso_now()));
        finished_record.insert(
            "terminalReceiptSha256".into(),
            json!(sha(stringify(terminal_receipt).as_bytes())),
        );
        let finished = stringify(&Value::Object(finished_record)).into_bytes();
        let file = self.file()?;
        file.set_len(0).map_err(|_| MarkerRefused::AppendPending)?;
        let mut file = file;
        file.seek(SeekFrom::Start(0)).map_err(|_| MarkerRefused::AppendPending)?;
        file.write_all(&finished).map_err(|_| MarkerRefused::AppendPending)?;
        file.sync_all().map_err(|_| MarkerRefused::AppendPending)?;
        let current = read_marker(&self.filename)?;
        let original = self.file()?.metadata().map_err(|_| MarkerRefused::AppendPending)?;
        if current.dev != original.dev() || current.ino != original.ino() || current.bytes != finished
        {
            return Err(MarkerRefused::AppendPending);
        }
        self.finished_bytes = Some(finished);
        Ok(())
    }

    /// The core's `rearm`: back to pending before the next batch, only after this marker's own
    /// finish and only while no other pending marker holds this profile and platform.
    pub fn rearm(&mut self) -> Checked<()> {
        let Some(finished_bytes) = self.finished_bytes.clone() else {
            return Err(MarkerRefused::AppendPending);
        };
        if self.released {
            return Err(MarkerRefused::AppendPending);
        }
        let current = read_marker(&self.filename)?;
        let original = self.file()?.metadata().map_err(|_| MarkerRefused::AppendPending)?;
        let root = marker_directory(&self.directory);
        let root_meta = fs::symlink_metadata(&root).map_err(|_| MarkerRefused::AppendPending)?;
        if !root_is_private(&self.directory)
            || !owned_private(&root_meta, true)
            || current.dev != original.dev()
            || current.ino != original.ino()
            || current.bytes != finished_bytes
        {
            return Err(MarkerRefused::AppendPending);
        }
        let profile = self.record["profileId"].as_str().unwrap_or_default();
        let platform = self.record["targetPlatform"].as_str().unwrap_or_default();
        if pending_marker_state(&self.directory, profile, platform).held {
            return Err(MarkerRefused::AppendPending);
        }
        let file = self.file()?;
        file.set_len(0).map_err(|_| MarkerRefused::AppendPending)?;
        let mut file = file;
        file.seek(SeekFrom::Start(0)).map_err(|_| MarkerRefused::AppendPending)?;
        file.write_all(&self.bytes).map_err(|_| MarkerRefused::AppendPending)?;
        file.sync_all().map_err(|_| MarkerRefused::AppendPending)?;
        self.finished_bytes = None;
        self.assert_bound()
    }

    /// The core's `release`: close this operation's descriptor; the file stays on disk.
    pub fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.file = None;
        }
    }
}

impl Drop for PendingMarker {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_from_ms_matches_date_to_iso_string() {
        assert_eq!(iso_from_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_759_363_200_000), "2025-10-02T00:00:00.000Z");
        assert_eq!(iso_from_ms(1_759_363_201_123), "2025-10-02T00:00:01.123Z");
    }

    #[test]
    fn iso_date_accepts_the_marker_forms() {
        assert!(iso_date("2026-10-02T00:00:00Z"));
        assert!(iso_date("2026-10-02T00:00:00.000Z"));
        assert!(iso_date("2026-10-02"));
        assert!(!iso_date("not-a-date"));
        assert!(!iso_date("2026-13-02T00:00:00Z"));
    }

    #[test]
    fn marker_names_bind_profile_platform_and_nonce() {
        assert_eq!(
            marker_name("platyr-windows-0123456789abcdef0123456789abcdef.json"),
            Some(("platyr", "windows", "0123456789abcdef0123456789abcdef"))
        );
        assert_eq!(marker_name("unknown-mac-0123456789abcdef0123456789abcdef.json"), None);
        assert_eq!(marker_name("platyr-linux-0123456789abcdef0123456789abcdef.json"), None);
        assert_eq!(marker_name("platyr-mac-short.json"), None);
    }
}
