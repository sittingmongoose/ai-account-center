"""Private native hook composition; one owned, coalesced Google reader.

Import never creates sockets/processes or reads credentials. Original native
sample age, owned process/input/PTY generation and exact current revision remain
independent guards. No observation or private profile is an HTTP DTO.
"""
from dataclasses import replace
from datetime import datetime, timezone
import os
from pathlib import Path
import re
import selectors
import time

from auth_platforms import AuthError
from native_status_attestor import (NativeStatusSocket, NativeStatusAttestor,
    OwnedStatusContext, StatusError, inspect_process, verified_lineage,
    make_hook_peer_validator, sha256, fail)
from native_status_present import ConditionalOriginBoundStatusInbox, project_present_status
from native_status_identity_worker import OwnedIdentityWorker
from runtime_continuity import ContinuityError
from resident_broker import actual_open_conversation

REFUSALS = (StatusError, ContinuityError, AuthError, OSError, ValueError, KeyError, TypeError)


class NativeStatusService:
    def __init__(self, broker, *, path, python_path, helper_path, original_command_file,
                 read_profile, read_snapshot, publisher_validator,
                 producer_contract_approved=False, inspector=inspect_process,
                 worker_factory=OwnedIdentityWorker, clock=time.monotonic):
        self.broker = broker
        self.runtime = broker.runtime
        self.receiver = NativeStatusSocket(Path(path), self)
        self.read_profile = read_profile
        self.read_snapshot = read_snapshot
        self.publisher_validator = publisher_validator
        self.approved = producer_contract_approved
        self.inspector = inspector
        self.worker_factory = worker_factory
        self.clock = clock
        self.records = {}
        self.refused_keys = {}
        self.selector = None
        self.profile = None
        self.worker = None
        self.worker_started = None
        self.verified_monotonic = None
        self.verified_roots = set()
        self.worker_roots = set()
        self.retry_after = 0
        self.peer_validator = make_hook_peer_validator(python_path=Path(python_path),
            helper_path=Path(helper_path), helper_fingerprint=sha256(Path(helper_path).read_bytes()),
            socket_path=Path(path), original_command_file=Path(original_command_file))

    def listen(self, selector):
        self.receiver.listen()
        self.selector = selector
        selector.register(self.receiver.listener, selectors.EVENT_READ, ('status', self))

    def receive(self):
        try:
            self.receiver.receive_one(timeout=0.1)
        except REFUSALS:
            pass  # Fixed refusal only; never native payload/logging.

    def _token(self, session):
        tokens = [token for token, current in self.runtime.sessions.items() if current is session]
        if len(tokens) != 1:
            fail('status-runtime-unbound')
        return tokens[0]

    def _key(self, session):
        root = self.inspector(session.process.pid)
        slave = os.fstat(session.slave)
        return (root, session.generation, self.broker.input_epochs.get(self._token(session), 0),
                session.master, session.slave, slave.st_rdev, slave.st_ino)

    def invalidate(self, session):
        record = self.records.pop(id(session), None)
        if record:
            record['inbox'].invalidate_for_input()

    def _bound_session(self, peer_pid, peer_uid):
        candidates = []
        for session in self.runtime.sessions.values():
            if session.stopped or session.process.poll() is not None:
                continue
            try:
                verified_lineage(peer_pid, peer_uid, self.inspector(session.process.pid), self.inspector)
                candidates.append(session)
            except REFUSALS:
                continue
        if len(candidates) != 1:
            fail('status-peer-unbound')
        return candidates[0]

    def refuse_peer(self, peer_pid, peer_uid):
        # A malformed/latest frame can clear only its earned native lineage.
        # An unrelated local peer cannot invalidate another session's sample.
        try:
            session = self._bound_session(peer_pid, peer_uid)
            self.refused_keys[id(session)] = self._key(session)
            record = self.records.get(id(session))
            if record:
                record['inbox'].observation = None
        except REFUSALS:
            pass

    def _observe(self, session, record):
        if self._key(session) != record['key']:
            fail('status-capture-context-mismatch')
        cid = actual_open_conversation(session, self.runtime, self.runtime.metadata)
        metadata = self.runtime.metadata(session.conversation, session.cwd)
        from runtime_continuity import inspect_private
        checkpoint = inspect_private(session.process.pid, self.runtime.allowed)
        return {'nativeOpenedConversation': cid, 'cwd': checkpoint.cwd,
            'hasNativePty': checkpoint.tty_number != 0,
            'runtimeStarted': session.process.poll() is None, 'sqliteIdle': metadata.get('idle') is True}

    def _new(self, session, value):
        key = self._key(session)
        context = OwnedStatusContext(key[0], session.conversation, session.cwd, '1.2.14', '0' * 64, key[2])
        record = {'key': key, 'session': session}
        record['inbox'] = ConditionalOriginBoundStatusInbox(context, expected_email=value['email'],
            runtime_observer=lambda: self._observe(session, record), inspector=self.inspector,
            peer_validator=self.peer_validator, native_publisher_validator=self.publisher_validator,
            producer_contract_approved=self.approved, clock=self.clock)
        # A new native birth earns a new post-start Google read even when the
        # saved credential bytes are unchanged. Existing frames are not retimed.
        if ((self.profile is not None and key[0] not in self.verified_roots) or
                (self.worker is not None and key[0] not in self.worker_roots)):
            self._close_worker()
            self._clear_profile()
            self.retry_after = 0
        self.records[id(session)] = record
        return record

    def accept(self, raw_projection, *, peer_pid, peer_uid):
        session = self._bound_session(peer_pid, peer_uid)
        # A bound malformed first/latest frame is a refusal, never startup wait.
        self.refused_keys[id(session)] = self._key(session)
        record = self.records.get(id(session))
        if record:
            record['inbox'].observation = None
        value = project_present_status(raw_projection, projected_only=True)
        if (not session.conversation or value['conversation_id'] != session.conversation or
                value['cwd'] != session.cwd):
            fail('status-native-context-mismatch')
        if record and record['key'] != self._key(session):
            self.invalidate(session)
            record = None
        if record is None:
            record = self._new(session, value)
        # No fork occurs in accept. The broker tick starts the coalesced worker
        # after receive_one has closed its transient accepted socket.
        result = record['inbox'].accept(raw_projection, peer_pid=peer_pid, peer_uid=peer_uid)
        self.refused_keys.pop(id(session), None)
        return result

    def startup_pending(self, session, expected, native_key, snapshot_key):
        """Classify only an owned new generation lacking its first earned proof.

        Busy, malformed, stale or changed observations are hard refusals. This
        method supplies no identity/idle proof and never extends sample age.
        """
        try:
            key = self._key(session)
            if (self.approved is not True or key != native_key or session.stopped or
                    session.process.poll() is not None or self.refused_keys.get(id(session)) == key or
                    self._snapshot_key(self.read_snapshot()) != snapshot_key or
                    self.publisher_validator.file_current() is not True or self.publisher_validator(key[0]) is not True):
                return False
            record = self.records.get(id(session))
            if record is None:
                return True  # No complete native frame has arrived for this pinned birth.
            captured = record['inbox'].observation
            if (record['key'] != key or captured is None or captured.generation != key[2] or
                    not 0 <= self.clock() - captured.sampled_monotonic <= 1):
                return False
            value = captured.projection
            if (value['email'] != expected.get('email') or value['conversation_id'] != session.conversation or
                    value['cwd'] != session.cwd or value['version'] != '1.2.14' or value['agent_state'] != 'idle' or
                    any(name in value and value[name] != 0 for name in ('pending_input_count', 'task_count')) or
                    ('tool_confirmation_pending' in value and value['tool_confirmation_pending'] is not False)):
                return False
            # Missing counters remain unearned here; only the final attestor may
            # apply the separately pinned conditional producer contract.
            observed = self._observe(session, record)
            return (self.profile is None and self.worker is not None and
                    self.clock() - self.worker_started <= 5 and
                    observed['runtimeStarted'] is True and observed['hasNativePty'] is True and
                    observed['sqliteIdle'] is True and observed['nativeOpenedConversation'] == session.conversation and
                    observed['cwd'] == session.cwd)
        except REFUSALS:
            return False

    def startup_ready(self, session, expected, snapshot_key):
        try:
            proof = self.proof(session)
            return (proof['identity']['email'] == expected.get('email') and
                    proof['identity']['subject'] == expected.get('subject') and
                    proof['credentialFingerprint'] == snapshot_key[3])
        except REFUSALS:
            return False

    @staticmethod
    def _snapshot_key(snapshot):
        return (snapshot.device, snapshot.inode, snapshot.modified_ns, snapshot.credential.revision)

    def _clear_profile(self):
        self.profile = None
        self.verified_roots = set()
        for record in self.records.values():
            record['inbox'].observation = None

    def _close_worker(self):
        if self.worker is not None:
            self.worker.close()
            self.worker = None

    def _worker_close_fds(self):
        values = [fd for current in self.runtime.sessions.values() for fd in (current.master, current.slave)]
        values += [self.receiver.listener.fileno()] if self.receiver.listener else []
        values += [self.broker.listener.fileno()] if self.broker.listener else []
        values += [client.fileno() for client in self.broker.clients]
        if self.selector is not None and hasattr(self.selector, 'fileno'):
            values.append(self.selector.fileno())
        return tuple(values)

    @staticmethod
    def _fresh_identity(value, *, source, allow_expired=False):
        if (type(value) is not dict or value.get('source') not in source or
                type(value.get('email')) is not str or not value['email'] or
                type(value.get('subject')) is not str or not re.fullmatch(r'[0-9]{1,128}', value['subject']) or
                type(value.get('verifiedAt')) is not str):
            return False
        try:
            verified = datetime.fromisoformat(value['verifiedAt'].replace('Z', '+00:00'))
            age = (datetime.now(timezone.utc) - verified).total_seconds()
            return verified.tzinfo is not None and age >= -5 and (allow_expired or age <= 300)
        except (TypeError, ValueError):
            return False

    def _quiesced_profile(self, current_key, expected):
        """Reuse one bounded current Google reader; never invent a stopped native proof."""
        if (self.profile is not None and
                tuple(self.profile.get('nativeSnapshotIdentity', ())) == current_key and
                ((self.verified_monotonic is not None and self.clock() - self.verified_monotonic > 300) or
                 (self._fresh_identity(self.profile, source=('Google OAuth2 userinfo',), allow_expired=True) and
                  not self._fresh_identity(self.profile, source=('Google OAuth2 userinfo',))))):
            # Refresh an expired authoritative observation only. A foreign
            # identity, denied-current binder or changed snapshot stays refused.
            if (not self._fresh_identity(self.profile, source=('Google OAuth2 userinfo',), allow_expired=True) or
                    self.profile.get('credentialCurrent') is not True or
                    self.profile.get('credentialFingerprint') != current_key[3] or
                    tuple(self.profile.get('nativeSnapshotIdentity', ())) != current_key or
                    any(self.profile[key] != expected[key] for key in ('email', 'subject'))):
                return None
            self._clear_profile()
        self.poll()
        if self.profile is None:
            if self.worker is None:
                if self.clock() < self.retry_after:return None
                self.worker_roots = set()  # A later native birth still needs its own post-start read.
                self.worker_started = self.clock()
                self.retry_after = self.clock() + 60
                self.worker = self.worker_factory(self.read_profile, close_fds=self._worker_close_fds())
            remaining = max(0, 5 - (self.clock() - self.worker_started))
            deadline = time.monotonic() + remaining
            while self.profile is None and self.worker is not None and time.monotonic() < deadline:
                self.poll()
                if self.worker is not None:time.sleep(.01)
            if self.worker is not None:self._close_worker()
        if (type(self.profile) is not dict or
                current_key != tuple(self.profile.get('nativeSnapshotIdentity', ())) or
                self._snapshot_key(self.read_snapshot()) != current_key):
            return None
        return self._current_profile()

    def poll(self):
        for record in list(self.records.values()):
            session = record['session']
            try:
                if (session.stopped or session.process.poll() is not None or
                        self._key(session) != record['key']):
                    self.invalidate(session)
            except REFUSALS:
                self.invalidate(session)
        try:
            if self.profile is not None:
                changed = self._snapshot_key(self.read_snapshot()) != tuple(self.profile['nativeSnapshotIdentity'])
                if changed or self.clock() - self.verified_monotonic > 300:
                    self._clear_profile()
                    self.retry_after = 0
            if self.worker is not None:
                if self.clock() - self.worker_started > 5:
                    self._close_worker()
                else:
                    profile = self.worker.poll()
                    if profile is not None:
                        self._close_worker()
                        # Never bless a worker's snapshot that changed in flight.
                        if self._snapshot_key(self.read_snapshot()) != tuple(profile['nativeSnapshotIdentity']):
                            fail('status-credential-unbound')
                        self.profile = profile
                        self.verified_roots = self.worker_roots.copy()
                        self.verified_monotonic = self.clock()
            pending = any(record['inbox'].observation is not None for record in self.records.values())
            if (self.approved is True and pending and self.profile is None and self.worker is None and
                    self.clock() >= self.retry_after):
                self.worker_roots = {record['key'][0] for record in self.records.values()}
                self.worker_started = self.clock()
                self.retry_after = self.clock() + 60
                self.worker = self.worker_factory(self.read_profile, close_fds=self._worker_close_fds())
        except REFUSALS:
            self._close_worker()
            self._clear_profile()

    def _current_profile(self):
        if type(self.profile) is not dict:
            fail('status-credential-unbound')
        result = dict(self.profile)
        snapshot_key = tuple(self.profile.get('nativeSnapshotIdentity', ()))
        result['credentialCurrent'] = (result.get('credentialCurrent') is True and self._snapshot_key(self.read_snapshot()) ==
            snapshot_key and result.get('credentialFingerprint') == snapshot_key[3])
        return result

    def proof(self, session):
        self.poll()
        record = self.records.get(id(session))
        if not record or record['key'] != self._key(session):
            fail('status-observation-missing')
        profile = self._current_profile()
        context = replace(record['inbox'].context, credential_fingerprint=profile['credentialFingerprint'])
        attestor = NativeStatusAttestor(context, credential_binder=self._current_profile,
            runtime_observer=lambda: self._observe(session, record), inspector=self.inspector,
            peer_validator=self.peer_validator, clock=self.clock)
        return record['inbox'].bind_verified(attestor, expected_subject=profile['subject'])

    def ready(self, session):
        try:
            self.proof(session)
            return True
        except REFUSALS:
            return False

    def identity(self, session):
        try:
            proof = self.proof(session)
            return {'email': proof['identity']['email'], 'source': 'native-runtime',
                'runtimeStarted': True, 'sessionRestored': True, 'conversationId': proof['conversationId']}
        except REFUSALS:
            raise ContinuityError('runtime-proof-unavailable') from None

    def bind(self, expected, proofs):
        try:
            return self._bind(expected, proofs)
        except REFUSALS:
            raise ContinuityError('runtime-proof-unavailable') from None

    def _bind(self, expected, proofs):
        earned = []
        for item in proofs:
            sessions = [s for s in self.runtime.sessions.values() if s.process.pid == item.get('pid')]
            if len(sessions) != 1:
                fail('status-native-birth-mismatch')
            proof = self.proof(sessions[0])
            identity = proof['identity']
            if (identity['email'] != expected.get('email') or identity['subject'] != expected.get('subject') or
                    item.get('birth') != self.inspector(sessions[0].process.pid).birth):
                fail('status-expected-profile-mismatch')
            earned.append(proof)
        if not earned or any(p['credentialFingerprint'] != earned[0]['credentialFingerprint'] for p in earned):
            fail('status-credential-unbound')
        return {key: earned[0][key] for key in ('identity', 'credentialFingerprint', 'runtimeStarted', 'sessionRestored')}

    def census(self, broker):
        unavailable = {'available': False, 'complete': False, 'busy': True,
            'manualActivationInProgress': False, 'sampledAt': None}
        if self.approved is not True or not broker.capability():
            return unavailable
        unmanaged = self.runtime.inspect_unmanaged()
        sessions = [s for s in self.runtime.sessions.values() if not s.stopped and s.process.poll() is None]
        if (unmanaged.get('complete') is not True or unmanaged.get('processes') or not sessions or
                any(not self.ready(s) for s in sessions)):
            return unavailable
        return {'available': True, 'complete': True, 'busy': False,
            'manualActivationInProgress': (broker.current_transaction is not None and
                broker.receipts.get(broker.current_transaction, {}).get('phase') != 'committed'),
            'sampledAt': datetime.now(timezone.utc).isoformat()}

    def validate_transaction(self, phase, state, expected, fingerprint):
        try:
            # The existing broker checks the exact stopped/blocked template,
            # PTY/cwd/env, census, deadline and one-use phase before this call.
            if (not hasattr(self.publisher_validator, 'file_current') or
                    self.publisher_validator.file_current() is not True):
                return False
            tokens = state['binding']['contexts']
            if phase == 'recovery-stop':
                return all(self.ready(self.runtime.sessions[t]) for t in tokens)
            if phase == 'commit':
                # The existing TS coordinator invokes complete only after its
                # durable registry commit. Re-earn runtime identity here; the
                # callback itself does not manufacture a registry observation.
                proofs = [self.proof(self.runtime.sessions[t]) for t in tokens]
                return bool(proofs) and all(
                    p['identity']['email'] == expected.get('email') and
                    p['identity']['subject'] == expected.get('subject') and
                    p['credentialFingerprint'] == self.read_snapshot().credential.revision for p in proofs)
            if phase not in ('quiesced', 'restart'):
                return False
            if any(not self.runtime.sessions[t].stopped or
                   self.runtime.sessions[t].process.poll() is None for t in tokens):
                return False
            current = self.read_snapshot()
            if phase == 'quiesced':
                if (not self.approved or current.credential.revision != fingerprint or
                        not self._fresh_identity(expected, source=('provider-userinfo', 'native-runtime')) or
                        any(expected[key] != state['binding']['expected'].get(key) for key in ('email', 'subject'))):
                    return False
                current_key = self._snapshot_key(current)
                profile = self._quiesced_profile(current_key, expected)
                # The current reader may have waited. Recheck quiescence after
                # that wait, without describing a stopped process as live idle.
                unmanaged = self.runtime.inspect_unmanaged()
                return (self._fresh_identity(profile, source=('Google OAuth2 userinfo',)) and
                        profile.get('credentialCurrent') is True and profile.get('credentialFingerprint') == fingerprint and
                        all(profile[key] == expected[key] for key in ('email', 'subject')) and
                        self._snapshot_key(self.read_snapshot()) == current_key and
                        unmanaged.get('complete') is True and not unmanaged.get('processes') and
                        all(session.stopped and session.process.poll() is not None
                            for session in self.runtime.sessions.values()) and
                        self.publisher_validator.file_current() is True)
            return type(current.credential.revision) is str and len(current.credential.revision) == 64
        except REFUSALS:
            return False

    def close(self):
        for record in list(self.records.values()):
            self.invalidate(record['session'])
        self._close_worker()
        self._clear_profile()
        self.refused_keys.clear()
        if self.receiver.listener and self.selector:
            self.selector.unregister(self.receiver.listener)
        self.receiver.close()
