"""Synthetic own-host failover fixtures; provider traffic is always mocked."""
import base64
from contextlib import closing
import datetime as dt
import hashlib
import io
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import urllib.error

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
sys.path.insert(0, str(ROOT / "browser-bridge/opencode-muse/native-host"))
import failover_special as special
import opencode_console as console

EMAIL = "alice@example.test"
SUBJECT = "123456789"
PUBLIC = ("locator", "selectors", "identity", "revision", "email", "plan")


def jwt(email=EMAIL, subject=SUBJECT):
    claims = base64.urlsafe_b64encode(json.dumps({"email": email, "sub": subject}).encode()).decode().rstrip("=")
    return "fixture." + claims + ".signature"


def native(access="fixture-private-access", expiry="2099-01-01T00:00:00Z", **overrides):
    value = {"auth_method": "consumer", "token": {"access_token": access, "refresh_token": "fixture-private-refresh",
             "token_type": "Bearer", "expiry": expiry}, "id_token": jwt()}
    value.update(overrides)
    return value


def identity(email=EMAIL, subject=SUBJECT):
    return special._digest(special._json(["antigravity", email.lower(), subject]))


def quota(credential, sample):
    sample["plan"] = "Pro"
    sample["windows"] = [special.helpers.quota_window("model-quota", "Model quota", used_percent=25,
                                                   reset="2099-01-01T00:00:00Z")]


class SpecialFailoverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="aac-special-failover-")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.ccs = self.home / ".ccs"
        self.network = patch.object(special.HttpClient, "request_json", side_effect=AssertionError("unexpected provider call"))
        self.network.start()
        self.addCleanup(self.network.stop)

    def write(self, relative, value, root=None):
        file = (root or self.home) / relative
        file.parent.mkdir(parents=True, exist_ok=True)
        for parent in [file.parent, *file.parent.parents]:
            if parent == self.home:
                break
            os.chmod(parent, 0o700)
        raw = value if isinstance(value, bytes) else special._json(value).encode()
        file.write_bytes(raw)
        os.chmod(file, 0o600)
        return file

    def native_file(self, **overrides):
        return self.write(".gemini/antigravity-cli/antigravity-oauth-token", native(**overrides))

    def saved(self, name="saved", email=EMAIL, subject=SUBJECT, ccs=None):
        ccs = ccs or self.ccs
        raw = special._json(native(id_token=jwt(email, subject))).encode()
        fingerprint = hashlib.sha256(raw).hexdigest()
        self.write(Path("antigravity-instances") / name / "ubuntu" / ("credential-" + fingerprint + ".bin"), raw, ccs)
        record = {"id": name, "email": email, "identityKey": identity(email, subject), "plan": "Pro",
                  "credential": {"fingerprint": fingerprint, "format": "consumer-json", "identity": {
                      "email": email, "subject": subject, "plan": "Pro", "verifiedAt": "2026-01-01T00:00:00Z",
                      "source": "provider-userinfo"}}}
        registry = self.write("antigravity-profiles/registry-000000000001.json", {
            "version": 1, "revision": 1, "profiles": [record], "active": None, "transaction": None}, ccs)
        return registry, record

    def test_missing_sources_and_native_without_strong_identity_are_excluded_without_network(self):
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])
        self.assertEqual(special.inventory("opencode-wallet", self.home, "ubuntu"), [])
        self.native_file(id_token="")
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])

    def test_native_locator_is_storage_bound_while_auth_revision_changes(self):
        file = self.native_file()
        first = special.inventory("antigravity", self.home, "windows")[0]
        self.native_file(access="replacement-private-access")
        second = special.inventory("antigravity", self.home, "windows")[0]
        self.assertEqual(first["locator"], second["locator"])
        self.assertNotEqual(first["revision"], second["revision"])
        self.assertEqual(first["identity"], {"kind": "antigravity-subject", "key": identity()})
        self.assertEqual(first["selectors"], ["discover"])
        public = json.dumps({key: first[key] for key in PUBLIC})
        for secret in ("fixture-private", str(file), SUBJECT, "id_token"):
            self.assertNotIn(secret, public)

    def test_explicit_scope_enumerates_inactive_saved_profiles_without_mutation(self):
        alternate = self.home / "isolated-aac-scope"
        registry, _ = self.saved(ccs=alternate)
        before = registry.read_bytes()
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])
        rows = special.inventory("antigravity", self.home, "ubuntu", alternate)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["selectors"], ["antigravity-profile:saved"])
        self.assertEqual(rows[0]["identity"]["key"], identity())
        self.assertEqual(registry.read_bytes(), before)

    def test_saved_profile_mismatch_and_newer_corrupt_registry_are_not_resurrected(self):
        registry, record = self.saved()
        record["identityKey"] = "0" * 64
        self.write(registry.relative_to(self.home), {"version": 1, "revision": 1, "profiles": [record]})
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])
        self.saved()
        self.write(".ccs/antigravity-profiles/registry-000000000002.json", b"broken")
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])

    def test_saved_inventory_keeps_each_account_distinct_without_changing_active_state(self):
        _, first = self.saved("first")
        _, second = self.saved("second", "bob@example.test", "987654321")
        registry = self.write(".ccs/antigravity-profiles/registry-000000000002.json", {
            "version": 1, "revision": 2, "profiles": [first, second],
            "active": {"profileId": "first", "verifiedAt": "2026-01-01T00:00:00Z"}, "transaction": None})
        before = registry.read_bytes()
        rows = special.inventory("antigravity", self.home, "ubuntu")
        self.assertEqual([row["selectors"] for row in rows],
                         [["antigravity-profile:first"], ["antigravity-profile:second"]])
        self.assertEqual([row["email"] for row in rows], [EMAIL, "bob@example.test"])
        self.assertNotEqual(rows[0]["identity"], rows[1]["identity"])
        self.assertEqual(registry.read_bytes(), before)

    def test_omp_enabled_oauth_rows_are_separate_and_read_only(self):
        database = self.home / ".omp/agent/agent.db"
        database.parent.mkdir(parents=True)
        with closing(sqlite3.connect(database)) as connection:
            connection.execute("CREATE TABLE auth_credentials(provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)")
            for index, disabled in ((1, None), (2, None), (3, "revoked")):
                value = {"access": "private-omp-" + str(index), "email": EMAIL, "id_token": jwt(subject=str(index))}
                connection.execute("INSERT INTO auth_credentials VALUES(?,?,?,?)",
                                   ("google-antigravity", "oauth", json.dumps(value), disabled))
            connection.commit()
        before = database.read_bytes()
        rows = special.inventory("antigravity", self.home, "ubuntu")
        self.assertEqual(len(rows), 2)
        self.assertNotEqual(rows[0]["locator"], rows[1]["locator"])
        self.assertNotEqual(rows[0]["identity"], rows[1]["identity"])
        self.assertEqual(database.read_bytes(), before)

    def test_google_subject_mismatch_blocks_quota_even_with_same_email(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        with patch.object(special.HttpClient, "request_json", return_value={"email": EMAIL, "id": "999", "verified_email": True}), \
                patch.object(special, "_quota") as query:
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "identity_mismatch"})
        query.assert_not_called()

    def test_verified_email_is_required_and_untrusted_errors_stay_private(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        with patch.object(special.HttpClient, "request_json", return_value={"email": EMAIL, "id": SUBJECT}), \
                patch.object(special, "_quota") as query:
            self.assertEqual(special.collect(candidate, self.home, "ubuntu")["failureCode"], "identity_mismatch")
        query.assert_not_called()
        with patch.object(special.HttpClient, "request_json", side_effect=ValueError("PRIVATE-CREDENTIAL")):
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_in_memory_refresh_verifies_the_exact_token_used_for_quota(self):
        file = self.native_file(expiry="2020-01-01T00:00:00Z")
        before = file.read_bytes()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        refreshed = special.helpers.Credential("renewed-private-access", email=EMAIL)
        with patch.object(special, "_refresh", return_value=refreshed) as refresh, \
                patch.object(special.HttpClient, "request_json", return_value={"email": EMAIL, "id": SUBJECT, "verified_email": True}) as user, \
                patch.object(special, "_quota", side_effect=quota) as query:
            result = special.collect(candidate, self.home, "ubuntu")
        refresh.assert_called_once()
        self.assertEqual(user.call_args.args[1]["Authorization"], "Bearer renewed-private-access")
        self.assertIs(query.call_args.args[0], refreshed)
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 25)
        self.assertEqual(result["sample"]["email"], EMAIL)
        self.assertEqual(result["sample"]["fetchedAt"], result["sample"]["sampledAt"])
        self.assertIsNone(result["failureCode"])
        self.assertNotIn("private-access", json.dumps(result))
        self.assertEqual(file.read_bytes(), before)

    def test_signin_change_during_query_discards_sample(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        def changed(credential, sample):
            quota(credential, sample)
            self.native_file(access="another-account-token")
        with patch.object(special.HttpClient, "request_json", return_value={"email": EMAIL, "id": SUBJECT, "verified_email": True}), \
                patch.object(special, "_quota", side_effect=changed):
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "source_changed"})

    def test_provider_omission_is_unavailable_without_invented_zero(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        with patch.object(special.HttpClient, "request_json", return_value={"email": EMAIL, "id": SUBJECT, "verified_email": True}), \
                patch.object(special, "_quota"):
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "unavailable"})

    def test_identity_rate_limit_stops_before_quota_and_retains_retry_after(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        with patch.object(special.HttpClient, "request_json", side_effect=special.RateLimited(321)) as request, \
                patch.object(special, "_quota") as query:
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 321})
        request.assert_called_once()
        query.assert_not_called()

    def test_refresh_rate_limit_stops_without_rewriting_the_native_credential(self):
        file = self.native_file(expiry="2020-01-01T00:00:00Z")
        before = file.read_bytes()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        with patch.object(special.desktop, "antigravity_oauth_client", return_value=("fixture", "private-app")), \
                patch.object(special.HttpClient, "request_json", side_effect=special.RateLimited(600)) as request, \
                patch.object(special, "_quota") as query:
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 600})
        request.assert_called_once()
        self.assertEqual(request.call_args.args[0], special.TOKEN_URL)
        query.assert_not_called()
        self.assertEqual(file.read_bytes(), before)

    def test_quota_rate_limit_drops_partial_metadata_and_stops_further_requests(self):
        self.native_file()
        candidate = special.inventory("antigravity", self.home, "ubuntu")[0]
        responses = [{"email": EMAIL, "id": SUBJECT, "verified_email": True},
                     {"cloudaicompanionProject": "private-project"}, special.RateLimited(123)]
        with patch.object(special.HttpClient, "request_json", side_effect=responses) as request:
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertEqual(result, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 123})
        self.assertEqual(request.call_count, 3)
        self.assertNotIn("private-project", json.dumps(result))

    def wallet_fixture(self):
        workspace = "wrk_fixture"
        cookies = [{"name": "auth", "value": "PRIVATE-WALLET-COOKIE", "domain": "opencode.ai", "path": "/",
                    "secure": True, "hostOnly": True}]
        self.write(".ccs/account-usage/opencode-console-session.json", {"schemaVersion": 1, "cookies": cookies,
                   "workspaceId": workspace, "origin": console.ORIGIN, "source": "browser-native-cookie-api",
                   "capturedAt": console.now_iso()})
        # Exercise the special helper on every OS without weakening the real
        # POSIX capsule reader or pretending Windows owns an installed bridge.
        support = patch.object(special, "_wallet_supported", return_value=True)
        reader = patch.object(console, "read_capsule", return_value=(cookies, workspace))
        support.start()
        reader.start()
        self.addCleanup(support.stop)
        self.addCleanup(reader.stop)
        return workspace

    def test_wallet_has_full_workspace_identity_separate_from_api_key_accounts(self):
        workspace = self.wallet_fixture()
        candidate = special.inventory("opencode-wallet", self.home, "mac")[0]
        self.assertEqual(candidate["identity"], {"kind": "opencode-workspace", "key": special._digest(workspace)})
        self.assertEqual(candidate["selectors"], ["wallet"])
        self.assertIsNone(candidate["email"])
        self.assertNotIn("PRIVATE", json.dumps({key: candidate[key] for key in PUBLIC}))
        class Client:
            def get(self, route, selected=None):
                if route.endswith("/orgs"):
                    return [{"id": workspace}]
                if route.endswith("/billing/status"):
                    return {"mode": "pay-as-you-go", "billingMode": "prepaid", "balanceMicroCents": "-250000000"}
                return {}
        real_collect = console.collect
        with patch.object(console, "collect", side_effect=lambda cookies, requested: real_collect(cookies, requested, Client())):
            result = special.collect(candidate, self.home, "ubuntu")
        self.assertIsNone(result["failureCode"])
        sample = result["sample"]
        self.assertEqual(sample["id"], "plan-opencode-go-console-mac-" + special._digest(workspace)[:12])
        self.assertEqual(sample["windows"][0]["remaining"], -2.5)
        self.assertEqual(sample["platform"], "ubuntu")
        self.assertIn("Ubuntu", sample["source"])
        self.assertIn("has not been linked", sample["message"])
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_wallet_wrong_workspace_never_reuses_same_provider_label(self):
        self.wallet_fixture()
        candidate = special.inventory("opencode-wallet", self.home, "mac")[0]
        with patch.object(console, "collect", return_value=("wrk_other", {})):
            self.assertEqual(special.collect(candidate, self.home, "mac"),
                             {"sample": None, "failureCode": "identity_mismatch"})

    def test_wallet_rate_limit_reaches_shared_identity_coordinator_without_partial_sample(self):
        self.wallet_fixture()
        candidate = special.inventory("opencode-wallet", self.home, "mac")[0]
        with patch.object(console, "collect", side_effect=console.Unavailable("rate_limited", 321)):
            self.assertEqual(special.collect(candidate, self.home, "mac"),
                             {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 321})
        with patch.object(console, "collect", side_effect=console.Unavailable("rate_limited")):
            self.assertEqual(special.collect(candidate, self.home, "mac"),
                             {"sample": None, "failureCode": "rate_limited"})

    def test_windows_without_supported_capsule_is_truthfully_unavailable(self):
        self.assertEqual(special.inventory("opencode-wallet", self.home, "windows"), [])

    @unittest.skipIf(os.name != "posix", "Real POSIX bridge capsule reader")
    def test_wallet_capsule_permissions_and_expiry_are_checked_without_writes(self):
        workspace = "wrk_readonly"
        file = self.write(".ccs/account-usage/opencode-console-session.json", {
            "schemaVersion": 1, "workspaceId": workspace, "origin": console.ORIGIN,
            "source": "browser-native-cookie-api", "capturedAt": console.now_iso(),
            "cookies": [{"name": "auth", "value": "PRIVATE", "domain": "opencode.ai", "path": "/",
                         "secure": True, "hostOnly": True}]})
        before = file.read_bytes()
        rows = special.inventory("opencode-wallet", self.home, "mac")
        self.assertEqual(rows[0]["identity"]["key"], special._digest(workspace))
        self.assertEqual(file.read_bytes(), before)
        expired = json.loads(before)
        expired["capturedAt"] = "2020-01-01T00:00:00Z"
        self.write(file.relative_to(self.home), expired)
        self.assertEqual(special.inventory("opencode-wallet", self.home, "mac"), [])

    @unittest.skipIf(os.name != "posix", "POSIX ownership/mode fixture")
    def test_private_storage_permissions_and_symlinks_are_refused(self):
        file = self.native_file()
        os.chmod(file, 0o644)
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])
        file.unlink()
        elsewhere = self.write("elsewhere", native())
        file.symlink_to(elsewhere)
        self.assertEqual(special.inventory("antigravity", self.home, "ubuntu"), [])


class HttpClientTests(unittest.TestCase):
    def test_429_is_distinct_and_does_not_read_or_emit_upstream_error_body(self):
        class PrivateBody(io.BytesIO):
            def read(self, *_):
                raise AssertionError("error body must not be read")
        error = urllib.error.HTTPError(special.USERINFO_URL, 429, "PRIVATE-ERROR",
                                       {"Retry-After": "321"}, PrivateBody(b"PRIVATE-BODY"))
        opener = Mock()
        opener.open.side_effect = error
        with patch.object(special.urllib.request, "build_opener", return_value=opener):
            with self.assertRaises(special.RateLimited) as failure:
                special.HttpClient.request_json(special.USERINFO_URL)
        self.assertEqual(failure.exception.retry_after_seconds, 321)
        self.assertNotIn("PRIVATE", str(failure.exception))
        self.assertTrue(error.fp.closed)

    def test_retry_after_dates_defaults_and_upper_bound_are_explicit(self):
        now = dt.datetime(2026, 10, 14, 12, tzinfo=dt.timezone.utc)
        self.assertEqual(special._retry_after("Wed, 14 Oct 2026 12:10:00 GMT", now), 600)
        self.assertEqual(special._retry_after("999999999999", now), 86400)
        self.assertEqual(special._retry_after(None, now), 60)
        self.assertEqual(special._retry_after("invalid", now), 60)

    def test_quota_transport_retains_native_user_agent_and_rejects_other_urls(self):
        opener = Mock()
        opener.open.return_value = io.BytesIO(b'{}')
        with patch.object(special.urllib.request, "build_opener", return_value=opener):
            special.HttpClient.request_json(special.desktop.AGY_BASE + "loadCodeAssist",
                                            {"User-Agent": "antigravity/1.0.0"}, {})
            self.assertEqual(opener.open.call_args.args[0].get_header("User-agent"), "antigravity/1.0.0")
            with self.assertRaises(special.helpers.UsageError):
                special.HttpClient.request_json("https://unrelated.example.test")
        opener.open.assert_called_once()


if __name__ == "__main__":
    unittest.main()
