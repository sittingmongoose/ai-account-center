"""Offline scalar-contract, credential isolation and request fixture tests."""

import json
from contextlib import closing
import base64
import os
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import urllib.parse


HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import plan_common as common
import plan_usage as usage


class FakeClient:
    def __init__(self, *payloads):
        self.payloads = list(payloads)
        self.calls = []

    def get(self, url, headers=None, body=None, text=False):
        self.calls.append((url, headers or {}, body, text))
        item = self.payloads.pop(0)
        if isinstance(item, Exception):
            raise item
        return item


def go_payload():
    return {"usage": {key: {"percent": value, "status": "ok", "resetsAt": "2026-10-02T12:00:00+00:00"}
                       for key, value in (("rolling", 0), ("weekly", 2.5), ("monthly", 27))}}


def safari_fixture(cookies):
    records = []
    for cookie in cookies:
        record = bytearray(56)
        offsets = []
        for value in (cookie.get("domain", ".qwencloud.com"), cookie.get("name", "session"),
                      cookie.get("path", "/"), cookie.get("value", "fixture-private")):
            offsets.append(len(record))
            record.extend(value.encode() + b"\x00")
        struct.pack_into("<I", record, 0, len(record))
        struct.pack_into("<IIII", record, 16, *offsets)
        struct.pack_into("<d", record, 40, cookie.get("expiry", 1790985600 - 978307200))
        records.append(bytes(record))
    position = 12 + 4 * len(records)
    offsets = []
    for record in records:
        offsets.append(position)
        position += len(record)
    page = b"\x00\x00\x01\x00" + struct.pack("<I", len(records))
    page += b"".join(struct.pack("<I", offset) for offset in offsets) + b"\x00" * 4 + b"".join(records)
    return b"cook" + struct.pack(">II", 1, len(page)) + page


class PlanUsageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = Path(self.temp.name)
        self.env = mock.patch.dict(os.environ, {}, clear=True)
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.temp.cleanup()

    def write_auth(self, payload):
        path = self.home / ".local" / "share" / "opencode" / "auth.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(payload))
        return path

    def write_omp(self, rows):
        path = self.home / ".omp" / "agent" / "agent.db"
        path.parent.mkdir(parents=True, exist_ok=True)
        with closing(sqlite3.connect(path)) as db:
            db.execute("CREATE TABLE auth_credentials(id INTEGER,provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)")
            db.executemany("INSERT INTO auth_credentials VALUES (?,?,?,?,?)", rows)
            db.commit()
        return path

    def test_unavailable_is_exact_dto_and_unknown_is_not_zero(self):
        result = usage.collect("qwen", "ubuntu", self.home, FakeClient())
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["windows"], [])
        self.assertIsNone(result["fetchedAt"])
        self.assertEqual(set(result), {"id", "provider", "providerLabel", "label", "email", "plan", "platform",
                                      "source", "status", "message", "fetchedAt", "sampledAt", "isActive", "windows", "capabilities"})
        self.assertFalse(result["isActive"])
        self.assertEqual(result["capabilities"], {"codexProfile": None, "claudeProfileId": None, "claudePlatforms": []})

    def test_readonly_omp_selects_enabled_only_and_never_outputs_credentials(self):
        path = self.write_omp([(1, "zai", "api_key", json.dumps({"key": "private-credential", "source": "api_key"}), None),
                               (2, "zai", "oauth", json.dumps({"access": "disabled-secret"}), "invalid_grant")])
        before = path.read_bytes()
        payload = {"success": True, "code": 200, "data": {"level": "pro", "limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 0}]}}
        client = FakeClient(payload)
        result = usage.collect("zai", "windows", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["source"], "OMP on Windows")
        self.assertEqual(client.calls[0][1]["Authorization"], "private-credential")
        self.assertNotIn("private-credential", json.dumps(result))
        self.assertNotIn("disabled-secret", json.dumps(result))
        self.assertEqual(before, path.read_bytes())

    def test_kimi_api_key_preferred_over_expired_omp_oauth(self):
        self.write_auth({"kimi-for-coding": {"type": "api", "key": "private-key"}})
        self.write_omp([(1, "kimi-code", "oauth", json.dumps({"access": "expired-token", "expires": 1}), None)])
        client = FakeClient({"usage": {"limit": "100", "remaining": "100", "resetTime": "2026-10-03T00:00:00Z"},
                             "limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                                         "detail": {"limit": "100", "remaining": "75"}}], "usages": {}})
        result = usage.collect("kimi-code", "mac", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(client.calls[0][1]["Authorization"], "Bearer private-key")
        self.assertEqual([item["key"] for item in result["windows"]], ["5h", "weekly"])
        self.assertEqual([item["usedPercent"] for item in result["windows"]], [25, 0])
        self.assertIsNone(result["windows"][0]["unit"])
        self.assertIsNone(result["windows"][1]["unit"])
        self.assertIsNone(result["windows"][0]["resetAt"])
        self.assertEqual(result["windows"][1]["resetAt"], "2026-10-03T00:00:00Z")

    def test_expired_kimi_does_not_make_network_request_or_refresh(self):
        self.write_omp([(1, "kimi-code", "oauth", json.dumps({"access": "expired-token", "expires": 1}), None)])
        client = FakeClient()
        result = usage.collect("kimi-code", "ubuntu", self.home, client)
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(client.calls, [])
        self.assertEqual(result["windows"], [])

    def test_kimi_modern_ratios_win_and_invalid_ratios_are_rejected(self):
        plan, windows = usage.parse_kimi({"usage": {"limit": 100, "used": 80},
                                        "limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"}, "detail": {"limit": 100, "used": 90}}],
                                        "usages": {"limit_5h": {"used_ratio": 0.2, "reset_time": "2026-10-02T00:00:00Z"},
                                                   "limit_7d": {"used_ratio": 0.5}, "limit_month_total": {"used_ratio": 2},
                                                   "limit_month_code": {"used_ratio": float("nan")}}})
        self.assertIsNone(plan)
        self.assertEqual([(item["key"], item["usedPercent"]) for item in windows], [("5h", 20), ("weekly", 50)])
        with self.assertRaises(common.CollectionError):
            usage.parse_kimi({"usages": {"limit_5h": {"used_ratio": -1}}})

    def test_go_requires_all_three_windows_and_does_not_infer_monthly_duration(self):
        plan, windows = usage.parse_go(go_payload())
        self.assertEqual(plan, "Go")
        self.assertEqual([item["usedPercent"] for item in windows], [0, 2.5, 27])
        self.assertIsNone(windows[-1]["windowMinutes"])
        incomplete = go_payload()
        del incomplete["usage"]["weekly"]
        with self.assertRaises(common.CollectionError):
            usage.parse_go(incomplete)

    def test_go_fixed_endpoint_bearer_and_stable_installation(self):
        self.write_auth({"opencode-go": {"type": "api", "key": "private-key"}})
        client = FakeClient(go_payload())
        result = usage.collect("opencode-go", "mac", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(client.calls[0][0], "https://opencode.ai/zen/go/v1/usage")
        self.assertEqual(client.calls[0][1]["Authorization"], "Bearer private-key")
        self.assertTrue(client.calls[0][1]["x-opencode-session"].startswith("ccs-usage-"))
        self.assertNotIn("private-key", json.dumps(result))

    def test_zai_exact_usage_ratio_precedes_rounded_percentage(self):
        plan, windows = usage.parse_zai({"success": True, "data": {"level": "pro", "limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "usage": 200, "currentValue": 51, "percentage": 26},
            {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 1, "nextResetTime": 1790899200000},
            {"type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "currentValue": 0, "percentage": 0},
            {"type": "unknown-private-meter", "unit": 5, "number": 1, "percentage": 10}]}})
        self.assertEqual(plan, "pro")
        self.assertEqual([item["usedPercent"] for item in windows], [25.5, 1, 0])
        self.assertEqual(windows[0]["windowMinutes"], 300)
        self.assertEqual(windows[1]["windowMinutes"], 10080)
        self.assertEqual(windows[1]["resetAt"], "2026-10-02T00:00:00Z")
        self.assertIsNone(windows[2]["windowMinutes"])
        self.assertEqual(windows[2]["unit"], "requests")

    def test_zai_invalid_values_are_not_zero(self):
        with self.assertRaises(common.CollectionError):
            usage.parse_zai({"data": {"limits": [{"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": "NaN"}]}})
        with self.assertRaises(common.CollectionError):
            usage.parse_zai({"success": False, "message": "private-echo", "data": {"limits": []}})

    def test_qwen_plain_token_is_unavailable_with_no_request(self):
        self.write_auth({"alibaba-token-plan": {"type": "api", "key": "sk-private"}})
        client = FakeClient()
        result = usage.collect("qwen", "mac", self.home, client)
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(client.calls, [])
        self.assertNotIn("sk-private", json.dumps(result))

    def test_qwen_selects_cookie_over_plain_key_and_uses_fixed_origin(self):
        self.write_auth({"alibaba-token-plan": {"type": "api", "key": "sk-private"}})
        packed = json.dumps({"token": "sk-private", "cookie": "session=private-cookie; csrf=private-csrf"})
        self.write_omp([(1, "alibaba-token-plan", "api_key", json.dumps({"key": packed}), None)])
        client = FakeClient({"data": {"secToken": "private-sec-token"}},
                            {"data": {"Data": json.dumps({"DataV2": {"data": {"per1MonthPercentage": .31,
                                                                                  "per1MonthResetTime": 1790899200}}})}})
        result = usage.collect("qwen", "ubuntu", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["source"], "OMP on Ubuntu")
        self.assertEqual(len(result["windows"]), 1)
        self.assertEqual(result["windows"][0]["usedPercent"], 31)
        self.assertEqual(client.calls[0][0], "https://home.qwencloud.com/tool/user/info.json")
        self.assertTrue(client.calls[1][0].startswith("https://cs-data.qwencloud.com/data/api.json?"))
        form = urllib.parse.parse_qs(client.calls[1][2].decode())
        self.assertEqual(form["region"], ["ap-southeast-1"])
        self.assertEqual(form["sec_token"], ["private-sec-token"])
        self.assertEqual(client.calls[1][1]["x-xsrf-token"], "private-csrf")
        self.assertIsNone(result["windows"][0]["windowMinutes"])
        serialized = json.dumps(result)
        for private in ("sk-private", "private-cookie", "private-csrf", "private-sec-token"):
            self.assertNotIn(private, serialized)

    def test_qwen_unknown_region_cannot_receive_credential(self):
        packed = json.dumps({"token": "sk-private", "cookie": "session=private", "baseUrl": "https://attacker.example"})
        client = FakeClient()
        with self.assertRaises(common.CollectionError):
            usage.fetch_qwen(client, packed)
        self.assertEqual(client.calls, [])

    def test_qwen_fraction_and_percentage_and_invalid_payload(self):
        _, windows = usage.parse_qwen({"data": {"per5HourPercentage": .125, "per1WeekPercentage": 50,
                                               "per1MonthPercentage": None}})
        self.assertEqual([window["usedPercent"] for window in windows], [12.5, 50])
        with self.assertRaises(common.CollectionError):
            usage.parse_qwen({"data": {"success": False, "message": "private"}})
        with self.assertRaises(common.CollectionError):
            usage.parse_qwen({"data": {"per5HourPercentage": float("inf")}})

    def test_safe_errors_do_not_echo_upstream_exception(self):
        self.write_auth({"opencode-go": {"type": "api", "key": "private-key"}})
        client = FakeClient(RuntimeError("Authorization Bearer private-key\nprivate-body"))
        result = usage.collect("opencode-go", "mac", self.home, client)
        self.assertEqual(result["status"], "error")
        self.assertEqual(result["windows"], [])
        self.assertNotIn("private", json.dumps(result))

    def test_numbers_and_timestamps_reject_nonfinite_naive_or_invalid_values(self):
        for value in (True, -1, "NaN", float("inf"), "bad", None):
            self.assertIsNone(common.number(value))
        self.assertEqual(common.reset_at(1790899200), "2026-10-02T00:00:00Z")
        self.assertEqual(common.reset_at(1790899200000), "2026-10-02T00:00:00Z")
        self.assertEqual(common.reset_at("2026-10-02T03:00:00+03:00"), "2026-10-02T00:00:00Z")
        for value in (True, 0, "2026-10-02T00:00:00", "private", float("inf")):
            self.assertIsNone(common.reset_at(value))

    def test_email_and_plan_are_scalar_allowlisted(self):
        self.assertEqual(common.email_address("user@example.com"), "user@example.com")
        self.assertIsNone(common.email_address("token secret"))
        self.assertIsNone(common.email_address("user@example.com\nprivate"))
        self.assertEqual(common.plan_label("pro"), "pro")
        self.assertIsNone(common.plan_label({"private": "body"}))
        self.assertIsNone(common.plan_label("private-key"))
        self.assertIsNone(common.plan_label("sk-" + "secret" * 10))

    def test_optional_metadata_keeps_expiration_separate_from_reset(self):
        window = common.usage_window("test", "Test", reset=1790899200, remaining=0,
                                     expires_at=1790985600000, kind="balance", enabled=False, unlimited=True)
        self.assertEqual(window["resetAt"], "2026-10-02T00:00:00Z")
        self.assertEqual(window["expiresAt"], "2026-10-03T00:00:00Z")
        self.assertEqual(window["remaining"], 0)
        self.assertFalse(window["enabled"])
        self.assertTrue(window["unlimited"])
        invalid = common.usage_window("invalid", "Invalid", remaining=float("nan"), expires_at="private",
                                      kind="private", enabled="yes", unlimited=1)
        self.assertIsNone(invalid["remaining"])
        self.assertIsNone(invalid["expiresAt"])
        self.assertNotIn("kind", invalid)
        self.assertNotIn("enabled", invalid)
        self.assertNotIn("unlimited", invalid)

    def test_kimi_booster_balance_and_monthly_spend_keep_actual_money_scale(self):
        plan, windows = usage.parse_kimi({"user": {"membership": {"level": "LEVEL_BASIC"}},
                                         "boosterWallet": {
                                             "balance": {"type": "BOOSTER", "amount": "200000000", "amountLeft": "0"},
                                             "monthlyChargeLimit": {"priceInCents": 1500, "currency": "USD"},
                                             "monthlyUsed": {"priceInCents": 123, "currency": "USD"},
                                             "monthlyChargeLimitEnabled": True}})
        self.assertEqual(plan, "Moderato")
        balance, spend = windows
        self.assertEqual((balance["limit"], balance["remaining"], balance["used"], balance["unit"]), (2, 0, 2, "USD"))
        self.assertEqual(balance["usedPercent"], 100)
        self.assertEqual(balance["kind"], "balance")
        self.assertEqual((spend["limit"], spend["used"], spend["remaining"]), (15, 1.23, 13.77))
        self.assertTrue(spend["enabled"])
        for window in windows:
            self.assertIsNone(window["resetAt"])
            self.assertNotIn("expiresAt", window)

    def test_kimi_unknown_currency_and_missing_optional_fields_are_not_invented(self):
        _, windows = usage.parse_kimi({"boosterWallet": {"balance": {"type": "BOOSTER", "amount": "1000000", "amountLeft": "1"}}})
        self.assertEqual(windows[0]["limit"], 1)
        self.assertEqual(windows[0]["remaining"], 1)  # Official tiny-positive rounding.
        self.assertEqual(windows[0]["unit"], "cents")
        self.assertNotIn("enabled", windows[0])
        self.assertEqual(len(windows), 1)
        with self.assertRaises(common.CollectionError):
            usage.parse_kimi({"boosterWallet": {"balance": {"type": "BOOSTER"}}})

    def test_kimi_modern_zero_placeholder_only_uses_matching_exact_legacy_window(self):
        payload = {"limits": [{"window": {"duration": 300, "timeUnit": "TIME_UNIT_MINUTE"},
                                "detail": {"limit": 100, "used": 70, "resetTime": "2026-10-02T00:00:00Z"}}],
                   "usages": {"limit_5h": {"used_ratio": 0, "reset_time": "2026-10-02T00:00:01Z"}}}
        self.assertEqual(usage.parse_kimi(payload)[1][0]["usedPercent"], 70)
        payload["usages"]["limit_month_total"] = {"used_ratio": 0}
        self.assertEqual(usage.parse_kimi(payload)[1][0]["usedPercent"], 0)
        del payload["usages"]["limit_month_total"]
        payload["usages"]["limit_5h"]["reset_time"] = "2026-10-02T00:00:03Z"
        self.assertEqual(usage.parse_kimi(payload)[1][0]["usedPercent"], 0)

    def test_reset_only_windows_are_retained_without_fake_percent(self):
        _, kimi = usage.parse_kimi({"usages": {"limit_5h": {"reset_time": "2026-10-02T00:00:00Z"}}})
        _, qwen = usage.parse_qwen({"data": {"per1WeekResetTime": 1790899200}})
        _, zai = usage.parse_zai({"data": {"limits": [{"type": "TOKENS_LIMIT", "unit": 3, "number": 5,
                                                         "nextResetTime": 1790899200000}]}})
        for window in (kimi[0], qwen[0], zai[0]):
            self.assertIsNone(window["usedPercent"])
            self.assertIsNone(window["remainingPercent"])
            self.assertEqual(window["resetAt"], "2026-10-02T00:00:00Z")

    def test_zai_feature_counters_preserve_remaining_without_inheriting_reset(self):
        _, windows = usage.parse_zai({"data": {"limits": [{"type": "TIME_LIMIT", "unit": 5, "number": 1,
                                                            "usage": 1000, "currentValue": 2, "remaining": 998,
                                                            "nextResetTime": 1790899200, "usageDetails": [
                                                                {"modelCode": "search-prime", "usage": 2},
                                                                {"modelCode": "web-reader", "usage": 0},
                                                                {"modelCode": "zread", "usage": 0}]}]}})
        self.assertEqual(windows[0]["remaining"], 998)
        self.assertEqual(windows[0]["resetAt"], "2026-10-02T00:00:00Z")
        self.assertIsNone(windows[0]["windowMinutes"])
        self.assertEqual([window["used"] for window in windows[1:]], [2, 0, 0])
        for window in windows[1:]:
            self.assertEqual(window["kind"], "extra_usage")
            self.assertIsNone(window["resetAt"])
            self.assertIsNone(window["limit"])
            self.assertIsNone(window["usedPercent"])

    def test_qwen_real_quota_ceilings_addon_credits_and_subscription_expiry(self):
        usage_payload = {"data": {"per1MonthPercentage": .25, "per1MonthResetTime": 1790899200}}
        subscription = {"data": {"specCode": "pro", "planName": "Pro", "endTime": 1790985600000}}
        config = {"data": {"pro": {"monthly": 2000, "five_hour": 100, "weekly": 500}}}
        addon = {"data": {"remainingCredits": 0, "totalCredits": 500, "activeCount": 2}}
        plan, windows = usage.parse_qwen(usage_payload, subscription, config, addon)
        self.assertEqual(plan, "pro")
        self.assertEqual([window["key"] for window in windows], ["monthly", "subscription", "addon-credits", "addon-packs"])
        monthly, entitlement, balance, packs = windows
        self.assertEqual((monthly["used"], monthly["limit"], monthly["remaining"]), (500, 2000, 1500))
        self.assertEqual(monthly["unit"], "credits")
        self.assertEqual(monthly["resetAt"], "2026-10-02T00:00:00Z")
        self.assertEqual(entitlement["expiresAt"], "2026-10-03T00:00:00Z")
        self.assertIsNone(entitlement["resetAt"])
        self.assertEqual((balance["used"], balance["remaining"], balance["usedPercent"]), (500, 0, 100))
        self.assertIsNone(balance["resetAt"])
        self.assertNotIn("expiresAt", balance)
        self.assertEqual(packs["remaining"], 2)

    def test_qwen_optional_endpoint_failures_do_not_erase_live_quota(self):
        packed = json.dumps({"token": "private", "cookie": "session=private"})
        client = FakeClient({"data": {"secToken": "private"}}, {"data": {"per1MonthPercentage": .1}},
                            common.CollectionError("error", "Unavailable"),
                            {"data": {"specCode": "pro", "endTime": 1790985600000}},
                            {"data": {"pro": {"monthly": 500}}})
        plan, windows = usage.parse_qwen(*usage.fetch_qwen(client, packed))
        self.assertEqual(windows[0]["used"], 50)
        self.assertEqual(windows[1]["expiresAt"], "2026-10-03T00:00:00Z")
        self.assertEqual(len(windows), 2)
        calls = [urllib.parse.parse_qs(call[2].decode()) for call in client.calls[1:]]
        self.assertTrue(json.loads(calls[1]["params"][0])["Api"].endswith("/addon/list"))
        subscription_params = json.loads(calls[2]["params"][0])
        self.assertEqual(subscription_params["Data"]["commodityCode"], "sfm_tokenplansolo_public_intl")

    def test_qwen_absent_extra_credit_fields_never_become_zero(self):
        _, windows = usage.parse_qwen({"data": {"per1MonthPercentage": .1}}, addon={"data": {"activeCount": 0}})
        self.assertEqual([window["key"] for window in windows], ["monthly", "addon-packs"])
        self.assertIsNone(windows[0]["limit"])
        self.assertIsNone(windows[0]["unit"])
        self.assertEqual(windows[1]["remaining"], 0)

    def test_windows_qwen_dpapi_capsule_is_readonly_and_never_emitted(self):
        capsule = self.home / ".ccs" / "account-usage" / "qwen-console-session.json"
        capsule.parent.mkdir(parents=True)
        capsule.write_text(json.dumps({"cookieDPAPI": base64.b64encode(b"encrypted-only").decode(), "region": "intl"}))
        before = capsule.read_bytes()
        self.write_auth({"alibaba-token-plan": {"type": "api", "key": "sk-private"}})
        client = FakeClient({"data": {"secToken": "private-sec-token"}}, {"data": {"per1MonthPercentage": .1}})
        with mock.patch.object(common, "_windows_unprotect", return_value="session=private-cookie") as decrypt:
            result = usage.collect("qwen", "windows", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["source"], "Brave console on Windows")
        self.assertEqual(client.calls[0][1]["Cookie"], "session=private-cookie")
        decrypt.assert_called_once_with(b"encrypted-only")
        self.assertEqual(before, capsule.read_bytes())
        serialized = json.dumps(result)
        for secret in ("sk-private", "private-cookie", "private-sec-token", "encrypted-only", "cookieDPAPI"):
            self.assertNotIn(secret, serialized)

    def test_windows_qwen_console_session_does_not_require_an_api_key(self):
        capsule = self.home / ".ccs" / "account-usage" / "qwen-console-session.json"
        capsule.parent.mkdir(parents=True)
        capsule.write_text(json.dumps({"cookieDPAPI": base64.b64encode(b"encrypted").decode(), "region": "cn"}))
        client = FakeClient('SEC_TOKEN: "private-sec-token"', {"data": {"per5HourPercentage": .5}})
        with mock.patch.object(common, "_windows_unprotect", return_value="session=private-cookie"):
            result = usage.collect("qwen", "windows", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertTrue(client.calls[0][0].startswith("https://bailian.console.aliyun.com/"))
        self.assertTrue(client.calls[1][0].startswith("https://bailian-cs.console.aliyun.com/"))
        self.assertEqual(result["windows"][0]["usedPercent"], 50)

    def test_invalid_dpapi_capsule_cannot_expand_origins_or_inject_headers(self):
        capsule = self.home / ".ccs" / "account-usage" / "qwen-console-session.json"
        capsule.parent.mkdir(parents=True)
        for region, cipher, decrypted in (("attacker", "valid-base64", "session=private"),
                                         ("intl", "bad!", "session=private"),
                                         ("intl", base64.b64encode(b"encrypted").decode(), "session=private\r\ninjected=value"),
                                         ("intl", base64.b64encode(b"encrypted").decode(), None)):
            capsule.write_text(json.dumps({"cookieDPAPI": cipher, "region": region}))
            with mock.patch.object(common, "_windows_unprotect", return_value=decrypted):
                self.assertEqual(common.credentials("qwen", self.home), [])

    def test_zai_conflicting_period_enums_do_not_invent_minutes_or_months(self):
        _, windows = usage.parse_zai({"data": {"limits": [
            {"type": "TOKENS_LIMIT", "unit": 5, "number": 1, "percentage": 20, "nextResetTime": 1790899200},
            {"type": "TOKENS_LIMIT", "unit": 4, "number": 1, "percentage": 30, "nextResetTime": 1790985600}]}})
        self.assertEqual([window["usedPercent"] for window in windows], [20, 30])
        for window in windows:
            self.assertIsNone(window["windowMinutes"])
            self.assertNotIn("Monthly", window["label"])
            self.assertNotIn("minutes", window["label"])
            self.assertIsNotNone(window["resetAt"])

    def test_zai_reset_pack_inventory_counts_and_authoritative_utc8_expiry(self):
        windows = usage.parse_zai_reset_packs({"code": 200, "success": True, "data": {
            "fiveHourResets": [{"recordId": "private-record-1", "expireTime": "2026-10-02 08:00:00", "available": True},
                               {"recordId": "private-record-2", "expireTime": "2026-09-01 08:00:00", "available": False}],
            "weekResets": [{"recordId": 3, "expireTime": "2026-10-03 08:00:00", "available": True}],
            "lastFiveHourResetTime": "2026-09-30 08:00:00"}})
        self.assertEqual(windows[0]["remaining"], 1)
        self.assertEqual(windows[0]["unit"], "packs")
        self.assertEqual(windows[1]["expiresAt"], "2026-10-02T00:00:00Z")
        self.assertEqual(windows[2]["remaining"], 0)
        self.assertIsNone(windows[2]["used"])  # False means used OR expired.
        self.assertEqual(windows[3]["remaining"], 1)
        self.assertEqual(windows[4]["expiresAt"], "2026-10-03T00:00:00Z")
        for window in windows:
            self.assertIsNone(window["resetAt"])
            self.assertIsNone(window["usedPercent"])
        self.assertNotIn("private-record", json.dumps(windows))

    def test_zai_empty_reset_lists_prove_zero_but_missing_lists_do_not(self):
        empty = usage.parse_zai_reset_packs({"success": True, "data": {"fiveHourResets": [], "weekResets": []}})
        self.assertEqual([window["remaining"] for window in empty], [0, 0])
        partial = usage.parse_zai_reset_packs({"success": True, "data": {"fiveHourResets": []}})
        self.assertEqual([window["key"] for window in partial], ["reset-packs-5h"])
        with self.assertRaises(common.CollectionError):
            usage.parse_zai_reset_packs({"success": False, "data": {"fiveHourResets": []}})
        with self.assertRaises(common.CollectionError):
            usage.parse_zai_reset_packs({"success": True, "data": {}})

    def test_zai_unknown_reset_pack_availability_is_not_zero(self):
        windows = usage.parse_zai_reset_packs({"success": True, "data": {"fiveHourResets": [
            {"recordId": 1, "expireTime": "2026-10-02 08:00:00", "available": "true"}]}})
        self.assertEqual(len(windows), 1)
        self.assertNotIn("remaining", windows[0])
        self.assertEqual(windows[0]["expiresAt"], "2026-10-02T00:00:00Z")

    def test_zai_denied_reset_inventory_keeps_current_quota_and_no_fake_cards(self):
        self.write_auth({"zai": {"type": "api", "key": "private-key"}})
        quota = {"success": True, "data": {"level": "pro", "limits": [{"type": "TOKENS_LIMIT", "unit": 3,
                                                                       "number": 5, "percentage": 10}]}}
        client = FakeClient(quota, common.CollectionError("needs_sign_in", "private upstream echo"))
        result = usage.collect("zai", "ubuntu", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertIn("reset pack inventory is unavailable", result["message"])
        self.assertEqual(len(result["windows"]), 1)
        self.assertEqual(result["windows"][0]["usedPercent"], 10)
        self.assertEqual(client.calls[1][0], "https://api.z.ai/api/biz/customer-package-reset/list?targetType=PERSONAL")
        self.assertEqual(client.calls[1][1]["Authorization"], "Bearer private-key")
        self.assertNotIn("private", json.dumps(result))

    def test_safari_binarycookies_scope_expiry_and_session_cookie(self):
        blob = safari_fixture([{"name": "valid"}, {"name": "expired", "expiry": 1790800000 - 978307200},
                               {"name": "session-cookie", "expiry": 0},
                               {"name": "attacker", "domain": ".qwencloud.com.evil.example"},
                               {"name": "other", "domain": ".example.com"},
                               {"name": "badpath", "path": "/unrelated"},
                               {"name": "injected", "value": "private\r\nheader=value"}])
        result = common._safari_qwen_cookies(blob, {"qwencloud.com"}, now=1790899200)
        self.assertEqual([cookie["name"] for cookie in result], ["valid", "session-cookie"])
        self.assertEqual(result[0]["expiry"] + 978307200, 1790985600)

    def test_safari_binarycookies_truncation_and_offsets_are_bounded(self):
        good = safari_fixture([{}])
        for length in (0, 7, 11, len(good) - 1):
            self.assertEqual(common._safari_qwen_cookies(good[:length], {"qwencloud.com"}), [])
        malformed = bytearray(good)
        struct.pack_into("<I", malformed, 20, 0xFFFFFFF0)  # Cookie record offset.
        self.assertEqual(common._safari_qwen_cookies(bytes(malformed), {"qwencloud.com"}), [])
        malformed = bytearray(good)
        struct.pack_into(">I", malformed, 4, 0xFFFFFFF0)  # Page count.
        self.assertEqual(common._safari_qwen_cookies(bytes(malformed), {"qwencloud.com"}), [])
        malformed = bytearray(good)
        struct.pack_into("<I", malformed, 12 + 16 + 16, 0xFFFFFFF0)  # Domain string offset.
        self.assertEqual(common._safari_qwen_cookies(bytes(malformed), {"qwencloud.com"}), [])

    def test_safari_console_candidate_stays_local_and_emits_only_usage_dto(self):
        path = self.home / "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies"
        path.parent.mkdir(parents=True)
        path.write_bytes(safari_fixture([{"expiry": 0}]))
        before = path.read_bytes()
        client = FakeClient({"data": {"secToken": "fixture-private-sec"}}, {"data": {"per1MonthPercentage": .1}})
        result = usage.collect("qwen", "mac", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["source"], "Safari console on Mac")
        self.assertEqual(client.calls[0][1]["Cookie"], "session=fixture-private")
        self.assertEqual(path.read_bytes(), before)
        self.assertNotIn("fixture-private", json.dumps(result))

    def test_qwen_v2_dpapi_capsule_keeps_console_and_gateway_headers_separate(self):
        capsule = self.home / ".ccs/account-usage/qwen-console-session.json"
        capsule.parent.mkdir(parents=True)
        capsule.write_text(json.dumps({"version": 2, "cookiesDPAPI": base64.b64encode(b"encrypted").decode(), "region": "intl"}))
        client = FakeClient({"data": {"secToken": "private-sec"}}, {"data": {"per1MonthPercentage": .1}})
        decrypted = json.dumps({"consoleCookie": "homeSession=home-private", "gatewayCookie": "gatewaySession=gateway-private"})
        with mock.patch.object(common, "_windows_unprotect", return_value=decrypted):
            result = usage.collect("qwen", "windows", self.home, client)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(client.calls[0][1]["Cookie"], "homeSession=home-private")
        self.assertEqual(client.calls[1][1]["Cookie"], "gatewaySession=gateway-private")
        self.assertEqual(client.calls[1][1]["Referer"], "https://home.qwencloud.com/analytics/token-plan/individual")
        self.assertNotIn("private", json.dumps(result))

    def test_qwen_v2_empty_gateway_cookie_never_forwards_home_cookie(self):
        secret = json.dumps({"cookie": "homeSession=private", "gatewayCookie": ""})
        client = FakeClient({"data": {"secToken": "private-sec"}}, {"data": {"per1MonthPercentage": .1}})
        usage.fetch_qwen(client, secret)
        self.assertIn("Cookie", client.calls[0][1])
        self.assertNotIn("Cookie", client.calls[1][1])

    def test_qwen_current_addon_list_preserves_each_balance_and_expiration(self):
        addon = {"data": {"DataV2": {"data": {"items": [
            {"orderId": "private-order-1", "remainingCredits": 25, "totalCredits": 100, "endTime": 1790985600000},
            {"orderId": "private-order-2", "remainingCredits": 0, "endTime": 1790899200000}]}}}}
        _, windows = usage.parse_qwen({"data": {"per1MonthPercentage": .1}}, addon=addon)
        self.assertEqual(windows[1]["usedPercent"], 75)
        self.assertEqual(windows[1]["remaining"], 25)
        self.assertEqual(windows[1]["expiresAt"], "2026-10-03T00:00:00Z")
        self.assertIsNone(windows[1]["resetAt"])
        self.assertEqual(windows[2]["remaining"], 0)
        self.assertIsNone(windows[2]["limit"])
        self.assertIsNone(windows[2]["usedPercent"])
        self.assertEqual(windows[3]["remaining"], 2)
        self.assertNotIn("private-order", json.dumps(windows))


    def test_actual_overage_keeps_reset_and_other_windows(self):
        payload = go_payload()
        payload["usage"]["weekly"]["percent"] = 125.5
        _, windows = usage.parse_go(payload)
        weekly = next(window for window in windows if window["key"] == "weekly")
        self.assertEqual(weekly["usedPercent"], 125.5)
        self.assertEqual(weekly["remainingPercent"], 0)
        self.assertEqual(weekly["resetAt"], "2026-10-02T12:00:00Z")
        self.assertEqual(len(windows), 3)
        for invalid in (-1, True, float("nan"), float("inf")):
            self.assertIsNone(common.percent(invalid))

    def test_safari_host_only_cookies_do_not_cross_qwencloud_hosts(self):
        path = self.home / "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies"
        path.parent.mkdir(parents=True)
        path.write_bytes(safari_fixture([
            {"domain": "home.qwencloud.com", "name": "home", "value": "home-private", "expiry": 0},
            {"domain": "cs-data.qwencloud.com", "name": "gateway", "value": "gateway-private", "expiry": 0},
            {"domain": ".qwencloud.com", "name": "shared", "value": "shared-private", "expiry": 0}]))
        client = FakeClient({"data": {"secToken": "private-sec"}}, {"data": {"per1MonthPercentage": .1}})
        result = usage.collect("qwen", "mac", self.home, client)
        self.assertEqual(result["status"], "ok")
        home_header, gateway_header = client.calls[0][1]["Cookie"], client.calls[1][1]["Cookie"]
        self.assertIn("home-private", home_header)
        self.assertNotIn("gateway-private", home_header)
        self.assertIn("gateway-private", gateway_header)
        self.assertNotIn("home-private", gateway_header)
        self.assertIn("shared-private", home_header)
        self.assertIn("shared-private", gateway_header)



class RegistryV2AccountTests(unittest.TestCase):
    """Registry v2 account arguments: one named store per account, never a fallback."""

    KEY_ID = "9f2c41d0"
    SECRET = "zai-dashboard-key-private-0123"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = Path(self.temp.name)
        self.env = mock.patch.dict(os.environ, {"ZAI_API_KEY": "env-private-key"}, clear=True)
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.temp.cleanup()

    def key_dir(self, mode=0o700):
        directory = self.home / ".ccs" / "account-usage" / "keys"
        directory.mkdir(parents=True, exist_ok=True)
        directory.chmod(mode)
        return directory

    def write_key(self, provider="zai", key_id=None, mode=0o600, stored_provider=None, **overrides):
        key_id = key_id or self.KEY_ID
        record = {"version": 1, "provider": provider, "keyId": key_id, "secret": self.SECRET,
                  "fingerprint": common.key_fingerprint(self.SECRET), "last4": self.SECRET[-4:],
                  "createdAt": "2026-10-02T07:00:00Z"}
        record.update(overrides)
        if stored_provider is not None:
            record["provider"] = stored_provider
        path = self.key_dir() / "{}-{}.json".format(provider, key_id)
        path.write_text(json.dumps(record))
        path.chmod(mode)
        return path

    def other_stores(self):
        auth = self.home / ".local" / "share" / "opencode" / "auth.json"
        auth.parent.mkdir(parents=True, exist_ok=True)
        auth.write_text(json.dumps({"zai": {"type": "api", "key": "opencode-private-key"}}))

    def aac(self, key_id=None):
        return {"kind": "aac-key", "keyId": key_id or self.KEY_ID}

    def test_aac_key_reads_only_its_own_file(self):
        self.other_stores()
        self.write_key()
        found = common.credentials("zai", self.home, self.aac())
        self.assertEqual(found, [{"secret": self.SECRET, "source": "Dashboard key", "email": None, "expires": None}])
        # Without a selection the existing first-working lookup is unchanged.
        self.assertEqual(common.credentials("zai", self.home)[0]["secret"], "opencode-private-key")

    def test_aac_key_collect_uses_the_key_and_carries_the_account_id(self):
        self.other_stores()
        self.write_key()
        payload = {"success": True, "code": 200, "data": {"level": "pro", "limits": [
            {"type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10}]}}
        client = FakeClient(payload, Exception("no reset packs"))
        result = usage.collect("zai", "ubuntu", self.home, client, self.aac(), "zai:acct:9f2c41d0")
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["id"], "zai:acct:9f2c41d0")
        self.assertEqual(client.calls[0][1]["Authorization"], self.SECRET)
        serialized = json.dumps(result)
        for secret in (self.SECRET, "env-private-key", "opencode-private-key"):
            self.assertNotIn(secret, serialized)

    def test_missing_or_unsafe_key_never_falls_back_to_other_stores(self):
        self.other_stores()
        cases = {
            "missing": lambda: self.key_dir(),
            "group readable": lambda: self.write_key(mode=0o640),
            "other readable": lambda: self.write_key(mode=0o604),
            "open directory": lambda: (self.write_key(), self.key_dir(0o755)),
            "wrong provider": lambda: self.write_key(stored_provider="kimi-code"),
            "wrong key id": lambda: self.write_key(keyId="0000aaaa"),
            "bad fingerprint": lambda: self.write_key(fingerprint="sha256:0000000000000000"),
            "whitespace secret": lambda: self.write_key(secret="has space secret", fingerprint=common.key_fingerprint("has space secret")),
            "short secret": lambda: self.write_key(secret="short", fingerprint=common.key_fingerprint("short")),
            "unknown field": lambda: self.write_key(command="PRIVATE"),
            "old version": lambda: self.write_key(version=2),
        }
        for name, prepare in cases.items():
            with self.subTest(name):
                keys = self.home / ".ccs" / "account-usage" / "keys"
                if keys.exists():
                    keys.chmod(0o700)
                    for child in keys.iterdir():
                        child.unlink()
                prepare()
                client = FakeClient()
                self.assertEqual(common.credentials("zai", self.home, self.aac()), [])
                result = usage.collect("zai", "ubuntu", self.home, client, self.aac(), "zai:acct:9f2c41d0")
                self.assertEqual(result["status"], "unavailable")
                self.assertEqual(result["message"], "The stored credential for this account could not be read.")
                self.assertEqual(client.calls, [])

    def test_symlinked_key_file_and_oversized_file_are_refused(self):
        target = self.home / "elsewhere.json"
        target.write_text(json.dumps({"version": 1, "provider": "zai", "keyId": self.KEY_ID, "secret": self.SECRET}))
        target.chmod(0o600)
        link = self.key_dir() / "zai-{}.json".format(self.KEY_ID)
        link.symlink_to(target)
        self.assertEqual(common.credentials("zai", self.home, self.aac()), [])
        link.unlink()
        self.write_key(createdAt="x" * 5000)
        self.assertEqual(common.credentials("zai", self.home, self.aac()), [])

    def test_key_selection_is_limited_to_key_providers_and_hex_ids(self):
        self.write_key(provider="qwen")
        self.assertEqual(common.credentials("qwen", self.home, self.aac()), [])
        for key_id in ("../zai", "9F2C41D0", "9f2c41d", "9f2c41d0\n"):
            self.assertIsNone(common.aac_key_credential("zai", key_id, self.home))

    def write_capsule(self, name):
        capsule = self.home / ".ccs" / "account-usage" / name
        capsule.parent.mkdir(parents=True, exist_ok=True)
        capsule.write_text(json.dumps({"cookieDPAPI": base64.b64encode(b"encrypted").decode(), "region": "intl"}))
        return capsule

    def test_browser_capsule_reads_only_the_named_capsule(self):
        self.write_capsule("qwen-console-session.json")
        self.write_capsule("qwen-console-session-0a1b2c3d.json")
        auth = self.home / ".local" / "share" / "opencode" / "auth.json"
        auth.parent.mkdir(parents=True, exist_ok=True)
        auth.write_text(json.dumps({"alibaba-token-plan": {"type": "api", "key": "sk-private"}}))
        with mock.patch.object(common, "_windows_unprotect", return_value="session=private-cookie"):
            named = common.credentials("qwen", self.home, {"kind": "browser-capsule", "capsuleId": "0a1b2c3d"})
            default = common.credentials("qwen", self.home, {"kind": "browser-capsule", "capsuleId": "default"})
            missing = common.credentials("qwen", self.home, {"kind": "browser-capsule", "capsuleId": "ffffffff"})
            invalid = common.credentials("qwen", self.home, {"kind": "browser-capsule", "capsuleId": "../x"})
        self.assertEqual(len(named), 1)
        self.assertEqual(len(default), 1)
        self.assertEqual(json.loads(named[0]["secret"])["cookie"], "session=private-cookie")
        self.assertNotIn("token", json.loads(named[0]["secret"]))
        self.assertEqual(missing, [])
        self.assertEqual(invalid, [])

    def run_main(self, *arguments):
        environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(self.home),
                       "USERPROFILE": str(self.home), "PYTHONDONTWRITEBYTECODE": "1"}
        return subprocess.run([sys.executable, str(HELPERS / "plan_usage.py"), *arguments],
                              capture_output=True, text=True, timeout=20, env=environment)

    def test_command_line_accepts_the_enumerated_arguments_and_echoes_the_account(self):
        completed = self.run_main("--provider", "zai", "--platform", "ubuntu", "--account", "zai:acct:9f2c41d0",
                                  "--credential", "aac-key", "--key-id", "9f2c41d0")
        self.assertEqual(completed.returncode, 0)
        result = json.loads(completed.stdout)
        self.assertEqual((result["id"], result["status"]), ("zai:acct:9f2c41d0", "unavailable"))
        legacy = self.run_main("--provider", "zai", "--platform", "ubuntu")
        self.assertEqual(legacy.returncode, 0)
        self.assertEqual(json.loads(legacy.stdout)["id"], "plan-zai-ubuntu")

    def test_command_line_rejects_inconsistent_arguments_with_usage_status(self):
        for arguments in (
            ("--provider", "zai", "--credential", "aac-key", "--key-id", "9f2c41d0"),
            ("--provider", "zai", "--account", "zai:acct:9f2c41d0", "--credential", "aac-key"),
            ("--provider", "zai", "--account", "zai:acct:9f2c41d0", "--credential", "aac-key", "--key-id", "ZZ"),
            ("--provider", "zai", "--account", "kimi-code:acct:9f2c41d0", "--credential", "aac-key", "--key-id", "9f2c41d0"),
            ("--provider", "qwen", "--account", "qwen:acct:9f2c41d0", "--credential", "aac-key", "--key-id", "9f2c41d0"),
            ("--provider", "zai", "--account", "zai:acct:9f2c41d0", "--credential", "browser-capsule", "--capsule-id", "default"),
            ("--provider", "zai", "--account", "zai:usage", "--credential", "discover", "--key-id", "9f2c41d0"),
            ("--provider", "zai", "--account", "zai:acct:9f2c41d0", "--credential", "config-home", "--home-id", "9f2c41d0"),
            ("--provider", "zai", "--key-id", "9f2c41d0"),
        ):
            with self.subTest(arguments=arguments):
                completed = self.run_main("--platform", "ubuntu", *arguments)
                self.assertEqual(completed.returncode, 2)
                self.assertEqual(completed.stdout, "")


if __name__ == "__main__":
    unittest.main()
