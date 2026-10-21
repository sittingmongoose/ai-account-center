"""Offline regression fixtures for the second updater review."""
import contextlib
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_common as common
import app_update_desktop as desktop
import app_update_processes as processes
import app_update_terminal as terminal
import app_update_t3 as t3
import app_update_zcode as zcode
import app_updates as updater


class ReviewTests(unittest.TestCase):
    def test_npm_editors_are_not_cli_processes_but_descendants_are(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            (root / "package.json").write_text(json.dumps({"bin": {"tool": "dist/main.js"}, "main": "index.js"}))
            install = common.Install("codex-cli", "ubuntu", root / "codex", manager="npm", package_root=root)
            def row(pid, exe, arg, parent=0):
                return processes.Process(pid, parent, 1, exe, str(pid), [exe, str(root / arg)])
            rows = [row(1, "/usr/bin/vim", "README.md"), row(2, "/usr/bin/less", "x"),
                    row(3, "/usr/bin/node", "README.md"), row(4, "/usr/bin/node", "dist/main.js"),
                    row(5, "/usr/bin/bun", "index.js"), row(6, "/usr/bin/python3", "x", 4)]
            self.assertEqual([p.pid for p in processes.family(install, rows)], [4, 5, 6])

    def test_updater_environment_strips_dashboard_secrets_but_keeps_cli_auth(self):
        values = {"CCS_SESSION_SECRET": "private", "CCS_DASHBOARD_PASSWORD_HASH": "private", "OTHER_PASSWORD_HASH": "private", "OPENAI_API_KEY": "cli-auth"}
        with mock.patch.dict(os.environ, values, clear=True):
            self.assertEqual(common.environment()["OPENAI_API_KEY"], "cli-auth")
            self.assertFalse(set(values) - {"OPENAI_API_KEY"} & common.environment().keys())
        context = processes.Process(1, 0, 1, "/fixture", "1", env=values)
        self.assertEqual(terminal.restart_environment(context)["OPENAI_API_KEY"], "cli-auth")
        with mock.patch.object(terminal.shutil, "which", return_value="/fixture/systemd-run"):
            argv = terminal.systemd_user_service_argv("fixture", "/tmp", values, ["fixture"])
        self.assertIn("--setenv=OPENAI_API_KEY", argv)
        self.assertNotIn("private", " ".join(argv))
        self.assertNotIn("cli-auth", " ".join(argv))
        self.assertFalse(any("CCS_SESSION_SECRET" in arg for arg in argv))

    def test_mac_inventory_filters_user_and_rejects_oversize(self):
        with mock.patch.object(processes, "command", side_effect=common.UpdateFailure()) as command:
            with self.assertRaises(common.UpdateFailure):
                processes.mac_processes()
        argv = command.call_args.args[0]
        self.assertEqual(argv[:3], ["/bin/ps", "-U", str(os.getuid())])
        self.assertEqual(command.call_args.kwargs["capture_limit"], 2 * 1024 * 1024)

    def test_npm_semver_does_not_change_vendor_comparisons(self):
        ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1"]
        self.assertEqual(sorted(reversed(ordered), key=common.npm_version_key), ordered)
        self.assertEqual(common.npm_version_key("1.0.0+build"), common.npm_version_key("1.0.0"))
        self.assertEqual(common.version_tuple("1.0.0-beta.1"), common.version_tuple("1.0.0"))

    def test_codex_store_unknown_never_trusts_old_direct_manifest(self):
        install = common.Install("codex-desktop", "windows", pathlib.Path("/fixture"), "26.1.0.0", "msix")
        with mock.patch.object(desktop, "codex_store_version", return_value=None), mock.patch.object(desktop, "remote_msix_identity") as direct:
            value = desktop.update_windows(install)
        self.assertEqual((value["status"], value["messageCode"]), ("unknown", "readiness_unknown"))
        direct.assert_not_called()

    def test_failed_msix_probes_and_successful_absence_are_distinct(self):
        for answer in (common.UpdateFailure(), "not-json", "{}", "[]"):
            with self.subTest(answer=answer), mock.patch.object(desktop, "powershell", **({"side_effect": answer} if isinstance(answer, Exception) else {"return_value": answer})):
                self.assertEqual(desktop.windows_package("codex-desktop").probe, "failed")
        with mock.patch.object(desktop, "powershell", return_value=""):
            self.assertIsNone(desktop.windows_package("codex-desktop"))

    def test_existing_linux_desktop_with_failed_package_probe_is_unknown(self):
        with mock.patch.object(pathlib.Path, "is_file", return_value=True), mock.patch.object(desktop, "command", side_effect=common.UpdateFailure()):
            self.assertEqual(desktop.detect_desktop("codex-desktop", "ubuntu").probe, "failed")

    def test_unreadable_windows_codex_package_is_unknown(self):
        with tempfile.TemporaryDirectory() as folder:
            shim = pathlib.Path(folder) / "codex.cmd"
            shim.touch()
            with mock.patch.object(updater, "_candidates", return_value=[shim]):
                install = updater.detect_cli("codex-cli", "windows")
            self.assertEqual(install.probe, "failed")

    def test_unknown_detection_never_updates(self):
        installs = {app: common.Install(app, "ubuntu", None, probe="failed") for app in common.APP_LABELS}
        with mock.patch.object(updater, "execution_lock", contextlib.nullcontext), mock.patch.object(updater, "detect", return_value=installs), mock.patch.object(updater, "update_cli") as update:
            rows = updater.run_apply("ubuntu")["results"]
        update.assert_not_called()
        self.assertTrue(all(row["status"] == "unknown" and row["messageCode"] == "readiness_unknown" for row in rows))

    def test_partial_adapter_update_fails_only_for_unfinished_required_parts(self):
        names = list(zcode.PACKAGES)
        install = zcode.AdaptersInstall("t3-acp-adapters", "ubuntu", pathlib.Path("/fixture"), parts={name: "1.0.0" for name in names})
        after = {names[0]: "2.0.0", names[1]: "1.0.0"}
        with mock.patch.object(zcode, "read_parts", return_value=after):
            row = zcode.adapters_outcome(install, "timeout", behind=names)
            self.assertEqual((row["status"], row["messageCode"]), ("failed", "timeout"))
            self.assertEqual(zcode.adapters_outcome(install, "timeout", behind=names[:1])["status"], "updated")
            self.assertEqual(len(row["parts"]), 2)

    def test_windows_timeout_keeps_completed_rows_and_task_busy_is_busy(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
            def start(script, **kwargs):
                if "Start-ScheduledTask" in script:
                    root = pathlib.Path(folder) / ".ccs/app-updates"
                    nonce = json.loads((root / "windows-task-request.json").read_text())["nonce"]
                    common.write_private_json(root / "windows-task-progress.json", {"nonce": nonce, "events": [{"event": "result", "result": common.result("omp", "windows", "updated")}]})
                return "0"
            with mock.patch.object(updater, "powershell", side_effect=start), mock.patch.object(updater.time, "monotonic", side_effect=[0, 0, 1000]), mock.patch.object(updater.time, "sleep"):
                rows = updater.windows_interactive_apply()["results"]
            self.assertEqual(next(row for row in rows if row["appId"] == "omp")["status"], "updated")
            self.assertTrue(all(row["messageCode"] == "host_timeout" for row in rows if row["appId"] != "omp"))
            with mock.patch.object(updater, "powershell", side_effect=common.UpdateFailure(exit_code=3)), self.assertRaises(common.UpdateFailure) as raised:
                updater.windows_interactive_apply()
            self.assertEqual(raised.exception.code, "busy")

    def test_windows_exited_task_reports_missing_results_without_waiting_for_timeout(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
            def shell(script, **kwargs):
                if "Start-ScheduledTask" in script:
                    root = pathlib.Path(folder) / ".ccs/app-updates"
                    nonce = json.loads((root / "windows-task-request.json").read_text())["nonce"]
                    common.write_private_json(root / "windows-task-progress.json", {"nonce": nonce, "events": [{"event": "result", "result": common.result("omp", "windows", "updated")}]})
                return "finished" if "'finished'" in script else "638000000000000000"
            with mock.patch.object(updater, "powershell", side_effect=shell) as command, mock.patch.object(updater.time, "monotonic", side_effect=[0, 5]), mock.patch.object(updater.time, "sleep") as sleep:
                rows = updater.windows_interactive_apply()["results"]
            self.assertEqual(next(row for row in rows if row["appId"] == "omp")["status"], "updated")
            self.assertTrue(all(row["status"] == "unknown" and row["messageCode"] == "task_failed" for row in rows if row["appId"] != "omp"))
            self.assertIn("LastRunTime.Ticks -ne 638000000000000000", command.call_args.args[0])
            sleep.assert_not_called()

    def test_windows_completed_task_final_file_wins_a_task_state_race(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
            def shell(script, **kwargs):
                if "'finished'" in script:
                    root = pathlib.Path(folder) / ".ccs/app-updates"
                    nonce = json.loads((root / "windows-task-request.json").read_text())["nonce"]
                    common.write_private_json(root / "windows-task-result.json", {"nonce": nonce, "results": [common.result("omp", "windows", "current")]})
                    return "finished"
                return "0"
            with mock.patch.object(updater, "powershell", side_effect=shell), mock.patch.object(updater.time, "monotonic", side_effect=[0, 5]):
                rows = updater.windows_interactive_apply()["results"]
            self.assertEqual([(row["appId"], row["status"]) for row in rows], [("omp", "current")])

    def test_windows_stale_task_result_does_not_end_a_new_run(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
            root = pathlib.Path(folder) / ".ccs/app-updates"
            common.write_private_json(root / "windows-task-result.json", {"nonce": "old", "results": [common.result("omp", "windows", "current")]})
            with mock.patch.object(updater, "powershell", side_effect=lambda script, **k: "" if "'finished'" in script else "0"), mock.patch.object(updater.time, "monotonic", side_effect=[0, 5, 1000]), mock.patch.object(updater.time, "sleep"):
                rows = updater.windows_interactive_apply()["results"]
            self.assertTrue(all(row["messageCode"] == "host_timeout" for row in rows))

    def test_windows_exited_task_keeps_progress_written_during_the_state_query(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
            def shell(script, **kwargs):
                if "'finished'" in script:
                    root = pathlib.Path(folder) / ".ccs/app-updates"
                    nonce = json.loads((root / "windows-task-request.json").read_text())["nonce"]
                    common.write_private_json(root / "windows-task-progress.json", {"nonce": nonce, "events": [{"event": "result", "result": common.result("omp", "windows", "updated")}]})
                    return "finished"
                return "0"
            events = []
            with mock.patch.object(updater, "powershell", side_effect=shell), mock.patch.object(updater.time, "monotonic", side_effect=[0, 5]):
                rows = updater.windows_interactive_apply(events.append)["results"]
            self.assertEqual(next(row for row in rows if row["appId"] == "omp")["status"], "updated")
            self.assertEqual([event["result"]["appId"] for event in events], ["omp"])
            self.assertTrue(all(row["messageCode"] == "task_failed" for row in rows if row["appId"] != "omp"))

    def test_windows_unreadable_run_baseline_never_queues_the_task(self):
        for baseline in ("", "not-a-time", "-1", None):
            with self.subTest(baseline=baseline), tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)), mock.patch.object(updater, "execution_lock", contextlib.nullcontext):
                with mock.patch.object(updater, "powershell", return_value=baseline) as command, self.assertRaises(common.UpdateFailure) as raised:
                    updater.windows_interactive_apply()
                self.assertEqual(raised.exception.code, "readiness_unknown")
                self.assertEqual(command.call_count, 1)
                self.assertFalse((pathlib.Path(folder) / ".ccs/app-updates/windows-task-request.json").exists())

    def test_desktop_budget_reserves_recovery_and_never_starts_at_limit(self):
        with mock.patch.object(desktop.time, "monotonic", return_value=100):
            self.assertEqual(desktop.desktop_budget(250, 900), 60)
            with self.assertRaises(common.UpdateFailure):
                desktop.desktop_budget(190, 900)

    def test_parent_permission_denied_keeps_restart_waiting(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            (root / "t3-code-pending-restart.json").write_text('{"version":"1.0.0"}')
            with mock.patch.object(t3.time, "monotonic", side_effect=[0, 0, 2000]), mock.patch.object(t3.time, "sleep"), mock.patch.object(t3.os, "kill", side_effect=PermissionError()), mock.patch.object(t3, "execution_lock", contextlib.nullcontext), mock.patch.object(t3, "command") as command:
                with self.assertRaises(common.UpdateFailure):
                    t3.deferred_restart(123, root)
            command.assert_not_called()

    def test_lock_identity_names_start_and_boot(self):
        identity = common.process_identity(os.getpid())
        self.assertIsNotNone(identity)
        self.assertEqual(identity, common.process_identity(os.getpid()))

    def test_t3_download_refusal_uses_a_supported_result_code(self):
        install = t3.T3Install("t3-code", "mac", pathlib.Path("/fixture"), "1.0.0", desktop=True)
        with mock.patch.object(t3, "latest_release", side_effect=common.UpdateFailure("download_blocked")):
            value = t3.update_t3(install, 1000)
        self.assertEqual((value["status"], value["messageCode"]), ("failed", "update_failed"))

    def test_main_keeps_streamed_rows_after_an_unexpected_coordinator_failure(self):
        def fail(platform, emit, cancelled, reviewed):
            emit({"event": "result", "result": common.result("omp", platform, "current")})
            raise common.UpdateFailure("timeout")
        output = io.StringIO()
        with mock.patch.object(updater, "run_apply", side_effect=fail), mock.patch.object(sys, "argv", ["helper", "--apply", "--platform", "ubuntu"]), contextlib.redirect_stdout(output):
            updater.main()
        rows = json.loads(output.getvalue())["results"]
        self.assertEqual(next(row for row in rows if row["appId"] == "omp")["status"], "current")
        self.assertTrue(all(row["messageCode"] == "host_timeout" for row in rows if row["appId"] != "omp"))

    def test_pending_desktop_restart_uses_installed_version_when_candidate_is_older(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(folder)):
            pending = pathlib.Path(folder) / ".ccs/app-updates/codex-desktop-pending-restart.json"
            common.write_private_json(pending, {"version": "2.0.0"})
            install = common.Install("codex-desktop", "ubuntu", pathlib.Path("/fixture"), "2.0.0", "apt", "chatgpt")
            calls = []
            def command(argv, **kwargs):
                calls.append(argv)
                if "policy" in argv:
                    return "Candidate: 1.0.0\n 500 https://persistent.oaistatic.com/codex-app-prod/linux/deb stable/main amd64 Packages"
                if "madison" in argv:
                    return "chatgpt | 1.0.0 | https://persistent.oaistatic.com/codex-app-prod/linux/deb stable/main amd64 Packages"
                return '{"status":"updated"}'
            with mock.patch.object(desktop, "command", side_effect=command):
                self.assertEqual(desktop.update_linux(install)["status"], "updated")
            bridge = calls[-1]
            self.assertEqual(bridge[bridge.index("--expected-version") + 1], "2.0.0")
            self.assertEqual(bridge[bridge.index("--restart-only") + 1], "true")
