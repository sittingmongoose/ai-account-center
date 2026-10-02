"""Uninstalled native composition factory; no startup or credential side effects.

A real identity/idle/capability validator must be supplied after independent
native verification. The default cannot stop, activate or claim restored runtime.
"""
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from native_header_graphics import NativeHeader
from runtime_continuity import ManagedPtyRuntime, ContinuityError, read_conversation_metadata
from resident_broker import ResidentBroker, actual_open_conversation


class NativeObserver:
    def __init__(self, session, metadata, idle_attestor=None):
        self.session = session; self.metadata = metadata
        self.view = NativeHeader(); self.failed = False; self.input_generation = 0
        self.idle_attestor = idle_attestor

    def feed(self, raw):
        try:self.view.feed(raw)
        except ValueError:self.failed = True
        # Physical terminal handles capabilities on the foreground channel;
        # do not duplicate replies or synthesize user/model/menu input here.
        self.view.take_replies()

    def input_pending(self):self.input_generation += 1
    def exited(self):self.failed = True

    def ready(self):
        return (not self.failed and self.idle_attestor is not None and
                self.idle_attestor(self.session, self.view, self.input_generation) is True)


def create_ubuntu_resident_broker(*, binary, database, socket_path, allowed_children=(),
                                 capability_validator=None, identity_binder=None,
                                 idle_attestor=None, census_provider=None, transaction_validator=None):
    observers = {}

    def metadata(cid, cwd):return read_conversation_metadata(database, cid, cwd)

    def observer_factory(session):
        observer = NativeObserver(session, metadata, idle_attestor)
        observers[id(session)] = observer
        return observer

    def idle(session):
        observed = observers.get(id(session))
        return observed.ready() if observed else False

    def identity(session):
        observed = observers.get(id(session))
        if not observed or observed.failed:raise ContinuityError('runtime-proof-unavailable')
        email = observed.view.email()
        cid = actual_open_conversation(session, runtime, metadata)
        if not email or cid != session.conversation:raise ContinuityError('runtime-proof-unavailable')
        # Google raw subject and fresh credential fingerprint are supplied only
        # by the independent native credential binder, never inferred from email.
        return {'email': email, 'source': 'native-runtime', 'runtimeStarted': True,
                'sessionRestored': True, 'conversationId': cid}

    runtime = ManagedPtyRuntime(binary, metadata, identity, idle, allowed_children)
    return ResidentBroker(runtime, socket_path, observer_factory,
                          lambda session, rt: actual_open_conversation(session, rt, metadata),
                          capability_validator, identity_binder,
                          census_provider=census_provider, transaction_validator=transaction_validator)
