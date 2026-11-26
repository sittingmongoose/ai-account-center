"""Claude sign-in renewal runner: decisions, collection and publishing, all offline."""
import json
import os
import pathlib
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts" / "claude-signin"))
import claude_signin as runner  # noqa: E402

H = 3600 * 1000
NOW = int(time.time() * 1000)
CFG = dict(runner.DEFAULTS, renewWithinHours=96, verifyAfterHours=9)


class Ssh(unittest.TestCase):
    def test_ssh_never_rewrites_known_hosts(self):
        args = runner.ssh_args({"ssh": "fixture.invalid"})
        self.assertIn("UpdateHostKeys=no", args)
        self.assertEqual(args[:1], ["ssh"])


class Home(unittest.TestCase):
    """A fixture home and CCS folder; the real ~/.claude and ~/.ccs are never read."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = pathlib.Path(tmp.name)
        env = mock.patch.dict(os.environ, {"HOME": str(self.home), "USERPROFILE": str(self.home),
                                           "CCS_HOME": str(self.home)})
        env.start()
        self.addCleanup(env.stop)
        linux = mock.patch.object(runner.platform, "system", return_value="Linux")
        linux.start()
        self.addCleanup(linux.stop)

    def profile(self, name, oauth, email="a@example.com"):
        root = self.home / ".claude-t3" / name
        root.mkdir(parents=True, exist_ok=True)
        (root / ".claude.json").write_text(json.dumps({"oauthAccount": {"emailAddress": email}}))
        if oauth is not None:
            (root / ".credentials.json").write_text(json.dumps({"claudeAiOauth": oauth}))
        return root


class NodeTests(Home):
    def executable(self, relative):
        path = self.home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/missing-fixture-interpreter\n")
        path.chmod(0o700)
        return str(path)

    def test_missing_and_non_executable_config_fall_back_to_path_then_homebrew(self):
        configured = self.home / "configured-node"
        path_node = self.executable("bin/node")
        arm_node = self.executable("opt/homebrew/bin/node")
        intel_node = self.executable("usr/local/bin/node")
        cfg = dict(CFG, nodePath=str(configured))
        with mock.patch.object(runner, "NODE_LOCATIONS", (arm_node, intel_node)), \
                mock.patch.dict(os.environ, {"PATH": str(self.home / "bin")}):
            self.assertEqual(runner.node_path(cfg), path_node)
            configured.touch()
            configured.chmod(0o600)
            self.assertEqual(runner.node_path(cfg), path_node)
            configured.chmod(0o700)
            self.assertEqual(runner.node_path(cfg), str(configured))
            cfg["nodePath"] = None
            pathlib.Path(path_node).unlink()
            self.assertEqual(runner.node_path(cfg), arm_node)
            pathlib.Path(arm_node).unlink()
            self.assertEqual(runner.node_path(cfg), intel_node)

    def test_popen_falls_back_if_configured_node_cannot_launch(self):
        bad = self.executable("configured-node")
        good = self.executable("opt/homebrew/bin/node")
        proc = mock.Mock(stdout=[b'{"result":"ok"}'], stderr=mock.Mock())
        proc.stderr.read.return_value = b""
        st = {"browserError": "no_node"}
        with mock.patch.object(runner, "NODE_LOCATIONS", (good,)), \
                mock.patch.dict(os.environ, {"PATH": str(self.home / "empty")}), \
                mock.patch.object(runner.subprocess, "Popen", side_effect=[OSError("private"), proc]) as popen:
            self.assertEqual(runner.browser(dict(CFG, nodePath=bad), "session", "fixture@example.test", state=st),
                             {"result": "ok"})
        self.assertEqual([call.args[0][0] for call in popen.call_args_list], [bad, good])
        self.assertNotIn("browserError", st)

    def test_tick_publishes_missing_or_unlaunchable_node_and_preserves_session_time(self):
        for reason in ("no_node", "node_launch_failed"):
            with self.subTest(reason=reason):
                folder = self.home / reason
                folder.mkdir()
                configured = (self.executable("broken-node") if reason == "node_launch_failed"
                              else str(self.home / "missing"))
                cfg = dict(CFG, nodePath=configured, publishDir=str(folder), hosts=[], chromeProfilesDir=str(self.home / "chrome"))
                checked = NOW - 1000
                st = {"profiles": {"mac/fixture": {"email": "fixture@example.test", "state": "ok"}},
                      "accounts": {"fixture@example.test": {"sessionCheckedAt": checked, "signedIn": True}}}
                # The broken fixture needs a browser operation to exercise the launch error.
                with mock.patch.object(runner, "NODE_LOCATIONS", ()), \
                        mock.patch.dict(os.environ, {"PATH": str(self.home / "empty")}), \
                        mock.patch.object(runner, "Lock"), mock.patch.object(runner, "load_state", return_value=st), \
                        mock.patch.object(runner, "sync_from_share"), mock.patch.object(runner, "scan", return_value=[]), \
                        mock.patch.object(runner, "renew_due"), mock.patch.object(runner, "save_state"):
                    self.assertEqual(runner.cmd_tick(None, cfg), 0)
                doc = json.loads((folder / runner.PUBLISH_JSON).read_text())
                self.assertEqual(doc["browserError"], reason)
                self.assertIsNotNone(doc["generatedAt"])
                self.assertEqual(doc["browserSessions"][0]["checkedAt"], runner.iso(checked))
                self.assertIn(runner.NODE_ERRORS[reason], (folder / runner.PUBLISH_MD).read_text())
                self.assertNotIn(configured, json.dumps(doc))


class CollectTests(Home):
    def test_reads_times_and_email_never_tokens(self):
        self.profile("gmail", {"accessToken": "sk-ant-oat01-SECRET", "refreshToken": "sk-ant-ort01-SECRET",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 20 * 24 * H})
        out = runner.collect_local([])
        text = json.dumps(out)
        self.assertNotIn("SECRET", text)
        row = out["profiles"][0]
        self.assertEqual((row["profile"], row["email"], row["state"]), ("gmail", "a@example.com", "ok"))
        self.assertEqual(row["signInEndsAt"], NOW + 20 * 24 * H)
        # the last 8-hour access token handed out before the end outlives it by under 8 hours
        self.assertGreaterEqual(row["logoutAt"], row["signInEndsAt"])
        self.assertLess(row["logoutAt"] - row["signInEndsAt"], 8 * H + 1)

    def test_blanked_tokens_read_as_logged_out_and_ignored_profiles_are_skipped(self):
        self.profile("me", {"accessToken": "", "refreshToken": "", "expiresAt": NOW - H})
        self.profile("retired", {"accessToken": "x", "refreshToken": "y", "expiresAt": NOW + H,
                                 "refreshTokenExpiresAt": NOW + 9 * H})
        rows = {r["profile"]: r for r in runner.collect_local(["retired"])["profiles"]}
        self.assertEqual(set(rows), {"me"})
        self.assertEqual(rows["me"]["state"], "logged_out")

    def test_default_folder_without_a_sign_in_is_not_tracked(self):
        (self.home / ".claude").mkdir()
        self.assertEqual(runner.collect_local([])["profiles"], [])

    def test_out_of_range_times_are_absent_not_guessed(self):
        self.profile("p", {"accessToken": "x", "refreshToken": "y", "expiresAt": 5, "refreshTokenExpiresAt": True})
        row = runner.collect_local([])["profiles"][0]
        self.assertEqual(row["state"], "no_end_date")
        self.assertIsNone(row["signInEndsAt"])


class DecisionTests(Home):
    def row(self, **kw):
        base = {"profile": "gmail", "email": "a@example.com", "state": "ok", "signInEndsAt": NOW + 30 * 24 * H}
        base.update(kw)
        return base

    def test_due_inside_the_window_and_not_before(self):
        self.assertIsNone(runner.due_reason(self.row(signInEndsAt=NOW + 97 * H), CFG, {}, NOW))
        self.assertEqual(runner.due_reason(self.row(signInEndsAt=NOW + 95 * H), CFG, {}, NOW), "ends_soon")

    def test_logged_out_is_due_but_unreadable_or_anonymous_is_not(self):
        self.assertEqual(runner.due_reason(self.row(state="logged_out"), CFG, {}, NOW), "logged_out")
        self.assertIsNone(runner.due_reason(self.row(state="unreadable"), CFG, {}, NOW))
        self.assertIsNone(runner.due_reason(self.row(state="logged_out", email=None), CFG, {}, NOW))

    def test_a_reverted_renewal_is_due_again_at_once(self):
        pst = {"lastRenewAt": NOW - 10 * H, "revertedAt": NOW - H}
        self.assertEqual(runner.due_reason(self.row(), CFG, pst, NOW), "reverted")

    def test_failures_back_off_and_cap_at_six_hours(self):
        pst = {}
        for _ in range(6):
            runner.note_failure(pst, NOW, "login_failed")
        self.assertEqual(pst["retryAfter"] - NOW, 6 * H)
        self.assertFalse(runner.backoff_ok(pst, NOW + H))

    def test_default_window_is_one_day(self):
        cfg = dict(runner.DEFAULTS)
        self.assertIsNone(runner.due_reason(self.row(signInEndsAt=NOW + 25 * H), cfg, {}, NOW))
        self.assertEqual(runner.due_reason(self.row(signInEndsAt=NOW + 23 * H), cfg, {}, NOW), "ends_soon")

    def test_an_unsolved_captcha_backs_off_instead_of_reopening_every_run(self):
        cfg = dict(runner.DEFAULTS)
        pst = {}
        runner.note_captcha_wait(pst, NOW, cfg, NOW + 3 * 24 * H)
        self.assertEqual(pst["lastResult"], "waiting_for_you")
        self.assertEqual(pst["retryAfter"] - NOW, 12 * H)
        self.assertFalse(runner.backoff_ok(pst, NOW + H))
        near = {}
        runner.note_captcha_wait(near, NOW, cfg, NOW + 5 * H)
        self.assertEqual(near["retryAfter"] - NOW, 4 * H)  # never waits past an hour before the end
        last = {}
        runner.note_captcha_wait(last, NOW, cfg, NOW + 30 * 60 * 1000)
        self.assertEqual(last["retryAfter"] - NOW, 12 * H)  # already inside the last hour: the plain wait

    def test_renew_due_skips_an_account_backing_off_from_a_captcha(self):
        cfg = dict(runner.DEFAULTS)
        st = {"profiles": {"ubuntu/gmail": {"retryAfter": NOW + 6 * H, "lastResult": "waiting_for_you"}}, "accounts": {}, "events": []}
        rows = [self.row(signInEndsAt=NOW + 10 * H)]
        with mock.patch.object(runner, "now_ms", return_value=NOW):
            out = runner.renew_due(cfg, st, [({"name": "ubuntu"}, {"profiles": rows})], dry_run=True)
        self.assertEqual(out, [])

    def test_verification_spots_an_old_sign_in_written_back(self):
        pst = {"expectEnd": NOW + 29 * 24 * H, "lastRenewAt": NOW - 9 * H, "verify": [NOW - 1]}
        st = {"events": []}
        runner.check_verification(st, "ubuntu/gmail", pst, self.row(signInEndsAt=NOW + 2 * 24 * H))
        self.assertTrue(pst["revertedAt"])
        self.assertEqual(pst["reverts"], 1)
        self.assertNotIn("expectEnd", pst)

    def test_verification_passes_once_every_check_time_is_behind_it(self):
        end = NOW + 29 * 24 * H
        pst = {"expectEnd": end, "lastRenewAt": NOW - 9 * H, "verify": [NOW - 1, NOW + 5 * H]}
        runner.check_verification({"events": []}, "k", pst, self.row(signInEndsAt=end))
        self.assertEqual(pst["verify"], [NOW + 5 * H])
        runner.check_verification({"events": []}, "k", pst, self.row(signInEndsAt=end))
        self.assertIn("expectEnd", pst)

    def test_a_captcha_wait_nothing_is_waiting_on_is_cleared(self):
        st = {"profiles": {}, "accounts": {"a@example.com": {"needs": "captcha", "needsHow": "x", "needsSince": NOW},
                                           "b@example.com": {"needs": "captcha", "needsHow": "x", "needsSince": NOW},
                                           "c@example.com": {"needs": "wrong_account", "needsHow": "x"}}}
        host = {"name": "ubuntu"}
        rows = [self.row(email="a@example.com"), self.row(profile="b", email="b@example.com", signInEndsAt=NOW + H)]
        runner.drop_stale_captchas(CFG, st, [(host, {"profiles": rows})])
        self.assertNotIn("needs", st["accounts"]["a@example.com"])
        self.assertEqual(st["accounts"]["b@example.com"]["needs"], "captcha")  # its renewal is due: still waiting
        self.assertEqual(st["accounts"]["c@example.com"]["needs"], "wrong_account")
        st["accounts"]["a@example.com"]["needs"] = "captcha"
        runner.drop_stale_captchas(CFG, st, [(host, {"profiles": rows}), ({"name": "mac"}, {"error": "ssh"})])
        self.assertEqual(st["accounts"]["a@example.com"]["needs"], "captcha")  # a computer went unread


class LoginCommandTests(Home):
    def test_windows_keeps_no_space_before_the_ampersands(self):
        host = {"name": "windows", "os": "windows", "ssh": "user@pc"}
        argv = runner.login_command(host, {"default": False, "dir": r"C:\Users\u\.claude-t3\party", "_claude": r"C:\c.exe"}, CFG)
        self.assertEqual(argv[:2], ["ssh", "-o"])
        self.assertIn(r"set CLAUDE_CONFIG_DIR=C:\Users\u\.claude-t3\party&& ", argv[-1])
        self.assertNotIn(" &&", argv[-1])

    def test_posix_default_profile_clears_the_variable(self):
        host = {"name": "ubuntu", "os": "linux", "ssh": "u@vm"}
        line = runner.login_command(host, {"default": True, "dir": "/h/.claude", "_claude": "/h/bin/claude"}, CFG)[-1]
        self.assertTrue(line.startswith("env -u CLAUDE_CONFIG_DIR BROWSER=true "))


class SecrecyTests(Home):
    def test_scrub_removes_links_codes_and_tokens(self):
        text = runner.scrub("open https://claude.ai/magic-link#abc then type " + "A" * 24 + "#" + "B" * 24 + " sk-ant-x1")
        self.assertNotIn("magic-link", text)
        self.assertNotIn("A" * 24, text)
        self.assertNotIn("sk-ant", text)

    def test_magic_link_survives_quoted_printable(self):
        src = 'href=3D"https://claude.ai/magic-link#tok=\r\nen-1&amp;x=3D2"'
        self.assertEqual(runner.link_from_source(src), "https://claude.ai/magic-link#token-1&x=2")

    def test_published_file_carries_times_and_states_only(self):
        folder = self.home / "share"
        folder.mkdir()
        cfg = dict(CFG, publishDir=str(folder), hosts=[])
        st = {"profiles": {"mac/gmail": {"email": "a@example.com", "state": "ok", "signInEndsAt": NOW + 50 * H,
                                         "logoutAt": NOW + 55 * H, "lastResult": "renewed", "lastRenewAt": NOW,
                                         "verify": [NOW + 9 * H], "note": "keychain_needs_desktop_session"}},
              "accounts": {"a@example.com": {"needs": "captcha", "needsHow": "Solve it.", "needsSince": NOW}},
              "hosts": {"mac": {"readAt": NOW}}}
        self.assertTrue(runner.publish(cfg, st))
        doc = json.loads((folder / runner.PUBLISH_JSON).read_text())
        row = doc["signIns"][0]
        self.assertEqual(set(row), {"host", "profile", "email", "state", "signInEndsAt", "logoutAt", "renewFrom",
                                    "autoRenew", "lastRenewAt", "lastResult", "nextVerifyAt", "retryAfter",
                                    "reverts", "readAt", "note"})
        self.assertIsNone(row["note"])  # notes are published only for unreadable rows
        self.assertEqual(doc["needsYou"][0]["reason"], "captcha")
        self.assertIn("Needs you", (folder / runner.PUBLISH_MD).read_text())





class InstalledRegistryTests(Home):
    def installed_labels(self, install):
        import runpy
        return runpy.run_path(str(install / "claude_signin.py"))["computer_labels"]()

    def test_install_delivers_registry_beside_runner(self):
        with mock.patch.object(runner.subprocess, "run"), mock.patch.object(runner, "node_path", return_value=None):
            runner.cmd_install(None, CFG)
        install = pathlib.Path(runner.state_dir()) / "app"
        self.assertEqual(self.installed_labels(install), runner.computer_labels())
        self.assertEqual(self.installed_labels(install)["alexis"], "Alexis PC")

    def test_self_update_delivers_snapshot_root_registry(self):
        import shutil
        install = pathlib.Path(runner.state_dir()) / "app"
        install.mkdir(parents=True)
        shutil.copyfile(runner.SCRIPT, install / "claude_signin.py")
        snapshot = self.home / "snapshot"
        (snapshot / "claude-signin").mkdir(parents=True)
        for name in ("claude_signin.py", "browser_auth.mjs", "README.md"):
            shutil.copyfile(pathlib.Path(runner.HERE) / name, snapshot / "claude-signin" / name)
        registry = pathlib.Path(runner.computer_registry_file()).read_bytes()
        (snapshot / "computers.json").write_bytes(registry)
        with mock.patch.object(runner, "HERE", str(install)):
            runner.sync_from_share(dict(CFG, publishDir=str(snapshot), publishVia=None))
        self.assertEqual(self.installed_labels(install)["alexis"], "Alexis PC")
        self.assertEqual((install / "computers.json").read_bytes(), registry)
        (snapshot / "computers.json").write_text("invalid")
        with mock.patch.object(runner, "HERE", str(install)):
            runner.sync_from_share(dict(CFG, publishDir=str(snapshot), publishVia=None))
        self.assertEqual((install / "computers.json").read_bytes(), registry)


if __name__ == "__main__":
    unittest.main()
