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
                                "expiresAt": True, "refreshTokenExpiresAt": float("inf")})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        # Tokens whose times cannot be read are unavailable, never healthy.
        self.assertEqual(rows["garbage"]["state"], "unavailable")
        self.assertEqual(rows["garbage"]["error"], "expiry times unreadable")
        self.assertIsNone(rows["garbage"]["accessExpiresAt"])
        self.assertIsNone(rows["garbage"]["loggedOutAt"])
        for value in (True, float("inf"), float("nan"), "1e300", -5):
            self.assertIsNone(collector.epoch_ms(value))

    def test_capability_matrix_reports_what_the_tokens_can_still_do(self):
        cases = [
            # (credentials, expected state, expected named end)
            ({"accessToken": "x", "refreshToken": "y",
              "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}, "ok", None),
            ({"accessToken": "x", "refreshToken": "y",
              "expiresAt": NOW + 30 * H, "refreshTokenExpiresAt": NOW + 2 * H}, "ok", NOW + 30 * H),
            ({"accessToken": "x", "refreshToken": "y",
              "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW - H}, "critical", NOW + 2 * H),
            # Access expired and no refresh token: dead, ended when it expired.
            ({"accessToken": "x", "expiresAt": NOW - H}, "logged_out", NOW - H),
            # Access ahead and no refresh token: cannot renew, so critical at that end.
            ({"accessToken": "x", "expiresAt": NOW + 2 * H}, "critical", NOW + 2 * H),
            # No access token but a live refresh token: the next run renews.
            ({"refreshToken": "y", "refreshTokenExpiresAt": NOW + 30 * H}, "warn", None),
            # Empty tokens are logged out; retained future times are never a named end.
            ({"expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + H}, "logged_out", None),
            # A token whose own expiry cannot be read leaves the end unknown.
            ({"accessToken": "x", "refreshToken": "y", "expiresAt": NOW + 2 * H}, "unavailable", None),
            ({"accessToken": "x", "refreshToken": "y", "refreshTokenExpiresAt": "garbage"}, "unavailable", None),
        ]
        for index, (credentials, state, end) in enumerate(cases):
            name = "cap%d" % index
            self._write(name, credentials)
            rows = {row["name"]: row for row in self.report()["profiles"]}
            self.assertEqual(rows[name]["state"], state, credentials)
            expected_stamp = collector.iso(end) if end else None
            self.assertEqual(rows[name]["loggedOutAt"], expected_stamp, credentials)

    def test_fix_command_uses_the_shell_of_the_machine_and_hides_paths(self):
        self._write("spaced", {"accessToken": "x", "refreshToken": "y", "windows": []})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["spaced"]["fixCommand"],
                         'CLAUDE_CONFIG_DIR="$HOME/.claude-t3/spaced" claude auth login')
        with mock.patch.object(collector.sys, "platform", "win32"):
            command = collector.fix_command(self.home / ".claude-t3" / "spaced", self.home)
        self.assertEqual(command,
                         '$env:CLAUDE_CONFIG_DIR = "$HOME\\.claude-t3\\spaced"; claude auth login')
        # No absolute path of this machine reaches the published rows.
        self.assertNotIn(str(self.home), json.dumps(rows))

    def test_a_name_the_sibling_discovery_refuses_still_reads_in_the_fallback(self):
        # The saved/T3 discovery bounds directory names; the plain fallback
        # scan keeps such a dir and the publisher sanitizes the name instead
        # of dropping the row.
        its = self.home / ".claude-t3" / "it's"
        its.mkdir(parents=True)
        (its / ".credentials.json").write_text("{}")
        with mock.patch.dict(sys.modules, {"failover_core": None}):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("it's", rows)
        self.assertEqual(rows["it's"]["state"], "logged_out")

    def test_saved_instance_homes_are_discovered_and_deduplicated(self):
        import failover_core
        mail = self.home / ".claude-t3" / "mail"
        mail.mkdir(parents=True, exist_ok=True)
        self._write("mail", {"accessToken": "x", "refreshToken": "y",
                             "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        pairs = [
            (self.home / ".claude", ["claude-profile:default"]),
            (self.home / ".claude-t3" / "mail", ["claude-profile:mail"]),
            (self.home / "gone", ["claude-profile:gone"]),
            (self.home / ".claude-t3" / "mail", ["claude-profile:mail"]),
        ]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("mail", rows)
        self.assertNotIn("gone", rows)
        self.assertEqual(rows["mail"]["state"], "ok")

    def test_an_unreadable_t3_directory_never_kills_the_report(self):
        default = self.home / ".claude"
        default.mkdir()
        (default / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        (self.home / ".claude-t3").mkdir()
        with mock.patch.dict(sys.modules, {"failover_core": None}):
            def broken_iterdir(_self):
                raise PermissionError(13, "fixture denies")
            with mock.patch.object(pathlib.Path, "iterdir", broken_iterdir):
                report = self.report()
        names = {row["name"] for row in report["profiles"]}
        self.assertEqual(names, {"default", "saved-profiles"} | ({"keychain"} if sys.platform == "darwin" else set()))
        partial = {row["name"]: row for row in report["profiles"]}["saved-profiles"]
        self.assertEqual(partial["state"], "unavailable")
        self.assertIn("incomplete", partial["error"])
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertEqual(rows["default"]["state"], "ok")

    def test_keychain_is_skipped_for_a_fixture_home_and_absent_elsewhere(self):
        # include_keychain=False keeps a --home fixture run off the real Keychain.
        report = collector.collect(self.home, NOW, include_keychain=False)
        self.assertNotIn("keychain", {row["name"] for row in report["profiles"]})
        entry = collector.mac_keychain_profile(NOW)
        if sys.platform != "darwin":
            self.assertEqual(entry["state"], "unavailable")
            self.assertNotIn("error", entry)

    def test_missing_and_malformed_files_read_as_unavailable_rows(self):
        (self.home / ".claude-t3" / "broken").mkdir(parents=True, exist_ok=True)
        (self.home / ".claude-t3" / "broken" / ".credentials.json").write_text("{not json")
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["broken"]["state"], "unavailable")
        self.assertEqual(rows["broken"]["error"], "credentials file missing or unreadable")
        self.assertEqual(rows["default"]["state"], "unavailable")
        self.assertEqual(rows["default"]["error"], "credentials file missing or unreadable")

    def test_time_is_imported_at_module_level_for_embedded_main(self):
        self.assertTrue(hasattr(collector, "time"))

    def test_token_values_never_reach_the_report(self):
        secret = "sk-ant-secret-token-value"
        self._write("leaky", {"accessToken": secret, "refreshToken": secret,
                              "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        self.assertNotIn(secret, json.dumps(self.report()))

    def test_fix_command_is_quoted_and_carries_the_config_dir(self):
        self._write("spaced", {"accessToken": "x", "refreshToken": "y", "windows": []})
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["spaced"]["configDir"], "~/.claude-t3/spaced")

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
        entry = collector.codex_profile(NOW, self.home)
        self.assertEqual(entry["configDir"], "~/.codex/auth.json")


if __name__ == "__main__":
    unittest.main()
