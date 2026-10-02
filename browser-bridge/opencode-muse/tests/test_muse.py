"""Muse portal fixtures: no network, real credentials or model calls."""
import copy
import io
import json
import math
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "native-host"
sys.path.insert(0, str(SOURCE))
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/account-usage"))
import muse_console as muse
import desktop_usage as usage
import desktop_helpers as helpers
import host

COOKIE = {"name": "llama_dev_sess", "value": "synthetic-web-session", "domain": "dev.meta.ai", "path": "/", "secure": True, "hostOnly": True}
EMAIL = "example@example.com"
PLAN = "Muse Code High Usage"
QUOTA = {"tier": PLAN, "window_weighted_used": "0", "window_weighted_limit": "1000", "window_duration_secs": 18000,
         "window_resets_at": None, "weekly_weighted_used": "60", "weekly_weighted_limit": "1000", "weekly_resets_at": 1790812800}


USER = "synthetic-user-1"
USER_HASH = muse.user_hash({"userId": USER})


class FakeClient:
    def __init__(self, email=EMAIL, teams=None, quota=None, user=USER):
        self.email = email
        self.user = user
        self.teams = [{"team_id": 42, "team_name": "Personal"}] if teams is None else teams
        self.quota = QUOTA if quota is None else quota
        self.calls = []

    def get(self, route):
        self.calls.append(route)
        if route == "/api/auth/me":
            return {"email": self.email, "userId": self.user, "secret": "sk-private"}
        if route == "/api/portal/teams":
            return {"teams": self.teams}
        return {"subscription_quota": self.quota, "api_key": "sk-private"}


class ShapeClient(FakeClient):
    """Answers /api/auth/me with an exact payload."""
    def __init__(self, me, **kwargs):
        super().__init__(**kwargs)
        self.me = me

    def get(self, route):
        if route == "/api/auth/me":
            self.calls.append(route)
            return self.me
        return super().get(route)


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

    def test_missing_blank_or_malformed_identity_is_not_a_proven_different_account(self):
        for payload in ({}, {"email": ""}, {"email": None}, {"email": " "}, {"email": 42},
                        {"email": "not-an-email"}, {"email": "bad\n@example.com"}, {"user": {"email": EMAIL}}):
            with self.subTest(payload=payload):
                client = FakeClient()
                with patch.object(client, "get", return_value=payload) as get:
                    with self.assertRaises(muse.MuseError) as error:
                        muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client)
                self.assertEqual(error.exception.code, "identity_unavailable")
                get.assert_called_once_with("/api/auth/me")

    def test_email_less_auth_me_reads_only_the_bound_team_for_the_pinned_user(self):
        # Live 2026-10-02: HTTP 200 /api/auth/me with "email": "" plus a userId.
        # A missing key or a malformed value is the same case; it proves no other account.
        shapes = ({"email": ""}, {}, {"email": None}, {"email": " "}, {"email": 42},
                  {"email": "not-an-email"}, {"email": "bad\n@example.com"}, {"user": {"email": EMAIL}})
        for shape in shapes:
            for requested in (None, "42"):
                with self.subTest(shape=shape, requested=requested):
                    client = ShapeClient(dict(shape, userId=USER), teams=[{"team_id": 42}, {"team_id": 43}])
                    observed = {}
                    team, quota = muse.fetch_quota([COOKIE], EMAIL, PLAN, requested, client, bound_team="42",
                                                   bound_user=USER_HASH, observed=observed)
                    self.assertEqual((team, quota), ("42", QUOTA))
                    self.assertEqual(observed, {"mode": "user", "userHash": USER_HASH})
                    self.assertEqual(client.calls, ["/api/auth/me", "/api/portal/teams",
                                                    "/api/portal/teams/42/subscription-quota"])

    def test_non_dict_or_userless_auth_me_is_never_bound(self):
        for payload in (None, [], "email", {"email": ""}, {"email": "", "userId": ""}, {"email": "", "userId": True},
                        {"email": "", "userId": "a b"}, {"email": "", "userId": "x" * 129}, {"email": "", "userId": {}}):
            with self.subTest(payload=payload):
                client = FakeClient()
                with patch.object(client, "get", return_value=payload) as get:
                    with self.assertRaises(muse.MuseError) as error:
                        muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", bound_user=USER_HASH,
                                         capsule_session=True)
                self.assertEqual(error.exception.code, "identity_unavailable")
                get.assert_called_once_with("/api/auth/me")

    def test_user_hash_is_a_stable_digest_never_the_id(self):
        self.assertEqual(muse.user_hash({"userId": 1234}), muse.user_hash({"userId": "1234"}))
        self.assertNotEqual(muse.user_hash({"userId": "1234"}), muse.user_hash({"userId": "1235"}))
        self.assertRegex(USER_HASH, r"^[0-9a-f]{64}$")
        self.assertNotIn(USER, USER_HASH)

    def test_another_web_user_is_account_mismatch_even_in_the_bound_team(self):
        # The review's case: another Muse web account in the same team signed into the browser.
        client = FakeClient(email="", user="someone-else")
        with self.assertRaises(muse.MuseError) as error:
            muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", bound_user=USER_HASH,
                             capsule_session=True)
        self.assertEqual(error.exception.code, "account_mismatch")
        self.assertEqual(client.calls, ["/api/auth/me"])

    def test_without_a_pin_only_the_capsule_session_with_one_team_is_bound(self):
        observed = {}
        client = FakeClient(email="")
        team, _ = muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", capsule_session=True,
                                   observed=observed)
        self.assertEqual((team, observed), ("42", {"mode": "session", "userHash": USER_HASH}))
        for teams in ([{"team_id": 42}, {"team_id": 43}], [{"team_id": 43}]):
            with self.subTest(teams=teams):
                client = FakeClient(email="", teams=teams)
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", capsule_session=True)
                self.assertEqual(error.exception.code, "identity_unavailable")
                self.assertEqual(client.calls, ["/api/auth/me", "/api/portal/teams"])
        # Any other browser session, or an unreadable or foreign pin, stops after auth/me.
        for bound_user, session in ((None, False), (None, 1), (False, True), ("not-a-hash", True), ("A" * 64, True)):
            with self.subTest(bound_user=bound_user, session=session):
                client = FakeClient(email="")
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", bound_user=bound_user,
                                     capsule_session=session)
                self.assertEqual(error.exception.code, "identity_unavailable")
                self.assertEqual(client.calls, ["/api/auth/me"])

    def test_blank_identity_never_chooses_or_switches_to_another_team(self):
        for teams in ([{"team_id": 43}], [{"team_id": 43}, {"team_id": 44}]):
            with self.subTest(teams=teams):
                client = FakeClient(email="", teams=teams)
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", bound_user=USER_HASH)
                self.assertEqual(error.exception.code, "identity_unavailable")
                self.assertEqual(client.calls, ["/api/auth/me", "/api/portal/teams"])

    def test_a_binding_never_reads_another_team_and_stops_before_any_request(self):
        for email in ("", EMAIL):
            with self.subTest(email=email):
                client = FakeClient(email=email, teams=[{"team_id": 42}, {"team_id": 43}])
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, "43", client, bound_team="42", bound_user=USER_HASH)
                self.assertEqual(error.exception.code, "identity_unavailable")
                self.assertEqual(client.calls, [])

    def test_blank_identity_without_a_valid_binding_stops_before_team_read(self):
        for bound in (None, "", 42, "abc", "4" * 33, True):
            with self.subTest(bound=bound):
                client = FakeClient(email="")
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team=bound, bound_user=USER_HASH,
                                     capsule_session=True)
                self.assertEqual(error.exception.code, "identity_unavailable")
                self.assertEqual(client.calls, ["/api/auth/me"])

    def test_valid_different_email_is_never_overridden_by_a_team_binding(self):
        client = FakeClient(email="other@example.com")
        with self.assertRaises(muse.MuseError) as error:
            muse.fetch_quota([COOKIE], EMAIL, PLAN, "42", client, bound_team="42", bound_user=USER_HASH)
        self.assertEqual(error.exception.code, "account_mismatch")
        self.assertEqual(client.calls, ["/api/auth/me"])

    def test_masked_email_is_a_different_account_on_purpose(self):
        # Deliberate: a syntactically valid email that differs stays account_mismatch,
        # even when the pinned web user matches. If Meta starts masking emails, this
        # is the line to revisit (a masked email would then need the pinned user).
        for masked in ("e***@example.com", "example@e***.com"):
            with self.subTest(masked=masked):
                client = FakeClient(email=masked)
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, "42", client, bound_team="42", bound_user=USER_HASH)
                self.assertEqual(error.exception.code, "account_mismatch")
                self.assertEqual(client.calls, ["/api/auth/me"])

    def test_team_bound_reading_still_requires_the_live_plan(self):
        for binding in ({"bound_user": USER_HASH}, {"capsule_session": True}):
            with self.subTest(binding=binding):
                client = FakeClient(email="", quota=dict(QUOTA, tier="Other"))
                with self.assertRaises(muse.MuseError) as error:
                    muse.fetch_quota([COOKIE], EMAIL, PLAN, client=client, bound_team="42", **binding)
                self.assertEqual(error.exception.code, "plan_mismatch")

    def test_email_verified_reading_reports_its_binding(self):
        observed = {}
        muse.fetch_quota([COOKIE], EMAIL, PLAN, client=FakeClient(), observed=observed)
        self.assertEqual(observed, {"mode": "email", "userHash": USER_HASH})

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
    def setUp(self):
        # Hermetic: no real Muse credential and no network, even if a patch below misses.
        for target, options in (("desktop_usage.muse_credentials", {"return_value": None}),
                                ("desktop_usage.request_json", {"side_effect": AssertionError("network")}),
                                ("muse_console.PortalClient.get", {"side_effect": AssertionError("network")})):
            patcher = patch(target, **options)
            patcher.start()
            self.addCleanup(patcher.stop)

    @patch("muse_console.read_capsule", return_value=([COOKIE], "42", EMAIL, PLAN))
    @patch("muse_console.quota_sample", return_value={"teamId": "42", "quota": QUOTA, "sampledAt": "2026-10-01T00:00:00Z", "cached": False, "message": None})
    @patch("desktop_usage.request_json", return_value={"user_email": EMAIL, "subs_tier_name": PLAN, "is_subs_active": True, "api_key": "sk-private"})
    def test_helper_fallback_preserves_identity_and_real_limits(self, request, quota, capsule):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential("dca:private"), result)
        self.assertEqual(result["status"], "ok")
        self.assertEqual([row["usedPercent"] for row in result["windows"]], [0, 6])
        self.assertIsNone(result["windows"][0]["resetAt"])
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(quota.call_args.args[2:], (EMAIL, PLAN, "42", "dca:private"))
        self.assertEqual(quota.call_args.kwargs, {"bound_team": "42"})

    @patch("muse_console.read_capsule", return_value=([COOKIE], "42", "other@example.com", PLAN))
    @patch("muse_console.quota_sample")
    @patch("desktop_usage.request_json", return_value={"user_email": EMAIL, "subs_tier_name": PLAN, "is_subs_active": True})
    def test_capsule_cannot_cross_accounts(self, request, quota, capsule):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential("dca:private"), result)
        quota.assert_not_called()
        self.assertEqual(result["status"], "unavailable")
        self.assertIn("differs", result["message"])

    @patch("muse_console.browser_reading", return_value=("42", {"email": EMAIL, "plan": PLAN, "windows": []}, True))
    @patch("muse_console.write_capsule")
    def test_combined_host_has_distinct_muse_action(self, writer, collector):
        response = host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], "teamId": None}, "/unused")
        self.assertTrue(response["ok"])
        self.assertEqual(response["teamId"], "42")
        writer.assert_called_once_with("/unused", [COOKIE], "42", EMAIL, PLAN)

    @patch("muse_console.browser_reading", return_value=("42", {"email": EMAIL, "plan": PLAN, "windows": []}, False))
    @patch("muse_console.write_capsule")
    def test_host_keeps_the_capsule_when_the_reading_did_not_verify_the_session(self, writer, collector):
        response = host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], "teamId": None}, "/unused")
        self.assertTrue(response["ok"])
        writer.assert_not_called()

    def test_native_action_rejects_unknown_paths_and_identity_claims(self):
        for extra in ({"url": "https://evil.example"}, {"email": EMAIL}, {"plan": PLAN}):
            with self.assertRaises(muse.MuseError):
                host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], **extra}, "/unused")

    @patch("muse_console.browser_reading", return_value=("42", {"email": EMAIL, "plan": PLAN, "windows": [], "status": "cached"}, True))
    @patch("muse_console.write_capsule")
    @patch("muse_console.restore_browser_sample", return_value=True)
    def test_distinct_normal_native_protocol_restores_before_cached_collection(self, restore, writer, collector):
        previous = {"teamId": "42", "sample": {"sampledAt": "2026-10-01T13:00:00Z"}}
        response = host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], "teamId": "42", "previousSample": previous}, "/unused")
        restore.assert_called_once_with("/unused", previous)
        self.assertEqual(response["sample"]["status"], "cached")
        self.assertEqual(collector.call_args.args, ([COOKIE], "42", "/unused"))

    @patch("muse_console.restore_browser_sample")
    def test_cached_sample_cannot_cross_the_explicit_native_protocol_team(self, restore):
        with self.assertRaises(muse.MuseError):
            host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [COOKIE], "teamId": "43", "previousSample": {"teamId": "42", "sample": {}}}, "/unused")
        restore.assert_not_called()



class EmailLessSyncTests(unittest.TestCase):
    """Blank portal email, end to end through the native host with real private files.

    The capsule's own session pins its web user on the first reading. After
    that, only that user's sessions are read, and only verified sessions reach
    the capsule. Fake portal: each session cookie value belongs to one user.
    """
    ACCESS = "dca:synthetic-private"
    USERS = {"synthetic-web-session": "user-a", "synthetic-rotated-session": "user-a",
             "synthetic-other-session": "user-b"}
    ROTATED = dict(COOKIE, value="synthetic-rotated-session")
    OTHER = dict(COOKIE, value="synthetic-other-session")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.root = self.home / ".ccs/account-usage"
        self.root.mkdir(parents=True, mode=0o700)
        muse.write_capsule(self.root, [COOKIE], "42", EMAIL, PLAN)
        self.clock, self.teams, self.email, self.calls = [1790863200.0], [42], "", []
        for target, options in (
                ("desktop_usage.muse_credentials", {"return_value": helpers.Credential(self.ACCESS, email=EMAIL)}),
                ("muse_console.verify_device_identity", {}),
                ("desktop_usage.request_json", {"return_value": {"user_email": EMAIL, "subs_tier_name": PLAN,
                                                                 "is_subs_active": True}}),
                ("muse_console.PortalClient.get", {"autospec": True, "side_effect": self.portal}),
                ("muse_console.time.time", {"side_effect": lambda: self.clock[0]})):
            patcher = patch(target, **options)
            patcher.start()
            self.addCleanup(patcher.stop)

    def portal(self, client, route):
        session = client.header.split("=", 1)[1]
        self.calls.append((session, route))
        if route == "/api/auth/me":
            me = {"email": self.email, "displayName": "Synthetic"}
            return dict(me, userId=self.USERS[session]) if self.USERS.get(session) else me
        if route == "/api/portal/teams":
            return {"teams": [{"team_id": team} for team in self.teams]}
        return {"subscription_quota": QUOTA}

    def dashboard(self):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential(self.ACCESS, email=EMAIL), result, self.home)
        return result

    def sync(self, cookie):
        return host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [cookie], "teamId": None},
                                   self.root)

    def capsule_session(self):
        return muse.read_capsule(self.root)[0][0]["value"]

    def later(self):
        self.clock[0] += muse.REFRESH_SECONDS + 1

    def test_first_reading_through_the_capsule_session_pins_its_user(self):
        result = self.dashboard()
        self.assertEqual(result["status"], "ok")
        pin = muse.read_binding(self.root)
        self.assertEqual((pin["teamId"], pin["userHash"]), ("42", muse.user_hash({"userId": "user-a"})))
        self.assertNotIn("user-a", (self.root / muse.BINDING_NAME).read_text())

    def test_without_a_pin_another_browser_session_is_refused_and_the_capsule_kept(self):
        before = (self.root / muse.CAPSULE_NAME).read_bytes()
        with self.assertRaises(muse.MuseError) as error:
            self.sync(self.ROTATED)
        self.assertEqual(error.exception.code, "identity_unavailable")
        self.assertEqual((self.root / muse.CAPSULE_NAME).read_bytes(), before)
        self.assertIsNone(muse.read_binding(self.root))

    def test_without_a_pin_the_capsule_session_must_list_one_team(self):
        self.teams = [42, 43]
        self.assertEqual(self.dashboard()["failureCode"], "identity_unavailable")
        self.assertIsNone(muse.read_binding(self.root))

    def test_pinned_users_rotated_session_replaces_the_capsule(self):
        self.dashboard()
        self.later()
        self.teams = [42, 43]  # Once pinned, membership in more teams is fine; only team 42 is read.
        response = self.sync(self.ROTATED)
        self.assertEqual((response["teamId"], response["sample"]["status"]), ("42", "ok"))
        self.assertEqual(self.capsule_session(), "synthetic-rotated-session")
        self.assertEqual(self.calls[-1], ("synthetic-rotated-session", "/api/portal/teams/42/subscription-quota"))

    def test_another_web_user_in_the_same_team_never_reaches_the_dashboard_or_the_capsule(self):
        self.dashboard()
        self.later()
        with self.assertRaises(muse.MuseError) as error:
            self.sync(self.OTHER)
        self.assertEqual(error.exception.code, "account_mismatch")
        self.assertEqual(self.capsule_session(), "synthetic-web-session")
        self.assertNotIn(("synthetic-other-session", "/api/portal/teams"), self.calls)
        self.assertIsNone(muse.read_usage_state(self.root)["quota"])
        self.later()
        self.assertEqual(self.dashboard()["status"], "ok")

    def test_an_unreadable_or_foreign_pin_closes_the_email_less_path(self):
        for content, mode in (("{not json", 0o600), ('{"schemaVersion": 1}', 0o600), (None, 0o644), ("foreign", 0o600)):
            with self.subTest(content=content, mode=mode):
                path = self.root / muse.BINDING_NAME
                path.unlink(missing_ok=True)
                (self.root / muse.CACHE_NAME).unlink(missing_ok=True)
                if content == "foreign":
                    muse.atomic_private_json(self.root, muse.BINDING_NAME, {"schemaVersion": 1, "email": EMAIL, "plan": PLAN,
                                             "teamId": "43", "userHash": muse.user_hash({"userId": "user-a"})})
                else:
                    path.write_text(content or json.dumps({"schemaVersion": 1, "email": EMAIL, "plan": PLAN, "teamId": "42",
                                                            "userHash": muse.user_hash({"userId": "user-a"})}))
                    os.chmod(path, mode)
                self.assertEqual(self.dashboard()["failureCode"], "identity_unavailable")

    def test_email_verified_popup_team_switch_still_works_and_moves_the_pin(self):
        self.email, self.teams = EMAIL, [42, 43]
        response = host.handle_request({"schemaVersion": 1, "action": "museSync", "cookies": [self.ROTATED],
                                        "teamId": "43"}, self.root)
        self.assertEqual((response["teamId"], response["sample"]["status"]), ("43", "ok"))
        self.assertEqual(muse.read_capsule(self.root)[1:], ("43", EMAIL, PLAN))
        self.assertEqual(muse.read_binding(self.root)["teamId"], "43")

    def test_a_cached_sync_replaces_the_capsule_only_under_a_pin(self):
        self.email = EMAIL
        self.USERS = {}  # An email-verified reading without a userId pins nothing.
        self.dashboard()
        self.assertIsNone(muse.read_binding(self.root))
        response = self.sync(self.ROTATED)
        self.assertEqual(response["sample"]["status"], "cached")
        self.assertEqual(self.capsule_session(), "synthetic-web-session")
        self.later()
        self.USERS = EmailLessSyncTests.USERS
        self.dashboard()  # Email-verified with a userId: pins user-a.
        self.assertIsNotNone(muse.read_binding(self.root))
        response = self.sync(self.ROTATED)
        self.assertEqual(response["sample"]["status"], "cached")
        self.assertEqual(self.capsule_session(), "synthetic-rotated-session")


class OfficialDisplayTests(unittest.TestCase):
    """The official dev.meta.ai/usage cards and their percent, against our windows.

    Source: subscriptionUsageDisplay(used, limit) in the first-party chunk
    https://dev.meta.ai/_next/static/chunks/06wr0f0d2-zni.js (SHA-256
    ceb8c6f5cad4b9e3837e3401cabe8500b547757ef8b3b9bad06bf889f4cefa94), called by
    the /usage page chunk 00zf9fj7en94j.js (SHA-256
    5f25b52f7920b85ada374eae2aa013c7941f37da9de29a6eaf9cf0776e1369e7) for its
    "Current usage" card (window_weighted_used / window_weighted_limit) and its
    "Weekly limit" card (weekly_weighted_used / weekly_weighted_limit). Both were
    captured unauthenticated on 2026-10-01 by the Codex owner thread.
    """

    @staticmethod
    def official(used, limit):
        # Transcribed rule: limit <= 0 is "100%"; used <= 0 is "0%"; else
        # floor(100 * used / limit) clamped to 0..100, and 0 shows as "<1%".
        used, limit = float(used), float(limit)
        if not limit > 0:
            return "100%"
        if not math.isfinite(used) or used <= 0:
            return "0%"
        percent = min(max(math.floor(100 * used / limit), 0), 100)
        return "<1%" if percent == 0 else str(percent) + "%"

    def test_our_labels_name_the_official_cards(self):
        windows = usage.normalize_muse({"subscription_quota": QUOTA})
        self.assertEqual([row["label"] for row in windows], ["Current usage (5-hour)", "Weekly limit"])

    def test_real_counters_differ_only_by_the_official_whole_percent_floor(self):
        # Saved provider readings: 2026-10-01T13:20Z and 2026-10-02T12:48Z (weekly, 300B limit).
        for used, ours, theirs in (("19020266200", 6.3401, "6%"), ("38146338720", 12.7154, "12%"),
                                   ("27000000000", 9, "9%"), ("1", 0, "<1%"), ("0", 0, "0%")):
            with self.subTest(used=used):
                quota = dict(QUOTA, weekly_weighted_used=used, weekly_weighted_limit="300000000000")
                weekly = usage.normalize_muse({"subscription_quota": quota})[1]
                self.assertEqual(weekly["usedPercent"], ours)
                self.assertEqual(self.official(used, "300000000000"), theirs)

    def test_ours_is_never_below_the_official_figure_and_at_most_one_point_above(self):
        # Ours keeps 4 decimals (the trays and web show up to 2); Meta floors to a whole percent.
        for used in range(26_999_000_000, 30_001_000_000, 99_999_937):
            weekly = usage.normalize_muse({"subscription_quota": dict(
                QUOTA, weekly_weighted_used=str(used), weekly_weighted_limit="300000000000")})[1]
            official = int(self.official(used, 300_000_000_000).rstrip("%"))
            self.assertLessEqual(official, weekly["usedPercent"])
            self.assertLessEqual(weekly["usedPercent"], official + 1)
        # The one edge: 4-decimal rounding can reach the next whole percent first.
        edge = usage.normalize_muse({"subscription_quota": dict(
            QUOTA, weekly_weighted_used="29999999999", weekly_weighted_limit="300000000000")})[1]
        self.assertEqual((edge["usedPercent"], self.official(29999999999, 300000000000)), (10, "9%"))

if __name__ == "__main__":
    unittest.main()
