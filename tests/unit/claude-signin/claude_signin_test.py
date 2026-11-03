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


if __name__ == "__main__":
    unittest.main()
