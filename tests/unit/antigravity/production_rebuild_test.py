"""Disposable bundle-rebuild fixtures; never touch live services or the real home.

Every installation, bundle, checkpoint and readiness result below is invented.
The runner only records argv; no systemctl, venv, pip or socket is contacted.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import sys
import tempfile
import unittest
from unittest.mock import Mock

SOURCE = Path(__file__).resolve().parents[3] / 'scripts/antigravity'
sys.path.insert(0, str(SOURCE))
import rebuild_bundle as rebuild
import runtime_installation as layout
from install_runtime import setup_plan as install_setup_plan

OLD_DIGEST = 'c' * 64
OLD_PIN = 'd' * 64


class RuntimeRebuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='aic-rebuild-')
        self.addCleanup(self.temp.cleanup)
        self.home = (Path(self.temp.name) / 'home').resolve()
        self.state = self.home / '.ccs/antigravity-switching'
        self.old_bundle = (self.home / '.local/share/ai-account-center/antigravity-runtime/bundles' / OLD_DIGEST)
        self.unit = self.home / '.config/systemd/user' / rebuild.UNIT
        self.checkpoint = self.state / 'runtime-adoption.json'
        self.descriptor = self.state / 'runtime-installation.json'
        self.profiles = self.home / '.ccs/antigravity-profiles'
        native = self.home / '.local/bin/agy'
        native.parent.mkdir(parents=True)
        native.write_bytes(b'synthetic new native executable, never launched')
        native.chmod(0o700)
        self.native_fp = hashlib.sha256(native.read_bytes()).hexdigest()
        self.source = Path(self.temp.name) / 'runtime'
        shutil.copytree(SOURCE / 'runtime', self.source, ignore=shutil.ignore_patterns('__pycache__'))
        release = json.loads((self.source / 'release.json').read_text())
        release.update(nativeSha256=self.native_fp)
        (self.source / 'release.json').write_text(json.dumps(release))
        self.version = release['nativeVersion']
        self.manifest()
        (self.old_bundle / 'lib').mkdir(parents=True, mode=0o700)
        (self.old_bundle / 'lib/release.json').write_text(json.dumps(
            {'nativeSha256': OLD_PIN, 'nativeVersion': '1.2.14'}))
        self.state.mkdir(parents=True, mode=0o700)
        self.shim = self.old_bundle.parent.parent / 'bin/agy'
        self.shim.parent.mkdir(parents=True, mode=0o700)
        self.shim.write_bytes(rebuild.expected_shim(self.old_bundle))
        self.shim.chmod(0o700)
        installation = {'schemaVersion': 1, 'bundleDirectory': str(self.old_bundle),
                        'nativeBinary': str(native), 'nativeSha256': self.native_fp,
                        'socketPath': str(self.home / '.ccs/antigravity-runtime/control.sock')}
        self.descriptor.write_text(json.dumps(installation) + '\n')
        self.descriptor.chmod(0o600)
        self.unit.parent.mkdir(parents=True, mode=0o700)
        self.unit.write_bytes(rebuild.unit_bytes(installation))
        self.unit.chmod(0o600)
        receipt = {'schemaVersion': 1, 'phase': 'adopted',
                   'changes': [{'before': {'path': str(self.unit)}, 'installed': rebuild.snapshot(self.unit)}]}
        self.checkpoint.write_text(json.dumps(receipt) + '\n')
        self.checkpoint.chmod(0o600)
        self.profiles.mkdir(parents=True, mode=0o700)
        (self.profiles / 'registry-000000000001.json').write_text(
            json.dumps({'transaction': None, 'profiles': []}))
        for root, dirs, _ in os.walk(self.home):
            os.chmod(root, 0o700)
            for name in dirs:
                os.chmod(os.path.join(root, name), 0o700)
        self.calls = []
        self.readiness = Mock(return_value={'serviceReady': True, 'nativeCapability': False})
        self.originals = {path: path.read_bytes() for path in
                          (self.shim, self.unit, self.descriptor, self.checkpoint)}

    def manifest(self):
        value = {'schemaVersion': 1, 'files': [
            {'path': item.name, 'sha256': hashlib.sha256(item.read_bytes()).hexdigest()}
            for item in sorted(self.source.iterdir()) if item.is_file() and item.name != 'runtime-manifest.json']}
        (self.source / 'runtime-manifest.json').write_text(json.dumps(value))

    def runner(self, argv, **kwargs):
        self.calls.append((list(argv), dict(kwargs)))

    def service_calls(self):
        return [argv for argv, _ in self.calls if argv[:2] == ['systemctl', '--user']]

    def assert_originals_intact(self):
        for path, raw in self.originals.items():
            with self.subTest(path=path.name):
                self.assertEqual(path.read_bytes(), raw)

    def test_plan_reports_stale_with_versions(self):
        info = rebuild.plan(self.home, self.source)
        self.assertTrue(info['stale'])
        self.assertEqual(info['version'], self.version)
        self.assertEqual(info['oldVersion'], '1.2.14')
        self.assertEqual(info['newPin'], self.native_fp)

    def test_apply_rebuilds_bundle_and_repoints_owned_files(self):
        result = rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assertEqual(result, {'status': 'rebuilt', 'version': self.version})
        new_bundle = next(item for item in (self.old_bundle.parent).iterdir() if item.name != OLD_DIGEST)
        self.assertTrue((new_bundle / 'lib/release.json').is_file())
        self.assertEqual(json.loads((new_bundle / 'lib/release.json').read_text())['nativeSha256'],
                         self.native_fp)
        self.assertEqual(self.shim.read_bytes(), rebuild.expected_shim(new_bundle))
        renewed = json.loads(self.descriptor.read_text())
        self.assertEqual(renewed['bundleDirectory'], str(new_bundle))
        self.assertEqual(renewed['nativeSha256'], self.native_fp)
        self.assertEqual(self.unit.read_bytes(), rebuild.unit_bytes(renewed))
        backup = self.state / 'descriptor-backups' / (OLD_PIN + '.json')
        self.assertEqual(backup.read_bytes(), self.originals[self.descriptor])
        self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o600)
        receipt = json.loads(self.checkpoint.read_text())
        installed = receipt['changes'][0]['installed']
        self.assertTrue(rebuild.same(rebuild.snapshot(self.unit), installed))
        self.assertEqual(json.loads((self.state / 'runtime-rebuild.json').read_text())['phase'], 'rebuilt')
        self.assertEqual(self.calls[0][0][:4], ['/usr/bin/python3', '-I', '-m', 'venv'])
        self.assertIn('pip', self.calls[1][0])
        self.assertEqual(len(self.calls), 6)
        self.assertEqual(self.service_calls(), [
            ['systemctl', '--user', 'stop', rebuild.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'start', rebuild.UNIT],
        ])
        self.readiness.assert_called_once()
        installed_arg = self.readiness.call_args[0][0]
        self.assertEqual(installed_arg['bundleDirectory'], str(new_bundle))
        self.assertEqual(installed_arg['nativeSha256'], self.native_fp)
        self.assertFalse(rebuild.plan(self.home, self.source)['stale'])
        reread = rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assertEqual(reread['status'], 'current')

    def test_apply_is_current_when_bundle_matches(self):
        (self.old_bundle / 'lib/release.json').write_text(json.dumps(
            {'nativeSha256': self.native_fp, 'nativeVersion': self.version}))
        self.assertFalse(rebuild.plan(self.home, self.source)['stale'])
        result = rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assertEqual(result['status'], 'current')
        self.assertEqual(self.calls, [])
        self.readiness.assert_not_called()
        self.assertFalse((self.state / 'runtime-rebuild.json').exists())
        self.assert_originals_intact()

    def test_refusals_write_nothing(self):
        def stale_descriptor():
            value = json.loads(self.descriptor.read_text())
            value['nativeSha256'] = OLD_PIN
            self.descriptor.write_text(json.dumps(value) + '\n')

        def unreviewed_binary():
            other = self.home / '.local/bin/agy'
            other.write_bytes(b'synthetic unreviewed native')

        def active_transaction():
            (self.profiles / 'registry-000000000001.json').write_text(json.dumps(
                {'transaction': {'id': 'f' * 32}, 'profiles': []}))

        def foreign_shim():
            with open(self.shim, 'ab') as stream:
                stream.write(b'# foreign')

        def foreign_unit():
            with open(self.unit, 'ab') as stream:
                stream.write(b'# foreign')

        def checkpoint_mismatch():
            receipt = json.loads(self.checkpoint.read_text())
            receipt['changes'][0]['installed'] = {'existed': True, 'path': str(self.unit)}
            self.checkpoint.write_text(json.dumps(receipt) + '\n')

        def missing_checkpoint():
            self.checkpoint.unlink()

        def bundle_present():
            target = Path(install_setup_plan(self.home, self.source)[0]['bundleDirectory'])
            target.mkdir(parents=True)

        cases = [
            ('runtime-descriptor-stale', stale_descriptor),
            ('runtime-native-unreviewed', unreviewed_binary),
            ('runtime-transaction-active', active_transaction),
            ('runtime-installation-foreign', foreign_shim),
            ('runtime-installation-foreign', foreign_unit),
            ('runtime-installation-foreign', checkpoint_mismatch),
            ('runtime-not-adopted', missing_checkpoint),
            ('runtime-bundle-already-present', bundle_present),
        ]
        for code, mutate in cases:
            with self.subTest(code=code):
                self.setUp()
                mutate()
                with self.assertRaisesRegex(layout.InstallationError, '^%s$' % code):
                    rebuild.plan(self.home, self.source)
                with self.assertRaisesRegex(layout.InstallationError, '^%s$' % code):
                    rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
                self.assertEqual(self.calls, [])
                self.assertFalse((self.state / 'runtime-rebuild.json').exists())
                self.readiness.assert_not_called()

    def test_second_generation_rebuild_replaces_finished_journal(self):
        first = rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assertEqual(first['status'], 'rebuilt')
        second_binary = b'synthetic third native executable, never launched'
        (self.home / '.local/bin/agy').write_bytes(second_binary)
        fp3 = hashlib.sha256(second_binary).hexdigest()
        release = json.loads((self.source / 'release.json').read_text())
        release.update(nativeSha256=fp3)
        (self.source / 'release.json').write_text(json.dumps(release))
        self.manifest()
        renewed = json.loads(self.descriptor.read_text())
        renewed['nativeSha256'] = fp3
        self.descriptor.write_text(json.dumps(renewed) + '\n')
        info = rebuild.plan(self.home, self.source)
        self.assertTrue(info['stale'])
        self.calls.clear()
        second = rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assertEqual(second['status'], 'rebuilt')
        journal = json.loads((self.state / 'runtime-rebuild.json').read_text())
        self.assertEqual(journal['phase'], 'rebuilt')
        self.assertEqual(journal['newPin'], fp3)
        self.assertEqual(json.loads(self.descriptor.read_text())['nativeSha256'], fp3)
        self.assertEqual(self.service_calls(), [
            ['systemctl', '--user', 'stop', rebuild.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'start', rebuild.UNIT],
        ])
        self.assertFalse(rebuild.plan(self.home, self.source)['stale'])

    def test_missing_registry_proceeds(self):
        shutil.rmtree(self.profiles)
        self.assertTrue(rebuild.plan(self.home, self.source)['stale'])

    def test_failed_readiness_restores_owned_bytes_and_restarts(self):
        self.readiness.return_value = {'serviceReady': False, 'nativeCapability': False}
        with self.assertRaisesRegex(layout.InstallationError, '^runtime-service-readiness-failed$'):
            rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assert_originals_intact()
        self.assertEqual(self.service_calls(), [
            ['systemctl', '--user', 'stop', rebuild.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'start', rebuild.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'start', rebuild.UNIT],
        ])
        self.assertEqual(json.loads((self.state / 'runtime-rebuild.json').read_text())['phase'], 'failed')
        self.assertTrue((self.state / 'descriptor-backups' / (OLD_PIN + '.json')).is_file())

    def test_build_failure_stops_nothing_and_recovers(self):
        def fail(argv, **kwargs):
            self.calls.append((list(argv), dict(kwargs)))
            raise OSError('synthetic venv failure')

        with self.assertRaises(OSError):
            rebuild.apply(self.home, self.source, runner=fail, readiness=self.readiness)
        self.assertEqual(self.service_calls(), [])
        self.assert_originals_intact()
        journal = json.loads((self.state / 'runtime-rebuild.json').read_text())
        self.assertEqual(journal['phase'], 'failed')
        self.assertTrue(Path(journal['newBundle']).is_dir())
        self.assertEqual(rebuild.recover_rebuild(self.home), {'status': 'recovered'})
        self.assertFalse(Path(journal['newBundle']).exists())
        self.assertFalse((self.state / 'runtime-rebuild.json').exists())
        self.assert_originals_intact()

    def test_recover_rebuild_refuses_diverged_old_files(self):
        def fail(argv, **kwargs):
            raise OSError('synthetic venv failure')

        with self.assertRaises(OSError):
            rebuild.apply(self.home, self.source, runner=fail, readiness=self.readiness)
        with open(self.shim, 'ab') as stream:
            stream.write(b'# foreign')
        with self.assertRaisesRegex(layout.InstallationError, '^runtime-rebuild-diverged$'):
            rebuild.recover_rebuild(self.home)
        self.assertTrue((self.state / 'runtime-rebuild.json').is_file())

    def test_recover_rebuild_with_nothing(self):
        with self.assertRaisesRegex(layout.InstallationError, '^runtime-rebuild-nothing-to-recover$'):
            rebuild.recover_rebuild(self.home)

    def test_divergent_backup_refuses_and_restores(self):
        backup = self.state / 'descriptor-backups' / (OLD_PIN + '.json')
        backup.parent.mkdir(parents=True, mode=0o700)
        backup.write_bytes(b'{"divergent": true}')
        backup.chmod(0o600)
        with self.assertRaisesRegex(layout.InstallationError, '^runtime-backup-divergent$'):
            rebuild.apply(self.home, self.source, runner=self.runner, readiness=self.readiness)
        self.assert_originals_intact()
        self.assertEqual(self.service_calls(), [
            ['systemctl', '--user', 'stop', rebuild.UNIT],
            ['systemctl', '--user', 'daemon-reload'],
            ['systemctl', '--user', 'start', rebuild.UNIT],
        ])


if __name__ == '__main__':
    unittest.main(verbosity=2)
