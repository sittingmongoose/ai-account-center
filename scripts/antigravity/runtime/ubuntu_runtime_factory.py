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
    def __init__(self, session, metadata, idle_attestor=None, invalidator=None):
        self.session = session; self.metadata = metadata
        self.view = NativeHeader(); self.failed = False; self.input_generation = 0
        self.idle_attestor = idle_attestor
        self.invalidator = invalidator

    def feed(self, raw):
        try:self.view.feed(raw)
        except ValueError:self.failed = True
        # Physical terminal handles capabilities on the foreground channel;
        # do not duplicate replies or synthesize user/model/menu input here.
        self.view.take_replies()

    def input_pending(self):
        self.input_generation += 1
        if self.invalidator:self.invalidator(self.session)
    def exited(self):
        self.failed = True
        if self.invalidator:self.invalidator(self.session)

    def ready(self):
        return (not self.failed and self.idle_attestor is not None and
                self.idle_attestor(self.session, self.view, self.input_generation) is True)


def create_ubuntu_resident_broker(*, binary, database, socket_path, allowed_children=(),
                                 capability_validator=None, identity_binder=None,
                                 idle_attestor=None, census_provider=None, transaction_validator=None,
                                 native_identity_attestor=None, status_service_factory=None):
    observers = {}
    status = [None]

    def metadata(cid, cwd):return read_conversation_metadata(database, cid, cwd)

    def observer_factory(session):
        observer = NativeObserver(session, metadata,
            lambda current,view,generation: status[0].ready(current) if status[0] else
                (idle_attestor(current,view,generation) is True if idle_attestor else False),
            lambda current:status[0].invalidate(current) if status[0] else None)
        observers[id(session)] = observer
        return observer

    def idle(session):
        observed = observers.get(id(session))
        return observed.ready() if observed else False

    def identity(session):
        observed = observers.get(id(session))
        if not observed or observed.failed:raise ContinuityError('runtime-proof-unavailable')
        if status[0]:return status[0].identity(session)
        if native_identity_attestor is None:raise ContinuityError('runtime-proof-unavailable')
        return native_identity_attestor(session)

    runtime = ManagedPtyRuntime(binary, metadata, identity, idle, allowed_children)
    broker = ResidentBroker(runtime, socket_path, observer_factory,
                          lambda session, rt: actual_open_conversation(session, rt, metadata),
                          capability_validator, identity_binder,
                          census_provider=census_provider, transaction_validator=transaction_validator)

    if status_service_factory:
        status[0] = status_service_factory(broker)
        broker.status_service = status[0]
        broker.identity_binder = status[0].bind
        broker.census_provider = status[0].census
        broker.transaction_validator = status[0].validate_transaction
    return broker
