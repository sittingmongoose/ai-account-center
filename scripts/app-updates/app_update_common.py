"""Shared bounded operations for the fixed installed-app updater."""

import contextlib
import dataclasses
import json
import os
import pathlib
import re
import subprocess
import tempfile
import time
import urllib.request

APP_LABELS = {
    "antigravity-cli": "Antigravity CLI", "muse-code": "Muse Code", "omp": "OMP",
    "codex-cli": "Codex CLI", "codex-desktop": "Codex Desktop",
    "claude-code": "Claude Code", "claude-desktop": "Claude Desktop",
}
NO_AUTO_UPDATE = {
    "MUSE_NO_AUTO_UPDATE": "1", "AGY_CLI_DISABLE_AUTO_UPDATE": "true",
    "DISABLE_AUTOUPDATER": "1", "NO_UPDATE_NOTIFIER": "1", "CODEX_NON_INTERACTIVE": "1",
}


class UpdateFailure(Exception):
    def __init__(self, code="update_failed"):
        super().__init__(code)
        self.code = code


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


def environment(extra=None):
    result = dict(os.environ)
    result.update(NO_AUTO_UPDATE)
    result.update(extra or {})
    return result


def command(argv, timeout=30, env=None, capture=False, preserve_env=False, capture_limit=65536):
    try:
        completed = subprocess.run(
            [str(arg) for arg in argv], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, timeout=timeout, env=dict(env) if preserve_env and env is not None else environment(env),
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        raise UpdateFailure("timeout" if timeout >= 60 else "update_failed") from None
    if completed.returncode != 0:
        raise UpdateFailure()
    data = completed.stdout or b""
    if len(data) > min(2 * 1024 * 1024, capture_limit):
        raise UpdateFailure()
    return data.decode("utf-8", "replace") if capture else ""


def powershell(script, timeout=30, capture_limit=65536):
    import base64
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    return command(["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded], timeout, capture=True, capture_limit=capture_limit)


def ps_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def version_text(value):
    if not isinstance(value, str) or len(value) > 65536:
        return None
    match = re.search(r"(?<!\d)(\d+\.\d+(?:\.[0-9]+){0,3}(?:-[A-Za-z0-9.]+)?)(?![0-9])", value)
    return match.group(1) if match else None


def version_tuple(value):
    return tuple(int(part) for part in re.findall(r"\d+", (value or "").split("-")[0]))


def cli_version(path, args=("--version",)):
    try:
        return version_text(command([path, *args], timeout=10, capture=True))
    except UpdateFailure:
        return None


def result(app_id, platform, status, before=None, after=None, manager=None, code=None, attempted=False, restarted=0):
    return {
        "appId": app_id, "platform": platform, "status": status,
        "previousVersion": before, "version": after, "manager": manager,
        "messageCode": code or status, "updateAttempted": attempted,
        "restartedProcesses": restarted,
    }


def download(url, destination, maximum=800 * 1024 * 1024, timeout=180):
    """Only callers' fixed official URLs reach here; final app signatures are verified."""
    deadline = time.monotonic() + timeout
    request = urllib.request.Request(url, headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
    try:
        with urllib.request.urlopen(request, timeout=15) as response, pathlib.Path(destination).open("wb") as target:
            if not response.geturl().startswith("https://"):
                raise UpdateFailure()
            length = response.headers.get("Content-Length")
            if length and int(length) > maximum:
                raise UpdateFailure()
            total = 0
            while True:
                if time.monotonic() >= deadline:
                    raise UpdateFailure("timeout")
                data = response.read(256 * 1024)
                if not data:
                    break
                total += len(data)
                if total > maximum:
                    raise UpdateFailure()
                target.write(data)
    except UpdateFailure:
        raise
    except Exception:
        raise UpdateFailure() from None


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
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, ensure_ascii=True, allow_nan=False)
    os.replace(temporary, path)
