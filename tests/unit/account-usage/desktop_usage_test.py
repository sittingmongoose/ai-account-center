"""Fixture tests for read-only collectors; no network or real credentials."""

import base64
import hashlib
import io
import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import Mock, patch

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

    def test_antigravity_gemini_window_derives_from_per_model_buckets(self):
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T15:27:22Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:30Z"}]}]})
        payload = {"buckets": [
            {"modelId": "gemini-3.8-flash-tiered", "tokenType": "WTUS", "remainingFraction": .55, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.1-pro", "tokenType": "WTUS", "remainingFraction": .4, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "claude-opus-5-5-high", "tokenType": "WTUS", "remainingFraction": .9, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-2.5-pro", "tokenType": "WTUS", "remainingFraction": .8},
        ]}
        windows = usage.normalize_antigravity_gemini(payload, summary)
        self.assertEqual(len(windows), 1)
        window = windows[0]
        self.assertEqual(window["key"], "gemini-5h")
        self.assertEqual(window["label"], "Gemini Models · 5-hour")
        self.assertEqual(window["remainingPercent"], 40)
        self.assertEqual(window["usedPercent"], 60)
        self.assertEqual(window["resetAt"], "2026-10-25T20:42:36Z")
        self.assertEqual(window["windowMinutes"], 300)
        self.assertEqual(window["modelIds"], ["gemini-3.1-pro", "gemini-3.8-flash-tiered"])
        self.assertNotIn("WTUS", json.dumps(windows))

    def test_antigravity_gemini_period_is_adopted_only_from_matching_summary_resets(self):
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T15:51:10Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:51:00Z"}]}]})
        weekly = usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash-tiered", "remainingFraction": .35, "resetTime": "2026-11-01T15:51:22Z"}]}, summary)
        self.assertEqual(weekly[0]["key"], "gemini-weekly")
        self.assertEqual(weekly[0]["label"], "Gemini Models · Weekly")
        self.assertEqual(weekly[0]["windowMinutes"], 10080)
        unmatched = usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.1-pro", "remainingFraction": .5, "resetTime": "2026-10-26T09:00:00Z"}]}, summary)
        self.assertEqual(unmatched[0]["key"], "gemini-window")
        self.assertEqual(unmatched[0]["label"], "Gemini Models")
        self.assertIsNone(unmatched[0]["windowMinutes"])

    def test_antigravity_gemini_conflicting_anchor_periods_stay_unknown(self):
        # A weekly and a 5-hour anchor resetting near the same instant match one
        # Gemini reset; the period must stay unknown whichever order they come in.
        anchors = [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-10-25T20:45:00Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:50:00Z"}]}]
        payload = {"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": .5, "resetTime": "2026-10-25T20:50:06Z"}]}
        for order in ({'groups': anchors}, {'groups': [{"displayName": "Claude and GPT models",
                                                        "buckets": list(reversed(anchors[0]["buckets"]))}]}):
            windows = usage.normalize_antigravity_gemini(payload, usage.normalize_antigravity(order))
            self.assertEqual(windows[0]["key"], "gemini-window")
            self.assertIsNone(windows[0]["windowMinutes"])
            self.assertEqual(windows[0]["label"], "Gemini Models")

    def test_antigravity_gemini_keys_are_stable_across_present_groups(self):
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T15:00:00Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:30Z"}]}]})
        both = {"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": .5, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.1-pro", "remainingFraction": .8, "resetTime": "2026-11-01T15:00:06Z"}]}
        mixed = usage.normalize_antigravity_gemini(both, summary)
        self.assertEqual([w["key"] for w in mixed], ["gemini-5h", "gemini-weekly"])
        weekly_alone = usage.normalize_antigravity_gemini({"buckets": [both["buckets"][1]]}, summary)
        self.assertEqual(weekly_alone[0]["key"], "gemini-weekly")
        # Only a repeated same-period group is suffixed.
        repeated = usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": .5, "resetTime": "2026-10-25T20:50:06Z"},
            {"modelId": "gemini-3.1-pro", "remainingFraction": .7, "resetTime": "2026-10-25T20:53:00Z"}]},
            usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:50:00Z"}]}]}))
        self.assertEqual([w["key"] for w in repeated], ["gemini-5h", "gemini-5h-2"])

    def test_antigravity_gemini_resets_across_a_second_boundary_share_one_window(self):
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:35Z"}]}]})
        windows = usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": .5, "resetTime": "2026-10-25T20:42:34.900Z"},
            {"modelId": "gemini-3.1-pro", "remainingFraction": .6, "resetTime": "2026-10-25T20:42:36.100Z"}]}, summary)
        self.assertEqual(len(windows), 1)
        self.assertEqual(windows[0]["key"], "gemini-5h")
        self.assertEqual(windows[0]["resetAt"], "2026-10-25T20:42:34Z")
        self.assertEqual(windows[0]["remainingPercent"], 50)

    def test_antigravity_gemini_missing_fraction_is_an_exhausted_window(self):
        # proto3 JSON omits zero-valued floats: a bucket with a reset and no
        # remainingFraction has run out, so the shared minimum is 0, and a model
        # whose id cannot be displayed still counts without being listed.
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:30Z"}]}]})
        windows = usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": .55, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.1-pro", "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-sk-task-1", "remainingFraction": .9, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.8-flash", "remainingFraction": .7, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.5-flash", "remainingAmount": "1200", "resetTime": "2026-10-25T20:42:36Z"}]}, summary)
        self.assertEqual(len(windows), 1)
        self.assertEqual(windows[0]["key"], "gemini-5h")
        self.assertEqual(windows[0]["remainingPercent"], 0)
        self.assertEqual(windows[0]["usedPercent"], 100)
        self.assertEqual(windows[0]["modelIds"], ["gemini-3.1-pro", "gemini-3.8-flash"])
        # An amount without a fraction has no denominator: no window, not a
        # fabricated 0%.
        self.assertEqual(usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.5-flash", "remainingAmount": "1200", "resetTime": "2026-10-25T20:42:36Z"}]}, summary), [])

    def test_antigravity_summary_with_version_gated_gemini_and_argon_groups(self):
        # The current-client User-Agent receives the Gemini and Argon groups the
        # 1.0.0-era User-Agent stopped seeing on 2026-10-22; the plain summary
        # normalizer already carries them, keyed by Google's own bucket ids.
        windows = usage.normalize_antigravity({"groups": [
            {"displayName": "Gemini Models", "buckets": [
                {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T18:36:00Z"},
                {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T23:36:00Z"}]},
            {"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T18:36:00Z"},
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T23:36:00Z"}]},
            {"displayName": "Gemini 4 Argon", "buckets": [
                {"bucketId": "gemini-argon-wtu-weekly", "window": "weekly", "remainingFraction": .93, "resetTime": "2026-11-01T11:10:00Z"}]},
        ]})
        self.assertEqual([window["key"] for window in windows],
                         ["gemini-weekly", "gemini-5h", "3p-weekly", "3p-5h", "gemini-argon-wtu-weekly"])
        argon = windows[-1]
        self.assertEqual(argon["label"], "Gemini 4 Argon · Weekly")
        self.assertEqual(argon["remainingPercent"], 93)

    def test_antigravity_gemini_derivation_requires_provider_reset_and_summary_gap(self):
        summary = usage.normalize_antigravity({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:30Z"}]}]})
        buckets_without_reset = {"buckets": [{"modelId": "gemini-3.8-flash-tiered", "remainingFraction": .5}]}
        self.assertEqual(usage.normalize_antigravity_gemini(buckets_without_reset, summary), [])
        self.assertEqual(usage.normalize_antigravity_gemini(None, summary), [])
        with_gemini_keys = summary + [{"key": "gemini-5h", "label": "Gemini Models · 5-hour"}]
        self.assertEqual(usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash-tiered", "remainingFraction": .5, "resetTime": "2026-10-25T20:42:36Z"}]},
            with_gemini_keys), [])
        # A restored summary group without bucket ids must also stand the derivation down.
        with_gemini_label = summary + [{"key": "group-0-0", "label": "Gemini Models · 5-hour",
                                        "resetAt": "2026-10-25T20:42:30Z", "windowMinutes": 300}]
        self.assertEqual(usage.normalize_antigravity_gemini({"buckets": [
            {"modelId": "gemini-3.8-flash-tiered", "remainingFraction": .5, "resetTime": "2026-10-25T20:42:36Z"}]},
            with_gemini_label), [])

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
        # An absolute XDG_CONFIG_HOME on the host machine would make the
        # collectors read the real ~/.config instead of this fixture home,
        # failing assertions with the real credentials in the diff.
        self._env = patch.dict("os.environ", {"XDG_CONFIG_HOME": str(self.home / ".config")})
        self._env.start()
        self.addCleanup(self._env.stop)

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
    def test_revoked_refresh_grant_is_sign_in_on_the_live_collector_path(self, _credentials):
        self.write()
        error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 400, "Bad Request", {},
                                      io.BytesIO(b'{"error":"invalid_grant","error_description":"Token has been expired or revoked."}'))
        self.addCleanup(error.close)
        with patch("desktop_helpers.urllib.request.build_opener") as opener:
            opener.return_value.open.side_effect = error
            result = usage.collect("antigravity", "ubuntu", self.home)
        self.assertEqual(opener.return_value.open.call_count, 1)
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(result["message"], "The saved account's sign-in has expired or was revoked.")
        self.assertEqual(result["windows"], [])
        encoded = json.dumps(result)
        self.assertNotIn("Token has been", encoded)
        self.assertNotIn("synthetic-refresh", encoded)

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
        with tempfile.TemporaryDirectory() as home:
            result = usage.collect("muse", "ubuntu", home)
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["email"], "example@example.com")
        self.assertEqual(result["plan"], "High Usage")
        self.assertIn("omitted", result["message"])
        self.assertEqual(result["windows"], [])
        self.assertNotIn("never-export", json.dumps(result))

    def test_complete_contract_on_unavailable_account(self):
        with tempfile.TemporaryDirectory() as home, \
                patch.dict("os.environ", {"XDG_CONFIG_HOME": str(pathlib.Path(home) / ".config")}):
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
        with tempfile.TemporaryDirectory() as home:
            result = usage.collect("muse", "ubuntu", home)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["email"], "example@example.com")
        encoded = json.dumps(result)
        self.assertNotIn("never-export", encoded)
        self.assertNotIn("private-fixture", encoded)
        self.assertNotIn("api_key", encoded)

    @patch("desktop_usage.muse_credentials", return_value=helpers.Credential("dca:private-fixture"))
    @patch("desktop_usage.request_json", side_effect=Exception("sk-secret in a private upstream error"))
    def test_exception_details_are_never_emitted(self, _request, _credentials):
        with tempfile.TemporaryDirectory() as home:
            result = usage.collect("muse", "ubuntu", home)
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
            helpers.UsageError("unavailable", "Per-model quota is unavailable."),
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

    @patch("desktop_usage.request_json")
    def test_antigravity_fetch_reads_per_model_quota_for_gemini_windows(self, request):
        request.side_effect = [
            {"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}},
            {"groups": [{"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": .7, "resetTime": "2026-10-25T20:42:30Z"}]}]},
            {"buckets": [{"modelId": "gemini-3.8-flash-tiered", "remainingFraction": .55, "resetTime": "2026-10-25T20:42:36Z"}]},
        ]
        result = helpers.account("antigravity", "ubuntu")
        usage.fetch_antigravity_quota(helpers.Credential("fixture-access"), result)
        self.assertEqual(request.call_count, 3)
        self.assertTrue(request.call_args_list[2].args[0].endswith("retrieveUserQuota"))
        self.assertEqual(request.call_args_list[2].args[2], {"project": "project-fixture"})
        self.assertEqual([window["key"] for window in result["windows"]], ["3p-5h", "gemini-5h"])

    @patch("desktop_usage.request_json")
    def test_antigravity_fetch_skips_per_model_read_when_summary_has_gemini(self, request):
        request.side_effect = [
            {"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}},
            {"groups": [
                {"displayName": "Gemini Models", "buckets": [
                    {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": .4, "resetTime": "2026-10-25T20:42:30Z"}]},
                {"displayName": "Claude and GPT models", "buckets": [
                    {"bucketId": "3p-5h", "window": "5h", "remainingFraction": .7, "resetTime": "2026-10-25T20:42:30Z"}]}]},
        ]
        result = helpers.account("antigravity", "ubuntu")
        usage.fetch_antigravity_quota(helpers.Credential("fixture-access"), result)
        self.assertEqual(request.call_count, 2)
        self.assertEqual([window["key"] for window in result["windows"]], ["gemini-5h", "3p-5h"])

    @patch("desktop_usage.time")
    @patch("desktop_usage.request_json")
    def test_antigravity_budget_spans_the_token_refresh(self, request, clock):
        # The budget starts at process start (module import), so a 6s refresh
        # plus 6s load and summary calls -- 18s in all -- leaves no room for
        # the optional per-model read; it must be skipped so the summary
        # still publishes inside the transport deadline.
        clock.monotonic.return_value = 118.0
        request.side_effect = [
            {"access_token": "renewed-access", "expires_in": 3600},
            {"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}},
            {"groups": [{"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": .7, "resetTime": "2026-10-25T20:42:30Z"}]}]},
        ]
        credential = helpers.Credential("expired-access", "OMP", "fixture-refresh", 1,
                                         "example@example.com", "project-fixture")
        result = helpers.account("antigravity", "ubuntu")
        with patch.object(usage, "AGY_BUDGET_STARTED", 100.0):
            usage.fetch_antigravity(credential, result)
        self.assertEqual(request.call_count, 3)
        self.assertEqual([window["key"] for window in result["windows"]], ["3p-5h"])

    @patch("desktop_usage.request_json")
    def test_antigravity_per_model_throttle_publishes_windows_and_keeps_cooldown(self, request):
        request.side_effect = [
            {"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}},
            {"groups": [{"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": .7, "resetTime": "2026-10-25T20:42:30Z"}]}]},
            helpers.UsageError("rate_limited", "Throttled.", retry_after=1800),
        ]
        result = helpers.account("antigravity", "ubuntu")
        usage.fetch_antigravity_quota(helpers.Credential("fixture-access"), result)
        self.assertEqual([window["key"] for window in result["windows"]], ["3p-5h"])
        self.assertEqual(result["retryAfterSeconds"], 1800)
        self.assertNotIn("Throttled", json.dumps(result))

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
                             {"User-Agent": usage.AGY_USER_AGENT, "Authorization": "Bearer private-fixture"}, {})
        request = opener.return_value.open.call_args.args[0]
        self.assertEqual(request.get_header("User-agent"), usage.AGY_USER_AGENT)
        # The summary gates its Gemini groups on this version prefix.
        self.assertTrue(usage.AGY_USER_AGENT.startswith("antigravity/hub/2."))
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

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_http_429_keeps_legacy_error_status_and_safe_structured_cooldown(self, opener):
        body = Mock()
        body.read.return_value = b"sk-private-response"
        error = urllib.error.HTTPError(usage.MUSE_URL, 429, "private upstream detail",
                                      {"Retry-After": "1800"}, body)
        self.addCleanup(error.close)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as caught:
            helpers.request_json(usage.MUSE_URL, {"Authorization": "Bearer dca:private-fixture"}, {})
        failure = caught.exception
        self.assertEqual((failure.status, failure.failure_code, failure.retry_after), ("error", "rate_limited", 1800))
        self.assertEqual(failure.message, "The usage service is limiting requests. Refresh resumes after a cooldown.")
        self.assertIsNone(failure.__cause__)
        body.read.assert_not_called()
        for secret in ("sk-private-response", "private upstream detail", "dca:private-fixture"):
            self.assertNotIn(secret, str(failure))

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_http_429_date_and_invalid_retry_headers_are_bounded(self, opener):
        # A fractional observation time makes the HTTP-date branch produce a
        # fractional delay; the cooldown must ceil to whole int seconds so no
        # consumer's integer gate can discard the provider's hold.
        for header, expected in (("Thu, 01 Jan 1970 00:46:40 GMT", 1800), ("0", 300),
                                 ("999999999999", 86400), (None, 600), ("malformed-private-value", 600),
                                 ("NaN", 600), ("-1", 600), ("x" * 129, 600)):
            with self.subTest(header=header):
                body = Mock()
                error = urllib.error.HTTPError(usage.MUSE_URL, 429, "private", {"Retry-After": header}, body)
                self.addCleanup(error.close)
                opener.return_value.open.side_effect = error
                with patch("desktop_helpers.time.time", return_value=1000.25), self.assertRaises(helpers.UsageError) as caught:
                    helpers.request_json(usage.MUSE_URL)
                self.assertEqual(caught.exception.status, "error")
                self.assertEqual(caught.exception.failure_code, "rate_limited")
                self.assertEqual(caught.exception.retry_after, expected)
                self.assertIsInstance(caught.exception.retry_after, int)
                body.read.assert_not_called()

    def test_legacy_usage_error_callers_need_no_structured_arguments(self):
        error = helpers.UsageError("error", "Fixed safe description.")
        self.assertEqual(error.status, "error")
        self.assertIsNone(error.failure_code)
        self.assertIsNone(error.retry_after)

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_oauth_invalid_grant_is_sign_in_and_never_echoes_the_body(self, opener):
        error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 400, "Bad Request", {},
                                      io.BytesIO(b'{"error":"invalid_grant","error_description":"contains ya29.FAKE-secret"}'))
        self.addCleanup(error.close)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as context:
            helpers.request_json("https://oauth2.googleapis.com/token", body={"refresh_token": "fixture"}, form=True)
        self.assertEqual(context.exception.status, "needs_sign_in")
        self.assertEqual(context.exception.message, "The saved account's sign-in has expired or was revoked.")
        self.assertIsNone(context.exception.__cause__)
        for leaked in ("ya29", "FAKE-secret", "invalid_grant", "contains"):
            self.assertNotIn(leaked, str(context.exception))

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_oauth_invalid_grant_body_read_is_capped_at_four_kib(self, opener):
        body = Mock()
        body.read.return_value = b'{"error":"invalid_grant"}'
        error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 400, "Bad Request", {}, body)
        self.addCleanup(error.close)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as context:
            helpers.request_json("https://oauth2.googleapis.com/token")
        self.assertEqual(context.exception.status, "needs_sign_in")
        body.read.assert_called_once_with(4096)

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_other_oauth_400_bodies_keep_the_generic_unavailable_error(self, opener):
        for body in (b'{"error":"invalid_request"}', b'not json', b'', b'["invalid_grant"]',
                     b'{"error":["invalid_grant"]}', b'{"error":"invalid_grant \xff"}'):
            with self.subTest(body=body):
                error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 400, "Bad Request", {}, io.BytesIO(body))
                self.addCleanup(error.close)
                opener.return_value.open.side_effect = error
                with self.assertRaises(helpers.UsageError) as context:
                    helpers.request_json("https://oauth2.googleapis.com/token")
                self.assertEqual(context.exception.status, "error")
                self.assertEqual(context.exception.message, "The usage service is temporarily unavailable.")

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_failed_oauth_400_body_read_keeps_the_generic_error(self, opener):
        body = Mock()
        body.read.side_effect = OSError("fixture socket reset")
        error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 400, "Bad Request", {}, body)
        self.addCleanup(error.close)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as context:
            helpers.request_json("https://oauth2.googleapis.com/token")
        self.assertEqual(context.exception.status, "error")
        self.assertNotIn("socket reset", str(context.exception))

    @patch("desktop_helpers.urllib.request.build_opener")
    def test_invalid_grant_body_maps_only_from_http_400(self, opener):
        error = urllib.error.HTTPError("https://oauth2.googleapis.com/token", 500, "Server Error", {},
                                      io.BytesIO(b'{"error":"invalid_grant"}'))
        self.addCleanup(error.close)
        opener.return_value.open.side_effect = error
        with self.assertRaises(helpers.UsageError) as context:
            helpers.request_json("https://oauth2.googleapis.com/token")
        self.assertEqual(context.exception.status, "error")

    def test_redirect_cannot_forward_authorization_to_another_host(self):
        with self.assertRaises(helpers.UsageError):
            helpers.NoRedirect().redirect_request(None, None, 302, "private fixture", {}, "https://other.example")


if __name__ == "__main__":
    unittest.main()
