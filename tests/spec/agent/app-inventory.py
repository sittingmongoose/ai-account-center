"""Build closed app inventory fixtures and get the Python helper's exact payload.

Only reads under the test root are admitted. The helper's fixed process probes
use captured stdout/stderr instead of launching a binary or updating anything.
"""
import contextlib
import json
import os
import pathlib
import plistlib
import struct
import sys
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_updates as updates
import app_update_antigravity as acp
import app_update_common as common
import app_update_cursor as cursor
import app_update_desktop as desktop
import app_update_t3 as t3
import app_update_zcode as zcode

root = pathlib.Path(sys.argv[1]).resolve()
platform, case = sys.argv[2:4]
home = root / "home"
local = root / "local"
roaming = root / "roaming"
program_files = root / "program-files"
system = root / "system"
folders = [root / "path", home / ".local/bin", local / "Programs/muse", roaming / "npm"]
shell = root / "Windows/powershell.exe"
home.mkdir()
probes = {}
packages = {}
executables = set()
unexpected = []


def put(path, value=b"fixture", executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(value if isinstance(value, bytes) else value.encode())
    if executable:
        path.chmod(0o755)
        executables.add(os.path.normpath(path))


def put_json(path, value):
    put(path, json.dumps(value))


def bundle(path, app, name, version, binary=False):
    target = path / app / "Contents/Info.plist"
    value = {"CFBundleIdentifier": name, "CFBundleShortVersionString": version}
    probes[str(target)] = json.dumps(value)
    put(
        target,
        plistlib.dumps(value,
                       fmt=plistlib.FMT_BINARY if binary else plistlib.FMT_XML),
    )


def asar(path, name, version, broken=False):
    data = json.dumps({"name": name, "version": version}).encode()
    header = json.dumps({"files": {"package.json": {"size": len(data), "offset": "0"}}}).encode()
    put(path, struct.pack("<IIII", 4, len(header) + 8, 0, len(header)) + header + (b"" if broken else data))


def command_file(path, text):
    put(path, executable=True)
    probes[str(path)] = text


def adapter(folder, name, version):
    put_json(folder / name / "package.json", {"name": name, "version": version})


arch = "aarch64" if platform == "mac" else "x86_64"
release_id = "9" * 64
platform_key = {"ubuntu": "linux-x64", "mac": "darwin-arm64", "windows": "win32-x64"}[platform]
runtime = home / ".t3/tools/antigravity-acp" / platform_key
release_dir = runtime / "versions" / release_id
fixture_dir = pathlib.Path(__file__).resolve().parents[2] / "unit/app-updates/fixtures/update-all-2026-11-13"

if case != "empty":
    put_json(runtime / "active.json", {"releaseId": release_id})
    if platform == "windows":
        record = json.loads((fixture_dir / "antigravity-windows-1.5.8-install-complete.json").read_text())
        record["releaseId"] = release_id
        # Preserve the receipt and its shape, with a fixture-sized executable.
        record["executable"]["bytes"] = 7
        if case != "degraded":
            put(release_dir / "agy.exe")
    else:
        record = {"releaseId": release_id, "version": "agy_acp_server_1.3.2",
                  "executable": {"name": "agy_acp_server.par", "bytes": 7},
                  "harness": {"name": "localharness_external", "bytes": 7}}
        put(release_dir / "agy_acp_server.par", executable=True)
        if case != "degraded":
            put(release_dir / "localharness_external", executable=True)
    put_json(release_dir / ".install-complete.json", record)

    if platform == "windows":
        command_file(folders[0] / "agy.exe", "agy 0.50.0-alpha-1")
        command_file(folders[0] / "claude.exe", "0.50.0+build.1 (Claude Code)")
        put(folders[0] / "omp.cmd")
        put(folders[2] / "muse.cmd")
        put(folders[2] / ".muse-launcher.ps1")
        probes["muse"] = "muse channel 9.1.0 (native 0.23.9)"
        put(folders[3] / "codex.cmd")
        adapter(folders[3] / "node_modules", "@openai/codex", "0.124.0")
        adapter(folders[3] / "node_modules", "zcode-acp-server", "0.84.4")
        ad_hoc = root / "other/bin/node_modules"
        adapter(ad_hoc, "@brokkai/muse-acp", "0.21.1")
        put_json(home / ".t3/userdata/settings.json", {"providerInstances": {
            "muse": {"config": {"commandArgs": [str(ad_hoc / "@brokkai/muse-acp/bin/acp.cjs")]}}
        }})
        for name, identity, executable, version in (
            ("codex", "OpenAI.Codex", "ChatGPT.exe", "26.1113.36452.0"),
            ("claude", "Claude", "Claude.exe", "1.18233.1"),
        ):
            installed = program_files / "Ren\u00e9" / name
            if case != "degraded" or name == "claude":
                put(installed / "app" / executable)
            packages[identity] = {"name": identity, "root": str(installed), "version": version, "publisher": "fixture"}
        installed = local / "Programs/t3code"
        put(installed / "T3 Code (Nightly).exe")
        asar(installed / "resources/app.asar", "t3code", "0.1.10-nightly.20261113.4616", case == "degraded")
        installed = local / "Programs/ZCode"
        put(installed / "ZCode.exe")
        asar(installed / "resources/app.asar", "@zcode/desktop", "3.14.5")
        installed = local / "cursor-agent"
        put(installed / "cursor-agent.ps1")
        folder = installed / "versions/2026.11.13-abcdef"
        put(folder / "node.exe")
        if case == "degraded":
            probes["cursor"] = {"failure": "PSSecurityException"}
        else:
            put(folder / "index.js")
    else:
        for name, release in (("agy", "1.7.2"), ("muse", "0.50.0-alpha-1"), ("omp", "0.50.0+build.1"),
                              ("codex", "0.124.0"), ("claude", "2.3.51")):
            command_file(folders[1] / name, "{} {}".format(name, release))
        default = (system / "opt/homebrew" if platform == "mac" else home / ".local") / "lib/node_modules"
        adapter(default, "@brokkai/muse-acp", "0.21.0")
        ad_hoc = home / ".local/opt/node/lib/node_modules"
        adapter(ad_hoc, "zcode-acp-server", "0.84.2" if platform == "mac" else "0.84.4")
        adapter(default, "zcode-acp-server", "0.84.3")
        put_json(home / ".t3/userdata/settings.json", {"providerInstances": {
            "disabled": {"enabled": False, "config": {"commandPath": str(default / "zcode-acp-server/cli.js")}},
            "zcode": {"config": {"commandArgs": [str(ad_hoc / "zcode-acp-server/cli.js")]}},
            "other-zcode": {"config": {"commandArgs": [str(default / "zcode-acp-server/cli.js")]}},
        }})
        if platform == "mac":
            apps = system / "Applications"
            bundle(apps, "ChatGPT.app", "bad.identity" if case == "degraded" else "com.openai.codex", "1.24.305")
            bundle(home / "Applications", "Claude.app", "com.anthropic.claudefordesktop", "1.18233.1", binary=True)
            bundle(apps, "T3 Code (Nightly).app", "com.t3tools.t3code", "0.1.10-nightly.20261113.4616")
            bundle(apps, "ZCode.app", "dev.zcode.app", "3.14.5")
        else:
            put(system / "usr/lib/chatgpt/ChatGPT")
            put(system / "usr/lib/claude-desktop/claude-desktop")
            probes["chatgpt"] = {"timeout": True} if case == "degraded" else "26.1113.36452"
            probes["claude-desktop"] = "1.18233.1"
            command_file(home / ".t3/runtime/versions/0.1.9-nightly.20261112.900/t3", "t3 0.1.9-nightly.20261112.900")
            command_file(home / ".t3/runtime/versions/0.1.10-nightly.20261113.4616/t3", "t3 0.1.10-nightly.20261113.4616")
            app = home / ".local/opt/zcode/app"
            app.mkdir(parents=True)
            put(app.parent / ".installed-version", "3.14.5\n")

        # With symlinks available, cover real managed/stray and Cursor decisions.
        # The rest of the three-platform suite still runs on Windows without the
        # privilege to create a symlink.
        managed = home / ".codex/packages/standalone/releases/0.124.0/codex"
        put(managed, executable=True)
        try:
            launcher = folders[1] / "codex"
            launcher.unlink()
            launcher.symlink_to(managed)
        except OSError:
            put(launcher, executable=True)
        else:
            command_file(folders[0] / "codex", "codex-cli 0.113.0")
            target = home / ".local/share/cursor-agent/versions/2026.11.13-abcdef/cursor-agent"
            put(target, executable=True)
            (folders[1] / "cursor-agent").symlink_to(target)
            probes[str(folders[1] / "cursor-agent")] = "Cursor 1.4.2"
    if case == "degraded":
        # A mismatched npm manifest means an installed shim has an unreadable version.
        if platform == "windows":
            adapter(folders[3] / "node_modules", "@openai/codex", "bogus")
        else:
            probes[str(folders[1] / "muse")] = {"timeout": True}


ConcretePath = type(pathlib.Path())


def fixture_path_parts(parts):
    if parts and isinstance(parts[0], str) and parts[0].startswith("/") and not parts[0].startswith("//"):
        # The reference can reconstruct a path using a separate POSIX root
        # component. Test the complete path before redirecting system paths.
        if not ConcretePath(*parts).is_relative_to(root):
            return (system / parts[0].lstrip("/"), *parts[1:])
    return parts


class FixturePath(ConcretePath):
    def __new__(cls, *parts):
        if sys.version_info >= (3, 12):
            return object.__new__(cls)
        return super().__new__(cls, *fixture_path_parts(parts))

    def __init__(self, *parts):
        if sys.version_info >= (3, 12):
            super().__init__(*fixture_path_parts(parts))

    def stat(self, *args, **kwargs):
        info = super().stat(*args, **kwargs)
        if os.name == "nt" and platform != "windows" and os.path.normpath(self) in executables:
            info = os.stat_result((info.st_mode | 0o111, *tuple(info)[1:]))
        return info

    @classmethod
    def home(cls):
        return cls(home)


def which(name):
    names = [name + suffix for suffix in (".exe", ".cmd", ".ps1")] if platform == "windows" else [name]
    for folder in folders:
        for stem in names:
            path = folder / stem
            if path.is_file():
                return str(path)
    return None


def probe(key):
    value = probes.get(key, "")
    if isinstance(value, dict):
        error = common.UpdateFailure("timeout" if "timeout" in value else "update_failed")
        error.output = value.get("failure")
        raise error
    return value


def command(argv, **_ignored):
    program, *args = [str(arg) for arg in argv]
    if "-W" in args:
        if program != "/usr/bin/dpkg-query":
            unexpected.append(argv)
            raise AssertionError("unexpected dpkg query")
        return probe(args[-1])
    if program == "powershell.exe":
        assert args[-1] == "--version"
        return probe("muse" if ".muse-launcher.ps1" in args[-2] else "cursor")
    if args != ["--version"] or not pathlib.Path(program).is_relative_to(root):
        unexpected.append(argv)
        raise AssertionError("unexpected version probe")
    return probe(program)


def powershell(script, **_ignored):
    assert script.startswith("$ErrorActionPreference='Stop'; $p=Get-AppxPackage -Name ")
    return next((json.dumps(value) for name, value in packages.items() if "'" + name + "'" in script), "")


env = {
    "PATH": os.pathsep.join(map(str, folders)), "LOCALAPPDATA": str(local), "APPDATA": str(roaming),
    "ProgramFiles": str(program_files),
}
def checked_detect(app, host):
    try:
        return real_detect(app, host)
    except Exception as error:
        unexpected.append((app, type(error).__name__, str(error)))
        raise


real_detect = updates._detect_one

# Deny process and network use even if a new probe bypasses the fakes.
with contextlib.ExitStack() as stack:
    def patch(*args, **kwargs):
        return stack.enter_context(mock.patch.object(*args, **kwargs))

    stack.enter_context(mock.patch.dict(os.environ, env, clear=True))
    patch(pathlib, "Path", FixturePath)
    patch(updates, "_detect_one", checked_detect)
    patch(updates.shutil, "which", which)
    patch(acp.platform, "machine", lambda: arch)
    patch(zcode, "MAC_PATH", system / "Applications/ZCode.app")
    patch(common.subprocess, "Popen", side_effect=AssertionError("real process blocked"))
    patch(common.urllib.request, "urlopen", side_effect=AssertionError("network blocked"))
    for module in (common, updates, cursor, desktop, t3):
        patch(module, "command", command)
    patch(desktop, "powershell", powershell)
    payload = updates.inventory_payload(platform)
    assert not unexpected, unexpected

print(json.dumps({
    "root": str(root), "probes": probes, "packages": packages, "path": list(map(str, folders)), "arch": arch,
    "expected": json.dumps(payload, ensure_ascii=True, allow_nan=False, separators=(",", ":")),
}, ensure_ascii=True))
