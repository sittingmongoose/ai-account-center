"""The failover adapter shares the existing Muse portal request budget."""
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
import urllib.error
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/account-usage"))
import failover_additional as adapter
import failover_usage as dispatcher
import desktop_helpers as helpers
import desktop_usage as usage
import muse_console as muse
import plan_common as plans

EMAIL, TOKEN = "fixture@example.test", "dca:fixture-portal"
COOKIE = {"name": "llama_dev_sess", "value": "fixture-session", "domain": "dev.meta.ai",
          "path": "/", "secure": True, "hostOnly": True}


@unittest.skipUnless(os.name == "posix", "Muse's existing portal store is POSIX-only")
class MuseFailoverBudgetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.ccs = self.home / "scope"
        self.root = self.ccs / "account-usage"
        self.root.mkdir(parents=True, mode=0o700)
        auth = self.home / ".config/muse/auth.json"
        auth.parent.mkdir(parents=True)
        auth.write_text(json.dumps({"providers": {"meta": {"access_token": TOKEN, "user_email": EMAIL}}}))
        muse.write_capsule(self.root, [COOKIE], "42", EMAIL, "Pro")
        # Advance cooldowns without making fixture observations future-dated
        # to the dispatcher's independent real-clock output validation.
        self.clock = int(time.time()) - 3600
        self.native = {"user_email": EMAIL, "subs_tier_name": "Pro", "is_subs_active": True,
                       "api_key": "sk-do-not-persist"}
        self.quota = {"tier": "Pro", "window_weighted_used": 10, "window_weighted_limit": 100,
                      "as_of": self.clock}
        self.native_client = Mock()
        self.native_client.get.side_effect = lambda *args, **kwargs: dict(self.native)
        self.portal_calls, self.portal_error = [], None
        for patcher in (
            patch.dict(os.environ, {"XDG_CONFIG_HOME": str(self.home / ".config")}, clear=True),
            patch.object(plans, "HttpClient", return_value=self.native_client),
            patch.object(muse.PortalClient, "get", side_effect=self.portal),
            patch.object(muse.time, "time", side_effect=lambda: self.clock),
            patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("Live network forbidden")),
        ):
            patcher.start()
            self.addCleanup(patcher.stop)

    def portal(self, route):
        self.portal_calls.append(route)
        if route == "/api/auth/me":
            return {"email": EMAIL, "userId": "fixture-user"}
        if route == "/api/portal/teams":
            return {"teams": [{"team_id": 42}]}
        if self.portal_error:
            raise self.portal_error
        return {"subscription_quota": dict(self.quota)}

    def collect(self):
        candidate = adapter.inventory("muse", self.home, "ubuntu", ccs_dir=self.ccs)[0]
        return adapter.collect(candidate, self.home, "ubuntu", ccs_dir=self.ccs)

    def test_healthy_cache_never_mints_another_key_or_queries_portal(self):
        first = self.collect()
        self.assertEqual(first["sample"]["status"], "ok")
        self.assertEqual(first["refreshAfterSeconds"], 300)
        self.clock += 1
        cached = self.collect()
        self.assertEqual(cached["sample"]["status"], "cached")
        self.assertEqual(cached["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(cached["observedPlanAt"], first["observedPlanAt"])
        self.assertEqual(cached["refreshAfterSeconds"], 299)
        self.assertEqual(self.native_client.get.call_count, 1)
        self.assertEqual(len(self.portal_calls), 3)
        self.assertNotIn("sk-do-not-persist", (self.root / muse.CACHE_NAME).read_text())

    def test_portal_429_retains_sample_and_remaining_retry_without_more_requests(self):
        first = self.collect()
        self.clock += 301
        self.portal_error = muse.MuseError("rate_limited", retry_after=900)
        limited = self.collect()
        self.assertEqual(limited["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(limited["failureCode"], "rate_limited")
        self.assertEqual(limited["retryAfterSeconds"], 900)
        self.assertNotIn("refreshAfterSeconds", limited)
        self.clock += 1
        cached = self.collect()
        self.assertEqual(cached["retryAfterSeconds"], 899)
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 6)

    def test_identity_endpoint_429_uses_lazy_cache_and_never_queries_portal_again(self):
        first = self.collect()
        self.clock += 301
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="rate_limited", retry_after=1200)
        limited = self.collect()
        self.assertEqual(limited["failureCode"], "rate_limited")
        self.assertEqual(limited["retryAfterSeconds"], 1200)
        self.assertEqual(limited["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(limited["observedPlanAt"], first["observedPlanAt"])
        self.clock += 1
        cached = self.collect()
        self.assertEqual(cached["retryAfterSeconds"], 1199)
        self.assertEqual(cached["observedPlanAt"], first["observedPlanAt"])
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 3)

    def test_standalone_bridge_http429_shares_full_hold_with_failover_adapter(self):
        first = self.collect()
        self.clock += 301
        held_at = self.clock
        body = Mock()
        body.read.return_value = b"sk-private-response"
        error = urllib.error.HTTPError(usage.MUSE_URL, 429, "private upstream detail",
                                      {"Retry-After": "1800"}, body)
        self.addCleanup(error.close)
        opener = Mock()
        opener.open.side_effect = error
        with patch("desktop_helpers.urllib.request.build_opener", return_value=opener), \
                patch.object(usage, "muse_credentials", return_value=helpers.Credential(TOKEN, email=EMAIL)):
            _, legacy = muse.collect_browser([COOKIE], root=self.root)
            self.assertEqual(legacy["status"], "cached")
            self.assertEqual(legacy["sampledAt"], first["sample"]["sampledAt"])
            self.assertNotIn("failureCode", legacy)
            cached = self.collect()
            self.assertEqual(cached["failureCode"], "rate_limited")
            self.assertEqual(cached["retryAfterSeconds"], 1800)
            self.assertEqual(cached["sample"]["sampledAt"], first["sample"]["sampledAt"])
            self.assertEqual(cached["observedPlanAt"], first["observedPlanAt"])
            raw = json.loads((self.root / muse.CACHE_NAME).read_text())
            self.assertEqual(raw["schemaVersion"], 1)
            self.assertEqual(set(raw), {"schemaVersion", "email", "plan", "teamId", "credentialFingerprint",
                                        "quota", "sampledAt", "nextRequestAt", "lastError"})
            self.assertEqual(raw["nextRequestAt"], held_at + 1800)
            self.assertEqual(raw["lastError"], "rate_limited")
            self.clock += 1799
            _, still_cached = muse.collect_browser([COOKIE], root=self.root)
            remaining = self.collect()
            self.assertEqual(remaining["retryAfterSeconds"], 1)
            self.assertEqual(still_cached["sampledAt"], first["sample"]["sampledAt"])
            self.assertEqual(remaining["observedPlanAt"], first["observedPlanAt"])
            self.assertEqual(opener.open.call_count, 1)
            self.assertEqual(self.native_client.get.call_count, 1)
            self.assertEqual(len(self.portal_calls), 3)
            body.read.assert_not_called()
            rendered = json.dumps([legacy, cached, raw, remaining])
            for secret in (TOKEN, "sk-private-response", "private upstream detail"):
                self.assertNotIn(secret, rendered)

    def test_cold_bridge_http429_uses_conservative_default_without_claiming_plan(self):
        for header in (None, "malformed-private-value"):
            with self.subTest(header=header):
                for name in (muse.CACHE_NAME, muse.PLAN_NAME):
                    (self.root / name).unlink(missing_ok=True)
                body = Mock()
                error = urllib.error.HTTPError(usage.MUSE_URL, 429, "private", {"Retry-After": header}, body)
                self.addCleanup(error.close)
                opener = Mock()
                opener.open.side_effect = error
                with patch("desktop_helpers.urllib.request.build_opener", return_value=opener), \
                        patch.object(usage, "muse_credentials", return_value=helpers.Credential(TOKEN, email=EMAIL)):
                    with self.assertRaises(muse.MuseError) as caught:
                        muse.collect_browser([COOKIE], root=self.root)
                    self.assertEqual((caught.exception.code, caught.exception.retry_after), ("rate_limited", 600))
                    self.assertIsNone(caught.exception.plan)
                    self.assertEqual(self.collect(), {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 600})
                    self.clock += 1
                    with self.assertRaises(muse.MuseError) as cached:
                        muse.collect_browser([COOKIE], root=self.root)
                    self.assertEqual(cached.exception.retry_after, 599)
                    self.assertEqual(opener.open.call_count, 1)
                    body.read.assert_not_called()
        self.native_client.get.assert_not_called()
        self.assertEqual(self.portal_calls, [])

    def test_initial_browser_native_request_reports_structured_429(self):
        (self.root / muse.CAPSULE_NAME).unlink()
        body = Mock()
        error = urllib.error.HTTPError(usage.MUSE_URL, 429, "private", {"Retry-After": "1800"}, body)
        self.addCleanup(error.close)
        opener = Mock()
        opener.open.side_effect = error
        with patch("desktop_helpers.urllib.request.build_opener", return_value=opener), \
                patch.object(usage, "muse_credentials", return_value=helpers.Credential(TOKEN, email=EMAIL)):
            with self.assertRaises(muse.MuseError) as caught:
                muse.collect_browser([COOKIE], root=self.root)
        self.assertEqual((caught.exception.code, caught.exception.retry_after), ("rate_limited", 1800))
        self.assertIsNone(caught.exception.plan)
        self.assertFalse((self.root / muse.CACHE_NAME).exists())
        self.assertEqual(self.portal_calls, [])
        body.read.assert_not_called()

    def test_network_failure_retains_sample_but_allows_coordinator_peer_recovery(self):
        first = self.collect()
        self.clock += 301
        self.portal_error = muse.MuseError("network_error")
        failed = self.collect()
        self.assertEqual(failed["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(failed["failureCode"], "network_error")
        self.assertNotIn("refreshAfterSeconds", failed)
        self.assertNotIn("retryAfterSeconds", failed)
        self.clock += 1
        self.assertEqual(self.collect()["failureCode"], "network_error")
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 6)

    def test_first_identity_failure_reserves_and_preserves_429_without_a_sample(self):
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="rate_limited", retry_after=900)
        limited = self.collect()
        self.assertEqual(limited, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 900})
        self.clock += 1
        self.assertEqual(self.collect(), {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 899})
        self.assertEqual(self.native_client.get.call_count, 1)
        self.assertEqual(self.portal_calls, [])

    def test_stale_capsule_plan_is_unverified_on_cold_identity_failure_and_retry(self):
        self.native["subs_tier_name"] = "Ultra"
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="network_error")
        failed = self.collect()
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error"})
        self.assertFalse(muse.read_usage_state(self.root)["planVerified"])
        self.clock += 1
        self.assertEqual(self.collect(), failed)
        self.assertEqual(self.native_client.get.call_count, 1)
        self.assertEqual(self.portal_calls, [])
        # Only an eventual successful native lookup can promote the tier.
        self.clock += 300
        self.native_client.get.side_effect = lambda *args, **kwargs: dict(self.native)
        self.portal_error = muse.MuseError("network_error")
        verified = self.collect()
        self.assertEqual(verified, {"sample": None, "failureCode": "network_error", "observedPlan": "Ultra",
                                    "observedPlanAt": muse.plan_timestamp(self.clock)})
        self.assertTrue(muse.read_usage_state(self.root)["planVerified"])
        self.clock += 1
        self.assertEqual(self.collect(), verified)
        self.assertEqual(self.native_client.get.call_count, 2)

    def test_token_rotation_cannot_promote_stale_capsule_plan_after_identity_failure(self):
        self.native["subs_tier_name"] = "Ultra"
        self.quota["tier"] = "Ultra"
        self.assertEqual(self.collect()["sample"]["plan"], "Ultra")
        auth = self.home / ".config/muse/auth.json"
        auth.write_text(json.dumps({"providers": {"meta": {"access_token": "dca:rotated-fixture", "user_email": EMAIL}}}))
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="network_error")
        self.clock += 1
        failed = self.collect()
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error"})
        state = muse.read_usage_state(self.root)
        self.assertEqual(state["plan"], "Pro")
        self.assertFalse(state["planVerified"])
        self.clock += 1
        self.assertEqual(self.collect(), failed)
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 3)

    def test_legacy_empty_reservation_cannot_claim_verified_plan(self):
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="network_error")
        self.collect()
        state = muse.read_usage_state(self.root)
        state.pop("planVerified")
        state.pop("planVerifiedAt")
        state.pop("restoreAllowed")
        state["schemaVersion"] = 1
        muse.atomic_private_json(self.root, muse.CACHE_NAME, state)
        self.clock += 1
        self.assertEqual(self.collect(), {"sample": None, "failureCode": "network_error"})
        self.assertFalse(muse.read_usage_state(self.root)["planVerified"])
        self.assertEqual(self.native_client.get.call_count, 1)

    def test_legacy_successful_quota_retains_verified_plan_provenance(self):
        first = self.collect()
        state = muse.read_usage_state(self.root)
        state.pop("planVerified")
        state.pop("planVerifiedAt")
        state.pop("restoreAllowed")
        state["schemaVersion"] = 1
        muse.atomic_private_json(self.root, muse.CACHE_NAME, state)
        (self.root / muse.PLAN_NAME).unlink()
        self.assertTrue(muse.read_usage_state(self.root)["planVerified"])
        self.assertIsNone(muse.read_usage_state(self.root)["planVerifiedAt"])
        self.clock += 301
        self.native_client.get.side_effect = plans.CollectionError("error", "private", failure_code="network_error")
        failed = self.collect()
        self.assertEqual(failed["failureCode"], "network_error")
        self.assertEqual(failed["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertTrue(muse.read_usage_state(self.root)["planVerified"])
        self.assertNotIn("observedPlanAt", failed)

    def test_new_usage_cache_remains_readable_by_exact_legacy_schema(self):
        def legacy_read():
            # Frozen predicate from the deployed reader, independent of the
            # current helper's constants and sidecar implementation.
            value = json.loads((self.root / "muse-console-usage.json").read_text())
            fields = {"schemaVersion", "email", "plan", "teamId", "credentialFingerprint",
                      "quota", "sampledAt", "nextRequestAt", "lastError"}
            if (not isinstance(value, dict) or set(value) != fields or
                    type(value["schemaVersion"]) is not int or value["schemaVersion"] != 1):
                raise ValueError("local_storage_error")
            return value
        first = self.collect()
        self.assertEqual(legacy_read()["sampledAt"], first["sample"]["sampledAt"])
        self.clock += 301
        self.portal_error = muse.MuseError("rate_limited", retry_after=900)
        limited = self.collect()
        legacy = legacy_read()
        self.assertEqual(legacy["lastError"], "rate_limited")
        self.assertEqual(legacy["nextRequestAt"], self.clock + 900)
        self.assertEqual(legacy["sampledAt"], limited["sample"]["sampledAt"])
        self.assertEqual(legacy["quota"]["tier"], "Pro")
        self.assertTrue((self.root / muse.PLAN_NAME).exists())

    def test_older_writer_changing_cache_invalidates_sidecar_provenance(self):
        self.native["subs_tier_name"] = "Ultra"
        self.quota["tier"] = "Ultra"
        self.collect()
        state = json.loads((self.root / muse.CACHE_NAME).read_text())
        state.update(plan="Pro", quota=None, sampledAt=None, lastError="network_error",
                     nextRequestAt=self.clock + 600)
        # An old helper has no knowledge of the sidecar and only replaces v1.
        muse.atomic_private_json(self.root, muse.CACHE_NAME, state)
        self.clock += 1
        self.assertEqual(self.collect(), {"sample": None, "failureCode": "network_error"})
        self.assertFalse(muse.read_usage_state(self.root)["planVerified"])
        self.assertIsNone(muse.read_usage_state(self.root)["planVerifiedAt"])
        self.assertEqual(self.native_client.get.call_count, 1)

    def test_new_plan_uses_verified_cache_tier_without_repeating_identity_lookup(self):
        self.collect()
        self.clock += 301
        self.native["subs_tier_name"] = "Ultra"
        self.quota.update(tier="Ultra", window_weighted_limit=200)
        fresh = self.collect()
        self.assertEqual(fresh["sample"]["plan"], "Ultra")
        self.clock += 1
        cached = self.collect()
        self.assertEqual(cached["sample"]["plan"], "Ultra")
        self.assertEqual(cached["sample"]["sampledAt"], fresh["sample"]["sampledAt"])
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 6)

    def test_failed_upgraded_plan_cannot_retain_previous_plan_quota(self):
        self.collect()
        self.clock += 301
        self.native["subs_tier_name"] = "Ultra"
        self.portal_error = muse.MuseError("network_error")
        failed = self.collect()
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error", "observedPlan": "Ultra",
                                  "observedPlanAt": muse.plan_timestamp(self.clock)})
        self.clock += 1
        self.assertEqual(self.collect(), failed)
        self.assertEqual(self.native_client.get.call_count, 2)

    def test_plan_invalidation_and_429_both_survive_dispatcher(self):
        self.collect()
        self.clock += 301
        self.native["subs_tier_name"] = "Ultra"
        self.portal_error = muse.MuseError("rate_limited", retry_after=900)
        candidate = adapter.inventory("muse", self.home, "ubuntu", ccs_dir=self.ccs)[0]
        request = {"version": 1, "action": "collect", "provider": "muse", "homeDir": str(self.home),
                   "ccsDir": str(self.ccs), "candidate": {key: value for key, value in candidate.items() if not key.startswith("_")}}
        failed = dispatcher.run(request)
        self.assertEqual(failed["failureCode"], "rate_limited")
        self.assertEqual(failed["observedPlan"], "Ultra")
        self.assertEqual(failed["observedPlanAt"], muse.plan_timestamp(self.clock))
        self.assertEqual(failed["retryAfterSeconds"], 900)
        self.assertIsNone(failed["sample"])
        self.clock += 1
        repeated = dispatcher.run(request)
        self.assertEqual(repeated["failureCode"], "rate_limited")
        self.assertEqual(repeated["observedPlan"], "Ultra")
        self.assertEqual(repeated["observedPlanAt"], failed["observedPlanAt"])
        self.assertEqual(repeated["retryAfterSeconds"], 899)
        self.assertEqual(self.native_client.get.call_count, 2)

    def test_successful_upgrade_keeps_its_quota_on_later_network_failure(self):
        self.native["subs_tier_name"] = "Ultra"
        self.quota["tier"] = "Ultra"
        upgraded = self.collect()
        self.clock += 301
        self.portal_error = muse.MuseError("network_error")
        failed = self.collect()
        self.assertEqual(failed["failureCode"], "network_error")
        self.assertEqual(failed["sample"]["plan"], "Ultra")
        self.assertEqual(failed["sample"]["sampledAt"], upgraded["sample"]["sampledAt"])

    def test_failed_downgrade_to_original_capsule_tier_invalidates_upgraded_quota(self):
        self.native["subs_tier_name"] = "Ultra"
        self.quota["tier"] = "Ultra"
        self.collect()
        self.clock += 301
        self.native["subs_tier_name"] = "Pro"
        self.portal_error = muse.MuseError("network_error")
        failed = self.collect()
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error", "observedPlan": "Pro",
                                  "observedPlanAt": muse.plan_timestamp(self.clock)})
        self.clock += 1
        self.assertEqual(self.collect(), failed)
        self.assertEqual(self.native_client.get.call_count, 2)


if __name__ == "__main__":
    unittest.main()
