"""Staged same-user resident PTY broker. Importing this file starts nothing.

Control responses whitelist identities/checkpoints; terminal bytes and original
argv/env stay in a separate private foreground channel and are never persisted.
Native proof/idle/capability providers are injected and default to unavailable.
"""
from __future__ import annotations
import base64
import errno
import json
import os
from pathlib import Path
import selectors
import socket
import stat
import struct
import time
import uuid
import datetime as dt
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_continuity import (ManagedPtyRuntime, ContinuityError, OwnedSession,
                                conversation_id, digest, inspect_private, census_cli,
                                APPROVAL_TTL_SECONDS, FORBIDDEN_REPLAY)

MAX_FRAME = 256 * 1024
MAX_OUTPUT_QUEUE = 1024 * 1024
MAX_CONNECTIONS = 16


def encode_frame(value):
    raw = json.dumps(value, separators=(',', ':')).encode()
    if len(raw) > MAX_FRAME:raise ContinuityError('ipc-frame-too-large')
    return struct.pack('!I', len(raw)) + raw


def extract_frame(buffer):
    if len(buffer) < 4:return None
    size = struct.unpack('!I', buffer[:4])[0]
    if size > MAX_FRAME:raise ContinuityError('ipc-frame-too-large')
    if len(buffer) < size + 4:return None
    raw = bytes(buffer[4:size + 4]); del buffer[:size + 4]
    try:value = json.loads(raw)
    except (ValueError, UnicodeDecodeError):raise ContinuityError('ipc-invalid-json') from None
    if not isinstance(value, dict):raise ContinuityError('ipc-invalid-request')
    return value


def private_directory(path):
    directory = Path(path)
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise ContinuityError('ipc-directory-unsafe')
    return directory


def actual_open_conversation(session, runtime, metadata):
    """Adopt only one birth-bound native-open UUID DB with exact workspace.

    This does not guess the newest summary or inspect conversation content. A
    CLI with no opened conversation remains unadopted/non-restorable.
    """
    before = inspect_private(session.process.pid, runtime.allowed)
    if before.cwd != session.cwd:raise ContinuityError('workspace-mismatch')
    candidates = set()
    try:
        for fd in (Path('/proc') / str(session.process.pid) / 'fd').iterdir():
            try:name = Path(os.readlink(fd)).name
            except FileNotFoundError:continue
            for suffix in ('.db', '.db-wal', '.lock'):
                if name.endswith(suffix):
                    candidate = name[:-len(suffix)]
                    try:candidates.add(conversation_id(candidate))
                    except ContinuityError:pass
    except (OSError, ValueError):raise ContinuityError('session-observation-unavailable') from None
    if len(candidates) != 1:return None
    cid = next(iter(candidates))
    observed = metadata(cid, session.cwd)
    if inspect_private(session.process.pid, runtime.allowed) != before:
        raise ContinuityError('stale-process')
    return cid if observed.get('cwd') == session.cwd else None


class ResidentBroker:
    def __init__(self, runtime, socket_path, observer_factory=None, adopter=None,
                 capability=None, identity_binder=None, launch_validator=None,
                 census_provider=None, transaction_validator=None, status_service=None):
        self.runtime = runtime
        self.path = Path(socket_path)
        self.observer_factory = observer_factory
        self.adopter = adopter
        self.capability = capability or (lambda: False)
        self.identity_binder = identity_binder
        self.launch_validator = launch_validator or self._ordinary_args
        self.census_provider = census_provider
        self.status_service = status_service
        # The installed native binder must independently validate exact native
        # identity/revision and phase context. Missing callback is unavailable.
        self.transaction_validator = transaction_validator
        self.plan_bindings = {}
        self.blocked_tokens = set()
        self.input_epochs = {}
        self.current_transaction = None
        self.observers = {}
        self.foregrounds = {}
        self.clients = {}
        self.plans = {}
        self.receipts = {}
        self.restart_tokens = set()
        self.selector = selectors.DefaultSelector()
        self.listener = None
        self.running = False

    @staticmethod
    def _ordinary_args(args):
        # No positional prompt, inference/headless/input replay or unknown flag.
        # The usual no-argument invocation is accepted; exact resume/project are
        # the only staged safe native options. Fixtures inject their own grammar.
        position = 0
        while position < len(args):
            if args[position] not in ('--conversation', '--project') or position + 1 >= len(args):
                return False
            if not args[position + 1] or args[position + 1].startswith('-'):
                return False
            position += 2
        return True

    def listen(self):
        private_directory(self.path.parent)
        # Never unlink a preexisting socket: another broker may own a live PTY.
        if self.path.exists() or self.path.is_symlink():raise ContinuityError('ipc-already-exists')
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        old = os.umask(0o177)
        try:listener.bind(str(self.path))
        finally:os.umask(old)
        listener.listen(MAX_CONNECTIONS); listener.setblocking(False)
        self.listener = listener
        self.selector.register(listener, selectors.EVENT_READ, ('listen', None))
        self.running = True
        if self.status_service:self.status_service.listen(self.selector)

    def _accept(self):
        client, _ = self.listener.accept()
        pid, uid, _ = struct.unpack('3i', client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        if uid != os.getuid() or len(self.clients) >= MAX_CONNECTIONS:
            client.close(); return
        client.setblocking(False)
        state = {'pid': pid, 'uid': uid, 'input': bytearray(), 'output': bytearray(),
                 'foreground': None, 'seen': time.monotonic()}
        self.clients[client] = state
        self.selector.register(client, selectors.EVENT_READ, ('client', client))

    def _send(self, client, value):
        state = self.clients[client]
        state['output'].extend(encode_frame(value))
        if len(state['output']) > MAX_OUTPUT_QUEUE:raise ContinuityError('ipc-output-budget')
        self.selector.modify(client, selectors.EVENT_READ | selectors.EVENT_WRITE, ('client', client))

    def _close_client(self, client):
        state = self.clients.pop(client, None)
        if not state:return
        token = state['foreground']
        if token and self.foregrounds.get(token) is client:self.foregrounds.pop(token, None)
        self.selector.unregister(client); client.close()
        # Client disconnect does not silently kill/restart the native session.

    def _attach_master(self, token):
        session = self.runtime.sessions[token]
        os.set_blocking(session.master, False)
        self.selector.register(session.master, selectors.EVENT_READ, ('pty', token))
        if self.observer_factory:self.observers[token] = self.observer_factory(session)

    def _start_foreground(self, client, message):
        if self.clients[client]['foreground']:raise ContinuityError('foreground-already-attached')
        args, cwd, env = message.get('args'), message.get('cwd'), message.get('environment')
        if (not isinstance(args, list) or len(args) > 64 or any(not isinstance(x, str) or '\0' in x or len(x) > 4096 for x in args)
                or not isinstance(cwd, str) or not os.path.isabs(cwd) or '\0' in cwd
                or not isinstance(env, dict) or len(env) > 1024
                or any(not isinstance(k, str) or not isinstance(v, str) or '\0' in k + v or '=' in k for k, v in env.items())):
            raise ContinuityError('foreground-context-invalid')
        if (any(x.split('=', 1)[0] in FORBIDDEN_REPLAY for x in args) or
                not self.launch_validator(args)):raise ContinuityError('unsafe-replay')
        explicit = [args[i + 1] for i, x in enumerate(args[:-1]) if x == '--conversation']
        if explicit:
            if len(explicit) != 1:raise ContinuityError('exact-conversation-required')
            token = self.runtime.start_owned(args, cwd, env, conversation_id(explicit[0]))
        else:
            # Ordinary user's no-conversation invocation. Preserve argv without
            # fabricating/replaying a prompt. Adoption is independently observed.
            token = self.runtime.start_user_owned(args, cwd, env)
        self._attach_master(token)
        self.input_epochs[token] = 0
        self.clients[client]['foreground'] = token; self.foregrounds[token] = client
        return {'ok': True, 'sessionToken': token}

    def _adopt(self, token):
        session = self.runtime.sessions[token]
        if session.conversation:return True
        if not self.adopter:return False
        cid = self.adopter(session, self.runtime)
        if cid:
            session.conversation = conversation_id(cid)
            # Only future restart adds the actual independently observed ID;
            # original user invocation is not rerun as a new conversation.
            session.argv = (*session.argv, '--conversation', session.conversation)
            return True
        return False

    def _inspect(self):
        unmanaged = self.runtime.inspect_unmanaged()
        processes = list(unmanaged['processes']); entries = []
        restorable = not processes
        for token, session in list(self.runtime.sessions.items()):
            if session.stopped or session.process.poll() is not None:continue
            if not self._adopt(token):
                restorable = False
                processes.extend(cp.public() for cp in self.runtime._family(session)); continue
            plan = self.runtime.review(token)
            # Foreground disconnect or queued user bytes cannot prove ready.
            observer = self.observers.get(token)
            if token not in self.foregrounds or observer is None or not observer.ready():
                plan['idle'] = False
            processes.extend(plan['processes']); entries.append((token, plan))
            restorable = restorable and plan['continuity']['restorable']
        if len(processes) > 128:raise ContinuityError('unreviewed-process')
        fingerprint = digest([processes, [p['continuity']['fingerprint'] for _, p in entries]])
        result = {'complete': True, 'processes': processes,
                  'continuity': {'fingerprint': fingerprint, 'restorable': restorable} if processes else None}
        self.plans[fingerprint] = (entries, result, time.monotonic(),
                                  bool(entries) and all(p.get('idle') is True for _, p in entries))
        return result

    def _context(self, token):
        session = self.runtime.sessions[token]
        peer = self.foregrounds.get(token)
        owner = self.clients.get(peer)
        return [session.conversation, session.cwd, session.master, session.slave,
                digest([session.argv, session.environment]), session.generation,
                self.input_epochs.get(token, 0),
                [owner['pid'], owner['uid'], peer.fileno()] if owner else None]

    def _new_session(self, tokens):
        return any(token not in tokens and not session.stopped and session.process.poll() is None
                   for token, session in self.runtime.sessions.items())

    def _approve_idle_plan(self, plan, expected, fingerprint):
        if (not self.capability() or not isinstance(fingerprint, str) or len(fingerprint) != 64 or
                not plan.get('processes') or not (plan.get('continuity') or {}).get('restorable')):
            return {'approved': False}
        fresh = self._inspect()
        if fresh != plan:return {'approved': False}
        saved = self.plans.get(plan['continuity']['fingerprint'])
        if not saved or saved[3] is not True:return {'approved': False}
        tokens = [token for token, _ in saved[0]]
        bound = self._proof(expected, tokens=tokens)
        if bound.get('credentialFingerprint') != fingerprint:return {'approved': False}
        self.plan_bindings[plan['continuity']['fingerprint']] = {
            'expected': dict(expected), 'fingerprint': fingerprint,
            'contexts': {token: self._context(token) for token in tokens},
            'created': time.monotonic()}
        return {'approved': True}

    def _stop(self, plan):
        fingerprint = (plan.get('continuity') or {}).get('fingerprint')
        saved = self.plans.get(fingerprint)
        if not saved or saved[1] != plan:raise ContinuityError('unowned-checkpoint')
        entries, _, created, idle = saved
        if time.monotonic() - created > APPROVAL_TTL_SECONDS:raise ContinuityError('expired-checkpoint')
        if not plan['continuity']['restorable'] or not idle:raise ContinuityError('busy-or-unproven-idle')
        binding = self.plan_bindings.get(fingerprint)
        if (not binding or time.monotonic() - binding['created'] > APPROVAL_TTL_SECONDS or
                any(self._context(token) != original for token, original in binding['contexts'].items())):
            raise ContinuityError('native-plan-unbound')
        proof = self._proof(binding['expected'], tokens=[token for token, _ in entries])
        if proof.get('credentialFingerprint') != binding['fingerprint']:
            raise ContinuityError('native-account-changed')
        # Complete whole-session revalidation before any signal, including any
        # newly opened unmanaged native instance and foreground input generation.
        if self._inspect() != plan:raise ContinuityError('stale-checkpoint')
        for token, original in entries:
            if self.runtime.review(token) != original:raise ContinuityError('stale-checkpoint')
            observer = self.observers.get(token)
            if not observer or not observer.ready():raise ContinuityError('busy-or-unproven-idle')
        for token, _ in entries:
            self.blocked_tokens.add(token)
            client = self.foregrounds.get(token)
            if client:self._send(client, {'switchState': 'stopping'})
        receipts, stopped = [], []
        complete = True
        try:
            for token, original in entries:
                receipt = self.runtime.stop_reviewed(original)
                receipts.append(receipt); stopped.extend(receipt['stopped'])
                if not receipt['complete']:complete = False; break
        except Exception:
            # Preserve known partial stops; transaction must enter recovery and
            # must not install new auth after incomplete whole-plan stopping.
            complete = False
        # A writer appearing during stopping never gets silently omitted or
        # stopped without review. Report incomplete and forbid credential install.
        try:
            if census_cli(self.runtime.binary, allowed_children=self.runtime.allowed[1:]):complete = False
        except ContinuityError:complete = False
        opaque = str(uuid.uuid4())
        public = {'stopped': stopped, 'complete': complete and len(receipts) == len(entries),
                  'restartState': opaque}
        self.receipts[opaque] = {'public': public, 'receipts': receipts, 'phase': 'stopped',
            'binding': binding, 'recoveryUsed': False, 'created': time.monotonic(),
            'restartContexts': {token: self._context(token) for token, _ in entries}}
        self.current_transaction = opaque
        return public

    def _quiesced(self, receipt, expected, fingerprint):
        unavailable = {'available': False, 'complete': False, 'busy': True,
                       'manualActivationInProgress': False, 'sampledAt': None}
        state = self.receipts.get(receipt.get('restartState'))
        if (not state or state['public'] != receipt or receipt.get('complete') is not True or
                state['phase'] != 'stopped' or self.transaction_validator is None or
                time.monotonic() - state['created'] > APPROVAL_TTL_SECONDS):return unavailable
        for token, original in state['binding']['contexts'].items():
            session = self.runtime.sessions.get(token)
            if (not session or token not in self.blocked_tokens or not session.stopped or
                    session.process.poll() is None or self._context(token) != original or
                    self.runtime.metadata(session.conversation, session.cwd).get('idle') is not True):return unavailable
        if census_cli(self.runtime.binary, allowed_children=self.runtime.allowed[1:]):return unavailable
        if self.transaction_validator('quiesced', state, expected, fingerprint) is not True:return unavailable
        return {'available': True, 'complete': True, 'busy': False,
                'manualActivationInProgress': False, 'sampledAt': dt.datetime.now(dt.timezone.utc).isoformat()}

    def _restart(self, receipt):
        opaque = receipt.get('restartState')
        saved_state = self.receipts.get(opaque)
        if (not receipt.get('complete') or not saved_state or saved_state['public'] != receipt or
                saved_state['phase'] not in ('stopped', 'recovery-stopped')):raise ContinuityError('incomplete-stop')
        for saved in saved_state['receipts']:
            if not saved.get('complete'):raise ContinuityError('incomplete-stop')
        if (self.transaction_validator is None or
                self.transaction_validator('restart', saved_state, None, None) is not True or
                any(self._context(token) != original for token, original in saved_state['restartContexts'].items()) or
                self._new_session(saved_state['binding']['contexts']) or
                self.runtime.inspect_unmanaged().get('processes')):
            raise ContinuityError('restart-context-changed')
        recovering = saved_state['phase'] == 'recovery-stopped'
        if recovering and saved_state['recoveryUsed']:raise ContinuityError('recovery-replay')
        # Phase advances BEFORE any child start, including partial failures.
        saved_state['phase'] = 'recovery-running' if recovering else 'target-running'
        if recovering:saved_state['recoveryUsed'] = True
        for saved in saved_state['receipts']:
            token = saved['restartState']
            self.runtime.restart(saved); self.restart_tokens.add(token)
            if self.observer_factory:self.observers[token] = self.observer_factory(self.runtime.sessions[token])
            # Input remains blocked through native proof and durable registry
            # commit. Restart alone cannot authorize foreground input.
        saved_state['runningContexts'] = {token: self._context(token) for token in saved_state['binding']['contexts']}
        saved_state['runningNativeKeys'] = ({token: self.status_service._key(self.runtime.sessions[token])
            for token in saved_state['binding']['contexts']} if self.status_service else {})
        saved_state['runningCredentialSnapshot'] = (self.status_service._snapshot_key(
            self.status_service.read_snapshot()) if self.status_service else None)
        return {'ok': True}

    def _startup_pending(self, tokens, unready, expected):
        state = self.receipts.get(self.current_transaction)
        if (not self.status_service or not state or state['phase'] not in ('target-running', 'recovery-running') or
                set(tokens) != set(state['binding']['contexts']) or self._new_session(state['binding']['contexts']) or
                any(self._context(token) != original for token, original in state['runningContexts'].items())):
            return False
        unmanaged = self.runtime.inspect_unmanaged()
        if unmanaged.get('complete') is not True or unmanaged.get('processes'):
            return False
        for token in tokens:
            session = self.runtime.sessions[token]
            native_key = state.get('runningNativeKeys', {}).get(token)
            if self.status_service._key(session) != native_key:
                return False
            if token in unready:
                if not self.status_service.startup_pending(session, expected, native_key,
                        state.get('runningCredentialSnapshot')):
                    return False
            else:
                if not self.status_service.startup_ready(session, expected, state.get('runningCredentialSnapshot')):
                    return False
        return True

    def _proof(self, expected, tokens=None):
        if not self.capability() or self.identity_binder is None:raise ContinuityError('runtime-proof-unavailable')
        if not isinstance(expected, dict) or not isinstance(expected.get('email'), str):
            raise ContinuityError('runtime-proof-unavailable')
        proofs = []
        tokens = list(tokens if tokens is not None else self.restart_tokens or
                      [token for token, session in self.runtime.sessions.items() if not session.stopped])
        unready = [token for token in tokens if token not in self.foregrounds or
            self.observers.get(token) is None or not self.observers[token].ready()]
        if unready:
            if self._startup_pending(tokens, unready, expected):
                raise ContinuityError('runtime-startup-not-ready')
            raise ContinuityError('runtime-proof-unavailable')
        for token in tokens:
            observer = self.observers.get(token)
            if token not in self.foregrounds or observer is None or not observer.ready():
                raise ContinuityError('runtime-proof-unavailable')
            proofs.append(self.runtime.prove(token, expected['email']))
        if not proofs or any(p['email'] != expected['email'] for p in proofs):raise ContinuityError('runtime-proof-unavailable')
        # Binder must compare freshly read native credential fingerprint/Google
        # subject against expected identity AND these actual live header proofs.
        bound = self.identity_binder(expected, proofs)
        required = {'identity', 'credentialFingerprint', 'runtimeStarted', 'sessionRestored'}
        if not isinstance(bound, dict) or set(bound) != required:raise ContinuityError('runtime-proof-unavailable')
        return bound

    def _complete(self, expected):
        active = [state for state in self.receipts.values() if state['phase'] in
                  ('target-running', 'recovery-running')]
        if len(active) > 1:raise ContinuityError('transaction-ambiguous')
        self._proof(expected)
        for state in active:
            if (self._new_session(state['binding']['contexts']) or
                    self.runtime.inspect_unmanaged().get('processes') or
                    any(self._context(token) != original for token, original in state.get('runningContexts', {}).items()) or
                    set(state.get('runningContexts', {})) != set(state['binding']['contexts'])):
                raise ContinuityError('transaction-context-changed')
            if self.transaction_validator is None or self.transaction_validator('commit', state, expected, None) is not True:
                raise ContinuityError('transaction-commit-unproved')
            state['phase'] = 'committed'  # A lost reply must never permit rollback.
            for token in state['binding']['contexts']:
                self.blocked_tokens.discard(token)
                self.restart_tokens.discard(token)
                client = self.foregrounds.get(token)
                if client:self._send(client, {'switchState': 'resumed'})
        return {'ok': True}

    def _stop_owned_restarts(self):
        state = self.receipts.get(self.current_transaction)
        if not state or state['phase'] == 'stopped':return {'ok': True}
        if state['phase'] != 'target-running':
            raise ContinuityError('recovery-unavailable')
        if self.transaction_validator is None or self.transaction_validator('recovery-stop', state, None, None) is not True:
            raise ContinuityError('recovery-stop-unproved')
        if self.runtime.inspect_unmanaged().get('processes'):raise ContinuityError('unmanaged-writer')
        if self._new_session(state['binding']['contexts']):raise ContinuityError('unmanaged-writer')
        plans = []
        for token, original in state['binding']['contexts'].items():
            session = self.runtime.sessions.get(token)
            current = self._context(token)
            if (not session or token not in self.blocked_tokens or current[:5] != original[:5] or
                    current[5] not in (original[5], original[5] + 1) or current[6] != original[6]):
                raise ContinuityError('recovery-context-changed')
            if current[7:] != original[7:]:raise ContinuityError('recovery-context-changed')
            if session.stopped and session.process.poll() is not None:continue
            observer = self.observers.get(token)
            if not observer or not observer.ready():raise ContinuityError('recovery-busy')
            plan = self.runtime.review(token)
            if plan.get('idle') is not True or not plan['continuity']['restorable']:
                raise ContinuityError('recovery-busy')
            plans.append((token, plan))
        stopped = []
        try:
            for token, plan in plans:
                receipt = self.runtime.stop_reviewed(plan)
                if receipt.get('complete') is not True:raise ContinuityError('recovery-stop-incomplete')
                stopped.append(receipt)
            if census_cli(self.runtime.binary, allowed_children=self.runtime.allowed[1:]):
                raise ContinuityError('unmanaged-writer')
        except BaseException:
            state['phase'] = 'recovery-required'
            raise
        # Preserve the original PTY/template and immutable public handle. Only
        # this private phase authorizes one recovery generation; no replay reset.
        existing = {receipt['restartState']: receipt for receipt in state['receipts']}
        existing.update({receipt['restartState']: receipt for receipt in stopped})
        state['receipts'] = list(existing.values())
        state['restartContexts'] = {token: self._context(token) for token in state['binding']['contexts']}
        state['phase'] = 'recovery-stopped'
        return {'ok': True}

    def dispatch(self, client, message):
        method = message.get('method')
        schemas = {'launch': {'args','cwd','environment'}, 'attach': {'sessionToken'},
                   'input': {'bytes'}, 'capability': set(), 'inspect': set(), 'census': set(),
                   'stop': {'plan'}, 'restart': {'receipt'}, 'prove': {'expected'},
                   'stop-owned-restarts': set(), 'resize': {'rows','columns'}}
        schemas.update({'approve-idle-plan': {'plan', 'expected', 'credentialFingerprint'},
                        'approve-quiesced-stop': {'receipt', 'expected', 'credentialFingerprint'},
                        'complete-transaction': {'expected'}})
        if method not in schemas:raise ContinuityError('ipc-method-unsupported')
        if set(message) != {'requestId','method'} | schemas[method]:raise ContinuityError('ipc-invalid-request')
        if method in ('stop','restart','prove') and not isinstance(message[next(iter(schemas[method]))], dict):
            raise ContinuityError('ipc-invalid-request')
        if method in ('approve-idle-plan','approve-quiesced-stop','complete-transaction'):
            if not isinstance(message.get('expected'), dict):raise ContinuityError('ipc-invalid-request')
            if method != 'complete-transaction':
                if (not isinstance(message.get('plan' if method == 'approve-idle-plan' else 'receipt'), dict) or
                        not isinstance(message.get('credentialFingerprint'), str) or
                        len(message['credentialFingerprint']) != 64 or
                        any(char not in '0123456789abcdef' for char in message['credentialFingerprint'])):
                    raise ContinuityError('ipc-invalid-request')
        if method == 'launch':return self._start_foreground(client, message)
        if method == 'attach':
            token = message.get('sessionToken')
            if token not in self.runtime.sessions or token in self.foregrounds or self.clients[client]['foreground']:
                raise ContinuityError('foreground-attach-unavailable')
            self.foregrounds[token] = client; self.clients[client]['foreground'] = token
            return {'ok': True}
        if method == 'input':
            token = self.clients[client]['foreground']
            if not token:raise ContinuityError('foreground-not-attached')
            try:raw = base64.b64decode(message.get('bytes', ''), validate=True)
            except (ValueError, TypeError):raise ContinuityError('foreground-input-invalid') from None
            if len(raw) > 16384:raise ContinuityError('foreground-input-budget')
            session = self.runtime.sessions[token]
            if session.stopped or token in self.blocked_tokens:
                self.input_epochs[token] = self.input_epochs.get(token, 0) + 1
                return {'ok': False, 'inputBlocked': True}
            self.input_epochs[token] = self.input_epochs.get(token, 0) + 1
            observer = self.observers.get(token)
            if observer:observer.input_pending()
            # Exactly one original user input delivery; never checkpoint/replay.
            if os.write(session.master, raw) != len(raw):raise ContinuityError('foreground-input-incomplete')
            return {'ok': True}
        if method == 'capability':return {'canProveRuntimeIdentity': self.capability() is True}
        if method == 'census':
            # The native hook binder must independently revalidate current
            # identity, actual open CID/PTY, full census and input generation.
            # A stored self.plans checkpoint never supplies a new observation.
            unavailable = {'available': False, 'complete': False, 'busy': True,
                           'manualActivationInProgress': False, 'sampledAt': None}
            if self.census_provider is None or self.capability() is not True:
                return unavailable
            value = self.census_provider(self)
            if (type(value) is not dict or set(value) != set(unavailable) or
                    any(type(value[k]) is not bool for k in ('available', 'complete', 'busy', 'manualActivationInProgress')) or
                    type(value['sampledAt']) is not str):
                return unavailable
            return value
        if method == 'inspect':return self._inspect()
        if method == 'stop':return self._stop(message.get('plan') or {})
        if method == 'restart':return self._restart(message.get('receipt') or {})
        if method == 'prove':return self._proof(message.get('expected'))
        if method == 'approve-idle-plan':return self._approve_idle_plan(message['plan'], message['expected'], message['credentialFingerprint'])
        if method == 'approve-quiesced-stop':return self._quiesced(message['receipt'], message['expected'], message['credentialFingerprint'])
        if method == 'complete-transaction':return self._complete(message['expected'])
        if method == 'stop-owned-restarts':return self._stop_owned_restarts()
        if method == 'resize':
            token = self.clients[client]['foreground']
            if not token:raise ContinuityError('foreground-not-attached')
            rows, columns = message.get('rows'), message.get('columns')
            if type(rows) is not int or type(columns) is not int or not 10 <= rows <= 200 or not 20 <= columns <= 500:
                raise ContinuityError('terminal-size-unsupported')
            import fcntl, termios
            fcntl.ioctl(self.runtime.sessions[token].slave, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
            return {'ok': True}
        raise ContinuityError('ipc-method-unsupported')

    def tick(self, timeout=0.1):
        if self.status_service:self.status_service.poll()
        for key, events in self.selector.select(timeout):
            kind, target = key.data
            if kind == 'listen':self._accept(); continue
            if kind == 'status':self.status_service.receive(); continue
            if kind == 'pty':
                token = target; session = self.runtime.sessions.get(token)
                if not session:continue
                try:raw = os.read(session.master, 16384)
                except OSError as error:
                    if error.errno in (errno.EAGAIN, errno.EIO):continue
                    raise ContinuityError('terminal-read-unavailable') from None
                if raw:
                    observer = self.observers.get(token)
                    if observer:
                        try:observer.feed(raw)
                        except (ValueError, ContinuityError):observer.exited()
                    client = self.foregrounds.get(token)
                    if client:
                        try:self._send(client, {'terminalOutput': base64.b64encode(raw).decode()})
                        except ContinuityError:self._close_client(client)
                continue
            client = target
            try:
                state = self.clients[client]
                if events & selectors.EVENT_WRITE:
                    count = client.send(state['output']); del state['output'][:count]
                    if not state['output']:self.selector.modify(client, selectors.EVENT_READ, ('client', client))
                if events & selectors.EVENT_READ:
                    raw = client.recv(65536)
                    if not raw:self._close_client(client); continue
                    state['seen'] = time.monotonic(); state['input'].extend(raw)
                    if len(state['input']) > MAX_FRAME + 4:raise ContinuityError('ipc-frame-too-large')
                    while (message := extract_frame(state['input'])) is not None:
                        request_id = message.get('requestId')
                        if not isinstance(request_id, str) or len(request_id) > 64:raise ContinuityError('ipc-request-invalid')
                        try:
                            result = self.dispatch(client, message)
                            if result is not None:self._send(client, {'requestId': request_id, 'result': result})
                        except ContinuityError as error:
                            self._send(client, {'requestId': request_id, 'error': str(error)})
            except (OSError, ContinuityError, KeyError):self._close_client(client)
        # Reap own exited roots without treating old PTY content as fresh proof.
        for token, session in list(self.runtime.sessions.items()):
            if not session.stopped and session.process.poll() is not None:
                client = self.foregrounds.get(token)
                if client and token not in self.blocked_tokens:self._send(client, {'nativeExit': session.process.returncode})
                observer = self.observers.get(token)
                if observer:observer.exited()
                session.stopped = True

    def serve(self):
        try:
            self.listen()
            while self.running:self.tick()
        finally:self.close()

    def close(self):
        failures = []
        if self.status_service:
            try:self.status_service.close()
            except Exception:failures.append('status-worker-cleanup')
        for token in list(self.runtime.sessions):
            try:
                session = self.runtime.sessions[token]
                self.selector.unregister(session.master)
                self.runtime.close(token)
            except Exception:failures.append(token)
        for client in list(self.clients):self._close_client(client)
        if self.listener:
            self.selector.unregister(self.listener); self.listener.close()
            self.path.unlink(missing_ok=True); self.listener = None
        self.selector.close(); self.running = False
        if failures:raise ContinuityError('owned-cleanup-incomplete')
