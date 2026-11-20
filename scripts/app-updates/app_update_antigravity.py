"""Read-only discovery and T3-owned Antigravity ACP installation.

AAC never opens, replaces or repairs active.json. T3's running server owns
release selection, validation, publication and activation through
provider.install.start; AAC follows the target T3 offers and the operation T3
returns (contract: T3 0.1.10-nightly.20261113.4616, AntigravityInstallation.ts).
"""

import contextlib
import dataclasses
import json
import os
import pathlib
import platform
import re
import socket
import time

from app_update_antigravity_release import CLI_MANIFEST_BASE, NATIVE_FROM, SIGNIN_CARRIES_TO_NATIVE
from app_update_common import Install, UpdateFailure, command, diagnostics, result, scrub, version_tuple
import app_update_t3_rpc as t3_rpc

RELEASE_ID = re.compile(r"^[a-f0-9]{64}$")
RECORD = ".install-complete.json"
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")
# T3 accepts older non-semver records (agy_acp_server_1.1.1); bounded plain words only.
STATE_VERSION = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$")
# How long a subscription may stay quiet while T3's background release check runs
# (its manifest read is bounded at 10 seconds).
OFFER_SETTLE_SECONDS = 15
MANIFEST_SECONDS = 10
MANIFEST_LIMIT = 16 * 1024
# A T3 server this run did not replace gets one short retry window, not the full wait.
QUIET_READY_SECONDS = 15
# T3 bounds one instance's model refresh at 90 seconds.
REFRESH_SECONDS = 120
# The account-isolation check after a new release takes about 3 to 5 seconds.
QUALIFY_SECONDS = 60
QUALIFY_REASON_MAX = 160
QUALIFY_LINE = re.compile(r"^(QUALIFY|WRAPPER-RUNS):[ \t]*(.*?)[ \t]*$")
WRAPPER_RELEASE = re.compile(r"^[0-9a-f]{8}\.\.$")
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


T3_SETTINGS_MAX_BYTES = 4 * 1024 * 1024


def custom_executable_instances():
    """The instance ids whose T3 settings name a custom Antigravity executable.

    T3 refuses to install its managed runtime through such an instance ("This instance
    uses a custom executable"), and its own post-install refresh skips them. The two
    instances run through a per-instance sign-in wrapper (2026-11-19), so the install
    goes through an instance without one, such as T3's disabled default. Only ids are
    read, never other settings. An unreadable file names none: T3's refusal then decides."""
    path = pathlib.Path.home() / ".t3/userdata/settings.json"
    try:
        if path.is_symlink() or path.stat().st_size > T3_SETTINGS_MAX_BYTES:
            return set()
        instances = json.loads(path.read_text(encoding="utf-8")).get("providerInstances")
    except (OSError, ValueError, AttributeError):
        return set()
    if not isinstance(instances, dict):
        return set()
    return {key for key, value in instances.items()
            if isinstance(value, dict) and isinstance(value.get("config"), dict)
            and isinstance(value["config"].get("binaryPath"), str) and value["config"]["binaryPath"].strip()}


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


def active_release(host):
    """T3's active release id, the one its wrapper selects; None when unreadable."""
    root = managed_root(host)
    if root is None:
        return None
    try:
        release_id = read_json(root / "active.json").get("releaseId")
    except (UpdateFailure, OSError, ValueError):
        return None
    return release_id if isinstance(release_id, str) and RELEASE_ID.fullmatch(release_id) else None


def qualification_command(host):
    """The kit's qualification command on this computer, or None when the kit is not installed.

    Linux, Nas1 and the Mac keep the kit under ~/.local/share and its launcher in
    ~/.local/bin; the PCs run the .cmd inside the kit itself. Either missing keeps
    Update all's row exactly as it was before this hook existed."""
    home = pathlib.Path.home()
    if host == "windows":
        command_path = home / ".local/share/t3-antigravity-isolated/t3-antigravity-check.cmd"
        return command_path if command_path.is_file() else None
    if not (home / ".local/share/t3-antigravity-isolated").is_dir():
        return None
    command_path = home / ".local/bin/t3-antigravity-check"
    return command_path if command_path.is_file() else None


@dataclasses.dataclass
class Qualification:
    """The kit's verdict on the release this run installed.

    passed is the command's exit code 0; anything else keeps T3 on the release
    the wrapper already runs. wrapper is that release's version (None when the
    check says it runs none), and wrapper_known is False when its WRAPPER-RUNS
    line did not parse, so the row never names a version it did not read.
    reason is the check's own one-line explanation when it could not test."""
    passed: bool
    wrapper: str = None
    wrapper_known: bool = False
    reason: str = None


def qualification_line(output, name):
    """The last QUALIFY: or WRAPPER-RUNS: line's text, or None; other lines are never read."""
    value = None
    for line in output.splitlines():
        match = QUALIFY_LINE.match(line.strip())
        if match and match.group(1) == name:
            value = match.group(2)
    return value


def wrapper_runs(line):
    """(known, version) from WRAPPER-RUNS: "<id8>.. <version>" or "none -"."""
    if not isinstance(line, str):
        return False, None
    parts = line.split()
    if parts and parts[0] == "none":
        return True, None
    if len(parts) == 2 and WRAPPER_RELEASE.fullmatch(parts[0]) and STATE_VERSION.fullmatch(parts[1]):
        return True, parts[1]
    return False, None


def cannot_test_reason(line):
    """The reason after "QUALIFY: CANNOT TEST [<id8>.. <version>]: ...", one bounded line."""
    if not isinstance(line, str) or not line.startswith("CANNOT TEST"):
        return None
    rest = line[len("CANNOT TEST"):].lstrip()
    rest = rest[1:] if rest.startswith(":") else rest.partition(":")[2]
    reason = " ".join(scrub(rest, lines=1).split())
    return reason if 0 < len(reason) <= QUALIFY_REASON_MAX else None


def qualify_release(host, release_id, deadline=None):
    """Run the kit's account-isolation check for a release this run installed.

    Returns None when the kit or its command is absent, so the row keeps the
    words it had before. Otherwise exit code 0 is a pass; 1, 2, a timeout (60 s)
    and any other ending are unverified, and exit 2 carries the check's reason."""
    executable = qualification_command(host)
    if executable is None:
        return None
    argv = ["cmd.exe", "/c", str(executable)] if host == "windows" else [str(executable)]
    argv.append("--qualify")
    if release_id:
        argv.append(release_id)
    limit = QUALIFY_SECONDS if deadline is None else max(1, min(QUALIFY_SECONDS, deadline - time.monotonic()))
    output, code, reason = "", 0, None
    try:
        output = command(argv, timeout=limit, capture=True, errors=True, quiet=True)
    except UpdateFailure as error:
        if error.code == "timeout":
            code, reason = 2, "the check did not finish in time"
        elif error.reason == "not_started":
            code, reason = 2, "the check could not be started"
        else:
            code = error.exit_code if error.exit_code in (1, 2) else 2
            output = error.output or ""
    if code == 2 and reason is None:
        reason = cannot_test_reason(qualification_line(output, "QUALIFY"))
    known, wrapper = wrapper_runs(qualification_line(output, "WRAPPER-RUNS"))
    return Qualification(passed=code == 0, wrapper=wrapper, wrapper_known=known, reason=reason)


def newest_cli_version(host, deadline):
    """The CLI channel's newest version for this computer (what T3's Install fetches), or None.

    Read by the same killable child as the ZCode release page, so DNS, a stalled
    server or a byte-by-byte body cannot hold the row past min(10 s, its deadline)."""
    from app_update_zcode import read_bounded
    target = cli_target(host)
    if target is None or deadline - time.monotonic() <= 0:
        return None
    try:
        raw = read_bounded(CLI_MANIFEST_BASE + target + ".json", CLI_MANIFEST_BASE, MANIFEST_LIMIT,
                           min(deadline, time.monotonic() + MANIFEST_SECONDS))
        value = json.loads(raw.decode("utf-8"))
    except (UpdateFailure, OSError, ValueError, UnicodeDecodeError):
        return None
    version = value.get("version") if isinstance(value, dict) else None
    return version if isinstance(version, str) and SEMVER.fullmatch(version) and version_tuple(version) >= NATIVE_FROM else None


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
    hiding this one's, and every instance is asked even after one needs a sign-in.
    A refusal whose words (its error tag and detail, never the instance name)
    mention sign-in, or an instance that now reads unauthenticated, means the new
    runtime cannot use the stored sign-in. Returns (outcome, reason), the worst of
    all instances: ("signin" | "failed" | "ok", word)."""
    signin, failed = None, None
    for instance in instances:
        try:
            reply = client.call("server.refreshProviders", {"instanceId": instance, "refreshModels": True})
        except UpdateFailure as error:
            if SIGNIN_WORDS.search(error.output or ""):
                signin = signin or "refresh_signin"
            else:
                failed = failed or ("refresh_timed_out" if "timed out" in (error.output or "").lower() else "refresh_failed")
            continue
        except (OSError, TimeoutError):
            failed = failed or "refresh_failed"
            continue
        providers = reply.get("providers") if isinstance(reply, dict) else None
        for provider in providers if isinstance(providers, list) else []:
            if (isinstance(provider, dict) and provider.get("instanceId") == instance and
                    isinstance(provider.get("auth"), dict) and provider["auth"].get("status") == "unauthenticated"):
                signin = signin or "unauthenticated"
    if signin:
        return ("signin", signin)
    if failed:
        return ("failed", failed)
    return ("ok", None)


def restart_pending(t3_row):
    """Whether T3 changed in this run without its new server running yet (the Linux deferred restart)."""
    if not isinstance(t3_row, dict) or t3_row.get("status") == "current":
        return False
    if any(isinstance(t, dict) and t.get("kind") == "systemd" for t in t3_row.get("restartTargets") or []):
        return True
    return t3_row.get("platform") == "ubuntu" and t3_row.get("updateAttempted") is True


def replaced_version(t3_row):
    """The server version T3's own row put in place in this run (Mac and Windows reopen it at once)."""
    if (isinstance(t3_row, dict) and t3_row.get("status") == "updated" and isinstance(t3_row.get("version"), str)
            and any(isinstance(target, dict) and target.get("kind") == "desktop"
                    for target in t3_row.get("restartTargets") or [])):
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
            # Install through an instance T3 manages (enabled first); refresh every enabled one after.
            custom = custom_executable_instances()
            managed = [p["instanceId"] for p in installable
                       if isinstance(p.get("instanceId"), str) and p["instanceId"] not in custom]
            instance = next((i for i in enabled if i in managed), managed[0] if managed else None)
            if instance is None:
                raise UpdateFailure("t3_install_failed:provider_unavailable", reason="no_managed_instance")
            offer = read_offer(client, instance)
            offered = offer.get("version") if offer and offer.get("version") and SEMVER.fullmatch(offer["version"]) else None
            current = detect_antigravity_acp(install.platform)
            if current is None or current.probe:
                raise UpdateFailure("t3_install_failed:readiness_unknown", reason="runtime_record_invalid")
            guarded = current.runtime_kind == "harness" and install.platform not in SIGNIN_CARRIES_TO_NATIVE
            # Google's channel is read only when T3 gave no usable offer, or when this runtime pair
            # could be replaced by the native CLI: T3's start re-reads that channel itself.
            newest = newest_cli_version(install.platform, deadline) if offered is None or guarded else None
            known = [version for version in (offered, newest) if version]
            target = max(known, key=version_tuple) if known else None
            record.owner = {**(current.owner or {}), "target": "t3-offer" if target and target == offered else "cli-manifest"}
            if guarded:
                native = [version for version in known if version_tuple(version) >= NATIVE_FROM]
                # With no target at all nothing can start either: that is "offer unknown" below, not a sign-in.
                if native or (newest is None and target is not None and newer(target, current.version)):
                    # Whatever the offer says, start may install the native CLI, which cannot read the
                    # sign-in this runtime pair stored here. (If both signals are stale at the instant
                    # of start, AAC cannot tell; T3 must carry the sign-in over to close that window.)
                    record.blame("signin_needed", "token_storage_change" if native else "migration_unchecked")
                    value = result(install.app_id, install.platform, "action_required", before, current.version,
                                   install.manager, "antigravity_acp_signin_hold")
                    held = max(native, key=version_tuple) if native else target
                    if held:
                        value["heldVersion"] = held
                    return value
            if target is None:
                record.blame("unknown", "no_offer")
                return result(install.app_id, install.platform, "unknown", before, current.version, install.manager,
                              "antigravity_acp_offer_unknown")
            if not newer(target, current.version):
                return result(install.app_id, install.platform, "current", before, current.version, install.manager, "current")
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
            # T3 must have activated at least what its operation named, and never gone backwards.
            if final is None or final.probe or newer(target, final.version) or newer(current.version, final.version):
                raise UpdateFailure("t3_install_failed:verification")
            record.owner = {**(final.owner or {}), "target": record.owner.get("target")}
            if not newer(final.version, current.version):
                # T3 only re-checked the installed runtime (a withdrawn release, its own manifest
                # read failed, a rollback): nothing new runs, so this is not an update.
                record.note("reverified")
                return result(install.app_id, install.platform, "current", before, final.version, install.manager,
                              "current", attempted)
            record.phase("qualify")
            # A new release was installed: the kit's check proves each account kept
            # its own sign-in. Its absence leaves the row exactly as it was before.
            qualification = qualify_release(install.platform, active_release(install.platform), deadline)
            record.phase("refresh_models")
            outcome, reason = refresh_models(client, enabled)
            if outcome == "signin":
                record.blame("signin_needed", reason)
                return result(install.app_id, install.platform, "action_required", before, final.version,
                              install.manager, "antigravity_acp_signin_needed", attempted)
            if qualification is not None and not qualification.passed:
                value = result(install.app_id, install.platform, "updated", before, final.version,
                               install.manager, "antigravity_acp_isolation_unverified", attempted)
                if qualification.wrapper_known:
                    value["wrapperVersion"] = qualification.wrapper
                if qualification.reason:
                    value["qualifyReason"] = qualification.reason
                return value
            if qualification is not None:
                return result(install.app_id, install.platform, "updated", before, final.version,
                              install.manager, "antigravity_acp_qualified", attempted)
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
