"""Ubuntu staged runtime adapter. Private argv/env/terminal state never serialize.

Only a managed PTY can be restarted. Discovery of an existing shell-launched CLI
does not grant ownership of its terminal. The identity/idle callbacks must come
from actual native runtime observations; credential files alone are insufficient.
"""
from __future__ import annotations

from contextlib import closing
import dataclasses
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import sqlite3
import subprocess
import struct
import termios
import fcntl
import time
import uuid

GUARD_PATH = Path(__file__).resolve().with_name('linux_process_guard.py')
spec = importlib.util.spec_from_file_location('aic_approved_linux_guard', GUARD_PATH)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
MAX_PROC_BYTES = 2 * 1024 * 1024
MAX_PROCESSES = 128
APPROVAL_TTL_SECONDS = 60
FORBIDDEN_REPLAY = {'--print', '-p', '--prompt', '--prompt-interactive', '-i',
                    '--input-format', '--remote-control', '--continue', '-c',
                    '--dangerously-skip-permissions'}


class ContinuityError(Exception):
    """Only fixed error codes may leave this boundary."""


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def conversation_id(value):
    if not isinstance(value, str):
        raise ContinuityError('invalid-conversation')
    try:
        normalized = str(uuid.UUID(value))
    except (ValueError, AttributeError):
        raise ContinuityError('invalid-conversation') from None
    if value != normalized:
        raise ContinuityError('invalid-conversation')
    return normalized


def _bounded(proc, leaf):
    with (proc / leaf).open('rb') as handle:
        raw = handle.read(MAX_PROC_BYTES + 1)
    if len(raw) > MAX_PROC_BYTES:
        raise ContinuityError('unreviewed-process')
    return raw


@dataclasses.dataclass(repr=False)
class ProcessCheckpoint:
    identity: dict
    parent_pid: int
    session_id: int
    process_group: int
    tty_number: int
    cwd: str = dataclasses.field(repr=False)
    argv: tuple[str, ...] = dataclasses.field(repr=False)
    environment: dict[str, str] = dataclasses.field(repr=False)
    stdin_identity: tuple = dataclasses.field(repr=False)
    checkpoint_fingerprint: str

    def public(self):
        return {'identity': self.identity, 'role': 'cli'}


def inspect_private(pid, allowed):
    identity = guard.inspect(pid, allowed)
    proc = Path('/proc') / str(pid)
    raw = (proc / 'stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    argv = tuple(x.decode('utf-8', errors='surrogateescape')
                 for x in _bounded(proc, 'cmdline').split(b'\0') if x)
    environment = {}
    for item in _bounded(proc, 'environ').split(b'\0'):
        if b'=' in item:
            key, value = item.split(b'=', 1)
            environment[key.decode(errors='surrogateescape')] = value.decode(errors='surrogateescape')
    cwd = os.readlink(proc / 'cwd')
    stdin = os.stat(proc / 'fd/0')
    stdin_identity = (stdin.st_dev, stdin.st_ino, stdin.st_rdev, os.readlink(proc / 'fd/0'))
    parent, group, session, tty = map(int, fields[1:5])
    if guard.inspect(pid, allowed) != identity:
        raise ContinuityError('stale-process')
    fingerprint = digest([identity, parent, group, session, tty, cwd, argv,
                          sorted(environment.items()), stdin_identity])
    return ProcessCheckpoint(identity, parent, session, group, tty, cwd, argv,
                             environment, stdin_identity, fingerprint)


def census_cli(binary, candidates=None, allowed_children=()):
    """Complete installed-AGY candidate census, not all executable discovery.

    Read UID/comm for every visible process. Kernel sets the installed executable
    comm at startup; known AGY helpers and managed ancestry are also reviewed.
    Any unreadable/unrecognized candidate fails closed, rather than being skipped.
    """
    binary = os.path.realpath(binary)
    allowed = [binary, *map(os.path.realpath, allowed_children)]
    names = {Path(filename).name[:15] for filename in allowed}
    names |= {'agy', 'agentapi', 'antigravity', 'language_server', 'language_server_'}
    found = []
    entries = Path('/proc').iterdir() if candidates is None else (
        Path('/proc') / str(pid) for pid in candidates)
    for item in entries:
        if not item.name.isdigit() or int(item.name) <= 1:
            continue
        try:
            if item.stat().st_uid != os.getuid():
                continue
            comm = (item / 'comm').read_text().strip()
            if comm not in names:
                continue
            found.append(inspect_private(int(item.name), allowed))
        except (FileNotFoundError, ProcessLookupError):
            continue
        except PermissionError:
            raise ContinuityError('census-unavailable') from None
    if len(found) > MAX_PROCESSES:
        raise ContinuityError('unreviewed-process')
    return found


def read_conversation_metadata(database, cid, expected_cwd, immutable=False):
    """Only safe metadata columns; never selects title/preview/raw_summary/steps.

    An immutable read deliberately cannot prove current idle state with live WAL.
    Production probe uses read-only SQLite/query_only against an existing DB.
    """
    cid = conversation_id(cid)
    location = Path(database).resolve()
    uri = location.as_uri() + '?mode=ro' + ('&immutable=1' if immutable else '')
    with closing(sqlite3.connect(uri, uri=True, timeout=0.5)) as con:
        con.execute('PRAGMA query_only=ON')
        columns = {row[1] for row in con.execute('PRAGMA table_info(conversation_summaries)')}
        required = {'conversation_id', 'workspace_uris', 'project_id', 'app_data_dir',
                    'not_fully_idle', 'killed', 'step_count', 'last_modified_time'}
        if not required <= columns:
            raise ContinuityError('unsupported-session-schema')
        row = con.execute('SELECT conversation_id,workspace_uris,project_id,app_data_dir,'
                          'not_fully_idle,killed,step_count,last_modified_time '
                          'FROM conversation_summaries WHERE conversation_id=?', (cid,)).fetchone()
    if row is None:
        raise ContinuityError('unknown-conversation')
    from urllib.parse import urlparse, unquote
    try:
        workspaces = json.loads(row[1])
        if isinstance(workspaces, str):
            workspaces = [workspaces]
        paths = []
        for value in workspaces:
            parsed = urlparse(value)
            if parsed.scheme != 'file' or parsed.netloc not in ('', 'localhost'):
                raise ValueError()
            paths.append(os.path.realpath(unquote(parsed.path)))
    except (TypeError, ValueError):
        raise ContinuityError('unverified-workspace') from None
    if os.path.realpath(expected_cwd) not in paths:
        raise ContinuityError('workspace-mismatch')
    idle = row[4] == 0 and row[5] == 0 and not immutable
    return {'conversationId': cid, 'projectId': row[2], 'appDataDir': row[3],
            'cwd': os.path.realpath(expected_cwd), 'idle': idle,
            'stepCount': row[6], 'lastModified': row[7],
            'fingerprint': digest(list(row))}


@dataclasses.dataclass(repr=False)
class OwnedSession:
    master: int
    slave: int
    process: subprocess.Popen = dataclasses.field(repr=False)
    argv: tuple = dataclasses.field(repr=False)
    environment: dict = dataclasses.field(repr=False)
    cwd: str = dataclasses.field(repr=False)
    conversation: str
    generation: int = 1
    stopped: bool = False
    owned_identities: dict = dataclasses.field(default_factory=dict, repr=False)


class ManagedPtyRuntime:
    def __init__(self, binary, metadata, native_identity, idle_signal, allowed_children=()):
        self.binary = os.path.realpath(binary)
        self.allowed = [self.binary, *map(os.path.realpath, allowed_children)]
        self.metadata = metadata
        self.native_identity = native_identity
        self.idle_signal = idle_signal
        self.sessions = {}
        self.approvals = {}

    @staticmethod
    def _safe_args(argv, cid):
        if any(arg.split('=', 1)[0] in FORBIDDEN_REPLAY for arg in argv):
            raise ContinuityError('unsafe-replay')
        explicit = [argv[i + 1] for i, arg in enumerate(argv[:-1]) if arg == '--conversation']
        if explicit != [cid]:
            raise ContinuityError('exact-conversation-required')

    def start_owned(self, args, cwd, env, cid):
        cid = conversation_id(cid)
        self._safe_args(args, cid)
        metadata = self.metadata(cid, cwd)  # Existing conversation and exact cwd.
        if metadata.get('idle') is not True:
            raise ContinuityError('busy-or-unproven-idle')
        master, slave = pty.openpty()
        # Size exists before exec/first native terminal query; never race a
        # native renderer's initial zero-sized PTY observation.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 150, 0, 0))
        session = OwnedSession(master, slave, None, (self.binary, *args), dict(env),
                               os.path.realpath(cwd), cid)
        try:
            self._spawn(session)
            # Record the birth-bound root before any terminal output parsing.
            checkpoint = inspect_private(session.process.pid, self.allowed)
            session.owned_identities[checkpoint.identity['pid']] = checkpoint.identity
        except BaseException:
            os.close(master)
            os.close(slave)
            raise
        token = str(uuid.uuid4())
        self.sessions[token] = session
        return token

    def start_user_owned(self, args, cwd, env):
        """Resident wrapper's ordinary user invocation, never called by HTTP.

        No conversation is guessed. It remains non-restorable until a unique
        actual-open UUID database and its matching workspace are observed.
        """
        if any(arg.split('=', 1)[0] in FORBIDDEN_REPLAY for arg in args):
            raise ContinuityError('unsafe-replay')
        if '--conversation' in args:
            raise ContinuityError('exact-conversation-required')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 150, 0, 0))
        session = OwnedSession(master, slave, None, (self.binary, *args), dict(env),
                               os.path.realpath(cwd), '')
        try:
            self._spawn(session)
            checkpoint = inspect_private(session.process.pid, self.allowed)
            session.owned_identities[checkpoint.identity['pid']] = checkpoint.identity
        except BaseException:
            os.close(master); os.close(slave)
            raise
        token = str(uuid.uuid4()); self.sessions[token] = session
        return token

    def _spawn(self, session):
        def prepare_child():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        session.process = subprocess.Popen(session.argv, cwd=session.cwd,
                                           env=session.environment,
                                           stdin=session.slave, stdout=session.slave,
                                           stderr=session.slave, close_fds=True,
                                           preexec_fn=prepare_child)
        session.stopped = False
        session.owned_identities = {}

    def _family(self, session):
        """Ancestry is discovered first, then every exact executable is reviewed."""
        parent_map = {}
        for item in Path('/proc').iterdir():
            if not item.name.isdigit():
                continue
            try:
                raw = (item / 'stat').read_text()
                fields = raw[raw.rfind(')') + 2:].split()
                parent_map[int(item.name)] = int(fields[1])
            except (FileNotFoundError, ProcessLookupError):
                continue
        owned = {session.process.pid}
        changed = True
        while changed:
            additions = {pid for pid, parent in parent_map.items() if parent in owned} - owned
            changed = bool(additions)
            owned |= additions
            if len(owned) > MAX_PROCESSES:
                raise ContinuityError('unreviewed-process')
        checkpoints = [inspect_private(pid, self.allowed) for pid in sorted(owned)]
        for checkpoint in checkpoints:
            previous = session.owned_identities.get(checkpoint.identity['pid'])
            if previous is not None and previous != checkpoint.identity:
                raise ContinuityError('stale-process')
            session.owned_identities[checkpoint.identity['pid']] = checkpoint.identity
        return checkpoints

    def review(self, token):
        session = self.sessions[token]
        metadata = self.metadata(session.conversation, session.cwd)
        checkpoints = self._family(session)
        if session.process.poll() is not None:
            raise ContinuityError('process-exited')
        # A summary flag alone does not establish idle: runtime prompt/provider
        # must independently prove idle and absence of pending work.
        idle = metadata.get('idle') is True and self.idle_signal(session) is True
        fingerprint = digest([metadata['fingerprint'], session.conversation, session.cwd,
                              session.master, session.slave, session.generation,
                              [cp.checkpoint_fingerprint for cp in checkpoints]])
        self.approvals[fingerprint] = (token, checkpoints, metadata, idle, time.monotonic())
        return {'complete': True, 'processes': [cp.public() for cp in checkpoints],
                'continuity': {'fingerprint': fingerprint, 'restorable': True}, 'idle': idle}

    def inspect_unmanaged(self):
        managed = set()
        for session in self.sessions.values():
            if not session.stopped and session.process.poll() is None:
                managed.update(cp.identity['pid'] for cp in self._family(session))
        rows = [cp.public() for cp in census_cli(self.binary, allowed_children=self.allowed[1:])
                if cp.identity['pid'] not in managed]
        return {'complete': True, 'processes': rows,
                'continuity': {'fingerprint': digest(rows), 'restorable': False} if rows else None}

    def stop_reviewed(self, plan, automatic=False):
        fingerprint = plan.get('continuity', {}).get('fingerprint')
        if fingerprint not in self.approvals:
            raise ContinuityError('unowned-checkpoint')
        token, original, metadata, idle, approved_at = self.approvals[fingerprint]
        if time.monotonic() - approved_at > APPROVAL_TTL_SECONDS:
            raise ContinuityError('expired-checkpoint')
        current = self.review(token)
        if current != plan:
            raise ContinuityError('stale-checkpoint')
        if automatic and not idle:
            raise ContinuityError('busy-or-unproven-idle')
        session = self.sessions[token]
        # Refuse pending native work even for a reviewed manual stop: preserving
        # the exact conversation cannot be inferred by replaying a prompt.
        if not idle:
            raise ContinuityError('busy-or-unproven-idle')
        identities = [cp.identity for cp in original]
        receipt = guard.stop_reviewed(identities, self.allowed)
        if receipt['complete']:
            session.process.wait(timeout=1)
            session.stopped = True
        return {**receipt, 'restartState': token}

    def restart(self, receipt):
        if not receipt.get('complete'):
            raise ContinuityError('incomplete-stop')
        session = self.sessions[receipt['restartState']]
        if not session.stopped or session.process.poll() is None:
            raise ContinuityError('not-stopped')
        metadata = self.metadata(session.conversation, session.cwd)
        if not metadata.get('idle'):
            raise ContinuityError('busy-or-unproven-idle')
        session.generation += 1
        self._spawn(session)  # Same open PTY/cwd/env/exact conversation; no input.

    def prove(self, token, expected_email):
        session = self.sessions[token]
        checkpoint = inspect_private(session.process.pid, self.allowed)
        metadata = self.metadata(session.conversation, session.cwd)
        identity = self.native_identity(session)
        if (session.process.poll() is not None or checkpoint.cwd != session.cwd or
                identity.get('source') != 'native-runtime' or
                identity.get('email') != expected_email or
                identity.get('runtimeStarted') is not True or
                identity.get('conversationId') != session.conversation or
                identity.get('sessionRestored') is not True):
            raise ContinuityError('runtime-proof-unavailable')
        return {**identity, 'cwd': checkpoint.cwd, 'pid': session.process.pid,
                'birth': checkpoint.identity['startTime'],
                'continuityFingerprint': metadata['fingerprint']}

    def close(self, token, allow_owned_pty_hangup=False):
        session = self.sessions[token]
        if session.process.poll() is None:
            self._family(session)
        # Keep birth-bound discovered descendants even if the root has exited
        # and they were reparented. Never discover new ownership from a reused
        # numeric PID or send a signal without the frozen guard validation.
        if session.owned_identities:
            identities = list(session.owned_identities.values())
            receipt = guard.stop_reviewed(identities, self.allowed,
                                         timeout_seconds=2 if allow_owned_pty_hangup else 8)
            if not receipt['complete']:
                if not allow_owned_pty_hangup:
                    raise ContinuityError('owned-cleanup-incomplete')
                # Probe-only terminal teardown, not an account-switch stop.
                # Acquire/revalidate every remaining birth-bound pidfd before
                # closing the exclusive managed PTY, then wait the full family.
                handles = []
                try:
                    for identity in identities:
                        try:
                            handle = os.pidfd_open(identity['pid'])
                        except ProcessLookupError:
                            continue
                        handles.append(handle)
                        if select.select([handle], [], [], 0)[0]:
                            continue
                        if guard.inspect(identity['pid'], self.allowed) != identity:
                            raise ContinuityError('stale-process')
                    if session.process.poll() is None:
                        self._family(session)
                        # Cleanup owns newly created descendants too. Validate
                        # and retain their exact pidfds before the PTY closes.
                        known = {i['pid'] for i in identities}
                        for identity in session.owned_identities.values():
                            if identity['pid'] in known:continue
                            handle = os.pidfd_open(identity['pid'])
                            handles.append(handle)
                            if not select.select([handle], [], [], 0)[0] and \
                                    guard.inspect(identity['pid'], self.allowed) != identity:
                                raise ContinuityError('stale-process')
                    os.close(session.master)
                    os.close(session.slave)
                    session.master = session.slave = -1
                    deadline = time.monotonic() + 8
                    pending = set(handles)
                    while pending and time.monotonic() < deadline:
                        ready = select.select(list(pending), [], [],
                                              max(0, deadline - time.monotonic()))[0]
                        pending.difference_update(ready)
                    if pending:
                        raise ContinuityError('owned-cleanup-incomplete')
                finally:
                    for handle in handles:
                        os.close(handle)
        session.process.wait(timeout=1)
        if session.master >= 0:
            os.close(session.master)
            os.close(session.slave)
        del self.sessions[token]

    def read_private(self, token, maximum=65536, timeout=1):
        session = self.sessions[token]
        if select.select([session.master], [], [], timeout)[0]:
            return os.read(session.master, maximum)
        return b''
