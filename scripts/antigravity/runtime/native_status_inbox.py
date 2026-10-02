"""Two-phase private hook capture; no sample alone proves an account.

Capture only process-bound native whitelist observations. Independently verified
post-start credentials can later bind the exact still-fresh original sample.
Native token refresh cannot turn an old credential fingerprint into a proof.
"""
from __future__ import annotations
from dataclasses import dataclass, field
import time

from native_status_attestor import (NativeStatusAttestor,
    OwnedStatusContext, STATUS_TTL_SECONDS, fail, project_native_status, verified_lineage)


@dataclass(frozen=True)
class OriginBoundSample:
    projection: dict = field(repr=False)
    sampled_monotonic: float
    generation: int
    lineage: tuple = field(repr=False)


class OriginBoundStatusInbox:
    def __init__(self, context: OwnedStatusContext, *, expected_email: str,
                 runtime_observer, inspector, peer_validator, clock=time.monotonic):
        self.context = context
        self.expected_email = expected_email
        self.runtime_observer = runtime_observer
        self.inspector = inspector
        self.peer_validator = peer_validator
        self.clock = clock
        self.input_generation = context.generation
        self.observation = None

    def invalidate_for_input(self):
        self.input_generation += 1
        self.observation = None

    def _runtime(self):
        if self.inspector(self.context.root.pid) != self.context.root:
            fail('status-native-birth-mismatch')
        value = self.runtime_observer()
        if (type(value) is not dict or value.get('nativeOpenedConversation') != self.context.conversation_id or
                value.get('cwd') != self.context.cwd or value.get('hasNativePty') is not True or
                value.get('runtimeStarted') is not True):
            fail('status-runtime-unbound')
        if self.inspector(self.context.root.pid) != self.context.root:
            fail('status-native-birth-mismatch')
        return value

    def accept(self, raw_projection: bytes, *, peer_pid: int, peer_uid: int):
        self.observation = None
        value = project_native_status(raw_projection, projected_only=True)
        lineage = verified_lineage(peer_pid, peer_uid, self.context.root, self.inspector)
        if self.peer_validator(peer_pid) is not True: fail('status-hook-peer-unbound')
        self._runtime()
        if (value['email'] != self.expected_email or value['conversation_id'] != self.context.conversation_id or
                value['cwd'] != self.context.cwd or value['version'] != self.context.version):
            fail('status-native-context-mismatch')
        self.observation = OriginBoundSample(value, self.clock(), self.input_generation, lineage)
        # This is solely a private receipt acknowledgement, never a RuntimeProof.
        return {'accepted': True, 'identityVerified': False}

    def bind_verified(self, attestor: NativeStatusAttestor, *, expected_subject: str):
        attestor.observation = None
        captured = self.observation
        if captured is None: fail('status-observation-missing')
        if (attestor.context.root != self.context.root or
                attestor.context.conversation_id != self.context.conversation_id or
                attestor.context.cwd != self.context.cwd or attestor.context.version != self.context.version or
                attestor.input_generation != captured.generation or
                self.input_generation != captured.generation):
            fail('status-capture-context-mismatch')
        age = self.clock() - captured.sampled_monotonic
        if age < 0 or age > STATUS_TTL_SECONDS: fail('status-observation-stale')
        self._runtime()
        # Attestor checks provider authority, raw subject, fresh verification,
        # current exact post-start native fingerprint and live native context.
        # Original observation time is retained, never restamped after network.
        attestor.observation = (captured.projection, captured.sampled_monotonic,
                                captured.generation, captured.lineage)
        try:
            proof = attestor.proof()
            if (proof['identity']['email'] != self.expected_email or
                    proof['identity']['subject'] != expected_subject):
                fail('status-expected-profile-mismatch')
            return proof
        except BaseException:
            attestor.observation = None
            raise
