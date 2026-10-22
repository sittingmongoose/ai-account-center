"""Offline identity and transport regressions for cross-machine core quotas."""

import base64
from contextlib import nullcontext
from email.message import Message
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock
import urllib.error

HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import failover_core as core

EMAIL = "fixture@example.com"
ACCOUNT = "11111111-1111-1111-1111-111111111111"
ORG = "22222222-2222-2222-2222-222222222222"
OTHER = "33333333-3333-3333-3333-333333333333"
CLIENT = "44444444-4444-4444-4444-444444444444"
TOKEN = "fixture-private-token"


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def jwt(payload):
    body = base64.urlsafe_b64encode(json.dumps(payload).encode()).decode().rstrip("=")
    return "eyJhbGciOiJSUzI1NiJ9." + body + ".fixture"


def codex_auth(**overrides):
    claims = {"chatgpt_account_id": ACCOUNT, "chatgpt_user_id": "fixture-user", "chatgpt_plan_type": "pro"}
    claims.update(overrides.pop("claims", {}))
    payload = {"email": EMAIL, "sub": "fixture-subject", "iss": "https://auth.openai.com",
               "https://api.openai.com/auth": claims}
    payload.update(overrides.pop("payload", {}))
    tokens = {"id_token": jwt(payload), "access_token": TOKEN, "account_id": ACCOUNT}
    tokens.update(overrides)
    return {"tokens": tokens}


def claude_files(home, root=None, token=TOKEN):
    root = root or home / ".claude"
    write(root / ".credentials.json", {"claudeAiOauth": {"accessToken": token,
          "refreshToken": "fixture-refresh-private", "subscriptionType": "max"}})
    write(home / ".claude.json" if root == home / ".claude" else root / ".claude.json",
          {"oauthAccount": {"accountUuid": ACCOUNT, "organizationUuid": ORG, "emailAddress": EMAIL}})


def profile(**overrides):
    value = {"account": {"uuid": ACCOUNT, "email": EMAIL, "has_claude_max": True},
             "organization": {"uuid": ORG}, "private": "fixture-upstream-private"}
    value.update(overrides)
    return value


class FakeClient:
    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []

    def get(self, *args):
        self.calls.append(args)
        value = self.responses.pop(0)
        if isinstance(value, Exception):
            raise value
        return value


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)

    def test_codex_inventory_default_saved_and_t3_homes(self):
        roots = [self.home / ".codex", self.home / ".ccs/codex-instances/saved",
                 self.home / ".codex-t3/personal", self.home / ".t3/userdata/providers/codex/configured/shadow",
                 self.home / "custom-codex"]
        for root in roots:
            write(root / "auth.json", codex_auth())
        write(self.home / ".t3/userdata/settings.json", {"providerInstances": {
            "one": {"driver": "codex", "config": {"homePath": str(roots[-1]), "shadowHomePath": str(roots[3])}},
            "other": {"driver": "claudeAgent", "config": {"homePath": str(self.home / "ignored")}}}})
        for platform in ("ubuntu", "windows", "mac"):
            with self.subTest(platform=platform), mock.patch.object(core, "Client", side_effect=AssertionError("inventory must be offline")):
                rows = core.inventory("codex", self.home, platform)
            self.assertEqual(len(rows), len(roots))
            self.assertEqual(len({row["identity"]["key"] for row in rows}), 1)
            self.assertIn("discover", rows[0]["selectors"])
            self.assertIn("codex-profile:saved", rows[1]["selectors"])
            public = [{key: value for key, value in row.items() if not key.startswith("_")} for row in rows]
            for row in public:
                for field in ("locator", "revision"):
                    self.assertRegex(row[field], r"^[a-f0-9]{64}$")
            serialized = json.dumps(public)
            for secret in (TOKEN, ACCOUNT, "fixture-subject", "fixture-user", str(self.home)):
                self.assertNotIn(secret, serialized)

    def test_same_profile_name_does_not_make_same_account(self):
        write(self.home / ".ccs/codex-instances/saved/auth.json", codex_auth())
        first = core.inventory("codex", self.home, "ubuntu")[0]
        write(self.home / ".ccs/codex-instances/saved/auth.json", codex_auth(claims={"chatgpt_user_id": "other-person"}))
        second = core.inventory("codex", self.home, "ubuntu")[0]
        self.assertEqual(first["locator"], second["locator"])
        self.assertEqual(first["selectors"], second["selectors"])
        self.assertNotEqual(first["identity"], second["identity"])
        self.assertNotEqual(first["revision"], second["revision"])

    def test_token_rotation_changes_revision_not_account_or_locator(self):
        path = self.home / ".codex/auth.json"
        write(path, codex_auth())
        before = core.inventory("codex", self.home, "ubuntu")[0]
        write(path, codex_auth(access_token="fixture-new-private-token"))
        after = core.inventory("codex", self.home, "ubuntu")[0]
        self.assertEqual(before["locator"], after["locator"])
        self.assertEqual(before["identity"], after["identity"])
        self.assertNotEqual(before["revision"], after["revision"])

    def test_workspace_mismatch_and_inconsistent_users_are_not_candidates(self):
        bad = [codex_auth(account_id=OTHER), codex_auth(claims={"user_id": "another-user"}),
               codex_auth(claims={"chatgpt_account_id": ""}),
               codex_auth(payload={"email": "invalid"}),
               codex_auth(access_token=jwt({"https://api.openai.com/auth": {"chatgpt_account_id": OTHER}}))]
        for value in bad:
            write(self.home / ".codex/auth.json", value)
            self.assertEqual(core.inventory("codex", self.home, "ubuntu"), [])

    def test_missing_principal_fields_do_not_downgrade_strong_binding(self):
        full = core._codex_identity(codex_auth()["tokens"])
        legacy = core._codex_identity(codex_auth(claims={"chatgpt_user_id": None},
                                               payload={"sub": None, "iss": None})["tokens"])
        self.assertNotEqual(core.digest(full["binding"]), core.digest(legacy["binding"]))
        self.assertEqual(legacy["binding"][-1], EMAIL)

    def test_explicit_ccs_root_and_host_environment_is_not_read(self):
        custom = self.home / "custom-ccs"
        write(custom / "codex-instances/saved/auth.json", codex_auth())
        with mock.patch.dict(os.environ, {"CCS_HOME": str(custom)}):
            self.assertEqual(core.inventory("codex", self.home, "ubuntu"), [])
            rows = core.inventory("codex", self.home, "ubuntu", ccs_dir=custom)
        self.assertEqual(len(rows), 1)
        self.assertIn("codex-profile:saved", rows[0]["selectors"])

    def test_claude_cli_default_saved_and_t3_candidates(self):
        roots = [self.home / ".claude", self.home / ".ccs/instances/saved", self.home / ".claude-t3/t3"]
        for root in roots:
            claude_files(self.home, root)
        for platform in ("ubuntu", "windows", "mac"):
            with mock.patch.object(core, "_mac_default_keychain", return_value=None):
                rows = core.inventory("claude", self.home, platform)
            self.assertEqual(len(rows), 3)
            self.assertEqual(len({row["identity"]["key"] for row in rows}), 1)
            self.assertIn("claude-profile:saved", rows[1]["selectors"])

    def test_claude_requires_account_and_organization_metadata(self):
        claude_files(self.home)
        write(self.home / ".claude.json", {"oauthAccount": {"accountUuid": ACCOUNT, "emailAddress": EMAIL}})
        self.assertEqual(core.inventory("claude", self.home, "ubuntu"), [])

    def test_mac_default_keychain_uses_same_identity_and_private_revision(self):
        claude_files(self.home)
        credentials = {"claudeAiOauth": {"accessToken": "fixture-keychain-private", "subscriptionType": "max"}}
        with mock.patch.object(core, "_mac_default_keychain", return_value=credentials):
            rows = core.inventory("claude", self.home, "mac")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["identity"], rows[1]["identity"])
        self.assertNotEqual(rows[0]["locator"], rows[1]["locator"])

    def test_windows_desktop_and_cli_match_by_account_and_organization(self):
        claude_files(self.home)
        root = self.home / "AppData/Roaming/Claude-work"
        write(root / "config.json", {})
        write(root / "Local State", {})
        write(self.home / ".ccs/claude-desktop-profiles.json", {"version": 1, "profiles": [
            {"id": "work", "email": EMAIL, "windows": {"profilePath": str(root), "launcherName": "Claude"}}]})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["identity"], rows[1]["identity"])
        self.assertEqual(set(rows[1]["selectors"]), {"claude-desktop-profile:work", "claude-profile:work"})
        self.assertFalse(any(selector.startswith("claude-desktop-profile:") for selector in rows[0]["selectors"]))

    def test_windows_desktop_native_roots_do_not_require_copied_manifest(self):
        root = self.home / "AppData/Roaming/Claude-work"
        write(root / "config.json", {})
        write(root / "Local State", {})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(len(rows), 1)
        self.assertEqual(set(rows[0]["selectors"]), {"claude-desktop-profile:work", "claude-profile:work"})
        self.assertIsNone(rows[0]["email"])
        client = FakeClient(profile(), {"five_hour": {"utilization": 0}})
        with mock.patch.object(core, "Client", return_value=client):
            result = core.collect(rows[0], self.home, "windows")
        self.assertEqual(result["sample"]["email"], EMAIL)
        self.assertIsNone(rows[0]["email"])

    def test_windows_desktop_keeps_alternate_oauth_clients_for_one_account(self):
        root = self.home / "AppData/Roaming/Claude-work"
        write(root / "config.json", {})
        write(root / "Local State", {})
        entries = [(2, [ACCOUNT, CLIENT, ORG], TOKEN), (1, [ACCOUNT, OTHER, ORG], TOKEN + "-second")]
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=entries):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["identity"], rows[1]["identity"])
        self.assertNotEqual(rows[0]["locator"], rows[1]["locator"])
        self.assertNotEqual(rows[0]["revision"], rows[1]["revision"])

    def test_named_default_desktop_profile_keeps_native_default_selectors(self):
        root = core.claude_usage.windows_profile_path("default", self.home, True)
        write(root / "config.json", {})
        write(root / "Local State", {})
        write(self.home / ".ccs/claude-desktop-profiles.json", {"version": 1, "profiles": [
            {"id": "work", "email": EMAIL, "windows": {"isDefault": True, "launcherName": "Claude"}}]})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(1, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(len(rows), 1)
        self.assertTrue({"discover", "claude-desktop-profile:default", "claude-desktop-profile:work"}.issubset(rows[0]["selectors"]))
        self.assertEqual(rows[0]["_profile"], "work")

    def test_explicit_desktop_profile_outside_home_is_allowed_without_general_discovery(self):
        with tempfile.TemporaryDirectory() as external:
            root = Path(external) / "ApprovedProfile"
            write(root / "config.json", {})
            write(root / "Local State", {})
            with mock.patch.object(core.sys, "platform", "win32"), \
                    mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})) as decrypt, \
                    mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(1, [ACCOUNT, CLIENT, ORG], TOKEN)]):
                self.assertEqual(core.inventory("claude", self.home, "windows"), [])
                decrypt.assert_not_called()
                write(self.home / ".ccs/claude-desktop-profiles.json", {"version": 1, "profiles": [
                    {"id": "work", "email": EMAIL, "windows": {"profilePath": str(root), "launcherName": "Claude"}}]})
                rows = core.inventory("claude", self.home, "windows")
            self.assertEqual(len(rows), 1)
            self.assertIn("claude-desktop-profile:work", rows[0]["selectors"])
            decrypt.assert_called_once_with(root.resolve())

    def test_server_selected_desktop_context_works_without_a_remote_manifest(self):
        with tempfile.TemporaryDirectory() as external:
            root = Path(external) / "ApprovedProfile"
            write(root / "config.json", {})
            write(root / "Local State", {})
            selected = {"id": "work", "email": EMAIL, "profilePath": str(root), "isDefault": False}
            with mock.patch.object(core.sys, "platform", "win32"), \
                    mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})) as decrypt, \
                    mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
                rows = core.inventory("claude", self.home, "windows", claude_profile=selected)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["email"], EMAIL)
            self.assertIn("claude-desktop-profile:work", rows[0]["selectors"])
            self.assertFalse((self.home / ".ccs/claude-desktop-profiles.json").exists())
            decrypt.assert_called_once_with(root.resolve())

    def test_selected_default_folder_has_default_alias_without_a_manifest_flag(self):
        root = self.home / "AppData/Roaming/Claude"
        write(root / "config.json", {})
        write(root / "Local State", {})
        selected = {"id": "work", "email": EMAIL, "profilePath": str(root)}
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows", claude_profile=selected)
        self.assertEqual(len(rows), 1)
        self.assertTrue({"claude-desktop-profile:default", "claude-desktop-profile:work"}.issubset(rows[0]["selectors"]))

    def test_selected_named_profile_uses_native_path_when_optional_context_is_omitted(self):
        root = core.claude_usage.windows_profile_path("work", self.home)
        write(root / "config.json", {})
        write(root / "Local State", {})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[(1, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows", claude_profile={"id": "work", "email": EMAIL})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["email"], EMAIL)

    def test_configured_desktop_paths_reject_shares_reparse_points_and_symlink_ancestors(self):
        for supplied in ("//server/share/Profile", "\\\\server\\share\\Profile"):
            self.assertIsNone(core._configured_windows_profile_root(Path(supplied)))
        with tempfile.TemporaryDirectory() as external:
            root = Path(external) / "ApprovedProfile"
            write(root / "config.json", {})
            write(root / "Local State", {})
            original = Path.lstat

            def reparse(value, *args, **kwargs):
                info = original(value, *args, **kwargs)
                if value == root:
                    return mock.Mock(st_mode=info.st_mode, st_file_attributes=1024)
                return info

            with mock.patch.object(Path, "lstat", new=reparse):
                self.assertIsNone(core._configured_windows_profile_root(root))
            linked = Path(external) / "Linked"
            try:
                linked.symlink_to(root, target_is_directory=True)
            except OSError:
                self.skipTest("OS does not permit fixture symlinks")
            self.assertIsNone(core._configured_windows_profile_root(linked))
            nested = root / "Nested"
            write(nested / "config.json", {})
            write(nested / "Local State", {})
            self.assertIsNone(core._configured_windows_profile_root(linked / "Nested"))
            (root / "config.json").unlink()
            (root / "config.json").symlink_to(nested / "config.json")
            self.assertIsNone(core._configured_windows_profile_root(root))

    def test_known_signed_out_desktop_reports_selector_without_fabricated_identity(self):
        root = self.home / "AppData/Roaming/Claude-work"
        write(root / "config.json", {})
        write(root / "Local State", {})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", side_effect=core.claude_usage.UsageUnavailable("needs_sign_in")):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(rows, [])
        self.assertEqual(set(rows.sign_in_needed_selectors), {"claude-profile:work", "claude-desktop-profile:work"})
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(rows, [])
        self.assertIn("claude-desktop-profile:work", rows.sign_in_needed_selectors)

    def test_unreadable_or_absent_desktop_does_not_claim_sign_in_is_needed(self):
        root = self.home / "AppData/Roaming/Claude-work"
        write(root / "config.json", {})
        write(root / "Local State", {})
        for failure in (core.claude_usage.UsageUnavailable(), ImportError("cryptography"), OSError("private")):
            with mock.patch.object(core.sys, "platform", "win32"), \
                    mock.patch.object(core.claude_usage, "decrypt_cache", side_effect=failure):
                rows = core.inventory("claude", self.home, "windows")
            self.assertEqual(rows.sign_in_needed_selectors, [])
        (root / "config.json").unlink()
        with mock.patch.object(core.sys, "platform", "win32"):
            rows = core.inventory("claude", self.home, "windows")
        self.assertEqual(rows.sign_in_needed_selectors, [])

    def test_malformed_oversized_and_external_setting_paths_are_ignored(self):
        root = self.home / ".codex"
        root.mkdir()
        (root / "auth.json").write_text("{" + "x" * (core.MAX_BYTES + 1))
        write(self.home / ".t3/userdata/settings.json", {"providerInstances": {
            "one": {"driver": "codex", "config": {"homePath": str(self.home.parent / "external")}}}})
        self.assertEqual(core.inventory("codex", self.home, "ubuntu"), [])

    def test_links_cannot_escape_home(self):
        with tempfile.TemporaryDirectory() as other:
            write(Path(other) / "auth.json", codex_auth())
            try:
                (self.home / ".codex").symlink_to(other, target_is_directory=True)
            except OSError:
                self.skipTest("OS does not permit fixture symlinks")
            self.assertEqual(core.inventory("codex", self.home, "ubuntu"), [])


class WindowsProfileRelocationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.legacy = self.home / "AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude"
        self.current = self.home / "AppData/Local/Claude-Data/claude_pzs8sxrjxfjjc"
        self.selected = {"id": "work", "email": EMAIL, "profilePath": str(self.legacy), "isDefault": True}

    def inventory(self, selected=None, failure=None):
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {}), side_effect=failure) as decrypt, \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[
                    (time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            rows = core.inventory("claude", self.home, "windows", claude_profile=selected)
        return rows, decrypt

    def filesystem_alias(self, root):
        if os.name == "nt":
            import ctypes.wintypes as wt
            api = core.ctypes.WinDLL("kernel32", use_last_error=True).GetShortPathNameW
            api.argtypes = [wt.LPCWSTR, wt.LPWSTR, wt.DWORD]
            api.restype = wt.DWORD
            buffer = core.ctypes.create_unicode_buffer(32768)
            if not api(str(root), buffer, len(buffer)) or Path(buffer.value) == root:
                self.skipTest("Fixture volume does not provide distinct Windows short names")
            return Path(buffer.value), nullcontext()
        # Model only filesystem alias resolution on POSIX; all validation,
        # ownership, decryption selection and protocol code remain real.
        alias = root.parent / ("ALIAS-" + root.name)
        alias.mkdir(exist_ok=True)
        original = Path.resolve

        def resolve(path, *args, **kwargs):
            return original(root, *args, **kwargs) if path == alias else original(path, *args, **kwargs)

        return alias, mock.patch.object(Path, "resolve", new=resolve)

    def test_selected_legacy_default_recovers_new_login_without_rewriting_manifest(self):
        for root in (self.legacy, self.current):
            write(root / "config.json", {})
            write(root / "Local State", {})
        rows, decrypt = self.inventory(self.selected)
        self.assertEqual(len(rows), 1)
        decrypt.assert_called_once_with(self.current.resolve())
        self.assertEqual(rows[0]["email"], EMAIL)
        self.assertEqual(rows[0]["_profile"], "work")
        self.assertTrue({"discover", "claude-desktop-profile:default", "claude-desktop-profile:work"}.issubset(rows[0]["selectors"]))
        self.assertFalse((self.home / ".ccs/claude-desktop-profiles.json").exists())
        client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
        with mock.patch.object(core, "Client", return_value=client), \
                mock.patch.object(core, "_claude_optional_windows", return_value=([], {})):
            result = core.collect(rows[0], self.home, "windows")
        self.assertIsNone(result["failureCode"])
        self.assertEqual(result["sample"]["email"], EMAIL)
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 7)

    def test_new_default_is_discovered_without_manifest_or_server_context(self):
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        rows, decrypt = self.inventory()
        self.assertEqual(len(rows), 1)
        decrypt.assert_called_once_with(self.current)
        self.assertIn("claude-desktop-profile:default", rows[0]["selectors"])

    def test_old_default_still_works_when_current_directory_absent(self):
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})
        rows, decrypt = self.inventory(self.selected)
        self.assertEqual(len(rows), 1)
        decrypt.assert_called_once_with(self.legacy.resolve())

    def test_empty_signed_out_and_unreadable_current_default_never_retry_old_login(self):
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})
        self.current.mkdir(parents=True)
        rows, decrypt = self.inventory(self.selected)
        self.assertEqual(rows, [])
        decrypt.assert_not_called()
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        for failure in [core.claude_usage.UsageUnavailable("needs_sign_in"), RuntimeError("private-fixture-error")]:
            with self.subTest(failure=type(failure).__name__):
                rows, decrypt = self.inventory(self.selected, failure)
                self.assertEqual(rows, [])
                decrypt.assert_called_once_with(self.current.resolve())

    def test_named_profile_does_not_inherit_default_selectors_or_expected_email(self):
        named = self.home / "AppData/Roaming/Claude-named"
        for root in (self.current, named):
            write(root / "config.json", {})
            write(root / "Local State", {})
        selected = {"id": "named", "email": EMAIL, "profilePath": str(named)}
        rows, decrypt = self.inventory(selected)
        self.assertEqual(len(rows), 2)
        named_row = next(row for row in rows if row["_profile"] == "named")
        default_row = next(row for row in rows if row["_profile"] == "default")
        self.assertEqual(set(named_row["selectors"]), {"claude-profile:named", "claude-desktop-profile:named"})
        self.assertIsNone(default_row["email"])
        self.assertEqual(named_row["email"], EMAIL)
        self.assertEqual({call.args[0] for call in decrypt.call_args_list}, {self.current, named.resolve()})

    def test_relocated_login_still_rejects_email_account_and_organization_mismatch(self):
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        rows, _ = self.inventory(self.selected)
        for live in [profile(account={"uuid": ACCOUNT, "email": "other@example.com"}),
                     profile(account={"uuid": OTHER, "email": EMAIL}), profile(organization={"uuid": OTHER})]:
            client = FakeClient(live)
            with mock.patch.object(core, "Client", return_value=client):
                result = core.collect(rows[0], self.home, "windows")
            self.assertEqual(result["failureCode"], "identity_mismatch")
            self.assertIsNone(result["sample"])
            self.assertEqual([call[0] for call in client.calls], [core.CLAUDE_PROFILE_URL])

    def test_unreadable_relocated_profile_is_incomplete_while_named_profile_still_works(self):
        import failover_usage
        named = self.home / "AppData/Roaming/Claude-named"
        for root in (self.current, named):
            write(root / "config.json", {})
            write(root / "Local State", {})
        original = Path.lstat

        def denied(path, *args, **kwargs):
            if path == self.current:
                raise PermissionError("fixture-private-error")
            return original(path, *args, **kwargs)

        with mock.patch.object(Path, "lstat", denied), \
                mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})) as decrypt, \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[
                    (time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]):
            result = failover_usage.run({"version": 1, "action": "inventory", "provider": "claude",
                                        "homeDir": str(self.home), "claudeProfile": self.selected})
        decrypt.assert_called_once_with(named)
        self.assertEqual(len(result["candidates"]), 1)
        incomplete = set(result["incompleteSelectors"])
        self.assertTrue({"claude-desktop-profile:work", "claude-desktop-profile:default"}.issubset(incomplete))
        self.assertFalse({"*", "claude-profile:*", "claude-desktop-profile:named"} & incomplete)

    def test_absent_current_login_is_not_reported_as_an_unreadable_profile(self):
        import failover_usage
        self.current.mkdir(parents=True)
        with mock.patch.object(core.sys, "platform", "win32"):
            result = failover_usage.run({"version": 1, "action": "inventory", "provider": "claude",
                                        "homeDir": str(self.home), "claudeProfile": self.selected})
        self.assertEqual(result["candidates"], [])
        self.assertNotIn("incompleteSelectors", result)

    def test_server_selected_binding_wins_relocated_remote_manifest_alias_collision(self):
        for root in (self.legacy, self.current):
            write(root / "config.json", {})
            write(root / "Local State", {})
        alias = self.current.parent / ".." / self.current.parent.name / self.current.name
        paths = [str(self.current), str(alias)]
        if os.name == "nt":
            paths.append(str(self.current).upper())
        for path in paths:
            with self.subTest(path=path):
                write(self.home / ".ccs/claude-desktop-profiles.json", {"version": 1, "profiles": [
                    {"id": "other", "email": "other@example.com", "windows": {"profilePath": path}}]})
                rows, decrypt = self.inventory(self.selected)
                self.assertEqual(len(rows), 1)
                decrypt.assert_called_once_with(self.current.resolve())
                self.assertEqual(rows[0]["email"], EMAIL)
                self.assertEqual(rows[0]["_profile"], "work")
                self.assertNotIn("claude-desktop-profile:other", rows[0]["selectors"])
                self.assertNotIn("claude-profile:other", rows[0]["selectors"])
                client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
                with mock.patch.object(core, "Client", return_value=client), \
                        mock.patch.object(core, "_claude_optional_windows", return_value=([], {})):
                    result = core.collect(rows[0], self.home, "windows")
                self.assertIsNone(result["failureCode"])
                self.assertEqual(result["sample"]["email"], EMAIL)

    def test_selected_canonical_alias_keeps_expected_email_through_public_protocol(self):
        import failover_usage
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        alias = self.current.parent / ".." / self.current.parent.name / self.current.name
        selected = {**self.selected, "profilePath": str(alias)}
        request = {"version": 1, "action": "inventory", "provider": "claude",
                   "homeDir": str(self.home), "claudeProfile": selected}
        client = FakeClient(profile(account={"uuid": ACCOUNT, "email": "other@example.com"}))
        with mock.patch.object(core.sys, "platform", "win32"), \
                mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                mock.patch.object(core.claude_usage, "eligible_entries", return_value=[
                    (time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]), \
                mock.patch.object(core, "Client", return_value=client):
            inventory = failover_usage.run(request)
            self.assertEqual(len(inventory["candidates"]), 1)
            self.assertEqual(inventory["candidates"][0]["email"], EMAIL)
            result = failover_usage.run({**request, "action": "collect", "candidate": inventory["candidates"][0]})
        self.assertEqual(result["failureCode"], "identity_mismatch")
        self.assertIsNone(result["sample"])
        self.assertEqual([call[0] for call in client.calls], [core.CLAUDE_PROFILE_URL])
        self.assertNotIn(str(self.home), json.dumps(result))

    def test_selected_short_current_or_empty_legacy_alias_keeps_binding_before_quota(self):
        import failover_usage
        self.legacy.mkdir(parents=True)
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        for root in [self.current, self.legacy]:
            alias, resolution = self.filesystem_alias(root)
            selected = {**self.selected, "profilePath": str(alias)}
            request = {"version": 1, "action": "inventory", "provider": "claude",
                       "homeDir": str(self.home), "claudeProfile": selected}
            client = FakeClient(profile(account={"uuid": ACCOUNT, "email": "other@example.com"}))
            with self.subTest(root=root), resolution, \
                    mock.patch.object(core.sys, "platform", "win32"), \
                    mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})) as decrypt, \
                    mock.patch.object(core.claude_usage, "eligible_entries", return_value=[
                        (time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]), \
                    mock.patch.object(core, "Client", return_value=client):
                inventory = failover_usage.run(request)
                self.assertEqual(len(inventory["candidates"]), 1)
                candidate = inventory["candidates"][0]
                self.assertEqual(candidate["email"], EMAIL)
                self.assertIn("claude-desktop-profile:work", candidate["selectors"])
                result = failover_usage.run({**request, "action": "collect", "candidate": candidate})
                self.assertTrue(all(call.args[0] == self.current.resolve() for call in decrypt.call_args_list))
            self.assertEqual(result["failureCode"], "identity_mismatch")
            self.assertIsNone(result["sample"])
            self.assertEqual([call[0] for call in client.calls], [core.CLAUDE_PROFILE_URL])

    def test_remote_short_alias_cannot_inherit_selected_profile_selectors(self):
        write(self.current / "config.json", {})
        write(self.current / "Local State", {})
        alias, resolution = self.filesystem_alias(self.current)
        write(self.home / ".ccs/claude-desktop-profiles.json", {"version": 1, "profiles": [
            {"id": "other", "email": "other@example.com", "windows": {"profilePath": str(alias)}}]})
        with resolution:
            rows, decrypt = self.inventory(self.selected)
        self.assertEqual(len(rows), 1)
        decrypt.assert_called_once_with(self.current.resolve())
        self.assertEqual(rows[0]["email"], EMAIL)
        self.assertEqual(rows[0]["_profile"], "work")
        self.assertFalse({"claude-profile:other", "claude-desktop-profile:other"} & set(rows[0]["selectors"]))

    def test_public_protocol_keeps_optional_429_hold_when_profile_relocates(self):
        import failover_usage
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})
        capsule = self.home / ".ccs/claude-session-migration/work-source.dpapi"
        capsule.parent.mkdir(parents=True)
        capsule.write_bytes(b"fixture-private-capsule")
        web_account = {"uuid": ACCOUNT, "email_address": EMAIL,
                       "memberships": [{"organization": {"uuid": ORG}}]}
        request = {"version": 1, "action": "inventory", "provider": "claude",
                   "homeDir": str(self.home), "claudeProfile": self.selected}
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
                        raise core.claude_usage.UsageUnavailable("rate_limited", retry)

                    web.get.side_effect = get
                    with mock.patch.object(core.sys, "platform", "win32"), \
                            mock.patch.object(core.claude_usage, "decrypt_cache", return_value=(ACCOUNT, {})), \
                            mock.patch.object(core.claude_usage, "eligible_entries", return_value=[
                                (time.time() * 1000 + 3600000, [ACCOUNT, CLIENT, ORG], TOKEN)]), \
                            mock.patch.object(core, "Client", return_value=native), \
                            mock.patch.object(core.claude_usage, "capsule_cookies", return_value=[]), \
                            mock.patch.object(core.claude_usage, "WebClient", return_value=web):
                        inventory = failover_usage.run(request)
                        result = failover_usage.run({**request, "action": "collect", "candidate": inventory["candidates"][0]})
                    self.assertEqual(result["failureCode"], "source_changed")
                    self.assertTrue(result["rateLimited"])
                    self.assertIsNone(result["sample"])
                    if retry is None:
                        self.assertNotIn("retryAfterSeconds", result)
                    else:
                        self.assertEqual(result["retryAfterSeconds"], retry)
                    self.assertEqual(web.get.call_count, position + 1)
                    for private in [TOKEN, ACCOUNT, ORG, str(self.home), "fixture-private-capsule"]:
                        self.assertNotIn(private, json.dumps(result))
                    self.current.rmdir()

    def test_relocation_between_discovery_and_manifest_resolution_removes_legacy_candidate(self):
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})
        original = core.claude_usage.resolve_windows_profile_root
        calls = 0

        def relocate(path, home=None):
            nonlocal calls
            if path == self.legacy:
                calls += 1
                if calls == 2:
                    write(self.current / "config.json", {})
                    write(self.current / "Local State", {})
            return original(path, home)

        with mock.patch.object(core.claude_usage, "resolve_windows_profile_root", side_effect=relocate):
            rows, decrypt = self.inventory(self.selected)
        self.assertEqual(len(rows), 1)
        decrypt.assert_called_once_with(self.current.resolve())

    def test_relocation_during_inventory_decryption_discards_legacy_candidate(self):
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})

        def relocate(path):
            self.current.mkdir(parents=True)
            return ACCOUNT, {}

        rows, decrypt = self.inventory(self.selected, relocate)
        self.assertEqual(rows, [])
        decrypt.assert_called_once_with(self.legacy.resolve())

    def test_relocation_during_later_named_inventory_discards_already_built_legacy_row(self):
        named = self.home / "AppData/Roaming/Claude-named"
        for root in (self.legacy, named):
            write(root / "config.json", {})
            write(root / "Local State", {})

        def relocate(path):
            if path == named:
                self.current.mkdir(parents=True)
            return ACCOUNT, {}

        rows, _ = self.inventory(self.selected, relocate)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["_profile_root"], named)
        self.assertNotIn("claude-desktop-profile:default", rows[0]["selectors"])

    def test_relocation_before_or_during_provider_read_discards_superseded_candidate(self):
        write(self.legacy / "config.json", {})
        write(self.legacy / "Local State", {})
        for phase in ["before", "profile", "usage", "extras"]:
            with self.subTest(phase=phase):
                rows, _ = self.inventory(self.selected)
                client = FakeClient(profile(), {"five_hour": {"utilization": 7}})
                original_get = client.get

                def get(route, token):
                    if route.endswith("/" + phase):
                        self.current.mkdir(parents=True)
                    return original_get(route, token)

                def extras(*args):
                    if phase == "extras":
                        self.current.mkdir(parents=True)
                    return [], {}

                if phase == "before":
                    self.current.mkdir(parents=True)
                client.get = get
                with mock.patch.object(core, "Client", return_value=client), \
                        mock.patch.object(core, "_claude_optional_windows", side_effect=extras):
                    result = core.collect(rows[0], self.home, "windows")
                self.assertEqual(result["failureCode"], "source_changed")
                self.assertIsNone(result["sample"])
                self.assertEqual(len(client.calls), {"before": 0, "profile": 1, "usage": 2, "extras": 2}[phase])
                self.current.rmdir()

    def test_other_providers_and_platforms_do_not_resolve_windows_desktop_paths(self):
        write(self.home / ".codex/auth.json", codex_auth())
        with mock.patch.object(core.claude_usage, "resolve_windows_profile_root", side_effect=AssertionError):
            self.assertEqual(len(core.inventory("codex", self.home, "windows")), 1)
            self.assertEqual(core.inventory("claude", self.home, "ubuntu"), [])
            with mock.patch.object(core, "_mac_default_keychain", return_value=None):
                self.assertEqual(core.inventory("claude", self.home, "mac"), [])


class CollectTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        write(self.home / ".codex/auth.json", codex_auth())
        claude_files(self.home)
        self.codex = core.inventory("codex", self.home, "ubuntu")[0]
        self.claude = core.inventory("claude", self.home, "ubuntu")[0]

    def collect(self, candidate, responses):
        client = FakeClient(*responses)
        with mock.patch.object(core, "Client", return_value=client):
            result = core.collect(candidate, self.home, "ubuntu")
        return result, client

    def test_claude_live_profile_must_match_before_usage(self):
        wrong = [profile(account={"uuid": ACCOUNT, "email": "other@example.com"}),
                 profile(account={"uuid": OTHER, "email": EMAIL}), profile(organization={"uuid": OTHER}),
                 profile(account={"email": EMAIL})]
        for value in wrong:
            result, client = self.collect(self.claude, [value])
            self.assertEqual(result, {"sample": None, "failureCode": "identity_mismatch"})
            self.assertEqual(len(client.calls), 1)

    def test_expired_native_claude_login_preserves_identity_without_requesting_provider(self):
        candidate = {**self.claude, "_expires": 1}
        result, client = self.collect(candidate, [])
        self.assertEqual(result, {"sample": None, "failureCode": "needs_sign_in"})
        self.assertEqual(client.calls, [])

    def test_claude_zero_is_preserved_and_raw_data_never_leaves(self):
        result, client = self.collect(self.claude, [profile(), {
            "five_hour": {"utilization": 0, "resets_at": "2026-10-16T13:30:00Z"},
            "private": "upstream-secret-marker"}])
        sample = result["sample"]
        self.assertEqual(sample["windows"][0]["usedPercent"], 0)
        self.assertEqual(sample["windows"][0]["remainingPercent"], 100)
        self.assertEqual(sample["status"], "ok")
        self.assertEqual(sample["plan"], "max")
        self.assertEqual(sample["sampledAt"], sample["fetchedAt"])
        self.assertEqual([call[0] for call in client.calls], [core.CLAUDE_PROFILE_URL, core.CLAUDE_USAGE_URL])
        for private in (TOKEN, ACCOUNT, ORG, "upstream-secret-marker", "fixture-upstream-private"):
            self.assertNotIn(private, json.dumps(result))

    def desktop_optional(self, optional, usage=None):
        candidate = {**self.claude, "_mode": "desktop", "_profile": "work"}
        client = FakeClient(profile(), usage if usage is not None else {"five_hour": {"utilization": 0}})
        with mock.patch.object(core, "Client", return_value=client), \
                mock.patch.object(core.claude_usage, "optional_web_windows", side_effect=optional) as collected:
            return core.collect(candidate, self.home, "windows"), collected

    def capsule(self):
        capsule = self.home / ".ccs/claude-session-migration/work-source.dpapi"
        capsule.parent.mkdir(parents=True, exist_ok=True)
        capsule.write_bytes(b"fixture-private-capsule")
        return capsule

    def test_windows_desktop_preserves_verified_optional_balances_with_bound_context(self):
        self.capsule()

        def optional(identifier, account, organization, home, availability, address, cooldown=None):
            self.assertEqual((identifier, account, organization, home, address), ("work", ACCOUNT, ORG, self.home, EMAIL))
            availability.update(resetCredits="ok", prepaidBalance="ok")
            return [core.claude_usage.balance_window("reset_credits_available", "Rate-limit resets available", 0, "resets"),
                    core.claude_usage.balance_window("prepaid_balance", "Prepaid balance", 0, "USD")]

        result, collected = self.desktop_optional(optional)
        self.assertEqual(collected.call_count, 1)
        self.assertEqual([row["key"] for row in result["sample"]["windows"]],
                         ["five_hour", "reset_credits_available", "prepaid_balance"])
        self.assertEqual(result["sample"]["optionalExtras"], {"resetCredits": "ok", "prepaidBalance": "ok"})
        self.assertRegex(result["sample"]["optionalExtrasContext"], r"^[a-f0-9]{64}$")
        self.assertNotIn("fixture-private-capsule", json.dumps(result))

    def test_optional_errors_do_not_drop_oauth_usage_or_fabricate_balances(self):
        self.capsule()
        result, _ = self.desktop_optional(ValueError("fixture-private-cookie"))
        self.assertEqual(len(result["sample"]["windows"]), 1)
        self.assertEqual(result["sample"]["optionalExtras"], {"resetCredits": "unavailable", "prepaidBalance": "unavailable"})
        self.assertRegex(result["sample"]["optionalExtrasContext"], r"^[a-f0-9]{64}$")
        self.assertNotIn("fixture-private-cookie", json.dumps(result))

    def test_optional_web_rate_limit_preserves_native_sample_and_stops_remaining_requests(self):
        self.capsule()
        web_account = {"uuid": ACCOUNT, "email_address": EMAIL,
                       "memberships": [{"organization": {"uuid": ORG}}]}
        reset = {"cedar_ember": {"eligible": True, "grants": []}}
        for position in range(3):
            with self.subTest(position=position):
                client = FakeClient(profile(), {"five_hour": {"utilization": 0}})
                web = mock.Mock()
                web.get.side_effect = [web_account, reset][:position] + [core.claude_usage.UsageUnavailable("rate_limited", 1800)]
                candidate = {**self.claude, "_mode": "desktop", "_profile": "work"}
                with mock.patch.object(core, "Client", return_value=client), \
                        mock.patch.object(core.claude_usage, "capsule_cookies", return_value=[]), \
                        mock.patch.object(core.claude_usage, "WebClient", return_value=web):
                    result = core.collect(candidate, self.home, "windows")
                self.assertEqual(result["failureCode"], "rate_limited")
                self.assertEqual(result["retryAfterSeconds"], 1800)
                self.assertEqual(web.get.call_count, position + 1)
                self.assertEqual(result["sample"]["status"], "ok")
                self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 0)
                self.assertEqual([window["key"] for window in result["sample"]["windows"]],
                                 ["five_hour", "reset_credits_available"] if position == 2 else ["five_hour"])
                self.assertEqual(result["sample"]["optionalExtras"], {
                    "resetCredits": "ok" if position == 2 else "unavailable", "prepaidBalance": "unavailable"})
                self.assertRegex(result["sample"]["optionalExtrasContext"], r"^[a-f0-9]{64}$")
                for private in (TOKEN, ACCOUNT, ORG, "fixture-private-capsule"):
                    self.assertNotIn(private, json.dumps(result))

    def test_optional_rate_limit_survives_changed_capsule_without_retaining_its_balances(self):
        capsule = self.capsule()

        def optional(*args):
            capsule.write_bytes(b"rotated-private-capsule")
            args[-1].update(failureCode="rate_limited", retryAfterSeconds=60)
            return [core.claude_usage.balance_window("prepaid_balance", "Prepaid balance", 10, "USD")]

        result, _ = self.desktop_optional(optional)
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assertEqual(result["retryAfterSeconds"], 60)
        self.assertEqual([window["key"] for window in result["sample"]["windows"]], ["five_hour"])
        self.assertNotIn("optionalExtrasContext", result["sample"])

    def test_optional_rate_limit_without_retry_after_still_holds_provider_and_preserves_quota(self):
        self.capsule()
        result, _ = self.desktop_optional(core.claude_usage.UsageUnavailable("rate_limited"))
        self.assertEqual(result["failureCode"], "rate_limited")
        self.assertNotIn("retryAfterSeconds", result)
        self.assertEqual(result["sample"]["status"], "ok")
        self.assertEqual(result["sample"]["windows"][0]["usedPercent"], 0)

    def test_rotated_or_missing_optional_cookie_context_never_retains_optional_reading(self):
        capsule = self.capsule()

        def rotate(*args):
            capsule.write_bytes(b"different-private-capsule")
            return [core.claude_usage.balance_window("prepaid_balance", "Prepaid balance", 10, "USD")]

        result, _ = self.desktop_optional(rotate)
        self.assertEqual(len(result["sample"]["windows"]), 1)
        self.assertNotIn("optionalExtrasContext", result["sample"])
        capsule.unlink()
        result, collected = self.desktop_optional(rotate)
        self.assertEqual(collected.call_count, 0)
        self.assertNotIn("optionalExtrasContext", result["sample"])

    def test_optional_cookie_read_waits_for_valid_oauth_usage(self):
        self.capsule()
        result, collected = self.desktop_optional(lambda *args: [], usage={})
        self.assertEqual(result, {"sample": None, "failureCode": "invalid_response"})
        self.assertEqual(collected.call_count, 0)

    def test_codex_weekly_primary_and_zero_credit_balance(self):
        result, client = self.collect(self.codex, [{"plan_type": "pro", "rate_limit": {
            "primary_window": {"used_percent": 0, "limit_window_seconds": 604800, "reset_at": 1792157400}},
            "credits": {"balance": "0", "has_credits": False, "unlimited": False},
            "rate_limit_reset_credits": {"available_count": 0}}])
        windows = result["sample"]["windows"]
        self.assertEqual(windows[0]["key"], "seven_day")
        self.assertEqual(windows[0]["usedPercent"], 0)
        self.assertEqual(windows[0]["windowMinutes"], 10080)
        self.assertEqual(windows[1]["remaining"], 0)
        self.assertEqual(windows[2]["remaining"], 0)
        self.assertEqual(client.calls, [(core.CODEX_URL, TOKEN, ACCOUNT)])

    def test_codex_missing_unknown_and_extra_windows_are_not_fabricated(self):
        result, _ = self.collect(self.codex, [{"rate_limit": {
            "primary_window": {"used_percent": 12, "limit_window_seconds": 3600}},
            "additional_rate_limits": [{"limit_name": "Spark", "rate_limit": {
                "primary_window": {"used_percent": 30, "limit_window_seconds": 18000}}}]}])
        windows = result["sample"]["windows"]
        self.assertEqual(windows[0]["key"], "primary_window")
        self.assertEqual(windows[0]["resetAt"], None)
        self.assertEqual(windows[1]["key"], "additional_0_five_hour")
        missing, _ = self.collect(self.codex, [{}])
        self.assertEqual(missing, {"sample": None, "failureCode": "invalid_response"})

    def test_upstream_rate_limit_is_failure_with_cooldown_not_zero(self):
        result, _ = self.collect(self.codex, [core.Failure("rate_limited", 120)])
        self.assertEqual(result, {"sample": None, "failureCode": "rate_limited", "retryAfterSeconds": 120})

    def test_unexpected_exceptions_do_not_expose_private_text(self):
        result, _ = self.collect(self.codex, [ValueError("Bearer " + TOKEN)])
        self.assertEqual(result, {"sample": None, "failureCode": "unavailable"})


class TransportTests(unittest.TestCase):
    def test_http_429_retry_after_is_preserved_without_reading_body(self):
        headers = Message()
        headers["Retry-After"] = "180"
        error = urllib.error.HTTPError(core.CODEX_URL, 429, "fixture-private-body", headers, io.BytesIO(b"secret"))
        opener = mock.Mock()
        opener.open.side_effect = error
        with mock.patch.object(core.urllib.request, "build_opener", return_value=opener):
            client = core.Client()
            with self.assertRaises(core.Failure) as caught:
                client.get(core.CODEX_URL, TOKEN, ACCOUNT)
        self.assertEqual(caught.exception.code, "rate_limited")
        self.assertEqual(caught.exception.retry_after, 180)
        self.assertNotIn("private", str(caught.exception))

    def test_retry_after_date_and_bounds(self):
        with mock.patch.object(core.time, "time", return_value=0):
            self.assertEqual(core._retry_after("Thu, 01 Jan 1970 00:01:30 GMT"), 90)
        self.assertEqual(core._retry_after("9999999999"), 86400)
        self.assertIsNone(core._retry_after("Bearer private"))

    def test_only_fixed_https_endpoints_are_allowed_and_redirects_are_denied(self):
        with mock.patch.object(core.urllib.request, "build_opener") as opener:
            client = core.Client()
            with self.assertRaises(core.Failure):
                client.get("https://example.invalid/", TOKEN)
        opener.return_value.open.assert_not_called()
        self.assertIsNone(core._NoRedirect().redirect_request(None, None, 302, "", {}, "https://example.invalid"))

    def test_deadline_prevents_next_provider_request(self):
        client = core.Client()
        client.deadline = time.monotonic() - 1
        with mock.patch.object(client.opener, "open") as opened:
            with self.assertRaises(core.Failure):
                client.get(core.CLAUDE_USAGE_URL, TOKEN)
        opened.assert_not_called()


class UnreadableStoreTests(unittest.TestCase):
    def test_path_check_permission_failure_is_counted_not_absent(self):
        import desktop_helpers
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            (home / ".codex").mkdir()
            (home / ".codex/auth.json").write_text("{}", encoding="utf-8")
            desktop_helpers.READ_FAILURES = 0
            self.assertIsNone(core._local_path(home, home / ".codex/missing.json"))
            self.assertEqual(desktop_helpers.READ_FAILURES, 0)
            original = Path.lstat
            def denied(path, *args, **kwargs):
                if path.name == ".codex":
                    raise PermissionError("private")
                return original(path, *args, **kwargs)
            with mock.patch.object(Path, "lstat", denied):
                self.assertIsNone(core._local_path(home, home / ".codex/auth.json"))
            self.assertGreater(desktop_helpers.READ_FAILURES, 0)


if __name__ == "__main__":
    unittest.main()
