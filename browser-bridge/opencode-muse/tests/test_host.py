import copy
import datetime as dt
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from types import SimpleNamespace

SOURCE = Path(__file__).resolve().parents[1] / "native-host"
sys.path.insert(0, str(SOURCE))
import host
import opencode_console as core

COOKIE = {"name": "__Host-console_session", "value": "synthetic-private-session",
          "domain": "opencode.ai", "path": "/", "secure": True, "hostOnly": True,
          "expirationDate": 2000000000}
WORKSPACE = "org_SYNTHETIC"
BILLING = {"billingMode": "prepaid", "mode": "pay-as-you-go", "balanceMicroCents": "1234567890"}
GO = {"access": {"endsAt": "2026-11-01T02:00:00Z", "meters": {
    "fiveHour": {"usedMicroCents": "10", "limitMicroCents": "100", "resetsAt": "2026-10-02T02:00:00Z"},
    "week": {"usedMicroCents": "20", "limitMicroCents": "100", "resetsAt": None},
    "month": {"usedMicroCents": "30", "limitMicroCents": "100"}}}}


class FakeClient:
    def __init__(self, orgs=None, billing=None, go=None, fail_go=False):
        self.orgs = [{"id": WORKSPACE}] if orgs is None else orgs
        self.billing = BILLING if billing is None else billing
        self.go = GO if go is None else go
        self.fail_go = fail_go
        self.calls = []

    def get(self, route, workspace=None):
        self.calls.append((route, workspace))
        if route == "/console/api/orgs":
            return self.orgs
        if route == "/console/api/billing/status":
            return self.billing
        if route == "/console/api/go/status":
            if self.fail_go:
                raise core.Unavailable("provider_error")
            return self.go
        raise AssertionError("Unexpected route")


class CookieTests(unittest.TestCase):
    def test_real_expiration_preserved(self):
        self.assertEqual(core.validate_cookies([COOKIE], now=1900000000), [COOKIE])

    def test_expired_is_missing_not_fake_login(self):
        with self.assertRaises(core.Unavailable) as caught:
            core.validate_cookies([COOKIE], now=2100000000)
        self.assertEqual(caught.exception.code, "no_browser_cookie")

    def test_session_cookie_keeps_no_invented_expiration(self):
        value = copy.deepcopy(COOKIE); value.pop("expirationDate")
        self.assertNotIn("expirationDate", core.validate_cookies([value])[0])

    def test_scope_rejections(self):
        for field, value in (("domain", "evil.opencode.ai"), ("name", "other"), ("path", "/console"),
                             ("secure", False), ("hostOnly", False), ("value", "x;secret"),
                             ("value", "x\nsecret"), ("value", "x secret"), ("value", "")):
            with self.subTest(field=field, value=value), self.assertRaises(core.Unavailable):
                row = copy.deepcopy(COOKIE); row[field] = value
                core.validate_cookies([row])

    def test_duplicate_rejected(self):
        with self.assertRaises(core.Unavailable):
            core.validate_cookies([COOKIE, COOKIE])

    def test_unknown_request_cookie_fields_rejected(self):
        value = copy.deepcopy(COOKIE); value["authorization"] = "secret"
        with self.assertRaises(core.Unavailable):
            core.validate_cookies([value])


class WorkspaceTests(unittest.TestCase):
    def test_only_workspace_without_url(self):
        self.assertEqual(core.select_workspace([{"id": WORKSPACE}], None), WORKSPACE)

    def test_multiple_require_selection(self):
        with self.assertRaises(core.Unavailable) as caught:
            core.select_workspace([{"id": WORKSPACE}, {"id": "wrk_OTHER"}], None)
        self.assertEqual(caught.exception.code, "choose_workspace")

    def test_valid_selected_member(self):
        self.assertEqual(core.select_workspace([{"id": WORKSPACE}, {"id": "wrk_OTHER"}], WORKSPACE), WORKSPACE)

    def test_selected_mismatch_is_denied(self):
        with self.assertRaises(core.Unavailable) as caught:
            core.select_workspace([{"id": WORKSPACE}], "wrk_OTHER")
        self.assertEqual(caught.exception.code, "workspace_mismatch")

    def test_invalid_or_duplicate_schema(self):
        for rows in ({"id": WORKSPACE}, [], [{"id": "../../secret"}], [{"id": WORKSPACE}, {"id": WORKSPACE}], [None]):
            with self.subTest(rows=rows), self.assertRaises(core.Unavailable):
                core.select_workspace(rows, None)

    def test_membership_denied_before_billing(self):
        client = FakeClient()
        with self.assertRaises(core.Unavailable):
            core.collect([COOKIE], "wrk_OTHER", client)
        self.assertEqual(client.calls, [("/console/api/orgs", None)])


class BalanceTests(unittest.TestCase):
    def test_signed_fractional_microcent_scaling(self):
        for raw, expected in (("1234567890", 12.3456789), ("0", 0), ("-123456789", -1.23456789), ("1", 0.00000001)):
            with self.subTest(raw=raw):
                value = dict(BILLING, balanceMicroCents=raw)
                self.assertEqual(core.balance_window(value)["remaining"], expected)

    def test_wallet_is_balance_not_percent_or_reset(self):
        row = core.balance_window(BILLING)
        self.assertIsNone(row["usedPercent"]); self.assertIsNone(row["resetAt"])
        self.assertIsNone(row["expiresAt"]); self.assertEqual(row["unit"], "USD")

    def test_actual_expiry_preserved(self):
        row = core.balance_window(dict(BILLING, expiresAt="2026-11-01T02:00:00-04:00"))
        self.assertEqual(row["expiresAt"], "2026-11-01T06:00:00.000Z")

    def test_malformed_expiry_not_invented(self):
        row = core.balance_window(dict(BILLING, expiresAt="tomorrow"))
        self.assertIsNone(row["expiresAt"])

    def test_bad_balance_schema(self):
        for raw in (None, True, 1, "1.5", "nan", "1e9", " 1", "1 USD", "1" * 21):
            with self.subTest(raw=raw), self.assertRaises(core.Unavailable):
                core.balance_window(dict(BILLING, balanceMicroCents=raw))

    def test_nonprepaid_mode_never_fabricates_zen_wallet(self):
        for changes in ({"mode": "invoiceable"}, {"billingMode": "seat"}, {"billingMode": "future"}):
            with self.subTest(changes=changes), self.assertRaises(core.Unavailable):
                core.balance_window(dict(BILLING, **changes))

    def test_independent_go_windows_actual_resets(self):
        rows = core.go_windows(GO)
        self.assertEqual([r["usedPercent"] for r in rows], [10, 20, 30])
        self.assertEqual(rows[0]["resetAt"], "2026-10-02T02:00:00.000Z")
        self.assertIsNone(rows[1]["resetAt"])
        self.assertEqual(rows[2]["resetAt"], "2026-11-01T02:00:00.000Z")

    def test_console_overage_survives_alongside_signed_wallet_and_resets(self):
        payload = copy.deepcopy(GO)
        payload["access"]["meters"]["fiveHour"].update(usedMicroCents="251", limitMicroCents="200")
        _, sample = core.collect([COOKIE], client=FakeClient(go=payload, billing=dict(BILLING, balanceMicroCents="-123456789")))
        rows = {row["key"]: row for row in sample["windows"]}
        self.assertEqual(rows["console-fiveHour"]["usedPercent"], 125.5)
        self.assertEqual(rows["console-fiveHour"]["remainingPercent"], 0)
        self.assertEqual(rows["console-fiveHour"]["resetAt"], "2026-10-02T02:00:00.000Z")
        self.assertEqual(rows["zen-balance"]["remaining"], -1.23456789)
        self.assertEqual(rows["zen-balance"]["unit"], "USD")
        self.assertEqual(json.loads(json.dumps(sample, allow_nan=False))["windows"][0]["usedPercent"], 125.5)

    def test_invalid_go_meter_values_do_not_fabricate_usage(self):
        for raw in ("-1", "NaN", "Infinity", "1e309", "1" * 21, True):
            with self.subTest(raw=raw):
                payload = copy.deepcopy(GO)
                payload["access"]["meters"]["fiveHour"]["usedMicroCents"] = raw
                _, sample = core.collect([COOKIE], client=FakeClient(go=payload))
                rows = {row["key"]: row for row in sample["windows"]}
                self.assertNotIn("console-fiveHour", rows)
                self.assertEqual(rows["zen-balance"]["remaining"], 12.3456789)

    def test_invalid_missing_go_does_not_hide_wallet(self):
        client = FakeClient(fail_go=True)
        workspace, sample = core.collect([COOKIE], client=client)
        self.assertEqual(workspace, WORKSPACE)
        self.assertEqual(len(sample["windows"]), 1)

    def test_unknown_go_meter_is_not_made_zero(self):
        payload = copy.deepcopy(GO); payload["access"]["meters"]["week"]["usedMicroCents"] = None
        self.assertNotIn("console-week", [row["key"] for row in core.go_windows(payload)])

    def test_public_sample_has_no_cookie_or_workspace_identifier(self):
        _, sample = core.collect([COOKIE], client=FakeClient())
        output = json.dumps(sample)
        self.assertNotIn(COOKIE["value"], output); self.assertNotIn(WORKSPACE, output)
        self.assertRegex(sample["id"], r"^plan-opencode-go-console-mac-[0-9a-f]{12}$")
        self.assertIn("identity has not been linked", sample["message"])


class CapsuleTests(unittest.TestCase):
    def test_roundtrip_owner_permissions(self):
        with tempfile.TemporaryDirectory() as folder:
            core.write_capsule(folder, [COOKIE], WORKSPACE)
            path = Path(folder) / core.CAPSULE_NAME
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(core.read_capsule(folder), ([COOKIE], WORKSPACE))

    def test_insecure_directory_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            os.chmod(folder, 0o755)
            with self.assertRaises(core.Unavailable):
                core.write_capsule(folder, [COOKIE], WORKSPACE)

    def test_insecure_file_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            core.write_capsule(folder, [COOKIE], WORKSPACE)
            os.chmod(Path(folder) / core.CAPSULE_NAME, 0o644)
            with self.assertRaises(core.Unavailable):
                core.read_capsule(folder)

    def test_symlink_not_followed(self):
        with tempfile.TemporaryDirectory() as folder:
            sentinel = Path(folder) / "sentinel"; sentinel.write_text("untouched")
            (Path(folder) / core.CAPSULE_NAME).symlink_to(sentinel)
            with self.assertRaises(core.Unavailable):
                core.write_capsule(folder, [COOKIE], WORKSPACE)
            with self.assertRaises(core.Unavailable):
                core.read_capsule(folder)
            self.assertEqual(sentinel.read_text(), "untouched")

    def test_expired_capsule_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            core.write_capsule(folder, [COOKIE], WORKSPACE)
            path = Path(folder) / core.CAPSULE_NAME; payload = json.loads(path.read_text())
            payload["capturedAt"] = "2000-01-01T00:00:00Z"; path.write_text(json.dumps(payload))
            with self.assertRaises(core.Unavailable):
                core.read_capsule(folder)

    def test_wrong_origin_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            core.write_capsule(folder, [COOKIE], WORKSPACE)
            path = Path(folder) / core.CAPSULE_NAME; payload = json.loads(path.read_text())
            payload["origin"] = "https://evil.example"; path.write_text(json.dumps(payload))
            with self.assertRaises(core.Unavailable):
                core.read_capsule(folder)


class NativeProtocolTests(unittest.TestCase):
    def test_native_frame_roundtrip(self):
        stream = io.BytesIO(); value = {"schemaVersion": 1, "action": "sync", "cookies": [COOKIE], "workspaceId": WORKSPACE}
        host.write_frame(stream, value); stream.seek(0)
        self.assertEqual(host.read_frame(stream), value); self.assertIsNone(host.read_frame(stream))

    def test_malformed_frames(self):
        for raw in (b"\x01", struct.pack("<I", 0), struct.pack("<I", core.MAX_BYTES + 1),
                    struct.pack("<I", 3) + b"{}", struct.pack("<I", 4) + b"xxxx"):
            with self.subTest(raw=raw), self.assertRaises(core.Unavailable):
                host.read_frame(io.BytesIO(raw))

    def test_invalid_request_never_saved(self):
        writes = []
        for request in ({"action": "purchase"}, {"schemaVersion": 1, "action": "sync", "cookies": [COOKIE], "url": "https://evil"}):
            with self.assertRaises(core.Unavailable):
                host.handle_request(request, "/unused", writer=lambda *args: writes.append(args))
        self.assertFalse(writes)

    def test_failed_verification_never_saved(self):
        writes = []
        def denied(*args): raise core.Unavailable("needs_sign_in")
        with self.assertRaises(core.Unavailable):
            host.handle_request({"schemaVersion": 1, "action": "sync", "cookies": [COOKIE]}, "/unused", denied, lambda *args: writes.append(args))
        self.assertFalse(writes)

    def test_complete_native_framed_child_e2e(self):
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder); (work / ".ccs/account-usage").mkdir(parents=True, mode=0o700)
            bootstrap = f'''import sys\nfrom pathlib import Path\nsys.path.insert(0,{str(SOURCE)!r})\nimport host,opencode_console as core\nPath.home=classmethod(lambda cls:Path({folder!r}))\nclass FixtureClient:\n def get(self,route,workspace=None):\n  return { {"/console/api/orgs": [{"id": WORKSPACE}], "/console/api/billing/status": BILLING, "/console/api/go/status": GO}!r}[route]\ncore.ConsoleClient=lambda cookies:FixtureClient()\nsys.argv=['host',host.ORIGIN]\nhost.main()\n'''
            request = {"schemaVersion": 1, "action": "sync", "cookies": [COOKIE], "workspaceId": WORKSPACE}
            encoded = json.dumps(request).encode()
            completed = subprocess.run([sys.executable, "-c", bootstrap], input=struct.pack("<I", len(encoded)) + encoded,
                                       capture_output=True, timeout=10, check=True)
            response = host.read_frame(io.BytesIO(completed.stdout))
            self.assertTrue(response["ok"])
            self.assertEqual(response["sample"]["windows"][-1]["remaining"], 12.3456789)
            self.assertNotIn(COOKIE["value"].encode(), completed.stdout)
            self.assertNotIn(WORKSPACE.encode(), completed.stdout)
            capsule = work / ".ccs/account-usage" / core.CAPSULE_NAME
            self.assertTrue(capsule.is_file()); self.assertEqual(capsule.stat().st_mode & 0o777, 0o600)
            self.assertEqual(completed.stderr, b"")

    def test_unapproved_native_origin_exits_without_response(self):
        result = subprocess.run([sys.executable, str(SOURCE / "host.py"), "chrome-extension://wrong/"],
                                input=b"", capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 1); self.assertEqual(result.stdout, b"")

    def test_collect_emits_no_network_for_wrong_route(self):
        client = core.ConsoleClient([COOKIE])
        for route in ("https://evil.example", "/console/api/billing/purchase", "/api/reset"):
            with self.subTest(route=route), self.assertRaises(core.Unavailable):
                client.get(route)


class HTTPBoundaryTests(unittest.TestCase):
    def run_request(self, status=200, body=b'[{"id":"org_SYNTHETIC"}]', route="/console/api/orgs", workspace=None):
        calls = []
        closed = []
        response = SimpleNamespace(status_code=status, iter_content=lambda **_: iter([body]), close=lambda: closed.append(True))
        fake = SimpleNamespace(requests=SimpleNamespace(get=lambda *args, **kwargs: calls.append((args, kwargs)) or response))
        with mock.patch.dict(sys.modules, {"curl_cffi": fake}):
            value = core.ConsoleClient([COOKIE]).get(route, workspace)
        return value, calls, closed

    def test_get_fixed_https_tls_and_no_redirects(self):
        value, calls, closed = self.run_request()
        self.assertEqual(value, [{"id": WORKSPACE}]); self.assertTrue(closed)
        args, options = calls[0]
        self.assertEqual(args, ("https://opencode.ai/console/api/orgs",))
        self.assertIs(options["verify"], True); self.assertIs(options["allow_redirects"], False)
        self.assertEqual(options["headers"]["Cookie"], "__Host-console_session=" + COOKIE["value"])
        self.assertNotIn("x-org-id", options["headers"])

    def test_workspace_header_only_for_exact_validated_routes(self):
        _, calls, _ = self.run_request(body=b'{}', route="/console/api/billing/status", workspace=WORKSPACE)
        self.assertEqual(calls[0][1]["headers"]["x-org-id"], WORKSPACE)
        for route, selected in (("/console/api/orgs", WORKSPACE), ("/console/api/billing/status", None),
                                ("/console/api/billing/status", "../secret")):
            with self.subTest(route=route, selected=selected), self.assertRaises(core.Unavailable):
                core.ConsoleClient([COOKIE]).get(route, selected)

    def test_denied_and_redirect_statuses_do_not_create_balance(self):
        for status in (301, 302, 401, 403, 404, 500):
            with self.subTest(status=status), self.assertRaises(core.Unavailable):
                self.run_request(status=status)

    def test_oversized_or_malformed_json_is_not_forwarded(self):
        for body in (b'x' * (core.MAX_BYTES + 1), b'token=private'):
            with self.subTest(size=len(body)), self.assertRaises(core.Unavailable):
                self.run_request(body=body)


if __name__ == "__main__":
    unittest.main()
