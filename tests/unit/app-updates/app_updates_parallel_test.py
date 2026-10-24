"""Offline updater scheduling, metadata recovery and package-deployment regressions."""
import contextlib
import ctypes
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
import app_update_pipe as pipes
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

    def test_metadata_child_deadline_covers_http_headers_chunks_trailers_and_body(self):
        for case in ("complete", "body", "headers", "chunk-extension", "trailer"):
            with self.subTest(case=case):
                server = socket.socket()
                server.bind(("127.0.0.1", 0))
                server.listen(1)
                server.settimeout(2)
                stop, received = threading.Event(), threading.Event()
                children = []
                real_popen = common.subprocess.Popen
                def spawn(*args, **kwargs):
                    child = real_popen(*args, **kwargs)
                    children.append(child)
                    return child
                def serve():
                    try:
                        connection, _ = server.accept()
                        with connection:
                            connection.settimeout(2)
                            connection.recv(8192)
                            received.set()
                            if case == "complete":
                                connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
                                return
                            prefix = {
                                "body": b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\n",
                                "headers": b"HTTP/1.1 200 OK\r\nX-Slow: ",
                                "chunk-extension": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1;extension=",
                                "trailer": b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n0\r\nX-Trailer: ",
                            }[case]
                            connection.sendall(prefix)
                            for _ in range(40):
                                if stop.wait(.05):
                                    return
                                connection.sendall(b"x")
                            connection.sendall(b"\r\n\r\n")
                    except OSError:
                        pass
                thread = threading.Thread(target=serve)
                thread.start()
                url = "http://127.0.0.1:%d/metadata" % server.getsockname()[1]
                try:
                    started = time.monotonic()
                    with mock.patch.object(common.subprocess, "Popen", side_effect=spawn):
                        if case == "complete":
                            self.assertEqual(zcode.read_bounded(url, "http://127.0.0.1:", 1024, deadline=started + .6), b"abc")
                        else:
                            with self.assertRaisesRegex(common.UpdateFailure, "timeout"):
                                zcode.read_bounded(url, "http://127.0.0.1:", 1024, deadline=started + .6)
                    self.assertLess(time.monotonic() - started, 1.2, "the complete operation must stop before a two-second drip finishes")
                    self.assertTrue(received.is_set())
                    self.assertEqual(len(children), 1)
                    self.assertIsNotNone(children[0].poll(), "the metadata child must be reaped")
                finally:
                    stop.set()
                    server.close()
                    thread.join(3)
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

    def test_forced_background_stop_never_signals_its_console_group(self):
        kernel = mock.Mock()
        kernel.OpenProcess.return_value = 123
        kernel.GetProcessTimes.side_effect = lambda handle, created, *args: (setattr(created._obj, "low", 1), 1)[-1]
        kernel.TerminateProcess.return_value = 1
        item = processes.Process(11, 0, 0, "/fixture/codex", "1")
        install = common.Install("codex-cli", "windows", pathlib.Path(item.exe))
        with mock.patch.object(processes.ctypes, "windll", types.SimpleNamespace(kernel32=kernel), create=True), \
                mock.patch.object(processes, "live_contexts", side_effect=[[item], [item], [item], []]):
            self.assertEqual(processes.terminate_cli(install, [item], graceful=False), 1)
        kernel.GenerateConsoleCtrlEvent.assert_not_called()
        kernel.AttachConsole.assert_not_called()
        kernel.TerminateProcess.assert_called_once()

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

    def test_concurrent_private_pipe_creation_uses_a_stable_ctypes_security_type(self):
        barrier = threading.Barrier(2)
        errors, created, invalid = [], [], []
        callback_type = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_wchar_p, *([pipes.wintypes.DWORD] * 6), ctypes.c_void_p)
        def create(endpoint, mode, kind, instances, out_size, in_size, timeout, attributes):
            value = ctypes.cast(attributes, ctypes.POINTER(pipes.SecurityAttributes)).contents
            if value.descriptor != 789 or value.inherit or value.length != ctypes.sizeof(pipes.SecurityAttributes):
                invalid.append("invalid security attributes")
            return 123
        native_create = callback_type(create)
        class Create:
            @property
            def argtypes(self):
                return native_create.argtypes
            @argtypes.setter
            def argtypes(self, value):
                native_create.argtypes = value
                barrier.wait(2)
            def __call__(self, *args):
                return native_create(*args)
        kernel, advapi = mock.Mock(), mock.Mock()
        kernel.GetCurrentProcess.return_value = 1
        kernel.CreateNamedPipeW = Create()
        advapi.OpenProcessToken.side_effect = lambda process, access, token: (setattr(token._obj, "value", 456), 1)[-1]
        def token_info(token, kind, data, size, needed):
            needed._obj.value = ctypes.sizeof(ctypes.c_void_p)
            if data is not None:
                ctypes.c_void_p.from_buffer(data).value = 555
            return 1
        advapi.GetTokenInformation.side_effect = token_info
        advapi.ConvertSidToStringSidW.side_effect = lambda sid, text: (setattr(text._obj, "value", "S-1-5-21-123"), 1)[-1]
        advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.side_effect = lambda descriptor, version, security, size: (setattr(security._obj, "value", 789), 1)[-1]
        def run(nonce):
            try:
                created.append(pipes.PrivatePipe(nonce))
            except Exception as error:
                errors.append(error)
        with mock.patch.object(pipes.ctypes, "windll", types.SimpleNamespace(kernel32=kernel, advapi32=advapi), create=True):
            threads = [threading.Thread(target=run, args=(digit * 32,)) for digit in ("1", "2")]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(3)
            self.assertTrue(all(not thread.is_alive() for thread in threads))
            for pipe in created:
                pipe.close()
        self.assertEqual(errors, [])
        self.assertEqual(invalid, [])
        self.assertEqual(len(created), 2)

    def test_msix_quiesces_only_the_target_package(self):
        with mock.patch.object(desktop, "powershell", return_value="CCS_ADDAPPX_OK") as command:
            desktop.add_appx_package(pathlib.Path("/fixture/update.msix"), timeout=321)
        script = command.call_args.args[0]
        self.assertIn("-ForceTargetApplicationShutdown", script)
        self.assertNotIn("-ForceApplicationShutdown", script)
        self.assertEqual(command.call_args.kwargs["timeout"], 321)

    def test_windows_download_budget_keeps_time_for_install_and_reopen(self):
        reserve = 90 + desktop.WINDOWS_REPLACE_SECONDS
        with mock.patch.object(desktop.time, "monotonic", return_value=100), mock.patch.object(desktop, "download") as download:
            desktop.download_desktop("https://fixture.invalid/update.msix", pathlib.Path("/fixture/package"), 100 + reserve + 120, reserve)
            self.assertEqual(download.call_args.kwargs["timeout"], 120)
            download.reset_mock()
            with self.assertRaisesRegex(common.UpdateFailure, "timeout"):
                desktop.download_desktop("https://fixture.invalid/update.msix", pathlib.Path("/fixture/package"), 100 + reserve, reserve)
            download.assert_not_called()


if __name__ == "__main__":
    unittest.main()
