"""key_store.py: the AAC-owned API key store helper (CONTRACT-registry-lifecycle 3.2).

Offline only: a temporary HOME, no network, no real ~/.ccs.
"""

import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


HELPERS = Path(__file__).resolve().parents[3] / "scripts" / "account-usage"
sys.path.insert(0, str(HELPERS))
import key_store  # noqa: E402
import plan_common as common  # noqa: E402

SECRET = "zai-TEST-key-0123456789-x7Qa"


class KeyStoreTests(unittest.TestCase):
    def setUp(self):
        self.home = Path(tempfile.mkdtemp(prefix="aac-key-store-"))

    def tearDown(self):
        for root, folders, files in os.walk(self.home, topdown=False):
            for name in files:
                os.unlink(os.path.join(root, name))
            for name in folders:
                path = os.path.join(root, name)
                os.unlink(path) if os.path.islink(path) else os.rmdir(path)
        os.rmdir(self.home)

    def test_put_writes_the_format_the_collector_reads(self):
        result = key_store.put("zai", "9f2c41d0", SECRET, home=self.home, windows=False)
        self.assertEqual(result, {"ok": True, "fingerprint": common.key_fingerprint(SECRET), "last4": "x7Qa"})
        folder = self.home / ".ccs" / "account-usage" / "keys"
        record_path = folder / "zai-9f2c41d0.json"
        self.assertEqual(stat.S_IMODE(os.stat(folder).st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(os.stat(record_path).st_mode), 0o600)
        self.assertEqual(sorted(os.listdir(folder)), ["zai-9f2c41d0.json"])
        record = json.loads(record_path.read_text())
        self.assertLessEqual(set(record), common.KEY_FILE_FIELDS)
        credential = common.aac_key_credential("zai", "9f2c41d0", home=self.home, windows=False)
        self.assertEqual(credential["secret"], SECRET)
        self.assertIsNone(common.aac_key_credential("zai", "00000000", home=self.home, windows=False))

    def test_put_tightens_a_loose_folder_and_replaces_atomically(self):
        folder = self.home / ".ccs" / "account-usage" / "keys"
        folder.mkdir(parents=True)
        os.chmod(folder, 0o755)
        key_store.put("kimi-code", "aaaaaaaa", "first-secret-1", home=self.home, windows=False)
        key_store.put("kimi-code", "aaaaaaaa", "second-secret-2", home=self.home, windows=False)
        self.assertEqual(stat.S_IMODE(os.stat(folder).st_mode), 0o700)
        credential = common.aac_key_credential("kimi-code", "aaaaaaaa", home=self.home, windows=False)
        self.assertEqual(credential["secret"], "second-secret-2")

    def test_refuses_a_symlinked_folder(self):
        target = self.home / "elsewhere"
        target.mkdir()
        (self.home / ".ccs" / "account-usage").mkdir(parents=True)
        os.symlink(target, self.home / ".ccs" / "account-usage" / "keys")
        with self.assertRaises(key_store.StoreError):
            key_store.put("zai", "9f2c41d0", SECRET, home=self.home, windows=False)
        self.assertEqual(os.listdir(target), [])

    def test_delete_is_idempotent(self):
        key_store.put("zai", "9f2c41d0", SECRET, home=self.home, windows=False)
        self.assertEqual(key_store.delete("zai", "9f2c41d0", home=self.home, windows=False), {"ok": True})
        self.assertEqual(key_store.delete("zai", "9f2c41d0", home=self.home, windows=False), {"ok": True})
        self.assertIsNone(common.aac_key_credential("zai", "9f2c41d0", home=self.home, windows=False))

    def test_windows_writes_the_raw_dpapi_blob_with_the_fixed_entropy(self):
        seen = []

        def protect(plaintext, entropy):
            seen.append((plaintext, entropy))
            return b"DPAPI:" + plaintext[::-1]

        with mock.patch.object(key_store, "_windows_protect", protect):
            key_store.put("zai", "9f2c41d0", SECRET, home=self.home, windows=True)
        folder = self.home / ".ccs" / "account-usage" / "keys"
        self.assertEqual(os.listdir(folder), ["zai-9f2c41d0.dpapi"])
        plaintext, entropy = seen[0]
        self.assertEqual(entropy, b"AAC/account-key/v1")
        self.assertEqual(json.loads(plaintext.decode("utf-8"))["secret"], SECRET)
        raw = (folder / "zai-9f2c41d0.dpapi").read_bytes()
        self.assertEqual(raw, b"DPAPI:" + plaintext[::-1])

    def test_read_secret_accepts_one_trailing_newline_only(self):
        self.assertEqual(key_store.read_secret(io.BytesIO(SECRET.encode() + b"\n")), SECRET)
        self.assertEqual(key_store.read_secret(io.BytesIO(SECRET.encode() + b"\r\n")), SECRET)
        for bad in (b"", b"short", b"has space inside", SECRET.encode() + b"\n\n", b"\xff" * 10,
                    b"x" * 5000):
            with self.assertRaises(key_store.StoreError):
                key_store.read_secret(io.BytesIO(bad))

    def test_command_line_reads_stdin_and_prints_only_the_summary(self):
        environment = dict(os.environ, HOME=str(self.home), PYTHONDONTWRITEBYTECODE="1")
        helper = str(HELPERS / "key_store.py")
        completed = subprocess.run(
            [sys.executable, helper, "put", "--provider", "zai", "--key-id", "9f2c41d0"],
            input=SECRET.encode(), capture_output=True, env=environment, timeout=30)
        self.assertEqual(completed.returncode, 0)
        self.assertEqual(json.loads(completed.stdout), {
            "ok": True, "fingerprint": common.key_fingerprint(SECRET), "last4": "x7Qa"})
        self.assertNotIn(SECRET.encode(), completed.stdout + completed.stderr)
        for argv, code in (
            (["put", "--provider", "zai", "--key-id", "NOTHEX00"], 2),
            (["put", "--provider", "qwen", "--key-id", "9f2c41d0"], 2),
            (["erase", "--provider", "zai", "--key-id", "9f2c41d0"], 2),
        ):
            failed = subprocess.run([sys.executable, helper, *argv], input=SECRET.encode(),
                                    capture_output=True, env=environment, timeout=30)
            self.assertEqual(failed.returncode, code)
            self.assertEqual(failed.stdout, b"")
        rejected = subprocess.run(
            [sys.executable, helper, "put", "--provider", "zai", "--key-id", "9f2c41d0"],
            input=b"short", capture_output=True, env=environment, timeout=30)
        self.assertEqual((rejected.returncode, rejected.stdout, rejected.stderr), (1, b"", b""))


if __name__ == "__main__":
    unittest.main()
