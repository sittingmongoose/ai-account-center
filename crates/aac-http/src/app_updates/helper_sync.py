# AAC Update apps helper sync. The dashboard sends this source as the first stdin line, run by
# the account that runs the agent. The second line is the manifest, then each file's bytes.
# It publishes one immutable generation, then atomically replaces only the launcher that the
# agent runs (~/.ccs/app-updates/app_updates.py). The previous generation stays for rollback.
import hashlib, json, os, pathlib, re, shutil, subprocess, sys, tempfile, uuid

FILES = (
    'app_updates.py', 'app_update_common.py', 'app_update_desktop.py', 'app_update_t3.py',
    'app_update_t3_rpc.py', 'app_update_cursor.py', 'app_update_antigravity.py',
    'app_update_antigravity_release.py', 'app_update_macos.py', 'app_update_processes.py',
    'app_update_terminal.py', 'app_update_terminal_child.py', 'app_update_pipe.py',
    'app_update_confirmed_codex.py', 'app_update_zcode.py',
    'app_update_codex.cjs',
)
LIMIT = 16 * 1024 * 1024
GENERATION = re.compile(r'helpers-[a-f0-9]{32}')
FORMAT = '# aac-helpers-format: 2'
HIDDEN = {'creationflags': 0x08000000} if os.name == 'nt' else {}
# Same InteractiveToken/Limited hidden task as scripts/app-updates/install-windows-task.ps1.
# The sync script is the only file the Windows host runs before helpers exist, so the body lives here.
WINDOWS_TASK_REGISTER = (
    "$ErrorActionPreference='Stop';"
    "$helper=Join-Path $HOME '.ccs\\app-updates\\app_updates.py';"
    "$present=Join-Path $HOME %s;"
    "if (-not (Test-Path -LiteralPath $present -PathType Leaf)) { throw 'Updater helper is not deployed.' };"
    "$python=(Get-Command python.exe -ErrorAction Stop).Source;"
    "$pythonw=Join-Path (Split-Path -Parent $python) 'pythonw.exe';"
    "if (-not (Test-Path -LiteralPath $pythonw -PathType Leaf)) { throw 'pythonw.exe was not found' };"
    "$arguments='\"'+$helper+'\" --apply --platform windows --task-child';"
    "$action=New-ScheduledTaskAction -Execute $pythonw -Argument $arguments;"
    "$principal=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) "
    "-LogonType Interactive -RunLevel Limited;"
    "$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 35) "
    "-AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -Hidden;"
    "Register-ScheduledTask -TaskName 'CCS App Updates' -Action $action -Principal $principal "
    "-Settings $settings -Force | Out-Null"
)


def powershell_literal(value):
    """One PowerShell single-quoted string: no escape processing in it, a quote written twice."""
    return "'" + str(value).replace("'", "''") + "'"


def home_relative(path):
    """A path under the user's home, relative and forward-slashed.

    The registration resolves it under PowerShell's `$HOME`, so the profile's own characters
    never enter the command's source: Windows permits U+2019 in a profile path, and Windows
    PowerShell treats it as a single-quote delimiter (ASCII doubling cannot escape it).
    """
    return pathlib.Path(os.path.relpath(path, pathlib.Path.home())).as_posix()


class Refused(Exception):
    def __init__(self, reason):
        super().__init__(reason)
        self.reason = reason


def answer(**fields):
    sys.stdout.write(json.dumps(fields, sort_keys=True) + '\n')
    sys.stdout.flush()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def receive(stream):
    """Read the whole manifest and every file; any short or altered file refuses the set."""
    try:
        header = json.loads(stream.readline(65536))
        platform, entries = header['platform'], header['files']
        names = [entry['name'] for entry in entries]
    except (ValueError, KeyError, TypeError):
        raise Refused('helper_upload_damaged')
    if platform not in ('ubuntu', 'mac', 'windows') or names != list(FILES):
        raise Refused('helper_upload_damaged')
    expected, payload = {}, {}
    for entry in entries:
        size, want = entry.get('size'), entry.get('sha256')
        if (not isinstance(size, int) or isinstance(size, bool) or not 0 <= size <= LIMIT
                or not isinstance(want, str) or not re.fullmatch('[a-f0-9]{64}', want)):
            raise Refused('helper_upload_damaged')
        data = stream.read(size)
        if len(data) != size or digest(data) != want:
            raise Refused('helper_upload_damaged')
        expected[entry['name']], payload[entry['name']] = want, data
    if stream.read(1):
        raise Refused('helper_upload_damaged')
    return platform, expected, payload


def selected(root):
    """The launcher's bytes and the generation it names (None for flat or absent helpers)."""
    try:
        text = (root / 'app_updates.py').read_bytes()
    except FileNotFoundError:
        return None, None, False
    lines = text.decode('utf-8', 'replace').splitlines()
    first = lines[0].strip() if lines else ''
    name = first[len('# aac-helpers: '):] if first.startswith('# aac-helpers: ') else ''
    marked = len(lines) > 1 and lines[1].strip() == FORMAT
    return text, (name if GENERATION.fullmatch(name) else None), marked


def matches(folder, expected):
    try:
        return all(digest((folder / name).read_bytes()) == want for name, want in expected.items())
    except OSError:
        return False


def capable(script, platform):
    """The agent's own probe: exit 0 and the single-app worker capability on the first line."""
    try:
        done = subprocess.run(
            [sys.executable, '-B', str(script), '--platform', platform, '--agent-capabilities'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=30, **HIDDEN)
        lines = done.stdout.splitlines()
        return done.returncode == 0 and bool(lines) and json.loads(lines[0]) == {'appWorker': 1}
    except (OSError, ValueError, subprocess.SubprocessError):
        return False


def run_powershell(script):
    return subprocess.run(
        ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        timeout=30, **HIDDEN)


def ensure_windows_task(present):
    """Register CCS App Updates when a Windows host has helpers but no task.

    `present` is the helper this publication is about to select; the task's action
    always names the stable launcher, so registering first is harmless and keeps
    an interrupted publication from leaving a task without its launcher.
    """
    current = run_powershell("Get-ScheduledTask -TaskName 'CCS App Updates' -ErrorAction Stop")
    if current.returncode == 0:
        return
    done = run_powershell(WINDOWS_TASK_REGISTER % powershell_literal(home_relative(present)))
    if done.returncode != 0:
        raise Refused('windows_task_missing')


def grant(folder):
    """Python's private Windows folders belong to an elevated owner; admit the real user."""
    sid = subprocess.check_output(
        ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command',
         '[Security.Principal.WindowsIdentity]::GetCurrent().User.Value'],
        stdin=subprocess.DEVNULL, text=True, encoding='ascii', timeout=15, **HIDDEN).strip()
    if not re.fullmatch(r'S-1-\d+(?:-\d+)+', sid):
        raise Refused('helper_not_writable')
    subprocess.run(['icacls.exe', str(folder), '/grant:r', '*' + sid + ':(OI)(CI)F'], check=True,
                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, timeout=15, **HIDDEN)


def replace(root, data):
    temporary = root / ('.launcher-' + uuid.uuid4().hex)
    try:
        temporary.write_bytes(data)
        temporary.chmod(0o600)
        os.replace(temporary, root / 'app_updates.py')
    finally:
        temporary.unlink(missing_ok=True)


def restore(root, launcher):
    """Put the launcher that answered before back; an absent one stays absent."""
    if launcher is None:
        (root / 'app_updates.py').unlink(missing_ok=True)
    else:
        replace(root, launcher)


def publish(root, platform, expected, payload):
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if os.name == 'nt':
        grant(root)
    launcher, current, marked = selected(root)
    if (current and (marked or os.name != 'nt') and matches(root / current, expected)
            and capable(root / 'app_updates.py', platform)):
        if platform == 'windows':
            ensure_windows_task(root / 'app_updates.py')
        return {'ok': True, 'changed': False, 'generation': current}
    previous = current
    if launcher is not None and current is None:
        # Flat helpers from before generations become a generation, so rollback can select them.
        previous = 'helpers-' + uuid.uuid4().hex
        keep = pathlib.Path(tempfile.mkdtemp(prefix='.helpers-stage-', dir=root))
        try:
            if os.name == 'nt':
                grant(keep)
            for name in FILES:
                if (root / name).is_file():
                    shutil.copy2(root / name, keep / name)
            os.rename(keep, root / previous)
        finally:
            shutil.rmtree(keep, ignore_errors=True)
    generation = 'helpers-' + uuid.uuid4().hex
    stage = pathlib.Path(tempfile.mkdtemp(prefix='.helpers-stage-', dir=root))
    try:
        if os.name == 'nt':
            grant(stage)
        for name, data in payload.items():
            (stage / name).write_bytes(data)
        if not matches(stage, expected):
            raise Refused('helper_upload_damaged')
        if not capable(stage / 'app_updates.py', platform):
            raise Refused('helper_capability_missing')
        os.rename(stage, root / generation)
    finally:
        shutil.rmtree(stage, ignore_errors=True)
    if platform == 'windows':
        # Registration runs before the swap: the task action names the stable launcher, and
        # this publication's staged generation is what the swap will select, so an
        # interrupted, refused or timed-out registration leaves the prior state selected.
        ensure_windows_task(root / generation / 'app_updates.py')
    replace(root, (
        '# aac-helpers: ' + generation + '\n' + FORMAT + '\n'
        + '# aac-helpers-previous: ' + (previous or 'none') + '\n'
        + 'import pathlib, runpy, sys\n'
        + 'root = pathlib.Path(__file__).resolve().parent / ' + repr(generation) + '\n'
        + 'sys.path.insert(0, str(root))\n'
        + 'runpy.run_path(str(root / "app_updates.py"), run_name="__main__")\n'
    ).encode('utf-8'))
    if not capable(root / 'app_updates.py', platform):
        # The launcher that answered before stays in force; an absent one stays absent.
        restore(root, launcher)
        raise Refused('helper_capability_missing')
    return {'ok': True, 'changed': True, 'generation': generation, 'previous': previous}


def main():
    try:
        try:
            platform, expected, payload = receive(sys.stdin.buffer)
        except Refused:
            # Drain the rest so the refusal, not a closed pipe, reaches the dashboard.
            while sys.stdin.buffer.read(65536):
                pass
            raise
        answer(**publish(pathlib.Path.home() / '.ccs' / 'app-updates', platform, expected, payload))
    except Refused as error:
        answer(ok=False, reason=error.reason)
    except subprocess.TimeoutExpired:
        # A timed-out step (task registration) is not a write-permission problem: the
        # prior launcher still answers, and the row says why this publication stopped.
        answer(ok=False, reason='helper_sync_timeout')
    except (OSError, subprocess.SubprocessError):
        answer(ok=False, reason='helper_not_writable')


main()
