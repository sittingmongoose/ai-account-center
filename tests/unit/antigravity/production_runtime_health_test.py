"""Disposable runtime-bundle health fixtures; never touch the real home or service.

Every bundle below is invented under a temporary directory. The probe runs the
real read-only runtime_health.py with this test's interpreter (`-I -B`), and the
resident entry point runs from a temporary copy of the packaged sources with no
parser, so it stops at its parser check before any socket or process work.
"""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[3] / 'scripts/antigravity'
sys.path.insert(0, str(SOURCE))
import runtime_health as health

CURRENT = '%d.%d' % sys.version_info[:2]
OTHER = '3.%d' % (sys.version_info[1] + 1)


def fake_parser(directory, versions=(('pyte', '0.8.2'), ('wcwidth', '0.9.1'))):
    directory = Path(directory)
    for name, version in versions:
        (directory / name).mkdir(parents=True, exist_ok=True)
        (directory / name / '__init__.py').write_text('# invented fixture package\n')
        info = directory / ('%s-%s.dist-info' % (name, version))
        info.mkdir(parents=True, exist_ok=True)
        (info / 'METADATA').write_text('Metadata-Version: 2.1\nName: %s\nVersion: %s\n' % (name, version))


class RuntimeHealthTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='aic-health-')
        self.addCleanup(self.temp.cleanup)
        self.bundle = (Path(self.temp.name) / 'bundles' / ('e' * 64)).resolve()
        (self.bundle / 'lib').mkdir(parents=True)

    def venv(self, minor):
        venv = self.bundle / 'venv'
        venv.mkdir(exist_ok=True)
        (venv / 'pyvenv.cfg').write_text('home = /usr/bin\ninclude-system-site-packages = false\n'
                                         'version = %s.7\n' % minor)
        return venv / 'lib' / ('python' + minor) / 'site-packages'

    def probe(self):
        return health.probe(self.bundle, python=sys.executable)

    def test_legacy_bundle_after_a_python_upgrade_names_the_missing_module(self):
        fake_parser(self.venv(OTHER))
        self.assertEqual(self.probe(), {'ok': False, 'reason': 'missing-python-module', 'module': 'pyte',
                                        'python': CURRENT, 'builtFor': OTHER})
        self.assertEqual(health.describe(self.probe()),
                         'missing Python module pyte (system Python is %s; the bundle was built for %s)'
                         % (CURRENT, OTHER))

    def test_legacy_bundle_on_its_own_python_still_loads(self):
        fake_parser(self.venv(CURRENT))
        self.assertEqual(self.probe(), {'ok': True, 'reason': None, 'module': None,
                                        'python': CURRENT, 'builtFor': CURRENT})

    def test_version_neutral_parser_loads_whatever_python_built_the_venv(self):
        self.venv(OTHER)
        fake_parser(self.bundle / 'parser')
        self.assertEqual(self.probe(), {'ok': True, 'reason': None, 'module': None,
                                        'python': CURRENT, 'builtFor': OTHER})

    def test_wrong_pinned_version_or_missing_second_module_is_named(self):
        fake_parser(self.bundle / 'parser', (('pyte', '0.8.1'), ('wcwidth', '0.9.1')))
        result = self.probe()
        self.assertEqual((result['ok'], result['reason'], result['module']), (False, 'parser-mismatch', 'pyte'))
        shutil.rmtree(self.bundle / 'parser')
        fake_parser(self.bundle / 'parser', (('pyte', '0.8.2'),))
        result = self.probe()
        self.assertEqual((result['ok'], result['reason'], result['module']),
                         (False, 'missing-python-module', 'wcwidth'))
        self.assertEqual(health.describe(result), 'missing Python module wcwidth')

    def test_probe_writes_nothing_into_the_bundle(self):
        fake_parser(self.bundle / 'parser')
        before = sorted(str(path) for path in self.bundle.rglob('*'))
        self.assertTrue(self.probe()['ok'])
        self.assertEqual(sorted(str(path) for path in self.bundle.rglob('*')), before)

    def test_an_unrunnable_or_hostile_probe_reads_as_unknown(self):
        def missing(*args, **kwargs):
            raise OSError('synthetic missing interpreter')

        unknown = {'ok': False, 'reason': 'probe-failed', 'module': None, 'python': None, 'builtFor': None}
        self.assertEqual(health.probe(self.bundle, run=missing), unknown)

        def hostile(argv, **kwargs):
            raw = json.dumps({'ok': False, 'reason': 'missing-python-module', 'module': 'pyte; rm -rf /',
                              'python': '3.14\nx', 'builtFor': 13}).encode()
            return subprocess.CompletedProcess(argv, 0, raw, b'')

        self.assertEqual(health.probe(self.bundle, run=hostile),
                         {'ok': False, 'reason': 'missing-python-module', 'module': None,
                          'python': None, 'builtFor': None})
        self.assertEqual(health.describe(unknown), 'the parser check could not run')

    def test_require_ok_exit_codes_and_argument_bounds(self):
        script = str(SOURCE / 'runtime_health.py')
        run = lambda *args: subprocess.run([sys.executable, '-I', '-B', script, *args],
                                           capture_output=True, timeout=30)
        self.assertEqual(run('--require-ok', str(self.bundle)).returncode, 1)
        self.assertEqual(run(str(self.bundle)).returncode, 0)
        fake_parser(self.bundle / 'parser')
        self.assertEqual(run('--require-ok', str(self.bundle)).returncode, 0)
        self.assertEqual(run('relative/bundle').returncode, 2)
        self.assertEqual(run().returncode, 2)


class ResidentEntryParserTests(unittest.TestCase):
    def test_missing_parser_names_the_module_instead_of_a_traceback(self):
        probe = subprocess.run([sys.executable, '-I', '-c', 'import pyte'], capture_output=True, timeout=30)
        if probe.returncode == 0:
            self.skipTest('this interpreter has a system pyte, so the bundle parser cannot be missing')
        with tempfile.TemporaryDirectory(prefix='aic-resident-') as temp:
            library = Path(temp) / 'bundle/lib'
            shutil.copytree(SOURCE / 'runtime', library, ignore=shutil.ignore_patterns('__pycache__'))
            completed = subprocess.run(
                [sys.executable, '-I', '-B', str(library / 'resident_main.py'),
                 '--binary', str(Path(temp) / 'invented-agy'), '--database', str(Path(temp) / 'invented.db'),
                 '--socket', str(Path(temp) / 'invented.sock')],
                stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
            self.assertEqual(completed.returncode, 1)
            self.assertEqual(completed.stderr.decode().strip(),
                             'Managed Antigravity runtime failed: missing Python module pyte (runtime-parser-missing).')
            self.assertNotIn(b'Traceback', completed.stderr)
            self.assertFalse((Path(temp) / 'invented.sock').exists())

    def test_resident_entry_loads_the_version_neutral_parser_directory(self):
        spec = importlib.util.spec_from_file_location('resident_main_fixture', SOURCE / 'runtime/resident_main.py')
        sys.path.insert(0, str(SOURCE / 'runtime'))
        try:
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
        finally:
            sys.path.remove(str(SOURCE / 'runtime'))
        self.assertEqual(module.PARSER, (SOURCE / 'runtime').resolve().parent / 'parser')
        self.assertNotIn('site-packages', str(module.PARSER))


if __name__ == '__main__':
    unittest.main(verbosity=2)
