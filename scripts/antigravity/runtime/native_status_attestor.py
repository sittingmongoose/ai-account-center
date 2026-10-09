"""Offline/uninstalled native statusLine attestor; importing starts nothing.

Only bounded documented fields cross the private socket. Native process lineage,
the exact owned runtime and freshly verified Google credential identity must all
agree. Missing/stale/busy observations never authorize an account operation.
"""
from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import socket
import stat
import struct
import time
from typing import Callable
import uuid

MAX_NATIVE_STDIN = 65536
MAX_PROJECTED_FRAME = 4096
MAX_JSON_DEPTH = 16
MAX_JSON_NODES = 4096
MAX_LINEAGE_DEPTH = 16
STATUS_TTL_SECONDS = 5.0
PROFILE_TTL_SECONDS = 300.0
FIELDS = ('email', 'conversation_id', 'cwd', 'version', 'agent_state',
          'pending_input_count', 'tool_confirmation_pending', 'task_count')
STATES = frozenset(('idle', 'thinking', 'working', 'tool_use', 'initializing'))


class StatusError(RuntimeError):
    """A fixed code only; never include JSON, subprocess or native error text."""


def fail(code):
    raise StatusError(code)


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            fail('status-json-duplicate-key')
        result[key] = value
    return result


def _reject_constant(_):
    fail('status-json-invalid-number')


def bounded_json(raw: bytes, limit=MAX_NATIVE_STDIN):
    if type(raw) is not bytes or not raw or len(raw) > limit:
        fail('status-input-size')
    try:
        value = json.loads(raw.decode('utf-8'), object_pairs_hook=_unique_object,
                           parse_constant=_reject_constant)
    except StatusError:
        raise
    except (ValueError, UnicodeDecodeError, RecursionError):
        fail('status-json-invalid')
    nodes = 0
    def visit(item, depth):
        nonlocal nodes
        nodes += 1
        if nodes > MAX_JSON_NODES or depth > MAX_JSON_DEPTH:
            fail('status-json-complexity')
        if type(item) is float and not math.isfinite(item):
            fail('status-json-invalid-number')
        if isinstance(item, dict):
            for child in item.values(): visit(child, depth + 1)
        elif isinstance(item, list):
            for child in item: visit(child, depth + 1)
    visit(value, 0)
    if type(value) is not dict:
        fail('status-json-object-required')
    return value


def read_native_stdin(stream):
    raw = stream.read(MAX_NATIVE_STDIN + 1)
    return project_native_status(raw)


def project_native_status(raw: bytes, *, projected_only=False):
    value = bounded_json(raw, MAX_PROJECTED_FRAME if projected_only else MAX_NATIVE_STDIN)
    # Native's documented JSON includes many private/unused fields. They are
    # bounded and discarded, never logged or sent to the receiver.
    if any(field not in value for field in FIELDS):
        fail('status-fields-missing')
    if projected_only and set(value) != set(FIELDS):
        fail('status-projection-extra-field')
    email = value['email']
    if (type(email) is not str or len(email) > 254 or
            not re.fullmatch(r'[A-Za-z0-9.!#$%&\'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?', email)):
        fail('status-email-invalid')
    cid = value['conversation_id']
    try:
        if type(cid) is not str or str(uuid.UUID(cid)) != cid: fail('status-conversation-invalid')
    except (ValueError, AttributeError):
        fail('status-conversation-invalid')
    cwd = value['cwd']
    if (type(cwd) is not str or not cwd.startswith('/') or len(cwd) > 4096 or
            any(ord(char) < 32 or ord(char) == 127 for char in cwd) or
            os.path.normpath(cwd) != cwd):
        fail('status-workspace-invalid')
    version = value['version']
    if type(version) is not str or not re.fullmatch(r'[0-9]{1,5}\.[0-9]{1,5}\.[0-9]{1,5}(?:[-+][A-Za-z0-9.-]{1,40})?', version):
        fail('status-version-invalid')
    if type(value['agent_state']) is not str or value['agent_state'] not in STATES:
        fail('status-state-invalid')
    for field in ('pending_input_count', 'task_count'):
        if type(value[field]) is not int or not 0 <= value[field] <= 1_000_000:
            fail('status-count-invalid')
    if type(value['tool_confirmation_pending']) is not bool:
        fail('status-confirmation-invalid')
    result = {field: value[field] for field in FIELDS}
    if len(json.dumps(result).encode()) > MAX_PROJECTED_FRAME:
        fail('status-projection-size')
    return result


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    birth: str
    uid: int
    parent: int
    executable: str
    executable_device: int
    executable_inode: int


def inspect_process(pid: int):
    if type(pid) is not int or pid <= 0:
        fail('status-process-invalid')
    try:
        proc = Path('/proc') / str(pid)
        raw = (proc / 'stat').read_text()
        rest = raw[raw.rfind(')') + 2:].split()
        parent, start = int(rest[1]), rest[19]
        uid = proc.stat().st_uid
        executable = os.readlink(proc / 'exe')
        exe = (proc / 'exe').stat()
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        return ProcessIdentity(pid, boot + ':' + start, uid, parent, executable,
                               exe.st_dev, exe.st_ino)
    except (OSError, ValueError, IndexError):
        fail('status-process-unreadable')


def verified_lineage(peer_pid, peer_uid, root: ProcessIdentity,
                     inspector=inspect_process):
    if peer_uid != root.uid or peer_pid == root.pid:
        fail('status-peer-unbound')
    observed = []
    pid = peer_pid
    for _ in range(MAX_LINEAGE_DEPTH):
        current = inspector(pid)
        if current.uid != root.uid or current.pid in [item.pid for item in observed]:
            fail('status-peer-unbound')
        observed.append(current)
        if current.pid == root.pid:
            if current != root: fail('status-native-birth-mismatch')
            break
        if current.parent <= 1: fail('status-peer-unbound')
        pid = current.parent
    else:
        fail('status-lineage-depth')
    # Birth/inode/parent checks bind the entire path at the receive boundary.
    if any(inspector(item.pid) != item for item in observed):
        fail('status-lineage-changed')
    return tuple(observed)


@dataclass(frozen=True)
class OwnedStatusContext:
    root: ProcessIdentity
    conversation_id: str
    cwd: str
    version: str
    credential_fingerprint: str
    generation: int


class NativeStatusAttestor:
    """Private in-memory observations only. No native calls or logs are made."""
    def __init__(self, context: OwnedStatusContext, *, credential_binder: Callable,
                 runtime_observer: Callable, inspector=inspect_process,
                 peer_validator=None, clock=time.monotonic,
                 wall_clock=lambda: datetime.now(timezone.utc)):
        self.context = context
        self.binder = credential_binder
        self.runtime_observer = runtime_observer
        self.inspector = inspector
        self.clock = clock
        self.wall_clock = wall_clock
        self.peer_validator = peer_validator or (lambda pid: False)
        self.observation = None
        self.input_generation = context.generation
        if (not re.fullmatch(r'[a-f0-9]{64}', context.credential_fingerprint) or
                type(context.generation) is not int or context.generation < 0):
            fail('status-context-invalid')

    def invalidate_for_input(self):
        self.input_generation += 1
        self.observation = None

    def _bound_profile(self):
        profile = self.binder()
        # This is an injected *fresh* independent Google userinfo result. Raw
        # subject stays in memory and is returned only in the private driver DTO.
        if (type(profile) is not dict or profile.get('source') != 'Google OAuth2 userinfo' or
                profile.get('credentialFingerprint') != self.context.credential_fingerprint or
                profile.get('credentialCurrent') is not True or
                type(profile.get('subject')) is not str or not re.fullmatch(r'[0-9]{1,128}', profile['subject']) or
                type(profile.get('verifiedAt')) is not str or not profile['verifiedAt']):
            fail('status-credential-unbound')
        try:
            verified = datetime.fromisoformat(profile['verifiedAt'].replace('Z', '+00:00'))
            if verified.tzinfo is None: fail('status-credential-unbound')
            age = (self.wall_clock() - verified).total_seconds()
            if not -5 <= age <= PROFILE_TTL_SECONDS: fail('status-credential-stale')
        except (ValueError, TypeError):
            fail('status-credential-unbound')
        return profile

    def _observe_runtime(self):
        if self.inspector(self.context.root.pid) != self.context.root:
            fail('status-native-birth-mismatch')
        runtime = self.runtime_observer()
        if (type(runtime) is not dict or runtime.get('nativeOpenedConversation') != self.context.conversation_id or
                runtime.get('cwd') != self.context.cwd or runtime.get('hasNativePty') is not True or
                runtime.get('runtimeStarted') is not True):
            fail('status-runtime-unbound')
        if self.inspector(self.context.root.pid) != self.context.root:
            fail('status-native-birth-mismatch')
        return runtime

    def accept(self, raw_projection: bytes, *, peer_pid: int, peer_uid: int):
        self.observation = None
        value = project_native_status(raw_projection, projected_only=True)
        lineage = verified_lineage(peer_pid, peer_uid, self.context.root, self.inspector)
        if self.peer_validator(peer_pid) is not True: fail('status-hook-peer-unbound')
        profile = self._bound_profile()
        self._observe_runtime()
        if (value['email'] != profile.get('email') or
                value['conversation_id'] != self.context.conversation_id or
                value['cwd'] != self.context.cwd or value['version'] != self.context.version):
            fail('status-native-context-mismatch')
        # Capture only the projection, monotonic observation time and session
        # generation. Full incoming JSON, environment, argv and chat are absent.
        self.observation = (value, self.clock(), self.input_generation, lineage)
        return {'accepted': True, 'agentState': value['agent_state']}

    def proof(self, *, require_idle=True):
        if self.observation is None: fail('status-observation-missing')
        value, sampled, generation, _ = self.observation
        age = self.clock() - sampled
        if age < 0 or age > STATUS_TTL_SECONDS or generation != self.input_generation:
            fail('status-observation-stale')
        runtime = self._observe_runtime()
        profile = self._bound_profile()
        if value['email'] != profile.get('email'): fail('status-credential-unbound')
        if require_idle and (value['agent_state'] != 'idle' or value['pending_input_count'] != 0 or
                value['tool_confirmation_pending'] or value['task_count'] != 0 or
                runtime.get('sqliteIdle') is not True):
            fail('status-native-not-idle')
        return {'identity': {'email': value['email'], 'subject': profile['subject'],
                             'plan': profile.get('plan'), 'verifiedAt': profile['verifiedAt'],
                             'source': 'native-runtime'},
                'credentialFingerprint': self.context.credential_fingerprint,
                'runtimeStarted': True, 'sessionRestored': True,
                'conversationId': value['conversation_id'], 'cwd': value['cwd'],
                'version': value['version'], 'agentState': value['agent_state']}


def make_hook_peer_validator(*, python_path: Path, helper_path: Path,
                             helper_fingerprint: str, socket_path: Path,
                             original_command_file: Path):
    """Require the pinned owned helper's exact executable/argv, never export it."""
    expected = [str(python_path), '-I', str(helper_path), '--socket', str(socket_path),
                '--original-command-file', str(original_command_file)]
    def validate(pid):
        try:
            info = helper_path.lstat()
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or
                    info.st_nlink != 1 or sha256(helper_path.read_bytes()) != helper_fingerprint):
                return False
            root = Path('/proc') / str(pid)
            if os.readlink(root / 'exe') != str(python_path.resolve()): return False
            raw = (root / 'cmdline').read_bytes()
            if not raw.endswith(b'\x00') or len(raw) > 16384: return False
            args = [part.decode('utf-8') for part in raw[:-1].split(b'\x00')]
            return args == expected
        except (OSError, UnicodeDecodeError):
            return False
    return validate


def validate_private_socket(path: Path):
    try:
        parent, leaf = path.parent.lstat(), path.lstat()
    except OSError: fail('status-socket-unavailable')
    if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or
            stat.S_IMODE(parent.st_mode) != 0o700 or not stat.S_ISSOCK(leaf.st_mode) or
            leaf.st_uid != os.getuid() or stat.S_IMODE(leaf.st_mode) != 0o600):
        fail('status-socket-unsafe')
    return leaf.st_dev, leaf.st_ino


def clear_stale_socket(path: Path) -> bool:
    """Unlink only a dead socket leftover owned by this uid; False leaves the path untouched.

    An absent path is clear. A symlink, regular file, foreign-owned path, live
    listener or probe error refuses. The unlink needs the inode seen before probing.
    """
    try: before = path.lstat()
    except FileNotFoundError: return True
    except OSError: return False
    if not stat.S_ISSOCK(before.st_mode) or before.st_uid != os.getuid(): return False
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            probe.settimeout(1.0)
            probe.connect(str(path))
    except ConnectionRefusedError: pass
    except OSError: return False
    else: return False
    try:
        after = path.lstat()
        if (after.st_dev, after.st_ino) != (before.st_dev, before.st_ino): return False
        path.unlink()
    except OSError: return False
    return True


def send_projection(path: Path, projection: dict, timeout=1.0):
    if not 0 < timeout <= 2.0: fail('status-socket-timeout-invalid')
    raw = json.dumps(projection, separators=(',', ':')).encode()
    project_native_status(raw, projected_only=True)
    inode = validate_private_socket(path)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(timeout)
        try:
            client.connect(str(path))
            if validate_private_socket(path) != inode: fail('status-socket-changed')
            _, uid, _ = struct.unpack('3i', client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid(): fail('status-socket-peer-unsafe')
            client.sendall(struct.pack('!I', len(raw)) + raw)
            client.shutdown(socket.SHUT_WR)
            # Remain alive while SO_PEERCRED ancestry is inspected. No status,
            # raw JSON or identity is echoed to the helper.
            if client.recv(1) != b'\x01': fail('status-socket-not-accepted')
        except StatusError: raise
        except OSError: fail('status-socket-send-failed')


class NativeStatusSocket:
    """Explicitly started private receiver; one bounded frame per invocation."""
    def __init__(self, path: Path, attestor: NativeStatusAttestor):
        self.path, self.attestor = Path(path), attestor
        self.listener, self.inode = None, None

    def listen(self):
        listener = None
        try:
            info = self.path.parent.lstat()
            if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or
                    stat.S_IMODE(info.st_mode) != 0o700): fail('status-socket-directory-unsafe')
            if not clear_stale_socket(self.path): fail('status-socket-already-exists')
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            old = os.umask(0o177)
            try: listener.bind(str(self.path))
            finally: os.umask(old)
            listener.listen(4)
            self.listener = listener
            self.inode = validate_private_socket(self.path)
        except StatusError:
            if listener is not None: listener.close()
            raise
        except OSError:
            if listener is not None: listener.close()
            fail('status-socket-start-failed')

    def receive_one(self, timeout=1.0):
        if self.listener is None or not 0 < timeout <= 2.0:
            fail('status-socket-not-ready')
        self.listener.settimeout(timeout)
        peer_identity = None
        try:
            peer, _ = self.listener.accept()
            with peer:
                deadline = time.monotonic() + timeout
                def bounded_recv(count):
                    remaining = deadline - time.monotonic()
                    if remaining <= 0: fail('status-socket-deadline')
                    peer.settimeout(remaining)
                    return peer.recv(count)
                pid, uid, _ = struct.unpack('3i', peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                peer_identity = (pid, uid)
                def exact(count):
                    output = bytearray()
                    while len(output) < count:
                        chunk = bounded_recv(count - len(output))
                        if not chunk: fail('status-socket-frame-incomplete')
                        output.extend(chunk)
                    return bytes(output)
                size = struct.unpack('!I', exact(4))[0]
                if not 0 < size <= MAX_PROJECTED_FRAME: fail('status-socket-frame-size')
                raw = exact(size)
                if bounded_recv(1): fail('status-socket-extra-frame')
                result = self.attestor.accept(raw, peer_pid=pid, peer_uid=uid)
                peer.sendall(b'\x01')
                return result
        except StatusError:
            if peer_identity is not None and hasattr(self.attestor, 'refuse_peer'):
                self.attestor.refuse_peer(*peer_identity)
            raise
        except OSError:
            if peer_identity is not None and hasattr(self.attestor, 'refuse_peer'):
                self.attestor.refuse_peer(*peer_identity)
            fail('status-socket-receive-failed')

    def close(self):
        if self.listener is not None:
            self.listener.close(); self.listener = None
        if self.inode is not None:
            try:
                if validate_private_socket(self.path) == self.inode: self.path.unlink()
            except StatusError: pass
        self.inode = None
