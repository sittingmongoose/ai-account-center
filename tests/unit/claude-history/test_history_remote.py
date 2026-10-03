"""Synthetic real-filesystem/private-stdio fixtures; no live profile calls."""
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import uuid
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).parent
SOURCE = HERE.parents[2] / 'scripts/claude-history/history_index_remote_v2.py'
spec = importlib.util.spec_from_file_location('history_helper_fixture', SOURCE)
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)

class Fixtures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='history-helper-fixture-')
        self.home = Path(self.temp.name)
        self.account, self.org = str(uuid.uuid4()), str(uuid.uuid4())
        self.policy = {'version': 1, 'enabled': True, 'sourcePlatform': 'windows', 'identity': {'accountUuid': self.account, 'organizationUuid': self.org}, 'project': {'cwd': '/mnt/Cursor/PuppetMaster', 'originCwd': '/mnt/Cursor/PuppetMaster', 'transcriptRoot': '/home/fixture/.claude/projects'}, 'ssh': {p: {'alias': 'fixture-' + p, 'hostname': 'fixture.invalid', 'username': 'fixture', 'port': 22} for p in ('mac', 'windows')}}
        self.root = h.fixed_root(self.home, 'mac', 'gmail')
        self.registry = self.root / 'claude-code-sessions' / self.account / self.org
        self.registry.mkdir(parents=True)
        (self.home / '.ssh').mkdir()
        (self.home / '.ssh/config').write_text('Host fixture-mac\n HostName fixture.invalid\n User fixture\n Port 22\n')
        (self.root / 'config.json').write_bytes(h.encode({'lastKnownAccountUuid': self.account}))
        (self.root / 'ssh_configs.json').write_bytes(h.encode({'configs': [{'sshHost': 'fixture-mac'}]}))
        self.native = {'version': 'fixture', 'managerSha256': 'f' * 64, 'warmGuardVerified': True, 'autoResumeGuardVerified': True}
        self.bound = h.BoundProfile('gmail', 'mac', self.policy, home=self.home, closed=lambda: True, native=lambda: self.native)

    def tearDown(self):
        self.temp.cleanup()

    def candidate(self, **changes):
        sid, cid = 'local_' + str(uuid.uuid4()), str(uuid.uuid4())
        value = {'sessionId': sid, 'cliSessionId': cid, 'cwd': '/mnt/Cursor/PuppetMaster', 'originCwd': '/mnt/Cursor/PuppetMaster', 'createdAt': 1, 'lastActivityAt': 2, 'lastFocusedAt': 0, 'title': 'Synthetic fixture', 'isArchived': False, 'permissionMode': 'default', 'importedFrom': 'local-1p-code', 'resumeConfirmed': False, 'remoteControlAutoEligible': False, 'sshConfig': {'sshHost': 'fixture-mac'}, 'sshRemoteTranscriptPath': '/home/fixture/.claude/projects/fixture/' + cid + '.jsonl'}
        value.update(changes)
        raw = h.encode(value)
        return {'name': sid + '.json', 'base64': base64.b64encode(raw).decode(), 'sha256': h.sha(raw)}

    def put(self, record):
        (self.registry / record['name']).write_bytes(base64.b64decode(record['base64']))

    def test_empty_registry_has_real_stable_protection(self):
        result = self.bound.collect()
        self.assertEqual(result['records'], [])
        self.assertTrue(result['snapshotStable'] and result['protectedSnapshotStable'])
        self.assertTrue(result['noPendingInput'] and result['noScheduledWork'])
        self.assertTrue(self.bound.unchanged(result, []))

    def test_empty_waiting_directory_is_not_a_queue(self):
        (self.registry / 'waiting-input').mkdir()
        result = self.bound.collect()
        self.assertTrue(result['noPendingInput'])
        self.assertEqual(result['protectedSnapshot']['waitingState']['entryNameHashes'], [])

    def test_waiting_record_contents_are_not_read(self):
        waiting = self.registry / 'waiting-input'
        waiting.mkdir()
        (waiting / 'opaque').mkdir()
        result = self.bound.collect()
        self.assertFalse(result['noPendingInput'])
        self.assertNotIn('queueCount', result)

    def test_saved_pending_field_is_not_clean(self):
        self.put(self.candidate(pendingMessages=[{'fixture': 'private'}]))
        self.assertFalse(self.bound.collect()['noPendingInput'])

    def test_tasks_unknown_schema_is_not_clean(self):
        (self.registry / 'scheduled-tasks.json').write_bytes(h.encode({'unexpected': []}))
        self.assertFalse(self.bound.collect()['noScheduledWork'])

    def test_actual_identity_swap_refuses(self):
        (self.root / 'config.json').write_bytes(h.encode({'lastKnownAccountUuid': str(uuid.uuid4())}))
        with self.assertRaises(h.Refused):
            self.bound.collect()

    def test_managed_policy_is_required(self):
        with self.assertRaises(h.Refused):
            h.BoundProfile('gmail', 'mac', {}, home=self.home)
        with self.assertRaises(h.Refused):
            h.BoundProfile('me', 'mac', self.policy, home=self.home)

    def test_protected_bytes_change_refuses(self):
        baseline = self.bound.collect()
        (self.root / 'ssh-remote-server-state.json').write_bytes(h.encode({'fixture': True}))
        with self.assertRaises(h.Refused):
            self.bound.unchanged(baseline, [])

    def test_open_or_unknown_process_guard_refuses(self):
        baseline = self.bound.collect()
        self.bound.closed = lambda: False
        with self.assertRaises(h.Refused):
            self.bound.unchanged(baseline, [])
        self.bound.closed = lambda: h.refuse('process_state_unknown')
        with self.assertRaises(h.Refused):
            self.bound.unchanged(baseline, [])

    def test_source_snapshot_race_is_detected(self):
        self.put(self.candidate())
        original = h.read_regular
        count = {'reads': 0}
        def changed(path, *args, **kwargs):
            raw = original(path, *args, **kwargs)
            if path.name.startswith('local_'):
                count['reads'] += 1
                if count['reads'] == 1:
                    path.write_bytes(raw + b' ')
            return raw
        with patch.object(h, 'read_regular', changed):
            self.assertFalse(self.bound.collect()['snapshotStable'])

    def test_duplicate_cli_identity_refuses(self):
        first = self.candidate()
        self.put(first)
        value = json.loads(base64.b64decode(first['base64']))
        self.put(self.candidate(cliSessionId=value['cliSessionId']))
        with self.assertRaises(h.Refused):
            self.bound.collect()

    def test_existing_native_record_change_refuses(self):
        record = self.candidate()
        self.put(record)
        baseline = self.bound.collect()
        (self.registry / record['name']).write_bytes(base64.b64decode(record['base64']) + b' ')
        with self.assertRaises(h.Refused):
            self.bound.unchanged(baseline, [])

    def test_new_neutral_candidate_and_atomic_stage_link_are_allowed(self):
        baseline = self.bound.collect()
        record = self.candidate()
        stage = self.registry / '.history-index-fixture-stage'
        stage.write_bytes(base64.b64decode(record['base64']))
        os.link(stage, self.registry / record['name'])
        self.assertTrue(self.bound.unchanged(baseline, [record]))
        with self.assertRaises(h.Refused):
            self.bound.collect()

    def test_runtime_or_resume_grant_candidate_refuses(self):
        baseline = self.bound.collect()
        for record in [self.candidate(query={'fixture': True}), self.candidate(resumeConfirmed=True), self.candidate(permissionMode='bypassPermissions')]:
            with self.assertRaises(h.Refused):
                self.bound.unchanged(baseline, [record])

    def test_symlink_descriptor_refuses(self):
        record = self.candidate()
        target = self.home / 'foreign'
        target.write_bytes(base64.b64decode(record['base64']))
        (self.registry / record['name']).symlink_to(target)
        with self.assertRaises(h.Refused):
            self.bound.collect()

    def test_ssh_include_or_wildcard_endpoint_is_unknown(self):
        for prefix in ['Include fixture.conf\n', 'Host *\n Port 22\n']:
            (self.home / '.ssh/config').write_text(prefix + 'Host fixture-mac\n HostName fixture.invalid\n User fixture\n')
            with self.assertRaises(h.Refused):
                self.bound.collect()

    def test_global_execution_directives_cannot_redirect_a_verified_alias(self):
        original = (self.home / '.ssh/config').read_bytes()
        for directive in ('ProxyCommand', 'ProxyJump', 'LocalCommand', 'RemoteCommand', 'PermitLocalCommand'):
            with self.subTest(directive=directive):
                (self.home / '.ssh/config').write_bytes((directive + ' synthetic\n').encode() + original)
                before = (self.home / '.ssh/config').read_bytes()
                with self.assertRaises(h.Refused):
                    self.bound.bind()
                self.assertEqual((self.home / '.ssh/config').read_bytes(), before)

    def test_negated_host_patterns_never_claim_selected_endpoint(self):
        for host in ('fixture-mac !fixture-mac', 'fixture-mac !fixture-*', '!unrelated fixture-mac'):
            with self.subTest(host=host):
                raw = ('Host ' + host + '\n HostName fixture.invalid\n User fixture\n Port 22\n').encode()
                with self.assertRaises(h.Refused):
                    h.endpoint_from_config(raw, self.policy['ssh']['mac'])

    def test_known_simple_selected_block_still_binds_endpoint(self):
        raw = b'# no commands\nHost unrelated\n HostName other.invalid\n User other\nHost fixture-mac\n HostName fixture.invalid\n User fixture\n Port 22\n'
        result = h.endpoint_from_config(raw, self.policy['ssh']['mac'])
        self.assertEqual(result, {'hostnameSha256': h.text_sha('fixture.invalid'), 'usernameSha256': h.text_sha('fixture'), 'portSha256': h.text_sha('22')})

    def test_spawned_real_helper_class_private_pipe_single_json(self):
        record = self.candidate()
        self.put(record)
        bootstrap = 'import sys,json\n' + 'ns={"__name__":"fixture_library"}\nexec(' + repr(SOURCE.read_text()) + ',ns)\n' + 'b=ns["BoundProfile"]("gmail","mac",' + repr(self.policy) + ',home=' + repr(str(self.home)) + ',closed=lambda:True,native=lambda:' + repr(self.native) + ')\n' + 'r=json.load(sys.stdin)\nresult=b.collect() if r["mode"]=="collect" else {"unchanged":b.unchanged(r["expectedTarget"],r.get("records",[]))}\nsys.stdout.buffer.write(ns["encode"](result))\n'
        def run(request):
            p = subprocess.run([sys.executable, '-I', '-c', bootstrap], input=h.encode(request), capture_output=True, timeout=5)
            self.assertEqual(p.returncode, 0)
            self.assertEqual(p.stderr, b'')
            return json.loads(p.stdout)
        baseline = run({'mode': 'collect'})
        self.assertEqual(baseline['records'], [record])
        self.assertEqual(run({'mode': 'protected-check', 'expectedTarget': baseline}), {'unchanged': True})

    def test_cli_invalid_append_marks_possible_mutation_recovery(self):
        p = subprocess.run([sys.executable, '-I', str(SOURCE)], input=h.encode({'mode': 'append'}), capture_output=True, timeout=5)
        output = json.loads(p.stdout)
        self.assertEqual(output, {'status': 'refused', 'reason': 'unavailable', 'createdCount': 0, 'recoveryRequired': True, 'writerQuiescent': True})
        self.assertEqual(p.stderr, b'')

    def test_missing_or_invalid_append_receipt_is_unknown(self):
        for raw in (b'{}', b'', b'{"status":"created_metadata","status":"refused"}', b'[]'):
            with self.assertRaises((h.Refused, ValueError, TypeError)):
                h.append_receipt(raw)

    def test_process_argument_filesystem_alias_is_active(self):
        alternate = self.home / 'a-different-spelling'
        identity = h.directory_identity(self.root)
        with patch.object(h.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'12\n')), patch.object(h, 'mac_arguments', return_value=['Claude', '--user-data-dir=' + str(alternate)]), patch.object(h, 'directory_identity', return_value=identity):
            self.assertFalse(h.local_closed('mac', 'gmail', self.root))

    def test_process_relative_empty_unresolvable_arguments_are_unknown(self):
        for argument in ['', 'relative', str(self.home / 'absent')]:
            with patch.object(h.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'12\n')), patch.object(h, 'mac_arguments', return_value=['Claude', '--user-data-dir=' + argument]):
                with self.assertRaises(h.Refused):
                    h.local_closed('mac', 'gmail', self.root)

    def test_process_other_fixed_profile_directory_is_closed(self):
        other = h.fixed_root(self.home, 'mac', 'party')
        other.mkdir(parents=True)
        with patch.object(h.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'12\n')), patch.object(h, 'mac_arguments', return_value=['Claude', '--user-data-dir=' + str(other)]):
            self.assertTrue(h.local_closed('mac', 'gmail', self.root))

    def test_registry_enumeration_is_bounded(self):
        with patch.object(Path, 'iterdir', return_value=iter(Path(str(i)) for i in range(1025))):
            with self.assertRaises(h.Refused):
                self.bound.record_names(self.registry)

    def test_process_symlink_alias_is_unknown_not_closed(self):
        alternate = self.home / 'alias'
        alternate.symlink_to(self.root, target_is_directory=True)
        with patch.object(h.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'12\n')), patch.object(h, 'mac_arguments', return_value=['Claude', '--user-data-dir=' + str(alternate)]):
            with self.assertRaises(h.Refused):
                h.local_closed('mac', 'gmail', self.root)

    def test_windows_reparse_directory_is_refused(self):
        info = SimpleNamespace(st_mode=0o040700, st_file_attributes=0x400, st_dev=1, st_ino=1)
        with patch.object(Path, 'lstat', return_value=info):
            with self.assertRaises(h.Refused):
                h.directory_identity(self.root)

    def snapshot(self, hex32, age_seconds, extra=None, manifest=True):
        folder = self.root / ('.history-index-snapshot-' + hex32)
        folder.mkdir()
        if manifest:
            (folder / 'snapshot-manifest.json').write_text('[]')
        (folder / 'protected-0.bin').write_bytes(b'fixture')
        if extra:
            (folder / extra).write_text('foreign')
        stamp = 1_700_000_000 - age_seconds
        os.utime(folder, (stamp, stamp))
        return folder.name

    def test_snapshot_cleanup_keeps_newest_three_and_deletes_older(self):
        names = [self.snapshot('%032x' % i, age_seconds=(5 - i) * 100) for i in range(5)]
        result = self.bound.snapshot_cleanup()
        self.assertEqual(result['kept'], names[2:][::-1])
        self.assertEqual(sorted(result['deleted']), sorted(names[:2]))
        self.assertEqual(result['skipped'], [])
        for name in names[2:]:
            self.assertTrue((self.root / name).is_dir())
        for name in names[:2]:
            self.assertFalse((self.root / name).exists())

    def test_snapshot_cleanup_skips_anything_not_exactly_aac_created(self):
        foreign_dir = self.root / '.history-index-snapshot-notes'
        foreign_dir.mkdir()
        (foreign_dir / 'snapshot-manifest.json').write_text('[]')
        no_manifest = self.snapshot('a' * 32, age_seconds=10, manifest=False)
        with_extra = self.snapshot('b' * 32, age_seconds=20, extra='notes.txt')
        link = self.root / ('.history-index-snapshot-' + 'c' * 32)
        link.symlink_to(self.root / no_manifest, target_is_directory=True)
        result = self.bound.snapshot_cleanup()
        self.assertEqual(result['deleted'], [])
        self.assertEqual(sorted(result['skipped']), sorted([no_manifest, with_extra, link.name]))
        self.assertTrue(foreign_dir.is_dir())
        self.assertTrue((self.root / no_manifest).is_dir())
        self.assertTrue((self.root / with_extra).is_dir())
        self.assertTrue(link.is_symlink())

    def test_snapshot_cleanup_with_three_or_fewer_deletes_nothing(self):
        names = [self.snapshot('%032x' % i, age_seconds=(2 - i) * 100) for i in range(2)]
        result = self.bound.snapshot_cleanup()
        self.assertEqual(sorted(result['kept']), sorted(names))
        self.assertEqual(result['deleted'], [])
        self.assertEqual(result['skipped'], [])

if __name__ == '__main__':
    unittest.main()
