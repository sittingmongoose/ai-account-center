"""T3 nightly desktop/bundled server and standalone runtime updates."""

import argparse
import base64
import collections
import contextlib
import dataclasses
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import re
import shutil
import stat
import sys
import time
import urllib.request
import unicodedata
import uuid
import zipfile

from app_update_common import (
    Install, UpdateFailure, asar_package, cli_probe, command, download, execution_lock, powershell, private_temporary,
    ps_quote, result, write_private_json,
)
from app_update_processes import live_contexts, main_contexts, restart_desktops, scan, terminate_desktops

RELEASES = "https://api.github.com/repos/pingdotgg/t3code/releases?per_page=20"
RELEASE_BASE = "https://github.com/pingdotgg/t3code/releases/download/"
VERSION = re.compile(r"^\d+\.\d+\.\d+-nightly\.\d{8}\.\d+$")
MAC_NAME = "T3 Code (Nightly).app"
MAC_ID = "com.t3tools.t3code"
MAC_TEAM = "ARK85ZXQ4Z"
WINDOWS_NAME = "T3 Code (Nightly).exe"
WINDOWS_PUBLISHER = "T3 Tools Inc"
MAX_PACKAGE = 800 * 1024 * 1024
# Read-only process table; tests point this at a fake one.
PROC_ROOT = "/proc"
ACP_UNIT = "t3-acp-update.service"
PRUNE_LIMIT = 20
COMPONENT_KEYS = ("utc", "cursorAgent", "acpUpdater", "prunedRuntimes", "daemonReload")


@dataclasses.dataclass
class T3Install(Install):
    runtime: pathlib.Path = None
    runtime_version: str = None
    desktop: bool = False


def exact_version(value):
    return value if isinstance(value, str) and len(value) <= 64 and VERSION.fullmatch(value) else None


def version_key(value):
    return tuple(int(part) for part in re.findall(r"\d+", value))


def state_root():
    # AAC resolves --config-dir / CCS_DIR / CCS_HOME; do not reinterpret them.
    return pathlib.Path(os.environ.get("AAC_UPDATE_STATE_DIR", str(pathlib.Path.home() / ".ccs/app-updates")))


def runtime_install():
    home = pathlib.Path.home()
    versions = home / ".t3/runtime/versions"
    launcher = home / ".local/bin/t3"
    if launcher.is_file() and launcher.resolve().parent.parent == versions.resolve():
        return launcher
    candidates = [path / "t3" for path in versions.glob("*") if exact_version(path.name) and (path / "t3").is_file()]
    return max(candidates, key=lambda path: version_key(path.parent.name)) if candidates else None


def windows_bundle_version(executable):
    # PE ProductVersion loses the nightly suffix. Read only the small root
    # package.json from Electron's ASAR, never execute bundled JavaScript.
    package = asar_package(executable.parent / "resources/app.asar")
    return exact_version(package.get("version")) if package and package.get("name") == "t3code" else None


def mac_bundle_version(bundle):
    try:
        with (bundle / "Contents/Info.plist").open("rb") as stream:
            info = plistlib.load(stream)
        return exact_version(info.get("CFBundleShortVersionString")) if info.get("CFBundleIdentifier") == MAC_ID else None
    except (OSError, ValueError, plistlib.InvalidFileException):
        return None


def detect_t3(host):
    runtime = runtime_install() if host != "windows" else None
    runtime_version, probe = cli_probe(runtime) if runtime else (None, None)
    runtime_version = exact_version(runtime_version)
    if host == "mac":
        bundle = pathlib.Path("/Applications") / MAC_NAME
        if bundle.is_dir():
            return T3Install("t3-code", host, bundle, mac_bundle_version(bundle), "official-download",
                             MAC_ID, MAC_TEAM, bundle, probe=probe, runtime=runtime, runtime_version=runtime_version, desktop=True)
    elif host == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
        executable = local / "Programs/t3code" / WINDOWS_NAME
        if executable.is_file():
            return T3Install("t3-code", host, executable, windows_bundle_version(executable), "official-download",
                             publisher=WINDOWS_PUBLISHER, package_root=executable.parent, desktop=True)
    if runtime:
        return T3Install("t3-code", host, runtime, runtime_version, "native", probe=probe,
                         runtime=runtime, runtime_version=runtime_version)
    return None


def latest_release():
    try:
        deadline = time.monotonic() + 30
        for page in range(1, 6):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise UpdateFailure("timeout")
            url = RELEASES if page == 1 else RELEASES + "&page=" + str(page)
            request = urllib.request.Request(url, headers={"User-Agent": "AAC-App-Updater", "Accept": "application/vnd.github+json"})
            with urllib.request.urlopen(request, timeout=min(20, remaining)) as response:
                if response.geturl() != url:
                    raise UpdateFailure()
                raw = response.read(2 * 1024 * 1024 + 1)
            if len(raw) > 2 * 1024 * 1024:
                raise UpdateFailure()
            releases = json.loads(raw)
            versions = [row["tag_name"][1:] for row in releases if isinstance(row, dict) and
                        not row.get("draft") and isinstance(row.get("tag_name"), str) and row["tag_name"].startswith("v") and exact_version(row["tag_name"][1:])]
            if versions:
                return max(versions, key=version_key)
            if not releases:
                break
        raise UpdateFailure()
    except UpdateFailure:
        raise
    except Exception:
        raise UpdateFailure() from None


def manifest_hash(text, version, asset):
    return manifest_asset(text, version, asset)[0]


def manifest_asset(text, version, asset):
    """(sha512 digest, size or None) of one asset in an electron-builder latest.yml.

    A bounded subset of electron-builder's YAML; no arbitrary YAML tags,
    URLs or commands are accepted. Bind the hash to this exact asset/version.
    """
    if len(text) > 65536 or not re.search(r"^version:\s*[\"']?" + re.escape(version) + r"[\"']?\s*$", text, re.M):
        raise UpdateFailure("signature_failed")
    entries = re.split(r"(?m)^\s*-\s+url:\s*", text)[1:]
    found = []
    for entry in entries:
        lines = entry.splitlines()
        if lines and lines[0].strip().strip("\"'") == asset:
            match = re.search(r"(?m)^\s+sha512:\s*([A-Za-z0-9+/=]+)\s*$", entry)
            if match:
                # Only this entry's own indented lines: the top-level keys follow the last one.
                size = re.search(r"\A[^\n]*\n(?:[ \t]+[^\n]*\n)*?[ \t]+size:\s*(\d{1,12})\s*$", entry, re.M)
                found.append((match.group(1), int(size.group(1)) if size else None))
    if len(found) != 1:
        raise UpdateFailure("signature_failed")
    try:
        digest = base64.b64decode(found[0][0], validate=True)
        if len(digest) == 64:
            return digest, found[0][1]
    except ValueError:
        pass
    raise UpdateFailure("signature_failed")


def verified_download(host, version, temporary, phase, deadline):
    architecture = platform.machine().lower()
    if host == "mac" and architecture in ("arm64", "aarch64"):
        asset, manifest = "T3-Code-" + version + "-arm64.zip", "nightly-mac.yml"
    elif host == "windows" and architecture in ("amd64", "x86_64"):
        asset, manifest = "T3-Code-" + version + "-x64.exe", "nightly.yml"
    else:
        raise UpdateFailure("unsupported")
    base = RELEASE_BASE + "v" + version + "/"
    metadata, package = temporary / manifest, temporary / asset
    phase("downloading")
    download(base + manifest, metadata, maximum=65536, timeout=30)
    digest = manifest_hash(metadata.read_text(encoding="utf-8"), version, asset)
    download(base + asset, package, maximum=MAX_PACKAGE, timeout=budget(deadline, 600))
    sha = hashlib.sha512()
    with package.open("rb") as stream:
        for chunk in iter(lambda: stream.read(256 * 1024), b""):
            sha.update(chunk)
    if sha.digest() != digest:
        raise UpdateFailure("signature_failed")
    phase("updating")
    return package


def budget(deadline, maximum):
    remaining = int(deadline - time.monotonic())
    if remaining < 1:
        raise UpdateFailure("timeout")
    return min(maximum, remaining)


def update_runtime(install, version, deadline):
    # No --yes: the native updater only rewrites the service definition with
    # non-TTY stdin. It must never restart the server that hosts these agents.
    timeout = budget(deadline, 300)
    if install.platform == "ubuntu":
        # Retain intent even if installation completes but its probe times out.
        write_private_json(state_root() / "t3-code-pending-restart.json", {"version": version})
    command([install.runtime, "update", version, "--channel", "nightly"], timeout=timeout)
    replacement = pathlib.Path.home() / ".t3/runtime/versions" / version / "t3"
    actual, _probe = cli_probe(replacement)
    if actual != version:
        raise UpdateFailure("version_unknown")


def verify_mac(bundle, version, identity=MAC_ID, team=MAC_TEAM, bundle_version=None):
    # codesign reads a bare -R value as a requirement file path; "-R=" passes the text inline.
    requirement = 'identifier "' + identity + '" and anchor apple generic and certificate leaf[subject.OU] = "' + team + '"'
    try:
        command(["/usr/bin/codesign", "--verify", "--deep", "--strict", bundle], timeout=60)
        command(["/usr/bin/codesign", "--verify", "-R=" + requirement, bundle], timeout=30)
        command(["/usr/sbin/spctl", "--assess", "--type", "execute", bundle], timeout=60)
    except UpdateFailure:
        raise UpdateFailure("signature_failed") from None
    if (bundle_version or mac_bundle_version)(bundle) != version:
        raise UpdateFailure("signature_failed")


def verify_windows(package):
    try:
        powershell("$ErrorActionPreference='Stop'; $s=Get-AuthenticodeSignature -LiteralPath " + ps_quote(package) + "; "
                   "if($s.Status -ne 'Valid' -or -not $s.SignerCertificate -or "
                   "$s.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false) -ne 'T3 Tools Inc'){exit 1}", timeout=60)
    except UpdateFailure:
        raise UpdateFailure("signature_failed") from None


def health_check(host):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen("http://127.0.0.1:3773/", timeout=2) as response:
                if response.status == 200:
                    return
        except Exception:
            pass
        time.sleep(.3)
    raise UpdateFailure("restart_failed")


def check_mac_archive(package):
    def key(name):
        # Default macOS volumes compare case and Unicode-normalized aliases.
        return pathlib.PurePosixPath(unicodedata.normalize("NFD", str(name)).casefold())

    with zipfile.ZipFile(package) as archive:
        entries = archive.infolist()
        if len(entries) > 100000 or sum(item.file_size for item in entries) > 2 * 1024 * 1024 * 1024:
            raise UpdateFailure("signature_failed")
        paths, links = set(), {}
        for item in entries:
            name = pathlib.PurePosixPath(item.filename)
            if not name.parts or name.is_absolute() or ".." in name.parts or "\\" in item.filename or "\0" in item.filename or key(name) in paths:
                raise UpdateFailure("signature_failed")
            paths.add(key(name))
            if stat.S_ISLNK(item.external_attr >> 16):
                if item.file_size > 4096:
                    raise UpdateFailure("signature_failed")
                try:
                    target = archive.read(item).decode("utf-8")
                except UnicodeError:
                    raise UpdateFailure("signature_failed") from None
                if not target or target.startswith("/") or "\\" in target or "\0" in target:
                    raise UpdateFailure("signature_failed")
                links[key(name)] = target
        # An extractor must never write through an alias, regardless of order.
        # Electron's framework aliases have no archive entries beneath them.
        if any(parent in links for name in paths for parent in name.parents):
            raise UpdateFailure("signature_failed")
        for name, target in links.items():
            remaining = collections.deque([*name.parent.parts, *target.split("/")])
            resolved, hops = [], 0
            while remaining:
                part = remaining.popleft()
                if part in ("", "."):
                    continue
                if part == "..":
                    if not resolved:
                        raise UpdateFailure("signature_failed")
                    resolved.pop()
                    continue
                resolved.append(part)
                link = key(pathlib.PurePosixPath(*resolved))
                if link in links:
                    hops += 1
                    if hops > 40:
                        raise UpdateFailure("signature_failed")
                    resolved.pop()
                    remaining.extendleft(reversed(links[link].split("/")))


def update_mac(install, version, temporary, deadline, phase):
    package = verified_download("mac", version, temporary, phase, deadline)
    check_mac_archive(package)
    extracted = temporary / "extracted"
    command(["/usr/bin/ditto", "-x", "-k", package, extracted], timeout=budget(deadline, 120))
    staged = extracted / MAC_NAME
    verify_mac(staged, version)
    backup = install.path.with_name(".aac-t3-rollback-" + uuid.uuid4().hex + ".app")
    adjacent = install.path.with_name(".aac-t3-stage-" + uuid.uuid4().hex + ".app")
    moved = completed = False
    contexts = []
    try:
        command(["/usr/bin/ditto", staged, adjacent], timeout=budget(deadline, 120))
        verify_mac(adjacent, version)
        contexts = main_contexts(install, scan("mac"))
        forced = terminate_desktops(install, contexts)
        os.rename(install.path, backup)
        moved = True
        os.rename(adjacent, install.path)
        verify_mac(install.path, version)
        if contexts:
            restart_desktops(install, contexts)
        else:
            command(["/usr/bin/open", "-a", install.path], timeout=15)
        health_check("mac")
        completed = True
    except Exception:
        if moved:
            terminate_desktops(install, main_contexts(install, scan("mac")))
            shutil.rmtree(install.path, ignore_errors=True)
            os.rename(backup, install.path)
        # Relaunch only when the captured app is gone; a failed quit must not open a second T3.
        with contextlib.suppress(UpdateFailure, OSError):
            if not live_contexts("mac", contexts):
                restart_desktops(install, contexts)
        raise
    finally:
        shutil.rmtree(adjacent, ignore_errors=True)
        if completed and backup.exists():
            shutil.rmtree(backup)
    return len(contexts), forced


def nsis_silent(package, seconds):
    """Run a verified NSIS installer with /S; its exit code decides, a timeout kills its whole tree."""
    # Give the installer its own wait/kill budget. Killing only an outer
    # PowerShell on timeout would leave NSIS replacing files during rollback.
    output = powershell("$ErrorActionPreference='Stop'; $p=Start-Process -FilePath " + ps_quote(package) +
                        " -ArgumentList '/S' -PassThru; if(-not $p.WaitForExit(" + str(seconds * 1000) + ")){" +
                        "& taskkill.exe /PID $p.Id /T /F | Out-Null; if(-not $p.WaitForExit(10000)){exit 1}; 'timeout'; exit 0}; " +
                        "$p.Refresh(); if($p.ExitCode -ne 0){exit 1}", timeout=seconds + 15)
    if output and output.strip() == "timeout":
        raise UpdateFailure("timeout")


def update_windows(install, version, temporary, deadline, phase):
    package = verified_download("windows", version, temporary, phase, deadline)
    verify_windows(package)
    contexts = main_contexts(install, scan("windows"))
    # The existing InteractiveToken/Limited task owns this helper; reuse its
    # session-bound process shutdown and detached desktop relaunch machinery.
    backup = temporary / "rollback"
    shutil.copytree(install.package_root, backup)
    forced = 0
    installed = False
    try:
        forced = terminate_desktops(install, contexts)
        nsis_silent(package, budget(deadline, 180))
        installed = True
        if windows_bundle_version(install.path) != version:
            raise UpdateFailure("version_unknown")
        if contexts:
            restart_desktops(install, contexts)
        else:
            powershell("Start-Process -FilePath " + ps_quote(install.path), timeout=15)
        health_check("windows")
    except Exception:
        # Stop only the new T3 family before restoring the previous bundle.
        try:
            terminate_desktops(install, main_contexts(install, scan("windows")))
            shutil.rmtree(install.package_root)
            shutil.copytree(backup, install.package_root)
        except Exception:
            # Keep the private recovery copy if files cannot be restored.
            recovery = state_root() / ("t3-code-rollback-" + uuid.uuid4().hex)
            recovery.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            shutil.move(str(backup), recovery)
            raise UpdateFailure("restart_failed" if installed else "update_failed") from None
        with contextlib.suppress(UpdateFailure, OSError):
            restart_desktops(install, contexts)
        raise
    return len(contexts), forced


def schedule_restart(version):
    root = state_root()
    marker = root / "t3-code-pending-restart.json"
    write_private_json(marker, {"version": version})
    unit = "aac-t3-restart-" + uuid.uuid4().hex[:12]
    command(["/usr/bin/systemd-run", "--user", "--collect", "--unit=" + unit,
             "--property=RuntimeMaxSec=1200", "/usr/bin/python3", pathlib.Path(__file__).resolve(),
             "--restart-service", str(os.getpid()), "--state-dir", str(root),
             *(["--dashboard-job"] if os.environ.get("AAC_UPDATE_DASHBOARD_JOB") == "1" else [])], timeout=15)
    return {"kind": "systemd", "service": "t3code.service", "delaySeconds": 30}


def job_finished(root, dashboard=False):
    try:
        raw = (root / "dashboard-job.json").read_bytes()
        if len(raw) > 1024 * 1024:
            return False
        job = json.loads(raw).get("job")
        if not isinstance(job, dict) or job.get("state") not in ("completed", "failed"):
            return False
        hosts = job.get("hosts")
        # Every computer the job saved must be done (Ubuntu, Mac, Windows and Nas1 today),
        # so the restart also waits for one added later. An empty table proves nothing.
        return hosts is None or (isinstance(hosts, dict) and len(hosts) > 0 and all(
            isinstance(host, dict) and host.get("state") == "done" for host in hosts.values()))
    except FileNotFoundError:
        return not dashboard  # Only standalone --apply may have no job file.
    except (OSError, ValueError, AttributeError):
        return False


def deferred_restart(parent_pid, root=None, dashboard=False):
    """Detached worker: never restart in a helper or dashboard job's lifetime.

    Holds the helper execution lock and AAC's exclusive dashboard lock across
    the final check and restart. No AAC service is touched.
    """
    root = root or state_root()
    lock = root / "dashboard-update.lock"
    marker = root / "t3-code-pending-restart.json"
    deadline = time.monotonic() + 18 * 60
    quiet_since = None
    while time.monotonic() < deadline:
        if not marker.exists():
            return  # Another detached worker already completed this restart.
        try:
            os.kill(parent_pid, 0)
            parent_alive = True
        except ProcessLookupError:
            parent_alive = False
        try:
            # Same per-user lock as standalone --apply, even when the dashboard
            # stores its job in a custom directory. File existence is not a lock.
            with execution_lock():
                if parent_alive or lock.exists() or not job_finished(root, dashboard):
                    quiet_since = None
                elif quiet_since is None:
                    quiet_since = time.monotonic()
                elif time.monotonic() - quiet_since >= 30:
                    try:
                        descriptor = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    except FileExistsError:
                        quiet_since = None
                        continue
                    try:
                        with os.fdopen(descriptor, "w") as output:
                            json.dump({"pid": os.getpid()}, output)
                        if marker.exists() and job_finished(root, dashboard):
                            # Without a readable target version the restart cannot be verified; do not start one.
                            expected = marker_version(marker)
                            if expected is None:
                                raise UpdateFailure("restart_failed")
                            daemon_reload = reload_unit()
                            command(["/usr/bin/systemctl", "--user", "restart", "t3code.service"], timeout=60)
                            health_check("ubuntu")
                            # A stale launcher can keep an old server up; keep the marker for a retry.
                            if running_runtime_version() != expected:
                                raise UpdateFailure("restart_failed")
                            marker.unlink(missing_ok=True)
                            finish_ubuntu_restart(root, daemon_reload)
                            return
                    finally:
                        lock.unlink(missing_ok=True)
                    quiet_since = None
        except UpdateFailure as error:
            if error.code != "busy":
                raise
            quiet_since = None
        time.sleep(1)
    raise UpdateFailure("restart_failed")


def reload_unit():
    """Re-read t3code.service after the native updater rewrote it; a reload never restarts anything.

    The updater rewrites the unit without a daemon-reload, so systemd keeps
    starting the old launcher until one runs. Returns whether a reload ran.
    """
    try:
        stale = command(["/usr/bin/systemctl", "--user", "show", "--property=NeedDaemonReload", "--value", "t3code.service"], timeout=10, capture=True)
        if stale.strip() != "yes":
            return False
        command(["/usr/bin/systemctl", "--user", "daemon-reload"], timeout=30)
        return True
    except UpdateFailure:
        return False


def real_path(path):
    # Unlike Path.resolve(), realpath never raises on a symlink loop.
    return pathlib.Path(os.path.realpath(path))


def runtime_under(path, versions):
    """The runtime version directory that path lies in, or None."""
    try:
        parts = path.relative_to(versions).parts
    except ValueError:
        return None
    return parts[0] if len(parts) > 1 else None


def runtime_of(pid, versions):
    """(version, argv) when a process runs a managed runtime's t3 binary, else None."""
    try:
        executable = pathlib.Path(os.readlink(os.path.join(PROC_ROOT, pid, "exe")).removesuffix(" (deleted)"))
    except OSError:
        return None
    if executable.name != "t3" or executable.parent.parent != versions or not exact_version(executable.parent.name):
        return None
    try:
        with pathlib.Path(PROC_ROOT, pid, "cmdline").open("rb") as stream:
            argv = stream.read(4096).decode("utf-8", "replace").split("\0")
    except OSError:
        argv = []
    return executable.parent.name, argv


def child_pids(pid):
    """Direct children of pid from the kernel's per-thread child lists; read-only and bounded."""
    children = []
    try:
        tasks = sorted(os.listdir(os.path.join(PROC_ROOT, pid, "task")))[:64]
    except OSError:
        return children
    for task in tasks:
        try:
            children += pathlib.Path(PROC_ROOT, pid, "task", task, "children").read_text(encoding="ascii").split()
        except (OSError, UnicodeError):
            continue
    return [child for child in children[:256] if child.isdigit()]


def running_runtime_version():
    """Read which installed runtime the service runs, without restarting it.

    The unit's main process is a launcher that starts the real server from the
    runtime named in service-state.json, so for a launcher the server's version
    is the one that matters.
    """
    try:
        pid = command(["/usr/bin/systemctl", "--user", "show", "--property=MainPID", "--value", "t3code.service"], timeout=10, capture=True).strip()
        if not pid.isdigit() or int(pid) <= 1:
            return None
        versions = real_path(pathlib.Path.home() / ".t3/runtime/versions")
        main = runtime_of(pid, versions)
        if main is None:
            return None
        if main[1][1:2] != ["__service-launcher"]:
            return exact_version(main[0])
        servers = set()
        for child in child_pids(pid):
            found = runtime_of(child, versions)
            if found and found[1][1:2] == ["serve"]:
                servers.add(found[0])
        return servers.pop() if len(servers) == 1 else None
    except (UpdateFailure, OSError):
        pass
    return None


def running_runtime_versions(versions):
    """Runtime versions that a process of this user executes or names right now; None when /proc is unreadable."""
    try:
        with os.scandir(PROC_ROOT) as entries:
            pids = [entry.name for entry in entries if entry.name.isdigit()]
    except OSError:
        return None
    # A name ends at a slash, NUL or space, so a bare "<versions>/<v>" argument counts too.
    pattern = re.compile(re.escape(str(versions) + "/") + r"([^/\x00\s]+)")
    kept = set()
    for pid in pids:
        try:
            executable = pathlib.Path(os.readlink(os.path.join(PROC_ROOT, pid, "exe")).removesuffix(" (deleted)"))
            version = runtime_under(executable, versions)
        except OSError:
            # A non-dumpable helper hides its exe even from this user, but its argv still names its files.
            version = None
        if version:
            kept.add(version)
        try:
            with pathlib.Path(PROC_ROOT, pid, "cmdline").open("rb") as stream:
                text = stream.read(65536).decode("utf-8", "replace")
        except OSError:
            continue
        kept.update(name for name in pattern.findall(text) if exact_version(name))
    return kept


def active_runtime_version(home):
    try:
        with (home / ".t3/runtime/service-state.json").open("rb") as stream:
            state = json.loads(stream.read(65537))
    except (OSError, ValueError):
        return None
    return exact_version(state.get("activeVersion")) if isinstance(state, dict) else None


def real_directory(path):
    try:
        return stat.S_ISDIR(os.lstat(path).st_mode)
    except OSError:
        return False


def prune_runtimes():
    """Delete superseded managed runtimes on Ubuntu and return the names deleted.

    Keeps the active version, the one ~/.local/bin/t3 resolves to, the newest
    older version as one rollback copy, and any version a running process of
    this user executes from or names in its command line. Symlinks are never
    followed or removed, at most PRUNE_LIMIT directories go per run, and every
    failure only deletes less.
    """
    home = pathlib.Path.home()
    versions = real_path(home / ".t3/runtime/versions")
    deleted = []
    try:
        active = active_runtime_version(home)
        if active is None:
            return deleted
        running = running_runtime_versions(versions)
        if running is None:
            return deleted
        keep = {active, runtime_under(real_path(home / ".local/bin/t3"), versions), *running}
        older = sorted((name for name in os.listdir(versions) if exact_version(name) and version_key(name) < version_key(active)
                        and real_directory(versions / name)), key=version_key)
        if older:
            keep.add(older[-1])
        for name in [name for name in older if name not in keep][:PRUNE_LIMIT]:
            try:
                shutil.rmtree(versions / name)
            except OSError:
                continue
            deleted.append(name)
    except OSError:
        pass
    return deleted


def cursor_agent_update(deadline):
    """Update the Cursor agent CLI behind T3's Cursor provider; returns a fixed record and never raises."""
    home = pathlib.Path.home()
    link = home / ".local/bin/cursor-agent"
    if not link.exists():
        return {"before": None, "after": None, "status": "absent"}
    if not real_path(link).is_relative_to(real_path(home / ".local/share/cursor-agent/versions")):
        return {"before": None, "after": None, "status": "unmanaged"}
    before = cli_probe(link)[0]
    try:
        command([link, "update"], timeout=budget(deadline, 180))
    except UpdateFailure:
        return {"before": before, "after": cli_probe(link)[0], "status": "failed"}
    after = cli_probe(link)[0]
    return {"before": before, "after": after, "status": "updated" if after and after != before else "current"}


def acp_unit_present():
    return (pathlib.Path.home() / ".config/systemd/user" / ACP_UNIT).exists()


def start_acp_updater():
    """Start Jared's own ACP/ZCode updater unit without waiting; it keeps its own lock, deferrals and log.

    Only the detached worker uses this, after a verified restart freed the adapters. Within a job the
    zcode and t3-acp-adapters rows run the unit and wait for it (app_update_zcode).
    """
    if not acp_unit_present():
        return "absent"
    try:
        command(["/usr/bin/systemctl", "--user", "start", "--no-block", ACP_UNIT], timeout=15)
    except UpdateFailure:
        return "failed"
    return "started"


def record_components(root, **values):
    """Private record of the last Ubuntu companion outcome; only COMPONENT_KEYS are kept."""
    path = root / "t3-components.json"
    try:
        previous = json.loads(path.read_bytes())
    except (OSError, ValueError):
        previous = {}
    if not isinstance(previous, dict):
        previous = {}
    record = {key: previous.get(key) for key in COMPONENT_KEYS}
    record.update(values)
    record["utc"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    write_private_json(path, record)
    return record


def ubuntu_companions(root, deadline, restarting, daemon_reload):
    """Ubuntu companions beside T3 (Cursor CLI, old runtimes); best effort.

    Nothing here may change the t3-code row. The zcode and t3-acp-adapters
    rows already ran the ACP updater earlier in this job ("rows"). A scheduled
    restart stops the running adapters, so the detached worker prunes and
    starts the ACP updater again after its verified restart; until then the
    record says "pending".
    """
    try:
        cursor = cursor_agent_update(deadline)
        if restarting:
            acp = "pending" if acp_unit_present() else "absent"
            pruned = []
        else:
            acp = "rows" if acp_unit_present() else "absent"
            pruned = prune_runtimes()
        record_components(root, cursorAgent=cursor, acpUpdater=acp, prunedRuntimes=pruned, daemonReload=daemon_reload)
    except Exception:
        pass  # A companion bug must never change the T3 row.


def finish_ubuntu_restart(root, daemon_reload):
    """After a verified restart, in the detached worker: prune, start the ACP updater, record both."""
    try:
        pruned = prune_runtimes()
        acp = start_acp_updater()
        record_components(root, acpUpdater=acp, prunedRuntimes=pruned, daemonReload=daemon_reload)
    except Exception:
        return  # The restart already succeeded; these are best effort.
    print("T3 companions: ACP updater " + acp + "; pruned " + (", ".join(pruned) or "none")
          + "; daemon-reload " + ("yes" if daemon_reload else "no"), flush=True)


def marker_version(marker):
    try:
        return exact_version(json.loads(marker.read_bytes()).get("version"))
    except (OSError, ValueError, AttributeError):
        return None


def update_t3(install, deadline, phase=None):
    phase = phase or (lambda name: None)
    before, attempted, after = install.version, False, install.version
    try:
        if not before or (install.runtime and not install.runtime_version):
            raise UpdateFailure("version_unknown")
        version = latest_release()
        desktop_needed = install.desktop and version_key(version) > version_key(before)
        runtime_needed = install.runtime and version_key(version) > version_key(install.runtime_version)
        root = state_root()
        pending = root / "t3-code-pending-restart.json"
        if not desktop_needed and not runtime_needed:
            if install.platform == "ubuntu" and pending.exists():
                # A failed probe leaves intent behind. If an external restart
                # already activated this exact version, no restart is needed.
                if running_runtime_version() == install.runtime_version:
                    pending.unlink()
                    ubuntu_companions(root, deadline, False, reload_unit())
                    return result("t3-code", "ubuntu", "current", before, before, install.manager)
                value = result("t3-code", "ubuntu", "updated", before, before, install.manager, "t3_restart_scheduled")
                ubuntu_companions(root, deadline, True, False)
                value["restartTargets"] = [schedule_restart(before)]
                return value
            if install.platform == "ubuntu":
                # A unit left stale by an earlier run is reloaded here, never restarted.
                ubuntu_companions(root, deadline, False, reload_unit())
            return result("t3-code", install.platform, "current", before, before, install.manager)
        restarted = forced = 0
        daemon_reload = False
        with private_temporary() as temporary:
            if runtime_needed:
                attempted = True
                update_runtime(install, version, deadline)
                if install.platform == "ubuntu":
                    daemon_reload = reload_unit()
                if not install.desktop:
                    after = version
            if desktop_needed:
                attempted = True
                restarted, forced = {"mac": update_mac, "windows": update_windows}[install.platform](install, version, temporary, deadline, phase)
                after = version
        code = "t3_restart_scheduled" if install.platform == "ubuntu" else "t3_updated"
        value = result("t3-code", install.platform, "updated", before, after, install.manager, code, attempted, restarted)
        value["forcedStops"] = forced
        if install.platform == "ubuntu":
            # Companions run before the restart is scheduled; the worker starts the ACP updater after it.
            ubuntu_companions(root, deadline, True, daemon_reload)
            value["restartTargets"] = [schedule_restart(after)]
        else:
            value["restartTargets"] = [{"kind": "desktop"}] if desktop_needed else []
        return value
    except UpdateFailure as error:
        # Rollback can itself fail (locked files, app refusing to close).
        # Report whatever bits are actually installed, not the old snapshot.
        if attempted and install.desktop:
            after = mac_bundle_version(install.path) if install.platform == "mac" else windows_bundle_version(install.path)
        status = "restart_failed" if (after and after != before) or error.code == "restart_failed" else "failed"
        return result("t3-code", install.platform, status, before, after, install.manager, error.code, attempted)
    except Exception:
        if attempted and install.desktop:
            after = mac_bundle_version(install.path) if install.platform == "mac" else windows_bundle_version(install.path)
        return result("t3-code", install.platform, "failed", before, after, install.manager, "update_failed", attempted)


def main():
    parser = argparse.ArgumentParser(description="Fixed detached T3 restart worker")
    parser.add_argument("--restart-service", type=int, required=True)
    parser.add_argument("--state-dir", type=pathlib.Path, default=state_root())
    parser.add_argument("--dashboard-job", action="store_true")
    args = parser.parse_args()
    if not sys.platform.startswith("linux") or args.restart_service <= 1 or not args.state_dir.is_absolute():
        parser.error("Only the fixed detached T3 restart worker is supported.")
    deferred_restart(args.restart_service, args.state_dir, args.dashboard_job)


if __name__ == "__main__":
    main()
