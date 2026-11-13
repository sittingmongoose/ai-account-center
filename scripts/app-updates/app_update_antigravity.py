"""Read-only discovery and T3-owned Antigravity ACP installation.

AAC never opens, replaces or repairs active.json. T3's running server owns
release selection, validation, publication and activation through
provider.install.start; AAC follows the target T3 offers and the operation T3
returns (contract: T3 0.1.10-nightly.20261113.4616, AntigravityInstallation.ts).
"""

import contextlib
import json
import os
import pathlib
import platform
import re
import socket
import time
import urllib.request

from app_update_antigravity_release import CLI_MANIFEST_BASE, NATIVE_FROM, SIGNIN_CARRIES_TO_NATIVE
from app_update_common import Install, UpdateFailure, diagnostics, result, version_tuple
import app_update_t3_rpc as t3_rpc

RELEASE_ID = re.compile(r"^[a-f0-9]{64}$")
RECORD = ".install-complete.json"
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")
# T3 accepts older non-semver records (agy_acp_server_1.1.1); bounded plain words only.
STATE_VERSION = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$")
# How long a subscription may stay quiet while T3's background release check runs
# (its manifest read is bounded at 10 seconds).
OFFER_SETTLE_SECONDS = 15
# A T3 server this run did not replace gets one short retry window, not the full wait.
QUIET_READY_SECONDS = 15
# T3 bounds one instance's model refresh at 90 seconds.
REFRESH_SECONDS = 120
SIGNIN_WORDS = re.compile(r"(?i)sign[- ]?in|log[- ]?in|logged in|not authenticated|unauthenticated|re-?authenticate")


def platform_key(host):
    machine = platform.machine().lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "x64" if machine in ("x86_64", "amd64") else None
    system = {"ubuntu": "linux", "mac": "darwin", "windows": "win32"}.get(host)
    return system + "-" + arch if system and arch else None


def cli_target(host):
    """T3's antigravityCliTarget: the CLI channel's manifest name for this computer."""
    machine = platform.machine().lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "amd64" if machine in ("x86_64", "amd64") else None
    system = {"ubuntu": "linux", "mac": "darwin", "windows": "windows"}.get(host)
    if arch is None or system is None:
        return None
    musl = host == "ubuntu" and pathlib.Path("/lib/libc.musl-" + ("aarch64" if arch == "arm64" else "x86_64") + ".so.1").exists()
    return system + "_" + arch + ("_musl" if musl else "")


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


def _artifact(directory, item, name, host):
    if (not isinstance(item, dict) or item.get("name") != name or
            type(item.get("bytes")) is not int or not 0 < item["bytes"] <= 9007199254740991):
        raise UpdateFailure("readiness_unknown")
    path = directory / name
    if (path.is_symlink() or not path.is_file() or path.stat().st_size != item["bytes"] or
            (host != "windows" and not path.stat().st_mode & 0o111)):
        raise UpdateFailure("readiness_unknown")


def completed(root, release_id, host):
    """A finished T3 install record, in either shape T3 writes (completedRelease):

    - the 1.3/1.4 runtime pair: executable agy_acp_server(.par|.exe) plus harness localharness_external(.exe);
    - the native 1.5+ CLI: executable antigravity (agy.exe on Windows) and "harness": null.
    """
    if not isinstance(release_id, str) or not RELEASE_ID.fullmatch(release_id):
        raise UpdateFailure("readiness_unknown")
    directory = root / "versions" / release_id
    if directory.is_symlink():
        raise UpdateFailure("readiness_unknown")
    record = read_json(directory / RECORD)
    version = record.get("version")
    if (record.get("releaseId") != release_id or not isinstance(version, str) or
            not STATE_VERSION.fullmatch(version)):
        raise UpdateFailure("readiness_unknown")
    if "harness" in record and record["harness"] is None:
        _artifact(directory, record.get("executable"), "agy.exe" if host == "windows" else "antigravity", host)
        return {**record, "kind": "native"}
    _artifact(directory, record.get("executable"), "agy_acp_server" + (".exe" if host == "windows" else ".par"), host)
    _artifact(directory, record.get("harness"), "localharness_external" + (".exe" if host == "windows" else ""), host)
    return {**record, "kind": "harness"}


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
        return Install("antigravity-acp", host, root, record["version"], "official-download",
                       runtime_kind=record["kind"], owner={"manager": "t3", "runtime": record["kind"]})
    except (UpdateFailure, OSError, ValueError, TypeError):
        return Install("antigravity-acp", host, root, manager="official-download", probe="failed",
                       probe_reason="runtime_record_invalid")


def newest_cli_version(host, deadline):
    """The CLI channel's newest version for this computer (what T3's Install fetches), or None."""
    target = cli_target(host)
    seconds = min(10, deadline - time.monotonic())
    if target is None or seconds <= 0:
        return None
    try:
        request = urllib.request.Request(CLI_MANIFEST_BASE + target + ".json", headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
        with urllib.request.urlopen(request, timeout=seconds) as response:
            if not response.geturl().startswith(CLI_MANIFEST_BASE):
                return None
            raw = response.read(16 * 1024 + 1)
        value = json.loads(raw.decode("utf-8")) if len(raw) <= 16 * 1024 else None
        version = value.get("version") if isinstance(value, dict) else None
        return version if isinstance(version, str) and SEMVER.fullmatch(version) and version_tuple(version) >= NATIVE_FROM else None
    except Exception:
        return None


def checked_state(value):
    """One ProviderInstallState. `version` is T3's offer and may be null until its release check ends."""
    if (not isinstance(value, dict) or value.get("driver") != "antigravity" or
            value.get("phase") not in ("idle", "downloading", "extracting", "verifying", "succeeded", "failed", "cancelled")):
        raise UpdateFailure("t3_install_failed:protocol")
    for key in ("version", "installedVersion"):
        if value.get(key) is not None and not (isinstance(value[key], str) and STATE_VERSION.fullmatch(value[key])):
            raise UpdateFailure("t3_install_failed:protocol")
    return value


def read_offer(client, instance, seconds=OFFER_SETTLE_SECONDS):
    """T3's current offer: the latest subscription state, waiting a bounded while for a non-null version.

    Subscribing starts T3's background release check, so the first state can
    precede it. A quiet stream past the settle time ends the read; the client
    reconnects for the next request with the same session."""
    client.refresh()
    rpc = client.client
    rpc.settle = time.monotonic() + seconds
    latest, identifier = None, None
    try:
        identifier = rpc.request("provider.install.subscribe", {"instanceId": instance})
        for value in rpc.receive(identifier):
            latest = checked_state(value)
            if latest.get("version") and latest["phase"] in ("idle", "succeeded", "failed", "cancelled"):
                break
    except (TimeoutError, socket.timeout):
        client.disconnect()
        return latest
    finally:
        if client.client is rpc:
            rpc.settle = None
            with contextlib.suppress(UpdateFailure, OSError):
                rpc.send({"_tag": "Interrupt", "requestId": identifier, "interruptors": []})
    return latest


def refresh_models(client, instances):
    """Ask T3 to rebuild every enabled Antigravity instance's model list after an install.

    T3 refreshes only the instance an install started on, and only when it is
    enabled. One request per instance keeps another provider's failure from
    hiding this one's. A refusal that mentions sign-in (or an instance that now
    reads unauthenticated) means the new runtime cannot use the stored sign-in.
    Returns (outcome, reason): ("ok" | "signin" | "failed", word)."""
    outcome = ("ok", None)
    for instance in instances:
        try:
            reply = client.call("server.refreshProviders", {"instanceId": instance, "refreshModels": True})
        except UpdateFailure as error:
            if SIGNIN_WORDS.search(error.output or ""):
                return ("signin", "refresh_signin")
            outcome = ("failed", "refresh_timed_out" if "timed out" in (error.output or "").lower() else "refresh_failed")
            continue
        except (OSError, TimeoutError):
            outcome = ("failed", "refresh_failed")
            continue
        providers = reply.get("providers") if isinstance(reply, dict) else None
        for provider in providers if isinstance(providers, list) else []:
            if (isinstance(provider, dict) and provider.get("instanceId") == instance and
                    isinstance(provider.get("auth"), dict) and provider["auth"].get("status") == "unauthenticated"):
                return ("signin", "unauthenticated")
    return outcome


def restart_pending(t3_row):
    """Whether T3 changed in this run without its new server running yet (the Linux deferred restart)."""
    if not isinstance(t3_row, dict) or t3_row.get("status") == "current":
        return False
    if any(isinstance(t, dict) and t.get("kind") == "systemd" for t in t3_row.get("restartTargets") or []):
        return True
    return t3_row.get("platform") == "ubuntu" and t3_row.get("updateAttempted") is True


def replaced_version(t3_row):
    """The server version T3's own row put in place in this run (Mac and Windows reopen it at once)."""
    if isinstance(t3_row, dict) and t3_row.get("status") == "updated" and isinstance(t3_row.get("version"), str):
        return t3_row["version"]
    return None


def newer(left, right):
    return version_tuple(left) > version_tuple(right)


def update_antigravity_acp(install, deadline=None, progress=None, t3_row=None):
    before, attempted = install.version, False
    record = diagnostics()
    record.owner = install.owner or {"manager": "t3"}
    deadline = min(deadline or float("inf"), time.monotonic() + 900)
    if platform_key(install.platform) is None:
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "unsupported")
    if restart_pending(t3_row):
        # The running server is the one this run replaced: its offer is the previous server's.
        record.blame("t3_restart_pending", "deferred_server_restart")
        return result(install.app_id, install.platform, "action_required", before, before, install.manager, "t3_restart_pending")
    try:
        record.phase("t3_ready")
        # A server replaced in this run gets time to come up; one left alone either answers or does not.
        replaced = replaced_version(t3_row)
        t3_rpc.wait_ready(install.platform, deadline, replaced, seconds=t3_rpc.READY_SECONDS if replaced else QUIET_READY_SECONDS)
        record.phase("discover")
        cli, base, origin = t3_rpc.discover(install.platform)
        with contextlib.closing(t3_rpc.InstallRpc(cli, base, origin, deadline)) as client:
            record.phase("offer")
            config = client.call("server.getConfig", {})
            providers = [p for p in (config.get("providers") or []) if isinstance(p, dict) and p.get("driver") == "antigravity"]
            installable = [p for p in providers if (p.get("setup") or {}).get("canInstall") is True]
            # T3 refreshes models after an install only for the instance it ran on, and only when enabled.
            enabled = [p["instanceId"] for p in installable if p.get("enabled") is True and isinstance(p.get("instanceId"), str)]
            if not enabled:
                raise UpdateFailure("t3_install_failed:provider_unavailable",
                                    reason="no_enabled_instance" if installable else "no_installable_instance")
            instance = enabled[0]
            offer = read_offer(client, instance)
            offered = offer.get("version") if offer else None
            newest = newest_cli_version(install.platform, deadline)
            target = offered if offered and SEMVER.fullmatch(offered) else newest
            current = detect_antigravity_acp(install.platform)
            if current is None or current.probe:
                raise UpdateFailure("t3_install_failed:readiness_unknown", reason="runtime_record_invalid")
            record.owner = {**(current.owner or {}), "target": "t3-offer" if target == offered else "cli-manifest"}
            if target is None:
                record.blame("unknown", "no_offer")
                return result(install.app_id, install.platform, "unknown", before, current.version, install.manager,
                              "antigravity_acp_offer_unknown")
            if not newer(target, current.version):
                return result(install.app_id, install.platform, "current", before, current.version, install.manager, "current")
            if (current.runtime_kind == "harness" and version_tuple(target) >= NATIVE_FROM and
                    install.platform not in SIGNIN_CARRIES_TO_NATIVE):
                # The native CLI cannot read the sign-in the runtime pair stored here.
                record.blame("signin_needed", "token_storage_change")
                value = result(install.app_id, install.platform, "action_required", before, current.version,
                               install.manager, "antigravity_acp_signin_hold")
                value["heldVersion"] = target
                return value
            if progress:
                progress("updating")
            record.phase("install")
            attempted = True
            state = checked_state(client.call("provider.install.start", {"instanceId": instance}))
            operation = state.get("operationId")
            if not isinstance(operation, str) or not operation:
                raise UpdateFailure("t3_install_failed:protocol", reason="no_operation")
            # T3 re-reads the release at start; follow what this operation installs.
            target = state.get("version") if state.get("version") and SEMVER.fullmatch(state["version"]) else target
            with contextlib.closing(client.subscribe(instance)) as states:
                while state["phase"] not in ("succeeded", "failed", "cancelled"):
                    state = checked_state(next(states))
                    if state.get("operationId") != operation:
                        raise UpdateFailure("t3_install_state_unknown", reason="operation_changed")
            if state["phase"] != "succeeded":
                # Server messages can contain local paths or credentials.
                # Emit only a fixed reason, never arbitrary diagnostic text.
                raise UpdateFailure("t3_install_failed:" + ("cancelled" if state["phase"] == "cancelled" else "installer"))
            record.phase("verify")
            final = detect_antigravity_acp(install.platform)
            if final is None or final.probe or newer(target, final.version):
                raise UpdateFailure("t3_install_failed:verification")
            record.owner = {**(final.owner or {}), "target": record.owner.get("target")}
            record.phase("refresh_models")
            outcome, reason = refresh_models(client, enabled)
            if outcome == "signin":
                record.blame("signin_needed", reason)
                return result(install.app_id, install.platform, "action_required", before, final.version,
                              install.manager, "antigravity_acp_signin_needed", attempted)
            if outcome == "failed":
                record.note(reason)
                return result(install.app_id, install.platform, "updated", before, final.version,
                              install.manager, "antigravity_acp_models_stale", attempted)
            return result(install.app_id, install.platform, "updated", before, final.version,
                          install.manager, "antigravity_acp_updated", attempted)
    except (UpdateFailure, OSError, ValueError, TypeError, KeyError, AttributeError, StopIteration) as error:
        refreshed = detect_antigravity_acp(install.platform)
        after = refreshed.version if refreshed else None
        code = error.code if isinstance(error, UpdateFailure) else (
            "t3_install_timeout" if isinstance(error, (TimeoutError, socket.timeout)) else
            "t3_unreachable" if isinstance(error, OSError) else "t3_install_failed:protocol")
        if isinstance(error, UpdateFailure) and (error.cause or error.reason):
            record.blame(error.cause, error.reason)
        status = "action_required" if code in ("t3_unreachable", "t3_auth_failed", "t3_absent", "t3_not_ready") else "failed"
        return result(install.app_id, install.platform, status, before, after, install.manager, code, attempted)
