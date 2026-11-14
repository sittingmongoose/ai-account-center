"""Retired switching markers must never route Update all through a resident runtime."""
import contextlib
import json
import os
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/app-updates'))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hermetic  # noqa: F401
import app_updates as updater
import app_update_common as common


class AntigravityNativeUpdateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='aac-agy-native-')
        self.addCleanup(self.temporary.cleanup)
        self.home = pathlib.Path(self.temporary.name)
        self.native = self.home / '.local/bin/agy'
        self.launcher = self.home / '.local/share/ai-account-center/antigravity-runtime/bin/agy'
        self.version = self.home / 'version'
        self.calls = self.home / 'calls'
        self.next_version = self.home / 'next-version'
        self.version.write_text('1.5.7')
        self.next_version.write_text('1.5.9')
        self.native.parent.mkdir(parents=True)
        # A real fixture executable exercises command execution and version rereads.
        self.native.write_text('#!' + sys.executable + '\n' + '''
import json, pathlib, sys
root = pathlib.Path(__file__).resolve().parents[2]
args = sys.argv[1:]
with (root / 'calls').open('a') as stream:
    stream.write(json.dumps(args) + '\\n')
if args == ['--version']:
    print((root / 'version').read_text())
elif args == ['update']:
    if (root / 'fail').exists():
        raise SystemExit(9)
    (root / 'version').write_text((root / 'next-version').read_text())
else:
    raise SystemExit(8)
''')
        self.native.chmod(0o700)
        self.launcher.parent.mkdir(parents=True)
        self.launcher.write_text('#!/bin/sh\nexit 99\n')
        self.launcher.chmod(0o700)
        self.marker = self.home / '.ccs/antigravity-switching/runtime-installation.json'
        self.marker.parent.mkdir(parents=True)
        self.marker.write_text('stale switching descriptor')

    def apply(self, platform='ubuntu', reviewed=None, shim_first=True, scope=None, restart_failure=False):
        candidates = updater._candidates
        command = updater.command
        def fixture_candidates(name, host):
            # Still exercise PATH launcher exclusion, but never discover system installs.
            return [item for item in candidates(name, host) if item.is_relative_to(self.home)]
        def fixture_command(argv, *args, **kwargs):
            self.assertEqual(pathlib.Path(argv[0]), self.native, 'No managed bridge or real app may run')
            self.assertIn(list(argv[1:]), [['--version'], ['update']])
            return command(argv, *args, **kwargs)
        env = {'CCS_HOME': str(self.home), 'CCS_DIR': '',
               'AAC_UPDATE_STATE_DIR': str((scope or self.home / '.ccs') / 'app-updates'),
               'PATH': os.pathsep.join([str(self.launcher.parent if shim_first else self.native.parent), '/usr/bin', '/bin'])}
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.dict(os.environ, env))
            stack.enter_context(mock.patch.object(pathlib.Path, 'home', return_value=self.home))
            stack.enter_context(mock.patch.object(updater, '_candidates', side_effect=fixture_candidates))
            stack.enter_context(mock.patch.object(updater, 'command', side_effect=fixture_command))
            stack.enter_context(mock.patch.object(common, 'command', side_effect=fixture_command))
            stack.enter_context(mock.patch.object(updater, 'scan', return_value=[]))
            stack.enter_context(mock.patch.object(updater, 'latest_agy_version', side_effect=AssertionError('No switching manifest'), create=True))
            if restart_failure:
                stack.enter_context(mock.patch.object(updater, 'cli_instances', return_value=([object()], [], [])))
                stack.enter_context(mock.patch.object(updater, 'check_terminal'))
                stack.enter_context(mock.patch.object(updater, 'restart_cli', side_effect=common.UpdateFailure('restart_failed')))
            install = updater.detect_cli('antigravity-cli', platform)
            self.assertEqual(install.path, self.native)
            self.assertEqual(install.version, self.version.read_text())
            installations = {key: None for key in common.APP_LABELS}
            installations['antigravity-cli'] = install
            stack.enter_context(mock.patch.object(updater, 'detect', return_value=installations))
            rows = updater.run_apply(platform, agy_reviewed=reviewed)['results']
        return next(row for row in rows if row['appId'] == 'antigravity-cli')

    def test_stale_default_marker_updates_the_native_with_either_path(self):
        for shim_first in (False, True):
            for reviewed in (None, frozenset({'1.5.7'})):
                with self.subTest(shim_first=shim_first, reviewed=reviewed):
                    self.version.write_text('1.5.7')
                    row = self.apply(shim_first=shim_first, reviewed=reviewed)
                    self.assertEqual((row['status'], row['messageCode'], row['previousVersion'], row['version']),
                                     ('updated', 'updated', '1.5.7', '1.5.9'))
                    self.assertTrue(row['updateAttempted'])
                    self.assertEqual(self.marker.read_text(), 'stale switching descriptor')
        self.assertEqual([json.loads(line) for line in self.calls.read_text().splitlines()].count(['update']), 4)

    def test_native_updater_failure_is_an_attempted_failure(self):
        (self.home / 'fail').touch()
        row = self.apply()
        self.assertEqual((row['status'], row['messageCode'], row['version'], row['updateAttempted']),
                         ('failed', 'update_failed', '1.5.7', True))
        self.assertIn(['update'], [json.loads(line) for line in self.calls.read_text().splitlines()])

    def test_noop_native_update_reports_current_after_an_attempt(self):
        self.next_version.write_text('1.5.7')
        row = self.apply()
        self.assertEqual((row['status'], row['messageCode'], row['version'], row['updateAttempted']),
                         ('current', 'current', '1.5.7', True))

    def test_changed_native_with_failed_restart_keeps_the_recovery_marker(self):
        row = self.apply(restart_failure=True)
        self.assertEqual((row['status'], row['messageCode'], row['version'], row['updateAttempted']),
                         ('restart_failed', 'restart_failed', '1.5.9', True))
        pending = self.home / '.ccs/app-updates/antigravity-cli-pending-restart.json'
        self.assertEqual(json.loads(pending.read_text()), {'version': '1.5.9'})

    def test_custom_and_dangling_markers_do_not_select_a_coordinator(self):
        self.marker.unlink()
        custom = self.home / 'custom-state'
        marker = custom / 'antigravity-switching/runtime-installation.json'
        marker.parent.mkdir(parents=True)
        marker.symlink_to(custom / 'missing')
        row = self.apply(scope=custom)
        self.assertEqual((row['status'], row['version']), ('updated', '1.5.9'))
        self.assertTrue(marker.is_symlink())

    def test_old_marker_does_not_make_other_platforms_unsupported(self):
        # Platform wrappers/process census are mocked; only the fixture native runs.
        for platform in ('mac', 'windows'):
            with self.subTest(platform=platform):
                self.version.write_text('1.5.7')
                row = self.apply(platform=platform, shim_first=False)
                self.assertEqual((row['status'], row['version']), ('updated', '1.5.9'))


if __name__ == '__main__':
    unittest.main()
