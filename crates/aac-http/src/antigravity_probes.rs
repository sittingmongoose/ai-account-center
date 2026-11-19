//! G6: the Antigravity runtime's read-only probes, behind injected runners.
//!
//! - `runtime-health.ts`: `parserProblemFromProbe`, `unitProblemFromShow`,
//!   `diagnoseRuntimeService` and `createRuntimeServiceProblemReader` over a bounded runner (the
//!   packaged `runtime_health.py` first, then `systemctl --user show`).
//! - `runtime-socket-probe.ts`: `probeRuntimeSocket` (`stopped`/`listening`/`unknown`).
//! - `native-version.ts`: `readNativeRelease` and `readNativeUpdatePaused` over the installed CLI.
//!
//! Every external command is an input: tests pass fake runners, fake scripts and a fake systemctl.
//! Nothing here starts, stops or changes a service, and no provider is reached.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The runtime's user unit (`runtime-health.ts`).
pub const RUNTIME_UNIT: &str = "ai-account-center-antigravity.service";
/// The system interpreter the runtime service runs (`runtime-health.ts`).
pub const SERVICE_PYTHON: &str = "/usr/bin/python3";
/// Every probe's stdout/line cap.
pub const OUTPUT_LIMIT: usize = 64 * 1024;
/// How long one diagnosis is reused while the socket is missing.
pub const HEALTH_TTL_MS: i64 = 60_000;

fn hash_word(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn minor(text: &str) -> bool {
    text.split_once('.').is_some_and(|(major, minor)| {
        (1..=2).contains(&major.len())
            && (1..=3).contains(&minor.len())
            && major
                .bytes()
                .chain(minor.bytes())
                .all(|b| b.is_ascii_digit())
    })
}

fn module_word(text: &str) -> bool {
    let bytes = text.as_bytes();
    matches!(bytes.first(), Some(c) if c.is_ascii_alphabetic() || *c == b'_')
        && text.len() <= 64
        && bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.')
}

/// `publicRuntimeServiceProblem(value)`: the bounded public row, or none.
pub fn public_problem(value: &Value) -> Option<Value> {
    let record = value.as_object()?;
    let reason = record.get("reason")?.as_str()?;
    if ![
        "missing-python-module",
        "parser-mismatch",
        "parser-import-failed",
        "service-failed",
    ]
    .contains(&reason)
    {
        return None;
    }
    let text = |key: &str, ok: fn(&str) -> bool| {
        record
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| ok(text))
            .map_or(Value::Null, |text| json!(text))
    };
    let exit = record
        .get("exitStatus")
        .and_then(Value::as_f64)
        .filter(|n| n.fract() == 0.0 && (0.0..=255.0).contains(n));
    Some(json!({
        "reason": reason,
        "module": text("module", module_word),
        "python": text("python", minor),
        "builtFor": text("builtFor", minor),
        "exitStatus": exit.map_or(Value::Null, |n| json!(n as i64)),
    }))
}

/// `parserProblemFromProbe(raw)`: one `runtime_health.py` JSON line, or none.
pub fn parser_problem_from_probe(raw: Option<&str>) -> Option<Value> {
    let raw = raw?;
    if raw.len() > OUTPUT_LIMIT {
        return None;
    }
    let value: Value = serde_json::from_str(raw.trim()).ok()?;
    let record = value.as_object()?;
    if record.get("ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let reason = record.get("reason")?.as_str()?;
    if ![
        "missing-python-module",
        "parser-mismatch",
        "parser-import-failed",
    ]
    .contains(&reason)
    {
        return None;
    }
    let mut copy = record.clone();
    copy.insert("exitStatus".into(), Value::Null);
    public_problem(&Value::Object(copy))
}

/// `unitProblemFromShow(raw)`: a failed unit, or none.
pub fn unit_problem_from_show(raw: Option<&str>) -> Option<Value> {
    let raw = raw?;
    if raw.len() > OUTPUT_LIMIT {
        return None;
    }
    let mut fields = HashMap::new();
    for line in raw.split('\n') {
        if let Some(index) = line.find('=').filter(|index| *index > 0) {
            fields.insert(
                line[..index].to_owned(),
                line[index + 1..].trim().to_owned(),
            );
        }
    }
    if fields.get("ActiveState").map(String::as_str) != Some("failed") {
        return None;
    }
    let status = fields
        .get("ExecMainStatus")
        .and_then(|text| text.parse::<i64>().ok())
        .filter(|status| (1..=255).contains(status));
    Some(json!({
        "reason": "service-failed",
        "module": Value::Null,
        "python": Value::Null,
        "builtFor": Value::Null,
        "exitStatus": status.map_or(Value::Null, |status| json!(status)),
    }))
}

/// A bounded runner: stdout of a zero exit, otherwise `None`.
pub type Runner<'a> = &'a dyn Fn(&str, &[&str], u64) -> Option<String>;

const UNIT_ARGS: [&str; 4] = [
    "--user",
    "show",
    RUNTIME_UNIT,
    "--property=ActiveState,Result,ExecMainStatus",
];

fn probe_args<'a>(script: &'a str, bundle: &'a str) -> [&'a str; 4] {
    ["-I", "-B", script, bundle]
}

/// `diagnoseRuntimeService`: the parser probe first, the unit only when the parser is fine.
pub fn diagnose(bundle: &Path, python: &str, script: &Path, run: Runner<'_>) -> Option<Value> {
    let bundle = bundle.to_string_lossy();
    let script = script.to_string_lossy();
    let parser =
        parser_problem_from_probe(run(python, &probe_args(&script, &bundle), 15_000).as_deref());
    if parser.is_some() {
        return parser;
    }
    unit_problem_from_show(run("systemctl", &UNIT_ARGS, 5_000).as_deref())
}

/// The real bounded runner: `Command`, scrubbed Python environment, 64 KiB stdout, a deadline.
pub fn bounded_run(file: &str, args: &[&str], timeout_ms: u64) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(file)
        .args(args)
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONSTARTUP")
        .env_remove("PYTHONUSERBASE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut body = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    body.extend_from_slice(&chunk[..n]);
                    if body.len() > OUTPUT_LIMIT {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        body
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let body = reader.join().ok()?;
    if !status?.success() || body.len() > OUTPUT_LIMIT {
        return None;
    }
    String::from_utf8(body).ok()
}

/// `createRuntimeServiceProblemReader`: no probe while the socket exists; one probe per minute
/// while it is missing.
pub struct ProblemReader {
    /// The installed bundle whose pinned parser the probe checks.
    pub bundle_directory: PathBuf,
    /// The runtime's private control socket; while it exists no probe runs.
    pub socket_path: PathBuf,
    cache: Option<(i64, Option<Value>)>,
}

impl ProblemReader {
    /// A reader over one installed runtime.
    pub fn new(bundle_directory: &Path, socket_path: &Path) -> Self {
        Self {
            bundle_directory: bundle_directory.to_path_buf(),
            socket_path: socket_path.to_path_buf(),
            cache: None,
        }
    }

    /// The cached diagnosis: none while the socket exists, else one probe per minute.
    pub fn problem(
        &mut self,
        now: i64,
        diagnose: &mut dyn FnMut(&Path) -> Option<Value>,
    ) -> Option<Value> {
        if socket_present(&self.socket_path) {
            self.cache = None;
            return None;
        }
        if let Some((at, value)) = &self.cache
            && now - at < HEALTH_TTL_MS
        {
            return value.clone();
        }
        let value = diagnose(&self.bundle_directory).and_then(|value| public_problem(&value));
        self.cache = Some((now, value.clone()));
        value
    }
}

#[cfg(unix)]
fn socket_present(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_socket())
}

#[cfg(not(unix))]
fn socket_present(_path: &Path) -> bool {
    false
}

/// `probeRuntimeSocket`: only absence or a refused connect is `stopped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketState {
    /// Nothing can be listening: the socket or its folder is absent, or a leftover file refused.
    Stopped,
    /// A resident runtime accepted the connection.
    Listening,
    /// Unsafe or unclear: never reported as stopped.
    Unknown,
}

#[cfg(unix)]
pub fn socket_state(socket_path: &Path, timeout_ms: u64) -> SocketState {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let uid = unsafe { libc::getuid() };
    let directory = std::fs::symlink_metadata(socket_path.parent().unwrap_or(Path::new(".")));
    match directory {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SocketState::Stopped,
        Ok(meta) if meta.is_dir() && meta.uid() == uid => {}
        _ => return SocketState::Unknown,
    }
    match std::fs::symlink_metadata(socket_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SocketState::Stopped,
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid => {}
        _ => return SocketState::Unknown,
    }
    use std::sync::mpsc;
    let path = socket_path.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::os::unix::net::UnixStream::connect(&path);
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Ok(Ok(stream)) => {
            drop(stream);
            SocketState::Listening
        }
        Ok(Err(error)) => {
            if error.raw_os_error() == Some(libc::ECONNREFUSED) {
                SocketState::Stopped
            } else if error.kind() == std::io::ErrorKind::NotFound {
                // ENOENT is proof only while the file is really gone.
                if std::fs::symlink_metadata(socket_path).is_err() {
                    SocketState::Stopped
                } else {
                    SocketState::Unknown
                }
            } else {
                SocketState::Unknown
            }
        }
        Err(_) => SocketState::Unknown,
    }
}

/// [`socket_state`] on a host without Unix sockets: always `unknown`.
#[cfg(not(unix))]
pub fn socket_state(_socket_path: &Path, _timeout_ms: u64) -> SocketState {
    SocketState::Unknown
}

// ---- native-version.ts --------------------------------------------------------------------------

const MAX_RELEASE_BYTES: u64 = 65_536;
const MAX_REVIEWED: usize = 16;

/// One reviewed native build from `release.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewedNative {
    /// The reviewed CLI version word.
    pub version: String,
    /// The reviewed native binary's sha256.
    pub sha256: String,
}

/// The packaged release's reviewed set.
#[derive(Clone, Debug, Default)]
pub struct NativeRelease {
    /// The release's activation gate (informational; callers gate separately).
    pub gate_open: bool,
    /// The reviewed builds, oldest first; empty when the file is unusable.
    pub reviewed: Vec<ReviewedNative>,
}

/// `/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/`, at most 128 characters.
fn native_version(text: &str) -> bool {
    if text.len() > 128 {
        return false;
    }
    let (core, suffix) = match text.split_once('-') {
        Some((core, suffix)) => (core, Some(suffix)),
        None => (text, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    suffix.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    })
}

fn reviewed_entry(value: &Value) -> Option<ReviewedNative> {
    let record = value.as_object()?;
    if record.len() != 2
        || !record.contains_key("nativeVersion")
        || !record.contains_key("nativeSha256")
    {
        return None;
    }
    let version = record.get("nativeVersion")?.as_str()?;
    let sha256 = record.get("nativeSha256")?.as_str()?;
    (native_version(version) && hash_word(sha256)).then(|| ReviewedNative {
        version: version.to_owned(),
        sha256: sha256.to_owned(),
    })
}

/// `readNativeRelease`: a closed gate with an empty set on anything malformed.
pub fn read_native_release(path: &Path) -> NativeRelease {
    let closed = NativeRelease::default();
    let policy = aac_files::read::ReadPolicy::capped(MAX_RELEASE_BYTES);
    let bytes = match aac_files::read::read(path, &policy) {
        aac_files::read::Read::Bytes(bytes, _) => bytes,
        _ => return closed,
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return closed;
    };
    let Some(record) = value.as_object() else {
        return closed;
    };
    let gate_open = record
        .get("nativeActivationReleased")
        .and_then(Value::as_bool)
        == Some(true)
        && record
            .get("nativeProofReceiptSha256")
            .and_then(Value::as_str)
            .is_some_and(hash_word);
    let reviewed = match record.get("reviewedNatives") {
        None => reviewed_entry(&json!({
            "nativeVersion": record.get("nativeVersion"),
            "nativeSha256": record.get("nativeSha256"),
        }))
        .into_iter()
        .collect::<Vec<_>>(),
        Some(value) => {
            let Some(entries) = value.as_array() else {
                return NativeRelease {
                    gate_open,
                    reviewed: Vec::new(),
                };
            };
            if entries.len() > MAX_REVIEWED {
                return NativeRelease {
                    gate_open,
                    reviewed: Vec::new(),
                };
            }
            let mut reviewed = Vec::with_capacity(entries.len());
            for entry in entries {
                let Some(parsed) = reviewed_entry(entry) else {
                    return NativeRelease {
                        gate_open,
                        reviewed: Vec::new(),
                    };
                };
                reviewed.push(parsed);
            }
            reviewed
        }
    };
    let versions: std::collections::HashSet<&str> = reviewed
        .iter()
        .map(|entry| entry.version.as_str())
        .collect();
    let hashes: std::collections::HashSet<&str> =
        reviewed.iter().map(|entry| entry.sha256.as_str()).collect();
    if versions.len() != reviewed.len() || hashes.len() != reviewed.len() {
        return NativeRelease {
            gate_open,
            reviewed: Vec::new(),
        };
    }
    NativeRelease {
        gate_open,
        reviewed,
    }
}

/// `nativeBinaryProblem(home, uid)`: null when `~/.local/bin/agy` is a safe owned executable.
fn native_binary_problem(home: &Path, uid: Option<u32>) -> bool {
    let _ = uid;
    let binary = home.join(".local").join("bin").join("agy");
    let Ok(meta) = std::fs::symlink_metadata(&binary) else {
        return true;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if uid.is_some_and(|uid| meta.uid() != uid) {
            return true;
        }
        if meta.mode() & 0o022 != 0 || meta.mode() & 0o100 == 0 {
            return true;
        }
    }
    false
}

fn binary_stat_key(binary: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(binary).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let modified = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
        Some(format!(
            "{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            modified,
            meta.len()
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// A paused managed update: the installed CLI matches no reviewed build.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeUpdatePaused {
    /// The installed CLI's version when it could be read as a reviewed-style word.
    pub installed_version: Option<String>,
}

/// The process's paused verdicts, keyed by binary and the reviewed set (`pausedCache`).
#[derive(Default)]
pub struct PausedCache {
    entries: HashMap<PathBuf, (String, Option<NativeUpdatePaused>)>,
}

/// `readNativeUpdatePaused`: no verdict for a missing/unsafe CLI or an empty release set; a
/// reviewed match unpauses; otherwise the installed version (bounded) marks the pause.
pub fn native_update_paused(
    home: &Path,
    release: &NativeRelease,
    pin_matches: &dyn Fn(&Path, &str) -> bool,
    read_version: &mut dyn FnMut() -> Option<String>,
    cache: &mut PausedCache,
) -> Option<NativeUpdatePaused> {
    if native_binary_problem(home, current_uid()) || release.reviewed.is_empty() {
        return None;
    }
    let binary = home.join(".local").join("bin").join("agy");
    let stat_key = binary_stat_key(&binary);
    let set_key = release
        .reviewed
        .iter()
        .map(|entry| format!("{}:{}", entry.version, entry.sha256))
        .collect::<Vec<_>>()
        .join(",");
    let cache_key = stat_key.map(|stat| format!("{stat}|{set_key}"));
    if let Some((key, value)) = cache.entries.get(&binary)
        && Some(key) == cache_key.as_ref()
    {
        return value.clone();
    }
    let matched = release
        .reviewed
        .iter()
        .any(|entry| pin_matches(&binary, &entry.sha256));
    let value = if matched {
        None
    } else {
        let version = read_version().filter(|version| native_version(version));
        Some(NativeUpdatePaused {
            installed_version: version,
        })
    };
    if cache.entries.len() >= 8
        && let Some(first) = cache.entries.keys().next().cloned()
    {
        cache.entries.remove(&first);
    }
    cache
        .entries
        .insert(binary, (cache_key.unwrap_or_default(), value.clone()));
    value
}

fn current_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    const MISSING_PYTE: &str = r#"{"ok":false,"reason":"missing-python-module","module":"pyte","python":"3.14","builtFor":"3.13"}"#;
    const FAILED_UNIT: &str = "ActiveState=failed\nResult=exit-code\nExecMainStatus=1\n";

    /// The TypeScript health test's own cases, ported.
    #[test]
    fn the_health_probe_reads_a_line_and_a_failed_unit_and_nothing_else() {
        assert_eq!(
            parser_problem_from_probe(Some(MISSING_PYTE)),
            Some(json!({
                "reason": "missing-python-module",
                "module": "pyte",
                "python": "3.14",
                "builtFor": "3.13",
                "exitStatus": null,
            }))
        );
        assert_eq!(
            parser_problem_from_probe(Some(r#"{"ok":true,"python":"3.14"}"#)),
            None
        );
        assert_eq!(
            parser_problem_from_probe(Some(r#"{"ok":false,"reason":"probe-failed"}"#)),
            None
        );
        assert_eq!(parser_problem_from_probe(Some("not json")), None);
        assert_eq!(parser_problem_from_probe(None), None);
        assert_eq!(
            parser_problem_from_probe(Some(
                r#"{"ok":false,"reason":"parser-mismatch","module":"x y","python":3}"#
            )),
            Some(json!({
                "reason": "parser-mismatch",
                "module": null,
                "python": null,
                "builtFor": null,
                "exitStatus": null,
            }))
        );
        assert_eq!(
            unit_problem_from_show(Some(FAILED_UNIT)),
            Some(json!({
                "reason": "service-failed",
                "module": null,
                "python": null,
                "builtFor": null,
                "exitStatus": 1,
            }))
        );
        assert_eq!(
            unit_problem_from_show(Some(
                "ActiveState=inactive\nResult=success\nExecMainStatus=0\n"
            )),
            None
        );
        assert_eq!(
            unit_problem_from_show(Some("ActiveState=failed\nExecMainStatus=nope\n")),
            Some(json!({
                "reason": "service-failed",
                "module": null,
                "python": null,
                "builtFor": null,
                "exitStatus": null,
            }))
        );
        assert_eq!(unit_problem_from_show(None), None);
    }

    /// The packaged probe first, with the fixed argv; the unit only when the parser is fine.
    #[test]
    fn the_diagnosis_asks_the_service_interpreter_first_and_the_unit_second() {
        let bundle = Path::new("/fixture/bundle");
        let script = Path::new("/fixture/runtime_health.py");
        let seen = std::cell::RefCell::new(Vec::<String>::new());
        let run = |file: &str, args: &[&str], _timeout: u64| {
            seen.borrow_mut().push(format!("{file} {}", args.join(" ")));
            if file == "systemctl" {
                Some(FAILED_UNIT.to_owned())
            } else {
                Some(MISSING_PYTE.to_owned())
            }
        };
        let problem = diagnose(bundle, SERVICE_PYTHON, script, &run).unwrap();
        assert_eq!(problem["reason"], json!("missing-python-module"));
        assert_eq!(
            seen.borrow().as_slice(),
            ["/usr/bin/python3 -I -B /fixture/runtime_health.py /fixture/bundle"]
        );
        seen.borrow_mut().clear();
        let run = |file: &str, _args: &[&str], _timeout: u64| {
            seen.borrow_mut().push(file.to_owned());
            if file == "systemctl" {
                Some(FAILED_UNIT.to_owned())
            } else {
                Some(r#"{"ok":true}"#.to_owned())
            }
        };
        assert_eq!(
            diagnose(bundle, SERVICE_PYTHON, script, &run).unwrap()["reason"],
            json!("service-failed")
        );
        assert_eq!(seen.borrow().as_slice(), ["/usr/bin/python3", "systemctl"]);
        let silent = |_: &str, _: &[&str], _: u64| None;
        assert_eq!(diagnose(bundle, SERVICE_PYTHON, script, &silent), None);
    }

    #[test]
    fn a_reader_probes_once_a_minute_only_while_the_socket_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let mut reader = ProblemReader::new(temp.path(), &socket);
        let calls = std::cell::Cell::new(0);
        let mut diagnose = |_: &Path| {
            calls.set(calls.get() + 1);
            parser_problem_from_probe(Some(MISSING_PYTE))
        };
        assert!(reader.problem(1_000, &mut diagnose).is_some());
        assert_eq!(calls.get(), 1);
        assert!(reader.problem(2_000, &mut diagnose).is_some());
        assert_eq!(calls.get(), 1, "the verdict is reused inside the minute");
        assert!(
            reader
                .problem(1_000 + HEALTH_TTL_MS, &mut diagnose)
                .is_some()
        );
        assert_eq!(calls.get(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_present_socket_silences_the_reader() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("control.sock");
        let _server = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut reader = ProblemReader::new(temp.path(), &socket);
        let calls = std::cell::Cell::new(0);
        let mut diagnose = |_: &Path| {
            calls.set(calls.get() + 1);
            parser_problem_from_probe(Some(MISSING_PYTE))
        };
        assert!(reader.problem(1_000, &mut diagnose).is_none());
        assert_eq!(calls.get(), 0);
        assert_eq!(socket_state(&socket, 500), SocketState::Listening);
        std::fs::remove_file(&socket).unwrap();
        assert_eq!(socket_state(&socket, 500), SocketState::Stopped);
    }

    #[test]
    fn the_release_stays_closed_on_anything_malformed() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("release.json");
        std::fs::write(
            &file,
            json!({
                "nativeActivationReleased": true,
                "nativeProofReceiptSha256": "a".repeat(64),
                "reviewedNatives": [
                    {"nativeVersion": "1.6.0", "nativeSha256": "b".repeat(64)},
                    {"nativeVersion": "1.7.0", "nativeSha256": "c".repeat(64)}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let release = read_native_release(&file);
        assert!(release.gate_open);
        assert_eq!(release.reviewed.len(), 2);
        // Duplicate versions or hashes close the set.
        std::fs::write(
            &file,
            json!({
                "reviewedNatives": [
                    {"nativeVersion": "1.6.0", "nativeSha256": "b".repeat(64)},
                    {"nativeVersion": "1.6.0", "nativeSha256": "c".repeat(64)}
                ]
            })
            .to_string(),
        )
        .unwrap();
        assert!(read_native_release(&file).reviewed.is_empty());
        std::fs::write(&file, b"not json").unwrap();
        assert!(read_native_release(&file).reviewed.is_empty());
    }

    #[test]
    fn a_reviewed_binary_unpauses_and_an_unreviewed_one_names_its_version() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let binary = home.join(".local").join("bin").join("agy");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"fixture native").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let release = NativeRelease {
            gate_open: true,
            reviewed: vec![ReviewedNative {
                version: "1.6.0".into(),
                sha256: "b".repeat(64),
            }],
        };
        let mut cache = PausedCache::default();
        let matched = |_: &Path, _: &str| true;
        let mut version = || Some("1.5.0".to_owned());
        assert_eq!(
            native_update_paused(home, &release, &matched, &mut version, &mut cache),
            None
        );
        let unmatched = |_: &Path, _: &str| false;
        let mut version = || Some("1.5.0".to_owned());
        assert_eq!(
            native_update_paused(home, &release, &unmatched, &mut version, &mut cache)
                .unwrap()
                .installed_version
                .as_deref(),
            Some("1.5.0")
        );
        // An empty reviewed set is not a pause; neither is a missing CLI.
        assert_eq!(
            native_update_paused(
                home,
                &NativeRelease::default(),
                &unmatched,
                &mut version,
                &mut cache
            ),
            None
        );
        assert_eq!(
            native_update_paused(
                &home.join("missing"),
                &release,
                &unmatched,
                &mut version,
                &mut cache
            ),
            None
        );
    }
}
