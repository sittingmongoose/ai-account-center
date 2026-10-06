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
import urllib.error
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
    # "timeout" when the read-only detection probe ran out of time: the app
    # is reported as "Check timed out", never as not installed or failed.
    probe: str = None


def environment(extra=None):
    result = dict(os.environ)
    result.update(NO_AUTO_UPDATE)
    result.update(extra or {})
    return result


def _stop_group(process):
    """Stop a timed-out child. On POSIX the child leads its own session, so the
    whole group goes (an installer's helpers included); a detached app relaunch
    always starts its own session and is never part of it."""
    try:
        if os.name == "nt":
            process.kill()
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


def _run_bounded(argv, timeout, env, capture):
    """Run one fixed command with no TTY and no stdin, killed at its timeout.

    A new session means the child has no controlling terminal, so a prompt that
    opens /dev/tty fails at once instead of waiting forever; stdin is empty.
    Returns (returncode, stdout bytes); raises subprocess.TimeoutExpired.
    """
    options = {} if os.name == "nt" else {"start_new_session": True}
    process = subprocess.Popen(
        argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, env=env, **options,
    )
    try:
        stdout, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        _stop_group(process)
        raise
    except BaseException:
        _stop_group(process)
        raise
    return process.returncode, stdout or b""


def command(argv, timeout=30, env=None, capture=False, preserve_env=False, capture_limit=65536):
    try:
        returncode, data = _run_bounded(
            [str(arg) for arg in argv], timeout,
            dict(env) if preserve_env and env is not None else environment(env), capture,
        )
    except subprocess.TimeoutExpired:
        raise UpdateFailure("timeout") from None
    except OSError:
        raise UpdateFailure("update_failed") from None
    if returncode != 0:
        raise UpdateFailure()
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


# Version probes run side by side now, so a busy host gets a little longer.
VERSION_PROBE_TIMEOUT = 20


def cli_probe(path, args=("--version",)):
    """(version or None, 'timeout' when the probe ran out of time, else None)."""
    try:
        return version_text(command([path, *args], timeout=VERSION_PROBE_TIMEOUT, capture=True)), None
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
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(value, output, ensure_ascii=True, allow_nan=False)
    os.replace(temporary, path)
