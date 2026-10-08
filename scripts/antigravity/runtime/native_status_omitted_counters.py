"""Pure conditional candidate; no I/O, native execution or production enabling.

PublisherBinding is supplied by a reviewed private runtime attestor, not HTTP
input. This module does not implement or earn that attestation itself.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import math
from types import MappingProxyType
from typing import Mapping


NATIVE_SHA256 = '32f9478e47d1d456e090f7ed0a297fcde2fa45ec10839904f03ea1ba00645e49'
PRODUCER_CONTRACT_SHA256 = '094c93a1f861168b1436648a49920d04eb903c48ad8453b279676c1d65b420fe'
MAX_FRAME_BYTES = 64 * 1024
MAX_SAMPLE_AGE_SECONDS = 1.0
REQUIRED_TEXT = ('email', 'conversation_id', 'cwd', 'version', 'agent_state')
COUNTER_DEFAULTS = {'pending_input_count': 0, 'tool_confirmation_pending': False, 'task_count': 0}


class NormalizationRefusal(Exception):
    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


@dataclass(frozen=True)
class PublisherBinding:
    native_sha256: str
    producer_contract_sha256: str
    expected_email: str
    expected_conversation_id: str
    expected_cwd: str
    sampled_at_monotonic: float
    owned_native_pid_birth_verified: bool = False
    unix_peer_and_ancestor_verified: bool = False
    exact_publisher_verified: bool = False
    credential_identity_and_revision_current: bool = False
    input_pty_generation_unchanged: bool = False
    runtime_session_workspace_verified: bool = False
    frame_completed: bool = False


@dataclass(frozen=True)
class NormalizedIdleFields:
    fields: Mapping[str, str | int | bool]
    omitted_fields: tuple[str, ...]
    sampled_at_monotonic: float


def normalize_idle_fields(
    raw_frame: bytes,
    binding: PublisherBinding,
    *,
    now_monotonic: float,
    producer_contract_approved: bool = False,
) -> NormalizedIdleFields:
    """Normalize only the pinned publisher after separate root contract approval.

    Success is private validated field data, not RuntimeProof or permission to
    stop processes. Positive counters, pending confirmation and non-idle agent
    state refuse; the caller retains its complete existing runtime gates.
    """
    if type(producer_contract_approved) is not bool or not producer_contract_approved:
        raise NormalizationRefusal('producer-contract-not-approved')
    if type(binding) is not PublisherBinding:
        raise NormalizationRefusal('publisher-unbound')
    if binding.native_sha256 != NATIVE_SHA256 or binding.producer_contract_sha256 != PRODUCER_CONTRACT_SHA256:
        raise NormalizationRefusal('publisher-pin-mismatch')
    checks = (
        binding.owned_native_pid_birth_verified, binding.unix_peer_and_ancestor_verified,
        binding.exact_publisher_verified, binding.credential_identity_and_revision_current,
        binding.input_pty_generation_unchanged, binding.runtime_session_workspace_verified,
        binding.frame_completed,
    )
    if any(type(value) is not bool or not value for value in checks):
        raise NormalizationRefusal('publisher-unbound')
    if type(now_monotonic) not in (int, float) or type(binding.sampled_at_monotonic) not in (int, float):
        raise NormalizationRefusal('sample-time-invalid')
    if not math.isfinite(now_monotonic) or not math.isfinite(binding.sampled_at_monotonic):
        raise NormalizationRefusal('sample-time-invalid')
    age = now_monotonic - binding.sampled_at_monotonic
    if not 0 <= age <= MAX_SAMPLE_AGE_SECONDS:
        raise NormalizationRefusal('sample-stale-or-future')
    if type(raw_frame) is not bytes or not 0 < len(raw_frame) <= MAX_FRAME_BYTES:
        raise NormalizationRefusal('frame-size-or-type-invalid')

    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise NormalizationRefusal('frame-duplicate-key')
            value[key] = item
        return value

    try:
        frame = json.loads(raw_frame.decode('utf-8'), object_pairs_hook=unique_object,
                           parse_constant=lambda _: (_ for _ in ()).throw(NormalizationRefusal('frame-nonfinite')))
    except NormalizationRefusal:
        raise
    except (ValueError, UnicodeError, RecursionError):
        raise NormalizationRefusal('frame-malformed-or-truncated') from None
    if type(frame) is not dict or len(frame) > 64:
        raise NormalizationRefusal('frame-object-invalid')
    for key in REQUIRED_TEXT:
        value = frame.get(key)
        if type(value) is not str or not 0 < len(value) <= 1024:
            raise NormalizationRefusal('identity-field-missing-or-invalid')
    expected = (binding.expected_email, binding.expected_conversation_id, binding.expected_cwd)
    if any(type(value) is not str or not value for value in expected):
        raise NormalizationRefusal('publisher-unbound')
    if (frame['email'], frame['conversation_id'], frame['cwd']) != expected or frame['version'] != '1.3.2':
        raise NormalizationRefusal('identity-session-or-version-mismatch')
    if frame['agent_state'] != 'idle':
        raise NormalizationRefusal('status-busy-or-unknown')
    fields = {key: frame[key] for key in REQUIRED_TEXT}
    omitted = []
    for key, default in COUNTER_DEFAULTS.items():
        if key not in frame:
            value = default
            omitted.append(key)
        else:
            value = frame[key]
            if type(value) is not type(default) or (type(value) is int and not 0 <= value <= 2**63 - 1):
                raise NormalizationRefusal('counter-type-or-range-invalid')
        if value != default:
            raise NormalizationRefusal('status-busy-or-unknown')
        fields[key] = value
    return NormalizedIdleFields(MappingProxyType(fields), tuple(omitted), binding.sampled_at_monotonic)
