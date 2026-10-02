"""Fixture tests for read-only collectors; no network or real credentials."""

import base64
import hashlib
import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import desktop_helpers as helpers
import desktop_usage as usage


def fake_jwt(claims):
    encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
    return "fake." + encoded + ".signature"


class NormalizationTests(unittest.TestCase):
    def test_antigravity_shared_credits_are_optional_reported_balance(self):
        windows = usage.normalize_antigravity_credits({"paidTier": {"availableCredits": [{"creditAmount": "125.5"}, {"creditAmount": 20}, {"creditAmount": "1e1"}, {"creditAmount": True}, {"creditAmount": "unknown"}], "private": "hidden"}})
        self.assertEqual(len(windows), 1)
        self.assertEqual(windows[0]["remaining"], 155.5)
        self.assertEqual(windows[0]["kind"], "balance")
        self.assertEqual(windows[0]["unit"], "credits")
        self.assertIsNone(windows[0]["usedPercent"])
        self.assertIsNone(windows[0]["resetAt"])
        self.assertNotIn("private", json.dumps(windows))
        self.assertNotIn("hidden", json.dumps(windows))

    def test_antigravity_missing_credits_are_not_zero(self):
        for value in ({}, {"paidTier": {}}, {"paidTier": {"availableCredits": None}}, {"paidTier": {"availableCredits": []}}, {"paidTier": {"availableCredits": [{"creditAmount": "nan"}]}}, {"paidTier": {"availableCredits": [{"creditAmount": "1e309"}]}}, {"paidTier": {"availableCredits": [{"creditAmount": 1e308}, {"creditAmount": 1e308}]}}):
            self.assertEqual(usage.normalize_antigravity_credits(value), [])
        self.assertEqual(usage.normalize_antigravity_credits({"paidTier": {"availableCredits": [{"creditAmount": "0"}]}})[0]["remaining"], 0)

    def test_unknown_is_not_zero(self):
        window = helpers.quota_window("unknown", "Unknown", used_percent="0", remaining_percent=True)
        self.assertIsNone(window["usedPercent"])
        self.assertIsNone(window["remainingPercent"])
        self.assertIsNone(window["resetAt"])
        for value in (-1, float("nan"), float("inf"), True, "20"):
            self.assertIsNone(helpers.percent(value))

    def test_reported_overage_is_preserved_with_no_negative_remaining(self):
        window = helpers.quota_window("usage", "Usage", used_percent=113.5)
        self.assertEqual(window["usedPercent"], 113.5)
        self.assertEqual(window["remainingPercent"], 0)

    def test_explicit_zero_is_real_zero(self):
        window = helpers.quota_window("usage", "Usage", used_percent=0)
        self.assertEqual(window["usedPercent"], 0)
        self.assertEqual(window["remainingPercent"], 100)

    def test_reset_requires_supplied_timezone_or_epoch(self):
        self.assertEqual(helpers.reset_at("2026-10-01T00:00:00-04:00"), "2026-10-01T04:00:00Z")
        self.assertEqual(helpers.reset_at(1790812800), "2026-10-01T00:00:00Z")
        self.assertEqual(helpers.reset_at(1790812800000), "2026-10-01T00:00:00Z")
        self.assertIsNone(helpers.reset_at("2026-10-01T00:00:00"))
        self.assertIsNone(helpers.reset_at(True))
        self.assertIsNone(helpers.reset_at("later"))

    def test_muse_whitelists_usage_ignores_minted_key(self):
        payload = {
            "api_key": "sk-private-fixture-key", "payment_metadata": {"secret": "private"},
            "subs_usage": {"window": {"used_percent": 0, "resets_at": 1790812800, "window_duration_mins": 300},
                           "weekly": {"used_percent": 6, "resets_at": 1790812800}},
        }
        windows = usage.normalize_muse(payload)
        self.assertEqual([item["usedPercent"] for item in windows], [0, 6])
        self.assertEqual([item["windowMinutes"] for item in windows], [300, 10080])
        self.assertNotIn("private", json.dumps(windows))

    def test_invalid_muse_percent_remains_unknown(self):
        windows = usage.normalize_muse({"subs_usage": {"window": {"used_percent": -1, "resets_at": 1790812800}}})
        self.assertEqual(len(windows), 1)
        self.assertIsNone(windows[0]["usedPercent"])

    def test_muse_portal_counts_and_exact_resets_without_inferred_zero(self):
        windows = usage.normalize_muse({"subscription_quota": {"tier": "High Usage", "window_weighted_used": "0",
            "window_weighted_limit": "1000", "window_duration_secs": "18000", "window_resets_at": None,
            "weekly_weighted_used": "1200", "weekly_weighted_limit": "1000", "weekly_resets_at": "1790812800",
            "api_key": "sk-not-exported"}})
        self.assertEqual([row["usedPercent"] for row in windows], [0, 120])
        self.assertEqual(windows[1]["remainingPercent"], 0)
        self.assertIsNone(windows[0]["resetAt"])
        self.assertEqual(windows[1]["resetAt"], "2026-10-01T00:00:00Z")
        self.assertEqual(windows[0]["windowMinutes"], 300)
        self.assertEqual(windows[1]["unit"], "weighted tokens")
        self.assertNotIn("not-exported", json.dumps(windows))
        self.assertEqual(usage.normalize_muse({"subscription_quota": {}}), [])

    def test_antigravity_grouped_and_nested_quota(self):
        windows = usage.normalize_antigravity({"response": {"summary": {"groups": [{"displayName": "Claude", "buckets": [
            {"bucketId": "claude-week", "window": "weekly", "remainingFraction": .5, "resetTime": "2026-10-01T04:00:00Z"},
            {"bucketId": "claude-fast", "window": "5h", "remaining": {"case": "remainingFraction", "value": 0}},
            {"bucketId": "disabled", "remainingFraction": 1, "disabled": True},
            {"bucketId": "invalid", "remainingFraction": 5, "resetTime": "2026-10-01T04:00:00Z"},
        ]}]}}})
        self.assertEqual(len(windows), 4)
        self.assertEqual(windows[0]["remainingPercent"], 50)
        self.assertEqual(windows[0]["windowMinutes"], 10080)
        self.assertEqual(windows[1]["usedPercent"], 100)
        self.assertFalse(windows[2]["enabled"])
        self.assertIsNone(windows[3]["remainingPercent"])

    def test_cursor_cents_and_fractional_percent(self):
        windows = usage.normalize_cursor({
            "billingCycleStart": "2026-09-01T00:00:00Z", "billingCycleEnd": "2026-10-01T00:00:00Z",
            "individualUsage": {"plan": {"used": 667, "limit": 2000, "autoPercentUsed": .36},
                                "onDemand": {"used": 0, "limit": None}},
            "teamUsage": {"pooled": {"used": 90000, "limit": 100000}},
        })
        self.assertEqual(windows[1]["used"], 6.67)
        self.assertEqual(windows[1]["limit"], 20)
        self.assertEqual(windows[1]["usedPercent"], 33.35)
        self.assertEqual(windows[0]["usedPercent"], .36)
        self.assertEqual(windows[2]["used"], 0)
        self.assertIsNone(windows[2]["limit"])
        self.assertIsNone(windows[2]["usedPercent"])

    def test_antigravity_amount_counter_does_not_invent_units_or_reset(self):
        windows = usage.normalize_antigravity({"buckets": [{"bucketId": "amount", "remainingAmount": "100"}]})
        self.assertEqual(windows[0]["remaining"], 100)
        self.assertIsNone(windows[0]["remainingPercent"])
        self.assertIsNone(windows[0]["resetAt"])
        self.assertIsNone(windows[0]["unit"])

    def test_cursor_does_not_infer_reset_from_start(self):
        windows = usage.normalize_cursor({"billingCycleStart": "2026-09-01T00:00:00Z",
                                          "individualUsage": {"plan": {"used": 10, "limit": 100}}})
        self.assertIsNone(windows[0]["resetAt"])

    def test_cursor_does_not_conflate_spending_with_model_percentage(self):
        windows = usage.normalize_cursor({"individualUsage": {"plan": {"used": 667, "limit": 2000, "totalPercentUsed": 1.4461}}})
        self.assertEqual(windows[1]["usedPercent"], 33.35)
        self.assertEqual(windows[1]["used"], 6.67)
        self.assertEqual(windows[0]["usedPercent"], 1.4461)
        self.assertIsNone(windows[0]["used"])
        self.assertIsNone(windows[0]["limit"])

    def test_spend_balance_and_flags_are_only_preserved_when_reported(self):
        windows = usage.normalize_cursor({"isUnlimited": True, "individualUsage": {
            "plan": {"used": 667, "limit": 2000, "remaining": 1333, "enabled": True, "totalPercentUsed": 1.4461},
            "onDemand": {"used": 0, "limit": None, "enabled": False},
        }})
        self.assertTrue(windows[0]["unlimited"])
        self.assertEqual(windows[1]["remaining"], 13.33)
        self.assertEqual(windows[1]["kind"], "spend")
        self.assertFalse(windows[2]["enabled"])
        self.assertEqual(windows[2]["kind"], "extra_usage")
        self.assertNotIn("expiresAt", windows[1])

    def test_muse_rolling_duration_is_not_assumed_to_be_five_hours(self):
        # Labels use Meta's own /usage card names ("Current usage", "Weekly limit") with our cadence.
        windows = usage.normalize_muse({"subs_usage": {"window": {"used_percent": 5, "window_duration_mins": 240}}})
        self.assertEqual(windows[0]["label"], "Current usage (4-hour)")
        self.assertEqual(windows[0]["windowMinutes"], 240)
        windows = usage.normalize_muse({"subs_usage": {"window": {"used_percent": 5, "window_duration_mins": 90}}})
        self.assertEqual(windows[0]["label"], "Current usage (90-minute)")
        windows = usage.normalize_muse({"subs_usage": {"window": {"used_percent": 5}}})
        self.assertEqual(windows[0]["label"], "Current usage")
        self.assertIsNone(windows[0]["windowMinutes"])
        windows = usage.normalize_muse({"subs_usage": {"weekly": {"used_percent": 5}}})
        self.assertEqual(windows[0]["label"], "Weekly limit")
        windows = usage.normalize_muse({"subscription_quota": {"window_weighted_used": "1", "window_weighted_limit": "4",
                                                               "window_duration_secs": 5400}})
        self.assertEqual(windows[0]["label"], "Current usage (90-minute)")

    def test_cursor_cookie_is_encoded_and_identifies_subject(self):
        token = fake_jwt({"sub": "auth0|user_123"})
        cookie = usage.cursor_cookie(helpers.Credential(token))
        self.assertTrue(cookie.startswith("WorkosCursorSessionToken=user_123%3A%3A"))
        with self.assertRaises(helpers.UsageError):
            usage.cursor_cookie(helpers.Credential("not-a-jwt"))


class CredentialTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_omp_reads_only_enabled_oauth_and_does_not_modify_database(self):
        path = self.home / ".omp/agent/agent.db"
        path.parent.mkdir(parents=True)
        connection = sqlite3.connect(path)
        connection.execute("CREATE TABLE auth_credentials(provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)")
        connection.executemany("INSERT INTO auth_credentials VALUES(?,?,?,?)", [
            ("muse-code", "oauth", json.dumps({"access": json.dumps({"oauthAccessToken": "dca:fixture-login", "apiKey": "sk-never-export"})}), None),
            ("muse-code", "oauth", json.dumps({"access": "dca:disabled"}), "disabled"),
            ("muse-code", "api_key", json.dumps({"access": "sk-key"}), None),
        ])
        connection.commit()
        connection.close()
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        credential = usage.muse_credentials(self.home)
        self.assertEqual(credential.access, "dca:fixture-login")
        self.assertEqual(credential.source, "OMP")
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), digest)
        self.assertNotIn("fixture-login", repr(credential))

    def test_native_muse_configuration_is_read_only(self):
        path = self.home / ".config/muse/auth.json"
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps({"providers": {"meta": {"access_token": "dca:fixture-login", "user_email": "example@example.com"}}}))
        before = path.read_bytes()
        with patch.dict("os.environ", {"XDG_CONFIG_HOME": "relative-ignored"}):
            credential = usage.muse_credentials(self.home)
        self.assertEqual(credential.email, "example@example.com")
        self.assertEqual(before, path.read_bytes())


    def test_cursor_native_sqlite_and_email(self):
        path = self.home / "Library/Application Support/Cursor/User/globalStorage/state.vscdb"
        path.parent.mkdir(parents=True)
        connection = sqlite3.connect(path)
        connection.execute("CREATE TABLE ItemTable(key TEXT PRIMARY KEY,value TEXT)")
        token = fake_jwt({"sub": "user_123", "exp": 2000000000})
        connection.executemany("INSERT INTO ItemTable VALUES(?,?)", [
            ("cursorAuth/accessToken", token), ("cursorAuth/cachedEmail", "example@example.com"),
        ])
        connection.commit()
        connection.close()
        credential = usage.cursor_credentials(self.home, "mac")
        self.assertEqual(credential.access, token)
        self.assertEqual(credential.email, "example@example.com")


class AntigravityAppMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.temp.name)
        self.path = self.home / ".ccs/account-usage/antigravity-oauth-client.json"
        self.path.parent.mkdir(parents=True)
        self.metadata = {"schemaVersion": 1, "clientId": "fixture-client.apps.googleusercontent.com",
                         "clientSecret": "synthetic-app-value"}

    def tearDown(self):
        self.temp.cleanup()

    def write(self, value=None, mode=0o600):
        self.path.write_text(json.dumps(self.metadata if value is None else value))
        self.path.chmod(mode)

    def test_private_metadata_read_uses_supplied_home_without_writes(self):
        self.write()
        before = self.path.read_bytes()
        self.assertEqual(usage.antigravity_oauth_client(self.home), (self.metadata["clientId"], self.metadata["clientSecret"]))
        self.assertEqual(before, self.path.read_bytes())

    @patch("desktop_usage.request_json")
    def test_missing_metadata_never_contacts_google_or_claims_failed_login(self, request):
        with self.assertRaises(helpers.UsageError) as error:
            usage.refresh_antigravity(helpers.Credential("old-access", refresh="synthetic-refresh"), self.home)
        self.assertEqual(error.exception.status, "unavailable")
        request.assert_not_called()
        self.assertNotIn("synthetic-refresh", str(error.exception))

    def test_shared_or_symlinked_metadata_is_rejected(self):
        self.write(mode=0o644)
        with self.assertRaises(helpers.UsageError):
            usage.antigravity_oauth_client(self.home)
        self.path.unlink()
        target = self.home / "outside.json"
        target.write_text(json.dumps(self.metadata)); target.chmod(0o600)
        self.path.symlink_to(target)
        with self.assertRaises(helpers.UsageError):
            usage.antigravity_oauth_client(self.home)

    def test_unknown_or_malformed_metadata_is_rejected_safely(self):
        for value in ({}, dict(self.metadata, schemaVersion=True), dict(self.metadata, token="private"),
                      dict(self.metadata, clientId="https://other.example"), dict(self.metadata, clientSecret="bad secret"),
                      dict(self.metadata, clientSecret="x" * 5000)):
            with self.subTest(value=value):
                self.write(value)
                with self.assertRaises(helpers.UsageError) as error:
                    usage.antigravity_oauth_client(self.home)
                self.assertNotIn("bad secret", str(error.exception))

    @patch("desktop_usage.request_json", return_value={"access_token": "renewed-fixture"})
    def test_refresh_has_fixed_google_route_and_keeps_original_credentials(self, request):
        self.write()
        original = helpers.Credential("old-access", "Antigravity CLI", "synthetic-refresh", email="example@example.com")
        renewed = usage.refresh_antigravity(original, self.home)
        self.assertEqual(request.call_args.args, ("https://oauth2.googleapis.com/token",))
        self.assertTrue(request.call_args.kwargs["form"])
        self.assertEqual(request.call_args.kwargs["body"]["client_id"], self.metadata["clientId"])
        self.assertEqual(request.call_args.kwargs["body"]["client_secret"], self.metadata["clientSecret"])
        self.assertEqual(renewed.access, "renewed-fixture")
        self.assertEqual(original.access, "old-access")
        self.assertEqual(renewed.refresh, original.refresh)
        self.assertEqual(renewed.email, original.email)

    @patch("desktop_usage.antigravity_credentials", return_value=helpers.Credential("old-access", refresh="synthetic-refresh", expires=1))
    @patch("desktop_usage.request_json")
    def test_collect_isolates_home_and_missing_metadata_leaves_limits_unknown(self, request, credentials):
        result = usage.collect("antigravity", "ubuntu", self.home)
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["windows"], [])
        request.assert_not_called()

    @patch("desktop_usage.subprocess.run")
    def test_windows_acl_check_fails_closed_and_never_reads_contents(self, run):
        run.return_value.returncode = 1
        run.return_value.stdout = "private"
        self.assertFalse(usage.windows_private_metadata(self.path))
        self.assertEqual(run.call_args.kwargs["timeout"], 8)
        self.assertNotIn(self.metadata["clientSecret"], str(run.call_args))



class CollectionTests(unittest.TestCase):
    @patch("desktop_usage.muse_credentials", return_value=helpers.Credential("dca:private-fixture"))
    @patch("desktop_usage.request_json")
    def test_active_muse_with_omitted_usage_is_not_a_sign_in_failure(self, request, _credentials):
        request.return_value = {"user_email": "example@example.com", "subs_tier_name": "High Usage", "is_subs_active": True, "api_key": "sk-never-export"}
        result = usage.collect("muse", "ubuntu")
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["email"], "example@example.com")
        self.assertEqual(result["plan"], "High Usage")
        self.assertIn("omitted", result["message"])
        self.assertEqual(result["windows"], [])
        self.assertNotIn("never-export", json.dumps(result))

    def test_complete_contract_on_unavailable_account(self):
        with tempfile.TemporaryDirectory() as home:
            result = usage.collect("muse", "ubuntu", home)
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["windows"], [])
        self.assertEqual(set(result), {"id", "provider", "providerLabel", "label", "email", "plan", "platform", "source",
                                       "status", "message", "fetchedAt", "sampledAt", "isActive", "windows", "capabilities"})
        self.assertEqual(result["capabilities"], {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []})

    @patch("desktop_usage.muse_credentials", return_value=helpers.Credential("dca:private-fixture"))
    @patch("desktop_usage.request_json")
    def test_successful_dto_cannot_leak_response_key(self, request, _credentials):
        request.return_value = {"api_key": "sk-never-export", "user_email": "example@example.com", "subs_tier_name": "High Usage",
                                "subs_usage": {"weekly": {"used_percent": 6, "resets_at": 1790812800}}}
        result = usage.collect("muse", "ubuntu")
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["email"], "example@example.com")
        encoded = json.dumps(result)
        self.assertNotIn("never-export", encoded)
        self.assertNotIn("private-fixture", encoded)
        self.assertNotIn("api_key", encoded)

    @patch("desktop_usage.muse_credentials", return_value=helpers.Credential("dca:private-fixture"))
    @patch("desktop_usage.request_json", side_effect=Exception("sk-secret in a private upstream error"))
    def test_exception_details_are_never_emitted(self, _request, _credentials):
        result = usage.collect("muse", "ubuntu")
        self.assertEqual(result["status"], "error")
        self.assertNotIn("sk-secret", json.dumps(result))
        self.assertEqual(result["windows"], [])

    @patch("desktop_usage.antigravity_oauth_client", return_value=("fixture-client.apps.googleusercontent.com", "synthetic-app-value"))
    @patch("desktop_usage.request_json")
    def test_google_refresh_is_in_memory_and_bounded_to_expired_token(self, request, client):
        credential = helpers.Credential("expired-access", "OMP", "fixture-refresh", 1, "example@example.com", "project-fixture")
        request.side_effect = [
            {"access_token": "renewed-access", "expires_in": 3600},
            {"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}},
            {"groups": [{"buckets": [{"bucketId": "limit", "remainingFraction": .25}]}]},
        ]
        result = helpers.account("antigravity", "mac")
        usage.fetch_antigravity(credential, result)
        self.assertEqual(credential.access, "expired-access")
        self.assertEqual(credential.refresh, "fixture-refresh")
        self.assertEqual(request.call_args_list[0].args[0], "https://oauth2.googleapis.com/token")
        self.assertTrue(request.call_args_list[0].kwargs["form"])
        self.assertEqual(request.call_args_list[1].args[1]["Authorization"], "Bearer renewed-access")
        self.assertEqual(result["windows"][0]["remainingPercent"], 25)
        self.assertNotIn("renewed-access", json.dumps(result))

    @patch("desktop_usage.antigravity_oauth_client", return_value=("fixture-client.apps.googleusercontent.com", "synthetic-app-value"))
    @patch("desktop_usage.request_json")
    def test_google_401_refreshes_at_most_once(self, request, client):
        credential = helpers.Credential("old-access", "OMP", "fixture-refresh")
        request.side_effect = [
            helpers.UsageError("needs_sign_in", "Authentication failed."),
            {"access_token": "renewed-access"},
            helpers.UsageError("needs_sign_in", "Authentication failed."),
        ]
        with self.assertRaises(helpers.UsageError):
            usage.fetch_antigravity(credential, helpers.account("antigravity", "ubuntu"))
        self.assertEqual(request.call_count, 3)


class HttpSafetyTests(unittest.TestCase):
    @patch("desktop_helpers.urllib.request.build_opener")
    def test_native_user_agent_survives_and_request_has_eight_second_timeout(self, opener):
        response = opener.return_value.open.return_value.__enter__.return_value
        response.read.return_value = b'{"groups":[]}'
        helpers.request_json("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
                             {"User-Agent": "antigravity/1.0.0", "Authorization": "Bearer private-fixture"}, {})
        request = opener.return_value.open.call_args.args[0]
        self.assertEqual(request.get_header("User-agent"), "antigravity/1.0.0")
        self.assertEqual(opener.return_value.open.call_args.kwargs["timeout"], 8)
        self.assertEqual(response.read.call_args.args, (65537,))

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_http_unauthorized_response_body_is_never_read_or_emitted(self, opener):
        error = urllib.error.HTTPError("https://cursor.com/api/usage-summary", 401,
                                      "private fixture credential", {}, None)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as context:
            helpers.request_json("https://cursor.com/api/usage-summary")
        self.assertEqual(context.exception.status, "needs_sign_in")
        self.assertNotIn("private fixture", str(context.exception))

    def test_redirect_cannot_forward_authorization_to_another_host(self):
        with self.assertRaises(helpers.UsageError):
            helpers.NoRedirect().redirect_request(None, None, 302, "private fixture", {}, "https://other.example")


if __name__ == "__main__":
    unittest.main()
