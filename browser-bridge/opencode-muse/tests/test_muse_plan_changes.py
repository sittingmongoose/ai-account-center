"""Plan changes preserve account identity, but never the previous plan's quota."""
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/account-usage"))
import desktop_helpers as helpers
import desktop_usage as usage
import muse_console as muse

EMAIL, OLD_PLAN, NEW_PLAN = "example@example.com", "Muse Code High Usage", "Muse Code Standard"
COOKIE = {"name": "llama_dev_sess", "value": "synthetic-session", "domain": "dev.meta.ai",
          "path": "/", "secure": True, "hostOnly": True}
ACCESS = "dca:synthetic-private"


class PlanChangeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.root = self.home / ".ccs/account-usage"
        self.root.mkdir(parents=True, mode=0o700)
        muse.write_capsule(self.root, [COOKIE], "42", EMAIL, OLD_PLAN)
        self.clock = 1790863200
        self.native = {"user_email": EMAIL, "subs_tier_name": OLD_PLAN, "is_subs_active": True}
        self.quota = {"tier": OLD_PLAN, "window_weighted_used": "100", "window_weighted_limit": "1000",
                      "window_duration_secs": 18000, "weekly_weighted_used": "600", "weekly_weighted_limit": "2000",
                      "as_of": self.clock}
        self.web_email, self.user, self.teams = "", "synthetic-user", [42]
        self.portal_error = None
        self.calls = []
        for target, options in (
                ("desktop_usage.muse_credentials", {"return_value": helpers.Credential(ACCESS, email=EMAIL)}),
                ("desktop_usage.request_json", {"side_effect": lambda *args: dict(self.native)}),
                ("muse_console.PortalClient.get", {"side_effect": self.portal}),
                ("muse_console.time.time", {"side_effect": lambda: self.clock})):
            patcher = patch(target, **options)
            mocked = patcher.start()
            self.addCleanup(patcher.stop)
            if target == "desktop_usage.request_json":
                self.native_request = mocked

    def portal(self, route):
        self.calls.append(route)
        if route == "/api/auth/me":
            return {"email": self.web_email, "userId": self.user}
        if route == "/api/portal/teams":
            return {"teams": [{"team_id": team} for team in self.teams]}
        if self.portal_error:
            raise self.portal_error
        return {"subscription_quota": dict(self.quota)}

    def collect(self, local_email=EMAIL):
        result = helpers.account("muse", "mac")
        usage.fetch_muse(helpers.Credential(ACCESS, email=local_email), result, self.home)
        return result

    def change_plan(self, plan=NEW_PLAN, limit="400"):
        self.clock += muse.REFRESH_SECONDS + 1
        self.native["subs_tier_name"] = plan
        self.quota.update(tier=plan, window_weighted_limit=limit, weekly_weighted_limit=limit, as_of=self.clock)

    def test_collector_detects_downgrade_and_upgrade_without_browser_resync(self):
        self.assertEqual(self.collect()["windows"][0]["usedPercent"], 10)
        original_pin = muse.read_binding(self.root)["userHash"]
        # After a user has been pinned, membership in another team cannot move usage.
        self.teams = [42, 43]
        for plan, limit, percentage in ((NEW_PLAN, "400", 25), (OLD_PLAN, "2000", 5)):
            with self.subTest(plan=plan):
                self.change_plan(plan, limit)
                sample = self.collect()
                self.assertEqual((sample["status"], sample["plan"]), ("ok", plan))
                self.assertEqual(sample["windows"][0]["usedPercent"], percentage)
                self.assertEqual(sample["windows"][0]["limit"], int(limit))
                self.assertEqual(muse.read_usage_state(self.root)["plan"], plan)
                self.assertEqual(muse.read_binding(self.root)["userHash"], original_pin)
                self.assertEqual(muse.read_binding(self.root)["plan"], plan)
                requests = self.native_request.call_count
                calls = list(self.calls)
                # The capsule still has its last synced tier. A cold caller
                # uses the new cache's tier and timestamp, without more I/O.
                cached = self.collect()
                self.assertEqual((cached["status"], cached["plan"]), ("cached", plan))
                self.assertEqual(cached["sampledAt"], sample["sampledAt"])
                self.assertEqual(cached["windows"], sample["windows"])
                self.assertEqual(self.native_request.call_count, requests)
                self.assertEqual(self.calls, calls)
        self.assertNotIn("/api/portal/teams/43/subscription-quota", self.calls)

    def test_first_capsule_read_can_detect_a_changed_plan_and_pin_the_same_session(self):
        self.change_plan()
        sample = self.collect()
        self.assertEqual((sample["status"], sample["plan"]), ("ok", NEW_PLAN))
        self.assertEqual(muse.read_binding(self.root)["plan"], NEW_PLAN)

    def test_browser_sync_updates_the_plan_and_saved_capsule(self):
        self.collect()
        self.change_plan()
        team, sample = muse.sync_browser([COOKIE], root=self.root)
        self.assertEqual((team, sample["status"], sample["plan"]), ("42", "ok", NEW_PLAN))
        self.assertEqual(muse.read_capsule(self.root)[1:], ("42", EMAIL, NEW_PLAN))
        self.assertEqual(sample["windows"][0]["usedPercent"], 25)

    def test_live_identity_can_recover_when_the_local_credential_has_no_email(self):
        self.collect()
        self.change_plan()
        sample = self.collect(local_email=None)
        self.assertEqual((sample["status"], sample["plan"]), ("ok", NEW_PLAN))
        self.assertEqual(sample["windows"][0]["usedPercent"], 25)

    def test_new_plan_detected_during_an_old_plan_429_clears_old_quota_without_bypassing_cooldown(self):
        self.collect()
        self.clock += muse.REFRESH_SECONDS + 1
        self.portal_error = muse.MuseError("rate_limited", retry_after=900)
        self.assertEqual(self.collect()["status"], "cached")
        cooldown = muse.read_usage_state(self.root)["nextRequestAt"]
        self.native["subs_tier_name"] = NEW_PLAN
        calls = list(self.calls)
        # Without a local email, the collector has already fetched native
        # identity before reaching the shared portal cooldown.
        changed = self.collect(local_email=None)
        self.assertEqual((changed["status"], changed["plan"]), ("unavailable", NEW_PLAN))
        self.assertEqual(changed["failureCode"], "rate_limited")
        state = muse.read_usage_state(self.root)
        self.assertEqual((state["plan"], state["nextRequestAt"]), (NEW_PLAN, cooldown))
        self.assertIsNone(state["quota"])
        self.assertIsNone(state["sampledAt"])
        requests = self.native_request.call_count
        cold = self.collect()
        self.assertEqual((cold["status"], cold["plan"]), ("unavailable", NEW_PLAN))
        self.assertEqual(cold["windows"], [])
        self.assertEqual(self.native_request.call_count, requests)
        self.assertEqual(self.calls, calls)

    def test_plan_change_drops_old_quota_on_transient_failure_and_obeys_cold_cooldown(self):
        for code in ("rate_limited", "network_error", "provider_error"):
            with self.subTest(code=code):
                self.portal_error = None
                self.change_plan(OLD_PLAN, "1000")
                self.assertEqual(self.collect()["status"], "ok")
                self.change_plan()
                self.portal_error = muse.MuseError(code, retry_after=900)
                sample = self.collect()
                self.assertEqual((sample["status"], sample["failureCode"]), ("unavailable", code))
                self.assertEqual(sample["plan"], NEW_PLAN)
                self.assertEqual(sample["windows"], [])
                self.assertIsNone(sample["sampledAt"])
                state = muse.read_usage_state(self.root)
                self.assertEqual(state["plan"], NEW_PLAN)
                self.assertIsNone(state["quota"])
                before = (self.root / muse.CACHE_NAME).read_bytes()
                requests, calls = self.native_request.call_count, list(self.calls)
                cached = self.collect()
                self.assertEqual(cached["failureCode"], code)
                self.assertEqual(cached["plan"], NEW_PLAN)
                self.assertEqual(cached["windows"], [])
                self.assertEqual((self.root / muse.CACHE_NAME).read_bytes(), before)
                self.assertEqual((self.native_request.call_count, self.calls), (requests, calls))
                self.clock = state["nextRequestAt"]
                self.portal_error = None
                self.assertEqual(self.collect()["plan"], NEW_PLAN)

    def test_plan_transition_cannot_change_web_user_team_or_portal_tier(self):
        for mutation, code in ((lambda: setattr(self, "user", "other-user"), "account_mismatch"),
                               (lambda: setattr(self, "teams", [43]), "team_mismatch"),
                               (lambda: self.quota.update(tier=OLD_PLAN), "plan_mismatch")):
            with self.subTest(code=code):
                self.user, self.teams = "synthetic-user", [42]
                self.change_plan(OLD_PLAN, "1000")
                self.assertEqual(self.collect()["status"], "ok")
                self.change_plan()
                mutation()
                sample = self.collect()
                self.assertEqual(sample["failureCode"], code)
                self.assertEqual(sample["windows"], [])
                self.assertIsNone(muse.read_usage_state(self.root)["quota"])
                self.assertEqual(muse.read_binding(self.root)["userHash"], muse.user_hash({"userId": "synthetic-user"}))

    def test_unexpected_browser_failure_still_reports_the_changed_plan(self):
        self.collect()
        self.change_plan()
        self.portal_error = RuntimeError("synthetic private upstream error")
        with self.assertRaises(muse.MuseError) as raised:
            muse.sync_browser([COOKIE], root=self.root)
        self.assertEqual((raised.exception.code, raised.exception.plan), ("error", NEW_PLAN))
        state = muse.read_usage_state(self.root)
        self.assertEqual(state["plan"], NEW_PLAN)
        self.assertIsNone(state["quota"])
        cold = self.collect()
        self.assertEqual((cold["failureCode"], cold["plan"], cold["windows"]), ("error", NEW_PLAN, []))
        self.assertNotIn("private", str(raised.exception))

    def test_capsule_save_failure_still_reports_the_changed_plan(self):
        self.collect()
        self.change_plan()
        for failure in (muse.MuseError("local_storage_error"), OSError("synthetic private path")):
            with self.subTest(failure=type(failure).__name__), \
                    patch.object(muse, "write_capsule", side_effect=failure), \
                    self.assertRaises(muse.MuseError) as raised:
                muse.sync_browser([COOKIE], root=self.root)
            self.assertEqual((raised.exception.code, raised.exception.plan), ("local_storage_error", NEW_PLAN))
            self.assertEqual(muse.read_usage_state(self.root)["plan"], NEW_PLAN)
            self.assertEqual(muse.read_capsule(self.root)[3], OLD_PLAN)
            self.assertNotIn("private", str(raised.exception))

    def test_invalid_or_inactive_native_subscription_never_inherits_usage(self):
        self.collect()
        for payload, code in (({"subs_tier_name": None}, "invalid_response"),
                              ({"is_subs_active": False}, "inactive_subscription"),
                              ({"user_email": "other@example.com"}, "account_mismatch")):
            with self.subTest(code=code):
                self.native = {"user_email": EMAIL, "subs_tier_name": NEW_PLAN, "is_subs_active": True, **payload}
                self.clock += muse.REFRESH_SECONDS + 1
                calls = list(self.calls)
                sample = self.collect()
                self.assertEqual(sample["failureCode"], code)
                self.assertEqual(sample["windows"], [])
                self.assertEqual(self.calls, calls)

    def test_previous_browser_sample_cannot_block_sync_after_a_plan_change(self):
        team, previous = muse.sync_browser([COOKIE], root=self.root)
        self.change_plan()
        muse.sync_browser([COOKIE], root=self.root)
        self.assertFalse(muse.restore_browser_sample(self.root, {"teamId": team, "sample": previous}, now=self.clock))
        _, sample = muse.sync_browser([COOKIE], root=self.root)
        self.assertEqual((sample["status"], sample["plan"]), ("cached", NEW_PLAN))


if __name__ == "__main__":
    unittest.main()
