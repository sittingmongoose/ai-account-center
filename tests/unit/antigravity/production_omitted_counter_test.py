from dataclasses import replace
import json
import unittest

import sys
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[3]/'scripts/antigravity/runtime'))

from native_status_omitted_counters import (
    COUNTER_DEFAULTS, MAX_FRAME_BYTES, NATIVE_SHA256, PRODUCER_CONTRACT_SHA256,
    NormalizationRefusal, PublisherBinding, normalize_idle_fields,
)


class ConditionalOmissionCandidateTests(unittest.TestCase):
    def setUp(self):
        self.frame = {'email': 'fixture@example.invalid', 'conversation_id': 'fixture-conversation',
                      'cwd': '/disposable/project', 'version': '1.2.16', 'agent_state': 'idle'}
        self.binding = PublisherBinding(NATIVE_SHA256, PRODUCER_CONTRACT_SHA256,
                                        self.frame['email'], self.frame['conversation_id'], self.frame['cwd'], 1000.0,
                                        True, True, True, True, True, True, True)

    def normalize(self, frame=None, binding=None, now=1000.5, approved=True):
        raw = json.dumps(self.frame if frame is None else frame).encode()
        return normalize_idle_fields(raw, self.binding if binding is None else binding,
                                     now_monotonic=now, producer_contract_approved=approved)

    def refusal(self, code, callable):
        with self.assertRaises(NormalizationRefusal) as result:
            callable()
        self.assertEqual(result.exception.code, code)
        self.assertEqual(str(result.exception), code)

    def test_omitted_three_defaults_only_under_approved_exact_publisher(self):
        result = self.normalize()
        self.assertEqual(dict(result.fields), self.frame | COUNTER_DEFAULTS)
        self.assertEqual(result.omitted_fields, tuple(COUNTER_DEFAULTS))
        self.assertEqual(result.sampled_at_monotonic, 1000.0)

    def test_explicit_zero_false_is_preserved_without_marking_omitted(self):
        result = self.normalize(self.frame | COUNTER_DEFAULTS)
        self.assertEqual(result.omitted_fields, ())

    def test_mixed_missing_and_explicit_defaults(self):
        result = self.normalize(self.frame | {'task_count': 0})
        self.assertEqual(result.omitted_fields, ('pending_input_count', 'tool_confirmation_pending'))

    def test_default_contract_approval_is_disabled(self):
        self.refusal('producer-contract-not-approved', lambda: normalize_idle_fields(
            json.dumps(self.frame).encode(), self.binding, now_monotonic=1000.5))

    def test_false_or_integer_approval_refuses(self):
        for value in (False, 1):
            with self.subTest(value=value):
                self.refusal('producer-contract-not-approved', lambda: self.normalize(approved=value))

    def test_every_missing_binding_guard_refuses(self):
        for name in ('owned_native_pid_birth_verified', 'unix_peer_and_ancestor_verified', 'exact_publisher_verified',
                     'credential_identity_and_revision_current', 'input_pty_generation_unchanged',
                     'runtime_session_workspace_verified', 'frame_completed'):
            with self.subTest(guard=name):
                self.refusal('publisher-unbound', lambda: self.normalize(binding=replace(self.binding, **{name: False})))

    def test_integer_binding_guard_cannot_substitute_for_true(self):
        self.refusal('publisher-unbound', lambda: self.normalize(binding=replace(self.binding, frame_completed=1)))

    def test_wrong_native_hash_refuses(self):
        self.refusal('publisher-pin-mismatch', lambda: self.normalize(binding=replace(self.binding, native_sha256='0' * 64)))

    def test_wrong_contract_hash_refuses(self):
        self.refusal('publisher-pin-mismatch', lambda: self.normalize(binding=replace(self.binding, producer_contract_sha256='0' * 64)))

    def test_positive_counts_and_pending_confirmation_never_become_idle(self):
        for key, value in [('pending_input_count', 1), ('task_count', 1), ('tool_confirmation_pending', True)]:
            with self.subTest(counter=key):
                self.refusal('status-busy-or-unknown', lambda: self.normalize(self.frame | {key: value}))

    def test_agent_work_or_unknown_state_refuses_even_zero_counts(self):
        for state in ('working', 'waiting', 'unknown', 'Idle'):
            with self.subTest(state=state):
                self.refusal('status-busy-or-unknown', lambda: self.normalize(self.frame | COUNTER_DEFAULTS | {'agent_state': state}))

    def test_boolean_float_negative_null_and_oversized_integer_counts_refuse(self):
        for value in (False, 0.0, -1, None, 2**63):
            with self.subTest(value=value):
                self.refusal('counter-type-or-range-invalid', lambda: self.normalize(self.frame | {'pending_input_count': value}))

    def test_integer_or_null_confirmation_refuses(self):
        for value in (0, None, 'false'):
            with self.subTest(value=value):
                self.refusal('counter-type-or-range-invalid', lambda: self.normalize(self.frame | {'tool_confirmation_pending': value}))

    def test_missing_or_invalid_identity_fields_refuse(self):
        for key in self.frame:
            value = dict(self.frame)
            del value[key]
            with self.subTest(key=key):
                self.refusal('identity-field-missing-or-invalid', lambda: self.normalize(value))

    def test_wrong_account_session_workspace_or_version_refuses(self):
        for key in ('email', 'conversation_id', 'cwd', 'version'):
            with self.subTest(key=key):
                self.refusal('identity-session-or-version-mismatch', lambda: self.normalize(self.frame | {key: 'wrong'}))

    def test_stale_and_future_original_samples_refuse(self):
        for now in (1001.0001, 999.999):
            with self.subTest(now=now):
                self.refusal('sample-stale-or-future', lambda: self.normalize(now=now))

    def test_nonfinite_or_boolean_time_refuses(self):
        for now in (float('nan'), float('inf'), True):
            with self.subTest(now=now):
                self.refusal('sample-time-invalid', lambda: self.normalize(now=now))

    def test_malformed_truncated_and_invalid_utf8_frames_refuse(self):
        for raw in (b'{', b'{"email":', b'\xff', b'{} garbage'):
            with self.subTest(raw=raw):
                self.refusal('frame-malformed-or-truncated', lambda: normalize_idle_fields(
                    raw, self.binding, now_monotonic=1000.5, producer_contract_approved=True))

    def test_duplicate_keys_refuse_and_errors_never_echo_values(self):
        raw = b'{"email":"PRIVATE-CANARY","email":"OTHER-CANARY"}'
        self.refusal('frame-duplicate-key', lambda: normalize_idle_fields(
            raw, self.binding, now_monotonic=1000.5, producer_contract_approved=True))

    def test_nonfinite_json_constant_refuses(self):
        raw = json.dumps(self.frame).encode()[:-1] + b',"pending_input_count":NaN}'
        self.refusal('frame-nonfinite', lambda: normalize_idle_fields(
            raw, self.binding, now_monotonic=1000.5, producer_contract_approved=True))

    def test_empty_oversized_and_nonbytes_refuse(self):
        for raw in (b'', b' ' * (MAX_FRAME_BYTES + 1), '{}'):
            with self.subTest(size=len(raw)):
                self.refusal('frame-size-or-type-invalid', lambda: normalize_idle_fields(
                    raw, self.binding, now_monotonic=1000.5, producer_contract_approved=True))

    def test_scalar_top_level_refuses(self):
        self.refusal('frame-object-invalid', lambda: self.normalize([]))

    def test_unrelated_native_metadata_is_not_returned_or_logged(self):
        result = self.normalize(self.frame | {'model': 'PRIVATE-CANARY', 'nested': {'unrelated': 'BODY-CANARY'}})
        self.assertNotIn('model', result.fields)
        self.assertNotIn('PRIVATE-CANARY', repr(result))

    def test_returned_fields_are_immutable_and_input_is_unchanged(self):
        original = dict(self.frame)
        result = self.normalize(self.frame)
        self.assertEqual(self.frame, original)
        with self.assertRaises(TypeError):
            result.fields['task_count'] = 1


if __name__ == '__main__':
    unittest.main()
