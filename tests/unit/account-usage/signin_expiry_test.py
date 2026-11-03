"""Sign-in expiry collector regressions."""
import json
import pathlib
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts" / "account-usage"))
import signin_expiry as collector

NOW = int(time.time() * 1000)
H = 3600 * 1000


class SignInExpiryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = pathlib.Path(temporary.name)
        # The codex probe must read the fixture home, never the real one.
        patcher = mock.patch.object(pathlib.Path, "home", return_value=self.home)
        patcher.start()
        self.addCleanup(patcher.stop)

    def _write(self, name, credentials):
        root = self.home / ".claude-t3" / name
        root.mkdir(parents=True, exist_ok=True)
        (root / ".credentials.json").write_text(json.dumps({"claudeAiOauth": credentials}))

    def report(self):
        return collector.collect(self.home, NOW)

    def test_refresh_passed_but_access_still_valid_ends_at_the_access_expiry(self):
        # The VM-party case: renewal at 03:25 could not push the login end
        # (09:43) back, so the login lives until the current access expiry.
        self._write("party", {"accessToken": "x", "refreshToken": "y",
                              "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW - H})
        (self.home / ".claude" ).mkdir(parents=True, exist_ok=True)
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["party"]["loggedOutAt"], collector.iso(NOW + 2 * H))
        self.assertEqual(rows["party"]["state"], "critical")

    def test_empty_tokens_read_as_logged_out_even_with_future_times(self):
        self._write("blank", {"accessToken": "", "refreshToken": "",
                              "expiresAt": NOW + 8 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["blank"]["state"], "logged_out")

    def test_access_before_refresh_has_no_derivable_end(self):
        # The end is the first access expiry AFTER the refresh expiry; the
        # current access expiry only bounds it, so none is named.
        self._write("bounded", {"accessToken": "x", "refreshToken": "y",
                                "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIsNone(rows["bounded"]["loggedOutAt"])
        self.assertEqual(rows["bounded"]["state"], "ok")

    def test_refresh_within_24h_without_a_named_end_degrades_to_warn(self):
        self._write("soon", {"accessToken": "x", "refreshToken": "y",
                             "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 10 * H})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["soon"]["state"], "warn")
        self.assertIsNone(rows["soon"]["loggedOutAt"])

    def test_out_of_range_or_garbage_epochs_read_as_absent(self):
        self._write("garbage", {"accessToken": "x", "refreshToken": "y",
                                "expiresAt": 1e300, "refreshTokenExpiresAt": "soon"})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["garbage"]["state"], "ok")
        self.assertIsNone(rows["garbage"]["accessExpiresAt"])
        self.assertIsNone(rows["garbage"]["loggedOutAt"])
        for value in (True, float("inf"), float("nan"), "1e300", -5):
            self.assertIsNone(collector.epoch_ms(value))

    def test_missing_and_malformed_files_read_as_unavailable_rows(self):
        (self.home / ".claude-t3" / "broken").mkdir(parents=True, exist_ok=True)
        (self.home / ".claude-t3" / "broken" / ".credentials.json").write_text("{not json")
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["broken"]["state"], "unavailable")
        self.assertTrue(any(row["name"] == "default" for row in self.report()["profiles"]))

    def test_token_values_never_reach_the_report(self):
        secret = "sk-ant-secret-token-value"
        self._write("leaky", {"accessToken": secret, "refreshToken": secret,
                              "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        self.assertNotIn(secret, json.dumps(self.report()))

    def test_fix_command_is_quoted_and_carries_the_config_dir(self):
        self._write("spaced", {"accessToken": "x", "refreshToken": "y", "windows": []})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["spaced"]["fixCommand"],
                         "CLAUDE_CONFIG_DIR='%s' claude auth login" % (self.home / ".claude-t3" / "spaced"))

    def test_codex_row_reports_unsupported_without_a_claude_fix_command(self):
        (self.home / ".codex").mkdir(parents=True, exist_ok=True)
        (self.home / ".codex" / "auth.json").write_text(json.dumps(
            {"access_token": "fixture-private", "tokens": {"access_token": "fixture-private"}}))
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["codex"]["state"], "unsupported")
        self.assertEqual(rows["codex"]["fixCommand"], "")
        # Key names only, and secret-shaped names are bounded by the collector's
        # own output contract (allowProfile filters on the TS side too).
        self.assertIn("access_token", rows["codex"]["fieldsAvailable"])
        self.assertNotIn("fixture-private", json.dumps(rows["codex"]))

    def test_probe_honors_the_home_override(self):
        (self.home / ".codex").mkdir(parents=True, exist_ok=True)
        (self.home / ".codex" / "auth.json").write_text(json.dumps({"last_refresh": "fixture"}))
        entry = collector.codex_profile(NOW, "fix", self.home)
        self.assertEqual(entry["configDir"], str(self.home / ".codex" / "auth.json"))


if __name__ == "__main__":
    unittest.main()
