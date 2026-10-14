"""Private, exact-credential request budget for Muse's key-minting endpoint.

This cache cannot establish a portal team. Only the normalized native reading
is stored; inference keys and response bodies never reach disk.
"""
from contextlib import contextmanager
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import stat
import tempfile
import time

from muse_console import MuseError, ERROR_MESSAGES, REFRESH_SECONDS, MAX_RETRY_SECONDS
from plan_common import CollectionError
from desktop_helpers import safe_text

MAX_BYTES = 128 * 1024
TRANSIENT = {"rate_limited", "network_error", "provider_error"}


def _linked(info):
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, "st_file_attributes", 0) & 0x400)


def _owned(info):
    return os.name != "posix" or (info.st_uid == os.getuid() and info.st_mode & 0o077 == 0)


def _regular(info):
    return not _linked(info) and stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and _owned(info)


def _root(root):
    root = Path(root).absolute()
    for parent in [*reversed(root.parents), root]:
        if parent.exists() or parent.is_symlink():
            info = parent.lstat()
            if _linked(info) or not stat.S_ISDIR(info.st_mode):
                raise MuseError("local_storage_error")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not _owned(root.stat()):
        raise MuseError("local_storage_error")
    return root


@contextmanager
def _lock(root, key):
    file = root / (".muse-native-" + key + ".lock")
    if file.exists() or file.is_symlink():
        if not _regular(file.lstat()):
            raise MuseError("local_storage_error")
    descriptor = os.open(file, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0) |
                         getattr(os, "O_BINARY", 0), 0o600)
    locked = False
    try:
        actual, linked = os.fstat(descriptor), file.lstat()
        if not _regular(actual) or _linked(linked) or (actual.st_dev, actual.st_ino) != (linked.st_dev, linked.st_ino):
            raise MuseError("local_storage_error")
        if actual.st_size == 0:
            os.write(descriptor, b"\0")
        deadline = time.monotonic() + 2
        while True:
            try:
                if os.name == "nt":
                    import msvcrt
                    os.lseek(descriptor, 0, os.SEEK_SET)
                    msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
                locked = True
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise MuseError("busy") from None
                time.sleep(.025)
        yield
    finally:
        if locked and os.name == "nt":
            import msvcrt
            os.lseek(descriptor, 0, os.SEEK_SET)
            msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
        os.close(descriptor)


def _sample(value, platform):
    # The same strict output boundary handles in-memory and persisted readings.
    from failover_usage import public_sample
    return public_sample(value, "muse", platform)


def _read(root, key, platform, now):
    file = root / ("muse-native-" + key + ".json")
    if not file.exists() and not file.is_symlink():
        return None
    before = file.lstat()
    if not _regular(before) or before.st_size > MAX_BYTES:
        raise MuseError("local_storage_error")
    descriptor = os.open(file, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    with os.fdopen(descriptor, "rb") as handle:
        opened = os.fstat(handle.fileno())
        if not _regular(opened) or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
            raise MuseError("local_storage_error")
        raw = handle.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES:
        raise MuseError("local_storage_error")
    value = json.loads(raw)
    if (not isinstance(value, dict) or set(value) != {"version", "key", "sample", "failureCode", "nextRequestAt", "observedPlan"}
            or type(value["version"]) is not int or value["version"] != 1 or value["key"] != key
            or value["failureCode"] is not None and value["failureCode"] not in ERROR_MESSAGES
            or isinstance(value["nextRequestAt"], bool) or not isinstance(value["nextRequestAt"], (int, float))
            or not math.isfinite(value["nextRequestAt"]) or not 0 <= value["nextRequestAt"] <= now + MAX_RETRY_SECONDS):
        raise MuseError("local_storage_error")
    if value["observedPlan"] is not None and safe_text(value["observedPlan"]) != value["observedPlan"]:
        raise MuseError("local_storage_error")
    sample = None if value["sample"] is None else _sample(value["sample"], platform)
    if value["sample"] is not None and sample is None:
        raise MuseError("local_storage_error")
    value["sample"] = sample
    return value


def _write(root, key, state):
    payload = json.dumps(state, allow_nan=False, separators=(",", ":")).encode()
    if len(payload) > MAX_BYTES:
        raise MuseError("local_storage_error")
    descriptor, temporary = tempfile.mkstemp(prefix=".muse-native-", dir=root)
    try:
        with os.fdopen(descriptor, "wb") as output:
            if os.name == "posix":
                os.fchmod(output.fileno(), 0o600)
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, root / ("muse-native-" + key + ".json"))
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def _result(state, now, cached):
    sample = copy.deepcopy(state["sample"])
    if sample is not None and cached:
        sample["status"] = "cached"
    result = {"sample": sample, "failureCode": state["failureCode"]}
    if state["observedPlan"] is not None:
        result["observedPlan"] = state["observedPlan"]
    remaining = min(MAX_RETRY_SECONDS, max(0, state["nextRequestAt"] - now))
    if remaining > 0:
        if state["failureCode"] == "rate_limited":
            result["retryAfterSeconds"] = remaining
        elif state["failureCode"] is None:
            result["refreshAfterSeconds"] = remaining
    return result


def collect(root, access, expected_email, platform, loader, now=None):
    """Reserve the request before calling /key, including on the first reading."""
    now = time.time() if now is None else now
    key = hashlib.sha256(("muse-native-v1:" + access).encode()).hexdigest()
    try:
        root = _root(root)
        with _lock(root, key):
            state = _read(root, key, platform, now)
            if (state and state["sample"] and expected_email and
                    (state["sample"]["email"] or "").lower() != expected_email.lower()):
                state.update(sample=None, failureCode="account_mismatch")
                _write(root, key, state)
                return _result(state, now, True)
            if state and state["nextRequestAt"] > now:
                return _result(state, now, True)
            previous = state["sample"] if state else None
            previous_plan = state["observedPlan"] if state else None
            state = {"version": 1, "key": key, "sample": previous, "failureCode": None,
                     "nextRequestAt": now + REFRESH_SECONDS, "observedPlan": previous_plan}
            _write(root, key, state)
            try:
                response = loader()
            except CollectionError as error:
                response = {"sample": None, "failureCode": error.failure_code or
                            ("needs_sign_in" if error.status == "needs_sign_in" else "provider_error"),
                            "retryAfterSeconds": error.retry_after}
            except Exception:
                response = {"sample": None, "failureCode": "error"}
            code = response.get("failureCode")
            if code is not None and code not in ERROR_MESSAGES:
                code = "error"
            sample = _sample(response.get("sample"), platform)
            if sample is None and code is None:
                code = "invalid_response"
            verified_plan = response.get("_verifiedPlan")
            if safe_text(verified_plan):
                state["observedPlan"] = safe_text(verified_plan)
                if previous is not None and verified_plan != previous["plan"]:
                    previous = None
            elif sample is not None:
                state["observedPlan"] = sample["plan"]
            state.update(sample=sample if sample is not None else previous if code in TRANSIENT else None,
                         failureCode=code)
            if code == "rate_limited":
                retry = response.get("retryAfterSeconds")
                retry = retry if isinstance(retry, (int, float)) and not isinstance(retry, bool) and math.isfinite(retry) else 600
                state["nextRequestAt"] = now + min(MAX_RETRY_SECONDS, max(REFRESH_SECONDS, retry))
            _write(root, key, state)
            return _result(state, now, sample is None)
    except MuseError as error:
        return {"sample": None, "failureCode": error.code}
    except (OSError, ValueError, TypeError):
        return {"sample": None, "failureCode": "local_storage_error"}
