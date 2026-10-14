"""Native Muse key minting is bounded across independent callers, without live I/O."""
from concurrent.futures import ThreadPoolExecutor
import datetime as dt
import importlib
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/account-usage"))
import desktop_helpers
import muse_native
from plan_common import CollectionError

EMAIL, TOKEN = "fixture@example.test", "dca:fixture-native"


def reading(percent=10):
    value = desktop_helpers.account("muse", "ubuntu")
    stamp = dt.datetime.now(dt.timezone.utc).isoformat()
    value.update(status="ok", email=EMAIL, plan="Pro", fetchedAt=stamp, sampledAt=stamp,
                 windows=[{"key": "weekly", "label": "Weekly", "usedPercent": percent,
                           "remainingPercent": 100 - percent, "unit": "tokens"}])
    return value


class NativeMuseBudgetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "account-usage"

    def collect(self, loader, now=1000, token=TOKEN, email=EMAIL):
        return muse_native.collect(self.root, token, email, "ubuntu", loader, now=now)

    def test_reserves_before_key_mint_and_reuses_real_sample_after_restart(self):
        def load():
            state = json.loads(next(self.root.glob("muse-native-*.json")).read_text())
            self.assertEqual(state["nextRequestAt"], 1300)
            self.assertIsNone(state["sample"])
            return {"sample": reading(), "failureCode": None}
        loader = Mock(side_effect=load)
        first = self.collect(loader)
        self.assertEqual(first["refreshAfterSeconds"], 300)
        self.assertEqual(first["sample"]["status"], "ok")
        importlib.reload(muse_native)
        second = self.collect(loader, now=1010)
        self.assertEqual(loader.call_count, 1)
        self.assertEqual(second["sample"]["status"], "cached")
        self.assertEqual(second["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(second["observedPlanAt"], first["observedPlanAt"])
        self.assertEqual(second["refreshAfterSeconds"], 290)

    def test_cached_quota_preserves_429_deadline_and_prevents_more_key_mints(self):
        first = self.collect(lambda: {"sample": reading(), "failureCode": None})
        loader = Mock(side_effect=CollectionError("error", "private", failure_code="rate_limited", retry_after=900))
        limited = self.collect(loader, now=1301)
        self.assertEqual(limited["failureCode"], "rate_limited")
        self.assertEqual(limited["retryAfterSeconds"], 900)
        self.assertEqual(limited["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertEqual(limited["observedPlanAt"], first["observedPlanAt"])
        again = self.collect(loader, now=1401)
        self.assertEqual(loader.call_count, 1)
        self.assertEqual(again["retryAfterSeconds"], 800)
        self.assertEqual(again["observedPlanAt"], first["observedPlanAt"])
        self.assertNotIn("refreshAfterSeconds", again)

    def test_network_failure_retains_dated_quota_without_hiding_peer_recovery(self):
        first = self.collect(lambda: {"sample": reading(), "failureCode": None})
        loader = Mock(side_effect=CollectionError("error", "private", failure_code="network_error"))
        failed = self.collect(loader, now=1301)
        self.assertEqual(failed["failureCode"], "network_error")
        self.assertEqual(failed["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertNotIn("refreshAfterSeconds", failed)
        self.assertNotIn("retryAfterSeconds", failed)
        self.collect(loader, now=1302)
        self.assertEqual(loader.call_count, 1)

    def test_hard_identity_failure_clears_native_cache(self):
        self.collect(lambda: {"sample": reading(), "failureCode": None})
        failed = self.collect(lambda: {"sample": None, "failureCode": "account_mismatch"}, now=1301)
        self.assertEqual(failed, {"sample": None, "failureCode": "account_mismatch", "observedPlan": "Pro",
                                  "observedPlanAt": muse_native.plan_timestamp(1000)})
        loader = Mock()
        self.assertEqual(self.collect(loader, now=1302), failed)
        loader.assert_not_called()

    def test_verified_plan_change_without_native_quota_invalidates_old_limits(self):
        self.collect(lambda: {"sample": reading(), "failureCode": None})
        failed = self.collect(lambda: {"sample": None, "failureCode": "no_browser_cookie", "_verifiedPlan": "Ultra"}, now=1301)
        self.assertEqual(failed, {"sample": None, "failureCode": "no_browser_cookie", "observedPlan": "Ultra",
                                  "observedPlanAt": muse_native.plan_timestamp(1301)})
        loader = Mock()
        self.assertEqual(self.collect(loader, now=1302), failed)
        loader.assert_not_called()

    def test_cold_identity_failure_never_invents_plan_on_cached_retry(self):
        loader = Mock(side_effect=CollectionError("error", "private", failure_code="rate_limited", retry_after=900))
        failed = self.collect(loader)
        self.assertEqual(failed, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 900})
        importlib.reload(muse_native)
        self.assertEqual(self.collect(loader, now=1001),
                         {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 899})
        self.assertEqual(loader.call_count, 1)
        state = json.loads(next(self.root.glob("muse-native-*.json")).read_text())
        self.assertIsNone(state["observedPlan"])

    def test_rotated_credential_does_not_reuse_old_verified_plan(self):
        original = reading()
        original["plan"] = "Ultra"
        self.collect(lambda: {"sample": original, "failureCode": None})
        loader = Mock(side_effect=CollectionError("error", "private", failure_code="network_error"))
        failed = self.collect(loader, now=1001, token="dca:rotated-native")
        self.assertEqual(failed, {"sample": None, "failureCode": "network_error"})
        self.assertEqual(self.collect(loader, now=1002, token="dca:rotated-native"), failed)
        self.assertEqual(loader.call_count, 1)

    def test_older_native_cache_does_not_invent_a_verification_time(self):
        first = self.collect(lambda: {"sample": reading(), "failureCode": None})
        file = next(self.root.glob("muse-native-*.json"))
        state = json.loads(file.read_text())
        state.update(version=1)
        state.pop("observedPlanAt")
        file.write_text(json.dumps(state))
        loader = Mock(side_effect=CollectionError("error", "private", failure_code="network_error"))
        cached = self.collect(loader, now=1001)
        self.assertEqual(cached["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertNotIn("observedPlan", cached)
        self.assertNotIn("observedPlanAt", cached)
        loader.assert_not_called()
        failed = self.collect(loader, now=1301)
        self.assertEqual(failed["sample"]["sampledAt"], first["sample"]["sampledAt"])
        self.assertNotIn("observedPlan", failed)
        self.assertNotIn("observedPlanAt", failed)

    def test_same_email_other_device_credential_has_an_independent_cache(self):
        first = self.collect(lambda: {"sample": reading(10), "failureCode": None})
        second = self.collect(lambda: {"sample": reading(80), "failureCode": None}, token="dca:other-native")
        self.assertEqual(first["sample"]["windows"][0]["usedPercent"], 10)
        self.assertEqual(second["sample"]["windows"][0]["usedPercent"], 80)
        self.assertEqual(len(list(self.root.glob("muse-native-*.json"))), 2)

    def test_current_email_conflict_cannot_read_a_previous_native_sample(self):
        self.collect(lambda: {"sample": reading(), "failureCode": None})
        loader = Mock()
        self.assertEqual(self.collect(loader, email="other@example.test"),
                         {"sample": None, "failureCode": "account_mismatch", "observedPlan": "Pro",
                          "observedPlanAt": muse_native.plan_timestamp(1000)})
        loader.assert_not_called()

    def test_raw_inference_keys_and_response_fields_are_never_persisted(self):
        value = reading()
        value.update(api_key="sk-private-response", rawResponse="private-body")
        result = self.collect(lambda: {"sample": value, "failureCode": None, "raw": TOKEN})
        persisted = "".join(file.read_text() for file in self.root.glob("*.json"))
        for secret in (TOKEN, "sk-private-response", "private-body", "rawResponse"):
            self.assertNotIn(secret, persisted)
            self.assertNotIn(secret, json.dumps(result))

    def test_parallel_callers_share_one_native_request(self):
        def load():
            time.sleep(.1)
            return {"sample": reading(), "failureCode": None}
        loader = Mock(side_effect=load)
        with ThreadPoolExecutor(max_workers=3) as pool:
            values = list(pool.map(lambda _: self.collect(loader), range(3)))
        self.assertEqual(loader.call_count, 1)
        self.assertEqual(sum(row["sample"]["status"] == "ok" for row in values), 1)
        self.assertTrue(all(row["refreshAfterSeconds"] == 300 for row in values))

    def test_linked_native_state_is_not_read_or_replaced(self):
        self.collect(lambda: {"sample": reading(), "failureCode": None})
        file = next(self.root.glob("muse-native-*.json"))
        os.link(file, self.root / "linked.json")
        loader = Mock()
        self.assertEqual(self.collect(loader, now=1400), {"sample": None, "failureCode": "local_storage_error"})
        loader.assert_not_called()


if __name__ == "__main__":
    unittest.main()
