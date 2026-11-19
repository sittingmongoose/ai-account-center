//! `ai-account-center antigravity ...` after CUT1 = A: recover is managed by the dashboard
//! (POST /api/antigravity/recover stays for the UI) and prints a pointer; status and
//! `runtime refresh` are ports of src/antigravity/release-status.ts, terminal-refresh.ts and
//! runtime-refresh.ts on the G6 probes/runtime reads. signin keeps the labelled TS delegation:
//! lane G's signin-session verb takes a prepared command, and the Antigravity prepare/complete
//! (staging, sandbox, registry writes) stay in TS by design until M5 (README-G "What is left").

use crate::antigravity_probes::{self as probes, NativeRelease, current_uid};
use crate::antigravity_runtime;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PRODUCT: &str = "AI Account Center";

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

pub fn help() -> &'static str {
    "AI Account Center Antigravity profiles

Usage
  ai-account-center antigravity signin <profile>
  ai-account-center antigravity status
  ai-account-center antigravity runtime refresh

Commands
  signin <profile>   Add a new saved profile, or sign in again to a saved one
  status             Show what the Antigravity runtime still needs (read-only)
  runtime refresh    Re-pin the installed runtime to the reviewed CLI build

Managed in the dashboard (the CLI points there):
  recover            Finish or undo a stuck switch from an older version

Notes
  Runs on Ubuntu in an interactive terminal (over SSH is fine). The official
  Antigravity CLI starts in a private sign-in home: choose Google OAuth, open the
  link in a browser, sign in to the Google account for this profile and paste
  the code back. History and settings are not touched. Sign in again keeps the
  same Google account; when the profile is the live Antigravity login, the live
  login is renewed too. Remove profiles in the dashboard. AI Account Center no
  longer switches the live login (activate is retired); it reads each profile's
  usage.
  Refresh re-verifies the installed CLI against the reviewed set and
  re-pins the install descriptor atomically, keeping the previous pin
  as a backup. An unreviewed CLI stays paused until it is reviewed.
"
}

fn runtime_help() -> &'static str {
    "AI Account Center Antigravity runtime

Usage
  ai-account-center antigravity runtime refresh

Commands
  refresh   Re-pin the installed runtime to the reviewed CLI build
"
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The packaged `scripts/` directory. `AAC_ANTIGRAVITY_SCRIPTS` overrides it (the same
/// setting antigravity_host.rs reads); otherwise the nearest ancestor of the executable (or
/// the working directory) that carries the packaged release file wins. A build whose target
/// directory sits outside the checkout still finds the package this way.
fn scripts_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os("AAC_ANTIGRAVITY_SCRIPTS") {
        return PathBuf::from(explicit);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            candidates.push(ancestor.join("scripts"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("scripts"));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("antigravity/runtime/release.json").is_file())
        .or_else(|| {
            std::env::current_exe().ok().and_then(|exe| {
                exe.parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .map(|root| root.join("scripts"))
            })
        })
        .unwrap_or_default()
}

pub fn run(args: &[String], ccs: &Path) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        out(help());
        return 0;
    };
    match first.as_str() {
        "help" | "--help" | "-h" => {
            out(help());
            0
        }
        "status" => {
            if !rest.is_empty() {
                out("[X] Usage: ai-account-center antigravity status\n");
                return 1;
            }
            status(ccs, &home_dir())
        }
        "recover" => {
            if !rest.is_empty() {
                out("[X] Usage: ai-account-center antigravity recover\n");
                return 1;
            }
            err(&format!(
                "[X] \"antigravity recover\" is managed in the dashboard now ({PRODUCT} account recovery).\n"
            ));
            err(
                "    Open ai-account-center dashboard; the Antigravity recovery action stays available there.\n",
            );
            1
        }
        "activate" => {
            out("[X] antigravity activate is retired in AI Account Center.\n");
            out(
                "    Account switching was removed; AI Account Center only reads usage and limits.\n",
            );
            1
        }
        "runtime" => {
            if rest.is_empty()
                || (rest.len() == 1 && matches!(rest[0].as_str(), "help" | "--help" | "-h"))
            {
                out(runtime_help());
                return 0;
            }
            if rest.len() != 1 || rest[0] != "refresh" {
                out("[X] Usage: ai-account-center antigravity runtime refresh\n");
                return 1;
            }
            refresh(ccs, &home_dir())
        }
        "signin" => {
            if rest.len() != 1 || rest[0].starts_with('-') {
                out("[X] Usage: ai-account-center antigravity signin <profile>\n");
                return 1;
            }
            // The verb takes a prepared command; the Antigravity prepare/complete stay in TS
            // until M5 (lane G design). Keep the labelled delegation so the terminal flow
            // works unchanged; the wrapper owns the usage contract above.
            let mut full = vec!["antigravity".to_string(), "signin".to_string()];
            full.extend(rest.iter().cloned());
            match super::typescript(&full) {
                Ok(code) => code,
                Err(message) => {
                    err(&format!("[X] {message}\n"));
                    1
                }
            }
        }
        other => {
            out(&format!(
                "[X] Unknown antigravity command: {other}. Run: ai-account-center antigravity help\n"
            ));
            1
        }
    }
}

// ── owned native pin (production-runtime.ts) ─────────────────────────────────

const MAX_NATIVE_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(unix)]
fn safe_native_file(stat: &fs::Metadata, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    stat.is_file()
        && stat.uid() == uid
        && stat.nlink() == 1
        && stat.mode() & 0o100 != 0
        && stat.mode() & 0o022 == 0
        && stat.len() > 0
        && stat.len() <= MAX_NATIVE_BYTES
}

#[cfg(unix)]
fn same_native_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
        && left.len() == right.len()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
}

/// hashOwnedAntigravityNative: one owned fd, identity checked before, during and after.
fn hash_owned_native(binary: &Path) -> Option<String> {
    let uid = current_uid()?;
    let before = fs::symlink_metadata(binary).ok()?;
    #[cfg(unix)]
    {
        use sha2::{Digest, Sha256};
        if !safe_native_file(&before, uid) {
            return None;
        }
        use std::os::unix::fs::{FileExt, OpenOptionsExt};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(binary)
            .ok()?;
        let opened = file.metadata().ok()?;
        if !safe_native_file(&opened, uid) || !same_native_file(&before, &opened) {
            return None;
        }
        let size = opened.len();
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; (size.min(64 * 1024)) as usize];
        let mut position: u64 = 0;
        while position < size {
            let length = ((size - position).min(buffer.len() as u64)) as usize;
            let count = file.read_at(&mut buffer[..length], position).ok()?;
            if count == 0 || count > length {
                return None;
            }
            hash.update(&buffer[..count]);
            position += count as u64;
        }
        let mut tail = [0u8; 1];
        if file.read_at(&mut tail, size).ok()? != 0 {
            return None;
        }
        let final_descriptor = file.metadata().ok()?;
        let final_leaf = fs::symlink_metadata(binary).ok()?;
        if !safe_native_file(&final_descriptor, uid)
            || !safe_native_file(&final_leaf, uid)
            || !same_native_file(&before, &final_descriptor)
            || !same_native_file(&before, &final_leaf)
        {
            return None;
        }
        Some(hex(&hash.finalize()))
    }
    #[cfg(not(unix))]
    {
        let _ = (binary, uid, before);
        None
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

fn verify_pin(binary: &Path, expected: &str) -> bool {
    expected.len() == 64
        && expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && hash_owned_native(binary).as_deref() == Some(expected)
}

// ── installed descriptor (production-runtime.ts / runtime-refresh.ts) ────────

fn hash_word(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The exact owned descriptor bytes with adopt-grade checks, or None.
fn read_descriptor_bytes(file: &Path, directory: &Path) -> Option<Vec<u8>> {
    let parent = fs::symlink_metadata(directory).ok()?;
    let before = fs::symlink_metadata(file).ok()?;
    let uid = current_uid()?;
    #[cfg(unix)]
    {
        use std::{io::Read, os::unix::fs::MetadataExt};
        if !parent.is_dir()
            || parent.file_type().is_symlink()
            || parent.mode() & 0o777 != 0o700
            || !before.is_file()
            || before.file_type().is_symlink()
            || before.nlink() != 1
            || before.mode() & 0o777 != 0o600
            || parent.uid() != uid
            || before.uid() != uid
            || before.len() > 8192
        {
            return None;
        }
        use std::os::unix::fs::OpenOptionsExt;
        let mut handle = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(file)
            .ok()?;
        let opened = handle.metadata().ok()?;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return None;
        }
        let mut raw = Vec::new();
        handle.read_to_end(&mut raw).ok()?;
        drop(handle);
        let after = fs::symlink_metadata(file).ok()?;
        if after.dev() != before.dev() || after.ino() != before.ino() || raw.len() > 8192 {
            return None;
        }
        Some(raw)
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, before, uid);
        None
    }
}

struct InstalledRecord {
    value: Value,
    bundle_directory: PathBuf,
    native_binary: PathBuf,
    native_sha256: String,
}

/// readInstalledAntigravityRuntime: the validated private installation record.
fn read_installed_record(ccs: &Path, home: &Path) -> Option<InstalledRecord> {
    let directory = ccs.join("antigravity-switching");
    let file = directory.join("runtime-installation.json");
    let raw = read_descriptor_bytes(&file, &directory)?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    let runtime_root = home.join(".local/share/ai-account-center/antigravity-runtime/bundles");
    let bundle = value.get("bundleDirectory")?.as_str()?;
    let bundle_path = PathBuf::from(bundle);
    let native_binary = home.join(".local/bin/agy");
    let native_sha256 = value.get("nativeSha256")?.as_str()?.to_string();
    if value.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || bundle_path.parent() != Some(runtime_root.as_path())
        || !bundle_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(hash_word)
        || value.get("nativeBinary").and_then(Value::as_str)
            != Some(native_binary.to_string_lossy().as_ref())
        || !hash_word(&native_sha256)
        || value.get("socketPath").and_then(Value::as_str)
            != Some(
                ccs.join("antigravity-runtime/control.sock")
                    .to_string_lossy()
                    .as_ref(),
            )
    {
        return None;
    }
    Some(InstalledRecord {
        value,
        bundle_directory: bundle_path,
        native_binary,
        native_sha256,
    })
}

fn bundle_gate_open(bundle: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(bundle.join("lib").join("release.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    value
        .get("nativeActivationReleased")
        .and_then(Value::as_bool)
        == Some(true)
        && value
            .get("nativeProofReceiptSha256")
            .and_then(Value::as_str)
            .is_some_and(hash_word)
}

fn default_release_file() -> PathBuf {
    scripts_dir().join("antigravity/runtime/release.json")
}

/// readInstalledNativeVersion: the fixed `agy --version` read, bounded; the stamp only.
fn read_installed_version(home: &Path) -> Option<String> {
    let binary = home.join(".local/bin/agy");
    let output = probes::bounded_run(&binary.to_string_lossy(), &["--version"], 5000)?;
    if output.len() > 64 * 1024 {
        return None;
    }
    // /\b(\d+\.\d+\.\d+(?:-[\w.-]+)?)\b/
    let bytes = output.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let boundary_before = start == 0 || !is_word_byte(bytes[start - 1]);
        if !boundary_before {
            start += 1;
            continue;
        }
        if let Some(end) = version_end(bytes, start) {
            let word = &output[start..end];
            let boundary_after = end == bytes.len() || !is_word_byte(bytes[end]);
            if boundary_after && word.len() <= 128 {
                return Some(word.to_string());
            }
        }
        start += 1;
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn version_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    for part in 0..3 {
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start {
            return None;
        }
        if part < 2 {
            if bytes.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
    }
    if bytes.get(i) == Some(&b'-') {
        let suffix_start = i + 1;
        let mut j = suffix_start;
        while j < bytes.len() && (is_word_byte(bytes[j]) || bytes[j] == b'.' || bytes[j] == b'-') {
            j += 1;
        }
        if j > suffix_start {
            i = j;
        }
    }
    Some(i)
}

// ── status (release-status.ts) ───────────────────────────────────────────────

fn read_adoption(ccs: &Path) -> &'static str {
    let file = ccs
        .join("antigravity-switching")
        .join("runtime-adoption.json");
    let Ok(stat) = fs::symlink_metadata(&file) else {
        return "none";
    };
    if !stat.is_file() || stat.len() > 4 * 1024 * 1024 {
        return "unfinished";
    }
    let Ok(raw) = fs::read_to_string(&file) else {
        return "unfinished";
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) if value.get("phase").and_then(Value::as_str) == Some("adopted") => "adopted",
        _ => "unfinished",
    }
}

fn problem_detail(problem: &Value) -> String {
    let module = problem
        .get("module")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let python = problem.get("python").and_then(Value::as_str);
    let built_for = problem.get("builtFor").and_then(Value::as_str);
    let moved = match (python, built_for) {
        (Some(p), Some(b)) if p != b => {
            format!(" (system Python is {p}; the runtime bundle was built for {b})")
        }
        _ => String::new(),
    };
    match problem.get("reason").and_then(Value::as_str) {
        Some("missing-python-module") => format!("missing Python module {module}{moved}"),
        Some("parser-mismatch") => {
            format!("Python module {module} is not the pinned version{moved}")
        }
        Some("parser-import-failed") => format!("Python module {module} does not load{moved}"),
        _ => match problem.get("exitStatus").and_then(Value::as_i64) {
            Some(status) => format!("exit status {status}"),
            None => "the unit failed".into(),
        },
    }
}

fn is_parser_problem(problem: &Value) -> bool {
    problem.get("reason").and_then(Value::as_str) != Some("service-failed")
}

struct Status {
    native_cli: &'static str,
    reviewed_versions: Vec<String>,
    dashboard_gate: bool,
    runtime_gate: bool,
    runtime_installed: bool,
    adoption: &'static str,
    service_socket: bool,
    service_problem: Option<Value>,
    profiles: Vec<(String, String, bool)>,
}

fn next_step(status: &Status) -> String {
    if status.native_cli == "missing" {
        return "Install the Antigravity CLI at ~/.local/bin/agy.".into();
    }
    if status.profiles.is_empty() {
        return "Save a profile: ai-account-center antigravity signin <profile>.".into();
    }
    if status.native_cli == "changed" {
        return "The Antigravity CLI is not the reviewed build; managed updates are paused until the runtime is reviewed for it.".into();
    }
    if !status.runtime_gate || !status.dashboard_gate {
        return "Open both release gates and deploy that build (releasing steps 1 and 2).".into();
    }
    if !status.runtime_installed {
        return "Install the runtime: python3 -I scripts/antigravity/install_runtime.py --apply (step 3).".into();
    }
    if status.adoption != "adopted" {
        return if status.adoption == "none" {
            "Adopt the runtime: python3 -I scripts/antigravity/adopt_runtime.py (step 4).".into()
        } else {
            "Runtime adoption did not finish; review it or roll it back with adopt_runtime.py --rollback.".into()
        };
    }
    if !status.service_socket {
        if let Some(problem) = &status.service_problem {
            if is_parser_problem(problem) {
                return format!(
                    "Runtime service failed: {}. Starting the service again will fail; rebuild the runtime bundle: python3 -I scripts/antigravity/rebuild_bundle.py --plan, then --apply. It restarts only the runtime service; the dashboard picks it up without a restart.",
                    problem_detail(problem)
                );
            }
            return format!(
                "Runtime service failed: {}. Read journalctl --user -u ai-account-center-antigravity.service and fix the cause before starting it again.",
                problem_detail(problem)
            );
        }
        return "Start ai-account-center-antigravity.service, then restart ccs-dashboard (steps 4 and 5).".into();
    }
    "The Antigravity runtime is ready.".into()
}

fn status(ccs: &Path, home: &Path) -> i32 {
    let ccs = std::path::absolute(ccs).unwrap_or_else(|_| ccs.to_path_buf());
    let home = std::path::absolute(home).unwrap_or_else(|_| home.to_path_buf());
    let release = probes::read_native_release(&default_release_file());
    let binary = home.join(".local/bin/agy");
    let native_cli = if probes::native_binary_problem_public(&home, current_uid()) {
        "missing"
    } else if release
        .reviewed
        .iter()
        .any(|entry| verify_pin(&binary, &entry.sha256))
    {
        "pinned"
    } else {
        "changed"
    };
    let installed = read_installed_record(&ccs, &home);
    let mut service_socket = false;
    let mut service_problem = None;
    if let Some(record) = &installed {
        let socket = ccs.join("antigravity-runtime/control.sock");
        service_socket = fs::symlink_metadata(&socket)
            .map(|meta| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    meta.file_type().is_socket()
                }
                #[cfg(not(unix))]
                {
                    let _ = meta;
                    false
                }
            })
            .unwrap_or(false);
        if !service_socket {
            let script = scripts_dir().join("antigravity/runtime_health.py");
            service_problem = probes::diagnose(
                &record.bundle_directory,
                "/usr/bin/python3",
                &script,
                &probes::bounded_run,
            )
            .and_then(|value| probes::public_problem(&value));
        }
    }
    let profiles = antigravity_runtime::read_state(&ccs)
        .map(|state| {
            state
                .profiles
                .iter()
                .map(|profile| {
                    (
                        profile.id.clone(),
                        profile.email.clone(),
                        state.active_for(&profile.id).is_some(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let status = Status {
        native_cli,
        reviewed_versions: release.reviewed.iter().map(|e| e.version.clone()).collect(),
        dashboard_gate: true, // ANTIGRAVITY_NATIVE_RELEASED
        runtime_gate: release.gate_open,
        runtime_installed: installed.is_some(),
        adoption: read_adoption(&ccs),
        service_socket,
        service_problem,
        profiles,
    };
    let yes = |value: bool| if value { "yes" } else { "no" };
    let gate = |open: bool| if open { "open" } else { "closed" };
    out("Antigravity runtime on Ubuntu\n");
    out(&format!(
        "  Native CLI:          {}\n",
        match status.native_cli {
            "pinned" => format!(
                "installed, reviewed {} build",
                status.reviewed_versions.join(" / ")
            ),
            "changed" => "installed, not the reviewed build".into(),
            _ => "missing".into(),
        }
    ));
    out(&format!(
        "  Dashboard gate:      {}\n",
        gate(status.dashboard_gate)
    ));
    out(&format!(
        "  Runtime gate:        {}\n",
        gate(status.runtime_gate)
    ));
    out(&format!(
        "  Runtime installed:   {}\n",
        yes(status.runtime_installed)
    ));
    out(&format!(
        "  Runtime adopted:     {}\n",
        match status.adoption {
            "none" => "no",
            "adopted" => "yes",
            _ => "unfinished",
        }
    ));
    out(&format!(
        "  Runtime service:     {}\n",
        if status.service_socket {
            "socket present".into()
        } else if let Some(problem) = &status.service_problem {
            format!("failed: {}", problem_detail(problem))
        } else {
            "not running".into()
        }
    ));
    out(&format!(
        "  Saved profiles:      {}\n",
        if status.profiles.is_empty() {
            "none".into()
        } else {
            status
                .profiles
                .iter()
                .map(|(id, email, active)| {
                    format!("{id} ({email}){}", if *active { " active" } else { "" })
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    out(&format!("  Next step:           {}\n", next_step(&status)));
    0
}

// ── refresh (runtime-refresh.ts + terminal-refresh.ts) ───────────────────────

fn owns_state(ccs: &Path) -> bool {
    let Ok(stat) = fs::symlink_metadata(ccs) else {
        return false;
    };
    if !stat.is_dir() || stat.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        current_uid().is_some_and(|uid| stat.uid() == uid)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn ensure_backup_directory(directory: &Path) -> bool {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if builder.create(directory).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).is_err() {
            return false;
        }
    }
    let Ok(info) = fs::symlink_metadata(directory) else {
        return false;
    };
    if !info.is_dir() || info.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        info.uid() == current_uid().unwrap_or(u32::MAX) && info.mode() & 0o777 == 0o700
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn write_file_exclusive(file: &Path, raw: &[u8]) -> bool {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let Ok(mut handle) = options.open(file) else {
        return false;
    };
    if handle.write_all(raw).is_err() {
        return false;
    }
    handle.sync_all().is_ok()
}

fn fsync_directory(directory: &Path) {
    if let Ok(handle) = fs::File::open(directory) {
        let _ = handle.sync_all();
    }
}

fn descriptor_backup_path(backups: &Path, pin: &str, raw_before: &[u8]) -> Option<PathBuf> {
    let named = backups.join(format!("{pin}.json"));
    let exists = fs::symlink_metadata(&named).is_ok();
    if !exists {
        return Some(named);
    }
    let kept = read_descriptor_bytes(&named, backups)?;
    if kept == raw_before {
        return Some(named);
    }
    let digest = &sha256_hex(raw_before)[..16];
    Some(backups.join(format!("{pin}-{digest}.json")))
}

#[derive(Debug)]
enum Refresh {
    Current { version: String },
    Refreshed { version: String },
    Unreviewed { installed_version: Option<String> },
    NoInstallation,
    GateClosed,
    Failed,
}

fn refresh_runtime_descriptor(ccs: &Path, home: &Path, release: &NativeRelease) -> Refresh {
    if !release.gate_open {
        return Refresh::GateClosed;
    }
    if release.reviewed.is_empty() {
        return Refresh::Failed;
    }
    let directory = ccs.join("antigravity-switching");
    let file = directory.join("runtime-installation.json");
    let binary = home.join(".local/bin/agy");
    let match_reviewed = || -> Option<&probes::ReviewedNative> {
        release
            .reviewed
            .iter()
            .find(|entry| verify_pin(&binary, &entry.sha256))
    };
    let refuse = || Refresh::Unreviewed {
        installed_version: read_installed_version(home),
    };
    for pass in 0..2 {
        let Some(raw_before) = read_descriptor_bytes(&file, &directory) else {
            return Refresh::NoInstallation;
        };
        let Some(record) = read_installed_record(ccs, home) else {
            return Refresh::NoInstallation;
        };
        if record.native_binary != binary {
            return Refresh::NoInstallation;
        }
        if !bundle_gate_open(&record.bundle_directory) {
            return Refresh::GateClosed;
        }
        let matched = if verify_pin(&binary, &record.native_sha256) {
            release
                .reviewed
                .iter()
                .find(|entry| entry.sha256 == record.native_sha256)
        } else {
            match_reviewed()
        };
        let Some(matched) = matched else {
            return refuse();
        };
        if matched.sha256 == record.native_sha256 {
            let settled = read_descriptor_bytes(&file, &directory);
            if settled.as_deref() != Some(raw_before.as_slice()) {
                continue;
            }
            return Refresh::Current {
                version: matched.version.clone(),
            };
        }
        let mut next_value = record.value.clone();
        next_value
            .as_object_mut()
            .expect("record object")
            .insert("nativeSha256".into(), json!(matched.sha256));
        let next = format!(
            "{}\n",
            serde_json::to_string(&next_value).unwrap_or_default()
        );
        let temp = directory.join(format!(
            ".runtime-installation.json.tmp-{}-{}",
            std::process::id(),
            super::codex_auth::random_suffix_public()
        ));
        if !write_file_exclusive(&temp, next.as_bytes()) {
            return Refresh::Failed;
        }
        let outcome = (|| -> Refresh {
            let pre_rename = read_descriptor_bytes(&file, &directory);
            if pre_rename.as_deref() != Some(raw_before.as_slice()) {
                return Refresh::Failed; // caller continues the pass loop on conflict
            }
            let backups = directory.join("descriptor-backups");
            if !ensure_backup_directory(&backups) {
                return Refresh::Failed;
            }
            let Some(backup) = descriptor_backup_path(&backups, &record.native_sha256, &raw_before)
            else {
                return Refresh::Failed;
            };
            if fs::symlink_metadata(&backup).is_ok() {
                let kept = read_descriptor_bytes(&backup, &backups);
                if kept.as_deref() != Some(raw_before.as_slice()) {
                    return Refresh::Failed;
                }
            } else if !write_file_exclusive(&backup, &raw_before) {
                return Refresh::Failed;
            }
            let final_check = read_descriptor_bytes(&file, &directory);
            if final_check.as_deref() != Some(raw_before.as_slice()) {
                return Refresh::Failed;
            }
            if fs::rename(&temp, &file).is_err() {
                return Refresh::Failed;
            }
            fsync_directory(&directory);
            let observed = read_descriptor_bytes(&file, &directory);
            if observed.as_deref() != Some(next.as_bytes()) {
                if fs::read(&backup)
                    .map(|kept| kept == raw_before)
                    .unwrap_or(false)
                {
                    let _ = fs::remove_file(&backup);
                }
                return Refresh::Failed;
            }
            Refresh::Refreshed {
                version: matched.version.clone(),
            }
        })();
        let _ = fs::remove_file(&temp);
        // A conflict before the rename restarts the pass; every other outcome is final.
        match outcome {
            Refresh::Failed if pass == 0 => {
                let pre = read_descriptor_bytes(&file, &directory);
                if pre.as_deref() != Some(raw_before.as_slice()) {
                    continue;
                }
                return Refresh::Failed;
            }
            other => return other,
        }
    }
    Refresh::Failed
}

fn refresh(ccs: &Path, home: &Path) -> i32 {
    let fail = |line: &str| -> i32 {
        out(&format!("[X] {line}\n"));
        1
    };
    if !owns_state(ccs) {
        return fail(
            "Antigravity runtime refresh runs only as the user that owns this computer's Antigravity state.",
        );
    }
    let ccs = std::path::absolute(ccs).unwrap_or_else(|_| ccs.to_path_buf());
    let home = std::path::absolute(home).unwrap_or_else(|_| home.to_path_buf());
    let release = probes::read_native_release(&default_release_file());
    match refresh_runtime_descriptor(&ccs, &home, &release) {
        Refresh::Current { version } => {
            out(&format!(
                "[OK] Antigravity runtime is current (reviewed {version} build).\n"
            ));
            0
        }
        Refresh::Refreshed { version } => {
            out(&format!(
                "[OK] Antigravity runtime refreshed to the reviewed {version} build.\n"
            ));
            out("The previous pin is kept as a backup; switching proofs re-read the descriptor.\n");
            0
        }
        Refresh::Unreviewed { installed_version } => {
            let version = installed_version
                .filter(|v| is_native_version(v))
                .map(|v| format!(" to {v}"))
                .unwrap_or_default();
            fail(&format!(
                "Antigravity updated{version}; switching paused until reviewed."
            ))
        }
        Refresh::NoInstallation => fail(
            "No Antigravity runtime is installed. Install it first: python3 -I scripts/antigravity/install_runtime.py --apply (step 3).",
        ),
        Refresh::GateClosed => {
            fail("Antigravity activation is not released in this build. Nothing was changed.")
        }
        Refresh::Failed => {
            fail("Antigravity runtime refresh could not complete its checks. Nothing was changed.")
        }
    }
}

fn is_native_version(value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let (core, suffix) = match value.split_once('-') {
        Some((core, suffix)) => (core, Some(suffix)),
        None => (value, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    match suffix {
        None => true,
        Some(suffix) => {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn version_stamps_follow_the_reference_grammar() {
        assert!(is_native_version("1.2.3"));
        assert!(is_native_version("1.2.3-beta.1"));
        assert!(!is_native_version("1.2"));
        assert!(!is_native_version("1.2.3-"));
        assert!(!is_native_version(&format!("1.2.3-{}", "a".repeat(128))));
    }

    #[test]
    fn the_version_reader_extracts_the_first_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let bin_dir = home.join(".local/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let agy = bin_dir.join("agy");
        fs::write(&agy, "#!/bin/sh\necho 'agy 1.2.3-beta.1 (build 5)'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&agy, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                read_installed_version(&home).as_deref(),
                Some("1.2.3-beta.1")
            );
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        ccs: PathBuf,
        home: PathBuf,
    }

    fn write_binary(path: &Path, content: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn fixture(binary_content: &[u8], pinned_sha: &str, gate: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let ccs = dir.path().join("ccs");
        let home = dir.path().join("home");
        let switching = ccs.join("antigravity-switching");
        fs::create_dir_all(&switching).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&switching, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let bundle = home
            .join(".local/share/ai-account-center/antigravity-runtime/bundles")
            .join("a".repeat(64));
        fs::create_dir_all(bundle.join("lib")).unwrap();
        fs::write(
            bundle.join("lib/release.json"),
            format!(
                "{{\"nativeActivationReleased\":{gate},\"nativeProofReceiptSha256\":\"{}\"}}",
                "b".repeat(64)
            ),
        )
        .unwrap();
        let binary = home.join(".local/bin/agy");
        write_binary(&binary, binary_content);
        let record = json!({
            "schemaVersion": 1,
            "bundleDirectory": bundle,
            "nativeBinary": binary,
            "nativeSha256": pinned_sha,
            "socketPath": ccs.join("antigravity-runtime/control.sock"),
        });
        let file = switching.join("runtime-installation.json");
        fs::write(
            &file,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        }
        Fixture { dir, ccs, home }
    }

    fn release(entries: &[(&str, &str)], gate: bool) -> NativeRelease {
        NativeRelease {
            gate_open: gate,
            reviewed: entries
                .iter()
                .map(|(version, sha256)| probes::ReviewedNative {
                    version: version.to_string(),
                    sha256: sha256.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn the_descriptor_reader_enforces_the_adopt_grade_rules() {
        let sha = sha256_hex(b"reviewed-one");
        let f = fixture(b"reviewed-one", &sha, true);
        assert!(read_installed_record(&f.ccs, &f.home).is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = f
                .ccs
                .join("antigravity-switching/runtime-installation.json");
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_installed_record(&f.ccs, &f.home).is_none());
        }
        let _ = &f.dir;
    }

    #[test]
    fn refresh_reports_current_refreshed_unreviewed_and_gates() {
        let sha_one = sha256_hex(b"reviewed-one");
        let sha_two = sha256_hex(b"reviewed-two");
        let reviewed = release(&[("1.0.0", &sha_one), ("2.0.0", &sha_two)], true);

        // Current: the binary matches the pinned reviewed build.
        let f = fixture(b"reviewed-one", &sha_one, true);
        match refresh_runtime_descriptor(&f.ccs, &f.home, &reviewed) {
            Refresh::Current { version } => assert_eq!(version, "1.0.0"),
            other => panic!("expected current, got {other:?}"),
        }

        // Refreshed: the binary moved to the second reviewed build; the pin follows,
        // and the previous descriptor bytes are kept as a backup.
        let f = fixture(b"reviewed-two", &sha_one, true);
        match refresh_runtime_descriptor(&f.ccs, &f.home, &reviewed) {
            Refresh::Refreshed { version } => assert_eq!(version, "2.0.0"),
            other => panic!("expected refreshed, got {other:?}"),
        }
        let record = read_installed_record(&f.ccs, &f.home).unwrap();
        assert_eq!(record.native_sha256, sha_two);
        let backups = f.ccs.join("antigravity-switching/descriptor-backups");
        assert!(backups.join(format!("{sha_one}.json")).is_file());

        // Unreviewed: the binary matches nothing in the set.
        let f = fixture(b"unknown-build", &sha_one, true);
        match refresh_runtime_descriptor(&f.ccs, &f.home, &reviewed) {
            Refresh::Unreviewed { .. } => {}
            other => panic!("expected unreviewed, got {other:?}"),
        }

        // Gate closed at the release or at the bundle.
        let f = fixture(b"reviewed-one", &sha_one, true);
        assert!(matches!(
            refresh_runtime_descriptor(&f.ccs, &f.home, &release(&[("1.0.0", &sha_one)], false)),
            Refresh::GateClosed
        ));
        let f = fixture(b"reviewed-one", &sha_one, false);
        assert!(matches!(
            refresh_runtime_descriptor(&f.ccs, &f.home, &reviewed),
            Refresh::GateClosed
        ));

        // No installation.
        let dir = tempfile::tempdir().unwrap();
        let ccs = dir.path().join("ccs");
        let home = dir.path().join("home");
        fs::create_dir_all(&ccs).unwrap();
        fs::create_dir_all(&home).unwrap();
        assert!(matches!(
            refresh_runtime_descriptor(&ccs, &home, &reviewed),
            Refresh::NoInstallation
        ));
    }

    #[test]
    fn adoption_and_problem_text_match_the_reference() {
        let dir = tempfile::tempdir().unwrap();
        let ccs = dir.path().join("ccs");
        assert_eq!(read_adoption(&ccs), "none");
        let switching = ccs.join("antigravity-switching");
        fs::create_dir_all(&switching).unwrap();
        fs::write(
            switching.join("runtime-adoption.json"),
            r#"{"phase":"adopted"}"#,
        )
        .unwrap();
        assert_eq!(read_adoption(&ccs), "adopted");
        fs::write(
            switching.join("runtime-adoption.json"),
            r#"{"phase":"pending"}"#,
        )
        .unwrap();
        assert_eq!(read_adoption(&ccs), "unfinished");

        let problem = json!({
            "reason": "parser-mismatch",
            "module": "anthropic",
            "python": "3.12",
            "builtFor": "3.11",
            "exitStatus": null
        });
        assert_eq!(
            problem_detail(&problem),
            "Python module anthropic is not the pinned version (system Python is 3.12; the runtime bundle was built for 3.11)"
        );
        assert!(is_parser_problem(&problem));
        assert!(!is_parser_problem(
            &json!({"reason": "service-failed", "exitStatus": 3})
        ));
        assert_eq!(
            problem_detail(&json!({"reason": "service-failed", "exitStatus": 3})),
            "exit status 3"
        );

        let status = Status {
            native_cli: "missing",
            reviewed_versions: vec![],
            dashboard_gate: true,
            runtime_gate: true,
            runtime_installed: false,
            adoption: "none",
            service_socket: false,
            service_problem: None,
            profiles: vec![],
        };
        assert_eq!(
            next_step(&status),
            "Install the Antigravity CLI at ~/.local/bin/agy."
        );
    }
}
