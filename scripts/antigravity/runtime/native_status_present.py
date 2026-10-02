"""Separate Probe13 adapter; no defaults before private earned runtime guards.

Five required explicit fields and only present counters cross a private socket.
The unchanged, reviewed pure normalizer is invoked only after fresh independent
Google/native-revision binding. Original complete-frame receive time is retained.
Nothing here starts a process, verifies a provider, or enables production.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import re
import socket
import struct
import time
import uuid

from native_status_attestor import (FIELDS, MAX_NATIVE_STDIN,
    MAX_PROJECTED_FRAME, STATES, StatusError, bounded_json, fail,
    validate_private_socket, verified_lineage)
from native_status_inbox import OriginBoundStatusInbox
from native_status_omitted_counters import (NATIVE_SHA256,
    PRODUCER_CONTRACT_SHA256, MAX_SAMPLE_AGE_SECONDS, PublisherBinding,
    NormalizationRefusal, normalize_idle_fields)


def project_present_status(raw: bytes, *, projected_only=False):
    """Validate original complete JSON and preserve every counter's absence."""
    value = bounded_json(raw, MAX_PROJECTED_FRAME if projected_only else MAX_NATIVE_STDIN)
    if projected_only and not set(value).issubset(FIELDS):
        fail('status-projection-extra-field')
    if any(key not in value for key in FIELDS[:5]):
        fail('status-fields-missing')
    email = value['email']
    if (type(email) is not str or len(email) > 254 or not re.fullmatch(
            r"[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?", email)):
        fail('status-email-invalid')
    cid = value['conversation_id']
    try:
        if type(cid) is not str or str(uuid.UUID(cid)) != cid:
            fail('status-conversation-invalid')
    except (ValueError, AttributeError):
        fail('status-conversation-invalid')
    cwd = value['cwd']
    if (type(cwd) is not str or not cwd.startswith('/') or len(cwd) > 1024 or
            any(ord(char) < 32 or ord(char) == 127 for char in cwd) or os.path.normpath(cwd) != cwd):
        fail('status-workspace-invalid')
    if type(value['version']) is not str or value['version'] != '1.2.14':
        fail('status-version-invalid')
    if type(value['agent_state']) is not str or value['agent_state'] not in STATES:
        fail('status-state-invalid')
    for key in ('pending_input_count', 'task_count'):
        if key in value and (type(value[key]) is not int or not 0 <= value[key] <= 1_000_000):
            fail('status-count-invalid')
    if 'tool_confirmation_pending' in value and type(value['tool_confirmation_pending']) is not bool:
        fail('status-confirmation-invalid')
    result = {key: value[key] for key in FIELDS if key in value}
    if len(json.dumps(result).encode()) > MAX_PROJECTED_FRAME:
        fail('status-projection-size')
    return result


def send_present_projection(path: Path, projection: dict, timeout=1.0):
    """Same reviewed private socket framing/peer safeguards; no default values."""
    if not 0 < timeout <= 2.0:
        fail('status-socket-timeout-invalid')
    raw = json.dumps(projection, separators=(',', ':')).encode()
    project_present_status(raw, projected_only=True)
    inode = validate_private_socket(path)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(timeout)
        try:
            client.connect(str(path))
            if validate_private_socket(path) != inode:
                fail('status-socket-changed')
            _, uid, _ = struct.unpack('3i', client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            if uid != os.getuid():
                fail('status-socket-peer-unsafe')
            client.sendall(struct.pack('!I', len(raw)) + raw)
            client.shutdown(socket.SHUT_WR)
            # Publisher stays alive through the receiver's SO_PEERCRED/lineage check.
            if client.recv(1) != b'\x01':
                fail('status-socket-not-accepted')
        except StatusError:
            raise
        except OSError:
            fail('status-socket-send-failed')


@dataclass(frozen=True)
class PresentOriginSample:
    raw: bytes = field(repr=False)
    projection: dict = field(repr=False)
    sampled_monotonic: float
    generation: int
    lineage: tuple = field(repr=False)


class ConditionalOriginBoundStatusInbox(OriginBoundStatusInbox):
    def __init__(self, *args, native_publisher_validator, producer_contract_approved=False,
                 normalizer=normalize_idle_fields, **kwargs):
        super().__init__(*args, **kwargs)
        self.native_publisher_validator = native_publisher_validator
        self.producer_contract_approved = producer_contract_approved
        self.normalizer = normalizer
        self.last_normalization = None

    def accept(self, raw_projection: bytes, *, peer_pid: int, peer_uid: int):
        # Start at complete-frame receipt, BEFORE potentially costly validation.
        sampled = self.clock()
        self.observation = None
        self.last_normalization = None
        value = project_present_status(raw_projection, projected_only=True)
        lineage = verified_lineage(peer_pid, peer_uid, self.context.root, self.inspector)
        if self.peer_validator(peer_pid) is not True:
            fail('status-hook-peer-unbound')
        self._runtime()
        if self.native_publisher_validator(self.context.root) is not True:
            fail('status-native-publisher-unbound')
        if (value['email'] != self.expected_email or value['conversation_id'] != self.context.conversation_id or
                value['cwd'] != self.context.cwd or value['version'] != self.context.version):
            fail('status-native-context-mismatch')
        self.observation = PresentOriginSample(raw_projection, value, sampled, self.input_generation, lineage)
        return {'accepted': True, 'identityVerified': False}

    def bind_verified(self, attestor, *, expected_subject: str):
        attestor.observation = None
        self.last_normalization = None
        captured = self.observation
        if captured is None:
            fail('status-observation-missing')
        if (attestor.context.root != self.context.root or
                attestor.context.conversation_id != self.context.conversation_id or
                attestor.context.cwd != self.context.cwd or attestor.context.version != self.context.version or
                attestor.input_generation != captured.generation or
                self.input_generation != captured.generation):
            fail('status-capture-context-mismatch')
        self._runtime()
        if self.native_publisher_validator(self.context.root) is not True:
            fail('status-native-publisher-unbound')
        # Earn current independent identity/revision BEFORE any omitted defaults.
        profile = attestor._bound_profile()
        if profile.get('email') != self.expected_email or profile.get('subject') != expected_subject:
            fail('status-expected-profile-mismatch')
        attestor._observe_runtime()
        binding = PublisherBinding(
            native_sha256=NATIVE_SHA256, producer_contract_sha256=PRODUCER_CONTRACT_SHA256,
            expected_email=self.expected_email, expected_conversation_id=self.context.conversation_id,
            expected_cwd=self.context.cwd, sampled_at_monotonic=captured.sampled_monotonic,
            owned_native_pid_birth_verified=True, unix_peer_and_ancestor_verified=True,
            exact_publisher_verified=True, credential_identity_and_revision_current=True,
            input_pty_generation_unchanged=True, runtime_session_workspace_verified=True,
            frame_completed=True)
        try:
            normalized = self.normalizer(captured.raw, binding, now_monotonic=self.clock(),
                producer_contract_approved=self.producer_contract_approved)
        except NormalizationRefusal as error:
            raise StatusError('status-normalization-' + error.code) from None
        attestor.observation = (dict(normalized.fields), captured.sampled_monotonic,
                                captured.generation, captured.lineage)
        try:
            proof = attestor.proof()
            # Proof work never extends the one-second original sample budget.
            age = self.clock() - captured.sampled_monotonic
            if not 0 <= age <= MAX_SAMPLE_AGE_SECONDS:
                fail('status-observation-stale')
            if (proof['identity']['email'] != self.expected_email or
                    proof['identity']['subject'] != expected_subject):
                fail('status-expected-profile-mismatch')
            self.last_normalization = normalized
            return proof
        except BaseException:
            attestor.observation = None
            raise
