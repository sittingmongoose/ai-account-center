"""Disposable public runtime preparation/update-pass-through fixtures only."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[3] / 'scripts/antigravity'
sys.path.insert(0, str(SOURCE))
import install_runtime as install
import runtime_installation as layout


class RuntimePreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = Path(self.temp.name) / 'home'; self.home.mkdir()
        self.source = Path(self.temp.name) / 'runtime'
        shutil.copytree(SOURCE / 'runtime', self.source, ignore=shutil.ignore_patterns('__pycache__'))
        native = self.home / '.local/bin/agy'; native.parent.mkdir(parents=True)
        native.write_bytes(b'synthetic native executable, never launched'); native.chmod(0o700)
        self.native = native.read_bytes()
        release = json.loads((self.source / 'release.json').read_text())
        release.update(nativeSha256=hashlib.sha256(self.native).hexdigest(),
                       nativeActivationReleased=True, nativeProofReceiptSha256='a' * 64)
        (self.source / 'release.json').write_text(json.dumps(release))
        self.manifest()
        self.calls = []

    def tearDown(self): self.temp.cleanup()

    def manifest(self):
        value = {'schemaVersion': 1, 'files': [
            {'path': item.name, 'sha256': hashlib.sha256(item.read_bytes()).hexdigest()}
            for item in sorted(self.source.iterdir()) if item.is_file() and item.name != 'runtime-manifest.json']}
        (self.source / 'runtime-manifest.json').write_text(json.dumps(value))

    def runner(self, argv, **kwargs): self.calls.append((argv, kwargs))

    def test_false_release_fails_before_preparation(self):
        release = json.loads((self.source / 'release.json').read_text())
        release['nativeActivationReleased'] = False
        (self.source / 'release.json').write_text(json.dumps(release)); self.manifest()
        with self.assertRaises(layout.InstallationError): install.install(self.home, self.source, runner=self.runner)
        self.assertEqual(self.calls, [])
        self.assertFalse((self.home / '.ccs').exists())
        self.assertEqual((self.home / '.local/bin/agy').read_bytes(), self.native)

    def test_failed_venv_retains_recoverable_preparation_and_retry(self):
        def fail(*args, **kwargs): raise OSError('synthetic venv failure')
        with self.assertRaises(OSError): install.install(self.home, self.source, runner=fail)
        state = self.home / '.ccs/antigravity-switching'
        marker = state / 'runtime-preparation.json'
        self.assertTrue(marker.is_file())
        self.assertFalse((state / 'runtime-installation.json').exists())
        with self.assertRaises(layout.InstallationError): install.install(self.home, self.source, runner=self.runner)
        install.recover_preparation(self.home)
        self.assertFalse(marker.exists())
        planned = install.install(self.home, self.source, runner=self.runner)
        self.assertTrue((state / 'runtime-installation.json').is_file())
        self.assertFalse(marker.exists())
        self.assertTrue((Path(planned['bundleDirectory']) / 'lib/runtime-manifest.json').is_file())
        self.assertEqual((self.home / '.local/bin/agy').read_bytes(), self.native)

    def test_parser_goes_to_a_version_neutral_directory_checked_by_the_service_python(self):
        planned = install.install(self.home, self.source, runner=self.runner)
        bundle = Path(planned['bundleDirectory'])
        argv = [call[0] for call in self.calls]
        self.assertEqual(len(argv), 3)
        self.assertEqual(argv[0], ['/usr/bin/python3', '-I', '-m', 'venv', str(bundle / 'venv')])
        pip = argv[1]
        self.assertEqual(pip[:5], [str(bundle / 'venv/bin/python3'), '-I', '-m', 'pip', 'install'])
        self.assertEqual(pip[pip.index('--target') + 1], str(bundle / 'parser'))
        for flag in ('--require-hashes', '--no-deps', '--only-binary=:all:'):
            self.assertIn(flag, pip)
        self.assertFalse(any('site-packages' in item for item in pip))
        # The service interpreter, not the venv one, proves the parser loads.
        self.assertEqual(argv[2], ['/usr/bin/python3', '-I', '-B', str(SOURCE / 'runtime_health.py'),
                                   '--require-ok', str(bundle)])
        self.assertEqual(self.calls[1][1]['env']['PIP_CONFIG_FILE'], '/dev/null')

    def test_preparation_recovery_refuses_foreign_bundle(self):
        def fail(*args, **kwargs): raise OSError('synthetic failure')
        with self.assertRaises(OSError): install.install(self.home, self.source, runner=fail)
        marker = self.home / '.ccs/antigravity-switching/runtime-preparation.json'
        bundle = Path(json.loads(marker.read_text())['bundleDirectory'])
        moved = bundle.with_name('foreign-backup'); bundle.rename(moved)
        bundle.mkdir(mode=0o700); (bundle / 'foreign.txt').write_text('foreign')
        with self.assertRaises(layout.InstallationError): install.recover_preparation(self.home)
        self.assertEqual((bundle / 'foreign.txt').read_text(), 'foreign')
        self.assertTrue(marker.exists())

    def test_preparation_recovery_refuses_committed_descriptor(self):
        def fail(*args, **kwargs): raise OSError('synthetic failure')
        with self.assertRaises(OSError): install.install(self.home, self.source, runner=fail)
        descriptor = self.home / '.ccs/antigravity-switching/runtime-installation.json'
        descriptor.write_text('{}'); descriptor.chmod(0o600)
        with self.assertRaises(layout.InstallationError): install.recover_preparation(self.home)
        self.assertEqual(descriptor.read_text(), '{}')

    def test_preparation_cleanup_resumes_after_bundle_removal(self):
        def fail(*args, **kwargs): raise OSError('synthetic failure')
        with self.assertRaises(OSError): install.install(self.home, self.source, runner=fail)
        marker = self.home / '.ccs/antigravity-switching/runtime-preparation.json'
        bundle = Path(json.loads(marker.read_text())['bundleDirectory']); shutil.rmtree(bundle)
        install.recover_preparation(self.home)
        self.assertFalse(marker.exists())

    def test_preparation_cleanup_preserves_foreign_marker_replaced_during_removal(self):
        def fail(*args, **kwargs): raise OSError('synthetic failure')
        with self.assertRaises(OSError): install.install(self.home, self.source, runner=fail)
        marker = self.home / '.ccs/antigravity-switching/runtime-preparation.json'
        original = shutil.rmtree
        def remove_and_replace(bundle):
            original(bundle)
            foreign = marker.with_name('foreign-marker'); foreign.write_text('foreign'); foreign.chmod(0o600)
            foreign.replace(marker)
        with patch.object(install.shutil, 'rmtree', side_effect=remove_and_replace):
            with self.assertRaises(layout.InstallationError): install.recover_preparation(self.home)
        self.assertEqual(marker.read_text(), 'foreign')

    def test_public_source_tamper_fails_before_runner(self):
        (self.source / 'resident_main.py').write_text('# altered public module\n')
        with self.assertRaises(layout.InstallationError): install.install(self.home, self.source, runner=self.runner)
        self.assertEqual(self.calls, [])

    def test_updated_binary_retains_all_native_control_dispatch(self):
        installed = {'nativeBinary': str(self.home / '.local/bin/agy'),
                     'bundleDirectory': str(self.home / 'bundle'), 'socketPath': str(self.home / 'control.sock')}
        with patch.object(layout, 'runtime_release', return_value=True):
            for args in ([], ['--version'], ['--help'], ['update'], ['login'], ['logout'], ['auth'], ['--unknown'], ['a prompt']):
                self.assertEqual(layout.launcher_argv(installed, args, native_pin_matches=False),
                                 [installed['nativeBinary'], *args])

    def test_no_experiment_paths_in_public_inputs(self):
        for path in SOURCE.rglob('*'):
            if path.is_file() and path.suffix in {'.py', '.json', '.txt', '.md'}:
                self.assertNotIn(b'PM-Experiments/', path.read_bytes(), path.name)


if __name__ == '__main__': unittest.main()
