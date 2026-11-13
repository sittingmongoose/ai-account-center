"""Fix round 1: one regression per blocking review finding (B1-B13) and the cheap fixes.

Each test reproduces a reviewer's case from share/ai-account-center/2026-11-13-update-all-robustness
(REVIEW-*.md, FIX-ROUND-1.md). They run unchanged against 726e454b7, where each fails for the defect
it is named after, and pass at this branch's head. Fakes only: nothing is installed, updated or restarted.
"""

import base64
import contextlib
import getpass
import http.server
import json
import os
import pathlib
import shutil
import socket
import stat
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_common as common
import app_update_cursor as cursor
import app_update_antigravity as acp
import app_update_macos as mac
import app_update_t3_rpc as rpc
import app_update_zcode as zcode
from app_update_robustness_test import EVIDENCE, FakeWindows, Home, attach, evidence, log_name, scope, write_package

POSIX = os.name != "nt"
ROOT = POSIX and os.geteuid() == 0


def record_of(row):
    return row.get("diagnostics", {})


# ---------------------------------------------------------------------------------------------
# B1: credentials survive both scrubbers (Astra H1), and the scrubber is linear (Fable L1, L10, Argon L2).
# ---------------------------------------------------------------------------------------------
class B1Scrubber(unittest.TestCase):
    ASTRA = "npm error //registry.example.test/:_auth=dXNlcjpmaXh0dXJl\nCookie: SID=fixtureCookieOne; HSID=fixtureCookieTwo"

    def failed_command_tail(self, stderr):
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            if errors_sink is not None:
                errors_sink.append(stderr.encode())
            return 1, b""
        with scope() as record, mock.patch.object(common, "_run_bounded", side_effect=run):
            with self.assertRaises(common.UpdateFailure):
                common.command(["/usr/bin/npm", "install", "--global", "zcode-acp-server@latest"])
            row = attach(common.result("t3-acp-adapters", "mac", "failed", code="update_failed"), record)
        # Owner decision U13: the row keeps no output at all, only the tool and exit code.
        self.assertNotIn("tail", record_of(row))
        self.assertEqual((record_of(row)["tool"], record_of(row)["exitCode"]), ("npm", 1))
        return json.dumps(row)

    def test_registry_auth_and_whole_cookie_headers_never_reach_the_row(self):
        saved = self.failed_command_tail(self.ASTRA)
        for secret in ("dXNlcjpmaXh0dXJl", "fixtureCookieOne", "fixtureCookieTwo"):
            self.assertNotIn(secret, saved)

    def test_other_credential_shapes_go_and_ordinary_words_stay(self):
        cases = {
            "--password hunter2secret --user bob": "hunter2secret",
            "secret was wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY end": "wJalrXUtnFEMI",
            "code=4/0AfJohXabc-def_ghi state=xyz": "0AfJohX",
            "Set-Cookie: sid=abc123; Path=/; HttpOnly": "abc123",
            "Authorization: Bearer abcd": "abcd",
            "AKIAIOSFODNN7EXAMPLE": "AKIAIOSFODNN7EXAMPLE",
            r"c:\users\Jane Doe\AppData\Roaming\npm": "Doe",
            "ssh: connect to host: someone@macbook-pro": "someone",
        }
        for text, secret in cases.items():
            with self.subTest(text=text):
                self.assertNotIn(secret, common.scrub(text))
        kept = "npm error code E401 zcode-acp-server@latest @brokkai/muse-acp@0.21.0 node_modules/zcode-acp-server/dist/cli.js"
        self.assertEqual(common.scrub(kept), kept)

    def test_home_folder_in_another_letter_case_is_replaced(self):
        home = pathlib.Path(tempfile.gettempdir()) / "Home-Case"
        with mock.patch.object(pathlib.Path, "home", return_value=home):
            self.assertEqual(common.scrub(str(home).upper() + "/.npm/_logs"), "~/.npm/_logs")

    def test_a_long_unbroken_run_scrubs_in_linear_time(self):
        started = time.perf_counter()
        common.scrub("a" * 8192)
        common.scrub(("x" * 2000 + "\n") * 40)
        self.assertLess(time.perf_counter() - started, 1.0)


# ---------------------------------------------------------------------------------------------
# B2: Cursor says "updated" while the copy first on PATH stays old (Astra M1, Sol M1).
# ---------------------------------------------------------------------------------------------
@unittest.skipUnless(POSIX, "POSIX launcher fixtures")
class B2CursorShadow(Home):
    def test_a_copy_first_on_path_makes_the_row_say_so_never_updated(self):
        binary = self.root / ".local/share/cursor-agent/versions/2026.11.11-b912975/cursor-agent"
        binary.parent.mkdir(parents=True)
        binary.write_text("#!/bin/sh\necho 2026.11.11-b912975\n"); binary.chmod(0o755)
        link = self.root / ".local/bin/cursor-agent"
        link.parent.mkdir(parents=True); link.symlink_to(binary)
        old = self.root / "opt/bin/cursor-agent"
        old.parent.mkdir(parents=True); old.write_text("#!/bin/sh\necho 2026.01.01-old\n"); old.chmod(0o755)
        newer = binary.parent.parent / "2026.11.13-abcd" / "cursor-agent"
        def update(argv, *args, **kwargs):
            if [str(a) for a in argv][-1] == "update":
                newer.parent.mkdir(parents=True, exist_ok=True)
                newer.write_text("#!/bin/sh\necho 2026.11.13-abcd\n"); newer.chmod(0o755)
                link.unlink(); link.symlink_to(newer)
                return ""
            raise AssertionError("unexpected " + str(argv))
        with mock.patch.dict(os.environ, {"PATH": os.pathsep.join([str(old.parent), str(link.parent)])}), scope() as record:
            install = cursor.detect_cursor("ubuntu")
            with mock.patch.object(cursor, "command", side_effect=update):
                row = attach(cursor.update_cursor(install, time.monotonic() + 60), record)
        self.assertNotEqual(row["status"], "updated")
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("action_required", "cursor_shadowed", "2026.11.13-abcd"))
        self.assertEqual((record_of(row)["cause"], record_of(row)["owner"]["firstOnPath"]), ("shadowed", "~/opt/bin/cursor-agent"))


# ---------------------------------------------------------------------------------------------
# B3: another Cursor session's log decides the cause (Astra M3, Sol M4, Fable L11).
# ---------------------------------------------------------------------------------------------
class B3CursorLogs(Home):
    def layout(self):
        root = self.root / "AppData/Local/cursor-agent"
        (root / "versions/2026.11.11-b912975").mkdir(parents=True)
        for name in ("cursor-agent.ps1", "cursor-agent.cmd"):
            (root / name).write_text("launcher")
        for name in ("node.exe", "index.js"):
            (root / "versions/2026.11.11-b912975" / name).write_text("x")

    def run_with_log(self, stderr, folder_user, when):
        self.layout()
        logs = self.root / "tmp" / ("cursor-agent-logs-" + folder_user)
        def write_log():
            logs.mkdir(parents=True, exist_ok=True)
            (logs / log_name(when)).write_text(evidence("cursor-owner-pc-session.log"))
        fake = FakeWindows(self, policy_restricted=False, update_stderr=stderr, write_log=write_log)
        with mock.patch.object(tempfile, "gettempdir", return_value=str(self.root / "tmp")), \
                mock.patch.object(common, "_run_bounded", side_effect=fake.run), scope() as record:
            install = cursor.detect_cursor("windows")
            return attach(cursor.update_cursor(install, time.monotonic() + 60), record)

    def test_the_updates_own_error_wins_over_a_concurrent_sessions_401(self):
        row = self.run_with_log("Error: ENOSPC: no space left on device, write", getpass.getuser(), time.time())
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))
        self.assertNotEqual(record_of(row)["cause"], "auth_required")

    def test_another_users_log_folder_never_decides(self):
        row = self.run_with_log("", "someone-else", time.time())
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))
        self.assertIn("signin_undetermined", record_of(row).get("notes", []))

    def test_a_log_from_before_this_run_never_decides(self):
        row = self.run_with_log("", getpass.getuser(), time.time() - 3600)
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))


# ---------------------------------------------------------------------------------------------
# B4: the Mac GUI path drops stderr and the exit code (Fable M3).
# ---------------------------------------------------------------------------------------------
@unittest.skipUnless(POSIX, "launchd fixture paths")
class B4MacGui(Home):
    def test_a_401_from_the_gui_session_is_sign_in_needed_with_its_command_and_exit(self):
        private = self.root / "agent"
        @contextlib.contextmanager
        def temporary():
            private.mkdir(exist_ok=True)
            yield private
        def launchctl(argv, timeout=30, **kwargs):
            argv = [str(a) for a in argv]
            if argv[1] == "bootstrap":
                (private / "stderr").write_text(evidence("cursor-owner-pc-stderr.txt"))
                return ""
            if argv[1] == "print" and "." in argv[2]:
                return "state = not running\nlast exit code = 1\n"
            return ""
        binary = self.root / ".local/share/cursor-agent/versions/2026.11.11-b912975/cursor-agent"
        binary.parent.mkdir(parents=True); binary.write_text("x"); binary.chmod(0o755)
        link = self.root / ".local/bin/cursor-agent"
        link.parent.mkdir(parents=True); link.symlink_to(binary)
        with mock.patch.object(mac, "private_temporary", temporary), mock.patch.object(mac, "command", side_effect=launchctl), \
                mock.patch.object(mac.os, "getuid", return_value=501, create=True), scope() as record:
            install = cursor.detect_cursor("mac")
            row = attach(cursor.update_cursor(install, time.monotonic() + 60), record)
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "cursor_signin_needed"))
        self.assertEqual((record_of(row)["tool"], record_of(row)["exitCode"]), ("cursor-agent", 1))
        self.assertNotIn("tail", record_of(row))


# ---------------------------------------------------------------------------------------------
# B5, B6, B7, B11: adapters.
# ---------------------------------------------------------------------------------------------
class Adapters(Home):
    def consumers(self, *paths):
        settings = self.root / ".t3/userdata/settings.json"
        settings.parent.mkdir(parents=True, exist_ok=True)
        settings.write_text(json.dumps({"providerInstances": {
            "zcode-%d" % index: {"enabled": True, "config": {"commandPath": "node", "commandArgs": [str(path), "server"]}}
            for index, path in enumerate(paths)}}))

    def npm_prefix(self, prefix, version):
        root = self.root / prefix / "lib/node_modules"
        write_package(root, "zcode-acp-server", version)
        (root / "zcode-acp-server/dist").mkdir(parents=True, exist_ok=True)
        (root / "zcode-acp-server/dist/cli.js").write_text("cli")
        return root

    def update(self, host, run, latest="0.84.3"):
        node, cli = self.root / "node/bin/node", self.root / "node/lib/node_modules/npm/bin/npm-cli.js"
        with mock.patch.object(zcode, "registry_version", side_effect={"zcode-acp-server": latest}.get), \
                mock.patch.object(zcode, "resolve_npm", return_value=(node, cli)), \
                mock.patch.object(common, "_run_bounded", side_effect=run), mock.patch.object(zcode, "scan", return_value=[]), \
                mock.patch.object(zcode, "acp_unit_present", return_value=False), scope() as record:
            install = zcode.detect_adapters(host)
            return install, attach(zcode.update_adapters(install, time.monotonic() + 600), record)

    def npm_writes(self, version_for=None, runs=None):
        """A fake npm that installs where it is told (--prefix), the version it is told (or a mirror's)."""
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            argv = [str(a) for a in argv]
            if runs is not None:
                runs.append(argv)
            prefix = pathlib.Path(argv[argv.index("--prefix") + 1])
            package, requested = argv[-1].rsplit("@", 1)
            version = (version_for or {}).get(str(prefix), "0.84.3" if requested == "latest" else requested)
            write_package(prefix / "lib/node_modules", package, version)
            return 0, b""
        return run

    def test_b5_every_enabled_t3_consumer_of_an_adapter_is_updated(self):
        first = self.npm_prefix("opt/homebrew", "0.84.2")
        second = self.npm_prefix("second-node", "0.84.2")
        self.consumers(first / "zcode-acp-server/dist/cli.js", second / "zcode-acp-server/dist/cli.js")
        runs = []
        install, row = self.update("mac", self.npm_writes(runs=runs))
        self.assertEqual((zcode.package_version(first, "zcode-acp-server"), zcode.package_version(second, "zcode-acp-server")), ("0.84.3", "0.84.3"))
        self.assertEqual(row["status"], "updated")
        self.assertEqual(len(runs), 2)

    def test_b5_one_consumer_left_behind_is_never_updated(self):
        first = self.npm_prefix("opt/homebrew", "0.84.2")
        second = self.npm_prefix("second-node", "0.84.2")
        self.consumers(first / "zcode-acp-server/dist/cli.js", second / "zcode-acp-server/dist/cli.js")
        writes = self.npm_writes(version_for={str(self.root / "second-node"): "0.84.2"})
        install, row = self.update("mac", writes)
        self.assertNotEqual(row["status"], "updated")
        part = next(p for p in row["parts"] if p["name"] == "zcode-acp-server")
        self.assertEqual(part["version"], "0.84.2")

    def test_b6_a_mirror_serving_an_older_latest_is_never_an_update(self):
        homebrew = self.npm_prefix("opt/homebrew", "0.84.2")
        self.consumers(homebrew / "zcode-acp-server/dist/cli.js")
        runs = []
        writes = self.npm_writes(version_for={str(self.root / "opt/homebrew"): "0.83.0"}, runs=runs)
        install, row = self.update("mac", writes)
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "version_went_down"))
        self.assertTrue(runs[0][-1].endswith("@0.84.3"), runs[0])

    @unittest.skipUnless(POSIX, "pnpm's store uses symlinks")
    def test_b7_pnpm_installs_into_its_global_dir_never_into_its_store(self):
        global_dir = self.root / ".local/share/pnpm/global"
        modules = global_dir / "5/node_modules"
        def store(version):
            entry = modules / ".pnpm" / ("zcode-acp-server@" + version) / "node_modules"
            write_package(entry, "zcode-acp-server", version)
            (entry / "zcode-acp-server/dist").mkdir(parents=True, exist_ok=True)
            (entry / "zcode-acp-server/dist/cli.js").write_text("cli")
            link = modules / "zcode-acp-server"
            if link.is_symlink():
                link.unlink()
            link.symlink_to(entry / "zcode-acp-server")
        store("0.84.2")
        self.consumers(modules / "zcode-acp-server/dist/cli.js")
        tool = self.root / "tools/pnpm"
        tool.parent.mkdir(parents=True); tool.write_text("x"); tool.chmod(0o755)
        seen = []
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            argv = [str(a) for a in argv]
            seen.append((argv, env))
            target = pathlib.Path(argv[argv.index("--global-dir") + 1])
            if target == global_dir:  # real pnpm installs where it is told
                store(argv[-1].rsplit("@", 1)[1] if not argv[-1].endswith("@latest") else "0.84.3")
            return 0, b""
        with mock.patch.object(zcode.shutil, "which", side_effect=lambda name: str(tool) if name == "pnpm" else None):
            install, row = self.update("ubuntu", run)
        argv, env = seen[0]
        self.assertEqual(argv[argv.index("--global-dir") + 1], str(global_dir))
        self.assertEqual(row["status"], "updated")
        self.assertEqual(env.get("PNPM_HOME"), str(global_dir.parent))

    def test_b7_windows_pnpm_cmd_runs_through_node_never_cmd_exe(self):
        shim = self.root / "AppData/Roaming/npm/pnpm.cmd"
        entry = shim.parent / "node_modules/pnpm/bin/pnpm.cjs"
        entry.parent.mkdir(parents=True); entry.write_text("x"); shim.write_text("@echo off")
        node = self.root / "nodejs/node.exe"
        node.parent.mkdir(parents=True); node.write_text("x")
        owner = zcode.Owner(self.root / "pnpm/global/5/node_modules", self.root / "pnpm/global", "pnpm")
        with mock.patch.object(zcode.shutil, "which", side_effect=lambda name: str(shim) if name == "pnpm" else str(node) if name.startswith("node") else None):
            planned = zcode.install_command(owner, "zcode-acp-server", "windows", None, *(["0.84.3"] if "version" in zcode.install_command.__code__.co_varnames else []))
        self.assertEqual(planned[0][:2], [str(node), str(entry)])

    @unittest.skipIf(not POSIX or ROOT, "a read-only folder needs POSIX permissions and a non-root user")
    def test_b11_an_owner_folder_this_account_cannot_write_runs_nothing(self):
        homebrew = self.npm_prefix("opt/homebrew", "0.84.2")
        self.consumers(homebrew / "zcode-acp-server/dist/cli.js")
        homebrew.chmod(0o555)
        self.addCleanup(homebrew.chmod, 0o755)
        runs = []
        with mock.patch.object(zcode.os, "access", return_value=True):  # what Windows reports through an ACL
            install, row = self.update("windows", self.npm_writes(runs=runs))
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "prefix_not_writable"))
        self.assertEqual(runs, [])

    def test_b11_npm_eperm_creating_in_a_folder_nothing_uses_is_not_in_use(self):
        modules = self.root / "Program Files/nodejs/node_modules"
        write_package(modules, "zcode-acp-server", "0.84.2")
        self.consumers(modules / "zcode-acp-server/package.json")
        denied = "\n".join([r"npm error code EPERM", r"npm error syscall mkdir",
                            r"npm error path C:\Program Files\nodejs\node_modules\zcode-acp-server",
                            r"npm error Error: EPERM: operation not permitted, mkdir 'C:\Program Files\nodejs\node_modules\zcode-acp-server'"])
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            return 1, denied.encode()
        with mock.patch.object(zcode, "writable", return_value=True, create=True):
            install, row = self.update("windows", run)
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "prefix_not_writable"))


# ---------------------------------------------------------------------------------------------
# B8, B9, B10: Antigravity ACP.
# ---------------------------------------------------------------------------------------------
class AcpFake(Home):
    def runtime(self, host, version, native):
        with mock.patch.object(acp.platform, "machine", return_value="x86_64"):
            root = acp.managed_root(host)
        directory = root / "versions" / ("a" * 64)
        directory.mkdir(parents=True)
        record = {"releaseId": directory.name, "version": version}
        names = [("executable", "agy.exe" if host == "windows" else "antigravity")] if native else [
            ("executable", "agy_acp_server" + (".exe" if host == "windows" else ".par")),
            ("harness", "localharness_external" + (".exe" if host == "windows" else ""))]
        if native:
            record["harness"] = None
        for kind, name in names:
            (directory / name).write_bytes(b"bin"); (directory / name).chmod(0o755)
            record[kind] = {"name": name, "bytes": 3}
        common.write_private_json(directory / acp.RECORD, record)
        common.write_private_json(root / "active.json", {"releaseId": directory.name})
        return root

    def run_acp(self, host, installed, native, offer, newest, start_version=None, phase="succeeded"):
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(mock.patch.object(acp.platform, "machine", return_value="x86_64"))
        self.runtime(host, installed, native)
        install = acp.detect_antigravity_acp(host)
        self.calls, self.manifest_reads = [], []
        idle = {"driver": "antigravity", "operationId": None, "phase": "idle", "downloadedBytes": 0, "totalBytes": None,
                "version": offer, "installedVersion": installed, "canRemove": True, "message": None}
        owner = self
        class Client:
            def __init__(self, *args): self.started = False; self.settle = None
            def close(self): pass
            def send(self, message): pass
            def call(self, method, payload):
                owner.calls.append(method)
                if method == "server.getConfig":
                    return {"providers": [{"driver": "antigravity", "instanceId": "agy", "enabled": True, "setup": {"canInstall": True}}]}
                if method == "server.refreshProviders":
                    return {"providers": []}
                self.started = True
                return {**idle, "operationId": "op", "phase": "verifying", "version": start_version or offer}
            def request(self, method, payload): return "subscription"
            def receive(self, identifier):
                if not self.started:
                    yield idle
                else:
                    yield {**idle, "operationId": "op", "phase": phase, "version": start_version or offer}
        stack.enter_context(mock.patch.object(rpc, "wait_ready", create=True))
        stack.enter_context(mock.patch.object(rpc, "discover", return_value=(["t3"], self.root, "origin")))
        stack.enter_context(mock.patch.object(rpc, "auth_command", side_effect=lambda cli, base, args, timeout=30: json.dumps({"sessionId": "s", "token": "t"}) if args[0] == "issue" else "[]"))
        stack.enter_context(mock.patch.object(rpc, "revoke_session", return_value=True))
        stack.enter_context(mock.patch.object(rpc, "Rpc", Client))
        stack.enter_context(mock.patch.object(acp, "newest_cli_version", create=True,
                                              side_effect=lambda host, deadline: (owner.manifest_reads.append(host), newest)[1]))
        with scope() as record:
            return attach(acp.update_antigravity_acp(install, time.monotonic() + 600), record)

    def test_b8_a_stale_harness_offer_never_bypasses_the_sign_in_hold(self):
        row = self.run_acp("ubuntu", "1.4.5", False, offer="1.4.6", newest="1.5.8", start_version="1.5.8")
        self.assertNotIn("provider.install.start", self.calls)
        self.assertEqual((row["status"], row["messageCode"], row.get("heldVersion")), ("action_required", "antigravity_acp_signin_hold", "1.5.8"))

    def test_b8_an_unreadable_channel_holds_rather_than_risk_the_migration(self):
        row = self.run_acp("mac", "1.4.5", False, offer="1.4.6", newest=None)
        self.assertNotIn("provider.install.start", self.calls)
        self.assertEqual(row["messageCode"], "antigravity_acp_signin_hold")

    def test_b9_a_recheck_of_the_same_runtime_is_current_never_updated(self):
        row = self.run_acp("windows", "1.5.8", True, offer=None, newest="1.5.9", start_version="1.5.8")
        self.assertEqual((row["status"], row["version"]), ("current", "1.5.8"))
        self.assertNotIn("server.refreshProviders", self.calls)

    def test_l9_no_offer_and_no_channel_is_offer_unknown_and_still_installs_nothing(self):
        # Recheck 1 L9: the hold stays (nothing starts), but the words say the target could not be read.
        row = self.run_acp("ubuntu", "1.4.5", False, offer=None, newest=None)
        self.assertNotIn("provider.install.start", self.calls)
        self.assertEqual((row["status"], row["messageCode"]), ("unknown", "antigravity_acp_offer_unknown"))

    def test_b10_a_usable_t3_offer_needs_no_outside_lookup(self):
        self.run_acp("windows", "1.5.8", True, offer="1.5.8", newest="1.5.8")
        self.assertEqual(self.manifest_reads, [])


class Dribble(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'{"version":"1.5.8"}'
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        for byte in body:
            self.wfile.write(bytes([byte])); self.wfile.flush()
            time.sleep(0.3)

    def log_message(self, *args):
        pass


class B10Deadline(unittest.TestCase):
    def test_a_trickling_manifest_never_outlives_the_row_budget(self):
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Dribble)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        base = "http://127.0.0.1:%d/manifests/" % server.server_address[1]
        with mock.patch.object(acp, "CLI_MANIFEST_BASE", base, create=True), mock.patch.object(acp.platform, "machine", return_value="x86_64"):
            started = time.monotonic()
            acp.newest_cli_version("ubuntu", started + 1.0)
            elapsed = time.monotonic() - started
        self.assertLess(elapsed, 3.0)  # a 19-byte body at one byte every 0.3 s takes 5.7 s


# ---------------------------------------------------------------------------------------------
# B12: PowerShell's OEM code page.
# ---------------------------------------------------------------------------------------------
class B12PowerShell(unittest.TestCase):
    def test_every_powershell_script_asks_for_utf8_output_and_a_non_ascii_path_survives(self):
        seen = []
        def run(argv, timeout, env, capture, visible=False, merge_errors=False, cwd=None, kill_tree=False, errors_sink=None):
            script = base64.b64decode(argv[argv.index("-EncodedCommand") + 1]).decode("utf-16le")
            seen.append(script)
            return 0, json.dumps({"path": "C:\\Users\\Ren\u00e9\\AppData"}, ensure_ascii=False).encode("utf-8")
        with mock.patch.object(common, "_run_bounded", side_effect=run):
            output = common.powershell("ConvertTo-Json @{path=$env:LOCALAPPDATA} -Compress")
        self.assertTrue(seen[0].startswith("[Console]::OutputEncoding="), seen[0][:80])
        self.assertIn("UTF8", seen[0].split(";")[0])
        self.assertIn("Ren\u00e9", json.loads(output)["path"])


# ---------------------------------------------------------------------------------------------
# B13: the Cursor policy fallback.
# ---------------------------------------------------------------------------------------------
class B13CursorFallback(Home):
    def layout(self, folders=("2026.11.11-b912975",)):
        root = self.root / "AppData/Local/cursor-agent"
        for folder in folders:
            (root / "versions" / folder).mkdir(parents=True)
            for name in ("node.exe", "index.js"):
                (root / "versions" / folder / name).write_text("x")
        for name in ("cursor-agent.ps1", "cursor-agent.cmd"):
            (root / name).write_text("launcher")
        return root

    def group_policy(self, direct_code=0, direct_out=b"Already up to date\n"):
        fake = FakeWindows(self, policy_restricted=True)
        def run(argv, *args, **kwargs):
            argv = [str(a) for a in argv]
            if "-File" in argv:  # Group Policy ignores the process-scoped Bypass
                fake.runs.append(argv)
                return 1, evidence("cursor-alexis-policy.txt").encode()
            if argv[0].endswith("node.exe"):
                fake.runs.append(argv)
                return direct_code, direct_out
            return fake.run(argv, *args, **kwargs)
        return fake, run

    def test_a_fallback_that_worked_is_what_ran(self):
        self.layout()
        fake, run = self.group_policy()
        with mock.patch.object(common, "_run_bounded", side_effect=run), scope() as record:
            row = attach(cursor.update_cursor(cursor.detect_cursor("windows"), time.monotonic() + 60), record)
        self.assertEqual(row["status"], "current")
        self.assertEqual((record_of(row)["tool"], record_of(row)["exitCode"]), ("cursor-agent", 0))

    def test_group_policy_plus_a_failed_direct_run_is_policy_blocked(self):
        self.layout()
        fake, run = self.group_policy(1, b"Error: getaddrinfo ENOTFOUND api2.cursor.sh\n")
        with mock.patch.object(common, "_run_bounded", side_effect=run), scope() as record:
            row = attach(cursor.update_cursor(cursor.detect_cursor("windows"), time.monotonic() + 60), record)
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "policy_blocked"))
        # Classified from the output in memory; the output itself is not kept (owner decision U13).
        self.assertNotIn("tail", record_of(row))
        self.assertNotIn("ENOTFOUND", json.dumps(row))

    def test_same_day_folders_under_group_policy_still_give_a_version(self):
        root = self.layout(("2026.11.13-aaaaaaa", "2026.11.13-bbbbbbb"))
        later = root / "versions/2026.11.13-bbbbbbb"
        os.utime(later, (time.time() + 60, time.time() + 60))
        fake, run = self.group_policy()
        with mock.patch.object(common, "_run_bounded", side_effect=run):
            install = cursor.detect_cursor("windows")
        self.assertEqual((install.version, install.probe), ("2026.11.13-bbbbbbb", None))


# ---------------------------------------------------------------------------------------------
# The cheap fixes in the same round.
# ---------------------------------------------------------------------------------------------
class AlsoFixed(Home):
    def test_readiness_requires_the_new_server_to_name_itself(self):
        (self.root / ".t3/bin").mkdir(parents=True); (self.root / ".t3/bin/t3").touch()
        with socket.socket() as server:
            server.bind(("127.0.0.1", 0)); server.listen(4)
            state = self.root / ".t3/userdata/server-runtime.json"
            state.parent.mkdir(parents=True)
            state.write_text(json.dumps({"version": 1, "origin": "http://127.0.0.1:%d" % server.getsockname()[1], "port": server.getsockname()[1]}))
            clock = [0.0]
            with mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]), self.assertRaises(common.UpdateFailure) as raised:
                rpc.wait_ready("windows", 1000, "0.1.10-nightly.20261113.4616", seconds=5, sleep=lambda s: clock.__setitem__(0, clock[0] + s))
        self.assertEqual(raised.exception.code, "t3_not_ready")

    def test_model_refresh_continues_past_a_sign_in_and_ignores_instance_names(self):
        asked = []
        class Client:
            def call(self, method, payload):
                asked.append(payload["instanceId"])
                if payload["instanceId"] == "first":
                    raise common.UpdateFailure("t3_install_failed:rpc", output="ProviderSetupError\nAntigravity model refresh timed out. Try again or check Google sign-in.")
                return {"providers": []}
        self.assertEqual(acp.refresh_models(Client(), ["first", "second"])[0], "signin")
        self.assertEqual(asked, ["first", "second"])
        detail = {"_tag": "ProviderSetupError", "instanceId": "antigravity_login2", "operation": "refresh-models",
                  "detail": "The provider instance configuration is invalid."}
        class Wire:
            def __init__(self): self.sent = []
            def request(self, method, payload): return "1"
            def messages(self):
                return [{"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Failure", "cause": [{"_tag": "Fail", "error": detail}]}}]
        client = rpc.Rpc.__new__(rpc.Rpc)
        client.messages = Wire().messages
        client.send = lambda message: None
        client.serial = 0
        with self.assertRaises(common.UpdateFailure) as raised:
            next(client.receive("1"))
        self.assertNotIn("antigravity_login2", raised.exception.output)

    def test_unauthorized_access_exception_is_not_a_policy_refusal(self):
        error = common.UpdateFailure("update_failed", "System.UnauthorizedAccessException: Access to the path 'versions\\x' is denied.", 1)
        self.assertNotEqual(cursor.classify(error, time.time()).code, "policy_blocked")

    def test_windows_npm_prefers_the_prefixs_own_node_and_t3s_node(self):
        prefix, other = self.root / "nvm/nodejs", self.root / "Program Files/nodejs"
        for folder in (prefix, other):
            (folder / "node_modules/npm/bin").mkdir(parents=True)
            (folder / "node.exe").write_text("x"); (folder / "node_modules/npm/bin/npm-cli.js").write_text("x")
        with mock.patch.object(common.shutil, "which", side_effect=lambda name: str(other / "npm.cmd") if name == "npm.cmd" else None):
            self.assertEqual(common.resolve_npm(prefix)[0], prefix / "node.exe")
        owner = zcode.Owner(prefix / "node_modules", prefix, "npm", node=prefix / "node.exe")
        with mock.patch.object(zcode, "resolve_npm", return_value=(other / "node.exe", other / "node_modules/npm/bin/npm-cli.js")):
            self.assertEqual(zcode.npm_pair(owner, "windows")[0], (prefix / "node.exe").resolve())


class Recheck1(Home):
    """Recheck 1 (RECHECK-1.md): L5 the PowerShell preamble, L1 the Cursor log when output names no cause,
    L9 a held row whose target could not be read."""

    def test_l5_the_prelude_asks_for_utf8_without_a_byte_order_mark(self):
        emitted = []
        def run(argv, *args, **kwargs):
            emitted.append(base64.b64decode(argv[-1]).decode("utf-16le"))
            return 0, b""
        with mock.patch.object(common, "_run_bounded", side_effect=run):
            common.powershell("Write-Output 'fixture'")
        self.assertIn("New-Object System.Text.UTF8Encoding $false", emitted[0])
        self.assertNotIn("[System.Text.Encoding]::UTF8", emitted[0])

    def test_l5_a_leading_bom_never_reaches_the_json_parse(self):
        reply = "\ufeff" + json.dumps({"a": "\u00e9"}, ensure_ascii=False)
        with mock.patch.object(common, "_run_bounded", return_value=(0, reply.encode("utf-8"))):
            output = common.powershell("Write-Output (ConvertTo-Json @{a='x'})")
        self.assertFalse(output.startswith("\ufeff"))
        self.assertEqual(json.loads(output), {"a": "\u00e9"})

    def cursor_with_log(self, stderr):
        root = self.root / "AppData/Local/cursor-agent"
        (root / "versions/2026.11.11-b912975").mkdir(parents=True)
        for name in ("cursor-agent.ps1", "cursor-agent.cmd"):
            (root / name).write_text("launcher")
        for name in ("node.exe", "index.js"):
            (root / "versions/2026.11.11-b912975" / name).write_text("x")
        logs = self.root / "tmp" / ("cursor-agent-logs-" + getpass.getuser())
        def write_log():
            logs.mkdir(parents=True, exist_ok=True)
            (logs / log_name(time.time())).write_text(evidence("cursor-owner-pc-session.log"))
        fake = FakeWindows(self, policy_restricted=False, update_stderr=stderr, write_log=write_log)
        with mock.patch.object(tempfile, "gettempdir", return_value=str(self.root / "tmp")), \
                mock.patch.object(common, "_run_bounded", side_effect=fake.run), scope() as record:
            return attach(cursor.update_cursor(cursor.detect_cursor("windows"), time.monotonic() + 60), record)

    def test_l1_unbound_logs_leave_signin_undetermined(self):
        row = self.cursor_with_log("Checking for updates...\nUpdate failed: Error\n")
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))
        self.assertIn("signin_undetermined", record_of(row)["notes"])

    def test_l1_output_naming_its_own_cause_still_wins_over_a_401_log(self):
        row = self.cursor_with_log("Error: ENOSPC: no space left on device, write\n")
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "update_failed"))
        self.assertNotIn("tail", record_of(row))
        self.assertNotIn("ENOSPC", json.dumps(row))


class SignInLossReachesTheInstall(AcpFake):
    """Astra L3 / Sol L3: with a non-null native offer, the code under test reaches provider.install.start
    on main; the hold must stop it before the migration."""

    def test_a_native_offer_on_a_linux_runtime_pair_never_starts_the_install(self):
        row = self.run_acp("ubuntu", "1.4.5", False, offer="1.5.8", newest="1.5.8", start_version="1.5.8")
        self.assertNotIn("provider.install.start", self.calls)
        self.assertEqual(row["messageCode"], "antigravity_acp_signin_hold")


if __name__ == "__main__":
    unittest.main()
