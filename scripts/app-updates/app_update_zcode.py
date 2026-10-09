"""ZCode desktop and T3's ACP adapters (muse-acp, zcode-acp-server).

ZCode comes only from its official release page and the CDN manifest of that
exact release; the adapters only from npm's latest tags. Nothing here stops a
T3 session: a row that would replace files a session uses reports
action_required / in_use and leaves them as they were; an adapter that is free
still updates on its own.
"""

import contextlib
import ctypes
import dataclasses
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import re
import shutil
import urllib.request
import uuid

from app_update_common import (
    Install, UpdateFailure, asar_package, command, download, powershell, private_temporary, ps_quote,
    resolve_npm, result, version_tuple,
)
from app_update_processes import (
    family, live_contexts, mac_arguments, main_contexts, restart_desktops, scan, terminate_desktops,
)
from app_update_t3 import (
    ACP_UNIT, acp_unit_present, budget, check_mac_archive, manifest_asset, nsis_silent, state_root, verify_mac,
)

PAGE = "https://zcode.z.ai/en"
RELEASES = "https://cdn-zcode.z.ai/zcode/electron/releases/"
VERSION = re.compile(r"^\d+\.\d+\.\d+$")
MAC_NAME = "ZCode.app"
MAC_PATH = pathlib.Path("/Applications") / MAC_NAME
MAC_ID = "dev.zcode.app"
MAC_TEAM = "8A5X4JJ39T"
# The signer's CN and O are Chinese; its registration number is the stable ASCII part.
WINDOWS_SIGNER = "SERIALNUMBER=91110108MA01KP2T5U"
MAX_PACKAGE = 800 * 1024 * 1024
PAGE_LIMIT = 4 * 1024 * 1024
# The asar header lists ZCode's whole node_modules (about 7 MB in 3.14.5).
ASAR_HEADER_LIMIT = 16 * 1024 * 1024
# The owner's own updaters: the Ubuntu oneshot unit (adapters and ZCode, with its
# own lock, deferrals and log) and the Mac script (Homebrew npm, both adapters).
UNIT_SECONDS = 600
MAC_SCRIPT = ".local/bin/t3-acp-adapters-update"
MAC_SCRIPT_SECONDS = 300
NPM_SECONDS = 300
PACKAGES = {"muse-acp": "@brokkai/muse-acp", "zcode-acp-server": "zcode-acp-server"}
REGISTRY = {
    "muse-acp": "https://registry.npmjs.org/@brokkai%2fmuse-acp/latest",
    "zcode-acp-server": "https://registry.npmjs.org/zcode-acp-server/latest",
}
PART_VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")
# A T3 ZCode session: the adapter, or ZCode's bundled CLI started by path.
SESSION_MARKERS = ("zcode-acp-server/dist/cli.js", "resources/glm/zcode.cjs")
# The adapter names t3-acp-update's own deferral looks for.
ADAPTER_NAMES = ("muse-acp", "muse-acp.cjs")
# npm's words for a file a running program holds: Windows refuses to replace or remove it.
LOCKED = re.compile(r"\b(?:EPERM|EBUSY)\b")
# Rows that change nothing and tell the user what to do.
ACTIONS = ("quit_first", "in_use")
# The one t3-acp-update run of this helper process (one Update apps job); both Ubuntu rows share it.
UBUNTU_RUN = {}


@dataclasses.dataclass
class AdaptersInstall(Install):
    # Each adapter's installed version; None when that package is absent or unreadable.
    parts: dict = None


def exact_version(value):
    return value if isinstance(value, str) and len(value) <= 32 and VERSION.fullmatch(value) else None


def part_version(value):
    return value if isinstance(value, str) and len(value) <= 64 and PART_VERSION.fullmatch(value) else None


def path_text(value):
    # One spelling for Windows and POSIX paths, so a marker matches either.
    return str(value).replace("\\", "/").casefold()


def asar_version(resources):
    package = asar_package(resources / "app.asar", ASAR_HEADER_LIMIT)
    return exact_version(package.get("version")) if package and package.get("name") == "@zcode/desktop" else None


def mac_version(bundle):
    try:
        with (pathlib.Path(bundle) / "Contents/Info.plist").open("rb") as stream:
            info = plistlib.load(stream)
        return exact_version(info.get("CFBundleShortVersionString")) if info.get("CFBundleIdentifier") == MAC_ID else None
    except (OSError, ValueError, plistlib.InvalidFileException):
        return None


def installed_version(install):
    """The version of the installed bits, read without running anything."""
    if install.platform == "mac":
        return mac_version(install.path)
    if install.platform == "windows":
        return asar_version(install.path.parent / "resources")
    version = asar_version(install.path / "resources")
    if version:
        return version
    # t3-acp-update's stamp beside the extracted AppImage (Nas1 has one).
    try:
        with (install.path.parent / ".installed-version").open("rb") as stream:
            return exact_version(stream.read(64).decode("ascii").strip())
    except (OSError, ValueError):
        return None


def detect_zcode(host):
    if host == "mac":
        path = MAC_PATH
        install = Install("zcode", host, path, None, "official-download", MAC_ID, MAC_TEAM, path)
    elif host == "windows":
        local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
        path = local / "Programs/ZCode/ZCode.exe"
        install = Install("zcode", host, path, None, "official-download", package_root=path.parent)
    else:
        path = pathlib.Path.home() / ".local/opt/zcode/app"
        install = Install("zcode", host, path, None, "official-download", package_root=path)
    if not (path.is_file() if host == "windows" else path.is_dir()):
        return None
    install.version = installed_version(install)
    return install


def adapter_root(host):
    if host == "windows":
        return pathlib.Path(os.environ.get("APPDATA", str(pathlib.Path.home() / "AppData/Roaming"))) / "npm/node_modules"
    if host == "mac":
        return pathlib.Path("/opt/homebrew/lib/node_modules")
    return pathlib.Path.home() / ".local/lib/node_modules"


def present_parts(root):
    return [name for name, package in PACKAGES.items() if (root / package / "package.json").is_file()]


def read_parts(root):
    parts = {}
    for name, package in PACKAGES.items():
        try:
            with (root / package / "package.json").open("rb") as stream:
                info = json.loads(stream.read(1024 * 1024))
            parts[name] = part_version(info.get("version")) if isinstance(info, dict) and info.get("name") == package else None
        except (OSError, ValueError):
            parts[name] = None
    return parts


def detect_adapters(host):
    root = adapter_root(host)
    if not present_parts(root):
        return None
    return AdaptersInstall("t3-acp-adapters", host, root, None, "npm", package_root=root, parts=read_parts(root))


def feed_key(host):
    """This computer's folder in the ZCode release tree, e.g. linux-x64."""
    if host == "windows":
        return "windows-arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "windows-x64"
    machine = platform.machine().lower()
    architecture = "arm64" if machine in ("arm64", "aarch64") else "x64" if machine in ("x86_64", "amd64") else None
    if architecture is None:
        raise UpdateFailure("unsupported")
    return ("macos-" if host == "mac" else "linux-") + architecture


def read_bounded(url, prefix, limit):
    """One fixed official URL, read within a size limit; anything unexpected raises UpdateFailure."""
    request = urllib.request.Request(url, headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            if not response.geturl().startswith(prefix):
                raise UpdateFailure()
            raw = response.read(limit + 1)
    except UpdateFailure:
        raise
    except Exception:
        raise UpdateFailure() from None
    if len(raw) > limit:
        raise UpdateFailure()
    return raw


def latest_zcode(key):
    """The newest version the official page lists for this platform, compared numerically like t3-acp-update."""
    listed = re.findall(rb"releases/(\d{1,6}\.\d{1,6}\.\d{1,6})/" + re.escape(key.encode("ascii")) + rb"/latest\.yml",
                        read_bounded(PAGE, "https://zcode.z.ai/", PAGE_LIMIT))
    if not listed:
        raise UpdateFailure()
    return max((item.decode("ascii") for item in listed), key=version_tuple)


def registry_version(name):
    """npm's latest version of one adapter, or None when the registry does not answer clearly. Read-only."""
    try:
        value = json.loads(read_bounded(REGISTRY[name], "https://registry.npmjs.org/", 1024 * 1024))
    except (UpdateFailure, ValueError):
        return None
    return part_version(value.get("version")) if isinstance(value, dict) and value.get("name") == PACKAGES[name] else None


def verified_package(key, version, temporary, deadline):
    """The exact release asset, checked against the size and sha512 in that release's own latest.yml."""
    system, architecture = key.split("-", 1)
    asset = "ZCode-" + version + ("-mac-" + architecture + ".zip" if system == "macos" else "-win-" + architecture + ".exe")
    base = RELEASES + version + "/" + key + "/"
    metadata, package = temporary / "latest.yml", temporary / asset
    download(base + "latest.yml", metadata, maximum=65536, timeout=30)
    digest, size = manifest_asset(metadata.read_text(encoding="utf-8"), version, asset)
    download(base + asset, package, maximum=min(size or MAX_PACKAGE, MAX_PACKAGE), timeout=budget(deadline, 600))
    sha = hashlib.sha512()
    with package.open("rb") as stream:
        for chunk in iter(lambda: stream.read(256 * 1024), b""):
            sha.update(chunk)
    if sha.digest() != digest or (size is not None and package.stat().st_size != size):
        raise UpdateFailure("signature_failed")
    return package


def with_arguments(host, processes):
    """The Mac process table carries no argv: read it for node and ZCode processes only."""
    if host == "mac":
        for item in processes:
            if not item.args and (pathlib.PurePath(item.exe).name in ("node", "zcode-cli") or "/zcode.app/" in path_text(item.exe)):
                try:
                    item.args = mac_arguments(item.pid)
                except (OSError, ValueError):
                    item.args = []
    return processes


def t3_sessions(processes):
    """T3's ZCode sessions: the zcode-acp-server adapter, its zcode-cli child and ZCode started as a CLI."""
    found = []
    for item in processes:
        args = [path_text(arg) for arg in item.args[:16]]
        # zcode-cli rewrites its argv: only that name shows, no path names the app.
        if args and (pathlib.PurePosixPath(args[0].split(" ")[0]).name in ("zcode-cli", "zcode-cli.exe") or
                     any(arg.endswith(marker) for arg in args for marker in SESSION_MARKERS)):
            found.append(item)
    return found


def adapter_owners(item, root):
    """The adapter packages one process belongs to: its executable or early arguments inside a package folder
    (any letter case, native subfolders such as native/x86_64-pc-windows-msvc too), or a name or session marker."""
    texts = [path_text(value) for value in (item.exe, *item.args[:16])]
    owners = {name for name, package in PACKAGES.items()
              if any(text.startswith(path_text(root / package) + "/") for text in texts)}
    if any(pathlib.PurePosixPath(text).name in ADAPTER_NAMES for text in texts[1:]):
        owners.add("muse-acp")
    if any(text.endswith(SESSION_MARKERS[0]) for text in texts[1:]):
        owners.add("zcode-acp-server")
    return owners


def busy_packages(processes, root):
    """The adapter packages a running process uses; npm cannot replace their files while it does."""
    return {name for item in processes for name in adapter_owners(item, root)}


def locked_here(output, name):
    """Whether npm failed on files this package holds open: npm's error lines, not its warnings, carry EPERM
    or EBUSY and name the package folder. A rollback that could not remove a folder is only a warning."""
    text = output.replace("\\\\", "\\").replace("\\", "/")
    errors = "\n".join(line for line in text.splitlines() if line.startswith("npm error"))
    return (LOCKED.search(errors) is not None and
            re.search(re.escape("node_modules/" + PACKAGES[name]) + r"(?![\w.@-])", errors, re.IGNORECASE) is not None)


def npm_argv(node, cli, prefix, name, shell):
    """One package's own global install: no other package rides along in the same npm run."""
    argv = [str(node), str(cli), "install", "--global", "--prefix", str(prefix)]
    if shell is not None:
        argv += ["--script-shell", str(shell)]
    return argv + [PACKAGES[name] + "@latest"]


def git_bash():
    """Git for Windows' bash.exe, the POSIX shell the adapters' postinstall scripts are written for; None when absent.

    Only Git's own bin folder counts, never the WSL launcher in System32. cmd.exe, npm's default
    script shell, cannot run `node x.js 2>/dev/null || true`: it cannot open /dev/null.
    """
    candidates = [pathlib.Path(os.environ[name]) / "Git/bin/bash.exe"
                  for name in ("ProgramW6432", "ProgramFiles", "ProgramFiles(x86)") if os.environ.get(name)]
    if os.environ.get("LOCALAPPDATA"):
        candidates.append(pathlib.Path(os.environ["LOCALAPPDATA"]) / "Programs/Git/bin/bash.exe")
    located = shutil.which("git")
    if located and len(pathlib.Path(located).parents) > 1:
        candidates.append(pathlib.Path(located).parents[1] / "bin/bash.exe")
    for candidate in candidates:
        if (candidate.is_file() and candidate.parent.name.casefold() == "bin"
                and candidate.parents[1].name.casefold() == "git"):
            return candidate
    return None


def runs_from(processes, folder):
    """Processes executing from folder or naming it, which t3-acp-update's swap waits for."""
    prefixes = {path_text(folder) + "/", path_text(os.path.realpath(folder)) + "/"}
    return [item for item in processes
            if any(prefix in text for prefix in prefixes for text in [path_text(item.exe), *map(path_text, item.args[:16])])]


def gui_processes(install, processes, sessions):
    """ZCode's own GUI: its bundle's processes that are neither a CLI session nor a session's child."""
    excluded = {item.pid for item in sessions}
    grown = True
    while grown:
        grown = False
        for item in processes:
            if item.pid not in excluded and item.ppid in excluded:
                excluded.add(item.pid)
                grown = True
    return [item for item in family(install, processes) if item.pid not in excluded]


def mac_blocker(install):
    """quit_first while ZCode's GUI runs (the Mac never quits it), in_use while T3 runs a ZCode session."""
    processes = with_arguments("mac", scan("mac"))
    sessions = t3_sessions(processes)
    if gui_processes(install, processes, sessions):
        raise UpdateFailure("quit_first")
    if sessions:
        raise UpdateFailure("in_use")


def verify_bundle(bundle, version):
    verify_mac(bundle, version, MAC_ID, MAC_TEAM, mac_version)


def windows_instances(install):
    """ZCode's running main instances, each reopened by path later; quit_first when one cannot be (another session)."""
    try:
        contexts = main_contexts(install, scan("windows"))
        if contexts:
            session = ctypes.c_ulong()
            if not ctypes.windll.kernel32.ProcessIdToSessionId(os.getpid(), ctypes.byref(session)):
                raise UpdateFailure("restart_context")
            if any(item.session != session.value for item in contexts):
                raise UpdateFailure("quit_first")
        return contexts
    except UpdateFailure as error:
        raise UpdateFailure("quit_first" if error.code == "restart_context" else error.code) from None


def verify_signer(package, installed):
    """Valid Authenticode on both files and the installer signed by the installed ZCode's signer; prints nothing."""
    try:
        powershell("$ErrorActionPreference='Stop'; $n=Get-AuthenticodeSignature -LiteralPath " + ps_quote(package) +
                   "; $o=Get-AuthenticodeSignature -LiteralPath " + ps_quote(installed) + "; "
                   "if($n.Status -ne 'Valid' -or $o.Status -ne 'Valid' -or -not $n.SignerCertificate -or -not $o.SignerCertificate -or "
                   "$n.SignerCertificate.Subject -cne $o.SignerCertificate.Subject -or "
                   "-not $o.SignerCertificate.Subject.Contains(" + ps_quote(WINDOWS_SIGNER) + ")){exit 1}", timeout=60)
    except UpdateFailure:
        raise UpdateFailure("signature_failed") from None


def run_unit(deadline):
    """Start t3-acp-update and wait for its run, once per job; returns its failure code or None.

    A oneshot's blocking start returns when the run ends, joining one the timer
    already started. At the timeout only this wait stops: the unit finishes on
    its own, under its own lock and TimeoutStartSec.
    """
    if "code" not in UBUNTU_RUN:
        seconds = budget(deadline, UNIT_SECONDS)
        try:
            command(["/usr/bin/systemctl", "--user", "start", ACP_UNIT], timeout=seconds)
            UBUNTU_RUN["code"] = None
        except UpdateFailure as error:
            UBUNTU_RUN["code"] = error.code
    return UBUNTU_RUN["code"]


def zcode_ubuntu(install, deadline, phase, state):
    before = install.version
    if version_tuple(latest_zcode(feed_key("ubuntu"))) <= version_tuple(before):
        return result("zcode", "ubuntu", "current", before, before, install.manager)
    if not acp_unit_present():
        raise UpdateFailure("unsupported")
    if t3_sessions(scan("ubuntu")):
        raise UpdateFailure("in_use")
    phase("updating")
    code = run_unit(deadline)
    state["attempted"] = True
    after = installed_version(install)
    if after and version_tuple(after) > version_tuple(before):
        return result("zcode", "ubuntu", "updated", before, after, install.manager, attempted=True)
    if not after:
        raise UpdateFailure("version_unknown")
    # t3-acp-update holds its swap while anything runs from the app folder (the GUI or a session).
    if runs_from(scan("ubuntu"), install.path):
        raise UpdateFailure("in_use")
    raise UpdateFailure(code or "update_failed")


def zcode_mac(install, deadline, phase, state):
    before = install.version
    key = feed_key("mac")
    newest = latest_zcode(key)
    if version_tuple(newest) <= version_tuple(before):
        return result("zcode", "mac", "current", before, before, install.manager)
    # Answered before any download: the Mac never quits ZCode and a T3 session is never stopped.
    mac_blocker(install)
    if not os.access(install.path.parent, os.W_OK):
        raise UpdateFailure("unsupported")
    with private_temporary() as temporary:
        phase("downloading")
        package = verified_package(key, newest, temporary, deadline)
        phase("updating")
        check_mac_archive(package)
        extracted = temporary / "extracted"
        command(["/usr/bin/ditto", "-x", "-k", package, extracted], timeout=budget(deadline, 120))
        verify_bundle(extracted / MAC_NAME, newest)
        return swap_mac(install, extracted / MAC_NAME, newest, deadline, state)


def swap_mac(install, staged, version, deadline, state):
    """T3's staged adjacent copy, atomic rename and rollback; ZCode was not running and is not opened."""
    backup = install.path.with_name(".aac-zcode-rollback-" + uuid.uuid4().hex + ".app")
    adjacent = install.path.with_name(".aac-zcode-stage-" + uuid.uuid4().hex + ".app")
    moved = completed = False
    try:
        command(["/usr/bin/ditto", staged, adjacent], timeout=budget(deadline, 120))
        verify_bundle(adjacent, version)
        # It may have been opened during the download: never swap under it.
        mac_blocker(install)
        state["attempted"] = True
        os.rename(install.path, backup)
        moved = True
        os.rename(adjacent, install.path)
        verify_bundle(install.path, version)
        completed = True
    except Exception:
        if moved:
            shutil.rmtree(install.path, ignore_errors=True)
            os.rename(backup, install.path)
        raise
    finally:
        shutil.rmtree(adjacent, ignore_errors=True)
        if completed:
            shutil.rmtree(backup, ignore_errors=True)
    value = result("zcode", "mac", "updated", install.version, version, install.manager, attempted=True)
    value["forcedStops"] = 0
    return value


def zcode_windows(install, deadline, phase, state):
    before = install.version
    key = feed_key("windows")
    newest = latest_zcode(key)
    if version_tuple(newest) <= version_tuple(before):
        return result("zcode", "windows", "current", before, before, install.manager)
    if t3_sessions(scan("windows")):
        raise UpdateFailure("in_use")
    # Read-only: an instance that could not be reopened answers before any download.
    windows_instances(install)
    with private_temporary() as temporary:
        phase("downloading")
        package = verified_package(key, newest, temporary, deadline)
        verify_signer(package, install.path)
        phase("updating")
        # Captured again right before closing: ZCode or a session may have started meanwhile.
        if t3_sessions(scan("windows")):
            raise UpdateFailure("in_use")
        contexts = windows_instances(install)
        return replace_windows(install, package, newest, contexts, temporary, deadline, state)


def replace_windows(install, package, version, contexts, temporary, deadline, state):
    """Close, install and reopen like Windows Codex and Claude, with T3's private rollback copy.

    Closing is terminate_desktops: WM_CLOSE, a bounded 15 s wait, then a stop of
    only identity-checked family survivors (forcedStops). A failed install
    restores the copy, and whatever closed reopens from it by path.
    """
    backup = temporary / "rollback"
    shutil.copytree(install.package_root, backup)
    seconds = budget(deadline, 180)
    state["attempted"] = True
    forced, started = 0, False
    try:
        if contexts:
            forced = terminate_desktops(install, contexts)
        started = True
        nsis_silent(package, seconds)
        if installed_version(install) != version:
            raise UpdateFailure("version_unknown")
    except Exception:
        if started:
            restore_windows(install, backup)
        with contextlib.suppress(Exception):
            live = live_contexts("windows", contexts)
            restart_desktops(install, [item for item in contexts if not any(item is other for other in live)])
        raise
    try:
        restarted = restart_desktops(install, contexts) if contexts else 0
    except Exception:
        value = result("zcode", "windows", "restart_failed", install.version, version, install.manager, "restart_failed", True)
        value["forcedStops"] = forced
        return value
    value = result("zcode", "windows", "updated", install.version, version, install.manager,
                   "desktop_reopened" if contexts else None, True, restarted)
    value["forcedStops"] = forced
    if contexts:
        value["restartTargets"] = [{"kind": "desktop"}]
    return value


def restore_windows(install, backup):
    """Put the previous ZCode back; a copy that cannot be restored stays private for recovery."""
    try:
        terminate_desktops(install, main_contexts(install, scan("windows")))
        shutil.rmtree(install.package_root)
        shutil.copytree(backup, install.package_root)
    except Exception:
        recovery = state_root() / ("zcode-rollback-" + uuid.uuid4().hex)
        recovery.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        with contextlib.suppress(OSError):
            shutil.move(str(backup), recovery)


def update_zcode(install, deadline, phase=None):
    phase = phase or (lambda name: None)
    before, state = install.version, {"attempted": False}
    try:
        if not before:
            raise UpdateFailure("version_unknown")
        return {"ubuntu": zcode_ubuntu, "mac": zcode_mac, "windows": zcode_windows}[install.platform](install, deadline, phase, state)
    except Exception as error:
        code = error.code if isinstance(error, UpdateFailure) else "update_failed"
        if code in ACTIONS:
            return result("zcode", install.platform, "action_required", before, before, install.manager, code, state["attempted"])
        # Report the bits actually installed after any rollback, never the old snapshot.
        after = installed_version(install) if state["attempted"] else before
        status = "restart_failed" if after and before and after != before else "failed"
        return result("zcode", install.platform, status, before, after, install.manager,
                      "update_failed" if code == "download_blocked" else code, state["attempted"])


def adapters_row(install, status, after=None, code=None, attempted=False, held=()):
    """Top-level versions stay null: each adapter reports its own in parts. A part in held was behind and
    skipped because it is in use: it keeps its installed version and carries inUse."""
    before = install.parts or {}
    after = before if after is None else after
    value = result("t3-acp-adapters", install.platform, status, None, None, "npm", code, attempted)
    value["parts"] = [{"name": name, "previousVersion": before.get(name), "version": after.get(name),
                       **({"inUse": True} if name in held else {})} for name in PACKAGES]
    return value


def adapters_outcome(install, code, busy=None, behind=()):
    """A behind adapter left unchanged while an adapter still runs is in_use; else updated when any changed; else failed."""
    after = read_parts(install.path)
    changed = [name for name in PACKAGES if after[name] and after[name] != install.parts.get(name)]
    unchanged = [name for name in behind if name not in changed]
    if unchanged and busy is not None and busy():
        return adapters_row(install, "action_required", after, "in_use", True, unchanged)
    if changed:
        return adapters_row(install, "updated", after, attempted=True)
    return adapters_row(install, "failed", after, code or "update_failed", True)


def adapters_ubuntu(install, deadline, phase, state, behind):
    if not acp_unit_present():
        raise UpdateFailure("unsupported")
    busy = lambda: bool(busy_packages(scan("ubuntu"), install.path))
    # The zcode row may already have run t3-acp-update in this job; it never runs twice.
    if "code" not in UBUNTU_RUN and busy():
        raise UpdateFailure("in_use")
    phase("updating")
    code = run_unit(deadline)
    state["attempted"] = True
    return adapters_outcome(install, code, busy, behind)


def adapters_mac(install, deadline, phase, state, behind):
    script = pathlib.Path.home() / MAC_SCRIPT
    if not script.is_file():
        raise UpdateFailure("unsupported")
    seconds = budget(deadline, MAC_SCRIPT_SECONDS)
    phase("updating")
    state["attempted"] = True
    try:
        command([script], timeout=seconds)
        code = None
    except UpdateFailure as error:
        code = error.code
    # The script does not defer: macOS replaces files under running adapters safely.
    return adapters_outcome(install, code, behind=behind)


def adapters_windows(install, deadline, phase, state, behind):
    """Each behind package installs in its own npm run, so none can block or roll back another.

    A package a running adapter uses is held (in_use), and so is one npm reports EPERM or EBUSY on;
    a package that is not behind is never installed. Lifecycle scripts run under Git for Windows'
    bash when it is installed (git_bash), since zcode-acp-server's postinstall is a POSIX command.
    """
    prefix = install.path.parent
    busy = busy_packages(scan("windows"), install.path)
    ready = [name for name in behind if name not in busy]
    resolved = resolve_npm(prefix) if ready else None
    if ready and resolved is None:
        raise UpdateFailure("unsupported")
    shell = git_bash() if ready else None
    failed, held = {}, {name for name in behind if name in busy}
    for name in ready:
        phase("updating")
        state["attempted"] = True
        try:
            command(npm_argv(*resolved, prefix, name, shell), timeout=budget(deadline, NPM_SECONDS), errors=True)
        except UpdateFailure as error:
            # npm's output is read here only, never shown: a lock on this package's files is in_use, not failed.
            if locked_here(error.output, name):
                held.add(name)
            else:
                failed[name] = error.code
    after = read_parts(install.path)
    changed = {name for name in behind if after.get(name) and after[name] != (install.parts or {}).get(name)}
    held -= changed
    for name in behind:
        if name not in changed and name not in held and name not in failed:
            failed[name] = "update_failed"  # npm finished, but this package's installed version did not move
    if failed:
        return adapters_row(install, "failed", after, next(iter(failed.values())), state["attempted"], held)
    if held:
        return adapters_row(install, "action_required", after, "in_use", state["attempted"], held)
    return adapters_row(install, "updated", after, attempted=state["attempted"])


def update_adapters(install, deadline, phase=None):
    phase = phase or (lambda name: None)
    state, behind = {"attempted": False}, []
    try:
        present = present_parts(install.path)
        if not present or not all((install.parts or {}).get(name) for name in present):
            raise UpdateFailure("version_unknown")
        latest = {name: registry_version(name) for name in present}
        if None in latest.values():
            # Without the registry's answer nothing can be called current, so nothing runs.
            raise UpdateFailure("update_failed")
        # Only a package behind its latest is installed; one already current is never reinstalled.
        behind = [name for name in present if version_tuple(latest[name]) > version_tuple(install.parts[name])]
        if not behind:
            return adapters_row(install, "current")
        return {"ubuntu": adapters_ubuntu, "mac": adapters_mac, "windows": adapters_windows}[install.platform](install, deadline, phase, state, behind)
    except Exception as error:
        code = error.code if isinstance(error, UpdateFailure) else "update_failed"
        if code in ACTIONS:
            return adapters_row(install, "action_required", code=code, attempted=state["attempted"], held=behind if code == "in_use" else ())
        return adapters_row(install, "failed", read_parts(install.path) if state["attempted"] else None, code, state["attempted"])
