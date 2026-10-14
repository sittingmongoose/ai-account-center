"""Additional-provider failover uses synthetic homes and never contacts a provider."""

import base64
from contextlib import closing
import io
import json
import os
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
import urllib.error
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/account-usage"))
import failover_additional as adapter
import muse_console
import plan_common


def jwt(subject, email="one@example.test", suffix="signature"):
    payload = base64.urlsafe_b64encode(json.dumps({"sub": subject, "email": email}).encode()).decode().rstrip("=")
    return "header." + payload + "." + suffix


def public(candidate):
    return {key: value for key, value in candidate.items() if not key.startswith("_")}


class AdditionalFailoverTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.ccs = self.home / "scope"
        self.addCleanup(patch.stopall)
        patch.dict(os.environ, {"XDG_CONFIG_HOME": str(self.home / ".config")}, clear=True).start()
        patch("urllib.request.OpenerDirector.open", side_effect=AssertionError("Network access is forbidden in fixtures")).start()

    def write(self, relative, payload):
        target = self.home / relative
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        target.write_text(json.dumps(payload), encoding="utf-8")
        target.chmod(0o600)
        return target

    def native_muse(self, token="dca:synthetic-muse", email="one@example.test", relative=".config/muse/auth.json"):
        return self.write(relative, {"providers": {"meta": {"access_token": token, "user_email": email}}})

    def muse_capsule(self, team="42", email="one@example.test", session="synthetic-session", plan="Pro"):
        return ([{"name": "fixture", "value": session}], team, email, plan)

    def omp(self, rows):
        path = self.home / ".omp/agent/agent.db"
        path.parent.mkdir(parents=True, exist_ok=True)
        with closing(sqlite3.connect(path)) as connection:
            connection.execute("CREATE TABLE auth_credentials (id INTEGER PRIMARY KEY,provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)")
            for index, provider, kind, data, disabled in rows:
                connection.execute("INSERT INTO auth_credentials VALUES (?,?,?,?,?)", (index, provider, kind, json.dumps(data), disabled))
            connection.commit()

    def read(self, provider):
        return adapter.inventory(provider, self.home, "ubuntu", ccs_dir=self.ccs)

    def fake_client(self, value):
        client = Mock()
        client.get.return_value = value
        patch.object(plan_common, "HttpClient", return_value=client).start()
        return client

    def key(self, provider, key_id, secret):
        self.write("scope/account-usage/keys/" + provider + "-" + key_id + ".json",
            {"version": 1, "provider": provider, "keyId": key_id, "secret": secret})
        (self.ccs / "account-usage/keys").chmod(0o700)

    def plaintext_fixture_keys(self):
        original = plan_common.aac_key_credential
        patch.object(plan_common, "aac_key_credential", side_effect=lambda provider, key_id, home=None, ccs_dir=None:
                     original(provider, key_id, home, windows=False, ccs_dir=ccs_dir)).start()

    def test_inventory_is_network_free_and_contains_every_muse_slot(self):
        self.native_muse()
        self.native_muse("dca:isolated", "two@example.test", "scope/muse-homes/1234abcd/muse/auth.json")
        self.omp([(1, "muse-code", "oauth", {"access": "dca:omp-first", "email": "one@example.test"}, None),
                  (2, "muse-code", "oauth", {"access": json.dumps({"oauthAccessToken": "dca:omp-second", "apiKey": "sk-unused"}), "email": "one@example.test"}, None),
                  (3, "muse-code", "oauth", {"access": "dca:disabled"}, "revoked")])
        candidates = self.read("muse")
        self.assertEqual(len(candidates), 4)
        self.assertIn(["config-home:1234abcd"], [row["selectors"] for row in candidates])
        rendered = json.dumps([public(row) for row in candidates])
        for secret in ("dca:", "sk-unused", "oauthAccessToken", "scope/", "auth.json"):
            self.assertNotIn(secret, rendered)
        for row in candidates:
            for digest in (row["locator"], row["identity"]["key"], row["revision"]):
                self.assertRegex(digest, r"^[a-f0-9]{64}$")

    def test_native_muse_requires_exact_credential_when_subscription_scope_is_unknown(self):
        self.native_muse()
        first = self.read("muse")[0]
        self.native_muse("dca:rotated")
        second = self.read("muse")[0]
        self.assertEqual(first["locator"], second["locator"])
        self.assertEqual(first["identity"]["kind"], "muse-native-credential")
        self.assertNotEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])
        self.native_muse("dca:other-user", "other@example.test")
        self.assertNotEqual(second["identity"], self.read("muse")[0]["identity"])

    def test_native_muse_reports_real_zero_without_browser_session(self):
        self.native_muse()
        self.fake_client({"user_email": "one@example.test", "subs_tier_name": "Pro", "is_subs_active": True,
                          "subs_usage": {"window": {"used_percent": 0}}})
        with patch.object(muse_console, "read_capsule", side_effect=muse_console.MuseError("no_browser_cookie")), \
             patch.object(muse_console, "quota_sample", side_effect=AssertionError("Native quota must not require a browser request")):
            result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertIsNone(result["failureCode"])
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 0)

    def test_muse_live_wrong_identity_is_rejected_before_browser_fallback(self):
        self.native_muse()
        self.fake_client({"user_email": "wrong@example.test", "subs_tier_name": "Pro", "subs_usage": {"window": {"used_percent": 4}}})
        with patch.object(muse_console, "quota_sample") as sample:
            result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        sample.assert_not_called()
        self.assertEqual(result, {"sample": None, "failureCode": "account_mismatch"})

    def test_muse_missing_native_quota_keeps_bound_browser_team_and_custom_scope(self):
        self.native_muse()
        self.fake_client({"user_email": "one@example.test", "subs_tier_name": "Pro", "is_subs_active": True})
        quota = {"tier": "Pro", "window_weighted_used": 0, "window_weighted_limit": 100}
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule(plan="Old Plan")) as capsule, \
             patch.object(muse_console, "quota_sample", return_value={"quota": quota, "cached": True,
                 "sampledAt": "2026-10-14T00:00:00Z", "message": None}) as sample:
            result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertTrue(all(call.args == (self.ccs / "account-usage",) for call in capsule.call_args_list))
        self.assertEqual(sample.call_args.kwargs["bound_team"], "42")
        self.assertEqual(sample.call_args.args[3], "Pro")
        self.assertEqual(result["sample"]["sampledAt"], "2026-10-14T00:00:00Z")
        self.assertEqual(result["sample"]["status"], "cached")

    def test_muse_refuses_foreign_browser_capsule(self):
        self.native_muse()
        self.fake_client({"user_email": "one@example.test", "subs_tier_name": "Pro"})
        with patch.object(muse_console, "read_capsule", return_value=([], "42", "other@example.test", "Pro")), \
             patch.object(muse_console, "quota_sample") as sample:
            result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        sample.assert_not_called()
        self.assertEqual(result["failureCode"], "no_browser_cookie")

    def test_muse_portal_rotation_preserves_team_identity_but_changes_revision(self):
        self.native_muse()
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            first = self.read("muse")[0]
            self.native_muse("dca:rotated")
            second = self.read("muse")[0]
        self.assertEqual(first["identity"]["kind"], "muse-team")
        self.assertEqual(first["locator"], second["locator"])
        self.assertEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])

    def test_muse_same_email_different_teams_never_share_identity(self):
        self.native_muse()
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule(team="42")):
            first = self.read("muse")[0]
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule(team="43")):
            second = self.read("muse")[0]
        self.assertNotEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])

    def test_muse_same_team_can_match_independent_device_and_browser_sessions(self):
        self.native_muse()
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            first = self.read("muse")[0]
        self.native_muse("dca:another-device")
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule(session="another-browser")):
            second = self.read("muse")[0]
        self.assertEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])
        for secret in ("dca:", "synthetic-session", "another-browser", "_muse_context"):
            self.assertNotIn(secret, json.dumps(public(second)))

    def test_muse_native_unknown_scope_cannot_match_portal_team(self):
        self.native_muse()
        native = self.read("muse")[0]
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            portal = self.read("muse")[0]
        self.assertNotEqual(native["identity"], portal["identity"])

    def test_muse_changed_team_or_browser_session_is_rejected_before_provider_requests(self):
        self.native_muse()
        client = self.fake_client({})
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            candidate = self.read("muse")[0]
        for changed in (self.muse_capsule(team="43"), self.muse_capsule(session="rotated-browser")):
            with patch.object(muse_console, "read_capsule", return_value=changed):
                result = adapter.collect(candidate, self.home, "ubuntu", ccs_dir=self.ccs)
            self.assertEqual(result["failureCode"], "source_changed")
        client.get.assert_not_called()

    def test_muse_new_capsule_cannot_change_native_scope_during_collection(self):
        self.native_muse()
        candidate = self.read("muse")[0]
        client = self.fake_client({})
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            result = adapter.collect(candidate, self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(result["failureCode"], "source_changed")
        client.get.assert_not_called()

    def test_muse_portal_identity_never_substitutes_native_quota_of_unknown_scope(self):
        self.native_muse()
        self.fake_client({"user_email": "one@example.test", "subs_tier_name": "Pro", "is_subs_active": True,
                          "subs_usage": {"window": {"used_percent": 3}}})
        quota = {"tier": "Pro", "window_weighted_used": 80, "window_weighted_limit": 100}
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()), \
             patch.object(muse_console, "quota_sample", return_value={"quota": quota, "cached": False,
                 "sampledAt": "2026-10-14T00:00:00Z", "message": None}) as sample:
            result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        sample.assert_called_once()
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 80)

    def test_expired_muse_capsule_retains_team_identity_but_cannot_be_collected(self):
        self.native_muse()
        self.fake_client({"user_email": "one@example.test", "subs_tier_name": "Pro", "is_subs_active": True})
        def expired_capsule(root, *, allow_expired=False):
            if allow_expired:
                return self.muse_capsule()
            raise muse_console.MuseError("no_browser_cookie")
        with patch.object(muse_console, "read_capsule", return_value=self.muse_capsule()):
            before = self.read("muse")[0]
        with patch.object(muse_console, "read_capsule", side_effect=expired_capsule), \
             patch.object(muse_console, "quota_sample") as sample:
            expired = self.read("muse")[0]
            result = adapter.collect(expired, self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(before["identity"], expired["identity"])
        self.assertEqual(before["revision"], expired["revision"])
        self.assertEqual(result["failureCode"], "no_browser_cookie")
        sample.assert_not_called()

    @unittest.skipUnless(os.name == "posix", "Muse capsules use the existing POSIX private store")
    def test_real_capsule_reader_keeps_expiry_enforcement_except_for_inventory(self):
        self.native_muse()
        stamp = 1792000000
        value = {"schemaVersion": 1, "provider": "muse", "origin": muse_console.ORIGIN,
                 "email": "one@example.test", "plan": "Pro", "teamId": "42",
                 "cookies": [{"name": "llama_dev_sess", "value": "synthetic-session",
                              "domain": "dev.meta.ai", "path": "/", "secure": True,
                              "hostOnly": True, "expirationDate": stamp + 1}]}
        file = self.write("scope/account-usage/muse-console-session.json", value)
        file.parent.chmod(0o700)
        with patch.object(muse_console.time, "time", return_value=stamp):
            first = self.read("muse")[0]
            self.assertEqual(muse_console.read_capsule(file.parent)[1], "42")
        with patch.object(muse_console.time, "time", return_value=stamp + 2):
            expired = self.read("muse")[0]
            self.assertEqual(first["identity"], expired["identity"])
            self.assertEqual(first["revision"], expired["revision"])
            with self.assertRaises(muse_console.MuseError) as raised:
                muse_console.read_capsule(file.parent)
            self.assertEqual(raised.exception.code, "no_browser_cookie")
            self.assertEqual(muse_console.read_capsule(file.parent, allow_expired=True)[1], "42")
            value["teamId"] = "43"
            self.write("scope/account-usage/muse-console-session.json", value)
            changed = self.read("muse")[0]
            self.assertNotEqual(expired["identity"], changed["identity"])
            self.assertNotEqual(expired["revision"], changed["revision"])

    def test_cursor_subject_survives_token_rotation_but_not_account_change(self):
        self.write(".config/Cursor/auth.json", {"accessToken": jwt("auth0|one")})
        first = self.read("cursor")[0]
        self.write(".config/Cursor/auth.json", {"accessToken": jwt("auth0|one", suffix="rotated")})
        second = self.read("cursor")[0]
        self.assertEqual(first["identity"], second["identity"])
        self.assertEqual(first["locator"], second["locator"])
        self.assertNotEqual(first["revision"], second["revision"])
        self.write(".config/Cursor/auth.json", {"accessToken": jwt("auth0|two")})
        self.assertNotEqual(first["identity"], self.read("cursor")[0]["identity"])

    def test_cursor_rejects_live_email_conflict(self):
        self.write(".config/Cursor/auth.json", {"accessToken": jwt("auth0|one")})
        client = self.fake_client(None)
        client.get.side_effect = [{"individualUsage": {"plan": {"used": 0, "limit": 100}}}, {"email": "other@example.test"}]
        result = adapter.collect(self.read("cursor")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(result["failureCode"], "account_mismatch")
        self.assertIsNone(result["sample"])

    def test_second_saved_api_key_is_collected_without_default_credential_substitution(self):
        self.plaintext_fixture_keys()
        self.key("opencode-go", "11111111", "sk-fixture-first")
        self.key("opencode-go", "22222222", "sk-fixture-second")
        candidates = self.read("opencode-go")
        self.assertEqual(len(candidates), 2)
        second = next(row for row in candidates if "aac-key:22222222" in row["selectors"])
        client = self.fake_client({"usage": {name: {"status": "ok", "percent": 0} for name in ("rolling", "weekly", "monthly")}})
        result = adapter.collect(second, self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(client.get.call_args.args[1]["Authorization"], "Bearer sk-fixture-second")
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 0)
        self.assertNotIn("sk-fixture", json.dumps(result))

    def test_saved_key_scope_is_not_the_default_ccs_directory(self):
        self.plaintext_fixture_keys()
        self.key("zai", "11111111", "sk-scope-key")
        self.assertEqual(len(self.read("zai")), 1)
        self.assertEqual(adapter.inventory("zai", self.home, "ubuntu"), [])

    def test_key_rotation_changes_identity_and_revision_but_preserves_locator(self):
        self.plaintext_fixture_keys()
        self.key("zai", "11111111", "sk-first-key")
        first = self.read("zai")[0]
        self.key("zai", "11111111", "sk-second-key")
        second = self.read("zai")[0]
        self.assertEqual(first["locator"], second["locator"])
        self.assertNotEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])

    def test_all_omp_keys_are_inventoried_without_unifying_equal_email(self):
        self.omp([(1, "kimi-code", "api_key", {"key": "sk-one", "email": "one@example.test"}, None),
                  (2, "kimi-code", "api_key", {"key": "sk-two", "email": "one@example.test"}, None)])
        candidates = self.read("kimi-code")
        self.assertEqual(len(candidates), 2)
        self.assertNotEqual(candidates[0]["identity"], candidates[1]["identity"])

    def test_kimi_expired_oauth_never_calls_provider(self):
        self.omp([(1, "kimi-code", "oauth", {"access": "expired-fixture", "expires": 1}, None)])
        client = self.fake_client({})
        result = adapter.collect(self.read("kimi-code")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        client.get.assert_not_called()
        self.assertEqual(result["failureCode"], "needs_sign_in")

    def test_qwen_identity_uses_quota_sessions_not_inference_key_or_json_order(self):
        one = {"secret": json.dumps({"cookie": "sid=one; csrf=two", "token": "key-one"}), "source": "OMP", "email": None}
        two = {"secret": json.dumps({"token": "key-two", "cookie": "csrf=two; sid=one"}), "source": "OMP", "email": None}
        first = adapter._candidate("qwen", "slot", ["discover"], one)
        second = adapter._candidate("qwen", "slot", ["discover"], two)
        self.assertEqual(first["identity"], second["identity"])
        changed = dict(two, secret=json.dumps({"token": "key-one", "cookie": "sid=other; csrf=two"}))
        self.assertNotEqual(first["identity"], adapter._candidate("qwen", "slot", ["discover"], changed)["identity"])
        changed = dict(two, secret=json.dumps({"cookie": "sid=one; csrf=two", "gatewayCookie": "sid=other"}))
        self.assertNotEqual(first["identity"], adapter._candidate("qwen", "slot", ["discover"], changed)["identity"])

    def test_qwen_plain_inference_keys_are_not_quota_sources(self):
        self.write(".local/share/opencode/auth.json", {"alibaba-token-plan": {"type": "api", "key": "sk-fixture-inference"}})
        self.assertEqual(self.read("qwen"), [])

    def test_multiple_qwen_capsules_are_enumerated_with_private_scope(self):
        for name in ("qwen-console-session.json", "qwen-console-session-1234abcd.json"):
            self.write("scope/account-usage/" + name, {"version": 2, "region": "intl", "cookiesDPAPI": base64.b64encode(b"fixture").decode()})
        with patch.object(plan_common, "_windows_unprotect", return_value=json.dumps({"consoleCookie": "sid=fixture", "gatewayCookie": "sid=fixture"})):
            candidates = self.read("qwen")
        self.assertEqual(len(candidates), 2)
        self.assertIn(["browser-capsule:1234abcd"], [row["selectors"] for row in candidates])
        self.assertIn(["browser-capsule:default", "discover"], [row["selectors"] for row in candidates])

    def test_structured_rate_limit_preserves_provider_cooldown_without_body(self):
        self.native_muse()
        client = self.fake_client({})
        client.get.side_effect = plan_common.CollectionError("error", "private upstream body", failure_code="rate_limited", retry_after=900)
        result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(result, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 900})
        self.assertNotIn("private", json.dumps(result))

    def test_http_client_classifies_429_and_caps_retry_after(self):
        error = urllib.error.HTTPError("https://example.invalid", 429, "fixture", {"Retry-After": "999999999"}, io.BytesIO(b"private"))
        opener = Mock()
        opener.open.side_effect = error
        with patch("urllib.request.build_opener", return_value=opener):
            with self.assertRaises(plan_common.CollectionError) as caught:
                plan_common.HttpClient().get("https://example.invalid")
        self.assertEqual(caught.exception.failure_code, "rate_limited")
        self.assertEqual(caught.exception.retry_after, 86400)
        self.assertEqual(error.fp.tell(), 0)

    def test_unexpected_exceptions_cannot_leak_tokens(self):
        self.native_muse()
        client = self.fake_client({})
        client.get.side_effect = ValueError("Authorization: Bearer dca:do-not-leak")
        result = adapter.collect(self.read("muse")[0], self.home, "ubuntu", ccs_dir=self.ccs)
        self.assertEqual(result, {"sample": None, "failureCode": "provider_error"})


if __name__ == "__main__":
    unittest.main()
