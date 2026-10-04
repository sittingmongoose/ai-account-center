"""Offline adoption boundaries using disposable homes and recorded service calls.

The completed original fixture remains unchanged. Its disposable setup is reused
by composition so this suite does not rerun or inherit its eighteen test methods.
No live installation, credentials, providers, processes or sockets are used.
"""
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import stat
import unittest
from unittest.mock import patch

import production_adoption_test as fixture_source


adoption = fixture_source.adoption
ORIGINAL_FILE_LIMIT = 256 * 1024
CHECKPOINT_LIMIT = 4 * 1024 * 1024


class ProductionAdoptionBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixture_source.ProductionAdoptionTests(
            methodName='test_success_puts_path_last_preserves_native_options_and_never_claims_capability'
        )
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def update_original(self, path, raw):
        f = self.fixture
        original = f.originals[path]
        path.write_bytes(raw)
        path.chmod(original['mode'])
        os.utime(path, ns=(original['mtimeNs'], original['mtimeNs']))
        original['raw'] = raw

    def partial_journal(self, phase, *, changed_shells=0, original_command=False):
        f = self.fixture
        snapshots = [adoption.snapshot(f.home / name) for name in ('.bashrc', '.profile')]
        snapshots.extend([adoption.snapshot(f.settings), adoption.snapshot(f.unit, missing=True)])
        changes = []
        for original in snapshots[:changed_shells]:
            raw = adoption.shell_path_proposal(
                base64.b64decode(original['rawBase64']), f.bundle.parent.parent / 'bin'
            )
            installed = adoption.publish(original, raw)
            changes.append({'before': original, 'installed': installed})
        receipt = {
            'schemaVersion': 1,
            'phase': phase,
            'installation': f.installation,
            'snapshots': snapshots,
            'changes': changes,
            'serviceStarted': False,
            'nativeCapability': False,
        }
        if original_command:
            adoption.write_exclusive(
                f.original_file, (json.dumps({'command': f.original_command}) + '\n').encode()
            )
            receipt['originalCommand'] = adoption.snapshot(f.original_file)
        adoption.write_exclusive(f.checkpoint, (json.dumps(receipt) + '\n').encode())
        return receipt

    def assert_partial_recovered(self):
        f = self.fixture
        adoption.rollback(f.home, runner=f.runner)
        f.assert_originals_restored()
        f.assert_rollback_material_removed()
        self.assertEqual(f.calls, [['systemctl', '--user', 'daemon-reload']])
        f.loader.assert_not_called()
        f.release.assert_not_called()
        f.readiness.assert_not_called()

    def test_near_limit_shells_create_large_valid_journal_and_restore_exactly(self):
        f = self.fixture
        for name in ('.bashrc', '.profile'):
            original = b'#' + b'x' * (ORIGINAL_FILE_LIMIT - 2) + b'\n'
            self.update_original(f.home / name, original)
        receipt = f.adopt()
        checkpoint_size = f.checkpoint.stat().st_size
        self.assertGreater(checkpoint_size, ORIGINAL_FILE_LIMIT)
        self.assertLess(checkpoint_size, CHECKPOINT_LIMIT)
        self.assertEqual(receipt['phase'], 'adopted')
        self.assertEqual(len(receipt['changes']), 4)
        for name in ('.bashrc', '.profile'):
            path = f.home / name
            raw = path.read_bytes()
            self.assertGreater(len(raw), ORIGINAL_FILE_LIMIT)
            self.assertTrue(raw.startswith(f.originals[path]['raw']))
            self.assertTrue(raw.endswith(b'# AI Account Center Antigravity PATH end\n'))
            self.assertEqual(raw.count(b'# AI Account Center Antigravity PATH begin'), 1)
            self.assertEqual(len(f.originals[path]['raw']), ORIGINAL_FILE_LIMIT)
        self.assertEqual(json.loads(f.checkpoint.read_bytes())['phase'], 'adopted')
        f.calls.clear()
        adoption.rollback(f.home, runner=f.runner)
        f.assert_originals_restored()
        f.assert_rollback_material_removed()
        self.assertEqual(f.calls, [
            ['systemctl', '--user', 'disable', '--now', adoption.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
        ])

    def test_original_shell_above_input_bound_is_refused_before_any_publication(self):
        f = self.fixture
        self.update_original(f.home / '.bashrc', b'#' + b'x' * ORIGINAL_FILE_LIMIT)
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-file-unsafe$'):
            f.adopt()
        f.assert_originals_restored()
        f.assert_rollback_material_removed()
        self.assertEqual(f.calls, [])
        f.readiness.assert_not_called()

    def test_empty_preparing_journal_recovers_without_disabling_service(self):
        self.partial_journal('preparing')
        self.assert_partial_recovered()

    def test_partial_preparing_journal_recovers_published_shell_and_original_command(self):
        self.partial_journal('preparing', changed_shells=1, original_command=True)
        self.assert_partial_recovered()

    def test_partial_recovery_required_journal_recovers_both_owned_shells(self):
        self.partial_journal('recovery-required', changed_shells=2, original_command=True)
        self.assert_partial_recovered()

    def test_partial_journal_refuses_foreign_untouched_snapshot_before_any_service_call(self):
        f = self.fixture
        self.partial_journal('recovery-required', changed_shells=1, original_command=True)
        checkpoint = f.checkpoint.read_bytes()
        command = f.original_file.read_bytes()
        installed_bashrc = (f.home / '.bashrc').read_bytes()
        foreign = b'# fixture foreign edit to untouched profile\n'
        f.replace_with_foreign(f.home / '.profile', foreign)
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-restore-conflict$'):
            adoption.rollback(f.home, runner=f.runner)
        self.assertEqual(f.calls, [])
        self.assertEqual((f.home / '.profile').read_bytes(), foreign)
        self.assertEqual((f.home / '.bashrc').read_bytes(), installed_bashrc)
        self.assertEqual(f.checkpoint.read_bytes(), checkpoint)
        self.assertEqual(f.original_file.read_bytes(), command)

    def test_restore_applies_mode_and_times_to_owned_temp_fd_before_publication(self):
        f = self.fixture
        f.adopt()
        events = []
        actual_fchmod = adoption.os.fchmod
        actual_utime = adoption.os.utime
        actual_replace = adoption.os.replace
        def fchmod(descriptor, mode):
            self.assertIs(type(descriptor), int)
            events.append(('mode', os.fstat(descriptor).st_ino, mode))
            return actual_fchmod(descriptor, mode)
        def utime(descriptor, **options):
            self.assertIs(type(descriptor), int)
            events.append(('times', os.fstat(descriptor).st_ino, options['ns']))
            return actual_utime(descriptor, **options)
        def replace(source, destination):
            self.assertIn(Path(destination), f.originals)
            events.append(('publish', Path(source).stat().st_ino, Path(destination)))
            return actual_replace(source, destination)
        with patch.object(adoption.os, 'fchmod', side_effect=fchmod), \
                patch.object(adoption.os, 'utime', side_effect=utime), \
                patch.object(adoption.os, 'replace', side_effect=replace), \
                patch.object(adoption.os, 'chmod', side_effect=AssertionError('named leaf metadata write')):
            adoption.rollback(f.home, runner=f.runner)
        publications = [event for event in events if event[0] == 'publish']
        self.assertEqual(len(publications), 3)
        for publication in publications:
            _, inode, target = publication
            preceding = events[:events.index(publication)]
            original = f.originals[target]
            self.assertIn(('mode', inode, original['mode']), preceding)
            self.assertIn(('times', inode, (original['mtimeNs'], original['mtimeNs'])), preceding)
        f.assert_originals_restored()
        f.assert_rollback_material_removed()

    def test_foreign_leaf_replaced_during_fd_metadata_restore_keeps_bytes_mode_times_and_controls(self):
        f = self.fixture
        f.adopt()
        target = f.home / '.profile'
        foreign = f.state / 'fixture-metadata-foreign-leaf'
        foreign_raw = b'# fixture metadata replacement leaf\n'
        foreign_mode = 0o400
        foreign_stamp = 1_680_000_000_111_222_333
        foreign.write_bytes(foreign_raw)
        foreign.chmod(foreign_mode)
        os.utime(foreign, ns=(foreign_stamp, foreign_stamp))
        checkpoint = f.checkpoint.read_bytes()
        command = f.original_file.read_bytes()
        actual_utime = adoption.os.utime
        actual_replace = adoption.os.replace
        replaced = False
        def utime(descriptor, **options):
            nonlocal replaced
            self.assertIs(type(descriptor), int)
            if options['ns'][1] == f.originals[target]['mtimeNs'] and not replaced:
                replaced = True
                actual_replace(foreign, target)
            return actual_utime(descriptor, **options)
        with patch.object(adoption.os, 'utime', side_effect=utime):
            with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-write-conflict$'):
                adoption.rollback(f.home, runner=f.runner)
        self.assertTrue(replaced)
        observed = target.stat()
        self.assertEqual(target.read_bytes(), foreign_raw)
        self.assertEqual(stat.S_IMODE(observed.st_mode), foreign_mode)
        self.assertEqual(observed.st_mtime_ns, foreign_stamp)
        self.assertEqual(f.checkpoint.read_bytes(), checkpoint)
        self.assertEqual(f.original_file.read_bytes(), command)


if __name__ == '__main__':
    unittest.main(verbosity=2)
