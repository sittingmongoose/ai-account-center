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
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.parse
import uuid

from app_update_common import CREATE_NO_WINDOW, UpdateFailure, command, environment, _stop_group


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
            shim = base / "bin/t3"
            candidates = ([shim] if shim.is_file() else []) + ([executable] if executable else [])
            version = state.get("serverVersion")
            if version:
                # Match the running server's published version, including older
                # installed runtimes. Do not silently use a different version.
                from app_update_t3 import exact_version
                if exact_version(version):
                    pinned = base / "runtime/versions" / version / "t3"
                    if pinned.is_file():
                        candidates.append(pinned)
                matched = []
                for candidate in candidates:
                    try:
                        output = command([str(candidate), "--version"], timeout=10,
                                         env=environment({"ELECTRON_RUN_AS_NODE": "1"}), capture=True,
                                         kill_tree=True)
                        words = output.strip().split()
                        if words and words[-1].removeprefix("v") == version:
                            matched.append(candidate)
                    except (UpdateFailure, OSError):
                        pass
                candidates = matched
            # Older metadata has no executable identity: Desktop's official
            # shim first, then the standalone runtime. Ties use the same order.
            if not candidates:
                raise OSError()
            cli = [str(candidates[0])]
        return cli, base, origin
    except (OSError, ValueError, KeyError, TypeError):
        raise UpdateFailure("t3_unreachable") from None


_issuers = {}

# A fresh interpreter is safe with threaded callers (unlike preexec_fn). The
# gate prevents T3 starting before tracking/job assignment. EOF fails closed.
_ISSUER_BOOTSTRAP = """
import os, sys, signal, subprocess
parent = int(sys.argv[1])
if sys.platform.startswith('linux'):
    import ctypes
    if ctypes.CDLL(None, use_errno=True).prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
        sys.exit(125)
    if os.getppid() != parent:
        sys.exit(125)
if sys.stdin.buffer.read(1) != b'1':
    sys.exit(125)
os.dup2(os.open(os.devnull, os.O_RDONLY), 0)
if os.name == 'nt':
    sys.exit(subprocess.call(sys.argv[2:]))
os.execvp(sys.argv[2], sys.argv[2:])
"""


class Issuer:
    def __init__(self):
        self.process = None
        self.job = None
        if os.name == "nt":
            import ctypes
            from ctypes import wintypes as w
            class Basic(ctypes.Structure):
                _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                            ("flags", w.DWORD), ("min_ws", ctypes.c_size_t),
                            ("max_ws", ctypes.c_size_t), ("active", w.DWORD),
                            ("affinity", ctypes.c_size_t), ("priority", w.DWORD),
                            ("scheduling", w.DWORD)]
            class Extended(ctypes.Structure):
                _fields_ = [("basic", Basic), ("io", ctypes.c_uint64 * 6),
                            ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                            ("peak_process", ctypes.c_size_t), ("peak_job", ctypes.c_size_t)]
            self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            self.kernel.CreateJobObjectW.restype = w.HANDLE
            self.kernel.CreateJobObjectW.argtypes = [w.LPVOID, w.LPCWSTR]
            self.kernel.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, w.LPVOID, w.DWORD]
            self.kernel.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
            self.kernel.CloseHandle.argtypes = [w.HANDLE]
            self.job = self.kernel.CreateJobObjectW(None, None)
            info = Extended()
            info.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            if not self.job or not self.kernel.SetInformationJobObject(self.job, 9, ctypes.byref(info), ctypes.sizeof(info)):
                self.stop()
                raise UpdateFailure("t3_issuer_protection_failed")

    def stop(self):
        if self.job:
            self.kernel.CloseHandle(self.job)
            self.job = None
        elif self.process is not None and os.name != "nt":
            with contextlib.suppress(OSError):
                os.killpg(self.process.pid, signal.SIGKILL)

    def start(self, argv):
        options = {"creationflags": CREATE_NO_WINDOW} if os.name == "nt" else {"start_new_session": True}
        self.process = subprocess.Popen(
            [sys.executable, "-B", "-c", _ISSUER_BOOTSTRAP, str(os.getpid()), *argv],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env=environment({"ELECTRON_RUN_AS_NODE": "1"}), **options)
        if os.name == "nt" and (not self.job or not self.kernel.AssignProcessToJobObject(self.job, int(self.process._handle))):
            # Nested jobs work on Windows 8+. Restrictive enclosing jobs fail
            # closed: the launcher has not received permission to start T3.
            self.process.stdin.close()
            self.process.stdin = None
            self.process.kill()
            raise UpdateFailure("t3_issuer_protection_failed")
        if _terminating:
            self.stop()
            raise UpdateFailure("t3_issuer_protection_failed")
        self.process.stdin.write(b"1")
        self.process.stdin.close()
        self.process.stdin = None
        return self.process


def auth_command(cli, base, args, timeout=30):
    # Keep spawn failure distinct: command() erases this evidence. Once Popen
    # succeeds, every failure is potentially after the session-store commit.
    options = {"creationflags": CREATE_NO_WINDOW} if os.name == "nt" else {"start_new_session": True}
    issuer = None
    try:
        if args[0] == "issue":
            issuer = Issuer()
            _issuers[id(issuer)] = issuer
            process = issuer.start([*cli, "auth", "session", *args, "--base-dir", str(base)])
        else:
            process = subprocess.Popen(
                [*cli, "auth", "session", *args, "--base-dir", str(base)],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                env=environment({"ELECTRON_RUN_AS_NODE": "1"}), **options,
            )
    except (OSError, UpdateFailure) as error:
        if issuer:
            issuer.stop()
            _issuers.pop(id(issuer), None)
            if issuer.process is not None:
                with contextlib.suppress(OSError, subprocess.TimeoutExpired):
                    issuer.process.communicate(timeout=1)
        if isinstance(error, UpdateFailure):
            raise
        if issuer and issuer.process is not None:
            raise UpdateFailure("t3_auth_failed") from None
        raise UpdateFailure("t3_cli_not_started") from None
    try:
        output, _ = process.communicate(timeout=timeout)
    except (subprocess.TimeoutExpired, OSError):
        _stop_group(process, tree=True)
        raise UpdateFailure("t3_auth_failed") from None
    except BaseException:
        _stop_group(process, tree=True)
        raise
    finally:
        if issuer:
            issuer.stop()
            _issuers.pop(id(issuer), None)
    if process.returncode or len(output) > 65536:
        raise UpdateFailure("t3_auth_failed")
    return output.decode("utf-8", "replace")


def find_issued_session(cli, base, label):
    """Best effort lookup after a lost reply; absence cannot prove no commit."""
    try:
        rows = json.loads(auth_command(cli, base, ["list", "--json"], FINAL_REVOKE_TIMEOUT))
        matches = [row["sessionId"] for row in rows
                   if row.get("client", {}).get("label") == label]
        if len(matches) == 1 and isinstance(matches[0], str) and matches[0]:
            return matches[0]
    except (UpdateFailure, OSError, ValueError, KeyError, TypeError, AttributeError):
        pass
    return None


# Persisted credentials rely on the accepted 15m TTL after hard termination.
SESSION_TTL = 900
REVOKE_TIMEOUT = 1.0
FINAL_REVOKE_TIMEOUT = 5.0
# Memory only. Signals read a snapshot without acquiring a worker-owned lock.
_known_sessions = {}
_terminating = False


def revoke_session(cli, base, session_id, timeout=FINAL_REVOKE_TIMEOUT):
    """Best effort, no diagnostics, and no common.command's 5s cleanup wait."""
    process = None
    try:
        options = {"creationflags": CREATE_NO_WINDOW} if os.name == "nt" else {"start_new_session": True}
        process = subprocess.Popen(
            [*cli, "auth", "session", "revoke", session_id, "--base-dir", str(base)],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            env=environment({"ELECTRON_RUN_AS_NODE": "1"}), **options,
        )
        return process.wait(timeout=timeout) == 0
    except (OSError, subprocess.TimeoutExpired):
        return False
    finally:
        if process is not None and process.poll() is None:
            with contextlib.suppress(OSError):
                process.kill()
            # Never wait here: the signal handler must exit within the grace.


def retire_session(cli, base, session_id):
    """Confirm revocation within one shared budget; no background worker."""
    end = time.monotonic() + FINAL_REVOKE_TIMEOUT
    while True:
        budget = end - time.monotonic()
        if budget <= 0:
            raise UpdateFailure("t3_session_revoke_failed")
        if revoke_session(cli, base, session_id, budget):
            _known_sessions.pop(session_id, None)
            return
        time.sleep(min(0.1, max(0, end - time.monotonic())))


def terminate(signum, frame=None):
    """Main-thread revoke followed by immediate exit; never unwind the pool."""
    global _terminating
    if _terminating:
        return
    _terminating = True
    # Includes process creation/OS stalls in the hard 2s bound.
    watchdog = threading.Timer(2, os._exit, args=(128 + signum,))
    watchdog.daemon = True
    watchdog.start()
    try:
        end = time.monotonic() + REVOKE_TIMEOUT
        for issuer in tuple(_issuers.values()):
            issuer.stop()
        for session_id, (cli, base) in tuple(_known_sessions.items()):
            budget = end - time.monotonic()
            if budget <= 0:
                break
            revoke_session(cli, base, session_id, budget)
    finally:
        os._exit(128 + signum)


@contextlib.contextmanager
def session_signals():
    """SIGBREAK is Windows' graceful console stop; kill issuers before exit."""
    previous = {}
    if threading.current_thread() is threading.main_thread():
        names = ("SIGTERM", "SIGINT", "SIGBREAK") if os.name == "nt" else ("SIGTERM", "SIGINT", "SIGHUP")
        for name in names:
            number = getattr(signal, name)
            previous[number] = signal.signal(number, terminate)
    try:
        yield
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)


@contextlib.contextmanager
def bearer(cli, base, deadline):
    """Retry only a proven spawn failure; never issue again after ambiguity."""
    session_id = None
    label = "aac-acp-" + uuid.uuid4().hex
    attempted = False
    try:
        try:
            backoff = 1
            while True:
                ttl = remaining(deadline, SESSION_TTL)
                try:
                    attempted = True
                    reply = auth_command(cli, base, [
                        "issue", "--scope", "providers:manage", "--scope", "orchestration:read",
                        "--ttl", str(ttl) + "s", "--label", label, "--json",
                    ], remaining(deadline, float("inf")))
                    issued = json.loads(reply)
                    break
                except UpdateFailure as error:
                    if error.code != "t3_cli_not_started":
                        raise
                    attempted = False
                    time.sleep(remaining(deadline, backoff))
                    backoff = min(8, backoff * 2)
            session_id = issued["sessionId"]
            if not isinstance(session_id, str) or not session_id:
                session_id = None
                raise ValueError()
            _known_sessions[session_id] = (cli, base)
            token = issued["token"]
            if not isinstance(token, str) or not token or "\r" in token or "\n" in token:
                raise ValueError()
        except (UpdateFailure, OSError, ValueError, KeyError, TypeError, AttributeError) as error:
            if isinstance(error, UpdateFailure) and error.code == "t3_issuer_protection_failed":
                raise
            if attempted and not session_id:
                session_id = find_issued_session(cli, base, label)
                if session_id:
                    _known_sessions[session_id] = (cli, base)
                raise UpdateFailure("t3_session_issue_unknown") from None
            raise UpdateFailure("t3_auth_failed") from None
        yield token
    finally:
        if session_id:
            retire_session(cli, base, session_id)


class InstallRpc:
    """One session; reconnect subscriptions with the same bearer, never start twice."""

    def __init__(self, cli, base, origin, deadline):
        self.cli, self.base, self.origin = cli, base, origin
        self.deadline = min(deadline, time.monotonic() + SESSION_TTL)
        self.stack = contextlib.ExitStack()
        self.client = None
        self.token = None
        self.started = False

    def disconnect(self):
        if self.client is not None:
            self.client.close()
            self.client = None

    def close(self):
        try:
            self.disconnect()
        finally:
            self.token = None
            self.stack.close()

    def refresh(self):
        remaining(self.deadline)
        if self.token is None:
            self.token = self.stack.enter_context(bearer(self.cli, self.base, self.deadline))
        if self.client is None:
            self.client = Rpc(self.origin, self.token, self.deadline)

    def call(self, method, payload):
        self.refresh()
        if method == "provider.install.start":
            if self.started:
                raise UpdateFailure("t3_install_state_unknown")
            self.started = True
        try:
            return self.client.call(method, payload)
        except (UpdateFailure, OSError) as error:
            if method == "provider.install.start" and self.recoverable(error):
                # T3 generates the ID server-side and exposes only an instance
                # snapshot. No request key ties a lost reply to that snapshot.
                raise UpdateFailure("t3_install_state_unknown") from None
            raise

    @staticmethod
    def recoverable(error):
        return isinstance(error, OSError) or error.code in ("t3_install_timeout", "t3_unreachable")

    def subscribe(self, instance):
        backoff = 1
        while True:
            client, identifier = None, None
            try:
                self.refresh()
                client = self.client
                identifier = client.request("provider.install.subscribe", {"instanceId": instance})
                yield from client.receive(identifier)
                return
            except (UpdateFailure, OSError) as error:
                if not self.recoverable(error):
                    raise
                self.disconnect()
                time.sleep(remaining(self.deadline, backoff))
                backoff = min(8, backoff * 2)
            finally:
                if identifier is not None and client is self.client:
                    with contextlib.suppress(UpdateFailure, OSError):
                        client.send({"_tag": "Interrupt", "requestId": identifier, "interruptors": []})


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
