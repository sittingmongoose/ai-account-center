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
                if path.name in ("agy_acp_server.par", "localharness_external"):
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

    def old_acp(self, host):
        root = acp.managed_root(host)
        old = root / "versions" / ("a" * 64)
        old.mkdir(parents=True, exist_ok=True)
        record = {"releaseId": old.name, "version": "1.3.0"}
        for kind, name in (("executable", "agy_acp_server" + (".exe" if host == "windows" else ".par")),
                           ("harness", "localharness_external" + (".exe" if host == "windows" else ""))):
            (old / name).write_bytes(b"old")
            (old / name).chmod(0o755)
            record[kind] = {"name": name, "bytes": 3}
        common.write_private_json(old / acp.RECORD, record)
        common.write_private_json(root / "active.json", {"releaseId": old.name})
        return acp.detect_antigravity_acp(host), old



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
    TOKEN = "private-fixture-bearer-never-output"

    def fake_t3(self, install, phase="succeeded", pin="1.4.5", failure=None, installed="1.4.5"):
        import app_update_t3_rpc as rpc
        self.calls, self.auth = [], []
        state = {"driver": "antigravity", "operationId": "op", "phase": "idle", "version": pin}
        owner = self
        old = install.path / "versions" / ("a" * 64)
        self.t3_writing = False

        def activate():
            directory = old.with_name("b" * 64)
            directory.mkdir(exist_ok=True)
            record = acp.read_json(old / acp.RECORD)
            record.update(releaseId=directory.name, version=installed)
            for kind in ("executable", "harness"):
                shutil.copyfile(old / record[kind]["name"], directory / record[kind]["name"])
            owner.t3_writing = True
            try:
                common.write_private_json(directory / acp.RECORD, record)
                common.write_private_json(install.path / "active.json", {"releaseId": directory.name})
            finally:
                owner.t3_writing = False

        def auth(argv, **kwargs):
            self.auth.append(argv)
            if "issue" in argv:
                if failure == "auth":
                    raise common.UpdateFailure(output=self.TOKEN)
                return json.dumps({"sessionId": "fixture-session", "token": self.TOKEN})
            if "list" in argv:
                return "[]"
            return ""

        class Client:
            def __init__(self, origin, token, deadline):
                owner.assertEqual(token, owner.TOKEN)
                if failure == "unreachable":
                    raise OSError(owner.TOKEN)
                self.started = False
            def close(self): pass
            def call(self, method, payload):
                owner.calls.append((method, payload))
                if method == "server.getConfig":
                    return {"providers": [{"driver": "codex", "instanceId": "wrong", "setup": {"canInstall": True}},
                                          {"driver": "antigravity", "instanceId": "agy-custom-id", "setup": {"canInstall": True}}]}
                owner.assertEqual(payload, {"instanceId": "agy-custom-id"})
                self.started = True
                return {**state, "phase": "downloading"}
            def request(self, method, payload):
                owner.calls.append((method, payload))
                return "subscription"
            def receive(self, identifier):
                if not self.started:
                    yield state
                else:
                    if failure == "timeout":
                        raise TimeoutError(owner.TOKEN)
                    if failure == "denied":
                        raise common.UpdateFailure("t3_auth_failed", output=owner.TOKEN)
                    if phase == "succeeded": activate()
                    yield {**state, "phase": phase, "message": owner.TOKEN}
            def send(self, message): pass

        self.stack.enter_context(mock.patch.object(rpc, "discover", return_value=(["fake-t3"], self.root, "fake-origin")))
        self.stack.enter_context(mock.patch.object(rpc, "command", side_effect=auth))
        self.stack.enter_context(mock.patch.object(rpc, "Rpc", Client))
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

    def run_update(self, install):
        import io
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            row = acp.update_antigravity_acp(install)
        self.assertNotIn(self.TOKEN, out.getvalue() + err.getvalue() + json.dumps(row))
        return row

    def assert_revoked(self):
        self.assertEqual(sum("revoke" in argv for argv in self.auth), 1)
        issue = next(argv for argv in self.auth if "issue" in argv)
        self.assertEqual([issue[i + 1] for i, arg in enumerate(issue) if arg == "--scope"],
                         ["providers:manage", "orchestration:read"])
        self.assertIn("20m", issue)
        self.assertNotIn(self.TOKEN, str(self.auth))

    def test_success_all_platforms_t3_is_the_only_pointer_writer(self):
        for host in ("windows", "mac", "ubuntu"):
            with self.subTest(host=host), contextlib.ExitStack() as nested:
                # Keep patches isolated between platform subtests.
                original = self.stack
                self.stack = nested
                try:
                    install, old = self.old_acp(host)
                    self.fake_t3(install)
                    row = self.run_update(install)
                    self.assertEqual((row["status"], row["version"], row["messageCode"]),
                                     ("updated", "1.4.5", "antigravity_acp_updated"))
                    self.assertTrue(old.is_dir())
                    self.assertEqual(row["restartedProcesses"], 0)
                    self.assert_revoked()
                finally: self.stack = original

    def test_current_and_newer_never_call_install(self):
        for version in ("1.4.5", "1.5.0"):
            with self.subTest(version=version), contextlib.ExitStack() as nested:
                original = self.stack; self.stack = nested
                try:
                    install, old = self.old_acp("windows")
                    record = acp.read_json(old / acp.RECORD); record["version"] = version
                    common.write_private_json(old / acp.RECORD, record)
                    pointer = (install.path / "active.json").read_bytes()
                    self.fake_t3(install)
                    row = self.run_update(install)
                    self.assertEqual(row["status"], "current")
                    self.assertEqual(row["messageCode"], "current" if version == "1.4.5" else "antigravity_acp_newer_kept")
                    self.assertFalse(any(method == "provider.install.start" for method, _ in self.calls))
                    self.assertEqual((install.path / "active.json").read_bytes(), pointer)
                    self.assert_revoked()
                finally: self.stack = original

    def test_live_server_pin_can_be_newer_than_aac(self):
        install, old = self.old_acp("windows")
        record = acp.read_json(old / acp.RECORD); record["version"] = "1.4.5"
        common.write_private_json(old / acp.RECORD, record)
        self.fake_t3(install, pin="1.5.0", installed="1.5.0")
        row = self.run_update(install)
        self.assertEqual((row["status"], row["version"]), ("updated", "1.5.0"))
        self.assert_revoked()

    def test_failures_never_write_pointer_or_leak_token_and_always_revoke(self):
        for failure, phase, code in [(None, "failed", "t3_install_failed:installer"),
                                      (None, "cancelled", "t3_install_failed:cancelled"),
                                      ("timeout", "succeeded", "t3_install_timeout"),
                                      ("unreachable", "succeeded", "t3_unreachable"),
                                      ("denied", "succeeded", "t3_auth_failed")]:
            with self.subTest(failure=failure, phase=phase), contextlib.ExitStack() as nested:
                original = self.stack; self.stack = nested
                try:
                    install, _ = self.old_acp("windows")
                    pointer = (install.path / "active.json").read_bytes()
                    self.fake_t3(install, phase=phase, failure=failure)
                    row = self.run_update(install)
                    self.assertEqual(row["messageCode"], code)
                    self.assertEqual(row["status"], "action_required" if failure in ("unreachable", "denied") else "failed")
                    self.assertEqual((install.path / "active.json").read_bytes(), pointer)
                    self.assert_revoked()
                finally: self.stack = original

    def test_auth_issue_failure_has_no_token_to_revoke(self):
        install, _ = self.old_acp("windows")
        self.fake_t3(install, failure="auth")
        row = self.run_update(install)
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "t3_auth_failed"))
        self.assertTrue(any("list" in argv for argv in self.auth))
        self.assertFalse(any(method == "provider.install.start" for method, _ in self.calls))

    def test_false_success_is_rejected(self):
        install, _ = self.old_acp("windows")
        self.fake_t3(install, installed="1.3.0")
        row = self.run_update(install)
        self.assertEqual(row["messageCode"], "t3_install_failed:verification")
        self.assert_revoked()

    def test_newer_t3_activation_is_preserved_at_completion(self):
        install, _ = self.old_acp("windows")
        self.fake_t3(install, installed="1.5.0")
        row = self.run_update(install)
        self.assertEqual((row["status"], row["version"]), ("updated", "1.5.0"))
        self.assertEqual(acp.detect_antigravity_acp("windows").version, "1.5.0")
        self.assert_revoked()

    def test_older_server_cannot_trigger_a_downgrade(self):
        install, _ = self.old_acp("windows")
        pointer = (install.path / "active.json").read_bytes()
        self.fake_t3(install, pin="1.3.0")
        row = self.run_update(install)
        self.assertEqual(row["messageCode"], "t3_install_failed:server_outdated")
        self.assertFalse(any(method == "provider.install.start" for method, _ in self.calls))
        self.assertEqual((install.path / "active.json").read_bytes(), pointer)
        self.assert_revoked()

    def test_unknown_pointer_is_unknown_and_absence_does_not_install(self):
        self.assertIsNone(acp.detect_antigravity_acp("ubuntu"))
        install, _ = self.old_acp("ubuntu")
        common.write_private_json(install.path / "active.json", {"releaseId": "../outside"})
        self.assertEqual(acp.detect_antigravity_acp("ubuntu").probe, "failed")


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
