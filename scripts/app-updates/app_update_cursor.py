"""Cursor's supported native updater, independently of T3's own update."""

import os
import pathlib
import re
import time

from app_update_common import Install, UpdateFailure, cli_probe, command, powershell, ps_quote, result, version_text
from app_update_macos import gui_command


CURSOR_VERSION = re.compile(r"^20\d{2}\.\d{2}\.\d{2}-[A-Za-z0-9.-]{1,80}$")


def cursor_launcher(platform):
    if platform == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
        ps1 = local / "cursor-agent/cursor-agent.ps1"
        return ps1 if ps1.is_file() else local / "cursor-agent/cursor-agent.cmd"
    return pathlib.Path.home() / ".local/bin/cursor-agent"


def windows_command(path, argument, timeout=20):
    # Both arguments are package-owned constants. PowerShell's call operator
    # handles the official ps1 launcher without cmd.exe's quoting rules.
    return powershell("$ErrorActionPreference='Stop'; & " + ps_quote(path) + " " + argument +
                      "; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }", timeout=timeout)


def detect_cursor(platform):
    path = cursor_launcher(platform)
    if not path.is_file():
        return None
    if platform != "windows":
        versions = pathlib.Path.home() / ".local/share/cursor-agent/versions"
        if not path.resolve().is_relative_to(versions.resolve()) or path.resolve().name != "cursor-agent":
            return Install("cursor-cli", platform, path, manager="unsupported")
        # Older Mac launchers consult Keychain even for --version. The managed
        # target's release directory is the installer's version, without auth.
        if platform == "mac" and CURSOR_VERSION.fullmatch(path.resolve().parent.name):
            return Install("cursor-cli", platform, path, path.resolve().parent.name)
        version, probe = cli_probe(path)
    else:
        try:
            version, probe = version_text(windows_command(path, "--version")), None
        except UpdateFailure as error:
            version, probe = None, "timeout" if error.code == "timeout" else "failed"
    return Install("cursor-cli", platform, path, version, probe=probe or (None if version else "failed"))


def cursor_staged(path):
    versions = path.parent / "versions"
    return (any(versions.glob("*.new")) or
            any((versions / "pending-launchers").glob("*.pending")))


def update_cursor(install, deadline=None, progress=None):
    before, attempted = install.version, False
    try:
        remaining = 180 if deadline is None else min(180, deadline - time.monotonic())
        if remaining <= 0:
            raise UpdateFailure("timeout")
        attempted = True
        if install.platform == "windows":
            windows_command(install.path, "update", remaining)
        elif install.platform == "mac":
            gui_command("cursor-update", remaining)
        else:
            command([install.path, "update"], timeout=remaining)
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
        if install.platform == "windows" and cursor_staged(install.path):
            return result("cursor-cli", install.platform, "action_required", before, after, install.manager,
                          "cursor_update_staged", attempted)
        return result("cursor-cli", install.platform, "failed", before, after, install.manager,
                      error.code if isinstance(error, UpdateFailure) else "update_failed", attempted)
