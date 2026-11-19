//! G16b: the answer store's writer lock in Rust, ported whole from
//! `src/web-server/status/writer-lock.ts`, so a Rust process and a TypeScript process share one
//! `<store>/answers.lock` and never both append to `answers.jsonl` or touch the answer key.
//!
//! The lock file holds the same JSON text the TypeScript writer writes
//! (`{"pid":..,"host":..,"boot":..,"start":..,"at":..}`), and a lock is judged by the same rules:
//! - on this host, it is stale when its pid no longer runs (a zombie counts as ended), or, on
//!   Linux, when it was written in an earlier boot or its pid now belongs to a process that
//!   started later. Nothing is judged by the clock;
//! - from another host, when no one has refreshed it for [`UNREFRESHED_MS`];
//! - with no readable holder, after [`STALE_UNREADABLE_MS`].
//!
//! Take-overs happen one at a time under `<lock>.taking`. The holder refreshes the lock's time
//! stamp on every confirmation. [`release_writer_locks`] is the exit hook the TypeScript writer
//! registers with `process.once('exit')`: the service calls it on shutdown.
use serde_json::{Map, Value as Json};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// The lock's file name in the store folder.
pub const WRITER_LOCK_FILE: &str = "answers.lock";
/// A lock with no readable holder is left behind after this long.
pub const STALE_UNREADABLE_MS: f64 = 10_000.0;
/// Another host's lock its holder has not refreshed for this long is gone.
pub const UNREFRESHED_MS: f64 = 10.0 * 60.0 * 1000.0;

/// Why the lock could not be held.
#[derive(Debug)]
pub enum LockError {
    /// `WriterBusyError`: another live process holds it or is taking it over. The text is the
    /// holder as TypeScript names it (`pid 12 on host`, or `a process`).
    Busy(String),
    /// The file system refused.
    Io(io::Error),
}

impl LockError {
    /// The TypeScript `WriterBusyError` message.
    pub fn message(&self) -> String {
        match self {
            Self::Busy(holder) => {
                format!("Another AI Account Center process ({holder}) records answers from this folder.")
            }
            Self::Io(error) => error.to_string(),
        }
    }
}

impl From<io::Error> for LockError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Holder {
    pid: i64,
    host: String,
    boot: Option<String>,
    start: Option<String>,
}

fn held() -> &'static Mutex<HashMap<PathBuf, String>> {
    static HELD: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `os.hostname()`.
pub fn host_name() -> String {
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        #[cfg(unix)]
        {
            let mut buf = [0u8; 256];
            // SAFETY: the buffer is valid for its length; gethostname NUL-terminates within it.
            let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
            if rc == 0 {
                let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
                return String::from_utf8_lossy(&buf[..end]).into_owned();
            }
            String::new()
        }
        #[cfg(not(unix))]
        {
            // libuv's uv_os_gethostname is the Winsock host name, which `hostname` prints.
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .filter(|name| !name.is_empty())
                .or_else(|| std::env::var("COMPUTERNAME").ok())
                .unwrap_or_default()
        }
    })
    .clone()
}

/// When a process started, in clock ticks since boot, on Linux; None elsewhere.
fn process_start(pid: i64) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rfind(')')? + 2;
    stat.get(after..)?.split(' ').nth(19).map(str::to_owned)
}

/// This boot's id on Linux; None elsewhere.
fn boot_id() -> Option<String> {
    let text = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = text.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

fn self_pid() -> i64 {
    i64::from(std::process::id())
}

fn read_text(file: &Path) -> io::Result<Option<String>> {
    match fs::read(file) {
        Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_holder(text: &str) -> Option<Holder> {
    let value: Json = serde_json::from_str(text).ok()?;
    let pid = value.get("pid")?;
    // Number.isSafeInteger
    let pid = pid
        .as_f64()
        .filter(|p| p.fract() == 0.0 && p.abs() <= 9_007_199_254_740_991.0)? as i64;
    let host = value.get("host")?.as_str()?.to_owned();
    let text_of = |key: &str| value.get(key).and_then(Json::as_str).map(str::to_owned);
    Some(Holder {
        pid,
        host,
        boot: text_of("boot"),
        start: text_of("start"),
    })
}

/// `process.kill(pid, 0)`, with a Linux zombie counted as ended.
fn running(pid: i64) -> bool {
    #[cfg(unix)]
    {
        let Ok(raw) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // kill(0, 0) and negative pids signal groups in POSIX; Node refuses neither, but a lock
        // never names them, so they read as ended.
        if raw <= 0 {
            return false;
        }
        // SAFETY: signal 0 sends nothing; it only checks the pid.
        let rc = unsafe { libc::kill(raw, 0) };
        if rc != 0 {
            return io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        }
    }
    #[cfg(not(unix))]
    {
        if !windows_running(pid) {
            return false;
        }
    }
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        if let Some(at) = stat.rfind(')') {
            let state = stat.as_bytes().get(at + 2).copied();
            if state == Some(b'Z') || state == Some(b'X') {
                return false;
            }
        }
    }
    true
}

#[cfg(not(unix))]
fn windows_running(pid: i64) -> bool {
    // Without a process API binding here, ask tasklist: a lock is only judged when it is not
    // ours, so this is rare. Unknown counts as running, which never takes a live lock.
    let Ok(out) = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
    else {
        return true;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.contains(&format!("\"{pid}\""))
}

fn stale(holder: Option<&Holder>, mtime_ms: f64) -> bool {
    let Some(holder) = holder else {
        return now_ms() - mtime_ms > STALE_UNREADABLE_MS;
    };
    if holder.host != host_name() {
        return now_ms() - mtime_ms > UNREFRESHED_MS;
    }
    if let (Some(boot), Some(theirs)) = (boot_id(), holder.boot.as_ref()) {
        if &boot != theirs {
            return true;
        }
    }
    if !running(holder.pid) {
        return true;
    }
    match (process_start(holder.pid), holder.start.as_ref()) {
        (Some(start), Some(theirs)) => &start != theirs,
        _ => false,
    }
}

/// Creates `file` holding `text`, only if nothing has that name; false when something does.
fn create_exclusive(file: &Path, text: &str) -> io::Result<bool> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = match options.open(file) {
        Ok(handle) => handle,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = handle.write_all(text.as_bytes()).and_then(|()| handle.sync_all()) {
        drop(handle);
        let _ = fs::remove_file(file);
        return Err(error);
    }
    Ok(true)
}

/// This process as a lock names it, in `JSON.stringify` order.
fn self_text() -> String {
    let mut map = Map::new();
    map.insert("pid".into(), Json::from(self_pid()));
    map.insert("host".into(), Json::from(host_name()));
    map.insert("boot".into(), boot_id().map_or(Json::Null, Json::from));
    map.insert("start".into(), process_start(self_pid()).map_or(Json::Null, Json::from));
    map.insert("at".into(), Json::from(now_ms() as u64));
    Json::Object(map).to_string()
}

fn is_self(holder: Option<&Holder>) -> bool {
    holder.is_some_and(|holder| {
        holder.pid == self_pid()
            && holder.host == host_name()
            && holder.boot.as_ref().is_none_or(|b| Some(b) == boot_id().as_ref())
            && holder
                .start
                .as_ref()
                .is_none_or(|s| Some(s) == process_start(self_pid()).as_ref())
    })
}

fn mtime_ms(meta: &fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// The text and time of `file`, or None when it is gone.
fn read_lock(file: &Path) -> io::Result<Option<(String, f64)>> {
    let Some(text) = read_text(file)? else {
        return Ok(None);
    };
    match fs::metadata(file) {
        Ok(meta) => Ok(Some((text, mtime_ms(&meta)))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_quietly(file: &Path) {
    let _ = fs::remove_file(file);
}

/// Removes the stale lock `judged`, one take-over at a time under `<lock>.taking`. False when
/// another live process is taking over.
fn take_over(file: &Path, judged: &str) -> io::Result<bool> {
    let mut taking = file.as_os_str().to_owned();
    taking.push(".taking");
    let taking = PathBuf::from(taking);
    let mine = self_text();
    if !create_exclusive(&taking, &mine)? {
        let Some((text, mtime)) = read_lock(&taking)? else {
            return Ok(true);
        };
        let taker = parse_holder(&text);
        if is_self(taker.as_ref()) || stale(taker.as_ref(), mtime) {
            if read_text(&taking)?.as_deref() == Some(text.as_str()) {
                remove_quietly(&taking);
            }
            return Ok(true);
        }
        return Ok(false);
    }
    let result = (|| {
        if read_text(file)?.as_deref() == Some(judged) {
            remove_quietly(file);
        }
        Ok(())
    })();
    if read_text(&taking)?.as_deref() == Some(mine.as_str()) {
        remove_quietly(&taking);
    }
    result.map(|()| true)
}

fn refresh(file: &Path) {
    if let Ok(handle) = fs::OpenOptions::new().write(true).open(file) {
        let now = SystemTime::now();
        let _ = handle.set_times(fs::FileTimes::new().set_accessed(now).set_modified(now));
    }
}

fn create_store_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// `holdWriterLock`: makes sure this process holds the answer store's writer lock in `dir`,
/// taking it if it is free or stale. Ok(true) when it was not held a moment ago (whatever this
/// process remembered of the store may be stale), Ok(false) when it still was.
pub fn hold_writer_lock(dir: &Path) -> Result<bool, LockError> {
    let file = dir.join(WRITER_LOCK_FILE);
    let mine = held().lock().unwrap_or_else(|e| e.into_inner()).get(&file).cloned();
    if let Some(mine) = mine {
        if read_text(&file)?.as_deref() == Some(mine.as_str()) {
            refresh(&file);
            return Ok(false);
        }
    }
    held().lock().unwrap_or_else(|e| e.into_inner()).remove(&file);
    create_store_dir(dir)?;
    for _ in 0..6 {
        let text = self_text();
        if create_exclusive(&file, &text)? {
            held()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(file.clone(), text);
            return Ok(true);
        }
        let Some((found, mtime)) = read_lock(&file)? else {
            continue;
        };
        let holder = parse_holder(&found);
        if is_self(holder.as_ref()) {
            held()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(file.clone(), found);
            return Ok(true);
        }
        if !stale(holder.as_ref(), mtime) || !take_over(&file, &found)? {
            return Err(LockError::Busy(match holder {
                Some(holder) => format!("pid {} on {}", holder.pid, holder.host),
                None => "a process".to_owned(),
            }));
        }
    }
    Err(LockError::Busy("a process".to_owned()))
}

/// The exit hook: removes every lock this process still holds, if it still names this process.
pub fn release_writer_locks() {
    let held = std::mem::take(&mut *held().lock().unwrap_or_else(|e| e.into_inner()));
    for (file, text) in held {
        if read_text(&file).ok().flatten().as_deref() == Some(text.as_str()) {
            remove_quietly(&file);
        }
    }
}

/// Releases the one lock in `dir`, if this process holds it.
pub fn release_writer_lock(dir: &Path) {
    let file = dir.join(WRITER_LOCK_FILE);
    let text = held().lock().unwrap_or_else(|e| e.into_inner()).remove(&file);
    if let Some(text) = text {
        if read_text(&file).ok().flatten().as_deref() == Some(text.as_str()) {
            remove_quietly(&file);
        }
    }
}

/// For tests: forget the lock in `dir` without removing it (a process that ended without its
/// exit hook, or another instance in this process).
#[doc(hidden)]
pub fn forget_writer_lock_for_tests(dir: &Path) {
    held()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&dir.join(WRITER_LOCK_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_text_is_the_typescript_shape() {
        let text = self_text();
        let keys: Vec<String> = serde_json::from_str::<Map<String, Json>>(&text)
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["pid", "host", "boot", "start", "at"]);
    }

    #[test]
    fn holds_refreshes_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        assert!(hold_writer_lock(dir.path()).unwrap());
        assert!(!hold_writer_lock(dir.path()).unwrap());
        // Forgotten but still naming this process: it is ours again, newly gained.
        forget_writer_lock_for_tests(dir.path());
        assert!(hold_writer_lock(dir.path()).unwrap());
        release_writer_lock(dir.path());
        assert!(!dir.path().join(WRITER_LOCK_FILE).exists());
    }

    #[test]
    fn a_live_holder_on_this_host_is_busy_and_a_dead_one_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(WRITER_LOCK_FILE);
        // A live process that is not this one: the parent of the test runner.
        #[cfg(unix)]
        let live = i64::from(unsafe { libc::getppid() });
        #[cfg(not(unix))]
        let live = {
            // Any running process other than this one: tasklist's own parent shell will do.
            let out = std::process::Command::new("tasklist")
                .args(["/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.lines()
                .filter_map(|l| l.split(',').nth(1))
                .filter_map(|p| p.trim_matches('"').parse::<i64>().ok())
                .find(|p| *p != self_pid() && *p > 4)
                .unwrap()
        };
        fs::write(
            &file,
            format!(
                r#"{{"pid":{live},"host":"{}","boot":null,"start":null,"at":1}}"#,
                host_name()
            ),
        )
        .unwrap();
        match hold_writer_lock(dir.path()) {
            Err(LockError::Busy(holder)) => assert_eq!(holder, format!("pid {live} on {}", host_name())),
            other => panic!("expected busy, got {other:?}"),
        }
        // A pid that cannot run: taken over.
        fs::write(
            &file,
            format!(
                r#"{{"pid":2147480000,"host":"{}","boot":null,"start":null,"at":1}}"#,
                host_name()
            ),
        )
        .unwrap();
        assert!(hold_writer_lock(dir.path()).unwrap());
        release_writer_lock(dir.path());
    }

    #[test]
    fn another_hosts_fresh_lock_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(WRITER_LOCK_FILE);
        fs::write(
            &file,
            r#"{"pid":1,"host":"elsewhere-host","boot":null,"start":null,"at":1}"#,
        )
        .unwrap();
        assert!(matches!(hold_writer_lock(dir.path()), Err(LockError::Busy(h)) if h == "pid 1 on elsewhere-host"));
        // An unreadable holder that was just written is busy too ("a process").
        fs::write(&file, "{").unwrap();
        assert!(matches!(hold_writer_lock(dir.path()), Err(LockError::Busy(h)) if h == "a process"));
    }
}
