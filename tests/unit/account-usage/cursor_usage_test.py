"""Cursor fixtures use public client schemas, synthetic tokens, and no live calls."""

import base64
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import cursor_usage as cursor
import desktop_helpers as desktop
import desktop_usage
import failover_additional
import plan_common

NOW = "2026-10-14T12:00:00Z"
RESET_MS = 1792022400000
RESET = "2026-10-15T00:00:00Z"


def credential(plan="ultra"):
    payload = base64.urlsafe_b64encode(json.dumps({"sub": "auth0|fixture", "email": "one@example.test"}).encode()).decode().rstrip("=")
    return desktop.Credential("fixture." + payload + ".signature", "Fixture", email="one@example.test", plan=plan)


def plan_info(**changes):
    info = {"includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY", "includedUsageResetsAt": str(RESET_MS),
            "billingCycleEnd": "1794697200000", "planName": "Premium"}
    info.update(changes)
    return {"isMerged": True, "planInfo": info}


def current(value=12.3456):
    return {"enabled": True, "billingCycleEnd": "1794697200000", "planUsage": {
        "totalPercentUsed": value, "autoPercentUsed": 91, "apiPercentUsed": 98,
        "includedSpend": 100, "totalSpend": 100, "limit": 10000}, "private": "not-for-export"}


def summary():
    return {"billingCycleEnd": "2026-11-14T00:00:00Z", "individualUsage": {
        "plan": {"used": 1234, "limit": 10000, "autoPercentUsed": 7, "apiPercentUsed": 35},
        "onDemand": {"used": 250, "limit": 2000, "remaining": 1750}}}


class CursorTests(unittest.TestCase):
    def setUp(self):
        self.addCleanup(patch.stopall)
        patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("Network forbidden in fixtures")).start()
        patch.object(cursor, "utc_now", return_value=NOW).start()
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)

    def fake_client(self, replies):
        client = Mock()
        client.get.side_effect = replies
        self.factory = patch.object(plan_common, "HttpClient", return_value=client).start()
        return client

    def failover(self, token=None):
        token = token or credential()
        candidate = {"_provider": "cursor", "_credential": token, "email": token.email, "plan": token.plan}
        return failover_additional.collect(candidate, Path(self.temporary.name), "ubuntu")

    def native(self, token=None):
        token = token or credential()
        with patch.object(desktop_usage, "cursor_credentials", return_value=token):
            return desktop_usage.collect("cursor", "ubuntu", home=self.temporary.name)

    def assert_observation(self, result, plan):
        self.assertEqual(result["observedPlan"], plan)
        self.assertEqual(result["observedPlanAt"], NOW)

    def test_all_new_tiers_use_shared_total_and_separate_spend(self):
        for plan in sorted(cursor.NEW_PLANS):
            with self.subTest(plan=plan):
                client = self.fake_client([{"membershipType": plan}, current(), plan_info(), summary(), {}])
                result = self.failover()
                self.assert_observation(result, plan)
                sample = result["sample"]
                self.assertEqual(sample["plan"], plan)
                pool, spend = sample["windows"]
                self.assertEqual(pool["key"], "grok-shared-weekly")
                self.assertEqual(pool["label"], "Grok" if plan == "plus" else "Grok and Composer")
                self.assertEqual(pool["usedPercent"], 12.3456)
                self.assertEqual(pool["resetAt"], RESET)
                self.assertEqual(pool["windowMinutes"], 10080)
                for key in ("used", "limit", "unit"):
                    self.assertIsNone(pool[key])
                self.assertEqual(spend["kind"], "extra_usage")
                self.assertEqual(spend["used"], 2.5)
                self.assertEqual(spend["limit"], 20)
                self.assertEqual(spend["resetAt"], "2026-11-14T00:00:00Z")
                self.assertEqual(client.get.call_count, 5)
                self.assertNotIn("not-for-export", json.dumps(result))
                self.factory.assert_called_once_with(deadline_seconds=16)

    def test_native_and_failover_use_the_same_plan_and_quota_contract(self):
        replies = [{"membershipType": "premium"}, current(0), plan_info(), {}, {}]
        self.fake_client(replies)
        native = self.native()
        self.assertEqual(native["status"], "ok")
        self.assert_observation(native, "premium")
        self.fake_client(replies)
        failover = self.failover()
        self.assertEqual(native["windows"], failover["sample"]["windows"])
        self.assertEqual(native["windows"][0]["usedPercent"], 0)

    def test_legacy_plans_preserve_existing_split_and_spend_semantics(self):
        for plan in ("free", "pro", "pro_plus", "ultra", "enterprise", "free_trial", "pro_student", "express"):
            with self.subTest(plan=plan):
                client = self.fake_client([{"membershipType": plan}, summary(), {}])
                result = self.failover(credential("premium"))
                self.assert_observation(result, plan)
                self.assertEqual(result["sample"]["windows"], desktop_usage.normalize_cursor(summary()))
                self.assertEqual([call.args[0] for call in client.get.call_args_list],
                                 [cursor.MEMBERSHIP_URL, cursor.SUMMARY_URL, cursor.PROFILE_URL])

    def test_reset_is_only_explicit_weekly_milliseconds(self):
        for value in (None, "later", True, "0", str(RESET_MS // 1000), float("inf")):
            with self.subTest(value=value):
                window = cursor.normalize_shared(current(), plan_info(includedUsageResetsAt=value), "premium")[0]
                self.assertIsNone(window["resetAt"])
        for period in (None, True, False, "weekly"):
            with self.subTest(period=period):
                window = cursor.normalize_shared(current(), plan_info(includedUsagePeriod=period), "premium")[0]
                self.assertIsNone(window["resetAt"])
        self.assertEqual(cursor.normalize_shared(current(), plan_info(includedUsagePeriod=2), "premium")[0]["resetAt"], RESET)

    def test_cadence_is_reported_independently_of_plan_name(self):
        for period in (None, False, True, "weekly", "monthly", 0, 3):
            with self.subTest(period=period):
                window = cursor.normalize_shared(current(), plan_info(includedUsagePeriod=period), "premium")[0]
                self.assertEqual(window["key"], "grok-shared-usage")
                self.assertIsNone(window["windowMinutes"])
                self.assertIsNone(window["resetAt"])
        for period in (1, "INCLUDED_USAGE_PERIOD_MONTHLY"):
            window = cursor.normalize_shared(current(), plan_info(includedUsagePeriod=period), "premium")[0]
            self.assertEqual(window["key"], "grok-shared-monthly")
            self.assertIsNone(window["windowMinutes"])
            self.assertNotEqual(window["resetAt"], RESET)

    def test_missing_or_invalid_percentage_never_becomes_zero_or_legacy_bucket(self):
        for value in ({}, {"planUsage": {"autoPercentUsed": 10}}, current(None), current(True), current("12"), current(-1)):
            self.assertEqual(cursor.normalize_shared(value, plan_info(), "premium"), [])
        self.assertEqual(cursor.normalize_shared(current(113), plan_info(), "premium")[0]["usedPercent"], 113)

    def test_plan_change_survives_primary_quota_and_unexpected_failure(self):
        for error in (plan_common.CollectionError("error", "private provider error"), ValueError("private detail")):
            with self.subTest(error=type(error).__name__):
                self.fake_client([{"membershipType": "premium"}, error])
                result = self.failover()
                self.assertIsNone(result["sample"])
                self.assert_observation(result, "premium")
                self.assertNotIn("private", json.dumps(result))
        self.fake_client([{"membershipType": "premium"}, plan_common.CollectionError("error", "Provider unavailable")])
        native = self.native()
        self.assertEqual(native["status"], "unavailable")
        self.assertEqual(native["windows"], [])
        self.assert_observation(native, "premium")

    def test_optional_rate_limit_preserves_pool_and_stops_more_calls(self):
        error = plan_common.CollectionError("error", "private", failure_code="rate_limited", retry_after=900)
        for replies, count in (([{"membershipType": "premium"}, current(), error], 3),
                               ([{"membershipType": "premium"}, current(), plan_info(), error], 4)):
            client = self.fake_client(replies)
            result = self.failover()
            self.assertEqual(result["failureCode"], "rate_limited")
            self.assertEqual(result["retryAfterSeconds"], 900)
            self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 12.3456)
            self.assertEqual(client.get.call_count, count)
            self.assert_observation(result, "premium")

    def test_primary_rate_limit_keeps_plan_without_reusing_any_quota(self):
        error = plan_common.CollectionError("error", "private", failure_code="rate_limited", retry_after=700)
        self.fake_client([{"membershipType": "premium"}, error])
        result = self.failover()
        self.assertIsNone(result["sample"])
        self.assertEqual(result["retryAfterSeconds"], 700)
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assert_observation(result, "premium")

    def test_optional_metadata_failure_does_not_invent_reset(self):
        error = plan_common.CollectionError("error", "private", failure_code="network_error")
        self.fake_client([{"membershipType": "premium"}, current(), error, error, error])
        result = self.failover()
        self.assertIsNone(result["failureCode"])
        self.assertIsNone(result["sample"]["windows"][0]["resetAt"])
        self.assertIsNone(result["sample"]["windows"][0]["windowMinutes"])
        self.assertEqual(result["sample"]["windows"][0]["key"], "grok-shared-usage")
        self.assertNotIn("retryAfterSeconds", result)

    def test_explicit_family_contradiction_rejects_pool_but_keeps_verified_plan(self):
        self.fake_client([{"membershipType": "premium"}, current(), {"isMerged": False}])
        result = self.failover()
        self.assertIsNone(result["sample"])
        self.assertEqual(result["failureCode"], "invalid_response")
        self.assert_observation(result, "premium")

    def test_unknown_tier_needs_explicit_merged_family_and_never_exports_raw_tier(self):
        self.fake_client([{"membershipType": "future_server_tier"}, plan_info(), current(), {}, {}])
        result = self.failover()
        self.assertEqual(result["sample"]["plan"], "unknown")
        self.assert_observation(result, "unknown")
        self.assertNotIn("future_server_tier", json.dumps(result))
        self.fake_client([{"membershipType": "future_server_tier"}, {"isMerged": False}])
        failed = self.failover()
        self.assertIsNone(failed["sample"])
        self.assert_observation(failed, "unknown")

    def test_malformed_membership_never_promotes_cached_desktop_plan(self):
        for value in ({}, {"membershipType": ""}, {"membershipType": None}, {"membershipType": True}, {"membershipType": "Bearer private"}):
            client = self.fake_client([value])
            result = self.failover(credential("premium"))
            self.assertIsNone(result["sample"])
            self.assertNotIn("observedPlan", result)
            self.assertEqual(client.get.call_count, 1)

    def test_identity_conflict_discards_plan_and_quota(self):
        replies = [{"membershipType": "premium"}, current(), plan_info(), {}, {"email": "other@example.test"}]
        self.fake_client(replies)
        result = self.failover()
        self.assertEqual(result["failureCode"], "account_mismatch")
        self.assertIsNone(result["sample"])
        self.assertNotIn("observedPlan", result)
        self.fake_client(replies)
        native = self.native()
        self.assertIsNone(native["plan"])
        self.assertEqual(native["windows"], [])
        self.assertNotIn("observedPlan", native)

    def test_real_http_client_sends_fixed_connect_json_shape_and_bounds_calls(self):
        # Restore the real bounded transport; replace only its network opener.
        client = plan_common.HttpClient(deadline_seconds=16)
        replies = [{"membershipType": "premium"}, current(), plan_info(), {}, {}]
        responses = [io.BytesIO(json.dumps(reply).encode()) for reply in replies]
        with patch.object(client.opener, "open", side_effect=responses) as opened:
            sample = desktop.account("cursor", "ubuntu")
            cursor.collect(credential(), sample, client)
        calls = opened.call_args_list
        self.assertEqual([call.args[0].full_url for call in calls], [cursor.MEMBERSHIP_URL, cursor.CURRENT_USAGE_URL,
                                                                 cursor.PLAN_INFO_URL, cursor.SUMMARY_URL, cursor.PROFILE_URL])
        for index, call in enumerate(calls):
            request = call.args[0]
            headers = {key.lower(): value for key, value in request.header_items()}
            self.assertGreater(call.kwargs["timeout"], 0)
            self.assertLessEqual(call.kwargs["timeout"], 4)
            if index in (1, 2):
                self.assertEqual(request.get_method(), "POST")
                self.assertEqual(request.data, b"{}")
                self.assertEqual(headers["connect-protocol-version"], "1")
                self.assertEqual(headers["content-type"], "application/json")
                self.assertEqual(headers["authorization"], "Bearer " + credential().access)
                self.assertNotIn("cookie", headers)
            else:
                self.assertEqual(request.get_method(), "GET")
                self.assertNotIn("authorization", headers)
                self.assertIn("cookie", headers)

    def test_unsupported_connect_codec_is_sanitized_and_preserves_observation(self):
        client = plan_common.HttpClient(deadline_seconds=16)
        response = io.BytesIO(b'{"membershipType":"premium"}')
        rejected = urllib.error.HTTPError(cursor.CURRENT_USAGE_URL, 415, "private upstream", {}, io.BytesIO(b"private body"))
        with patch.object(client.opener, "open", side_effect=[response, rejected]), \
                patch.object(plan_common, "HttpClient", return_value=client):
            result = self.failover()
        self.assertIsNone(result["sample"])
        self.assertEqual(result["failureCode"], "provider_error")
        self.assert_observation(result, "premium")
        self.assertNotIn("private", json.dumps(result))


if __name__ == "__main__":
    unittest.main()
