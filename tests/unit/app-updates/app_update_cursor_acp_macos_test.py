"""Offline fixtures for Cursor, T3's ACP pair, and Mac login-session execution."""

import contextlib
import json
import os
import pathlib
import plistlib
import shutil
import stat
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
import app_update_macos as mac
import app_updates as updater


OLD, NEW = "2026.11.05-old", "2026.11.11-new"


class Fixtures(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="aac-new-update-fixture-")
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=self.root))
        self.stack.enter_context(mock.patch.dict(os.environ, {"LOCALAPPDATA": str(self.root / "Local")}))
        self.stack.enter_context(mock.patch.object(acp.platform, "machine", return_value="x86_64"))
        self.stack.enter_context(mock.patch.object(mac.os, "getuid", return_value=501, create=True))
        if os.name == "nt":
            # Simulated Unix runtimes need Unix executable bits, absent on NTFS.
            real_stat = pathlib.Path.stat
            def unix_mode(path, *args, **kwargs):
                value = real_stat(path, *args, **kwargs)
                if path.name in ("agy_acp_server.par", "localharness_external", "antigravity"):
                    fields = list(value)
                    fields[0] |= 0o111
                    return os.stat_result(fields)
                return value
            self.stack.enter_context(mock.patch.object(pathlib.Path, "stat", unix_mode))

    def cursor(self, host="ubuntu", version=OLD):
        path = cursor.cursor_launcher(host)
        path.parent.mkdir(parents=True, exist_ok=True)
        if host == "windows":
            path.touch()
        else:
            binary = self.root / ".local/share/cursor-agent/versions" / version / "cursor-agent"
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text("#!/bin/sh\necho " + version + "\n")
            binary.chmod(0o755)
            path.unlink(missing_ok=True)
            path.symlink_to(binary)
        return common.Install("cursor-cli", host, path, version)



class CursorTests(Fixtures):
    def test_detects_mac_from_official_release_link_without_accessing_keychain(self):
        install = self.cursor("mac")
        with mock.patch.object(cursor, "cli_probe") as probe:
            found = cursor.detect_cursor("mac")
        self.assertEqual((found.path, found.version, found.probe), (install.path, OLD, None))
        probe.assert_not_called()

    def test_windows_uses_powershell_launcher_for_probe_and_supported_update(self):
        install = self.cursor("windows")
        with mock.patch.object(cursor, "powershell", side_effect=[OLD, "", NEW]) as ps:
            found = cursor.detect_cursor("windows")
            row = cursor.update_cursor(found)
        self.assertEqual((row["previousVersion"], row["version"], row["status"]), (OLD, NEW, "updated"))
        self.assertIn(str(install.path), ps.call_args_list[1].args[0])
        self.assertIn(" update;", ps.call_args_list[1].args[0])
        self.assertNotIn("Remove-Item", str(ps.call_args_list))

    def test_windows_in_use_staging_is_action_required_on_success_and_failure(self):
        install = self.cursor("windows")
        staged = install.path.parent / "versions" / (NEW + ".new")
        staged.mkdir(parents=True)
        for error in (None, common.UpdateFailure()):
            with self.subTest(error=error), mock.patch.object(cursor, "windows_command", side_effect=error), \
                    mock.patch.object(cursor, "detect_cursor", return_value=install):
                row = cursor.update_cursor(install)
            self.assertEqual((row["status"], row["messageCode"], row["version"]),
                             ("action_required", "cursor_update_staged", OLD))
            self.assertTrue(staged.is_dir())

    def test_windows_prefers_ps1_and_accepts_the_older_cmd_only_layout(self):
        install = self.cursor("windows")
        self.assertEqual(install.path.suffix, ".cmd")
        ps1 = install.path.with_suffix(".ps1"); ps1.touch()
        self.assertEqual(cursor.cursor_launcher("windows"), ps1)

    def test_mac_update_runs_in_gui_and_reports_both_versions(self):
        install = self.cursor("mac")
        with mock.patch.object(cursor, "gui_command", side_effect=lambda *args: self.cursor("mac", NEW)) as gui:
            row = cursor.update_cursor(install, time.monotonic() + 60)
        self.assertEqual((row["status"], row["previousVersion"], row["version"]), ("updated", OLD, NEW))
        self.assertEqual(gui.call_args.args[0], "cursor-update")

    @unittest.skipIf(os.name == "nt", "executes Unix shebang fixtures")
    def test_linux_native_update_keeps_running_process_and_login_files(self):
        install = self.cursor()
        login = self.root / ".cursor/fixture-login"
        login.parent.mkdir(); login.write_text("unchanged")
        old = install.path.resolve()
        new = old.parent.parent / NEW / "cursor-agent"
        old.write_text("#!" + sys.executable + "\nimport sys,pathlib,os,time\n"
                       "p=pathlib.Path(" + repr(str(install.path)) + ")\n"
                       "if sys.argv[1]=='--version': print(" + repr(OLD) + ")\n"
                       "elif sys.argv[1]=='update':\n"
                       " n=pathlib.Path(" + repr(str(new)) + "); n.parent.mkdir(); n.write_text('#!/bin/sh\\necho " + NEW + "\\n'); n.chmod(0o755); p.unlink(); p.symlink_to(n)\n"
                       "else: time.sleep(60)\n")
        process = subprocess.Popen([str(old), "hold"])
        try:
            row = cursor.update_cursor(install)
            self.assertEqual((row["status"], row["previousVersion"], row["version"]), ("updated", OLD, NEW))
            self.assertIsNone(process.poll())
            self.assertTrue(old.is_file())
            self.assertEqual(login.read_text(), "unchanged")
        finally:
            process.terminate(); process.wait(timeout=5)

    def test_absent_unmanaged_and_failed_probes_do_not_claim_installed(self):
        self.assertIsNone(cursor.detect_cursor("ubuntu"))
        path = cursor.cursor_launcher("ubuntu"); path.parent.mkdir(parents=True); path.write_text("outside")
        self.assertEqual(cursor.detect_cursor("ubuntu").manager, "unsupported")
        self.cursor()
        with mock.patch.object(cursor, "cli_probe", return_value=(None, "timeout")):
            self.assertEqual(cursor.detect_cursor("ubuntu").probe, "timeout")

    def test_deadline_never_starts_updater(self):
        install = self.cursor()
        with mock.patch.object(cursor, "command") as run:
            row = cursor.update_cursor(install, time.monotonic() - 1)
        run.assert_not_called()
        self.assertFalse(row["updateAttempted"])
        self.assertEqual(row["messageCode"], "timeout")


class AcpTests(Fixtures):
    """T3 0.1.10-nightly.20261113.4616's managed Antigravity contract, offline.

    T3 offers its target (null until its release check ends), installs Google's
    newest native CLI on provider.install.start, refreshes models only for the
    instance an install ran on and only when it is enabled, and lists its
    disabled default `antigravity` instance first."""
    TOKEN = "private-fixture-bearer-never-output"
    SIGNIN = "Antigravity model refresh timed out. Try again or check Google sign-in."

    def install_record(self, host, version, native, release=None):
        root = acp.managed_root(host)
        directory = root / "versions" / (release or ("a" * 64))
        directory.mkdir(parents=True, exist_ok=True)
        record = {"releaseId": directory.name, "version": version}
        if native:
            names = (("executable", "agy.exe" if host == "windows" else "antigravity"),)
            record["harness"] = None
        else:
            names = (("executable", "agy_acp_server" + (".exe" if host == "windows" else ".par")),
                     ("harness", "localharness_external" + (".exe" if host == "windows" else "")))
        for kind, name in names:
            (directory / name).write_bytes(b"bin")
            (directory / name).chmod(0o755)
            record[kind] = {"name": name, "bytes": 3}
        common.write_private_json(directory / acp.RECORD, record)
        return directory, record

    def runtime(self, host, version="1.4.5", native=False):
        directory, _ = self.install_record(host, version, native)
        common.write_private_json(acp.managed_root(host) / "active.json", {"releaseId": directory.name})
        return acp.detect_antigravity_acp(host), directory

    def fake_t3(self, install, phase="succeeded", offer="1.5.8", failure=None, installed=None, newest=None,
                refresh=None, first_null=False, providers=None):
        import app_update_t3_rpc as rpc
        self.calls, self.auth, self.connections, self.ready = [], [], [], []
        self.failure = failure
        self.clock = [100]
        installed = installed or offer
        if failure in ("auth", "timeout"):
            self.stack.enter_context(mock.patch.object(rpc.time, "monotonic", side_effect=lambda: self.clock[0]))
        idle = {"driver": "antigravity", "operationId": None, "phase": "idle", "downloadedBytes": 0, "totalBytes": None,
                "version": offer, "installedVersion": install.version, "canRemove": True, "message": None}
        owner = self
        self.t3_writing = False
        # The enabled instance comes first by default, so a test reaches the behavior it is named for on any
        # helper (main installed on the first installable instance); the instance test lists the disabled one first.
        providers = providers if providers is not None else [
            {"driver": "codex", "instanceId": "wrong", "enabled": True, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": True, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "antigravity", "enabled": False, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": True, "setup": {"canInstall": True}},
        ]

        def activate():
            directory, _ = owner.install_record(install.platform, installed, acp.version_tuple(installed) >= (1, 5, 0), "b" * 64)
            owner.t3_writing = True
            try:
                common.write_private_json(install.path / "active.json", {"releaseId": directory.name})
            finally:
                owner.t3_writing = False

        def auth(argv, **kwargs):
            self.auth.append(argv)
            if "issue" in argv:
                if failure == "auth":
                    self.clock[0] += 30
                    raise common.UpdateFailure(output=self.TOKEN)
                return json.dumps({"sessionId": "fixture-session", "token": self.TOKEN})
            if "list" in argv:
                return "[]"
            return ""

        class Client:
            def __init__(self, origin, token, deadline):
                owner.connections.append(self)
                owner.assertEqual(token, owner.TOKEN)
                if failure == "unreachable":
                    raise OSError(owner.TOKEN)
                self.started = False
                self.settle = None
            def close(self): pass
            def call(self, method, payload):
                owner.calls.append((method, payload))
                if method == "server.getConfig":
                    return {"providers": providers}
                if method == "server.refreshProviders":
                    owner.assertEqual(payload["refreshModels"], True)
                    if refresh:
                        raise common.UpdateFailure("t3_install_failed:rpc", output="ProviderSetupError\n" + payload["instanceId"] + "\nrefresh-models\n" + refresh)
                    return {"providers": [{"instanceId": payload["instanceId"], "driver": "antigravity", "auth": {"status": "authenticated"}}]}
                owner.assertEqual(payload, {"instanceId": "agy-custom-id"})
                self.started = True
                if failure == "ambiguous":
                    raise OSError(owner.TOKEN)
                return {**idle, "operationId": "op", "phase": "downloading"}
            def request(self, method, payload):
                owner.calls.append((method, payload))
                owner.assertEqual(payload, {"instanceId": "agy-custom-id"})
                return "subscription"
            def receive(self, identifier):
                if not self.started:
                    if first_null:
                        yield {**idle, "version": None}
                    yield idle
                else:
                    if failure == "timeout":
                        owner.clock[0] += 30
                        raise TimeoutError(owner.TOKEN)
                    if failure == "denied":
                        raise common.UpdateFailure("t3_auth_failed", output=owner.TOKEN)
                    if phase == "succeeded": activate()
                    yield {**idle, "operationId": "op", "phase": phase, "message": owner.TOKEN}
            def send(self, message): pass

        self.stack.enter_context(mock.patch.object(rpc, "wait_ready", create=True, side_effect=lambda host, deadline, expected=None, **kwargs: owner.ready.append((expected, kwargs.get("seconds")))))
        self.stack.enter_context(mock.patch.object(rpc, "discover", return_value=(["fake-t3"], self.root, "fake-origin")))
        self.stack.enter_context(mock.patch.object(rpc, "auth_command", side_effect=lambda cli, base, args, timeout=30: auth([*cli, "auth", "session", *args], timeout=timeout)))
        self.stack.enter_context(mock.patch.object(rpc, "Rpc", Client))
        self.stack.enter_context(mock.patch.object(rpc, "revoke_session", side_effect=lambda cli, base, identifier, timeout=5: (auth([*cli, "auth", "session", "revoke", identifier]), True)[1]))
        self.stack.enter_context(mock.patch.object(acp, "newest_cli_version", create=True, return_value=newest))
        # Trap opens AND atomic replacements. Only the fake T3 writer is allowed.
        original_open, original_os_open, original_replace = pathlib.Path.open, os.open, os.replace
        def path_open(path, mode="r", *args, **kwargs):
            if path.name == "active.json" and any(flag in mode for flag in "wax+"):
                owner.assertTrue(owner.t3_writing, "AAC opened active.json for writing")
            return original_open(path, mode, *args, **kwargs)
        def os_open(path, flags, *args, **kwargs):
            if pathlib.Path(path).name.startswith("active.json") and flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT):
                owner.assertTrue(owner.t3_writing, "AAC created an active.json temp file")
            return original_os_open(path, flags, *args, **kwargs)
        def replace(source, target):
            if pathlib.Path(target).name == "active.json":
                owner.assertTrue(owner.t3_writing, "AAC replaced active.json")
            return original_replace(source, target)
        self.stack.enter_context(mock.patch.object(pathlib.Path, "open", path_open))
        self.stack.enter_context(mock.patch.object(os, "open", os_open))
        self.stack.enter_context(mock.patch.object(os, "replace", replace))

    def run_update(self, install, t3_row=None):
        import io
        out, err = io.StringIO(), io.StringIO()
        # Diagnostics are attached where the helper has them (the fail-before run against main has none).
        recorder = common.row_diagnostics() if hasattr(common, "row_diagnostics") else contextlib.nullcontext(None)
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), recorder as record:
            arguments = {"t3_row": t3_row} if t3_row is not None else {}
            row = acp.update_antigravity_acp(install, deadline=time.monotonic() + 20 if self.failure in ("auth", "timeout") else None, **arguments)
            row = common.with_diagnostics(row, record) if record is not None else row
        self.assertNotIn(self.TOKEN, out.getvalue() + err.getvalue() + json.dumps(row))
        return row

    def assert_revoked(self):
        self.assertEqual(sum("revoke" in argv for argv in self.auth), sum("issue" in argv for argv in self.auth))
        issue = next(argv for argv in self.auth if "issue" in argv)
        self.assertEqual([issue[i + 1] for i, arg in enumerate(issue) if arg == "--scope"],
                         ["providers:manage", "orchestration:read"])
        ttl = issue[issue.index("--ttl") + 1]
        self.assertGreater(float(ttl[:-1]), 0)
        self.assertLessEqual(float(ttl[:-1]), 900)
        self.assertNotIn(self.TOKEN, str(self.auth))

    def started(self):
        return sum(method == "provider.install.start" for method, _ in self.calls)

    def isolated(self):
        """Fresh patches per subtest."""
        @contextlib.contextmanager
        def scope():
            original = self.stack
            with contextlib.ExitStack() as nested:
                self.stack = nested
                try:
                    yield
                finally:
                    self.stack = original
        return scope()

    # --- record shapes (lead fix 3) -------------------------------------------------------------
    def test_detects_both_record_shapes_t3_writes(self):
        # Shapes captured 2026-11-13 (share/2026-11-13-antigravity-models): VM 1.4.5 pair and 1.5.8 CLI, Windows 1.5.8 CLI.
        for host, version, native in (("ubuntu", "1.4.5", False), ("ubuntu", "1.5.8", True), ("windows", "1.5.8", True),
                                      ("windows", "1.3.0", False), ("mac", "1.5.8", True)):
            with self.subTest(host=host, version=version), self.isolated():
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                found, _ = self.runtime(host, version, native)
                self.assertEqual((found.version, found.probe), (version, None))
                self.assertEqual(found.runtime_kind, "native" if native else "harness")

    def test_a_native_record_with_the_wrong_executable_or_size_is_unknown(self):
        found, directory = self.runtime("ubuntu", "1.5.8", native=True)
        record = acp.read_json(directory / acp.RECORD)
        for change in ({"executable": {"name": "agy.exe", "bytes": 3}}, {"executable": {"name": "antigravity", "bytes": 4}}):
            common.write_private_json(directory / acp.RECORD, {**record, **change})
            self.assertEqual(acp.detect_antigravity_acp("ubuntu").probe, "failed")

    # --- target and instance (lead fixes 1 and 2) ----------------------------------------------
    def test_installs_t3s_offer_on_an_enabled_instance_and_refreshes_every_enabled_one(self):
        install, old = self.runtime("windows", "1.3.0")
        # T3 lists its disabled default instance first.
        self.fake_t3(install, offer="1.5.8", providers=[
            {"driver": "codex", "instanceId": "wrong", "enabled": True, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "antigravity", "enabled": False, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": True, "setup": {"canInstall": True}},
            {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": True, "setup": {"canInstall": True}},
        ])
        row = self.run_update(install)
        self.assertEqual((row["status"], row["previousVersion"], row["version"], row["messageCode"]),
                         ("updated", "1.3.0", "1.5.8", "antigravity_acp_updated"))
        # T3 was not replaced in this run: one short readiness window, no version expectation.
        self.assertEqual(self.ready, [(None, acp.QUIET_READY_SECONDS)])
        # Never the disabled default instance T3 lists first: T3 would skip its model refresh.
        self.assertTrue(all(payload.get("instanceId") != "antigravity" for _, payload in self.calls))
        self.assertEqual([payload["instanceId"] for method, payload in self.calls if method == "server.refreshProviders"],
                         ["agy-custom-id", "agy-second-id"])
        self.assertEqual(self.started(), 1)
        self.assertEqual(acp.detect_antigravity_acp("windows").runtime_kind, "native")
        self.assertTrue(old.is_dir())
        self.assertEqual(row["diagnostics"]["owner"]["target"], "t3-offer")
        self.assert_revoked()

    def test_a_null_offer_waits_for_t3s_check_and_falls_back_to_the_cli_manifest(self):
        with self.isolated():
            install, _ = self.runtime("windows", "1.5.7", native=True)
            self.fake_t3(install, offer=None, installed="1.5.8", newest="1.5.8", first_null=True)
            row = self.run_update(install)
            self.assertEqual((row["status"], row["version"], row["diagnostics"]["owner"]["target"]), ("updated", "1.5.8", "cli-manifest"))
        with self.isolated():
            shutil.rmtree(self.root / ".t3")
            install, _ = self.runtime("windows", "1.5.7", native=True)
            self.fake_t3(install, offer=None, newest=None, first_null=True)
            row = self.run_update(install)
            self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["reason"]), ("unknown", "antigravity_acp_offer_unknown", "no_offer"))
            self.assertEqual(self.started(), 0)

    def test_current_or_newer_never_calls_install_because_t3_would_reinstall_the_same_cli(self):
        for installed, offer in (("1.5.8", "1.5.8"), ("1.5.9", "1.5.8")):
            with self.subTest(installed=installed), self.isolated():
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                install, _ = self.runtime("windows", installed, native=True)
                pointer = (install.path / "active.json").read_bytes()
                self.fake_t3(install, offer=offer)
                row = self.run_update(install)
                self.assertEqual((row["status"], row["messageCode"], row["version"]), ("current", "current", installed))
                self.assertEqual(self.started(), 0)
                self.assertEqual((install.path / "active.json").read_bytes(), pointer)
                self.assert_revoked()

    def test_no_enabled_instance_fails_before_any_install(self):
        install, _ = self.runtime("windows", "1.3.0")
        self.fake_t3(install, providers=[{"driver": "antigravity", "instanceId": "antigravity", "enabled": False, "setup": {"canInstall": True}}])
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["reason"]),
                         ("failed", "t3_install_failed:provider_unavailable", "no_enabled_instance"))
        self.assertEqual(self.started(), 0)

    # --- sign-in (lead fix 4) -----------------------------------------------------------------
    def test_linux_and_mac_hold_the_move_to_the_native_cli_and_windows_makes_it(self):
        for host in ("ubuntu", "mac", "windows"):
            with self.subTest(host=host), self.isolated():
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                install, _ = self.runtime(host, "1.4.5")
                pointer = (install.path / "active.json").read_bytes()
                self.fake_t3(install, offer="1.5.8")
                row = self.run_update(install)
                if host == "windows":
                    self.assertEqual((row["status"], row["version"]), ("updated", "1.5.8"))
                    continue
                self.assertEqual((row["status"], row["messageCode"], row["version"], row["heldVersion"], row["updateAttempted"]),
                                 ("action_required", "antigravity_acp_signin_hold", "1.4.5", "1.5.8", False))
                self.assertEqual((row["diagnostics"]["cause"], row["diagnostics"]["reason"]), ("signin_needed", "token_storage_change"))
                self.assertEqual(self.started(), 0)
                self.assertEqual((install.path / "active.json").read_bytes(), pointer)

    def test_a_refresh_that_asks_for_sign_in_reports_sign_in_needed_not_updated(self):
        # T3 4616's words when the new CLI cannot read the stored sign-in (VM, 2026-11-13).
        install, _ = self.runtime("ubuntu", "1.5.7", native=True)
        self.fake_t3(install, offer="1.5.8", refresh=self.SIGNIN)
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"], row["updateAttempted"]),
                         ("action_required", "antigravity_acp_signin_needed", "1.5.7", "1.5.8", True))
        self.assertEqual(row["diagnostics"]["cause"], "signin_needed")
        self.assertNotIn("Google", json.dumps(row))

    def test_a_refresh_that_fails_for_another_reason_keeps_updated_and_says_models_are_stale(self):
        install, _ = self.runtime("windows", "1.5.7", native=True)
        self.fake_t3(install, offer="1.5.8", refresh="The provider instance configuration is invalid.")
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"], row["version"]), ("updated", "antigravity_acp_models_stale", "1.5.8"))
        self.assertEqual(row["diagnostics"]["notes"], ["refresh_failed"])

    # --- ordering against T3 ------------------------------------------------------------------
    def test_a_t3_restart_still_pending_never_uses_the_previous_servers_offer(self):
        install, _ = self.runtime("ubuntu", "1.5.7", native=True)
        self.fake_t3(install)
        t3_row = {"appId": "t3-code", "platform": "ubuntu", "status": "updated", "updateAttempted": True,
                  "restartTargets": [{"kind": "systemd", "service": "t3code.service", "delaySeconds": 30}]}
        row = self.run_update(install, t3_row)
        self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["cause"]),
                         ("action_required", "t3_restart_pending", "t3_restart_pending"))
        self.assertEqual((self.ready, self.calls, self.auth), ([], [], []))

    def test_a_replaced_desktop_server_is_awaited_by_its_new_version(self):
        import app_update_t3_rpc as rpc
        install, _ = self.runtime("windows", "1.5.7", native=True)
        self.fake_t3(install)
        t3_row = {"appId": "t3-code", "platform": "windows", "status": "updated", "version": "0.1.10-nightly.20261113.4616",
                  "updateAttempted": True, "restartTargets": [{"kind": "desktop"}]}
        row = self.run_update(install, t3_row)
        self.assertEqual(self.ready, [("0.1.10-nightly.20261113.4616", rpc.READY_SECONDS)])
        self.assertEqual(row["status"], "updated")

    # --- session and operation safety (unchanged rules) ---------------------------------------
    def test_reconnected_subscription_checks_original_operation_and_never_restarts(self):
        import app_update_t3_rpc as rpc
        for changed in (False, True):
            with self.subTest(changed=changed), self.isolated():
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                install, _ = self.runtime("windows", "1.3.0")
                self.fake_t3(install)
                transport = rpc.Rpc
                clock = [100]
                connections = []
                class RenewingClient(transport):
                    def __init__(self, *args):
                        super().__init__(*args)
                        # A reconnected subscription sees the operation already running in T3.
                        self.started = any(other.started for other in connections)
                        connections.append(self)
                    def receive(self, identifier):
                        if self.started and len(connections) == 1:
                            clock[0] = 130
                            raise TimeoutError()
                        for state in super().receive(identifier):
                            if len(connections) > 1 and changed:
                                state = {**state, "operationId": "different"}
                            yield state
                self.stack.enter_context(mock.patch.object(rpc, "Rpc", RenewingClient))
                self.stack.enter_context(mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]))
                row = self.run_update(install)
                self.assertEqual(row["messageCode"], "t3_install_state_unknown" if changed else "antigravity_acp_updated")
                self.assertEqual(self.started(), 1)
                self.assertEqual(sum("revoke" in argv for argv in self.auth), 1)

    def test_failures_never_write_pointer_or_leak_token_and_always_revoke(self):
        for failure, phase, code in [("ambiguous", "succeeded", "t3_install_state_unknown"),
                                      (None, "failed", "t3_install_failed:installer"),
                                      (None, "cancelled", "t3_install_failed:cancelled"),
                                      ("timeout", "succeeded", "t3_install_timeout"),
                                      ("unreachable", "succeeded", "t3_unreachable"),
                                      ("denied", "succeeded", "t3_auth_failed")]:
            with self.subTest(failure=failure, phase=phase), self.isolated():
                shutil.rmtree(self.root / ".t3", ignore_errors=True)
                install, _ = self.runtime("windows", "1.3.0")
                pointer = (install.path / "active.json").read_bytes()
                self.fake_t3(install, phase=phase, failure=failure)
                row = self.run_update(install)
                self.assertEqual(row["messageCode"], code)
                self.assertEqual(row["status"], "action_required" if failure in ("unreachable", "denied") else "failed")
                self.assertEqual((install.path / "active.json").read_bytes(), pointer)
                if failure != "unreachable":
                    self.assert_revoked()

    def test_failed_revoke_is_reported_in_the_row_without_token(self):
        import app_update_t3_rpc as rpc
        install, _ = self.runtime("windows", "1.3.0")
        self.fake_t3(install)
        clock = [100]
        with mock.patch.object(rpc, "revoke_session", return_value=False), mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]), mock.patch.object(rpc.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0] + seconds)):
            row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]), ("failed", "t3_session_revoke_failed"))
        rpc._known_sessions.clear()

    def test_an_issue_failure_without_a_listed_session_is_issue_unknown_and_never_installs(self):
        install, _ = self.runtime("windows", "1.3.0")
        self.fake_t3(install, failure="auth")
        row = self.run_update(install)
        # The CLI failed (it did not answer with an empty reply): not a lost reply, and not "T3 reply lost".
        self.assertEqual((row["status"], row["messageCode"], row["diagnostics"]["cause"]), ("failed", "t3_session_issue_unknown", "unknown"))
        self.assertEqual(sum("issue" in argv for argv in self.auth), 1)
        self.assertTrue(any("list" in argv for argv in self.auth))
        self.assertEqual(self.started(), 0)

    def test_false_success_is_rejected(self):
        install, _ = self.runtime("windows", "1.3.0")
        self.fake_t3(install, offer="1.5.8", installed="1.4.5")
        row = self.run_update(install)
        self.assertEqual(row["messageCode"], "t3_install_failed:verification")
        self.assert_revoked()

    def test_unknown_pointer_is_unknown_and_absence_does_not_install(self):
        self.assertIsNone(acp.detect_antigravity_acp("ubuntu"))
        install, _ = self.runtime("ubuntu", "1.4.5")
        common.write_private_json(install.path / "active.json", {"releaseId": "../outside"})
        found = acp.detect_antigravity_acp("ubuntu")
        self.assertEqual((found.probe, found.probe_reason), ("failed", "runtime_record_invalid"))


class PrivateJsonTests(Fixtures):
    def test_replace_failure_removes_temp_and_preserves_original_exception(self):
        target = self.root / "private.json"
        failure = PermissionError("fixture locked destination")
        with mock.patch.object(os, "replace", side_effect=failure):
            with self.assertRaises(PermissionError) as raised:
                common.write_private_json(target, {"private": "fixture"})
        self.assertIs(raised.exception, failure)
        self.assertFalse(target.exists())
        self.assertEqual(list(self.root.glob("private.json.*.tmp")), [])

    def test_serialization_failure_also_removes_temp(self):
        target = self.root / "private.json"
        with self.assertRaises(ValueError): common.write_private_json(target, {"value": float("nan")})
        self.assertEqual(list(self.root.glob("*.tmp")), [])

    def test_cleanup_failure_preserves_the_replace_exception(self):
        target = self.root / "private.json"
        failure = PermissionError("fixture replace failed")
        with mock.patch.object(os, "replace", side_effect=failure), \
                mock.patch.object(pathlib.Path, "unlink", side_effect=OSError("fixture cleanup failed")):
            with self.assertRaises(PermissionError) as raised:
                common.write_private_json(target, {"private": "fixture"})
        self.assertIs(raised.exception, failure)


class MacGuiTests(Fixtures):
    def gui(self, failure=None, running=False):
        seen = []
        def run(argv, **kwargs):
            seen.append(argv)
            if argv[1] == "bootstrap":
                plist = plistlib.loads(pathlib.Path(argv[3]).read_bytes())
                self.plist = plist
                self.assertEqual(plist["EnvironmentVariables"]["MUSE_NO_AUTO_UPDATE"], "1")
                self.assertNotIn("HOME", plist["EnvironmentVariables"])
                self.assertNotIn("Keychain", str(plist))
                if failure:
                    pathlib.Path(plist["StandardErrorPath"]).write_text(failure)
            if argv[1] == "print" and "/com.ccs." in argv[2]:
                return "state = running\n" if running else "state = not running\nlast exit code = " + ("1" if failure else "0")
            return ""
        self.stack.enter_context(mock.patch.object(mac, "command", side_effect=run))
        return seen

    def test_muse_warm_is_fixed_gui_launchagent_and_is_removed(self):
        seen = self.gui()
        mac.gui_command("muse-warm", 5)
        self.assertEqual(self.plist["ProgramArguments"], [str(self.root / ".local/bin/muse"), "exec", "--contributor-mode", "off", mac.MUSE_WARM_PROMPT])
        self.assertEqual(seen[1][1:3], ["bootstrap", "gui/501"])
        self.assertEqual(seen[-1][1], "bootout")
        self.assertFalse(pathlib.Path(self.plist["WorkingDirectory"]).exists())

    def test_locked_keychain_is_distinguished_from_catalog_failure_and_cleanup_runs(self):
        seen = self.gui("Muse Code couldn't access your Meta credentials because macOS Keychain is locked or requires approval")
        with self.assertRaises(common.UpdateFailure) as caught:
            mac.gui_command("muse-warm", 5)
        self.assertEqual(caught.exception.code, "mac_keychain_locked")
        self.assertEqual(seen[-1][1], "bootout")

    def test_no_gui_session_is_actionable_and_does_not_bootstrap(self):
        with mock.patch.object(mac, "command", side_effect=common.UpdateFailure()) as run:
            with self.assertRaises(common.UpdateFailure) as caught:
                mac.gui_command("muse-warm", 5)
        self.assertEqual(caught.exception.code, "mac_gui_unavailable")
        self.assertEqual(run.call_count, 1)

    def test_session_loss_after_bootstrap_is_distinguished_from_service_failure(self):
        for lost in ("missing", "service", "permission"):
            seen = []
            def run(argv, **kwargs):
                seen.append(argv)
                if argv[1] == "print" and "/com.ccs." in argv[2]:
                    raise common.UpdateFailure()
                if argv[1] == "print" and len(seen) > 2 and lost != "service":
                    raise common.UpdateFailure(output="Could not find domain for" if lost == "missing" else "Permission denied")
                return ""
            with mock.patch.object(mac, "command", side_effect=run):
                with self.assertRaises(common.UpdateFailure) as caught:
                    mac.gui_command("muse-warm", 5)
            self.assertEqual(caught.exception.code, "mac_gui_unavailable" if lost == "missing" else "update_failed")
            self.assertEqual(seen[1][1], "bootstrap")
            self.assertTrue(any(argv[1] == "bootout" for argv in seen))

    def test_no_arbitrary_operation_can_be_launched(self):
        with mock.patch.object(mac, "command") as run:
            with self.assertRaises(common.UpdateFailure):
                mac.gui_command("caller-command", 5)
        run.assert_not_called()

    def test_timeout_boots_out_worker_without_changing_keychain(self):
        seen = self.gui(running=True)
        with mock.patch.object(mac.time, "monotonic", side_effect=[0, 0, 0, 0, 2, 2]):
            with self.assertRaises(common.UpdateFailure):
                mac.gui_command("muse-warm", 1)
        self.assertEqual(seen[-1][1], "bootout")

    def test_updater_warm_uses_gui_on_mac_and_preserves_specific_keychain_error(self):
        install = common.Install("muse-code", "mac", self.root / ".local/bin/muse", "1.4.15")
        install.path.parent.mkdir(parents=True); install.path.touch()
        with mock.patch.object(updater, "gui_command") as gui, \
                mock.patch.object(updater, "command") as direct, mock.patch.object(updater, "muse_catalog_fresh", return_value=True):
            self.assertTrue(updater.muse_warm_catalog(install, time.monotonic() + 60, time.time()))
        gui.assert_called_once(); direct.assert_not_called()
        with mock.patch.object(updater, "gui_command", side_effect=common.UpdateFailure("mac_keychain_locked")):
            with self.assertRaises(common.UpdateFailure) as caught:
                updater.muse_warm_catalog(install, time.monotonic() + 60, time.time())
        self.assertEqual(caught.exception.code, "mac_keychain_locked")

    def test_muse_result_preserves_keychain_error_after_changed_and_unchanged_installs(self):
        install = common.Install("muse-code", "mac", self.root / ".local/bin/muse", "1.4.14")
        install.path.parent.mkdir(parents=True); install.path.touch()
        for code, version, status in ((code, version, status)
                for code in ("mac_keychain_locked", "mac_gui_unavailable")
                for version, status in (("1.4.15", "restart_failed"), ("1.4.14", "failed"))):
            refreshed = common.Install("muse-code", "mac", install.path, version)
            with self.subTest(version=version), mock.patch.object(updater, "scan", return_value=[]), \
                    mock.patch.object(updater, "cli_contexts", return_value=([], [])), mock.patch.object(updater, "perform_cli_update"), \
                    mock.patch.object(updater, "detect_cli", return_value=refreshed), mock.patch.object(updater, "muse_update_busy", return_value=False), \
                    mock.patch.object(updater, "muse_catalog_fresh", return_value=False), \
                    mock.patch.object(updater, "gui_command", side_effect=common.UpdateFailure(code)), \
                    mock.patch.object(updater, "restart_cli", return_value=[]):
                row = updater.update_cli(install, time.monotonic() + 60)
            self.assertEqual((row["status"], row["messageCode"], row["version"]), (status, code, version))


if __name__ == "__main__":
    unittest.main()
