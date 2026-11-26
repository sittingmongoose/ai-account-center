//! Helpers more than one lane uses (W1 owns this file). Python homes:
//! `app_update_common.py`, and the pieces `app_update_zcode.py` imports from
//! `app_update_t3.py` (so W3 never waits on W5).
use super::{CommandSpec, Context, Host, Platform, UpdateFailure};
use base64::Engine as _;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

/// An ordered environment, names as the process had them.
pub(crate) type Env = Vec<(String, String)>;

/// Python `ACP_UNIT`: Ubuntu's adapter/ZCode oneshot.
pub(crate) const ACP_UNIT: &str = "t3-acp-update.service";
/// Python `SSH_SESSION_ENV`.
pub(crate) const SSH_SESSION_ENV: [&str; 5] = [
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    "SSH_AUTH_SOCK",
    "SSH_ORIGINAL_COMMAND",
];
/// Python `VERSION_PROBE_TIMEOUT`.
pub(crate) const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

fn dashboard_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name == "CCS_SESSION_SECRET"
        || name.starts_with("CCS_DASHBOARD_")
        || name.ends_with("_PASSWORD_HASH")
}

/// Python `strip_dashboard_secrets`.
pub(crate) fn strip_dashboard_secrets(values: Env) -> Env {
    values
        .into_iter()
        .filter(|(key, _)| !dashboard_secret(key))
        .collect()
}

fn set(values: &mut Env, key: &str, value: &str) {
    match values.iter_mut().find(|(name, _)| name == key) {
        Some(entry) => entry.1 = value.to_owned(),
        None => values.push((key.to_owned(), value.to_owned())),
    }
}

/// Python `environment(extra)`: this process's environment, the no-auto-update switches, then `extra`.
pub(crate) fn environment(extra: &[(String, String)]) -> Env {
    let mut values: Env = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    for (key, value) in super::common::NO_AUTO_UPDATE {
        set(&mut values, key, value);
    }
    for (key, value) in extra {
        set(&mut values, key, value);
    }
    strip_dashboard_secrets(values)
}

/// Python `captured_launch_environment`: only what the captured app had, each
/// name once (case-folded), plus the no-auto-update switches replacing any spelling.
pub(crate) fn captured_launch_environment(captured: &[(String, String)]) -> Env {
    let mut values: Env = Vec::new();
    for (key, value) in captured {
        if !values
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(key))
        {
            values.push((key.clone(), value.clone()));
        }
    }
    for (key, value) in super::common::NO_AUTO_UPDATE {
        values.retain(|(name, _)| !name.eq_ignore_ascii_case(key));
        values.push((key.to_owned(), value.to_owned()));
    }
    strip_dashboard_secrets(values)
}

/// Python `gui_launch_environment`: [`environment`] without the SSH session.
pub(crate) fn gui_launch_environment(extra: &[(String, String)]) -> Env {
    environment(extra)
        .into_iter()
        .filter(|(key, _)| {
            !SSH_SESSION_ENV
                .iter()
                .any(|name| name.eq_ignore_ascii_case(key))
        })
        .collect()
}

/// Python `sanitize_unit_fragment`.
pub(crate) fn sanitize_unit_fragment(value: &str) -> String {
    static RUNS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[^a-z0-9]+").expect("unit runs"));
    let cleaned = RUNS
        .replace_all(&value.to_lowercase(), "-")
        .trim_matches('-')
        .to_owned();
    if cleaned.is_empty() {
        "app".into()
    } else {
        cleaned
    }
}

/// Python `systemd_can_carry`.
pub(crate) fn systemd_can_carry(key: &str) -> bool {
    static NAME: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\A[A-Za-z_][A-Za-z0-9_]*\z").expect("env name"));
    NAME.is_match(key)
}

const LOADER_ENV: [&str; 2] = ["LD_PRELOAD", "LD_LIBRARY_PATH"];
const LOADER_ENV_PREFIX: &str = "AAC_LOADER_";

fn loader_trampoline() -> String {
    let mut parts: Vec<String> = LOADER_ENV
        .iter()
        .map(|name| {
            format!(
                "if [ \"$${{{LOADER_ENV_PREFIX}{name}+x}}\" ]; then export {name}=\"$${LOADER_ENV_PREFIX}{name}\"; unset {LOADER_ENV_PREFIX}{name}; fi"
            )
        })
        .collect();
    parts.push("exec \"$$@\"".into());
    parts.join("; ")
}

fn unit_environment(env: &[(String, String)]) -> Env {
    env.iter()
        .map(|(key, value)| {
            if LOADER_ENV.contains(&key.as_str()) {
                (format!("{LOADER_ENV_PREFIX}{key}"), value.clone())
            } else {
                (key.clone(), value.clone())
            }
        })
        .collect()
}

/// Python `systemd_run_environment`.
pub(crate) fn systemd_run_environment(env: &[(String, String)]) -> Env {
    let mut values = unit_environment(env);
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        && !runtime.is_empty()
    {
        set(&mut values, "XDG_RUNTIME_DIR", &runtime);
    }
    values
}

/// Python `systemd_user_service_argv`: wrap argv in a transient user service outside our cgroup.
pub(crate) fn systemd_user_service_argv(
    host: &Host,
    unit: &str,
    cwd: Option<&str>,
    env: &[(String, String)],
    argv: &[String],
    service_type: &str,
    properties: &[&str],
) -> Result<Vec<String>, UpdateFailure> {
    let runner = host
        .which("systemd-run")
        .ok_or_else(|| UpdateFailure::new("restart_context"))?;
    let mut wrapped = vec![
        runner.to_string_lossy().into_owned(),
        "--user".into(),
        format!("--unit={unit}"),
        "--collect".into(),
    ];
    if service_type != "simple" {
        wrapped.push(format!("--service-type={service_type}"));
    }
    wrapped.extend(properties.iter().map(|item| format!("--property={item}")));
    if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
        wrapped.push(format!("--working-directory={cwd}"));
    }
    let names: Vec<String> = strip_dashboard_secrets(unit_environment(env))
        .into_iter()
        .filter(|(key, _)| systemd_can_carry(key))
        .map(|(key, _)| key)
        .collect();
    wrapped.extend(names.iter().map(|key| format!("--setenv={key}")));
    wrapped.push("--".into());
    let first = argv.first().map(String::as_str).unwrap_or_default();
    if first.contains('$') || names.iter().any(|key| key.starts_with(LOADER_ENV_PREFIX)) {
        wrapped.extend([
            "/bin/sh".into(),
            "-c".into(),
            loader_trampoline(),
            "sh".into(),
        ]);
    }
    wrapped.extend(argv.iter().map(|word| word.replace('$', "$$")));
    Ok(wrapped)
}

/// Python `in_service_cgroup`.
pub(crate) fn in_service_cgroup() -> bool {
    static SERVICE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"/user@[0-9]+\.service/(?:.*/)?[^/]+\.service(?:/|$)").expect("cgroup")
    });
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|text| text.lines().any(|line| SERVICE.is_match(line.trim())))
}

fn numeric_version(value: &str) -> Option<&str> {
    static NUMERIC: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"[0-9]+\.[0-9]+(?:\.[0-9]+){0,3}").expect("numeric version")
    });
    NUMERIC.find(value).map(|found| found.as_str())
}

/// Python `marker_names_version`.
pub(crate) fn marker_names_version(
    marker_version: Option<&Value>,
    installed: Option<&str>,
) -> bool {
    let marker = marker_version
        .and_then(Value::as_str)
        .and_then(numeric_version);
    marker.is_some() && marker == installed.and_then(numeric_version)
}

/// Python `pending_marker`: a marker's fields, or an empty object.
pub(crate) fn pending_marker(path: &Path) -> serde_json::Map<String, Value> {
    match crate::apps::read_json(path, 64 * 1024) {
        Some(Value::Object(fields)) => fields,
        _ => serde_json::Map::new(),
    }
}

/// Python `marker_owes_restart`.
pub(crate) fn marker_owes_restart(marker: &serde_json::Map<String, Value>) -> bool {
    marker.get("desktop") == Some(&Value::Bool(true))
        || marker.get("daemon") == Some(&Value::Bool(true))
}

/// Python `marker_applies_to`.
pub(crate) fn marker_applies_to(
    marker: &serde_json::Map<String, Value>,
    installed: Option<&str>,
) -> bool {
    ["version", "target"]
        .iter()
        .any(|key| marker_names_version(marker.get(*key), installed))
}

/// Python `npm_version_key`: a semver ordering key, `version_unknown` when it is not one.
/// Python `npm_version_key`'s tuple: (major, minor, patch, released, prerelease identifiers).
pub(crate) type NpmKey = (u64, u64, u64, bool, Vec<(u8, u64, String)>);

pub(crate) fn npm_version_key(value: &str) -> Result<NpmKey, UpdateFailure> {
    static SEMVER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"\Av?([0-9]+)\.([0-9]+)\.([0-9]+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?\z",
        )
        .expect("semver")
    });
    let unknown = || UpdateFailure::new("version_unknown");
    let found = SEMVER.captures(value).ok_or_else(unknown)?;
    let part = |index: usize| found[index].parse::<u64>().map_err(|_| unknown());
    let prerelease = found.get(4).map(|item| item.as_str());
    let identifiers = prerelease
        .unwrap_or("")
        .split('.')
        .map(|item| match item.parse::<u64>() {
            Ok(number) if item.bytes().all(|byte| byte.is_ascii_digit()) => {
                (0, number, String::new())
            }
            _ => (1, 0, item.to_owned()),
        })
        .collect();
    Ok((
        part(1)?,
        part(2)?,
        part(3)?,
        prerelease.is_none(),
        identifiers,
    ))
}

/// Python `process_identity`: Linux boot id and start ticks.
pub(crate) fn process_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat
        .get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .nth(19)?
        .to_owned();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(format!("{}:{start}", boot.trim()))
}

/// Python `asar_package`: the root package.json of an Electron `app.asar`, bounded reads only.
pub(crate) fn asar_package(
    path: &Path,
    header_limit: u64,
) -> Option<serde_json::Map<String, Value>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut stream = std::fs::File::open(path).ok()?;
    let mut prefix = [0_u8; 16];
    stream.read_exact(&mut prefix).ok()?;
    let word = |at: usize| {
        u64::from(u32::from_le_bytes(
            prefix[at..at + 4].try_into().expect("4 bytes"),
        ))
    };
    let (header_size, json_size) = (word(4), word(12));
    if !(0 < json_size
        && json_size <= header_limit
        && json_size + 8 <= header_size
        && header_size <= header_limit + 16)
    {
        return None;
    }
    let mut header = vec![0; usize::try_from(json_size).ok()?];
    stream.read_exact(&mut header).ok()?;
    let header: Value = serde_json::from_slice(&header).ok()?;
    let entry = &header["files"]["package.json"];
    let size = entry["size"]
        .as_u64()
        .or_else(|| entry["size"].as_str()?.parse().ok())?;
    let offset = entry["offset"]
        .as_u64()
        .or_else(|| entry["offset"].as_str()?.parse().ok())?;
    let truthy = |value: &Value| !matches!(value, Value::Null | Value::Bool(false));
    if !(0 < size && size <= 65_536) || truthy(&entry["unpacked"]) || truthy(&entry["link"]) {
        return None;
    }
    stream
        .seek(SeekFrom::Start(8 + header_size + offset))
        .ok()?;
    let mut package = vec![0; usize::try_from(size).ok()?];
    stream.read_exact(&mut package).ok()?;
    match serde_json::from_slice(&package).ok()? {
        Value::Object(package) => Some(package),
        _ => None,
    }
}

/// Python `resolve_npm(prefix)`: (node, npm-cli.js) so npm runs without a shell.
pub(crate) fn resolve_npm(host: &Host, prefix: &Path) -> Option<(PathBuf, PathBuf)> {
    if host.platform != Platform::Windows {
        return posix_npm(host, prefix);
    }
    let mut roots = vec![prefix.to_path_buf()];
    if let Some(located) = host.which("npm").filter(|path| {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    }) && let Some(parent) = located.parent()
    {
        roots.push(parent.to_path_buf());
    }
    roots.push(host.program_files.join("nodejs"));
    for root in roots {
        let (node, cli) = (
            root.join("node.exe"),
            root.join("node_modules/npm/bin/npm-cli.js"),
        );
        if node.is_file() && cli.is_file() {
            return Some((node, cli));
        }
    }
    let node = host.which("node")?;
    let cli = node.parent()?.join("node_modules/npm/bin/npm-cli.js");
    cli.is_file().then_some((node, cli))
}

fn posix_npm(host: &Host, prefix: &Path) -> Option<(PathBuf, PathBuf)> {
    for candidate in [Some(prefix.join("bin/node")), host.which("node")]
        .into_iter()
        .flatten()
    {
        if candidate.is_file() {
            let node = crate::apps::real(&candidate);
            let Some(root) = node.parent().and_then(Path::parent) else {
                continue;
            };
            for cli in [
                root.join("lib/node_modules/npm/bin/npm-cli.js"),
                root.join("libexec/lib/node_modules/npm/bin/npm-cli.js"),
            ] {
                if cli.is_file() {
                    return Some((node, cli));
                }
            }
        }
    }
    let (npm, node) = (host.which("npm")?, host.which("node")?);
    let cli = crate::apps::real(&npm);
    (cli.file_name().is_some_and(|name| name == "npm-cli.js") && cli.is_file())
        .then(|| (crate::apps::real(&node), cli))
}

/// The outcome of a read-only version probe (Python `cli_probe`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Probed {
    Version(Option<String>),
    Timeout,
    Failed,
}

/// Python `cli_probe(path, args)`: a quiet 20 s probe.
pub(crate) async fn cli_probe(cx: &Context, path: &Path, args: &[&str]) -> Probed {
    let mut argv = vec![path.as_os_str().to_owned()];
    argv.extend(args.iter().map(Into::into));
    let mut spec = CommandSpec::new(argv)
        .timeout(VERSION_PROBE_TIMEOUT)
        .capture()
        .quiet();
    spec.capture_limit = 65_536;
    match cx.run(&spec).await {
        Ok(output) => Probed::Version(super::common::version_text(&output.stdout)),
        Err(failure) if failure.code == "timeout" => Probed::Timeout,
        Err(_) => Probed::Failed,
    }
}

/// Python `budget(deadline, maximum)`: seconds left, at most `maximum`; `timeout` when none.
pub(crate) fn budget(cx: &Context, maximum: u64) -> Result<u64, UpdateFailure> {
    let remaining = cx.remaining().as_secs();
    if remaining < 1 {
        return Err(UpdateFailure::new("timeout"));
    }
    Ok(maximum.min(remaining))
}

/// Python `acp_unit_present`.
pub(crate) fn acp_unit_present(host: &Host) -> bool {
    host.home
        .join(".config/systemd/user")
        .join(ACP_UNIT)
        .exists()
}

/// Python `manifest_asset`: (sha512 digest, size) of one asset in an electron-builder
/// `latest.yml`, bound to this exact version; `signature_failed` otherwise.
pub(crate) fn manifest_asset(
    text: &str,
    version: &str,
    asset: &str,
) -> Result<(Vec<u8>, Option<u64>), UpdateFailure> {
    static ENTRY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?m)^[ \t\r\n\x0b\x0c]*-[ \t\r\n\x0b\x0c]+url:[ \t\r\n\x0b\x0c]*")
            .expect("entry")
    });
    static SHA: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?m)^[ \t\r\n\x0b\x0c]+sha512:[ \t\r\n\x0b\x0c]*([A-Za-z0-9+/=]+)[ \t\r\n\x0b\x0c]*$",
        )
        .expect("sha")
    });
    static SIZE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?m)\A[^\n]*\n(?:[ \t]+[^\n]*\n)*?[ \t]+size:[ \t\r\n\x0b\x0c]*([0-9]{1,12})[ \t\r\n\x0b\x0c]*$")
            .expect("size")
    });
    let failed = || UpdateFailure::new("signature_failed");
    let version_line = regex::Regex::new(&format!(
        r#"(?m)^version:[ \t\r\n\x0b\x0c]*["']?{}["']?[ \t\r\x0b\x0c]*$"#,
        regex::escape(version)
    ))
    .map_err(|_| failed())?;
    if text.len() > 65_536 || !version_line.is_match(text) {
        return Err(failed());
    }
    let mut found = Vec::new();
    for entry in ENTRY.split(text).skip(1) {
        let first = entry.lines().next().unwrap_or_default();
        if first.trim().trim_matches(['"', '\'']) == asset
            && let Some(sha) = SHA.captures(entry)
        {
            let size = SIZE.captures(entry).and_then(|size| size[1].parse().ok());
            found.push((sha[1].to_owned(), size));
        }
    }
    if found.len() != 1 {
        return Err(failed());
    }
    let digest = base64::engine::general_purpose::STANDARD
        .decode(&found[0].0)
        .map_err(|_| failed())?;
    if digest.len() != 64 {
        return Err(failed());
    }
    Ok((digest, found[0].1))
}

/// Python `verify_mac` (signature part): codesign deep/strict, the designated
/// requirement for `identity` and `team`, and Gatekeeper. Callers compare the
/// bundle version themselves. Any failure is `signature_failed`.
pub(crate) async fn verify_mac_signature(
    cx: &Context,
    bundle: &Path,
    identity: &str,
    team: &str,
) -> Result<(), UpdateFailure> {
    let requirement = format!(
        "identifier \"{identity}\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\""
    );
    let bundle = bundle.as_os_str().to_owned();
    let runs = [
        (
            vec![
                "/usr/bin/codesign".into(),
                "--verify".into(),
                "--deep".into(),
                "--strict".into(),
                bundle.clone(),
            ],
            60,
        ),
        (
            vec![
                "/usr/bin/codesign".into(),
                "--verify".into(),
                format!("-R={requirement}").into(),
                bundle.clone(),
            ],
            30,
        ),
        (
            vec![
                "/usr/sbin/spctl".into(),
                "--assess".into(),
                "--type".into(),
                "execute".into(),
                bundle,
            ],
            60,
        ),
    ];
    for (argv, seconds) in runs {
        let argv: Vec<std::ffi::OsString> = argv;
        cx.run(&CommandSpec::new(argv).timeout(Duration::from_secs(seconds)))
            .await
            .map_err(|_| UpdateFailure::new("signature_failed"))?;
    }
    Ok(())
}

/// The Windows PowerShell executable (Python runs `powershell.exe` from PATH).
pub(crate) fn powershell_path(host: &Host) -> PathBuf {
    host.powershell
        .clone()
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// Python `powershell(script)`: run, capture, and drop a leading byte-order mark.
pub(crate) async fn powershell(
    cx: &Context,
    script: &str,
    timeout: Duration,
    label: Option<&'static str>,
    quiet: bool,
) -> Result<String, UpdateFailure> {
    let mut spec = CommandSpec::powershell(powershell_path(&cx.host), script).timeout(timeout);
    spec.label = label;
    spec.quiet = quiet;
    let output = cx.run(&spec).await?.stdout;
    Ok(output
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(output))
}

/// Python `ps_quote`.
pub(crate) fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Python `nsis_silent(package, seconds)`: a verified NSIS installer with `/S`;
/// its exit code decides, a timeout kills its whole tree and is `timeout`.
pub(crate) async fn nsis_silent(
    cx: &Context,
    package: &Path,
    seconds: u64,
) -> Result<(), UpdateFailure> {
    let script = format!(
        "$ErrorActionPreference='Stop'; $p=Start-Process -FilePath {} -ArgumentList '/S' -PassThru; if(-not $p.WaitForExit({})){{& taskkill.exe /PID $p.Id /T /F | Out-Null; if(-not $p.WaitForExit(10000)){{exit 1}}; 'timeout'; exit 0}}; $p.Refresh(); if($p.ExitCode -ne 0){{exit 1}}",
        ps_quote(&package.to_string_lossy()),
        seconds * 1000
    );
    let output = powershell(cx, &script, Duration::from_secs(seconds + 15), None, false).await?;
    if output.trim() == "timeout" {
        return Err(UpdateFailure::new("timeout"));
    }
    Ok(())
}

/// Python `private_temporary()`: a private `stage-*` folder under the app's staging folder.
pub(crate) fn private_temporary(cx: &Context) -> Result<tempfile::TempDir, UpdateFailure> {
    super::common::private_dir(&cx.staging).map_err(|_| UpdateFailure::failed())?;
    tempfile::Builder::new()
        .prefix("stage-")
        .tempdir_in(&cx.staging)
        .map_err(|_| UpdateFailure::failed())
}

/// One ZIP central-directory entry.
struct ZipEntry {
    name: String,
    raw_name: Vec<u8>,
    size: u64,
    compressed: u64,
    method: u16,
    mode: u32,
    offset: u64,
}

fn le16(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_le_bytes(
        data.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le32(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        data.get(at..at + 4)?.try_into().ok()?,
    )))
}

fn le64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Why a ZIP read stopped: a byte range it still needs (remote reads), or a bad archive.
enum Need {
    Missing(u64, u64),
    Bad,
}

/// Random access to a ZIP's bytes.
trait At {
    fn size(&self) -> u64;
    fn at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Need>;
}

impl At for std::fs::File {
    fn size(&self) -> u64 {
        self.metadata().map_or(0, |info| info.len())
    }

    fn at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Need> {
        use std::io::{Read, Seek, SeekFrom};
        self.seek(SeekFrom::Start(offset)).map_err(|_| Need::Bad)?;
        let mut data = vec![0; usize::try_from(len).map_err(|_| Need::Bad)?];
        self.read_exact(&mut data).map_err(|_| Need::Bad)?;
        Ok(data)
    }
}

/// Fetched byte ranges of one remote file (Python `RangeReader`'s spans).
struct Spans {
    size: u64,
    spans: Vec<(u64, Vec<u8>)>,
}

impl At for Spans {
    fn size(&self) -> u64 {
        self.size
    }

    fn at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Need> {
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= self.size)
            .ok_or(Need::Bad)?;
        for (start, data) in &self.spans {
            if *start <= offset && end <= start + data.len() as u64 {
                let from = usize::try_from(offset - start).map_err(|_| Need::Bad)?;
                let to = usize::try_from(end - start).map_err(|_| Need::Bad)?;
                return Ok(data[from..to].to_vec());
            }
        }
        Err(Need::Missing(offset, len))
    }
}

fn bad<T>(value: Option<T>) -> Result<T, Need> {
    value.ok_or(Need::Bad)
}

/// The central directory of a ZIP (ZIP64 included).
fn zip_entries(source: &mut impl At) -> Result<Vec<ZipEntry>, Need> {
    let length = source.size();
    let tail_size = length.min(65_536 + 22);
    let tail = source.at(length - tail_size, tail_size)?;
    let end = bad((0..tail.len().saturating_sub(21))
        .rev()
        .find(|at| tail[*at..].starts_with(&[0x50, 0x4b, 0x05, 0x06])))?;
    let mut count = bad(le16(&tail, end + 10))?;
    let mut size = bad(le32(&tail, end + 12))?;
    let mut start = bad(le32(&tail, end + 16))?;
    if count == 0xffff || size == 0xffff_ffff || start == 0xffff_ffff {
        // ZIP64: the locator sits right before the end record.
        let locator = bad(end.checked_sub(20))?;
        if !tail[locator..].starts_with(&[0x50, 0x4b, 0x06, 0x07]) {
            return Err(Need::Bad);
        }
        let record = source.at(bad(le64(&tail, locator + 8))?, 56)?;
        if !record.starts_with(&[0x50, 0x4b, 0x06, 0x06]) {
            return Err(Need::Bad);
        }
        count = bad(le64(&record, 32))?;
        size = bad(le64(&record, 40))?;
        start = bad(le64(&record, 48))?;
    }
    if count > 100_000 || size > 64 * 1024 * 1024 || bad(start.checked_add(size))? > length {
        return Err(Need::Bad);
    }
    let directory = source.at(start, size)?;
    bad(parse_directory(&directory, count))
}

fn parse_directory(directory: &[u8], count: u64) -> Option<Vec<ZipEntry>> {
    let mut entries = Vec::new();
    let mut at = 0;
    for _ in 0..count {
        if !directory.get(at..)?.starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
            return None;
        }
        let method = u16::try_from(le16(directory, at + 10)?).ok()?;
        let mut compressed = le32(directory, at + 20)?;
        let mut uncompressed = le32(directory, at + 24)?;
        let name_len = usize::try_from(le16(directory, at + 28)?).ok()?;
        let extra_len = usize::try_from(le16(directory, at + 30)?).ok()?;
        let comment_len = usize::try_from(le16(directory, at + 32)?).ok()?;
        let mode = u32::try_from(le32(directory, at + 38)? >> 16).ok()?;
        let mut offset = le32(directory, at + 42)?;
        let raw_name = directory.get(at + 46..at + 46 + name_len)?.to_vec();
        let extra = directory.get(at + 46 + name_len..at + 46 + name_len + extra_len)?;
        // ZIP64 extended information: only the fields whose 32-bit value is saturated, in order.
        let mut cursor = 0;
        while cursor + 4 <= extra.len() {
            let id = le16(extra, cursor)?;
            let len = usize::try_from(le16(extra, cursor + 2)?).ok()?;
            let body = extra.get(cursor + 4..cursor + 4 + len)?;
            if id == 1 {
                let mut field = 0;
                for value in [&mut uncompressed, &mut compressed, &mut offset] {
                    if *value == 0xffff_ffff {
                        *value = le64(body, field)?;
                        field += 8;
                    }
                }
            }
            cursor += 4 + len;
        }
        let name = match std::str::from_utf8(&raw_name) {
            Ok(name) => name.to_owned(),
            // Python decodes names without the UTF-8 flag as CP437; every byte maps to one character.
            Err(_) => raw_name.iter().map(|byte| char::from(*byte)).collect(),
        };
        entries.push(ZipEntry {
            name,
            raw_name,
            size: uncompressed,
            compressed,
            method,
            mode,
            offset,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(entries)
}

/// One entry's contents (stored or deflated), at most `cap` bytes either way.
fn zip_read(source: &mut impl At, entry: &ZipEntry, cap: u64) -> Result<Vec<u8>, Need> {
    use std::io::Read as _;
    if entry.compressed > cap || entry.size > cap {
        return Err(Need::Bad);
    }
    let header = source.at(entry.offset, 30)?;
    if !header.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
        return Err(Need::Bad);
    }
    let skip = bad(le16(&header, 26))? + bad(le16(&header, 28))?;
    let data = source.at(entry.offset + 30 + skip, entry.compressed)?;
    match entry.method {
        0 => Ok(data),
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(&data[..])
                .take(cap)
                .read_to_end(&mut out)
                .map_err(|_| Need::Bad)?;
            Ok(out)
        }
        _ => Err(Need::Bad),
    }
}

/// Python `RangeReader` + `zipfile` reading one member: the contents of `member` in the
/// remote ZIP at `url`, read through HTTP byte ranges without downloading the package.
/// Fixed limits: `timeout` in total, 12 requests, 8 MiB. `Ok(None)` when the member is absent.
pub(crate) async fn remote_zip_member(
    cx: &Context,
    url: &str,
    member: &str,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, UpdateFailure> {
    const MAXIMUM_BYTES: u64 = 8 * 1024 * 1024;
    const MAXIMUM_REQUESTS: u32 = 12;
    const READAHEAD: u64 = 256 * 1024;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut url = url.to_owned();
    let mut fetched = 0;
    let mut requests = 0;
    let mut get = async |url: &mut String, range: String| -> Result<RangeReply, UpdateFailure> {
        requests += 1;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if requests > MAXIMUM_REQUESTS || remaining.is_zero() {
            return Err(UpdateFailure::new("timeout"));
        }
        let reply = cx
            .system
            .range(url, &range, remaining.min(Duration::from_secs(15)))
            .await?;
        fetched += reply.data.len() as u64;
        if fetched > MAXIMUM_BYTES {
            return Err(UpdateFailure::failed());
        }
        url.clone_from(&reply.url);
        Ok(reply)
    };
    let first = get(&mut url, "bytes=-65536".into()).await?;
    let mut spans = Spans {
        size: first.total,
        spans: vec![(first.start, first.data)],
    };
    loop {
        let missing = match zip_entries(&mut spans) {
            Ok(entries) => match entries.iter().find(|entry| entry.name == member) {
                None => return Ok(None),
                Some(entry) => match zip_read(&mut spans, entry, 4 * 1024 * 1024) {
                    Ok(data) => return Ok(Some(data)),
                    Err(Need::Bad) => return Err(UpdateFailure::failed()),
                    Err(Need::Missing(offset, len)) => (offset, len),
                },
            },
            Err(Need::Bad) => return Err(UpdateFailure::failed()),
            Err(Need::Missing(offset, len)) => (offset, len),
        };
        let (offset, len) = missing;
        let last = spans.size.min((offset + len).max(offset + READAHEAD)) - 1;
        let reply = get(&mut url, format!("bytes={offset}-{last}")).await?;
        if reply.start != offset {
            return Err(UpdateFailure::failed());
        }
        spans.spans.push((reply.start, reply.data));
    }
}

/// One `206 Partial Content` answer (see [`super::System::range`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RangeReply {
    pub start: u64,
    pub data: Vec<u8>,
    pub total: u64,
    /// The final https URL behind any redirect.
    pub url: String,
}

/// Python `PurePosixPath(name).parts`.
fn posix_parts(name: &str) -> Vec<&str> {
    name.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// Python `check_mac_archive`'s `key`: default macOS volumes fold case and Unicode form.
fn archive_key(parts: &[&str]) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    parts.join("/").nfd().collect::<String>().to_lowercase()
}

/// Python `check_mac_archive(package)`: refuse (`signature_failed`) a ZIP whose entries
/// could write outside its folder or through a link. An unreadable ZIP is `update_failed`.
pub(crate) fn check_mac_archive(package: &Path) -> Result<(), UpdateFailure> {
    let refused = || UpdateFailure::new("signature_failed");
    let mut file = std::fs::File::open(package).map_err(|_| UpdateFailure::failed())?;
    let entries = zip_entries(&mut file).map_err(|_| UpdateFailure::failed())?;
    if entries.len() > 100_000
        || entries.iter().map(|item| item.size).sum::<u64>() > 2 * 1024 * 1024 * 1024
    {
        return Err(refused());
    }
    let mut paths: HashSet<String> = HashSet::new();
    let mut all_parts: Vec<Vec<String>> = Vec::new();
    let mut links: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &entries {
        let parts = posix_parts(&item.name);
        if parts.is_empty()
            || item.name.starts_with('/')
            || parts.contains(&"..")
            || item.name.contains('\\')
            || item.raw_name.contains(&0)
        {
            return Err(refused());
        }
        let key = archive_key(&parts);
        if !paths.insert(key.clone()) {
            return Err(refused());
        }
        all_parts.push(parts.iter().map(|part| (*part).to_owned()).collect());
        if item.mode & 0o170_000 == 0o120_000 {
            if item.size > 4096 {
                return Err(refused());
            }
            let target = zip_read(&mut file, item, 4096).map_err(|_| refused())?;
            let target = String::from_utf8(target).map_err(|_| refused())?;
            if target.is_empty()
                || target.starts_with('/')
                || target.contains('\\')
                || target.contains('\0')
            {
                return Err(refused());
            }
            links.insert(key, target);
        }
    }
    // An extractor must never write through an alias, whatever the order.
    for parts in &all_parts {
        for end in 1..parts.len() {
            let parent: Vec<&str> = parts[..end].iter().map(String::as_str).collect();
            if links.contains_key(&archive_key(&parent)) {
                return Err(refused());
            }
        }
    }
    for (name, target) in &links {
        let name_parts: Vec<&str> = name.split('/').collect();
        let mut remaining: std::collections::VecDeque<String> = name_parts[..name_parts.len() - 1]
            .iter()
            .map(|part| (*part).to_owned())
            .chain(target.split('/').map(str::to_owned))
            .collect();
        let mut resolved: Vec<String> = Vec::new();
        let mut hops = 0;
        while let Some(part) = remaining.pop_front() {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                if resolved.pop().is_none() {
                    return Err(refused());
                }
                continue;
            }
            resolved.push(part);
            let as_parts: Vec<&str> = resolved.iter().map(String::as_str).collect();
            if let Some(next) = links.get(&archive_key(&as_parts)) {
                hops += 1;
                if hops > 40 {
                    return Err(refused());
                }
                resolved.pop();
                for piece in next.split('/').rev() {
                    remaining.push_front(piece.to_owned());
                }
            }
        }
    }
    Ok(())
}

static CONTROL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[\x00-\x08\x0b-\x1f\x7f-\x{9f}]").expect("control"));
static ANSI: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\x1b\[[0-9;?]{0,32}[ -/]{0,8}[@-~]|\x1b\][^\x07\x1b\n]{0,512}(?:\x07|\x1b\\)?",
    )
    .expect("ansi")
});
static CREDENTIAL_FLAG: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)--?(?:password|passwd|pass|token|api-?key|auth(?:-?token)?|secret|access-?token|otp)(?-u:\b)")
        .expect("credential flag")
});
static SHORT_PASSWORD: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"-p(?:[ \t\r\n\x0b\x0c]*=|[ \t\r\n\x0b\x0c]*["'])"#).expect("short flag")
});

/// Python `CREDENTIAL_REST`.
pub(crate) const CREDENTIAL_REST: &str = "<rest of output removed: may contain a credential>";

fn flag_in(view: &str) -> bool {
    let view = CONTROL.replace_all(view, "");
    if CREDENTIAL_FLAG.is_match(&view) {
        return true;
    }
    // `(?<![\w-])-p...`: the regex crate has no look-behind, so check the byte before each match.
    SHORT_PASSWORD.find_iter(&view).any(|found| {
        view[..found.start()]
            .chars()
            .next_back()
            .is_none_or(|before| !(before.is_alphanumeric() || before == '_' || before == '-'))
    })
}

/// Python `_credential_flag`: complete text holds a recognised credential flag.
pub(crate) fn credential_flag(text: &str) -> bool {
    flag_in(text) || flag_in(&ANSI.replace_all(text, ""))
}

fn home_forms(home: &str) -> Vec<String> {
    let mut forms = vec![
        home.to_owned(),
        home.replace('\\', "/"),
        home.replace('\\', "\\\\"),
        home.replace('/', "\\"),
    ];
    forms.retain(|form| form.chars().count() > 2);
    forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
    forms.dedup();
    forms
}

/// Python `_scrub_paths`: home as `~`, any other user's folder as `<user>`.
pub(crate) fn scrub_paths(host: &Host, text: &str) -> String {
    static WINDOWS_USER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)([A-Z]:(?:\\\\|\\|/){1,4}Users(?:\\\\|\\|/){1,4})[^\\/"'\n]{1,128}"#,
        )
        .expect("windows user")
    });
    static MAC_USER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(/Users/)[^/"'\n]{1,128}"#).expect("mac user"));
    static LINUX_USER: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(/home/)[^/ \t\r\n\x0b\x0c"']{1,128}"#).expect("linux user")
    });
    let mut text = text.to_owned();
    for form in home_forms(&host.home.to_string_lossy()) {
        // Case-insensitive, and only a whole folder name: the next character is a separator or the end.
        let Ok(pattern) = regex::Regex::new(&format!("(?i){}", regex::escape(&form))) else {
            continue;
        };
        let mut out = String::new();
        let mut last = 0;
        for found in pattern.find_iter(&text) {
            let next = text[found.end()..].chars().next();
            if next.is_none_or(|ch| {
                matches!(ch, '\\' | '/' | '"' | '\'' | ':' | ';' | ',' | ')') || ch.is_whitespace()
            }) {
                out.push_str(&text[last..found.start()]);
                out.push('~');
                last = found.end();
            }
        }
        out.push_str(&text[last..]);
        text = out;
    }
    let text = WINDOWS_USER.replace_all(&text, "${1}<user>");
    let text = MAC_USER.replace_all(&text, "${1}<user>");
    LINUX_USER.replace_all(&text, "${1}<user>").into_owned()
}

fn last_chars(text: &str, count: usize) -> &str {
    match text.char_indices().rev().nth(count.saturating_sub(1)) {
        Some((start, _)) if text.chars().count() > count => &text[start..],
        _ => text,
    }
}

/// Python `display_path`: a path with home as `~` and other users as `<user>`, bounded;
/// a path holding a credential flag shows only [`CREDENTIAL_REST`].
pub(crate) fn display_path(host: &Host, path: &Path) -> String {
    let text = path.to_string_lossy();
    if credential_flag(&text.replace(['\r', '\n'], " ")) {
        return CREDENTIAL_REST.into();
    }
    let scrubbed = scrub_paths(host, last_chars(&text, 1024));
    last_chars(&scrubbed, 160).to_owned()
}

/// A held Python `execution_lock`: released when dropped.
pub(crate) struct ExecutionLock(#[allow(dead_code)] std::fs::File);

/// Python `execution_lock(name, root)`: a non-blocking exclusive lock file; `busy` when held.
/// Read+write like Python's `a+b`: Windows cannot lock an append-only handle.
pub(crate) fn execution_lock(root: &Path, name: &str) -> Result<ExecutionLock, UpdateFailure> {
    super::common::private_dir(root).map_err(|_| UpdateFailure::new("busy"))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(name))
        .map_err(|_| UpdateFailure::new("busy"))?;
    file.try_lock().map_err(|_| UpdateFailure::new("busy"))?;
    Ok(ExecutionLock(file))
}
