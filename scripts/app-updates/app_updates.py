#!/usr/bin/env python3
"""Fixed installed-app updater. Default inventory is read-only.

Only --apply (the authenticated dashboard's one-button action) installs or
restarts anything. There are no configurable app IDs, commands, hosts or URLs.
"""

import argparse
import ctypes
import json
import os
import pathlib
import re
import shutil
import sys
import time
import uuid

from app_update_common import (
    APP_LABELS, Install, UpdateFailure, cli_probe, command, download, execution_lock,
    powershell, private_temporary, ps_quote, resolve_npm, result, version_text, version_tuple, write_private_json,
)
from app_update_desktop import detect_desktop, update_desktop
from app_update_processes import cli_contexts, family, scan, t3_owned, terminate_cli
from app_update_terminal import check_terminal, restart_cli
from app_update_t3 import detect_t3, update_t3
from app_update_zcode import detect_adapters, detect_zcode, update_adapters, update_zcode

CLI_NAMES = {"antigravity-cli": "agy", "muse-code": "muse", "omp": "omp", "codex-cli": "codex", "claude-code": "claude"}
# Apps with their own verified update flow; ZCode and the ACP adapters never stop a T3 session.
OWN_UPDATERS = ("zcode", "t3-acp-adapters", "t3-code")
# The Codex bridge's whole budget: lock (30 s) + update (180 s) + idle wait
# (60 s) + proxy restart checks. It never waits hours for a busy Codex.
CODEX_BRIDGE_SECONDS = 420

# Antigravity switching works only with native CLI builds that passed a
# switching review (scripts/antigravity/runtime/release.json, reviewedNatives).
# `agy update` always installs the newest build, so Update all reads the
# official release manifest first (the same fixed URL the CLI's own updater and
# installer use) and holds the installed build when the newest one is not
# reviewed. Nothing here installs, stops or restarts anything.
AGY_MANIFEST_BASE = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/"
AGY_VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[A-Za-z0-9_.-]+)?$")
AGY_MAX_REVIEWED = 16
AGY_RELEASE_FILE = pathlib.Path(__file__).resolve().parents[1] / "antigravity/runtime/release.json"


def _agy_version(value):
    return value if isinstance(value, str) and len(value) <= 128 and AGY_VERSION.match(value) else None


def parse_reviewed_versions(value):
    """A comma-separated reviewed list from the dashboard, or None when unusable."""
    if not isinstance(value, str) or not value or len(value) > 4096:
        return None
    items = value.split(",")
    versions = [_agy_version(item) for item in items]
    if len(items) > AGY_MAX_REVIEWED or None in versions or len(set(versions)) != len(versions):
        return None
    return frozenset(versions)


def read_release_versions(release_file=AGY_RELEASE_FILE):
    """Reviewed versions from the packaged release file, or None (fail closed).

    Mirrors readNativeRelease: exact two-key entries, unique versions and
    hashes, at most 16; a release without reviewedNatives is its single pin.
    """
    try:
        raw = pathlib.Path(release_file).read_bytes()
        if len(raw) > 65536:
            return None
        value = json.loads(raw.decode("utf-8"))
        entries = value.get("reviewedNatives")
        if entries is None:
            entries = [{"nativeVersion": value.get("nativeVersion"), "nativeSha256": value.get("nativeSha256")}]
        if not isinstance(entries, list) or not entries or len(entries) > AGY_MAX_REVIEWED:
            return None
        versions, hashes = [], []
        for entry in entries:
            if (not isinstance(entry, dict) or set(entry) != {"nativeVersion", "nativeSha256"} or
                    not _agy_version(entry["nativeVersion"]) or not isinstance(entry["nativeSha256"], str) or
                    not re.fullmatch(r"[a-f0-9]{64}", entry["nativeSha256"])):
                return None
            versions.append(entry["nativeVersion"])
            hashes.append(entry["nativeSha256"])
        if len(set(versions)) != len(versions) or len(set(hashes)) != len(hashes):
            return None
        return frozenset(versions)
    except (OSError, ValueError, AttributeError, TypeError):
        return None


def agy_manifest_key(platform, machine=None):
    """The official manifest name for this computer, e.g. linux_amd64; None when unknown."""
    import platform as host
    machine = (machine or host.machine() or "").lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "amd64" if machine in ("x86_64", "amd64") else None
    system = {"ubuntu": "linux", "mac": "darwin", "windows": "windows"}.get(platform)
    return None if arch is None or system is None else system + "_" + arch


def latest_agy_version(platform):
    """The newest official build's version for this computer, or None. Read-only."""
    import urllib.request
    key = agy_manifest_key(platform)
    if key is None:
        return None
    try:
        request = urllib.request.Request(AGY_MANIFEST_BASE + key + ".json", headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
        with urllib.request.urlopen(request, timeout=20) as response:
            if not response.geturl().startswith(AGY_MANIFEST_BASE):
                return None
            raw = response.read(16 * 1024 + 1)
        if len(raw) > 16 * 1024:
            return None
        value = json.loads(raw.decode("utf-8"))
        return _agy_version(value.get("version")) if isinstance(value, dict) else None
    except Exception:
        return None


def antigravity_hold(install, reviewed, latest=None):
    """None when the update may run; otherwise the finished row for this app.

    A newest build outside the reviewed set is never installed: the row is
    `held` and names that build. When the reviewed set or the newest version
    cannot be read, nothing is installed either (fail closed). An installed
    build that already is the newest one reports current without running the
    official updater, so no later publication can slip in.
    """
    before = install.version
    if not before:
        return None
    newest = None if reviewed is None else (latest or latest_agy_version)(install.platform)
    if newest is None:
        row = result(install.app_id, install.platform, "held", before, before, install.manager, "held_unchecked")
        row["heldVersion"] = None
        return row
    if newest == before:
        return result(install.app_id, install.platform, "current", before, before, install.manager)
    if newest in reviewed:
        return None
    row = result(install.app_id, install.platform, "held", before, before, install.manager, "held_for_review")
    row["heldVersion"] = newest
    return row


def mark_unreviewed(row, reviewed):
    """An update that still landed outside the reviewed set says so, never silently."""
    if (isinstance(row, dict) and row.get("appId") == "antigravity-cli" and row.get("status") == "updated" and
            (reviewed is None or row.get("version") not in reviewed)):
        row["messageCode"] = "updated_unreviewed"
    return row


# Claude Code's native updater and installer (claude.ai/install.sh) read the
# newest version from these plain-text channel pointers. Reading them is
# read-only; nothing named there is downloaded or run.
CLAUDE_RELEASES = "https://downloads.claude.ai/claude-code-releases/"
CLAUDE_VERSION = re.compile(r"^\d{1,6}\.\d{1,6}\.\d{1,6}$")


def claude_pointer(channel):
    """One official channel pointer's version, or None when it does not answer clearly."""
    import urllib.request
    try:
        request = urllib.request.Request(CLAUDE_RELEASES + channel, headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
        with urllib.request.urlopen(request, timeout=10) as response:
            if response.status != 200 or not response.geturl().startswith(CLAUDE_RELEASES):
                return None
            raw = response.read(65)
        text = raw.decode("ascii").strip() if len(raw) <= 64 else ""
        return text if CLAUDE_VERSION.fullmatch(text) else None
    except Exception:
        return None


def latest_claude_version():
    """The newer of the `latest` and `stable` pointers; None unless both are readable."""
    found = [claude_pointer(channel) for channel in ("latest", "stable")]
    return None if None in found else max(found, key=version_tuple)


def claude_current(install, newest=None):
    """A `current` row when native Claude Code is already the newest official build, else None.

    Decided before any process scan or `claude update`, so a running instance can
    never turn an install that has nothing to update into a failure. Any doubt
    (unreadable pointers, an unusual version, a pending restart) returns None
    and the usual checks run.
    """
    before = install.version
    if install.app_id != "claude-code" or install.manager != "native" or not CLAUDE_VERSION.fullmatch(before or ""):
        return None
    if (pathlib.Path.home() / ".ccs/app-updates/claude-code-pending-restart.json").exists():
        return None
    newest = (newest or latest_claude_version)()
    if newest is None or version_tuple(before) < version_tuple(newest):
        return None
    return result(install.app_id, install.platform, "current", before, before, install.manager)


def _candidates(name, platform):
    home = pathlib.Path.home()
    if platform == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(home / "AppData/Local")))
        roaming = pathlib.Path(os.environ.get("APPDATA", str(home / "AppData/Roaming")))
        paths = [home / ".local/bin" / (name + ".exe"), local / name / "bin" / (name + ".exe"), local / name / (name + ".exe"),
                 local / "Programs" / name / (name + ".exe"), local / "Programs" / name / (name + ".ps1"),
                 local / "Programs" / name / (name + ".cmd"), roaming / "npm" / (name + ".cmd")]
    else:
        paths = [home / ".local/bin" / name, home / ".bun/bin" / name, pathlib.Path("/opt/homebrew/bin") / name, pathlib.Path("/usr/local/bin") / name]
    found = shutil.which(name)
    if found:
        located = pathlib.Path(found)
        managed = home / '.local/share/ai-account-center/antigravity-runtime/bin/agy'
        # Process census/update must name the native executable, never the PATH
        # launcher shim; native version/help/update controls remain pass-through.
        if not (name == 'agy' and platform == 'ubuntu' and located == managed):
            paths.insert(0, located)
    if platform != "windows" and name in MANAGED_RELEASES:
        # Every PATH copy is a candidate, so a stray ahead on PATH is seen beside the managed install.
        paths = list(dict.fromkeys(_path_copies(name) + paths))
    return paths


# A managed Codex or Claude install is a release folder that its ~/.local/bin link
# resolves into. Detection picks that copy whatever the PATH order. Any other copy
# that resolves to a different file is a stray: probed and reported, never updated.
MANAGED_RELEASES = {"codex": ".codex/packages/standalone/releases", "claude": ".local/share/claude/versions"}
STRAY_LIMIT = 4


def _path_folders():
    """This process's PATH folders in order; empty or relative entries never name a copy to probe."""
    return [folder for folder in os.environ.get("PATH", os.defpath).split(os.pathsep) if os.path.isabs(folder)]


def _path_copies(name):
    """`name` in every absolute PATH folder, in PATH order."""
    return [pathlib.Path(folder) / name for folder in _path_folders()]


def _path_position(path):
    """Where path's folder sits on this process's PATH (0 first), or None when it is not on PATH."""
    folder = os.path.realpath(path.parent)
    for index, entry in enumerate(_path_folders()):
        if os.path.realpath(entry) == folder:
            return index
    return None


def _within(path, root):
    """True when the resolved `path` is `root` itself or a file inside it (case-folded on Windows only)."""
    text, base = os.path.normcase(str(path)), os.path.normcase(str(root))
    return text == base or text.startswith(base + os.sep)


def _managed_copy(name, platform, found):
    """The first existing copy that resolves into this CLI's managed release folder, or None.

    Windows and the other CLIs have no managed folder, so they always get None.
    """
    if platform == "windows" or name not in MANAGED_RELEASES:
        return None
    release = (pathlib.Path.home() / MANAGED_RELEASES[name]).resolve()
    return next((path for path in found if _within(path.resolve(), release)), None)


def _stray_location(path, resolved):
    """One fixed word for where a stray sits; its folder is never reported."""
    text, home = str(path), str(pathlib.Path.home())
    if text.startswith("/usr/local/"):
        return "usr-local"
    if text.startswith(("/opt/homebrew/", "/home/linuxbrew/")):
        return "homebrew"
    if text.startswith(os.path.join(home, ".bun") + os.sep):
        return "bun"
    if text.startswith(home + os.sep) and "node_modules" in str(resolved):
        return "user-npm"
    return "other"


def _strays(found, managed):
    """The other copies that resolve to a file different from the managed install.

    Each kept copy gets one bounded, read-only version probe, run side by side.
    At most STRAY_LIMIT are kept, in PATH order. `shadows` is true when a copy comes
    before the managed install on this PATH, or when that install is not on PATH at all.
    """
    from concurrent.futures import ThreadPoolExecutor
    target = managed.resolve()
    seen, copies = {target}, []
    for path in found:
        real = path.resolve()
        if real not in seen and len(copies) < STRAY_LIMIT:
            seen.add(real)
            copies.append((path, real))
    if not copies:
        return []
    managed_at = _path_position(managed)
    with ThreadPoolExecutor(max_workers=len(copies)) as pool:
        versions = list(pool.map(lambda item: _stray_version(item[0], item[1]), copies))
    strays = []
    for (path, real), version in zip(copies, versions):
        at = _path_position(path)
        strays.append({"location": _stray_location(path, real), "version": version,
                       "shadows": at is not None and (managed_at is None or at < managed_at)})
    return strays


# An npm global Codex CLI on Ubuntu or Mac is `<prefix>/bin/codex` linking into
# `<prefix>/lib/node_modules/@openai/codex`. Its update runs npm itself under that
# prefix, because `codex update` would run whichever npm PATH finds (see perform_cli_update).
CODEX_NPM_PACKAGE = "@openai/codex"
PACKAGE_JSON_LIMIT = 64 * 1024


def _package_json(folder):
    """The parsed package.json of a package folder, or None. The read is bounded and nothing in it runs."""
    try:
        with (pathlib.Path(folder) / "package.json").open("rb") as handle:
            raw = handle.read(PACKAGE_JSON_LIMIT + 1)
        info = json.loads(raw) if len(raw) <= PACKAGE_JSON_LIMIT else None
    except (OSError, ValueError):
        return None
    return info if isinstance(info, dict) else None


def _stray_version(path, real):
    """A stray's version. An npm-script copy (`.../@openai/codex/bin/codex.js`, package named @openai/codex)
    reports its package.json version and is never run; any other copy gets the bounded --version probe."""
    root = real.parent.parent
    if real.name == "codex.js" and real.parent.name == "bin" and root.parts[-3:] == ("node_modules", "@openai", "codex"):
        info = _package_json(root)
        if info is not None and info.get("name") == CODEX_NPM_PACKAGE:
            return version_text(info.get("version"))
    return cli_probe(path)[0]


def _npm_codex_root(path, resolved):
    """The package folder when this POSIX Codex CLI is an npm global install, else None.

    `path` must be `<prefix>/bin/codex`, `resolved` must lie inside `<prefix>/lib/node_modules/@openai/codex`,
    and that package.json must name @openai/codex. Windows shims never reach here (see detect_cli).
    """
    if path.parent.name != "bin":
        return None
    root = path.parent.parent / "lib/node_modules/@openai/codex"
    if not _within(resolved, root.resolve()):
        return None
    info = _package_json(root)
    return root if info is not None and info.get("name") == CODEX_NPM_PACKAGE else None


def npm_prefix(install):
    """The npm global prefix that owns an npm-managed install: the folder of codex.cmd on Windows, the folder above bin on POSIX."""
    return install.path.parent if install.platform == "windows" else install.path.parent.parent


def detect_cli(app_id, platform):
    name = CLI_NAMES[app_id]
    found = [path for path in _candidates(name, platform) if path.is_file()]
    managed = _managed_copy(name, platform, found)
    path = managed if managed is not None else (found[0] if found else None)
    if path is None:
        return None
    manager, root = "native", None
    if platform == "windows" and app_id == "muse-code":
        # Meta's install.ps1 writes only muse.cmd (a hand-added `muse` shim is also
        # accepted). Probe its PowerShell launcher directly: CreateProcess cannot run .cmd.
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
        expected = local / "Programs/muse"
        launcher = expected / ".muse-launcher.ps1"
        # shutil.which spells the shim from PATHEXT (muse.CMD), so fold its case.
        if path.parent.resolve() != expected.resolve() or path.name.lower() not in ("muse", "muse.cmd") or not launcher.is_file():
            return Install(app_id, platform, path, manager="unsupported")
        try:
            output = command(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", launcher, "--version"], timeout=20, capture=True)
            version = version_text(output.rsplit("(", 1)[-1])
            probe = None
        except UpdateFailure as error:
            version, probe = None, "timeout" if error.code == "timeout" else "failed"
        return Install(app_id, platform, expected / "muse.cmd", version, manager, probe=probe)
    if platform == "windows" and path.suffix.lower() == ".cmd":
        if app_id != "codex-cli":
            return Install(app_id, platform, path, manager="unsupported")
        root = path.parent / "node_modules/@openai/codex"
        try:
            info = json.loads((root / "package.json").read_text(encoding="utf-8"))
            if info.get("name") != "@openai/codex":
                return None
            version = version_text(info.get("version"))
        except (OSError, ValueError):
            return None
        return Install(app_id, platform, path, version, "npm", package_root=root)
    version, probe = cli_probe(path)
    if probe == "timeout":
        return Install(app_id, platform, path, None, "native", probe="timeout")
    resolved = path.resolve()
    home = pathlib.Path.home()
    # normcase folds case on Windows only, where resolve() reports the on-disk spelling.
    resolved_text = os.path.normcase(str(resolved))
    npm_root = _npm_codex_root(path, resolved) if app_id == "codex-cli" and platform != "windows" else None
    if app_id == "codex-cli" and os.path.normcase(str(home / ".codex/packages/standalone/releases")) in resolved_text:
        root = home / ".codex/packages/standalone/releases"
    elif app_id == "claude-code" and os.path.normcase(str(home / ".local/share/claude/versions")) in resolved_text:
        root = home / ".local/share/claude/versions"
    elif npm_root is not None:
        manager, root = "npm", npm_root
    strays = _strays(found, managed) if managed is not None else []
    return Install(app_id, platform, path, version, manager, package_root=root, strays=strays)


def _detect_one(app_id, platform):
    if app_id == "t3-code":
        return detect_t3(platform)
    if app_id == "zcode":
        return detect_zcode(platform)
    if app_id == "t3-acp-adapters":
        return detect_adapters(platform)
    return detect_desktop(app_id, platform) if app_id.endswith("-desktop") else detect_cli(app_id, platform)


def detect(platform):
    """Read-only version probes for every app, side by side.

    Each probe has its own timeout, so the slowest one bounds the whole check
    instead of all ten adding up. Nothing here installs or stops anything.
    """
    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=len(APP_LABELS)) as pool:
        futures = {app_id: pool.submit(_detect_one, app_id, platform) for app_id in APP_LABELS}
    found = {}
    for app_id, future in futures.items():
        try:
            found[app_id] = future.result()
        except Exception:
            found[app_id] = Install(app_id, platform, None, probe="failed")
    return found


def resolve_muse_shell(platform):
    """The Meta installer is bash-only ([[ ]], arrays); /bin/sh is dash on
    Ubuntu and fails immediately. Resolve bash first so a host without one
    reports unsupported without downloading anything."""
    if platform == "windows":
        return None
    shell = shutil.which("bash")
    if shell is None and pathlib.Path("/bin/bash").is_file():
        shell = "/bin/bash"
    if shell is None:
        raise UpdateFailure("unsupported")
    return shell


def _pid_alive(pid):
    """Windows only: True while `pid` is a running process."""
    from ctypes import wintypes
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return False
    try:
        status = wintypes.DWORD()
        # 259 is STILL_ACTIVE: the process has not exited yet.
        return bool(kernel.GetExitCodeProcess(handle, ctypes.byref(status)) and status.value == 259)
    finally:
        kernel.CloseHandle(handle)


def muse_update_busy(directory):
    """True while the Muse launcher's own updater holds its lock with a live PID.

    Read-only: the launcher reclaims a dead holder's lock itself, so AAC never
    deletes the lock or a `.muse-update.<pid>.exe` partial beside it.
    """
    try:
        with (pathlib.Path(directory) / ".muse-update-lock" / "pid").open("rb") as handle:
            text = handle.read(64).decode("ascii").strip()
    except (OSError, ValueError):
        return False
    if not re.fullmatch(r"[1-9][0-9]{0,8}", text):
        return False
    return _pid_alive(int(text))


def _clamp(seconds, deadline):
    if deadline is None:
        return seconds
    return max(30, min(seconds, int(deadline - time.monotonic())))


def npm_view_latest(install):
    """Read-only registry check for the npm path. None when unknowable."""
    resolved = resolve_npm(install.path.parent)
    if resolved is None:
        return None
    node, cli = resolved
    try:
        return command([str(node), str(cli), "view", "@openai/codex", "version"], timeout=60, capture=True).strip() or None
    except UpdateFailure:
        return None


def perform_cli_update(install, deadline=None):
    path = install.path
    if install.app_id == "muse-code":
        expected = pathlib.Path.home() / ".local/bin/muse"
        if install.platform != "windows" and path.resolve() != expected.resolve():
            raise UpdateFailure("unsupported")
        if install.platform == "windows":
            local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
            if path.resolve() != (local / "Programs/muse/muse.cmd").resolve() or not path.with_name(".muse-launcher.ps1").is_file():
                raise UpdateFailure("unsupported")
        shell = resolve_muse_shell(install.platform)
        with private_temporary() as temporary:
            target = temporary / ("muse-install.ps1" if install.platform == "windows" else "muse-install.sh")
            download("https://dev.meta.ai/install.ps1" if install.platform == "windows" else "https://dev.meta.ai/install.sh", target, maximum=512 * 1024, timeout=60)
            env = {"MUSE_UPGRADE_MODE": "1", "MUSE_NO_MODIFY_PATH": "1"}
            if install.platform == "windows":
                env["MUSE_INSTALL_DIR"] = str(path.parent)
                command(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", target], timeout=_clamp(180, deadline), env=env)
            else:
                command([shell, target], timeout=_clamp(600, deadline), env=env)
        return
    if install.manager == "npm":
        # npm itself, under the install's own prefix: never `codex update`, which runs whichever npm PATH finds.
        prefix = npm_prefix(install)
        resolved = resolve_npm(prefix)
        if resolved is None:
            raise UpdateFailure("unsupported")
        node, cli = resolved
        command([str(node), str(cli), "install", "--global", "--prefix", str(prefix), "@openai/codex@latest"], timeout=_clamp(300, deadline))
        return
    if install.manager != "native":
        raise UpdateFailure("unsupported")
    command([path, "update"], timeout=180, env={"PATH": str(path.parent) + os.pathsep + os.environ.get("PATH", "")})


def npm_bridge_arguments(install):
    """The Codex bridge's npm flags for an npm-managed install: node, npm-cli.js and its prefix. None otherwise.

    The bridge then runs the same npm command as perform_cli_update, under the same prefix.
    """
    if install.manager != "npm":
        return []
    prefix = npm_prefix(install)
    resolved = resolve_npm(prefix)
    if resolved is None:
        raise UpdateFailure("unsupported")
    node, cli = resolved
    return ["--npm-node", str(node), "--npm-cli", str(cli), "--npm-prefix", str(prefix)]


def update_cli(install, deadline):
    before = install.version
    attempted = False
    changed = False
    pre_stopped = False
    pre_forced = 0
    pending = pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-pending-restart.json")
    try:
        if not before:
            raise UpdateFailure("version_unknown")
        descriptor = pathlib.Path.home() / '.ccs/antigravity-switching/runtime-installation.json'
        if install.app_id == 'antigravity-cli' and install.platform == 'ubuntu' and (descriptor.exists() or descriptor.is_symlink()):
            # The same account lock and proved resident PTY coordinator owns
            # managed updates. Never move this session into a replacement tmux.
            bridge = pathlib.Path(__file__).parents[2] / 'dist/antigravity/managed-update-command.js'
            if not bridge.is_file(): raise UpdateFailure('unsupported')
            seconds = max(30, min(300, int(deadline - time.monotonic())))
            payload = json.loads(command([shutil.which('node') or '/usr/bin/node', bridge],
                timeout=seconds, capture=True))
            if (type(payload) is not dict or payload.get('appId') != 'antigravity-cli' or
                    payload.get('platform') != 'ubuntu' or payload.get('status') not in
                    {'current', 'updated', 'failed', 'restart_failed'}): raise UpdateFailure()
            return payload
        processes = scan(install.platform)
        # Linux Codex's shared daemon needs the existing startup-lock and idle
        # protocol, not an idle TUI/PTY restart. It is handled by the fixed bridge.
        if install.app_id == "codex-cli" and install.platform == "ubuntu" and any("app-server" in item.args for item in family(install, processes)):
            contexts, targets = cli_contexts(install, [item for item in processes if "app-server" not in item.args])
            check_terminal(install.platform, contexts)
            bridge = pathlib.Path(__file__).with_name("app_update_codex.cjs")
            seconds = max(30, min(CODEX_BRIDGE_SECONDS, int(deadline - time.monotonic())))
            payload = json.loads(command([shutil.which("node") or "/usr/bin/node", bridge, "--operation", "cli", "--timeout-seconds", str(seconds), *npm_bridge_arguments(install)], timeout=seconds + 15, capture=True))
            if payload.get("status") == "updated" and contexts:
                refreshed = detect_cli(install.app_id, install.platform)
                if refreshed is None or not refreshed.version:
                    raise UpdateFailure("version_unknown")
                write_private_json(pending, {"version": refreshed.version})
                try:
                    forced = terminate_cli(install, targets)
                    sessions = restart_cli(refreshed, contexts)
                    payload["restartedProcesses"] = payload.get("restartedProcesses", 0) + len(contexts)
                    payload.update(restartTargets=sessions, forcedStops=forced)
                    pending.unlink(missing_ok=True)
                except (UpdateFailure, OSError):
                    payload.update(status="restart_failed", messageCode="restart_failed")
            return payload
        if install.app_id == "muse-code" and install.platform == "windows":
            # T3's muse-acp adapter hosts `muse serve` from this folder: never stop or restart it.
            contexts, targets, t3_sessions = [], [], []
        else:
            # cli_contexts leaves T3's own sessions out: they are never stopped or
            # relaunched and keep running their current files through the update.
            contexts, targets = cli_contexts(install, processes)
            t3_sessions = t3_owned(install, processes)
        check_terminal(install.platform, contexts)
        if install.manager == "npm" and install.platform == "windows":
            if (contexts or t3_sessions) and npm_view_latest(install) == before:
                # Already current: never stop running sessions for a no-op.
                # Anything mapped is running, so no stale marker can matter.
                if contexts:
                    pending.unlink(missing_ok=True)
                return result(install.app_id, install.platform, "current", before, before, install.manager, attempted=False)
            if t3_sessions:
                # npm cannot replace files a running process holds open on
                # Windows, and a T3 session is never stopped: change nothing.
                return result(install.app_id, install.platform, "action_required", before, before, install.manager, "in_use")
            if contexts:
                # Windows cannot replace a running npm tree (locked files fail
                # the install), so mapped instances stop before npm runs. The
                # pending marker lets a later explicit click retry the restart
                # if this one leaves processes down.
                write_private_json(pending, {"version": before})
                try:
                    pre_forced = terminate_cli(install, targets)
                except UpdateFailure:
                    try:
                        restart_cli(install, contexts)
                        pending.unlink(missing_ok=True)
                    except (UpdateFailure, OSError):
                        pass
                    return result(install.app_id, install.platform, "failed", before, before, install.manager, "restart_context", False)
                pre_stopped = True
        attempted = True
        perform_cli_update(install, deadline)
        refreshed = detect_cli(install.app_id, install.platform)
        if refreshed is None or not refreshed.version:
            raise UpdateFailure("version_unknown")
        marker_version = None
        try:
            marker_version = json.loads(pending.read_text(encoding="utf-8")).get("version")
        except (OSError, ValueError):
            pass
        needs_restart = marker_version == refreshed.version and marker_version != before
        if refreshed.version == before and not needs_restart:
            if install.app_id == "muse-code" and install.platform == "windows" and muse_update_busy(install.path.parent):
                # The launcher's own updater holds the lock, so Meta's installer
                # skipped this run. Report busy, never a false "current"; the lock stays.
                raise UpdateFailure("busy")
            if pre_stopped:
                # Stopped for the install; relaunch the same version. The
                # marker must go: there is nothing newer to retry towards.
                pending.unlink(missing_ok=True)
                try:
                    sessions = restart_cli(refreshed, contexts)
                except (UpdateFailure, OSError):
                    write_private_json(pending, {"version": refreshed.version})
                    return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", True)
                value = result(install.app_id, install.platform, "current", before, before, install.manager, attempted=True, restarted=len(contexts))
                value.update(restartTargets=sessions, forcedStops=pre_forced)
                return value
            if marker_version == before:
                # Stale pre-stop bookkeeping from an earlier run. Dropping it
                # keeps a later run from reporting "updated" for a version
                # that never changed.
                pending.unlink(missing_ok=True)
            return result(install.app_id, install.platform, "current", before, before, install.manager, attempted=True)
        changed = True
        write_private_json(pending, {"version": refreshed.version})
        forced = (terminate_cli(install, targets) if targets else 0) + pre_forced
        try:
            sessions = restart_cli(refreshed, contexts)
        except (UpdateFailure, OSError):
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", True)
        # Running T3 sessions keep the previous version until T3 starts them again.
        code = "t3_sessions_kept" if t3_sessions else None
        value = result(install.app_id, install.platform, "updated", before, refreshed.version, install.manager, code, attempted=True, restarted=len(contexts))
        value.update(restartTargets=sessions, forcedStops=forced)
        pending.unlink(missing_ok=True)
        return value
    except UpdateFailure as error:
        if changed:
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", attempted)
        if pre_stopped:
            relaunch_after_failed_install(install, contexts, pending)
        code = "update_failed" if error.code == "download_blocked" else error.code
        return result(install.app_id, install.platform, "failed", before, before, install.manager, code, attempted)
    except Exception:
        if changed:
            return result(install.app_id, install.platform, "restart_failed", before, refreshed.version, install.manager, "restart_failed", attempted)
        if pre_stopped:
            relaunch_after_failed_install(install, contexts, pending)
        return result(install.app_id, install.platform, "failed", before, before, install.manager, "update_failed", attempted)


def relaunch_after_failed_install(install, contexts, pending):
    """Relaunch the old version after a post-stop install failure, best effort.

    The pending marker stays when the relaunch fails, so a later explicit
    click retries the restart through the normal pending path.
    """
    try:
        restart_cli(install, contexts)
        pending.unlink(missing_ok=True)
    except (UpdateFailure, OSError):
        pass


def check_readiness(install):
    """Per-app pre-flight before an update is attempted. Read-only: no installs,
    stops or restarts. Returns None when ready, else (status, code): 'failed'
    when the check ran and said no (no supported updater, or running instances
    that cannot restart safely), 'unknown' when the check itself could not run.
    """
    try:
        if install.manager == "unsupported":
            return ("failed", "unsupported")
        if install.app_id.endswith("-desktop") or install.app_id in OWN_UPDATERS:
            # Desktop updaters verify before stopping anything; the manager
            # check above is the whole pre-flight.
            return None
        if install.app_id == "muse-code" and install.platform == "windows":
            # Never stopped or restarted on Windows (see update_cli), so nothing to check.
            return None
        # Only the user's own instances are judged: T3 sessions are left out and
        # one that exits during the check is dropped, never a failure.
        contexts, _targets = cli_contexts(install, scan(install.platform))
        check_terminal(install.platform, contexts)
    except UpdateFailure as error:
        return ("failed", error.code)
    except Exception:
        return ("unknown", "readiness_unknown")
    return None


# One computer's whole run; apps not started by then report a timeout row.
HOST_DEADLINE_SECONDS = 15 * 60


def run_apply(platform, emit=None, cancelled=None, agy_reviewed=None):
    """Check and update every app on this computer, one installer at a time.

    emit(event) receives {"event": "app", ...} before each app's check and
    update, and {"event": "result", ...} as soon as its row is known, so the
    dashboard can show live progress. cancelled() is polled between apps: once
    true, every app not yet started is reported as skipped. agy_reviewed is the
    Antigravity CLI's reviewed version set (None: unknown, so it is held).
    """
    emit = emit or (lambda event: None)
    cancelled = cancelled or (lambda: False)
    results = []
    deadline = time.monotonic() + HOST_DEADLINE_SECONDS

    def report(row):
        # A Codex or Claude row names the stray copies beside its managed install.
        strays = getattr(installations.get(row.get("appId")), "strays", None)
        if strays:
            row["strays"] = strays
        results.append(row)
        emit({"event": "result", "result": row})

    with execution_lock():
        emit({"event": "app", "appId": None, "phase": "checking"})
        installations = detect(platform)
        # Update package apps first (ZCode and the ACP adapters among them),
        # then the shared Codex daemon idle wait. T3 is last because its
        # desktop/server replacement interrupts threads.
        order = [app_id for app_id in APP_LABELS if app_id not in ("codex-cli", "t3-code")] + ["codex-cli", "t3-code"]
        for app_id in order:
            install = installations[app_id]
            if cancelled():
                report(result(app_id, platform, "skipped", code="skipped_cancelled"))
            elif install is None:
                report(result(app_id, platform, "not_installed"))
            elif install.probe == "timeout":
                report(result(app_id, platform, "unknown", None, None, install.manager, "check_timeout"))
            elif install.probe is not None:
                report(result(app_id, platform, "unknown", None, None, None, "readiness_unknown"))
            elif time.monotonic() >= deadline:
                report(result(app_id, platform, "failed", install.version, install.version, install.manager, "timeout"))
            else:
                emit({"event": "app", "appId": app_id, "phase": "checking"})
                if app_id == "antigravity-cli" and install.manager != "unsupported":
                    try:
                        held = antigravity_hold(install, agy_reviewed)
                    except Exception:
                        held = result(app_id, platform, "held", install.version, install.version, install.manager, "held_unchecked")
                    if held is not None:
                        report(held)
                        continue
                if app_id == "claude-code":
                    try:
                        newest = claude_current(install)
                    except Exception:
                        newest = None
                    if newest is not None:
                        report(newest)
                        continue
                gate = check_readiness(install)
                if gate is not None:
                    status, code = gate
                    report(result(app_id, platform, status, install.version, install.version, install.manager, code))
                    continue
                emit({"event": "app", "appId": app_id, "phase": "updating"})
                if app_id.endswith("-desktop") or app_id in OWN_UPDATERS:
                    # A desktop app reports its long steps (a package download,
                    # then the install) so the page shows what it waits on.
                    def phase(name, app_id=app_id):
                        emit({"event": "app", "appId": app_id, "phase": name})
                    update = {"zcode": update_zcode, "t3-acp-adapters": update_adapters, "t3-code": update_t3}.get(app_id, update_desktop)
                    try: report(update(install, deadline, phase))
                    except Exception: report(result(app_id, platform, "failed", install.version, install.version, install.manager, "update_failed"))
                elif app_id == "antigravity-cli":
                    report(mark_unreviewed(update_cli(install, deadline), agy_reviewed))
                else:
                    report(update_cli(install, deadline))
    return {"results": results}


def windows_interactive_apply(emit=None, cancelled=None, agy_reviewed=None):
    """The fixed separate InteractiveToken task owns GUI/terminal restarts.

    The task child writes its progress to a private file; this coordinator
    relays new events to emit() and forwards a cancel through a nonce-bound
    cancel file the child polls between apps. The task starts with fixed
    arguments, so the Antigravity reviewed set travels in the request file.
    """
    emit = emit or (lambda event: None)
    cancelled = cancelled or (lambda: False)
    root = pathlib.Path.home() / ".ccs/app-updates"
    request_path, result_path = root / "windows-task-request.json", root / "windows-task-result.json"
    progress_path, cancel_path = root / "windows-task-progress.json", root / "windows-task-cancel.json"
    with execution_lock("windows-coordinator.lock"):
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}", timeout=15)
        nonce = uuid.uuid4().hex
        for stale in (progress_path, cancel_path):
            stale.unlink(missing_ok=True)
        request = {"nonce": nonce}
        if agy_reviewed is not None:
            request["agyReviewed"] = sorted(agy_reviewed)
        write_private_json(request_path, request)
        powershell("$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}; Start-ScheduledTask -TaskName 'CCS App Updates'", timeout=15)
        deadline = time.monotonic() + 16 * 60
        relayed, cancel_sent = 0, False
        while time.monotonic() < deadline:
            if not cancel_sent and cancelled():
                try:
                    write_private_json(cancel_path, {"nonce": nonce})
                    cancel_sent = True
                except OSError:
                    pass
            try:
                value = json.loads(progress_path.read_text(encoding="utf-8"))
                events = value.get("events") if value.get("nonce") == nonce else None
                if isinstance(events, list):
                    for event in events[relayed:]:
                        emit(event)
                    relayed = max(relayed, len(events))
            except (OSError, ValueError, AttributeError):
                pass
            try:
                value = json.loads(result_path.read_text(encoding="utf-8"))
                if value.get("nonce") == nonce and isinstance(value.get("results"), list):
                    return {"results": value["results"]}
            except (OSError, ValueError, AttributeError):
                pass
            time.sleep(1)
        raise UpdateFailure("timeout")


def task_child_progress(nonce):
    """emit/cancelled pair for the Windows task child, bound to its nonce."""
    root = pathlib.Path.home() / ".ccs/app-updates"
    events = []

    def emit(event):
        events.append(event)
        try:
            write_private_json(root / "windows-task-progress.json", {"nonce": nonce, "events": events})
        except (OSError, ValueError):
            pass

    def cancelled():
        try:
            return json.loads((root / "windows-task-cancel.json").read_text(encoding="utf-8")).get("nonce") == nonce
        except (OSError, ValueError, AttributeError):
            return False

    return emit, cancelled


def stream_progress():
    """emit/cancelled pair for a dashboard that asked for live progress.

    Events go to stdout as one JSON object per line, flushed at once; the final
    {"results": [...]} line stays last. A "cancel" line on stdin (the dashboard
    writes it; over ssh it arrives the same way) stops apps not yet started.
    """
    import threading
    stop = threading.Event()

    def listen():
        # Raw reads on the descriptor: a daemon thread parked inside the
        # buffered sys.stdin would hold its lock while the interpreter exits.
        try:
            descriptor = sys.stdin.fileno()
        except (AttributeError, OSError, ValueError):
            return
        seen = b""
        while not stop.is_set():
            try:
                chunk = os.read(descriptor, 256)
            except OSError:
                return
            if not chunk:
                return
            seen = (seen + chunk)[-64:]
            if b"cancel\n" in seen or b"cancel\r\n" in seen:
                stop.set()

    threading.Thread(target=listen, daemon=True).start()

    def emit(event):
        try:
            print(json.dumps(event, ensure_ascii=True, allow_nan=False, separators=(",", ":")), flush=True)
        except (OSError, ValueError):
            pass

    return emit, stop.is_set


def read_task_request(request_path):
    """(nonce, Antigravity reviewed set) from the coordinator's request file.

    The scheduled task starts with fixed arguments, so the reviewed set comes
    only from a request carrying a valid nonce; anything else is None (held).
    """
    try:
        request = json.loads(pathlib.Path(request_path).read_text(encoding="utf-8"))
        value = request.get("nonce")
        if not (isinstance(value, str) and len(value) == 32 and all(char in "0123456789abcdef" for char in value)):
            return None, None
        reviewed = request.get("agyReviewed")
        if not isinstance(reviewed, list) or not all(isinstance(item, str) for item in reviewed):
            return value, None
        return value, parse_reviewed_versions(",".join(reviewed))
    except (OSError, ValueError, AttributeError, TypeError):
        return None, None


def main():
    parser = argparse.ArgumentParser(description="Inventory or explicitly update the fixed installed app set")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--inventory", action="store_true")
    mode.add_argument("--apply", action="store_true")
    parser.add_argument("--platform", choices=("ubuntu", "mac", "windows"), required=True)
    parser.add_argument("--task-child", action="store_true")
    parser.add_argument("--state-dir", type=pathlib.Path)
    parser.add_argument("--dashboard-job", action="store_true")
    # The dashboard passes the Antigravity CLI's reviewed versions (from its
    # packaged release file); without it a packaged release file next to this
    # helper is read, and without either the Antigravity update is held.
    parser.add_argument("--agy-reviewed")
    args = parser.parse_args()
    native = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "ubuntu"
    if args.platform != native:
        parser.error("The selected platform must match this computer.")
    if args.state_dir is not None:
        if not args.state_dir.is_absolute():
            parser.error("The update state directory must be absolute.")
        os.environ["AAC_UPDATE_STATE_DIR"] = str(args.state_dir)
    if args.dashboard_job:
        if args.state_dir is None or args.platform != "ubuntu":
            parser.error("A dashboard job requires Ubuntu and an explicit state directory.")
        os.environ["AAC_UPDATE_DASHBOARD_JOB"] = "1"
    task_nonce = None
    agy_reviewed = parse_reviewed_versions(args.agy_reviewed) if args.agy_reviewed is not None else read_release_versions()
    if args.task_child and args.apply and args.platform == "windows":
        task_nonce, agy_reviewed = read_task_request(pathlib.Path.home() / ".ccs/app-updates/windows-task-request.json")
    emit = cancelled = None
    if args.apply and args.task_child and task_nonce:
        emit, cancelled = task_child_progress(task_nonce)
    elif args.apply and os.environ.get("AAC_UPDATE_PROGRESS") == "1":
        # Only a dashboard that reads line-by-line progress sets this; an
        # older one keeps receiving exactly one JSON document.
        emit, cancelled = stream_progress()
    try:
        if not args.apply:
            installations = detect(args.platform)
            payload = {"inventory": True, "apps": [
                {"appId": app_id, "installed": install is not None, "version": install.version if install else None, "manager": install.manager if install else None,
                 **({"parts": install.parts} if getattr(install, "parts", None) is not None else {}),
                 **({"strays": install.strays} if getattr(install, "strays", None) else {})}
                for app_id, install in installations.items()
            ]}
        elif args.platform == "windows" and not args.task_child:
            payload = windows_interactive_apply(emit, cancelled, agy_reviewed)
        else:
            payload = run_apply(args.platform, emit, cancelled, agy_reviewed)
    except UpdateFailure as error:
        payload = {"results": [result(app_id, args.platform, "failed", code=error.code) for app_id in APP_LABELS]}
    except Exception:
        payload = {"results": [result(app_id, args.platform, "failed", code="update_failed") for app_id in APP_LABELS]}
    if args.task_child and args.apply and args.platform == "windows":
        request_path = pathlib.Path.home() / ".ccs/app-updates/windows-task-request.json"
        try:
            if task_nonce:
                write_private_json(request_path.with_name("windows-task-result.json"), {"nonce": task_nonce, **payload})
        except (OSError, ValueError):
            pass
    print(json.dumps(payload, ensure_ascii=True, allow_nan=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
