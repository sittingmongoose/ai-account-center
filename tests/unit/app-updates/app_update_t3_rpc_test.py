"""No-network fixtures for T3 CLI discovery, temporary auth and Effect WS frames."""

import base64
import hashlib
import io
import json
import os
import pathlib
import struct
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
    def test_revoke_on_success_exception_and_cancellation(self):
        for failure in (None, OSError("fixture"), KeyboardInterrupt()):
            with self.subTest(failure=type(failure).__name__):
                with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "session-fixture", "token": TOKEN}), ""]) as command:
                    try:
                        with rpc.bearer(["fixture-t3"], pathlib.Path("fixture"), time.monotonic() + 90) as token:
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
                with rpc.bearer(["fixture"], pathlib.Path("fixture"), time.monotonic() + 90): self.fail("no token")
        self.assertEqual(raised.exception.code, "t3_auth_failed")
        self.assertIn("recovered", commands[-1])
        self.assertNotIn("someone-else", commands[-1])

    def test_revoke_failure_cannot_claim_success(self):
        with mock.patch.object(rpc, "command", side_effect=[json.dumps({"sessionId": "fixture", "token": TOKEN}), common.UpdateFailure(output=TOKEN)]):
            with self.assertRaises(common.UpdateFailure) as raised:
                with rpc.bearer(["fixture"], pathlib.Path("fixture"), time.monotonic() + 90): pass
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
