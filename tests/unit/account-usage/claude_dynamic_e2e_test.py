"""End-to-end regression: raw upstream /api/oauth/usage JSON through producer to DTO.

This test pushes a REAL upstream-shaped payload (limits[] + cedar_ember + juniper_tide)
through the actual producer entry points (claude_usage.collect and failover_core.collect)
and then through failover_usage.public_sample (the same sanitizer the TS dashboard sees),
asserting the DTO-equivalent windows:

- seven_day_opus is scoped with its own reset
- grant with clears is preserved
- missing five_hour stays absent (never synthesized)
- eligible != true grants stay unknown (never zero)

On the pre-fix base (adb52ce45) the test FAILS: the producers discard cedar_ember from
the OAuth payload (only optional_web_windows reads it), and failover_usage drops clears.
"""

import sys
import time
from pathlib import Path
from unittest import mock
import unittest

HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import claude_usage as usage
import failover_core as core
import failover_usage as fusage

ACCOUNT = "11111111-1111-1111-1111-111111111111"
CLIENT = "22222222-2222-2222-2222-222222222222"
ORG = "33333333-3333-3333-3333-333333333333"
TOKEN = "fixture-token"
EMAIL = "fixture-profile@example.com"


def _key():
    return f"acct:{ACCOUNT}|{CLIENT}:{ORG}:https://api.anthropic.com:user:inference user:profile"


def _cache():
    return {_key(): {"token": TOKEN, "expiresAt": (time.time() + 3600) * 1000}}


def _profile():
    return {"account": {"uuid": ACCOUNT, "email": EMAIL, "has_claude_max": True},
            "organization": {"uuid": ORG}}


def _raw_oauth_usage():
    """REAL upstream shape: OAuth usage JSON as the API returns it.

    five_hour is null (removed upstream on many accounts) — the producer must NOT
    synthesize it. limits[] carries per-model scoped weeklies with distinct resets.
    cedar_ember carries grants with clears. juniper_tide carries the weekly allowance.
    """
    return {
        "seven_day": {"utilization": 12, "resets_at": "2026-10-08T12:00:00Z"},
        "five_hour": None,
        "limits": [
            {"kind": "weekly_scoped", "group": "weekly", "percent": 40,
             "resets_at": "2026-10-09T12:00:00Z", "is_active": True,
             "scope": {"model": {"display_name": "Opus"}, "surface": None}},
            {"kind": "weekly_scoped", "group": "weekly", "percent": 30,
             "resets_at": "2026-10-10T12:00:00Z", "is_active": True,
             "scope": {"model": {"display_name": "Sonnet"}, "surface": None}},
        ],
        "cedar_ember": {"eligible": True, "grants": [
            {"resets_left": 2, "resets_total": 3, "paused": False,
             "clears": ["five_hour", "seven_day_opus"]},
        ]},
        "juniper_tide": {"eligible": True, "available": True,
                         "resets_per_week": 2,
                         "next_available_at": "2026-10-12T00:00:00Z"},
    }


def _raw_oauth_usage_ineligible():
    raw = _raw_oauth_usage()
    raw["cedar_ember"] = {"eligible": False, "grants": []}
    raw["juniper_tide"] = {"eligible": False, "available": True, "resets_per_week": 2,
                           "next_available_at": "2026-10-12T00:00:00Z"}
    return raw


class _FakeClient:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []
    def get(self, route, token):
        self.calls.append(route)
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response


class DesktopLiveE2ETest(unittest.TestCase):
    """claude_usage.collect (the helper the live service spawns) end-to-end."""

    def test_raw_oauth_payload_yields_scoped_grant_and_absence(self):
        client = _FakeClient(_profile(), _raw_oauth_usage())
        # No capsule: optional_web is unavailable, so only the OAuth payload's own
        # cedar_ember/juniper_tide can supply grants. This is the production gap:
        # pre-fix the collector discards them and only the web path can supply grants.
        with mock.patch.object(usage.sys, "platform", "win32"), \
             mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, _cache())), \
             mock.patch.object(usage, "Client", return_value=client), \
             mock.patch.object(usage, "optional_web_windows", return_value=[]):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "ok")
        by_key = {w["key"]: w for w in result["windows"]}
        # seven_day_opus scoped with own reset
        self.assertIn("seven_day_opus", by_key)
        self.assertEqual(by_key["seven_day_opus"]["usedPercent"], 40)
        self.assertEqual(by_key["seven_day_opus"]["resetAt"], "2026-10-09T12:00:00.000Z")
        self.assertEqual(by_key["seven_day_opus"]["windowMinutes"], 10080)
        # Sonnet also scoped with distinct reset
        self.assertIn("seven_day_sonnet", by_key)
        self.assertEqual(by_key["seven_day_sonnet"]["resetAt"], "2026-10-10T12:00:00.000Z")
        # Grant with clears preserved (from OAuth cedar_ember, not web)
        grants = [w for w in result["windows"] if "grant" in w["key"]]
        self.assertTrue(grants, "OAuth cedar_ember grant must survive via the producer even with no web capsule")
        grant = grants[0]
        self.assertEqual(grant.get("clears"), ["five_hour", "seven_day_opus"])
        # Summary also present
        self.assertIn("reset_credits_available", by_key)
        # Juniper allowance from OAuth
        self.assertIn("session_reset_allowance", by_key)
        # Missing five_hour stays absent (never synthesized from other windows)
        self.assertNotIn("five_hour", by_key)

    def test_ineligible_grants_stay_unknown_never_zero(self):
        client = _FakeClient(_profile(), _raw_oauth_usage_ineligible())
        with mock.patch.object(usage.sys, "platform", "win32"), \
             mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, _cache())), \
             mock.patch.object(usage, "Client", return_value=client), \
             mock.patch.object(usage, "optional_web_windows", return_value=[]):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "ok")
        # Eligible != true withholds detail: no summary, no grants, no synthesis
        keys = {w["key"] for w in result["windows"]}
        self.assertNotIn("reset_credits_available", keys)
        self.assertFalse(any(k.startswith("reset_credit_") for k in keys))
        self.assertNotIn("session_reset_allowance", keys)
        # Scoped windows still appear
        self.assertIn("seven_day_opus", {w["key"] for w in result["windows"]})


class FailoverCoreE2ETest(unittest.TestCase):
    """failover_core.collect (dashboard failover path) end-to-end."""

    def _candidate(self, mode="cli"):
        return {
            "_provider": "claude",
            "_token": TOKEN,
            "_account": ACCOUNT,
            "_organization": ORG,
            "email": EMAIL,
            "plan": "max",
            "identity": {"kind": "claude-account-org-v1", "key": "0" * 64},
            "revision": "1" * 64,
            "locator": "a" * 64,
            "selectors": ["claude-profile:default"],
            "_mode": mode,
            "_profile_root": None,
            "_expires": (time.time() + 3600) * 1000,
        }

    def test_raw_oauth_payload_yields_scoped_grant_and_absence(self):
        raw = _raw_oauth_usage()
        client = _FakeClient(_profile(), raw)
        with mock.patch.object(core, "Client", return_value=client):
            # For CLI mode on Ubuntu/Mac there is no web capsule; _claude_optional_windows
            # returns unavailable. The OAuth payload's cedar_ember must still supply grants.
            with mock.patch.object(core, "_claude_optional_windows",
                                   return_value=([], {"optionalExtras": {"resetCredits": "unavailable", "prepaidBalance": "unavailable"}})):
                result = core.collect(self._candidate("cli"), "/tmp/fakehome", "ubuntu")
        self.assertIsNone(result.get("failureCode"))
        sample = result.get("sample")
        self.assertIsNotNone(sample)
        by_key = {w["key"]: w for w in sample["windows"]}
        self.assertIn("seven_day_opus", by_key)
        self.assertEqual(by_key["seven_day_opus"]["usedPercent"], 40)
        self.assertEqual(by_key["seven_day_opus"]["resetAt"], "2026-10-09T12:00:00.000Z")
        grants = [w for w in sample["windows"] if "grant" in w["key"]]
        self.assertTrue(grants, "failover_core must parse OAuth cedar_ember grants, not just web")
        self.assertEqual(grants[0].get("clears"), ["five_hour", "seven_day_opus"])
        self.assertNotIn("five_hour", by_key)

    def test_desktop_mode_clears_preserved_through_public_sample(self):
        """The full failover sanitizer (failover_usage.public_sample) must keep clears."""
        raw = _raw_oauth_usage()
        # Build windows as the collector would (after fix) and push through the
        # sanitizer the TS server actually sees.
        from failover_core import Client as _C  # noqa: F401 (ensure import)
        wins = usage.normalize_windows(raw)
        wins.extend(usage.normalize_reset_credits(raw["cedar_ember"]))
        wins.extend(usage.normalize_session_reset_allowance(raw["juniper_tide"]))
        sample = {
            "status": "ok",
            "provider": "claude",
            "platform": "windows",
            "sampledAt": "2026-10-01T12:00:00.000Z",
            "fetchedAt": "2026-10-01T12:00:00.000Z",
            "email": EMAIL,
            "plan": "max",
            "windows": wins,
        }
        pub = fusage.public_sample(sample, "claude", "windows")
        self.assertIsNotNone(pub)
        by_key = {w["key"]: w for w in pub["windows"]}
        self.assertIn("seven_day_opus", by_key)
        grant = [w for w in pub["windows"] if "grant" in w["key"]][0]
        self.assertEqual(grant.get("clears"), ["five_hour", "seven_day_opus"],
                         "failover_usage._window must preserve clears; pre-fix it drops them")
        self.assertNotIn("five_hour", by_key)


class ClearsPreservationTest(unittest.TestCase):
    def test_public_sample_preserves_clears(self):
        wins = usage.normalize_reset_credits({"eligible": True, "grants": [
            {"resets_left": 1, "resets_total": 1, "paused": False,
             "clears": ["five_hour", "seven_day"]},
        ]})
        self.assertEqual(wins[1].get("clears"), ["five_hour", "seven_day"])
        sample = {
            "status": "ok",
            "provider": "claude",
            "platform": "windows",
            "sampledAt": "2026-10-01T12:00:00.000Z",
            "fetchedAt": "2026-10-01T12:00:00.000Z",
            "email": EMAIL,
            "plan": "max",
            "windows": wins,
        }
        pub = fusage.public_sample(sample, "claude", "windows")
        by_key = {w["key"]: w for w in pub["windows"]}
        grant = [w for w in pub["windows"] if "grant" in w["key"]][0]
        self.assertEqual(grant.get("clears"), ["five_hour", "seven_day"])


if __name__ == "__main__":
    unittest.main()
