"""Tests of the bounded single-app helper used by the Rust agent."""
import contextlib
import ctypes
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_updates as updates
import app_update_common as common

CHILD = r'''
import json, os, pathlib, subprocess, sys, time
root = pathlib.Path(__file__).parent
if "--descendant" in sys.argv:
    (root / "descendant-pid").write_text(str(os.getpid()))
    time.sleep(45)
else:
    print(json.dumps({"event":"app","appId":"codex-desktop","phase":"downloading"}), flush=True)
    child = subprocess.Popen([sys.executable, "-B", __file__, "--descendant"],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    child.wait()
'''


def running(pid):
    if os.name == "nt":
        kernel = ctypes.windll.kernel32
        open_process = kernel.OpenProcess
        open_process.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        open_process.restype = ctypes.c_void_p
        handle = open_process(0x00100000, False, pid)  # SYNCHRONIZE, no write access
        if not handle:
            return False
        try:
            wait = kernel.WaitForSingleObject
            wait.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
            wait.restype = ctypes.c_uint32
            return wait(handle, 0) == 258
        finally:
            close = kernel.CloseHandle
            close.argtypes = [ctypes.c_void_p]
            close(handle)
    if sys.platform == "darwin":
        info = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "state="],
                              capture_output=True, text=True, timeout=1)
        return bool(info.stdout.strip()) and "Z" not in info.stdout
    try:
        return pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[-1].split()[0] != "Z"
    except FileNotFoundError:
        return False


class AgentWorkerTest(unittest.TestCase):
    def test_cancel_reaps_the_child_tree(self):
        with tempfile.TemporaryDirectory(prefix="aac-worker-") as directory:
            root = pathlib.Path(directory)
            helper = root / "worker-fixture.py"
            helper.write_text(CHILD, encoding="ascii")
            source = root / "descendant-pid"
            stopping = None
            events = []
            app = "codex-desktop"
            platform = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "ubuntu"
            args = types.SimpleNamespace(app=app, platform=platform, worker_seconds=8,
                                         t3_restart_pending=False)

            def cancel():
                nonlocal stopping
                if source.is_file() and source.stat().st_size:
                    stopping = int(source.read_text())
                    self.assertTrue(running(stopping))
                    return True
                return False

            stage = root / "agent-staging" / app
            stage.mkdir(parents=True)
            (stage / ".aac-app-worker").write_bytes(b"aac app worker stage v1")
            (stage / "stale.part").write_bytes(b"old fixture")
            (root / "unrelated.part").write_bytes(b"preserved")
            with mock.patch.object(updates, "__file__", str(helper)), mock.patch.dict(os.environ, {"AAC_UPDATE_STATE_DIR": str(root)}):
                payload = updates.supervised_app(args, events.append, cancel)
            self.assertFalse(stage.exists())
            self.assertEqual((root / "unrelated.part").read_bytes(), b"preserved")
            self.assertIsNotNone(stopping)
            self.assertEqual([event["phase"] for event in events], ["downloading"])
            self.assertEqual(payload["results"][0]["messageCode"], "skipped_cancelled")
            for _ in range(80):
                if not running(stopping):
                    break
                time.sleep(.025)
            self.assertFalse(running(stopping), "owned descendant survived cancellation")

    def test_one_app_path_keeps_held_row_and_installer_arguments(self):
        platform = "ubuntu"
        app = "antigravity-cli"
        installed = common.Install(app, platform, None, "1.2.3", "native")
        expected = common.result(app, platform, "held", before="1.2.3", after="1.2.3",
                                 manager="native", code="held_for_review")
        expected["heldVersion"] = "1.2.4"
        events = []
        with contextlib.ExitStack() as stack:
            probe = stack.enter_context(mock.patch.object(updates, "_detect_one", return_value=installed))
            stack.enter_context(mock.patch.object(updates, "check_readiness", return_value=None))
            apply = stack.enter_context(mock.patch.object(updates, "update_cli", return_value=expected))
            result = updates.apply_single_app(app, platform, events.append, 10)
        probe.assert_called_once_with(app, platform)
        self.assertIs(apply.call_args.args[0], installed)
        self.assertEqual([event["phase"] for event in events], ["checking", "updating"])
        row = result["results"][0]
        self.assertEqual(row["heldVersion"], "1.2.4")
        self.assertEqual(row["status"], "held")


    def test_windows_interactive_task_accepts_one_app_and_a_bound_cancel(self):
        with tempfile.TemporaryDirectory(prefix="aac-task-fixture-") as directory:
            path = pathlib.Path(directory)
            root = path / ".ccs/app-updates"
            root.mkdir(parents=True)
            request_file = root / "windows-task-request.json"
            cancel_file = root / "windows-task-cancel.json"
            app = "codex-desktop"
            events = []
            started = False

            def powershell(command, **_):
                nonlocal started
                if "Start-ScheduledTask" not in command:
                    self.assertFalse(started)
                    return "123"
                started = True
                request = json.loads(request_file.read_text())
                self.assertEqual((request["appId"], request["workerSeconds"]), (app, 45))
                common.write_private_json(root / "windows-task-progress.json", {
                    "nonce":request["nonce"], "events":[{"event":"app","appId":app,"phase":"downloading"}]
                })
                return ""

            def sleep(_seconds):
                request = json.loads(request_file.read_text())
                self.assertEqual(json.loads(cancel_file.read_text()), {"nonce":request["nonce"]})
                common.write_private_json(root / "windows-task-result.json", {
                    "nonce":request["nonce"], "results":[common.result(
                        app, "windows", "skipped", code="skipped_cancelled")]
                })

            with contextlib.ExitStack() as stack:
                stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=path))
                stack.enter_context(mock.patch.object(updates, "execution_lock", contextlib.nullcontext))
                stack.enter_context(mock.patch.object(updates, "powershell", powershell))
                stack.enter_context(mock.patch.object(updates.time, "sleep", sleep))
                answer = updates.windows_interactive_apply(events.append, lambda:True, app=app, budget=45)
            self.assertEqual([event["phase"] for event in events], ["downloading"])
            self.assertEqual(len(answer["results"]), 1)
            self.assertEqual(answer["results"][0]["messageCode"], "skipped_cancelled")


if __name__ == "__main__":
    unittest.main()
