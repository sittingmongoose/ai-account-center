"""Read-only discovery and T3-owned Antigravity ACP installation.

AAC never opens, replaces or repairs active.json. T3's running server owns
validation, publication and activation through provider.install.start.
"""

import contextlib
import json
import pathlib
import platform
import re
import socket
import time

from app_update_antigravity_release import RELEASES
from app_update_common import Install, UpdateFailure, result, version_text, version_tuple
import app_update_t3_rpc as t3_rpc

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
                type(item.get("bytes")) is not int or not 0 < item["bytes"] <= 9007199254740991):
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


def install_state(client, instance):
    identifier = client.request("provider.install.subscribe", {"instanceId": instance})
    try:
        yield from client.receive(identifier)
    finally:
        with contextlib.suppress(UpdateFailure, OSError):
            client.send({"_tag": "Interrupt", "requestId": identifier, "interruptors": []})


def checked_state(value):
    if (not isinstance(value, dict) or value.get("driver") != "antigravity" or
            value.get("phase") not in ("idle", "downloading", "extracting", "verifying", "succeeded", "failed", "cancelled") or
            not isinstance(value.get("version"), str) or version_text(value["version"]) != value["version"]):
        raise UpdateFailure("t3_install_failed:protocol")
    return value


def update_antigravity_acp(install, deadline=None, progress=None):
    before, attempted = install.version, False
    deadline = min(deadline or float("inf"), time.monotonic() + 900)
    asset = RELEASES.get(platform_key(install.platform))
    if asset is None:
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "unsupported")
    try:
        cli, base, origin = t3_rpc.discover(install.platform)
        with t3_rpc.bearer(cli, base, deadline) as token:
            with contextlib.closing(t3_rpc.Rpc(origin, token, deadline)) as client:
                config = client.call("server.getConfig", {})
                instances = [p["instanceId"] for p in config.get("providers", [])
                             if p.get("driver") == "antigravity" and p.get("setup", {}).get("canInstall")]
                if not instances:
                    raise UpdateFailure("t3_install_failed:provider_unavailable")
                instance = instances[0]
                # Learn the RUNNING server's pin, which can be newer than AAC's
                # offline pin. Never issue start against an older T3 server.
                with contextlib.closing(install_state(client, instance)) as states:
                    initial = checked_state(next(states))
                pin = initial["version"]
                current = detect_antigravity_acp(install.platform)
                if current is None or current.probe:
                    raise UpdateFailure("t3_install_failed:readiness_unknown")
                if version_tuple(current.version) >= max(version_tuple(pin), version_tuple(asset["version"])):
                    code = "antigravity_acp_newer_kept" if version_tuple(current.version) > version_tuple(pin) else "current"
                    return result(install.app_id, install.platform, "current", before, current.version, install.manager, code)
                if version_tuple(pin) < version_tuple(asset["version"]):
                    raise UpdateFailure("t3_install_failed:server_outdated")
                if progress:
                    progress("updating")
                attempted = True
                state = checked_state(client.call("provider.install.start", {"instanceId": instance}))
                operation = state.get("operationId")
                if not isinstance(operation, str) or not operation or state["version"] != pin:
                    raise UpdateFailure("t3_install_failed:protocol")
                with contextlib.closing(install_state(client, instance)) as states:
                    while state["phase"] not in ("succeeded", "failed", "cancelled"):
                        state = checked_state(next(states))
                        if state.get("operationId") != operation or state["version"] != pin:
                            raise UpdateFailure("t3_install_failed:operation_changed")
                if state["phase"] != "succeeded":
                    # Server messages can contain local paths or credentials.
                    # Emit only a fixed reason, never arbitrary diagnostic text.
                    raise UpdateFailure("t3_install_failed:" + ("cancelled" if state["phase"] == "cancelled" else "installer"))
                final = detect_antigravity_acp(install.platform)
                if final is None or final.probe or version_tuple(final.version) < version_tuple(pin):
                    raise UpdateFailure("t3_install_failed:verification")
                return result(install.app_id, install.platform, "updated", before, final.version,
                              install.manager, "antigravity_acp_updated", attempted)
    except (UpdateFailure, OSError, ValueError, TypeError, KeyError, AttributeError, StopIteration) as error:
        refreshed = detect_antigravity_acp(install.platform)
        after = refreshed.version if refreshed else None
        code = error.code if isinstance(error, UpdateFailure) else (
            "t3_install_timeout" if isinstance(error, (TimeoutError, socket.timeout)) else
            "t3_unreachable" if isinstance(error, OSError) else "t3_install_failed:protocol")
        return result(install.app_id, install.platform, "action_required" if code in ("t3_unreachable", "t3_auth_failed") else "failed",
                      before, after, install.manager, code, attempted)
