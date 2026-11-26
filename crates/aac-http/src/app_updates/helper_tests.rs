#![cfg(test)]
use super::*;
use aac_config::{ComputerConfig, TransportConfig};
use aac_core::ComputerId;
use aac_transport::SshPolicy;
use std::path::PathBuf;

const STAND_IN: &str = "import argparse\nparser = argparse.ArgumentParser()\nparser.add_argument('--platform', required=True)\nparser.add_argument('--apply', action='store_true')\nparser.parse_args()\n";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A package with the checkout's helpers only: the TypeScript build that generated the Codex
/// runtime into `dist/app-updates` was retired, so the package has no `dist/app-updates`.
fn package(root: &Path) -> PathBuf {
    let package = root.join("package");
    let scripts = package.join("scripts/app-updates");
    std::fs::create_dir_all(&scripts).expect("package helpers");
    for name in HELPER_FILES {
        std::fs::copy(
            repo().join("scripts/app-updates").join(name),
            scripts.join(name),
        )
        .expect("copy helper");
    }
    package
}

fn output(code: i32, stdout: &str) -> ProcessOutput {
    ProcessOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

/// The fixture's own home and CCS home, both under `root`.
///
/// A helper child must never see the caller's real home: on 2026-11-22 a fixture without
/// HOME published into the machine's real `~/.ccs/app-updates`.
#[cfg(unix)]
fn fixture_homes(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let ccs = root.join("ccs-home");
    std::fs::create_dir_all(&home).expect("fixture home");
    std::fs::create_dir_all(&ccs).expect("fixture ccs home");
    (home, ccs)
}

/// A fixture child: HOME and CCS_HOME both under the test's temporary directory.
#[cfg(unix)]
fn fixture_command<S: AsRef<std::ffi::OsStr>>(root: &Path, program: S) -> std::process::Command {
    let (home, ccs) = fixture_homes(root);
    let mut command = std::process::Command::new(program);
    command.env("HOME", home).env("CCS_HOME", ccs);
    command
}

#[cfg(unix)]
#[test]
fn fixture_children_never_inherit_the_callers_home() {
    let dir = tempfile::tempdir().expect("fixture");
    let (home, ccs) = fixture_homes(dir.path());
    let output = fixture_command(dir.path(), "/usr/bin/python3")
        .args([
            "-B",
            "-c",
            "import os, pathlib; print(os.environ['HOME']); print(os.environ['CCS_HOME']); print(pathlib.Path.home())",
        ])
        .output()
        .expect("run home guard child");
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let values: Vec<&str> = text.lines().collect();
    assert_eq!(values.len(), 3, "{text}");
    for value in &values {
        assert!(
            Path::new(value).starts_with(dir.path()),
            "a fixture child must use the fixture directory, not {value}: {text}"
        );
    }
    let (home, ccs) = (home.to_string_lossy(), ccs.to_string_lossy());
    assert_eq!(
        (values[0], values[1], values[2]),
        (home.as_ref(), ccs.as_ref(), home.as_ref()),
        "{text}"
    );
    assert_ne!(
        values[0],
        std::env::var("HOME").unwrap_or_default(),
        "a fixture child inherited the caller's HOME"
    );
}

#[test]
fn payload_carries_the_script_manifest_and_files_in_order() {
    let dir = tempfile::tempdir().expect("fixture");
    let set = HelperSet::load(&package(dir.path())).expect("helper set");
    let payload = set.payload("mac");
    let mut lines = payload.splitn(3, |byte| *byte == b'\n');
    let script = STANDARD
        .decode(lines.next().expect("script"))
        .expect("base64");
    assert_eq!(script, SCRIPT.as_bytes());
    let header: Value = serde_json::from_slice(lines.next().expect("manifest")).expect("json");
    assert_eq!(header["platform"], "mac");
    let mut body = lines.next().expect("files");
    let files = header["files"].as_array().expect("files");
    assert_eq!(files.len(), HELPER_FILES.len());
    for (entry, name) in files.iter().zip(HELPER_FILES) {
        assert_eq!(entry["name"], name);
        let size = entry["size"].as_u64().expect("size") as usize;
        assert_eq!(aac_core::sha256_hex(&body[..size]), entry["sha256"]);
        body = &body[size..];
    }
    assert!(body.is_empty());

    assert!(!HELPER_FILES.contains(&"app_update_codex_runtime.cjs"));
    assert!(!dir.path().join("package/dist").exists());
    std::fs::remove_file(
        dir.path()
            .join("package/scripts/app-updates/app_update_codex.cjs"),
    )
    .expect("drop a helper");
    assert_eq!(
        HelperSet::load(&dir.path().join("package")).err(),
        Some(failed("helper_set_missing"))
    );
}

#[test]
fn replies_map_to_honest_causes() {
    assert_eq!(
        interpret(&output(0, "{\"ok\":true,\"changed\":false}\n")),
        Ok(())
    );
    for (reason, cause) in [
        ("helper_upload_damaged", "unknown"),
        ("helper_capability_missing", "unknown"),
        ("helper_not_writable", "not_writable"),
        ("helper_sync_timeout", "timeout"),
    ] {
        let reply = format!("{{\"ok\":false,\"reason\":\"{reason}\"}}\n");
        assert_eq!(
            interpret(&output(0, &reply)),
            Err(SyncError::Failed { reason, cause })
        );
    }
    assert_eq!(
        interpret(&output(0, "{\"ok\":false,\"reason\":\"PRIVATE\"}")),
        Err(failed("helper_reply_invalid"))
    );
    assert_eq!(
        interpret(&output(127, "")),
        Err(failed("helper_python_missing"))
    );
    assert_eq!(
        interpret(&output(1, "{\"ok\":true}")),
        Err(failed("helper_reply_invalid"))
    );
}

#[test]
fn windows_task_missing_is_an_honest_sync_reason() {
    assert_eq!(
        interpret(&output(
            0,
            "{\"ok\":false,\"reason\":\"windows_task_missing\"}\n"
        )),
        Err(SyncError::Failed {
            reason: "windows_task_missing",
            cause: "unknown",
        })
    );
}

#[cfg(unix)]
#[test]
fn refused_task_registration_restores_the_launcher_that_answered_before() {
    // M5: registration runs after the swap, so a refused registration (for example a host with
    // no pythonw.exe) must put the prior launcher back, or leave a fresh host without one.
    let dir = tempfile::tempdir().expect("fixture");
    let harness = r#"
import pathlib, subprocess, sys

root = pathlib.Path(sys.argv[1])
fresh = pathlib.Path(sys.argv[2])
prior = b'# prior launcher\n'
(root / 'app_updates.py').write_bytes(prior)

class Done:
    def __init__(self, code=0, stdout=''):
        self.returncode = code
        self.stdout = stdout
        self.stderr = ''

def run(args, **kwargs):
    script = args[-1] if args else ''
    if args and args[0] == 'powershell.exe' and 'Get-ScheduledTask' in script:
        return Done(1)
    if args and args[0] == 'powershell.exe' and 'Register-ScheduledTask' in script:
        return Done(1)
    return Done(0)

subprocess.run = run
subprocess.check_output = lambda args, **kwargs: 'S-1-5-21-1-2-3-1001'
source = sys.stdin.read().removesuffix('\nmain()\n')
namespace = {}
exec(source, namespace)
namespace['capable'] = lambda script, platform: True
data = b'# new helper\n'
expected = {'app_updates.py': namespace['digest'](data)}
payload = {'app_updates.py': data}
reasons = []
for folder in (root, fresh):
    try:
        namespace['publish'](folder, 'windows', expected, payload)
        reasons.append('ok')
    except namespace['Refused'] as error:
        reasons.append(error.reason)
print('reasons=%s' % ','.join(reasons))
print('prior_restored=%s' % ((root / 'app_updates.py').read_bytes() == prior))
print('fresh_absent=%s' % (not (fresh / 'app_updates.py').exists()))
"#;
    let mut command = fixture_command(dir.path(), "/usr/bin/python3");
    command
        .args(["-B", "-c", harness])
        .arg(dir.path())
        .arg(dir.path().join("fresh"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("run windows task rollback fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child
        .wait_with_output()
        .expect("helper task rollback result");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("reasons=windows_task_missing,windows_task_missing"),
        "a refused registration is helper_sync_failed for both hosts: {stdout}"
    );
    assert!(
        stdout.contains("prior_restored=True"),
        "the prior launcher must answer again: {stdout}"
    );
    assert!(
        stdout.contains("fresh_absent=True"),
        "a fresh host must be left without a launcher: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn task_registration_timeout_restores_the_launcher_that_answered_before() {
    // N1: a registration that times out (TimeoutExpired) or cannot start is as unfinished as a
    // refused one. The replacement launcher must not stay active with no task behind it.
    let dir = tempfile::tempdir().expect("fixture");
    let harness = r#"
import pathlib, subprocess, sys

root = pathlib.Path(sys.argv[1])
fresh = pathlib.Path(sys.argv[2])
prior = b'# prior launcher\n'
(root / 'app_updates.py').write_bytes(prior)

class Done:
    def __init__(self, code=0, stdout=''):
        self.returncode = code
        self.stdout = stdout
        self.stderr = ''

def run(args, **kwargs):
    script = args[-1] if args else ''
    if args and args[0] == 'powershell.exe' and 'Get-ScheduledTask' in script:
        return Done(1)
    if args and args[0] == 'powershell.exe' and 'Register-ScheduledTask' in script:
        raise subprocess.TimeoutExpired(args, kwargs.get('timeout'))
    return Done(0)

subprocess.run = run
subprocess.check_output = lambda args, **kwargs: 'S-1-5-21-1-2-3-1001'
source = sys.stdin.read().removesuffix('\nmain()\n')
namespace = {}
exec(source, namespace)
namespace['capable'] = lambda script, platform: True
data = b'# new helper\n'
expected = {'app_updates.py': namespace['digest'](data)}
payload = {'app_updates.py': data}
outcomes = []
for folder in (root, fresh):
    try:
        namespace['publish'](folder, 'windows', expected, payload)
        outcomes.append('ok')
    except subprocess.TimeoutExpired:
        outcomes.append('timeout')
    except namespace['Refused'] as error:
        outcomes.append(error.reason)
print('outcomes=%s' % ','.join(outcomes))
print('prior_restored=%s' % ((root / 'app_updates.py').read_bytes() == prior))
print('fresh_absent=%s' % (not (fresh / 'app_updates.py').exists()))
"#;
    let mut command = fixture_command(dir.path(), "/usr/bin/python3");
    command
        .args(["-B", "-c", harness])
        .arg(dir.path())
        .arg(dir.path().join("fresh"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("run windows task timeout fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child
        .wait_with_output()
        .expect("helper task timeout result");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("outcomes=timeout,timeout"),
        "a timed-out registration is reported, not swallowed: {stdout}"
    );
    assert!(
        stdout.contains("prior_restored=True"),
        "a timed-out registration must leave the prior launcher answering: {stdout}"
    );
    assert!(
        stdout.contains("fresh_absent=True"),
        "a timed-out registration must leave a fresh host without a launcher: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn registration_under_a_typographic_apostrophe_home_keeps_the_profile_path_out_of_source() {
    // M2: Windows allows U+2019 in a profile path, and Windows PowerShell treats it as a
    // single-quote delimiter. The registration command must therefore never carry the profile
    // path: the staged helper is named relative to `$HOME` and resolved inside PowerShell.
    let dir = tempfile::tempdir().expect("fixture");
    let home = dir.path().join("o\u{2019}brien");
    let ccs = dir.path().join("ccs-home");
    std::fs::create_dir_all(&home).expect("fixture home");
    std::fs::create_dir_all(&ccs).expect("fixture ccs home");
    let harness = r#"
import pathlib, subprocess, sys

home = pathlib.Path(sys.argv[1])

class Done:
    def __init__(self, code=0, stdout=''):
        self.returncode, self.stdout, self.stderr = code, stdout, ''

seen = []

def run(args, **kwargs):
    script = args[-1] if args else ''
    if args and args[0] == 'powershell.exe' and 'Get-ScheduledTask' in script:
        return Done(1)
    if args and args[0] == 'powershell.exe' and 'Register-ScheduledTask' in script:
        seen.append(script)
        return Done(0)
    return Done(0)

subprocess.run = run
subprocess.check_output = lambda args, **kwargs: 'S-1-5-21-1-2-3-1001'
source = sys.stdin.read().removesuffix('\nmain()\n')
namespace = {}
exec(source, namespace)
namespace['capable'] = lambda script, platform: True
data = b'# new helper\n'
expected = {'app_updates.py': namespace['digest'](data)}
namespace['publish'](home / '.ccs' / 'app-updates', 'windows', expected, {'app_updates.py': data})
script = seen[0]
prefix = '$present=Join-Path $HOME '
lines = [part for part in script.split(';') if part.startswith(prefix)]
print('registrations=%d' % len(seen))
print('home_in_source=%s' % (str(home) in script))
print('smart_quote_in_source=%s' % ('\u2019' in script))
print('join_line=%s' % bool(lines))
if lines:
    staged = home / lines[0][len(prefix):].strip("'")
    print('present_exists=%s' % staged.is_file())
"#;
    let mut command = std::process::Command::new("/usr/bin/python3");
    command
        .args(["-B", "-c", harness])
        .arg(&home)
        .env("HOME", &home)
        .env("CCS_HOME", &ccs)
        .env("TMPDIR", dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("run registration fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child.wait_with_output().expect("registration result");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("registrations=1"), "{stdout}");
    assert!(
        stdout.contains("home_in_source=False"),
        "the profile path must not be PowerShell source: {stdout}"
    );
    assert!(
        stdout.contains("smart_quote_in_source=False"),
        "no typographic apostrophe may reach the script: {stdout}"
    );
    assert!(
        stdout.contains("join_line=True"),
        "the staged helper is resolved under $HOME: {stdout}"
    );
    assert!(
        stdout.contains("present_exists=True"),
        "the resolved staged helper must exist: {stdout}"
    );
}

#[cfg(unix)]
/// Publish through the real script in a child process, with registration told to behave as `mode`:
/// "exit" loses the updater process at registration, "timeout" returns PowerShell's timeout.
fn interrupted_registration(mode: &str) -> String {
    let dir = tempfile::tempdir().expect("fixture");
    let harness = r#"
import base64, json, os, pathlib, subprocess, sys

mode = sys.argv[1]
prior_home = pathlib.Path(sys.argv[2])
fresh_home = pathlib.Path(sys.argv[3])
prior = b'# prior launcher\n'
root = prior_home / '.ccs' / 'app-updates'
root.mkdir(parents=True)
(root / 'app_updates.py').write_bytes(prior)
script = sys.stdin.buffer.read()
namespace = {}
exec(script.decode().removesuffix('\nmain()\n'), namespace)
files = {name: b'# fixture ' + name.encode() for name in namespace['FILES']}
header = {'platform': 'windows', 'files': [
    {'name': name, 'size': len(data), 'sha256': namespace['digest'](data)}
    for name, data in files.items()]}
payload = (base64.b64encode(script).decode() + '\n'
           + json.dumps(header, sort_keys=True) + '\n').encode()
for data in files.values():
    payload += data
action = ('os._exit(3)' if mode == 'exit'
          else "raise subprocess.TimeoutExpired(args, kwargs.get('timeout'))")
child = r"""
import base64, os, subprocess, sys

class Done:
    def __init__(self, code=0, stdout=''):
        self.returncode, self.stdout, self.stderr = code, stdout, ''

def run(args, **kwargs):
    script = args[-1] if args else ''
    if args and args[0] == 'powershell.exe' and 'Register-ScheduledTask' in script:
        {action}
    if args and args[0] == 'powershell.exe' and 'Get-ScheduledTask' in script:
        return Done(1)
    return Done(0)

subprocess.run = run
subprocess.check_output = lambda args, **kwargs: 'S-1-5-21-1-2-3-1001'
namespace = {}
exec(base64.b64decode(sys.stdin.buffer.readline()).decode().removesuffix('\nmain()\n'), namespace)
namespace['capable'] = lambda script, platform: True
namespace['main']()
""".replace('{action}', action)
launcher = pathlib.Path('.ccs/app-updates/app_updates.py')
for home in (prior_home, fresh_home):
    done = subprocess.run([sys.executable, '-B', '-c', child], input=payload,
                          capture_output=True, timeout=60,
                          env={**os.environ, 'HOME': str(home),
                               'CCS_HOME': str(home / 'ccs-home')})
    reply = done.stdout.decode().strip().splitlines()
    print('mode=%s exit=%d reply=%s' % (mode, done.returncode, reply[0] if reply else ''))
print('prior_selected=%s' % ((prior_home / launcher).read_bytes() == prior))
print('fresh_absent=%s' % (not (fresh_home / launcher).exists()))
"#;
    let mut command = fixture_command(dir.path(), "/usr/bin/python3");
    command
        .args(["-B", "-c", harness, mode])
        .arg(dir.path().join("home"))
        .arg(dir.path().join("fresh"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .expect("run registration interruption fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child
        .wait_with_output()
        .expect("registration interruption result");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout} stderr={stderr}");
    stdout.into_owned()
}

#[cfg(unix)]
#[test]
fn a_process_exit_at_registration_leaves_the_previous_launcher_selected() {
    // M6: the swap is last, so losing the updater process while Task Scheduler works changes
    // nothing on disk: the launcher that answered before still answers (absent stays absent).
    let stdout = interrupted_registration("exit");
    assert!(
        stdout.contains("mode=exit exit=3"),
        "the fixture must lose the process at registration: {stdout}"
    );
    assert!(
        stdout.contains("prior_selected=True"),
        "the previous launcher must stay selected: {stdout}"
    );
    assert!(
        stdout.contains("fresh_absent=True"),
        "a fresh host must stay without a launcher: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn a_registration_timeout_answers_the_timeout_reason_and_keeps_the_prior_launcher() {
    // L2: a timed-out registration is a timeout, not helper_not_writable, and the launcher
    // bytes are the ones that answered before.
    let stdout = interrupted_registration("timeout");
    assert!(
        stdout.contains(r#"reply={"ok": false, "reason": "helper_sync_timeout"}"#),
        "the script must answer the timeout reason: {stdout}"
    );
    assert!(
        stdout.contains("prior_selected=True"),
        "the previous launcher must stay selected: {stdout}"
    );
    assert!(
        stdout.contains("fresh_absent=True"),
        "a fresh host must stay without a launcher: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn windows_sync_registers_the_task_the_coordinator_starts() {
    // The registered task is the one the coordinator's Windows task start dispatches; the row
    // comes from that dispatch, not from the publication reply.
    let dir = tempfile::tempdir().expect("fixture");
    let home = dir.path().join("home");
    let harness = r#"
import json, pathlib, subprocess, sys

root = pathlib.Path(sys.argv[1])
home = pathlib.Path(sys.argv[2])
source_dir = pathlib.Path(sys.argv[3])
home.mkdir(parents=True, exist_ok=True)
registers = []

class Done:
    def __init__(self, code=0, stdout=''):
        self.returncode = code
        self.stdout = stdout
        self.stderr = ''

def run(args, **kwargs):
    script = args[-1] if args else ''
    if args and args[0] == 'powershell.exe' and 'Register-ScheduledTask' in script:
        registers.append(script)
        return Done(0)
    if args and args[0] == 'powershell.exe' and 'Get-ScheduledTask' in script:
        return Done(1)
    return Done(0)

subprocess.run = run
subprocess.check_output = lambda args, **kwargs: 'S-1-5-21-1-2-3-1001'
source = sys.stdin.read().removesuffix('\nmain()\n')
sync = {}
exec(source, sync)
sync['capable'] = lambda script, platform: True
payload = {path.name: path.read_bytes() for path in sorted(source_dir.glob('*.py'))}
expected = {name: sync['digest'](data) for name, data in payload.items()}
result = sync['publish'](root, 'windows', expected, payload)
started = []

def powershell(script, timeout=15):
    folder = home / '.ccs/app-updates'
    if 'Start-ScheduledTask' in script:
        started.append(script)
        nonce = json.loads((folder / 'windows-task-request.json').read_text())['nonce']
        row = {'appId': 'codex-cli', 'platform': 'windows', 'status': 'updated', 'before': '1.0.0',
               'after': '2.0.0', 'restartedProcesses': 0, 'updateAttempted': True}
        (folder / 'windows-task-result.json').write_text(json.dumps({'nonce': nonce, 'results': [row]}))
        return ''
    if 'LastRunTime' in script:
        return '0'
    raise AssertionError('unexpected task command: ' + script)

generation = root / result['generation']
sys.path.insert(0, str(generation))
updater = {'__name__': 'fixture', '__file__': str(generation / 'app_updates.py')}
exec(compile((generation / 'app_updates.py').read_text(), 'app_updates.py', 'exec'), updater)
updater['powershell'] = powershell
answer = updater['windows_interactive_apply'](lambda event: None, lambda: False, None, 'codex-cli', 60)
row = answer['results'][0]
print('ok=%s' % result.get('ok'))
print('registers=%d' % len(registers))
print('started=%d' % len(started))
print('row=%s' % row['status'])
"#;
    let mut command = fixture_command(dir.path(), "/usr/bin/python3");
    command
        .args(["-B", "-c", harness])
        .arg(dir.path())
        .arg(&home)
        .arg(repo().join("scripts/app-updates"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("run windows task start fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child.wait_with_output().expect("helper task start result");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(result.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("registers=1"),
        "the sync registers the missing task: {stdout}"
    );
    assert!(
        stdout.contains("started=1"),
        "the coordinator starts the registered task: {stdout}"
    );
    assert!(
        stdout.contains("row=updated"),
        "the first update row comes from the task start: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn windows_flat_helper_backup_gets_access_before_publication() {
    let dir = tempfile::tempdir().expect("fixture");
    let harness = r#"
import pathlib, subprocess, sys

root = pathlib.Path(sys.argv[1])
launcher = root / 'app_updates.py'
launcher.write_bytes(b'# flat helper\n')

def check_output(args, **kwargs):
    assert args[0] == 'powershell.exe'
    return 'S-1-5-21-1-2-3-1001'

def run(args, **kwargs):
    if args[0] == 'powershell.exe':
        class Done:
            returncode = 0
            stdout = ''
            stderr = ''
        return Done()
    assert args[0] == 'icacls.exe'
    folder = pathlib.Path(args[1])
    assert launcher.read_bytes() == b'# flat helper\n'
    (folder / 'acl-granted').touch()

subprocess.check_output = check_output
subprocess.run = run
source = sys.stdin.read().removesuffix('\nmain()\n')
namespace = {}
exec(source.replace("if os.name == 'nt':", 'if True:'), namespace)
namespace['capable'] = lambda script, platform: True
data = b'# new helper\n'
result = namespace['publish'](root, 'windows', {'app_updates.py': namespace['digest'](data)},
                              {'app_updates.py': data})
backup = root / result['previous']
assert (backup / 'app_updates.py').read_bytes() == b'# flat helper\n'
assert (root / result['generation'] / 'acl-granted').is_file(), 'new helper ACL missing'
assert (backup / 'acl-granted').is_file(), 'rollback helper ACL missing'
"#;
    let mut command = fixture_command(dir.path(), "/usr/bin/python3");
    command
        .args(["-B", "-c", harness])
        .arg(dir.path())
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("run helper ACL fixture");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(SCRIPT.as_bytes())
        .expect("helper script");
    let result = child.wait_with_output().expect("helper ACL result");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// The real script on this computer in an isolated home, through the local transport the
/// dashboard uses for its own computer. A child process owns the isolated HOME.
#[cfg(unix)]
#[test]
fn local_sync_publishes_a_capable_generation_and_keeps_the_previous_one() {
    let dir = tempfile::tempdir().expect("fixture");
    let root = dir.path().canonicalize().expect("canonical root");
    fixture_homes(&root);
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "app_updates::helpers::tests::local_sync_child",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("CCS_HOME", root.join("ccs-home"))
        .env("AAC_TEST_HELPER_SYNC_ROOT", &root);
    for name in ["AAC_TEST_PACKAGED_AGENT", "AAC_TEST_WAVE84_HELPERS"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let child = command.output().expect("run isolated child");
    print!("{}", String::from_utf8_lossy(&child.stdout));
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
}

#[cfg(unix)]
fn native() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "ubuntu"
    }
}

#[cfg(unix)]
fn probe(root: &Path, launcher: &Path) -> bool {
    fixture_command(root, "/usr/bin/python3")
        .arg("-B")
        .arg(launcher)
        .args(["--platform", native(), "--agent-capabilities"])
        .output()
        .is_ok_and(|done| {
            done.status.success()
                && serde_json::from_slice::<Value>(
                    done.stdout
                        .split(|b| *b == b'\n')
                        .next()
                        .unwrap_or_default(),
                )
                .ok()
                    == Some(json!({"appWorker":1}))
        })
}

#[cfg(unix)]
fn generations(helpers: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(helpers)
        .expect("helper root")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("helpers-") || name.starts_with(".helpers-stage-"))
        .collect();
    names.sort();
    names
}

#[cfg(unix)]
fn launcher_line(helpers: &Path, prefix: &str) -> String {
    std::fs::read_to_string(helpers.join("app_updates.py"))
        .expect("launcher")
        .lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::to_owned))
        .expect("launcher line")
}

/// The Wave 84 layout: a format-2 launcher naming an immutable generation. Its helper exits 2
/// on the capability probe, as the exact Wave 84 helper did. AAC_TEST_WAVE84_HELPERS supplies
/// the real Wave 84 files instead of the stand-in.
#[cfg(unix)]
fn seed_wave84(helpers: &Path) -> String {
    let generation = format!("helpers-{}", "8".repeat(32));
    let folder = helpers.join(&generation);
    std::fs::create_dir_all(&folder).expect("Wave 84 generation");
    match std::env::var_os("AAC_TEST_WAVE84_HELPERS") {
        Some(source) => {
            for name in HELPER_FILES {
                std::fs::copy(Path::new(&source).join(name), folder.join(name))
                    .expect("copy Wave 84 helper");
            }
        }
        None => {
            for name in HELPER_FILES {
                std::fs::write(folder.join(name), "# Wave 84 stand-in\n").expect("stand-in");
            }
            std::fs::write(folder.join("app_updates.py"), STAND_IN).expect("stand-in helper");
        }
    }
    std::fs::write(
        helpers.join("app_updates.py"),
        format!("# aac-helpers: {generation}\n# aac-helpers-format: 2\nimport pathlib, runpy, sys\nroot = pathlib.Path(__file__).resolve().parent / {generation:?}\nsys.path.insert(0, str(root))\nrunpy.run_path(str(root / \"app_updates.py\"), run_name=\"__main__\")\n"),
    )
    .expect("Wave 84 launcher");
    generation
}

/// The packaged agent's own decision. A Python stand-in answers each installer with
/// `not_installed`, so no real installer runs; the capability probe runs the synced helper.
#[cfg(target_os = "linux")]
fn agent(root: &Path, home: &Path) -> Option<Value> {
    use std::io::{BufRead, Write};
    let binary = std::env::var_os("AAC_TEST_PACKAGED_AGENT")?;
    let python = root.join("python-stand-in");
    if !python.exists() {
        std::fs::write(
            &python,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = --agent-capabilities ] && exec /usr/bin/python3 \"$@\"; done\napp=; last=\nfor arg in \"$@\"; do [ \"$last\" = --app ] && app=$arg; last=$arg; done\nprintf '{\"results\":[{\"appId\":\"%s\",\"platform\":\"ubuntu\",\"status\":\"not_installed\",\"messageCode\":\"not_installed\"}]}\\n' \"$app\"\n",
        )
        .expect("python stand-in");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o700))
            .expect("stand-in mode");
    }
    let mut child = std::process::Command::new(binary)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("CCS_HOME", root.join("ccs-home"))
        .env("AAC_APP_UPDATE_PYTHON", &python)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start packaged agent");
    // A fresh job each time: a repeated job id would only replay the saved journal.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let job =
        aac_core::sha256_hex(format!("{}:{call}", home.display()).as_bytes())[..32].to_owned();
    let request = json!({"protocol":1,"verb":"apps","id":job,"deadlineMs":120000,
        "args":{"action":"update","platform":"ubuntu","jobId":job}});
    writeln!(child.stdin.take().expect("stdin"), "{request}").expect("request");
    let reader = std::io::BufReader::new(child.stdout.take().expect("stdout"));
    let result = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|event| event["type"] == "result" || event["type"] == "failure")
        .expect("agent terminal event");
    child.wait().expect("reap agent");
    Some(result["value"].clone())
}

#[cfg(not(target_os = "linux"))]
fn agent(_: &Path, _: &Path) -> Option<Value> {
    None
}

#[cfg(unix)]
fn assert_agent(root: &Path, home: &Path, capable: bool) {
    let Some(value) = agent(root, home) else {
        println!("packaged agent not supplied; the launcher probe stands for the agent's");
        return;
    };
    if capable {
        let rows = value["results"].as_array().expect("agent rows");
        assert_eq!(rows.len(), 12, "{value}");
        assert!(
            rows.iter().all(|row| row["status"] == "not_installed"),
            "{value}"
        );
        println!("packaged agent: {} rows after sync", rows.len());
    } else {
        assert_eq!(value, json!({"fallback":"python"}));
        println!("packaged agent: fallback before sync");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn local_sync_child() {
    let Some(root) = std::env::var_os("AAC_TEST_HELPER_SYNC_ROOT").map(PathBuf::from) else {
        return;
    };
    let set = HelperSet::load(&package(&root)).expect("helper set");
    let computer = Computer::new(
        ComputerConfig {
            id: ComputerId::new("ubuntu").expect("id"),
            label: "Fixture".into(),
            os: if native() == "mac" {
                OperatingSystem::Mac
            } else {
                OperatingSystem::Linux
            },
            transport: TransportConfig::Local,
            roles: Vec::new(),
            metadata: Default::default(),
        },
        SshPolicy::new(root.join("known_hosts"), root.join("run")),
    )
    .expect("local computer");
    let cancel = CancellationToken::new();
    let home = root.join("home");
    let helpers = home.join(".ccs/app-updates");

    // Absent helper: the agent falls back, the sync publishes, the probe and agent pass.
    assert_agent(&root, &home, false);
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert!(probe(&root, &helpers.join("app_updates.py")));
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), "none");
    assert_agent(&root, &home, true);
    let first = generations(&helpers);
    assert_eq!(first.len(), 1, "{first:?}");
    let launcher = std::fs::read(helpers.join("app_updates.py")).expect("launcher");

    // A current computer pays one round trip and changes nothing.
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert_eq!(generations(&helpers), first);
    assert_eq!(
        std::fs::read(helpers.join("app_updates.py")).expect("same"),
        launcher
    );

    // A damaged upload is refused before anything is staged or swapped.
    let mut damaged = set.payload(native());
    let last = damaged.len() - 3;
    damaged[last] ^= 0x20;
    let options = CallOptions::new(Duration::from_secs(60), 64 * 1024);
    let reply = computer
        .execute(&argv(computer.config().os), &damaged, &options)
        .await
        .expect("damaged upload reply");
    assert_eq!(interpret(&reply), Err(failed("helper_upload_damaged")));
    assert_eq!(generations(&helpers), first);
    assert_eq!(
        std::fs::read(helpers.join("app_updates.py")).expect("kept"),
        launcher
    );

    // The Wave 84 helper: refused by the agent until the sync, then rollback stays possible.
    std::fs::remove_dir_all(&helpers).expect("reset helpers");
    std::fs::create_dir_all(&helpers).expect("helpers");
    let wave84 = seed_wave84(&helpers);
    let before: Vec<_> = HELPER_FILES
        .iter()
        .map(|name| std::fs::read(helpers.join(&wave84).join(name)).expect("Wave 84 bytes"))
        .collect();
    assert!(!probe(&root, &helpers.join("app_updates.py")));
    assert_agent(&root, &home, false);
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert!(probe(&root, &helpers.join("app_updates.py")));
    assert_agent(&root, &home, true);
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), wave84);
    let current = launcher_line(&helpers, "# aac-helpers: ");
    assert_ne!(current, wave84);
    assert_eq!(generations(&helpers), {
        let mut both = vec![current.clone(), wave84.clone()];
        both.sort();
        both
    });
    for (name, bytes) in HELPER_FILES.iter().zip(&before) {
        assert_eq!(
            &std::fs::read(helpers.join(&wave84).join(name)).expect("kept"),
            bytes
        );
    }
    // The documented rollback selects the previous generation again.
    let rollback = fixture_command(&root, "/usr/bin/python3")
        .args(["-c", include_str!("helper_rollback.py")])
        .output()
        .expect("rollback");
    assert!(rollback.status.success(), "{rollback:?}");
    assert_eq!(launcher_line(&helpers, "# aac-helpers: "), wave84);
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), current);
    assert!(!probe(&root, &helpers.join("app_updates.py")));

    // Flat helpers from before generations are kept as a generation too.
    std::fs::remove_dir_all(&helpers).expect("reset helpers");
    std::fs::create_dir_all(&helpers).expect("helpers");
    std::fs::write(helpers.join("app_updates.py"), STAND_IN).expect("flat helper");
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    let previous = launcher_line(&helpers, "# aac-helpers-previous: ");
    assert_eq!(
        std::fs::read_to_string(helpers.join(&previous).join("app_updates.py")).expect("flat"),
        STAND_IN
    );
    assert!(probe(&root, &helpers.join("app_updates.py")));
}
