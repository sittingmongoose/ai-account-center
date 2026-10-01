"""Muse portal fixtures: no network, real credentials or model calls."""
import copy
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "native-host"
sys.path.insert(0, str(SOURCE))
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "ccs/scripts/account-usage"))
import muse_console as muse
import desktop_usage as usage
import desktop_helpers as helpers
import host

COOKIE = {"name": "llama_dev_sess", "value": "synthetic-web-session", "domain": "dev.meta.ai", "path": "/", "secure": True, "hostOnly": True}
EMAIL = "example@example.com"
PLAN = "Muse Code High Usage"
QUOTA = {"tier": PLAN, "window_weighted_used": "0", "window_weighted_limit": "1000", "window_duration_secs": 18000,
         "window_resets_at": None, "weekly_weighted_used": "60", "weekly_weighted_limit": "1000", "weekly_resets_at": 1790812800}


class FakeClient:
    def __init__(self, email=EMAIL, teams=None, quota=None):
        self.email = email
        self.teams = [{"team_id": 42, "team_name": "Personal"}] if teams is None else teams
        self.quota = QUOTA if quota is None else quota
        self.calls = []

    def get(self, route):
        self.calls.append(route)
        if route == "/api/auth/me":
            return {"email": self.email, "secret": "sk-private"}
        if route == "/api/portal/teams":
            return {"teams": self.teams}
        return {"subscription_quota": self.quota, "api_key": "sk-private"}


class CookieTests(unittest.TestCase):
    def test_only_exact_session_scope(self):
        self.assertEqual(muse.validate_cookies([COOKIE]), [COOKIE])
        for field, value in (("domain", ".meta.ai"), ("name", "datr"), ("path", "/api"), ("value", "dca:device-token"),
                             ("value", "sk-inference-key"), ("value", "x;secret"), ("secure", False), ("hostOnly", None)):
            with self.subTest(field=field), self.assertRaises(muse.MuseError):
                muse.validate_cookies([dict(COOKIE, **{field: value})])

    def test_expired_or_duplicate_not_reused(self):
        for cookies in ([dict(COOKIE, expirationDate=1)], [COOKIE, COOKIE]):
            with self.assertRaises(muse.MuseError):
                muse.validate_cookies(cookies)

    def test_unknown_fields_are_rejected(self):
        with self.assertRaises(muse.MuseError):
            muse.validate_cookies([dict(COOKIE, bearer="secret")])

    def test_redirect_never_forwards_web_cookie(self):
        with self.assertRaises(muse.MuseError):
            muse.NoRedirect().redirect_request(None, None, 302, "redirect", {}, "https://other.example/")


class IdentityTests(unittest.TestCase):
    def test_same_email_plan_and_unique_team(self):
        client = FakeClient()
        team, quota = muse.fetch_quota([COOKIE], EMAIL.upper(), PLAN, client=client)
        self.assertEqual(team, "42")
        self.assertEqual(client.calls, ["/api/auth/me", "/api/portal/teams", "/api/portal/teams/42/subscription-quota"])
        self.assertEqual(quota, QUOTA)

    def test_different_account_stops_before_team_read(self):
        client = FakeClient(email="other@example.com")
        with self.assertRaises(muse.MuseError) as error:
            muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client)
        self.assertEqual(error.exception.code, "account_mismatch")
        self.assertEqual(client.calls, ["/api/auth/me"])

    def test_multiple_teams_need_explicit_choice(self):
        client = FakeClient(teams=[{"team_id": 42, "team_name": "First"}, {"team_id": 43, "team_name": "Second"}])
        with self.assertRaises(muse.MuseError) as error:
            muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client)
        self.assertEqual(error.exception.code, "choose_team")
        self.assertEqual(len(error.exception.teams), 2)
        self.assertEqual(len(client.calls), 2)

    def test_selected_team_is_verified(self):
        client = FakeClient(teams=[{"team_id": 42}, {"team_id": 43}])
        team, _ = muse.fetch_quota([COOKIE], EMAIL, PLAN, "43", client)
        self.assertEqual(team, "43")
        with self.assertRaises(muse.MuseError):
            muse.fetch_quota([COOKIE], EMAIL, PLAN, "99", FakeClient())

    def test_plan_mismatch_is_not_another_accounts_quota(self):
        with self.assertRaises(muse.MuseError) as error:
            muse.fetch_quota([COOKIE], EMAIL, PLAN, client=FakeClient(quota=dict(QUOTA, tier="Other")))
        self.assertEqual(error.exception.code, "plan_mismatch")

    def test_unknown_upstream_secrets_are_discarded(self):
        _, quota = muse.fetch_quota([COOKIE], EMAIL, PLAN, client=FakeClient(quota=dict(QUOTA, api_key="sk-private", payments={"secret": "private"})))
        self.assertNotIn("private", json.dumps(quota))

    def test_duplicate_unsafe_or_empty_teams_refused(self):
        for teams in ([], [{"team_id": "../secret"}], [{"team_id": 42}, {"team_id": 42}], [{"team_id": True}], [None]):
            with self.subTest(teams=teams), self.assertRaises(muse.MuseError):
                muse.fetch_quota([COOKIE], EMAIL, PLAN, client=FakeClient(teams=teams))


class CapsuleTests(unittest.TestCase):
    def test_bound_private_capsule_and_no_invented_expiry(self):
        with tempfile.TemporaryDirectory() as root:
            muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            path = Path(root) / muse.CAPSULE_NAME
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(muse.read_capsule(root), ([COOKIE], "42", EMAIL, PLAN))

    def test_insecure_root_and_symlink_refused(self):
        with tempfile.TemporaryDirectory() as root:
            os.chmod(root, 0o755)
            with self.assertRaises(muse.MuseError):
                muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            os.chmod(root, 0o700)
            (Path(root) / muse.CAPSULE_NAME).symlink_to(Path(root) / "other")
            with self.assertRaises(muse.MuseError):
                muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)

    def test_missing_capsule_is_no_browser_not_signin(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(muse.MuseError) as error:
                muse.read_capsule(root)
            self.assertEqual(error.exception.code, "no_browser_cookie")

    def test_bad_permissions_refused(self):
        with tempfile.TemporaryDirectory() as root:
            muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            os.chmod(Path(root) / muse.CAPSULE_NAME, 0o644)
            with self.assertRaises(muse.MuseError):
                muse.read_capsule(root)


class CollectionTests(unittest.TestCase):
    @patch("muse_console.read_capsule", return_value=([COOKIE], "42", EMAIL, PLAN))
    @patch("muse_console.fetch_quota", return_value=("42", QUOTA))
    @patch("desktop_usage.request_json", return_value={"user_email": EMAIL, "subs_tier_name": PLAN, "is_subs_active": True, "api_key": "sk-private"})
    def test_helper_fallback_preserves_identity_and_real_limits(self, request, quota, capsule):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential("dca:private"), result)
        self.assertEqual(result["status"], "ok")
        self.assertEqual([row["usedPercent"] for row in result["windows"]], [0, 6])
        self.assertIsNone(result["windows"][0]["resetAt"])
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(quota.call_args.args[1:], (EMAIL, PLAN, "42"))

    @patch("muse_console.read_capsule", return_value=([COOKIE], "42", "other@example.com", PLAN))
    @patch("muse_console.fetch_quota")
    @patch("desktop_usage.request_json", return_value={"user_email": EMAIL, "subs_tier_name": PLAN, "is_subs_active": True})
    def test_capsule_cannot_cross_accounts(self, request, quota, capsule):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential("dca:private"), result)
        quota.assert_not_called()
        self.assertEqual(result["status"], "unavailable")
        self.assertIn("differs", result["message"])

    @patch("muse_console.collect_browser", return_value=("42", {"email": EMAIL, "plan": PLAN, "windows": []}))
    @patch("muse_console.write_capsule")
    def test_combined_host_has_distinct_muse_action(self, writer, collector):
        response = host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], "teamId": None}, "/unused")
        self.assertTrue(response["ok"])
        self.assertEqual(response["teamId"], "42")
        writer.assert_called_once_with("/unused", [COOKIE], "42", EMAIL, PLAN)

    def test_native_action_rejects_unknown_paths_and_identity_claims(self):
        for extra in ({"url": "https://evil.example"}, {"email": EMAIL}, {"plan": PLAN}):
            with self.assertRaises(muse.MuseError):
                host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], **extra}, "/unused")


if __name__ == "__main__":
    unittest.main()
