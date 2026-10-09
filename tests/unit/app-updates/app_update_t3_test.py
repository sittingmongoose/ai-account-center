"""Offline T3 and Windows Muse fixtures; no installed app or service touched."""

import base64
import contextlib
import io
import json
import os
import pathlib
import plistlib
import re
import shutil
import stat
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
import app_updates as updater

OLD = "0.0.46-nightly.20261006.2735"
NEW = "0.0.46-nightly.20261007.2774"


def named(number):
    # Runtime directory names for the prune layouts; only the last number differs.
    return "0.0.46-nightly.20261007." + str(number)


def bundle(path, version):
    (path / "Contents").mkdir(parents=True)
    with (path / "Contents/Info.plist").open("wb") as stream:
        plistlib.dump({"CFBundleIdentifier": t3.MAC_ID, "CFBundleShortVersionString": version}, stream)


def asar(executable, version, name="t3code"):
    executable.parent.mkdir(parents=True, exist_ok=True)
    executable.touch()
    package = json.dumps({"name": name, "version": version}).encode()
    header = json.dumps({"files": {"package.json": {"size": len(package), "offset": "0"}}}).encode()
    padded = header + b"\0" * ((4 - len(header) % 4) % 4)
    resources = executable.parent / "resources"
    resources.mkdir(exist_ok=True)
    (resources / "app.asar").write_bytes(struct.pack("<4I", 4, 8 + len(padded), 4 + len(padded), len(header)) + padded + package)


def systemctl(argv, reload="no"):
    # Fixed answers for the unit queries; any other call changes nothing in this fixture.
    return reload if "--property=NeedDaemonReload" in argv else ""


class T3Fixtures(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(pathlib.Path, "home", return_value=self.root).start()
        mock.patch.dict(os.environ, {"CCS_HOME": str(self.root / "state"), "LOCALAPPDATA": str(self.root / "local")}).start()
        # The detached worker prints one journal line; keep the test output clean.
        self.stdout = io.StringIO()
        mock.patch.object(sys, "stdout", self.stdout).start()

    def test_state_directory_is_explicit_and_defaults_to_home_ccs(self):
        with mock.patch.dict(os.environ, {"CCS_HOME": str(self.root / "legacy"), "CCS_DIR": str(self.root / "config")}, clear=True):
            self.assertEqual(t3.state_root(), self.root / ".ccs/app-updates")
            resolved = self.root / "legacy/.ccs/app-updates"
            with mock.patch.object(sys, "argv", ["helper", "--apply", "--platform", "ubuntu", "--state-dir", str(resolved), "--dashboard-job"]), \
                    mock.patch.object(updater, "run_apply", return_value={"results": []}), contextlib.redirect_stdout(io.StringIO()):
                updater.main()
            self.assertEqual(t3.state_root(), resolved)
            with mock.patch.object(t3, "command") as command:
                t3.schedule_restart(NEW)
            argv = command.call_args.args[0]
            self.assertEqual(argv[argv.index("--state-dir") + 1], str(resolved))
            self.assertIn("--dashboard-job", argv)
            self.assertFalse(any(str(arg).startswith("--setenv=CCS_HOME") for arg in argv))
            worker = argv[argv.index("/usr/bin/python3") + 1:]
            with mock.patch.object(sys, "argv", worker), mock.patch.object(t3, "deferred_restart") as restart:
                t3.main()
            restart.assert_called_once_with(os.getpid(), resolved, True)

    def test_dashboard_worker_with_missing_job_fails_closed(self):
        root = self.root / "resolved/app-updates"
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        self.assertFalse(t3.job_finished(root, dashboard=True))
        self.assertTrue(t3.job_finished(root))
        now = [0]
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), \
                mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + 600)), \
                mock.patch.object(t3, "command") as command:
            with self.assertRaisesRegex(common.UpdateFailure, "restart_failed"):
                t3.deferred_restart(12345, root, dashboard=True)
        command.assert_not_called()

    def test_windows_detects_full_nightly_from_asar_not_pe_version(self):
        executable = self.root / "local/Programs/t3code" / t3.WINDOWS_NAME
        asar(executable, NEW)
        install = t3.detect_t3("windows")
        self.assertEqual(install.version, NEW)
        self.assertEqual(install.package_root, executable.parent)
        asar(executable, NEW, "impostor")
        self.assertIsNone(t3.detect_t3("windows").version)

    def test_absent_installations_do_not_install_or_probe_real_apps(self):
        with mock.patch.object(t3, "cli_probe") as probe:
            self.assertIsNone(t3.detect_t3("ubuntu"))
            self.assertIsNone(t3.detect_t3("windows"))
        probe.assert_not_called()

    def test_runtime_detection_uses_launcher_and_timeout_is_unknown(self):
        runtime = self.root / ".t3/runtime/versions" / OLD / "t3"
        runtime.parent.mkdir(parents=True); runtime.touch()
        launcher = self.root / ".local/bin/t3"
        launcher.parent.mkdir(parents=True); launcher.symlink_to(runtime)
        with mock.patch.object(t3, "cli_probe", return_value=(None, "timeout")):
            install = t3.detect_t3("ubuntu")
        self.assertEqual(install.path, launcher)
        self.assertEqual(install.probe, "timeout")

    def test_release_selection_ignores_drafts_and_stable_and_uses_full_nightly(self):
        response = mock.MagicMock()
        response.geturl.return_value = t3.RELEASES
        response.read.return_value = json.dumps([
            {"tag_name": "v" + OLD}, {"tag_name": "v" + NEW},
            {"tag_name": "v99.0.0"}, {"tag_name": "v9.0.0-nightly.20261007.9999", "draft": True},
        ]).encode()
        with mock.patch.object(t3.urllib.request, "urlopen", return_value=contextlib.nullcontext(response)):
            self.assertEqual(t3.latest_release(), NEW)

    def test_release_lookup_uses_small_bounded_pages_and_checks_later_pages(self):
        self.assertTrue(t3.RELEASES.endswith("per_page=20"))
        responses = []
        for page, rows in ((1, [{"tag_name": "v1.0.0"}]), (2, [{"tag_name": "v" + NEW}])):
            response = mock.MagicMock()
            response.geturl.return_value = t3.RELEASES + ("&page=2" if page == 2 else "")
            response.read.return_value = json.dumps(rows).encode()
            responses.append(contextlib.nullcontext(response))
        with mock.patch.object(t3.urllib.request, "urlopen", side_effect=responses) as get:
            self.assertEqual(t3.latest_release(), NEW)
        self.assertEqual(get.call_count, 2)

    def test_manifest_binds_hash_to_exact_asset_and_rejects_duplicate_or_wrong_version(self):
        asset = "T3-Code-" + NEW + "-arm64.zip"
        checksum = base64.b64encode(b"x" * 64).decode()
        text = "version: " + NEW + "\nfiles:\n  - url: " + asset + "\n    sha512: " + checksum + "\n"
        self.assertEqual(t3.manifest_hash(text, NEW, asset), b"x" * 64)
        for bad in (text.replace(NEW, OLD), text + text.split("files:\n")[1], text.replace(checksum, "aaaa")):
            with self.assertRaises(common.UpdateFailure):
                t3.manifest_hash(bad, NEW, asset)

    def test_checksum_failure_never_executes_installer_or_stops_processes(self):
        install = t3.T3Install("t3-code", "windows", self.root / t3.WINDOWS_NAME, OLD, "official-download", desktop=True)
        def download(url, path, **kwargs):
            if url.endswith("nightly.yml"):
                asset = "T3-Code-" + NEW + "-x64.exe"
                path.write_text("version: " + NEW + "\nfiles:\n  - url: " + asset + "\n    sha512: " + base64.b64encode(b"x" * 64).decode() + "\n")
            else:
                path.write_bytes(b"untrusted fixture")
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3.platform, "machine", return_value="AMD64"), \
                mock.patch.object(t3, "download", side_effect=download) as get, mock.patch.object(t3, "powershell") as ps, \
                mock.patch.object(t3, "terminate_desktops") as stop:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["messageCode"], "signature_failed")
        self.assertEqual(get.call_args_list[1].kwargs["maximum"], t3.MAX_PACKAGE)
        ps.assert_not_called(); stop.assert_not_called()

    def test_current_t3_skips_downloads_installs_and_restarts(self):
        install = t3.T3Install("t3-code", "windows", self.root / t3.WINDOWS_NAME, NEW, "official-download", desktop=True)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "download") as get, \
                mock.patch.object(t3, "schedule_restart") as restart:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "current")
        self.assertFalse(row["updateAttempted"])
        get.assert_not_called(); restart.assert_not_called()

    def test_runtime_update_and_detached_schedule_never_restart_service_synchronously(self):
        runtime = self.root / ".t3/runtime/versions" / OLD / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, OLD, runtime=runtime, runtime_version=OLD)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command") as run, \
                mock.patch.object(t3, "cli_probe", return_value=(NEW, None)):
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["messageCode"], "t3_restart_scheduled")
        self.assertEqual(row["version"], NEW)
        self.assertEqual(row["restartedProcesses"], 0)
        update, query, schedule = [call.args[0] for call in run.call_args_list]
        self.assertEqual(update[1:], ["update", NEW, "--channel", "nightly"])
        self.assertNotIn("--yes", update)
        self.assertIn("--property=NeedDaemonReload", query)  # Re-read the rewritten unit; this never restarts it.
        self.assertEqual(schedule[0], "/usr/bin/systemd-run")
        self.assertIn("--restart-service", schedule)
        self.assertNotIn("restart", schedule)
        self.assertEqual(row["restartTargets"], [{"kind": "systemd", "service": "t3code.service", "delaySeconds": 30}])

    def test_pending_restart_is_retried_and_schedule_failure_is_reported(self):
        runtime = self.root / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, NEW, runtime=runtime, runtime_version=NEW)
        common.write_private_json(t3.state_root() / "t3-code-pending-restart.json", {"version": NEW})
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command", side_effect=common.UpdateFailure("restart_failed")):
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "restart_failed")
        self.assertFalse(row["updateAttempted"])

    def test_successful_runtime_install_with_probe_timeout_retains_restart_intent(self):
        runtime = self.root / ".t3/runtime/versions" / OLD / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, OLD, runtime=runtime, runtime_version=OLD)
        marker = t3.state_root() / "t3-code-pending-restart.json"
        replacement = runtime.parent.parent / NEW / "t3"
        def update(argv, **kwargs):
            self.assertEqual(json.loads(marker.read_text()), {"version": NEW})
            replacement.parent.mkdir(parents=True); replacement.touch()
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command", side_effect=update), \
                mock.patch.object(t3, "cli_probe", return_value=(None, "timeout")), mock.patch.object(t3, "schedule_restart") as restart:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "failed")
        self.assertTrue(row["updateAttempted"])
        self.assertTrue(marker.exists())
        restart.assert_not_called()
        current = t3.T3Install("t3-code", "ubuntu", replacement, NEW, runtime=replacement, runtime_version=NEW)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "running_runtime_version", return_value=OLD), \
                mock.patch.object(t3, "command") as command, mock.patch.object(t3, "update_runtime") as update:
            row = t3.update_t3(current, time.monotonic() + 60)
        self.assertEqual(row["messageCode"], "t3_restart_scheduled")
        self.assertFalse(row["updateAttempted"])
        update.assert_not_called()
        self.assertEqual(command.call_args.args[0][0], "/usr/bin/systemd-run")

    def test_pending_restart_cleared_only_when_running_service_has_installed_version(self):
        runtime = self.root / ".t3/runtime/versions" / NEW / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, NEW, runtime=runtime, runtime_version=NEW)
        marker = t3.state_root() / "t3-code-pending-restart.json"
        common.write_private_json(marker, {"version": NEW})
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command", return_value="12345\n") as command, \
                mock.patch.object(t3.os, "readlink", return_value=str(runtime)), mock.patch.object(t3, "schedule_restart") as restart:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "current")
        self.assertFalse(marker.exists())
        restart.assert_not_called()
        self.assertIn(["/usr/bin/systemctl", "--user", "show", "--property=MainPID", "--value", "t3code.service"],
                      [call.args[0] for call in command.call_args_list])
        for executable in (self.root / "other" / NEW / "t3", runtime.with_name("impostor")):
            with mock.patch.object(t3, "command", return_value="12345"), mock.patch.object(t3.os, "readlink", return_value=str(executable)):
                self.assertIsNone(t3.running_runtime_version())

    def test_detached_restart_waits_for_all_hosts_job_and_grace_then_locks(self):
        root = t3.state_root(); root.mkdir(parents=True)
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        now = [0]
        def sleep(seconds):
            now[0] += seconds
        def job_finished(_root, _dashboard):
            return now[0] >= 45  # remote hosts continue after Ubuntu finishes
        def run(argv, **kwargs):
            self.assertGreaterEqual(now[0], 75)
            self.assertTrue((root / "dashboard-update.lock").exists())
            with self.assertRaisesRegex(common.UpdateFailure, "busy"):
                with common.execution_lock():
                    self.fail("A standalone update acquired the restart worker's lock")
            if "--property=NeedDaemonReload" in argv:
                return systemctl(argv)
            self.assertEqual(argv, ["/usr/bin/systemctl", "--user", "restart", "t3code.service"])
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=sleep), mock.patch.object(t3, "job_finished", side_effect=job_finished), \
                mock.patch.object(t3, "command", side_effect=run) as command, mock.patch.object(t3, "health_check"), \
                mock.patch.object(t3, "running_runtime_version", return_value=NEW):
            t3.deferred_restart(12345)
        self.assertEqual(command.call_count, 2)  # The unit query, then the one restart.
        self.assertFalse((root / "dashboard-update.lock").exists())
        self.assertFalse((root / "t3-code-pending-restart.json").exists())

    def test_active_parent_or_lock_prevents_detached_restart(self):
        root = t3.state_root(); root.mkdir(parents=True)
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        for locked in (False, True):
            with self.subTest(locked=locked):
                now = [0]
                if locked:
                    (root / "dashboard-update.lock").write_text('{"pid":1}')
                with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError if locked else None), \
                        mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                        mock.patch.object(t3.time, "sleep", side_effect=lambda value: now.__setitem__(0, now[0] + 600)), \
                        mock.patch.object(t3, "command") as command:
                    with self.assertRaises(common.UpdateFailure):
                        t3.deferred_restart(12345)
                command.assert_not_called()

    def test_a_new_job_during_grace_restarts_the_thirty_second_wait(self):
        root = t3.state_root(); root.mkdir(parents=True)
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        now = [0]
        def sleep(seconds):
            now[0] += seconds
        def finished(_root, _dashboard):
            return not 10 <= now[0] < 40
        def run(argv, **kwargs):
            self.assertGreaterEqual(now[0], 70)
            return systemctl(argv)
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), \
                mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=sleep), \
                mock.patch.object(t3, "job_finished", side_effect=finished), \
                mock.patch.object(t3, "command", side_effect=run), \
                mock.patch.object(t3, "running_runtime_version", return_value=NEW), \
                mock.patch.object(t3, "health_check"):
            t3.deferred_restart(12345)

    def test_standalone_update_during_grace_delays_restart_and_final_locks_cover_health(self):
        root = self.root / "custom/app-updates"
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        now, holder = [0], [None]
        def sleep(seconds):
            now[0] += seconds
            if now[0] == 10:
                holder[0] = common.execution_lock(); holder[0].__enter__()
            elif now[0] == 45:
                holder[0].__exit__(None, None, None); holder[0] = None
        def check_locks():
            self.assertGreaterEqual(now[0], 75)
            self.assertTrue((root / "dashboard-update.lock").exists())
            with self.assertRaisesRegex(common.UpdateFailure, "busy"):
                with common.execution_lock():
                    self.fail("helper lock was not held")
        def command_locked(argv, **kwargs):
            check_locks()
            return systemctl(argv)
        def health_locked(host):
            check_locks()
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), \
                mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=sleep), \
                mock.patch.object(t3, "command", side_effect=command_locked) as command, mock.patch.object(t3, "health_check", side_effect=health_locked), \
                mock.patch.object(t3, "running_runtime_version", return_value=NEW):
            t3.deferred_restart(12345, root)
        self.assertEqual(command.call_count, 2)  # The unit query, then the one restart.
        self.assertIsNone(holder[0])
        self.assertFalse((root / "dashboard-update.lock").exists())
        with common.execution_lock():
            pass  # Both locks are released after successful health verification.

    def test_second_detached_worker_does_not_restart_a_completed_update_again(self):
        root = t3.state_root(); root.mkdir(parents=True)
        with mock.patch.object(t3, "command") as command:
            t3.deferred_restart(12345)
        command.assert_not_called()

    def test_job_completion_reads_the_persisted_envelope_and_fails_closed(self):
        root = t3.state_root(); root.mkdir(parents=True)
        path = root / "dashboard-job.json"
        for state in ("running", "completed", "failed"):
            common.write_private_json(path, {"job": {"state": state}})
            self.assertEqual(t3.job_finished(root), state != "running")
        hosts = {host: {"state": "done"} for host in ("ubuntu", "mac", "windows", "nas1")}
        common.write_private_json(path, {"job": {"state": "failed", "hosts": hosts}})
        self.assertTrue(t3.job_finished(root))
        hosts["windows"]["state"] = "running"
        common.write_private_json(path, {"job": {"state": "failed", "hosts": hosts}})
        self.assertFalse(t3.job_finished(root))
        path.write_text("invalid")
        self.assertFalse(t3.job_finished(root))

    def test_job_completion_waits_for_every_saved_host_including_nas1(self):
        root = t3.state_root(); root.mkdir(parents=True)
        path = root / "dashboard-job.json"

        def finished(hosts, state="completed"):
            common.write_private_json(path, {"job": {"state": state, "hosts": hosts}})
            return t3.job_finished(root)

        four = {host: {"state": "done"} for host in ("ubuntu", "mac", "windows", "nas1")}
        self.assertTrue(finished(four))
        for state in ("waiting", "running"):
            four["nas1"]["state"] = state  # Nas1 alone holds the restart
            self.assertFalse(finished(four), state)
        # A job saved before Nas1 existed lists three computers; all done is finished.
        three = {host: {"state": "done"} for host in ("ubuntu", "mac", "windows")}
        self.assertTrue(finished(three, "failed"))
        # Whatever the job saved must be done, even a computer added after this helper was written.
        self.assertFalse(finished({**three, "nas1": {"state": "done"}, "later": {"state": "running"}}))
        # Fail closed: nothing listed, or an entry that is not a host record in the done state.
        for hosts in ({}, [], "done", {"ubuntu": "done"}, {**three, "nas1": None}, {**three, "nas1": {"state": "DONE"}}):
            self.assertFalse(finished(hosts), hosts)

    def test_detached_restart_waits_for_nas1_to_finish_in_the_saved_job(self):
        root = t3.state_root(); root.mkdir(parents=True)
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        path = root / "dashboard-job.json"
        hosts = {host: {"state": "done"} for host in ("ubuntu", "mac", "windows")}
        hosts["nas1"] = {"state": "running"}
        common.write_private_json(path, {"job": {"state": "completed", "hosts": hosts}})
        now = [0]

        def sleep(seconds):
            now[0] += seconds
            if now[0] == 45:  # Nas1 finishes long after the other three computers
                hosts["nas1"]["state"] = "done"
                common.write_private_json(path, {"job": {"state": "completed", "hosts": hosts}})

        def run(argv, **kwargs):
            # The 30-second quiet period starts only once Nas1 is done.
            self.assertGreaterEqual(now[0], 75)
            if "--property=NeedDaemonReload" in argv:
                return systemctl(argv)
            self.assertEqual(argv, ["/usr/bin/systemctl", "--user", "restart", "t3code.service"])

        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=sleep), mock.patch.object(t3, "command", side_effect=run) as command, \
                mock.patch.object(t3, "health_check"), mock.patch.object(t3, "running_runtime_version", return_value=NEW):
            t3.deferred_restart(12345)
        self.assertEqual(command.call_count, 2)  # The unit query, then the one restart.
        self.assertFalse((root / "t3-code-pending-restart.json").exists())

    def test_t3_is_last_and_each_host_installer_stays_sequential(self):
        installations = {key: common.Install(key, "ubuntu", self.root / key, "1.0.0") for key in common.APP_LABELS}
        seen = []
        def update(install, deadline, phase=None):
            seen.append(install.app_id)
            return common.result(install.app_id, "ubuntu", "current", "1.0.0", "1.0.0", "native")
        with mock.patch.object(updater, "detect", return_value=installations), mock.patch.object(updater, "check_readiness", return_value=None), \
                mock.patch.object(updater, "antigravity_hold", return_value=None), mock.patch.object(updater, "update_cli", side_effect=update), \
                mock.patch.object(updater, "update_desktop", side_effect=update), mock.patch.object(updater, "update_t3", side_effect=update):
            updater.run_apply("ubuntu")
        self.assertEqual(seen[-2:], ["codex-cli", "t3-code"])
        self.assertEqual(len(seen), len(common.APP_LABELS))

    def test_mac_archive_rejects_escape_paths_and_symlinks_before_extracting(self):
        for name, target in (("../escape", None), ("T3.app/escape", "../../outside"), ("T3.app/escape", "/absolute")):
            package = self.root / "fixture.zip"
            with zipfile.ZipFile(package, "w") as archive:
                info = zipfile.ZipInfo(name)
                if target:
                    info.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(info, target or "fixture")
            with self.assertRaises(common.UpdateFailure):
                t3.check_mac_archive(package)

    def test_mac_archive_accepts_internal_framework_symlinks(self):
        package = self.root / "fixture.zip"
        with zipfile.ZipFile(package, "w") as archive:
            # Matches the installed T3 Electron/Mantle/ReactiveObjC/Squirrel
            # framework structure inspected read-only on the Mac.
            for framework, binary in (("Electron Framework", "Electron Framework"), ("Mantle", "Mantle"), ("ReactiveObjC", "ReactiveObjC"), ("Squirrel", "Squirrel")):
                root = t3.MAC_NAME + "/Contents/Frameworks/" + framework + ".framework/"
                links = {"Versions/Current": "A", binary: "Versions/Current/" + binary, "Resources": "Versions/Current/Resources"}
                if framework == "Electron Framework":
                    links.update({name: "Versions/Current/" + name for name in ("Libraries", "Helpers")})
                for name, target in links.items():
                    info = zipfile.ZipInfo(root + name)
                    info.external_attr = (stat.S_IFLNK | 0o777) << 16
                    archive.writestr(info, target)
                archive.writestr(root + "Versions/A/" + binary, "fixture")
                archive.writestr(root + "Versions/A/Resources/fixture", "fixture")
        t3.check_mac_archive(package)

    def test_mac_archive_rejects_symlink_ancestors_chains_and_cycles_in_any_order(self):
        fixtures = (
            [("a", "."), ("a/b", "../outside"), ("a/b/payload", None)],
            [("a", "."), ("b", "a/../outside")],
            [("a", "b"), ("b", "a")],
            [("dir/link", "../data"), ("dir/link/payload", None)],
            [("Alias", "."), ("alias/payload", None)],
            [("caf\u00e9", "."), ("cafe\u0301/payload", None)],
        )
        for entries in fixtures:
            for ordered in (entries, list(reversed(entries))):
                with self.subTest(entries=ordered):
                    package = self.root / "fixture.zip"
                    with zipfile.ZipFile(package, "w") as archive:
                        for name, target in ordered:
                            info = zipfile.ZipInfo(name)
                            if target is not None:
                                info.external_attr = (stat.S_IFLNK | 0o777) << 16
                            archive.writestr(info, target if target is not None else "fixture")
                    install = t3.T3Install("t3-code", "mac", self.root / t3.MAC_NAME, OLD, desktop=True)
                    with mock.patch.object(t3, "verified_download", return_value=package), mock.patch.object(t3, "command") as extract, \
                            mock.patch.object(t3, "verify_mac") as verify, mock.patch.object(t3, "terminate_desktops") as stop:
                        with self.assertRaisesRegex(common.UpdateFailure, "signature_failed"):
                            t3.update_mac(install, NEW, self.root, time.monotonic() + 60, lambda name: None)
                    extract.assert_not_called(); verify.assert_not_called(); stop.assert_not_called()

    def test_mac_runtime_updated_even_when_desktop_is_already_current(self):
        install = t3.T3Install("t3-code", "mac", self.root / t3.MAC_NAME, NEW, "official-download", runtime=self.root / "t3", runtime_version=OLD, desktop=True)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "update_runtime") as runtime, \
                mock.patch.object(t3, "update_mac") as desktop:
            row = t3.update_t3(install, time.monotonic() + 60)
        runtime.assert_called_once(); desktop.assert_not_called()
        self.assertEqual(row["status"], "updated")
        self.assertEqual(row["version"], NEW)

    def test_mac_codesign_requirement_is_one_inline_argument(self):
        bundle_path = self.root / t3.MAC_NAME; bundle(bundle_path, NEW)
        with mock.patch.object(t3, "command") as command:
            t3.verify_mac(bundle_path, NEW)
        argv = [call.args[0] for call in command.call_args_list]
        requirement = 'identifier "com.t3tools.t3code" and anchor apple generic and certificate leaf[subject.OU] = "ARK85ZXQ4Z"'
        self.assertEqual(argv, [
            ["/usr/bin/codesign", "--verify", "--deep", "--strict", bundle_path],
            ["/usr/bin/codesign", "--verify", "-R=" + requirement, bundle_path],
            ["/usr/sbin/spctl", "--assess", "--type", "execute", bundle_path],
        ])
        # codesign reads a bare -R value as a file path, so the requirement must stay attached as -R=<text>.
        self.assertTrue(argv[1][2].startswith("-R="))
        self.assertNotIn("-R", [arg for call in argv for arg in call])

    def test_mac_bundle_swap_rolls_back_on_health_failure(self):
        installed = self.root / t3.MAC_NAME; bundle(installed, OLD)
        staged = self.root / "stage"; bundle(staged / t3.MAC_NAME, NEW)
        package = self.root / "app.zip"
        with zipfile.ZipFile(package, "w") as archive:
            archive.writestr(t3.MAC_NAME + "/Contents/fixture", "fixture")
        install = t3.T3Install("t3-code", "mac", installed, OLD, "official-download", package_root=installed, desktop=True)
        def run(argv, **kwargs):
            if "-x" in argv:
                shutil.copytree(staged, argv[-1])
            elif argv[0] == "/usr/bin/ditto":
                shutil.copytree(argv[1], argv[2])
        with mock.patch.object(t3, "verified_download", return_value=package), mock.patch.object(t3, "command", side_effect=run), \
                mock.patch.object(t3, "verify_mac"), mock.patch.object(t3, "scan", return_value=[]), \
                mock.patch.object(t3, "terminate_desktops", return_value=0), mock.patch.object(t3, "restart_desktops"), \
                mock.patch.object(t3, "health_check", side_effect=common.UpdateFailure("restart_failed")):
            with self.assertRaises(common.UpdateFailure):
                t3.update_mac(install, NEW, self.root / "work", time.monotonic() + 60, lambda name: None)
        self.assertEqual(t3.mac_bundle_version(installed), OLD)
        self.assertFalse(list(self.root.glob(".aac-t3-*")))

    def test_windows_signature_failure_never_stops_app_and_valid_update_reuses_session_helpers(self):
        executable = self.root / "install" / t3.WINDOWS_NAME; asar(executable, OLD)
        package = self.root / "installer.exe"; package.touch()
        install = t3.T3Install("t3-code", "windows", executable, OLD, "official-download", package_root=executable.parent, desktop=True)
        with mock.patch.object(t3, "verified_download", return_value=package), \
                mock.patch.object(t3, "powershell", side_effect=common.UpdateFailure()), mock.patch.object(t3, "terminate_desktops") as stop:
            with self.assertRaisesRegex(common.UpdateFailure, "signature_failed"):
                t3.update_windows(install, NEW, self.root, time.monotonic() + 60, lambda name: None)
        stop.assert_not_called()
        contexts = [mock.Mock()]
        with mock.patch.object(t3, "verified_download", return_value=package), mock.patch.object(t3, "verify_windows"), \
                mock.patch.object(t3, "powershell", side_effect=lambda script, **kwargs: asar(executable, NEW)) as ps, \
                mock.patch.object(t3, "scan", return_value=[]), mock.patch.object(t3, "main_contexts", return_value=contexts), \
                mock.patch.object(t3, "terminate_desktops", return_value=0) as stop, \
                mock.patch.object(t3, "restart_desktops", return_value=1) as restart, mock.patch.object(t3, "health_check"):
            self.assertEqual(t3.update_windows(install, NEW, self.root, time.monotonic() + 60, lambda name: None), (1, 0))
        stop.assert_called_once_with(install, contexts)
        restart.assert_called_once_with(install, contexts)
        self.assertIn("'/S' -PassThru", ps.call_args.args[0])
        self.assertIn("WaitForExit(", ps.call_args.args[0])
        self.assertIn("taskkill.exe /PID $p.Id /T /F", ps.call_args.args[0])

    def test_windows_scanner_captures_owned_t3_family_for_install_and_rollback(self):
        executable = self.root / "install" / t3.WINDOWS_NAME; asar(executable, OLD)
        install = t3.T3Install("t3-code", "windows", executable, OLD, "official-download", package_root=executable.parent, desktop=True)
        rows = [
            {"pid": 1, "ppid": 0, "exe": str(executable), "args": str(executable), "identity": "main-start", "session": 10},
            {"pid": 2, "ppid": 1, "exe": str(executable), "args": str(executable), "identity": "child-start", "session": 10},
            {"pid": 3, "ppid": 0, "exe": str(self.root / "unrelated" / t3.WINDOWS_NAME), "args": "unrelated", "identity": "other-start", "session": 10},
        ]
        for pid, name in enumerate(("t3-resource-monitor.exe", "cursorsandbox.exe", "rg.exe", "OpenConsole.exe", "elevate.exe"), 4):
            rows.append({"pid": pid, "ppid": 1, "exe": str(executable.parent / "resources" / name), "args": name, "identity": "helper-start-" + str(pid), "session": 10})
        def inventory(script, **kwargs):
            allowed = re.search(r"\$p.Name -notin @\((.*?)\)\)\{continue\}", script).group(1)
            names = re.findall(r"'([^']+)'", allowed)
            self.assertIn("$o.Sid -ne $me -or -not $p.ExecutablePath", script)
            self.assertIn("StartTime.ToFileTimeUtc()", script)
            self.assertIn("session=[int]$p.SessionId", script)
            return json.dumps([row for row in rows if pathlib.Path(row["exe"]).name in names])
        stopped = []
        def stop(_install, contexts):
            self.assertEqual([item.pid for item in contexts], [1])
            self.assertEqual(contexts[0].identity, "main-start")
            self.assertEqual(contexts[0].session, 10)
            stopped.append(contexts)
            return 0
        def installer(script, **kwargs):
            self.assertEqual(len(stopped), 1)
            asar(executable, NEW)
        with mock.patch.object(processes, "powershell", side_effect=inventory), \
                mock.patch.object(processes, "windows_arguments", side_effect=lambda value: [value]), \
                mock.patch.object(t3, "verified_download", return_value=self.root / "installer.exe"), mock.patch.object(t3, "verify_windows"), \
                mock.patch.object(t3, "powershell", side_effect=installer), mock.patch.object(t3, "terminate_desktops", side_effect=stop), \
                mock.patch.object(t3, "restart_desktops"), mock.patch.object(t3, "health_check", side_effect=common.UpdateFailure("restart_failed")):
            selected = processes.family(install, processes.scan("windows"))
            self.assertEqual([item.pid for item in selected], [1, 2, 4, 5, 6, 7, 8])
            with self.assertRaisesRegex(common.UpdateFailure, "restart_failed"):
                t3.update_windows(install, NEW, self.root, time.monotonic() + 60, lambda name: None)
        self.assertEqual(len(stopped), 2)
        self.assertEqual(t3.windows_bundle_version(executable), OLD)

    def test_mac_scanner_captures_t3_main_and_framework_helpers_only_for_current_user(self):
        root = self.root / t3.MAC_NAME
        install = t3.T3Install("t3-code", "mac", root, OLD, package_root=root, desktop=True)
        main = str(root / "Contents/MacOS/T3 Code (Nightly)")
        paths = {1: main, 2: str(root / "Contents/Frameworks/T3 Code (Nightly) Helper.app/Contents/MacOS/T3 Code (Nightly) Helper"),
                 3: str(self.root / "unrelated/T3 Code (Nightly)"), 4: main}
        uid = os.getuid()
        output = "\n".join(f"{pid} {1 if pid == 2 else 0} {uid if pid != 4 else uid + 1} start-{pid}" for pid in paths)
        libproc = mock.Mock()
        def pidpath(pid, buffer, size):
            buffer.value = paths[pid].encode()
            return len(buffer.value)
        libproc.proc_pidpath.side_effect = pidpath
        with mock.patch.object(processes, "command", return_value=output), mock.patch.object(processes.ctypes, "CDLL", return_value=libproc), \
                mock.patch.object(processes, "mac_arguments", return_value=[main]):
            rows = processes.scan("mac")
            self.assertEqual([item.pid for item in processes.family(install, rows)], [1, 2])
            contexts = processes.main_contexts(install, rows)
        self.assertEqual([item.pid for item in contexts], [1])
        self.assertFalse(processes.same_process(contexts[0], processes.Process(1, 0, uid, main, "reused-pid")))

    def test_t3_windows_close_still_rejects_another_session(self):
        executable = self.root / t3.WINDOWS_NAME
        install = t3.T3Install("t3-code", "windows", executable, OLD, package_root=self.root, desktop=True)
        context = processes.Process(1, 0, 0, str(executable), "start", [str(executable)], session=11)
        kernel = mock.Mock()
        kernel.ProcessIdToSessionId.side_effect = lambda pid, session: setattr(session._obj, "value", 10)
        with mock.patch.object(processes, "scan", return_value=[context]), \
                mock.patch.object(processes.ctypes, "windll", types.SimpleNamespace(kernel32=kernel), create=True):
            with self.assertRaisesRegex(common.UpdateFailure, "restart_context"):
                processes.terminate_desktops(install, [context])

    def test_windows_installer_failure_restores_previous_bundle(self):
        executable = self.root / "install" / t3.WINDOWS_NAME; asar(executable, OLD)
        install = t3.T3Install("t3-code", "windows", executable, OLD, "official-download", package_root=executable.parent, desktop=True)
        def fail(script, **kwargs):
            asar(executable, NEW)
            raise common.UpdateFailure()
        with mock.patch.object(t3, "verified_download", return_value=self.root / "installer.exe"), \
                mock.patch.object(t3, "verify_windows"), mock.patch.object(t3, "scan", return_value=[]), \
                mock.patch.object(t3, "terminate_desktops", return_value=0), mock.patch.object(t3, "restart_desktops"), \
                mock.patch.object(t3, "powershell", side_effect=fail):
            with self.assertRaises(common.UpdateFailure):
                t3.update_windows(install, NEW, self.root, time.monotonic() + 60, lambda name: None)
        self.assertEqual(t3.windows_bundle_version(executable), OLD)

    def test_windows_installer_timeout_restores_old_version_and_retains_timeout_code(self):
        executable = self.root / "install" / t3.WINDOWS_NAME; asar(executable, OLD)
        install = t3.T3Install("t3-code", "windows", executable, OLD, "official-download", package_root=executable.parent, desktop=True)
        with mock.patch.object(t3, "verified_download", return_value=self.root / "installer.exe"), \
                mock.patch.object(t3, "verify_windows"), mock.patch.object(t3, "scan", return_value=[]), \
                mock.patch.object(t3, "terminate_desktops", return_value=0), mock.patch.object(t3, "restart_desktops"), \
                mock.patch.object(t3, "powershell", return_value="timeout") as ps:
            with self.assertRaisesRegex(common.UpdateFailure, "timeout"):
                t3.update_windows(install, NEW, self.root, time.monotonic() + 60, lambda name: None)
        self.assertEqual(t3.windows_bundle_version(executable), OLD)
        self.assertIn("taskkill.exe /PID $p.Id /T /F", ps.call_args.args[0])

    def runtime(self, version):
        binary = self.root / ".t3/runtime/versions" / version / "t3"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.touch()
        return binary

    def fake_proc(self, processes):
        # {pid: (exe, argv, child pids)}; exe links point at real files, so readlink works unmocked. None leaves no exe link.
        proc = pathlib.Path(tempfile.mkdtemp(dir=self.root))
        for pid, (exe, argv, children) in processes.items():
            (proc / pid / "task" / pid).mkdir(parents=True)
            if exe:
                (proc / pid / "exe").symlink_to(exe)
            (proc / pid / "cmdline").write_bytes(b"".join(item.encode() + b"\0" for item in argv))
            (proc / pid / "task" / pid / "children").write_text("".join(child + " " for child in children))
        return proc

    def runtime_layout(self, numbers, active):
        versions = self.root / ".t3/runtime/versions"
        for number in numbers:
            self.runtime(named(number))
        common.write_private_json(self.root / ".t3/runtime/service-state.json", {"protocol": 3, "activeVersion": named(active)})
        (self.root / ".local/bin").mkdir(parents=True, exist_ok=True)
        (self.root / ".local/bin/t3").symlink_to(versions / named(active) / "t3")
        return versions

    def acp_unit(self):
        unit = self.root / ".config/systemd/user" / t3.ACP_UNIT
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text("[Unit]\n")
        return unit

    def managed_cursor(self):
        binary = self.root / ".local/share/cursor-agent/versions/2026.10.01-fixture/cursor-agent"
        binary.parent.mkdir(parents=True)
        binary.touch()
        (self.root / ".local/bin").mkdir(parents=True, exist_ok=True)
        (self.root / ".local/bin/cursor-agent").symlink_to(binary)
        return self.root / ".local/bin/cursor-agent"

    def components(self):
        return json.loads((t3.state_root() / "t3-components.json").read_text())

    def test_running_version_is_the_server_child_of_the_service_launcher(self):
        old, new = self.runtime(OLD), self.runtime(NEW)
        outside = self.root / "other" / NEW / "t3"
        outside.parent.mkdir(parents=True)
        outside.touch()
        launcher = (old, [str(old), "__service-launcher"], ["4343"])
        cases = {
            "server child": ({"4242": launcher, "4343": (new, [str(new), "serve"], [])}, NEW),
            "child is not a server": ({"4242": launcher, "4343": (new, [str(new), "doctor"], [])}, None),
            "server outside the runtime tree": ({"4242": launcher, "4343": (outside, [str(outside), "serve"], [])}, None),
            "launcher without a child": ({"4242": (old, [str(old), "__service-launcher"], [])}, None),
            "plain server as main process": ({"4242": (new, [str(new), "serve"], [])}, NEW),
        }
        for name, (processes, expected) in cases.items():
            with self.subTest(name), mock.patch.object(t3, "command", return_value="4242\n"), \
                    mock.patch.object(t3, "PROC_ROOT", str(self.fake_proc(processes))):
                self.assertEqual(t3.running_runtime_version(), expected)

    def test_stale_unit_is_reloaded_after_a_runtime_install_and_never_restarted(self):
        runtime = self.root / ".t3/runtime/versions" / OLD / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, OLD, runtime=runtime, runtime_version=OLD)
        with mock.patch.object(t3, "latest_release", return_value=NEW), \
                mock.patch.object(t3, "command", side_effect=lambda argv, **kwargs: systemctl(argv, reload="yes")) as command, \
                mock.patch.object(t3, "cli_probe", return_value=(NEW, None)):
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["messageCode"], "t3_restart_scheduled")
        argv = [call.args[0] for call in command.call_args_list]
        self.assertEqual(argv[2], ["/usr/bin/systemctl", "--user", "daemon-reload"])
        self.assertNotIn("restart", [item for call in argv for item in call])
        self.assertTrue(self.components()["daemonReload"])

    def test_stale_unit_is_reloaded_on_the_current_path_without_any_restart(self):
        runtime = self.root / ".t3/runtime/versions" / NEW / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, NEW, runtime=runtime, runtime_version=NEW)
        with mock.patch.object(t3, "latest_release", return_value=NEW), \
                mock.patch.object(t3, "command", side_effect=lambda argv, **kwargs: systemctl(argv, reload="yes")) as command, \
                mock.patch.object(t3, "schedule_restart") as schedule:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "current")
        schedule.assert_not_called()
        self.assertEqual([call.args[0] for call in command.call_args_list], [
            ["/usr/bin/systemctl", "--user", "show", "--property=NeedDaemonReload", "--value", "t3code.service"],
            ["/usr/bin/systemctl", "--user", "daemon-reload"],
        ])
        components = self.components()
        self.assertEqual(set(components), set(t3.COMPONENT_KEYS))
        self.assertEqual((components["cursorAgent"]["status"], components["acpUpdater"]), ("absent", "absent"))

    def test_detached_restart_reloads_the_stale_unit_right_before_restarting(self):
        root = t3.state_root(); root.mkdir(parents=True)
        common.write_private_json(root / "t3-code-pending-restart.json", {"version": NEW})
        now = [0]
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)), \
                mock.patch.object(t3, "job_finished", return_value=True), \
                mock.patch.object(t3, "command", side_effect=lambda argv, **kwargs: systemctl(argv, reload="yes")) as command, \
                mock.patch.object(t3, "health_check"), mock.patch.object(t3, "running_runtime_version", return_value=NEW), \
                mock.patch.object(t3, "prune_runtimes", return_value=[]):
            t3.deferred_restart(12345)
        self.assertEqual([call.args[0][2] for call in command.call_args_list], ["show", "daemon-reload", "restart"])
        self.assertTrue(self.components()["daemonReload"])

    def test_post_restart_version_mismatch_keeps_the_marker_and_finishes_nothing(self):
        root = t3.state_root(); root.mkdir(parents=True)
        marker = root / "t3-code-pending-restart.json"
        common.write_private_json(marker, {"version": NEW})
        now = [0]
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)), \
                mock.patch.object(t3, "job_finished", return_value=True), \
                mock.patch.object(t3, "command", side_effect=lambda argv, **kwargs: systemctl(argv)) as command, mock.patch.object(t3, "health_check"), \
                mock.patch.object(t3, "running_runtime_version", return_value=OLD), mock.patch.object(t3, "finish_ubuntu_restart") as finish:
            with self.assertRaisesRegex(common.UpdateFailure, "restart_failed"):
                t3.deferred_restart(12345)
        self.assertIn(["/usr/bin/systemctl", "--user", "restart", "t3code.service"], [call.args[0] for call in command.call_args_list])
        self.assertTrue(marker.exists())
        finish.assert_not_called()

    def test_unreadable_pending_marker_never_restarts_the_service(self):
        root = t3.state_root(); root.mkdir(parents=True)
        (root / "t3-code-pending-restart.json").write_text("not json")
        now = [0]
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)), \
                mock.patch.object(t3, "job_finished", return_value=True), mock.patch.object(t3, "command") as command:
            with self.assertRaisesRegex(common.UpdateFailure, "restart_failed"):
                t3.deferred_restart(12345)
        command.assert_not_called()

    def test_prune_keeps_the_vm_active_rollback_and_running_runtimes(self):
        versions = self.runtime_layout([2735, 2752, 2774, 2833, 2849], active=2849)
        proc = self.fake_proc({"4242": (versions / named(2833) / "t3", [str(versions / named(2833) / "t3"), "__service-launcher"], [])})
        with mock.patch.object(t3, "PROC_ROOT", str(proc)):
            deleted = t3.prune_runtimes()
        self.assertEqual(deleted, [named(number) for number in (2735, 2752, 2774)])
        self.assertEqual(sorted(path.name for path in versions.iterdir()), sorted([named(2833), named(2849)]))

    def test_prune_keeps_an_old_runtime_that_a_process_still_executes(self):
        versions = self.runtime_layout([2735, 2752, 2833, 2849], active=2849)
        proc = self.fake_proc({"4242": (versions / named(2735) / "t3", [str(versions / named(2735) / "t3"), "__service-launcher"], [])})
        with mock.patch.object(t3, "PROC_ROOT", str(proc)):
            deleted = t3.prune_runtimes()
        self.assertEqual(deleted, [named(2752)])  # 2833 is the rollback copy; 2735 is still running.
        self.assertEqual(sorted(path.name for path in versions.iterdir()), sorted([named(2735), named(2833), named(2849)]))

    def test_prune_keeps_runtimes_named_on_a_command_line_when_the_exe_is_elsewhere_or_hidden(self):
        versions = self.runtime_layout([2600, 2735, 2752, 2774, 2833, 2849], active=2849)
        node = "/usr/bin/node"
        proc = self.fake_proc({
            "4242": (node, [node, str(versions / named(2735) / "node_modules/x.js")], []),
            "4243": (None, [node, str(versions / named(2752) / "resource-monitor")], []),  # hidden exe, like a non-dumpable helper
            "4244": (node, [node, "--runtime-dir", str(versions / named(2774))], []),  # a bare directory argument
        })
        with mock.patch.object(t3, "PROC_ROOT", str(proc)):
            self.assertEqual(t3.running_runtime_versions(versions), {named(2735), named(2752), named(2774)})
            self.assertEqual(t3.prune_runtimes(), [named(2600)])
        self.assertEqual(sorted(path.name for path in versions.iterdir()), sorted([named(number) for number in (2735, 2752, 2774, 2833, 2849)]))

    def test_command_line_names_count_only_as_exact_version_directories(self):
        versions = self.runtime_layout([2735, 2849], active=2849)
        node = "/usr/bin/node"
        names = ["0.0.46-nightly.2735", "latest", named(2735) + ".bak", "../" + named(2735)]
        proc = self.fake_proc({"4242": (node, [node] + [str(versions / name / "x.js") for name in names], [])})
        with mock.patch.object(t3, "PROC_ROOT", str(proc)):
            self.assertEqual(t3.running_runtime_versions(versions), set())

    def test_unreadable_command_line_is_skipped_and_missing_proc_gives_none(self):
        versions = self.runtime_layout([2735, 2849], active=2849)
        proc = self.fake_proc({"4242": (str(versions / named(2735) / "t3"), [], [])})
        (proc / "4242" / "cmdline").unlink()
        with mock.patch.object(t3, "PROC_ROOT", str(proc)):
            self.assertEqual(t3.running_runtime_versions(versions), {named(2735)})
        with mock.patch.object(t3, "PROC_ROOT", str(self.root / "no-proc")):
            self.assertIsNone(t3.running_runtime_versions(versions))

    def test_prune_keeps_only_the_nas1_rollback_copy(self):
        versions = self.runtime_layout([2787, 2833, 2849], active=2849)
        with mock.patch.object(t3, "PROC_ROOT", str(self.fake_proc({}))):
            deleted = t3.prune_runtimes()
        self.assertEqual(deleted, [named(2787)])
        self.assertEqual(sorted(path.name for path in versions.iterdir()), sorted([named(2833), named(2849)]))

    def test_prune_deletes_nothing_without_a_readable_active_version(self):
        versions = self.runtime_layout([2735, 2849], active=2849)
        state = self.root / ".t3/runtime/service-state.json"
        for content in ("{not json", json.dumps({"activeVersion": "../escape"}), json.dumps(["no"]), None):
            with self.subTest(content=content):
                if content is None:
                    state.unlink()
                else:
                    state.write_text(content)
                with mock.patch.object(t3, "PROC_ROOT", str(self.fake_proc({}))):
                    self.assertEqual(t3.prune_runtimes(), [])
                self.assertTrue((versions / named(2735) / "t3").exists())

    def test_prune_never_follows_or_removes_symlinks(self):
        versions = self.runtime_layout([2849], active=2849)
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "keep.txt").write_text("keep")
        (versions / named(2600)).symlink_to(outside)
        with mock.patch.object(t3, "PROC_ROOT", str(self.fake_proc({}))):
            self.assertEqual(t3.prune_runtimes(), [])
        self.assertTrue((versions / named(2600)).is_symlink())
        self.assertEqual((outside / "keep.txt").read_text(), "keep")

    def test_prune_deletes_at_most_twenty_directories_per_run_oldest_first(self):
        versions = self.runtime_layout(list(range(1000, 1025)) + [2849], active=2849)
        with mock.patch.object(t3, "PROC_ROOT", str(self.fake_proc({}))):
            deleted = t3.prune_runtimes()
        self.assertEqual(deleted, [named(number) for number in range(1000, 1020)])
        self.assertEqual(len(list(versions.iterdir())), 6)  # 1020 to 1024, the rollback 1024 among them, plus 2849.

    def test_cursor_agent_update_runs_only_for_the_managed_install(self):
        deadline = time.monotonic() + 60
        with mock.patch.object(t3, "command") as command, mock.patch.object(t3, "cli_probe") as probe:
            self.assertEqual(t3.cursor_agent_update(deadline), {"before": None, "after": None, "status": "absent"})
            outside = self.root / "outside/cursor-agent"
            outside.parent.mkdir()
            outside.touch()
            (self.root / ".local/bin").mkdir(parents=True, exist_ok=True)
            (self.root / ".local/bin/cursor-agent").symlink_to(outside)
            self.assertEqual(t3.cursor_agent_update(deadline)["status"], "unmanaged")
            command.assert_not_called(); probe.assert_not_called()
        (self.root / ".local/bin/cursor-agent").unlink()
        link = self.managed_cursor()
        with mock.patch.object(t3, "command") as command, \
                mock.patch.object(t3, "cli_probe", side_effect=[("2026.10.01-old", None), ("2026.10.02-new", None)]):
            self.assertEqual(t3.cursor_agent_update(deadline), {"before": "2026.10.01-old", "after": "2026.10.02-new", "status": "updated"})
        command.assert_called_once_with([link, "update"], timeout=mock.ANY)
        with mock.patch.object(t3, "command", side_effect=common.UpdateFailure()), \
                mock.patch.object(t3, "cli_probe", side_effect=[("2026.10.01-old", None), ("2026.10.01-old", None)]):
            self.assertEqual(t3.cursor_agent_update(deadline)["status"], "failed")
        with mock.patch.object(t3, "command") as command, mock.patch.object(t3, "cli_probe", return_value=("2026.10.01-old", None)):
            self.assertEqual(t3.cursor_agent_update(time.monotonic() - 1)["status"], "failed")
        command.assert_not_called()

    def test_acp_updater_starts_from_the_row_only_when_no_restart_is_scheduled(self):
        self.acp_unit()
        runtime = self.root / ".t3/runtime/versions" / NEW / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, NEW, runtime=runtime, runtime_version=NEW)
        with mock.patch.object(t3, "latest_release", return_value=NEW), \
                mock.patch.object(t3, "command", side_effect=lambda argv, **kwargs: systemctl(argv)) as command:
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual(row["status"], "current")
        argv = [call.args[0] for call in command.call_args_list]
        self.assertIn(["/usr/bin/systemctl", "--user", "start", "--no-block", "t3-acp-update.service"], argv)
        self.assertNotIn(["/usr/bin/systemctl", "--user", "daemon-reload"], argv)  # The unit query said "no".
        self.assertEqual(self.components()["acpUpdater"], "started")

    def test_acp_updater_waits_for_the_verified_restart_when_one_is_scheduled(self):
        self.acp_unit()
        runtime = self.root / ".t3/runtime/versions" / OLD / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, OLD, runtime=runtime, runtime_version=OLD)
        calls = []
        def run(argv, **kwargs):
            calls.append([str(item) for item in argv])
            return systemctl(argv)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command", side_effect=run), \
                mock.patch.object(t3, "cli_probe", return_value=(NEW, None)):
            row = t3.update_t3(install, time.monotonic() + 60)
        start = ["/usr/bin/systemctl", "--user", "start", "--no-block", "t3-acp-update.service"]
        restart = ["/usr/bin/systemctl", "--user", "restart", "t3code.service"]
        self.assertEqual(row["messageCode"], "t3_restart_scheduled")
        self.assertNotIn(start, calls)
        self.assertEqual(self.components()["acpUpdater"], "pending")
        now = [0]
        with mock.patch.object(t3.os, "kill", side_effect=ProcessLookupError), mock.patch.object(t3.time, "monotonic", side_effect=lambda: now[0]), \
                mock.patch.object(t3.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)), \
                mock.patch.object(t3, "job_finished", return_value=True), mock.patch.object(t3, "command", side_effect=run), \
                mock.patch.object(t3, "health_check"), mock.patch.object(t3, "running_runtime_version", return_value=NEW), \
                mock.patch.object(t3, "prune_runtimes", return_value=[OLD]):
            t3.deferred_restart(12345)
        self.assertLess(calls.index(restart), calls.index(start))
        components = self.components()
        self.assertEqual((components["acpUpdater"], components["prunedRuntimes"], components["daemonReload"]), ("started", [OLD], False))
        self.assertIn("T3 companions: ACP updater started; pruned " + OLD, self.stdout.getvalue())

    def test_companion_failures_never_change_the_t3_row(self):
        self.managed_cursor()
        self.acp_unit()
        runtime = self.root / ".t3/runtime/versions" / NEW / "t3"
        install = t3.T3Install("t3-code", "ubuntu", runtime, NEW, runtime=runtime, runtime_version=NEW)
        def run(argv, **kwargs):
            if "update" in argv:
                raise common.UpdateFailure("timeout")
            if "start" in argv:
                raise common.UpdateFailure("update_failed")
            return systemctl(argv)
        with mock.patch.object(t3, "latest_release", return_value=NEW), mock.patch.object(t3, "command", side_effect=run), \
                mock.patch.object(t3, "cli_probe", return_value=("2026.10.01-fixture", None)):
            row = t3.update_t3(install, time.monotonic() + 60)
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]), ("current", "current", NEW, False))
        self.assertEqual((self.components()["cursorAgent"]["status"], self.components()["acpUpdater"]), ("failed", "failed"))

    def test_muse_cmd_detection_versions_official_launcher_and_disables_auto_update(self):
        root = self.root / "local/Programs/muse"; root.mkdir(parents=True)
        launcher = root / "muse.cmd"; launcher.touch()
        (root / ".muse-launcher.ps1").touch()
        with mock.patch.object(updater.shutil, "which", return_value=None), \
                mock.patch.object(updater, "command", return_value="Muse Code 1.4.3 (1.4.3-R5018.1)") as run:
            install = updater.detect_cli("muse-code", "windows")
        self.assertEqual(install.version, "1.4.3-R5018.1")
        self.assertEqual(install.manager, "native")
        self.assertEqual(run.call_args.args[0][-2:], [root / ".muse-launcher.ps1", "--version"])
        self.assertEqual(common.environment()["MUSE_NO_AUTO_UPDATE"], "1")
        (root / "muse").touch()
        with mock.patch.object(updater.shutil, "which", return_value=str(root / "muse")), mock.patch.object(updater, "command", return_value="1.4.3"):
            self.assertEqual(updater.detect_cli("muse-code", "windows").path, launcher)

    def test_muse_windows_updates_official_install_in_upgrade_mode_without_path_changes(self):
        root = self.root / "local/Programs/muse"; root.mkdir(parents=True)
        (root / ".muse-launcher.ps1").touch()
        install = common.Install("muse-code", "windows", root / "muse.cmd", "1.4.3")
        with mock.patch.object(updater, "download") as get, mock.patch.object(updater, "command") as run:
            updater.perform_cli_update(install, time.monotonic() + 60)
        self.assertEqual(get.call_args.args[0], "https://dev.meta.ai/install.ps1")
        self.assertEqual(run.call_args.kwargs["env"], {"MUSE_UPGRADE_MODE": "1", "MUSE_NO_MODIFY_PATH": "1", "MUSE_INSTALL_DIR": str(root)})
        install.path = self.root / "unrelated/muse.cmd"
        with mock.patch.object(updater, "download") as get:
            with self.assertRaises(common.UpdateFailure):
                updater.perform_cli_update(install)
        get.assert_not_called()

    def test_windows_muse_probe_timeout_and_unknown_shim_do_not_invent_version(self):
        root = self.root / "local/Programs/muse"; root.mkdir(parents=True)
        (root / "muse.cmd").touch(); (root / ".muse-launcher.ps1").touch()
        with mock.patch.object(updater.shutil, "which", return_value=None), \
                mock.patch.object(updater, "command", side_effect=common.UpdateFailure("timeout")):
            install = updater.detect_cli("muse-code", "windows")
        self.assertIsNone(install.version)
        self.assertEqual(install.probe, "timeout")
        unrelated = self.root / "other/muse.cmd"
        unrelated.parent.mkdir(); unrelated.touch()
        with mock.patch.object(updater.shutil, "which", return_value=str(unrelated)), mock.patch.object(updater, "command") as probe:
            self.assertEqual(updater.detect_cli("muse-code", "windows").manager, "unsupported")
        probe.assert_not_called()


if __name__ == "__main__":
    unittest.main()
