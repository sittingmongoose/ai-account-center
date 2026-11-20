"""Antigravity ACP after a Linux computer's deferred T3 restart (Update all, 2026-11-20).

On the Linux computers T3's new server starts only after the job's part for that
computer ends, so the run's Antigravity ACP row said "checked on the next Update
apps" and nothing checked it again. The dashboard now runs the helper once more
with --acp-after-t3-restart: it waits, without the helper lock the restart
worker needs, for the new T3 to answer, then runs the same guarded install
through T3 and returns that row.
"""

import contextlib
import io
import json
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401  (discovery stays inside each test's home)
import app_update_common as common
import app_update_t3_rpc as t3_rpc
import app_updates as updater

NEW_T3 = "0.1.12-nightly.20261120.4994"


class Clock:
    def __init__(self):
        self.now = 1000.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FollowUpTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.events, self.ready, self.updates = [], [], []
        self.lock_held = False
        self.lock_busy = 0
        self.ready_after = 0
        self.install = common.Install("antigravity-acp", "ubuntu", pathlib.Path("/fixture/acp"), "1.5.12",
                                      "official-download", runtime_kind="native",
                                      owner={"manager": "t3", "runtime": "native"})
        stack = contextlib.ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(mock.patch.object(updater, "detect_antigravity_acp", side_effect=lambda host: self.install))
        stack.enter_context(mock.patch.object(t3_rpc, "wait_ready", side_effect=self.wait_ready))
        stack.enter_context(mock.patch.object(updater, "execution_lock", self.execution_lock))
        stack.enter_context(mock.patch.object(updater, "update_antigravity_acp", side_effect=self.update))

    def wait_ready(self, host, deadline, expected=None, seconds=None, **kwargs):
        # The restart worker takes the helper lock to restart T3: waiting here must never hold it.
        self.assertFalse(self.lock_held, "waited for T3 while holding the helper lock")
        self.ready.append((expected, seconds))
        self.clock.now += seconds
        if len(self.ready) <= self.ready_after:
            raise common.UpdateFailure("t3_not_ready", cause="t3_not_ready", reason="previous_server")
        return 1

    @contextlib.contextmanager
    def execution_lock(self, name="helper-update.lock"):
        if self.lock_busy:
            self.lock_busy -= 1
            raise common.UpdateFailure("busy")
        self.lock_held = True
        try:
            yield
        finally:
            self.lock_held = False

    def update(self, install, deadline=None, progress=None, t3_row=None, expected_t3=None):
        self.assertTrue(self.lock_held, "installed without the helper lock")
        self.updates.append((install.version, t3_row, expected_t3))
        common.diagnostics().phase("install")
        return common.result("antigravity-acp", "ubuntu", "updated", "1.5.12", "1.5.13", "official-download",
                             "antigravity_acp_updated", True)

    def follow_up(self, expected=NEW_T3, cancelled=None, seconds=None):
        arguments = {"seconds": seconds} if seconds is not None else {}
        return updater.follow_up_antigravity_acp("ubuntu", expected, self.events.append, cancelled,
                                                 clock=self.clock, sleep=self.clock.sleep, **arguments)

    def test_waits_for_the_new_t3_then_installs_through_it(self):
        self.ready_after = 3  # The restart worker waits 30 quiet seconds, then T3 starts.
        row = self.follow_up()
        self.assertEqual((row["status"], row["messageCode"], row["previousVersion"], row["version"]),
                         ("updated", "antigravity_acp_updated", "1.5.12", "1.5.13"))
        # Every readiness read names the server this run installed, so the old one never passes.
        self.assertEqual({expected for expected, _ in self.ready}, {NEW_T3})
        self.assertEqual(len(self.ready), 4)
        # The guarded install runs once, against the new server, never with the run's T3 row.
        self.assertEqual(self.updates, [("1.5.12", None, NEW_T3)])
        self.assertEqual(row["diagnostics"]["cause"], "none")
        self.assertIn("t3_restart", row["diagnostics"]["phases"])
        self.assertIn({"event": "app", "appId": "antigravity-acp", "phase": "updating"}, self.events)

    def test_a_restart_never_seen_is_t3_not_ready_and_installs_nothing(self):
        self.ready_after = 10 ** 6
        row = self.follow_up(seconds=120)
        self.assertEqual((row["status"], row["messageCode"], row["version"], row["updateAttempted"]),
                         ("action_required", "t3_not_ready", "1.5.12", False))
        self.assertEqual((row["diagnostics"]["cause"], row["diagnostics"]["reason"]), ("t3_not_ready", "restart_not_seen"))
        self.assertEqual(self.updates, [])
        self.assertLessEqual(self.clock.now - 1000.0, 120 + updater.FOLLOW_UP_POLL_SECONDS)

    def test_waits_for_the_helper_lock_the_restart_worker_holds(self):
        self.lock_busy = 3
        row = self.follow_up()
        self.assertEqual(row["status"], "updated")
        self.assertEqual(len(self.updates), 1)

    def test_a_cancel_skips_the_follow_up(self):
        self.ready_after = 10 ** 6
        calls = []
        row = self.follow_up(cancelled=lambda: calls.append(1) or len(calls) > 2)
        self.assertEqual((row["status"], row["messageCode"]), ("skipped", "skipped_cancelled"))
        self.assertEqual(self.updates, [])

    def test_any_t3_accepts_the_server_that_answers(self):
        # A T3 row that did not name its new version: the server running now is the one to check.
        row = self.follow_up(expected=None)
        self.assertEqual(row["status"], "updated")
        self.assertEqual(self.ready[0][0], None)
        self.assertEqual(self.updates, [("1.5.12", None, None)])

    def test_no_runtime_installed_is_not_installed(self):
        self.install = None
        row = self.follow_up()
        self.assertEqual(row["status"], "not_installed")
        self.assertEqual((self.ready, self.updates), ([], []))

    def test_t3_absent_stops_at_once(self):
        def absent(host, deadline, expected=None, seconds=None, **kwargs):
            self.ready.append(expected)
            raise common.UpdateFailure("t3_absent", cause="t3_absent", reason="no_t3_install")
        with mock.patch.object(t3_rpc, "wait_ready", side_effect=absent):
            row = self.follow_up()
        self.assertEqual((row["status"], row["messageCode"]), ("action_required", "t3_absent"))
        self.assertEqual(len(self.ready), 1)


class FollowUpCommandTests(unittest.TestCase):
    def run_main(self, *argv):
        out = io.StringIO()
        # main() exports --state-dir to the process; keep it inside this test.
        with mock.patch.object(sys, "argv", ["app_updates.py", *argv]), contextlib.redirect_stdout(out), \
                mock.patch.dict(updater.os.environ, {}):
            main = updater.main
            main()
        return out.getvalue()

    def test_prints_only_the_antigravity_acp_row(self):
        row = common.result("antigravity-acp", "ubuntu", "current", "1.5.13", "1.5.13", "official-download", "current")
        with mock.patch.object(updater, "follow_up_antigravity_acp", return_value=row) as follow_up, \
                mock.patch.object(updater.sys, "platform", "linux"), mock.patch.object(updater.os, "name", "posix"):
            output = self.run_main("--apply", "--platform", "ubuntu", "--state-dir", "/fixture/state",
                                   "--acp-after-t3-restart", NEW_T3)
        self.assertEqual(json.loads(output.strip().splitlines()[-1]), {"results": [row]})
        self.assertEqual(follow_up.call_args.args[:2], ("ubuntu", NEW_T3))

    def test_any_passes_no_expected_version(self):
        row = common.result("antigravity-acp", "ubuntu", "current", "1.5.13", "1.5.13", "official-download", "current")
        with mock.patch.object(updater, "follow_up_antigravity_acp", return_value=row) as follow_up, \
                mock.patch.object(updater.sys, "platform", "linux"), mock.patch.object(updater.os, "name", "posix"):
            self.run_main("--apply", "--platform", "ubuntu", "--acp-after-t3-restart", "any")
        self.assertEqual(follow_up.call_args.args[:2], ("ubuntu", None))

    def test_refuses_a_malformed_version_and_other_platforms(self):
        for argv in (["--apply", "--platform", "ubuntu", "--acp-after-t3-restart", "1.0; rm -rf"],
                     ["--platform", "ubuntu", "--acp-after-t3-restart", NEW_T3]):
            with self.subTest(argv=argv), mock.patch.object(updater, "follow_up_antigravity_acp") as follow_up, \
                    mock.patch.object(updater.sys, "platform", "linux"), mock.patch.object(updater.os, "name", "posix"), \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                self.run_main(*argv)
            follow_up.assert_not_called()


if __name__ == "__main__":
    unittest.main()
