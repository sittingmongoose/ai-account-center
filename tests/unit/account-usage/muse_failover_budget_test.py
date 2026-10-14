"""The failover adapter shares the existing Muse portal request budget."""
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/account-usage"))
import failover_additional as adapter
import failover_usage as dispatcher
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
        self.clock = int(time.time())
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
        self.clock += 1
        self.assertEqual(self.collect()["retryAfterSeconds"], 1199)
        self.assertEqual(self.native_client.get.call_count, 2)
        self.assertEqual(len(self.portal_calls), 3)

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
        self.assertEqual(limited, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 900,
                                   "observedPlan": "Pro"})
        self.clock += 1
        self.assertEqual(self.collect()["retryAfterSeconds"], 899)
        self.assertEqual(self.native_client.get.call_count, 1)
        self.assertEqual(self.portal_calls, [])

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
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error", "observedPlan": "Ultra"})
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
        self.assertEqual(failed["retryAfterSeconds"], 900)
        self.assertIsNone(failed["sample"])
        self.clock += 1
        repeated = dispatcher.run(request)
        self.assertEqual(repeated["failureCode"], "rate_limited")
        self.assertEqual(repeated["observedPlan"], "Ultra")
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
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error", "observedPlan": "Pro"})
        self.clock += 1
        self.assertEqual(self.collect(), failed)
        self.assertEqual(self.native_client.get.call_count, 2)


if __name__ == "__main__":
    unittest.main()
