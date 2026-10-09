"""Offline ZCode and T3 ACP adapter fixtures; nothing real is downloaded, installed, stopped or started."""

import base64
import contextlib
import hashlib
import io
import json
import os
import pathlib
import plistlib
import re
import shutil
import struct
import sys
import tempfile
import time
import types
import unittest
import zipfile
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_common as common
import app_update_processes as processes
import app_update_t3 as t3
import app_update_zcode as zcode
import app_updates as updater

OLD, NEW = "3.14.4", "3.14.5"
UNIT_START = ["/usr/bin/systemctl", "--user", "start", "t3-acp-update.service"]
# Windows paths for the npm runs and the Git for Windows shell; the tests run on any host.
NODE = pathlib.PureWindowsPath(r"C:\Program Files\nodejs\node.exe")
CLI = pathlib.PureWindowsPath(r"C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js")
GIT_BASH = pathlib.PureWindowsPath(r"C:\Program Files\Git\bin\bash.exe")


def write_asar(resources, version, name="@zcode/desktop", filler=0):
    """An app.asar whose root package.json carries version; filler entries grow the header like ZCode's node_modules."""
    resources.mkdir(parents=True, exist_ok=True)
    package = json.dumps({"name": name, "productName": "ZCode", "version": version}).encode()
    files = {"package.json": {"size": len(package), "offset": "0"}}
    if filler:
        files["node_modules"] = {"files": {"f%06d" % index: {"size": 1, "offset": "0"} for index in range(filler)}}
    header = json.dumps({"files": files}).encode()
    padded = header + b"\0" * ((4 - len(header) % 4) % 4)
    (resources / "app.asar").write_bytes(struct.pack("<4I", 4, 8 + len(padded), 4 + len(padded), len(header)) + padded + package)


def write_package(root, package, version):
    (root / package).mkdir(parents=True, exist_ok=True)
    (root / package / "package.json").write_text(json.dumps({"name": package, "version": version}))


def mac_bundle(path, version, identity=zcode.MAC_ID):
    (path / "Contents").mkdir(parents=True, exist_ok=True)
    with (path / "Contents/Info.plist").open("wb") as stream:
        plistlib.dump({"CFBundleIdentifier": identity, "CFBundleShortVersionString": version}, stream)


def manifest(version, asset, data, size=True):
    digest = base64.b64encode(hashlib.sha512(data).digest()).decode()
    return ("version: " + version + "\nfiles:\n  - url: " + asset + "\n    sha512: " + digest + "\n" +
            ("    size: " + str(len(data)) + "\n" if size else "") + "path: " + asset + "\nsha512: " + digest +
            "\nreleaseDate: '2026-09-30T08:16:39.977Z'\nreleaseNotes: |-\n  ## Fixes\n\n  - Fixture.\n")


def page(*entries):
    return ("<html>" + "".join('<a href="https://cdn-zcode.z.ai/zcode/electron/releases/%s/%s/latest.yml">x</a>' % entry
                               for entry in entries) + "</html>").encode()


def response(body, url):
    value = mock.MagicMock()
    value.geturl.return_value = url
    value.read.side_effect = lambda size=-1: body[:size] if size >= 0 else body
    return contextlib.nullcontext(value)


class ZCodeFixtures(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(pathlib.Path, "home", return_value=self.root).start()
        mock.patch.dict(os.environ, {"LOCALAPPDATA": str(self.root / "local"), "APPDATA": str(self.root / "roaming"),
                                     "PROCESSOR_ARCHITECTURE": "AMD64"}).start()
        zcode.UBUNTU_RUN.clear()
        self.addCleanup(zcode.UBUNTU_RUN.clear)
        self.deadline = time.monotonic() + 900

    # ------------------------------------------------------------ fixtures
    def unit(self):
        unit = self.root / ".config/systemd/user" / t3.ACP_UNIT
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text("[Service]\nType=oneshot\n")

    def ubuntu_app(self, version=OLD):
        app = self.root / ".local/opt/zcode/app"
        write_asar(app / "resources", version)
        (app / "zcode").touch()
        return app

    def node_modules(self, host="ubuntu"):
        return self.root / (".local/lib/node_modules" if host == "ubuntu" else "roaming/npm/node_modules")

    def adapters(self, host="ubuntu", muse="0.10.0", acp="0.65.1"):
        root = self.node_modules(host)
        for package, version in (("@brokkai/muse-acp", muse), ("zcode-acp-server", acp)):
            if version:
                write_package(root, package, version)
        return zcode.detect_adapters(host)

    def windows_app(self, version=OLD):
        executable = self.root / "local/Programs/ZCode/ZCode.exe"
        write_asar(executable.parent / "resources", version)
        executable.touch()
        (executable.parent / "Uninstall ZCode.exe").touch()
        return executable

    def mac_app(self, version=OLD):
        bundle = self.root / "Applications" / zcode.MAC_NAME
        mac_bundle(bundle, version)
        return common.Install("zcode", "mac", bundle, version, "official-download", zcode.MAC_ID, zcode.MAC_TEAM, bundle)

    def registry(self, muse="0.10.1", acp="0.65.1"):
        return mock.patch.object(zcode, "registry_version", side_effect={"muse-acp": muse, "zcode-acp-server": acp}.get)

    def parts(self, row):
        return [(part["name"], part["previousVersion"], part["version"]) for part in row["parts"]]

    # ------------------------------------------------------------ detection
    def test_detects_zcode_on_every_platform_from_its_files_without_running_anything(self):
        with mock.patch.object(zcode, "command") as run, mock.patch.object(zcode, "MAC_PATH", self.root / "Applications" / zcode.MAC_NAME):
            for host in ("ubuntu", "mac", "windows"):
                self.assertIsNone(zcode.detect_zcode(host))
            app = self.ubuntu_app()
            install = zcode.detect_zcode("ubuntu")
            self.assertEqual((install.app_id, install.version, install.manager, install.package_root), ("zcode", OLD, "official-download", app))
            # The asar wins; t3-acp-update's stamp is the fallback when it cannot be read.
            (app.parent / ".installed-version").write_text(NEW + "\n")
            self.assertEqual(zcode.detect_zcode("ubuntu").version, OLD)
            (app / "resources/app.asar").unlink()
            self.assertEqual(zcode.detect_zcode("ubuntu").version, NEW)
            executable = self.windows_app()
            install = zcode.detect_zcode("windows")
            self.assertEqual((install.path, install.version, install.package_root), (executable, OLD, executable.parent))
            write_asar(executable.parent / "resources", NEW, name="impostor")
            self.assertIsNone(zcode.detect_zcode("windows").version)
            bundle = self.mac_app().path
            install = zcode.detect_zcode("mac")
            self.assertEqual((install.path, install.version, install.identity, install.publisher), (bundle, OLD, zcode.MAC_ID, zcode.MAC_TEAM))
            mac_bundle(bundle, NEW, identity="com.example.other")
            self.assertIsNone(zcode.detect_zcode("mac").version)
        run.assert_not_called()

    def test_zcode_asar_header_of_several_megabytes_is_read_and_t3_keeps_its_two_megabyte_bound(self):
        resources = self.root / "resources"
        write_asar(resources, NEW, filler=70000)
        self.assertGreater((resources / "app.asar").stat().st_size, 2 * 1024 * 1024)
        self.assertEqual(zcode.asar_version(resources), NEW)
        self.assertIsNone(common.asar_package(resources / "app.asar"))
        t3_resources = self.root / "t3/resources"
        write_asar(t3_resources, "0.0.46-nightly.20261007.2774", name="t3code", filler=70000)
        self.assertIsNone(t3.windows_bundle_version(t3_resources.parent / t3.WINDOWS_NAME))
        write_asar(t3_resources, "0.0.46-nightly.20261007.2774", name="t3code")
        self.assertEqual(t3.windows_bundle_version(t3_resources.parent / t3.WINDOWS_NAME), "0.0.46-nightly.20261007.2774")

    def test_detects_adapters_with_each_part_and_null_for_a_missing_one(self):
        self.assertIsNone(zcode.detect_adapters("ubuntu"))
        install = self.adapters(acp=None)
        self.assertEqual((install.app_id, install.version, install.manager), ("t3-acp-adapters", None, "npm"))
        self.assertEqual(install.parts, {"muse-acp": "0.10.0", "zcode-acp-server": None})
        install = self.adapters("windows")
        self.assertEqual(install.path, self.root / "roaming/npm/node_modules")
        self.assertEqual(install.parts, {"muse-acp": "0.10.0", "zcode-acp-server": "0.65.1"})
        self.assertEqual(zcode.adapter_root("mac"), pathlib.Path("/opt/homebrew/lib/node_modules"))

    def test_detect_returns_ten_apps_and_inventory_lists_adapter_parts(self):
        self.assertEqual(list(common.APP_LABELS)[-3:], ["zcode", "t3-acp-adapters", "t3-code"])
        self.assertEqual((common.APP_LABELS["zcode"], common.APP_LABELS["t3-acp-adapters"]), ("ZCode", "T3 ACP adapters"))
        self.ubuntu_app()
        self.adapters()
        with mock.patch.object(updater, "detect_cli", return_value=None), mock.patch.object(updater, "detect_desktop", return_value=None), \
                mock.patch.object(updater, "detect_t3", return_value=None):
            found = updater.detect("ubuntu")
            output = io.StringIO()
            with mock.patch.object(sys, "argv", ["helper", "--platform", "ubuntu"]), contextlib.redirect_stdout(output):
                updater.main()
        self.assertEqual(list(found), list(common.APP_LABELS))
        self.assertEqual(len(found), 10)
        self.assertEqual(found["zcode"].version, OLD)
        apps = {row["appId"]: row for row in json.loads(output.getvalue())["apps"]}
        self.assertEqual(len(apps), 10)
        self.assertEqual(apps["t3-acp-adapters"]["parts"], {"muse-acp": "0.10.0", "zcode-acp-server": "0.65.1"})
        self.assertEqual((apps["zcode"]["version"], apps["zcode"]["manager"]), (OLD, "official-download"))
        self.assertNotIn("parts", apps["zcode"])

    # ------------------------------------------------------------ feeds
    def test_newest_zcode_comes_from_the_bounded_official_page_sorted_numerically(self):
        body = page(("3.9.9", "linux-x64"), (NEW, "linux-x64"), ("3.10.1", "linux-x64"), ("9.9.9", "windows-x64"), ("3.14.6", "linux-arm64"))
        with mock.patch.object(zcode.urllib.request, "urlopen", return_value=response(body, "https://zcode.z.ai/en")) as get:
            self.assertEqual(zcode.latest_zcode("linux-x64"), NEW)
        self.assertEqual(get.call_args.args[0].full_url, "https://zcode.z.ai/en")
        for body, url in ((body, "https://example.com/en"), (page(("3.9.9", "macos-arm64")), "https://zcode.z.ai/en"),
                          (b"x" * (zcode.PAGE_LIMIT + 1), "https://zcode.z.ai/en")):
            with mock.patch.object(zcode.urllib.request, "urlopen", return_value=response(body, url)):
                with self.assertRaises(common.UpdateFailure):
                    zcode.latest_zcode("linux-x64")
        with mock.patch.object(zcode.urllib.request, "urlopen", side_effect=OSError("offline")):
            with self.assertRaisesRegex(common.UpdateFailure, "update_failed"):
                zcode.latest_zcode("linux-x64")

    def test_feed_folder_follows_this_computer(self):
        with mock.patch.object(zcode.platform, "machine", return_value="x86_64"):
            self.assertEqual((zcode.feed_key("ubuntu"), zcode.feed_key("windows")), ("linux-x64", "windows-x64"))
        with mock.patch.object(zcode.platform, "machine", return_value="arm64"):
            self.assertEqual(zcode.feed_key("mac"), "macos-arm64")
        with mock.patch.dict(os.environ, {"PROCESSOR_ARCHITECTURE": "ARM64"}):
            self.assertEqual(zcode.feed_key("windows"), "windows-arm64")

    def test_registry_version_requires_the_exact_package_and_registry(self):
        good = json.dumps({"name": "zcode-acp-server", "version": "0.65.2"}).encode()
        with mock.patch.object(zcode.urllib.request, "urlopen", return_value=response(good, zcode.REGISTRY["zcode-acp-server"])) as get:
            self.assertEqual(zcode.registry_version("zcode-acp-server"), "0.65.2")
        self.assertEqual(get.call_args.args[0].full_url, "https://registry.npmjs.org/zcode-acp-server/latest")
        for body, url in ((json.dumps({"name": "other", "version": "1.0.0"}).encode(), zcode.REGISTRY["zcode-acp-server"]),
                          (good, "https://example.com/zcode-acp-server"), (b"not json", zcode.REGISTRY["zcode-acp-server"])):
            with mock.patch.object(zcode.urllib.request, "urlopen", return_value=response(body, url)):
                self.assertIsNone(zcode.registry_version("zcode-acp-server"))
        self.assertEqual(zcode.REGISTRY["muse-acp"], "https://registry.npmjs.org/@brokkai%2fmuse-acp/latest")

    def test_manifest_binds_sha512_and_size_to_the_exact_zcode_asset(self):
        zip_data, dmg_data = b"zip", b"dmg-longer"
        text = (manifest(NEW, "ZCode-3.14.5-mac-arm64.zip", zip_data).replace("path:", "  - url: ZCode-3.14.5-mac-arm64.dmg\n    sha512: " +
                base64.b64encode(hashlib.sha512(dmg_data).digest()).decode() + "\n    size: 10\npath:", 1))
        self.assertEqual(t3.manifest_asset(text, NEW, "ZCode-3.14.5-mac-arm64.zip"), (hashlib.sha512(zip_data).digest(), 3))
        self.assertEqual(t3.manifest_asset(text, NEW, "ZCode-3.14.5-mac-arm64.dmg")[1], 10)
        self.assertEqual(t3.manifest_hash(text, NEW, "ZCode-3.14.5-mac-arm64.zip"), hashlib.sha512(zip_data).digest())
        # A top-level size never stands in for the asset's own.
        self.assertIsNone(t3.manifest_asset(manifest(NEW, "A.exe", b"a", size=False).replace("releaseDate", "size: 99\nreleaseDate"), NEW, "A.exe")[1])
        with self.assertRaisesRegex(common.UpdateFailure, "signature_failed"):
            t3.manifest_asset(text, OLD, "ZCode-3.14.5-mac-arm64.zip")

    def test_package_download_is_bound_to_the_manifest_size_and_sha512(self):
        data, asset = b"MZ fixture installer", "ZCode-" + NEW + "-win-x64.exe"
        folder = self.root / "stage"
        folder.mkdir()
        # Same length, one byte changed: only the sha512 can tell.
        for served, accepted in ((data, True), (data[:-1] + b"!", False)):
            def download(url, target, **kwargs):
                if url.endswith("latest.yml"):
                    self.assertEqual(kwargs["maximum"], 65536)
                    target.write_text(manifest(NEW, asset, data))
                else:
                    self.assertEqual((url, kwargs["maximum"]), (zcode.RELEASES + NEW + "/windows-x64/" + asset, len(data)))
                    target.write_bytes(served)
            with self.subTest(accepted=accepted), mock.patch.object(zcode, "download", side_effect=download):
                if accepted:
                    self.assertEqual(zcode.verified_package("windows-x64", NEW, folder, self.deadline).read_bytes(), data)
                else:
                    with self.assertRaisesRegex(common.UpdateFailure, "signature_failed"):
                        zcode.verified_package("windows-x64", NEW, folder, self.deadline)

    # ------------------------------------------------------------ Ubuntu
    def ubuntu_patches(self, run=None, processes_list=None, newest=NEW):
        return [mock.patch.object(zcode, "latest_zcode", return_value=newest), mock.patch.object(zcode.platform, "machine", return_value="x86_64"),
                mock.patch.object(zcode, "command", side_effect=run), mock.patch.object(zcode, "scan", return_value=processes_list or [])]

    def test_ubuntu_current_zcode_and_adapters_run_nothing(self):
        self.unit()
        self.ubuntu_app(NEW)
        install = zcode.detect_zcode("ubuntu")
        adapters = self.adapters(muse="0.10.1")
        with contextlib.ExitStack() as stack, self.registry():
            run, scan = [stack.enter_context(patch) for patch in self.ubuntu_patches()][2:]
            zrow = zcode.update_zcode(install, self.deadline)
            arow = zcode.update_adapters(adapters, self.deadline)
        self.assertEqual((zrow["status"], zrow["previousVersion"], zrow["version"], zrow["updateAttempted"]), ("current", NEW, NEW, False))
        self.assertEqual((arow["status"], arow["previousVersion"], arow["version"], arow["manager"]), ("current", None, None, "npm"))
        self.assertEqual(self.parts(arow), [("muse-acp", "0.10.1", "0.10.1"), ("zcode-acp-server", "0.65.1", "0.65.1")])
        run.assert_not_called(); scan.assert_not_called()

    def test_ubuntu_t3_session_markers_report_in_use_without_running_the_unit(self):
        self.unit()
        app = self.ubuntu_app()
        sessions = (
            processes.Process(10, 1, 1, "/usr/bin/node", "10", ["node", str(self.node_modules() / "zcode-acp-server/dist/cli.js"), "--acp"]),
            # The adapter's child rewrites its argv: no path to the app is left on its command line.
            processes.Process(11, 10, 1, "/usr/bin/node", "11", ["zcode-cli"]),
            processes.Process(12, 1, 1, str(app / "zcode"), "12", [str(app / "zcode"), str(app / "resources/glm/zcode.cjs"), "--version"]),
        )
        for session in sessions:
            with self.subTest(pid=session.pid), contextlib.ExitStack() as stack:
                run = [stack.enter_context(patch) for patch in self.ubuntu_patches(processes_list=[session])][2]
                row = zcode.update_zcode(zcode.detect_zcode("ubuntu"), self.deadline)
                self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), ("action_required", "in_use", OLD, False))
                run.assert_not_called()
        unrelated = processes.Process(13, 1, 1, "/usr/bin/node", "13", ["node", "/srv/zcode-acp-server-notes/cli.js"])
        self.assertEqual(zcode.t3_sessions([unrelated]), [])

    def test_ubuntu_one_unit_run_serves_both_rows(self):
        self.unit()
        app = self.ubuntu_app()
        install, adapters = zcode.detect_zcode("ubuntu"), self.adapters()
        def run(argv, **kwargs):
            # t3-acp-update swaps ZCode and updates muse-acp in its one run.
            write_asar(app / "resources", NEW)
            write_package(self.node_modules(), "@brokkai/muse-acp", "0.10.1")
        phases = []
        with contextlib.ExitStack() as stack, self.registry():
            command = [stack.enter_context(patch) for patch in self.ubuntu_patches(run)][2]
            zrow = zcode.update_zcode(install, self.deadline, phases.append)
            arow = zcode.update_adapters(adapters, self.deadline, phases.append)
        command.assert_called_once()
        self.assertEqual(command.call_args.args[0], UNIT_START)
        self.assertLessEqual(command.call_args.kwargs["timeout"], zcode.UNIT_SECONDS)
        self.assertEqual((zrow["status"], zrow["messageCode"], zrow["previousVersion"], zrow["version"], zrow["manager"], zrow["updateAttempted"]),
                         ("updated", "updated", OLD, NEW, "official-download", True))
        self.assertEqual((arow["status"], arow["messageCode"], arow["previousVersion"], arow["version"], arow["updateAttempted"]),
                         ("updated", "updated", None, None, True))
        self.assertEqual(self.parts(arow), [("muse-acp", "0.10.0", "0.10.1"), ("zcode-acp-server", "0.65.1", "0.65.1")])
        self.assertEqual(phases, ["updating", "updating"])

    def test_ubuntu_unit_missing_is_unsupported_for_both_rows(self):
        self.ubuntu_app()
        with contextlib.ExitStack() as stack, self.registry():
            run = [stack.enter_context(patch) for patch in self.ubuntu_patches()][2]
            zrow = zcode.update_zcode(zcode.detect_zcode("ubuntu"), self.deadline)
            arow = zcode.update_adapters(self.adapters(), self.deadline)
        self.assertEqual((zrow["status"], zrow["messageCode"], zrow["updateAttempted"]), ("failed", "unsupported", False))
        self.assertEqual((arow["status"], arow["messageCode"], arow["updateAttempted"]), ("failed", "unsupported", False))
        run.assert_not_called()

    def test_ubuntu_unchanged_after_the_run_is_in_use_while_the_app_runs_else_its_failure(self):
        self.unit()
        app = self.ubuntu_app()
        gui = processes.Process(20, 1, 1, str(app / "zcode"), "20", [str(app / "zcode")])
        for running, outcome, expected in (([gui], None, ("action_required", "in_use")), ([], None, ("failed", "update_failed")),
                                           ([], common.UpdateFailure("timeout"), ("failed", "timeout"))):
            zcode.UBUNTU_RUN.clear()
            with self.subTest(expected=expected), contextlib.ExitStack() as stack:
                scans = iter([[], running])
                patches = self.ubuntu_patches(outcome)
                patches[3] = mock.patch.object(zcode, "scan", side_effect=lambda host: next(scans))
                run = [stack.enter_context(patch) for patch in patches][2]
                row = zcode.update_zcode(zcode.detect_zcode("ubuntu"), self.deadline)
                self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), (*expected, OLD, True))
                run.assert_called_once()

    def test_ubuntu_unit_wait_is_bounded_by_the_host_deadline(self):
        self.unit()
        self.ubuntu_app()
        for remaining, bound in ((100, 100), (3600, zcode.UNIT_SECONDS)):
            zcode.UBUNTU_RUN.clear()
            with contextlib.ExitStack() as stack:
                run = [stack.enter_context(patch) for patch in self.ubuntu_patches()][2]
                zcode.update_zcode(zcode.detect_zcode("ubuntu"), time.monotonic() + remaining)
            self.assertLessEqual(run.call_args.kwargs["timeout"], bound)
            self.assertGreater(run.call_args.kwargs["timeout"], bound - 5)
        zcode.UBUNTU_RUN.clear()
        with contextlib.ExitStack() as stack:
            run = [stack.enter_context(patch) for patch in self.ubuntu_patches()][2]
            row = zcode.update_zcode(zcode.detect_zcode("ubuntu"), time.monotonic() - 1)
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("failed", "timeout", False))
        run.assert_not_called()

    def test_ubuntu_adapters_run_the_unit_when_the_zcode_row_did_not_unless_an_adapter_runs(self):
        self.unit()
        muse = processes.Process(30, 1, 1, "/usr/bin/node", "30", ["node", str(self.root / ".local/bin/muse-acp")])
        acp = processes.Process(31, 1, 1, "/usr/bin/node", "31", ["node", str(self.node_modules() / "zcode-acp-server/dist/cli.js")])
        for running in ([muse], [acp]):
            with self.subTest(pid=running[0].pid), contextlib.ExitStack() as stack, self.registry():
                run = [stack.enter_context(patch) for patch in self.ubuntu_patches(processes_list=running)][2]
                row = zcode.update_adapters(self.adapters(), self.deadline)
                self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", False))
                self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", "0.65.1", "0.65.1")])
                run.assert_not_called()
        with contextlib.ExitStack() as stack, self.registry():
            run = [stack.enter_context(patch) for patch in self.ubuntu_patches(lambda argv, **kwargs: write_package(self.node_modules(), "@brokkai/muse-acp", "0.10.1"))][2]
            row = zcode.update_adapters(self.adapters(), self.deadline)
        self.assertEqual(row["status"], "updated")
        run.assert_called_once_with(UNIT_START, timeout=mock.ANY)
        # An adapter still running after an unchanged run is in_use; with none left it failed.
        zcode.UBUNTU_RUN.clear()
        for running, expected in (([muse], ("action_required", "in_use")), ([], ("failed", "update_failed"))):
            zcode.UBUNTU_RUN.clear()
            scans = iter([[], running])
            with self.subTest(expected=expected), contextlib.ExitStack() as stack, self.registry(muse="0.10.2"):
                patches = self.ubuntu_patches()
                patches[3] = mock.patch.object(zcode, "scan", side_effect=lambda host: next(scans))
                [stack.enter_context(patch) for patch in patches]
                row = zcode.update_adapters(self.adapters(), self.deadline)
                self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), (*expected, True))

    def test_unreadable_feed_or_registry_fails_without_running_anything(self):
        self.unit()
        self.ubuntu_app()
        with mock.patch.object(zcode.urllib.request, "urlopen", side_effect=OSError("offline")), \
                mock.patch.object(zcode.platform, "machine", return_value="x86_64"), mock.patch.object(zcode, "command") as run, \
                mock.patch.object(zcode, "scan", return_value=[]):
            zrow = zcode.update_zcode(zcode.detect_zcode("ubuntu"), self.deadline)
            arow = zcode.update_adapters(self.adapters(), self.deadline)
        self.assertEqual((zrow["status"], zrow["messageCode"], zrow["version"], zrow["updateAttempted"]), ("failed", "update_failed", OLD, False))
        self.assertEqual((arow["status"], arow["messageCode"], arow["updateAttempted"]), ("failed", "update_failed", False))
        self.assertEqual(self.parts(arow), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", "0.65.1", "0.65.1")])
        run.assert_not_called()

    def test_run_apply_runs_zcode_rows_before_codex_and_t3_with_one_unit_run(self):
        self.unit()
        app = self.ubuntu_app()
        installs = {key: None for key in common.APP_LABELS}
        installs.update(zcode=zcode.detect_zcode("ubuntu"), **{"t3-acp-adapters": self.adapters()})
        def run(argv, **kwargs):
            write_asar(app / "resources", NEW)
            write_package(self.node_modules(), "@brokkai/muse-acp", "0.10.1")
        events = []
        with contextlib.ExitStack() as stack, self.registry(), mock.patch.object(updater, "detect", return_value=installs):
            command = [stack.enter_context(patch) for patch in self.ubuntu_patches(run)][2]
            value = updater.run_apply("ubuntu", emit=events.append)
        rows = {row["appId"]: row for row in value["results"]}
        self.assertEqual(len(rows), 10)
        self.assertEqual((rows["zcode"]["status"], rows["t3-acp-adapters"]["status"], rows["t3-code"]["status"]), ("updated", "updated", "not_installed"))
        command.assert_called_once()
        phases = [(event["appId"], event["phase"]) for event in events if event.get("event") == "app" and event.get("appId") in ("zcode", "t3-acp-adapters")]
        self.assertEqual(phases, [("zcode", "checking"), ("zcode", "updating"), ("zcode", "updating"),
                                  ("t3-acp-adapters", "checking"), ("t3-acp-adapters", "updating"), ("t3-acp-adapters", "updating")])
        self.assertEqual([row["appId"] for row in value["results"]][-4:], ["zcode", "t3-acp-adapters", "codex-cli", "t3-code"])

    # ------------------------------------------------------------ Mac
    def mac_run(self, install, gui=None, session=None, fail=None, served=None, entries=None, opened_later=False):
        """Drive the Mac flow with fixture downloads, extraction and codesign; returns (row, calls, downloads).

        The manifest describes the fixture archive (entries); served replaces the bytes actually downloaded.
        """
        archive_path = self.root / "fixture.zip"
        with zipfile.ZipFile(archive_path, "w") as archive:
            for name, data in entries or [(zcode.MAC_NAME + "/Contents/Info.plist", plistlib.dumps({"CFBundleIdentifier": zcode.MAC_ID, "CFBundleShortVersionString": NEW})),
                                          (zcode.MAC_NAME + "/Contents/MacOS/ZCode", b"fixture")]:
                archive.writestr(name, data)
        asset = "ZCode-" + NEW + "-mac-arm64.zip"
        calls, downloads = [], []
        def download(url, target, **kwargs):
            downloads.append(url)
            target.write_bytes(manifest(NEW, asset, archive_path.read_bytes()).encode() if url.endswith("latest.yml") else served or archive_path.read_bytes())
        def command(argv, **kwargs):
            argv = [str(item) for item in argv]
            calls.append(argv)
            if fail and fail(argv):
                raise common.UpdateFailure()
            if argv[:2] == ["/usr/bin/ditto", "-x"]:
                with zipfile.ZipFile(argv[3]) as archive:
                    archive.extractall(argv[4])
            elif argv[0] == "/usr/bin/ditto":
                shutil.copytree(argv[1], argv[2], symlinks=True)
        main = str(install.path / "Contents/MacOS/ZCode")
        table = []
        if gui:
            table += [processes.Process(40, 1, os.getuid(), main, "40"), processes.Process(41, 40, os.getuid(), main + " Helper", "41")]
        if session:
            table += [processes.Process(50, 1, os.getuid(), "/opt/homebrew/bin/node", "50"),
                      processes.Process(51, 50, os.getuid(), main, "51")]
        arguments = {40: [main], 41: [main + " Helper", "--type=renderer"],
                     50: ["node", "/opt/homebrew/lib/node_modules/zcode-acp-server/dist/cli.js"],
                     51: [main, str(install.path / "Contents/Resources/glm/zcode.cjs")]}
        scans = [0]
        def scan(host):
            scans[0] += 1
            later = opened_later and scans[0] > 1
            rows = table + ([processes.Process(60, 1, os.getuid(), main, "60")] if later else [])
            return [processes.Process(item.pid, item.ppid, item.uid, item.exe, item.identity) for item in rows]
        arguments[60] = [main]
        with mock.patch.object(zcode, "latest_zcode", return_value=NEW), mock.patch.object(zcode.platform, "machine", return_value="arm64"), \
                mock.patch.object(zcode, "download", side_effect=download), mock.patch.object(zcode, "command", side_effect=command), \
                mock.patch.object(t3, "command", side_effect=command), mock.patch.object(zcode, "scan", side_effect=scan), \
                mock.patch.object(zcode, "mac_arguments", side_effect=lambda pid: list(arguments[pid])):
            row = zcode.update_zcode(install, self.deadline)
        return row, calls, downloads

    def test_mac_running_gui_reports_quit_first_before_any_download(self):
        install = self.mac_app()
        row, calls, downloads = self.mac_run(install, gui=True, session=True)
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), ("action_required", "quit_first", OLD, False))
        self.assertEqual((calls, downloads), ([], []))

    def test_mac_t3_session_alone_is_in_use_not_quit_first(self):
        install = self.mac_app()
        row, calls, downloads = self.mac_run(install, session=True)
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", False))
        self.assertEqual((calls, downloads), ([], []))

    def test_mac_installs_the_verified_bundle_with_an_inline_requirement_and_never_opens_it(self):
        install = self.mac_app()
        row, calls, downloads = self.mac_run(install)
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"], row["updateAttempted"], row["forcedStops"]),
                         ("updated", "updated", OLD, NEW, True, 0))
        self.assertEqual(zcode.mac_version(install.path), NEW)
        self.assertEqual(downloads, [zcode.RELEASES + NEW + "/macos-arm64/latest.yml", zcode.RELEASES + NEW + "/macos-arm64/ZCode-" + NEW + "-mac-arm64.zip"])
        requirement = '-R=identifier "dev.zcode.app" and anchor apple generic and certificate leaf[subject.OU] = "8A5X4JJ39T"'
        self.assertIn(["/usr/bin/codesign", "--verify", requirement, str(install.path)], calls)
        self.assertIn(["/usr/sbin/spctl", "--assess", "--type", "execute", str(install.path)], calls)
        self.assertFalse(any(call[0] == "/usr/bin/open" for call in calls))
        self.assertEqual(sorted(path.name for path in install.path.parent.iterdir()), [zcode.MAC_NAME])

    def test_mac_verification_failures_never_swap(self):
        older = [(zcode.MAC_NAME + "/Contents/Info.plist", plistlib.dumps({"CFBundleIdentifier": zcode.MAC_ID, "CFBundleShortVersionString": "3.14.3"}))]
        cases = {
            "sha512": dict(served=b"tampered"),
            "version": dict(entries=older),
            "codesign": dict(fail=lambda argv: argv[:3] == ["/usr/bin/codesign", "--verify", "--deep"]),
            "team": dict(fail=lambda argv: argv[0] == "/usr/bin/codesign" and argv[2].startswith("-R=")),
            "spctl": dict(fail=lambda argv: argv[0] == "/usr/sbin/spctl"),
        }
        for name, options in cases.items():
            with self.subTest(name):
                shutil.rmtree(self.root / "Applications", ignore_errors=True)
                install = self.mac_app()
                row, calls, _ = self.mac_run(install, **options)
                self.assertEqual((row["status"], row["messageCode"], row["version"]), ("failed", "signature_failed", OLD))
                self.assertEqual(zcode.mac_version(install.path), OLD)
                self.assertEqual(sorted(path.name for path in install.path.parent.iterdir()), [zcode.MAC_NAME])
                if name == "sha512":
                    self.assertEqual(calls, [])  # Nothing is extracted from an unverified package.

    def test_mac_archive_escape_is_rejected_before_extraction(self):
        install = self.mac_app()
        row, calls, _ = self.mac_run(install, entries=[("../escape", b"fixture")])
        # The manifest vouches for this archive, so only the archive check can refuse it.
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "signature_failed"))
        self.assertFalse(any(call[:2] == ["/usr/bin/ditto", "-x"] for call in calls))

    def test_mac_final_check_failure_rolls_back_the_swap(self):
        install = self.mac_app()
        checks = [0]
        def fail(argv):
            # The third strict check is the installed bundle right after the swap.
            if argv[:3] == ["/usr/bin/codesign", "--verify", "--deep"]:
                checks[0] += 1
                return checks[0] == 3
            return False
        row, _, _ = self.mac_run(install, fail=fail)
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), ("failed", "signature_failed", OLD, True))
        self.assertEqual(zcode.mac_version(install.path), OLD)
        self.assertEqual(sorted(path.name for path in install.path.parent.iterdir()), [zcode.MAC_NAME])

    def test_mac_app_opened_during_the_download_is_never_swapped(self):
        install = self.mac_app()
        row, calls, downloads = self.mac_run(install, opened_later=True)
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("action_required", "quit_first", OLD))
        self.assertEqual(len(downloads), 2)
        self.assertEqual(zcode.mac_version(install.path), OLD)
        self.assertEqual(sorted(path.name for path in install.path.parent.iterdir()), [zcode.MAC_NAME])

    def test_mac_adapters_run_the_script_and_report_their_parts(self):
        root = self.root / "homebrew"
        for package, version in (("@brokkai/muse-acp", "0.10.0"), ("zcode-acp-server", "0.65.1")):
            write_package(root, package, version)
        with mock.patch.object(zcode, "adapter_root", return_value=root):
            install = zcode.detect_adapters("mac")
        script = self.root / zcode.MAC_SCRIPT
        with self.registry(), mock.patch.object(zcode, "command") as run:
            row = zcode.update_adapters(install, self.deadline)
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "unsupported"))
        run.assert_not_called()
        script.parent.mkdir(parents=True, exist_ok=True)
        script.touch()
        for outcome, expected in ((lambda argv, **kwargs: write_package(root, "@brokkai/muse-acp", "0.10.1"), ("updated", "updated")),
                                  (None, ("failed", "update_failed")), (common.UpdateFailure("timeout"), ("failed", "timeout"))):
            write_package(root, "@brokkai/muse-acp", "0.10.0")
            with self.subTest(expected=expected), self.registry(), mock.patch.object(zcode, "command", side_effect=outcome) as run, \
                    mock.patch.object(zcode, "scan") as scan:
                row = zcode.update_adapters(install, self.deadline)
                self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), (*expected, True))
                run.assert_called_once_with([script], timeout=mock.ANY)
                self.assertLessEqual(run.call_args.kwargs["timeout"], zcode.MAC_SCRIPT_SECONDS)
                scan.assert_not_called()  # The Mac script never defers for running adapters.
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", "0.65.1", "0.65.1")])

    # ------------------------------------------------------------ Windows
    def windows_run(self, install, running=(), session=1, signer=None, installer=None, reopen=None, extra=()):
        """Drive the Windows flow; returns (row, events, closed contexts, reopened contexts)."""
        data = b"MZ fixture installer"
        asset = "ZCode-" + NEW + "-win-x64.exe"
        events, closed, reopened = [], [], []
        self.table = [*extra]
        for pid in running:
            exe = str(install.path)
            self.table += [processes.Process(pid, 4, 0, exe, str(pid), [exe], session=session),
                           processes.Process(pid + 1, pid, 0, exe, str(pid + 1), [exe, "--type=renderer"], session=session)]
        def download(url, target, **kwargs):
            events.append("download")
            target.write_bytes(manifest(NEW, asset, data).encode() if url.endswith("latest.yml") else data)
        def powershell(script, **kwargs):
            events.append("verify")
            if signer:
                raise signer
            return ""
        def close(value, contexts):
            events.append("close")
            closed.append(list(contexts))
            mains = {item.pid for item in contexts}
            self.table = [item for item in self.table if item.pid not in mains and item.ppid not in mains]
            return 1 if contexts else 0
        def install_package(package, seconds):
            events.append("install")
            self.assertEqual(package.name, asset)
            write_asar(install.path.parent / "resources", NEW)
            (install.path.parent / "new-file.dll").touch()
            if installer:
                raise installer
        def restart(value, contexts):
            events.append("reopen")
            reopened.append(list(contexts))
            if reopen:
                raise reopen
            return len(contexts)
        kernel = mock.Mock()
        kernel.ProcessIdToSessionId.side_effect = lambda pid, value: (setattr(value._obj, "value", 1), True)[1]
        with mock.patch.object(zcode, "latest_zcode", return_value=NEW), mock.patch.object(zcode, "download", side_effect=download), \
                mock.patch.object(zcode, "powershell", side_effect=powershell), mock.patch.object(zcode, "terminate_desktops", side_effect=close), \
                mock.patch.object(zcode, "nsis_silent", side_effect=install_package), mock.patch.object(zcode, "restart_desktops", side_effect=restart), \
                mock.patch.object(zcode, "live_contexts", return_value=[]), mock.patch.object(zcode, "scan", side_effect=lambda host: list(self.table)), \
                mock.patch.object(zcode.ctypes, "windll", types.SimpleNamespace(kernel32=kernel), create=True):
            row = zcode.update_zcode(install, self.deadline)
        return row, events, closed, reopened

    def windows_install(self):
        self.windows_app()
        return zcode.detect_zcode("windows")

    def test_windows_running_zcode_downloads_first_then_closes_installs_and_reopens(self):
        install = self.windows_install()
        row, events, closed, reopened = self.windows_run(install, running=(100,))
        self.assertEqual(events, ["download", "download", "verify", "close", "install", "reopen"])
        self.assertEqual([[item.pid for item in batch] for batch in closed], [[100]])
        self.assertEqual([[item.pid for item in batch] for batch in reopened], [[100]])
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"], row["manager"]),
                         ("updated", "desktop_reopened", OLD, NEW, "official-download"))
        self.assertEqual((row["restartedProcesses"], row["forcedStops"], row["updateAttempted"], row["restartTargets"]), (1, 1, True, [{"kind": "desktop"}]))

    def test_windows_not_running_installs_without_closing_or_reopening(self):
        install = self.windows_install()
        row, events, closed, reopened = self.windows_run(install)
        self.assertEqual(events, ["download", "download", "verify", "install"])
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["restartedProcesses"]), ("updated", "updated", NEW, 0))
        self.assertNotIn("restartTargets", row)
        self.assertEqual((closed, reopened), ([], []))

    def test_windows_signer_mismatch_is_signature_failed_and_nothing_closes(self):
        install = self.windows_install()
        row, events, closed, _ = self.windows_run(install, running=(100,), signer=common.UpdateFailure())
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), ("failed", "signature_failed", OLD, False))
        self.assertEqual(events, ["download", "download", "verify"])
        self.assertEqual(closed, [])
        package = self.root / "ZCode-3.14.5-win-x64.exe"
        with mock.patch.object(zcode, "powershell", return_value="") as ps:
            zcode.verify_signer(package, install.path)
        script = ps.call_args.args[0]
        for part in ("Get-AuthenticodeSignature -LiteralPath '" + str(package) + "'", "Get-AuthenticodeSignature -LiteralPath '" + str(install.path) + "'",
                     "$n.Status -ne 'Valid'", "$o.Status -ne 'Valid'", "$n.SignerCertificate.Subject -cne $o.SignerCertificate.Subject",
                     "Contains('SERIALNUMBER=91110108MA01KP2T5U')"):
            self.assertIn(part, script)
        # The subjects are compared inside PowerShell and never written out.
        self.assertNotIn("Write-Output", script)

    def test_windows_installer_failure_restores_the_rollback_copy_and_reopens_the_old_zcode(self):
        install = self.windows_install()
        row, events, closed, reopened = self.windows_run(install, running=(100,), installer=common.UpdateFailure())
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"], row["updateAttempted"]),
                         ("failed", "update_failed", OLD, OLD, True))
        self.assertEqual(zcode.installed_version(install), OLD)
        self.assertFalse((install.path.parent / "new-file.dll").exists())
        self.assertEqual(events[-3:], ["install", "close", "reopen"])  # The restore closes only the (now empty) family first.
        self.assertEqual([[item.pid for item in batch] for batch in reopened], [[100]])
        row, _, _, _ = self.windows_run(install, running=(100,), installer=common.UpdateFailure("timeout"))
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("failed", "timeout", OLD))

    def test_windows_reopen_failure_is_restart_failed_with_the_new_version(self):
        install = self.windows_install()
        row, _, _, _ = self.windows_run(install, running=(100,), reopen=common.UpdateFailure("restart_failed"))
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"], row["forcedStops"]),
                         ("restart_failed", "restart_failed", OLD, NEW, 1))
        self.assertEqual(zcode.installed_version(install), NEW)

    def test_windows_t3_session_is_in_use_and_another_session_is_quit_first_before_any_download(self):
        install = self.windows_install()
        node = processes.Process(70, 4, 0, "C:/Program Files/nodejs/node.exe", "70",
                                 ["node.exe", str(self.root / "roaming/npm/node_modules/zcode-acp-server/dist/cli.js")], session=1)
        row, events, closed, _ = self.windows_run(install, extra=(node,))
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", False))
        self.assertEqual((events, closed), ([], []))
        row, events, closed, _ = self.windows_run(install, running=(100,), session=2)
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("action_required", "quit_first", OLD))
        self.assertEqual((events, closed), ([], []))

    def test_windows_process_inventory_lists_zcode_for_this_user_only(self):
        executable = self.windows_app()
        install = zcode.detect_zcode("windows")
        rows = [{"pid": 1, "ppid": 0, "exe": str(executable), "args": str(executable), "identity": "start", "session": 1}]
        def inventory(script, **kwargs):
            names = re.findall(r"'([^']+)'", re.search(r"\$p\.Name -notin @\((.*?)\) -and -not \$tree\)\{continue\}", script).group(1))
            self.assertIn("ZCode.exe", names)
            self.assertIn("node.exe", names)
            self.assertIn("$o.Sid -ne $me -or -not $p.ExecutablePath", script)
            # Anything running from the npm global tree is listed whatever its name (martty.exe, Muse's native exe).
            self.assertIn("$npm=[IO.Path]::Combine($env:APPDATA,'npm','node_modules');", script)
            self.assertIn("$p.ExecutablePath.StartsWith($npm,[StringComparison]::OrdinalIgnoreCase)", script)
            return json.dumps([row for row in rows if pathlib.Path(row["exe"]).name in names])
        with mock.patch.object(processes, "powershell", side_effect=inventory), \
                mock.patch.object(processes, "windows_arguments", side_effect=lambda value: [value]):
            self.assertEqual([item.pid for item in processes.main_contexts(install, processes.scan("windows"))], [1])

    # Windows adapters: each behind package has its own npm run; a running or locked one is held, never failed.
    def windows_npm_run(self, installed, latest, running=(), outcomes=None, shell=True):
        """update_adapters on Windows with npm faked: outcomes maps a package to "updated", "unchanged" or an
        UpdateFailure to raise. Returns (row, [(npm argv, timeout), ...])."""
        install = self.adapters("windows", muse=installed["muse-acp"], acp=installed["zcode-acp-server"])
        prefix, runs = self.root / "roaming/npm", []
        def npm(argv, **kwargs):
            name = next(name for name, package in zcode.PACKAGES.items() if argv[-1] == package + "@latest")
            runs.append((argv, kwargs["timeout"]))
            outcome = (outcomes or {}).get(name, "updated")
            if isinstance(outcome, Exception):
                raise outcome
            if outcome == "updated":
                write_package(prefix / "node_modules", zcode.PACKAGES[name], latest[name])
        with self.registry(muse=latest["muse-acp"], acp=latest["zcode-acp-server"]), \
                mock.patch.object(zcode, "scan", return_value=list(running)), \
                mock.patch.object(zcode, "resolve_npm", return_value=(NODE, CLI)), \
                mock.patch.object(zcode, "git_bash", return_value=GIT_BASH if shell else None), \
                mock.patch.object(zcode, "command", side_effect=npm):
            row = zcode.update_adapters(install, self.deadline)
        return row, runs

    def muse_running(self, pid=80):
        """The native muse-acp.exe of the npm tree, as the process scan reports it."""
        exe = str(self.root / "roaming/npm/node_modules/@brokkai/muse-acp/native/x86_64-pc-windows-msvc/muse-acp.exe")
        return processes.Process(pid, 4, 0, exe, str(pid), [exe], session=1)

    def test_windows_adapters_each_install_in_its_own_npm_run_with_git_bash_as_script_shell(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"})
        prefix = str(self.root / "roaming/npm")
        self.assertEqual([argv for argv, _ in runs], [
            [str(NODE), str(CLI), "install", "--global", "--prefix", prefix, "--script-shell", str(GIT_BASH), "@brokkai/muse-acp@latest"],
            [str(NODE), str(CLI), "install", "--global", "--prefix", prefix, "--script-shell", str(GIT_BASH), "zcode-acp-server@latest"],
        ])
        self.assertTrue(all(timeout <= zcode.NPM_SECONDS for _, timeout in runs))
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("updated", "updated", True))
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.1"), ("zcode-acp-server", "0.65.0", "0.65.1")])
        self.assertTrue(all("inUse" not in part for part in row["parts"]))

    def test_windows_current_package_is_never_reinstalled(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"})
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("current", "current", False))
        self.assertEqual(runs, [])
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.1"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"})
        self.assertEqual([argv[-1] for argv, _ in runs], ["@brokkai/muse-acp@latest"])

    def test_windows_zcode_postinstall_failure_leaves_muse_acp_updated(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                         outcomes={"zcode-acp-server": common.UpdateFailure()})
        self.assertEqual(len(runs), 2)
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("failed", "update_failed", True))
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.1"), ("zcode-acp-server", "0.65.0", "0.65.0")])

    def test_windows_running_muse_is_never_installed_while_zcode_still_updates(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                         running=[self.muse_running()])
        self.assertEqual([argv[-1] for argv, _ in runs], ["zcode-acp-server@latest"])
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", True))
        self.assertEqual(row["parts"], [
            {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.0", "inUse": True},
            {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.1"},
        ])

    def test_windows_running_muse_alone_is_in_use_and_npm_never_runs(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.1"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                         running=[self.muse_running()])
        self.assertEqual(runs, [])
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", False))
        self.assertEqual([part.get("inUse", False) for part in row["parts"]], [True, False])

    def test_windows_npm_lock_on_a_package_is_in_use_for_that_package_only(self):
        # npm's fatal error lines name the lock and its folder; the path is read with and without escaped backslashes.
        escaped = "\n".join([
            r"npm error code EPERM",
            r"npm error syscall rmdir",
            r"npm error path C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp\\native",
            r"npm error Error: EPERM: operation not permitted, rmdir 'C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native'",
        ])
        row, _ = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                      outcomes={"muse-acp": common.UpdateFailure("update_failed", escaped)})
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", True))
        self.assertEqual(row["parts"], [
            {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.0", "inUse": True},
            {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.1"},
        ])
        busy = "\n".join([
            r"npm error code EBUSY",
            r"npm error path C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server",
            r"npm error Error: EBUSY: resource busy or locked, unlink 'C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server\dist\cli.js'",
        ])
        row, _ = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                      outcomes={"zcode-acp-server": common.UpdateFailure("update_failed", busy)})
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "in_use"))
        self.assertEqual(row["parts"], [
            {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.1"},
            {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.0", "inUse": True},
        ])

    def test_windows_npm_permission_error_naming_no_such_package_is_a_failure(self):
        other = "\n".join([r"npm error code EPERM", r"npm error path C:\Users\sitti\AppData\Local\npm-cache\_cacache\tmp"])
        cross = "\n".join([r"npm error code EPERM", r"npm error path C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server\dist"])
        for text in (other, cross):
            with self.subTest(text=text[:40]):
                row, _ = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.1"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                              outcomes={"muse-acp": common.UpdateFailure("update_failed", text)})
                self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))
                self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", "0.65.1", "0.65.1")])

    def test_windows_clean_npm_run_that_moves_nothing_is_a_failure(self):
        row, _ = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                      outcomes={"muse-acp": "unchanged", "zcode-acp-server": "unchanged"})
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("failed", "update_failed", True))
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", "0.65.0", "0.65.0")])

    def test_windows_without_git_bash_npm_gets_no_script_shell_and_zcode_fails_visibly(self):
        row, runs = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                         outcomes={"zcode-acp-server": common.UpdateFailure()}, shell=False)
        self.assertTrue(all("--script-shell" not in argv for argv, _ in runs))
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))

    def test_git_bash_is_found_only_in_git_for_windows_own_bin(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            base = pathlib.Path(directory)
            git = base / "Program Files/Git/bin/bash.exe"
            git.parent.mkdir(parents=True)
            git.touch()
            system = base / "Windows/System32/bash.exe"
            system.parent.mkdir(parents=True)
            system.touch()
            stack.enter_context(mock.patch.dict(os.environ, {"ProgramFiles": str(base / "Program Files")}))
            for key in ("ProgramW6432", "ProgramFiles(x86)"):
                stack.enter_context(mock.patch.dict(os.environ, {key: ""}))
                os.environ.pop(key)
            stack.enter_context(mock.patch.object(zcode.shutil, "which", return_value=None))
            self.assertEqual(zcode.git_bash(), git)
            os.environ["ProgramFiles"] = str(base / "Nothing")
            self.assertIsNone(zcode.git_bash())
            stack.enter_context(mock.patch.object(zcode.shutil, "which", return_value=str(base / "Program Files/Git/cmd/git.exe")))
            self.assertEqual(zcode.git_bash(), git)

    def test_adapter_owners_match_the_muse_native_exe_in_any_letter_case_on_windows(self):
        root = pathlib.PureWindowsPath(r"C:\Users\sitti\AppData\Roaming\npm\node_modules")
        native = r"C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native\x86_64-pc-windows-msvc\muse-acp.exe"
        for exe in (native, native.lower(), native.upper(), r"c:\Users\Sitti\AppData\Roaming\NPM\node_modules\@Brokkai\Muse-Acp\native\x86_64-pc-windows-msvc\muse-acp.exe"):
            with self.subTest(exe=exe):
                item = processes.Process(40, 4, 0, exe, "40", [exe], session=1)
                self.assertEqual(zcode.adapter_owners(item, root), {"muse-acp"})
                self.assertEqual(zcode.busy_packages([item], root), {"muse-acp"})
        node = r"C:\Program Files\nodejs\node.exe"
        script = r"C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\bin\muse-acp.cjs"
        self.assertEqual(zcode.adapter_owners(processes.Process(41, 4, 0, node, "41", [node, script], session=1), root), {"muse-acp"})
        martty = r"C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server\node_modules\zcode-acp-martty\vendor\win32-x64\martty.exe"
        self.assertEqual(zcode.adapter_owners(processes.Process(42, 4, 0, martty, "42", [martty], session=1), root), {"zcode-acp-server"})
        cli = r"C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server\dist\cli.js"
        self.assertEqual(zcode.adapter_owners(processes.Process(43, 4, 0, node, "43", [node, cli], session=1), root), {"zcode-acp-server"})
        sibling = r"C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp-tools\muse-acp-tools.exe"
        self.assertEqual(zcode.busy_packages([processes.Process(44, 4, 0, sibling, "44", [sibling], session=1)], root), set())

    def test_only_npm_error_lines_naming_a_package_folder_lock_that_package(self):
        escaped = "\n".join([r"npm error code EPERM", r"npm error path C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp\\native"])
        plain = "\n".join([r"npm error code EBUSY", r"npm error path C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native"])
        for text in (escaped, plain):
            self.assertTrue(zcode.locked_here(text, "muse-acp"))
            self.assertFalse(zcode.locked_here(text, "zcode-acp-server"))
        # npm's warnings are not failures: a rollback that could not remove a folder names no lock.
        warning = r"npm warn cleanup [ 'C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp', [Error: EPERM: operation not permitted, rmdir 'C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp\native'] ]"
        self.assertFalse(zcode.locked_here(warning + "\nnpm error code 1\nnpm error path C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp", "muse-acp"))
        self.assertFalse(zcode.locked_here("npm error code ENOENT\nnpm error path C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\@brokkai\\muse-acp", "muse-acp"))
        # A sibling folder with a longer name is not this package.
        self.assertFalse(zcode.locked_here(r"npm error code EPERM" + "\n" + r"npm error path C:\Users\sitti\AppData\Roaming\npm\node_modules\@brokkai\muse-acp-tools", "muse-acp"))

    def test_windows_postinstall_failure_with_a_rollback_warning_is_failed_not_in_use(self):
        # The 14:14 job: npm's rollback warned of EPERM on zcode's folder, but the failure was the postinstall.
        incident = "\n".join([
            r"npm warn cleanup Failed to remove some directories [",
            r"npm warn cleanup   [",
            r"npm warn cleanup     'C:\\Users\\sitti\\AppData\\Roaming\\npm\\node_modules\\zcode-acp-server',",
            r"npm warn cleanup     [Error: EPERM: operation not permitted, rmdir 'C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server\node_modules\zod\src'] {",
            r"npm warn cleanup       code: 'EPERM',",
            r"npm warn cleanup     }",
            r"npm warn cleanup   ]",
            r"npm warn cleanup ]",
            r"npm error code 1",
            r"npm error path C:\Users\sitti\AppData\Roaming\npm\node_modules\zcode-acp-server",
            r"npm error command failed",
            r"npm error command C:\WINDOWS\system32\cmd.exe /d /s /c node dist/remote/hub-upgrade-notify.js 2>/dev/null || true",
            r"npm error The system cannot find the path specified.",
        ])
        row, _ = self.windows_npm_run({"muse-acp": "0.10.0", "zcode-acp-server": "0.65.0"}, {"muse-acp": "0.10.1", "zcode-acp-server": "0.65.1"},
                                      outcomes={"zcode-acp-server": common.UpdateFailure("update_failed", incident)})
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("failed", "update_failed", True))
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.1"), ("zcode-acp-server", "0.65.0", "0.65.0")])
        self.assertTrue(all("inUse" not in part for part in row["parts"]))

    def test_ubuntu_partial_run_with_an_adapter_still_running_holds_the_unchanged_part(self):
        self.unit()
        acp = processes.Process(50, 1, 1, "/usr/bin/node", "50", ["node", str(self.node_modules() / "zcode-acp-server/dist/cli.js")])
        install = self.adapters(muse="0.10.0", acp="0.65.0")
        def run(argv, **kwargs):
            # The unit moves muse-acp only; zcode-acp-server stays put while its session runs.
            write_package(self.node_modules(), "@brokkai/muse-acp", "0.10.1")
        scans = iter([[], [acp]])
        with contextlib.ExitStack() as stack, self.registry(muse="0.10.1", acp="0.65.1"):
            patches = self.ubuntu_patches(run)
            patches[3] = mock.patch.object(zcode, "scan", side_effect=lambda host: next(scans))
            command = [stack.enter_context(patch) for patch in patches][2]
            row = zcode.update_adapters(install, self.deadline)
        command.assert_called_once()
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "in_use", True))
        self.assertEqual(row["parts"], [
            {"name": "muse-acp", "previousVersion": "0.10.0", "version": "0.10.1"},
            {"name": "zcode-acp-server", "previousVersion": "0.65.0", "version": "0.65.0", "inUse": True},
        ])

    def test_adapter_rows_always_carry_both_parts_and_null_top_level_versions(self):
        install = self.adapters(acp=None)
        with self.registry(muse="0.10.0", acp=None), mock.patch.object(zcode, "command") as run:
            row = zcode.update_adapters(install, self.deadline)
        self.assertEqual((row["status"], row["previousVersion"], row["version"]), ("current", None, None))
        self.assertEqual(self.parts(row), [("muse-acp", "0.10.0", "0.10.0"), ("zcode-acp-server", None, None)])
        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
