"""T3's managed Antigravity ACP pair, separate from the agy CLI.

Mirrors AntigravityInstallation.ts: pinned Google ZIP, SHA-256 and byte counts,
exactly two plain files and ACP initialize verification. T3 owns activation.
Old releases are never removed, even when no running session is visible.
"""

import contextlib
import hashlib
import json
import os
import pathlib
import platform
import queue
import re
import shutil
import stat
import subprocess
import threading
import tempfile
import time
import zipfile

from app_update_antigravity_release import RELEASES
from app_update_common import (
    CREATE_NO_WINDOW, Install, UpdateFailure, _stop_group, command, download,
    environment, private_temporary, result, version_text, version_tuple, write_private_json,
)


RELEASE_ID = re.compile(r"^[a-f0-9]{64}$")
RECORD = ".install-complete.json"


def platform_key(host):
    machine = platform.machine().lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "x64" if machine in ("x86_64", "amd64") else None
    system = {"ubuntu": "linux", "mac": "darwin", "windows": "win32"}.get(host)
    return system + "-" + arch if system and arch else None


def managed_root(host):
    key = platform_key(host)
    return pathlib.Path.home() / ".t3/tools/antigravity-acp" / key if key else None


def read_json(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 8192:
        raise UpdateFailure("readiness_unknown")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise UpdateFailure("readiness_unknown")
    return value


def completed(root, release_id, host):
    if not isinstance(release_id, str) or not RELEASE_ID.fullmatch(release_id):
        raise UpdateFailure("readiness_unknown")
    directory = root / "versions" / release_id
    if directory.is_symlink():
        raise UpdateFailure("readiness_unknown")
    record = read_json(directory / RECORD)
    if record.get("releaseId") != release_id or not isinstance(record.get("version"), str) or not record.get("version") or version_text(record.get("version")) != record.get("version"):
        raise UpdateFailure("readiness_unknown")
    for kind, name in (("executable", "agy_acp_server" + (".exe" if host == "windows" else ".par")),
                       ("harness", "localharness_external" + (".exe" if host == "windows" else ""))):
        item = record.get(kind)
        if (not isinstance(item, dict) or item.get("name") != name or
                type(item.get("bytes")) is not int or item["bytes"] <= 0):
            raise UpdateFailure("readiness_unknown")
        path = directory / name
        if (path.is_symlink() or not path.is_file() or path.stat().st_size != item["bytes"] or
                (host != "windows" and not path.stat().st_mode & 0o111)):
            raise UpdateFailure("readiness_unknown")
    return record


def detect_antigravity_acp(host):
    root = managed_root(host)
    if root is None:
        return None
    active = root / "active.json"
    if not active.exists() and not active.is_symlink():
        return None
    try:
        release_id = read_json(active).get("releaseId")
        record = completed(root, release_id, host)
        return Install("antigravity-acp", host, root, record["version"], "official-download")
    except (UpdateFailure, OSError, ValueError, TypeError):
        return Install("antigravity-acp", host, root, manager="official-download", probe="failed")


def budget(deadline, maximum):
    remaining = maximum if deadline is None else min(maximum, deadline - time.monotonic())
    if remaining <= 0:
        raise UpdateFailure("timeout")
    return remaining


def has_capability(value):
    # ACP uses empty objects for capabilities; they are truthy in T3's JS.
    return isinstance(value, dict) or value is True


def matches_asset(record, asset, host):
    if record.get("version") != asset["version"]:
        return False
    for kind in ("executable", "harness"):
        if record[kind]["name"] != asset[kind]["name"]:
            return False
        # T3 re-signs Google's Intel Mac pair ad hoc after ZIP verification.
        # Its completion record stores the resulting file sizes.
        if platform_key(host) != "darwin-x64" and record[kind]["bytes"] != asset[kind]["bytes"]:
            return False
    return True


def validate_runtime(directory, asset, host, deadline):
    """Unauthenticated ACP initialize, with a disposable GEMINI_HOME, as T3 uses.

    No session/new, authenticate or prompt is sent. Stop only this validation
    process tree; installed/running Antigravity sessions are never scanned.
    """
    with private_temporary() as profile:
        (profile / "antigravity-acp").mkdir(mode=0o700)
        write_private_json(profile / "antigravity-acp/settings.json", {"auth": {"type": "oauth-personal"}})
        env = environment()
        for key in ("GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_APPLICATION_CREDENTIALS",
                    "GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_LOCATION", "GOOGLE_CLOUD_QUOTA_PROJECT",
                    "GOOGLE_GENAI_USE_VERTEXAI", "GCLOUD_PROJECT", "CLOUDSDK_CORE_PROJECT",
                    "AGY_ACP_CCPA_PROJECT", "AGY_ACP_ENABLE_OAUTH", "CHROME_CRASHPAD_PIPE_NAME"):
            env.pop(key, None)
        env.update(GEMINI_HOME=str(profile), AGY_ACP_FORCE_FILE_STORAGE="1", PYTHONUNBUFFERED="1",
                   ELECTRON_RUN_AS_NODE="1", ANTIGRAVITY_HARNESS_PATH=str(directory / asset["harness"]["name"]),
                   BROWSER="false", TEMP=str(profile), TMP=str(profile), TMPDIR=str(profile))
        if host == "ubuntu" and not env.get("SSL_CERT_FILE") and not env.get("SSL_CERT_DIR"):
            if pathlib.Path("/etc/ssl/certs/ca-certificates.crt").is_file():
                env["SSL_CERT_FILE"] = "/etc/ssl/certs/ca-certificates.crt"
        argv = [str(directory / asset["executable"]["name"])] + (["--uid="] if host == "ubuntu" else [])
        options = {"creationflags": CREATE_NO_WINDOW} if host == "windows" else {"start_new_session": True}
        process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, cwd=profile, env=env, **options)
        responses = queue.Queue(maxsize=1)

        def read_response():
            try:
                for _ in range(128):
                    line = process.stdout.readline(65537)
                    if not line or len(line) > 65536:
                        break
                    value = json.loads(line)
                    if isinstance(value, dict) and value.get("id") == 1:
                        responses.put(value.get("result"))
                        return
            except (OSError, ValueError):
                pass
            responses.put(None)

        reader = threading.Thread(target=read_response, daemon=True)
        try:
            reader.start()
            request = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": 1, "clientCapabilities": {},
                "clientInfo": {"name": "aac-runtime-validation", "version": "1.0.0"},
            }}
            process.stdin.write((json.dumps(request) + "\n").encode("utf-8"))
            process.stdin.flush()
            try:
                initialized = responses.get(timeout=budget(deadline, 90))
            except queue.Empty:
                raise UpdateFailure("timeout") from None
            if not isinstance(initialized, dict):
                raise UpdateFailure("signature_failed")
            info, capabilities = initialized.get("agentInfo"), initialized.get("agentCapabilities")
            if not isinstance(info, dict) or not isinstance(capabilities, dict):
                raise UpdateFailure("signature_failed")
            sessions, auth = capabilities.get("sessionCapabilities"), capabilities.get("auth")
            methods = initialized.get("authMethods")
            if not isinstance(sessions, dict) or not isinstance(auth, dict) or not isinstance(methods, list):
                raise UpdateFailure("signature_failed")
            if (info.get("name") != "antigravity-acp" or info.get("version") != asset["version"] or
                    initialized.get("protocolVersion") not in (1, 2) or capabilities.get("loadSession") is not True or
                    not has_capability(sessions.get("resume")) or
                    not has_capability(auth.get("logout")) or
                    not any(item.get("id") == "oauth-personal" for item in methods
                            if isinstance(item, dict))):
                raise UpdateFailure("signature_failed")
        finally:
            _stop_group(process, tree=True)
            reader.join(timeout=1)
            with contextlib.suppress(OSError):
                process.stdin.close()
                process.stdout.close()


def extract_runtime(archive, directory, asset):
    expected = {asset[kind]["name"]: asset[kind]["bytes"] for kind in ("executable", "harness")}
    with zipfile.ZipFile(archive) as source:
        entries = source.infolist()
        if len(entries) != 2 or {entry.filename for entry in entries} != set(expected):
            raise UpdateFailure("signature_failed")
        for entry in entries:
            mode = (entry.external_attr >> 16) & 0o170000
            if (entry.file_size != expected[entry.filename] or mode not in (0, stat.S_IFREG) or
                    entry.external_attr & 0x10 or entry.flag_bits & 1 or
                    entry.compress_type not in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED)):
                raise UpdateFailure("signature_failed")
            target = directory / entry.filename
            with source.open(entry) as incoming, target.open("xb") as outgoing:
                shutil.copyfileobj(incoming, outgoing, 256 * 1024)
            if target.stat().st_size != expected[entry.filename]:
                raise UpdateFailure("signature_failed")
            target.chmod(0o755)


def update_antigravity_acp(install, deadline=None, progress=None):
    before, attempted = install.version, False
    asset = RELEASES.get(platform_key(install.platform))
    if asset is None:
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "unsupported")
    try:
        # Never downgrade a release T3 installed after this AAC package's pin.
        if version_tuple(before) >= version_tuple(asset["version"]):
            return result(install.app_id, install.platform, "current", before, before, install.manager)
        root = managed_root(install.platform)
        versions = root / "versions"
        destination = versions / asset["sha256"]
        attempted = True
        if destination.exists() or destination.is_symlink():
            record = completed(root, asset["sha256"], install.platform)
            if not matches_asset(record, asset, install.platform):
                raise UpdateFailure("signature_failed")
            validate_runtime(destination, asset, install.platform, deadline)
        else:
            needed = asset["archiveBytes"] + asset["executable"]["bytes"] + asset["harness"]["bytes"] + 256 * 1024 * 1024
            if shutil.disk_usage(versions).free < needed:
                raise UpdateFailure()
            # Same filesystem: rename publishes the complete pair in one step.
            with tempfile.TemporaryDirectory(prefix=".install-", dir=versions) as staging:
                staging = pathlib.Path(staging)
                archive, pair = staging / "download.zip", staging / "runtime"
                pair.mkdir(mode=0o700)
                if progress:
                    progress("downloading")
                download(asset["url"], archive, maximum=asset["archiveBytes"], timeout=budget(deadline, 900))
                digest = hashlib.sha256()
                with archive.open("rb") as stream:
                    for chunk in iter(lambda: stream.read(256 * 1024), b""):
                        digest.update(chunk)
                if archive.stat().st_size != asset["archiveBytes"] or digest.hexdigest() != asset["sha256"]:
                    raise UpdateFailure("signature_failed")
                if progress:
                    progress("updating")
                extract_runtime(archive, pair, asset)
                if platform_key(install.platform) == "darwin-x64":
                    command(["/usr/bin/codesign", "--force", "--sign", "-", pair / asset["executable"]["name"],
                             pair / asset["harness"]["name"]], timeout=budget(deadline, 30))
                validate_runtime(pair, asset, install.platform, deadline)
                record = {"releaseId": asset["sha256"], "version": asset["version"]}
                for kind in ("executable", "harness"):
                    name = asset[kind]["name"]
                    record[kind] = {"name": name, "bytes": (pair / name).stat().st_size}
                write_private_json(pair / RECORD, record)
                try:
                    pair.rename(destination)
                except OSError:
                    # T3 may have installed the same release concurrently.
                    record = completed(root, asset["sha256"], install.platform)
                    if not matches_asset(record, asset, install.platform):
                        raise UpdateFailure("signature_failed") from None
                    validate_runtime(destination, asset, install.platform, deadline)
        budget(deadline, 1)
        current = detect_antigravity_acp(install.platform)
        if current is None or current.probe:
            raise UpdateFailure("readiness_unknown")
        if version_tuple(current.version) >= version_tuple(asset["version"]):
            # Another T3 install won the race with a newer pin. Keep it.
            return result(install.app_id, install.platform, "current", before, current.version, install.manager,
                          attempted=True)
        # T3's ab8e488e installer uses an in-process semaphore and unconditional
        # rename, with no shared lock/CAS protocol. Replacing the pointer here
        # can erase an activation we never observed, even with a post-write read.
        # Publish only the verified pair; let T3 activate it in provider settings.
        return result(install.app_id, install.platform, "action_required", before, current.version, install.manager,
                      "antigravity_acp_staged", True)
    except (UpdateFailure, OSError, ValueError, TypeError, zipfile.BadZipFile) as error:
        refreshed = detect_antigravity_acp(install.platform)
        after = refreshed.version if refreshed else None
        return result(install.app_id, install.platform, "failed", before, after, install.manager,
                      error.code if isinstance(error, UpdateFailure) else "update_failed", attempted)
