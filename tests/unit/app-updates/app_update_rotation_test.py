"""Offline rotation regressions; fake time avoids waiting for credential TTLs."""
import contextlib
import json
import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[3] / 'scripts/app-updates'))
import app_update_t3_rpc as rpc
from app_update_common import UpdateFailure


class RotationTests(unittest.TestCase):
    def setUp(self):
        self.now = 100.0
        self.events = []
        self.serial = 0
        self.addCleanup(rpc._known_sessions.clear)

    def issue(self, cli, base, args, timeout):
        self.assertGreaterEqual(timeout, 16)
        self.now += 16
        self.serial += 1
        self.events.append(('issue', self.serial))
        return json.dumps({'sessionId': str(self.serial), 'token': 'fixture'})

    def test_sixteen_second_issue_and_overlapping_rotation(self):
        owner = self
        class Client:
            def __init__(self, *args): pass
            def close(self): owner.events.append(('close', owner.serial))
        with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc, 'auth_command', side_effect=self.issue), mock.patch.object(rpc, 'revoke_session', return_value=True), mock.patch.object(rpc, 'Rpc', Client):
            with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as client:
                client.refresh()
                self.now = client.renew_at
                client.refresh()
                self.assertLess(self.events.index(('issue', 2)), self.events.index(('close', 2)))

    def test_start_with_point_two_seconds_left_rotates(self):
        owner = self
        class Client:
            def __init__(self, *args): self.deadline = args[-1]
            def close(self): pass
            def call(self, *args):
                owner.assertGreater(self.deadline - owner.now, 1)
                return {'operationId': 'new'}
        with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc, 'auth_command', return_value=json.dumps({'sessionId': 's', 'token': 'fixture'})), mock.patch.object(rpc, 'revoke_session', return_value=True), mock.patch.object(rpc, 'Rpc', Client):
            with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as client:
                client.refresh()
                self.now = client.renew_at - .2
                client.call('provider.install.start', {'instanceId': 'agy'})

    def test_revoke_cli_taking_one_point_three_seconds(self):
        process = mock.Mock()
        def wait(timeout):
            if timeout < 1.3:
                raise rpc.subprocess.TimeoutExpired('fake', timeout)
            return 0
        process.wait.side_effect = wait
        process.poll.return_value = 0
        with mock.patch.object(rpc.subprocess, 'Popen', return_value=process):
            self.assertTrue(rpc.revoke_session([], None, 's'))

    def test_ambiguous_start_recovers_new_operation_without_replay(self):
        for failure in (TimeoutError(), OSError(), UpdateFailure('t3_unreachable')):
            with self.subTest(failure=type(failure).__name__):
                owner = self
                starts = []
                class Client:
                    def __init__(self, *args): pass
                    def close(self): pass
                    def send(self, *args): pass
                    def call(self, method, payload):
                        starts.append(method)
                        raise failure
                    def request(self, method, payload):
                        owner.assertEqual(payload, {'instanceId': 'agy'})
                        return 'subscription'
                    def receive(self, identifier):
                        yield {'operationId': 'old', 'phase': 'succeeded'}
                        yield {'operationId': 'new', 'phase': 'succeeded'}
                with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc, 'auth_command', return_value=json.dumps({'sessionId': 's', 'token': 'fixture'})), mock.patch.object(rpc, 'revoke_session', return_value=True), mock.patch.object(rpc, 'Rpc', Client):
                    with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as client:
                        client.snapshots['agy'] = {'operationId': 'old', 'phase': 'succeeded'}
                        self.assertEqual(client.call('provider.install.start', {'instanceId': 'agy'})['operationId'], 'new')
                    self.assertEqual(starts, ['provider.install.start'])

    def test_failed_revoke_is_retained_and_retried_until_success_or_expiry(self):
        for expires in (200, 101):
            rpc._known_sessions['s'] = ([], None)
            with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc.time, 'sleep', side_effect=lambda seconds: setattr(self, 'now', self.now + seconds)), mock.patch.object(rpc, 'revoke_session', side_effect=[False, True]) as revoke, mock.patch.object(rpc.threading, 'Thread') as thread:
                rpc.retire_session([], None, 's', expires)
                self.assertIn('s', rpc._known_sessions)
                thread.call_args.kwargs['target']()
                self.assertNotIn('s', rpc._known_sessions)
                self.assertEqual(revoke.call_count, 2 if expires == 200 else 1)
            self.now = 100

    def test_transient_issue_failure_retries_with_backoff(self):
        calls = []
        def issue(*args):
            calls.append(args[-1])
            if len(calls) == 1:
                raise UpdateFailure('command_timeout')
            return self.issue(*args)
        with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc.time, 'sleep', side_effect=lambda seconds: setattr(self, 'now', self.now + seconds)), mock.patch.object(rpc, 'auth_command', side_effect=issue), mock.patch.object(rpc, 'revoke_session', return_value=True):
            with rpc.bearer([], None, 1000): pass
        self.assertEqual(len(calls), 2)
        self.assertEqual(self.now, 117)

    def test_real_slow_cli_processes_are_offline(self):
        # CLI-shaped Python children only; no installed T3 or network access.
        with mock.patch.object(rpc, 'environment', return_value=dict(rpc.os.environ)):
            self.assertTrue(rpc.revoke_session([sys.executable, '-c', 'import time; time.sleep(1.3)'], None, 's'))
            cli = [sys.executable, '-c', 'import time,json; time.sleep(16); print(json.dumps({"sessionId":"slow","token":"fixture"}))']
            with mock.patch.object(rpc, 'revoke_session', return_value=True):
                with rpc.bearer(cli, None, rpc.time.monotonic() + 900) as token:
                    self.assertEqual(token, 'fixture')

    def test_renewal_crossing_expiry_recovers_original_operation(self):
        owner = self
        issued = 0
        starts = []
        def issue(*args):
            nonlocal issued
            issued += 1
            if issued == 2:
                # Unexpectedly slow startup exceeds the old session's TTL.
                self.now += 40
            return json.dumps({'sessionId': str(issued), 'token': 'fixture'})
        class Client:
            def __init__(self, *args): self.number = issued; self.deadline = args[-1]
            def close(self): pass
            def send(self, *args): pass
            def call(self, method, payload):
                starts.append(method)
                return {'operationId': 'op'}
            def request(self, *args): return 'subscription'
            def receive(self, identifier):
                if self.number == 1:
                    owner.now = self.deadline
                    raise TimeoutError()
                yield {'operationId': 'op', 'phase': 'succeeded'}
        with mock.patch.object(rpc.time, 'monotonic', side_effect=lambda: self.now), mock.patch.object(rpc, 'auth_command', side_effect=issue), mock.patch.object(rpc, 'revoke_session', return_value=True), mock.patch.object(rpc, 'Rpc', Client):
            with contextlib.closing(rpc.InstallRpc([], None, None, 1000)) as client:
                client.call('provider.install.start', {'instanceId': 'agy'})
                with contextlib.closing(client.subscribe('agy')) as states:
                    self.assertEqual(next(states)['operationId'], 'op')
                self.assertGreater(self.now, 160)
                self.assertEqual(starts, ['provider.install.start'])


if __name__ == '__main__': unittest.main()
