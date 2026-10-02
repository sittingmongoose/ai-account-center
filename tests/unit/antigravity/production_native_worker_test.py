"""Offline private worker protocol fixtures; temporary stores and invented tokens.

Never create UbuntuFileStore, probe a keyring, contact a provider, run a CLI, or
read the real home directory. SecureFileStore below addresses only an owned
TemporaryDirectory. Its Linux atomic exchanges change disposable fixture files.
"""
from __future__ import annotations

import base64
from contextlib import ExitStack
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
SOURCE = Path(__file__).resolve().parents[3] / "scripts/antigravity"
_import_path = list(sys.path)
_spec = importlib.util.spec_from_file_location(
    "aac_offline_private_native_worker", SOURCE / "native_credential_worker.py"
)
worker = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = worker
_spec.loader.exec_module(worker)
sys.path[:] = _import_path
auth = sys.modules["auth_platforms"]

FORMAT = "antigravity-consumer-json"
PRIVATE_FAILURE = "invented-private-worker-diagnostic-sentinel"
SAFE_STDERR = "Antigravity private store request failed safely.\n"
ORIGINAL_MODIFIED_NS = 1_660_000_000_123_456_789
JS_SAFE_INTEGER = (1 << 53) - 1


def private_fixture(name="original"):
    return json.dumps(
        {
            "auth_method": "consumer",
            "token": {
                "access_token": "invented-fixture-access-" + name,
                "refresh_token": "invented-fixture-refresh-" + name,
                "token_type": "Bearer",
                "expiry": "2030-01-01T00:00:00Z",
            },
        },
        separators=(",", ":"),
    ).encode()


def encoded(raw):
    return base64.b64encode(raw).decode()


class TrackingStore:
    def __init__(self, store):
        self.store = store
        self.calls = []

    def read_current(self):
        self.calls.append("read")
        return self.store.read_current()

    def install(self, replacement, before):
        self.calls.append("install")
        return self.store.install(replacement, before)

    def rollback(self, receipt):
        self.calls.append("rollback")
        return self.store.rollback(receipt)


class NativeWorkerProtocolTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="aac-native-worker-fixture-")
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name) / "fixture-home"
        self.home.mkdir(mode=0o700)
        directory = self.home / ".gemini/antigravity-cli"
        directory.mkdir(parents=True, mode=0o700)
        directory.parent.chmod(0o700)
        self.file = directory / "antigravity-oauth-token"
        self.original = private_fixture()
        self.replacement = private_fixture("replacement")
        self.foreign = private_fixture("foreign")
        self.file.write_bytes(self.original)
        self.file.chmod(0o600)
        os.utime(self.file, ns=(ORIGINAL_MODIFIED_NS, ORIGINAL_MODIFIED_NS))
        self.history = directory / "fixture-history.json"
        self.history.write_bytes(b'{"fixture":"history must remain untouched"}\n')
        self.history.chmod(0o600)
        self.history_before = self.file_identity(self.history)
        self.secure = auth.SecureFileStore(self.home)
        self.store = TrackingStore(self.secure)
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.constructor = self.stack.enter_context(
            patch.object(
                worker,
                "UbuntuFileStore",
                side_effect=AssertionError("default Ubuntu backend is forbidden in fixtures"),
            )
        )
        self.probe = self.stack.enter_context(
            patch.object(
                auth,
                "probe_ubuntu_backend",
                side_effect=AssertionError("keyring/native backend probe is forbidden"),
            )
        )

    def tearDown(self):
        self.assertEqual(self.file_identity(self.history), self.history_before)
        self.probe.assert_not_called()
        self.constructor.assert_not_called()

    @staticmethod
    def file_identity(filename):
        info = filename.lstat()
        return (
            info.st_dev,
            info.st_ino,
            info.st_mtime_ns,
            info.st_mode,
            info.st_nlink,
            filename.read_bytes(),
        )

    def install_packet(self, **changes):
        return {
            "operation": "install",
            "credentialBase64": encoded(self.replacement),
            "expectedFingerprint": auth.revision(self.original),
            **changes,
        }

    def install(self):
        return worker.execute(self.install_packet(), self.store)

    def rollback_packet(self, receipt, **changes):
        return {
            "operation": "rollback",
            "receipt": receipt,
            "previousCredentialBase64": encoded(self.original),
            **changes,
        }

    def main_with_fake_store(self, raw, store=None, home=None):
        output = io.BytesIO()
        diagnostic = io.StringIO()
        reads = []

        class BoundedStdin(io.BytesIO):
            def read(self, size=-1):
                reads.append(size)
                return super().read(size)

        constructor = Mock(return_value=store or self.store)
        with ExitStack() as stack:
            stack.enter_context(patch.object(worker, "UbuntuFileStore", constructor))
            stack.enter_context(
                patch.object(sys, "argv", ["offline-worker-fixture", "--home", home or str(self.home)])
            )
            stack.enter_context(patch.object(sys, "stdin", SimpleNamespace(buffer=BoundedStdin(raw))))
            stack.enter_context(patch.object(sys, "stdout", SimpleNamespace(buffer=output)))
            stack.enter_context(patch.object(sys, "stderr", diagnostic))
            status = worker.main()
        return status, output.getvalue(), diagnostic.getvalue(), constructor, reads

    def test_import_and_injected_store_do_not_construct_ubuntu_backend(self):
        self.assertEqual(self.store.calls, [])
        self.assertEqual(self.file.read_bytes(), self.original)
        self.constructor.assert_not_called()
        self.probe.assert_not_called()

    def test_exact_json_packet_at_65kb_boundary_is_allowed(self):
        raw = b'{"operation":"read"}'.ljust(worker.MAX_PACKET, b" ")
        self.assertEqual(len(raw), worker.MAX_PACKET)
        self.assertEqual(worker.bounded_json(raw), {"operation": "read"})
        self.assertEqual(self.store.calls, [])

    def test_packet_over_65kb_or_nonbytes_is_refused(self):
        for raw in (b'{"operation":"read"}'.ljust(worker.MAX_PACKET + 1, b" "),
                    "{}", bytearray(b"{}"), None):
            with self.subTest(kind=type(raw).__name__):
                with self.assertRaises(ValueError):
                    worker.bounded_json(raw)
        self.assertEqual(self.store.calls, [])

    def test_duplicate_packet_keys_are_refused_including_nested_receipts(self):
        for raw in (
            b'{"operation":"read","operation":"install"}',
            b'{"operation":"rollback","receipt":{"before":{},"before":{}}}',
            b'{"operation":"install","expectedFingerprint":"one","expectedFingerprint":"two"}',
        ):
            with self.subTest(raw_kind=len(raw)):
                with self.assertRaises(ValueError):
                    worker.bounded_json(raw)
        self.assertEqual(self.store.calls, [])

    def test_malformed_or_nonobject_json_is_refused(self):
        for raw in (b"", b"{", b"\xff", b"null", b"[]", b"4", b'"text"',
                    b'{"value":NaN}', b'{"value":Infinity}', b'{"value":-Infinity}', b"{}{}"):
            with self.subTest(raw_kind=len(raw)):
                with self.assertRaises((ValueError, UnicodeError)):
                    worker.bounded_json(raw)
        self.assertEqual(self.store.calls, [])

    def test_operations_require_exact_allowlisted_packet_keys(self):
        for packet in (
            {}, {"operation": "unknown"}, {"operation": "read", "extra": True},
            {"operation": "read", "home": str(self.home)},
            {"operation": "install", "credentialBase64": encoded(self.replacement)},
            {**self.install_packet(), "extra": True},
            {"operation": "rollback"},
        ):
            with self.subTest(operation=packet.get("operation")):
                with self.assertRaises(ValueError):
                    worker.execute(packet, self.store)
        self.assertEqual(self.store.calls, [])

    def test_base64_decode_is_exact_and_preserves_saved_bytes(self):
        value = worker.decode(encoded(self.original))
        self.assertEqual(value.raw, self.original)
        self.assertEqual(value.revision, auth.revision(self.original))
        padded = self.original.ljust(worker.MAX_CREDENTIAL, b" ")
        self.assertEqual(worker.decode(encoded(padded)).raw, padded)

    def test_base64_and_decoded_credentials_are_bounded(self):
        for value in (None, 1, [], "", "!!!!", "YQ", "YQ==\n", "f" * 22001,
                      encoded(self.original.ljust(worker.MAX_CREDENTIAL + 1, b" "))):
            with self.subTest(kind=type(value).__name__, size=len(value) if isinstance(value, str) else 0):
                with self.assertRaises(ValueError):
                    worker.decode(value)
        self.assertEqual(self.store.calls, [])

    def test_decoded_consumer_credential_schema_is_checked(self):
        for raw in (b"{}", b"[]", b'{"auth_method":"service_account"}',
                    b'{"auth_method":"consumer","token":{}}', self.original[:-1] + b',"unexpected":true}'):
            with self.subTest(size=len(raw)):
                with self.assertRaises(auth.AuthError):
                    worker.decode(encoded(raw))
        self.assertEqual(self.store.calls, [])

    def test_fingerprint_requires_exact_lowercase_sha256(self):
        digest = auth.revision(self.original)
        self.assertEqual(worker.fingerprint(digest), digest)
        for value in (None, 123, "", "0" * 63, "0" * 65, digest.upper(), "g" * 64):
            with self.subTest(kind=type(value).__name__):
                with self.assertRaises(ValueError):
                    worker.fingerprint(value)

    def test_read_returns_only_private_protocol_fields_and_does_not_write(self):
        before = self.file_identity(self.file)
        result = worker.execute({"operation": "read"}, self.store)
        self.assertEqual(result, {"format": FORMAT, "credentialBase64": encoded(self.original),
                                  "revision": auth.revision(self.original)})
        self.assertEqual(self.store.calls, ["read"])
        self.assertEqual(self.file_identity(self.file), before)

    def test_install_returns_exact_before_and_installed_receipt(self):
        before = self.secure.read_current()
        result = self.install()
        installed = self.secure.read_current()
        self.assertEqual(set(result), {"installedFingerprint", "rollbackState"})
        self.assertEqual(result["installedFingerprint"], auth.revision(self.replacement))
        self.assertEqual(result["rollbackState"], {
            "before": worker.snapshot_dto(before), "installed": worker.snapshot_dto(installed)})
        self.assertEqual(self.file.read_bytes(), self.replacement)
        self.assertEqual(self.file.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.store.calls, ["read", "install"])
        self.assertEqual(sorted(path.name for path in self.file.parent.iterdir()),
                         ["antigravity-oauth-token", "fixture-history.json"])

    def test_receipt_integer_identity_fields_roundtrip_as_strings_above_js_safe_integer(self):
        raw = auth.credential(self.original)
        values = (JS_SAFE_INTEGER + 123, (1 << 63) - 1, ORIGINAL_MODIFIED_NS)
        value = auth.FileSnapshot(raw, *values)
        dto = worker.snapshot_dto(value)
        for key, expected in zip(("device", "inode", "modifiedNs"), values):
            self.assertIs(type(dto[key]), str)
            self.assertEqual(dto[key], str(expected))
            self.assertGreater(expected, JS_SAFE_INTEGER)
        reparsed = worker.snapshot(json.loads(json.dumps(dto)))
        self.assertEqual(reparsed, value)
        self.assertEqual(worker.snapshot_dto(reparsed), dto)

    def test_actual_install_receipt_preserves_nanosecond_timestamp_without_js_rounding(self):
        result = self.install()
        before = result["rollbackState"]["before"]
        self.assertEqual(before["modifiedNs"], str(ORIGINAL_MODIFIED_NS))
        self.assertGreater(int(before["modifiedNs"]), JS_SAFE_INTEGER)
        self.assertEqual(worker.snapshot(before).modified_ns, ORIGINAL_MODIFIED_NS)

    def test_expected_revision_mismatch_performs_no_install(self):
        before = self.file_identity(self.file)
        with self.assertRaises(ValueError):
            worker.execute(self.install_packet(expectedFingerprint="0" * 64), self.store)
        self.assertEqual(self.store.calls, ["read"])
        self.assertEqual(self.file_identity(self.file), before)

    def test_malformed_expected_revision_or_replacement_is_refused_before_store_access(self):
        for packet in (self.install_packet(expectedFingerprint="not-sha256"),
                       self.install_packet(expectedFingerprint=42),
                       self.install_packet(credentialBase64="!!!!")):
            with self.subTest(expected_kind=type(packet["expectedFingerprint"]).__name__):
                with self.assertRaises(ValueError):
                    worker.execute(packet, self.store)
        self.assertEqual(self.store.calls, [])

    def test_rollback_exact_receipt_restores_previous_saved_bytes(self):
        receipt = self.install()["rollbackState"]
        result = worker.execute(self.rollback_packet(receipt), self.store)
        self.assertEqual(result, {"restored": True})
        self.assertEqual(self.file.read_bytes(), self.original)
        self.assertEqual(self.file.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.store.calls, ["read", "install", "rollback"])
        self.assertEqual(sorted(path.name for path in self.file.parent.iterdir()),
                         ["antigravity-oauth-token", "fixture-history.json"])

    def test_foreign_changed_byte_replacement_refuses_rollback_and_preserves_file(self):
        receipt = self.install()["rollbackState"]
        incoming = self.file.with_name("fixture-foreign-login")
        incoming.write_bytes(self.foreign)
        incoming.chmod(0o600)
        incoming.replace(self.file)
        foreign = self.file_identity(self.file)
        with self.assertRaises(auth.AuthError):
            worker.execute(self.rollback_packet(receipt), self.store)
        self.assertEqual(self.file_identity(self.file), foreign)
        self.assertEqual(self.file.read_bytes(), self.foreign)
        self.assertEqual(self.store.calls, ["read", "install", "rollback"])

    def test_foreign_same_byte_replacement_refuses_rollback_by_inode_identity(self):
        receipt = self.install()["rollbackState"]
        incoming = self.file.with_name("fixture-foreign-same-login")
        incoming.write_bytes(self.replacement)
        incoming.chmod(0o600)
        incoming.replace(self.file)
        foreign = self.file_identity(self.file)
        self.assertNotEqual(str(foreign[1]), receipt["installed"]["inode"])
        with self.assertRaises(auth.AuthError):
            worker.execute(self.rollback_packet(receipt), self.store)
        self.assertEqual(self.file_identity(self.file), foreign)
        self.assertEqual(self.file.read_bytes(), self.replacement)

    def test_previous_credential_mismatch_refuses_before_backend_rollback(self):
        receipt = self.install()["rollbackState"]
        installed = self.file_identity(self.file)
        with self.assertRaises(ValueError):
            worker.execute(self.rollback_packet(receipt, previousCredentialBase64=encoded(self.foreign)), self.store)
        self.assertEqual(self.store.calls, ["read", "install"])
        self.assertEqual(self.file_identity(self.file), installed)

    def test_receipt_requires_exact_structure_and_snapshot_keys(self):
        receipt = self.install()["rollbackState"]
        installed = self.file_identity(self.file)
        values = [None, [], {}, {"before": receipt["before"]}, {**receipt, "extra": True},
                  {**receipt, "before": {**receipt["before"], "extra": True}},
                  {**receipt, "installed": {key: value for key, value in receipt["installed"].items()
                                              if key != "modifiedNs"}}]
        for value in values:
            with self.subTest(kind=type(value).__name__):
                with self.assertRaises(ValueError):
                    worker.execute(self.rollback_packet(value), self.store)
        self.assertEqual(self.store.calls, ["read", "install"])
        self.assertEqual(self.file_identity(self.file), installed)

    def test_snapshot_integer_fields_refuse_numbers_negative_signs_and_overlong_digits(self):
        value = worker.snapshot_dto(self.secure.read_current())
        for key in ("device", "inode", "modifiedNs"):
            for invalid in (1, True, None, "", "-1", "+1", "1.0", " 1", "1" * 25):
                with self.subTest(key=key, kind=type(invalid).__name__):
                    with self.assertRaises(ValueError):
                        worker.snapshot({**value, key: invalid})
        self.assertEqual(self.store.calls, [])

    def test_installed_snapshot_identity_mismatch_cannot_overwrite_current_file(self):
        receipt = self.install()["rollbackState"]
        installed = self.file_identity(self.file)
        for key in ("device", "inode", "modifiedNs"):
            changed = {**receipt, "installed": {**receipt["installed"],
                                                key: str(int(receipt["installed"][key]) + 1)}}
            with self.subTest(key=key):
                with self.assertRaises(auth.AuthError):
                    worker.execute(self.rollback_packet(changed), self.store)
                self.assertEqual(self.file_identity(self.file), installed)
        self.assertEqual(self.file.read_bytes(), self.replacement)

    def test_main_reads_bounded_stdin_and_returns_private_json_using_only_injected_store(self):
        status, output, stderr, constructor, reads = self.main_with_fake_store(b'{"operation":"read"}')
        self.assertEqual(status, 0)
        self.assertEqual(stderr, "")
        self.assertEqual(json.loads(output), {"format": FORMAT, "credentialBase64": encoded(self.original),
                                              "revision": auth.revision(self.original)})
        self.assertLessEqual(len(output), worker.MAX_PACKET)
        self.assertEqual(reads, [worker.MAX_PACKET + 1])
        constructor.assert_called_once_with(self.home)
        self.assertEqual(self.store.calls, ["read"])

    def test_main_rejects_duplicate_or_oversize_packet_before_constructing_injected_store(self):
        for raw in (b'{"operation":"read","operation":"read"}',
                    b'{"operation":"read"}'.ljust(worker.MAX_PACKET + 1, b" ")):
            with self.subTest(size=len(raw)):
                status, output, stderr, constructor, reads = self.main_with_fake_store(raw)
                self.assertEqual((status, output, stderr), (1, b"", SAFE_STDERR))
                self.assertEqual(reads, [worker.MAX_PACKET + 1])
                constructor.assert_not_called()
        self.assertEqual(self.store.calls, [])

    def test_main_error_output_is_fixed_and_never_echoes_raw_backend_diagnostics(self):
        for kind in (auth.AuthError, OSError, ValueError, TypeError, KeyError):
            fake = SimpleNamespace(read_current=Mock(side_effect=kind(PRIVATE_FAILURE)))
            with self.subTest(kind=kind.__name__):
                status, output, stderr, constructor, _ = self.main_with_fake_store(b'{"operation":"read"}', fake)
                self.assertEqual((status, output, stderr), (1, b"", SAFE_STDERR))
                self.assertNotIn(PRIVATE_FAILURE, stderr)
                constructor.assert_called_once_with(self.home)
        self.assertEqual(self.store.calls, [])

    def test_main_output_over_65kb_is_refused_without_printing_private_bytes(self):
        fake = SimpleNamespace(read_current=Mock(return_value=SimpleNamespace(
            credential=auth.PrivateCredential(b"invented-fixture" * 5000, "0" * 64))))
        status, output, stderr, constructor, _ = self.main_with_fake_store(b'{"operation":"read"}', fake)
        self.assertEqual((status, output, stderr), (1, b"", SAFE_STDERR))
        constructor.assert_called_once_with(self.home)
        self.assertEqual(self.store.calls, [])

    def test_main_relative_home_is_refused_without_store_construction(self):
        status, output, stderr, constructor, reads = self.main_with_fake_store(b'{"operation":"read"}', home="fixture-relative-home")
        self.assertEqual((status, output, stderr), (1, b"", SAFE_STDERR))
        self.assertEqual(reads, [])
        constructor.assert_not_called()
        self.assertEqual(self.store.calls, [])


if __name__ == "__main__":
    unittest.main()
