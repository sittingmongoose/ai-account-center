"""Cursor's supported native updater, independently of T3's own update."""

import calendar
import getpass
import os
import pathlib
import re
import tempfile
import time

import app_update_common as _common
from app_update_common import (
    Install, UpdateFailure, cli_probe, command, diagnostics, display_path, environment, powershell, ps_quote, result, version_text,
)
from app_update_macos import gui_command


CURSOR_VERSION = re.compile(r"^20\d{2}\.\d{2}\.\d{2}-[A-Za-z0-9.-]{1,80}$")
# The Windows launcher's own selection (cursor-agent.ps1, read on both Windows PCs 2026-11-13): version
# folders in this form, sorted by their date alone (Parse-VersionString), newest first.
VERSION_DIR = re.compile(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-\d{2}-\d{2}-\d{2})?-[a-f0-9]+$")
# PowerShell's words when an execution policy refuses a script file. Not UnauthorizedAccessException
# (a file that cannot be removed), which is not a policy refusal.
POLICY = re.compile(r"PSSecurityException|FullyQualifiedErrorId\s*:\s*UnauthorizedAccess\b(?!Exception)"
                    r"|running scripts is disabled on this system|is not digitally signed", re.I)
# Cursor's words when its update service refuses the CLI's sign-in (HTTP 401, ERROR_NOT_LOGGED_IN).
AUTH = re.compile(r"\[unauthenticated\]|ERROR_NOT_LOGGED_IN|not logged in|\"status\"\s*:\s*401\b|\b401\b[^\n]{0,40}unauthori[sz]ed|please (?:log|sign) in", re.I)
# Words by which a run names its own cause (an errno, a disk or permission refusal): then its own output decides.
NAMED_CAUSE = re.compile(r"\bE(?:NOSPC|PERM|ACCES|BUSY|NOENT|NOTFOUND|CONNREFUSED|CONNRESET|TIMEDOUT|AI_AGAIN|NETUNREACH|HOSTUNREACH|ROFS|MFILE|EXIST|NOTEMPTY|IO)\b"
                         r"|no space left on device|permission denied|operation not permitted|read-only file system", re.I)
# A session log belongs to an update run only when it records the update's own failure.
UPDATE_FAILED = re.compile(r"Update failed:", re.I)
LOG_NAME = re.compile(r"^session-(\d{4})-(\d{2})-(\d{2})T(\d{2})-(\d{2})-(\d{2})-(\d{3})Z-(\d+)-\d+\.log$")
LOG_TAIL = 65536
LOG_LIMIT = 20


def cursor_root():
    local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
    return local / "cursor-agent"


def cursor_launcher(platform):
    if platform == "windows":
        ps1 = cursor_root() / "cursor-agent.ps1"
        return ps1 if ps1.is_file() else cursor_root() / "cursor-agent.cmd"
    return pathlib.Path.home() / ".local/bin/cursor-agent"


def windows_version_dir(root, newest_on_tie=False):
    """The version folder Cursor's launcher would run now, read without running it.

    Mirrors cursor-agent.ps1: folders matching its pattern, sorted by their date,
    newest first, with node.exe and index.js inside. Same-day folders cannot be
    ordered the way PowerShell would, so they return None (the launcher decides),
    unless newest_on_tie: when no launcher can run (Group Policy), the most recently
    written of them, which is the one an update just installed."""
    try:
        folders = [d for d in (root / "versions").iterdir() if d.is_dir() and VERSION_DIR.fullmatch(d.name)]
    except OSError:
        return None
    def day(folder):
        year, month, date = VERSION_DIR.fullmatch(folder.name).groups()[:3]
        return int(year + month.zfill(2) + date.zfill(2))
    folders.sort(key=day, reverse=True)
    if not folders:
        return None
    if len(folders) > 1 and day(folders[0]) == day(folders[1]):
        if not newest_on_tie:
            return None
        def written(folder):
            try:
                return folder.stat().st_mtime
            except OSError:
                return 0
        folders = sorted((folder for folder in folders if day(folder) == day(folders[0])), key=written, reverse=True)
    newest = folders[0]
    return newest if (newest / "node.exe").is_file() and (newest / "index.js").is_file() else None


def windows_command(path, argument, timeout=20, quiet=False):
    """Cursor's own launcher, the way its cursor-agent.cmd runs it.

    The official .cmd runs `powershell.exe -NoProfile -ExecutionPolicy Bypass -File cursor-agent.ps1`:
    a process-scoped policy for this one launcher, which leaves the user's own policy
    as it is. Group Policy still overrides it; that refusal is classified by its words.
    The launcher ends with `exit $LASTEXITCODE`, so a failed update is a failed run."""
    if pathlib.Path(path).suffix.lower() == ".ps1":
        argv = ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(path), argument]
        return command(argv, timeout=timeout, capture=True, errors=True, label="cursor-agent " + argument, quiet=quiet)
    # An older .cmd-only install: PowerShell's call operator, no script file, no policy.
    return powershell("$ErrorActionPreference='Stop'; & " + ps_quote(path) + " " + argument +
                      "; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }", timeout=timeout, label="cursor-agent " + argument, quiet=quiet)


def windows_direct(root, argument, timeout, quiet=False):
    """The launcher's last step without PowerShell, for a policy that refuses every script:
    the selected version's own node.exe and index.js, with the variable the launcher sets."""
    folder = windows_version_dir(root, newest_on_tie=True)
    if folder is None:
        raise UpdateFailure("policy_blocked", cause="policy_blocked", reason="no_direct_entry")
    env = environment({"CURSOR_INVOKED_AS": "cursor-agent.ps1",
                       "NODE_COMPILE_CACHE": str(pathlib.Path(os.environ.get("LOCALAPPDATA", "")) / "cursor-compile-cache")})
    return command([folder / "node.exe", folder / "index.js", argument], timeout=timeout, env=env, preserve_env=True,
                   capture=True, errors=True, label="cursor-agent " + argument + " (node direct)", quiet=quiet)


def windows_version(path, timeout=20):
    """(version or None, probe word, reason) for a Windows install whose folder cannot decide it:
    the launcher's own --version, and its node step when a policy refuses the launcher."""
    try:
        return version_text(windows_command(path, "--version", timeout, quiet=True)), None, None
    except UpdateFailure as error:
        if error.code == "timeout":
            return None, "timeout", "version_probe_timeout"
        if not POLICY.search(error.output or ""):
            return None, "failed", "version_probe_failed"
    # No launcher can run: the version is the folder its node step would run.
    folder = windows_version_dir(path.parent, newest_on_tie=True)
    if folder is not None:
        diagnostics().note("policy_version_from_folder")
        return folder.name, None, None
    return None, "failed", "policy_blocked"


def path_copies(platform):
    """Every cursor-agent on the effective PATH, in PATH order, one per folder (a .cmd and a .ps1
    side by side are one copy)."""
    names = ("cursor-agent.cmd", "cursor-agent.ps1", "cursor-agent.exe") if platform == "windows" else ("cursor-agent",)
    found, seen = [], set()
    for folder in _common.effective_path(platform):
        for name in names:
            candidate = pathlib.Path(folder or ".") / name
            try:
                key = str(candidate.parent.resolve() if platform == "windows" else candidate.resolve())
                if candidate.is_file() and key not in seen:
                    seen.add(key)
                    found.append(candidate)
            except OSError:
                continue
    return found


def same_copy(platform, left, right):
    try:
        if platform == "windows":
            return left.parent.resolve() == right.parent.resolve()
        return left.resolve() == right.resolve()
    except OSError:
        return False


def owner_for(platform, path, copies):
    first = copies[0] if copies else None
    on_path = any(same_copy(platform, copy, path) for copy in copies)
    value = {"manager": "native", "executable": display_path(path), "onPath": on_path, "copies": min(len(copies), 32)}
    if first is not None and not same_copy(platform, first, path):
        value["firstOnPath"] = display_path(first)
    return value


def shadow_of(platform, path, copies):
    """The copy that runs instead of the managed one: the first on PATH when it is another copy."""
    first = copies[0] if copies else None
    if first is None or same_copy(platform, first, path):
        return None
    # Reading another installer's version runs it; only POSIX binaries are probed, never a Windows script.
    version = cli_probe(first)[0] if platform != "windows" else None
    return {"path": display_path(first), "version": version}


def detect_cursor(platform):
    path = cursor_launcher(platform)
    copies = path_copies(platform)
    if not path.is_file():
        if copies:
            # Another installer's copy (Homebrew, a manual copy): it runs, but has no supported updater here.
            return Install("cursor-cli", platform, copies[0], manager="unsupported", probe_reason="unmanaged_location",
                           owner={"manager": "other", "executable": display_path(copies[0]), "onPath": True, "copies": min(len(copies), 32)})
        return None
    owner = owner_for(platform, path, copies)
    shadow = shadow_of(platform, path, copies)
    if shadow and shadow["version"]:
        owner["firstOnPathVersion"] = shadow["version"]
    if platform != "windows":
        versions = pathlib.Path.home() / ".local/share/cursor-agent/versions"
        if not path.resolve().is_relative_to(versions.resolve()) or path.resolve().name != "cursor-agent":
            return Install("cursor-cli", platform, path, manager="unsupported", probe_reason="unmanaged_location", owner=owner)
        # Older Mac launchers consult Keychain even for --version. The managed
        # target's release directory is the installer's version, without auth.
        if platform == "mac" and CURSOR_VERSION.fullmatch(path.resolve().parent.name):
            return Install("cursor-cli", platform, path, path.resolve().parent.name, owner=owner, shadow=shadow)
        version, probe = cli_probe(path)
        return Install("cursor-cli", platform, path, version, probe=probe or (None if version else "failed"),
                       probe_reason=None if version else "version_probe_failed", owner=owner, shadow=shadow)
    # Windows: the folder the launcher would run is the version, read without
    # PowerShell (its staged promotion and execution policy stay out of a check).
    folder = windows_version_dir(path.parent)
    if folder is not None:
        return Install("cursor-cli", platform, path, folder.name, owner={**owner, "source": "versions-folder"}, shadow=shadow)
    version, probe, reason = windows_version(path)
    return Install("cursor-cli", platform, path, version, probe=probe or (None if version else "failed"),
                   probe_reason=reason or (None if version else "version_unparsed"), owner=owner, shadow=shadow)


def cursor_staged(path):
    versions = path.parent / "versions"
    return (any(versions.glob("*.new")) or
            any((versions / "pending-launchers").glob("*.pending")))


def _log_folder():
    """This user's own Cursor log folder (cursor-agent-logs-<user>), never another user's."""
    try:
        folder = pathlib.Path(tempfile.gettempdir()) / ("cursor-agent-logs-" + getpass.getuser())
        if not folder.is_dir():
            return None
        if os.name != "nt" and folder.stat().st_uid != os.getuid():
            return None
        return folder
    except (OSError, KeyError, ImportError):
        return None


def session_log_says_signin(started, ended):
    """Whether Cursor's own log of this update run records a sign-in refusal.

    Only this user's folder, only a log whose name says it started during this run,
    and only one that records the update's own failure ("Update failed:") with the
    sign-in words: another Cursor session's 401 never decides this row."""
    folder = _log_folder()
    if folder is None:
        return False
    try:
        logs = sorted((log for log in folder.glob("session-*.log") if LOG_NAME.fullmatch(log.name)), key=lambda log: log.name)[-LOG_LIMIT:]
    except OSError:
        return False
    for log in reversed(logs):
        year, month, day, hour, minute, second, millis = map(int, LOG_NAME.fullmatch(log.name).groups()[:7])
        stamp = calendar.timegm((year, month, day, hour, minute, second)) + millis / 1000
        if not started - 2 <= stamp <= ended + 2:
            continue
        try:
            with log.open("rb") as stream:
                stream.seek(max(0, log.stat().st_size - LOG_TAIL))
                text = stream.read(LOG_TAIL).decode("utf-8", "replace")
        except OSError:
            continue
        if UPDATE_FAILED.search(text) and AUTH.search(text):
            return True
    return False


def classify(error, started, ended=None):
    """A failed Cursor run, by its own words. Output that names a cause (sign-in, policy, an errno)
    decides; otherwise (nothing printed, or only a generic line) Cursor's own log of this run is read."""
    text = error.output or ""
    if error.code not in ("update_failed", "policy_blocked"):
        return error
    if AUTH.search(text):
        return UpdateFailure("cursor_signin_needed", text, error.exit_code, cause="auth_required",
                             reason="http_401" if re.search(r"401|unauthenticated", text, re.I) else "not_logged_in")
    if POLICY.search(text):
        return UpdateFailure("policy_blocked", text, error.exit_code, cause="policy_blocked", reason="execution_policy")
    if error.code == "update_failed" and not NAMED_CAUSE.search(text):
        if session_log_says_signin(started, ended if ended is not None else time.time()):
            return UpdateFailure("cursor_signin_needed", text, error.exit_code, cause="auth_required", reason="session_log_401")
        diagnostics().note("signin_undetermined")
    return error


def run_update(install, remaining):
    started = time.time()
    if install.platform == "windows":
        try:
            windows_command(install.path, "update", remaining)
            return
        except UpdateFailure as error:
            failure = classify(error, started)
            if failure.code != "policy_blocked":
                raise failure from None
        # Group Policy overrides the process-scoped Bypass: run the launcher's last step directly.
        diagnostics().note("policy_fallback_direct")
        if cursor_staged(install.path):
            raise UpdateFailure("cursor_update_staged") from None
        started = time.time()
        try:
            windows_direct(install.path.parent, "update", remaining)
        except UpdateFailure as error:
            failure = classify(error, started)
            if failure.code in ("cursor_signin_needed", "timeout"):
                raise failure from None
            # The script was refused and its direct step failed too: still a policy outcome.
            raise UpdateFailure("policy_blocked", error.output, error.exit_code, cause="policy_blocked",
                                reason="direct_failed" if error.code != "policy_blocked" else error.reason) from None
        return
    try:
        if install.platform == "mac":
            gui_command("cursor-update", remaining)
        else:
            # stdout too: Cursor may print its refusal there.
            command([install.path, "update"], timeout=remaining, capture=True, errors=True, label="cursor-agent update")
    except UpdateFailure as error:
        raise classify(error, started) from None


def update_cursor(install, deadline=None, progress=None):
    before, attempted = install.version, False
    record = diagnostics()
    record.owner = install.owner
    try:
        remaining = 180 if deadline is None else min(180, deadline - time.monotonic())
        if remaining <= 0:
            raise UpdateFailure("timeout")
        attempted = True
        record.phase("update")
        run_update(install, remaining)
        record.phase("verify")
        refreshed = detect_cursor(install.platform)
        after = refreshed.version if refreshed else None
        if not after:
            raise UpdateFailure("version_unknown")
        if install.platform == "windows" and cursor_staged(install.path):
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "cursor_update_staged", True)
        shadow = (refreshed.shadow if refreshed else None) or install.shadow
        if shadow:
            # The managed copy is updated, but a bare cursor-agent runs another copy: never "updated".
            record.blame("shadowed", "other_copy_first")
            if shadow.get("version"):
                record.note("first_on_path_" + ("older" if shadow["version"] < after else "same" if shadow["version"] == after else "newer"))
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "cursor_shadowed", True)
        return result("cursor-cli", install.platform, "updated" if after != before else "current",
                      before, after, install.manager, "cursor_updated" if after != before else "current", True)
    except (UpdateFailure, OSError) as error:
        # A native Windows updater may safely stage its files while handles are
        # open and exit nonzero. Never stop those agents or delete their trees.
        refreshed = detect_cursor(install.platform)
        after = refreshed.version if refreshed else None
        if isinstance(error, UpdateFailure) and (error.cause or error.reason):
            record.blame(error.cause, error.reason)
        if isinstance(error, UpdateFailure) and error.code == "cursor_signin_needed":
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "cursor_signin_needed", attempted)
        if install.platform == "windows" and cursor_staged(install.path):
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "cursor_update_staged", attempted)
        if isinstance(error, UpdateFailure) and error.code == "policy_blocked":
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "policy_blocked", attempted)
        return result("cursor-cli", install.platform, "failed", before, after, install.manager,
                      error.code if isinstance(error, UpdateFailure) else "update_failed", attempted)
