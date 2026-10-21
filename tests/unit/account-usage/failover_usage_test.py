"""Offline protocol, output-boundary, and credential-rotation tests."""

import datetime as dt
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import failover_usage as usage
from cursor_failure_contract_fixture import scenario as cursor_scenario, PRIVATE as CURSOR_PRIVATE, token as cursor_token

TOKEN = "fixture-private-access-token"
EMAIL = "fixture@example.com"


def candidate(provider="codex", **overrides):
    row = {"locator": "1" * 64, "selectors": ["discover"],
           "identity": {"kind": sorted(usage.KINDS[provider])[0], "key": "2" * 64},
           "revision": "3" * 64, "email": EMAIL, "plan": "Pro", "_token": TOKEN,
           "_provider": provider, "_raw": {"private": TOKEN}}
    row.update(overrides)
    return row


def public(row):
    return {key: value for key, value in row.items() if key in usage.CANDIDATE_KEYS}


def sample(provider="codex", **overrides):
    stamp = dt.datetime.now(dt.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    row = {"provider": "opencode-go" if provider == "opencode-wallet" else provider,
           "platform": "ubuntu", "status": "ok", "email": EMAIL, "plan": "Pro",
           "fetchedAt": stamp, "sampledAt": stamp, "source": "Native login",
           "id": "private-account-id", "label": "Original label", "message": TOKEN,
           "windows": [{"key": "five_hour", "label": "5-hour usage", "usedPercent": 0,
                        "remainingPercent": 100, "resetAt": None, "windowMinutes": 300,
                        "used": None, "limit": None, "unit": None}], "private": TOKEN,
           "capabilities": {"codexProfile": "not-a-real-profile", "token": TOKEN}}
    row.update(overrides)
    return row


class FakeAdapter:
    def __init__(self, before=None, after=None, result=None):
        self.before = before if before is not None else [candidate()]
        self.after = after if after is not None else self.before
        self.result = result if result is not None else {"sample": sample(), "failureCode": None}
        self.inventory_calls = []
        self.collect_calls = []

    def inventory(self, *args, **kwargs):
        self.inventory_calls.append((args, kwargs))
        return self.before if len(self.inventory_calls) == 1 else self.after

    def collect(self, *args, **kwargs):
        self.collect_calls.append((args, kwargs))
        if isinstance(self.result, Exception):
            raise self.result
        return self.result


class Inventory(list):
    def __init__(self, values=(), statuses=()):
        super().__init__(values)
        self.sign_in_needed_selectors = list(statuses)


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)

    def request(self, action="collect", provider="codex", row=None, **overrides):
        request = {"version": 1, "action": action, "provider": provider, "homeDir": str(self.home)}
        if action == "collect":
            request["candidate"] = public(row or candidate(provider))
        request.update(overrides)
        return request

    def run_helper(self, request, adapter=None):
        adapter = adapter or FakeAdapter()
        with mock.patch.object(usage, "_adapter", return_value=adapter), mock.patch.object(usage.sys, "platform", "linux"):
            return usage.run(request), adapter

    def test_inventory_emits_only_bounded_public_fields(self):
        result, _ = self.run_helper(self.request("inventory"))
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": [public(candidate())]})
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_known_sign_in_selectors_are_optional_claude_only_and_strictly_bounded(self):
        statuses = ["claude-desktop-profile:work", "claude-desktop-profile:work", "claude-profile:default",
                    "discover", "Bearer " + TOKEN, "claude-profile:../../private", "codex-profile:work"]
        adapter = FakeAdapter(before=Inventory(statuses=statuses))
        result, _ = self.run_helper(self.request("inventory", provider="claude"), adapter)
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": [],
                                  "signInNeededSelectors": ["claude-desktop-profile:work", "claude-profile:default"]})
        result, _ = self.run_helper(self.request("inventory", provider="codex"), adapter)
        self.assertNotIn("signInNeededSelectors", result)
        adapter = FakeAdapter(before=Inventory(statuses=["claude-profile:default"] * (usage.MAX_CANDIDATES * 2 + 1)))
        result, _ = self.run_helper(self.request("inventory", provider="claude"), adapter)
        self.assertNotIn("signInNeededSelectors", result)

    def test_collect_reinventories_and_passes_private_current_credential(self):
        result, adapter = self.run_helper(self.request())
        self.assertEqual(len(adapter.inventory_calls), 2)
        self.assertEqual(len(adapter.collect_calls), 1)
        self.assertEqual(adapter.collect_calls[0][0][0]["_token"], TOKEN)
        self.assertEqual(adapter.collect_calls[0][1], {"ccs_dir": self.home / ".ccs"})
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 0)
        self.assertEqual(result["sample"]["capabilities"], {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []})
        self.assertNotIn(TOKEN, json.dumps(result))
        self.assertNotIn("private-account-id", json.dumps(result))

    def test_private_claude_profile_context_is_forwarded_for_both_inventory_checks(self):
        selected = {"id": "work", "email": EMAIL, "profilePath": str(self.home / "PrivateProfile"), "isDefault": False}
        row = candidate("claude")
        adapter = FakeAdapter(before=[row], result={"sample": sample("claude"), "failureCode": None})
        result, adapter = self.run_helper(self.request(provider="claude", row=row, claudeProfile=selected), adapter)
        self.assertIsNone(result["failureCode"])
        self.assertEqual(len(adapter.inventory_calls), 2)
        for _, kwargs in adapter.inventory_calls + adapter.collect_calls:
            self.assertEqual(kwargs, {"ccs_dir": self.home / ".ccs", "claude_profile": selected})
        self.assertNotIn("PrivateProfile", json.dumps(result))

    def test_claude_profile_context_rejects_ambiguous_or_untrusted_fields(self):
        good = {"id": "work", "email": EMAIL, "profilePath": str(self.home / "PrivateProfile")}
        bad = [None, {}, {**good, "token": TOKEN}, {**good, "id": "../work"},
               {**good, "email": "not-email"}, {**good, "isDefault": "true"},
               {**good, "profilePath": "relative/Profile"}, {**good, "profilePath": "//server/share"},
               {**good, "profilePath": str(self.home) + "\x00private"}]
        for value in bad:
            with self.subTest(value=value):
                result, adapter = self.run_helper(self.request("inventory", provider="claude", claudeProfile=value))
                self.assertEqual(result["candidates"], [])
                self.assertEqual(adapter.inventory_calls, [])
        result, adapter = self.run_helper(self.request("inventory", provider="codex", claudeProfile=good))
        self.assertEqual(result["candidates"], [])
        self.assertEqual(adapter.inventory_calls, [])

    def test_rotated_revision_before_collection_never_queries_provider(self):
        adapter = FakeAdapter(before=[candidate(revision="4" * 64)])
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["failureCode"], "source_changed")
        self.assertIsNone(result["candidate"])
        self.assertEqual(adapter.collect_calls, [])

    def test_rotated_identity_before_collection_never_queries_provider(self):
        adapter = FakeAdapter(before=[candidate(identity={"kind": "codex-principal-v1", "key": "4" * 64})])
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["failureCode"], "source_changed")
        self.assertEqual(adapter.collect_calls, [])

    def test_rotation_or_removal_during_query_discards_sample(self):
        for after in ([], [candidate(revision="4" * 64)],
                      [candidate(identity={"kind": "codex-principal-v1", "key": "4" * 64})]):
            result, adapter = self.run_helper(self.request(), FakeAdapter(after=after))
            self.assertEqual(result["failureCode"], "source_changed")
            self.assertIsNone(result["sample"])
            self.assertEqual(len(adapter.collect_calls), 1)

    def test_rotation_or_removal_discards_quota_but_preserves_original_request_429(self):
        for after in ([], [candidate(revision="4" * 64)],
                      [candidate(identity={"kind": "codex-principal-v1", "key": "4" * 64})]):
            adapter = FakeAdapter(after=after, result={"sample": sample(), "failureCode": "rate_limited",
                                                      "retryAfterSeconds": 1800})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertEqual(result, {"version": 1, "action": "collect", "candidate": None,
                                      "sample": None, "failureCode": "source_changed",
                                      "rateLimited": True, "retryAfterSeconds": 1800})
        for retry in (None, -1, float("nan"), TOKEN, 86401, True):
            adapter = FakeAdapter(after=[], result={"failureCode": "rate_limited", "retryAfterSeconds": retry})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertIs(result["rateLimited"], True)
            self.assertNotIn("retryAfterSeconds", result)
        adapter = FakeAdapter(after=[], result={"failureCode": "network_error", "retryAfterSeconds": 1800})
        result, _ = self.run_helper(self.request(), adapter)
        self.assertNotIn("rateLimited", result)
        self.assertNotIn("retryAfterSeconds", result)

    def test_failed_inventory_is_marked_failed_not_reported_as_no_sign_ins(self):
        adapter = FakeAdapter()
        def broken(*args, **kwargs):
            raise OSError("private store unreadable " + TOKEN)
        adapter.inventory = broken
        result, _ = self.run_helper(self.request("inventory"), adapter)
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": [], "failed": True})
        result, _ = self.run_helper(self.request("inventory"), FakeAdapter(before=Inventory()))
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": []})

    def test_failed_post_collection_inventory_preserves_original_rate_limit(self):
        adapter = FakeAdapter(result={"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 1800})
        inventory = adapter.inventory
        def changed_inventory(*args, **kwargs):
            if adapter.inventory_calls:
                raise OSError("private source disappeared")
            return inventory(*args, **kwargs)
        adapter.inventory = changed_inventory
        result, _ = self.run_helper(self.request(), adapter)
        # An unreadable re-check is not a changed sign-in: keep the hold, drop the quota.
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assertNotIn("rateLimited", result)
        self.assertEqual(result["retryAfterSeconds"], 1800)
        self.assertIsNone(result["sample"])

    def test_unreadable_store_marks_inventory_incomplete_and_collection_unavailable(self):
        import desktop_helpers
        adapter = FakeAdapter()
        inventory = adapter.inventory
        def unreadable(*args, **kwargs):
            desktop_helpers.read_failed()
            return []
        adapter.inventory = unreadable
        result, _ = self.run_helper(self.request("inventory"), adapter)
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": [], "incomplete": True})
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["failureCode"], "unavailable")
        # The same missing sign-in on a fully readable store is a changed source.
        adapter.inventory = lambda *args, **kwargs: []
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["failureCode"], "source_changed")
        # A sign-in that disappears on an unreadable re-check after collection is unverified.
        adapter = FakeAdapter()
        first = adapter.inventory
        def later_unreadable(*args, **kwargs):
            if adapter.inventory_calls:
                adapter.inventory_calls.append((args, kwargs))
                desktop_helpers.read_failed()
                return []
            return first(*args, **kwargs)
        adapter.inventory = later_unreadable
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual((result["failureCode"], result["sample"]), ("unavailable", None))

    def test_adapter_binding_rejection_preserves_its_original_rate_limit(self):
        for after in (None, []):
            adapter = FakeAdapter(after=after, result={"sample": sample(), "failureCode": "source_changed",
                                                      "rateLimited": True, "retryAfterSeconds": 1800})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertEqual(result["failureCode"], "source_changed")
            self.assertIs(result["rateLimited"], True)
            self.assertEqual(result["retryAfterSeconds"], 1800)
            self.assertIsNone(result["sample"])

    def test_conflicting_same_locator_sources_are_not_selected(self):
        adapter = FakeAdapter(before=[candidate(), candidate(revision="4" * 64)])
        result, _ = self.run_helper(self.request("inventory"), adapter)
        self.assertEqual(result["candidates"], [])

    def test_caller_metadata_does_not_replace_native_metadata(self):
        request = self.request()
        request["candidate"].update(email="different@example.com", plan="Other plan", selectors=["arbitrary-profile"])
        result, adapter = self.run_helper(request)
        self.assertEqual(result["candidate"]["email"], EMAIL)
        self.assertEqual(adapter.collect_calls[0][0][0]["email"], EMAIL)

    def test_private_fields_in_request_are_rejected_before_inventory(self):
        request = self.request()
        request["candidate"]["_token"] = TOKEN
        result, adapter = self.run_helper(request)
        self.assertEqual(result["failureCode"], "invalid_response")
        self.assertEqual(adapter.inventory_calls, [])

    def test_cross_provider_identity_kinds_and_invalid_shapes_are_rejected(self):
        bad = [candidate(identity={"kind": "muse-email", "key": "2" * 64}),
               candidate(locator="../../private"), candidate(selectors=[]),
               candidate(selectors=["$(command)"]), candidate(email="Bearer " + TOKEN),
               candidate(plan="https://private.invalid/"), candidate(revision="F" * 64)]
        for row in bad:
            result, adapter = self.run_helper(self.request(row=row))
            self.assertEqual(result["failureCode"], "invalid_response")
            self.assertEqual(adapter.inventory_calls, [])

    def test_all_provider_routes_keep_provider_specific_identity(self):
        for provider in usage.KINDS:
            row = candidate(provider)
            adapter = FakeAdapter(before=[row], result={"sample": sample(provider), "failureCode": None})
            result, _ = self.run_helper(self.request(provider=provider, row=row), adapter)
            self.assertIsNone(result["failureCode"], provider)
            self.assertEqual(result["candidate"]["identity"], row["identity"])

    def test_muse_protocol_accepts_only_team_or_exact_native_credential_identities(self):
        for kind in ("muse-team", "muse-native-credential"):
            row = candidate("muse", identity={"kind": kind, "key": "2" * 64})
            adapter = FakeAdapter(before=[row], result={"sample": sample("muse"), "failureCode": None})
            result, _ = self.run_helper(self.request(provider="muse", row=row), adapter)
            self.assertIsNone(result["failureCode"])
        for kind in ("muse-email", "muse-credential"):
            row = candidate("muse", identity={"kind": kind, "key": "2" * 64})
            result, adapter = self.run_helper(self.request(provider="muse", row=row))
            self.assertEqual(result["failureCode"], "invalid_response")
            self.assertEqual(adapter.inventory_calls, [])

    def test_rate_limit_and_retry_after_survive_sanitization(self):
        adapter = FakeAdapter(result={"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 180,
                                      "rawResponse": TOKEN})
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assertEqual(result["retryAfterSeconds"], 180)
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_unknown_failure_and_invalid_cooldown_do_not_escape(self):
        for retry in (-1, True, float("nan"), float("inf"), 86401, TOKEN):
            adapter = FakeAdapter(result={"sample": None, "failureCode": "Bearer " + TOKEN, "retryAfterSeconds": retry})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertEqual(result["failureCode"], "unavailable")
            self.assertNotIn("retryAfterSeconds", result)

    def test_valid_cached_quota_survives_transient_failure_but_not_identity_failure(self):
        for code in ("rate_limited", "network_error", "provider_error", "busy", "unavailable"):
            adapter = FakeAdapter(result={"sample": sample(status="cached"), "failureCode": code,
                                          "retryAfterSeconds": 180, "rawResponse": TOKEN})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertEqual(result["failureCode"], code)
            self.assertEqual(result["sample"]["status"], "cached")
            self.assertEqual(result["retryAfterSeconds"], 180)
            self.assertNotIn(TOKEN, json.dumps(result))
        for code in ("needs_sign_in", "account_mismatch", "plan_mismatch", "source_changed"):
            adapter = FakeAdapter(result={"sample": sample(), "failureCode": code})
            result, _ = self.run_helper(self.request(), adapter)
            self.assertIsNone(result["sample"])

    def test_healthy_refresh_budget_is_bounded_and_never_exposed_after_failure(self):
        adapter = FakeAdapter(result={"sample": sample(status="cached"), "failureCode": None,
                                      "refreshAfterSeconds": 270})
        result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(result["refreshAfterSeconds"], 270)
        for refresh in (-1, 0, True, float("nan"), float("inf"), 86401, TOKEN):
            adapter.result["refreshAfterSeconds"] = refresh
            result, _ = self.run_helper(self.request(), adapter)
            self.assertNotIn("refreshAfterSeconds", result)
        adapter.result.update(failureCode="network_error", refreshAfterSeconds=270)
        result, _ = self.run_helper(self.request(), adapter)
        self.assertNotIn("refreshAfterSeconds", result)

    def test_failed_muse_refresh_can_report_a_sanitized_plan_without_inventing_quota(self):
        row = candidate("muse")
        adapter = FakeAdapter(before=[row], result={"sample": None, "failureCode": "rate_limited",
                                                   "observedPlan": "Max", "observedPlanAt": "2026-10-14T12:00:00.000Z", "retryAfterSeconds": 180})
        result, _ = self.run_helper(self.request(provider="muse", row=row), adapter)
        self.assertEqual(result["observedPlan"], "Max")
        self.assertEqual(result["observedPlanAt"], "2026-10-14T12:00:00.000Z")
        self.assertEqual(result["retryAfterSeconds"], 180)
        self.assertIsNone(result["sample"])
        adapter.result["observedPlan"] = "Bearer " + TOKEN
        result, _ = self.run_helper(self.request(provider="muse", row=row), adapter)
        self.assertNotIn("observedPlan", result)
        self.assertNotIn("observedPlanAt", result)
        adapter.result["observedPlan"] = "Max"
        for at in (None, True, TOKEN, "2099-01-01T00:00:00Z"):
            adapter.result["observedPlanAt"] = at
            result, _ = self.run_helper(self.request(provider="muse", row=row), adapter)
            self.assertNotIn("observedPlan", result)
            self.assertNotIn("observedPlanAt", result)
        adapter.result.update(sample=sample("muse"), failureCode=None,
                              observedPlanAt="2026-10-14T12:00:00.000Z")
        result, _ = self.run_helper(self.request(provider="muse", row=row), adapter)
        self.assertEqual(result["observedPlan"], "Max")
        self.assertEqual(result["observedPlanAt"], "2026-10-14T12:00:00.000Z")

    def test_adapter_error_and_output_are_not_forwarded(self):
        adapter = FakeAdapter(result=ValueError("Bearer " + TOKEN))
        captured_out, captured_err = io.StringIO(), io.StringIO()
        original = adapter.inventory

        def noisy(*args, **kwargs):
            print(TOKEN)
            print(TOKEN, file=sys.stderr)
            return original(*args, **kwargs)

        adapter.inventory = noisy
        with mock.patch.object(sys, "stdout", captured_out), mock.patch.object(sys, "stderr", captured_err):
            result, _ = self.run_helper(self.request(), adapter)
        self.assertEqual(captured_out.getvalue(), "")
        self.assertEqual(captured_err.getvalue(), "")
        self.assertEqual(result["failureCode"], "unavailable")
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_untrusted_metadata_in_window_fails_closed(self):
        for field, value in (("label", "Bearer " + TOKEN), ("key", "https://private.invalid"),
                             ("usedPercent", float("nan")), ("usedPercent", True),
                             ("remainingPercent", 101), ("unit", "sk-private")):
            raw = sample()
            raw["windows"][0][field] = value
            result, _ = self.run_helper(self.request(), FakeAdapter(result={"sample": raw, "failureCode": None}))
            self.assertEqual(result["failureCode"], "invalid_response")
            self.assertIsNone(result["sample"])

    def test_missing_future_and_reversed_observation_times_fail_closed(self):
        for changes in ({"sampledAt": None}, {"sampledAt": "2199-01-01T00:00:00Z"},
                        {"sampledAt": "2026-01-02T00:00:00Z", "fetchedAt": "2026-01-01T00:00:00Z"}):
            result, _ = self.run_helper(self.request(), FakeAdapter(result={"sample": sample(**changes), "failureCode": None}))
            self.assertEqual(result["failureCode"], "invalid_response")

    def test_qwen_reading_with_date_only_subscription_row_survives_sanitization(self):
        import plan_usage
        plan, windows = plan_usage.parse_qwen(
            {"data": {"per1MonthPercentage": .25, "per1MonthResetTime": 1790899200}},
            {"data": {"specCode": "pro", "planName": "Pro", "endTime": 1790985600000}},
            {"data": {"pro": {"monthly": 2000}}},
            {"data": {"remainingCredits": 0, "totalCredits": 500, "activeCount": 2}})
        self.assertIn("subscription", [window["key"] for window in windows])
        row = candidate("qwen")
        adapter = FakeAdapter(before=[row], result={"sample": sample("qwen", plan=plan, windows=windows),
                                                    "failureCode": None})
        result, _ = self.run_helper(self.request(provider="qwen", row=row), adapter)
        self.assertIsNone(result["failureCode"])
        rebuilt = {window["key"]: window for window in result["sample"]["windows"]}
        self.assertEqual(list(rebuilt), [window["key"] for window in windows])
        self.assertEqual(rebuilt["subscription"]["expiresAt"], "2026-10-03T00:00:00.000Z")
        self.assertIsNone(rebuilt["subscription"]["usedPercent"])

    def test_window_with_no_quota_or_date_still_fails_closed(self):
        raw = sample()
        raw["windows"][0] = {"key": "empty", "label": "Empty", "usedPercent": None, "resetAt": None}
        result, _ = self.run_helper(self.request(), FakeAdapter(result={"sample": raw, "failureCode": None}))
        self.assertEqual(result["failureCode"], "invalid_response")

    def test_antigravity_plan_is_rebuilt_without_private_fields(self):
        row = candidate("antigravity")
        plan = {"planType": "pro", "paidTier": {"id": "pro-tier", "name": "Google AI Pro", "private": TOKEN},
                "currentTier": {"id": "standard-tier"}, "private": TOKEN}
        adapter = FakeAdapter(before=[row], result={"sample": sample("antigravity", reportedPlan=plan), "failureCode": None})
        result, _ = self.run_helper(self.request(provider="antigravity", row=row), adapter)
        self.assertEqual(result["sample"]["reportedPlan"], {"planType": "pro", "paidTier": {"id": "pro-tier", "name": "Google AI Pro"},
                                                           "currentTier": {"id": "standard-tier"}})
        self.assertNotIn(TOKEN, json.dumps(result))

    def test_claude_optional_context_is_a_private_bounded_enum_and_digest(self):
        row = candidate("claude")
        raw = sample("claude", optionalExtras={"resetCredits": "ok", "prepaidBalance": "unavailable", "secret": TOKEN},
                     optionalExtrasContext="a" * 64)
        adapter = FakeAdapter(before=[row], result={"sample": raw, "failureCode": None})
        result, _ = self.run_helper(self.request(provider="claude", row=row), adapter)
        self.assertEqual(result["sample"]["optionalExtras"], {"resetCredits": "ok", "prepaidBalance": "unavailable"})
        self.assertEqual(result["sample"]["optionalExtrasContext"], "a" * 64)
        self.assertNotIn(TOKEN, json.dumps(result))
        raw["optionalExtrasContext"] = TOKEN
        result, _ = self.run_helper(self.request(provider="claude", row=row), adapter)
        self.assertNotIn("optionalExtrasContext", result["sample"])
        raw["optionalExtras"]["resetCredits"] = TOKEN
        result, _ = self.run_helper(self.request(provider="claude", row=row), adapter)
        self.assertNotIn("optionalExtras", result["sample"])

    def test_invalid_request_paths_and_types_do_not_touch_adapter(self):
        for changes in ({"homeDir": "relative"}, {"ccsDir": ".."}, {"version": True},
                        {"provider": "not-a-provider"}, {"command": "anything"}):
            request = self.request()
            request.update(changes)
            result, adapter = self.run_helper(request)
            self.assertIsNone(result["sample"])
            self.assertEqual(adapter.inventory_calls, [])

    def test_explicit_ccs_directory_is_passed_without_modifying_home(self):
        root = self.home / "other-ccs"
        result, adapter = self.run_helper(self.request(ccsDir=str(root)))
        self.assertIsNone(result["failureCode"])
        self.assertEqual(adapter.inventory_calls[0][0][1], self.home)
        self.assertEqual(adapter.inventory_calls[0][1], {"ccs_dir": root})


class CursorDispatcherContractTests(unittest.TestCase):
    def test_real_cursor_http_failures_preserve_verified_plan_at_dispatcher_boundary(self):
        for status, code in ((415, "provider_error"), (429, "rate_limited")):
            with self.subTest(status=status):
                case = cursor_scenario(status)
                result = case["failed"]
                self.assertEqual(case["initial"]["sample"]["plan"], "ultra")
                self.assertEqual(result["failureCode"], code)
                self.assertIsNone(result["sample"])
                self.assertEqual(result["observedPlan"], "premium")
                self.assertEqual(result["observedPlanAt"], "2026-10-14T12:01:00.000Z")
                self.assertEqual(result["candidate"], case["candidate"])
                self.assertEqual(case["providerRequests"], 2)
                self.assertEqual(case["externalRequests"], 0)
                if status == 429:
                    self.assertEqual(result["retryAfterSeconds"], 1800)
                else:
                    self.assertNotIn("retryAfterSeconds", result)
                for private in (CURSOR_PRIVATE, cursor_token(), "auth0|cursor-fixture", "auth.json", "accessToken"):
                    self.assertNotIn(private, json.dumps(case))

    def test_real_cursor_rotation_strips_plan_observation_but_preserves_http_cooldown(self):
        for change in ("revision", "identity"):
            with self.subTest(change=change):
                result = cursor_scenario(429, change)["failed"]
                self.assertEqual(result["failureCode"], "source_changed")
                self.assertIsNone(result["sample"])
                self.assertIsNone(result["candidate"])
                self.assertNotIn("observedPlan", result)
                self.assertNotIn("observedPlanAt", result)
                self.assertTrue(result["rateLimited"])
                self.assertEqual(result["retryAfterSeconds"], 1800)

    def test_real_cursor_account_mismatch_strips_plan_observation(self):
        result = cursor_scenario(415, "account_mismatch")["failed"]
        self.assertEqual(result["failureCode"], "account_mismatch")
        self.assertIsNone(result["sample"])
        self.assertNotIn("observedPlan", result)
        self.assertNotIn("observedPlanAt", result)
        self.assertNotIn("retryAfterSeconds", result)


class CommandTests(unittest.TestCase):
    def invoke(self, request):
        result = subprocess.run([sys.executable, "-B", str(HELPERS / "failover_usage.py")], input=request,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, b"")
        return json.loads(result.stdout)

    def test_cli_accepts_empty_synthetic_home_without_network(self):
        with tempfile.TemporaryDirectory() as home:
            result = self.invoke(json.dumps({"version": 1, "action": "inventory", "provider": "codex", "homeDir": home}).encode())
        self.assertEqual(result, {"version": 1, "action": "inventory", "candidates": []})

    def test_cli_rejects_duplicates_malformed_and_oversized_json_without_echo(self):
        values = [b'{"version":1,"version":2}', b'{"private":"' + TOKEN.encode(),
                  b'{"version":NaN}', b"x" * (usage.MAX_REQUEST + 1)]
        for value in values:
            result = self.invoke(value)
            self.assertEqual(result["failureCode"], "invalid_response")
            self.assertNotIn(TOKEN, json.dumps(result))


if __name__ == "__main__":
    unittest.main()
