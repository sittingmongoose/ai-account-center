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
        # A fixture home does not isolate the macOS Keychain: the probe
        # reads through this mock (empty unless a test supplies a payload),
        # so no test can touch the real item on a Mac.
        keychain = mock.patch.object(collector, "_mac_keychain_text", return_value=None)
        keychain.start()
        self.addCleanup(keychain.stop)

    def _write(self, name, credentials):
        root = self.home / ".claude-t3" / name
        root.mkdir(parents=True, exist_ok=True)
        (root / ".credentials.json").write_text(json.dumps({"claudeAiOauth": credentials}))

    def report(self):
        return collector.collect(self.home, NOW, include_keychain=False)

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
        # Both shell forms are selected explicitly: the host running this
        # suite must not decide which syntax the first assertion expects.
        with mock.patch.object(collector.sys, "platform", "linux"):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["spaced"]["fixCommand"],
                         '(unset CLAUDE_SECURESTORAGE_CONFIG_DIR USE_LOCAL_OAUTH '
                         'USE_STAGING_OAUTH CLAUDE_LOCAL_OAUTH_API_BASE '
                         'CLAUDE_CODE_CUSTOM_OAUTH_URL && '
                         'CLAUDE_CONFIG_DIR="$HOME/.claude-t3/spaced" claude auth login)')
        with mock.patch.object(collector.sys, "platform", "win32"):
            command = collector.fix_command(self.home / ".claude-t3" / "spaced", self.home)
        # The home expands the way Python's Path.home() resolved it on
        # Windows: USERPROFILE, falling back to HOMEDRIVE+HOMEPATH when
        # USERPROFILE is unset; $HOME can name another directory in a
        # PowerShell whose home drive is mapped.
        # Session-scoped like the POSIX subshell: every selector the
        # command clears or sets is saved first and restored after, so
        # pasting it changes nothing in the user's own PowerShell.
        self.assertEqual(
            command,
            '$aac0 = $env:CLAUDE_CONFIG_DIR; $aac1 = $env:CLAUDE_SECURESTORAGE_CONFIG_DIR; '
            '$aac2 = $env:USE_LOCAL_OAUTH; $aac3 = $env:USE_STAGING_OAUTH; '
            '$aac4 = $env:CLAUDE_LOCAL_OAUTH_API_BASE; $aac5 = $env:CLAUDE_CODE_CUSTOM_OAUTH_URL; '
            '$env:CLAUDE_CONFIG_DIR = "$(if (Test-Path env:USERPROFILE) { $env:USERPROFILE } '
            'else { $env:HOMEDRIVE + $env:HOMEPATH })\\.claude-t3\\spaced"; '
            '$env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $null; $env:USE_LOCAL_OAUTH = $null; '
            '$env:USE_STAGING_OAUTH = $null; $env:CLAUDE_LOCAL_OAUTH_API_BASE = $null; '
            '$env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $null; claude auth login; '
            '$env:CLAUDE_CONFIG_DIR = $aac0; $env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $aac1; '
            '$env:USE_LOCAL_OAUTH = $aac2; $env:USE_STAGING_OAUTH = $aac3; '
            '$env:CLAUDE_LOCAL_OAUTH_API_BASE = $aac4; $env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $aac5')
        # No absolute path of this machine reaches the published rows.
        self.assertNotIn(str(self.home), json.dumps(rows))

    def test_fix_command_single_quotes_a_bang_carrying_dir(self):
        # Interactive bash history-expands `!` inside double quotes (and
        # `\!` keeps the backslash), so this dir pastes single-quoted.
        with mock.patch.object(collector.sys, "platform", "linux"):
            command = collector.fix_command(self.home / ".claude-t3" / "a!b", self.home)
        self.assertEqual(
            command,
            "(unset CLAUDE_SECURESTORAGE_CONFIG_DIR USE_LOCAL_OAUTH "
            "USE_STAGING_OAUTH CLAUDE_LOCAL_OAUTH_API_BASE "
            "CLAUDE_CODE_CUSTOM_OAUTH_URL && "
            "CLAUDE_CONFIG_DIR=\"$HOME\"'/.claude-t3/a!b' claude auth login)")

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
        self.assertEqual(names, {"default", "saved-profiles"})
        partial = {row["name"]: row for row in report["profiles"]}["saved-profiles"]
        self.assertEqual(partial["state"], "unavailable")
        self.assertIn("incomplete", partial["error"])
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertEqual(rows["default"]["state"], "ok")

    def test_keychain_is_skipped_for_a_fixture_home_and_absent_elsewhere(self):
        # include_keychain=False keeps a --home fixture run off the real Keychain.
        report = collector.collect(self.home, NOW, include_keychain=False)
        self.assertNotIn("keychain", {row["name"] for row in report["profiles"]})
        entry = collector.mac_keychain_profile(NOW, self.home)
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

    def test_a_profile_under_a_relocated_ccs_scope_still_reads(self):
        import failover_core
        external = tempfile.TemporaryDirectory()
        self.addCleanup(external.cleanup)
        ccs_dir = pathlib.Path(external.name) / "ccs"
        profile = ccs_dir / "instances" / "alpha"
        profile.mkdir(parents=True)
        (profile / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        pairs = [(profile, ["claude-profile:alpha"])]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = {row["name"]: row for row in
                    collector.collect(self.home, NOW, include_keychain=False,
                                      ccs_dir=ccs_dir)["profiles"]}
        self.assertEqual(rows["alpha"]["state"], "ok")

    def test_a_profile_outside_home_and_ccs_scope_is_refused(self):
        outside = tempfile.TemporaryDirectory()
        self.addCleanup(outside.cleanup)
        profile = pathlib.Path(outside.name) / "elsewhere"
        profile.mkdir()
        (profile / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        row = collector.claude_profile("elsewhere", profile, NOW, self.home,
                                       self.home / ".ccs")
        self.assertEqual(row["state"], "unavailable")
        self.assertIn("allowed roots", row["error"])

    def test_run_uses_the_apps_ccs_precedence(self):
        import os
        seen = {}

        def capture(home, now_ms, include_keychain=True, ccs_dir=None):
            seen["ccs_dir"] = ccs_dir
            return {"schemaVersion": 1, "profiles": []}

        with mock.patch.object(collector, "collect", capture):
            with mock.patch.dict(os.environ, {"CCS_HOME": "/scope/home"}, clear=False):
                os.environ.pop("CCS_DIR", None)
                collector.run()
                self.assertEqual(seen["ccs_dir"], pathlib.Path("/scope/home/.ccs"))
            with mock.patch.dict(os.environ,
                                 {"CCS_HOME": "/scope/home", "CCS_DIR": "/scope/direct"}):
                collector.run()
                self.assertEqual(seen["ccs_dir"], pathlib.Path("/scope/direct"))
            with mock.patch.dict(os.environ, {}, clear=False):
                os.environ.pop("CCS_DIR", None)
                os.environ.pop("CCS_HOME", None)
                collector.run()
                self.assertIsNone(seen["ccs_dir"])

    def test_a_sibling_read_failure_marks_discovery_incomplete(self):
        import desktop_helpers
        import failover_core

        def failing_roots(home, provider, ccs_dir):
            desktop_helpers.read_failed()
            return [(home / ".claude", ["claude-profile:default"])]

        with mock.patch.object(failover_core, "_roots", failing_roots):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_settings_truncated_before_the_claude_filter_is_incomplete(self):
        settings_home = self.home / ".claude-settings-alpha"
        settings_home.mkdir()
        (settings_home / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        instances = {}
        for index in range(64):
            instances["codex-%02d" % index] = {"driver": "codex", "config": {}}
        instances["claude-alpha"] = {"driver": "claudeAgent",
                                      "config": {"homePath": str(settings_home)}}
        userdata = self.home / ".t3" / "userdata"
        userdata.mkdir(parents=True)
        (userdata / "settings.json").write_text(
            json.dumps({"providerInstances": instances}))
        rows = {row["name"]: row for row in self.report()["profiles"]}
        # The Claude entry sat past the sibling's pre-filter bound: unseen,
        # and the report says so instead of implying a complete list.
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_iso_keeps_milliseconds_so_same_second_crossings_stay_visible(self):
        self.assertIn(".123Z", collector.iso(1700000000123))

    def test_a_saved_instance_with_a_dotted_name_is_discovered(self):
        import failover_core
        profile = self.home / ".ccs" / "instances" / "work.example"
        profile.mkdir(parents=True)
        (profile / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        # The sibling's name filter drops this dir; the collector's own
        # saved-instance scan must still find it.
        pairs = [(self.home / ".claude", ["claude-profile:default"])]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["work.example"]["state"], "ok")

    def test_an_unreadable_saved_profile_marks_discovery_incomplete(self):
        import failover_core
        import os
        if not hasattr(os, "geteuid") or os.geteuid() == 0:
            self.skipTest("POSIX permission bits do not apply here")
        blocked = self.home / "blocked"
        profile = blocked / "configured"
        profile.mkdir(parents=True)
        (profile / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        blocked.chmod(0)
        self.addCleanup(blocked.chmod, 0o700)
        pairs = [(profile, ["claude-profile:configured"])]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        # is_dir() would swallow this permission failure into False; the
        # explicit stat must surface it as incomplete discovery instead.
        self.assertNotIn("configured", rows)
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_a_relative_settings_home_marks_discovery_incomplete(self):
        userdata = self.home / ".t3" / "userdata"
        userdata.mkdir(parents=True)
        (userdata / "settings.json").write_text(json.dumps(
            {"providerInstances": {"claude-rel": {
                "driver": "claudeAgent", "config": {"homePath": "relative/home"}}}}))
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_codex_auth_link_outside_the_home_is_not_followed(self):
        outside = tempfile.TemporaryDirectory()
        self.addCleanup(outside.cleanup)
        target = pathlib.Path(outside.name) / "auth.json"
        target.write_text(json.dumps({"access_token": "fixture-private"}))
        (self.home / ".codex").mkdir(parents=True, exist_ok=True)
        (self.home / ".codex" / "auth.json").symlink_to(target)
        entry = collector.codex_profile(NOW, self.home)
        # Present but refused: an unavailable row, never the linked values.
        self.assertEqual(entry["state"], "unavailable")
        self.assertNotIn("fieldsAvailable", entry)

    def test_a_dangling_profile_link_marks_discovery_incomplete(self):
        (self.home / ".claude-t3" / "gone").mkdir(parents=True, exist_ok=True)
        link = self.home / ".claude-t3" / "dangling"
        link.symlink_to(self.home / ".claude-t3" / "gone")
        (self.home / ".claude-t3" / "gone").rmdir()
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertNotIn("dangling", rows)
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_a_symlinked_instance_is_not_counted_twice(self):
        real = self.home / ".ccs" / "instances" / "realone"
        real.mkdir(parents=True)
        (real / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        (self.home / ".ccs" / "instances" / "linkone").symlink_to(real, target_is_directory=True)
        names = [row["name"] for row in self.report()["profiles"]]
        # One row, under the real spelling even though the link's name
        # sorts (and is discovered) first.
        self.assertEqual(sorted(n for n in names if n in ("realone", "linkone")), ["realone"])

    def test_an_overlong_settings_home_marks_discovery_incomplete(self):
        userdata = self.home / ".t3" / "userdata"
        userdata.mkdir(parents=True)
        long_home = str(self.home / ("deep" * 300))
        (userdata / "settings.json").write_text(json.dumps(
            {"providerInstances": {"claude-long": {
                "driver": "claudeAgent", "config": {"homePath": long_home}}}}))
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_a_configured_home_under_a_dangling_ancestor_is_incomplete(self):
        import failover_core
        (self.home / "dead-link").symlink_to(
            self.home / "missing-target", target_is_directory=True)
        profile = self.home / "dead-link" / "configured"
        pairs = [(profile, ["claude-profile:configured"])]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = {row["name"]: row for row in self.report()["profiles"]}
        # stat and lstat both fail at the leaf; the broken ancestor is what
        # marks this unseen instead of absent.
        self.assertNotIn("configured", rows)
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_a_dangling_ccs_link_marks_discovery_incomplete(self):
        (self.home / ".ccs").symlink_to(
            self.home / "missing-ccs", target_is_directory=True)
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_codex_under_an_unreadable_parent_reports_unavailable(self):
        import os
        if not hasattr(os, "geteuid") or os.geteuid() == 0:
            self.skipTest("POSIX permission bits do not apply here")
        codex = self.home / ".codex"
        codex.mkdir(parents=True)
        (codex / "auth.json").write_text(json.dumps({"last_refresh": "fixture"}))
        codex.chmod(0)
        self.addCleanup(codex.chmod, 0o700)
        entry = collector.codex_profile(NOW, self.home)
        # lexists would call this absent; the stat verdict calls it unseen.
        self.assertIsNotNone(entry)
        self.assertEqual(entry["state"], "unavailable")

    def test_a_fifo_at_a_credentials_path_never_blocks_the_report(self):
        import os
        if not hasattr(os, "mkfifo"):
            self.skipTest("FIFOs do not apply here")
        root = self.home / ".claude-t3" / "fifo"
        root.mkdir(parents=True)
        os.mkfifo(root / ".credentials.json")
        codex = self.home / ".codex"
        codex.mkdir(parents=True)
        os.mkfifo(codex / "auth.json")
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertEqual(rows["fifo"]["state"], "unavailable")
        self.assertEqual(rows["codex"]["state"], "unavailable")

    def test_a_falsy_settings_home_marks_discovery_incomplete(self):
        userdata = self.home / ".t3" / "userdata"
        userdata.mkdir(parents=True)
        (userdata / "settings.json").write_text(json.dumps(
            {"providerInstances": {"claude-zero": {
                "driver": "claudeAgent", "config": {"homePath": 0}}}}))
        rows = {row["name"]: row for row in self.report()["profiles"]}
        self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_windows_fix_command_omits_a_curly_quoted_dir(self):
        # PowerShell reads curly quotes as quotes, so a name carrying one
        # would break out of either quoted form; no command publishes.
        with mock.patch.object(collector.sys, "platform", "win32"):
            inside = collector.fix_command(
                self.home / ".claude-t3" / "x\u201d; calc; \u201c", self.home)
            outside = collector.fix_command(
                pathlib.Path("C:/elsewhere/it\u2019s"), self.home)
            clean = collector.fix_command(self.home / ".claude-t3" / "plain", self.home)
        self.assertEqual(inside, "")
        self.assertEqual(outside, "")
        self.assertIn("$env:USERPROFILE", clean)

    def test_malformed_settings_containers_mark_discovery_incomplete(self):
        userdata = self.home / ".t3" / "userdata"
        userdata.mkdir(parents=True)
        settings_home = self.home / ".claude-settings-alpha"
        cases = (
            # The instances container itself is a list, hiding its entries.
            {"providerInstances": [{"driver": "claudeAgent",
                                    "config": {"homePath": str(settings_home)}}]},
            # A matching instance whose config is not an object.
            {"providerInstances": {"claude-bad": {
                "driver": "claudeAgent", "config": False}}},
        )
        for payload in cases:
            with self.subTest(payload=payload):
                (userdata / "settings.json").write_text(json.dumps(payload))
                rows = {row["name"]: row for row in self.report()["profiles"]}
                self.assertIn("incomplete", rows["saved-profiles"]["error"])

    def test_mac_default_login_reads_through_the_keychain_when_no_file(self):
        payload = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=payload):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {row["name"]: row for row in report["profiles"]}
        # One default login, one row: never a failed `default` beside a
        # healthy `keychain` for the same login.
        self.assertNotIn("keychain", rows)
        self.assertEqual(rows["default"]["state"], "ok")
        self.assertEqual(rows["default"]["refreshExpiresAt"], collector.iso(NOW + 30 * H))
        # The row names its config dir, never the opaque Keychain label
        # (whose hash fingerprints the absolute path).
        self.assertEqual(rows["default"]["configDir"], "~/.claude")

    def test_mac_keychain_verdict_wins_over_a_stale_fallback_file(self):
        # The Keychain is the canonical macOS store and the credentials
        # file only its fallback, so a readable Keychain verdict stands in
        # both directions: a healthy file cannot mask a logged-out
        # Keychain login, and a blanked fallback file cannot mask a
        # healthy one.
        import hashlib
        healthy = {"accessToken": "x", "refreshToken": "y",
                   "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}
        blank = {"accessToken": "", "refreshToken": "",
                 "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}
        for file_creds, keychain_creds, expected in (
                (healthy, blank, "logged_out"),
                (blank, healthy, "ok")):
            with self.subTest(expected=expected):
                default = self.home / ".claude"
                default.mkdir(parents=True, exist_ok=True)
                (default / ".credentials.json").write_text(
                    json.dumps({"claudeAiOauth": file_creds}))
                with mock.patch.object(collector.sys, "platform", "darwin"), \
                        mock.patch.object(
                            collector, "_mac_keychain_text",
                            return_value=json.dumps({"claudeAiOauth": keychain_creds})):
                    report = collector.collect(self.home, NOW, include_keychain=True)
                rows = {row["name"]: row for row in report["profiles"]}
                self.assertNotIn("keychain", rows)
                self.assertEqual(rows["default"]["state"], expected)
                self.assertEqual(rows["default"]["configDir"], "~/.claude")
        # The hash helper itself is part of the contract.
        self.assertEqual(
            collector._keychain_service_for("/Users/x/.claude-t3/acct"),
            "Claude Code-credentials-" + hashlib.sha256(
                "/Users/x/.claude-t3/acct".encode("utf-8")).hexdigest()[:8])

    def test_mac_per_account_login_reads_its_hashed_keychain_item(self):
        # Claude Code names a per-config-dir Keychain item by hashing the
        # config dir string; a T3 account on the Mac lives there, not in a
        # credentials file.
        import hashlib
        import unicodedata
        root = self.home / ".claude-t3" / "acct"
        root.mkdir(parents=True)
        expected = "Claude Code-credentials-" + hashlib.sha256(
            unicodedata.normalize("NFC", str(root)).encode("utf-8")).hexdigest()[:8]
        payload = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        seen = []

        def keychain_text(service="Claude Code-credentials"):
            seen.append(service)
            return payload if service == expected else None

        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", side_effect=keychain_text):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertIn(expected, seen)
        self.assertEqual(rows["acct"]["state"], "ok")
        self.assertEqual(rows["acct"]["configDir"], "~/.claude-t3/acct")

    def test_exactly_sixty_four_readable_homes_stay_complete(self):
        # 64 published homes is the bound exactly, not past it: with every
        # home readable and no truncation signal from the sibling, no
        # incomplete-discovery warning may publish.
        for index in range(63):
            self._write("acct%02d" % index,
                        {"accessToken": "x", "refreshToken": "y",
                         "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H})
        rows = self.report()["profiles"]
        self.assertEqual(len(rows), 64)
        self.assertNotIn("saved-profiles", {row["name"] for row in rows})

    def test_a_directory_swapped_at_open_leaks_no_descriptor(self):
        import os as real_os
        if not pathlib.Path("/proc/self/fd").is_dir():
            self.skipTest("fd accounting needs /proc")
        target = self.home / "swap.json"
        real_open = real_os.open

        def swapping_open(path, flags, *args, **kwargs):
            pathlib.Path(path).unlink(missing_ok=True)
            pathlib.Path(path).mkdir(exist_ok=True)
            return real_open(path, flags, *args, **kwargs)

        def fd_count():
            return len(list(pathlib.Path("/proc/self/fd").iterdir()))

        before = fd_count()
        for _ in range(3):
            target.rmdir() if target.is_dir() else None
            target.write_text("{}")
            with mock.patch.object(collector.os, "open", swapping_open):
                self.assertIsNone(collector._read_regular_bytes(target, 1024))
        self.assertEqual(fd_count(), before)

    def test_mac_default_file_verdict_stands_and_keychain_adds_no_row(self):
        default = self.home / ".claude"
        default.mkdir(parents=True, exist_ok=True)
        (default / ".credentials.json").write_text(json.dumps(
            {"claudeAiOauth": {"accessToken": "x", "refreshToken": "y",
                               "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=None):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertNotIn("keychain", rows)
        self.assertEqual(rows["default"]["state"], "ok")
        self.assertEqual(rows["default"]["configDir"], "~/.claude")

    def test_mac_default_unreadable_in_both_sources_is_one_row(self):
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=None):
            report = collector.collect(self.home, NOW, include_keychain=True)
        names = [row["name"] for row in report["profiles"]]
        self.assertEqual(names.count("default"), 1)
        self.assertNotIn("keychain", names)
        default = {row["name"]: row for row in report["profiles"]}["default"]
        self.assertEqual(default["state"], "unavailable")
        self.assertEqual(default["error"], "keychain item unreadable")

    def test_keychain_rows_gate_stamps_on_token_presence(self):
        payload = json.dumps({"claudeAiOauth": {
            "accessToken": "", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=payload):
            entry = collector.mac_keychain_profile(NOW, self.home)
        self.assertEqual(entry["state"], "warn")
        self.assertIsNone(entry["accessExpiresAt"])
        self.assertEqual(entry["refreshExpiresAt"], collector.iso(NOW + 30 * H))

    def test_fix_commands_scope_the_credential_selectors(self):
        # A pasted re-login must repair the login its row names even when
        # the user's shell exports credential selectors for another
        # account or storage identity: the selectors the command does
        # not set are unset for the invocation (never emptied — an empty
        # securestorage value selects the plain service upstream), the
        # OAuth-variant selectors that move storage to the custom/local
        # Keychain identities included; a failed unset (readonly
        # variable) stops the command instead of proceeding unscoped.
        healthy = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        default = self.home / ".claude"
        default.mkdir(parents=True, exist_ok=True)
        (default / ".credentials.json").write_text(healthy)
        with mock.patch.object(collector.sys, "platform", "linux"):
            entry = collector.claude_profile(
                "default", default, NOW, self.home, kind="default")
        self.assertEqual(
            entry["fixCommand"],
            "(unset CLAUDE_CONFIG_DIR CLAUDE_SECURESTORAGE_CONFIG_DIR "
            "USE_LOCAL_OAUTH USE_STAGING_OAUTH CLAUDE_LOCAL_OAUTH_API_BASE "
            "CLAUDE_CODE_CUSTOM_OAUTH_URL && claude auth login)")
        with mock.patch.object(collector.sys, "platform", "win32"):
            entry = collector.claude_profile(
                "default", default, NOW, self.home, kind="default")
        self.assertEqual(
            entry["fixCommand"],
            "$aac0 = $env:CLAUDE_CONFIG_DIR; $aac1 = $env:CLAUDE_SECURESTORAGE_CONFIG_DIR; "
            "$aac2 = $env:USE_LOCAL_OAUTH; $aac3 = $env:USE_STAGING_OAUTH; "
            "$aac4 = $env:CLAUDE_LOCAL_OAUTH_API_BASE; $aac5 = $env:CLAUDE_CODE_CUSTOM_OAUTH_URL; "
            "$env:CLAUDE_CONFIG_DIR = $null; $env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $null; "
            "$env:USE_LOCAL_OAUTH = $null; $env:USE_STAGING_OAUTH = $null; "
            "$env:CLAUDE_LOCAL_OAUTH_API_BASE = $null; $env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $null; "
            "claude auth login; "
            "$env:CLAUDE_CONFIG_DIR = $aac0; $env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $aac1; "
            "$env:USE_LOCAL_OAUTH = $aac2; $env:USE_STAGING_OAUTH = $aac3; "
            "$env:CLAUDE_LOCAL_OAUTH_API_BASE = $aac4; $env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $aac5")
        with mock.patch.object(collector.sys, "platform", "win32"):
            literal = collector.fix_command(pathlib.Path("C:/elsewhere/acct"), self.home)
        self.assertEqual(
            literal,
            "$aac0 = $env:CLAUDE_CONFIG_DIR; $aac1 = $env:CLAUDE_SECURESTORAGE_CONFIG_DIR; "
            "$aac2 = $env:USE_LOCAL_OAUTH; $aac3 = $env:USE_STAGING_OAUTH; "
            "$aac4 = $env:CLAUDE_LOCAL_OAUTH_API_BASE; $aac5 = $env:CLAUDE_CODE_CUSTOM_OAUTH_URL; "
            "$env:CLAUDE_CONFIG_DIR = 'C:/elsewhere/acct'; "
            "$env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $null; $env:USE_LOCAL_OAUTH = $null; "
            "$env:USE_STAGING_OAUTH = $null; $env:CLAUDE_LOCAL_OAUTH_API_BASE = $null; "
            "$env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $null; claude auth login; "
            "$env:CLAUDE_CONFIG_DIR = $aac0; $env:CLAUDE_SECURESTORAGE_CONFIG_DIR = $aac1; "
            "$env:USE_LOCAL_OAUTH = $aac2; $env:USE_STAGING_OAUTH = $aac3; "
            "$env:CLAUDE_LOCAL_OAUTH_API_BASE = $aac4; $env:CLAUDE_CODE_CUSTOM_OAUTH_URL = $aac5")

    def test_mac_readable_keychain_unknown_expiry_stands_over_healthy_file(self):
        # A Keychain item that reads but whose lifetime is unknown HAS
        # answered: its unavailable verdict is the canonical store's,
        # and the stale fallback file must not replace it with healthy.
        default = self.home / ".claude"
        default.mkdir(parents=True, exist_ok=True)
        (default / ".credentials.json").write_text(json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        cases = (
            (json.dumps({"claudeAiOauth": {
                "accessToken": "x", "refreshToken": "y", "expiresAt": NOW + 2 * H}}),
             "expiry times unreadable"),
            ("not json at all", "keychain item holds no readable credentials"),
        )
        for payload, error in cases:
            with self.subTest(error=error):
                with mock.patch.object(collector.sys, "platform", "darwin"), \
                        mock.patch.object(collector, "_mac_keychain_text",
                                          return_value=payload):
                    report = collector.collect(self.home, NOW, include_keychain=True)
                row = {r["name"]: r for r in report["profiles"]}["default"]
                self.assertEqual(row["state"], "unavailable")
                self.assertEqual(row["error"], error)
                self.assertIsNone(row["refreshExpiresAt"])

    def test_mac_spelled_keychain_item_binds_before_the_resolved_spelling(self):
        # One dir under two spellings can hold two Keychain logins; each
        # row probes exactly its own spelling's service — the login its
        # own fix command repairs — and no other spelling's item.
        target = self.home / ".claude-t3" / "target"
        target.mkdir(parents=True)
        alias = self.home / ".claude-t3" / "alias"
        alias.symlink_to(target, target_is_directory=True)
        healthy = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        blank = json.dumps({"claudeAiOauth": {
            "accessToken": "", "refreshToken": "",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        services = {
            collector._keychain_service_for(str(alias)): blank,
            collector._keychain_service_for(str(target)): healthy,
        }

        def keychain_text(service="Claude Code-credentials"):
            return services.get(service)

        with mock.patch.object(collector.sys, "platform", "darwin"):
            service = collector._keychain_service_for_root(alias, "dir")
            with mock.patch.object(collector, "_mac_keychain_text",
                                   side_effect=keychain_text):
                report = collector.collect(self.home, NOW, include_keychain=True)
        self.assertEqual(service, collector._keychain_service_for(str(alias)))
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertEqual(rows["alias"]["state"], "logged_out")
        self.assertEqual(rows["target"]["state"], "ok")

    def test_mac_missing_spelled_item_never_inherits_the_resolved_login(self):
        # The alias's own item is absent and the shared credentials file
        # reads logged out; the resolved spelling's healthy item is a
        # different login and must not answer for the alias row — the
        # alias keeps its file verdict, the target keeps its own.
        target = self.home / ".claude-t3" / "target"
        target.mkdir(parents=True)
        (target / ".credentials.json").write_text(json.dumps({"claudeAiOauth": {
            "accessToken": "", "refreshToken": "",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        alias = self.home / ".claude-t3" / "alias"
        alias.symlink_to(target, target_is_directory=True)
        healthy = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        asked = []

        def keychain_text(service="Claude Code-credentials"):
            asked.append(service)
            if service == collector._keychain_service_for(str(target)):
                return healthy
            return None

        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text",
                                  side_effect=keychain_text):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {row["name"]: row for row in report["profiles"]}
        self.assertEqual(rows["alias"]["state"], "logged_out")
        self.assertEqual(rows["target"]["state"], "ok")
        # Each identity asked exactly its own service, once.
        self.assertEqual(sorted(asked), sorted([
            "Claude Code-credentials",
            collector._keychain_service_for(str(alias)),
            collector._keychain_service_for(str(target)),
        ]))

    def test_mac_dedupe_keeps_each_spelling_but_other_platforms_merge(self):
        target = self.home / ".claude-t3" / "target"
        target.mkdir(parents=True)
        (self.home / ".claude-t3" / "alias").symlink_to(
            target, target_is_directory=True)
        with mock.patch.object(collector.sys, "platform", "darwin"):
            darwin_names = [name for name, *_ in
                            collector.claude_roots(self.home)[0]]
        with mock.patch.object(collector.sys, "platform", "linux"):
            linux_names = [name for name, *_ in
                           collector.claude_roots(self.home)[0]]
        # On macOS each spelling is a distinct Keychain identity; off it,
        # both spellings read the same file and publish once.
        self.assertIn("alias", darwin_names)
        self.assertIn("target", darwin_names)
        self.assertEqual(
            sorted(n for n in linux_names if n in ("alias", "target")), ["target"])

    def test_mac_securestorage_override_is_never_the_default_rows_service(self):
        # The default row is the login its repair command recreates:
        # the command unsets every selector, so it always targets the
        # plain service. A storage identity inherited by THIS probe
        # process is a different login and publishes its own row
        # instead of speaking for the default.
        default = self.home / ".claude"
        with mock.patch.dict(collector.os.environ,
                             {"CLAUDE_SECURESTORAGE_CONFIG_DIR": "/somewhere/else"}):
            self.assertEqual(
                collector._keychain_service_for_root(default, "default"),
                "Claude Code-credentials")
            self.assertEqual(
                collector._ambient_storage_roots(self.home),
                [(collector.pathlib.Path("/somewhere/else"), "storage-dir")])
        with mock.patch.dict(collector.os.environ, {}, clear=False):
            collector.os.environ.pop("CLAUDE_SECURESTORAGE_CONFIG_DIR", None)
            collector.os.environ.pop("CLAUDE_CONFIG_DIR", None)
            self.assertEqual(collector._ambient_storage_roots(self.home), [])
            collector.os.environ["CLAUDE_CONFIG_DIR"] = str(
                self.home / ".claude-t3" / "ambient")
            self.assertEqual(
                collector._ambient_storage_roots(self.home),
                [(self.home / ".claude-t3" / "ambient", "dir")])
        # And at collection: a healthy override item answers on its own
        # row, never for the default, whose logged-out file stands.
        elsewhere = self.home / "elsewhere"
        elsewhere.mkdir(parents=True, exist_ok=True)
        default.mkdir(parents=True, exist_ok=True)
        (default / ".credentials.json").write_text(json.dumps({"claudeAiOauth": {
            "accessToken": "", "refreshToken": "",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        healthy = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})

        def keychain_text(service="Claude Code-credentials"):
            if service == collector._keychain_service_for(str(elsewhere)):
                return healthy
            return None

        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.dict(collector.os.environ,
                                {"CLAUDE_SECURESTORAGE_CONFIG_DIR": str(elsewhere)}), \
                mock.patch.object(collector, "_mac_keychain_text",
                                  side_effect=keychain_text):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {r["name"]: r for r in report["profiles"]}
        self.assertEqual(rows["default"]["state"], "logged_out")
        self.assertEqual(rows["elsewhere"]["state"], "ok")
        self.assertEqual(
            rows["elsewhere"]["fixCommand"],
            "(unset CLAUDE_CONFIG_DIR USE_LOCAL_OAUTH USE_STAGING_OAUTH "
            "CLAUDE_LOCAL_OAUTH_API_BASE CLAUDE_CODE_CUSTOM_OAUTH_URL && "
            'CLAUDE_SECURESTORAGE_CONFIG_DIR="$HOME/elsewhere" claude auth login)')

    def test_mac_settings_default_dir_is_its_own_explicit_identity(self):
        # A configured home naming ~/.claude is launched with
        # CLAUDE_CONFIG_DIR set, which upstream turns into the hashed
        # service — a different login from the plain default. It gets
        # its own row whose verdict is that item's and whose repair
        # command sets the config dir explicitly.
        import failover_core
        default = self.home / ".claude"
        default.mkdir(parents=True, exist_ok=True)
        (default / ".credentials.json").write_text(json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        logged_out = json.dumps({"claudeAiOauth": {
            "accessToken": "", "refreshToken": "",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        pairs = [
            (default, ["discover", "claude-profile:default"]),
            (default, ["t3"]),
        ]

        def keychain_text(service="Claude Code-credentials"):
            if service == collector._keychain_service_for(str(default)):
                return logged_out
            return None

        with mock.patch.object(failover_core, "_roots", return_value=pairs), \
                mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text",
                                  side_effect=keychain_text):
            report = collector.collect(self.home, NOW, include_keychain=True)
        rows = {r["name"]: r for r in report["profiles"]}
        self.assertEqual(rows["default"]["state"], "ok")
        self.assertEqual(rows["default-configured"]["state"], "logged_out")
        self.assertEqual(
            rows["default-configured"]["fixCommand"],
            "(unset CLAUDE_SECURESTORAGE_CONFIG_DIR USE_LOCAL_OAUTH "
            "USE_STAGING_OAUTH CLAUDE_LOCAL_OAUTH_API_BASE "
            "CLAUDE_CODE_CUSTOM_OAUTH_URL && "
            'CLAUDE_CONFIG_DIR="$HOME/.claude" claude auth login)')

    def test_mac_answered_keychain_profile_binds_even_when_unreadable(self):
        # An empty item body is a definite read of a present item: the
        # profile carries the internal answered marker so the file
        # fallback cannot speak over it. A silent lookup carries none.
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=""):
            profile = collector.mac_keychain_profile_for_service(
                NOW, "Claude Code-credentials")
        self.assertTrue(profile["answered"])
        self.assertEqual(profile["error"],
                         "keychain item holds no readable credentials")
        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.object(collector, "_mac_keychain_text", return_value=None):
            profile = collector.mac_keychain_profile_for_service(
                NOW, "Claude Code-credentials")
        self.assertNotIn("answered", profile)

    def test_configured_root_spelling_follows_the_launcher(self):
        # T3 trims and normalizes a configured home before setting
        # CLAUDE_CONFIG_DIR; the hashed probe must hash the same
        # string. A root that normalizes outside the allowed roots is
        # refused, never read under its unnormalized spelling.
        padded = "  " + str(self.home / ".claude-t3" / "x" / ".." / "y") + "  "
        self.assertEqual(
            collector._launcher_spelling(padded, self.home, self.home / ".ccs"),
            self.home / ".claude-t3" / "y")
        escape = str(self.home / ".claude-t3" / ".." / ".." / "outside")
        with self.assertRaises(ValueError):
            collector._launcher_spelling(escape, self.home, self.home / ".ccs")

    def test_mac_keychain_account_follows_the_upstream_rule(self):
        # Upstream keys items to $USER, falling back to the account
        # database name and then a fixed name when either is outside
        # [A-Za-z0-9._-]; an unscoped lookup can return another
        # account's item for the same service.
        import getpass as _getpass
        with mock.patch.dict(collector.os.environ, {"USER": "alice_1"}):
            self.assertEqual(collector._mac_keychain_account(), "alice_1")
        with mock.patch.dict(collector.os.environ, {"USER": "not safe!"}):
            self.assertEqual(collector._mac_keychain_account(), "claude-code-user")
        with mock.patch.dict(collector.os.environ, {"USER": ""}), \
                mock.patch.object(_getpass, "getuser", return_value="bob-2"):
            self.assertEqual(collector._mac_keychain_account(), "bob-2")

    def test_mac_custom_oauth_selectors_probe_the_variant_services(self):
        # A custom or local OAuth overlay stores its login under the
        # suffixed services; those identities publish their own rows
        # (repaired by the user's existing overlay session, so no fix
        # command) instead of staying invisible.
        acct = self.home / ".claude-t3" / "acct"
        acct.mkdir(parents=True)
        healthy = json.dumps({"claudeAiOauth": {
            "accessToken": "x", "refreshToken": "y",
            "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}})
        asked = []

        def keychain_text(service="Claude Code-credentials"):
            asked.append(service)
            return healthy

        with mock.patch.object(collector.sys, "platform", "darwin"), \
                mock.patch.dict(collector.os.environ, {
                    "CLAUDE_CODE_CUSTOM_OAUTH_URL": "https://example.invalid",
                    "USE_LOCAL_OAUTH": "1"}), \
                mock.patch.object(collector, "_mac_keychain_text",
                                  side_effect=keychain_text):
            report = collector.collect(self.home, NOW, include_keychain=True)
        suffix = collector._keychain_service_for(str(acct)).removeprefix(
            "Claude Code-credentials")
        self.assertIn("Claude Code-custom-oauth-credentials" + suffix, asked)
        self.assertIn("Claude Code-local-oauth-credentials" + suffix, asked)
        rows = {r["name"]: r for r in report["profiles"]}
        variant = rows["acct-custom-oauth"]
        self.assertEqual(variant["state"], "ok")
        self.assertEqual(variant["fixCommand"], "")

    def test_same_name_distinct_homes_both_publish(self):
        # Sanitized-name collisions must not erase a login: two homes
        # whose labels collide get distinct published names.
        import failover_core
        for parent in ("a", "b"):
            root = self.home / parent / "same"
            root.mkdir(parents=True)
            (root / ".credentials.json").write_text(json.dumps({"claudeAiOauth": {
                "accessToken": "x", "refreshToken": "y",
                "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        pairs = [
            (self.home / ".claude", ["discover", "claude-profile:default"]),
            (self.home / "a" / "same", ["t3"]),
            (self.home / "b" / "same", ["t3"]),
        ]
        with mock.patch.object(failover_core, "_roots", return_value=pairs):
            rows = self.report()["profiles"]
        names = [row["name"] for row in rows]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(names.count("same"), 1)
        self.assertTrue(any(n.startswith("same-") for n in names))

    def test_non_utf8_profile_name_does_not_abort_mac_collection(self):
        # Hashing a surrogate-escaped dir name must degrade that one
        # profile's Keychain probe, never abort the whole report.
        import os as real_os
        base = self.home / ".claude-t3"
        base.mkdir(parents=True)
        odd = real_os.fsencode(base) + b"/bad\xff"
        real_os.mkdir(odd)
        pathlib.Path(real_os.fsdecode(odd), ".credentials.json").write_text(
            json.dumps({"claudeAiOauth": {
                "accessToken": "x", "refreshToken": "y",
                "expiresAt": NOW + 2 * H, "refreshTokenExpiresAt": NOW + 30 * H}}))
        with mock.patch.object(collector.sys, "platform", "darwin"):
            report = collector.collect(self.home, NOW, include_keychain=True)
        states = {row["name"]: row["state"] for row in report["profiles"]}
        self.assertIn("ok", states.values())

    def test_dotfiles_do_not_count_toward_the_scan_bound(self):
        # Dot-prefixed entries are never homes; seventy of them beside
        # one real account must not read as truncated discovery.
        base = self.home / ".claude-t3"
        base.mkdir(parents=True, exist_ok=True)
        for index in range(70):
            (base / (".hidden%02d" % index)).write_text("x")
        self._write("only", {"accessToken": "x", "refreshToken": "y",
                             "expiresAt": NOW + 2 * H,
                             "refreshTokenExpiresAt": NOW + 30 * H})
        names = {row["name"] for row in self.report()["profiles"]}
        self.assertIn("only", names)
        self.assertNotIn("saved-profiles", names)

    def test_fdopen_refusal_never_strands_a_descriptor(self):
        import failover_core
        if not pathlib.Path("/proc/self/fd").is_dir():
            self.skipTest("fd accounting needs /proc")
        target = self.home / "creds.json"
        target.write_text("{}")

        def fd_count():
            return len(list(pathlib.Path("/proc/self/fd").iterdir()))

        before = fd_count()
        with mock.patch("os.fdopen", side_effect=ValueError("fixture refusal")):
            self.assertIsNone(collector._read_regular_bytes(target, 1024))
            self.assertIsNone(failover_core._read_present(target, 1024))
        self.assertEqual(fd_count(), before)


if __name__ == "__main__":
    unittest.main()
