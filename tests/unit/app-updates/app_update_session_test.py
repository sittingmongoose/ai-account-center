"""Offline single-session regressions (replaces the former rotation suite)."""
import contextlib
import json
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / 'scripts/app-updates'))
import app_update_t3_rpc as rpc
from app_update_common import UpdateFailure


class SingleSessionTests(unittest.TestCase):
    def setUp(self):
        self.now = 100.0
        self.addCleanup(rpc._known_sessions.clear)
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now))
        self.stack.enter_context(mock.patch.object(rpc.time, 'sleep', side_effect=lambda seconds: setattr(self, 'now', self.now + seconds)))
        self.issue = self.stack.enter_context(mock.patch.object(rpc, 'auth_command', return_value=json.dumps({'sessionId': 's', 'token': 'private-fixture'})))
        self.revoke = self.stack.enter_context(mock.patch.object(rpc, 'revoke_session', return_value=True))

    def test_ttl_tracks_remaining_deadline_and_caps_at_fifteen_minutes(self):
        for deadline, ttl in ((1000, 900), (140.25, 40.25), (2000, 900)):
            with rpc.bearer([], None, deadline): pass
            args = self.issue.call_args.args[2]
            self.assertEqual(float(args[args.index('--ttl') + 1][:-1]), ttl)

    def test_slow_twenty_nine_second_issue_reconnects_without_starving_or_reissuing(self):
        owner = self
        connections, calls = [], []
        def issue(*args):
            self.now += 29
            return json.dumps({'sessionId': 's', 'token': 'private-fixture'})
        self.issue.side_effect = issue
        class Client:
            def __init__(self, origin, token, deadline):
                self.deadline = deadline
                connections.append(token)
                self.number = len(connections)
            def close(self): pass
            def send(self, *args): pass
            def call(self, method, payload):
                calls.append(method)
                owner.assertGreater(self.deadline - owner.now, 800)
                return {'operationId': 'ours'}
            def request(self, method, payload):
                calls.append(method)
                return 'subscription'
            def receive(self, identifier):
                if self.number == 1:
                    raise OSError('offline')
                yield {'operationId': 'ours', 'phase': 'succeeded'}
        with mock.patch.object(rpc, 'Rpc', Client):
            with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as client:
                self.assertEqual(client.call('provider.install.start', {'instanceId': 'agy'})['operationId'], 'ours')
                self.assertEqual(next(client.subscribe('agy'))['operationId'], 'ours')
        self.issue.assert_called_once()
        self.assertEqual(connections, ['private-fixture', 'private-fixture'])
        self.assertEqual(calls.count('provider.install.start'), 1)
        self.assertEqual(calls.count('provider.install.subscribe'), 2)
        self.revoke.assert_called_once()

    def test_ambiguous_start_cannot_adopt_unrelated_snapshot(self):
        # Even a newly changed snapshot has no causal link to our lost reply.
        for error in (OSError(), UpdateFailure('t3_unreachable')):
            client = mock.Mock()
            client.call.side_effect = error
            client.receive.return_value = iter([{'operationId': 'another-callers-operation'}])
            with mock.patch.object(rpc, 'Rpc', return_value=client):
                with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as install:
                    with self.assertRaisesRegex(UpdateFailure, 't3_install_state_unknown'):
                        install.call('provider.install.start', {'instanceId': 'agy'})
            client.call.assert_called_once()
            client.request.assert_not_called()

    def test_finally_retries_confirmed_revoke_on_success_failure_and_exception(self):
        for failure in (None, UpdateFailure('t3_install_failed:installer'), ValueError('private-fixture')):
            self.revoke.reset_mock(side_effect=True)
            self.revoke.side_effect = [False, True]
            try:
                with rpc.bearer([], None, 1000):
                    if failure: raise failure
            except Exception as error:
                self.assertIs(error, failure)
            self.assertEqual(self.revoke.call_count, 2)
            self.assertNotIn('s', rpc._known_sessions)
            self.assertLessEqual(self.revoke.call_args.args[-1], 5)

    def test_failed_revoke_is_bounded_and_sanitized(self):
        self.revoke.return_value = False
        with self.assertRaisesRegex(UpdateFailure, 't3_session_revoke_failed') as error:
            with rpc.bearer([], None, 1000): pass
        self.assertNotIn('private-fixture', str(error.exception))
        self.assertLessEqual(self.now, 105.01)
        self.assertIn('s', rpc._known_sessions)

    def test_issuance_retry_backoff_uses_remaining_deadline(self):
        self.issue.side_effect = [UpdateFailure('t3_cli_not_started'), json.dumps({'sessionId': 's', 'token': 'private-fixture'})]
        with rpc.bearer([], None, 150): pass
        self.assertEqual(self.issue.call_count, 2)
        self.assertEqual(self.issue.call_args_list[0].args[-1], 50)
        self.assertEqual(self.issue.call_args_list[1].args[-1], 49)
        self.assertEqual(self.now, 101)

    def test_slow_issue_gets_the_whole_remaining_deadline(self):
        def issue(cli, base, args, timeout):
            self.assertEqual(timeout, 900)
            self.now += 120
            return json.dumps({'sessionId': 's', 'token': 'private-fixture'})
        self.issue.side_effect = issue
        with rpc.bearer([], None, 1000): pass
        self.issue.assert_called_once()
        self.revoke.assert_called_once()


if __name__ == '__main__': unittest.main()
