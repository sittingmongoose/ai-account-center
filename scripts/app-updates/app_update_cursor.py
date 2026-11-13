"""Cursor's supported native updater, independently of T3's own update."""

import os
import pathlib
import re
import shutil
import tempfile
import time

from app_update_common import (
    Install, UpdateFailure, cli_probe, command, diagnostics, display_path, environment, powershell, ps_quote, result, version_text,
)
from app_update_macos import gui_command


CURSOR_VERSION = re.compile(r"^20\d{2}\.\d{2}\.\d{2}-[A-Za-z0-9.-]{1,80}$")
# The Windows launcher's own selection (cursor-agent.ps1): version folders in this form, newest date first.
VERSION_DIR = re.compile(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-\d{2}-\d{2}-\d{2})?-[a-f0-9]+$")
# PowerShell's words when an execution policy refuses a script file.
POLICY = re.compile(r"PSSecurityException|UnauthorizedAccess|running scripts is disabled|is not digitally signed|cannot be loaded because", re.I)
# Cursor's words when its update service refuses the CLI's sign-in (HTTP 401, ERROR_NOT_LOGGED_IN).
AUTH = re.compile(r"\[unauthenticated\]|ERROR_NOT_LOGGED_IN|not logged in|\"status\"\s*:\s*401\b|\b401\b[^\n]{0,40}unauthori[sz]ed|please (?:log|sign) in", re.I)
LOG_TAIL = 65536


def cursor_root():
    local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
    return local / "cursor-agent"


def cursor_launcher(platform):
    if platform == "windows":
        ps1 = cursor_root() / "cursor-agent.ps1"
        return ps1 if ps1.is_file() else cursor_root() / "cursor-agent.cmd"
    return pathlib.Path.home() / ".local/bin/cursor-agent"


def windows_version_dir(root):
    """The version folder Cursor's launcher would run now, read without running it.

    Mirrors cursor-agent.ps1: folders matching its pattern, sorted by their date,
    newest first, with node.exe and index.js inside. Same-day folders cannot be
    ordered the way PowerShell would, so they return None (the launcher decides)."""
    try:
        folders = [d for d in (root / "versions").iterdir() if d.is_dir() and VERSION_DIR.fullmatch(d.name)]
    except OSError:
        return None
    def day(folder):
        year, month, date = VERSION_DIR.fullmatch(folder.name).groups()[:3]
        return int(year + month.zfill(2) + date.zfill(2))
    folders.sort(key=day, reverse=True)
    if not folders or (len(folders) > 1 and day(folders[0]) == day(folders[1])):
        return None
    newest = folders[0]
    return newest if (newest / "node.exe").is_file() and (newest / "index.js").is_file() else None


def windows_command(path, argument, timeout=20):
    """Cursor's own launcher, the way its cursor-agent.cmd runs it.

    The official .cmd runs `powershell.exe -NoProfile -ExecutionPolicy Bypass -File cursor-agent.ps1`:
    a process-scoped policy for this one launcher, which leaves the user's own policy
    as it is. Group Policy still overrides it; that refusal is classified by its words."""
    if pathlib.Path(path).suffix.lower() == ".ps1":
        argv = ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(path), argument]
        return command(argv, timeout=timeout, capture=True, errors=True, label="cursor-agent " + argument)
    # An older .cmd-only install: PowerShell's call operator, no script file, no policy.
    return powershell("$ErrorActionPreference='Stop'; & " + ps_quote(path) + " " + argument +
                      "; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }", timeout=timeout, label="cursor-agent " + argument)


def windows_direct(root, argument, timeout):
    """The launcher's last step without PowerShell, for a policy that refuses every script:
    the selected version's own node.exe and index.js, with the variable the launcher sets."""
    folder = windows_version_dir(root)
    if folder is None:
        raise UpdateFailure("policy_blocked", cause="policy_blocked", reason="no_direct_entry")
    env = environment({"CURSOR_INVOKED_AS": "cursor-agent.ps1",
                       "NODE_COMPILE_CACHE": str(pathlib.Path(os.environ.get("LOCALAPPDATA", "")) / "cursor-compile-cache")})
    return command([folder / "node.exe", folder / "index.js", argument], timeout=timeout, env=env, preserve_env=True,
                   capture=True, errors=True, label="cursor-agent " + argument + " (node direct)")


def path_copies(platform):
    """Every cursor-agent on PATH, in PATH order, as real files."""
    names = ("cursor-agent.cmd", "cursor-agent.ps1", "cursor-agent.exe") if platform == "windows" else ("cursor-agent",)
    found, seen = [], set()
    for folder in os.environ.get("PATH", "").split(os.pathsep):
        for name in names:
            candidate = pathlib.Path(folder or ".") / name
            try:
                real = candidate.resolve()
                if candidate.is_file() and str(real) not in seen:
                    seen.add(str(real))
                    found.append(candidate)
            except OSError:
                continue
    return found


def owner_for(platform, path, copies):
    try:
        real = path.resolve()
    except OSError:
        real = path
    first = copies[0] if copies else None
    on_path = any(c.resolve() == real for c in copies) if copies else False
    value = {"manager": "native", "executable": display_path(path), "onPath": on_path, "copies": min(len(copies), 32)}
    if first is not None and first.resolve() != real and first.parent.resolve() != path.parent.resolve():
        value["firstOnPath"] = display_path(first)
    return value


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
    if platform != "windows":
        versions = pathlib.Path.home() / ".local/share/cursor-agent/versions"
        if not path.resolve().is_relative_to(versions.resolve()) or path.resolve().name != "cursor-agent":
            return Install("cursor-cli", platform, path, manager="unsupported", probe_reason="unmanaged_location", owner=owner)
        # Older Mac launchers consult Keychain even for --version. The managed
        # target's release directory is the installer's version, without auth.
        if platform == "mac" and CURSOR_VERSION.fullmatch(path.resolve().parent.name):
            return Install("cursor-cli", platform, path, path.resolve().parent.name, owner=owner)
        version, probe = cli_probe(path)
        return Install("cursor-cli", platform, path, version, probe=probe or (None if version else "failed"),
                       probe_reason=None if version else "version_probe_failed", owner=owner)
    # Windows: the folder the launcher would run is the version, read without
    # PowerShell (its staged promotion and execution policy stay out of a check).
    folder = windows_version_dir(path.parent)
    if folder is not None:
        return Install("cursor-cli", platform, path, folder.name, owner={**owner, "source": "versions-folder"})
    try:
        version, probe, reason = version_text(windows_command(path, "--version")), None, None
    except UpdateFailure as error:
        version, probe = None, "timeout" if error.code == "timeout" else "failed"
        reason = "policy_blocked" if POLICY.search(error.output or "") else "version_probe_failed"
    return Install("cursor-cli", platform, path, version, probe=probe or (None if version else "failed"),
                   probe_reason=reason or (None if version else "version_unparsed"), owner=owner)


def cursor_staged(path):
    versions = path.parent / "versions"
    return (any(versions.glob("*.new")) or
            any((versions / "pending-launchers").glob("*.pending")))


def session_log_says_signin(started):
    """Cursor writes each run's log under the temp folder (cursor-agent-logs-<user>/session-*.log).
    Read only the newest one written since this update started, in memory, for its 401 words."""
    try:
        logs = [log for folder in pathlib.Path(tempfile.gettempdir()).glob("cursor-agent-logs-*")
                for log in folder.glob("session-*.log") if log.stat().st_mtime >= started - 2]
        if not logs:
            return False
        newest = max(logs, key=lambda log: log.stat().st_mtime)
        with newest.open("rb") as stream:
            stream.seek(max(0, newest.stat().st_size - LOG_TAIL))
            return bool(AUTH.search(stream.read(LOG_TAIL).decode("utf-8", "replace")))
    except OSError:
        return False


def classify(error, started):
    """A failed Cursor run, by its own words: sign-in refused, policy refused, or an installer failure."""
    text = error.output or ""
    if AUTH.search(text) or (error.code == "update_failed" and session_log_says_signin(started)):
        return UpdateFailure("cursor_signin_needed", text, error.exit_code, cause="auth_required",
                             reason="http_401" if re.search(r"401|unauthenticated", text, re.I) or not text else "not_logged_in")
    if POLICY.search(text):
        return UpdateFailure("policy_blocked", text, error.exit_code, cause="policy_blocked", reason="execution_policy")
    return error


def run_update(install, remaining):
    started = time.time()
    try:
        if install.platform == "windows":
            try:
                windows_command(install.path, "update", remaining)
            except UpdateFailure as error:
                failure = classify(error, started)
                if failure.code != "policy_blocked":
                    raise failure from None
                # Group Policy overrides the process-scoped Bypass: run the launcher's last step directly.
                diagnostics().note("policy_fallback_direct")
                if cursor_staged(install.path):
                    raise UpdateFailure("cursor_update_staged") from None
                windows_direct(install.path.parent, "update", remaining)
        elif install.platform == "mac":
            gui_command("cursor-update", remaining)
        else:
            command([install.path, "update"], timeout=remaining, label="cursor-agent update")
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
