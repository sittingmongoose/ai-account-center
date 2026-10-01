"""Offline Claude Desktop quota reader identity and output-boundary regressions."""

import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock
import urllib.error

HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import claude_usage as usage

ACCOUNT = "11111111-1111-1111-1111-111111111111"
CLIENT = "22222222-2222-2222-2222-222222222222"
ORG = "33333333-3333-3333-3333-333333333333"
OTHER = "44444444-4444-4444-4444-444444444444"
TOKEN = "fixture-private-access-token"


def key(account=ACCOUNT, client=CLIENT, org=ORG, host="https://api.anthropic.com", scopes="user:inference user:profile"):
    return "acct:{}|{}:{}:{}:{}".format(account, client, org, host, scopes)


def cache(cache_key=None, **overrides):
    return {cache_key or key(): {
        "token": TOKEN, "refreshToken": "fixture-private-refresh-token",
        "expiresAt": (time.time() + 3600) * 1000, **overrides,
    }}


def profile(**overrides):
    return {
        "account": {"uuid": ACCOUNT, "email": usage.PROFILE_EMAILS["gmail"], "has_claude_max": True},
        "organization": {"uuid": ORG}, "private": "upstream-private-sentinel", **overrides,
    }


class FakeClient:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []

    def get(self, route, token):
        self.calls.append((route, token))
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response


class IdentityTests(unittest.TestCase):
    def collect(self, responses, saved_cache=None):
        client = FakeClient(*responses)
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, saved_cache or cache())), \
                mock.patch.object(usage, "Client", return_value=client):
            result = usage.collect("gmail", "windows")
        return result, client

    def test_exact_native_key_and_scope_are_eligible(self):
        entries = usage.eligible_entries(ACCOUNT, cache())
        self.assertEqual(entries[0][1], [ACCOUNT, CLIENT, ORG])
        self.assertEqual(entries[0][2], TOKEN)

    def test_lookalike_origin_scope_and_account_are_rejected(self):
        bad_keys = [
            key(account=OTHER), key(host="https://api.anthropic.com.evil.invalid"),
            key(host="http://api.anthropic.com"), key(scopes="user:profile_extra"),
            key(scopes="user:inference"), key(scopes="user:profile:other"),
            "prefix:" + key(), key() + ":suffix", key().replace("acct:", ""),
        ]
        for value in bad_keys:
            with self.subTest(value=value):
                self.assertEqual(usage.eligible_entries(ACCOUNT, cache(value)), [])

    def test_expiry_and_bad_token_scalars_are_rejected_before_http(self):
        for entry in [
            {"expiresAt": 0}, {"expiresAt": True}, {"expiresAt": float("inf")},
            {"expiresAt": (time.time() + 10) * 1000}, {"token": ""},
            {"token": True}, {"token": "private\nheader"}, {"token": "x" * 32769},
        ]:
            with self.subTest(entry=entry):
                self.assertEqual(usage.eligible_entries(ACCOUNT, cache(**entry)), [])

    def test_live_profile_and_org_verified_before_quota(self):
        result, client = self.collect([profile(), {
            "five_hour": {"utilization": 0, "resets_at": None},
            "seven_day": {"utilization": 100, "resets_at": "2026-10-02T02:59:59.716415Z"},
            "extra_usage": {"is_enabled": False, "monthly_limit": None, "used_credits": None, "utilization": None},
            "secret": "upstream-secret-sentinel",
        }])
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["email"], usage.PROFILE_EMAILS["gmail"])
        self.assertEqual(result["plan"], "max")
        self.assertTrue(result["accountVerified"])
        self.assertTrue(result["organizationVerified"])
        self.assertEqual(result["windows"][0]["usedPercent"], 0)
        self.assertEqual(result["windows"][1]["resetAt"], "2026-10-02T02:59:59.716Z")
        self.assertFalse(result["windows"][2]["enabled"])
        self.assertIsNone(result["windows"][2]["used"])
        self.assertIsNone(result["windows"][2]["resetAt"])
        self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile", "/api/oauth/usage"])
        for forbidden in (TOKEN, "fixture-private-refresh-token", "upstream-private", "upstream-secret", ACCOUNT, CLIENT, ORG):
            self.assertNotIn(forbidden, json.dumps(result))

    def test_wrong_email_account_and_org_cannot_produce_quota(self):
        wrong_profiles = [
            profile(account={"uuid": ACCOUNT, "email": "different@example.com"}),
            profile(account={"uuid": OTHER, "email": usage.PROFILE_EMAILS["gmail"]}),
            profile(organization={"uuid": CLIENT}),
            profile(organization={"uuid": OTHER}),
        ]
        for value in wrong_profiles:
            with self.subTest(value=value):
                result, client = self.collect([value])
                self.assertNotEqual(result["status"], "ok")
                self.assertIsNone(result["email"])
                self.assertEqual(result["windows"], [])
                self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile"])

    def test_expired_or_empty_cache_never_calls_network(self):
        result, client = self.collect([], cache(expiresAt=0))
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(client.calls, [])

    def test_unavailable_platform_does_not_read_any_credentials(self):
        with mock.patch.object(usage, "decrypt_cache") as decrypt:
            for platform in ("mac", "ubuntu"):
                self.assertEqual(usage.collect("gmail", platform)["status"], "unavailable")
            decrypt.assert_not_called()

    def test_native_failure_does_not_emit_exception_or_partial_identity(self):
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", side_effect=RuntimeError("PRIVATE_PATH_TOKEN")):
            result = usage.collect("gmail", "windows")
        self.assertEqual(result["status"], "unavailable")
        self.assertIsNone(result["email"])
        self.assertNotIn("PRIVATE", json.dumps(result))


class WindowTests(unittest.TestCase):
    def test_invalid_percent_and_unknown_window_do_not_become_zero(self):
        self.assertEqual(usage.normalize_windows({
            "five_hour": {"utilization": True}, "seven_day": {"utilization": -1},
            "seven_day_opus": {"utilization": float("inf")}, "unknown": {"utilization": 4},
        }), [])

    def test_over_limit_usage_preserves_mixed_quota_resets_and_extra_budget(self):
        windows = usage.normalize_windows({
            "five_hour": {"utilization": 125.5, "resets_at": "2026-10-02T01:00:00Z"},
            "seven_day": {"utilization": 8, "resets_at": "2026-10-06T20:00:00Z"},
            "extra_usage": {"is_enabled": True, "utilization": 112.25, "used_credits": 2245,
                "monthly_limit": 2000, "currency": "USD", "expires_at": "2026-10-22T16:00:00Z"},
        })
        self.assertEqual(len(windows), 3)
        self.assertEqual((windows[0]["usedPercent"], windows[0]["remainingPercent"]), (125.5, 0))
        self.assertEqual(windows[0]["resetAt"], "2026-10-02T01:00:00.000Z")
        self.assertEqual((windows[1]["usedPercent"], windows[1]["remainingPercent"]), (8, 92))
        self.assertEqual(windows[1]["resetAt"], "2026-10-06T20:00:00.000Z")
        self.assertEqual((windows[2]["usedPercent"], windows[2]["remainingPercent"]), (112.25, 0))
        self.assertEqual((windows[2]["used"], windows[2]["limit"], windows[2]["remaining"]), (22.45, 20, 0))
        self.assertEqual(windows[2]["expiresAt"], "2026-10-22T16:00:00.000Z")

    def test_actual_extra_currency_is_minor_units_and_dates_are_not_inferred(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "used_credits": 500, "monthly_limit": 2000,
            "utilization": 25, "currency": "USD", "is_unlimited": False,
        }})[0]
        self.assertEqual((window["used"], window["limit"], window["remaining"]), (5, 20, 15))
        self.assertEqual(window["unit"], "USD")
        self.assertFalse(window["unlimited"])
        self.assertIsNone(window["resetAt"])
        self.assertIsNone(window["expiresAt"])

    def test_absent_currency_preserves_raw_credit_values(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "used_credits": 500, "monthly_limit": 2000,
        }})[0]
        self.assertEqual((window["used"], window["limit"]), (500, 2000))
        self.assertEqual(window["unit"], "credits")
        self.assertNotIn("unlimited", window)
        self.assertIsNone(window["usedPercent"])

    def test_extra_usage_preserves_reported_expiry_reset_and_unlimited_only(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "is_unlimited": True,
            "resets_at": "2026-10-01T00:00:00-04:00", "expires_at": "2026-12-01T00:00:00Z",
        }})[0]
        self.assertEqual(window["resetAt"], "2026-10-01T04:00:00.000Z")
        self.assertEqual(window["expiresAt"], "2026-12-01T00:00:00.000Z")
        self.assertTrue(window["unlimited"])

    def test_timezone_missing_invalid_calendar_and_arbitrary_text_rejected(self):
        for value in ("2026-10-01T00:00:00", "2026-02-31T00:00:00Z", "token-secret", True, None):
            self.assertIsNone(usage.reset_at(value))

    def test_windows_profile_paths_are_fixed_and_do_not_use_caller_path(self):
        home = Path("/fixture")
        self.assertEqual(usage.windows_profile_path("gmail", home), home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")
        self.assertEqual(usage.windows_profile_path("party", home), home / "AppData/Roaming/Claude-party")


class WebExtraTests(unittest.TestCase):
    def capsule(self, **overrides):
        return {"schemaVersion": 1, "profileId": "gmail", "email": usage.PROFILE_EMAILS["gmail"],
                "accountUuid": ACCOUNT, "capturedAt": "2020-01-01T00:00:00Z", "cookies": [
                    {"name": "sessionKey", "host_key": ".claude.ai", "path": "/", "top_frame_site_key": "",
                     "value": "fixture-private-session", "is_secure": 1, "is_httponly": 1,
                     "has_expires": 1, "expires_utc": (time.time() + 3600) * 1000000 + usage.CHROMIUM_EPOCH},
                    {"name": "lastActiveOrg", "host_key": ".claude.ai", "path": "/", "value": ORG}], **overrides}

    def test_still_live_capsule_ignores_old_migration_capture_time(self):
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(self.capsule()).encode()) as decrypt:
                cookies = usage.capsule_cookies("gmail", ACCOUNT, ORG, home)
            self.assertEqual([cookie["name"] for cookie in cookies], ["sessionKey", "lastActiveOrg"])
            decrypt.assert_called_once_with(b"fixture-protected", expected_length=None)

    def test_capsule_scope_account_and_expired_cookie_fail_before_http(self):
        cases = [self.capsule(profileId="party"), self.capsule(email=usage.PROFILE_EMAILS["party"]),
                 self.capsule(accountUuid=OTHER)]
        for field, value in (("host_key", ".evil.invalid"), ("path", "/other"), ("is_httponly", 0),
                             ("expires_utc", 1), ("value", "secret;foreign=header")):
            case = self.capsule(); case["cookies"][0][field] = value; cases.append(case)
        case = self.capsule(); case["cookies"][1]["value"] = OTHER; cases.append(case)
        case = self.capsule(); case["cookies"].append(dict(case["cookies"][0])); cases.append(case)
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            for case in cases:
                with self.subTest(case=case), mock.patch.object(usage, "unprotect", return_value=json.dumps(case).encode()), self.assertRaises(usage.UsageUnavailable):
                    usage.capsule_cookies("gmail", ACCOUNT, ORG, home)

    def test_duplicate_native_org_selectors_coalesce_only_when_identical_and_bound(self):
        value = self.capsule()
        value["cookies"].append({**value["cookies"][1], "samesite": -1})
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(value).encode()):
                self.assertEqual(len(usage.capsule_cookies("gmail", ACCOUNT, ORG, home)), 2)
            value["cookies"][-1]["value"] = OTHER
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(value).encode()), self.assertRaises(usage.UsageUnavailable):
                usage.capsule_cookies("gmail", ACCOUNT, ORG, home)

    def test_historical_used_reset_expiry_is_separate_from_available_zero(self):
        windows = usage.normalize_reset_credits({"eligible": True, "grants": [{
            "resets_left": 0, "resets_total": 1, "paused": False,
            "starts_at": "2026-09-22T16:00:00Z", "ends_at": "2026-10-22T16:00:00Z",
            "id": "private-grant-sentinel"}]}, now=1780000000)
        self.assertEqual(windows[0]["remaining"], 0)
        self.assertIsNone(windows[0]["expiresAt"])
        self.assertEqual(windows[1]["label"], "Used rate-limit reset grant 1")
        self.assertEqual(windows[1]["expiresAt"], "2026-10-22T16:00:00.000Z")
        self.assertEqual((windows[1]["used"], windows[1]["remaining"]), (1, 0))
        self.assertNotIn("enabled", windows[0])
        self.assertNotIn("enabled", windows[1])
        self.assertNotIn("private-grant", json.dumps(windows))

    def test_saved_resets_count_inventory_despite_redemption_gate(self):
        now = 1780000000
        grant = {"resets_left": 2, "resets_total": 3, "paused": False,
                 "starts_at": None, "ends_at": None, "usable_now": False}
        windows = usage.normalize_reset_credits({"eligible": True, "grants": [grant]}, now)
        self.assertEqual(windows[0]["remaining"], 2)
        self.assertEqual(windows[1]["key"], "reset_credit_available_grant_1")
        for change in ({"paused": True}, {"starts_at": "2027-01-01T00:00:00Z"}, {"ends_at": "2020-01-01T00:00:00Z"}):
            values = usage.normalize_reset_credits({"eligible": True, "grants": [{**grant, **change}]}, now)
            self.assertEqual(values[0]["remaining"], 0)
            self.assertNotIn("enabled", values[1])

    def test_absent_malformed_reset_inventory_is_unknown_not_zero(self):
        cases = [None, {}, {"eligible": True}, {"eligible": 1, "grants": []},
                 {"eligible": True, "grants": [{"resets_left": 1, "paused": False, "ends_at": "private-date"}]},
                 {"eligible": True, "grants": [{"resets_left": True, "paused": False}]},
                 {"eligible": True, "grants": [{"resets_left": 2, "resets_total": 1, "paused": False}]},
                 {"eligible": True, "grants": [{"resets_left": 1}]},
                 {"eligible": True, "grants": [{"resets_left": 50, "paused": False}] * 2}]
        for value in cases:
            self.assertEqual(usage.normalize_reset_credits(value), [])
        zero = usage.normalize_reset_credits({"eligible": False, "grants": []})
        self.assertEqual(zero[0]["remaining"], 0)
        self.assertNotIn("enabled", zero[0])

    def test_prepaid_currency_null_and_actual_expiry_are_preserved(self):
        window = usage.normalize_prepaid({"amount": 1234, "currency": "USD",
            "next_expires_at": "2026-10-22T16:00:00Z", "token": "private-prepaid"})[0]
        self.assertEqual((window["remaining"], window["unit"]), (12.34, "USD"))
        self.assertEqual(window["expiresAt"], "2026-10-22T16:00:00.000Z")
        self.assertIsNone(window["resetAt"])
        self.assertNotIn("private-prepaid", json.dumps(window))
        raw = usage.normalize_prepaid({"amount": 0, "currency": None, "tranches": None})[0]
        self.assertEqual((raw["remaining"], raw["unit"]), (0, "credits"))
        self.assertIsNone(raw["expiresAt"])
        for value in ({}, {"amount": True}, {"amount": -1}, {"amount": 0, "currency": "unknown"}):
            self.assertEqual(usage.normalize_prepaid(value), [])

    def web_windows(self, *responses):
        client = mock.Mock()
        client.get.side_effect = responses
        with mock.patch.object(usage, "capsule_cookies", return_value=self.capsule()["cookies"]), mock.patch.object(usage, "WebClient", return_value=client):
            windows = usage.optional_web_windows("gmail", ACCOUNT, ORG)
        return windows, client

    def web_account(self, **overrides):
        return {"email_address": usage.PROFILE_EMAILS["gmail"], "uuid": ACCOUNT,
                "memberships": [{"organization": {"uuid": ORG}}], **overrides}

    def test_optional_extras_verify_email_uuid_and_org_before_fetch(self):
        for account in (self.web_account(email_address=usage.PROFILE_EMAILS["me"]),
                        self.web_account(uuid=OTHER), self.web_account(memberships=[{"organization": {"uuid": OTHER}}])):
            windows, client = self.web_windows(account)
            self.assertEqual(windows, [])
            self.assertEqual([call.args[0] for call in client.get.call_args_list], ["/api/account"])
        windows, client = self.web_windows(self.web_account(), {"cedar_ember": {"eligible": True, "grants": []}},
                                           {"amount": 0, "currency": "USD"})
        self.assertEqual([window["remaining"] for window in windows], [0, 0])
        for forbidden in (ACCOUNT, ORG, "fixture-private-session"):
            self.assertNotIn(forbidden, json.dumps(windows))

    def test_optional_failures_do_not_invent_zero_or_drop_other_balance(self):
        windows, _ = self.web_windows(self.web_account(), RuntimeError("private-error"), {"amount": 0, "currency": "USD"})
        self.assertEqual([window["key"] for window in windows], ["prepaid_balance"])
        windows, _ = self.web_windows(self.web_account(), {"cedar_ember": {"eligible": False, "grants": []}}, RuntimeError("private-error"))
        self.assertEqual([window["key"] for window in windows], ["reset_credits_available"])
        with mock.patch.object(usage, "capsule_cookies", side_effect=RuntimeError("private-path")):
            self.assertEqual(usage.optional_web_windows("gmail", ACCOUNT, ORG), [])

    def test_native_quota_survives_missing_optional_session(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
        with mock.patch.object(usage.sys, "platform", "win32"), mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), mock.patch.object(usage, "Client", return_value=client), mock.patch.object(usage, "capsule_cookies", side_effect=ValueError("PRIVATE")):
            result = usage.collect("gmail", "windows")
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["windows"][0]["usedPercent"], 7)
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_web_client_hosts_routes_redirect_and_response_boundaries(self):
        requests = mock.Mock()
        with mock.patch.dict(sys.modules, {"curl_cffi": mock.Mock(requests=requests)}):
            client = usage.WebClient([{ "name": "sessionKey", "value": "fixture-private-session"}], ORG)
        for route in ("https://evil.invalid", "/api/organizations/" + OTHER + "/prepaid/credits", "/api/organizations/" + ORG + "/usage"):
            with self.assertRaises(usage.UsageUnavailable):
                client.get(route)
        requests.get.assert_not_called()
        response = mock.Mock(status_code=200)
        response.iter_content.return_value = [b"{}"]
        requests.get.return_value = response
        self.assertEqual(client.get("/api/account"), {})
        self.assertTrue(requests.get.call_args.kwargs["verify"])
        self.assertFalse(requests.get.call_args.kwargs["allow_redirects"])
        response.close.assert_called_once()
        response.iter_content.return_value = [b" " * (usage.MAX_BYTES + 1)]
        with self.assertRaises(usage.UsageUnavailable):
            client.get("/api/account")
        client.deadline = 0
        with self.assertRaises(usage.UsageUnavailable):
            client.get("/api/account")


class HttpBoundaryTests(unittest.TestCase):
    def test_redirect_handler_refuses_cross_origin(self):
        self.assertIsNone(usage.NoRedirect().redirect_request(None, None, 302, "", {}, "https://evil.invalid"))

    def test_http_only_admits_fixed_profile_and_usage_routes(self):
        client = usage.Client()
        with mock.patch.object(client.opener, "open") as request:
            with self.assertRaises(usage.UsageUnavailable):
                client.get("https://evil.invalid", TOKEN)
            request.assert_not_called()

    def test_json_read_is_size_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_bytes(b" " * (usage.MAX_BYTES + 1))
            with self.assertRaises(usage.UsageUnavailable):
                usage.read_json(path)

    def test_oversized_http_response_rejected(self):
        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def read1(self, count):
                return b"x" * count

        client = usage.Client()
        with mock.patch.object(client.opener, "open", return_value=Response()):
            with self.assertRaises(usage.UsageUnavailable):
                client.get("/api/oauth/profile", TOKEN)

    def test_expired_overall_deadline_prevents_new_request(self):
        client = usage.Client()
        client.deadline = 0
        with mock.patch.object(client.opener, "open") as request:
            with self.assertRaises(usage.UsageUnavailable):
                client.get("/api/oauth/usage", TOKEN)
            request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
