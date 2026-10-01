"""Independent dashboard/bar processes share a private Muse request budget."""
import importlib
import copy
import datetime as dt
import io
import json
import multiprocessing
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
import urllib.error
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
if not (ROOT / "scripts/account-usage").exists():
    ROOT = ROOT / "ccs"
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import muse_console as muse
import desktop_usage as usage
import desktop_helpers as helpers

EMAIL, PLAN, ACCESS = "example@example.com", "Muse Code High Usage", "dca:synthetic-private"
COOKIE = {"name": "llama_dev_sess", "value": "synthetic-web-session", "domain": "dev.meta.ai", "path": "/", "secure": True, "hostOnly": True}
QUOTA = {"tier": PLAN, "window_weighted_used": "0", "window_weighted_limit": "1000", "window_duration_secs": 18000,
         "window_resets_at": None, "weekly_weighted_used": "60", "weekly_weighted_limit": "1000", "weekly_resets_at": 1791158400}


class Client:
    def __init__(self, error=None, counter=None, quota=None):
        self.error, self.counter, self.calls = error, counter, []
        self.quota = QUOTA if quota is None else quota

    def get(self, route):
        self.calls.append(route)
        if route == "/api/auth/me":
            return {"email": EMAIL}
        if route == "/api/portal/teams":
            return {"teams": [{"team_id": 42, "team_name": "Personal"}]}
        if self.counter is not None:
            with self.counter.get_lock():
                self.counter.value += 1
            time.sleep(0.15)
        if self.error:
            raise self.error
        return {"subscription_quota": self.quota, "api_key": "sk-discard"}


def concurrent_reader(root, counter, identities, results):
    def identify():
        with identities.get_lock():
            identities.value += 1
    try:
        value = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(counter=counter), identity_loader=identify)
        results.put((value["cached"], value["sampledAt"]))
    except Exception:
        results.put(("failed", None))


class CacheTests(unittest.TestCase):
    def test_success_survives_cold_process_reload_with_original_sample_and_zero(self):
        with tempfile.TemporaryDirectory() as root:
            first = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            importlib.reload(muse)
            client = Client()
            second = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, client, now=1100)
            self.assertTrue(second["cached"])
            self.assertEqual(second["sampledAt"], first["sampledAt"])
            self.assertEqual(client.calls, [])
            self.assertEqual(usage.normalize_muse({"subscription_quota": second["quota"]})[0]["usedPercent"], 0)
            self.assertIsNone(second["quota"]["window_resets_at"])
            self.assertEqual((Path(root) / muse.CACHE_NAME).stat().st_mode & 0o777, 0o600)
            self.assertNotIn("synthetic-private", (Path(root) / muse.CACHE_NAME).read_text())
            self.assertNotIn("sk-discard", (Path(root) / muse.CACHE_NAME).read_text())

    def test_429_retains_sample_and_persisted_retry_after_stops_all_followup_calls(self):
        with tempfile.TemporaryDirectory() as root:
            first = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            limited = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                        Client(muse.MuseError("rate_limited", retry_after=1800)), now=1400)
            self.assertTrue(limited["cached"])
            self.assertEqual(limited["sampledAt"], first["sampledAt"])
            state = muse.read_usage_state(root)
            self.assertEqual(state["nextRequestAt"], 3200)
            self.assertEqual(state["lastError"], "rate_limited")
            client = Client()
            identity = unittest.mock.Mock()
            after_restart = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, client, now=3199, identity_loader=identity)
            self.assertEqual(after_restart["sampledAt"], first["sampledAt"])
            self.assertEqual(client.calls, [])
            identity.assert_not_called()

    def test_first_rate_limit_keeps_unknown_usage_and_cooldown_instead_of_fake_zero(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(muse.MuseError) as error:
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(muse.MuseError("rate_limited")), now=1000)
            self.assertEqual(error.exception.code, "rate_limited")
            state = muse.read_usage_state(root)
            self.assertIsNone(state["quota"])
            self.assertIsNone(state["sampledAt"])
            self.assertEqual(state["nextRequestAt"], 1600)
            client = Client()
            with self.assertRaises(muse.MuseError):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, client, now=1100)
            self.assertEqual(client.calls, [])

    def test_request_reservation_survives_a_cold_process_crash_without_retry_storm(self):
        with tempfile.TemporaryDirectory() as root:
            def interrupted_identity():
                raise SystemExit("synthetic interruption")
            with self.assertRaises(SystemExit):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000, identity_loader=interrupted_identity)
            self.assertIsNone(muse.read_usage_state(root)["quota"])
            self.assertEqual(muse.read_usage_state(root)["nextRequestAt"], 1300)
            client = Client()
            with self.assertRaises(muse.MuseError):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, client, now=1100)
            self.assertEqual(client.calls, [])

    def test_actual_observation_after_cooldown_replaces_cache_and_its_observation_time(self):
        with tempfile.TemporaryDirectory() as root:
            with patch("desktop_helpers.utc_now", return_value="2026-10-01T13:00:00Z"):
                first = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(muse.MuseError("rate_limited")), now=1400)
            with patch("desktop_helpers.utc_now", return_value="2026-10-01T14:00:00Z"):
                fresh = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=2000)
            self.assertFalse(fresh["cached"])
            self.assertNotEqual(fresh["sampledAt"], first["sampledAt"])
            self.assertEqual(muse.read_usage_state(root)["lastError"], None)

    def test_cache_cannot_cross_email_plan_team_or_native_credential(self):
        with tempfile.TemporaryDirectory() as root:
            muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            for email, plan, team, access in [("other@example.com", PLAN, "42", ACCESS), (EMAIL, "Other", "42", ACCESS),
                                             (EMAIL, PLAN, "43", ACCESS), (EMAIL, PLAN, "42", "dca:other")]:
                self.assertIsNone(muse.cached_quota(root, email, plan, team, access, now=1100))

    def test_expired_auth_clears_last_good_instead_of_showing_another_session(self):
        with tempfile.TemporaryDirectory() as root:
            muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            with self.assertRaises(muse.MuseError):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(muse.MuseError("needs_sign_in")), now=1400)
            self.assertIsNone(muse.read_usage_state(root)["quota"])

    def test_private_state_and_lock_reject_symlink_or_world_readable_file(self):
        with tempfile.TemporaryDirectory() as root:
            muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=1000)
            path = Path(root) / muse.CACHE_NAME
            os.chmod(path, 0o644)
            with self.assertRaises(muse.MuseError):
                muse.cached_quota(root, EMAIL, PLAN, "42", ACCESS, now=1100)
            path.unlink()
            path.symlink_to(Path(root) / "other")
            with self.assertRaises(muse.MuseError):
                muse.read_usage_state(root)
            lock = Path(root) / ".muse-usage.lock"
            lock.unlink()
            lock.symlink_to(Path(root) / "other")
            with self.assertRaises(muse.MuseError):
                with muse.usage_lock(root):
                    self.fail("symlink lock accepted")

    def test_three_independent_dashboard_and_bar_processes_make_one_portal_and_key_read(self):
        with tempfile.TemporaryDirectory() as root:
            context = multiprocessing.get_context("fork")
            counter, identities, results = context.Value("i", 0), context.Value("i", 0), context.Queue()
            processes = [context.Process(target=concurrent_reader, args=(root, counter, identities, results)) for _ in range(3)]
            for process in processes:
                process.start()
            for process in processes:
                process.join(5)
                self.assertEqual(process.exitcode, 0)
            values = [results.get(timeout=1) for _ in processes]
            self.assertEqual(counter.value, 1)
            self.assertEqual(identities.value, 1)
            self.assertEqual(sorted(value[0] for value in values), [False, True, True])
            self.assertEqual(len({value[1] for value in values}), 1)

    def test_collector_cached_timestamp_not_replaced_and_no_duplicate_key_minted(self):
        with tempfile.TemporaryDirectory() as home:
            root = Path(home) / ".ccs/account-usage"
            root.mkdir(parents=True, mode=0o700)
            muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            first = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client())
            with patch("desktop_usage.muse_credentials", return_value=helpers.Credential(ACCESS, email=EMAIL)), patch("desktop_usage.request_json") as request:
                result = usage.collect("muse", "mac", home)
            request.assert_not_called()
            self.assertEqual(result["status"], "cached")
            self.assertEqual(result["sampledAt"], first["sampledAt"])
            self.assertEqual([row["usedPercent"] for row in result["windows"]], [0, 6])

    def test_no_last_good_during_cooldown_remains_unknown_without_key_or_portal_request(self):
        with tempfile.TemporaryDirectory() as home:
            root = Path(home) / ".ccs/account-usage"
            root.mkdir(parents=True, mode=0o700)
            muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            with self.assertRaises(muse.MuseError):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(muse.MuseError("rate_limited")))
            with patch("desktop_usage.muse_credentials", return_value=helpers.Credential(ACCESS, email=EMAIL)), patch("desktop_usage.request_json") as request:
                result = usage.collect("muse", "mac", home)
            request.assert_not_called()
            self.assertEqual(result["status"], "unavailable")
            self.assertEqual(result["plan"], PLAN)
            self.assertEqual(result["windows"], [])
            self.assertIsNone(result["sampledAt"])


class ProviderObservationTimeTests(unittest.TestCase):
    NOW = 1790863200
    ARRIVAL = "2026-10-01T14:00:00Z"

    @staticmethod
    def iso(seconds):
        return dt.datetime.fromtimestamp(seconds, dt.timezone.utc).isoformat().replace("+00:00", "Z")

    def test_fresh_provider_observation_precedes_arrival_without_changing_counters(self):
        for timestamp in (self.NOW - 3600, str(self.NOW - 3600), self.NOW - 3600.125):
            with self.subTest(timestamp=timestamp), tempfile.TemporaryDirectory() as root:
                quota = dict(QUOTA, as_of=timestamp)
                with patch("desktop_helpers.utc_now", return_value=self.ARRIVAL):
                    sample = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                               Client(quota=quota), now=self.NOW)
                self.assertFalse(sample["cached"])
                self.assertEqual(sample["sampledAt"], self.iso(float(timestamp)))
                self.assertEqual(sample["quota"]["as_of"], float(timestamp))
                self.assertNotEqual(sample["sampledAt"], self.ARRIVAL)
                self.assertEqual([row["usedPercent"] for row in usage.normalize_muse({"subscription_quota": sample["quota"]})], [0, 6])

    def test_invalid_or_missing_optional_timestamp_does_not_discard_usable_quota(self):
        bad_values = (None, True, False, -1, 0, float("nan"), float("inf"), -float("inf"),
                      10 ** 1000, "1" * 65, "not-a-time", {}, [], self.NOW * 1000,
                      self.NOW + muse.AS_OF_FUTURE_SKEW_SECONDS + 1, muse.MIN_PROVIDER_AS_OF - 1)
        for timestamp in bad_values:
            with self.subTest(timestamp=repr(timestamp)[:80]), tempfile.TemporaryDirectory() as root:
                with patch("desktop_helpers.utc_now", return_value=self.ARRIVAL):
                    sample = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                               Client(quota=dict(QUOTA, as_of=timestamp)), now=self.NOW)
                self.assertFalse(sample["cached"])
                self.assertEqual(sample["sampledAt"], self.ARRIVAL)
                self.assertNotIn("as_of", sample["quota"])
                self.assertEqual([row["usedPercent"] for row in usage.normalize_muse({"subscription_quota": sample["quota"]})], [0, 6])
        with tempfile.TemporaryDirectory() as root, patch("desktop_helpers.utc_now", return_value=self.ARRIVAL):
            sample = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=self.NOW)
            self.assertEqual(sample["sampledAt"], self.ARRIVAL)
            self.assertNotIn("as_of", sample["quota"])

    def test_reasonable_utc_and_future_skew_bounds_are_explicit(self):
        self.assertEqual(muse.provider_as_of(muse.MIN_PROVIDER_AS_OF, self.NOW), muse.MIN_PROVIDER_AS_OF)
        self.assertEqual(muse.provider_as_of(self.NOW + 300, self.NOW), self.NOW + 300)
        self.assertIsNone(muse.provider_as_of(self.NOW + 301, self.NOW))
        self.assertIsNone(muse.provider_as_of(muse.MAX_PROVIDER_AS_OF + 1, muse.MAX_PROVIDER_AS_OF))
        self.assertIsNone(muse.provider_as_of("1e999", self.NOW))
        self.assertIsNone(muse.provider_as_of(self.NOW, float("nan")))

    def test_good_429_cold_cache_and_recovery_preserve_actual_provider_observation(self):
        with tempfile.TemporaryDirectory() as root:
            original = self.NOW - 3600
            first = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                      Client(quota=dict(QUOTA, as_of=original)), now=self.NOW)
            limited = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                        Client(muse.MuseError("rate_limited", retry_after=1800)), now=self.NOW + 400)
            self.assertTrue(limited["cached"])
            self.assertEqual(limited["sampledAt"], first["sampledAt"])
            self.assertEqual(limited["quota"]["as_of"], original)
            self.assertEqual(muse.read_usage_state(root)["nextRequestAt"], self.NOW + 2200)
            importlib.reload(muse)
            untouched = Client(quota=dict(QUOTA, as_of=self.NOW))
            identity = unittest.mock.Mock()
            with patch("desktop_helpers.utc_now", return_value="2026-10-01T18:00:00Z"):
                cold = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, untouched,
                                         now=self.NOW + 2199, identity_loader=identity)
            self.assertEqual(cold["sampledAt"], self.iso(original))
            self.assertEqual(untouched.calls, [])
            identity.assert_not_called()
            recovered_as_of = self.NOW + 1800
            recovery_quota = dict(QUOTA, as_of=recovered_as_of, weekly_weighted_used="90")
            with patch("desktop_helpers.utc_now", return_value="2026-10-01T18:00:00Z"):
                recovered = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS,
                                              Client(quota=recovery_quota), now=self.NOW + 2200)
            self.assertFalse(recovered["cached"])
            self.assertEqual(recovered["sampledAt"], self.iso(recovered_as_of))
            self.assertEqual([row["usedPercent"] for row in usage.normalize_muse({"subscription_quota": recovered["quota"]})], [0, 9])
            self.assertIsNone(muse.read_usage_state(root)["lastError"])

    def test_legacy_cache_is_not_retimed_or_retrofitted_from_a_new_client(self):
        with tempfile.TemporaryDirectory() as root:
            original = "2026-10-01T13:20:06Z"
            with patch("desktop_helpers.utc_now", return_value=original):
                muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(), now=self.NOW)
            path = Path(root) / muse.CACHE_NAME
            before = path.read_bytes()
            newer_client = Client(quota=dict(QUOTA, as_of=self.NOW))
            with patch("desktop_helpers.utc_now", return_value="2026-10-01T18:00:00Z"):
                cached = muse.quota_sample(root, [COOKIE], EMAIL, PLAN, "42", ACCESS, newer_client, now=self.NOW + 100)
            self.assertEqual(cached["sampledAt"], original)
            self.assertNotIn("as_of", cached["quota"])
            self.assertEqual(newer_client.calls, [])
            self.assertEqual(path.read_bytes(), before)

    def test_browser_collector_keeps_provider_time_separate_from_fetch_time(self):
        with tempfile.TemporaryDirectory() as root:
            muse.write_capsule(root, [COOKIE], "42", EMAIL, PLAN)
            client = Client(quota=dict(QUOTA, as_of=self.NOW - 3600))
            with patch("desktop_usage.muse_credentials", return_value=helpers.Credential(ACCESS, email=EMAIL)), \
                 patch("muse_console.verify_device_identity") as identity, \
                 patch("muse_console.PortalClient.get", side_effect=client.get), \
                 patch("muse_console.time.time", return_value=self.NOW), \
                 patch("desktop_helpers.utc_now", return_value=self.ARRIVAL), \
                 patch("desktop_usage.request_json") as native_request:
                team, sample = muse.collect_browser([COOKIE], root=root)
            self.assertEqual(team, "42")
            self.assertEqual(sample["status"], "ok")
            self.assertEqual(sample["sampledAt"], self.iso(self.NOW - 3600))
            self.assertEqual(sample["fetchedAt"], self.ARRIVAL)
            self.assertEqual(len(client.calls), 3)
            identity.assert_called_once()
            native_request.assert_not_called()

    def test_shipped_helper_is_byte_identical(self):
        self.assertEqual((ROOT / "scripts/account-usage/muse_console.py").read_bytes(),
                         (ROOT / "browser-bridge/opencode-muse/native-host/muse_console.py").read_bytes())


class RetryTests(unittest.TestCase):
    def test_retry_after_number_httpdate_invalid_and_excess_are_bounded(self):
        self.assertEqual(muse.retry_seconds("1800", 1000), 1800)
        self.assertEqual(muse.retry_seconds("Thu, 01 Jan 1970 00:46:40 GMT", 1000), 1800)
        self.assertEqual(muse.retry_seconds("999999999999", 1000), 86400)
        self.assertEqual(muse.retry_seconds("0", 1000), 300)
        for value in (None, "garbage", "-1", "Bearer private"):
            self.assertEqual(muse.retry_seconds(value, 1000), 600)

    def test_real_http429_projection_discards_response_body(self):
        client = muse.PortalClient([COOKIE])
        error = urllib.error.HTTPError(muse.ORIGIN + "/api/auth/me", 429, "private", {"Retry-After": "1800"}, io.BytesIO(b"sk-private"))
        with patch.object(client.opener, "open", side_effect=error), self.assertRaises(muse.MuseError) as caught:
            client.get("/api/auth/me")
        self.assertEqual(caught.exception.code, "rate_limited")
        self.assertEqual(caught.exception.retry_after, 1800)
        self.assertEqual(str(caught.exception), "rate_limited")


class BrowserMigrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = self.temporary.name
        self.credential = patch("desktop_usage.muse_credentials", return_value=helpers.Credential(ACCESS, email=EMAIL))
        self.credential.start()
        muse.write_capsule(self.root, [COOKIE], "42", EMAIL, PLAN)
        with self.assertRaises(muse.MuseError):
            muse.quota_sample(self.root, [COOKIE], EMAIL, PLAN, "42", ACCESS, Client(muse.MuseError("rate_limited")), now=1790863200)
        sample = helpers.account("muse", "mac")
        sample.update(email=EMAIL, plan=PLAN, status="ok", source="Authenticated Meta web quota on Mac",
                      sampledAt="2026-10-01T13:00:00.000Z", fetchedAt="2026-10-01T13:05:00.000Z",
                      windows=usage.normalize_muse({"subscription_quota": QUOTA}))
        self.previous = {"teamId": "42", "sample": sample}

    def tearDown(self):
        self.credential.stop()
        self.temporary.cleanup()

    def test_normal_browser_sample_transfer_is_cached_preserves_time_cooldown_and_private_capsule(self):
        before = muse.read_usage_state(self.root)
        cookie_bytes = (Path(self.root) / muse.CAPSULE_NAME).read_bytes()
        with patch("desktop_usage.request_json") as request, patch("muse_console.PortalClient.get") as portal:
            self.assertTrue(muse.restore_browser_sample(self.root, self.previous, now=1790863800))
        request.assert_not_called()
        portal.assert_not_called()
        importlib.reload(muse)
        after = muse.read_usage_state(self.root)
        self.assertEqual(after["sampledAt"], self.previous["sample"]["sampledAt"])
        self.assertEqual(after["nextRequestAt"], before["nextRequestAt"])
        self.assertEqual(after["lastError"], "rate_limited")
        self.assertEqual((Path(self.root) / muse.CAPSULE_NAME).read_bytes(), cookie_bytes)
        sample = muse.cached_quota(self.root, EMAIL, PLAN, "42", ACCESS, now=1790863300)
        self.assertTrue(sample["cached"])
        self.assertEqual([row["usedPercent"] for row in usage.normalize_muse({"subscription_quota": sample["quota"]})], [0, 6])
        self.assertNotIn("synthetic", (Path(self.root) / muse.CACHE_NAME).read_text())

    def test_other_account_plan_team_source_and_native_credential_are_not_inherited(self):
        mutations = [{"email": "other@example.com"}, {"plan": "Other"}, {"source": "Arbitrary web response"}, {"platform": "ubuntu"}]
        for changes in mutations:
            value = copy.deepcopy(self.previous)
            value["sample"].update(changes)
            with self.subTest(changes=changes), self.assertRaises(muse.MuseError):
                muse.restore_browser_sample(self.root, value, now=1790863800)
        with self.assertRaises(muse.MuseError):
            muse.restore_browser_sample(self.root, dict(self.previous, teamId="43"), now=1790863800)
        with patch("desktop_usage.muse_credentials", return_value=helpers.Credential("dca:different", email=EMAIL)):
            self.assertFalse(muse.restore_browser_sample(self.root, self.previous, now=1790863800))
        self.assertIsNone(muse.read_usage_state(self.root)["quota"])

    def test_real_weighted_counts_preserve_the_existing_four_decimal_percent_projection(self):
        previous = copy.deepcopy(self.previous)
        previous["sample"]["windows"] = usage.normalize_muse({"subscription_quota": dict(QUOTA,
            weekly_weighted_used="19020266200", weekly_weighted_limit="300000000000")})
        self.assertEqual(previous["sample"]["windows"][1]["usedPercent"], 6.3401)
        self.assertTrue(muse.restore_browser_sample(self.root, previous, now=1790863800))
        quota = muse.read_usage_state(self.root)["quota"]
        self.assertEqual(quota["weekly_weighted_used"], 19020266200)
        self.assertEqual(quota["weekly_weighted_limit"], 300000000000)

    def test_future_stale_or_backwards_observation_times_are_rejected(self):
        for sampled, fetched in [("2026-10-02T00:00:00Z", "2026-10-02T00:00:00Z"), ("2026-09-01T00:00:00Z", "2026-09-01T00:00:00Z"),
                                 ("2026-10-01T13:00:00Z", "2026-10-01T12:00:00Z"), ("2026-10-01T13:00:00", "2026-10-01T13:05:00Z")]:
            value = copy.deepcopy(self.previous)
            value["sample"].update(sampledAt=sampled, fetchedAt=fetched)
            with self.subTest(sampled=sampled), self.assertRaises(muse.MuseError):
                muse.restore_browser_sample(self.root, value, now=1790863800)

    def test_unknown_fields_duplicate_or_inconsistent_quota_windows_cannot_seed_cache(self):
        for field, value in [("usedPercent", 99), ("used", -1), ("unit", "USD"), ("kind", "balance"), ("resetAt", "not-a-date"), ("api_key", "sk-private")]:
            previous = copy.deepcopy(self.previous)
            previous["sample"]["windows"][0][field] = value
            with self.subTest(field=field), self.assertRaises(muse.MuseError):
                muse.restore_browser_sample(self.root, previous, now=1790863800)
        previous = copy.deepcopy(self.previous)
        previous["sample"]["windows"] = [previous["sample"]["windows"][0]] * 2
        with self.assertRaises(muse.MuseError):
            muse.restore_browser_sample(self.root, previous, now=1790863800)

    def test_newer_real_host_observation_is_never_replaced_by_old_browser_cache(self):
        state = muse.read_usage_state(self.root)
        state.update(quota=QUOTA, sampledAt="2026-10-01T14:00:00Z", lastError=None)
        muse.atomic_private_json(self.root, muse.CACHE_NAME, state)
        self.assertFalse(muse.restore_browser_sample(self.root, self.previous, now=1790863800))
        self.assertEqual(muse.read_usage_state(self.root)["sampledAt"], "2026-10-01T14:00:00Z")

    def test_absent_state_or_failed_identity_cannot_bootstrap_an_unverified_account(self):
        state = muse.read_usage_state(self.root)
        state["lastError"] = "needs_sign_in"
        muse.atomic_private_json(self.root, muse.CACHE_NAME, state)
        self.assertFalse(muse.restore_browser_sample(self.root, self.previous, now=1790863800))
        (Path(self.root) / muse.CACHE_NAME).unlink()
        self.assertFalse(muse.restore_browser_sample(self.root, self.previous, now=1790863800))


if __name__ == "__main__":
    unittest.main()
