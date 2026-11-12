"""No-network fixtures for T3 CLI discovery, temporary auth and Effect WS frames."""

import base64
import hashlib
import io
import json
import os
import pathlib
import struct
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.parse
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / "scripts/app-updates"))
import app_update_common as common
import app_update_t3_rpc as rpc


TOKEN = "private-rpc-fixture-secret"


class DiscoveryTests(unittest.TestCase):
    def test_unix_layouts_and_running_server_version(self):
        for host in ("mac", "ubuntu"):
            for standalone, desktop in ((True, False), (False, True), (True, True), (False, False)):
                with self.subTest(host=host, standalone=standalone, desktop=desktop), tempfile.TemporaryDirectory() as temporary, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(temporary)):
                    base = pathlib.Path(temporary) / ".t3"
                    state = base / "userdata/server-runtime.json"
                    state.parent.mkdir(parents=True)
                    metadata = {"version": 1, "origin": "http://localhost:4444", "port": 4444}
                    state.write_text(json.dumps(metadata))
                    runtime = base / "runtime/versions/0.1.10-nightly.20261112.4580/t3"
                    shim = base / "bin/t3"
                    for enabled, path in ((standalone, runtime), (desktop, shim)):
                        if enabled:
                            path.parent.mkdir(parents=True); path.touch()
                    if not (standalone or desktop):
                        with self.assertRaises(common.UpdateFailure): rpc.discover(host)
                        continue
                    self.assertEqual(rpc.discover(host)[0], [str(shim if desktop else runtime)])
                    metadata["serverVersion"] = runtime.parent.name
                    state.write_text(json.dumps(metadata))
                    def version(argv, **kwargs):
                        return "different" if argv[0] == str(shim) and standalone else metadata["serverVersion"]
                    with mock.patch.object(rpc, "command", side_effect=version):
                        self.assertEqual(rpc.discover(host)[0], [str(runtime if standalone else shim)])
                    with mock.patch.object(rpc, "command", return_value="wrong"):
                        with self.assertRaises(common.UpdateFailure): rpc.discover(host)

    def test_all_platforms_use_persisted_endpoint_and_official_cli(self):
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(temporary)):
            home = pathlib.Path(temporary)
            state = home / ".t3/userdata/server-runtime.json"
            state.parent.mkdir(parents=True)
            state.write_text(json.dumps({"version": 1, "origin": "http://127.0.0.1:44771", "port": 44771}))
            runtime = home / ".t3/runtime/versions/0.1.10-nightly.20261112.4580/t3"
            runtime.parent.mkdir(parents=True); runtime.touch()
            for host in ("ubuntu", "mac"):
                cli, base, origin = rpc.discover(host)
                self.assertEqual(cli, [str(runtime)])
                self.assertEqual(base, home / ".t3")
                self.assertEqual(origin.port, 44771)
            local = home / "Local"
            executable = local / "Programs/t3code/T3 Code (Nightly).exe"
            executable.parent.mkdir(parents=True); executable.touch()
            archive = executable.parent / "resources/server.asar"
            archive.parent.mkdir(); archive.touch()
            with mock.patch.dict(os.environ, {"LOCALAPPDATA": str(local)}):
                cli, _, origin = rpc.discover("windows")
                self.assertEqual(cli, [str(executable), str(archive / "apps/server/dist/bin.mjs")])
                self.assertEqual(origin.port, 44771)

    def test_remote_origin_missing_cli_and_missing_state_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary, mock.patch.object(pathlib.Path, "home", return_value=pathlib.Path(temporary)):
            state = pathlib.Path(temporary) / ".t3/userdata/server-runtime.json"
            state.parent.mkdir(parents=True)
            for origin in ("http://example.com:4444", "http://user:secret@localhost:4444", "https://localhost:4444", "http://127.0.0.1:4444/other"):
                state.write_text(json.dumps({"version": 1, "origin": origin, "port": 4444}))
                with self.assertRaises(common.UpdateFailure) as raised: rpc.discover("ubuntu")
                self.assertEqual(raised.exception.code, "t3_unreachable")
            state.write_text(json.dumps({"version": 1, "origin": "http://localhost:4444", "port": 4444}))
            with self.assertRaises(common.UpdateFailure): rpc.discover("ubuntu")
            state.unlink()
            with self.assertRaises(common.UpdateFailure): rpc.discover("ubuntu")


class AuthTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = pathlib.Path(directory.name)

    def test_next_apply_supervises_before_any_update(self):
        import app_updates as updates
        pending = self.base / ".t3/userdata/aac-pending-sessions/aac-acp-fixture.json"
        common.write_private_json(pending, {"sessionId": "abandoned"})
        host = "windows" if os.name == "nt" else "mac" if sys.platform == "darwin" else "ubuntu"
        events = []
        def apply(*args):
            events.append("apply")
            return {"results": []}
        with mock.patch.object(pathlib.Path, "home", return_value=self.base), mock.patch.object(sys, "argv", ["helper", "--apply", "--platform", host]), mock.patch.object(rpc, "discover", return_value=(["fake"], self.base / ".t3", None)), mock.patch.object(rpc, "recover_pending", side_effect=lambda *args: events.append("recover")), mock.patch.object(updates, "windows_interactive_apply", side_effect=apply), mock.patch.object(updates, "run_apply", side_effect=apply), mock.patch("sys.stdout", new_callable=io.StringIO):
            updates.main()
        self.assertEqual(events, ["recover", "apply"])

    def test_killed_process_is_reaped_by_next_run_on_windows_and_unix(self):
        script = '''import json, pathlib, sys, time
from unittest import mock
sys.path.insert(0, sys.argv[1])
import app_update_t3_rpc as rpc
def command(argv, **kwargs):
    if "issue" in argv:
        return json.dumps({"sessionId": "killed-session", "token": sys.argv[3]})
    return ""
with mock.patch.object(rpc, "command", side_effect=command):
    with rpc.bearer(["fake-t3"], pathlib.Path(sys.argv[2]), time.monotonic()+90):
        print("ready", flush=True)
        time.sleep(60)
'''
        child = subprocess.Popen([sys.executable, "-B", "-c", script, str(pathlib.Path(rpc.__file__).parent), str(self.base), TOKEN], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertEqual(child.stdout.readline().strip(), "ready")
            records = list(self.base.rglob("*.json"))
            self.assertEqual(len(records), 1)
            self.assertEqual(json.loads(records[0].read_text())["sessionId"], "killed-session")
            for path in self.base.rglob("*"):
                if path.suffix == ".json": self.assertNotIn(TOKEN.encode(), path.read_bytes())
            child.kill()
            output, errors = child.communicate(timeout=10)
            self.assertNotIn(TOKEN, output + errors)
            for path in self.base.rglob("*"):
                if path.is_file(): self.assertNotIn(TOKEN.encode(), path.read_bytes())
            with mock.patch.object(rpc, "command", return_value="") as command:
                rpc.recover_pending(["fake-t3"], self.base)
            self.assertIn("killed-session", command.call_args.args[0])
            self.assertFalse(list(self.base.rglob("*.json")))
        finally:
            if child.poll() is None: child.kill()
            child.communicate(timeout=10)

    def test_overlapping_runs_are_locked_and_revoke_only_their_own_id(self):
        issued = iter(("first", "second"))
        revoked = []
        def command(argv, **kwargs):
            if "issue" in argv: return json.dumps({"sessionId": next(issued), "token": TOKEN})
            if "revoke" in argv: revoked.append(argv[argv.index("revoke") + 1])
            return ""
        with mock.patch.object(rpc, "command", side_effect=command):
            with rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                with rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                    rpc.recover_pending(["fake"], self.base)
                    self.assertEqual(revoked, [])
                self.assertEqual(revoked, ["second"])
            self.assertEqual(revoked, ["second", "first"])

    def test_unix_signal_handler_cleanup_with_mock_delivery_on_any_host(self):
        handlers = {}
        fake_os = mock.Mock(wraps=os)
        fake_os.name = "posix"
        def install(number, handler):
            previous = handlers.get(number, signal.SIG_DFL)
            handlers[number] = handler
            return previous
        with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "signal-session", "token": TOKEN}), ""]) as command, mock.patch.object(signal, "SIGHUP", 1, create=True), mock.patch.object(signal, "signal", side_effect=install):
            context = rpc.unix_signals()
            with mock.patch.object(rpc, "os", fake_os):
                context.__enter__()
            try:
                with self.assertRaises(SystemExit):
                    with rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                        handlers[signal.SIGTERM](signal.SIGTERM, None)
            finally:
                context.__exit__(None, None, None)
            self.assertIn("signal-session", command.call_args.args[0])
            self.assertTrue(all(value == signal.SIG_DFL for value in handlers.values()))

    def test_death_before_reply_recovers_only_exact_preissue_label(self):
        path = self.base / "userdata/aac-pending-sessions/aac-acp-fixture.json"
        common.write_private_json(path, {"label": path.stem})
        path.with_suffix(".lock").write_bytes(b"0")
        with mock.patch.object(rpc, "command", return_value="[]"):
            rpc.recover_pending(["fake"], self.base)
        self.assertTrue(path.exists(), "issuance may still commit after helper death")
        rows = [{"sessionId": "owned", "client": {"label": path.stem}},
                {"sessionId": "other", "client": {"label": "aac-acp-other"}}]
        with mock.patch.object(rpc, "command", side_effect=[json.dumps(rows), common.UpdateFailure()]) as command:
            with self.assertRaises(common.UpdateFailure): rpc.recover_pending(["fake"], self.base)
        self.assertTrue(path.exists())
        with mock.patch.object(rpc, "command", side_effect=[json.dumps(rows), ""]) as command:
            rpc.recover_pending(["fake"], self.base)
        self.assertIn("owned", command.call_args.args[0])
        self.assertNotIn("other", command.call_args.args[0])
        self.assertFalse(path.exists())

    @unittest.skipIf(os.name == "nt", "Unix signal delivery only")
    def test_unix_signal_unwinds_bearer_and_restores_handlers(self):
        for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            old = signal.getsignal(number)
            with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "signal-session", "token": TOKEN}), ""]) as command:
                with self.assertRaises(SystemExit):
                    with rpc.unix_signals(), rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                        os.kill(os.getpid(), number)
                self.assertIn("signal-session", command.call_args.args[0])
                self.assertEqual(signal.getsignal(number), old)

    def test_revoke_on_success_exception_and_cancellation(self):
        for failure in (None, OSError("fixture"), KeyboardInterrupt()):
            with self.subTest(failure=type(failure).__name__):
                with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "session-fixture", "token": TOKEN}), ""]) as command:
                    try:
                        with rpc.bearer(["fixture-t3"], self.base, time.monotonic() + 90) as token:
                            self.assertEqual(token, TOKEN)
                            if failure: raise failure
                    except BaseException as error:
                        self.assertIs(error, failure)
                    self.assertIn("revoke", command.call_args_list[-1].args[0])
                    self.assertNotIn(TOKEN, str(command.call_args_list))
                    self.assertTrue(command.call_args.kwargs["capture"])

    def test_issue_timeout_recovers_by_unique_label_then_revokes(self):
        commands = []
        def run(argv, **kwargs):
            commands.append(argv)
            if "issue" in argv: raise common.UpdateFailure("timeout", output=TOKEN)
            if "list" in argv:
                label = commands[0][commands[0].index("--label") + 1]
                return json.dumps([{"sessionId": "recovered", "client": {"label": label}},
                                   {"sessionId": "someone-else", "client": {"label": "unrelated"}}])
            return ""
        with mock.patch.object(rpc, "command", side_effect=run):
            with self.assertRaises(common.UpdateFailure) as raised:
                with rpc.bearer(["fixture"], self.base, time.monotonic() + 90): self.fail("no token")
        self.assertEqual(raised.exception.code, "t3_auth_failed")
        self.assertIn("recovered", commands[-1])
        self.assertNotIn("someone-else", commands[-1])

    def test_revoke_failure_cannot_claim_success(self):
        with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "fixture", "token": TOKEN}), common.UpdateFailure(output=TOKEN)]):
            with self.assertRaises(common.UpdateFailure) as raised:
                with rpc.bearer(["fixture"], self.base, time.monotonic() + 90): pass
        self.assertEqual(raised.exception.code, "t3_auth_failed")


def frame(value, opcode=1, final=True):
    payload = value if isinstance(value, bytes) else json.dumps(value).encode()
    size = len(payload)
    prefix = bytes([(0x80 if final else 0) | opcode])
    return prefix + (bytes([size]) if size < 126 else bytes([126]) + struct.pack("!H", size)) + payload


class Socket:
    def __init__(self, data, status="101 Switching Protocols", accept=True):
        key = base64.b64encode(b"f" * 16).decode()
        digest = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.stream = io.BytesIO((f"HTTP/1.1 {status}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {digest if accept else 'bad'}\r\n\r\n").encode() + data)
        self.sent, self.closed = [], False
    def makefile(self, mode): return self.stream
    def sendall(self, value): self.sent.append(value)
    def settimeout(self, value): pass
    def close(self): self.closed = True


class WireTests(unittest.TestCase):
    def client(self, data, **kwargs):
        sock = Socket(data, **kwargs)
        with mock.patch.object(rpc.socket, "create_connection", return_value=sock), mock.patch.object(rpc.os, "urandom", return_value=b"f" * 16):
            client = rpc.Rpc(urllib.parse.urlsplit("http://127.0.0.1:4444"), TOKEN, time.monotonic() + 90)
        self.addCleanup(client.close)
        return client, sock

    def test_effect_requests_chunks_acks_ping_and_success(self):
        messages = [{"_tag": "Ping"}, {"_tag": "Chunk", "requestId": "1", "values": [{"phase": "verifying"}]},
                    {"_tag": "Chunk", "requestId": "1", "values": [{"phase": "succeeded"}]},
                    {"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Success", "value": None}}]
        client, sock = self.client(frame(messages))
        identifier = client.request("provider.install.subscribe", {"instanceId": "agy"})
        self.assertEqual(list(client.receive(identifier)), [{"phase": "verifying"}, {"phase": "succeeded"}, None])
        self.assertIn(b"/ws?orchestrationProtocol=2", sock.sent[0])
        self.assertIn(("Authorization: Bearer " + TOKEN).encode(), sock.sent[0])
        decoded = []
        for data in sock.sent[1:]:
            self.assertTrue(data[1] & 128)
            length = data[1] & 127
            offset = 2 if length < 126 else 4
            mask, payload = data[offset:offset + 4], data[offset + 4:]
            decoded.append(json.loads(bytes(v ^ mask[i % 4] for i, v in enumerate(payload))))
        self.assertEqual(decoded[0], {"_tag": "Request", "id": "1", "tag": "provider.install.subscribe", "payload": {"instanceId": "agy"}, "headers": []})
        self.assertEqual([m["_tag"] for m in decoded[1:]], ["Pong", "Ack", "Ack"])

    def test_fragmentation_and_websocket_ping(self):
        client, sock = self.client(frame(b'{"_tag":', final=False) + frame(b"pulse", opcode=9) + frame(b'"Pong"}', opcode=0))
        self.assertEqual(client.messages(), [{"_tag": "Pong"}])
        self.assertEqual(sock.sent[-1][0], 0x8a)

    def test_authorization_and_installer_errors(self):
        for error, code in [("EnvironmentAuthorizationError", "t3_auth_failed"), ("ProviderSetupError", "t3_install_failed:rpc")]:
            client, _ = self.client(frame({"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Failure", "cause": [{"_tag": "Fail", "error": {"_tag": error, "detail": TOKEN}}]}}))
            with self.assertRaises(common.UpdateFailure) as raised: client.call("provider.install.start", {"instanceId": "agy"})
            self.assertEqual(raised.exception.code, code)
            self.assertNotIn(TOKEN, str(raised.exception))

    def test_handshake_failure_closes_socket(self):
        for status, accept, code in [("401 Unauthorized", True, "t3_auth_failed"), ("403 Forbidden", True, "t3_auth_failed"), ("426 Upgrade Required", True, "t3_unreachable"), ("101 Switching Protocols", False, "t3_unreachable")]:
            sock = Socket(b"", status, accept)
            with mock.patch.object(rpc.socket, "create_connection", return_value=sock), mock.patch.object(rpc.os, "urandom", return_value=b"f" * 16):
                with self.assertRaises(common.UpdateFailure) as raised: rpc.Rpc(urllib.parse.urlsplit("http://localhost:4444"), TOKEN, time.monotonic() + 30)
            self.assertEqual(raised.exception.code, code)
            self.assertTrue(sock.closed)


if __name__ == "__main__": unittest.main()
