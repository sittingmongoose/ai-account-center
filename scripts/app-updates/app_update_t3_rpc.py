"""Local-only T3 install client. No credential or server diagnostic is emitted.

Wire contract: T3 ab8e488e, /ws?orchestrationProtocol=2, Effect 4 JSON RPC.
Python's standard library keeps the synced helper independent of pip/Node.
"""

import base64
import contextlib
import hashlib
import json
import os
import pathlib
import socket
import struct
import time
import urllib.parse
import uuid

from app_update_common import UpdateFailure, command, environment


def remaining(deadline, maximum=30):
    value = min(maximum, deadline - time.monotonic())
    if value <= 0:
        raise UpdateFailure("t3_install_timeout")
    return value


def discover(host):
    # Explicit --base-dir binds issue/revoke to the same local server's store.
    # Never inherit a remote endpoint or another install's T3CODE_HOME.
    base = pathlib.Path.home() / ".t3"
    try:
        state = json.loads((base / "userdata/server-runtime.json").read_text(encoding="utf-8"))
        origin = urllib.parse.urlsplit(state["origin"])
        if (state.get("version") != 1 or origin.scheme != "http" or
                origin.hostname not in ("127.0.0.1", "localhost", "::1") or
                origin.username or origin.password or origin.query or origin.fragment or
                origin.path not in ("", "/") or not origin.port or origin.port != state.get("port")):
            raise ValueError()
        if host == "windows":
            local = pathlib.Path(os.environ.get("LOCALAPPDATA", str(pathlib.Path.home() / "AppData/Local")))
            executable = local / "Programs/t3code/T3 Code (Nightly).exe"
            archive = executable.parent / "resources/server.asar"
            # This is precisely the generated t3.cmd invocation, without cmd.exe.
            if not executable.is_file() or not archive.is_file():
                raise OSError()
            cli = [str(executable), str(archive / "apps/server/dist/bin.mjs")]
        else:
            from app_update_t3 import runtime_install
            executable = runtime_install()
            if executable is None:
                raise OSError()
            cli = [str(executable)]
        return cli, base, origin
    except (OSError, ValueError, KeyError, TypeError):
        raise UpdateFailure("t3_unreachable") from None


@contextlib.contextmanager
def bearer(cli, base, deadline):
    """Issue a 20-minute session; revoke even on cancellation or RPC failure.

    CLI stdout stays in memory. A unique label lets finally recover the session
    ID if issuance committed but the CLI timed out before returning JSON.
    """
    label = "aac-acp-" + uuid.uuid4().hex
    session_id = None
    attempted = False
    env = environment({"ELECTRON_RUN_AS_NODE": "1"})

    def auth(args, timeout=30):
        return command([*cli, "auth", "session", *args, "--base-dir", str(base)],
                       timeout=timeout, env=env, capture=True, kill_tree=True)

    try:
        timeout = remaining(deadline)
        attempted = True
        try:
            issued = json.loads(auth(["issue", "--scope", "providers:manage", "--scope",
                                      "orchestration:read", "--ttl", "20m", "--label", label,
                                      "--json"], timeout))
            session_id = issued["sessionId"]
            token = issued["token"]
            if (not isinstance(session_id, str) or not session_id or
                    not isinstance(token, str) or not token or "\r" in token or "\n" in token):
                raise ValueError()
        except (UpdateFailure, OSError, ValueError, KeyError, TypeError, AttributeError):
            raise UpdateFailure("t3_auth_failed") from None
        yield token
    finally:
        if attempted:
            try:
                ids = [session_id] if isinstance(session_id, str) and session_id else [
                    row["sessionId"] for row in json.loads(auth(["list", "--json"]))
                    if row.get("client", {}).get("label") == label
                ]
                for identifier in ids:
                    auth(["revoke", identifier])
            except (UpdateFailure, OSError, ValueError, KeyError, TypeError, AttributeError):
                # Do not hide a failed revocation behind a successful install.
                raise UpdateFailure("t3_auth_failed") from None


class Rpc:
    """Bounded RFC 6455 client for Effect Request/Exit/Chunk/Ack messages."""

    LIMIT = 4 * 1024 * 1024

    def __init__(self, origin, token, deadline):
        self.deadline, self.serial = deadline, 0
        self.socket = None
        self.stream = None
        try:
            self.socket = socket.create_connection((origin.hostname, origin.port), remaining(deadline, 10))
            self.stream = self.socket.makefile("rb")
            key = base64.b64encode(os.urandom(16)).decode("ascii")
            host = "[::1]" if origin.hostname == "::1" else origin.hostname
            request = ("GET /ws?orchestrationProtocol=2 HTTP/1.1\r\n"
                       f"Host: {host}:{origin.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                       f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
                       f"Authorization: Bearer {token}\r\n\r\n")
            self.socket.sendall(request.encode("ascii"))
            status = self.stream.readline(8193).split()
            headers, size = {}, 0
            while True:
                line = self.stream.readline(8193)
                size += len(line)
                if not line or len(line) > 8192 or size > 32768:
                    raise UpdateFailure("t3_unreachable")
                if line == b"\r\n":
                    break
                name, value = line.decode("ascii").split(":", 1)
                headers[name.lower()] = value.strip()
            if len(status) < 2 or status[1] != b"101":
                raise UpdateFailure("t3_auth_failed" if len(status) > 1 and status[1] in (b"401", b"403") else "t3_unreachable")
            expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            if (headers.get("sec-websocket-accept") != expected or
                    headers.get("upgrade", "").lower() != "websocket" or
                    "upgrade" not in headers.get("connection", "").lower()):
                raise UpdateFailure("t3_unreachable")
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.stream:
            self.stream.close()
        if self.socket:
            self.socket.close()

    def frame(self, payload, opcode=1):
        self.socket.settimeout(remaining(self.deadline))
        mask = os.urandom(4)
        size = len(payload)
        header = bytes([0x80 | opcode, 0x80 | size]) if size < 126 else (
            bytes([0x80 | opcode, 0xfe]) + struct.pack("!H", size) if size < 65536 else
            bytes([0x80 | opcode, 0xff]) + struct.pack("!Q", size))
        self.socket.sendall(header + mask + bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))

    def send(self, message):
        self.frame(json.dumps(message).encode("utf-8"))

    def request(self, method, payload):
        self.serial += 1
        identifier = str(self.serial)
        self.send({"_tag": "Request", "id": identifier, "tag": method, "payload": payload, "headers": []})
        return identifier

    def read(self, size):
        # ACP validation can be quiet for 90 seconds. A quiet stream must keep
        # the full operation budget rather than fail at the CLI's 30s bound.
        self.socket.settimeout(remaining(self.deadline, 900))
        data = self.stream.read(size)
        if len(data) != size:
            raise UpdateFailure("t3_unreachable")
        return data

    def messages(self):
        data = bytearray()
        started = False
        while True:
            first, second = self.read(2)
            opcode, size = first & 15, second & 127
            if first & 0x70 or second & 0x80:
                raise UpdateFailure("t3_install_failed:protocol")
            if size == 126:
                size = struct.unpack("!H", self.read(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.read(8))[0]
            if size > self.LIMIT or len(data) + size > self.LIMIT:
                raise UpdateFailure("t3_install_failed:protocol")
            payload = self.read(size)
            if opcode == 9:
                self.frame(payload, 10)
                continue
            if opcode == 10:
                continue
            if opcode == 8:
                raise UpdateFailure("t3_unreachable")
            if (opcode not in (0, 1) or (opcode == 0 and not started) or
                    (opcode == 1 and started)):
                raise UpdateFailure("t3_install_failed:protocol")
            started = True
            data.extend(payload)
            if first & 0x80:
                value = json.loads(data.decode("utf-8"))
                return value if isinstance(value, list) else [value]

    def receive(self, identifier):
        while True:
            for message in self.messages():
                if message.get("_tag") == "Ping":
                    self.send({"_tag": "Pong"})
                    continue
                if message.get("requestId") != identifier:
                    continue
                if message.get("_tag") == "Chunk":
                    self.send({"_tag": "Ack", "requestId": identifier})
                    yield from message["values"]
                elif message.get("_tag") == "Exit":
                    exit_value = message["exit"]
                    if exit_value.get("_tag") != "Success":
                        causes = exit_value.get("cause", [])
                        denied = any(c.get("error", {}).get("_tag") == "EnvironmentAuthorizationError" for c in causes)
                        raise UpdateFailure("t3_auth_failed" if denied else "t3_install_failed:rpc")
                    yield exit_value.get("value")
                    return

    def call(self, method, payload):
        return next(self.receive(self.request(method, payload)))
