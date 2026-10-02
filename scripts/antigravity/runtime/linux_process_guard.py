#!/usr/bin/env python3
"""Ubuntu-only reviewed PID guards. No credentials, argv or environment leave this helper.

Executable paths come from the trusted native driver, never from an HTTP request.
pidfds are mandatory for stop; there is deliberately no PID-only signal fallback.
This helper does not discover, restart, checkpoint, or resume application sessions.
"""
import hashlib
import json
import os
import select
import signal
import sys
import time
from pathlib import Path

MAX_INPUT = 64 * 1024
MAX_CMDLINE = 1024 * 1024


class GuardError(Exception):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


def _allowed_executables(executable_paths):
    if not isinstance(executable_paths, list) or not 1 <= len(executable_paths) <= 16:
        raise GuardError("invalid-request")
    allowed = set()
    for filename in executable_paths:
        if not isinstance(filename, str) or not os.path.isabs(filename):
            raise GuardError("invalid-request")
        resolved = os.path.realpath(filename)
        stat = os.stat(resolved)
        if not os.path.isfile(resolved) or not os.access(resolved, os.X_OK):
            raise GuardError("invalid-request")
        allowed.add((resolved, stat.st_dev, stat.st_ino))
    return allowed


def _pid(value):
    if isinstance(value, bool) or not isinstance(value, int) or value <= 1 or value == os.getpid():
        raise GuardError("invalid-request")
    return value


def _inspect(pid, allowed):
    pid = _pid(pid)
    proc = Path("/proc") / str(pid)
    before = (proc / "stat").read_text()
    fields = before[before.rfind(")") + 2:].split()
    if len(fields) < 20:
        raise GuardError("unreviewed-process")
    start = fields[19]
    if os.stat(proc).st_uid != os.getuid():
        raise GuardError("foreign-owner")
    exe = os.readlink(proc / "exe")
    if exe.endswith(" (deleted)"):
        raise GuardError("unreviewed-process")
    executable_stat = os.stat(proc / "exe")
    if (exe, executable_stat.st_dev, executable_stat.st_ino) not in allowed:
        raise GuardError("unreviewed-process")
    with open(proc / "cmdline", "rb") as handle:
        cmdline = handle.read(MAX_CMDLINE + 1)
    if len(cmdline) > MAX_CMDLINE:
        raise GuardError("unreviewed-process")
    cwd = os.readlink(proc / "cwd")
    after = (proc / "stat").read_text()
    after_fields = after[after.rfind(")") + 2:].split()
    if len(after_fields) < 20 or after_fields[19] != start:
        raise GuardError("stale-process")
    boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    digest = hashlib.sha256()
    digest.update(json.dumps([exe, executable_stat.st_dev, executable_stat.st_ino,
                              executable_stat.st_size, executable_stat.st_mtime_ns, cwd],
                             separators=(",", ":")).encode())
    digest.update(b"\0")
    digest.update(cmdline)
    return {"pid": pid, "startTime": boot + ":" + start,
            "ownerId": str(os.getuid()), "fingerprint": digest.hexdigest()}


def inspect(pid, executable_paths):
    return _inspect(pid, _allowed_executables(executable_paths))


def stop_reviewed(identities, executable_paths, timeout_seconds=8.0):
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise GuardError("pidfd-unavailable")
    if not isinstance(identities, list) or not 1 <= len(identities) <= 128:
        raise GuardError("invalid-request")
    if not 0.01 <= timeout_seconds <= 15:
        raise GuardError("invalid-request")
    allowed = _allowed_executables(executable_paths)
    handles = []
    gone = []
    pids = set()
    try:
        # Acquire pidfds and validate the entire approval before any signal.
        for identity in identities:
            if not isinstance(identity, dict) or set(identity) != {
                    "pid", "startTime", "ownerId", "fingerprint"}:
                raise GuardError("invalid-request")
            pid = _pid(identity["pid"])
            if pid in pids:
                raise GuardError("invalid-request")
            pids.add(pid)
            try:
                fd = os.pidfd_open(pid, 0)
            except ProcessLookupError:
                gone.append(identity)
                continue
            try:
                if _inspect(pid, allowed) != identity:
                    raise GuardError("stale-process")
            except BaseException:
                os.close(fd)
                raise
            handles.append((fd, identity))
        # Recheck all live identities after opening all handles. A reused PID can
        # never redirect a signal because the signal addresses its opened pidfd.
        for fd, identity in handles:
            if select.select([fd], [], [], 0)[0]:
                continue
            if _inspect(identity["pid"], allowed) != identity:
                raise GuardError("stale-process")
        for fd, _identity in handles:
            try:
                if select.select([fd], [], [], 0)[0]:
                    continue
                if _inspect(_identity["pid"], allowed) != _identity:
                    raise GuardError("stale-process")
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + timeout_seconds
        remaining = {fd for fd, _identity in handles}
        while remaining and time.monotonic() < deadline:
            ready = select.select(list(remaining), [], [], max(0, deadline - time.monotonic()))[0]
            remaining.difference_update(ready)
        stopped = gone + [identity for fd, identity in handles if fd not in remaining]
        return {"complete": not remaining, "stopped": stopped}
    finally:
        for fd, _identity in handles:
            os.close(fd)


def main():
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT + 1)
        if len(raw) > MAX_INPUT:
            raise GuardError("invalid-request")
        request = json.loads(raw)
        if request.get("operation") == "inspect" and set(request) == {
                "operation", "pid", "executablePaths"}:
            result = {"ok": True, "identity": inspect(request["pid"], request["executablePaths"])}
        elif request.get("operation") == "stop" and set(request) == {
                "operation", "identities", "executablePaths"}:
            result = {"ok": True, **stop_reviewed(request["identities"], request["executablePaths"])}
        else:
            raise GuardError("invalid-request")
    except GuardError as error:
        result = {"ok": False, "code": error.code}
    except (FileNotFoundError, ProcessLookupError):
        result = {"ok": False, "code": "process-exited"}
    except BaseException:
        # No exception text/tracebacks: /proc/native commands may contain secrets.
        result = {"ok": False, "code": "guard-failed"}
    sys.stdout.write(json.dumps(result, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
