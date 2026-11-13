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
import sys
import tempfile
import time
import urllib.request
import uuid

import app_update_common as _common

from app_update_common import (
    Install, UpdateFailure, asar_package, command, diagnostics, display_path, download, environment, powershell,
    private_temporary, ps_quote, resolve_npm, result, version_tuple, npm_version_key,
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
PAGE_ATTEMPTS = 3
# The asar header lists ZCode's whole node_modules (about 7 MB in 3.14.5).
ASAR_HEADER_LIMIT = 16 * 1024 * 1024
# The owner's Ubuntu oneshot unit (adapters and ZCode, with its own lock, deferrals and log). It is used only
# when it installs into the copies T3 runs; otherwise, and on the Mac and Windows, AAC installs each adapter
# itself, bound to the prefix its owner names (see discover_owners).
UNIT_SECONDS = 600
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
LOCKED = re.compile(r"\b(?:EPERM|EACCES|EBUSY)\b")
# Rows that change nothing and tell the user what to do.
ACTIONS = ("quit_first", "in_use")
# The one t3-acp-update run of this helper process (one Update apps job); both Ubuntu rows share it.
UBUNTU_RUN = {}


@dataclasses.dataclass
class AdaptersInstall(Install):
    # Each adapter's installed version (the lowest across its owners' copies); None when absent or unreadable.
    parts: dict = None
    # {adapter: [Owner, ...]}: every copy T3 runs and the installer that owns it.
    owners: dict = None
    # {adapter: [(node_modules, version)]}: every copy found, the owners' included.
    copies: dict = None
    # {adapter: [version of each owner's copy]} at detection.
    before: dict = None


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
    """The usual global npm folder for this OS: the last-resort owner when nothing names another."""
    if host == "windows":
        return pathlib.Path(os.environ.get("APPDATA", str(pathlib.Path.home() / "AppData/Roaming"))) / "npm/node_modules"
    if host == "mac":
        return pathlib.Path("/opt/homebrew/lib/node_modules")
    return pathlib.Path.home() / ".local/lib/node_modules"


# ---------------------------------------------------------------------------
# Which copy of each adapter T3 actually runs, and which installer owns it.
# A successful `npm install -g` can land in a prefix nothing runs (the Mac on
# 2026-11-13: Homebrew's node became a standalone Node whose default prefix is
# its own folder). So the owner is found from what consumes the package, the
# install is bound to that owner's prefix, and verification reads that copy.
# ---------------------------------------------------------------------------

BINS = {"muse-acp": ("muse-acp",), "zcode-acp-server": ("zcode-acp-server", "zcode-acp")}
SETTINGS_LIMIT = 4 * 1024 * 1024
COPY_LIMIT = 4


@dataclasses.dataclass
class Owner:
    """One global package folder: node_modules itself, the prefix an installer is bound to and its manager."""
    root: pathlib.Path
    prefix: pathlib.Path
    manager: str
    source: str = "default"
    node: pathlib.Path = None
    # pnpm only: the folder its global bin links go to (PNPM_HOME).
    bin_dir: pathlib.Path = None

    def describe(self, name, version):
        return {"package": name, "manager": self.manager, "prefix": display_path(self.prefix), "source": self.source,
                "version": version}


def _real(path):
    try:
        return pathlib.Path(path).resolve()
    except (OSError, RuntimeError):
        return pathlib.Path(path)


def _lexical(path):
    """The path as written, made absolute, without following links."""
    return pathlib.Path(os.path.normpath(os.path.abspath(str(path))))


def owner_of_modules(modules, source="default", node=None, host=None):
    """The installer that owns one node_modules folder, by its layout. On Linux and the Mac npm's
    global folder is <prefix>/lib/node_modules; any other plain folder has no installer AAC can bind."""
    modules = _real(modules)
    text = path_text(modules)
    if "/.pnpm/" in text + "/":
        # A path inside pnpm's store is never an install target (package_in maps it back first).
        return Owner(modules, modules.parent, "other", source, node)
    if "/pnpm/global/" in text or text.endswith("/.pnpm-global/node_modules"):
        global_dir = modules.parent.parent if modules.parent.name.isdigit() else modules.parent
        return Owner(modules, global_dir, "pnpm", source, node, global_dir.parent)
    if text.endswith("/install/global/node_modules") and "/.bun/" in text:
        return Owner(modules, modules.parent, "bun", source, node)
    if text.endswith("/yarn/global/node_modules"):
        return Owner(modules, modules.parent, "yarn", source, node)
    if modules.parent.name == "lib":
        return Owner(modules, modules.parent.parent, "npm", source, node)
    # Windows npm: <prefix>\node_modules, with the bin shims in <prefix> itself.
    return Owner(modules, modules.parent, "npm" if host in (None, "windows") else "other", source, node)


def _modules_of(path):
    parts = path.parts
    if ".pnpm" in parts:
        # pnpm's store: <global>/<n>/node_modules/.pnpm/<pkg>@<v>/node_modules/<pkg>. The install
        # belongs to the node_modules folder that holds .pnpm, never to the store entry.
        index = parts.index(".pnpm")
        tail = "/".join(parts[index + 1:])
        if index > 0 and parts[index - 1] == "node_modules":
            for name, package in PACKAGES.items():
                if "/node_modules/" + package in "/" + tail:
                    return pathlib.Path(*parts[:index]), name
        return None
    for index in range(len(parts) - 1, -1, -1):
        if parts[index] != "node_modules":
            continue
        rest = parts[index + 1:index + 3]
        for name, package in PACKAGES.items():
            if "/".join(rest[:len(package.split("/"))]) == package:
                return pathlib.Path(*parts[:index + 1]), name
    return None


def package_in(path):
    """(node_modules folder, adapter name) for a file inside one adapter package, else None.

    The path as written is read first: pnpm's global node_modules/<pkg> is a link into its
    store, and following it would name the store as the install. Links are followed only
    when the written path names no package (a bin link such as /opt/homebrew/bin/zcode-acp-server)."""
    return _modules_of(_lexical(path)) or _modules_of(_real(path))


def _shim_package(path):
    """A Windows npm shim (<prefix>\\zcode-acp-server.cmd) names a package in <prefix>\\node_modules."""
    path = pathlib.Path(path)
    stem = path.stem.casefold() if path.suffix.casefold() in (".cmd", ".ps1", ".exe", "") else None
    for name, bins in BINS.items():
        if stem in bins and (path.parent / "node_modules" / PACKAGES[name] / "package.json").is_file():
            return path.parent / "node_modules", name
    return None


def t3_consumers(host=None):
    """{adapter: [(node_modules, node, refusal), ...]}: copies T3's enabled instances run.

    Only config.commandPath and config.commandArgs are read: they are paths and flags. Environment values,
    which can be secrets, are never read. A missing or unreadable settings file names nothing."""
    path = pathlib.Path.home() / ".t3/userdata/settings.json"
    try:
        if path.stat().st_size > SETTINGS_LIMIT:
            return {}
        settings = json.loads(path.read_text(encoding="utf-8"))
        instances = settings.get("providerInstances")
    except (OSError, ValueError, AttributeError):
        return {}
    found = {}
    on_path = path_consumers(host)
    for instance in (instances.values() if isinstance(instances, dict) else []):
        if not isinstance(instance, dict) or instance.get("enabled") is False:
            continue
        config = instance.get("config") if isinstance(instance.get("config"), dict) else {}
        command_path = config.get("commandPath") if isinstance(config.get("commandPath"), str) else None
        args = [arg for arg in (config.get("commandArgs") or [])[:16] if isinstance(arg, str)]
        node = pathlib.Path(command_path) if command_path and pathlib.PurePath(command_path).stem.casefold() == "node" else None
        bare = pathlib.PurePath(command_path).stem.casefold() if command_path else None
        if command_path and "/" not in command_path and "\\" not in command_path:
            for name, bins in BINS.items():
                if bare in bins:
                    root = on_path.get(name)
                    found.setdefault(name, []).append((root or adapter_root(host), node,
                                                       None if root else "mixed_ownership"))
        for candidate in ([command_path] if command_path else []) + args:
            if len(candidate) > 1024 or not ("/" in candidate or "\\" in candidate):
                continue
            hit = package_in(candidate) or _shim_package(candidate)
            if hit:
                pinned_store = (".pnpm" in _lexical(candidate).parts or
                                (".pnpm" in _real(candidate).parts and _modules_of(_lexical(candidate)) is None))
                refusal = ("pnpm_store_copy" if pinned_store else
                           "mixed_ownership" if not pathlib.Path(candidate).exists() else None)
                found.setdefault(hit[1], []).append((hit[0], node, refusal))
    return found


def effective_path(host):
    """PATH as ordinary processes see it (app_update_common.effective_path)."""
    return _common.effective_path(host)


def path_consumers(host):
    """{adapter: node_modules} for the adapter commands found on PATH, first match wins."""
    found = {}
    for folder in effective_path(host):
        for name, bins in BINS.items():
            if name in found:
                continue
            for binary in bins:
                for suffix in ((".cmd", ".ps1", "") if host == "windows" else ("",)):
                    candidate = pathlib.Path(folder) / (binary + suffix)
                    if not candidate.is_file():
                        continue
                    hit = package_in(candidate) or _shim_package(candidate)
                    if hit:
                        found[name] = hit[0]
                        break
                if name in found:
                    break
    return found


def npmrc_prefix():
    """A prefix= line in the user's ~/.npmrc, or npm_config_prefix: where plain npm -g installs."""
    value = os.environ.get("npm_config_prefix") or os.environ.get("NPM_CONFIG_PREFIX")
    if value:
        return pathlib.Path(value).expanduser()
    with contextlib.suppress(OSError):
        for line in (pathlib.Path.home() / ".npmrc").read_text(encoding="utf-8").splitlines()[:200]:
            key, _, rest = line.partition("=")
            if key.strip() == "prefix" and rest.strip():
                return pathlib.Path(os.path.expandvars(rest.strip())).expanduser()
    return None


def candidate_roots(host):
    """Every global package folder an adapter copy could live in, most usual first."""
    home = pathlib.Path.home()
    roots = [adapter_root(host)]
    prefix = npmrc_prefix()
    if prefix is not None:
        roots.append(prefix / ("node_modules" if host == "windows" else "lib/node_modules"))
    located = shutil.which("node")
    if located:
        node_home = _real(located).parents[1] if host != "windows" else _real(located).parent
        roots.append(node_home / ("node_modules" if host == "windows" else "lib/node_modules"))
    if host == "windows":
        roots.append(pathlib.Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "nodejs/node_modules")
    else:
        roots += [home / ".local/lib/node_modules", home / ".npm-global/lib/node_modules",
                  pathlib.Path("/opt/homebrew/lib/node_modules"), pathlib.Path("/usr/local/lib/node_modules"),
                  pathlib.Path("/usr/lib/node_modules")]
    roots += [home / ".bun/install/global/node_modules", home / ".local/share/pnpm/global/5/node_modules",
              home / "Library/pnpm/global/5/node_modules", home / ".config/yarn/global/node_modules"]
    if host == "windows" and os.environ.get("LOCALAPPDATA"):
        roots.append(pathlib.Path(os.environ["LOCALAPPDATA"]) / "pnpm/global/5/node_modules")
    unique, seen = [], set()
    for root in roots:
        real = str(_real(root))
        if real not in seen:
            seen.add(real)
            unique.append(root)
    return unique


def package_version(root, name):
    try:
        with (pathlib.Path(root) / PACKAGES[name] / "package.json").open("rb") as stream:
            info = json.loads(stream.read(1024 * 1024))
        return part_version(info.get("version")) if isinstance(info, dict) and info.get("name") == PACKAGES[name] else None
    except (OSError, ValueError):
        return None


def copies_of(host, name):
    """[(node_modules, version)] of every copy of one adapter, deduplicated by real folder."""
    found = []
    for root in candidate_roots(host):
        if (root / PACKAGES[name] / "package.json").is_file():
            found.append((root, package_version(root, name)))
    return found


def discover_owners(host):
    """{adapter: [Owner, ...]} for each installed adapter, and {adapter: [(root, version)]} of every copy.

    Precedence: every distinct copy T3's enabled instances run (two instances may run two
    copies, and each is updated and verified), else the command on PATH, else the usual
    folder for this OS, else the first other copy found. Each owner records its source,
    so a row says why it updated (and verified) that copy."""
    consumers, on_path = t3_consumers(host), path_consumers(host)
    owners, copies = {}, {}
    for name in PACKAGES:
        copies[name] = copies_of(host, name)
        if name in consumers:
            owners[name] = []
            for root, node, refusal in consumers[name]:
                owner = owner_of_modules(root, refusal or "t3-config", node, host)
                if refusal or not any(_real(other.root) == _real(root) for other in owners[name]):
                    owners[name].append(owner)
        elif name in on_path:
            owners[name] = [owner_of_modules(on_path[name], "path", host=host)]
        elif (adapter_root(host) / PACKAGES[name] / "package.json").is_file():
            owners[name] = [owner_of_modules(adapter_root(host), "default", host=host)]
        elif copies[name]:
            owners[name] = [owner_of_modules(copies[name][0][0], "found", host=host)]
        for owner in owners.get(name, []):
            real = _real(owner.root)
            if not any(_real(root) == real for root, _ in copies[name]):
                copies[name].insert(0, (owner.root, package_version(owner.root, name)))
    return owners, copies


def present_parts(root):
    return [name for name, package in PACKAGES.items() if (root / package / "package.json").is_file()]


def read_parts(root):
    return {name: package_version(root, name) for name in PACKAGES}


def lowest(versions):
    """The lowest of several copies' versions; None when any copy's version is unreadable."""
    if not versions or any(version is None for version in versions):
        return None
    return min(versions, key=npm_version_key)


def owner_versions(install):
    """{adapter: [version of each owner's copy]}, in owner order."""
    return {name: [package_version(owner.root, name) for owner in owners] for name, owners in (install.owners or {}).items()}


def owned_parts(install):
    """Each adapter's version in the copies its owners name (the lowest, so one unchanged copy
    never reads as updated); None for an adapter without one."""
    versions = owner_versions(install)
    return {name: lowest(versions[name]) if name in versions else None for name in PACKAGES}


def elsewhere(install, name):
    """The other copies of one adapter as they are now (a run can create one), with their versions."""
    skip = {str(_real(owner.root)) for owner in (install.owners or {}).get(name, [])}
    found = []
    for root, _ in copies_of(install.platform, name) + (install.copies or {}).get(name, []):
        key = str(_real(root))
        if key not in skip:
            skip.add(key)
            found.append((root, package_version(root, name)))
    return found[:COPY_LIMIT]


def detect_adapters(host):
    owners, copies = discover_owners(host)
    if not owners:
        return None
    first = owners[next(name for name in PACKAGES if name in owners)][0]
    install = AdaptersInstall("t3-acp-adapters", host, first.root, None, "npm", package_root=first.root,
                              owners=owners, copies=copies)
    install.parts = owned_parts(install)
    install.before = owner_versions(install)
    install.owner = adapters_owner(install)
    return install


def adapters_owner(install, after=None):
    """diagnostics.owner for an adapters row: every owner of each package and any other copies."""
    now = owner_versions(install) if after is not None else (install.before or owner_versions(install))
    value = {"packages": [], "elsewhere": []}
    for name in PACKAGES:
        for index, owner in enumerate((install.owners or {}).get(name, [])):
            value["packages"].append(owner.describe(name, now.get(name, [None] * (index + 1))[index]))
        for root, version in elsewhere(install, name):
            value["elsewhere"].append({"package": name, "prefix": display_path(owner_of_modules(root).prefix), "version": version})
    value["packages"] = value["packages"][:4]
    value["elsewhere"] = value["elsewhere"][:COPY_LIMIT * 2]
    if not value["elsewhere"]:
        del value["elsewhere"]
    return value


def feed_key(host):
    """This computer's folder in the ZCode release tree, e.g. linux-x64."""
    if host == "windows":
        return "windows-arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "windows-x64"
    machine = platform.machine().lower()
    architecture = "arm64" if machine in ("arm64", "aarch64") else "x64" if machine in ("x86_64", "amd64") else None
    if architecture is None:
        raise UpdateFailure("unsupported")
    return ("macos-" if host == "mac" else "linux-") + architecture


def _read_metadata_file(url, prefix, limit, destination, timeout):
    """The read-only child operation; its parent bounds DNS, HTTP framing and body reads together."""
    request = urllib.request.Request(url, headers={"User-Agent": "CCS-Installed-App-Updater/1.0"})
    try:
        with urllib.request.urlopen(request, timeout=min(20, timeout)) as response:
            if response.status != 200 or not response.geturl().startswith(prefix):
                raise UpdateFailure()
            raw = response.read(limit + 1)
        if len(raw) > limit:
            raise UpdateFailure()
        descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(raw)
    except UpdateFailure:
        raise
    except TimeoutError:
        raise UpdateFailure("timeout") from None
    except Exception:
        raise UpdateFailure() from None

def _metadata_command(url, prefix, limit, destination, timeout):
    try:
        command([sys.executable, pathlib.Path(__file__).resolve(), "--read-metadata", url, prefix, str(limit), str(destination), str(timeout)], timeout=timeout)
    except UpdateFailure as error:
        if error.exit_code == 2:
            raise UpdateFailure("timeout") from None
        raise


def read_bounded(url, prefix, limit, deadline=None):
    """A fixed publisher lookup, with a hard deadline even during DNS and HTTP parsing."""
    deadline = min(deadline if deadline is not None else float("inf"), time.monotonic() + 20)
    with private_temporary() as temporary:
        destination = temporary / "metadata"
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise UpdateFailure("timeout")
        # Killing this read-only child closes sockets blocked in DNS, response
        # headers, chunk extensions or trailers. Socket inactivity timeouts
        # alone cannot bound a peer that keeps sending another byte.
        _metadata_command(url, prefix, limit, destination, remaining)
        if time.monotonic() >= deadline:
            raise UpdateFailure("timeout")
        if destination.stat().st_size > limit:
            raise UpdateFailure()
        return destination.read_bytes()


def latest_zcode(key, deadline=None):
    """The newest version the official page lists for this platform, compared numerically like t3-acp-update."""
    # A page/connection failure can fail Nas1 before its updater even runs.
    # Retry only this bounded official lookup; never call an old
    # installed version current when every lookup fails.
    deadline = min(deadline if deadline is not None else float("inf"), time.monotonic() + 60)
    for attempt in range(PAGE_ATTEMPTS):
        try:
            listed = re.findall(rb"releases/(\d{1,6}\.\d{1,6}\.\d{1,6})/" + re.escape(key.encode("ascii")) + rb"/latest\.yml",
                                read_bounded(PAGE, "https://zcode.z.ai/", PAGE_LIMIT, deadline=deadline))
            if listed:
                return max((item.decode("ascii") for item in listed), key=version_tuple)
            raise UpdateFailure()
        except UpdateFailure as error:
            if attempt + 1 == PAGE_ATTEMPTS:
                raise UpdateFailure("timeout" if error.code == "timeout" else "zcode_release_unavailable") from None
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise UpdateFailure("timeout") from None
            time.sleep(min(attempt + 1, remaining))


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
    (any letter case, native subfolders such as native/x86_64-pc-windows-msvc too), or a name or session marker.
    root is one node_modules folder, or {adapter: Owner} when each package has its own."""
    texts = [path_text(value) for value in (item.exe, *item.args[:16])]
    if isinstance(root, dict):
        roots = {name: [owner.root for owner in (value if isinstance(value, list) else [value])] for name, value in root.items()}
    else:
        roots = {name: [root] for name in PACKAGES}
    owners = {name for name, package in PACKAGES.items() if name in roots
              and any(text.startswith(path_text(folder / package) + "/") for folder in roots[name] for text in texts)}
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


def npm_argv(node, cli, prefix, name, shell, version="latest"):
    """One package's own global install: no other package rides along in the same npm run.
    version is the exact release the public registry named: a mirror that cannot serve it fails
    honestly instead of installing another (older) version under @latest."""
    argv = [str(node), str(cli), "install", "--global", "--prefix", str(prefix)]
    if shell is not None:
        argv += ["--script-shell", str(shell)]
    return argv + [PACKAGES[name] + "@" + version]


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
            # Stopping systemctl's wait does not stop the oneshot's installer.
            # Keep its shared npm resources reserved until a terminal state
            # is verified; an unreadable state remains busy.
            UBUNTU_RUN["unsettled"] = True
    return UBUNTU_RUN["code"]


def ubuntu_unit_busy():
    """An interrupted wait never grants another installer ownership of the prefix."""
    if not UBUNTU_RUN.get("unsettled"):
        return False
    try:
        state = command(["/usr/bin/systemctl", "--user", "show", ACP_UNIT, "--property=SubState", "--value"], timeout=15, capture=True).strip()
        if state in ("dead", "failed", "exited"):
            UBUNTU_RUN["unsettled"] = False
            return False
    except UpdateFailure:
        pass
    return True


def zcode_ubuntu(install, deadline, phase, state):
    before = install.version
    if version_tuple(latest_zcode(feed_key("ubuntu"), deadline=deadline)) <= version_tuple(before):
        return result("zcode", "ubuntu", "current", before, before, install.manager)
    if not acp_unit_present():
        raise UpdateFailure("unsupported")
    if t3_sessions(scan("ubuntu")):
        raise UpdateFailure("in_use")
    # The shared unit also touches both adapters. Do not let this row lower a sibling.
    modules = pathlib.Path.home() / ".local/lib/node_modules"
    siblings = {name: package_version(modules, name) for name in PACKAGES
                if (modules / PACKAGES[name] / "package.json").exists()}
    offers = {name: registry_version(name) for name in siblings}
    if any(not old or not offers[name] or npm_version_key(offers[name]) < npm_version_key(old)
           for name, old in siblings.items()):
        return result("zcode", "ubuntu", "skipped", before, before, install.manager, "shared_updater_unsafe")
    phase("updating")
    code = run_unit(deadline)
    state["attempted"] = True
    after = installed_version(install)
    for name, old in siblings.items():
        current = package_version(modules, name)
        if current is None:
            raise UpdateFailure("version_unknown")
        if npm_version_key(current) < npm_version_key(old):
            raise UpdateFailure("version_went_down")
    if after and version_tuple(after) < version_tuple(before):
        raise UpdateFailure("version_went_down")
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
    newest = latest_zcode(key, deadline=deadline)
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
    newest = latest_zcode(key, deadline=deadline)
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
        status = "restart_failed" if code != "version_went_down" and after and before and after != before else "failed"
        return result("zcode", install.platform, status, before, after, install.manager,
                      "update_failed" if code == "download_blocked" else code, state["attempted"])


def adapters_row(install, status, after=None, code=None, attempted=False, held=()):
    """Top-level versions stay null: each adapter reports its own in parts, read from the copy its owner
    names. A part in held was behind and skipped because it is in use: it keeps its version and carries inUse."""
    before = install.parts or {}
    after = before if after is None else after
    value = result("t3-acp-adapters", install.platform, status, None, None, "npm", code, attempted)
    value["parts"] = [{"name": name, "previousVersion": before.get(name), "version": after.get(name),
                       **({"inUse": True} if name in held else {})} for name in PACKAGES]
    if install.owners:
        diagnostics().owner = adapters_owner(install, after)
    return value


def npm_pair(owner, host):
    """(node, npm-cli.js) for an npm owner: T3's own node when its config names one, else the prefix's pair."""
    if owner.node is not None:
        node = _real(owner.node)
        home = node.parent if host == "windows" else node.parents[1]
        for cli in ((home / "node_modules/npm/bin/npm-cli.js",) if host == "windows" else
                    (home / "lib/node_modules/npm/bin/npm-cli.js", home / "libexec/lib/node_modules/npm/bin/npm-cli.js")):
            if node.is_file() and cli.is_file():
                return node, cli
    return resolve_npm(owner.prefix)


def pnpm_argv(host):
    """pnpm itself, never through cmd.exe: on Windows a pnpm.cmd shim is replaced by node and
    pnpm's own pnpm.cjs, as npm runs (a & or % in a path would otherwise be interpreted)."""
    located = shutil.which("pnpm")
    if not located:
        return None
    path = pathlib.Path(located)
    if host == "windows" and path.suffix.casefold() in (".cmd", ".bat", ".ps1"):
        node = shutil.which("node.exe") or shutil.which("node")
        for cli in (path.parent / "node_modules/pnpm/bin/pnpm.cjs", path.parent / "node_modules/pnpm/bin/pnpm.js"):
            if node and cli.is_file():
                return [node, str(cli)]
        return None
    return [str(path)]


def install_command(owner, name, host, shell, version):
    """(argv, extra env, label) for one package bound to its owner; None when that manager is not here."""
    package = PACKAGES[name] + "@" + version
    if owner.manager == "npm":
        pair = npm_pair(owner, host)
        if pair is None:
            return None
        return npm_argv(*pair, owner.prefix, name, shell, version), None, "npm install --global --prefix " + display_path(owner.prefix) + " " + package
    if owner.manager == "pnpm":
        pnpm = pnpm_argv(host)
        if pnpm is None:
            return None
        bin_dir = str(owner.bin_dir or owner.prefix)
        # pnpm refuses a global install without its bin folder (PNPM_HOME) on PATH.
        extra = {"PNPM_HOME": bin_dir, "PATH": bin_dir + os.pathsep + os.environ.get("PATH", "")}
        return ([*pnpm, "add", "--global", "--global-dir", str(owner.prefix), package], extra,
                "pnpm add --global --global-dir " + display_path(owner.prefix) + " " + package)
    if owner.manager == "bun":
        bun = shutil.which("bun") or (str(pathlib.Path.home() / ".bun/bin/bun") if (pathlib.Path.home() / ".bun/bin/bun").is_file() else None)
        if bun is None:
            return None
        return [bun, "add", "--global", package], {"BUN_INSTALL_GLOBAL_DIR": str(owner.prefix)}, "bun add --global " + package
    return None


def writable(owner, host):
    """Whether this account can write the owner's folders, by creating and removing a file there.
    os.access cannot see a Windows ACL: a non-elevated task reads C:\\Program Files as writable."""
    folders = [owner.root] + ([owner.prefix / "bin"] if host != "windows" and owner.manager == "npm" else [])
    for folder in folders:
        if not folder.exists():
            continue
        try:
            handle, name = tempfile.mkstemp(prefix=".aac-write-check-", dir=str(folder))
            os.close(handle)
            os.unlink(name)
        except OSError:
            return False
    return True


# A syscall name is not lock evidence: ACLs can deny rename/unlink even when the parent is writable.
DENIED = re.compile(r"\b(?:EPERM|EACCES)\b")


def install_owned(install, deadline, phase, state, plan, held, targets, busy=frozenset()):
    """Each behind copy installs in its own run, bound to the owner that holds it.

    plan is [(adapter, owner index)]. Returns {adapter: failure code}. A package npm reports
    EBUSY on, or EPERM while an adapter process uses it (Windows), joins held; EPERM or EACCES
    with nothing using it is an unwritable folder."""
    failed = {}
    shell = git_bash() if install.platform == "windows" else None
    for name, index in plan:
        if name in held or name in failed:
            continue
        owner = install.owners[name][index]
        if not writable(owner, install.platform):
            failed[name] = "prefix_not_writable"
            diagnostics().blame("not_writable", "prefix_not_writable")
            continue
        planned = install_command(owner, name, install.platform, shell, targets[name])
        if planned is None:
            failed[name] = "unsupported"
            diagnostics().note("manager_missing")
            continue
        argv, extra, label = planned
        phase("updating")
        state["attempted"] = True
        try:
            command(argv, timeout=budget(deadline, NPM_SECONDS), errors=True, label=label,
                    **({"env": environment(extra), "preserve_env": True} if extra else {}))
        except UpdateFailure as error:
            # npm's output is read here only, never shown in full.
            if install.platform == "windows" and locked_here(error.output, name):
                if name in busy or (not DENIED.search(error.output or "") and "EBUSY" in error.output):
                    held.add(name)
                else:
                    failed[name] = "prefix_not_writable"
                    diagnostics().blame("not_writable", "npm_denied")
            else:
                failed[name] = error.code
    return failed


def moved_elsewhere(install, name):
    """Whether a copy other than the owners' changed (or appeared) during this row."""
    owners = {str(_real(owner.root)) for owner in (install.owners or {}).get(name, [])}
    before = {str(_real(root)): version for root, version in (install.copies or {}).get(name, [])}
    for root, version in copies_of(install.platform, name):
        key = str(_real(root))
        if key not in owners and version and before.get(key) != version:
            return True
    return False


def adapters_settle(install, behind, targets, failed, held, attempted, unit_code=None, busy=None):
    """The row for what really changed in the owners' copies.

    A package is updated only when every copy T3 runs reached the registry's newest version.
    Precedence: a wrong prefix (npm changed a copy T3 does not run), a version other than
    the newest (an older or mirrored registry) or another real failure makes the row failed;
    an unwritable owner needs the user; a held part is in_use; else updated."""
    after = owned_parts(install)
    now = owner_versions(install)
    before = install.before or {}
    changed = set()
    held = set(held)
    # Check all snapshotted owners before success shortcuts, including siblings outside the plan.
    for name, originals in before.items():
        versions = now.get(name, [])
        if any(old is not None and new is not None and npm_version_key(new) < npm_version_key(old)
               for old, new in zip(originals, versions)):
            diagnostics().blame("installer_failed", "version_went_down")
            return adapters_row(install, "failed", after, "version_went_down", attempted, held)
        if len(versions) != len(originals) or any(old is not None and new is None
                                                 for old, new in zip(originals, versions)):
            failed[name] = "version_unknown"
    for name in behind:
        versions = now.get(name, [])
        reached = [version is not None and npm_version_key(version) >= npm_version_key(targets[name]) for version in versions]
        if versions and all(reached):
            changed.add(name)
            continue
        if any(reached):
            diagnostics().note("owners_partial")
        if name in held or name in failed:
            continue
        moved = [old != new for old, new in zip(before.get(name, []), versions)]
        if any(moved):
            failed[name] = "registry_mismatch"
            diagnostics().blame("installer_failed", "registry_other")
        elif moved_elsewhere(install, name):
            failed[name] = "wrong_prefix"
        elif busy is not None and busy():
            held.add(name)
        else:
            failed[name] = unit_code or "update_failed"  # it finished, but this package's installed version did not move
    held -= changed
    if "wrong_prefix" in failed.values():
        diagnostics().blame("wrong_prefix", "owner_unchanged")
        return adapters_row(install, "failed", after, "wrong_prefix", attempted, held)
    if "registry_mismatch" in failed.values():
        return adapters_row(install, "failed", after, "registry_mismatch", attempted, held)
    real = {name: code for name, code in failed.items() if code != "prefix_not_writable"}
    if real:
        return adapters_row(install, "failed", after, next(iter(real.values())), attempted, held)
    if failed:
        return adapters_row(install, "action_required", after, "prefix_not_writable", attempted, held)
    if held:
        return adapters_row(install, "action_required", after, "in_use", attempted, held)
    return adapters_row(install, "updated", after, attempted=attempted)


def unit_owns(install):
    """Whether the owner's t3-acp-update unit installs into the copies T3 runs (npm in ~/.local)."""
    local = _real(pathlib.Path.home() / ".local/lib/node_modules")
    return all(owner.manager == "npm" and _real(owner.root) == local
               for owners in (install.owners or {}).values() for owner in owners)


def adapters_ubuntu(install, deadline, phase, state, plan, targets):
    behind = list(dict.fromkeys(name for name, _ in plan))
    busy = lambda: bool(busy_packages(scan("ubuntu"), install.owners))
    unit_safe = (set(install.owners or {}) == set(PACKAGES) and
                 all(targets.get(name) and all(npm_version_key(targets[name]) >= npm_version_key(old)
                                              for old in versions)
                     for name, versions in (install.before or {}).items()))
    if acp_unit_present() and unit_owns(install) and unit_safe:
        # The zcode row may already have run t3-acp-update in this job; it never runs twice.
        if "code" not in UBUNTU_RUN and busy():
            raise UpdateFailure("in_use")
        desktop = detect_zcode("ubuntu")
        if desktop is not None and not desktop.version:
            return adapters_row(install, "skipped", code="shared_updater_unsafe")
        phase("updating")
        code = run_unit(deadline)
        state["attempted"] = True
        if desktop is not None:
            after_desktop = installed_version(desktop)
            if after_desktop is None:
                return adapters_row(install, "failed", owned_parts(install), "version_unknown", True)
            if version_tuple(after_desktop) < version_tuple(desktop.version):
                return adapters_row(install, "failed", owned_parts(install), "version_went_down", True)
        return adapters_settle(install, behind, targets, {}, set(), True, code, busy)
    # No unit, or T3 runs a copy the unit does not install: install there directly.
    diagnostics().note("direct_install")
    in_use = busy_packages(scan("ubuntu"), install.owners)
    held = set(behind) & in_use
    failed = install_owned(install, deadline, phase, state, plan, held, targets, in_use)
    return adapters_settle(install, behind, targets, failed, held, state["attempted"])


def adapters_mac(install, deadline, phase, state, plan, targets):
    """macOS replaces files under running adapters safely, so nothing is held. The install is bound to
    the prefix T3's copy lives in: the owner's own script ran plain `npm install -g`, which follows
    whichever Node is first and updated an unused prefix on 2026-11-13."""
    behind = list(dict.fromkeys(name for name, _ in plan))
    failed = install_owned(install, deadline, phase, state, plan, set(), targets)
    return adapters_settle(install, behind, targets, failed, set(), state["attempted"])


def adapters_windows(install, deadline, phase, state, plan, targets):
    """Each behind copy installs in its own npm run, so none can block or roll back another.

    A package a running adapter uses is held (in_use), and so is one npm reports EBUSY on;
    a package that is not behind is never installed. Lifecycle scripts run under Git for Windows'
    bash when it is installed (git_bash), since zcode-acp-server's postinstall is a POSIX command.
    """
    behind = list(dict.fromkeys(name for name, _ in plan))
    in_use = busy_packages(scan("windows"), install.owners)
    held = {name for name in behind if name in in_use}
    failed = install_owned(install, deadline, phase, state, plan, held, targets, in_use)
    return adapters_settle(install, behind, targets, failed, held, state["attempted"])


def update_adapters(install, deadline, phase=None):
    phase = phase or (lambda name: None)
    state, behind = {"attempted": False}, []
    try:
        for owners in (install.owners or {}).values():
            for owner in owners:
                if owner.source in ("mixed_ownership", "pnpm_store_copy"):
                    diagnostics().note(owner.source)
                    return adapters_row(install, "skipped", code=owner.source)
        present = [name for name in PACKAGES if name in (install.owners or {})]
        versions = install.before or owner_versions(install)
        if not present or not all(all(versions.get(name, [None])) for name in present):
            raise UpdateFailure("version_unknown", reason="adapter_version_unreadable")
        latest = {name: registry_version(name) for name in present}
        if None in latest.values():
            # Without the registry's answer nothing can be called current, so nothing runs.
            raise UpdateFailure("update_failed", reason="registry_unreadable")
        # Only a copy behind its latest is installed; one already current is never reinstalled.
        plan = [(name, index) for name in present for index, version in enumerate(versions[name])
                if npm_version_key(latest[name]) > npm_version_key(version)]
        behind = list(dict.fromkeys(name for name, _ in plan))
        if not plan:
            return adapters_row(install, "current")
        return {"ubuntu": adapters_ubuntu, "mac": adapters_mac, "windows": adapters_windows}[install.platform](install, deadline, phase, state, plan, latest)
    except Exception as error:
        code = error.code if isinstance(error, UpdateFailure) else "update_failed"
        if isinstance(error, UpdateFailure) and (error.cause or error.reason):
            diagnostics().blame(error.cause, error.reason)
        if code in ACTIONS:
            return adapters_row(install, "action_required", code=code, attempted=state["attempted"], held=behind if code == "in_use" else ())
        return adapters_row(install, "failed", owned_parts(install) if state["attempted"] else None, code, state["attempted"])


if __name__ == "__main__":
    # Only the parent's fixed read-only metadata operation is executable here.
    # This helper never starts an installer or touches account state in a child.
    try:
        if len(sys.argv) != 7 or sys.argv[1] != "--read-metadata":
            raise UpdateFailure()
        _read_metadata_file(sys.argv[2], sys.argv[3], int(sys.argv[4]), pathlib.Path(sys.argv[5]), float(sys.argv[6]))
    except UpdateFailure as error:
        raise SystemExit(2 if error.code == "timeout" else 1)
    except Exception:
        raise SystemExit(1)
