"""Offline updater scheduling, metadata recovery and package-deployment regressions."""
import contextlib
import ctypes
import http.client
import pathlib
import socket
import sys
import tempfile
import threading
import time
import types
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_common as common
import app_update_desktop as desktop
import app_update_processes as processes
import app_update_zcode as zcode
import app_updates as updater


class ParallelUpdateTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="aac-parallel-fixture-")
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name)
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=self.root))
        self.stack.enter_context(mock.patch.object(updater, "execution_lock", contextlib.nullcontext))
        self.stack.enter_context(mock.patch.object(updater, "check_readiness", return_value=None))
        self.stack.enter_context(mock.patch.object(updater, "antigravity_plan", return_value=(None, False)))
        self.stack.enter_context(mock.patch.object(updater, "claude_current", return_value=None))
        self.stack.enter_context(mock.patch.object(updater, "ubuntu_unit_busy", return_value=False))
        self.installs = {key: common.Install(key, "windows", self.root / key, "1.0.0") for key in common.APP_LABELS}
        for key in ("codex-cli", "t3-acp-adapters"):
            self.installs[key].manager = "npm"
        for key in ("claude-desktop", "codex-desktop"):
            self.installs[key].manager = "msix"
        self.stack.enter_context(mock.patch.object(updater, "detect", return_value=self.installs))

    def workers(self, worker):
        for name in ("update_cli", "update_desktop", "update_zcode", "update_adapters", "update_t3"):
            self.stack.enter_context(mock.patch.object(updater, name, side_effect=worker))

    def gated_run(self, cancelled=False):
        release, entered = threading.Event(), threading.Event()
        lock, emitting = threading.Lock(), threading.Lock()
        state = {"active": 0, "peak": 0, "cancelled": False}
        started, events, answer, errors = [], [], [], []

        def worker(install, deadline, *args, **kwargs):
            with lock:
                started.append(install.app_id)
                state["active"] += 1
                state["peak"] = max(state["peak"], state["active"])
                if state["active"] == updater.MAX_PARALLEL_UPDATES:
                    entered.set()
            if not release.wait(3):
                raise AssertionError("independent updates did not overlap")
            with lock:
                state["active"] -= 1
            return common.result(install.app_id, "windows", "current", "1.0.0", "1.0.0", install.manager)

        def emit(event):
            if not emitting.acquire(blocking=False):
                errors.append("concurrent progress write")
                return
            try:
                events.append(event)
                time.sleep(.001)
            finally:
                emitting.release()

        def run():
            try:
                answer.append(updater.run_apply("windows", emit=emit, cancelled=lambda: state["cancelled"]))
            except Exception as error:
                errors.append(error)

        self.workers(worker)
        thread = threading.Thread(target=run)
        thread.start()
        try:
            self.assertTrue(entered.wait(2), "four independent installers should start before any finishes")
            state["cancelled"] = cancelled
        finally:
            release.set()
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(state["peak"], updater.MAX_PARALLEL_UPDATES)
        return answer[0]["results"], started, events

    def test_independent_updates_overlap_with_bounded_workers_and_serial_progress(self):
        rows, started, events = self.gated_run()
        self.assertEqual(len(rows), len(common.APP_LABELS))
        self.assertEqual({row["status"] for row in rows}, {"current"})
        self.assertEqual(started[-1], "t3-code")
        self.assertEqual(len([event for event in events if event["event"] == "result"]), len(rows))
        active, visible = set(), None
        for index, event in enumerate(events):
            if event["event"] == "app" and event.get("appId"):
                active.add(event["appId"])
                visible = event["appId"]
            elif event["event"] == "result":
                app_id = event["result"]["appId"]
                active.discard(app_id)
                if app_id == visible and active:
                    self.assertEqual(events[index + 1]["event"], "app")
                    self.assertIn(events[index + 1]["appId"], active)

    def test_cancel_finishes_active_updates_and_skips_every_queued_update(self):
        rows, started, _ = self.gated_run(cancelled=True)
        self.assertEqual(len(started), updater.MAX_PARALLEL_UPDATES)
        self.assertEqual(sum(row["status"] == "current" for row in rows), len(started))
        self.assertEqual(sum(row["status"] == "skipped" for row in rows), len(rows) - len(started))
        self.assertNotIn("t3-code", started)

    def test_shared_installers_and_codex_are_serialized_and_t3_waits_for_every_app(self):
        active, finished, violations = set(), set(), []
        lock = threading.Lock()

        def worker(install, deadline, *args, **kwargs):
            app_id = install.app_id
            resources = updater.update_resources(install)
            with lock:
                if any(resources & updater.update_resources(self.installs[other]) for other in active):
                    violations.append("shared installer overlap")
                if app_id == "codex-desktop" and "codex-cli" not in finished:
                    violations.append("desktop before CLI")
                if app_id == "t3-acp-adapters" and "zcode" not in finished:
                    violations.append("adapters before ZCode")
                if app_id == "t3-code" and (active or len(finished) != len(common.APP_LABELS) - 1):
                    violations.append("T3 before barrier")
                active.add(app_id)
            time.sleep(.01)
            with lock:
                active.remove(app_id)
                finished.add(app_id)
            return common.result(app_id, "windows", "current", "1.0.0", "1.0.0", install.manager)

        self.workers(worker)
        rows = updater.run_apply("windows")["results"]
        self.assertEqual(violations, [])
        self.assertEqual({row["status"] for row in rows}, {"current"})

    def test_failed_app_does_not_lose_another_apps_result(self):
        def worker(install, deadline, *args, **kwargs):
            if install.app_id == "omp":
                raise OSError("fixture failure")
            return common.result(install.app_id, "windows", "current", "1.0.0", "1.0.0", install.manager)
        self.workers(worker)
        rows = updater.run_apply("windows")["results"]
        self.assertEqual([row["appId"] for row in rows if row["status"] == "failed"], ["omp"])
        self.assertEqual(sum(row["status"] == "current" for row in rows), len(common.APP_LABELS) - 1)

    def test_unsettled_ubuntu_oneshot_blocks_npm_and_t3_without_stopping_the_unit(self):
        zcode.UBUNTU_RUN.clear()
        self.addCleanup(zcode.UBUNTU_RUN.clear)
        with mock.patch.object(zcode, "command", side_effect=common.UpdateFailure("timeout")):
            self.assertEqual(zcode.run_unit(time.monotonic() + 600), "timeout")
        with mock.patch.object(zcode, "command", return_value="start"):
            self.assertTrue(zcode.ubuntu_unit_busy())
        with mock.patch.object(zcode, "command", return_value="dead"):
            self.assertFalse(zcode.ubuntu_unit_busy())
        self.installs = {key: None for key in common.APP_LABELS}
        for key in ("codex-cli", "t3-acp-adapters", "t3-code"):
            self.installs[key] = common.Install(key, "ubuntu", self.root / key, "1.0.0", "npm" if key != "t3-code" else "native")
        with mock.patch.object(updater, "detect", return_value=self.installs), mock.patch.object(updater, "ubuntu_unit_busy", return_value=True), \
                mock.patch.object(updater, "update_cli") as cli, mock.patch.object(updater, "update_t3") as t3:
            rows = updater.run_apply("ubuntu")["results"]
        cli.assert_not_called()
        t3.assert_not_called()
        self.assertEqual({row["messageCode"] for row in rows if self.installs[row["appId"]]}, {"busy"})


class UpdateRecoveryTests(unittest.TestCase):
    def test_new_background_session_after_unknown_registry_check_is_never_stopped(self):
        with tempfile.TemporaryDirectory(prefix="aac-force-race-") as folder:
            root = pathlib.Path(folder)
            install = common.Install("codex-cli", "windows", root / "npm/codex.cmd", "1.0.0", "npm", package_root=root / "npm/node_modules/@openai/codex")
            terminal = processes.Process(11, 0, 0, "/fixture/codex", "11")
            background = processes.Process(22, 0, 0, "/fixture/codex", "22")
            with mock.patch.object(pathlib.Path, "home", return_value=root), mock.patch.object(updater, "scan", return_value=[]), \
                    mock.patch.object(updater, "cli_instances", side_effect=[([terminal], [terminal], []), ([terminal], [terminal], [background])]), \
                    mock.patch.object(updater, "check_terminal"), mock.patch.object(updater, "npm_view_latest", return_value=None), \
                    mock.patch.object(updater, "write_private_json") as marker, mock.patch.object(updater, "terminate_cli") as stop, \
                    mock.patch.object(updater, "perform_cli_update") as install_package:
                row = updater.update_cli(install, time.monotonic() + 60)
            self.assertEqual((row["status"], row["messageCode"]), ("unknown", "readiness_unknown"))
            stop.assert_not_called()
            marker.assert_not_called()
            install_package.assert_not_called()

    def test_zcode_recovers_a_transient_lookup_and_never_claims_current_after_failure(self):
        body = b"releases/3.16.3/linux-x64/latest.yml"
        with mock.patch.object(zcode, "read_bounded", side_effect=[common.UpdateFailure(), body]) as read, mock.patch.object(zcode.time, "sleep"):
            self.assertEqual(zcode.latest_zcode("linux-x64"), "3.16.3")
        self.assertEqual(read.call_count, 2)
        with mock.patch.object(zcode, "read_bounded", side_effect=common.UpdateFailure()) as read, mock.patch.object(zcode.time, "sleep"):
            with self.assertRaises(common.UpdateFailure):
                zcode.latest_zcode("linux-x64")
        self.assertEqual(read.call_count, zcode.PAGE_ATTEMPTS)

    def test_zcode_does_not_start_another_attempt_after_the_total_lookup_deadline(self):
        clock = {"now": 100.0}
        def read(*args, **kwargs):
            clock["now"] += 61
            raise common.UpdateFailure("timeout")
        with mock.patch.object(zcode, "read_bounded", side_effect=read) as fetch, \
                mock.patch.object(zcode.time, "monotonic", side_effect=lambda: clock["now"]), mock.patch.object(zcode.time, "sleep") as sleep:
            with self.assertRaisesRegex(common.UpdateFailure, "timeout"):
                zcode.latest_zcode("linux-x64", deadline=105)
        fetch.assert_called_once()
        self.assertEqual(fetch.call_args.kwargs["deadline"], 105)
        sleep.assert_not_called()

    def test_zcode_dripping_http_body_cannot_reset_the_total_deadline(self):
        reader, writer = socket.socketpair()
        stop = threading.Event()
        def drip():
            try:
                writer.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n")
                for _ in range(40):
                    if stop.wait(.02):
                        return
                    writer.sendall(b"x")
            except OSError:
                pass
        thread = threading.Thread(target=drip)
        thread.start()
        response = http.client.HTTPResponse(reader, url=zcode.PAGE)
        response.url = zcode.PAGE  # urllib sets this after constructing HTTPResponse.
        try:
            response.begin()
            started = time.monotonic()
            with mock.patch.object(zcode.urllib.request, "urlopen", return_value=response):
                with self.assertRaisesRegex(common.UpdateFailure, "timeout"):
                    zcode.read_bounded(zcode.PAGE, "https://zcode.z.ai/", 1024, deadline=started + .12)
            self.assertLess(time.monotonic() - started, .4, "a buffered read would wait for the entire 800 ms body")
        finally:
            stop.set()
            response.close()
            reader.close()
            writer.close()
            thread.join(2)
        self.assertFalse(thread.is_alive())

    def test_parallel_windows_stops_do_not_interleave_console_attachment(self):
        state, operations = threading.local(), []
        def live(platform, targets):
            state.calls = getattr(state, "calls", 0) + 1
            return targets if state.calls == 1 else []
        kernel = mock.Mock()
        kernel.AttachConsole.side_effect = lambda pid: (operations.append(("attach", pid)), time.sleep(.02), True)[-1]
        kernel.GenerateConsoleCtrlEvent.side_effect = lambda event, pid: operations.append(("signal", pid))
        install = common.Install("codex-cli", "windows", pathlib.Path("/fixture/codex"))
        barrier = threading.Barrier(2)
        errors = []
        def stop(pid):
            try:
                barrier.wait(2)
                processes.terminate_cli(install, [processes.Process(pid, 0, 0, "/fixture/codex", str(pid))])
            except Exception as error:
                errors.append(error)
        with mock.patch.object(processes, "live_contexts", side_effect=live), \
                mock.patch.object(processes.ctypes, "windll", types.SimpleNamespace(kernel32=kernel), create=True):
            threads = [threading.Thread(target=stop, args=(pid,)) for pid in (11, 22)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(3)
            self.assertTrue(all(not thread.is_alive() for thread in threads))
        self.assertEqual(errors, [])
        self.assertEqual([kind for kind, _ in operations], ["attach", "signal", "attach", "signal"])
        self.assertEqual(operations[0][1], operations[1][1])
        self.assertEqual(operations[2][1], operations[3][1])

    def test_concurrent_windows_identity_read_and_forced_stop_share_a_stable_ctypes_signature(self):
        # Use real ctypes argument conversion, not a Mock that accepts pointers
        # to incompatible, newly-created local FILETIME classes.
        barrier = threading.Barrier(2)
        callback_type = ctypes.CFUNCTYPE(ctypes.c_int, *([ctypes.c_void_p] * 5))
        def times_callback(handle, created, exited, system, user):
            ctypes.cast(created, ctypes.POINTER(processes.WindowsFileTime)).contents.low = 1
            return 1
        native_times = callback_type(times_callback)
        class Times:
            @property
            def argtypes(self):
                return native_times.argtypes
            @argtypes.setter
            def argtypes(self, values):
                native_times.argtypes = values
                barrier.wait(2)
            def __call__(self, *args):
                return native_times(*args)
        kernel = mock.Mock()
        kernel.OpenProcess.return_value = 123
        kernel.GetProcessTimes = Times()
        kernel.GetExitCodeProcess.side_effect = lambda handle, status: (setattr(status._obj, "value", 259), 1)[-1]
        kernel.TerminateProcess.return_value = 1
        item = processes.Process(11, 0, 0, "/fixture/codex", "1")
        install = common.Install("codex-cli", "windows", pathlib.Path(item.exe))
        real_live = processes.live_contexts
        errors, answers = [], []
        def run(call):
            try:
                answers.append(call())
            except Exception as error:
                errors.append(error)
        with mock.patch.object(processes.ctypes, "windll", types.SimpleNamespace(kernel32=kernel), create=True), \
                mock.patch.object(processes, "live_contexts", side_effect=[[item], [item], [item], []]):
            threads = [threading.Thread(target=run, args=(lambda: real_live("windows", [item]),)),
                       threading.Thread(target=run, args=(lambda: processes.terminate_cli(install, [item], grace=0),))]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(3)
            self.assertTrue(all(not thread.is_alive() for thread in threads))
        self.assertEqual(errors, [])
        self.assertIn([item], answers)
        self.assertIn(1, answers)
        kernel.TerminateProcess.assert_called_once()

    def test_msix_quiesces_only_the_target_package(self):
        with mock.patch.object(desktop, "powershell", return_value="CCS_ADDAPPX_OK") as command:
            desktop.add_appx_package(pathlib.Path("/fixture/update.msix"), timeout=321)
        script = command.call_args.args[0]
        self.assertIn("-ForceTargetApplicationShutdown", script)
        self.assertNotIn("-ForceApplicationShutdown", script)
        self.assertEqual(command.call_args.kwargs["timeout"], 321)


if __name__ == "__main__":
    unittest.main()
