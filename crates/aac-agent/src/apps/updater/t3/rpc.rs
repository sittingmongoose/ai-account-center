//! Local-only T3 install client and desktop self-updater (Python `app_update_t3_rpc.py`).
//!
//! Wire contract: T3 `/ws?orchestrationProtocol=2`, Effect 4 JSON RPC.
//! No credential or server diagnostic is emitted.
#![allow(unsafe_code)]

use super::{WINDOWS_NAME, exact_version, runtime_install, truthy};
use crate::apps::updater::{
    CommandSpec, Context, FailureCause, Platform, UpdateFailure, processes, shared,
};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::Digest as _;
#[cfg(unix)]
use std::sync::atomic::AtomicU32;
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    hash::{BuildHasher as _, Hasher as _},
    io::{BufReader, Read as _, Write as _},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

/// `aac-agent --t3-issuer ...`: the gated launcher that starts T3's CLI to issue a session.
pub(in crate::apps::updater) const ISSUER_FLAG: &str = "--t3-issuer";

/// Python `SESSION_TTL`: a session outlives a hard termination by no more than this.
pub(super) const SESSION_TTL: Duration = Duration::from_secs(900);
const REVOKE_TIMEOUT: Duration = Duration::from_secs(1);
const FINAL_REVOKE_TIMEOUT: Duration = Duration::from_secs(5);
const REPLY_LIMIT: usize = 65_536;
const WS_LIMIT: u64 = 4 * 1024 * 1024;
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const DESCRIPTOR_PATH: &str = "/.well-known/t3/environment";
const DESCRIPTOR_LIMIT: usize = 256 * 1024;
const MAINTAIN_SCOPE: &str = "environment:maintain";
const DEFAULT_SCOPES: [&str; 2] = ["providers:manage", "orchestration:read"];
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A loopback origin: `127.0.0.1`, `localhost` or `::1`, with its explicit port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::apps::updater) struct Origin {
    /// Loopback host string without IPv6 brackets (`127.0.0.1`, `localhost`, or `::1`).
    pub host: String,
    /// Explicit TCP port.
    pub port: u16,
}

impl Origin {
    fn host_header(&self) -> String {
        if self.host == "::1" {
            format!("[::1]:{}", self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn connect_addrs(&self) -> Vec<SocketAddr> {
        match self.host.as_str() {
            "127.0.0.1" => vec![SocketAddr::from((Ipv4Addr::LOCALHOST, self.port))],
            "::1" => vec![SocketAddr::from((Ipv6Addr::LOCALHOST, self.port))],
            _ => (self.host.as_str(), self.port)
                .to_socket_addrs()
                .map(|addrs| addrs.collect())
                .unwrap_or_else(|_| {
                    vec![
                        SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
                        SocketAddr::from((Ipv6Addr::LOCALHOST, self.port)),
                    ]
                }),
        }
    }
}

/// Python `discover`'s answer: the official CLI that talks to one local server, the
/// `--base-dir` it binds to, and the loopback origin that server published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::apps::updater) struct Endpoint {
    /// The T3 CLI: the standalone runtime, the desktop's shim, or the desktop's bundled server.
    pub cli: Vec<OsString>,
    /// The `~/.t3` base directory passed to `--base-dir`.
    pub base: PathBuf,
    /// The local server's published loopback origin.
    pub origin: Origin,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// FIPS 180-4 SHA-1 for RFC 6455 `Sec-WebSocket-Accept` verification (no extra crate).
pub(super) fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity((data.len() + 9).next_multiple_of(64));
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0_u32; 80];
        for (index, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[index] = u32::from_be_bytes(*word);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (index, &word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0_u8; 20];
    for (index, word) in h.iter().enumerate() {
        out[index * 4..(index + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Cryptographic random bytes from the OS (Unix `/dev/urandom`, fallback / Windows via
/// stdlib's OS-seeded `RandomState` mixed through SHA-256).
pub(super) fn random_bytes(count: usize) -> Vec<u8> {
    #[cfg(unix)]
    {
        let mut bytes = vec![0_u8; count];
        if let Ok(mut file) = std::fs::File::open("/dev/urandom")
            && file.read_exact(&mut bytes).is_ok()
        {
            return bytes;
        }
    }
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let mut hasher = sha2::Sha256::new();
        for _ in 0..4 {
            let state = std::collections::hash_map::RandomState::new();
            let mut h = state.build_hasher();
            h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
            hasher.update(h.finish().to_le_bytes());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        hasher.update(now.to_le_bytes());
        hasher.update(std::process::id().to_le_bytes());
        let block = hasher.finalize();
        let take = (count - out.len()).min(block.len());
        out.extend_from_slice(&block[..take]);
    }
    out
}

/// `count` random bytes formatted as lowercase hex (Python `uuid4().hex`).
pub(super) fn random_hex(count: usize) -> String {
    random_bytes(count)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `str(value)` for classification-only RPC error detail (never written to rows).
fn python_text(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Python `remaining(deadline, maximum)`: positive time left, capped at `maximum`.
pub(super) fn remaining(
    deadline: Instant,
    maximum: Option<Duration>,
) -> Result<Duration, UpdateFailure> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(UpdateFailure::new("t3_install_timeout"));
    }
    Ok(match maximum {
        Some(cap) => left.min(cap),
        None => left,
    })
}

/// Python `ttl_argument(deadline)`: whole seconds up to `SESSION_TTL` (`<N>s`), `< 1s` times out.
pub(super) fn ttl_argument(deadline: Instant) -> Result<String, UpdateFailure> {
    let seconds = remaining(deadline, Some(SESSION_TTL))?.as_secs();
    if seconds < 1 {
        return Err(UpdateFailure::new("t3_install_timeout"));
    }
    Ok(format!("{seconds}s"))
}

// ---------------------------------------------------------------------------
// Local server discovery & readiness (Python `discover`, `wait_ready`, `t3_installed`).
// ---------------------------------------------------------------------------

/// The loopback origin a URL string names, when it has an explicit port and loopback host.
pub(super) fn origin_of(raw: &str) -> Option<Origin> {
    let url = url::Url::parse(raw).ok()?;
    let host = match url.host()? {
        url::Host::Domain("localhost") => "localhost",
        url::Host::Ipv4(addr) if addr == Ipv4Addr::LOCALHOST => "127.0.0.1",
        url::Host::Ipv6(addr) if addr == Ipv6Addr::LOCALHOST => "::1",
        _ => return None,
    };
    let port = url.port().or_else(|| {
        let after_scheme = raw.split_once("://")?.1;
        let authority = after_scheme.split(['/', '?', '#']).next()?;
        let host_port = authority.rsplit('@').next()?;
        let port_str = if let Some(rest) = host_port.strip_prefix('[') {
            rest.split_once("]:")?.1
        } else {
            host_port.split_once(':')?.1
        };
        port_str.parse::<u16>().ok()
    })?;
    if port == 0 {
        return None;
    }
    Some(Origin {
        host: host.to_owned(),
        port,
    })
}

/// Python `server_state`: T3's published endpoint record, or `None`. Read only; never guessed.
pub(super) fn server_state(home: &Path) -> Option<Value> {
    let path = home.join(".t3/userdata/server-runtime.json");
    if std::fs::metadata(&path).ok()?.len() > 65_536 {
        return None;
    }
    match crate::apps::read_json(&path, 65_536)? {
        value @ Value::Object(_) => Some(value),
        _ => None,
    }
}

/// Python `t3_installed`: whether the desktop app, its CLI shim, or a standalone runtime exists.
pub(super) fn t3_installed(cx: &Context) -> bool {
    let home = &cx.host.home;
    let candidates: Vec<PathBuf> = match cx.platform {
        Platform::Windows => vec![cx.host.local.join("Programs/t3code").join(WINDOWS_NAME)],
        Platform::Mac => vec![
            home.join(".t3/bin/t3"),
            home.join(".local/bin/t3"),
            cx.host.system(processes::T3_MAC_BUNDLE),
        ],
        Platform::Ubuntu => vec![home.join(".t3/bin/t3"), home.join(".local/bin/t3")],
    };
    let versions = home.join(".t3/runtime/versions");
    if versions.is_dir()
        && std::fs::read_dir(&versions).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|child| child.path().join("t3").is_file())
        })
    {
        return true;
    }
    candidates.iter().any(|path| path.exists())
}

/// Python `shlex.split` (POSIX mode, comments off): `None` on an unterminated quote.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn shell_split(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut has_token = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                has_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => word.push(inner),
                        None => return None,
                    }
                }
            }
            '"' => {
                has_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(inner @ ('"' | '\\' | '$' | '`')) => word.push(inner),
                            Some('\n') => {}
                            Some(inner) => {
                                word.push('\\');
                                word.push(inner);
                            }
                            None => return None,
                        },
                        Some(inner) => word.push(inner),
                        None => return None,
                    }
                }
            }
            '\\' => {
                has_token = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(inner) => word.push(inner),
                    None => return None,
                }
            }
            ch if ch.is_whitespace() => {
                if has_token {
                    words.push(std::mem::take(&mut word));
                    has_token = false;
                }
            }
            ch => {
                has_token = true;
                word.push(ch);
            }
        }
    }
    if has_token {
        words.push(word);
    }
    Some(words)
}

fn protection_failed() -> UpdateFailure {
    UpdateFailure::new("t3_issuer_protection_failed")
}

/// Python `linux_desktop_cli`: resolve the running AppImage mount without executing its
/// forking shim. Missing or inaccessible mounts fail closed.
pub(super) fn linux_desktop_cli(shim: &Path, proc: &Path) -> Result<Vec<OsString>, UpdateFailure> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let resolve = || -> Option<Vec<OsString>> {
            let text = std::fs::read_to_string(shim).ok()?;
            let lines: Vec<&str> = text.lines().collect();
            if !lines.contains(&"# Written by T3 Code: runs the desktop app's bundled t3 CLI.") {
                return None;
            }
            let mut images = Vec::new();
            for line in &lines {
                if let Some(rest) = line.strip_prefix("app=") {
                    images.push(shell_split(rest)?.into_iter().next()?);
                }
            }
            let [image] = images.as_slice() else {
                return None;
            };
            let image = std::fs::canonicalize(image).ok()?;
            for entry in std::fs::read_dir(proc).ok()?.flatten() {
                if !entry.file_name().to_str().is_some_and(|name| {
                    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
                }) {
                    continue;
                }
                let found = || -> Option<Vec<OsString>> {
                    let raw = std::fs::read(entry.path().join("environ")).ok()?;
                    let variable = |name: &[u8]| -> Option<Vec<u8>> {
                        raw.split(|byte| *byte == 0)
                            .filter_map(|item| item.strip_prefix(name).map(<[u8]>::to_vec))
                            .next_back()
                    };
                    let appimage = variable(b"APPIMAGE=".as_slice()).unwrap_or_default();
                    let appdir = variable(b"APPDIR=".as_slice())?;
                    if crate::apps::real(Path::new(std::ffi::OsStr::from_bytes(&appimage))) != image
                    {
                        return None;
                    }
                    let executable = std::fs::canonicalize(entry.path().join("exe")).ok()?;
                    let mounted =
                        std::fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(&appdir)))
                            .ok()?;
                    let entry_path = mounted.join("resources/app.asar/apps/server/dist/bin.mjs");
                    if executable.parent() == Some(mounted.as_path())
                        && executable.file_name().is_some_and(|name| name == "t3code")
                        && crate::apps::executable(&executable)
                        && mounted.join("resources/app.asar").is_file()
                    {
                        return Some(vec![
                            executable.into_os_string(),
                            entry_path.into_os_string(),
                        ]);
                    }
                    None
                };
                if let Some(cli) = found() {
                    return Some(cli);
                }
            }
            None
        };
        resolve().ok_or_else(protection_failed)
    }
    #[cfg(not(unix))]
    {
        let _ = (shim, proc);
        Err(protection_failed())
    }
}

fn version_spec(candidate: &[OsString]) -> CommandSpec {
    let mut argv = candidate.to_vec();
    argv.push("--version".into());
    let mut spec = CommandSpec::new(argv)
        .timeout(Duration::from_secs(10))
        .capture()
        .kill_tree();
    spec.env.push(("ELECTRON_RUN_AS_NODE".into(), "1".into()));
    spec
}

async fn discover_endpoint(
    cx: &Context,
    proc: &Path,
    resolve_linux_shim: impl Fn(&Path, &Path) -> Result<Vec<OsString>, UpdateFailure>,
) -> Result<Endpoint, Option<UpdateFailure>> {
    let base = cx.host.home.join(".t3");
    let fields = server_state(&cx.host.home)
        .ok_or(None)?
        .as_object()
        .cloned()
        .ok_or(None)?;
    let origin_raw = fields.get("origin").and_then(Value::as_str).ok_or(None)?;
    let url = url::Url::parse(origin_raw).map_err(|_| None)?;
    let origin = origin_of(origin_raw).ok_or(None)?;
    let version_ok = fields
        .get("version")
        .filter(|v| !v.is_boolean())
        .and_then(Value::as_i64)
        == Some(1);
    let port_ok = fields
        .get("port")
        .filter(|v| !v.is_boolean())
        .and_then(Value::as_u64)
        == Some(u64::from(origin.port));
    let bare = url.username().is_empty()
        && url.password().is_none_or(str::is_empty)
        && url.query().is_none_or(str::is_empty)
        && url.fragment().is_none_or(str::is_empty)
        && matches!(url.path(), "" | "/");
    if url.scheme() != "http" || !bare || !port_ok || !version_ok {
        return Err(None);
    }
    let cli = if cx.platform == Platform::Windows {
        let executable = cx.host.local.join("Programs/t3code").join(WINDOWS_NAME);
        let archive = executable
            .parent()
            .ok_or(None)?
            .join("resources/server.asar");
        if !executable.is_file() || !archive.is_file() {
            return Err(None);
        }
        vec![
            executable.into_os_string(),
            archive.join("apps/server/dist/bin.mjs").into_os_string(),
        ]
    } else {
        let shim = base.join("bin/t3");
        let mut candidates: Vec<Vec<OsString>> = Vec::new();
        if shim.is_file() {
            candidates.push(if cx.platform == Platform::Ubuntu {
                resolve_linux_shim(&shim, proc).map_err(Some)?
            } else {
                vec![shim.into_os_string()]
            });
        }
        if let Some(executable) = runtime_install(&cx.host.home) {
            candidates.push(vec![executable.into_os_string()]);
        }
        if let Some(version) = fields
            .get("serverVersion")
            .filter(|value| truthy(value))
            .and_then(Value::as_str)
        {
            if exact_version(version).is_some() {
                let pinned = base.join("runtime/versions").join(version).join("t3");
                if pinned.is_file() {
                    candidates.push(vec![pinned.into_os_string()]);
                }
            }
            let mut matched = Vec::new();
            for candidate in candidates {
                let answered = cx.run(&version_spec(&candidate)).await.is_ok_and(|output| {
                    output
                        .stdout
                        .split_whitespace()
                        .next_back()
                        .is_some_and(|last| last.strip_prefix('v').unwrap_or(last) == version)
                });
                if answered {
                    matched.push(candidate);
                }
            }
            candidates = matched;
        }
        candidates.into_iter().next().ok_or(None)?
    };
    Ok(Endpoint { cli, base, origin })
}

/// Testable `discover` with explicit `/proc` path and Linux shim resolver.
pub(super) async fn discover_with(
    cx: &Context,
    proc: &Path,
    resolve_linux_shim: impl Fn(&Path, &Path) -> Result<Vec<OsString>, UpdateFailure>,
) -> Result<Endpoint, UpdateFailure> {
    match discover_endpoint(cx, proc, resolve_linux_shim).await {
        Ok(endpoint) => Ok(endpoint),
        Err(Some(failure)) => Err(failure),
        Err(None) if t3_installed(cx) => Err(UpdateFailure::new("t3_unreachable")
            .with_cause(FailureCause::T3NotReady, Some("no_server_endpoint"))),
        Err(None) => Err(UpdateFailure::new("t3_absent")
            .with_cause(FailureCause::T3Absent, Some("no_t3_install"))),
    }
}

/// Python `discover`: the official CLI of the local server's own install. `t3_absent` when
/// nothing of T3 is installed here, `t3_unreachable` when no server endpoint can be used.
pub(in crate::apps::updater) async fn discover(cx: &Context) -> Result<Endpoint, UpdateFailure> {
    discover_with(cx, Path::new("/proc"), linux_desktop_cli).await
}

fn tcp_connect(origin: &Origin, timeout: Duration) -> std::io::Result<TcpStream> {
    let deadline = StdInstant::now() + timeout;
    let mut last_error = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no addresses");
    for addr in origin.connect_addrs() {
        let left = deadline.saturating_duration_since(StdInstant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connect timeout",
            ));
        }
        match TcpStream::connect_timeout(&addr, left) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

async fn ready_attempt(
    state: Option<Value>,
    expected: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    let state = state.ok_or("no_server_endpoint")?;
    let origin = state
        .get("origin")
        .and_then(Value::as_str)
        .and_then(origin_of)
        .ok_or("no_server_endpoint")?;
    let running = state.get("serverVersion");
    if expected.is_some_and(|want| running.and_then(Value::as_str) != Some(want)) {
        return Err(if running.is_some_and(truthy) {
            "previous_server"
        } else {
            "no_server_version"
        });
    }
    let connected = tokio::task::spawn_blocking(move || tcp_connect(&origin, timeout)).await;
    match connected {
        Ok(Ok(_)) => Ok(()),
        _ => Err("connection_refused"),
    }
}

/// Testable `wait_ready` returning attempt count, with injectable sleeper.
pub(super) async fn wait_ready_attempts<S, SF>(
    cx: &Context,
    expected: Option<&str>,
    seconds: Duration,
    mut sleep: S,
) -> Result<u32, UpdateFailure>
where
    S: FnMut(Duration) -> SF,
    SF: std::future::Future<Output = ()>,
{
    let end = cx.deadline.min(Instant::now() + seconds);
    let mut backoff = Duration::from_secs(1);
    let mut attempts = 0_u32;
    loop {
        attempts += 1;
        let state = server_state(&cx.host.home);
        if state.is_none() && attempts == 1 && !t3_installed(cx) {
            return Err(UpdateFailure::new("t3_absent")
                .with_cause(FailureCause::T3Absent, Some("no_t3_install")));
        }
        let timeout = end
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(3))
            .max(Duration::from_millis(200));
        let reason = match ready_attempt(state, expected, timeout).await {
            Ok(()) => return Ok(attempts),
            Err(failed) => failed,
        };
        let wait = backoff.min(end.saturating_duration_since(Instant::now()));
        if wait.is_zero() {
            return Err(UpdateFailure::new("t3_not_ready")
                .with_cause(FailureCause::T3NotReady, Some(reason)));
        }
        sleep(wait).await;
        backoff = (backoff * 2).min(Duration::from_secs(8));
    }
}

/// Python `wait_ready`: wait, bounded, until the local T3 server answers on its published
/// endpoint. Only safe, repeatable reads happen here (the endpoint record and a TCP connect);
/// no session is issued until this returns.
pub(in crate::apps::updater) async fn wait_ready(
    cx: &Context,
    expected: Option<&str>,
    seconds: Duration,
) -> Result<(), UpdateFailure> {
    wait_ready_attempts(cx, expected, seconds, tokio::time::sleep)
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// Temporary sessions, gated issuer & signal cleanup (Python `Issuer`, `auth_command`,
// `revoke_session`, `retire_session`, `terminate`, `session_signals`, `bearer`).
// ---------------------------------------------------------------------------

/// Python `_known_sessions`: every session this process may hold, so `terminate` can revoke
/// them. Memory only.
static KNOWN: Mutex<Vec<Known>> = Mutex::new(Vec::new());

#[derive(Clone)]
struct Known {
    id: String,
    cli: Vec<OsString>,
    base: PathBuf,
}

/// Python `_issuers`: the launchers of issues that have not answered yet.
static ISSUERS: Mutex<Vec<Arc<Issuer>>> = Mutex::new(Vec::new());

/// Python `_terminating`: once terminating, no new issuer is released.
static TERMINATING: AtomicBool = AtomicBool::new(false);

fn known_register(id: &str, cli: &[OsString], base: &Path) {
    let mut known = lock(&KNOWN);
    known.retain(|entry| entry.id != id);
    known.push(Known {
        id: id.to_owned(),
        cli: cli.to_vec(),
        base: base.to_path_buf(),
    });
}

fn known_forget(id: &str) {
    lock(&KNOWN).retain(|entry| entry.id != id);
}

#[cfg(windows)]
mod win_job {
    use super::lock;
    use std::{
        ffi::c_void,
        os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
        sync::Mutex,
    };
    use windows_sys::Win32::Foundation::HANDLE;

    #[repr(C)]
    struct BasicLimit {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(security: *const c_void, name: *const u16) -> HANDLE;
        fn SetInformationJobObject(
            job: HANDLE,
            class: i32,
            info: *const c_void,
            length: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: HANDLE, process: HANDLE) -> i32;
    }

    pub(super) struct Job(Mutex<Option<OwnedHandle>>);

    impl Job {
        pub(super) fn create() -> Option<Self> {
            // SAFETY: null pointers create an anonymous job object; result is checked for null.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            // SAFETY: handle was created by CreateJobObjectW and is owned exclusively here.
            let job = unsafe { OwnedHandle::from_raw_handle(handle) };
            // SAFETY: all-zero ExtendedLimit is a valid Win32 JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
            let mut info: ExtendedLimit = unsafe { std::mem::zeroed() };
            info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: class 9 matches ExtendedLimit and length matches its byte size.
            let ok = unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    std::ptr::from_ref(&info).cast(),
                    std::mem::size_of::<ExtendedLimit>() as u32,
                )
            };
            (ok != 0).then(|| Self(Mutex::new(Some(job))))
        }

        pub(super) fn assign(&self, process: RawHandle) -> bool {
            let held = lock(&self.0);
            let Some(job) = held.as_ref() else {
                return false;
            };
            // SAFETY: both handles are open kernel handles for a job and a process.
            !process.is_null()
                && unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } != 0
        }

        pub(super) fn close(&self) {
            let _ = lock(&self.0).take();
        }
    }
}

/// A live issue's launcher (Python `Issuer`): what `terminate` stops before it has answered.
pub(super) struct Issuer {
    /// Held around `try_wait` reaping and `killpg` so a reaped PID is never signalled.
    reap_lock: Mutex<()>,
    stopped: AtomicBool,
    reaped: AtomicBool,
    #[cfg(unix)]
    pid: AtomicU32,
    #[cfg(windows)]
    job: win_job::Job,
}

impl Issuer {
    pub(super) fn create() -> Result<Self, UpdateFailure> {
        #[cfg(unix)]
        {
            Ok(Self {
                reap_lock: Mutex::new(()),
                stopped: AtomicBool::new(false),
                reaped: AtomicBool::new(false),
                pid: AtomicU32::new(0),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                reap_lock: Mutex::new(()),
                stopped: AtomicBool::new(false),
                reaped: AtomicBool::new(false),
                job: win_job::Job::create().ok_or_else(protection_failed)?,
            })
        }
    }

    fn running(&self, pid: u32) {
        #[cfg(unix)]
        self.pid.store(pid, Ordering::SeqCst);
        #[cfg(windows)]
        let _ = pid;
    }

    /// Python `Issuer.stop`: idempotent, never blocks on a worker holding `reap_lock`,
    /// never signals a reaped PID.
    pub(super) fn stop(&self) {
        #[cfg(windows)]
        {
            if self.stopped.swap(true, Ordering::SeqCst) {
                return;
            }
            self.job.close();
        }
        #[cfg(unix)]
        {
            if self.stopped.load(Ordering::SeqCst) {
                return;
            }
            let Ok(_guard) = self.reap_lock.try_lock() else {
                return;
            };
            if self.stopped.swap(true, Ordering::SeqCst) {
                return;
            }
            if !self.reaped.load(Ordering::SeqCst) {
                let raw = self.pid.load(Ordering::SeqCst);
                if let Ok(pid) = i32::try_from(raw)
                    && pid > 0
                {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
            }
        }
    }

    fn wait_reap_locked(
        &self,
        child: &mut Child,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let _guard = lock(&self.reap_lock);
        let status = child.try_wait()?;
        if status.is_some() {
            self.reaped.store(true, Ordering::SeqCst);
        }
        Ok(status)
    }
}

fn auth_environment() -> Vec<(String, String)> {
    shared::environment(&[("ELECTRON_RUN_AS_NODE".to_owned(), "1".to_owned())])
}

/// `t3 auth session <args> --base-dir <base>`.
fn auth_command_line(cli: &[OsString], base: &Path, args: &[&str]) -> Vec<OsString> {
    let mut argv = cli.to_vec();
    argv.extend(["auth", "session"].map(OsString::from));
    argv.extend(args.iter().map(OsString::from));
    argv.extend(["--base-dir".into(), base.as_os_str().to_owned()]);
    argv
}

fn spawn_auth_command(
    argv: &[OsString],
    gate: bool,
    capture_stdout: bool,
) -> std::io::Result<Child> {
    let Some(program) = argv.first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "empty argv",
        ));
    };
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .env_clear()
        .envs(auth_environment())
        .stdin(if gate { Stdio::piped() } else { Stdio::null() })
        .stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: `setsid` is async-signal-safe and places the child in its own session/group.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn()
}

fn stop_child_group(child: &mut Child, tree: bool) {
    let pid = child.id();
    #[cfg(unix)]
    {
        let _ = tree;
        if let Ok(raw) = i32::try_from(pid)
            && raw > 0
        {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(raw),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
    #[cfg(windows)]
    {
        if tree {
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = child.kill();
    }
    let end = StdInstant::now() + Duration::from_secs(5);
    while StdInstant::now() < end {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `aac-agent --t3-issuer <parent pid> <t3 CLI argv...>`: Python's `_ISSUER_BOOTSTRAP`.
/// Dies with the parent on Linux (`PR_SET_PDEATHSIG`), verifies `getppid() == parent`,
/// waits for one `b'1'` gate byte on stdin (EOF fails closed with exit 125), then execs T3.
pub(super) fn issuer_bootstrap(args: &[OsString]) -> i32 {
    let run = || -> Option<i32> {
        let [parent, cli @ ..] = args else {
            return None;
        };
        if cli.is_empty() {
            return Some(125);
        }
        let parent: i64 = parent.to_str()?.trim().parse().ok()?;
        #[cfg(target_os = "linux")]
        // SAFETY: prctl(PR_SET_PDEATHSIG, SIGKILL) and getppid touch no process memory.
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Some(125);
            }
            if i64::from(libc::getppid()) != parent {
                return Some(125);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = parent;
        let mut gate = [0_u8; 1];
        if std::io::stdin().read_exact(&mut gate).is_err() || gate != *b"1" {
            return Some(125);
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd as _;
            use std::os::unix::process::CommandExt as _;
            let null = std::fs::File::open("/dev/null").ok()?;
            // SAFETY: both descriptors belong to this process.
            if unsafe { libc::dup2(null.as_raw_fd(), 0) } == -1 {
                return Some(125);
            }
            drop(null);
            let mut command = Command::new(&cli[0]);
            command.args(&cli[1..]);
            let _ = command.exec();
            Some(125)
        }
        #[cfg(windows)]
        {
            let mut command = Command::new(&cli[0]);
            command
                .args(&cli[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::null());
            command.status().ok()?.code().or(Some(125))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Some(125)
        }
    };
    run().unwrap_or(125)
}

fn communicate_bounded(
    child: &mut Child,
    timeout: Duration,
    issuer: Option<&Issuer>,
) -> Result<String, UpdateFailure> {
    let deadline = StdInstant::now() + timeout;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut kept = Vec::new();
        let Some(mut pipe) = stdout else {
            return Ok(kept);
        };
        let mut buf = [0_u8; 8192];
        loop {
            let read = pipe.read(&mut buf)?;
            if read == 0 {
                break;
            }
            if kept.len() <= REPLY_LIMIT {
                let take = (REPLY_LIMIT + 1 - kept.len()).min(read);
                kept.extend_from_slice(&buf[..take]);
            }
        }
        Ok(kept)
    });
    let status = loop {
        let polled = match issuer {
            Some(issuer) => issuer.wait_reap_locked(child),
            None => child.try_wait(),
        };
        match polled {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                if let Some(issuer) = issuer {
                    issuer.stop();
                } else {
                    stop_child_group(child, true);
                }
                return Err(UpdateFailure {
                    reason: Some("issuer_timeout"),
                    ..UpdateFailure::new("t3_auth_failed")
                });
            }
        }
        if StdInstant::now() >= deadline {
            if let Some(issuer) = issuer {
                issuer.stop();
            } else {
                stop_child_group(child, true);
            }
            return Err(UpdateFailure {
                reason: Some("issuer_timeout"),
                ..UpdateFailure::new("t3_auth_failed")
            });
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let kept = reader.join().ok().and_then(Result::ok).unwrap_or_default();
    if !status.success() {
        return Err(UpdateFailure {
            reason: Some("issuer_exit_nonzero"),
            ..UpdateFailure::new("t3_auth_failed")
        });
    }
    if kept.len() > REPLY_LIMIT {
        return Err(UpdateFailure {
            reason: Some("reply_too_large"),
            ..UpdateFailure::new("t3_auth_failed")
        });
    }
    Ok(String::from_utf8_lossy(&kept).into_owned())
}

fn auth_command_blocking(
    cli: &[OsString],
    base: &Path,
    args: &[String],
    timeout: Duration,
    helper: OsString,
) -> Result<String, UpdateFailure> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if refs.first() != Some(&"issue") {
        let argv = auth_command_line(cli, base, &refs);
        let mut child = spawn_auth_command(&argv, false, true)
            .map_err(|_| UpdateFailure::new("t3_cli_not_started"))?;
        return communicate_bounded(&mut child, timeout, None);
    }
    let issuer = Arc::new(Issuer::create()?);
    lock(&ISSUERS).push(Arc::clone(&issuer));
    let outcome = (|| -> Result<String, UpdateFailure> {
        let mut argv = vec![
            helper,
            ISSUER_FLAG.into(),
            std::process::id().to_string().into(),
        ];
        argv.extend(auth_command_line(cli, base, &refs));
        let mut child = spawn_auth_command(&argv, true, true)
            .map_err(|_| UpdateFailure::new("t3_cli_not_started"))?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            if !issuer.job.assign(child.as_raw_handle()) {
                drop(child.stdin.take());
                let _ = child.kill();
                let _ = child.wait();
                return Err(protection_failed());
            }
        }
        issuer.running(child.id());
        if TERMINATING.load(Ordering::SeqCst) {
            issuer.stop();
            return Err(protection_failed());
        }
        if let Some(mut gate) = child.stdin.take()
            && gate.write_all(b"1").is_err()
        {
            issuer.stop();
            let _ = child.wait();
            return Err(UpdateFailure::new("t3_auth_failed"));
        }
        communicate_bounded(&mut child, timeout, Some(&issuer))
    })();
    issuer.stop();
    lock(&ISSUERS).retain(|other| !Arc::ptr_eq(other, &issuer));
    outcome
}

/// Python `auth_command`: one `t3 auth session` run.
pub(super) async fn auth_command(
    cli: &[OsString],
    base: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, UpdateFailure> {
    let cli = cli.to_vec();
    let base = base.to_path_buf();
    let args: Vec<String> = args.iter().map(|word| (*word).to_owned()).collect();
    let helper = super::helper_binary();
    tokio::task::spawn_blocking(move || auth_command_blocking(&cli, &base, &args, timeout, helper))
        .await
        .map_err(|_| UpdateFailure::new("t3_auth_failed"))?
}

/// Python `revoke_session`: best effort, no diagnostics, no 5s cleanup wait.
pub(super) fn revoke_session_blocking(
    cli: &[OsString],
    base: &Path,
    session_id: &str,
    timeout: Duration,
) -> bool {
    let argv = auth_command_line(cli, base, &["revoke", session_id]);
    let Ok(mut child) = spawn_auth_command(&argv, false, false) else {
        return false;
    };
    let deadline = StdInstant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
        if StdInstant::now() >= deadline {
            let _ = child.kill();
            // Never wait here: the signal handler must exit within the grace.
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Python `find_issued_session`: best-effort lookup after a lost reply.
async fn find_issued_session_with<F, Fut>(
    cli: &[OsString],
    base: &Path,
    label: &str,
    run_auth: &F,
) -> Option<String>
where
    F: Fn(&[OsString], &Path, &[&str], Duration) -> Fut,
    Fut: std::future::Future<Output = Result<String, UpdateFailure>>,
{
    let reply = run_auth(cli, base, &["list", "--json"], FINAL_REVOKE_TIMEOUT)
        .await
        .ok()?;
    let Value::Array(rows) = serde_json::from_str::<Value>(&reply).ok()? else {
        return None;
    };
    let mut matches: Vec<String> = Vec::new();
    for row in rows {
        let Some(object) = row.as_object() else {
            continue;
        };
        let row_label = object
            .get("client")
            .and_then(Value::as_object)
            .and_then(|client| client.get("label"))
            .and_then(Value::as_str);
        if row_label == Some(label)
            && let Some(id) = object.get("sessionId").and_then(Value::as_str)
            && !id.is_empty()
        {
            matches.push(id.to_owned());
        }
    }
    (matches.len() == 1).then(|| matches.swap_remove(0))
}

/// Python `retire_session`: confirm revocation within one shared 5s budget.
async fn retire_session_with<R, RFut, S, SFut>(
    cli: &[OsString],
    base: &Path,
    session_id: &str,
    revoke: &R,
    sleep: &mut S,
) -> Result<(), UpdateFailure>
where
    R: Fn(&[OsString], &Path, &str, Duration) -> RFut,
    RFut: std::future::Future<Output = bool>,
    S: FnMut(Duration) -> SFut,
    SFut: std::future::Future<Output = ()>,
{
    let end = Instant::now() + FINAL_REVOKE_TIMEOUT;
    loop {
        let budget = end.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return Err(UpdateFailure::new("t3_session_revoke_failed"));
        }
        if revoke(cli, base, session_id, budget).await {
            known_forget(session_id);
            return Ok(());
        }
        let pause = end
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));
        if pause.is_zero() {
            return Err(UpdateFailure::new("t3_session_revoke_failed"));
        }
        sleep(pause).await;
    }
}

fn hard_exit(code: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: `_exit` terminates the process immediately without unwinding.
    unsafe {
        libc::_exit(code);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: terminates this process immediately without unwinding.
        unsafe {
            TerminateProcess(GetCurrentProcess(), code as u32);
        }
        std::process::exit(code);
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::process::exit(code);
    }
}

/// Stop every active issuer and revoke every known session within `REVOKE_TIMEOUT`.
pub(super) fn terminate_cleanup(revoke: impl Fn(&[OsString], &Path, &str, Duration) -> bool) {
    let end = StdInstant::now() + REVOKE_TIMEOUT;
    let issuers: Vec<Arc<Issuer>> = lock(&ISSUERS).clone();
    for issuer in issuers {
        issuer.stop();
    }
    let known: Vec<Known> = lock(&KNOWN).clone();
    for entry in known {
        let budget = end.saturating_duration_since(StdInstant::now());
        if budget.is_zero() {
            break;
        }
        let _ = revoke(&entry.cli, &entry.base, &entry.id, budget);
    }
}

/// Python `terminate(signum)`: stop issuers, revoke known sessions within 1s, hard-exit
/// `128 + signum` under a 2s watchdog without unwinding the runtime.
fn terminate(signum: i32) -> ! {
    let code = 128 + signum;
    if TERMINATING.swap(true, Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(3));
        hard_exit(code);
    }
    let _ = std::thread::Builder::new()
        .name("t3-watchdog".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            hard_exit(code);
        });
    terminate_cleanup(revoke_session_blocking);
    hard_exit(code);
}

#[cfg(unix)]
mod unix_signals {
    use super::terminate;
    use std::sync::{
        OnceLock,
        atomic::{AtomicI32, Ordering},
    };

    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    static INSTALLED: OnceLock<()> = OnceLock::new();

    extern "C" fn signal_trampoline(signum: libc::c_int) {
        let fd = WRITE_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            let byte = signum as u8;
            // SAFETY: `write` on a non-blocking pipe fd is async-signal-safe.
            let _ = unsafe { libc::write(fd, std::ptr::from_ref(&byte).cast(), 1) };
        }
    }

    pub(super) fn install() {
        INSTALLED.get_or_init(|| {
            let mut fds = [-1_i32; 2];
            // SAFETY: valid 2-int array for POSIX `pipe`.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return;
            }
            for fd in fds {
                // SAFETY: valid fds returned by `pipe`.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                    }
                }
            }
            // Non-blocking write end so the signal handler can never stall.
            // SAFETY: valid write fd returned by `pipe`.
            unsafe {
                let flags = libc::fcntl(fds[1], libc::F_GETFL);
                if flags >= 0 {
                    libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            WRITE_FD.store(fds[1], Ordering::Release);
            let read_fd = fds[0];
            let _ = std::thread::Builder::new()
                .name("t3-signals".into())
                .spawn(move || {
                    let mut byte = 0_u8;
                    loop {
                        // SAFETY: blocking 1-byte read on our own pipe read end.
                        let read =
                            unsafe { libc::read(read_fd, std::ptr::from_mut(&mut byte).cast(), 1) };
                        if read == 1 {
                            terminate(i32::from(byte));
                        }
                        if read == 0 {
                            break;
                        }
                    }
                });
            use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
            let action = SigAction::new(
                SigHandler::Handler(signal_trampoline),
                SaFlags::SA_RESTART,
                SigSet::empty(),
            );
            for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
                // SAFETY: `signal_trampoline` only calls async-signal-safe `libc::write`.
                let _ = unsafe { sigaction(sig, &action) };
            }
        });
    }
}

#[cfg(windows)]
mod windows_signals {
    use super::terminate;
    use std::sync::OnceLock;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };

    static INSTALLED: OnceLock<()> = OnceLock::new();

    unsafe extern "system" fn console_handler(ctrl_type: u32) -> i32 {
        let signum = match ctrl_type {
            CTRL_C_EVENT => 2,
            CTRL_BREAK_EVENT => 21,
            _ => 15,
        };
        terminate(signum);
    }

    pub(super) fn install() {
        INSTALLED.get_or_init(|| {
            // SAFETY: registers our static console control handler once.
            unsafe {
                SetConsoleCtrlHandler(Some(console_handler), 1);
            }
        });
    }
}

/// Python `app_update_t3_rpc.session_signals`: install process termination handlers once
/// so `SIGTERM`/`SIGINT`/`SIGHUP` (Windows: `CTRL_C`/`CTRL_BREAK`/`CTRL_CLOSE`) stop active
/// T3 issuers, revoke every live T3 session within 1 s, and exit `128 + signum`.
pub(in crate::apps::updater) async fn session_signals() {
    #[cfg(unix)]
    unix_signals::install();
    #[cfg(windows)]
    windows_signals::install();
}

/// An issued temporary T3 bearer token, retired on `close` or best-effort on `Drop`.
pub(super) struct Bearer {
    pub(super) session_id: String,
    pub(super) token: String,
    cli: Vec<OsString>,
    base: PathBuf,
    retired: bool,
}

impl Bearer {
    async fn issue(
        cli: &[OsString],
        base: &Path,
        deadline: Instant,
        scopes: &[&str],
        prefix: &str,
    ) -> Result<Self, UpdateFailure> {
        let run_auth = |c: &[OsString], b: &Path, a: &[&str], t: Duration| {
            let c = c.to_vec();
            let b = b.to_path_buf();
            let a: Vec<String> = a.iter().map(|s| (*s).to_owned()).collect();
            async move {
                let refs: Vec<&str> = a.iter().map(String::as_str).collect();
                auth_command(&c, &b, &refs, t).await
            }
        };
        let revoke = |c: &[OsString], b: &Path, id: &str, t: Duration| {
            let c = c.to_vec();
            let b = b.to_path_buf();
            let id = id.to_owned();
            async move {
                tokio::task::spawn_blocking(move || revoke_session_blocking(&c, &b, &id, t))
                    .await
                    .unwrap_or(false)
            }
        };
        Self::issue_with(
            cli,
            base,
            deadline,
            scopes,
            prefix,
            run_auth,
            revoke,
            tokio::time::sleep,
        )
        .await
    }

    #[expect(clippy::too_many_arguments, reason = "injectable test seam for bearer")]
    pub(super) async fn issue_with<A, AFut, R, RFut, S, SFut>(
        cli: &[OsString],
        base: &Path,
        deadline: Instant,
        scopes: &[&str],
        prefix: &str,
        run_auth: A,
        revoke: R,
        mut sleep: S,
    ) -> Result<Self, UpdateFailure>
    where
        A: Fn(&[OsString], &Path, &[&str], Duration) -> AFut,
        AFut: std::future::Future<Output = Result<String, UpdateFailure>>,
        R: Fn(&[OsString], &Path, &str, Duration) -> RFut,
        RFut: std::future::Future<Output = bool>,
        S: FnMut(Duration) -> SFut,
        SFut: std::future::Future<Output = ()>,
    {
        let label = format!("{prefix}{}", random_hex(16));
        let mut attempted = false;
        let mut lost: &'static str = "reply_unparsed";
        let mut session_id: Option<String> = None;
        let mut backoff = Duration::from_secs(1);

        let issued_reply: Result<Value, UpdateFailure> = loop {
            let ttl = match ttl_argument(deadline) {
                Ok(ttl) => ttl,
                Err(_) => break Err(UpdateFailure::new("t3_auth_failed")),
            };
            let timeout = match remaining(deadline, None) {
                Ok(left) => left,
                Err(_) => break Err(UpdateFailure::new("t3_auth_failed")),
            };
            let mut argv: Vec<String> = vec!["issue".into()];
            for scope in scopes {
                argv.push("--scope".into());
                argv.push((*scope).to_owned());
            }
            argv.extend([
                "--ttl".into(),
                ttl,
                "--label".into(),
                label.clone(),
                "--json".into(),
            ]);
            let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            attempted = true;
            match run_auth(cli, base, &arg_refs, timeout).await {
                Ok(reply) => {
                    lost = if reply.trim().is_empty() {
                        "reply_empty"
                    } else {
                        "reply_invalid"
                    };
                    match serde_json::from_str::<Value>(&reply) {
                        Ok(value @ Value::Object(_)) => break Ok(value),
                        _ => break Err(UpdateFailure::new("t3_auth_failed")),
                    }
                }
                Err(error) if error.code == "t3_issuer_protection_failed" => return Err(error),
                Err(error) if error.code == "t3_cli_not_started" => {
                    attempted = false;
                    let Ok(wait) = remaining(deadline, Some(backoff)) else {
                        break Err(UpdateFailure::new("t3_auth_failed"));
                    };
                    sleep(wait).await;
                    backoff = (backoff * 2).min(Duration::from_secs(8));
                }
                Err(error) => {
                    lost = error.reason.unwrap_or("issuer_failed");
                    break Err(error);
                }
            }
        };

        let validated: Result<String, UpdateFailure> = (|| {
            let issued = issued_reply?;
            let id = issued
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| UpdateFailure::new("t3_auth_failed"))?;
            session_id = Some(id.to_owned());
            known_register(id, cli, base);
            let token = issued
                .get("token")
                .and_then(Value::as_str)
                .filter(|tok| !tok.is_empty() && !tok.contains('\r') && !tok.contains('\n'))
                .ok_or_else(|| UpdateFailure::new("t3_auth_failed"))?;
            Ok(token.to_owned())
        })();

        match validated {
            Ok(token) => Ok(Self {
                session_id: session_id.unwrap_or_default(),
                token,
                cli: cli.to_vec(),
                base: base.to_path_buf(),
                retired: false,
            }),
            Err(_) => {
                if let Some(id) = session_id {
                    retire_session_with(cli, base, &id, &revoke, &mut sleep).await?;
                    return Err(UpdateFailure::new("t3_auth_failed"));
                }
                if attempted {
                    let cause = if lost.starts_with("reply_") {
                        FailureCause::LostSessionReply
                    } else {
                        FailureCause::Unknown
                    };
                    if let Some(found) =
                        find_issued_session_with(cli, base, &label, &run_auth).await
                    {
                        known_register(&found, cli, base);
                        // Revoked right away; a failed revoke replaces this with `t3_session_revoke_failed`.
                        retire_session_with(cli, base, &found, &revoke, &mut sleep).await?;
                        return Err(UpdateFailure::new("t3_session_reply_lost")
                            .with_cause(cause, Some(lost)));
                    }
                    return Err(UpdateFailure::new("t3_session_issue_unknown")
                        .with_cause(cause, Some(lost)));
                }
                Err(UpdateFailure::new("t3_auth_failed"))
            }
        }
    }

    pub(super) async fn retire(mut self) -> Result<(), UpdateFailure> {
        self.retired = true;
        let revoke = |c: &[OsString], b: &Path, id: &str, t: Duration| {
            let c = c.to_vec();
            let b = b.to_path_buf();
            let id = id.to_owned();
            async move {
                tokio::task::spawn_blocking(move || revoke_session_blocking(&c, &b, &id, t))
                    .await
                    .unwrap_or(false)
            }
        };
        let mut sleep = tokio::time::sleep;
        retire_session_with(&self.cli, &self.base, &self.session_id, &revoke, &mut sleep).await
    }

    /// Test seam: [`Self::retire`] with injected revoke and sleep.
    #[cfg(test)]
    pub(super) async fn retire_with<R, RFut, S, SFut>(
        mut self,
        revoke: R,
        mut sleep: S,
    ) -> Result<(), UpdateFailure>
    where
        R: Fn(&[OsString], &Path, &str, Duration) -> RFut,
        RFut: std::future::Future<Output = bool>,
        S: FnMut(Duration) -> SFut,
        SFut: std::future::Future<Output = ()>,
    {
        self.retired = true;
        retire_session_with(&self.cli, &self.base, &self.session_id, &revoke, &mut sleep).await
    }

    fn revoke_best_effort(&mut self, budget: Duration) {
        if self.retired {
            return;
        }
        self.retired = true;
        let cli = self.cli.clone();
        let base = self.base.clone();
        let id = self.session_id.clone();
        let handle = std::thread::Builder::new()
            .name("t3-revoke".into())
            .spawn(move || {
                if revoke_session_blocking(&cli, &base, &id, budget) {
                    known_forget(&id);
                }
            });
        if let Ok(handle) = handle {
            let _ = handle.join();
        }
    }
}

impl Drop for Bearer {
    fn drop(&mut self) {
        self.revoke_best_effort(REVOKE_TIMEOUT);
    }
}

// ---------------------------------------------------------------------------
// Bounded RFC 6455 WebSocket + Effect JSON RPC client (Python `Rpc`).
// ---------------------------------------------------------------------------

/// Underlying byte stream for [`Rpc`]: real [`TcpStream`] in production, fake in tests.
pub(super) trait WsTransport: Send {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()>;
    fn read_once(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()>;
    fn close(&mut self);
}

struct TcpTransport {
    reader: BufReader<TcpStream>,
}

impl TcpTransport {
    fn connect(origin: &Origin, timeout: Duration) -> std::io::Result<Self> {
        let stream = tcp_connect(origin, timeout)?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }
}

impl WsTransport for TcpTransport {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        let clamped = timeout.max(Duration::from_millis(1));
        self.reader.get_ref().set_read_timeout(Some(clamped))?;
        self.reader.get_ref().set_write_timeout(Some(clamped))
    }

    fn read_once(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }

    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.reader.get_mut().write_all(data)
    }

    fn close(&mut self) {
        let _ = self.reader.get_ref().shutdown(std::net::Shutdown::Both);
    }
}

/// Internal RPC error keeping settle timeouts and transport drops distinct.
#[derive(Debug)]
pub(super) enum RpcError {
    Failure(UpdateFailure),
    Io,
    SettleTimeout,
}

impl RpcError {
    pub(super) fn is_recoverable(&self) -> bool {
        match self {
            Self::Io | Self::SettleTimeout => true,
            Self::Failure(failure) => {
                matches!(failure.code, "t3_install_timeout" | "t3_unreachable")
            }
        }
    }

    pub(super) fn into_failure(self) -> UpdateFailure {
        match self {
            Self::Failure(failure) => failure,
            Self::Io => UpdateFailure::new("t3_unreachable"),
            Self::SettleTimeout => UpdateFailure::new("t3_install_timeout"),
        }
    }
}

/// Python `Rpc`: bounded RFC 6455 client for Effect `Request`/`Exit`/`Chunk`/`Ack` messages.
pub(super) struct Rpc {
    transport: Box<dyn WsTransport>,
    deadline: StdInstant,
    handshake_deadline: Option<StdInstant>,
    pub(super) settle: Option<StdInstant>,
    serial: u64,
    pending_messages: VecDeque<Value>,
    pending_values: VecDeque<Value>,
    exit_reached: bool,
}

impl Rpc {
    pub(super) fn connect(
        origin: &Origin,
        token: &str,
        deadline: StdInstant,
    ) -> Result<Self, RpcError> {
        let left = deadline.saturating_duration_since(StdInstant::now());
        if left.is_zero() {
            return Err(RpcError::Failure(UpdateFailure::new("t3_install_timeout")));
        }
        let transport = TcpTransport::connect(origin, left.min(Duration::from_secs(10)))
            .map_err(|_| RpcError::Io)?;
        Self::handshake(
            Box::new(transport),
            origin,
            token,
            deadline,
            &random_bytes(16),
        )
    }

    pub(super) fn handshake(
        transport: Box<dyn WsTransport>,
        origin: &Origin,
        token: &str,
        deadline: StdInstant,
        key_bytes: &[u8],
    ) -> Result<Self, RpcError> {
        let handshake_deadline = deadline.min(StdInstant::now() + Duration::from_secs(10));
        let mut client = Self {
            transport,
            deadline,
            handshake_deadline: Some(handshake_deadline),
            settle: None,
            serial: 0,
            pending_messages: VecDeque::new(),
            pending_values: VecDeque::new(),
            exit_reached: false,
        };
        let outcome = (|| -> Result<(), RpcError> {
            let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
            let request = format!(
                "GET /ws?orchestrationProtocol=2 HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer {token}\r\n\r\n",
                origin.host_header()
            );
            let timeout = client.read_timeout()?;
            client
                .transport
                .set_timeout(timeout)
                .map_err(|_| RpcError::Io)?;
            client
                .transport
                .write_all(request.as_bytes())
                .map_err(|_| RpcError::Io)?;
            let status_line = client.read_line(8193)?;
            let status: Vec<&[u8]> = status_line
                .split(|byte| byte.is_ascii_whitespace())
                .filter(|part| !part.is_empty())
                .collect();
            let mut headers: HashMap<String, String> = HashMap::new();
            let mut total_size = 0_usize;
            loop {
                let line = client.read_line(8193)?;
                total_size += line.len();
                if line.is_empty() || line.len() > 8192 || total_size > 32_768 {
                    return Err(RpcError::Failure(UpdateFailure::new("t3_unreachable")));
                }
                if line == b"\r\n" {
                    break;
                }
                let text = std::str::from_utf8(&line)
                    .ok()
                    .filter(|text| text.is_ascii())
                    .ok_or_else(|| RpcError::Failure(UpdateFailure::new("t3_unreachable")))?;
                let (name, value) = text
                    .split_once(':')
                    .ok_or_else(|| RpcError::Failure(UpdateFailure::new("t3_unreachable")))?;
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
            if status.get(1).copied() != Some(b"101".as_slice()) {
                let code = match status.get(1).copied() {
                    Some(b"401" | b"403") => "t3_auth_failed",
                    _ => "t3_unreachable",
                };
                return Err(RpcError::Failure(UpdateFailure::new(code)));
            }
            let accept_input = format!("{key}{WS_GUID}");
            let expected = base64::engine::general_purpose::STANDARD
                .encode(sha1_digest(accept_input.as_bytes()));
            let upgrade_ok = headers
                .get("upgrade")
                .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
            let connection_ok = headers
                .get("connection")
                .is_some_and(|value| value.to_ascii_lowercase().contains("upgrade"));
            if headers.get("sec-websocket-accept").map(String::as_str) != Some(expected.as_str())
                || !upgrade_ok
                || !connection_ok
            {
                return Err(RpcError::Failure(UpdateFailure::new("t3_unreachable")));
            }
            client.handshake_deadline = None;
            Ok(())
        })();
        if let Err(error) = outcome {
            client.close();
            return Err(error);
        }
        Ok(client)
    }

    pub(super) fn close(&mut self) {
        self.transport.close();
    }

    fn read_timeout(&self) -> Result<Duration, RpcError> {
        let now = StdInstant::now();
        let effective = match self.handshake_deadline {
            Some(hs) => self.deadline.min(hs),
            None => self.deadline,
        };
        let left = effective.saturating_duration_since(now);
        if left.is_zero() {
            return Err(RpcError::Failure(UpdateFailure::new("t3_install_timeout")));
        }
        let mut timeout = left.min(SESSION_TTL);
        if let Some(settle) = self.settle {
            let quiet = settle.saturating_duration_since(now);
            if quiet.is_zero() {
                return Err(RpcError::SettleTimeout);
            }
            timeout = timeout.min(quiet);
        }
        Ok(timeout)
    }

    fn read_exact_bounded(&mut self, size: usize) -> Result<Vec<u8>, RpcError> {
        let mut data = Vec::with_capacity(size.min(65_536));
        let mut scratch = [0_u8; 65_536];
        while data.len() < size {
            let timeout = self.read_timeout()?;
            self.transport
                .set_timeout(timeout)
                .map_err(|_| RpcError::Io)?;
            let want = (size - data.len()).min(scratch.len());
            match self.transport.read_once(&mut scratch[..want]) {
                Ok(0) => return Err(RpcError::Failure(UpdateFailure::new("t3_unreachable"))),
                Ok(read) => {
                    data.extend_from_slice(&scratch[..read]);
                    let _ = self.read_timeout()?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let _ = self.read_timeout()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                    let _ = self.read_timeout()?;
                }
                Err(_) => return Err(RpcError::Io),
            }
        }
        Ok(data)
    }

    fn read_line(&mut self, limit: usize) -> Result<Vec<u8>, RpcError> {
        let mut line = Vec::new();
        while line.len() < limit {
            let byte = self.read_exact_bounded(1)?;
            line.push(byte[0]);
            if byte[0] == b'\n' {
                return Ok(line);
            }
        }
        Err(RpcError::Failure(UpdateFailure::new("t3_unreachable")))
    }

    fn send_frame(&mut self, payload: &[u8], opcode: u8) -> Result<(), RpcError> {
        let timeout = self.read_timeout()?;
        self.transport
            .set_timeout(timeout)
            .map_err(|_| RpcError::Io)?;
        let mask = random_bytes(4);
        let size = payload.len();
        let mut frame = Vec::with_capacity(14 + size);
        frame.push(0x80 | opcode);
        if size < 126 {
            frame.push(0x80 | (size as u8));
        } else if size < 65_536 {
            frame.push(0xfe);
            frame.extend_from_slice(&(size as u16).to_be_bytes());
        } else {
            frame.push(0xff);
            frame.extend_from_slice(&(size as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (index, &byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[index % 4]);
        }
        self.transport.write_all(&frame).map_err(|_| RpcError::Io)
    }

    pub(super) fn send(&mut self, message: &Value) -> Result<(), RpcError> {
        let bytes = serde_json::to_vec(message)
            .map_err(|_| RpcError::Failure(UpdateFailure::new("t3_install_failed:protocol")))?;
        self.send_frame(&bytes, 1)
    }

    pub(super) fn request(&mut self, method: &str, payload: Value) -> Result<String, RpcError> {
        self.serial += 1;
        let identifier = self.serial.to_string();
        self.send(&json!({
            "_tag": "Request",
            "id": identifier,
            "tag": method,
            "payload": payload,
            "headers": [],
        }))?;
        Ok(identifier)
    }

    pub(super) fn messages(&mut self) -> Result<Vec<Value>, RpcError> {
        let protocol = || RpcError::Failure(UpdateFailure::new("t3_install_failed:protocol"));
        let mut data = Vec::new();
        let mut started = false;
        loop {
            let head = self.read_exact_bounded(2)?;
            let (first, second) = (head[0], head[1]);
            let (opcode, raw_size) = (first & 0x0f, u64::from(second & 0x7f));
            if (first & 0x70) != 0 || (second & 0x80) != 0 {
                return Err(protocol());
            }
            let size = match raw_size {
                126 => {
                    let ext = self.read_exact_bounded(2)?;
                    u64::from(u16::from_be_bytes([ext[0], ext[1]]))
                }
                127 => {
                    let ext = self.read_exact_bounded(8)?;
                    u64::from_be_bytes([
                        ext[0], ext[1], ext[2], ext[3], ext[4], ext[5], ext[6], ext[7],
                    ])
                }
                other => other,
            };
            if size > WS_LIMIT || (data.len() as u64).saturating_add(size) > WS_LIMIT {
                return Err(protocol());
            }
            let payload =
                self.read_exact_bounded(usize::try_from(size).map_err(|_| protocol())?)?;
            match opcode {
                9 => {
                    self.send_frame(&payload, 10)?;
                    continue;
                }
                10 => continue,
                8 => return Err(RpcError::Failure(UpdateFailure::new("t3_unreachable"))),
                0 if started => {}
                1 if !started => {}
                _ => return Err(protocol()),
            }
            started = true;
            data.extend_from_slice(&payload);
            if (first & 0x80) != 0 {
                let text = std::str::from_utf8(&data).map_err(|_| protocol())?;
                let value: Value = serde_json::from_str(text).map_err(|_| protocol())?;
                return Ok(match value {
                    Value::Array(items) => items,
                    other => vec![other],
                });
            }
        }
    }

    pub(super) fn reset_stream(&mut self) {
        self.pending_values.clear();
        self.exit_reached = false;
    }

    pub(super) fn receive_next(&mut self, identifier: &str) -> Result<Option<Value>, RpcError> {
        let protocol = || RpcError::Failure(UpdateFailure::new("t3_install_failed:protocol"));
        if let Some(value) = self.pending_values.pop_front() {
            return Ok(Some(value));
        }
        if self.exit_reached {
            return Ok(None);
        }
        loop {
            if self.pending_messages.is_empty() {
                let batch = self.messages()?;
                self.pending_messages.extend(batch);
            }
            let Some(message) = self.pending_messages.pop_front() else {
                continue;
            };
            let Some(object) = message.as_object() else {
                return Err(protocol());
            };
            let tag = object.get("_tag").and_then(Value::as_str);
            if tag == Some("Ping") {
                self.send(&json!({"_tag": "Pong"}))?;
                continue;
            }
            if object.get("requestId").and_then(Value::as_str) != Some(identifier) {
                continue;
            }
            match tag {
                Some("Chunk") => {
                    self.send(&json!({"_tag": "Ack", "requestId": identifier}))?;
                    let values = object
                        .get("values")
                        .and_then(Value::as_array)
                        .ok_or_else(protocol)?;
                    self.pending_values.extend(values.iter().cloned());
                    if let Some(value) = self.pending_values.pop_front() {
                        return Ok(Some(value));
                    }
                }
                Some("Exit") => {
                    let exit = object
                        .get("exit")
                        .and_then(Value::as_object)
                        .ok_or_else(protocol)?;
                    if exit.get("_tag").and_then(Value::as_str) != Some("Success") {
                        let errors: Vec<&serde_json::Map<String, Value>> = exit
                            .get("cause")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|cause| cause.as_object()?.get("error")?.as_object())
                            .collect();
                        let denied = errors.iter().any(|error| {
                            error.get("_tag").and_then(Value::as_str)
                                == Some("EnvironmentAuthorizationError")
                        });
                        // Only `_tag` and `detail`: an instance or operation name is never evidence.
                        let mut parts = Vec::with_capacity(errors.len() * 2);
                        for error in &errors {
                            for key in ["_tag", "detail"] {
                                let text = match error.get(key) {
                                    Some(value) => python_text(value),
                                    None => String::new(),
                                };
                                parts.push(text.chars().take(2000).collect::<String>());
                            }
                        }
                        return Err(RpcError::Failure(UpdateFailure {
                            code: if denied {
                                "t3_auth_failed"
                            } else {
                                "t3_install_failed:rpc"
                            },
                            output: parts.join("\n"),
                            ..UpdateFailure::default()
                        }));
                    }
                    self.exit_reached = true;
                    return Ok(Some(exit.get("value").cloned().unwrap_or(Value::Null)));
                }
                _ => {}
            }
        }
    }

    pub(super) fn call(&mut self, method: &str, payload: Value) -> Result<Value, RpcError> {
        self.reset_stream();
        let identifier = self.request(method, payload)?;
        let value = self
            .receive_next(&identifier)?
            .ok_or_else(|| RpcError::Failure(UpdateFailure::new("t3_install_failed:protocol")))?;
        self.reset_stream();
        Ok(value)
    }

    pub(super) fn interrupt(&mut self, identifier: &str) {
        self.settle = None;
        let _ = self.send(&json!({
            "_tag": "Interrupt",
            "requestId": identifier,
            "interruptors": [],
        }));
    }
}

// ---------------------------------------------------------------------------
// Single-session RPC client & subscriptions (Python `InstallRpc` + `bearer`).
// ---------------------------------------------------------------------------

struct SessionInner {
    endpoint: Endpoint,
    deadline: Instant,
    scopes: Vec<&'static str>,
    prefix: &'static str,
    bearer: Option<Bearer>,
    client: Option<Rpc>,
    started: bool,
}

impl SessionInner {
    fn disconnect(&mut self) {
        if let Some(mut client) = self.client.take() {
            client.close();
        }
    }

    async fn refresh(&mut self) -> Result<(), UpdateFailure> {
        let _ = remaining(self.deadline, Some(Duration::from_secs(30)))?;
        if self.bearer.is_none() {
            let issued = Bearer::issue(
                &self.endpoint.cli,
                &self.endpoint.base,
                self.deadline,
                &self.scopes,
                self.prefix,
            )
            .await?;
            self.bearer = Some(issued);
        }
        if self.client.is_none() {
            let origin = self.endpoint.origin.clone();
            let token = self
                .bearer
                .as_ref()
                .map(|bearer| bearer.token.clone())
                .unwrap_or_default();
            let std_deadline = std_deadline_from(self.deadline);
            let connected =
                tokio::task::spawn_blocking(move || Rpc::connect(&origin, &token, std_deadline))
                    .await
                    .map_err(|_| UpdateFailure::new("t3_unreachable"))?;
            self.client = Some(connected.map_err(RpcError::into_failure)?);
        }
        Ok(())
    }

    async fn call_raw(&mut self, method: &str, payload: Value) -> Result<Value, RpcError> {
        self.refresh().await.map_err(RpcError::Failure)?;
        if method == "provider.install.start" {
            if self.started {
                return Err(RpcError::Failure(UpdateFailure::new(
                    "t3_install_state_unknown",
                )));
            }
            self.started = true;
        }
        let Some(mut client) = self.client.take() else {
            return Err(RpcError::Io);
        };
        let method_owned = method.to_owned();
        let (client, outcome) = tokio::task::spawn_blocking(move || {
            let result = client.call(&method_owned, payload);
            (client, result)
        })
        .await
        .map_err(|_| RpcError::Io)?;
        self.client = Some(client);
        match outcome {
            Ok(value) => Ok(value),
            Err(error) if method == "provider.install.start" && error.is_recoverable() => Err(
                RpcError::Failure(UpdateFailure::new("t3_install_state_unknown")),
            ),
            Err(error) => Err(error),
        }
    }
}

/// Python `InstallRpc` + `bearer`: one temporary T3 session; reconnects subscriptions with
/// the same bearer and never starts `provider.install.start` twice.
pub(in crate::apps::updater) struct Session<'a> {
    cx: &'a Context,
    inner: SessionInner,
}

impl<'a> Session<'a> {
    /// Open a session handle for Antigravity ACP (`providers:manage`, `orchestration:read`).
    pub(in crate::apps::updater) fn new(cx: &'a Context, endpoint: &Endpoint) -> Self {
        Self::with_scopes(cx, endpoint, &DEFAULT_SCOPES, "aac-acp-")
    }

    pub(super) fn with_scopes(
        cx: &'a Context,
        endpoint: &Endpoint,
        scopes: &[&'static str],
        prefix: &'static str,
    ) -> Self {
        let deadline = cx.deadline.min(Instant::now() + SESSION_TTL);
        Self {
            cx,
            inner: SessionInner {
                endpoint: endpoint.clone(),
                deadline,
                scopes: scopes.to_vec(),
                prefix,
                bearer: None,
                client: None,
                started: false,
            },
        }
    }

    /// Close the current WebSocket connection while keeping the bearer session alive.
    pub(in crate::apps::updater) fn disconnect(&mut self) {
        self.inner.disconnect();
    }

    /// Run one RPC request. Enforces the start-once guard on `provider.install.start` and maps
    /// any recoverable transport error on it to `t3_install_state_unknown`.
    pub(in crate::apps::updater) async fn call(
        &mut self,
        method: &str,
        payload: Value,
    ) -> Result<Value, UpdateFailure> {
        self.inner
            .call_raw(method, payload)
            .await
            .map_err(RpcError::into_failure)
    }

    async fn call_raw(&mut self, method: &str, payload: Value) -> Result<Value, RpcError> {
        self.inner.call_raw(method, payload).await
    }

    /// Python `InstallRpc.subscribe`: reconnects on recoverable errors with the same session.
    pub(in crate::apps::updater) async fn subscribe(
        &mut self,
        instance: &str,
    ) -> Result<Subscription<'_>, UpdateFailure> {
        Ok(Subscription {
            session: &mut self.inner,
            instance: instance.to_owned(),
            mode: SubscriptionMode::Reconnect {
                backoff: Duration::from_secs(1),
            },
            identifier: None,
            finished: false,
        })
    }

    /// Python `read_offer`'s bounded subscription on the current connection: a quiet stream past
    /// `settle` disconnects and yields `Some(Err(t3_install_timeout))` once.
    pub(in crate::apps::updater) async fn watch(
        &mut self,
        instance: &str,
        settle: Duration,
    ) -> Result<Subscription<'_>, UpdateFailure> {
        self.inner.refresh().await?;
        let Some(mut client) = self.inner.client.take() else {
            return Err(UpdateFailure::new("t3_unreachable"));
        };
        let instance_owned = instance.to_owned();
        let settle_at = StdInstant::now() + settle;
        let (client, requested) = tokio::task::spawn_blocking(move || {
            client.reset_stream();
            client.settle = Some(settle_at);
            let id = client.request(
                "provider.install.subscribe",
                json!({"instanceId": instance_owned}),
            );
            (client, id)
        })
        .await
        .map_err(|_| UpdateFailure::new("t3_unreachable"))?;
        self.inner.client = Some(client);
        let identifier = match requested {
            Ok(id) => id,
            Err(error) => {
                if let Some(client) = self.inner.client.as_mut() {
                    client.settle = None;
                }
                return Err(error.into_failure());
            }
        };
        Ok(Subscription {
            session: &mut self.inner,
            instance: instance.to_owned(),
            mode: SubscriptionMode::Watch,
            identifier: Some(identifier),
            finished: false,
        })
    }

    /// Disconnect and confirm revocation of the bearer session (`t3_session_revoke_failed` on failure).
    pub(in crate::apps::updater) async fn close(mut self) -> Result<(), UpdateFailure> {
        self.inner.disconnect();
        if let Some(bearer) = self.inner.bearer.take() {
            bearer.retire().await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn with_test_bearer(cx: &'a Context, endpoint: &Endpoint, token: &str) -> Self {
        let mut session = Self::new(cx, endpoint);
        session.inner.bearer = Some(Bearer {
            session_id: "test-session".into(),
            token: token.to_owned(),
            cli: endpoint.cli.clone(),
            base: endpoint.base.clone(),
            retired: true,
        });
        session
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.inner.disconnect();
        if let Some(mut bearer) = self.inner.bearer.take() {
            let budget = if self.cx.cancelled() || Instant::now() >= self.inner.deadline {
                REVOKE_TIMEOUT
            } else {
                FINAL_REVOKE_TIMEOUT
            };
            bearer.revoke_best_effort(budget);
        }
    }
}

enum SubscriptionMode {
    Reconnect { backoff: Duration },
    Watch,
}

/// Active subscription stream returned by [`Session::subscribe`] or [`Session::watch`].
/// Dropping it sends `Interrupt` best-effort when the connection is still live.
pub(in crate::apps::updater) struct Subscription<'s> {
    session: &'s mut SessionInner,
    instance: String,
    mode: SubscriptionMode,
    identifier: Option<String>,
    finished: bool,
}

impl Subscription<'_> {
    /// Yield the next subscription state, or `None` once the `Exit` value was yielded.
    pub(in crate::apps::updater) async fn next(&mut self) -> Option<Result<Value, UpdateFailure>> {
        if self.finished {
            return None;
        }
        match &mut self.mode {
            SubscriptionMode::Watch => {
                let Some(identifier) = self.identifier.clone() else {
                    self.finished = true;
                    return None;
                };
                let Some(mut client) = self.session.client.take() else {
                    self.finished = true;
                    return Some(Err(UpdateFailure::new("t3_unreachable")));
                };
                let (client, item) = tokio::task::spawn_blocking(move || {
                    let outcome = client.receive_next(&identifier);
                    (client, outcome)
                })
                .await
                .ok()?;
                self.session.client = Some(client);
                match item {
                    Ok(Some(value)) => Some(Ok(value)),
                    Ok(None) => {
                        self.finished = true;
                        None
                    }
                    Err(RpcError::SettleTimeout) => {
                        self.identifier = None;
                        self.finished = true;
                        self.session.disconnect();
                        Some(Err(UpdateFailure::new("t3_install_timeout")))
                    }
                    Err(error) => {
                        self.finished = true;
                        Some(Err(error.into_failure()))
                    }
                }
            }
            SubscriptionMode::Reconnect { backoff } => loop {
                if self.identifier.is_none() {
                    if let Err(error) = self.session.refresh().await {
                        let rpc_err = RpcError::Failure(error);
                        if !rpc_err.is_recoverable() {
                            self.finished = true;
                            return Some(Err(rpc_err.into_failure()));
                        }
                        self.session.disconnect();
                        let Ok(wait) = remaining(self.session.deadline, Some(*backoff)) else {
                            self.finished = true;
                            return Some(Err(UpdateFailure::new("t3_install_timeout")));
                        };
                        tokio::time::sleep(wait).await;
                        *backoff = (*backoff * 2).min(Duration::from_secs(8));
                        continue;
                    }
                    let Some(mut client) = self.session.client.take() else {
                        self.session.disconnect();
                        continue;
                    };
                    let instance = self.instance.clone();
                    let Ok((client, req)) = tokio::task::spawn_blocking(move || {
                        client.reset_stream();
                        let res = client.request(
                            "provider.install.subscribe",
                            json!({"instanceId": instance}),
                        );
                        (client, res)
                    })
                    .await
                    else {
                        self.session.disconnect();
                        continue;
                    };
                    self.session.client = Some(client);
                    match req {
                        Ok(id) => self.identifier = Some(id),
                        Err(error) if error.is_recoverable() => {
                            self.session.disconnect();
                            let Ok(wait) = remaining(self.session.deadline, Some(*backoff)) else {
                                self.finished = true;
                                return Some(Err(UpdateFailure::new("t3_install_timeout")));
                            };
                            tokio::time::sleep(wait).await;
                            *backoff = (*backoff * 2).min(Duration::from_secs(8));
                            continue;
                        }
                        Err(error) => {
                            self.finished = true;
                            return Some(Err(error.into_failure()));
                        }
                    }
                }
                let identifier = self.identifier.clone().unwrap_or_default();
                let Some(mut client) = self.session.client.take() else {
                    self.identifier = None;
                    continue;
                };
                let Ok((client, item)) = tokio::task::spawn_blocking(move || {
                    let outcome = client.receive_next(&identifier);
                    (client, outcome)
                })
                .await
                else {
                    self.identifier = None;
                    self.session.disconnect();
                    continue;
                };
                self.session.client = Some(client);
                match item {
                    Ok(Some(value)) => return Some(Ok(value)),
                    Ok(None) => {
                        self.identifier = None;
                        self.finished = true;
                        return None;
                    }
                    Err(error) if error.is_recoverable() => {
                        self.identifier = None;
                        self.session.disconnect();
                        let Ok(wait) = remaining(self.session.deadline, Some(*backoff)) else {
                            self.finished = true;
                            return Some(Err(UpdateFailure::new("t3_install_timeout")));
                        };
                        tokio::time::sleep(wait).await;
                        *backoff = (*backoff * 2).min(Duration::from_secs(8));
                    }
                    Err(error) => {
                        self.finished = true;
                        return Some(Err(error.into_failure()));
                    }
                }
            },
        }
    }
}

impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        if let (Some(identifier), Some(client)) =
            (self.identifier.take(), self.session.client.as_mut())
        {
            client.interrupt(&identifier);
        } else if let Some(client) = self.session.client.as_mut() {
            client.settle = None;
        }
    }
}

fn std_deadline_from(deadline: Instant) -> StdInstant {
    StdInstant::now() + deadline.saturating_duration_since(Instant::now())
}

// ---------------------------------------------------------------------------
// Desktop self-updater (Python `environment_descriptor`, `desktop_updater_offered`,
// `desktop_self_update`).
// ---------------------------------------------------------------------------

/// Python `environment_descriptor`: one bounded read-only loopback GET to
/// `/.well-known/t3/environment`, or `None`.
pub(super) async fn environment_descriptor(home: &Path, timeout: Duration) -> Option<Value> {
    let state = server_state(home)?;
    let raw_origin = state.get("origin").and_then(Value::as_str)?;
    let url = url::Url::parse(raw_origin).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    let origin = origin_of(raw_origin)?;
    let endpoint = format!("http://{}{DESCRIPTOR_PATH}", origin.host_header());
    let client = reqwest::Client::builder().no_proxy().build().ok()?;
    let mut response = client.get(&endpoint).timeout(timeout).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if raw.len().saturating_add(chunk.len()) > DESCRIPTOR_LIMIT {
            return None;
        }
        raw.extend_from_slice(&chunk);
    }
    match serde_json::from_slice::<Value>(&raw).ok()? {
        value @ Value::Object(_) => Some(value),
        _ => None,
    }
}

/// Python `desktop_updater_offered`: whether the running local T3 desktop offers its own
/// updater for `installed_version`.
pub(super) fn desktop_updater_offered(
    installed_version: Option<&str>,
    descriptor: Option<&Value>,
) -> bool {
    let (Some(installed), Some(Value::Object(value))) = (installed_version, descriptor) else {
        return false;
    };
    let Some(Value::Object(capabilities)) = value.get("capabilities") else {
        return false;
    };
    capabilities.get("serverSelfUpdate").and_then(Value::as_str) == Some("desktop-managed")
        && capabilities.get("desktopAppUpdate") == Some(&Value::Bool(true))
        && value.get("serverVersion").and_then(Value::as_str) == Some(installed)
}

/// Check the live local T3 server's environment descriptor for desktop self-update support.
pub(super) async fn desktop_offered(cx: &Context, installed_version: &str) -> bool {
    let descriptor = environment_descriptor(&cx.host.home, Duration::from_secs(5)).await;
    desktop_updater_offered(Some(installed_version), descriptor.as_ref())
}

fn self_update_reason(code: &str) -> &'static str {
    match code {
        "t3_unreachable" => "t3_self_update:t3_unreachable",
        "t3_auth_failed" => "t3_self_update:t3_auth_failed",
        "t3_not_ready" => "t3_self_update:t3_not_ready",
        "t3_install_timeout" => "t3_self_update:t3_install_timeout",
        "t3_install_failed:rpc" => "t3_self_update:t3_install_failed:rpc",
        "t3_install_failed:protocol" => "t3_self_update:t3_install_failed:protocol",
        "t3_session_revoke_failed" => "t3_self_update:t3_session_revoke_failed",
        "t3_session_reply_lost" => "t3_self_update:t3_session_reply_lost",
        "t3_session_issue_unknown" => "t3_self_update:t3_session_issue_unknown",
        "t3_issuer_protection_failed" => "t3_self_update:t3_issuer_protection_failed",
        "t3_cli_not_started" => "t3_self_update:t3_cli_not_started",
        "timeout" => "t3_self_update:timeout",
        _ => "t3_self_update:other",
    }
}

/// Core state machine of Python `desktop_self_update`, generic over transport so tests can
/// drive every commit, drop, wait-ready and revoke-failure path deterministically.
pub(super) async fn desktop_self_update_core<C, CFut, W, WFut, R, RFut>(
    cx: &Context,
    target: &str,
    accept: impl Fn(&str) -> bool,
    ready_seconds: u64,
    mut rpc_call: C,
    mut wait_for_ready: W,
    close_session: R,
) -> Result<String, UpdateFailure>
where
    C: FnMut(&'static str, Value) -> CFut,
    CFut: std::future::Future<Output = Result<Value, RpcError>>,
    W: FnMut(&str, Duration) -> WFut,
    WFut: std::future::Future<Output = Result<(), UpdateFailure>>,
    R: FnOnce() -> RFut,
    RFut: std::future::Future<Output = Result<(), UpdateFailure>>,
{
    let mut handed_off = false;
    let mut ready = false;
    let mut version: Option<String> = None;

    let body: Result<(), RpcError> = async {
        cx.phase("downloading");
        let prepared = rpc_call("server.updateServer", json!({"targetVersion": target})).await?;
        let Some(fields) = prepared.as_object() else {
            return Err(RpcError::Failure(
                UpdateFailure::new("update_failed")
                    .with_cause(FailureCause::Unknown, Some("t3_self_update_reply")),
            ));
        };
        let target_version = fields.get("targetVersion").and_then(Value::as_str);
        let request = fields.get("desktopUpdateToken").and_then(Value::as_str);
        if fields.get("method").and_then(Value::as_str) != Some("desktop-app")
            || request.is_none_or(|token| token.is_empty() || token.len() > 256)
            || target_version.is_none_or(|value| !accept(value))
        {
            return Err(RpcError::Failure(
                UpdateFailure::new("update_failed")
                    .with_cause(FailureCause::Unknown, Some("t3_self_update_reply")),
            ));
        }
        let target_version = target_version.unwrap_or_default().to_owned();
        let request = request.unwrap_or_default().to_owned();
        version = Some(target_version.clone());
        cx.phase("updating");
        match rpc_call("server.commitDesktopUpdate", json!({"requestId": request})).await {
            Ok(_) => {
                return Err(RpcError::Failure(
                    UpdateFailure::new("update_failed")
                        .with_cause(FailureCause::Unknown, Some("t3_self_update_commit")),
                ));
            }
            Err(RpcError::Io) => handed_off = true,
            Err(RpcError::Failure(error)) if error.code == "t3_unreachable" => handed_off = true,
            Err(error) => return Err(error),
        }
        match wait_for_ready(&target_version, Duration::from_secs(ready_seconds)).await {
            Ok(()) => {
                ready = true;
                Ok(())
            }
            Err(error) => Err(RpcError::Failure(
                UpdateFailure::new("restart_failed").with_cause(
                    FailureCause::T3NotReady,
                    Some(error.reason.unwrap_or("t3_not_ready")),
                ),
            )),
        }
    }
    .await;

    let close_outcome = close_session().await;
    // Mirrors Python `with bearer(...):`: a failed revoke in `finally` replaces the in-flight outcome.
    let combined: Result<(), RpcError> = match close_outcome {
        Err(revoke_error) => Err(RpcError::Failure(revoke_error)),
        Ok(()) => body,
    };

    match combined {
        Ok(()) => Ok(version.unwrap_or_default()),
        Err(RpcError::Io) => Err(UpdateFailure::new(if handed_off {
            "restart_failed"
        } else {
            "update_failed"
        })
        .with_cause(FailureCause::Unknown, Some("t3_self_update_transport"))),
        Err(RpcError::SettleTimeout) => Err(UpdateFailure::new(if handed_off {
            "restart_failed"
        } else {
            "update_failed"
        })
        .with_cause(FailureCause::Timeout, Some("t3_self_update_transport"))),
        Err(RpcError::Failure(error)) => {
            if handed_off
                && ready
                && error.code == "t3_session_revoke_failed"
                && let Some(installed) = version
            {
                // Confirmed on the new build; the short-lived session ends at its own TTL.
                return Ok(installed);
            }
            if handed_off && !ready && error.code == "t3_session_revoke_failed" {
                return Err(UpdateFailure::new("restart_failed")
                    .with_cause(FailureCause::T3NotReady, Some("t3_not_ready")));
            }
            if handed_off && error.code != "restart_failed" {
                return Err(UpdateFailure::new("restart_failed").with_cause(
                    error.cause.unwrap_or(FailureCause::Unknown),
                    Some(error.reason.unwrap_or("after_handoff")),
                ));
            }
            if !handed_off && !matches!(error.code, "update_failed" | "t3_absent") {
                return Err(UpdateFailure::new("update_failed").with_cause(
                    error.cause.unwrap_or(FailureCause::Unknown),
                    Some(self_update_reason(error.code)),
                ));
            }
            Err(error)
        }
    }
}

/// Python `desktop_self_update`: update the running T3 desktop through its own RPC updater.
pub(super) async fn desktop_self_update(
    cx: &Context,
    target: &str,
    accept: impl Fn(&str) -> bool,
    ready_seconds: u64,
) -> Result<String, UpdateFailure> {
    let endpoint = discover(cx).await?;
    let session = Arc::new(tokio::sync::Mutex::new(Some(Session::with_scopes(
        cx,
        &endpoint,
        &[MAINTAIN_SCOPE],
        "aac-t3-update-",
    ))));
    let call_session = Arc::clone(&session);
    let wait_session = Arc::clone(&session);
    let close_session = Arc::clone(&session);
    desktop_self_update_core(
        cx,
        target,
        accept,
        ready_seconds,
        move |method, payload| {
            let call_session = Arc::clone(&call_session);
            async move {
                let mut guard = call_session.lock().await;
                let Some(sess) = guard.as_mut() else {
                    return Err(RpcError::Io);
                };
                sess.call_raw(method, payload).await
            }
        },
        move |expected, seconds| {
            let expected = expected.to_owned();
            let wait_session = Arc::clone(&wait_session);
            async move {
                // Mirrors `finally: client.close()` before `wait_ready`.
                if let Some(sess) = wait_session.lock().await.as_mut() {
                    sess.disconnect();
                }
                wait_ready(cx, Some(&expected), seconds).await
            }
        },
        move || async move {
            let taken = close_session.lock().await.take();
            if let Some(sess) = taken {
                sess.close().await?;
            }
            Ok(())
        },
    )
    .await
}
