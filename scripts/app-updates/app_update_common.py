"""Shared bounded operations for the fixed installed-app updater."""

import contextlib
import dataclasses
import json
import os
import pathlib
import re
import shutil
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

APP_LABELS = {
    "antigravity-cli": "Antigravity CLI", "muse-code": "Muse Code", "omp": "OMP",
    "codex-cli": "Codex CLI", "codex-desktop": "Codex Desktop",
    "claude-code": "Claude Code", "claude-desktop": "Claude Desktop", "zcode": "ZCode",
    "t3-acp-adapters": "T3 ACP adapters", "t3-code": "T3 Code",
    "cursor-cli": "Cursor CLI", "antigravity-acp": "Antigravity ACP (T3)",
}
NO_AUTO_UPDATE = {
    "MUSE_NO_AUTO_UPDATE": "1", "AGY_CLI_DISABLE_AUTO_UPDATE": "true",
    "DISABLE_AUTOUPDATER": "1", "NO_UPDATE_NOTIFIER": "1", "CODEX_NON_INTERACTIVE": "1",
}


class UpdateFailure(Exception):
    def __init__(self, code="update_failed", output="", exit_code=None, cause=None, reason=None):
        super().__init__(code)
        self.code = code
        # Diagnostic text for in-memory classification only; it is never emitted as is
        # (a row carries only scrub()'s bounded tail of a command's stderr).
        self.output = output
        # The exit code of a command that ran to completion and failed (an installer's HRESULT, for example).
        self.exit_code = exit_code
        # A fixed cause class (CAUSES) and a fixed reason word, when the raiser knows better than the code.
        self.cause = cause
        self.reason = reason


# ---------------------------------------------------------------------------
# Diagnosable outcomes. Every row carries a fixed cause class and, where they
# exist, the command that decided it, its exit code, phase times, the install
# owner it used and a short stderr tail with secrets scrubbed. Nothing here is
# free text from a caller: words are fixed, paths are home-relative, and the
# tail passes scrub() twice (here and at the dashboard boundary).
# ---------------------------------------------------------------------------

CAUSES = (
    "none",                # the row finished as it says (updated, current, not installed, skipped)
    "shadowed",            # another copy comes first on PATH, so the one updated is not the one that runs
    "auth_required",       # the app's own updater was refused for its sign-in (Cursor's HTTP 401)
    "signin_needed",       # the update needs, or left, a sign-in only the user can give
    "policy_blocked",      # an execution policy refused the app's launcher
    "wrong_prefix",        # the package manager changed a copy nothing runs
    "not_writable",        # the copy in use belongs to another user or a system folder
    "lost_session_reply",  # T3 issued a session but its reply never arrived
    "t3_absent",           # T3 is not installed, or has never started, on this computer
    "t3_not_ready",        # T3 is installed but its server did not answer in time
    "t3_restart_pending",  # T3 was updated in this run; its new server starts after the job
    "installer_failed",    # the supported installer ran and failed
    "in_use",              # running copies keep the files; nothing changed
    "timeout",             # a bounded step ran out of time
    "unknown",             # anything else; reason names the fixed step
)
REASON = re.compile(r"^[a-z0-9][a-z0-9_.:-]{0,63}$")
TAIL_LINES, TAIL_LINE_CHARS, TAIL_CHARS = 12, 200, 1200
# Every pattern runs on at most SCRUB_LINES lines cut to SCRUB_LINE_CHARS, and every repeat
# below is bounded, so scrubbing stays linear in the text (an unbroken 64 KiB run included).
SCRUB_LINES, SCRUB_LINE_CHARS = 48, 600
_ANSI = re.compile(r"\x1b\[[0-9;?]{0,32}[ -/]{0,8}[@-~]|\x1b\][^\x07\x1b\n]{0,512}(?:\x07|\x1b\\)?")
_CREDENTIAL_WORD = (r"(?:token|secret|password|passwd|passphrase|apikey|api[_-]key|authorization|credential"
                    r"|session[_-]?id|bearer|access[_-]?key|private[_-]?key|client[_-]?secret|cookie)")
_SECRET_PATTERNS = (
    # Whole credential-carrying headers and config lines: everything after the marker goes,
    # so a value with ';', spaces or several cookies cannot keep a tail.
    (re.compile(r"(?im)\b((?:set-)?cookie|(?:proxy-)?authorization|x-api-key|x-auth-token|private-token)\s*:.{0,600}$"), r"\1: <redacted>"),
    (re.compile(r"(?im)((?://[^\s=]{0,200}?:)?_(?:auth|authtoken|password)\b)\s*=.{0,600}$"), r"\1=<redacted>"),
    (re.compile(r"(?im)^.{0,600}PRIVATE KEY.{0,600}$"), "<redacted>"),
    (re.compile(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{1,600}"), r"\1 <redacted>"),
    # key=value and key: value forms, JSON included: the rest of the line goes.
    (re.compile(r"(?im)([\"']?\b[a-z0-9_.-]{0,40}" + _CREDENTIAL_WORD + r"[a-z0-9_.-]{0,40}[\"']?\s*[:=]\s*)\S.{0,600}$"), r"\1<redacted>"),
    (re.compile(r"(?im)(\bpass(?:word|wd|phrase)\b[^:=\n]{0,40}[:=]\s*)\S.{0,600}$"), r"\1<redacted>"),
    # Command-line flag forms: --password value, --token=value.
    (re.compile(r"(?i)(--?(?:password|passwd|pass|token|api-?key|auth(?:-?token)?|secret|access-?token|otp)\b)(?:\s+|=)(?!-)\S{1,600}"), r"\1 <redacted>"),
    (re.compile(r"(?i)\bcode=\S{1,600}"), "code=<redacted>"),
    (re.compile(r"(?i)(one[- ]time (?:code|password)\D{0,20})\d{4,10}"), r"\1<redacted>"),
    (re.compile(r"\beyJ[A-Za-z0-9_-]{8,600}\.[A-Za-z0-9_-]{8,600}(?:\.[A-Za-z0-9_-]{1,600})?"), "<redacted>"),
    (re.compile(r"(?i)\b(?:npm_|ghp_|gho_|ghs_|ghu_|github_pat_|sk-|xox[abpr]-|aiza)[A-Za-z0-9_-]{8,600}"), "<redacted>"),
    (re.compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"), "<redacted>"),
    (re.compile(r"(?i)\b(https?|wss?)://[^/\s@]{1,200}@"), r"\1://"),
    (re.compile(r"(?i)\b((?:https?|wss?)://[^\s?#\"']{1,600})[?#][^\s\"']{0,600}"), r"\1"),
    (re.compile(r"[\w.+-]{1,64}@[A-Za-z0-9-]{1,63}(?:\.[A-Za-z0-9-]{1,63}){0,8}\.[A-Za-z]{2,24}\b"), "<email>"),
    # user@host without a domain; npm's name@tag and @scope/name forms survive.
    (re.compile(r"(?<![\w/@.-])[A-Za-z][\w.+-]{0,63}@(?!(?:latest|next|beta|alpha|canary|rc|stable|dev|legacy|lts|insiders|nightly)\b)[A-Za-z][\w-]{0,62}\b(?![./@])"), "<user>@<host>"),
    (re.compile(r"\b[0-9a-fA-F]{32,600}\b"), "<redacted>"),
    (re.compile(r"[A-Za-z0-9+_=-]{40,600}"), "<redacted>"),
)
# A line naming any credential marker goes whole, with the lines that continue it (indented, or
# after a line ending in a separator), before any cut: no partial redaction can keep a tail.
CREDENTIAL_LINE = "<line removed: may contain a credential>"
_CREDENTIAL_MARKER = re.compile(r"(?i)auth|token|passw(?:or)?d|passphrase|pwd|secret|cookie|bearer|api[_-]?key"
                                r"|credential|session|private[\s_-]?key")
_CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]")
_INDENTED = re.compile(r"[\s\x00-\x1f\x7f-\x9f]")
_CONTINUES = ("\\", ",", ";", ":", "=", "{", "[", "(")


def _drop_credential_lines(lines):
    """Drop credential carriers and continuations before taking any diagnostic tail."""
    kept = []
    removing = carry = False
    for line in lines:
        probe = _CONTROL.sub("", line)
        continued = removing and (carry or bool(_INDENTED.match(line)))
        if continued or _CREDENTIAL_MARKER.search(probe):
            if not removing:
                kept.append(CREDENTIAL_LINE)
            removing = True
            carry = probe.rstrip().endswith(_CONTINUES)
            continue
        removing = carry = False
        kept.append(line)
    return kept


# Runs with '/' (base64 secrets) are redacted only when they look random, never ordinary paths.
_SLASHED = re.compile(r"(?<![\w/+=-])[A-Za-z0-9+/=_-]{32,600}(?![\w/+=-])")


def _random_looking(match):
    text = match.group(0)
    upper = sum(char.isupper() for char in text)
    if any(char.isdigit() for char in text) and any(char.islower() for char in text) and upper >= len(text) * 0.15:
        return "<redacted>"
    return text


def _home_forms():
    home = str(pathlib.Path.home())
    forms = {home, home.replace("\\", "/"), home.replace("\\", "\\\\"), home.replace("/", "\\")}
    return sorted((form for form in forms if len(form) > 2), key=len, reverse=True)


def _scrub_paths(text):
    # Case-insensitive (Windows tools print c:\users\... as often as C:\Users\...), and only a whole folder name.
    for form in _home_forms():
        text = re.sub(re.escape(form) + r"(?=[\\/\s\"':;,)]|$)", "~", text, flags=re.IGNORECASE)
    # Another user's folder: everything up to the next separator, so a name with a space goes whole.
    text = re.sub(r"(?i)([A-Z]:(?:\\\\|\\|/){1,4}Users(?:\\\\|\\|/){1,4})[^\\/\"'\n]{1,128}", r"\1<user>", text)
    text = re.sub(r"(/Users/)[^/\"'\n]{1,128}", r"\1<user>", text)
    return re.sub(r"(/home/)[^/\s\"']{1,128}", r"\1<user>", text)


def display_path(path):
    """A path with the home folder as ~ and any other user's folder as <user>; bounded."""
    return _scrub_paths(str(path)[-1024:])[-160:]


def scrub(text, lines=TAIL_LINES):
    """The bounded tail of command output with credentials, people and private paths removed."""
    if not isinstance(text, str) or not text:
        return ""
    raw = (_ANSI.sub("", line) for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"))
    candidates = [line[:SCRUB_LINE_CHARS] for line in _drop_credential_lines(raw) if line.strip()]
    text = "\n".join(candidates[-SCRUB_LINES:])
    text = "".join(char if char in "\n\t" or 32 <= ord(char) < 127 or ord(char) > 159 else " " for char in text)
    text = _scrub_paths(text)
    for pattern, replacement in _SECRET_PATTERNS:
        text = pattern.sub(replacement, text)
    text = _SLASHED.sub(_random_looking, text)
    kept = [line.rstrip()[:TAIL_LINE_CHARS] for line in text.split("\n") if line.strip()][-lines:]
    return "\n".join(kept)[-TAIL_CHARS:]


def command_display(argv, label=None):
    """What ran, for a row: the program's own name and its fixed arguments, never a script body."""
    if label:
        return scrub(label, 1)[:160]
    words = []
    skip = False
    for index, arg in enumerate(str(item) for item in argv):
        if skip:
            words.append("<script>")
            skip = False
            continue
        if index == 0 or ("/" in arg or "\\" in arg) and len(arg) > 3:
            arg = re.split(r"[\\/]", arg)[-1] if index == 0 else display_path(arg)
        if arg.lower() in ("-encodedcommand", "-command", "-c") and index > 0:
            skip = True
        words.append(arg if len(arg) <= 60 else arg[:57] + "...")
    return scrub(" ".join(words), 1)[:160]


class Diagnostics:
    """One row's record. Helpers write to the recorder of the row their thread runs.

    Every recorded command is kept (bounded); the row shows the one that decided it:
    for a row that finished cleanly the last command that succeeded (a fallback that
    worked, not the attempt it replaced), otherwise the last command that failed.
    Read-only probes run quiet and are never "what ran"."""

    RUNS = 16

    def __init__(self):
        self.started = time.monotonic()
        self.phases = {}
        self._phase = None
        self.runs = []
        self.owner = None
        self.cause = None
        self.reason = None
        self.notes = []

    def phase(self, name):
        now = time.monotonic()
        if self._phase is not None:
            previous, since = self._phase
            self.phases[previous] = self.phases.get(previous, 0) + int((now - since) * 1000)
        self._phase = (name, now) if name else None

    def ran(self, argv, exit_code=None, output="", label=None, failed=None):
        failed = exit_code not in (None, 0) if failed is None else failed
        self.runs.append({"command": command_display(argv, label), "exitCode": exit_code,
                          "tail": scrub(output) if failed else "", "failed": failed})
        del self.runs[:-self.RUNS]

    def deciding(self, success):
        """The run that decided a row: the last success for a clean row, else the last failure."""
        for run in reversed(self.runs):
            if run["failed"] != success:
                return run
        return self.runs[-1] if self.runs and not success else None

    # Older call sites and tests read these.
    @property
    def command(self):
        run = self.deciding(False)
        return run["command"] if run else None

    @property
    def exit_code(self):
        run = self.deciding(False)
        return run["exitCode"] if run else None

    def blame(self, cause, reason=None):
        if cause in CAUSES:
            self.cause = cause
        if reason is not None:
            self.reason = reason if isinstance(reason, str) and REASON.fullmatch(reason) else "unlisted"

    def note(self, word):
        if isinstance(word, str) and REASON.fullmatch(word) and word not in self.notes and len(self.notes) < 6:
            self.notes.append(word)

    def export(self, row):
        self.phase(None)
        cause, reason = self.cause, self.reason
        failure = self.deciding(False)
        if cause is None:
            cause, derived = cause_for(row, failure["exitCode"] if failure and failure["failed"] else None)
            reason = reason or derived
        run = self.deciding(cause == "none")
        value = {"cause": cause}
        if reason:
            value["reason"] = reason
        if run:
            value["command"] = run["command"]
            if isinstance(run["exitCode"], int) and -2 ** 31 <= run["exitCode"] < 2 ** 32:
                value["exitCode"] = run["exitCode"]
        phases = {name: max(0, min(ms, 86400000)) for name, ms in list(self.phases.items())[:8] if REASON.fullmatch(name)}
        if phases:
            value["phases"] = phases
        if self.owner:
            value["owner"] = self.owner
        if self.notes:
            value["notes"] = list(self.notes)
        if run and run["tail"] and cause != "none":
            value["tail"] = run["tail"]
        return value


# Fixed classification of a finished row whose helper named no cause itself.
_CODE_CAUSES = {
    "signin_needed": "signin_needed", "cursor_signin_needed": "auth_required",
    "antigravity_acp_signin_hold": "signin_needed", "policy_blocked": "policy_blocked",
    "wrong_prefix": "wrong_prefix", "prefix_not_writable": "not_writable",
    "t3_session_issue_unknown": "lost_session_reply", "t3_session_reply_lost": "lost_session_reply", "t3_absent": "t3_absent",
    "t3_unreachable": "t3_not_ready", "t3_not_ready": "t3_not_ready", "t3_restart_pending": "t3_restart_pending",
    "in_use": "in_use", "quit_first": "in_use", "busy": "in_use", "codex_busy": "in_use",
    "cursor_update_staged": "in_use", "package_in_use": "in_use",
    "timeout": "timeout", "check_timeout": "timeout", "host_timeout": "timeout", "t3_install_timeout": "timeout",
    "t3_install_failed:installer": "installer_failed", "version_went_down": "installer_failed",
}


def cause_for(row, exit_code=None):
    status, code = row.get("status"), row.get("messageCode")
    if status in ("updated", "current", "not_installed", "skipped", "held") and code not in _CODE_CAUSES:
        return "none", None
    if code in _CODE_CAUSES:
        return _CODE_CAUSES[code], None
    if code == "update_failed" and isinstance(exit_code, int) and exit_code != 0:
        return "installer_failed", None
    return "unknown", (code if isinstance(code, str) and REASON.fullmatch(code) else "unlisted")


_scope = threading.local()


class _Silent(Diagnostics):
    """Writes outside any row (inventory, tests) go nowhere."""


def diagnostics():
    """The recorder of the row this thread runs, or a throwaway one."""
    value = getattr(_scope, "row", None)
    return value if value is not None else _Silent()


@contextlib.contextmanager
def row_diagnostics():
    previous = getattr(_scope, "row", None)
    _scope.row = Diagnostics()
    try:
        yield _scope.row
    finally:
        _scope.row = previous


def with_diagnostics(row, recorder):
    """A row with its diagnostics attached; a row that already carries some keeps them."""
    if isinstance(row, dict) and "diagnostics" not in row:
        row["diagnostics"] = recorder.export(row)
    return row


@dataclasses.dataclass
class Install:
    app_id: str
    platform: str
    path: pathlib.Path
    version: str = None
    manager: str = "native"
    identity: str = None
    publisher: str = None
    package_root: pathlib.Path = None
    command: list = dataclasses.field(default_factory=list, repr=False)
    # "timeout" when the read-only detection probe ran out of time: the app
    # is reported as "Check timed out", never as not installed or failed.
    probe: str = None
    # Other copies beside a managed Codex or Claude install: fixed location word,
    # bounded version and whether they shadow it on PATH. Set by app_updates.detect_cli.
    strays: list = dataclasses.field(default_factory=list)
    # Why a probe failed, as one fixed word (policy_blocked, no_version_dir, ...), for the row's diagnostics.
    probe_reason: str = None
    # Which copy detection chose and how (diagnostics.owner): fixed words and home-relative paths only.
    owner: dict = None
    # Antigravity ACP only: "harness" for a 1.3/1.4 runtime pair, "native" for the 1.5+ CLI.
    runtime_kind: str = None
    # Cursor only: another copy that comes first on PATH and runs instead ({"path", "version"}).
    shadow: dict = None


def strip_dashboard_secrets(values):
    return {key: value for key, value in values.items() if key.upper() != "CCS_SESSION_SECRET"
            and not key.upper().startswith("CCS_DASHBOARD_") and not key.upper().endswith("_PASSWORD_HASH")}


def environment(extra=None):
    result = dict(os.environ)
    result.update(NO_AUTO_UPDATE)
    result.update(extra or {})
    return strip_dashboard_secrets(result)


def sanitize_unit_fragment(value):
    cleaned = re.sub(r"[^a-z0-9]+", "-", str(value).lower()).strip("-")
    return cleaned or "app"


def systemd_can_carry(key, value):
    """Whether systemd accepts this variable. A name like BASH_FUNC_ml%% or a.b, or a value that is
    not UTF-8, fails the whole systemd-run; the app would stay closed after its update."""
    if value is None or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key):
        return False
    try:
        str(value).encode("utf-8")
    except UnicodeEncodeError:
        return False
    return True


LOADER_ENV = ("LD_PRELOAD", "LD_LIBRARY_PATH")
LOADER_ENV_PREFIX = "AAC_LOADER_"
# Puts the loader settings back for the app alone. systemd-run and this shell run without them, the
# values stay off every argv, and exec keeps the unit's main process id. systemd expands $NAME in a
# command line before it runs, which would put the value on this shell's argv, so every $ is $$.
LOADER_TRAMPOLINE = "; ".join(
    ['if [ "$${%s%s+x}" ]; then export %s="$$%s%s"; unset %s%s; fi' % (LOADER_ENV_PREFIX, name, name, LOADER_ENV_PREFIX, name, LOADER_ENV_PREFIX, name)
     for name in LOADER_ENV] + ['exec "$$@"'])


def _unit_environment(env):
    """What systemd-run receives to hand on: entries as given, with loader settings renamed, since
    a captured LD_PRELOAD or LD_LIBRARY_PATH (AppImage, snap) would load into systemd-run itself."""
    return {(LOADER_ENV_PREFIX + key if key in LOADER_ENV else key): value for key, value in env.items()}


def systemd_run_environment(env):
    """The environment systemd-run runs with: the app's values for its bare --setenv flags, and this
    process's XDG_RUNTIME_DIR, which is how systemd-run reaches the user manager."""
    values = _unit_environment(env)
    if os.environ.get("XDG_RUNTIME_DIR"):
        values["XDG_RUNTIME_DIR"] = os.environ["XDG_RUNTIME_DIR"]
    return values


def systemd_user_service_argv(unit, cwd, env, argv, service_type="simple", properties=()):
    """Wrap argv so it starts in a transient user service OUTSIDE our cgroup.

    Why: the dashboard runs as a systemd user unit with KillMode=control-group.
    A plain spawn keeps the child (here: the restarted tmux server, and with it
    the user's CLI session) in the unit's cgroup, so the next dashboard
    restart or deploy would kill it. A transient user service re-parents the
    process to app.slice instead. Transient *scopes* are deliberately NOT used:
    this host's systemd aborts `--scope` units, so scope launches silently do
    nothing. When systemd-run is missing there is no safe launch: fail closed
    rather than cage the session in the service cgroup.

    Values never go on this argv, which any local user can read: a bare
    --setenv=KEY takes the value from systemd-run's own environment, so run it
    with systemd_run_environment(env).

    The owner's decision (2026-11-07): systemd stores those values in the app's
    transient unit file under /run/user/<uid>/systemd/transient/. That is tmpfs
    inside the user's 0700 runtime folder, so only the user and root can read
    it, and it exists while the unit is loaded and is collected after: the same
    exposure as the running app's /proc/<pid>/environ.
    """
    runner = shutil.which("systemd-run")
    if runner is None:
        raise UpdateFailure("restart_context")
    wrapped = [runner, "--user", "--unit=" + unit, "--collect"]
    if service_type != "simple":
        wrapped.append("--service-type=" + service_type)
    wrapped.extend("--property=" + item for item in properties)
    if cwd:
        wrapped.append("--working-directory=" + str(cwd))
    names = [key for key, value in strip_dashboard_secrets(_unit_environment(env or {})).items()
             if systemd_can_carry(key, value)]
    wrapped.extend("--setenv=" + key for key in names)
    wrapped.append("--")
    # systemd-run looks up the executable itself and does not expand it, so an escaped $ there would
    # name a path that does not exist. The sh step takes that path as an ordinary argument.
    if "$" in str(argv[0]) or any(key.startswith(LOADER_ENV_PREFIX) for key in names):
        wrapped.extend(["/bin/sh", "-c", LOADER_TRAMPOLINE, "sh"])
    # systemd expands $NAME in a command line, and a relaunch replays whatever the app was given.
    wrapped.extend(str(word).replace("$", "$$") for word in argv)
    return wrapped


def in_service_cgroup():
    """Whether this process runs in a systemd user service's cgroup (the dashboard), where everything
    it starts is charged to that service: its MemoryMax, and with KillMode=process its cgroup and
    memory.peak stay alive across restarts. An ssh session or a plain shell needs no transient unit."""
    try:
        with open("/proc/self/cgroup", encoding="utf-8") as lines:
            return any(re.search(r"/user@\d+\.service/(?:.*/)?[^/]+\.service(?:/|$)", line.strip()) for line in lines)
    except OSError:
        return False


def _posix_process_table():
    """pid -> (ppid, start ticks) for this user's /proc entries, for owned-tree walks."""
    table = {}
    for entry in pathlib.Path("/proc").glob("[0-9]*"):
        if not entry.is_dir():
            continue
        try:
            text = (entry / "stat").read_text()
            parts = text[text.rfind(")") + 2:].split()
            if len(parts) < 20:
                continue
            table[int(entry.name)] = (int(parts[1]), parts[19])
        except (OSError, ValueError, IndexError):
            continue
    return table


def _posix_process_identity(pid):
    try:
        text = pathlib.Path("/proc/%d/stat" % pid).read_text()
        return text[text.rfind(")") + 2:].split()[19]
    except (OSError, IndexError):
        return None


def _posix_descendants(root_pid, table=None):
    """(pid, start) descendants of root_pid, deepest first, from one /proc snapshot.

    Only processes whose whole parent chain leads to root_pid are returned, so a
    pre-existing host that is merely a sibling is never returned. The start ticks
    pin each PID at snapshot time; callers re-check them before signalling, so a
    reused PID that happens to have the same number is never signalled. Ticks are
    clock-ticks-since-boot strings and compare as ints: a candidate is a true
    descendant only if its ticks are >= its parent's along the whole chain to
    the root, so a stale older process whose recorded parent pid was recycled as
    the root is never selected. A missing or unparsable start never selects.
    """
    table = _posix_process_table() if table is None else table
    root_entry = table.get(root_pid)
    if root_entry is None:
        return []
    try:
        root_start = int(root_entry[1])
    except (ValueError, TypeError):
        return []
    children = {}
    for pid, (ppid, _start) in table.items():
        children.setdefault(ppid, []).append(pid)
    from collections import deque

    queue = deque((pid, 1) for pid in children.get(root_pid, []))
    seen = {root_pid}
    validated = {root_pid: root_start}
    found = []
    while queue:
        pid, depth = queue.popleft()
        if pid in seen:
            continue
        seen.add(pid)
        entry = table.get(pid)
        if entry is None:
            continue
        ppid, start = entry
        try:
            pid_start = int(start)
        except (ValueError, TypeError):
            continue
        parent_start = validated.get(ppid)
        if parent_start is None:
            continue
        if pid_start < parent_start:
            continue
        validated[pid] = pid_start
        found.append((depth, pid, start))
        for child in children.get(pid, []):
            queue.append((child, depth + 1))
    found.sort(reverse=True)
    return [(pid, start) for _depth, pid, start in found]


def _kill_posix_tree(process):
    import signal

    root = process.pid
    try:
        table = _posix_process_table()
    except Exception:
        table = {}
    try:
        root_entry = table.get(root)
        root_identity = root_entry[1] if root_entry is not None else None
    except Exception:
        root_identity = None
    try:
        descendants = _posix_descendants(root, table) if root_identity is not None else []
    except Exception:
        descendants = []
    for pid, start in descendants:
        try:
            if root_identity is None or start is None:
                continue
            if _posix_process_identity(root) != root_identity:
                continue
            current = _posix_process_identity(pid)
            if current is None or current != start:
                continue
            os.kill(pid, signal.SIGKILL)
        except (OSError, ProcessLookupError):
            continue
    try:
        os.killpg(root, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass


def _windows_process_table():
    """pid -> ppid for every process in the Toolhelp snapshot, or {} on failure."""
    import ctypes
    from ctypes import wintypes

    kernel = ctypes.windll.kernel32
    kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]

    class PROCESSENTRY32(ctypes.Structure):
        _fields_ = [
            ("dwSize", wintypes.DWORD),
            ("cntUsage", wintypes.DWORD),
            ("th32ProcessID", wintypes.DWORD),
            ("th32DefaultHeapID", ctypes.c_ulonglong),
            ("th32ModuleID", wintypes.DWORD),
            ("cntThreads", wintypes.DWORD),
            ("th32ParentProcessID", wintypes.DWORD),
            ("pcPriClassBase", wintypes.LONG),
            ("dwFlags", wintypes.DWORD),
            ("szExeFile", ctypes.c_char * 260),
        ]

    kernel.Process32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32)]
    kernel.Process32First.restype = wintypes.BOOL
    kernel.Process32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32)]
    kernel.Process32Next.restype = wintypes.BOOL

    TH32CS_SNAPPROCESS = 0x2
    snapshot = kernel.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if not snapshot or snapshot == ctypes.c_void_p(-1).value:
        return {}
    table = {}
    try:
        entry = PROCESSENTRY32()
        entry.dwSize = ctypes.sizeof(PROCESSENTRY32)
        ok = kernel.Process32First(snapshot, ctypes.byref(entry))
        while ok:
            table[int(entry.th32ProcessID)] = int(entry.th32ParentProcessID)
            ok = kernel.Process32Next(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    return table


def _windows_filetime():
    import ctypes

    class FILETIME(ctypes.Structure):
        _fields_ = [("low", ctypes.c_uint32), ("high", ctypes.c_uint32)]

    return FILETIME


def _windows_process_identity(pid):
    import ctypes
    from ctypes import wintypes

    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    FILETIME = _windows_filetime()
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(FILETIME)] * 4
    kernel.GetProcessTimes.restype = wintypes.BOOL
    handle = kernel.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return None
    try:
        created, exited, system, user = FILETIME(), FILETIME(), FILETIME(), FILETIME()
        if not kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited),
                                      ctypes.byref(system), ctypes.byref(user)):
            return None
        return str((created.high << 32) | created.low)
    finally:
        kernel.CloseHandle(handle)


def _terminate_windows_process(pid, expected=None):
    import ctypes
    from ctypes import wintypes

    if expected is None:
        return
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
    handle = kernel.OpenProcess(0x1001, False, pid)  # PROCESS_TERMINATE | QUERY_LIMITED
    if not handle:
        return
    try:
        if expected is not None:
            FILETIME = _windows_filetime()
            kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(FILETIME)] * 4
            kernel.GetProcessTimes.restype = wintypes.BOOL
            created, exited, system, user = FILETIME(), FILETIME(), FILETIME(), FILETIME()
            if not kernel.GetProcessTimes(handle, ctypes.byref(created), ctypes.byref(exited),
                                          ctypes.byref(system), ctypes.byref(user)):
                return
            if str((created.high << 32) | created.low) != expected:
                return  # PID was reused; never kill a new occupant
        kernel.TerminateProcess(handle, 0)
    finally:
        kernel.CloseHandle(handle)


def _windows_descendants(root_pid, table=None):
    """(pid, creation filetime) descendants of root_pid, deepest first.

    Same ancestry rule as the POSIX walk: only processes whose parent chain
    leads to root_pid are returned, so pre-existing muse serve hosts and other
    unrelated processes are never touched. The filetime pins each PID. The
    root and every candidate identity are captured in this same snapshot pass
    before selection; filetimes compare numerically and a candidate counts
    only if its filetime is >= its parent's along the whole chain to the root,
    so a stale older process whose recorded parent pid was recycled as the
    root is never selected. An unknown (None) identity never selects.
    """
    table = _windows_process_table() if table is None else table
    try:
        root_identity = _windows_process_identity(root_pid)
    except Exception:
        return []
    if root_identity is None:
        return []
    try:
        root_start = int(root_identity)
    except (ValueError, TypeError):
        return []
    children = {}
    for pid, ppid in table.items():
        children.setdefault(ppid, []).append(pid)
    from collections import deque

    queue = deque((pid, 1) for pid in children.get(root_pid, []))
    seen = {root_pid}
    validated = {root_pid: root_start}
    identity_map = {root_pid: root_identity}
    found = []
    while queue:
        pid, depth = queue.popleft()
        if pid in seen:
            continue
        seen.add(pid)
        try:
            identity = _windows_process_identity(pid)
        except Exception:
            continue
        if identity is None:
            continue
        try:
            pid_start = int(identity)
        except (ValueError, TypeError):
            continue
        ppid = table.get(pid)
        if ppid is None:
            continue
        parent_start = validated.get(ppid)
        if parent_start is None:
            continue
        if pid_start < parent_start:
            continue
        validated[pid] = pid_start
        identity_map[pid] = identity
        found.append((depth, pid))
        for child in children.get(pid, []):
            queue.append((child, depth + 1))
    found.sort(reverse=True)
    return [(pid, identity_map[pid]) for _depth, pid in found]


def _kill_windows_tree(process):
    try:
        root = process.pid
        try:
            root_identity = _windows_process_identity(root)
        except Exception:
            root_identity = None
        try:
            descendants = _windows_descendants(root) if root_identity is not None else []
        except Exception:
            descendants = []
    except Exception:
        root, root_identity, descendants = None, None, []
    for pid, identity in descendants:
        try:
            if root_identity is None or identity is None:
                continue
            if _windows_process_identity(root) != root_identity:
                continue
            _terminate_windows_process(pid, identity)
        except Exception:
            continue
    try:
        process.kill()
    except (OSError, ProcessLookupError):
        pass


def _stop_group(process, tree=False):
    """Stop a timed-out child, and only with tree=True its owned tree.

    Every bounded command leads its own session/group on POSIX
    (start_new_session) and is the only process in its own tree on Windows.
    tree=False reproduces the pre-tree cleanup exactly: POSIX killpg of the
    child's own session group, Windows process.kill() of the root only, so a
    detached relaunch that preserves ppid ancestry in a new session is spared.
    Only the muse warm opts into tree=True, which stops only validated
    descendants of the Popen root, each creation-ordered (>= parent along the
    whole chain) and pinned by identity before it is signalled, so pre-existing
    serve hosts and reused PIDs are never killed. An unknown identity never
    selects and never terminates a pid lookup; the root handle kill remains.
    """
    try:
        if os.name == "nt":
            if tree:
                _kill_windows_tree(process)
            else:
                process.kill()
        else:
            if tree:
                _kill_posix_tree(process)
            else:
                import signal
                os.killpg(process.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass
    if process.stdout is not None:
        try: process.stdout.close()
        except OSError: pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass


# pythonw (the task's interpreter) has no console, so Windows would give each
# console child a new visible window. GUI programs ignore this flag.
CREATE_NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0x08000000)


def _run_bounded(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
    """Run one fixed command with no TTY and no stdin, killed at its timeout.

    A new session means the child has no controlling terminal, so a prompt that
    opens /dev/tty fails at once instead of waiting forever; stdin is empty.
    On Windows a child gets no console window unless visible is set. kill_tree
    opts this one call into whole-tree cleanup; every other caller keeps the
    pre-tree cleanup. Returns (returncode, stdout bytes), with stderr appended
    when merge_errors is set; raises subprocess.TimeoutExpired.

    stderr goes to an unnamed temporary file, never a pipe: a detached
    grandchild that inherits it cannot hold communicate() open, and the run's
    last 64 KiB are read back into errors_sink (a list) for classification.
    """
    if os.name == "nt":
        options = {} if visible else {"creationflags": CREATE_NO_WINDOW}
    else:
        options = {"start_new_session": True}
    sink = None
    if not merge_errors and errors_sink is not None:
        try:
            sink = tempfile.TemporaryFile()
        except OSError:
            sink = None  # no temp space: the command still runs, its stderr just goes unread
    try:
        process = subprocess.Popen(
            argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if capture or merge_errors else subprocess.DEVNULL,
            stderr=subprocess.STDOUT if merge_errors else (sink if sink is not None else subprocess.DEVNULL),
            env=env, cwd=cwd, **options,
        )
        try:
            stdout, _ = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            _stop_group(process, tree=kill_tree)
            raise
        except BaseException:
            _stop_group(process, tree=kill_tree)
            raise
        return process.returncode, stdout or b""
    finally:
        if sink is not None:
            with contextlib.suppress(OSError, ValueError):
                size = sink.seek(0, os.SEEK_END)
                sink.seek(0)
                # A truncated credential block cannot be scrubbed safely without its marker.
                errors_sink.append(sink.read(65536) if size <= 65536 else b"<output omitted: diagnostic limit>")
            sink.close()


def command(argv, timeout=30, env=None, capture=False, preserve_env=False, capture_limit=65536, visible=False, errors=False, cwd=None, kill_tree=False, label=None, stderr_tail=True, quiet=False):
    """Run one fixed command; a non-zero exit raises UpdateFailure. errors keeps the run's stderr,
    merged with its stdout, in the exception's output for in-memory classification only.
    kill_tree opts this one call into whole-tree cleanup; only the muse warm sets it.

    Every run is recorded in the row's diagnostics: what ran (label, or the program's
    own name and fixed arguments), its exit code, and for a failure the scrubbed tail
    of its stderr. Without errors the failure's output is that stderr too, still in
    memory only, so callers can classify a refusal (sign-in, policy) by its words.
    stderr_tail=False keeps stderr on the null device: for commands that hand off a
    long-lived app (open -a, wt.exe, systemd-run), whose output must not land anywhere.
    quiet=True leaves the run out of the row's record: read-only probes (versions,
    service states) never become "what ran"."""
    record = _Silent() if quiet else diagnostics()
    stderr = []
    try:
        returncode, data = _run_bounded(
            [str(arg) for arg in argv], timeout,
            strip_dashboard_secrets(env) if preserve_env and env is not None else environment(env), capture, visible, errors, cwd, kill_tree,
            None if errors or not stderr_tail else stderr,
        )
    except subprocess.TimeoutExpired:
        record.ran(argv, None, label=label, failed=True)
        record.note("timed_out")
        raise UpdateFailure("timeout") from None
    except OSError:
        record.ran(argv, None, label=label, failed=True)
        record.note("not_started")
        raise UpdateFailure("update_failed", reason="not_started") from None
    if returncode != 0:
        text = data.decode("utf-8", "replace")[-262144:] if errors else b"".join(stderr).decode("utf-8", "replace")
        diagnostic = data.decode("utf-8", "replace") if errors or not text else text
        record.ran(argv, returncode, diagnostic, label)
        raise UpdateFailure(output=text, exit_code=returncode)
    record.ran(argv, 0, label=label)
    if capture and len(data) > min(2 * 1024 * 1024, capture_limit):
        raise UpdateFailure()
    return data.decode("utf-8", "replace") if capture else ""


# Windows PowerShell 5.1 writes to a pipe in the OEM code page (CP437, CP850, ...), which
# command() would read as UTF-8: a "C:\\Users\\Ren\u00e9" path or a localized error came back as U+FFFD.
# UTF8Encoding($false) has no byte-order mark: [System.Text.Encoding]::UTF8 carries one, and a BOM
# before the first character would break every json.loads of a PowerShell reply.
POWERSHELL_UTF8 = "[Console]::OutputEncoding=(New-Object System.Text.UTF8Encoding $false); "


def powershell(script, timeout=30, capture_limit=65536, label=None, quiet=False):
    import base64
    encoded = base64.b64encode((POWERSHELL_UTF8 + script).encode("utf-16le")).decode("ascii")
    output = command(["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded], timeout, capture=True,
                     capture_limit=capture_limit, label=label, quiet=quiet)
    # A preamble is never part of the reply, whatever the console writer does.
    return output[1:] if output.startswith("\ufeff") else output


def ps_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def version_text(value):
    if not isinstance(value, str) or len(value) > 65536:
        return None
    match = re.search(r"(?<!\d)(\d+\.\d+(?:\.[0-9]+){0,3}(?:-[A-Za-z0-9.]+)?)(?![0-9])", value)
    return match.group(1) if match else None


def marker_names_version(marker_version, installed):
    """Whether a pending-restart marker is for this install. The Codex bridge writes the numeric
    part alone (0.50.0), where version_text keeps a prerelease (0.50.0-alpha.1)."""
    def numeric(value):
        match = re.search(r"\d+\.\d+(?:\.\d+){0,3}", value) if isinstance(value, str) else None
        return match.group(0) if match else None
    return numeric(marker_version) is not None and numeric(marker_version) == numeric(installed)


def pending_marker(pending):
    """A pending-restart marker's fields, or none without a readable one."""
    try:
        value = json.loads(pending.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def marker_owes_restart(marker):
    """Whether a marker names something an earlier run closed. That is owed whatever version is
    installed now, since a helper can be killed after replacing the app; the bridge checks what runs."""
    return marker.get("desktop") is True or marker.get("daemon") is True


def marker_applies_to(marker, installed):
    """Whether a pending-restart marker is for this install: the version it records, or the version a
    desktop install was about to put in place when it was interrupted."""
    return isinstance(marker, dict) and any(marker_names_version(marker.get(key), installed) for key in ("version", "target"))


def version_tuple(value):
    return tuple(int(part) for part in re.findall(r"\d+", (value or "").split("-")[0]))


def npm_version_key(value):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?", value or "")
    if not match:
        raise UpdateFailure("version_unknown")
    prerelease = match[4]
    identifiers = tuple((0, int(part)) if part.isdigit() else (1, part) for part in (prerelease or "").split("."))
    return (*map(int, match.groups()[:3]), prerelease is None, identifiers)


def process_identity(pid):
    """Linux start ticks and boot identity, shared with the dashboard lock writer."""
    try:
        stat = pathlib.Path("/proc", str(pid), "stat").read_text()
        start = stat[stat.rfind(")") + 2:].split()[19]
        boot = pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        return boot + ":" + start
    except (OSError, IndexError):
        return None


def asar_package(path, header_limit=2 * 1024 * 1024):
    """The root package.json of an Electron app.asar, or None. Bounded reads only; nothing is executed.

    ZCode's header lists its whole node_modules (about 7 MB), so callers pass their own limit.
    """
    try:
        with pathlib.Path(path).open("rb") as stream:
            prefix = stream.read(16)
            header_size, json_size = struct.unpack("<I", prefix[4:8])[0], struct.unpack("<I", prefix[12:16])[0]
            if not 0 < json_size <= header_limit or not json_size + 8 <= header_size <= header_limit + 16:
                return None
            entry = json.loads(stream.read(json_size))["files"]["package.json"]
            size, offset = int(entry["size"]), int(entry["offset"])
            if not 0 < size <= 65536 or offset < 0 or entry.get("unpacked") or entry.get("link"):
                return None
            stream.seek(8 + header_size + offset)
            package = json.loads(stream.read(size))
        return package if isinstance(package, dict) else None
    except (OSError, ValueError, KeyError, TypeError, struct.error):
        return None


def resolve_npm(prefix):
    """Locate node.exe plus its npm-cli.js so npm runs without any shell.

    cmd.exe /d /s /c mangles a quoted executable containing spaces, so the
    previous '"C:\\Program Files\\nodejs\\npm.cmd" install ...' line died with
    "not recognized" before npm ever started. Returns (node, cli) or None.
    """
    # The prefix's own Node first (nvm-windows, a portable Node that owns the prefix), then PATH's, then the default.
    roots = [pathlib.Path(prefix)]
    located = shutil.which("npm.cmd")
    if located:
        roots.append(pathlib.Path(located).parent)
    roots.append(pathlib.Path(os.environ.get("ProgramFiles", "C:\\Program Files")) / "nodejs")
    for root in roots:
        node, cli = root / "node.exe", root / "node_modules/npm/bin/npm-cli.js"
        if node.is_file() and cli.is_file():
            return node, cli
    located_node = shutil.which("node.exe") or shutil.which("node")
    if located_node:
        root = pathlib.Path(located_node).parent
        cli = root / "node_modules/npm/bin/npm-cli.js"
        if cli.is_file():
            return pathlib.Path(located_node), cli
    return None if os.name == "nt" else _posix_npm(prefix)


def _posix_npm(prefix):
    """(node, npm-cli.js) of one POSIX install, both as real paths, or None.

    The prefix's own bin/node comes first (the node its `bin/codex` runs), then PATH's node.
    npm-cli.js must sit beside that node's real install: lib/node_modules/npm, or Homebrew's
    libexec copy. PATH's npm-cli.js is the last resort, for distribution layouts.
    """
    for candidate in (pathlib.Path(prefix) / "bin/node", shutil.which("node")):
        if candidate and pathlib.Path(candidate).is_file():
            node = pathlib.Path(candidate).resolve()
            for cli in (node.parents[1] / "lib/node_modules/npm/bin/npm-cli.js", node.parents[1] / "libexec/lib/node_modules/npm/bin/npm-cli.js"):
                if cli.is_file():
                    return node, cli
    located_npm, located_node = shutil.which("npm"), shutil.which("node")
    if located_npm and located_node:
        cli = pathlib.Path(located_npm).resolve()
        if cli.name == "npm-cli.js" and cli.is_file():
            return pathlib.Path(located_node).resolve(), cli
    return None


def effective_path(host):
    """PATH as ordinary processes see it: the helper's own, plus the Mac's system folders.
    The helper arrives over ssh with a shorter PATH than launchd and Terminal give apps."""
    folders = os.environ.get("PATH", "").split(os.pathsep)
    if host == "mac":
        with contextlib.suppress(OSError):
            folders += pathlib.Path("/etc/paths").read_text(encoding="utf-8").split()
        with contextlib.suppress(OSError):
            for extra in sorted(pathlib.Path("/etc/paths.d").iterdir())[:32]:
                with contextlib.suppress(OSError):
                    folders += extra.read_text(encoding="utf-8").split()
        folders += ["/opt/homebrew/bin", "/usr/local/bin"]
    return [folder for folder in dict.fromkeys(folders) if folder]


# Version probes run side by side now, so a busy host gets a little longer.
VERSION_PROBE_TIMEOUT = 20


def cli_probe(path, args=("--version",)):
    """(version or None, 'timeout' when the probe ran out of time, else None)."""
    try:
        return version_text(command([path, *args], timeout=VERSION_PROBE_TIMEOUT, capture=True, quiet=True)), None
    except UpdateFailure as error:
        return None, ("timeout" if error.code == "timeout" else None)


def cli_version(path, args=("--version",)):
    return cli_probe(path, args)[0]


def result(app_id, platform, status, before=None, after=None, manager=None, code=None, attempted=False, restarted=0):
    return {
        "appId": app_id, "platform": platform, "status": status,
        "previousVersion": before, "version": after, "manager": manager,
        "messageCode": code or status, "updateAttempted": attempted,
        "restartedProcesses": restarted,
    }


def _challenge_page(head):
    lead = head[:4096].lstrip()[:64].lower()
    return lead.startswith(b"<!doctype html") or lead.startswith(b"<html")


def download(url, destination, maximum=800 * 1024 * 1024, timeout=180):
    """Only callers' fixed official URLs reach here; final app signatures are verified.

    A bot-challenge refusal (HTTP 403 or an HTML challenge page served with a
    200) maps to download_blocked so callers can report an actionable state
    instead of a generic failure. Nothing is written for a blocked download.
    """
    deadline = time.monotonic() + timeout
    request = urllib.request.Request(url, headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
    try:
        try:
            opened = urllib.request.urlopen(request, timeout=15)
        except urllib.error.HTTPError as denied:
            if denied.code == 403:
                raise UpdateFailure("download_blocked") from None
            raise
        with opened as response:
            if not response.geturl().startswith("https://"):
                raise UpdateFailure()
            length = response.headers.get("Content-Length")
            if length and int(length) > maximum:
                raise UpdateFailure()
            if (response.headers.get("Content-Type") or "").split(";")[0].strip().lower() == "text/html":
                raise UpdateFailure("download_blocked")
            total = 0
            target = None
            try:
                while True:
                    if time.monotonic() >= deadline:
                        raise UpdateFailure("timeout")
                    data = response.read(256 * 1024)
                    if not data:
                        break
                    if target is None:
                        if _challenge_page(data):
                            raise UpdateFailure("download_blocked")
                        target = pathlib.Path(destination).open("wb")
                    total += len(data)
                    if total > maximum:
                        raise UpdateFailure()
                    target.write(data)
            finally:
                if target is not None:
                    target.close()
                elif total == 0:
                    pathlib.Path(destination).unlink(missing_ok=True)
            if target is None:
                pathlib.Path(destination).touch()
    except UpdateFailure:
        raise
    except Exception:
        raise UpdateFailure() from None


_CONTENT_RANGE = re.compile(r"^bytes (\d+)-(\d+)/(\d+)$")


class RangeReader:
    """A read-only, seekable view of one remote file, fetched in HTTP byte ranges.

    zipfile can read a single member (an MSIX's AppxManifest.xml) through it
    without downloading the whole package. Every limit is fixed: total time,
    request count and bytes, so a slow or odd server only makes the caller
    fall back, never wait. Anything unexpected raises UpdateFailure.
    """

    def __init__(self, url, timeout=20, maximum_bytes=8 * 1024 * 1024, maximum_requests=12, readahead=256 * 1024):
        self.deadline = time.monotonic() + timeout
        self.maximum_bytes, self.maximum_requests, self.readahead = maximum_bytes, maximum_requests, readahead
        self.fetched = self.requests = self.position = 0
        self.url = url
        self.spans = []
        # A suffix range learns the size (and the final URL behind any
        # redirect) and already holds the zip's end records.
        start, data, self.size = self._get("bytes=-65536")
        self.spans.append((start, data))

    def _get(self, value):
        self.requests += 1
        remaining = self.deadline - time.monotonic()
        if self.requests > self.maximum_requests or remaining <= 0:
            raise UpdateFailure("timeout")
        request = urllib.request.Request(self.url, headers={"User-Agent": "CCS-Installed-App-Updater/1.0", "Range": value})
        try:
            with urllib.request.urlopen(request, timeout=min(15, remaining)) as response:
                if response.status != 206 or not response.geturl().startswith("https://"):
                    raise UpdateFailure()
                match = _CONTENT_RANGE.match(response.headers.get("Content-Range") or "")
                if not match:
                    raise UpdateFailure()
                first, last, total = (int(part) for part in match.groups())
                expected = last - first + 1
                if expected <= 0 or self.fetched + expected > self.maximum_bytes:
                    raise UpdateFailure()
                data = response.read(expected + 1)
                if len(data) != expected:
                    raise UpdateFailure()
                self.fetched += expected
                self.url = response.geturl()
                return first, data, total
        except UpdateFailure:
            raise
        except Exception:
            raise UpdateFailure() from None

    def seekable(self):
        return True

    def tell(self):
        return self.position

    def seek(self, offset, whence=0):
        base = {0: 0, 1: self.position, 2: self.size}[whence]
        if base + offset < 0:
            raise ValueError("negative seek")
        self.position = base + offset
        return self.position

    def read(self, size=-1):
        end = self.size if size is None or size < 0 else min(self.size, self.position + size)
        if end <= self.position:
            return b""
        for start, data in self.spans:
            if start <= self.position and end <= start + len(data):
                chunk = data[self.position - start:end - start]
                self.position = end
                return chunk
        last = min(self.size, max(end, self.position + self.readahead)) - 1
        start, data, _ = self._get("bytes=%d-%d" % (self.position, last))
        if start != self.position:
            raise UpdateFailure()
        self.spans.append((start, data))
        chunk = data[:end - start]
        self.position = end
        return chunk

    def close(self):
        self.spans = []


def remote_fingerprint(url, timeout=15):
    """The final URL, size and validator of a remote file, from one HEAD request.

    Lets a caller recognise a package it already verified without downloading
    it again. None when the server does not answer clearly.
    """
    request = urllib.request.Request(url, method="HEAD", headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            if response.status != 200 or not response.geturl().startswith("https://"):
                return None
            length = response.headers.get("Content-Length")
            validator = response.headers.get("ETag") or response.headers.get("Last-Modified")
            if not length or not length.isdigit() or not validator:
                return None
            return {"url": response.geturl(), "size": int(length), "validator": validator[:200]}
    except Exception:
        return None


@contextlib.contextmanager
def private_temporary():
    root = pathlib.Path.home() / ".ccs/app-updates"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix="stage-", dir=root) as value:
        yield pathlib.Path(value)


@contextlib.contextmanager
def execution_lock(name="helper-update.lock"):
    root = pathlib.Path.home() / ".ccs/app-updates"
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    handle = (root / name).open("a+b")
    try:
        if os.name == "nt":
            import msvcrt
            handle.seek(0)
            if handle.read(1) == b"":
                handle.write(b"0")
                handle.flush()
            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except (BlockingIOError, PermissionError, OSError):
        handle.close()
        raise UpdateFailure("busy") from None
    try:
        yield
    finally:
        handle.close()


def write_private_json(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary = path.with_name(path.name + "." + str(os.getpid()) + ".tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            json.dump(value, output, ensure_ascii=True, allow_nan=False)
        os.replace(temporary, path)
    finally:
        # Cleanup must not replace the original serialization/replace exception.
        with contextlib.suppress(OSError):
            temporary.unlink(missing_ok=True)
