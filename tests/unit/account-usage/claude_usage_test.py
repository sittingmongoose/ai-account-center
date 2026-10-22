"""Offline Claude Desktop quota reader identity and output-boundary regressions."""

import json
import os
from contextlib import nullcontext
from pathlib import Path
import re
import sys
import tempfile
import time
import unittest
from unittest import mock
import urllib.error

HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import claude_usage as usage

ACCOUNT = "11111111-1111-1111-1111-111111111111"
CLIENT = "22222222-2222-2222-2222-222222222222"
ORG = "33333333-3333-3333-3333-333333333333"
OTHER = "44444444-4444-4444-4444-444444444444"
TOKEN = "fixture-private-access-token"
# Fixture caller binding: the manifest email the server would pass per call.
EMAIL = "fixture-profile@example.com"
OTHER_EMAIL = "other-profile@example.com"


def key(account=ACCOUNT, client=CLIENT, org=ORG, host="https://api.anthropic.com", scopes="user:inference user:profile"):
    return "acct:{}|{}:{}:{}:{}".format(account, client, org, host, scopes)


def cache(cache_key=None, **overrides):
    return {cache_key or key(): {
        "token": TOKEN, "refreshToken": "fixture-private-refresh-token",
        "expiresAt": (time.time() + 3600) * 1000, **overrides,
    }}


def profile(**overrides):
    return {
        "account": {"uuid": ACCOUNT, "email": EMAIL, "has_claude_max": True},
        "organization": {"uuid": ORG}, "private": "upstream-private-sentinel", **overrides,
    }


class FakeClient:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []

    def get(self, route, token):
        self.calls.append((route, token))
        response = self.responses.pop(0)
        if isinstance(response, Exception):
            raise response
        return response


class IdentityTests(unittest.TestCase):
    def collect(self, responses, saved_cache=None):
        client = FakeClient(*responses)
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, saved_cache or cache())), \
                mock.patch.object(usage, "Client", return_value=client), \
                mock.patch.object(usage, "optional_web_windows", return_value=[]):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        return result, client

    def test_exact_native_key_and_scope_are_eligible(self):
        entries = usage.eligible_entries(ACCOUNT, cache())
        self.assertEqual(entries[0][1], [ACCOUNT, CLIENT, ORG])
        self.assertEqual(entries[0][2], TOKEN)

    def test_lookalike_origin_scope_and_account_are_rejected(self):
        bad_keys = [
            key(account=OTHER), key(host="https://api.anthropic.com.evil.invalid"),
            key(host="http://api.anthropic.com"), key(scopes="user:profile_extra"),
            key(scopes="user:inference"), key(scopes="user:profile:other"),
            "prefix:" + key(), key() + ":suffix", key().replace("acct:", ""),
        ]
        for value in bad_keys:
            with self.subTest(value=value):
                self.assertEqual(usage.eligible_entries(ACCOUNT, cache(value)), [])

    def test_expiry_and_bad_token_scalars_are_rejected_before_http(self):
        for entry in [
            {"expiresAt": 0}, {"expiresAt": True}, {"expiresAt": float("inf")},
            {"expiresAt": (time.time() + 10) * 1000}, {"token": ""},
            {"token": True}, {"token": "private\nheader"}, {"token": "x" * 32769},
        ]:
            with self.subTest(entry=entry):
                self.assertEqual(usage.eligible_entries(ACCOUNT, cache(**entry)), [])

    def test_inventory_can_keep_typed_expired_identity_without_relaxing_token_grammar(self):
        self.assertEqual(usage.eligible_entries(ACCOUNT, cache(expiresAt=1), allow_expired=True),
                         [(1, [ACCOUNT, CLIENT, ORG], TOKEN)])
        self.assertEqual(usage.eligible_entries(ACCOUNT, cache(token="bad\nheader", expiresAt=1), allow_expired=True), [])
        self.assertEqual(usage.eligible_entries(ACCOUNT, cache(expiresAt=True), allow_expired=True), [])

    def test_live_profile_and_org_verified_before_quota(self):
        result, client = self.collect([profile(), {
            "five_hour": {"utilization": 0, "resets_at": None},
            "seven_day": {"utilization": 100, "resets_at": "2026-10-02T02:59:59.716415Z"},
            "extra_usage": {"is_enabled": False, "monthly_limit": None, "used_credits": None, "utilization": None},
            "secret": "upstream-secret-sentinel",
        }])
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["email"], EMAIL)
        self.assertEqual(result["plan"], "max")
        self.assertTrue(result["accountVerified"])
        self.assertTrue(result["organizationVerified"])
        self.assertEqual(result["windows"][0]["usedPercent"], 0)
        self.assertEqual(result["windows"][1]["resetAt"], "2026-10-02T02:59:59.716Z")
        self.assertFalse(result["windows"][2]["enabled"])
        self.assertIsNone(result["windows"][2]["used"])
        self.assertIsNone(result["windows"][2]["resetAt"])
        self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile", "/api/oauth/usage"])
        for forbidden in (TOKEN, "fixture-private-refresh-token", "upstream-private", "upstream-secret", ACCOUNT, CLIENT, ORG):
            self.assertNotIn(forbidden, json.dumps(result))

    def test_wrong_email_account_and_org_cannot_produce_quota(self):
        wrong_profiles = [
            profile(account={"uuid": ACCOUNT, "email": "different@example.com"}),
            profile(account={"uuid": OTHER, "email": EMAIL}),
            profile(organization={"uuid": CLIENT}),
            profile(organization={"uuid": OTHER}),
        ]
        for value in wrong_profiles:
            with self.subTest(value=value):
                result, client = self.collect([value])
                self.assertNotEqual(result["status"], "ok")
                self.assertIsNone(result["email"])
                self.assertEqual(result["windows"], [])
                self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile"])

    def test_opaque_source_context_changes_with_verified_organization_and_client(self):
        quota = {"five_hour": {"utilization": 0}}
        first, _ = self.collect([profile(), quota])
        other_client, _ = self.collect([profile(), quota], cache(key(client=OTHER)))
        other_org, _ = self.collect([profile(organization={"uuid": OTHER}), quota], cache(key(org=OTHER)))
        fingerprints = [sample["sourceContextFingerprint"] for sample in [first, other_client, other_org]]
        self.assertEqual(len(set(fingerprints)), 3)
        for value in fingerprints:
            self.assertRegex(value, r"^[a-f0-9]{64}$")
        for sample in [first, other_client, other_org]:
            for identity in [ACCOUNT, CLIENT, ORG, OTHER, TOKEN]:
                self.assertNotIn(identity, json.dumps(sample))

    def test_expired_or_empty_cache_never_calls_network(self):
        result, client = self.collect([], cache(expiresAt=0))
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(client.calls, [])

    def test_unavailable_platform_does_not_read_any_credentials(self):
        with mock.patch.object(usage, "decrypt_cache") as decrypt:
            for platform in ("mac", "ubuntu"):
                self.assertEqual(usage.collect("gmail", platform, expected_email=EMAIL)["status"], "unavailable")
            decrypt.assert_not_called()

    def test_native_failure_does_not_emit_exception_or_partial_identity(self):
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", side_effect=RuntimeError("PRIVATE_PATH_TOKEN")):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "unavailable")
        self.assertIsNone(result["email"])
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_new_profile_id_with_caller_email_is_accepted(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 3}})
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), \
                mock.patch.object(usage, "Client", return_value=client), \
                mock.patch.object(usage, "optional_web_windows", return_value=[]):
            result = usage.collect("added-profile", "windows", expected_email=EMAIL,
                                   profile_dir="C:\\Users\\fixture\\AppData\\Roaming\\Claude-added-profile")
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["profileId"], "added-profile")
        self.assertEqual(result["email"], EMAIL)
        self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile", "/api/oauth/usage"])

    def test_missing_invalid_email_or_unsafe_id_reads_no_credentials(self):
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache") as decrypt, \
                mock.patch.dict("os.environ", {}, clear=False):
            os.environ.pop(usage.EXPECTED_EMAIL_ENV, None)
            for profile_id, email in [("added-profile", None), ("added-profile", "not-an-email"),
                                      ("added-profile", "x" * 250 + "@example.com"),
                                      ("bad;id", EMAIL), ("../gmail", EMAIL), ("", EMAIL)]:
                with self.subTest(profile_id=profile_id, email=email):
                    result = usage.collect(profile_id, "windows", expected_email=email)
                    self.assertEqual(result["status"], "unavailable")
                    self.assertIsNone(result["email"])
            decrypt.assert_not_called()

    def test_expected_email_env_fallback_binds_one_invocation(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 3}})
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), \
                mock.patch.object(usage, "Client", return_value=client), \
                mock.patch.object(usage, "optional_web_windows", return_value=[]), \
                mock.patch.dict("os.environ", {usage.EXPECTED_EMAIL_ENV: EMAIL}):
            self.assertEqual(usage.collect("gmail", "windows")["status"], "ok")
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), \
                mock.patch.object(usage, "Client", return_value=FakeClient(profile(), {"five_hour": {"utilization": 3}})), \
                mock.patch.object(usage, "optional_web_windows", return_value=[]), \
                mock.patch.dict("os.environ", {usage.EXPECTED_EMAIL_ENV: "not-an-email"}):
            self.assertEqual(usage.collect("gmail", "windows")["status"], "unavailable")

    def test_collector_source_holds_no_email_mapping_or_addresses(self):
        self.assertFalse(hasattr(usage, "PROFILE_EMAILS"))
        source = (HELPERS / "claude_usage.py").read_text()
        self.assertNotIn("PROFILE_EMAILS", source)
        remaining = "\n".join(line for line in source.splitlines() if "EXPECTED_EMAIL = " not in line)
        self.assertEqual(re.findall(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", remaining), [])


class WindowsProfileRelocationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.legacy = self.home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude"
        self.current = self.home / "AppData/Local/Claude-Data/claude_pzs8sxrjxfjjc"
        # Python 3.13+ ntpath rejects POSIX fixture roots as drive-relative.
        # Accept only this temporary root in addition to real Windows paths.
        is_absolute = usage.ntpath.isabs
        fixture_paths = mock.patch.object(usage.ntpath, "isabs", side_effect=lambda value:
                                         is_absolute(value) or str(value).startswith(str(self.home) + os.sep))
        fixture_paths.start()
        self.addCleanup(fixture_paths.stop)

    def test_legacy_default_still_works_before_relocation(self):
        self.legacy.mkdir(parents=True)
        self.assertEqual(usage.windows_profile_path("work", self.home, True), self.legacy)
        self.assertEqual(usage.resolve_profile_path("work", str(self.legacy), self.home), self.legacy)

    def test_existing_manifest_and_default_flag_follow_current_store_profile(self):
        self.legacy.mkdir(parents=True)
        self.current.mkdir(parents=True)
        self.assertEqual(usage.resolve_profile_path("work", str(self.legacy), self.home), self.current)
        self.assertEqual(usage.resolve_profile_path("work", None, self.home, True), self.current)
        self.assertEqual(usage.resolve_profile_path("work", str(self.current), self.home), self.current)
        self.assertEqual(usage.resolve_windows_profile_root(Path(str(self.legacy).upper()), self.home), self.current)

    def test_named_custom_and_other_users_paths_never_adopt_default_login(self):
        self.current.mkdir(parents=True)
        for path in [self.home / "AppData/Roaming/Claude-work", self.home / "custom/Claude",
                     self.home / "other-user" / self.legacy.relative_to(self.home),
                     self.legacy.parent / "Claude-lookalike"]:
            with self.subTest(path=path):
                self.assertEqual(usage.resolve_profile_path("work", str(path), self.home, True), path)
        self.assertEqual(usage.resolve_profile_path("work", None, self.home),
                         self.home / "AppData/Roaming/Claude-work")

    def test_unreadable_current_location_does_not_revive_legacy_login(self):
        self.legacy.mkdir(parents=True)
        self.current.mkdir(parents=True)
        original = Path.lstat

        def lstat(path, *args, **kwargs):
            if path == self.current:
                raise PermissionError()
            return original(path, *args, **kwargs)

        with mock.patch.object(Path, "lstat", new=lstat), \
                mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache") as decrypt, \
                mock.patch.object(usage, "Client") as client:
            result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                   profile_dir=str(self.legacy))
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["windows"], [])
        decrypt.assert_not_called()
        client.assert_not_called()

    def filesystem_alias(self, root):
        if os.name == "nt":
            import ctypes
            import ctypes.wintypes as wt
            api = ctypes.WinDLL("kernel32", use_last_error=True).GetShortPathNameW
            api.argtypes = [wt.LPCWSTR, wt.LPWSTR, wt.DWORD]
            api.restype = wt.DWORD
            buffer = ctypes.create_unicode_buffer(32768)
            if not api(str(root), buffer, len(buffer)) or Path(buffer.value) == root:
                self.skipTest("Fixture volume does not provide distinct Windows short names")
            return Path(buffer.value), nullcontext()
        alias = root.parent / ("ALIAS-" + root.name)
        alias.mkdir(exist_ok=True)
        original = Path.resolve

        def resolve(path, *args, **kwargs):
            return original(root, *args, **kwargs) if path == alias else original(path, *args, **kwargs)

        return alias, mock.patch.object(Path, "resolve", new=resolve)

    def test_short_legacy_alias_follows_current_root_before_decryption(self):
        self.legacy.mkdir(parents=True)
        alias, resolution = self.filesystem_alias(self.legacy)
        with resolution:
            for relocated in [False, True]:
                if relocated:
                    self.current.mkdir(parents=True)
                client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
                with self.subTest(relocated=relocated), \
                        mock.patch.object(usage.sys, "platform", "win32"), \
                        mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())) as decrypt, \
                        mock.patch.object(usage, "Client", return_value=client), \
                        mock.patch.object(usage, "optional_web_windows", return_value=[]):
                    result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                           profile_dir=str(alias))
                decrypt.assert_called_once_with(self.current if relocated else self.legacy)
                self.assertEqual(result["status"], "ok")
                self.assertEqual(result["email"], EMAIL)

    def test_empty_modern_directory_blocks_old_credentials_through_short_legacy_alias(self):
        self.legacy.mkdir(parents=True)
        self.current.mkdir(parents=True)
        alias, resolution = self.filesystem_alias(self.legacy)
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})

        def decrypt(path):
            if path == self.current:
                raise usage.UsageUnavailable("needs_sign_in")
            return ACCOUNT, cache()

        with resolution, mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", side_effect=decrypt) as reader, \
                mock.patch.object(usage, "Client", return_value=client):
            result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                   profile_dir=str(alias))
        reader.assert_called_once_with(self.current)
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(result["windows"], [])
        self.assertIsNone(result["email"])
        self.assertEqual(client.calls, [])

    def test_short_legacy_alias_rechecks_migration_after_decryption(self):
        self.legacy.mkdir(parents=True)
        alias, resolution = self.filesystem_alias(self.legacy)
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})

        def decrypt(path):
            self.current.mkdir(parents=True)
            return ACCOUNT, cache()

        with resolution, mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", side_effect=decrypt) as reader, \
                mock.patch.object(usage, "Client", return_value=client):
            result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                   profile_dir=str(alias))
        reader.assert_called_once_with(self.legacy)
        self.assertEqual(result["status"], "unavailable")
        self.assertEqual(result["windows"], [])
        self.assertIsNone(result["email"])
        self.assertEqual(client.calls, [])

    def test_native_alias_resolution_rejects_shares_and_linked_directories(self):
        root = self.home / "custom-profile"
        root.mkdir()
        real_lstat = Path.lstat

        def reparse_lstat(path, *args, **kwargs):
            info = real_lstat(path, *args, **kwargs)
            return mock.Mock(st_mode=info.st_mode, st_file_attributes=1024) if path == root else info

        with self.assertRaises(usage.UsageUnavailable):
            usage.resolve_profile_path("work", r"\\server\share\Claude", self.home)
        with mock.patch.object(Path, "lstat", new=reparse_lstat), \
                mock.patch.object(Path, "resolve") as resolve, \
                self.assertRaises(usage.UsageUnavailable):
            usage.resolve_profile_path("work", str(root), self.home)
        resolve.assert_not_called()

    def collect(self, client, decrypt):
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", side_effect=decrypt) as reader, \
                mock.patch.object(usage, "Client", return_value=client), \
                mock.patch.object(usage, "optional_web_windows", return_value=[]):
            result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                   profile_dir=str(self.legacy))
        reader.assert_called_once_with(self.current)
        return result

    def test_fresh_login_is_verified_at_relocated_path_before_reading_usage(self):
        self.current.mkdir(parents=True)
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
        result = self.collect(client, lambda path: (ACCOUNT, cache()))
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["email"], EMAIL)
        self.assertEqual(result["windows"][0]["usedPercent"], 7)
        self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile", "/api/oauth/usage"])

    def test_current_signed_out_corrupt_or_expired_login_never_retries_legacy(self):
        self.legacy.mkdir(parents=True)
        self.current.mkdir(parents=True)
        for error in [usage.UsageUnavailable("needs_sign_in"), usage.UsageUnavailable(),
                      RuntimeError("fixture-private-error"), None]:
            with self.subTest(error=type(error).__name__):
                def decrypt(path):
                    if error is not None:
                        raise error
                    return ACCOUNT, cache(expiresAt=0)
                client = FakeClient()
                result = self.collect(client, decrypt)
                self.assertNotEqual(result["status"], "ok")
                self.assertEqual(result["windows"], [])
                self.assertEqual(client.calls, [])
                self.assertNotIn("fixture-private", json.dumps(result))

    def test_different_account_in_current_default_cannot_supply_requested_usage(self):
        self.current.mkdir(parents=True)
        client = FakeClient(profile(account={"uuid": ACCOUNT, "email": OTHER_EMAIL}))
        result = self.collect(client, lambda path: (ACCOUNT, cache()))
        self.assertEqual(result["status"], "needs_sign_in")
        self.assertEqual(result["windows"], [])
        self.assertEqual([call[0] for call in client.calls], ["/api/oauth/profile"])

    def test_relocation_during_decryption_requests_or_extras_discards_superseded_login(self):
        self.legacy.mkdir(parents=True)
        for phase in ["decrypt", "profile", "usage", "extras"]:
            with self.subTest(phase=phase):
                client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
                original_get = client.get

                def get(route, token):
                    if route.endswith("/" + phase):
                        self.current.mkdir(parents=True)
                    return original_get(route, token)

                def decrypt(path):
                    if phase == "decrypt":
                        self.current.mkdir(parents=True)
                    return ACCOUNT, cache()

                def extras(*args):
                    if phase == "extras":
                        self.current.mkdir(parents=True)
                    return []

                client.get = get
                with mock.patch.object(usage.sys, "platform", "win32"), \
                        mock.patch.object(usage, "decrypt_cache", side_effect=decrypt) as reader, \
                        mock.patch.object(usage, "Client", return_value=client), \
                        mock.patch.object(usage, "optional_web_windows", side_effect=extras):
                    result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                           profile_dir=str(self.legacy))
                reader.assert_called_once_with(self.legacy)
                self.assertEqual(result["status"], "unavailable")
                self.assertEqual(result["windows"], [])
                self.assertIsNone(result["email"])
                self.assertEqual(len(client.calls), {"decrypt": 0, "profile": 1, "usage": 2, "extras": 2}[phase])
                self.current.rmdir()

    def test_relocation_during_optional_429_discards_quota_but_keeps_provider_hold(self):
        self.legacy.mkdir(parents=True)
        web_account = {"uuid": ACCOUNT, "email_address": EMAIL,
                       "memberships": [{"organization": {"uuid": ORG}}]}
        for position in range(3):
            for retry in [1800, None]:
                with self.subTest(position=position, retry=retry):
                    native = FakeClient(profile(), {"five_hour": {"utilization": 7}})
                    web = mock.Mock()
                    responses = [web_account, {}][:position]

                    def get(route):
                        if responses:
                            return responses.pop(0)
                        self.current.mkdir(parents=True)
                        raise usage.UsageUnavailable("rate_limited", retry)

                    web.get.side_effect = get
                    with mock.patch.object(usage.sys, "platform", "win32"), \
                            mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), \
                            mock.patch.object(usage, "Client", return_value=native), \
                            mock.patch.object(usage, "capsule_cookies", return_value=[]), \
                            mock.patch.object(usage, "WebClient", return_value=web):
                        result = usage.collect("work", "windows", home=self.home, expected_email=EMAIL,
                                               profile_dir=str(self.legacy))
                    self.assertEqual(result["status"], "unavailable")
                    self.assertEqual(result["failureCode"], "rate_limited")
                    self.assertEqual(result["windows"], [])
                    self.assertIsNone(result["email"])
                    if retry is None:
                        self.assertNotIn("retryAfterSeconds", result)
                    else:
                        self.assertEqual(result["retryAfterSeconds"], retry)
                    self.assertEqual(web.get.call_count, position + 1)
                    self.current.rmdir()


class WindowTests(unittest.TestCase):
    def fable(self, **overrides):
        return {"kind": "weekly_scoped", "group": "weekly", "percent": 0,
                "resets_at": "2026-10-08T12:00:00+00:00", "is_active": False,
                "scope": {"model": {"id": None, "display_name": "Fable"}, "surface": None}, **overrides}

    def test_reported_inactive_fable_zero_preserves_core_model_and_extra_windows(self):
        windows = usage.normalize_windows({
            "five_hour": {"utilization": 11, "resets_at": "2026-10-01T21:20:00.499Z"},
            "seven_day": {"utilization": 9, "resets_at": "2026-10-08T12:00:00.499Z"},
            "seven_day_opus": {"utilization": 3},
            "limits": [{"kind": "session", "group": "session", "percent": 11}, self.fable()],
            "extra_usage": {"is_enabled": False, "monthly_limit": 5000, "used_credits": 0,
                            "currency": "USD", "utilization": 0},
        })
        self.assertEqual([window["key"] for window in windows],
                         ["five_hour", "seven_day", "seven_day_opus", "seven_day_fable", "extra_usage"])
        self.assertEqual(windows[3], {
            "key": "seven_day_fable", "label": "Weekly Fable usage", "kind": "rate_limit",
            "usedPercent": 0, "remainingPercent": 100, "resetAt": "2026-10-08T12:00:00.000Z",
            "expiresAt": None, "windowMinutes": 10080, "used": None, "limit": None, "unit": None,
        })
        self.assertEqual(windows[0]["resetAt"], "2026-10-01T21:20:00.499Z")
        self.assertEqual(windows[1]["resetAt"], "2026-10-08T12:00:00.499Z")
        self.assertEqual((windows[4]["limit"], windows[4]["remaining"], windows[4]["enabled"]), (50, 50, False))

    def test_fable_is_never_inferred_from_legacy_other_models_or_surface_caps(self):
        cases = [None, [], [self.fable(kind="weekly_all")], [self.fable(group="session")],
                 [self.fable(scope={"model": {"display_name": "Opus"}, "surface": None})],
                 [self.fable(scope={"model": {"display_name": "Fable"}, "surface": {"id": "code"}})]]
        for limits in cases:
            with self.subTest(limits=limits):
                windows = usage.normalize_windows({"seven_day": {"utilization": 42},
                    "seven_day_opus": {"utilization": 80}, "seven_day_fable": {"utilization": 12}, "limits": limits})
                self.assertEqual([window["key"] for window in windows], ["seven_day", "seven_day_opus"])

    def test_fable_nonzero_over_limit_and_duplicate_conflicts_are_safe(self):
        for percent in (12.5, 125.5):
            window = usage.normalize_windows({"limits": [self.fable(percent=percent)]})[0]
            self.assertEqual((window["usedPercent"], window["remainingPercent"]), (percent, max(0, 100 - percent)))
        self.assertEqual(len(usage.normalize_windows({"limits": [self.fable(), self.fable()]})), 1)
        self.assertEqual(usage.normalize_windows({"limits": [self.fable(), self.fable(percent=4)]}), [])
        for value in (True, -1, float("inf"), "0", None):
            with self.subTest(value=value):
                self.assertEqual(usage.normalize_windows({"limits": [self.fable(percent=value)]}), [])
        self.assertEqual(usage.normalize_windows({"limits": [self.fable()] * 65}), [])

    def test_invalid_percent_and_unknown_window_do_not_become_zero(self):
        self.assertEqual(usage.normalize_windows({
            "five_hour": {"utilization": True}, "seven_day": {"utilization": -1},
            "seven_day_opus": {"utilization": float("inf")}, "unknown": {"utilization": 4},
        }), [])

    def test_over_limit_usage_preserves_mixed_quota_resets_and_extra_budget(self):
        windows = usage.normalize_windows({
            "five_hour": {"utilization": 125.5, "resets_at": "2026-10-02T01:00:00Z"},
            "seven_day": {"utilization": 8, "resets_at": "2026-10-06T20:00:00Z"},
            "extra_usage": {"is_enabled": True, "utilization": 112.25, "used_credits": 2245,
                "monthly_limit": 2000, "currency": "USD", "expires_at": "2026-10-22T16:00:00Z"},
        })
        self.assertEqual(len(windows), 3)
        self.assertEqual((windows[0]["usedPercent"], windows[0]["remainingPercent"]), (125.5, 0))
        self.assertEqual(windows[0]["resetAt"], "2026-10-02T01:00:00.000Z")
        self.assertEqual((windows[1]["usedPercent"], windows[1]["remainingPercent"]), (8, 92))
        self.assertEqual(windows[1]["resetAt"], "2026-10-06T20:00:00.000Z")
        self.assertEqual((windows[2]["usedPercent"], windows[2]["remainingPercent"]), (112.25, 0))
        self.assertEqual((windows[2]["used"], windows[2]["limit"], windows[2]["remaining"]), (22.45, 20, 0))
        self.assertEqual(windows[2]["expiresAt"], "2026-10-22T16:00:00.000Z")

    def test_actual_extra_currency_is_minor_units_and_dates_are_not_inferred(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "used_credits": 500, "monthly_limit": 2000,
            "utilization": 25, "currency": "USD", "is_unlimited": False,
        }})[0]
        self.assertEqual((window["used"], window["limit"], window["remaining"]), (5, 20, 15))
        self.assertEqual(window["unit"], "USD")
        self.assertFalse(window["unlimited"])
        self.assertIsNone(window["resetAt"])
        self.assertIsNone(window["expiresAt"])

    def test_absent_currency_preserves_raw_credit_values(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "used_credits": 500, "monthly_limit": 2000,
        }})[0]
        self.assertEqual((window["used"], window["limit"]), (500, 2000))
        self.assertEqual(window["unit"], "credits")
        self.assertNotIn("unlimited", window)
        self.assertIsNone(window["usedPercent"])

    def test_extra_usage_preserves_reported_expiry_reset_and_unlimited_only(self):
        window = usage.normalize_windows({"extra_usage": {
            "is_enabled": True, "is_unlimited": True,
            "resets_at": "2026-10-01T00:00:00-04:00", "expires_at": "2026-12-01T00:00:00Z",
        }})[0]
        self.assertEqual(window["resetAt"], "2026-10-01T04:00:00.000Z")
        self.assertEqual(window["expiresAt"], "2026-12-01T00:00:00.000Z")
        self.assertTrue(window["unlimited"])

    def test_timezone_missing_invalid_calendar_and_arbitrary_text_rejected(self):
        for value in ("2026-10-01T00:00:00", "2026-02-31T00:00:00Z", "token-secret", True, None):
            self.assertIsNone(usage.reset_at(value))

    def test_windows_profile_paths_are_selected_by_caller_not_guessed_from_id(self):
        home = Path("/fixture")
        self.assertEqual(usage.windows_profile_path("gmail", home, True),
                         home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")
        self.assertEqual(usage.windows_profile_path("gmail", home),
                         home / "AppData/Roaming/Claude-gmail")
        self.assertEqual(usage.windows_profile_path("added-profile", home),
                         home / "AppData/Roaming/Claude-added-profile")
        explicit = "C:\\Users\\fixture\\AppData\\Roaming\\Claude-added-profile"
        self.assertEqual(usage.resolve_profile_path("added-profile", explicit, home), Path(explicit))
        self.assertEqual(usage.resolve_profile_path("added-profile", None, home),
                         home / "AppData/Roaming/Claude-added-profile")
        for bad in ("relative\\path", "", "C:\\bad\x00path", "x" * 4097):
            with self.subTest(bad=bad):
                with self.assertRaises(usage.UsageUnavailable):
                    usage.resolve_profile_path("added-profile", bad, home)
        with self.assertRaises(usage.UsageUnavailable):
            usage.resolve_profile_path("bad;id", None, home)


class WebExtraTests(unittest.TestCase):
    def capsule(self, **overrides):
        return {"schemaVersion": 1, "profileId": "gmail", "email": EMAIL,
                "accountUuid": ACCOUNT, "capturedAt": "2020-01-01T00:00:00Z", "cookies": [
                    {"name": "sessionKey", "host_key": ".claude.ai", "path": "/", "top_frame_site_key": "",
                     "value": "fixture-private-session", "is_secure": 1, "is_httponly": 1,
                     "has_expires": 1, "expires_utc": (time.time() + 3600) * 1000000 + usage.CHROMIUM_EPOCH},
                    {"name": "lastActiveOrg", "host_key": ".claude.ai", "path": "/", "value": ORG}], **overrides}

    def test_still_live_capsule_ignores_old_migration_capture_time(self):
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(self.capsule()).encode()) as decrypt:
                cookies = usage.capsule_cookies("gmail", ACCOUNT, ORG, home, EMAIL)
            self.assertEqual([cookie["name"] for cookie in cookies], ["sessionKey", "lastActiveOrg"])
            decrypt.assert_called_once_with(b"fixture-protected", expected_length=None)

    def test_capsule_scope_account_and_expired_cookie_fail_before_http(self):
        cases = [self.capsule(profileId="party"), self.capsule(email=OTHER_EMAIL),
                 self.capsule(accountUuid=OTHER)]
        for field, value in (("host_key", ".evil.invalid"), ("path", "/other"), ("is_httponly", 0),
                             ("expires_utc", 1), ("value", "secret;foreign=header")):
            case = self.capsule(); case["cookies"][0][field] = value; cases.append(case)
        case = self.capsule(); case["cookies"][1]["value"] = OTHER; cases.append(case)
        case = self.capsule(); case["cookies"].append(dict(case["cookies"][0])); cases.append(case)
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            for case in cases:
                with self.subTest(case=case), mock.patch.object(usage, "unprotect", return_value=json.dumps(case).encode()), self.assertRaises(usage.UsageUnavailable):
                    usage.capsule_cookies("gmail", ACCOUNT, ORG, home, EMAIL)

    def test_duplicate_native_org_selectors_coalesce_only_when_identical_and_bound(self):
        value = self.capsule()
        value["cookies"].append({**value["cookies"][1], "samesite": -1})
        with tempfile.TemporaryDirectory() as home:
            path = Path(home) / ".ccs/claude-session-migration/gmail-source.dpapi"
            path.parent.mkdir(parents=True); path.write_bytes(b"fixture-protected")
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(value).encode()):
                self.assertEqual(len(usage.capsule_cookies("gmail", ACCOUNT, ORG, home, EMAIL)), 2)
            value["cookies"][-1]["value"] = OTHER
            with mock.patch.object(usage, "unprotect", return_value=json.dumps(value).encode()), self.assertRaises(usage.UsageUnavailable):
                usage.capsule_cookies("gmail", ACCOUNT, ORG, home, EMAIL)

    def test_historical_used_reset_expiry_is_separate_from_available_zero(self):
        windows = usage.normalize_reset_credits({"eligible": True, "grants": [{
            "resets_left": 0, "resets_total": 1, "paused": False,
            "starts_at": "2026-09-22T16:00:00Z", "ends_at": "2026-10-22T16:00:00Z",
            "id": "private-grant-sentinel"}]}, now=1780000000)
        self.assertEqual(windows[0]["remaining"], 0)
        self.assertIsNone(windows[0]["expiresAt"])
        self.assertEqual(windows[1]["label"], "Used rate-limit reset grant 1")
        self.assertEqual(windows[1]["expiresAt"], "2026-10-22T16:00:00.000Z")
        self.assertEqual((windows[1]["used"], windows[1]["remaining"]), (1, 0))
        self.assertNotIn("enabled", windows[0])
        self.assertNotIn("enabled", windows[1])
        self.assertNotIn("private-grant", json.dumps(windows))

    def test_saved_resets_count_inventory_despite_redemption_gate(self):
        now = 1780000000
        grant = {"resets_left": 2, "resets_total": 3, "paused": False,
                 "starts_at": None, "ends_at": None, "usable_now": False}
        windows = usage.normalize_reset_credits({"eligible": True, "grants": [grant]}, now)
        self.assertEqual(windows[0]["remaining"], 2)
        self.assertEqual(windows[1]["key"], "reset_credit_available_grant_1")
        for change in ({"paused": True}, {"starts_at": "2027-01-01T00:00:00Z"}, {"ends_at": "2020-01-01T00:00:00Z"}):
            values = usage.normalize_reset_credits({"eligible": True, "grants": [{**grant, **change}]}, now)
            self.assertEqual(values[0]["remaining"], 0)
            self.assertNotIn("enabled", values[1])

    def test_absent_malformed_reset_inventory_is_unknown_not_zero(self):
        cases = [None, {}, {"eligible": True}, {"eligible": 1, "grants": []},
                 {"eligible": True, "grants": [{"resets_left": 1, "paused": False, "ends_at": "private-date"}]},
                 {"eligible": True, "grants": [{"resets_left": True, "paused": False}]},
                 {"eligible": True, "grants": [{"resets_left": 2, "resets_total": 1, "paused": False}]},
                 {"eligible": True, "grants": [{"resets_left": 1}]},
                 {"eligible": True, "grants": [{"resets_left": 50, "paused": False}] * 2}]
        for value in cases:
            self.assertEqual(usage.normalize_reset_credits(value), [])
        zero = usage.normalize_reset_credits({"eligible": False, "grants": []})
        self.assertEqual(zero[0]["remaining"], 0)
        self.assertNotIn("enabled", zero[0])

    def test_prepaid_currency_null_and_actual_expiry_are_preserved(self):
        window = usage.normalize_prepaid({"amount": 1234, "currency": "USD",
            "next_expires_at": "2026-10-22T16:00:00Z", "token": "private-prepaid"})[0]
        self.assertEqual((window["remaining"], window["unit"]), (12.34, "USD"))
        self.assertEqual(window["expiresAt"], "2026-10-22T16:00:00.000Z")
        self.assertIsNone(window["resetAt"])
        self.assertNotIn("private-prepaid", json.dumps(window))
        raw = usage.normalize_prepaid({"amount": 0, "currency": None, "tranches": None})[0]
        self.assertEqual((raw["remaining"], raw["unit"]), (0, "credits"))
        self.assertIsNone(raw["expiresAt"])
        for value in ({}, {"amount": True}, {"amount": -1}, {"amount": 0, "currency": "unknown"}):
            self.assertEqual(usage.normalize_prepaid(value), [])

    def web_windows(self, *responses, availability=None, cooldown=None):
        client = mock.Mock()
        client.get.side_effect = responses
        with mock.patch.object(usage, "capsule_cookies", return_value=self.capsule()["cookies"]), mock.patch.object(usage, "WebClient", return_value=client):
            windows = usage.optional_web_windows("gmail", ACCOUNT, ORG, availability=availability, expected_email=EMAIL, cooldown=cooldown)
        return windows, client

    def web_account(self, **overrides):
        return {"email_address": EMAIL, "uuid": ACCOUNT,
                "memberships": [{"organization": {"uuid": ORG}}], **overrides}

    def test_optional_extras_verify_email_uuid_and_org_before_fetch(self):
        for account in (self.web_account(email_address=OTHER_EMAIL),
                        self.web_account(uuid=OTHER), self.web_account(memberships=[{"organization": {"uuid": OTHER}}])):
            windows, client = self.web_windows(account)
            self.assertEqual(windows, [])
            self.assertEqual([call.args[0] for call in client.get.call_args_list], ["/api/account"])
        windows, client = self.web_windows(self.web_account(), {"cedar_ember": {"eligible": True, "grants": []}},
                                           {"amount": 0, "currency": "USD"})
        self.assertEqual([window["remaining"] for window in windows], [0, 0])
        for forbidden in (ACCOUNT, ORG, "fixture-private-session"):
            self.assertNotIn(forbidden, json.dumps(windows))

    def test_optional_failures_do_not_invent_zero_or_drop_other_balance(self):
        windows, _ = self.web_windows(self.web_account(), RuntimeError("private-error"), {"amount": 0, "currency": "USD"})
        self.assertEqual([window["key"] for window in windows], ["prepaid_balance"])
        windows, _ = self.web_windows(self.web_account(), {"cedar_ember": {"eligible": False, "grants": []}}, RuntimeError("private-error"))
        self.assertEqual([window["key"] for window in windows], ["reset_credits_available"])
        with mock.patch.object(usage, "capsule_cookies", side_effect=RuntimeError("private-path")):
            self.assertEqual(usage.optional_web_windows("gmail", ACCOUNT, ORG, expected_email=EMAIL), [])

    def test_optional_rate_limits_stop_at_each_endpoint_with_bounded_metadata(self):
        reset = {"cedar_ember": {"eligible": True, "grants": []}}
        for position in range(3):
            with self.subTest(position=position):
                cooldown, availability = {}, {}
                responses = [self.web_account(), reset][:position] + [usage.UsageUnavailable("rate_limited", 1800)]
                windows, client = self.web_windows(*responses, availability=availability, cooldown=cooldown)
                self.assertEqual(client.get.call_count, position + 1)
                self.assertEqual(cooldown, {"failureCode": "rate_limited", "retryAfterSeconds": 1800})
                self.assertEqual([window["key"] for window in windows], ["reset_credits_available"] if position == 2 else [])
                self.assertEqual(availability, {"resetCredits": "ok" if position == 2 else "unavailable",
                                                "prepaidBalance": "unavailable"})

    def test_default_optional_caller_keeps_list_result_and_stops_on_429_without_an_output_parameter(self):
        windows, client = self.web_windows(self.web_account(), usage.UsageUnavailable("rate_limited", 60),
                                          {"amount": 0, "currency": "USD"})
        self.assertEqual(windows, [])
        self.assertEqual(client.get.call_count, 2)

    def test_optional_group_markers_distinguish_failure_from_successful_empty_null_and_zero(self):
        availability = {}
        windows, _ = self.web_windows(self.web_account(), RuntimeError("private-error"),
            {"amount": 0, "currency": "USD"}, availability=availability)
        self.assertEqual(availability, {"resetCredits": "unavailable", "prepaidBalance": "ok"})
        self.assertEqual(windows[0]["remaining"], 0)
        for reset, prepaid in (({}, {}), ({"cedar_ember": None}, {"amount": None}),
                               ({"cedar_ember": {"eligible": False, "grants": []}}, {"amount": 0})):
            with self.subTest(reset=reset, prepaid=prepaid):
                availability = {}
                windows, _ = self.web_windows(self.web_account(), reset, prepaid, availability=availability)
                self.assertEqual(availability, {"resetCredits": "ok", "prepaidBalance": "ok"})
                self.assertEqual(len(windows), 2 if prepaid.get("amount") == 0 else 0)
        availability = {}
        self.web_windows(self.web_account(uuid=OTHER), availability=availability)
        self.assertEqual(availability, {"resetCredits": "unavailable", "prepaidBalance": "unavailable"})

    def test_native_collector_reports_successful_empty_web_groups_without_invented_windows(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 0}, "limits": [WindowTests().fable()]})
        web = mock.Mock()
        web.get.side_effect = [self.web_account(), {"cedar_ember": None}, {"amount": None}]
        with mock.patch.object(usage.sys, "platform", "win32"), mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), mock.patch.object(usage, "Client", return_value=client), mock.patch.object(usage, "capsule_cookies", return_value=self.capsule()["cookies"]), mock.patch.object(usage, "WebClient", return_value=web):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["optionalExtras"], {"resetCredits": "ok", "prepaidBalance": "ok"})
        self.assertEqual([window["key"] for window in result["windows"]], ["five_hour", "seven_day_fable"])

    def test_native_quota_survives_missing_optional_session(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
        with mock.patch.object(usage.sys, "platform", "win32"), mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), mock.patch.object(usage, "Client", return_value=client), mock.patch.object(usage, "capsule_cookies", side_effect=ValueError("PRIVATE")):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["windows"][0]["usedPercent"], 7)
        self.assertEqual(result["optionalExtras"], {"resetCredits": "unavailable", "prepaidBalance": "unavailable"})
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_legacy_native_quota_survives_optional_429_with_signature_and_cooldown(self):
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
        web = mock.Mock()
        web.get.side_effect = [self.web_account(), usage.UsageUnavailable("rate_limited", 1800)]
        with mock.patch.object(usage.sys, "platform", "win32"), \
                mock.patch.object(usage, "decrypt_cache", return_value=(ACCOUNT, cache())), \
                mock.patch.object(usage, "Client", return_value=client), \
                mock.patch.object(usage, "capsule_cookies", return_value=self.capsule()["cookies"]), \
                mock.patch.object(usage, "WebClient", return_value=web):
            result = usage.collect("gmail", "windows", expected_email=EMAIL)
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["windows"][0]["usedPercent"], 7)
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assertEqual(result["retryAfterSeconds"], 1800)
        self.assertTrue(result["accountVerified"] and result["organizationVerified"])
        self.assertRegex(result["sourceContextFingerprint"], r"^[a-f0-9]{64}$")
        self.assertEqual(web.get.call_count, 2)

    def test_web_client_preserves_rate_limit_without_reading_its_response_body(self):
        for header, delay in [("1800", 1800), ("999999999", 86400), ("-1", None),
                              (None, None), ("Bearer PRIVATE", None), ("9" * 129, None)]:
            with self.subTest(header=header):
                response = mock.Mock(status_code=429, headers={"Retry-After": header})
                if header == "1800":
                    response.close.side_effect = RuntimeError("PRIVATE-close-error")
                requests = mock.Mock()
                requests.get.return_value = response
                with mock.patch.dict(sys.modules, {"curl_cffi": mock.Mock(requests=requests)}):
                    client = usage.WebClient([{"name": "sessionKey", "value": "fixture-private-session"}], ORG)
                with self.assertRaises(usage.UsageUnavailable) as caught:
                    client.get("/api/account")
                self.assertEqual(caught.exception.status, "rate_limited")
                self.assertEqual(caught.exception.retry_after, delay)
                response.iter_content.assert_not_called()
                response.close.assert_called_once()
        with mock.patch.object(usage.time, "time", return_value=0):
            self.assertEqual(usage.retry_after_seconds("Thu, 01 Jan 1970 00:30:00 GMT"), 1800)

    def test_web_client_hosts_routes_redirect_and_response_boundaries(self):
        requests = mock.Mock()
        with mock.patch.dict(sys.modules, {"curl_cffi": mock.Mock(requests=requests)}):
            client = usage.WebClient([{ "name": "sessionKey", "value": "fixture-private-session"}], ORG)
        for route in ("https://evil.invalid", "/api/organizations/" + OTHER + "/prepaid/credits", "/api/organizations/" + ORG + "/usage"):
            with self.assertRaises(usage.UsageUnavailable):
                client.get(route)
        requests.get.assert_not_called()
        response = mock.Mock(status_code=200)
        response.iter_content.return_value = [b"{}"]
        requests.get.return_value = response
        self.assertEqual(client.get("/api/account"), {})
        self.assertTrue(requests.get.call_args.kwargs["verify"])
        self.assertFalse(requests.get.call_args.kwargs["allow_redirects"])
        response.close.assert_called_once()
        response.iter_content.return_value = [b" " * (usage.MAX_BYTES + 1)]
        with self.assertRaises(usage.UsageUnavailable):
            client.get("/api/account")
        client.deadline = 0
        with self.assertRaises(usage.UsageUnavailable):
            client.get("/api/account")


class HttpBoundaryTests(unittest.TestCase):
    def test_redirect_handler_refuses_cross_origin(self):
        self.assertIsNone(usage.NoRedirect().redirect_request(None, None, 302, "", {}, "https://evil.invalid"))

    def test_http_only_admits_fixed_profile_and_usage_routes(self):
        client = usage.Client()
        with mock.patch.object(client.opener, "open") as request:
            with self.assertRaises(usage.UsageUnavailable):
                client.get("https://evil.invalid", TOKEN)
            request.assert_not_called()

    def test_json_read_is_size_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_bytes(b" " * (usage.MAX_BYTES + 1))
            with self.assertRaises(usage.UsageUnavailable):
                usage.read_json(path)

    def test_oversized_http_response_rejected(self):
        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

            def read1(self, count):
                return b"x" * count

        client = usage.Client()
        with mock.patch.object(client.opener, "open", return_value=Response()):
            with self.assertRaises(usage.UsageUnavailable):
                client.get("/api/oauth/profile", TOKEN)

    def test_expired_overall_deadline_prevents_new_request(self):
        client = usage.Client()
        client.deadline = 0
        with mock.patch.object(client.opener, "open") as request:
            with self.assertRaises(usage.UsageUnavailable):
                client.get("/api/oauth/usage", TOKEN)
            request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
