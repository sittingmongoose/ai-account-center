"""Public collector compatibility fixtures; invented private metadata only.

Only explicit temporary homes are supplied to the fixed metadata reader. No
real metadata/token store, provider request, process, new account or grant is
read or created. Refresh integration injects a fake request_json callback.
"""
from __future__ import annotations

from contextlib import ExitStack
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SOURCE = Path(__file__).resolve().parents[3] / "scripts/account-usage"
_import_path = list(sys.path)
sys.path.insert(0, str(SOURCE))
_spec = importlib.util.spec_from_file_location(
    "aac_offline_oauth_metadata_collector", SOURCE / "desktop_usage.py"
)
usage = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = usage
_spec.loader.exec_module(usage)
sys.path[:] = _import_path
helpers = sys.modules["desktop_helpers"]

CLIENT_ID = "invented-fixture-client.apps.googleusercontent.com"
APP_VALUE = "invented_fixture_app_value"
SAFE_FAILURE = "Antigravity's private app OAuth metadata is unavailable on this computer."


def metadata(discriminator="schemaVersion", version=1):
    return {discriminator: version, "clientId": CLIENT_ID, "clientSecret": APP_VALUE}


class OAuthMetadataCompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="aac-oauth-metadata-fixture-")
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name) / "fixture-home"
        self.home.mkdir(mode=0o700)
        self.path = self.home / ".ccs/account-usage/antigravity-oauth-client.json"
        self.path.parent.mkdir(parents=True, mode=0o700)
        self.path.parent.parent.chmod(0o700)
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.default_home = self.stack.enter_context(
            patch.object(usage.pathlib.Path, "home", side_effect=AssertionError("real home is forbidden"))
        )
        self.request = self.stack.enter_context(
            patch.object(usage, "request_json", side_effect=AssertionError("provider transport is forbidden"))
        )
        self.process = self.stack.enter_context(
            patch.object(usage.subprocess, "run", side_effect=AssertionError("native process is forbidden"))
        )

    def tearDown(self):
        self.default_home.assert_not_called()
        self.request.assert_not_called()
        self.process.assert_not_called()
        self.assertFalse((self.home / ".gemini").exists())

    def write(self, value=None, raw=None, mode=0o600):
        if self.path.exists():
            self.path.chmod(0o600)
        self.path.write_bytes(raw if raw is not None else json.dumps(metadata() if value is None else value).encode())
        self.path.chmod(mode)

    def file_identity(self, filename=None):
        filename = self.path if filename is None else filename
        info = filename.lstat()
        return (info.st_dev, info.st_ino, info.st_mtime_ns, info.st_mode,
                info.st_nlink, filename.read_bytes())

    def assert_unavailable(self):
        with self.assertRaises(helpers.UsageError) as caught:
            usage.antigravity_oauth_client(self.home)
        self.assertEqual(caught.exception.status, "unavailable")
        self.assertEqual(str(caught.exception), SAFE_FAILURE)
        for private in (CLIENT_ID, APP_VALUE, "invented-private-invalid-value"):
            self.assertNotIn(private, str(caught.exception))

    def test_exact_canonical_and_legacy_schema_are_read_without_metadata_writes(self):
        for discriminator in ("schemaVersion", "schema"):
            with self.subTest(discriminator=discriminator):
                self.write(metadata(discriminator))
                before = self.file_identity()
                self.assertEqual(usage.antigravity_oauth_client(self.home), (CLIENT_ID, APP_VALUE))
                self.assertEqual(self.file_identity(), before)

    def test_both_schema_discriminators_are_rejected(self):
        self.write({**metadata(), "schema": 1})
        before = self.file_identity()
        self.assert_unavailable()
        self.assertEqual(self.file_identity(), before)

    def test_missing_keys_and_unknown_discriminator_are_rejected(self):
        for value in ({}, {"clientId": CLIENT_ID, "clientSecret": APP_VALUE},
                      {"schemaVersion": 1, "clientId": CLIENT_ID},
                      {"schema": 1, "clientSecret": APP_VALUE},
                      {"version": 1, "clientId": CLIENT_ID, "clientSecret": APP_VALUE}):
            with self.subTest(keys=sorted(value)):
                self.write(value)
                self.assert_unavailable()

    def test_duplicate_discriminator_and_client_fields_are_rejected(self):
        for discriminator in ("schemaVersion", "schema"):
            for key, value in ((discriminator, 1), ("clientId", CLIENT_ID), ("clientSecret", APP_VALUE)):
                duplicate = json.dumps(metadata(discriminator))[:-1] + "," + json.dumps(key) + ":" + json.dumps(value) + "}"
                with self.subTest(discriminator=discriminator, key=key):
                    self.write(raw=duplicate.encode())
                    before = self.file_identity()
                    self.assert_unavailable()
                    self.assertEqual(self.file_identity(), before)

    def test_nested_duplicate_keys_are_also_rejected(self):
        raw = (b'{"schemaVersion":1,"clientId":"' + CLIENT_ID.encode()
               + b'","clientSecret":{"duplicate":"one","duplicate":"two"}}')
        self.write(raw=raw)
        self.assert_unavailable()

    def test_extra_provider_token_and_unknown_fields_are_rejected_for_each_schema(self):
        for discriminator in ("schemaVersion", "schema"):
            for key, value in (("provider", "cursor"), ("provider", "antigravity"),
                               ("accessToken", "invented-private-invalid-value"),
                               ("refreshToken", "invented-private-invalid-value"), ("extra", None)):
                with self.subTest(discriminator=discriminator, key=key):
                    self.write({**metadata(discriminator), key: value})
                    self.assert_unavailable()

    def test_schema_version_requires_integer_one_without_coercion(self):
        for discriminator in ("schemaVersion", "schema"):
            for version in (True, False, 1.0, "1", None, 0, 2, [], {}):
                with self.subTest(discriminator=discriminator, kind=type(version).__name__):
                    self.write(metadata(discriminator, version))
                    self.assert_unavailable()

    def test_nonjson_number_constants_are_rejected(self):
        for constant in ("NaN", "Infinity", "-Infinity"):
            with self.subTest(constant=constant):
                self.write(raw=(json.dumps(metadata()).replace('"schemaVersion": 1', '"schemaVersion": ' + constant)).encode())
                self.assert_unavailable()

    def test_invalid_client_fields_preserve_existing_regexes_and_type_guards(self):
        for key, values in (
            ("clientId", (None, True, 1, "", "other-provider-client", "https://other.example",
                          "fixture.apps.googleusercontent.com\n", "a" * 257 + ".apps.googleusercontent.com")),
            ("clientSecret", (None, True, 1, "", "invented private invalid value", "has.dot", "s" * 257)),
        ):
            for value in values:
                with self.subTest(key=key, kind=type(value).__name__):
                    self.write({**metadata(), key: value})
                    self.assert_unavailable()

    def test_existing_client_field_maximum_lengths_remain_supported(self):
        value = {**metadata(), "clientId": "a" * 256 + ".apps.googleusercontent.com",
                 "clientSecret": "s" * 256}
        self.write(value)
        self.assertEqual(usage.antigravity_oauth_client(self.home), (value["clientId"], value["clientSecret"]))

    def test_private_metadata_mode_is_required_without_changing_permissions(self):
        for mode in (0o644, 0o640, 0o400, 0o660):
            with self.subTest(mode=mode):
                self.write(mode=mode)
                before = self.file_identity()
                self.assert_unavailable()
                self.assertEqual(self.file_identity(), before)

    def test_wrong_file_owner_is_rejected_without_chown_or_other_mutation(self):
        self.write()
        before = self.file_identity()
        owner = os.getuid()
        with patch.object(usage.os, "getuid", return_value=owner + 1):
            self.assert_unavailable()
        self.assertEqual(self.file_identity(), before)

    def test_file_size_at_4096_bytes_remains_valid(self):
        raw = json.dumps(metadata()).encode().ljust(4096, b" ")
        self.write(raw=raw)
        before = self.file_identity()
        self.assertEqual(usage.antigravity_oauth_client(self.home), (CLIENT_ID, APP_VALUE))
        self.assertEqual(self.file_identity(), before)

    def test_file_size_above_4096_bytes_is_rejected_without_read_or_write_expansion(self):
        self.write(raw=json.dumps(metadata()).encode().ljust(4097, b" "))
        before = self.file_identity()
        self.assert_unavailable()
        self.assertEqual(self.file_identity(), before)

    def test_missing_or_nonregular_metadata_is_unavailable(self):
        self.assert_unavailable()
        self.path.mkdir(mode=0o700)
        self.assert_unavailable()
        self.assertTrue(self.path.is_dir())

    def test_leaf_symlink_is_rejected_and_its_target_is_unchanged(self):
        target = self.home / "invented-target-metadata.json"
        target.write_text(json.dumps(metadata()))
        target.chmod(0o600)
        self.path.symlink_to(target)
        before = self.file_identity(target)
        self.assert_unavailable()
        self.assertTrue(self.path.is_symlink())
        self.assertEqual(self.file_identity(target), before)

    def test_parent_directory_symlink_is_rejected_without_changing_target(self):
        target = self.home / "invented-target-directory"
        target.mkdir(mode=0o700)
        self.path.parent.rmdir()
        self.path.parent.symlink_to(target)
        self.write()
        actual_file = target / self.path.name
        before = self.file_identity(actual_file)
        self.assert_unavailable()
        self.assertTrue(self.path.parent.is_symlink())
        self.assertEqual(self.file_identity(actual_file), before)

    def test_malformed_nonobject_and_invalid_utf8_json_are_rejected(self):
        for raw in (b"", b"{", b"\xff", b"[]", b"null", b'"text"', b"1", b"{}{}"):
            with self.subTest(size=len(raw)):
                self.write(raw=raw)
                self.assert_unavailable()

    def test_deeply_nested_bounded_json_maps_recursion_error_to_fixed_unavailable(self):
        raw = (b'{"schemaVersion":1,"clientId":"' + CLIENT_ID.encode()
               + b'","clientSecret":' + b'[' * 1500 + b'0' + b']' * 1500 + b'}')
        self.assertLessEqual(len(raw), 4096)
        self.write(raw=raw)
        # The stdlib Python scanner reproduces its recursion guard across
        # interpreter versions whose optimized C scanner has different limits.
        with patch.object(json.scanner, "make_scanner", json.scanner.py_make_scanner):
            with self.assertRaises(RecursionError):
                json.loads(raw, object_pairs_hook=dict)
            self.assert_unavailable()

    def test_existing_refresh_grant_integration_for_each_schema_is_mocked_and_read_only(self):
        for discriminator in ("schemaVersion", "schema"):
            with self.subTest(discriminator=discriminator):
                self.write(metadata(discriminator))
                before = self.file_identity()
                original = helpers.Credential(
                    "invented-existing-access", "Fixture existing Antigravity login",
                    "invented-existing-refresh-grant", email="party@example.com",
                    project="fixture-project", plan="fixture-plan", google_client="antigravity"
                )
                original_fields = copy.deepcopy(vars(original))
                with patch.object(usage, "request_json", return_value={"access_token": "invented-renewed-access"}) as request:
                    renewed = usage.refresh_antigravity(original, self.home)
                request.assert_called_once_with("https://oauth2.googleapis.com/token", body={
                    "grant_type": "refresh_token", "refresh_token": original.refresh,
                    "client_id": CLIENT_ID, "client_secret": APP_VALUE,
                }, form=True)
                self.assertEqual(renewed.access, "invented-renewed-access")
                self.assertEqual(renewed.refresh, original.refresh)
                for key in ("source", "email", "project", "plan", "google_client"):
                    self.assertEqual(getattr(renewed, key), getattr(original, key))
                self.assertEqual(vars(original), original_fields)
                self.assertEqual(self.file_identity(), before)

    def test_refused_metadata_never_invokes_mocked_refresh_transport(self):
        self.write({**metadata(), "schema": 1})
        credential = helpers.Credential("invented-access", refresh="invented-refresh")
        with self.assertRaises(helpers.UsageError) as caught:
            usage.refresh_antigravity(credential, self.home)
        self.assertEqual(caught.exception.status, "unavailable")
        self.assertEqual(str(caught.exception), SAFE_FAILURE)
        self.request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
