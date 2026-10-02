"""Staged runner-owned creation retention; original runtime stays unchanged."""
from dataclasses import dataclass
import os
import select
import subprocess

from runtime_continuity import ManagedPtyRuntime, guard
from native_status_attestor import inspect_process, fail


@dataclass
class CreatedRuntime:
    session: object
    process: subprocess.Popen
    pidfd: int | None = None
    identity: dict | None = None
    birth: str | None = None


class SpawnRetainingRuntime(ManagedPtyRuntime):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.created = []

    def _spawn(self, session):
        super()._spawn(session)
        # Retain Popen immediately, before any following inspection can fail.
        created = CreatedRuntime(session, session.process)
        self.created.append(created)
        if session.process.poll() is not None: return
        created.pidfd = os.pidfd_open(session.process.pid)
        native = inspect_process(session.process.pid)
        if native.parent != os.getpid() or native.uid != os.getuid():
            fail('status-created-child-unbound')
        identity = guard.inspect(session.process.pid, self.allowed)
        if identity['startTime'] != native.birth or inspect_process(native.pid) != native:
            fail('status-created-child-changed')
        created.identity, created.birth = identity, native.birth
        session.owned_identities[identity['pid']] = identity

    def cleanup_created(self):
        complete = True
        for created in self.created:
            try:
                if created.process.poll() is None:
                    if created.pidfd is None or created.identity is None:
                        complete = False; continue
                    if select.select([created.pidfd], [], [], 0)[0]:
                        created.process.wait(timeout=1); continue
                    native = inspect_process(created.process.pid)
                    if (native.birth != created.birth or native.parent != os.getpid() or
                            native.uid != os.getuid() or
                            guard.inspect(created.process.pid, self.allowed) != created.identity):
                        complete = False; continue
                    # Reuse the original full-family observation; unknown or
                    # unreadable family members fail without claiming cleanup.
                    self._family(created.session)
                    receipt = guard.stop_reviewed(list(created.session.owned_identities.values()),
                                                  self.allowed, timeout_seconds=1)
                    if not receipt['complete']:
                        complete = False; continue
                    created.process.wait(timeout=1)
                else:
                    created.process.wait(timeout=1)
            except BaseException:
                complete = False
            finally:
                if created.pidfd is not None:
                    os.close(created.pidfd); created.pidfd = None
        return complete
