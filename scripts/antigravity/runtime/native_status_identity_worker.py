"""Owned bounded private provider reader; no account/native calls on import."""
import json
import os
from pathlib import Path
import signal
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_continuity import guard
from native_status_attestor import bounded_json, fail

MAX_PRIVATE_RESULT = 4096
PROFILE_FIELDS = {'email', 'subject', 'source', 'verifiedAt', 'credentialFingerprint',
                  'credentialCurrent', 'nativeSnapshotIdentity'}


class OwnedIdentityWorker:
    """Forked source-pinned callback, one small pipe result, mandatory pidfd stop.

    The injected callback is the auth owner's read-only reader. Its process is
    owned by this instance; no preexisting process is discovered or stopped.
    """
    def __init__(self, callback, *, close_fds=()):
        self.pid = None
        self.identity = None
        self.read_fd = None
        self.buffer = bytearray()
        self.closed = False
        self.done = False
        self.allowed = [os.path.realpath(sys.executable)]
        incoming, outgoing = os.pipe()
        start_incoming, start_outgoing = os.pipe()
        pid = os.fork()
        if pid == 0:
            try:
                os.close(incoming)
                os.close(start_outgoing)
                signal.signal(signal.SIGTERM, signal.SIG_DFL)
                # Parent obtains the exact live birth before even a very quick
                # callback can return/exit. Missing approval closes harmlessly.
                if os.read(start_incoming, 1) != b'\x01': os._exit(0)
                os.close(start_incoming)
                for descriptor in close_fds:
                    if descriptor not in (outgoing, incoming):
                        try: os.close(descriptor)
                        except OSError: pass
                null = os.open(os.devnull, os.O_RDWR)
                for descriptor in (0, 1, 2): os.dup2(null, descriptor)
                if null > 2: os.close(null)
                try:
                    value = callback()
                    if type(value) is not dict or set(value) != PROFILE_FIELDS: raise ValueError()
                    raw = json.dumps(value, separators=(',', ':')).encode()
                    if len(raw) > MAX_PRIVATE_RESULT: raise ValueError()
                except BaseException:
                    raw = b'{"safeError":"profile-worker-unavailable"}'
                os.write(outgoing, raw)
            finally:
                os._exit(0)
        os.close(outgoing)
        os.close(start_incoming)
        self.pid, self.read_fd = pid, incoming
        os.set_blocking(incoming, False)
        try:
            self.identity = guard.inspect(pid, self.allowed)
            os.write(start_outgoing, b'\x01')
        except BaseException:
            os.close(start_outgoing)
            os.close(incoming)
            os.waitpid(pid, 0)
            raise
        else:
            os.close(start_outgoing)

    def poll(self):
        if self.closed: fail('profile-worker-closed')
        if self.done: return None
        try: chunk = os.read(self.read_fd, MAX_PRIVATE_RESULT + 1)
        except BlockingIOError: return None
        if chunk:
            self.buffer.extend(chunk)
            if len(self.buffer) > MAX_PRIVATE_RESULT: fail('profile-worker-result-size')
            return None
        self.done = True
        value = bounded_json(bytes(self.buffer), MAX_PRIVATE_RESULT)
        if value == {'safeError': 'profile-worker-unavailable'}:
            fail('profile-worker-unavailable')
        if set(value) != PROFILE_FIELDS: fail('profile-worker-result-invalid')
        snapshot = value['nativeSnapshotIdentity']
        if (type(snapshot) is not list or len(snapshot) != 4 or
                any(type(item) is not int or item < 0 for item in snapshot[:3]) or
                snapshot[3] != value['credentialFingerprint']):
            fail('profile-worker-snapshot-invalid')
        value['nativeSnapshotIdentity'] = tuple(snapshot)
        self.buffer.clear()
        return value

    def close(self):
        if self.closed: return True
        try:
            finished, _ = os.waitpid(self.pid, os.WNOHANG)
            if not finished:
                result = guard.stop_reviewed([self.identity], self.allowed, timeout_seconds=1)
                if not result['complete']: fail('profile-worker-owned-cleanup-incomplete')
                os.waitpid(self.pid, 0)
            return True
        finally:
            os.close(self.read_fd)
            self.read_fd = None
            self.closed = True
            self.buffer.clear()
