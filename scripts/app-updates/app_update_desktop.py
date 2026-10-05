"""Publisher-verified desktop package updates for the detected installation."""

import json
import os
import pathlib
import plistlib
import shutil
import time
import uuid
import zipfile
import xml.etree.ElementTree as ET

from app_update_common import (
    Install, UpdateFailure, command, download, powershell, private_temporary,
    ps_quote, result, version_text, version_tuple, write_private_json,
)
from app_update_processes import family, live_contexts, main_contexts, restart_desktops, scan, terminate_desktops

# The fixed publisher desktop packages are large (the Codex Windows MSIX is
# ~870 MiB, the ChatGPT DMG ~716 MiB); the CLI-installer caps stay small.
DESKTOP_MAXIMUM = 2 * 1024 * 1024 * 1024
# Bounds one desktop download inside the host's 15-minute apply budget.
DESKTOP_TIMEOUT = 780
CLAUDE_DARWIN_FEED = "https://downloads.claude.ai/releases/darwin/universal/RELEASES.json"
CLAUDE_DARWIN_PREFIX = "https://downloads.claude.ai/releases/darwin/"
MAC = {
    "codex-desktop": ("ChatGPT.app", "com.openai.codex", "2DC432GLL2", "https://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg"),
    # Claude's darwin package comes from CLAUDE_DARWIN_FEED; its old claude.ai
    # redirect answers 403 to every non-browser client.
    "claude-desktop": ("Claude.app", "com.anthropic.claudefordesktop", "Q6L2SF6YDW", None),
}
WINDOWS = {
    "codex-desktop": ("OpenAI.Codex", "app/ChatGPT.exe"),
    "claude-desktop": ("Claude", "app/Claude.exe"),
}


def bundle_info(path):
    try:
        with (pathlib.Path(path) / "Contents/Info.plist").open("rb") as handle:
            return plistlib.load(handle)
    except (OSError, ValueError, plistlib.InvalidFileException):
        return {}


def windows_package(app_id):
    identity, executable = WINDOWS[app_id]
    script = "$ErrorActionPreference='Stop'; $p=Get-AppxPackage -Name " + ps_quote(identity) + " | Sort-Object {[Version]$_.Version} -Descending | Select-Object -First 1; if($p){@{name=$p.Name;version=$p.Version.ToString();publisher=$p.Publisher;root=$p.InstallLocation}|ConvertTo-Json -Compress}"
    try:
        raw = powershell(script, timeout=15)
        value = json.loads(raw) if raw.strip() else None
        if not isinstance(value, dict) or value.get("name") != identity or not isinstance(value.get("root"), str):
            return None
        root = pathlib.Path(value["root"])
        target = root / executable
        if not target.is_file():
            return None
        return Install(app_id, "windows", target, version_text(value.get("version")), "msix", identity, value.get("publisher"), root)
    except (UpdateFailure, ValueError):
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
        text = command(["/usr/bin/dpkg-query", "-W", "-f=${Version}", package], timeout=10, capture=True)
        return Install(app_id, platform, pathlib.Path(executable), version_text(text), "apt", package, package_root=pathlib.Path(executable).parent)
    except UpdateFailure:
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


def restore_closed(install, contexts):
    """Reopen only the instances a graceful quit attempt actually closed."""
    try:
        closed = [item for item in contexts if item not in live_contexts(install.platform, contexts)]
        if closed:
            restart_desktops(install, closed)
    except (UpdateFailure, OSError):
        pass


def dmg_candidates(install, temporary):
    """Mount the fixed publisher DMG; return (matching apps, device to detach)."""
    dmg = temporary / "update.dmg"
    download(MAC[install.app_id][3], dmg, maximum=DESKTOP_MAXIMUM, timeout=DESKTOP_TIMEOUT)
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


def claude_zip_candidates(install, temporary):
    """Claude's own release feed; None when the installed app is already current.

    The old claude.ai download redirect now answers 403 to every non-browser
    client, so the version check and the package come from the publisher's
    plain-CDN release feed that redirect pointed at. Checking the feed first
    keeps an up-to-date app from downloading hundreds of megabytes.
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
    url = None
    for entry in releases:
        target = entry.get("updateTo") if isinstance(entry, dict) else None
        if not isinstance(target, dict) or version_text(str(target.get("version") or "")) != parsed:
            continue
        candidate = target.get("url")
        if isinstance(candidate, str) and candidate.startswith(CLAUDE_DARWIN_PREFIX):
            url = candidate
            break
    if url is None:
        raise UpdateFailure("update_failed")
    package = temporary / "update.zip"
    download(url, package, maximum=DESKTOP_MAXIMUM, timeout=DESKTOP_TIMEOUT)
    extracted = temporary / "extracted"
    extracted.mkdir()
    # ditto keeps symlinks, resources and the code signature intact.
    command(["/usr/bin/ditto", "-x", "-k", str(package), str(extracted)], timeout=600)
    return [path for path in extracted.glob("*.app") if bundle_info(path).get("CFBundleIdentifier") == install.identity]


def update_mac(install):
    before = install.version
    contexts, installed = [], False
    backup, stage, attached = None, None, None
    with private_temporary() as temporary:
        try:
            if install.app_id == "claude-desktop":
                candidates = claude_zip_candidates(install, temporary)
                if candidates is None:
                    return result(install.app_id, "mac", "current", before, before, install.manager)
            else:
                candidates, attached = dmg_candidates(install, temporary)
            if len(candidates) != 1:
                raise UpdateFailure("signature_failed")
            info = verify_mac(candidates[0], install.app_id)
            after = version_text(info.get("CFBundleShortVersionString"))
            if version_tuple(after) <= version_tuple(before):
                return result(install.app_id, "mac", "current", before, before, install.manager)
            if not os.access(install.path.parent, os.W_OK):
                raise UpdateFailure("unsupported")
            stage = install.path.parent / (".CCS-update-stage-" + uuid.uuid4().hex + ".app")
            backup = install.path.parent / (".CCS-update-backup-" + uuid.uuid4().hex + ".app")
            shutil.copytree(candidates[0], stage, symlinks=True)
            verify_mac(stage, install.app_id)
            try:
                contexts = main_contexts(install, scan("mac"))
            except UpdateFailure as error:
                if error.code != "restart_context":
                    raise
                # Its running instances cannot be captured for a safe relaunch.
                return result(install.app_id, "mac", "action_required", before, before, install.manager, "quit_required")
            if terminate_desktops(install, contexts):
                # The app refused to quit. Never force it and never swap the
                # bundle under a running app: its later helper spawns would mix
                # old and new files. Report the actionable quit state instead.
                restore_closed(install, contexts)
                return result(install.app_id, "mac", "action_required", before, before, install.manager, "quit_required")
            os.rename(install.path, backup)
            try:
                os.rename(stage, install.path)
                installed = True
            except OSError:
                os.rename(backup, install.path)
                raise UpdateFailure()
            install.version = after
            mark_restart(install, after)
            try:
                restarted = restart_desktops(install, contexts)
            except (UpdateFailure, OSError):
                return result(install.app_id, "mac", "restart_failed", before, after, install.manager, "restart_failed", True)
            value = result(install.app_id, "mac", "updated", before, after, install.manager, attempted=True, restarted=restarted)
            value["forcedStops"] = 0
            clear_restart(install)
            return value
        except (UpdateFailure, OSError) as error:
            if contexts and not installed:
                # Restore only the closed instances; the update never landed.
                restore_closed(install, contexts)
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
    """Register the verified package; 'ok', or 'in_use' when Windows refuses
    because instances are running. Never requests a forced shutdown."""
    script = ("$ErrorActionPreference='Stop'; try { Add-AppxPackage -Path " + ps_quote(package) + " } catch {"
              " if(('{0:X8}' -f $_.Exception.HResult) -in @('80073D31','80073CFF')){ 'in_use'; exit 0 }; throw };"
              " 'ok'")
    value = (powershell(script, timeout=300) or "").strip().splitlines()
    return value[-1].strip().lower() if value else "ok"


def update_windows(install):
    before = install.version
    contexts, installed = [], False
    architecture = "arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "x64"
    url = ("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-" + architecture + ".msix" if install.app_id == "codex-desktop"
           else "https://claude.ai/api/desktop/win32/" + architecture + "/msix/latest/redirect")
    with private_temporary() as temporary:
        package = temporary / "update.msix"
        try:
            download(url, package, maximum=DESKTOP_MAXIMUM, timeout=DESKTOP_TIMEOUT)
            info = msix_info(package)
            if info.get("Name") != install.identity or info.get("Publisher") != install.publisher or info.get("ProcessorArchitecture", "").lower() not in (architecture, "neutral"):
                raise UpdateFailure("signature_failed")
            after = version_text(info.get("Version"))
            if not after:
                raise UpdateFailure("signature_failed")
            if version_tuple(after) <= version_tuple(before):
                return result(install.app_id, "windows", "current", before, before, install.manager)
            try:
                contexts = main_contexts(install, scan("windows"))
                remaining = terminate_desktops(install, contexts)
            except UpdateFailure as error:
                if error.code != "restart_context":
                    raise
                # Its instances cannot be captured for a graceful close and
                # relaunch; the OS-level in-use update below needs no context.
                contexts, remaining = [], family(install, scan("windows"))
            # Add-AppxPackage verifies the Microsoft Store/publisher signature
            # and upgrades the same per-user package, preserving LocalState.
            state = add_appx_package(package)
            installed = state == "ok"
            if remaining:
                if not installed:
                    # Windows refused while instances run and could not defer:
                    # nothing was installed; report the actionable quit state.
                    restore_closed(install, contexts)
                    return result(install.app_id, "windows", "action_required", before, before, install.manager, "quit_required", True)
                # In-use staged update: registered now; the running instances
                # keep the old build until they quit, then the new one starts.
                restore_closed(install, contexts)
                refreshed = windows_package(install.app_id)
                value = result(install.app_id, "windows", "staged", before,
                               refreshed.version if refreshed and refreshed.version == after else after,
                               install.manager, "staged", True)
                value["forcedStops"] = 0
                return value
            if not installed:
                raise UpdateFailure("update_failed")
            mark_restart(install, after)
            refreshed = windows_package(install.app_id)
            if refreshed is None or refreshed.version != after:
                raise UpdateFailure("version_unknown")
            try:
                restarted = restart_desktops(refreshed, contexts)
            except (UpdateFailure, OSError):
                return result(install.app_id, "windows", "restart_failed", before, refreshed.version, install.manager, "restart_failed", True)
            value = result(install.app_id, "windows", "updated", before, refreshed.version, install.manager, attempted=True, restarted=restarted)
            value["forcedStops"] = 0
            clear_restart(install)
            return value
        except UpdateFailure as error:
            if contexts and not installed:
                restore_closed(install, contexts)
            return result(install.app_id, "windows", "failed", before, before, install.manager, error.code, installed)


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
            seconds = max(30, min(900, int((deadline or time.monotonic() + 900) - time.monotonic())))
            return json.loads(command([shutil.which("node") or "/usr/bin/node", bridge, "--operation", "desktop", "--timeout-seconds", str(seconds)], timeout=seconds + 15, capture=True))
        try:
            contexts = main_contexts(install, scan("ubuntu"))
        except UpdateFailure as error:
            if error.code != "restart_context":
                raise
            # Its running instances cannot be captured for a safe relaunch.
            return result(install.app_id, "ubuntu", "action_required", before, before, "apt", "quit_required")
        if terminate_desktops(install, contexts):
            # Upgrading under a running desktop app would mix old and new files
            # on its next helper spawn. Ask for a quit instead; never force it.
            restore_closed(install, contexts)
            return result(install.app_id, "ubuntu", "action_required", before, before, "apt", "quit_required")
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
        value["forcedStops"] = 0
        return value
    except UpdateFailure as error:
        if contexts:
            restore_closed(install, contexts)
        return result(install.app_id, "ubuntu", "failed", before, before, "apt", error.code)


def update_desktop(install, deadline=None):
    if install.platform == "ubuntu":
        return update_linux(install, deadline)
    # A prior verified package update may have timed out during relaunch. A
    # subsequent explicit button click retries that restart without reinstalling.
    try:
        pending = json.loads(restart_marker(install).read_text(encoding="utf-8"))
        retry = pending.get("version") == install.version
    except (OSError, ValueError):
        retry = False
    if retry:
        try:
            contexts = main_contexts(install, scan(install.platform))
            if terminate_desktops(install, contexts):
                # Still running after a graceful quit attempt: the update is
                # already installed; its relaunch stays pending, never forced.
                return result(install.app_id, install.platform, "restart_failed", install.version, install.version, install.manager, "restart_failed")
            restarted = restart_desktops(install, contexts)
            clear_restart(install)
            value = result(install.app_id, install.platform, "updated", install.version, install.version, install.manager, attempted=False, restarted=restarted)
            value["forcedStops"] = 0
            return value
        except (UpdateFailure, OSError):
            return result(install.app_id, install.platform, "restart_failed", install.version, install.version, install.manager, "restart_failed")
    return {"mac": update_mac, "windows": update_windows}[install.platform](install)


def restart_marker(install):
    return pathlib.Path.home() / ".ccs/app-updates" / (install.app_id + "-pending-restart.json")


def mark_restart(install, version):
    write_private_json(restart_marker(install), {"version": version})


def clear_restart(install):
    restart_marker(install).unlink(missing_ok=True)
