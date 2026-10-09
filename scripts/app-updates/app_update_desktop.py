"""Publisher-verified desktop package updates for the detected installation."""

import contextlib
import ctypes
import dataclasses
import json
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import time
import uuid
import zipfile
import xml.etree.ElementTree as ET

from app_update_common import (
    Install, RangeReader, UpdateFailure, command, download, powershell, private_temporary,
    ps_quote, remote_fingerprint, result, version_text, version_tuple, write_private_json,
)
from app_update_processes import (
    desktop_arguments, family, live_contexts, main_contexts, restart_desktops, scan, terminate_desktops,
)


# Desktop packages are hundreds of megabytes (the Codex MSIX is over 900 MB)
# and its host links need minutes, not seconds. Still bounded, still verified.
DESKTOP_DOWNLOAD_MAXIMUM = 2 * 1024 * 1024 * 1024
DESKTOP_DOWNLOAD_TIMEOUT = 600
DOWNLOAD_BLOCKED_RETRIES = 3
# Claude's darwin packages live on the publisher's plain-CDN release feed;
# the old claude.ai redirect answers 403 to every non-browser client.
CLAUDE_DARWIN_FEED = "https://downloads.claude.ai/releases/darwin/universal/RELEASES.json"
CLAUDE_DARWIN_PREFIX = "https://downloads.claude.ai/releases/darwin/"
# Windows Codex now ships through the Microsoft Store; the direct ChatGPT-x64.msix
# stopped moving at 26.930.7945.0. The app itself reads this feed for updates.
CODEX_STORE_FEED = "https://persistent.oaistatic.com/codex-app-prod/windows-store-update.json"
CODEX_STORE_ID = "9PLM9XGG6VKS"
# winget exit codes (read unsigned) for a Store package with nothing newer to install here:
# 0x8A15002B UPDATE_NOT_APPLICABLE ("No applicable update found") and 0x8A150061 PACKAGE_ALREADY_INSTALLED.
WINGET_NO_NEWER = {0x8A15002B, 0x8A150061}
# winget's "nothing newer" answer is trusted this long for the same feed build and installed version.
STORE_NO_NEWER_SECONDS = 6 * 60 * 60
STORE_MEMORY_BYTES = 4096


def download_desktop(url, destination):
    """Fetch a desktop package, tolerating flaky bot-challenge refusals.

    Raises UpdateFailure('download_blocked') when every attempt is refused so
    callers report check_in_app; any other failure propagates unchanged.
    """
    for attempt in range(DOWNLOAD_BLOCKED_RETRIES):
        try:
            download(url, destination, maximum=DESKTOP_DOWNLOAD_MAXIMUM, timeout=DESKTOP_DOWNLOAD_TIMEOUT)
            return
        except UpdateFailure as error:
            if error.code != "download_blocked" or attempt + 1 == DOWNLOAD_BLOCKED_RETRIES:
                raise
            time.sleep(2)


MAC = {
    "codex-desktop": ("ChatGPT.app", "com.openai.codex", "2DC432GLL2", "https://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg"),
    # Claude's package URL comes from CLAUDE_DARWIN_FEED at run time.
    "claude-desktop": ("Claude.app", "com.anthropic.claudefordesktop", "Q6L2SF6YDW", None),
}
# Identity, executable and the fixed AppId of its Application element: a packaged
# instance without arguments reopens by AUMID <PackageFamilyName>!<AppId>, as the Start menu does.
WINDOWS = {
    "codex-desktop": ("OpenAI.Codex", "app/ChatGPT.exe", "App"),
    "claude-desktop": ("Claude", "app/Claude.exe", "Claude"),
}
# A relaunched MSIX instance cold-starts slower than T3; still bounded.
WINDOWS_REOPEN_SECONDS = 45
# DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, spelled out so fixtures run on any OS.
DETACHED = 0x00000008 | 0x00000200
APPMODEL_ERROR_NO_PACKAGE = 15700


@dataclasses.dataclass
class MsixInstall(Install):
    # <Name>_<PublisherId> of the registered package; None when it was not exactly that shape.
    package_family: str = None


# Reading only an MSIX's manifest over HTTP ranges takes well under a second
# (three requests, under 1 MB); past this the caller falls back to the full
# download, shown live as "Downloading".
REMOTE_MANIFEST_SECONDS = 20


def desktop_running(install):
    """True while any process of this desktop app runs. Read-only: never asks it to quit.

    A running Mac desktop app is never quit, closed, restarted or killed by the
    updater (N3). Its update waits until the user quits it, and that is
    reported at once instead of after a download. Windows closes and reopens
    it instead (windows_replace).
    """
    return bool(family(install, scan(install.platform)))


def quit_first(install, before):
    return result(install.app_id, install.platform, "action_required", before, before, install.manager, "quit_first", False)


def remote_msix_identity(url, timeout=REMOTE_MANIFEST_SECONDS):
    """The Identity of the published MSIX, read from its manifest alone; None when unavailable.

    Only decides whether a newer version exists. Installing still downloads
    the whole package and verifies it (manifest here, publisher signature in
    Add-AppxPackage) before anything changes.
    """
    try:
        reader = RangeReader(url, timeout=timeout)
        try:
            return msix_info(reader)
        finally:
            reader.close()
    except (UpdateFailure, OSError, ValueError):
        return None


def bundle_info(path):
    try:
        with (pathlib.Path(path) / "Contents/Info.plist").open("rb") as handle:
            return plistlib.load(handle)
    except (OSError, ValueError, plistlib.InvalidFileException):
        return {}


def windows_package(app_id):
    identity, executable, _ = WINDOWS[app_id]
    # Sort-Object keeps the query a single object when two versions coexist mid-staging.
    script = "$ErrorActionPreference='Stop'; $p=Get-AppxPackage -Name " + ps_quote(identity) + " | Sort-Object {[Version]$_.Version} -Descending | Select-Object -First 1; if($p){@{name=$p.Name;version=$p.Version.ToString();publisher=$p.Publisher;root=$p.InstallLocation;family=$p.PackageFamilyName}|ConvertTo-Json -Compress}"
    try:
        raw = powershell(script, timeout=30)
        value = json.loads(raw) if raw.strip() else None
        if not isinstance(value, dict) or value.get("name") != identity or not isinstance(value.get("root"), str):
            return None
        root = pathlib.Path(value["root"])
        target = root / executable
        if not target.is_file():
            return None
        # Only the exact <Name>_<13-character publisher id> may become an AUMID.
        package_family = value.get("family")
        if not isinstance(package_family, str) or not re.fullmatch(re.escape(identity) + "_[a-z0-9]{13}", package_family):
            package_family = None
        return MsixInstall(app_id, "windows", target, version_text(value.get("version")), "msix", identity, value.get("publisher"), root,
                           package_family=package_family)
    except UpdateFailure as error:
        # A query that ran out of time says nothing about the package: report
        # "Check timed out" rather than "not installed".
        return Install(app_id, "windows", None, manager="msix", probe="timeout") if error.code == "timeout" else None
    except ValueError:
        return None


def detect_desktop(app_id, platform):
    if platform == "mac":
        name, identity, _, _ = MAC[app_id]
        for path in (pathlib.Path("/Applications") / name, pathlib.Path.home() / "Applications" / name):
            info = bundle_info(path)
            if info.get("CFBundleIdentifier") == identity:
                return Install(app_id, platform, path, version_text(info.get("CFBundleShortVersionString")), "official-download", identity, package_root=path)
        return None
    if platform == "windows":
        return windows_package(app_id)
    package, executable = ("chatgpt", "/usr/lib/chatgpt/ChatGPT") if app_id == "codex-desktop" else ("claude-desktop", "/usr/lib/claude-desktop/claude-desktop")
    if not pathlib.Path(executable).is_file():
        return None
    try:
        text = command(["/usr/bin/dpkg-query", "-W", "-f=${Version}", package], timeout=20, capture=True)
        return Install(app_id, platform, pathlib.Path(executable), version_text(text), "apt", package, package_root=pathlib.Path(executable).parent)
    except UpdateFailure as error:
        if error.code == "timeout":
            return Install(app_id, platform, pathlib.Path(executable), None, "apt", package, probe="timeout")
        return None


def verify_mac(app, app_id):
    _, identity, team, _ = MAC[app_id]
    info = bundle_info(app)
    if info.get("CFBundleIdentifier") != identity or not version_text(info.get("CFBundleShortVersionString")):
        raise UpdateFailure("signature_failed")
    try:
        command(["/usr/bin/codesign", "--verify", "--deep", "--strict", app], timeout=60)
    except UpdateFailure:
        raise UpdateFailure("signature_failed") from None
    # codesign writes metadata to stderr; capture this one fixed verification
    # command locally rather than exposing its text through any result.
    import subprocess
    try:
        completed = subprocess.run(["/usr/bin/codesign", "-dv", "--verbose=4", str(app)], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=15)
        metadata = completed.stderr.decode("utf-8", "replace")
        if completed.returncode != 0 or len(metadata) > 65536 or ("TeamIdentifier=" + team) not in metadata.splitlines():
            raise UpdateFailure("signature_failed")
    except (OSError, subprocess.TimeoutExpired):
        raise UpdateFailure("signature_failed") from None
    return info


def dmg_candidates(install, temporary):
    """Mount the fixed publisher DMG; return (matching apps, device to detach)."""
    dmg = temporary / "update.dmg"
    download_desktop(MAC[install.app_id][3], dmg)
    mounted = command(["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-plist", dmg], timeout=60, capture=True)
    try:
        mounted = plistlib.loads(mounted.encode("utf-8"))
    except Exception:
        raise UpdateFailure("update_failed") from None
    entities = [entry for entry in mounted.get("system-entities", []) if isinstance(entry, dict) and entry.get("mount-point")]
    volumes = [pathlib.Path(entry["mount-point"]) for entry in entities]
    attached = next((entry.get("dev-entry") for entry in entities), None)
    candidates = [path for volume in volumes for path in volume.glob("*.app") if bundle_info(path).get("CFBundleIdentifier") == install.identity]
    return candidates, attached


def claude_release_url(install, temporary):
    """Claude's own release feed: the newer package URL, or None when the installed app is current.

    The old claude.ai download redirect answers 403 (Cloudflare challenge) to
    every non-browser client, so the version check and the package come from
    the publisher's plain-CDN release feed that redirect pointed at. Checking
    the feed first keeps a current app from downloading hundreds of megabytes,
    and lets a running app report "quit first" before any download.
    """
    feed = temporary / "RELEASES.json"
    download(CLAUDE_DARWIN_FEED, feed, maximum=256 * 1024, timeout=60)
    try:
        value = json.loads(feed.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        raise UpdateFailure("update_failed") from None
    current = value.get("currentRelease") if isinstance(value, dict) else None
    releases = value.get("releases") if isinstance(value, dict) else None
    parsed = version_text(current)
    if not parsed or not isinstance(releases, list):
        raise UpdateFailure("update_failed")
    if version_tuple(parsed) <= version_tuple(install.version):
        return None
    for entry in releases:
        target = entry.get("updateTo") if isinstance(entry, dict) else None
        if not isinstance(target, dict) or version_text(str(target.get("version") or "")) != parsed:
            continue
        candidate = target.get("url")
        if isinstance(candidate, str) and candidate.startswith(CLAUDE_DARWIN_PREFIX):
            return candidate
    raise UpdateFailure("update_failed")


def claude_zip_candidates(install, temporary, url):
    package = temporary / "update.zip"
    download_desktop(url, package)
    extracted = temporary / "extracted"
    extracted.mkdir()
    # ditto keeps symlinks, resources and the code signature intact.
    command(["/usr/bin/ditto", "-x", "-k", str(package), str(extracted)], timeout=600)
    return [path for path in extracted.glob("*.app") if bundle_info(path).get("CFBundleIdentifier") == install.identity]


def package_memory(install):
    """Where the version of the last verified Mac DMG is remembered, keyed by its HEAD fingerprint."""
    return pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-mac-package.json")


def remembered_dmg_version(install, fingerprint):
    """The verified version of this exact published DMG, or None when it was never verified here.

    The Codex DMG has no version feed; without this every check of a running
    Codex would download 750 MB just to learn whether it is newer.
    """
    if fingerprint is None:
        return None
    try:
        value = json.loads(package_memory(install).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(value, dict) or any(value.get(key) != fingerprint[key] for key in ("url", "size", "validator")):
        return None
    return version_text(str(value.get("version") or "")) or None


def remember_dmg_version(install, fingerprint, version):
    if fingerprint is None:
        return
    try:
        write_private_json(package_memory(install), {**fingerprint, "version": version})
    except (OSError, ValueError):
        pass


def update_mac(install, phase=None):
    """Update a Mac desktop app only while it is not running; never quit it.

    A running app with a newer version reports quit_first within seconds:
    Claude knows the newer version from its small release feed, Codex from the
    remembered fingerprint of its last verified DMG. Only when the version is
    unknown does a running Codex wait for the download (shown live as
    Downloading) before it can say whether it needs a quit.
    """
    phase = phase or (lambda name: None)
    before = install.version
    installed = False
    backup, stage = None, None
    try:
        running = desktop_running(install)
    except UpdateFailure as error:
        return result(install.app_id, "mac", "failed", before, before, install.manager, error.code)
    with private_temporary() as temporary:
        attached = None
        fingerprint = None
        try:
            if install.app_id == "claude-desktop":
                url = claude_release_url(install, temporary)
                if url is None:
                    return result(install.app_id, "mac", "current", before, before, install.manager)
                if running:
                    return quit_first(install, before)
                phase("downloading")
                candidates = claude_zip_candidates(install, temporary, url)
            else:
                fingerprint = remote_fingerprint(MAC[install.app_id][3])
                known = remembered_dmg_version(install, fingerprint)
                if known and version_tuple(known) <= version_tuple(before):
                    return result(install.app_id, "mac", "current", before, before, install.manager)
                if known and running:
                    return quit_first(install, before)
                phase("downloading")
                candidates, attached = dmg_candidates(install, temporary)
            phase("updating")
            if len(candidates) != 1:
                raise UpdateFailure("signature_failed")
            info = verify_mac(candidates[0], install.app_id)
            after = version_text(info.get("CFBundleShortVersionString"))
            if install.app_id != "claude-desktop":
                remember_dmg_version(install, fingerprint, after)
            if version_tuple(after) <= version_tuple(before):
                return result(install.app_id, "mac", "current", before, before, install.manager)
            if running or desktop_running(install):
                return quit_first(install, before)
            if not os.access(install.path.parent, os.W_OK):
                raise UpdateFailure("unsupported")
            stage = install.path.parent / (".CCS-update-stage-" + uuid.uuid4().hex + ".app")
            backup = install.path.parent / (".CCS-update-backup-" + uuid.uuid4().hex + ".app")
            shutil.copytree(candidates[0], stage, symlinks=True)
            verify_mac(stage, install.app_id)
            # The app may have been opened while the package downloaded: look
            # again right before the swap and never replace it under a user.
            if desktop_running(install):
                return quit_first(install, before)
            os.rename(install.path, backup)
            try:
                os.rename(stage, install.path)
                installed = True
            except OSError:
                os.rename(backup, install.path)
                raise UpdateFailure()
            install.version = after
            try:
                verify_mac(install.path, install.app_id)
            except UpdateFailure:
                os.rename(install.path, stage)
                os.rename(backup, install.path)
                install.version = before
                installed = False
                raise
            value = result(install.app_id, "mac", "updated", before, after, install.manager, attempted=True, restarted=0)
            value["forcedStops"] = 0
            return value
        except (UpdateFailure, OSError) as error:
            if isinstance(error, UpdateFailure) and error.code == "download_blocked":
                return result(install.app_id, "mac", "action_required", before, before, install.manager, "check_in_app", False)
            return result(install.app_id, "mac", "failed", before, install.version, install.manager, error.code if isinstance(error, UpdateFailure) else "update_failed", installed)
        finally:
            if attached:
                try: command(["/usr/bin/hdiutil", "detach", attached], timeout=30)
                except UpdateFailure: pass
            if stage and stage.exists():
                shutil.rmtree(stage, ignore_errors=True)
            if installed and backup and backup.exists():
                shutil.rmtree(backup, ignore_errors=True)


def msix_info(path):
    try:
        with zipfile.ZipFile(path) as package:
            member = package.getinfo("AppxManifest.xml")
            if member.file_size > 1024 * 1024:
                raise UpdateFailure("signature_failed")
            xml = ET.fromstring(package.read(member))
        identity = next((element for element in xml if element.tag.split("}")[-1] == "Identity"), None)
        if identity is None:
            raise UpdateFailure("signature_failed")
        return identity.attrib
    except (OSError, ValueError, zipfile.BadZipFile, KeyError, ET.ParseError):
        raise UpdateFailure("signature_failed") from None


def add_appx_package(package):
    """Install the verified MSIX, classifying needs-closing rejections.

    The wrapper always exits 0 and reports on stdout, which stays private and
    is never emitted: a rejection for running apps maps to quit_first, while
    any other deployment failure stays update_failed.
    """
    script = ("$ErrorActionPreference='Stop'; try { Add-AppxPackage -Path " + ps_quote(package)
              + "; Write-Output 'CCS_ADDAPPX_OK' } catch { Write-Output ('CCS_ADDAPPX_FAILED: ' + $_.Exception.ToString()) }")
    text = powershell(script, timeout=600)
    if "CCS_ADDAPPX_OK" in text.split():
        return
    lowered = text.lower()
    if "ccs_addappx_failed" in lowered and ("0x80073d02" in lowered or "need to be closed" in lowered):
        raise UpdateFailure("quit_first")
    raise UpdateFailure()


def codex_store_version(install):
    """The Store build the Codex app itself offers, or None when the feed is unreadable or foreign."""
    try:
        with private_temporary() as temporary:
            target = temporary / "windows-store-update.json"
            download(CODEX_STORE_FEED, target, maximum=65536, timeout=30)
            value = json.loads(target.read_text(encoding="utf-8"))
    except (UpdateFailure, OSError, ValueError):
        return None
    if not isinstance(value, dict) or value.get("packageIdentity") != install.identity or value.get("storeProductId") != CODEX_STORE_ID:
        return None
    return version_text(value.get("buildVersion"))


def process_package(item):
    """The package full name of a running process, or None when it has no package identity.

    Read per instance: a WindowsApps executable started by path can be packaged
    too (AAC's Claude profiles are), so the start method is never assumed.
    """
    from ctypes import wintypes
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.GetPackageFullName.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.UINT), wintypes.LPWSTR]
    kernel.GetPackageFullName.restype = wintypes.LONG
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x1000, False, item.pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        raise UpdateFailure("restart_context")
    try:
        length = wintypes.UINT(128)  # PACKAGE_FULL_NAME_MAX_LENGTH and its terminator.
        name = ctypes.create_unicode_buffer(length.value)
        status = kernel.GetPackageFullName(handle, ctypes.byref(length), name)
        if status == APPMODEL_ERROR_NO_PACKAGE:
            return None
        if status != 0:
            raise UpdateFailure("restart_context")
        return name.value
    finally:
        kernel.CloseHandle(handle)


def windows_explorer():
    return pathlib.Path(os.environ.get("WINDIR") or "C:\\Windows") / "explorer.exe"


def windows_capture(install):
    """The running main instances and how to reopen each exactly as it was started.

    Read-only. An instance with a data-directory argument reopens from the
    refreshed package the way AAC's launcher starts a Claude profile (that exe,
    its folder, the same argument), packaged or not. One without arguments
    reopens through its AUMID when packaged (the Start menu, or Claude's
    default) and by path when not. One in another session, one of another
    package, one whose AUMID cannot be built and any it cannot inspect raise
    UpdateFailure("quit_first"): nothing may close for it.
    """
    try:
        contexts = main_contexts(install, scan("windows"))
        if not contexts:
            return [], []
        identity, _, application = WINDOWS[install.app_id]
        session = ctypes.c_ulong()
        if not ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session)):
            raise UpdateFailure("restart_context")
        plans = []
        for item in contexts:
            if item.session != session.value:
                raise UpdateFailure("quit_first")
            name, arguments = process_package(item), desktop_arguments(item, "windows")
            # Another package's instance: this start cannot be reproduced, so nothing closes for it.
            if name is not None and not name.startswith(identity + "_"):
                raise UpdateFailure("quit_first")
            if arguments or name is None:
                # By path: the launcher's start for a profile, or a plain start of an unpackaged app.
                plans.append((item, None, arguments))
            elif install.package_family and windows_explorer().is_file():
                plans.append((item, install.package_family + "!" + application, []))
            else:
                raise UpdateFailure("quit_first")
        return contexts, plans
    except UpdateFailure as error:
        raise UpdateFailure("quit_first" if error.code == "restart_context" else error.code) from None


def reopen_windows(install, plans):
    """Start each planned instance from this install, then wait for its main processes to show."""
    for _, aumid, arguments in plans:
        if aumid:
            # explorer.exe exits 1 even after it opened the app: only the scan below decides.
            argv, cwd = [str(windows_explorer()), "shell:AppsFolder\\" + aumid], None
        else:
            # The same direct start as AAC's Claude launcher: current exe, its folder, one data directory.
            argv, cwd = [str(install.path), *arguments], str(install.path.parent)
        subprocess.Popen(argv, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=DETACHED)
    if not plans:
        return 0
    deadline = time.monotonic() + WINDOWS_REOPEN_SECONDS
    while True:
        existing = family(install, scan("windows"))
        pids = {item.pid for item in existing}
        if len([item for item in existing if item.ppid not in pids]) >= len(plans):
            return len(plans)
        if time.monotonic() >= deadline:
            raise UpdateFailure("restart_failed")
        time.sleep(.5)


def windows_replace(install, before, deploy, accepted, current_codes=()):
    """Install a newer package; a running app is closed first and reopened as each instance was started.

    The instances are captured right before closing (the app may have been
    opened or closed meanwhile) and every reopen is planned before anything
    closes. Closing is terminate_desktops: WM_CLOSE, a bounded 15 s wait, then
    a stop of only identity-checked family survivors (forcedStops). A failed
    close or install reopens what was closed from the old package, which is
    still registered; a failed reopen after a good install is restart_failed.
    A failure code in current_codes means nothing newer was installed: once
    reopened the row is current, not failed.
    """
    app_id, manager = install.app_id, install.manager
    try:
        contexts, plans = windows_capture(install)
    except UpdateFailure as error:
        if error.code == "quit_first":
            return quit_first(install, before)
        return result(app_id, "windows", "failed", before, before, manager, error.code)
    forced = 0
    if contexts:
        try:
            forced = terminate_desktops(install, contexts)
        except Exception as error:
            # Never leave a closed instance closed: reopen what already ended from the untouched package.
            with contextlib.suppress(Exception):
                live = live_contexts("windows", contexts)
                reopen_windows(install, [plan for plan in plans if not any(plan[0] is item for item in live)])
            return result(app_id, "windows", "failed", before, before, manager, error.code if isinstance(error, UpdateFailure) else "restart_failed")
    refreshed = None
    try:
        deploy()
        refreshed = windows_package(app_id)
        if refreshed is None or refreshed.path is None or not accepted(refreshed):
            raise UpdateFailure("version_unknown")
    except Exception as error:
        # Reopen from whatever package is registered now: the old one when the install failed.
        reopened = 0
        with contextlib.suppress(Exception):
            reopened = reopen_windows(refreshed if refreshed is not None and refreshed.path is not None else install, plans)
        code = error.code if isinstance(error, UpdateFailure) else "update_failed"
        # A needs-closing rejection even after the close still asks the user to quit.
        if code == "quit_first":
            return quit_first(install, before)
        if code in current_codes:
            # Nothing newer was installed, so the app is current; a reopen that failed is still reported.
            if reopened != len(plans):
                return result(app_id, "windows", "restart_failed", before, before, manager, "restart_failed", True)
            return result(app_id, "windows", "current", before, before, manager, code, True, reopened)
        return result(app_id, "windows", "failed", before, before, manager, code, True)
    try:
        restarted = reopen_windows(refreshed, plans)
    except Exception:
        return result(app_id, "windows", "restart_failed", before, refreshed.version, manager, "restart_failed", True)
    value = result(app_id, "windows", "updated", before, refreshed.version, manager, "desktop_reopened" if plans else None, True, restarted)
    value["forcedStops"] = forced
    if plans:
        value["restartTargets"] = [{"kind": "desktop"}]
    return value


def store_memory(install):
    """Where winget's last "nothing newer" answer for Codex is remembered, beside the Mac package memory."""
    return pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-windows-store.json")


def remembered_store_answer(install, feed):
    """True while winget's "nothing newer" answer holds for this feed build and installed version.

    The answer holds for STORE_NO_NEWER_SECONDS from when it was given. A memory
    for another installed version is dropped. A missing, unreadable, oversized,
    malformed, expired or future-dated (the clock went backwards) memory is not
    trusted, so the install decides.
    """
    try:
        with store_memory(install).open("rb") as handle:
            raw = handle.read(STORE_MEMORY_BYTES + 1)
        if len(raw) > STORE_MEMORY_BYTES:
            return False
        value = json.loads(raw.decode("utf-8"))
    except (OSError, ValueError, RecursionError):  # Some JSON parsers recurse: a deeply nested file exhausts them.
        return False
    if not isinstance(value, dict):
        return False
    remembered_feed, remembered_installed, checked = value.get("feedBuild"), value.get("installedVersion"), value.get("checkedAt")
    if not isinstance(remembered_feed, str) or not isinstance(remembered_installed, str) or type(checked) is not int:
        return False
    if remembered_installed != install.version:
        forget_store_answer(install)
        return False
    return remembered_feed == feed and 0 <= time.time() - checked < STORE_NO_NEWER_SECONDS


def remember_store_answer(install, feed):
    """Remember that winget found nothing newer than the installed version for this feed build."""
    try:
        write_private_json(store_memory(install), {"feedBuild": feed, "installedVersion": install.version, "checkedAt": int(time.time())})
    except (OSError, ValueError):
        pass


def forget_store_answer(install):
    with contextlib.suppress(OSError):
        store_memory(install).unlink(missing_ok=True)


def update_windows_store(install, phase, feed):
    """Install the newer Codex Store build through winget's msstore source, reopening a running Codex.

    Only winget's install can say whether the Store offers a newer build here: its
    listings never name this product. So its "nothing newer" answer is remembered
    for STORE_NO_NEWER_SECONDS, and while that holds a running Codex is not closed
    again. Otherwise the install decides: when it reports nothing newer, the closed
    Codex is reopened and the row is current; any other failure stays failed.
    """
    if remembered_store_answer(install, feed):
        return result(install.app_id, "windows", "current", install.version, install.version, install.manager, "store_no_newer")
    winget = shutil.which("winget.exe") or shutil.which("winget")
    if not winget:
        return result(install.app_id, "windows", "failed", install.version, install.version, install.manager, "unsupported")
    phase("updating")

    def deploy():
        # The Store verifies the package and upgrades the same OpenAI.Codex family in place.
        try:
            command([winget, "install", "--id", CODEX_STORE_ID, "--source", "msstore", "--exact", "--silent",
                     "--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"], timeout=900)
        except UpdateFailure as error:
            if error.exit_code is not None and (error.exit_code & 0xFFFFFFFF) in WINGET_NO_NEWER:
                remember_store_answer(install, feed)
                raise UpdateFailure("store_no_newer") from None
            raise
        forget_store_answer(install)

    def accepted(refreshed):
        return refreshed.publisher == install.publisher and bool(refreshed.version) and version_tuple(refreshed.version) > version_tuple(install.version)
    return windows_replace(install, install.version, deploy, accepted, current_codes=("store_no_newer",))


def update_windows(install, phase=None):
    """Update a Windows MSIX desktop app, closing and reopening it when it runs.

    The published manifest alone (HTTP ranges, under a second) says whether a
    newer version exists, so a current app reports current, and a running app
    that could not be reopened reports quit_first, before any download. Only
    when the manifest cannot be read does the full package decide. The
    download (shown live as Downloading) and its verification finish while
    the app keeps running; windows_replace then closes, installs and reopens.
    """
    phase = phase or (lambda name: None)
    before = install.version
    architecture = "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "x64"
    url = ("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-" + architecture + ".msix" if install.app_id == "codex-desktop"
           else "https://claude.ai/api/desktop/win32/" + architecture + "/msix/latest/redirect")

    def published(info):
        if info.get("Name") != install.identity or info.get("Publisher") != install.publisher or info.get("ProcessorArchitecture", "").lower() not in (architecture, "neutral"):
            return None
        return version_text(info.get("Version"))

    if install.app_id == "codex-desktop":
        store = codex_store_version(install)
        if store and version_tuple(store) > version_tuple(before):
            return update_windows_store(install, phase, store)
    try:
        remote = remote_msix_identity(url)
        available = published(remote) if remote is not None else None
        if available and version_tuple(available) <= version_tuple(before):
            return result(install.app_id, "windows", "current", before, before, install.manager)
        if available:
            # Read-only: an instance that could not be reopened answers before any download.
            windows_capture(install)
    except UpdateFailure as error:
        if error.code == "quit_first":
            return quit_first(install, before)
        return result(install.app_id, "windows", "failed", before, before, install.manager, error.code)
    with private_temporary() as temporary:
        package = temporary / "update.msix"
        try:
            phase("downloading")
            download_desktop(url, package)
            phase("updating")
            after = published(msix_info(package))
            if not after:
                raise UpdateFailure("signature_failed")
            if version_tuple(after) <= version_tuple(before):
                return result(install.app_id, "windows", "current", before, before, install.manager)
        except UpdateFailure as error:
            if error.code == "download_blocked":
                return result(install.app_id, "windows", "action_required", before, before, install.manager, "check_in_app", False)
            return result(install.app_id, "windows", "failed", before, before, install.manager, error.code)
        # Add-AppxPackage verifies the Microsoft Store/publisher signature and
        # upgrades the same per-user package, preserving LocalState. A
        # rejection for a running app (0x80073D02) reports quit_first.
        return windows_replace(install, before, lambda: add_appx_package(package), lambda refreshed: refreshed.version == after)


def apt_package_sources(policy):
    """Package sources in apt-cache policy's version table, minus the installed status file."""
    sources = []
    for line in policy.splitlines():
        # A source row reads "<priority> <url or path>"; a version row starts with the version itself.
        fields = line.split()
        if len(fields) > 1 and fields[0].isdigit() and ("/" in fields[1] or ":" in fields[1]) and fields[1] != "/var/lib/dpkg/status":
            sources.append(fields[1])
    return sources


def update_linux(install, deadline=None):
    before = install.version
    contexts = []
    try:
        # Refresh package metadata, then scope the actual upgrade to this one
        # already installed, signed-repository package; no full system upgrade.
        command(["/usr/bin/sudo", "-n", "/usr/bin/apt-get", "update", "-qq"], timeout=180)
        policy = command(["/usr/bin/apt-cache", "policy", install.identity], timeout=15, capture=True)
        candidate = next((version_text(line.split(":", 1)[1]) for line in policy.splitlines() if line.strip().startswith("Candidate:")), None)
        if not candidate:
            raise UpdateFailure("version_unknown")
        # An Ubuntu upgrade turns third-party sources off. Without one, apt only
        # knows the installed copy, so "current" would be a false answer.
        if not apt_package_sources(policy):
            return result(install.app_id, "ubuntu", "action_required", before, before, "apt", "source_disabled", False)
        pending = pathlib.Path.home() / ".ccs/app-updates/codex-desktop-pending-restart.json"
        retry_restart = False
        try:
            retry_restart = json.loads(pending.read_text(encoding="utf-8")).get("version") == before
        except (OSError, ValueError):
            pass
        if version_tuple(candidate) <= version_tuple(before) and not retry_restart:
            return result(install.app_id, "ubuntu", "current", before, before, "apt")
        if install.app_id == "codex-desktop":
            sources = command(["/usr/bin/apt-cache", "madison", "chatgpt"], timeout=15, capture=True)
            trusted = any(len(parts := [part.strip() for part in line.split("|")]) == 3 and version_text(parts[1]) == candidate and parts[2].startswith("https://persistent.oaistatic.com/codex-app-prod/linux/deb ") for line in sources.splitlines())
            if not trusted:
                raise UpdateFailure("signature_failed")
            bridge = pathlib.Path(__file__).with_name("app_update_codex.cjs")
            # Same bounded bridge budget as the Codex CLI (lock, idle wait, apt).
            seconds = max(30, min(420, int((deadline or time.monotonic() + 420) - time.monotonic())))
            return json.loads(command([shutil.which("node") or "/usr/bin/node", bridge, "--operation", "desktop", "--timeout-seconds", str(seconds)], timeout=seconds + 15, capture=True))
        contexts = main_contexts(install, scan("ubuntu"))
        forced = terminate_desktops(install, contexts)
        command(["/usr/bin/sudo", "-n", "/usr/bin/apt-get", "install", "--only-upgrade", "-y", install.identity], timeout=180,
                env={"DEBIAN_FRONTEND": "noninteractive"})
        refreshed = detect_desktop(install.app_id, "ubuntu")
        if refreshed is None or refreshed.version == before:
            raise UpdateFailure("version_unknown")
        try:
            restarted = restart_desktops(refreshed, contexts)
        except (UpdateFailure, OSError):
            return result(install.app_id, "ubuntu", "restart_failed", before, refreshed.version, "apt", "restart_failed", True)
        value = result(install.app_id, "ubuntu", "updated", before, refreshed.version, "apt", attempted=True, restarted=restarted)
        value["forcedStops"] = forced
        return value
    except UpdateFailure as error:
        if contexts:
            from app_update_processes import live_contexts
            closed = [item for item in contexts if item not in live_contexts("ubuntu", contexts)]
            try: restart_desktops(install, closed)
            except (UpdateFailure, OSError): pass
        return result(install.app_id, "ubuntu", "failed", before, before, "apt", error.code)


def update_desktop(install, deadline=None, phase=None):
    """phase(name) reports a long step ("downloading", then "updating") so the page can show it live."""
    if install.platform == "ubuntu":
        return update_linux(install, deadline)
    # Older builds quit the app, installed, then relaunched it, and left this
    # marker when the relaunch failed. The installed bits are already the new
    # version and no flow reads the marker now (Windows reopens in the same
    # run), so it is obsolete: drop it and run the normal check.
    try:
        clear_restart(install)
    except OSError:
        pass
    return {"mac": update_mac, "windows": update_windows}[install.platform](install, phase)


def restart_marker(install):
    return pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-pending-restart.json")


def clear_restart(install):
    restart_marker(install).unlink(missing_ok=True)
