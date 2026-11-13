"""Regressions for the five rows that failed in Update all on 2026-11-13, from their scrubbed evidence.

Each test reproduces one root cause (share/ai-account-center/2026-11-13-update-all-robustness/ROOT-CAUSES.md)
with the inputs captured then: Cursor's 401 on the owner's Windows PC, the PowerShell execution policy on Alexis PC,
npm's wrong prefix on the Mac, and the lost T3 session reply under pythonw on both Windows PCs, plus the
Antigravity sign-in loss on Linux (share/ai-account-center/2026-11-13-antigravity-models/REPORT.md).
They also cover setups unlike the owner's: no PATH entry, a second copy, another package manager,
T3 missing and T3 not ready yet. Everything is offline: no real installer, T3 or network is touched.
"""

import base64
import contextlib
import io
import json
import os
import pathlib
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_common as common
import app_update_cursor as cursor
import app_update_antigravity as acp
import app_update_t3_rpc as rpc
import app_update_zcode as zcode

EVIDENCE = pathlib.Path(__file__).resolve().parent / "fixtures/update-all-2026-11-13"


def evidence(name):
    return (EVIDENCE / name).read_text(encoding="utf-8")


def scope():
    """The row's diagnostics recorder; an older helper (the fail-before run against main) has none."""
    return common.row_diagnostics() if hasattr(common, "row_diagnostics") else contextlib.nullcontext(None)


def attach(row, record):
    return common.with_diagnostics(row, record) if record is not None else row


class Home(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="aac-robust-")
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=self.root))
        self.stack.enter_context(mock.patch.dict(os.environ, {
            "LOCALAPPDATA": str(self.root / "AppData/Local"), "APPDATA": str(self.root / "AppData/Roaming"),
            "PATH": str(self.root / "empty-path")}))
        zcode.UBUNTU_RUN.clear()
        self.addCleanup(zcode.UBUNTU_RUN.clear)


# ---------------------------------------------------------------------------------------------
# Rows 1 and 2: Cursor CLI on Windows. A fake Windows: PowerShell refuses a script file unless the
# process runs with -ExecutionPolicy Bypass (Alexis: every scope Undefined, so Restricted), and
# Cursor's own update answers like the owner's Windows PC did (HTTP 401, [unauthenticated]).
# ---------------------------------------------------------------------------------------------
class FakeWindows:
    def __init__(self, test, policy_restricted, update_stderr=None, write_log=None):
        self.test, self.restricted = test, policy_restricted
        self.update_stderr, self.write_log = update_stderr, write_log
        self.runs = []

    def script_of(self, argv):
        if "-EncodedCommand" in argv:
            return base64.b64decode(argv[argv.index("-EncodedCommand") + 1]).decode("utf-16le")
        return " ".join(str(arg) for arg in argv)

    def run(self, argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
        argv = [str(arg) for arg in argv]
        script = self.script_of(argv)
        self.runs.append(argv)
        runs_ps1 = ".ps1" in script
        bypass = any(a.lower() == "-executionpolicy" for a in argv) and "Bypass" in argv
        def finish(code, out="", err=""):
            if errors_sink is not None and err:
                errors_sink.append(err.encode())
            data = (out + (err if merge_errors else "")).encode()
            return code, data
        if runs_ps1 and self.restricted and not bypass:
            return finish(1, err=evidence("cursor-alexis-policy.txt"))
        if script.rstrip().endswith("--version") or " --version" in script:
            return finish(0, "2026.11.11-b912975\n")
        if " update" in script or argv[-1] == "update":
            if self.update_stderr is not None:
                if self.write_log:
                    self.write_log()
                return finish(1, err=self.update_stderr)
            return finish(0, "Already up to date\n")
        return finish(0)


class CursorWindowsRegressions(Home):
    def layout(self):
        """Alexis PC's real %LOCALAPPDATA%\\cursor-agent layout (cursor-alexis-layout.json)."""
        spec = json.loads(evidence("cursor-alexis-layout.json"))
        root = self.root / "AppData/Local/cursor-agent"
        for name in spec["launchers"]:
            (root / name).parent.mkdir(parents=True, exist_ok=True)
            (root / name).write_text("launcher fixture\n")
        for version in spec["versions"]:
            for name in spec["versionFiles"]:
                (root / "versions" / version).mkdir(parents=True, exist_ok=True)
                (root / "versions" / version / name).write_text("fixture\n")
        (root / spec["promoteStaged"]).write_text("@echo off\n")
        return root

    def run_row(self, fake):
        with mock.patch.object(common, "_run_bounded", side_effect=fake.run), scope() as record:
            install = cursor.detect_cursor("windows")
            if install is None or install.probe:
                return install, None
            row = attach(cursor.update_cursor(install, time.monotonic() + 300), record)
        return install, row

    def test_alexis_restricted_policy_no_longer_blocks_detection_or_update(self):
        """Row 2. On main the .ps1 version probe ran through PowerShell without a process policy, was refused,
        and the row read 'Unknown: the readiness check could not run'. Now the version comes from the
        versions folder the launcher itself picks, and the update runs the way Cursor's own .cmd does."""
        self.layout()
        fake = FakeWindows(self, policy_restricted=True)
        install, row = self.run_row(fake)
        self.assertIsNotNone(install)
        self.assertEqual((install.version, install.probe), ("2026.11.11-b912975", None),
                         "the readiness probe was refused: the row would read 'Unknown: the readiness check could not run'")
        self.assertEqual((row["status"], row["version"]), ("current", "2026.11.11-b912975"))
        update = next(argv for argv in fake.runs if argv[-1] == "update")
        self.assertEqual(update[:7], ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                                      str(self.root / "AppData/Local/cursor-agent/cursor-agent.ps1")])
        self.assertEqual(row["diagnostics"]["command"], "cursor-agent update")

    def test_group_policy_that_overrides_bypass_is_reported_as_policy_blocked_after_a_direct_try(self):
        self.layout()
        fake = FakeWindows(self, policy_restricted=True)
        real = fake.run
        def enforced(argv, *args, **kwargs):
            argv = [str(arg) for arg in argv]
            if "-File" in argv:  # Group Policy ignores the process scope.
                fake.runs.append(argv)
                sink = kwargs.get("errors_sink")
                text = evidence("cursor-alexis-policy.txt")
                return 1, text.encode() if kwargs.get("merge_errors") or (len(args) > 4 and args[4]) else b""
            if argv[0].endswith("node.exe"):
                fake.runs.append(argv)
                return 0, b"updated\n"
            return real(argv, *args, **kwargs)
        fake.run = enforced
        install, row = self.run_row(fake)
        self.assertTrue(any(argv[0].endswith("node.exe") and argv[-1] == "update" for argv in fake.runs))
        self.assertEqual(row["status"], "current")
        self.assertIn("policy_fallback_direct", row["diagnostics"]["notes"])

    def test_owner_pc_401_is_sign_in_needed_not_update_failed(self):
        """Row 1. Cursor's updater was refused for its sign-in; main reported 'The app update failed'."""
        self.layout()
        fake = FakeWindows(self, policy_restricted=False, update_stderr=evidence("cursor-owner-pc-stderr.txt"))
        install, row = self.run_row(fake)
        self.assertEqual((row["status"], row["messageCode"], row["updateAttempted"]), ("action_required", "cursor_signin_needed", True))
        self.assertEqual((row["diagnostics"]["cause"], row["diagnostics"]["exitCode"]), ("auth_required", 1))
        self.assertIn("[unauthenticated]", row["diagnostics"]["tail"])
        # Every user folder in the row is the placeholder, never an account name.
        self.assertEqual(set(re.findall(r"Users(?:\\\\|\\|/)+([^\\/\"]+)", json.dumps(row))), {"<user>"})
        self.assertIn("<user>", row["diagnostics"]["tail"])

    def test_401_found_only_in_cursors_own_session_log_is_still_sign_in_needed(self):
        self.layout()
        logs = self.root / "tmp/cursor-agent-logs-user"
        def write_log():
            logs.mkdir(parents=True, exist_ok=True)
            (logs / "session-2026-11-13T04-55-21-130Z-30540-1.log").write_text(evidence("cursor-owner-pc-session.log"))
        fake = FakeWindows(self, policy_restricted=False, update_stderr="", write_log=write_log)
        with mock.patch.object(tempfile, "gettempdir", return_value=str(self.root / "tmp")):
            install, row = self.run_row(fake)
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "cursor_signin_needed"))


class DiagnosticsRecordTests(unittest.TestCase):
    @staticmethod
    def fake(code, stderr=b"", stdout=b"", sinks=None):
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            if sinks is not None:
                sinks.append(errors_sink)
            if errors_sink is not None and stderr:
                errors_sink.append(stderr)
            return code, stdout
        return run

    def test_a_failed_command_stays_the_rows_record_after_a_later_success(self):
        with common.row_diagnostics() as record:
            with mock.patch.object(common, "_run_bounded", side_effect=self.fake(1, b"npm error code E401\nnpm error Unable to authenticate, token=abc")):
                with self.assertRaises(common.UpdateFailure):
                    common.command(["/opt/homebrew/bin/npm", "install", "-g", "zcode-acp-server@latest"])
            with mock.patch.object(common, "_run_bounded", side_effect=self.fake(0, stdout=b"0.84.2\n")):
                common.command(["/opt/homebrew/bin/zcode-acp-server", "--version"], capture=True)
            exported = record.export({"status": "failed", "messageCode": "update_failed"})
        self.assertEqual((exported["cause"], exported["command"], exported["exitCode"]),
                         ("installer_failed", "npm install -g zcode-acp-server@latest", 1))
        self.assertIn("npm error code E401", exported["tail"])
        self.assertNotIn("abc", exported["tail"])

    def test_launches_that_hand_off_an_app_keep_stderr_on_the_null_device(self):
        sinks = []
        with mock.patch.object(common, "_run_bounded", side_effect=self.fake(0, sinks=sinks)):
            common.command(["/usr/bin/open", "-a", "Terminal"], stderr_tail=False)
            common.command(["/usr/bin/true"])
        self.assertEqual((sinks[0], sinks[1]), (None, []))

    @unittest.skipIf(os.name == "nt", "a POSIX shell child")
    def test_real_stderr_reaches_the_tail_through_a_temporary_file(self):
        with common.row_diagnostics() as record:
            with self.assertRaises(common.UpdateFailure) as raised:
                common.command(["/bin/sh", "-c", "echo out; echo 'npm error EACCES /home/someone/.npm' >&2; exit 243"])
            exported = record.export({"status": "failed", "messageCode": "update_failed"})
        self.assertEqual((raised.exception.exit_code, exported["exitCode"]), (243, 243))
        self.assertEqual(exported["tail"], "npm error EACCES /home/<user>/.npm")


class CursorOwnershipTests(Home):
    def linux_install(self, version="2026.11.11-b912975"):
        binary = self.root / ".local/share/cursor-agent/versions" / version / "cursor-agent"
        binary.parent.mkdir(parents=True)
        binary.write_text("#!/bin/sh\necho " + version + "\n")
        binary.chmod(0o755)
        link = self.root / ".local/bin/cursor-agent"
        link.parent.mkdir(parents=True)
        link.symlink_to(binary)
        return link

    @unittest.skipIf(os.name == "nt", "runs a POSIX launcher fixture")
    def test_no_path_entry_still_finds_the_managed_install_and_says_so(self):
        self.linux_install()
        found = cursor.detect_cursor("ubuntu")
        self.assertEqual((found.version, found.owner["onPath"], found.owner["executable"]), ("2026.11.11-b912975", False, "~/.local/bin/cursor-agent"))

    @unittest.skipIf(os.name == "nt", "runs a POSIX launcher fixture")
    def test_a_second_copy_first_on_path_is_named(self):
        self.linux_install()
        other = self.root / "opt/bin/cursor-agent"
        other.parent.mkdir(parents=True)
        other.write_text("#!/bin/sh\necho 2026.01.01-old\n"); other.chmod(0o755)
        with mock.patch.dict(os.environ, {"PATH": os.pathsep.join([str(other.parent), str(self.root / ".local/bin")])}):
            found = cursor.detect_cursor("ubuntu")
        self.assertEqual((found.owner["copies"], found.owner["firstOnPath"], found.owner["onPath"]), (2, "~/opt/bin/cursor-agent", True))

    def test_a_copy_from_another_installer_is_unsupported_not_absent(self):
        other = self.root / "homebrew/bin/cursor-agent"
        other.parent.mkdir(parents=True)
        other.write_text("x"); other.chmod(0o755)
        with mock.patch.dict(os.environ, {"PATH": str(other.parent)}):
            found = cursor.detect_cursor("ubuntu")
        self.assertEqual((found.manager, found.probe_reason, found.owner["manager"]), ("unsupported", "unmanaged_location", "other"))


# ---------------------------------------------------------------------------------------------
# Row 3: T3 ACP adapters on the Mac. npm "succeeded" into the standalone Node's own prefix while
# T3 runs the Homebrew prefix. The fake npm installs where real npm would: --prefix when given,
# else its node's own folder (npm's default global prefix).
# ---------------------------------------------------------------------------------------------
class AdapterOwnershipRegressions(Home):
    def mac(self):
        spec = json.loads(evidence("mac-adapters.json"))
        self.brew = self.root / "opt/homebrew"
        self.node_home = self.root / ".local/opt/node-v26.13.1-darwin-arm64"
        def place(text):
            return pathlib.Path(text.replace("{HOME}", str(self.root)).replace("/opt/homebrew", str(self.brew)))
        for folder, packages in spec["before"].items():
            for package, version in packages.items():
                write_package(place(folder), package, version)
        (self.node_home / "bin").mkdir(parents=True)
        (self.node_home / "bin/node").write_text("node fixture"); (self.node_home / "bin/node").chmod(0o755)
        npm_cli = self.node_home / "lib/node_modules/npm/bin/npm-cli.js"
        npm_cli.parent.mkdir(parents=True); npm_cli.write_text("npm fixture")
        (self.brew / "bin").mkdir(parents=True)
        for link, target in spec["symlinks"].items():
            path = place(link)
            path.symlink_to(target.replace("{HOME}", str(self.root)) if target.startswith("{HOME}") else target)
        dist = self.brew / "lib/node_modules/zcode-acp-server/dist"
        dist.mkdir(parents=True); (dist / "cli.js").write_text("cli")
        muse_bin = self.brew / "lib/node_modules/@brokkai/muse-acp/bin"
        muse_bin.mkdir(parents=True); (muse_bin / "muse-acp.cjs").write_text("cli")
        instance = json.loads(json.dumps(spec["t3Instance"]).replace("/opt/homebrew", str(self.brew)))
        settings = self.root / ".t3/userdata/settings.json"
        settings.parent.mkdir(parents=True)
        settings.write_text(json.dumps({"providerInstances": {"zcode": instance}}))
        script = place(spec["ownerScript"])
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text("#!/bin/sh\nnpm install -g @brokkai/muse-acp@latest zcode-acp-server@latest\n")
        script.chmod(0o755)
        self.latest = spec["registryLatest"]
        self.script = script
        self.npm_runs = []
        # The Mac's PATH has Homebrew's bin, where node and muse-acp resolve.
        self.stack.enter_context(mock.patch.dict(os.environ, {"PATH": str(self.brew / "bin")}))

    def npm(self, argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
        argv = [str(arg) for arg in argv]
        self.npm_runs.append(argv)
        if argv[0] == str(self.script):
            prefix = self.node_home  # plain `npm install -g`: npm's default prefix is its node's own folder
            packages = ["@brokkai/muse-acp", "zcode-acp-server"]
        else:
            self.assertIn("install", argv)
            prefix = pathlib.Path(argv[argv.index("--prefix") + 1])
            packages = [argv[-1].rsplit("@", 1)[0]]
        for package in packages:
            name = next(key for key, value in zcode.PACKAGES.items() if value == package)
            write_package(prefix / "lib/node_modules", package, self.latest[name])
        return 0, b""

    def update(self, host="mac"):
        with mock.patch.object(zcode, "adapter_root", return_value=self.brew / "lib/node_modules"), \
                mock.patch.object(zcode, "registry_version", side_effect=self.latest.get), \
                mock.patch.object(common, "_run_bounded", side_effect=self.npm), \
                mock.patch.object(zcode, "scan", return_value=[]), scope() as record:
            install = zcode.detect_adapters(host)
            return install, attach(zcode.update_adapters(install, time.monotonic() + 600), record)

    @unittest.skipIf(os.name == "nt", "the Mac fixture uses symlinks, which Windows creates only with Developer Mode")
    def test_mac_installs_into_the_prefix_t3_runs_and_verifies_that_copy(self):
        """Row 3. On main the owner's script installed zcode-acp-server 0.84.3 into the standalone Node's prefix,
        AAC read Homebrew's unchanged 0.84.2 and reported 'The app update failed'."""
        self.mac()
        install, row = self.update()
        self.assertEqual((row["status"], row["updateAttempted"]), ("updated", True))
        self.assertEqual([(p["name"], p["previousVersion"], p["version"]) for p in row["parts"]],
                         [("muse-acp", "0.21.0", "0.21.0"), ("zcode-acp-server", "0.84.2", "0.84.3")])
        install_run = next(argv for argv in self.npm_runs if "install" in argv)
        self.assertEqual(install_run[install_run.index("--prefix") + 1], str(self.brew))
        # T3's own node (the symlink it is configured with) runs npm, bound to the prefix T3 reads.
        self.assertEqual(install_run[:2], [str(self.node_home / "bin/node"), str(self.node_home / "lib/node_modules/npm/bin/npm-cli.js")])
        owner = row["diagnostics"]["owner"]["packages"]
        self.assertEqual({(p["package"], p["source"]) for p in owner}, {("zcode-acp-server", "t3-config"), ("muse-acp", "path")})
        self.assertNotIn(str(self.root), json.dumps(row))

    @unittest.skipIf(os.name == "nt", "the Mac fixture uses symlinks, which Windows creates only with Developer Mode")
    def test_an_installer_that_changes_another_copy_is_wrong_prefix_with_both_folders(self):
        """Partial progress is reported as it is: the copy T3 runs did not move, another copy did."""
        self.mac()
        def unit_into_wrong_prefix(argv, *args, **kwargs):
            self.npm_runs.append([str(a) for a in argv])
            write_package(self.node_home / "lib/node_modules", "zcode-acp-server", "0.84.3")
            return 0, b""
        self.npm = unit_into_wrong_prefix
        install, row = self.update()
        self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["cause"]), ("failed", "wrong_prefix", "wrong_prefix"))
        self.assertEqual([(p["name"], p["previousVersion"], p["version"]) for p in row["parts"]][1], ("zcode-acp-server", "0.84.2", "0.84.2"))
        self.assertIn({"package": "zcode-acp-server", "prefix": "~/.local/opt/node-v26.13.1-darwin-arm64", "version": "0.84.3"},
                      row["diagnostics"]["owner"]["elsewhere"])

    def test_alternate_managers_install_into_their_own_global_folder(self):
        for manager, root_parts in (("pnpm", ".local/share/pnpm/global/5/node_modules"), ("bun", ".bun/install/global/node_modules")):
            with self.subTest(manager=manager):
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                root = self.root / root_parts
                write_package(root, "zcode-acp-server", "0.84.2")
                (root / "zcode-acp-server/dist").mkdir(parents=True, exist_ok=True)
                (root / "zcode-acp-server/dist/cli.js").write_text("cli")
                settings = self.root / ".t3/userdata/settings.json"
                settings.parent.mkdir(parents=True, exist_ok=True)
                settings.write_text(json.dumps({"providerInstances": {"zcode": {"enabled": True, "config": {
                    "commandPath": "node", "commandArgs": [str(root / "zcode-acp-server/dist/cli.js"), "server"]}}}}))
                tool = self.root / "tools" / manager
                tool.parent.mkdir(parents=True, exist_ok=True); tool.write_text("x"); tool.chmod(0o755)
                runs = []
                def run(argv, timeout, env, *args, **kwargs):
                    runs.append(([str(a) for a in argv], env))
                    write_package(root, "zcode-acp-server", "0.84.3")
                    return 0, b""
                with mock.patch.object(zcode, "registry_version", side_effect={"zcode-acp-server": "0.84.3"}.get), \
                        mock.patch.object(common, "_run_bounded", side_effect=run), mock.patch.object(zcode, "scan", return_value=[]), \
                        mock.patch.object(zcode, "acp_unit_present", return_value=False), \
                        mock.patch.object(zcode.shutil, "which", side_effect=lambda name: str(tool) if name == manager else None):
                    install = zcode.detect_adapters("ubuntu")
                    row = zcode.update_adapters(install, time.monotonic() + 600)
                self.assertEqual(install.owners["zcode-acp-server"].manager, manager)
                self.assertEqual(row["status"], "updated")
                argv, env = runs[0]
                if manager == "pnpm":
                    self.assertEqual(argv, [str(tool), "add", "--global", "--global-dir", str(root.parent.parent), "zcode-acp-server@latest"])
                else:
                    self.assertEqual(argv, [str(tool), "add", "--global", "zcode-acp-server@latest"])
                    self.assertEqual(env["BUN_INSTALL_GLOBAL_DIR"], str(root.parent))
                shutil.rmtree(root.parents[2] if manager == "pnpm" else root.parents[2], ignore_errors=True)

    def test_a_plain_node_modules_folder_on_posix_has_no_installer_to_bind_and_runs_nothing(self):
        root = self.root / "vendor/node_modules"
        write_package(root, "zcode-acp-server", "0.84.2")
        with mock.patch.object(zcode, "adapter_root", return_value=root), \
                mock.patch.object(zcode, "registry_version", side_effect={"zcode-acp-server": "0.84.3"}.get), \
                mock.patch.object(common, "_run_bounded", side_effect=AssertionError("nothing may run")):
            install = zcode.detect_adapters("mac")
            row = zcode.update_adapters(install, time.monotonic() + 600)
        self.assertEqual(install.owners["zcode-acp-server"].manager, "other")
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "unsupported"))

    @unittest.skipIf(os.name == "nt", "the Mac fixture uses symlinks, which Windows creates only with Developer Mode")
    def test_an_owner_in_a_folder_this_user_cannot_write_needs_action_and_runs_nothing(self):
        self.mac()
        with mock.patch.object(zcode, "writable", return_value=False):
            install, row = self.update()
        self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["cause"]), ("action_required", "prefix_not_writable", "not_writable"))
        self.assertFalse(any("install" in argv for argv in self.npm_runs))


def write_package(root, package, version):
    (pathlib.Path(root) / package).mkdir(parents=True, exist_ok=True)
    (pathlib.Path(root) / package / "package.json").write_text(json.dumps({"name": package, "version": version}))


# ---------------------------------------------------------------------------------------------
# Rows 4 and 5: Antigravity ACP on both Windows PCs. The task's pythonw.exe is windowless, and a
# child of a windowless parent gets standard handles only when subprocess sets all three
# (STARTF_USESTDHANDLES). The issuer's nested call relied on inheritance, so T3's JSON reply was lost.
# ---------------------------------------------------------------------------------------------
WINDOWLESS_PRELUDE = r'''
import ctypes, os, subprocess, sys  # ctypes first: it reads os.name when imported
_real_call = subprocess.call
def _windowless_call(argv, stdin=None, stdout=None, stderr=None, **kwargs):
    # Windows from a GUI-subsystem parent: no STARTF_USESTDHANDLES unless every handle is explicit,
    # so the child's writes go nowhere (ROOT-CAUSES.md rows 4 and 5, pythonw probe).
    if stdin is None or stdout is None or stderr is None:
        return _real_call(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return _real_call(argv, stdin=stdin, stdout=stdout, stderr=stderr)
subprocess.call = _windowless_call
os.name = "nt"
'''


class LostSessionReplyRegressions(unittest.TestCase):
    def windowless_issue(self):
        reply = json.dumps(json.loads(evidence("t3-session-lost-reply.json"))["issueReply"])
        child = [sys.executable, "-c", "import sys; sys.stdout.write(sys.argv[1])", reply]
        process = subprocess.Popen([sys.executable, "-B", "-c", WINDOWLESS_PRELUDE + rpc._ISSUER_BOOTSTRAP, str(os.getpid()), *child],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        output, _ = process.communicate(b"1", timeout=30)
        return process.returncode, output.decode(), reply

    @unittest.skipUnless(sys.platform.startswith("linux"), "the bootstrap's Linux parent check runs here; Windows has its own test")
    def test_issuer_relays_t3s_reply_from_a_windowless_parent(self):
        """Rows 4 and 5. On main the reply arrived empty, AAC revoked the issued session and installed nothing."""
        code, output, reply = self.windowless_issue()
        self.assertEqual(code, 0)
        self.assertEqual(output, reply)

    @unittest.skipUnless(os.name == "nt" and pathlib.Path(sys.executable).with_name("pythonw.exe").is_file(), "needs pythonw.exe")
    def test_real_pythonw_issuer_captures_the_childs_reply(self):
        """The production context: the real Issuer under pythonw.exe, its result written to a file."""
        with tempfile.TemporaryDirectory() as folder:
            result = pathlib.Path(folder) / "result.txt"
            script = ("import sys, pathlib; sys.path.insert(0, %r); import app_update_t3_rpc as rpc; "
                      "i = rpc.Issuer(); p = i.start([sys.executable.replace('pythonw.exe', 'python.exe'), '-c', 'print(42)']); "
                      "out, _ = p.communicate(timeout=30); i.stop(); pathlib.Path(%r).write_bytes(out)") % (
                str(pathlib.Path(rpc.__file__).parent), str(result))
            subprocess.run([str(pathlib.Path(sys.executable).with_name("pythonw.exe")), "-c", script], timeout=60, check=True)
            self.assertEqual(result.read_text().strip(), "42")

    def test_a_reply_lost_after_issue_is_named_and_its_session_revoked(self):
        sessions, revoked = [], []
        def command(cli, base, args, timeout=30):
            if args[0] == "issue":
                sessions.append(args[args.index("--label") + 1])
                return ""  # what pythonw delivered on 2026-11-13
            return json.dumps([{"sessionId": "fixture-session", "client": {"label": sessions[0]}}])
        with mock.patch.object(rpc, "auth_command", side_effect=command), \
                mock.patch.object(rpc, "revoke_session", side_effect=lambda cli, base, identifier, timeout=5: revoked.append(identifier) or True), \
                self.assertRaises(common.UpdateFailure) as raised:
            with rpc.bearer(["t3"], pathlib.Path("/fixture"), time.monotonic() + 900):
                self.fail("no session token may be used after a lost reply")
        self.assertEqual(raised.exception.code, "t3_session_reply_lost")
        self.assertEqual((raised.exception.cause, raised.exception.reason), ("lost_session_reply", "reply_empty"))
        self.assertEqual((len(sessions), revoked), (1, ["fixture-session"]))  # never reissued


# ---------------------------------------------------------------------------------------------
# T3 missing and T3 not ready yet: bounded, safe waits before any session is issued.
# ---------------------------------------------------------------------------------------------
class T3ReadinessTests(Home):
    def state(self, port, version=None):
        path = self.root / ".t3/userdata/server-runtime.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"version": 1, "origin": "http://127.0.0.1:%d" % port, "port": port,
                                    **({"serverVersion": version} if version else {})}))

    def free_port(self):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            return probe.getsockname()[1]

    def test_t3_missing_is_absent_at_once(self):
        with self.assertRaises(common.UpdateFailure) as raised:
            rpc.wait_ready("ubuntu", time.monotonic() + 60, sleep=self.fail)
        self.assertEqual((raised.exception.code, raised.exception.cause), ("t3_absent", "t3_absent"))

    def test_t3_installed_but_not_answering_retries_with_backoff_then_is_not_ready(self):
        (self.root / ".t3/bin").mkdir(parents=True); (self.root / ".t3/bin/t3").touch()
        self.state(self.free_port())
        clock, waits = [0.0], []
        def sleep(seconds):
            waits.append(seconds); clock[0] += seconds
        with mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]), self.assertRaises(common.UpdateFailure) as raised:
            rpc.wait_ready("ubuntu", 1000, seconds=20, sleep=sleep)
        self.assertEqual((raised.exception.code, raised.exception.reason), ("t3_not_ready", "connection_refused"))
        self.assertEqual(waits[:4], [1.0, 2.0, 4.0, 8.0])
        self.assertLessEqual(sum(waits), 20)

    def test_the_previous_servers_endpoint_never_passes_after_t3_was_replaced(self):
        (self.root / ".t3/bin").mkdir(parents=True); (self.root / ".t3/bin/t3").touch()
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0)); server.listen(4)
            self.state(server.getsockname()[1], "0.1.10-nightly.20261112.4580")
            clock = [0.0]
            with mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]), self.assertRaises(common.UpdateFailure) as raised:
                rpc.wait_ready("windows", 1000, "0.1.10-nightly.20261113.4616", seconds=5, sleep=lambda s: clock.__setitem__(0, clock[0] + s))
            self.assertEqual(raised.exception.reason, "previous_server")
            # The new server publishes its own record: ready.
            self.state(server.getsockname()[1], "0.1.10-nightly.20261113.4616")
            self.assertEqual(rpc.wait_ready("windows", time.monotonic() + 10, "0.1.10-nightly.20261113.4616", seconds=5), 1)

    def test_a_server_that_comes_up_during_the_wait_is_ready(self):
        (self.root / ".t3/bin").mkdir(parents=True); (self.root / ".t3/bin/t3").touch()
        server = socket.socket(); self.addCleanup(server.close)
        server.bind(("127.0.0.1", 0))
        port = server.getsockname()[1]
        calls = []
        def sleep(seconds):
            calls.append(seconds)
            if len(calls) == 1:
                self.state(port)
            if len(calls) == 2:
                server.listen(4)
        attempts = rpc.wait_ready("ubuntu", time.monotonic() + 60, seconds=30, sleep=sleep)
        self.assertEqual(attempts, 3)


# ---------------------------------------------------------------------------------------------
# The Antigravity sign-in case (pm7-build, 2026-11-13): headless Linux on the 1.4.5 runtime pair,
# T3 4616 offering the 1.5.8 native CLI, which cannot read the stored sign-in.
# ---------------------------------------------------------------------------------------------
class AntigravitySignInRegression(Home):
    @unittest.skipIf(os.name == "nt", "a Linux runtime record needs Unix executable bits")
    def test_vm_records_hold_the_move_to_the_native_cli_and_install_nothing(self):
        records = json.loads(evidence("antigravity-vm-records.json"))["records"]
        old = next(record for record in records if record["version"] == "1.4.5")
        with mock.patch.object(acp.platform, "machine", return_value="x86_64"):
            root = acp.managed_root("ubuntu")
            directory = root / "versions" / old["releaseId"]
            directory.mkdir(parents=True)
            for kind in ("executable", "harness"):
                path = directory / old[kind]["name"]
                with path.open("wb") as stream:
                    stream.truncate(old[kind]["bytes"])
                path.chmod(0o755)
            common.write_private_json(directory / acp.RECORD, old)
            common.write_private_json(root / "active.json", {"releaseId": old["releaseId"]})
            install = acp.detect_antigravity_acp("ubuntu")
            calls = []
            class Client:
                def __init__(self, *args): self.started = False; self.settle = None
                def close(self): pass
                def send(self, message): pass
                def call(self, method, payload):
                    calls.append(method)
                    if method == "server.getConfig":
                        return {"providers": [{"driver": "antigravity", "instanceId": "antigravity", "enabled": False, "setup": {"canInstall": True}},
                                              {"driver": "antigravity", "instanceId": "agy-instance", "enabled": True, "setup": {"canInstall": True}}]}
                    raise AssertionError("no install across the sign-in change: " + method)
                def request(self, method, payload): return "subscription"
                def receive(self, identifier):
                    base = {"driver": "antigravity", "operationId": None, "phase": "idle", "downloadedBytes": 0, "totalBytes": None,
                            "installedVersion": "1.4.5", "canRemove": True, "message": None}
                    yield {**base, "version": None}  # T3 4616's first state, before its release check
                    yield {**base, "version": "1.5.8"}
            with mock.patch.object(rpc, "wait_ready", create=True), \
                    mock.patch.object(rpc, "discover", return_value=(["t3"], self.root, "origin")), \
                    mock.patch.object(rpc, "auth_command", side_effect=lambda cli, base, args, timeout=30: json.dumps({"sessionId": "s", "token": "t"}) if args[0] == "issue" else "[]"), \
                    mock.patch.object(rpc, "revoke_session", return_value=True), mock.patch.object(rpc, "Rpc", Client), \
                    mock.patch.object(acp, "newest_cli_version", return_value="1.5.8", create=True):
                row = acp.update_antigravity_acp(install, time.monotonic() + 600)
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("action_required", "antigravity_acp_signin_hold", "1.4.5"))
        self.assertNotIn("provider.install.start", calls)


if __name__ == "__main__":
    unittest.main()
