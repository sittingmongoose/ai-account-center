"""Disposable, offline adoption transaction fixtures; never invoke real services.

Every home, settings document, installation identity and readiness result below
is invented. Public proposal helpers are copied into the disposable bundle.
No executable, credential store, provider, socket or systemctl is contacted.
"""
from __future__ import annotations

import base64
from contextlib import ExitStack
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch


sys.dont_write_bytecode = True
SOURCE_DIRECTORY = Path(__file__).resolve().parents[3] / 'scripts/antigravity'
_import_path = list(sys.path)
_spec = importlib.util.spec_from_file_location(
    'ai_account_center_offline_adoption_fixture', SOURCE_DIRECTORY / 'adopt_runtime.py'
)
adoption = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = adoption
with patch('subprocess.run', side_effect=AssertionError('unexpected import subprocess')):
    _spec.loader.exec_module(adoption)
sys.path[:] = _import_path


class ProductionAdoptionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='aic-offline-adoption-')
        self.addCleanup(self.temporary.cleanup)
        self.home = (Path(self.temporary.name) / 'fixture-home').resolve()
        self.home.mkdir(mode=0o700)
        self.state = self.home / '.ccs/antigravity-switching'
        self.bundle = self.home / '.local/share/ai-account-center/antigravity-runtime/bundles' / (
            'a' * 64
        )
        self.library = self.bundle / 'lib'
        for directory in (
            self.state,
            self.library,
            self.home / '.gemini/antigravity-cli',
            self.home / '.config/systemd/user',
        ):
            directory.mkdir(parents=True, mode=0o700, exist_ok=True)
            current = directory
            while current != self.home:
                current.chmod(0o700)
                current = current.parent
        for name in ('native_status_config.py', 'native_status_attestor.py'):
            shutil.copyfile(SOURCE_DIRECTORY / 'runtime' / name, self.library / name)
        self.original_command = "printf 'fixture prior status'; true"
        status = {
            'type': 'command',
            'command': self.original_command,
            'enabled': True,
            'stack_with_default': False,
            'refreshIntervalMs': 1750,
            'fixtureOption': {'preserve': ['one', 'two']},
        }
        settings = (
            b'{\r\n  "theme": {"name":"fixture", "enabled":true},\r\n  "statusLine": '
            + json.dumps(status).encode()
            + b',\r\n  "tail": [3,2,1]\r\n}\n'
        )
        self.settings = self.home / '.gemini/antigravity-cli/settings.json'
        self.unit = self.home / '.config/systemd/user' / adoption.UNIT
        self.checkpoint = self.state / 'runtime-adoption.json'
        self.original_file = self.state / 'original-status-command.json'
        self.originals = {}
        for index, (path, raw, mode) in enumerate((
            (self.home / '.bashrc', b'# fixture bashrc\r\ncase $- in *i*) ;; *) return;; esac\n', 0o640),
            (self.home / '.profile', b'# fixture profile\nexport FIXTURE_ONLY=one\n', 0o600),
            (self.settings, settings, 0o600),
        )):
            path.write_bytes(raw)
            path.chmod(mode)
            stamp = 1_660_000_000_123_456_789 + index
            os.utime(path, ns=(stamp, stamp))
            self.originals[path] = {'raw': raw, 'mode': mode, 'mtimeNs': stamp}
        self.installation = {
            'schemaVersion': 1,
            'bundleDirectory': str(self.bundle),
            'nativeBinary': str(self.home / '.local/bin/agy'),
            'nativeSha256': 'b' * 64,
            'socketPath': str(self.home / '.ccs/antigravity-runtime/control.sock'),
        }
        self.calls = []
        self.runner_failure = None
        self.readiness = Mock(return_value={'serviceReady': True, 'nativeCapability': False})
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.loader = self.stack.enter_context(
            patch.object(adoption, 'load_installation', return_value=self.installation)
        )
        self.release = self.stack.enter_context(
            patch.object(adoption, 'runtime_release', return_value=True)
        )
        self.saved_import_path = list(sys.path)
        names = ('native_status_config', 'native_status_attestor')
        self.saved_modules = {name: sys.modules.pop(name, None) for name in names}
        self.addCleanup(self.restore_imports)

    def restore_imports(self):
        sys.path[:] = self.saved_import_path
        for name, previous in self.saved_modules.items():
            sys.modules.pop(name, None)
            if previous is not None:
                sys.modules[name] = previous

    def runner(self, argv, **options):
        self.assertEqual(argv[:2], ['systemctl', '--user'])
        self.assertEqual(options, {
            'check': True,
            'stdin': subprocess.DEVNULL,
            'stdout': subprocess.DEVNULL,
            'stderr': subprocess.DEVNULL,
        })
        self.calls.append(list(argv))
        if self.runner_failure is not None:
            self.runner_failure(argv)

    def adopt(self):
        return adoption.adopt(self.home, runner=self.runner, readiness=self.readiness)

    def assert_originals_restored(self):
        for path, expected in self.originals.items():
            with self.subTest(path=path.name):
                observed = path.stat()
                self.assertEqual(path.read_bytes(), expected['raw'])
                self.assertEqual(stat.S_IMODE(observed.st_mode), expected['mode'])
                self.assertEqual(observed.st_mtime_ns, expected['mtimeNs'])

    def assert_rollback_material_removed(self):
        self.assertFalse(self.unit.exists())
        self.assertFalse(self.checkpoint.exists())
        self.assertFalse(self.original_file.exists())

    def replace_with_foreign(self, target, raw):
        temporary = self.state / 'fixture-concurrent-leaf'
        temporary.write_bytes(raw)
        temporary.chmod(0o600)
        os.replace(temporary, target)

    def test_success_puts_path_first_preserves_native_options_and_never_claims_capability(self):
        receipt = self.adopt()
        self.assertEqual(receipt['phase'], 'adopted')
        self.assertTrue(receipt['serviceStarted'])
        self.assertFalse(receipt['nativeCapability'])
        self.assertEqual(len(receipt['changes']), 4)
        self.loader.assert_called_once_with(self.home)
        self.release.assert_called_once_with(self.bundle)
        self.readiness.assert_called_once_with(self.installation)
        for name in ('.bashrc', '.profile'):
            path = self.home / name
            raw = path.read_bytes()
            original = self.originals[path]['raw']
            self.assertEqual(raw, adoption.shell_path_proposal(original, self.bundle.parent.parent / 'bin'))
            self.assertTrue(raw.startswith(b'# AI Account Center Antigravity PATH begin\nexport PATH='))
            self.assertTrue(raw.endswith(original))
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), self.originals[path]['mode'])
        original = json.loads(self.originals[self.settings]['raw'])
        patched_bytes = self.settings.read_bytes()
        patched = json.loads(patched_bytes)
        self.assertEqual(patched['theme'], original['theme'])
        self.assertEqual(patched['tail'], original['tail'])
        for key, value in original['statusLine'].items():
            if key != 'command':
                self.assertEqual(patched['statusLine'][key], value)
        self.assertIn('native_status_hook.py', patched['statusLine']['command'])
        self.assertIn('--original-command-file', patched['statusLine']['command'])
        self.assertTrue(patched_bytes.startswith(
            self.originals[self.settings]['raw'].split(b'"statusLine": ', 1)[0]
        ))
        self.assertTrue(patched_bytes.endswith(b',\r\n  "tail": [3,2,1]\r\n}\n'))
        self.assertEqual(json.loads(self.original_file.read_bytes()), {'command': self.original_command})
        self.assertEqual(self.calls, [
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'enable', '--now', adoption.UNIT],
        ])
        self.assertEqual(self.unit.stat().st_mode & 0o777, 0o600)
        self.assertIn(b'Restart=no\n', self.unit.read_bytes())
        self.assertEqual(json.loads(self.checkpoint.read_bytes())['phase'], 'adopted')

    def test_rollback_restores_exact_bytes_modes_mtimes_and_removes_owned_material(self):
        self.adopt()
        self.calls.clear()
        adoption.rollback(self.home, runner=self.runner)
        self.assert_originals_restored()
        self.assert_rollback_material_removed()
        self.assertEqual(self.calls, [
            ['systemctl', '--user', 'disable', '--now', adoption.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
        ])

    def test_cli_rollback_dispatches_to_fixture_home_and_restores_it(self):
        self.adopt()
        real_rollback = adoption.rollback
        with patch.object(adoption, 'rollback', side_effect=lambda home: real_rollback(home, runner=self.runner)) as dispatch:
            with patch.object(adoption.Path, 'home', return_value=self.home), patch.object(sys, 'argv', ['adopt_runtime.py', '--rollback']):
                self.assertEqual(adoption.main(), 0)
        dispatch.assert_called_once_with(self.home)
        self.assert_originals_restored()
        self.assert_rollback_material_removed()

    def test_closed_native_gate_makes_no_files_service_or_readiness_changes(self):
        self.release.return_value = False
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-native-gate-pending$'):
            self.adopt()
        self.assertEqual(self.calls, [])
        self.readiness.assert_not_called()
        self.assert_originals_restored()
        self.assert_rollback_material_removed()

    def test_rollback_refuses_foreign_profile_before_disabling_any_service(self):
        self.adopt()
        installed = {path: path.read_bytes() for path in self.originals}
        journal_before = self.checkpoint.read_bytes()
        original_command_before = self.original_file.read_bytes()
        target = self.home / '.profile'
        foreign = b'# fixture concurrent foreign profile\n'
        target.write_bytes(foreign)
        self.calls.clear()
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-restore-conflict$'):
            adoption.rollback(self.home, runner=self.runner)
        self.assertEqual(self.calls, [])
        self.assertEqual(target.read_bytes(), foreign)
        for path, raw in installed.items():
            if path != target:
                self.assertEqual(path.read_bytes(), raw)
        self.assertEqual(self.checkpoint.read_bytes(), journal_before)
        self.assertEqual(self.original_file.read_bytes(), original_command_before)
        self.assertTrue(self.unit.exists())

    def test_rollback_refuses_foreign_original_command_before_disabling(self):
        self.adopt()
        self.original_file.write_bytes(b'{"command":"fixture foreign replacement"}\n')
        self.calls.clear()
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-original-command-changed$'):
            adoption.rollback(self.home, runner=self.runner)
        self.assertEqual(self.calls, [])
        self.assertTrue(self.checkpoint.exists())
        self.assertTrue(self.unit.exists())

    def test_checkpoint_replaced_during_disable_is_retained_with_remaining_recovery_material(self):
        self.adopt()
        foreign = b'{"fixture":"concurrent foreign checkpoint"}\n'
        def replace_during_disable(argv):
            if argv[2:4] == ['disable', '--now']:
                self.replace_with_foreign(self.checkpoint, foreign)
        self.runner_failure = replace_during_disable
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-checkpoint-changed$'):
            adoption.rollback(self.home, runner=self.runner)
        self.assertEqual(self.checkpoint.read_bytes(), foreign)
        self.assertTrue(self.original_file.exists())
        self.assertEqual(json.loads(self.original_file.read_bytes()), {'command': self.original_command})

    def test_original_command_replaced_during_restore_is_retained_with_journal(self):
        self.adopt()
        foreign = b'{"command":"fixture concurrent foreign command"}\n'
        actual_restore = adoption.restore
        replaced = False
        def replace_during_restore(change):
            nonlocal replaced
            result = actual_restore(change)
            if not replaced:
                replaced = True
                self.replace_with_foreign(self.original_file, foreign)
            return result
        with patch.object(adoption, 'restore', side_effect=replace_during_restore):
            with self.assertRaisesRegex(adoption.InstallationError, '^runtime-original-command-changed$'):
                adoption.rollback(self.home, runner=self.runner)
        self.assertEqual(self.original_file.read_bytes(), foreign)
        self.assertTrue(self.checkpoint.exists())
        journal = json.loads(self.checkpoint.read_bytes())
        self.assertEqual(len(journal['changes']), 4)
        for change in journal['changes']:
            path = Path(change['before']['path'])
            if path in self.originals:
                self.assertEqual(base64.b64decode(change['before']['rawBase64']), self.originals[path]['raw'])

    def test_failed_enable_cleans_up_possibly_started_unit_and_restores_originals(self):
        def fail_enable(argv):
            if argv[2:4] == ['enable', '--now']:
                raise subprocess.CalledProcessError(1, 'fixture-service-enable')
        self.runner_failure = fail_enable
        with self.assertRaises(subprocess.CalledProcessError):
            self.adopt()
        self.assertEqual(self.calls, [
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'enable', '--now', adoption.UNIT],
            ['systemctl', '--user', 'disable', '--now', adoption.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
        ])
        self.readiness.assert_not_called()
        self.assert_originals_restored()
        self.assert_rollback_material_removed()

    def test_failed_enable_with_foreign_unit_never_disables_it_and_retains_recovery(self):
        foreign = b'[Unit]\nDescription=fixture foreign replacement unit\n'
        def replace_and_fail_enable(argv):
            if argv[2:4] == ['enable', '--now']:
                self.replace_with_foreign(self.unit, foreign)
                raise subprocess.CalledProcessError(1, 'fixture-service-enable')
        self.runner_failure = replace_and_fail_enable
        with self.assertRaises(subprocess.CalledProcessError):
            self.adopt()
        self.assertEqual(self.calls, [
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'enable', '--now', adoption.UNIT],
        ])
        self.assertEqual(self.unit.read_bytes(), foreign)
        journal = json.loads(self.checkpoint.read_bytes())
        self.assertEqual(journal['phase'], 'recovery-required')
        self.assertTrue(journal['serviceStarted'])
        self.assertEqual(len(journal['changes']), 4)
        self.assertTrue(self.original_file.exists())
        self.readiness.assert_not_called()

    def test_failed_readiness_restores_files_and_disables_owned_service(self):
        self.readiness.return_value = {'serviceReady': False, 'nativeCapability': False}
        with self.assertRaisesRegex(adoption.InstallationError, '^runtime-service-readiness-failed$'):
            self.adopt()
        self.assertEqual(self.calls[-2:], [
            ['systemctl', '--user', 'disable', '--now', adoption.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
        ])
        self.assert_originals_restored()
        self.assert_rollback_material_removed()

    def test_publish_records_own_committed_inode_before_post_replace_snapshot_failure(self):
        target = self.home / '.bashrc'
        before = adoption.snapshot(target)
        actual_snapshot = adoption.snapshot
        committed = []
        target_reads = 0
        def fail_post_commit(path, **options):
            nonlocal target_reads
            if Path(path) == target:
                target_reads += 1
                if target_reads == 3:
                    raise OSError('fixture post-replace observation failure')
            return actual_snapshot(path, **options)
        replacement = b'# fixture committed publication\n'
        with patch.object(adoption, 'snapshot', side_effect=fail_post_commit):
            with self.assertRaisesRegex(OSError, '^fixture post-replace observation failure$'):
                adoption.publish(before, replacement, on_committed=committed.append)
        self.assertEqual(len(committed), 1)
        known = committed[0]
        self.assertEqual(base64.b64decode(known['rawBase64']), replacement)
        self.assertNotEqual(known['inode'], before['inode'])
        self.assertEqual(known['inode'], str(target.stat().st_ino))
        self.assertTrue(adoption.same(actual_snapshot(target), known))

    def test_transaction_post_commit_observation_failure_can_restore_owned_publication(self):
        target = self.home / '.profile'
        actual_snapshot = adoption.snapshot
        target_reads = 0
        def fail_once(path, **options):
            nonlocal target_reads
            if Path(path) == target:
                target_reads += 1
                if target_reads == 4:
                    raise OSError('fixture transaction post-commit observation failure')
            return actual_snapshot(path, **options)
        with patch.object(adoption, 'snapshot', side_effect=fail_once):
            with self.assertRaisesRegex(OSError, '^fixture transaction post-commit observation failure$'):
                self.adopt()
        self.assert_originals_restored()
        self.assert_rollback_material_removed()
        self.assertEqual(self.calls, [['systemctl', '--user', 'daemon-reload']])
        self.readiness.assert_not_called()

    def test_foreign_post_write_leaf_is_never_adopted_or_overwritten_and_journal_retains_sources(self):
        target = self.home / '.bashrc'
        foreign_raw = b'# fixture foreign leaf published during observation\n'
        actual_replace = adoption.os.replace
        foreign_inode = None
        replaced = False
        def replace_with_foreign(source, destination):
            nonlocal foreign_inode, replaced
            result = actual_replace(source, destination)
            if Path(destination) == target and not replaced:
                replaced = True
                foreign = self.home / 'fixture-foreign-leaf'
                foreign.write_bytes(foreign_raw)
                foreign.chmod(0o600)
                foreign_inode = str(foreign.stat().st_ino)
                actual_replace(foreign, target)
            return result
        with patch.object(adoption.os, 'replace', side_effect=replace_with_foreign):
            with self.assertRaisesRegex(adoption.InstallationError, '^runtime-adoption-post-write-conflict$'):
                self.adopt()
        self.assertEqual(target.read_bytes(), foreign_raw)
        self.assertEqual(str(target.stat().st_ino), foreign_inode)
        journal = json.loads(self.checkpoint.read_bytes())
        self.assertEqual(journal['phase'], 'recovery-required')
        self.assertEqual(len(journal['changes']), 1)
        change = journal['changes'][0]
        self.assertEqual(base64.b64decode(change['before']['rawBase64']), self.originals[target]['raw'])
        self.assertEqual(base64.b64decode(change['installed']['rawBase64']), adoption.shell_path_proposal(
            self.originals[target]['raw'], self.bundle.parent.parent / 'bin'
        ))
        self.assertNotEqual(change['installed']['inode'], foreign_inode)
        self.assertEqual(json.loads(self.original_file.read_bytes()), {'command': self.original_command})
        self.assertEqual(self.calls, [])
        self.readiness.assert_not_called()
        for path, expected in self.originals.items():
            if path != target:
                self.assertEqual(path.read_bytes(), expected['raw'])
                self.assertEqual(path.stat().st_mtime_ns, expected['mtimeNs'])
        self.assertFalse(self.unit.exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
