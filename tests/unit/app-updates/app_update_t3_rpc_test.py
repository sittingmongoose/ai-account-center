"""No-network fixtures for T3 CLI discovery, temporary auth and Effect WS frames."""

import base64
import contextlib
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
import threading
from concurrent.futures import ThreadPoolExecutor
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


class FakeAuthStore:
    """Only metadata is persisted by this fake T3 store; never the bearer."""

    def __init__(self, now, path=None):
        self.now, self.path = now, path
        self.rows, self.events = {}, []

    def command(self, cli, base, args, timeout=30):
        assert args[0] == "issue"
        assert [args[i + 1] for i, v in enumerate(args) if v == "--scope"] == ["providers:manage", "orchestration:read"]
        ttl = int(args[args.index("--ttl") + 1][:-1])
        identifier = "session-" + str(len(self.rows) + 1)
        self.rows[identifier] = {"expires": self.now() + ttl, "revoked": False}
        self.events.append(("issue", identifier))
        self.save()
        return json.dumps({"sessionId": identifier, "token": TOKEN})

    def revoke(self, cli, base, identifier, timeout=1):
        self.rows[identifier]["revoked"] = True
        self.events.append(("revoke", identifier))
        self.save()

    def save(self):
        if self.path:
            self.path.write_text(json.dumps(self.rows))

    def valid(self, identifier, now):
        row = self.rows[identifier]
        return not row["revoked"] and now < row["expires"]


def crash_fixture(stage, base):
    """An actual abrupt process exit bypasses all finally blocks."""
    import app_update_antigravity as acp
    clock = [100]
    store = FakeAuthStore(lambda: clock[0], base / "fake-t3-auth-store.json")

    def kill(point):
        if stage == point:
            os._exit(77)

    def issue(*args, **kwargs):
        kill("before_issue")
        reply = store.command(*args, **kwargs)
        kill("issue_committed_before_reply")
        return reply

    class Client:
        def __init__(self, origin, token, deadline):
            kill("connect")
            self.number = len(store.rows)
        def close(self): pass
        def send(self, value): pass
        def call(self, method, payload):
            kill("start_committed_before_reply")
            return {"operationId": "operation"}
        def request(self, method, payload):
            kill("subscribe")
            return "subscription"
        def receive(self, identifier):
            if self.number == 1:
                clock[0] = 130
                raise TimeoutError()
            kill("renewed_subscription")
            yield {"operationId": "operation"}

    original_loads = json.loads
    def loads(value):
        reply = original_loads(value)
        kill("reply_before_registration")
        return reply

    def revoke(*args, **kwargs):
        kill("before_revoke")
        store.revoke(*args, **kwargs)

    with mock.patch.object(rpc, "auth_command", side_effect=issue), mock.patch.object(rpc, "revoke_session", side_effect=revoke), mock.patch.object(rpc, "Rpc", Client), mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]), mock.patch.object(rpc.json, "loads", side_effect=loads):
        client = rpc.InstallRpc(["fake"], base, None, 1000)
        client.refresh()
        kill("authenticated")
        client.call("provider.install.start", {})
        next(acp.install_state(client, "agy"))
        client.close()
    raise AssertionError("kill point not reached")


class AuthTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.base = pathlib.Path(directory.name)
        self.addCleanup(rpc._known_sessions.clear)

    def test_stopped_t3_with_legacy_leftovers_affects_only_acp_in_twelve_row_apply(self):
        import app_updates as updates
        import app_update_antigravity as acp
        pending = self.base / ".t3/userdata/aac-pending-sessions/aac-acp-abandoned.json"
        pending.parent.mkdir(parents=True)
        pending.write_text('{"label":"aac-acp-abandoned","sessionId":"old"}')
        pending.with_suffix(".lock").write_bytes(b"0")
        installs = {key: common.Install(key, "windows", self.base / key, "1.0.0") for key in common.APP_LABELS}
        def current(install, *args, **kwargs):
            return common.result(install.app_id, "windows", "current", "1.0.0", "1.0.0", install.manager, "current")
        out = io.StringIO()
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(pathlib.Path, "home", return_value=self.base))
            stack.enter_context(mock.patch.object(sys, "argv", ["helper", "--apply", "--platform", "windows", "--task-child"]))
            stack.enter_context(mock.patch.object(updates, "os", mock.Mock(wraps=os, name="nt")))
            # main's platform guard is native even on a Unix test runner.
            updates.os.name = "nt"
            stack.enter_context(mock.patch.object(updates, "execution_lock", contextlib.nullcontext))
            stack.enter_context(mock.patch.object(updates, "detect", return_value=installs))
            stack.enter_context(mock.patch.object(updates, "check_readiness", return_value=None))
            stack.enter_context(mock.patch.object(updates, "antigravity_plan", return_value=(None, False)))
            stack.enter_context(mock.patch.object(updates, "claude_current", return_value=None))
            stack.enter_context(mock.patch.object(acp, "detect_antigravity_acp", return_value=installs["antigravity-acp"]))
            stack.enter_context(mock.patch.object(rpc, "auth_command", side_effect=AssertionError("stopped T3 must not issue")))
            discover = stack.enter_context(mock.patch.object(rpc, "discover", wraps=rpc.discover))
            for name in ("update_cli", "update_desktop", "update_zcode", "update_adapters", "update_t3", "update_cursor"):
                stack.enter_context(mock.patch.object(updates, name, side_effect=current))
            stack.enter_context(mock.patch("sys.stdout", out))
            updates.main()
        rows = json.loads(out.getvalue().splitlines()[-1])["results"]
        self.assertEqual(len(rows), 12)
        for row in rows:
            if row["appId"] == "antigravity-acp":
                self.assertEqual((row["status"], row["messageCode"]), ("action_required", "t3_unreachable"))
            else:
                self.assertEqual(row["status"], "current", row)
        discover.assert_called_once_with("windows")
        self.assertTrue(pending.exists(), "legacy records are ignored")
        self.assertNotIn(TOKEN, out.getvalue())

    def test_ttl_is_fixed_even_for_a_long_install_and_no_aac_files_are_created(self):
        clock = [100]
        store = FakeAuthStore(lambda: clock[0])
        with mock.patch.object(rpc, "auth_command", side_effect=store.command) as auth, mock.patch.object(rpc, "revoke_session", side_effect=store.revoke), mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]):
            with rpc.bearer(["fake"], self.base, 10000) as token:
                self.assertEqual(token, TOKEN)
                self.assertEqual(store.rows["session-1"]["expires"], 160)
                self.assertEqual(set(rpc._known_sessions), {"session-1"})
                self.assertFalse(list(self.base.rglob("*")))
            self.assertEqual(rpc._known_sessions, {})
            self.assertFalse(store.valid("session-1", 100))
        self.assertNotIn(TOKEN, str(auth.call_args_list))

    def test_actual_kill_at_each_stage_needs_no_future_run_to_expire(self):
        stages = ("before_issue", "issue_committed_before_reply", "reply_before_registration", "connect", "authenticated", "start_committed_before_reply", "subscribe", "before_revoke", "renewed_subscription")
        script = "import pathlib,sys; sys.path.insert(0,sys.argv[1]); from app_update_t3_rpc_test import crash_fixture; crash_fixture(sys.argv[2],pathlib.Path(sys.argv[3]))"
        for stage in stages:
            with self.subTest(stage=stage), tempfile.TemporaryDirectory() as temporary:
                child = subprocess.run([sys.executable, "-B", "-c", script, str(pathlib.Path(__file__).parent), stage, temporary], capture_output=True, text=True, timeout=10)
                self.assertEqual(child.returncode, 77, child.stderr)
                self.assertNotIn(TOKEN, child.stdout + child.stderr)
                for path in pathlib.Path(temporary).rglob("*"):
                    if path.is_file(): self.assertNotIn(TOKEN.encode(), path.read_bytes())
                path = pathlib.Path(temporary) / "fake-t3-auth-store.json"
                rows = json.loads(path.read_text()) if path.exists() else {}
                kill_time = 130 if stage in ("before_revoke", "renewed_subscription") else 100
                for row in rows.values():
                    self.assertLessEqual(row["expires"], kill_time + 60)
                    self.assertFalse(not row["revoked"] and kill_time + 60 < row["expires"])

    def test_renewal_revokes_old_session_and_resubscribes_without_restarting_install(self):
        import app_update_antigravity as acp
        clock = [100]
        store = FakeAuthStore(lambda: clock[0])
        calls, connections = [], []
        owner = self
        class Client:
            def __init__(self, origin, token, deadline):
                self.identifier = list(store.rows)[-1]
                self.deadline = deadline
                connections.append(self)
            def close(self): calls.append(("close", self.identifier))
            def send(self, value): pass
            def call(self, method, payload):
                calls.append((method, self.identifier))
                return {"operationId": "op"}
            def request(self, method, payload):
                owner.assertEqual(payload, {"instanceId": "agy"})
                owner.assertTrue(store.valid(self.identifier, clock[0]))
                calls.append((method, self.identifier))
                return "subscription"
            def receive(self, identifier):
                if self.identifier == "session-1":
                    clock[0] = self.deadline
                    raise TimeoutError()
                yield {"operationId": "op", "phase": "succeeded"}
        with mock.patch.object(rpc, "auth_command", side_effect=store.command), mock.patch.object(rpc, "revoke_session", side_effect=store.revoke), mock.patch.object(rpc, "Rpc", Client), mock.patch.object(rpc.time, "monotonic", side_effect=lambda: clock[0]):
            with contextlib.closing(rpc.InstallRpc(["fake"], self.base, None, 1000)) as client:
                client.call("provider.install.start", {})
                with contextlib.closing(acp.install_state(client, "agy")) as states:
                    self.assertEqual(next(states), {"operationId": "op", "phase": "succeeded"})
                self.assertEqual(store.events, [("issue", "session-1"), ("revoke", "session-1"), ("issue", "session-2")])
            self.assertTrue(all(row["revoked"] for row in store.rows.values()))
        self.assertEqual(len(connections), 2)
        self.assertEqual(sum(method == "provider.install.start" for method, _ in calls), 1)
        self.assertEqual(sum(method == "provider.install.subscribe" for method, _ in calls), 2)
        self.assertEqual(connections[0].deadline, 130)
        self.assertEqual(connections[1].deadline, 160)

    def test_issuance_errors_are_sanitized_and_never_enumerate_or_recover(self):
        for reply in (common.UpdateFailure(output=TOKEN), TOKEN, json.dumps({"sessionId": "known", "token": "\r" + TOKEN})):
            with self.subTest(reply=type(reply).__name__), mock.patch.object(rpc, "auth_command", side_effect=[reply]) as auth, mock.patch.object(rpc, "revoke_session") as revoke:
                with self.assertRaises(common.UpdateFailure) as raised:
                    with rpc.bearer(["fake"], self.base, time.monotonic() + 900): self.fail("invalid issue")
                self.assertEqual(raised.exception.code, "t3_auth_failed")
                self.assertNotIn(TOKEN, str(raised.exception))
                self.assertEqual(auth.call_count, 1)
                self.assertEqual(revoke.call_count, int(isinstance(reply, str) and reply.startswith("{")))
                self.assertFalse(list(self.base.rglob("*")))

    def test_revoke_on_success_exception_and_cancellation(self):
        for failure in (None, OSError(TOKEN), KeyboardInterrupt()):
            with mock.patch.object(rpc, "auth_command", return_value=json.dumps({"sessionId": "session", "token": TOKEN})), mock.patch.object(rpc, "revoke_session") as revoke:
                try:
                    with rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                        if failure: raise failure
                except BaseException as error: self.assertIs(error, failure)
                revoke.assert_called_once_with(["fake"], self.base, "session")

    def test_revoke_timeout_never_uses_common_cleanup_or_waits_again(self):
        process = mock.Mock()
        process.wait.side_effect = subprocess.TimeoutExpired("fake", 1)
        process.poll.return_value = None
        with mock.patch.object(rpc.subprocess, "Popen", return_value=process) as spawn, mock.patch.object(rpc, "command") as command:
            rpc.revoke_session(["fake"], self.base, "session")
        process.wait.assert_called_once_with(timeout=1)
        process.kill.assert_called_once()
        command.assert_not_called()
        self.assertEqual(spawn.call_args.kwargs["stdout"], subprocess.DEVNULL)
        self.assertEqual(spawn.call_args.kwargs["stderr"], subprocess.DEVNULL)
        self.assertNotIn(TOKEN, str(spawn.call_args))

    def test_signal_revokes_on_main_thread_and_exits_without_pool_unwind(self):
        for signum in (signal.SIGTERM, signal.SIGINT, getattr(signal, "SIGHUP", 1), getattr(signal, "SIGBREAK", 21)):
            with self.subTest(signal=signum):
                entered, release = threading.Event(), threading.Event()
                store = FakeAuthStore(time.monotonic)
                def worker():
                    with rpc.bearer(["fake"], self.base, time.monotonic() + 90):
                        entered.set()
                        release.wait(5)
                pool = ThreadPoolExecutor(max_workers=1)
                try:
                    with mock.patch.object(rpc, "auth_command", side_effect=store.command), mock.patch.object(rpc, "revoke_session") as revoke, mock.patch.object(rpc, "_terminating", False), mock.patch.object(rpc.threading, "Timer") as timer, mock.patch.object(rpc.os, "_exit", side_effect=SystemExit) as exit:
                        future = pool.submit(worker)
                        self.assertTrue(entered.wait(2))
                        threads = []
                        revoke.side_effect = lambda *a: threads.append(threading.current_thread())
                        start = time.monotonic()
                        with self.assertRaises(SystemExit): rpc.terminate(signum)
                        self.assertLess(time.monotonic() - start, 2)
                        self.assertFalse(future.done(), "worker must still be blocked at immediate exit")
                        self.assertEqual(threads, [threading.main_thread()])
                        self.assertLessEqual(revoke.call_args.args[-1], 1)
                        self.assertEqual(timer.call_args.args[0], 2)
                        exit.assert_called_once_with(128 + signum)
                        release.set()
                        future.result(2)
                finally:
                    release.set()
                    pool.shutdown()

    def test_signal_process_exits_with_a_blocked_pool_worker_and_revoke(self):
        script = r'''import json,pathlib,sys,threading,time
from concurrent.futures import ThreadPoolExecutor
from unittest import mock
sys.path.insert(0,sys.argv[1])
import app_update_t3_rpc as rpc
base=pathlib.Path(sys.argv[2])
entered=threading.Event()
def worker():
    with rpc.bearer([sys.executable, '-c', "import pathlib,sys,time; pathlib.Path(sys.argv[-1], 'revoked').touch(); time.sleep(30)"],base,time.monotonic()+900):
        entered.set()
        threading.Event().wait(60)
with mock.patch.object(rpc,'auth_command',return_value=json.dumps({'sessionId':'known','token':'fixture'})):
    with ThreadPoolExecutor(max_workers=1) as pool:
        pool.submit(worker)
        entered.wait(2)
        print('ready', flush=True)
        rpc.terminate(15)
'''
        self.assert_signal_child_exits(script)
        self.assertTrue((self.base / "revoked").exists())

    def test_signal_watchdog_exits_even_when_revoke_itself_stalls(self):
        script = r'''import pathlib,sys,time
from unittest import mock
sys.path.insert(0,sys.argv[1])
import app_update_t3_rpc as rpc
rpc._known_sessions['known']=(['fake'],pathlib.Path(sys.argv[2]))
with mock.patch.object(rpc,'revoke_session',side_effect=lambda *a: time.sleep(30)):
    print('ready', flush=True)
    rpc.terminate(15)
'''
        self.assert_signal_child_exits(script)

    def assert_signal_child_exits(self, script):
        child = subprocess.Popen([sys.executable, "-B", "-c", script, str(pathlib.Path(rpc.__file__).parent), str(self.base)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # Python startup is outside the host's signal grace. Start the
            # measurement at delivery, allowing parent/OS observation latency.
            self.assertEqual(child.stdout.readline().strip(), "ready")
            start = time.monotonic()
            output, errors = child.communicate(timeout=3)
            self.assertEqual(child.returncode, 143, errors)
            self.assertLess(time.monotonic() - start, 3)
            self.assertNotIn(TOKEN, output + errors)
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=3)

    def test_installs_and_restores_native_signal_handlers(self):
        numbers = [signal.SIGTERM, signal.SIGINT, getattr(signal, "SIGBREAK" if os.name == "nt" else "SIGHUP")]
        originals = {n: signal.getsignal(n) for n in numbers}
        with rpc.session_signals():
            for number in numbers: self.assertIs(signal.getsignal(number), rpc.terminate)
        for number in numbers: self.assertEqual(signal.getsignal(number), originals[number])

    @unittest.skipIf(os.name == "nt", "Unix signal delivery only")
    def test_unix_real_signals_use_direct_exit(self):
        for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            with mock.patch.object(rpc.os, "_exit", side_effect=SystemExit), mock.patch.object(rpc.threading, "Timer"), mock.patch.object(rpc, "_terminating", False):
                with self.assertRaises(SystemExit), rpc.session_signals():
                    os.kill(os.getpid(), number)


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
