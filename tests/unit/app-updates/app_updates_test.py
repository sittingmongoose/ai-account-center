"""Fixture-only app updater verification; never updates a real installed app."""
import contextlib
import io
import json
import os
import pathlib
import pty
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/app-updates'))
import urllib.error
import app_updates as updater
import app_update_common as common
import app_update_desktop as desktop
import app_update_processes as processes
import app_update_terminal as terminal
import app_update_confirmed_codex as confirmed
import app_update_pipe as pipes


class UpdaterTests(unittest.TestCase):
    def test_inventory_has_no_update_side_effect(self):
        installations = {key: None for key in common.APP_LABELS}
        output = io.StringIO()
        with mock.patch.object(updater, 'detect', return_value=installations), mock.patch.object(updater, 'run_apply') as apply, mock.patch.object(sys, 'argv', ['helper', '--platform', 'ubuntu']), contextlib.redirect_stdout(output):
            updater.main()
        apply.assert_not_called()
        self.assertTrue(json.loads(output.getvalue())['inventory'])

    def test_family_does_not_select_generic_node_or_other_native_binaries(self):
        path = pathlib.Path('/fixture/bin/muse')
        install = common.Install('muse-code', 'ubuntu', path)
        rows = [processes.Process(pid, 1, 1, exe, str(pid)) for pid, exe in enumerate(['/fixture/bin/muse-bin-1.4.2', '/fixture/bin/agy', '/usr/bin/node', '/elsewhere/muse-bin-1.4.2'], 1)]
        self.assertEqual([item.pid for item in processes.family(install, rows)], [1])

    def test_npm_selection_requires_exact_owned_package_argument(self):
        install = common.Install('codex-cli', 'ubuntu', pathlib.Path('/fixture/codex.cmd'), manager='npm', package_root=pathlib.Path('/fixture/node_modules/@openai/codex'))
        rows = [processes.Process(1, 0, 1, '/usr/bin/node', '1', ['node', '/fixture/node_modules/@openai/codex/bin/codex.js']), processes.Process(2, 0, 1, '/usr/bin/node', '2', ['node', '/other/server.js'])]
        self.assertEqual([item.pid for item in processes.family(install, rows)], [1])

    def test_prompt_arguments_are_not_replayed(self):
        value = '01998a55-88aa-7666-a999-010101010101'
        self.assertEqual(terminal.resume_arguments('claude-code', ['claude', '--resume', value, '--print', 'ORIGINAL_PROMPT']), ['--resume', value])
        self.assertEqual(terminal.resume_arguments('codex-cli', ['codex', 'exec', 'ORIGINAL_PROMPT']), [])
        self.assertEqual(terminal.resume_arguments('omp', ['omp', '--resume', '/private/session-file', 'ORIGINAL_PROMPT']), [])

    def test_restart_env_is_private_and_idle(self):
        context = processes.Process(1, 0, 1, '/fixture/agy', '1', env={'HOME': '/fixture', 'TEST_SECRET': 'PRIVATE', 'MUSE_LAUNCHER_INSTALL': '1', 'MUSE_UPGRADE_MODE': '1'})
        values = terminal.restart_environment(context)
        self.assertEqual(values['TEST_SECRET'], 'PRIVATE')
        self.assertNotIn('MUSE_LAUNCHER_INSTALL', values)
        self.assertNotIn('CODEX_NON_INTERACTIVE', values)
        self.assertNotIn('PRIVATE', repr(context))

    def test_confirmed_codex_check_is_read_only_and_rejects_prompts(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)):
            home = pathlib.Path(directory)
            binary = home / '.codex/packages/standalone/releases/1.0.0/bin/codex'
            binary.parent.mkdir(parents=True); binary.write_text('fixture')
            active = home / '.local/bin/codex'; active.parent.mkdir(parents=True); active.symlink_to(binary)
            context = {'exe': str(active), 'cwd': str(home), 'env': {'HOME': str(home)}}
            with mock.patch.object(confirmed, 'check_terminal') as check, mock.patch.object(confirmed, 'restart_cli') as launch:
                self.assertEqual(confirmed.restart(context, check=True), {'success': True})
                check.assert_called_once(); launch.assert_not_called()
                with self.assertRaises(ValueError): confirmed.restart({**context, 'args': ['original prompt']}, check=True)
                with self.assertRaises(ValueError): confirmed.restart({**context, 'env': {'CODEX_HOME': '/other'}}, check=True)
                with self.assertRaises(ValueError): confirmed.restart({**context, 'exe': '/usr/bin/node'}, check=True)

    def test_confirmed_codex_npm_requires_exact_installed_package_layout(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)):
            root = pathlib.Path(directory) / '.local/lib/node_modules/@openai/codex'
            binary = root / 'node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex'
            binary.parent.mkdir(parents=True); binary.write_text('native fixture')
            (root / 'package.json').write_text('{"name":"@openai/codex"}')
            self.assertEqual(confirmed.installed_codex(binary), binary)
            (root / 'package.json').write_text('{"name":"impostor"}')
            with self.assertRaises(ValueError): confirmed.installed_codex(binary)

    def test_windows_broker_endpoint_is_fixed_local_nonce_only(self):
        self.assertTrue(pipes.valid_endpoint(pipes.PREFIX+'0123456789abcdef'*2))
        self.assertFalse(pipes.valid_endpoint(r'\\remote\pipe\ccs-update-terminal-'+'a'*32))
        self.assertFalse(pipes.valid_endpoint(pipes.PREFIX+'../other'))

    def test_lock_does_not_mislabel_body_io_error_as_busy(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)):
            with self.assertRaisesRegex(OSError, 'fixture'):
                with common.execution_lock():
                    raise OSError('fixture')

    def test_private_inventory_limit_does_not_expand_public_output_limit(self):
        response = subprocess.CompletedProcess(['fixture'], 0, stdout=b'x' * 112071)
        with mock.patch.object(common.subprocess, 'run', return_value=response):
            with self.assertRaises(common.UpdateFailure):
                common.command(['fixture'], capture=True)
            self.assertEqual(len(common.command(['fixture'], capture=True, capture_limit=2 * 1024 * 1024)), 112071)

    def test_msix_identity_mismatch_stops_before_process_shutdown(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            package = root / 'update.msix'
            with zipfile.ZipFile(package, 'w') as value:
                value.writestr('AppxManifest.xml', '<Package><Identity Name="Impostor" Publisher="Other" Version="9.0.0.0" ProcessorArchitecture="x64"/></Package>')
            install = common.Install('codex-desktop', 'windows', root / 'ChatGPT.exe', '1.0.0.0', 'msix', 'OpenAI.Codex', 'Expected')
            with mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(root)), mock.patch.object(desktop, 'download'), mock.patch.object(desktop, 'terminate_desktops') as stop:
                value = desktop.update_windows(install)
            self.assertEqual(value['messageCode'], 'signature_failed')
            stop.assert_not_called()

    def test_current_cli_keeps_existing_processes(self):
        install = common.Install('agy', 'ubuntu', pathlib.Path('/fixture/agy'), '1.0.0')
        install.app_id = 'antigravity-cli'
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'perform_cli_update') as update, mock.patch.object(updater, 'detect_cli', return_value=install), mock.patch.object(updater, 'terminate_cli') as stop:
            value = updater.update_cli(install, time.monotonic() + 60)
        update.assert_called_once()
        stop.assert_not_called()
        self.assertEqual(value['status'], 'current')

    def test_stale_same_version_marker_reports_current_without_restart(self):
        # A marker matching the installed version is stale pre-stop
        # bookkeeping, never a retry request: reporting "updated" for it
        # would claim an update that never happened.
        install = common.Install('antigravity-cli', 'ubuntu', pathlib.Path('/fixture/agy'), '2.0.0')
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'perform_cli_update'), mock.patch.object(updater, 'detect_cli', return_value=install), mock.patch.object(updater, 'restart_cli', return_value=[]) as restart:
            marker = pathlib.Path(directory) / '.ccs/app-updates/antigravity-cli-pending-restart.json'
            common.write_private_json(marker, {'version': '2.0.0'})
            value = updater.update_cli(install, time.monotonic() + 60)
            self.assertFalse(marker.exists())
        self.assertEqual(value['status'], 'current')
        restart.assert_not_called()

    def test_untrusted_apt_origin_never_stops_or_installs(self):
        install = common.Install('codex-desktop', 'ubuntu', pathlib.Path('/usr/lib/chatgpt/ChatGPT'), '1.0.0', 'apt', 'chatgpt')
        def command(argv, **kwargs):
            if 'policy' in argv: return 'Candidate: 2.0.0\n'
            if 'madison' in argv: return 'chatgpt | 2.0.0 | https://impostor.test/deb stable/main amd64 Packages'
            return ''
        with mock.patch.object(desktop, 'command', side_effect=command), mock.patch.object(desktop, 'terminate_desktops') as stop:
            value = desktop.update_linux(install)
        self.assertEqual(value['messageCode'], 'signature_failed')
        stop.assert_not_called()

    def test_absent_apps_never_install(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'detect', return_value={key: None for key in common.APP_LABELS}), mock.patch.object(updater, 'perform_cli_update') as update:
            value = updater.run_apply('ubuntu')
        self.assertEqual(len(value['results']), 7)
        self.assertTrue(all(item['status'] == 'not_installed' for item in value['results']))
        update.assert_not_called()

    def test_readiness_without_supported_updater_is_failed(self):
        install = common.Install('muse-code', 'ubuntu', pathlib.Path('/fixture/muse'), '1.0.0', 'unsupported')
        with mock.patch.object(updater, 'scan') as scan:
            self.assertEqual(updater.check_readiness(install), ('failed', 'unsupported'))
            scan.assert_not_called()

    def test_readiness_with_unrestartable_instances_is_failed(self):
        install = common.Install('muse-code', 'ubuntu', pathlib.Path('/fixture/muse'), '1.0.0')
        with mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'check_terminal', side_effect=common.UpdateFailure('restart_context')):
            self.assertEqual(updater.check_readiness(install), ('failed', 'restart_context'))

    def test_readiness_probe_crash_is_unknown_not_failed(self):
        install = common.Install('muse-code', 'ubuntu', pathlib.Path('/fixture/muse'), '1.0.0')
        with mock.patch.object(updater, 'scan', side_effect=RuntimeError('fixture boom')):
            self.assertEqual(updater.check_readiness(install), ('unknown', 'readiness_unknown'))

    def test_readiness_ready_install_has_no_gate(self):
        install = common.Install('muse-code', 'ubuntu', pathlib.Path('/fixture/muse'), '1.0.0')
        with mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'check_terminal') as check:
            self.assertIsNone(updater.check_readiness(install))
            check.assert_called_once()

    def test_run_apply_skips_unready_apps_without_attempting(self):
        installations = {key: common.Install(key, 'ubuntu', pathlib.Path('/fixture/app'), '1.0.0') for key in common.APP_LABELS}
        def gate(install):
            return ('failed', 'unsupported') if install.app_id == 'muse-code' else None
        def cli(install, deadline):
            return common.result(install.app_id, 'ubuntu', 'current', '1.0.0', '1.0.0', 'native', attempted=True)
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'detect', return_value=installations), mock.patch.object(updater, 'check_readiness', side_effect=gate), mock.patch.object(updater, 'update_cli', side_effect=cli) as update, mock.patch.object(updater, 'update_desktop', side_effect=cli) as desktop_update:
            value = updater.run_apply('ubuntu')
        rows = {item['appId']: item for item in value['results']}
        self.assertEqual(len(value['results']), 7)
        self.assertEqual(rows['muse-code']['status'], 'failed')
        self.assertEqual(rows['muse-code']['messageCode'], 'unsupported')
        self.assertFalse(rows['muse-code']['updateAttempted'])
        attempted = {call.args[0].app_id for call in update.call_args_list} | {call.args[0].app_id for call in desktop_update.call_args_list}
        self.assertNotIn('muse-code', attempted)
        self.assertEqual(len(attempted), 6)

    def test_muse_installer_runs_under_bash_not_posix_sh(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)):
            launcher = pathlib.Path(directory) / '.local/bin/muse'
            launcher.parent.mkdir(parents=True); launcher.write_text('fixture launcher')
            install = common.Install('muse-code', 'ubuntu', launcher, '1.4.2')
            with mock.patch.object(updater, 'download') as fetch, mock.patch.object(updater, 'command') as run:
                updater.perform_cli_update(install)
            fetch.assert_called_once()
            argv = run.call_args.args[0]
            self.assertTrue(argv[0].endswith('bash'), argv)
            self.assertNotEqual(argv[0], '/bin/sh')

    def test_muse_installer_without_bash_is_unsupported(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)):
            launcher = pathlib.Path(directory) / '.local/bin/muse'
            launcher.parent.mkdir(parents=True); launcher.write_text('fixture launcher')
            install = common.Install('muse-code', 'ubuntu', launcher, '1.4.2')
            with mock.patch.object(updater.shutil, 'which', return_value=None), mock.patch.object(pathlib.Path, 'is_file', return_value=False):
                with self.assertRaises(common.UpdateFailure) as raised:
                    updater.perform_cli_update(install)
            self.assertEqual(raised.exception.code, 'unsupported')

    def test_npm_update_runs_node_directly_without_cmd_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            node_dir = root / 'nodejs'; node_dir.mkdir()
            (node_dir / 'npm.cmd').write_text('fixture')
            (node_dir / 'node.exe').write_text('fixture')
            cli = node_dir / 'node_modules/npm/bin/npm-cli.js'
            cli.parent.mkdir(parents=True); cli.write_text('fixture')
            prefix = root / 'npm'; prefix.mkdir()
            install = common.Install('codex-cli', 'windows', prefix / 'codex.cmd', '0.153.4', 'npm', package_root=prefix / 'node_modules/@openai/codex')
            with mock.patch.object(updater.shutil, 'which', side_effect=lambda name: str(node_dir / (name or ''))), mock.patch.object(updater, 'command') as run:
                updater.perform_cli_update(install)
            argv = run.call_args.args[0]
            self.assertEqual(argv[0], str(node_dir / 'node.exe'))
            self.assertEqual(argv[1], str(cli))
            self.assertEqual(argv[2:6], ['install', '--global', '--prefix', str(prefix)])
            self.assertEqual(argv[6], '@openai/codex@latest')
            self.assertNotIn('cmd.exe', ' '.join(argv).lower())

    def test_npm_windows_stops_running_cli_before_install_and_relaunches_on_failure(self):
        install = common.Install('codex-cli', 'windows', pathlib.Path('/fixture/npm/codex.cmd'), '0.153.4', 'npm', package_root=pathlib.Path('/fixture/npm/node_modules/@openai/codex'))
        context = processes.Process(11, 1, 1, '/fixture/node', '11', ['node', '/fixture/npm/node_modules/@openai/codex/bin/codex.js'])
        calls = []
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([context], [context])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'npm_view_latest', return_value=None), \
                mock.patch.object(updater, 'terminate_cli', side_effect=lambda *args: calls.append('stop') or 0), \
                mock.patch.object(updater, 'perform_cli_update', side_effect=lambda item, deadline=None: calls.append('install') or (_ for _ in ()).throw(common.UpdateFailure())), \
                mock.patch.object(updater, 'restart_cli', side_effect=lambda *args: calls.append('relaunch') or []), \
                mock.patch.object(updater, 'detect_cli', return_value=install):
            value = updater.update_cli(install, time.monotonic() + 60)
            marker = pathlib.Path(directory) / '.ccs/app-updates/codex-cli-pending-restart.json'
            self.assertFalse(marker.exists())
        self.assertEqual(calls[0], 'stop')
        self.assertLess(calls.index('stop'), calls.index('install'))
        self.assertIn('relaunch', calls)
        self.assertEqual(value['status'], 'failed')
        self.assertEqual(value['messageCode'], 'update_failed')
        self.assertTrue(value['updateAttempted'])

    def test_npm_current_version_skips_install_without_stopping(self):
        install = common.Install('codex-cli', 'windows', pathlib.Path('/fixture/npm/codex.cmd'), '0.160.0', 'npm', package_root=pathlib.Path('/fixture/npm/node_modules/@openai/codex'))
        context = processes.Process(12, 1, 1, '/fixture/node', '12', ['node', '/fixture/npm/node_modules/@openai/codex/bin/codex.js'])
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([context], [context])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'npm_view_latest', return_value='0.160.0'), \
                mock.patch.object(updater, 'terminate_cli') as stop, mock.patch.object(updater, 'perform_cli_update') as install_step:
            value = updater.update_cli(install, time.monotonic() + 60)
        stop.assert_not_called(); install_step.assert_not_called()
        self.assertEqual(value['status'], 'current')
        self.assertFalse(value['updateAttempted'])

    def test_npm_unknown_registry_version_proceeds_to_install(self):
        install = common.Install('codex-cli', 'windows', pathlib.Path('/fixture/npm/codex.cmd'), '0.153.4', 'npm', package_root=pathlib.Path('/fixture/npm/node_modules/@openai/codex'))
        context = processes.Process(13, 1, 1, '/fixture/node', '13', ['node', '/fixture/npm/node_modules/@openai/codex/bin/codex.js'])
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([context], [context])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'npm_view_latest', return_value=None), \
                mock.patch.object(updater, 'terminate_cli', return_value=0) as stop, \
                mock.patch.object(updater, 'perform_cli_update') as install_step, \
                mock.patch.object(updater, 'restart_cli', return_value=[]), \
                mock.patch.object(updater, 'detect_cli', return_value=common.Install('codex-cli', 'windows', pathlib.Path('/fixture/npm/codex.cmd'), '0.160.0', 'npm')):
            value = updater.update_cli(install, time.monotonic() + 60)
        self.assertEqual(stop.call_count, 2)
        install_step.assert_called_once()
        self.assertEqual(value['status'], 'updated')

    def test_cli_blocked_download_reports_update_failed(self):
        install = common.Install('muse-code', 'ubuntu', pathlib.Path('/fixture/muse'), '1.4.2')
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'perform_cli_update', side_effect=common.UpdateFailure('download_blocked')):
            value = updater.update_cli(install, time.monotonic() + 60)
        self.assertEqual(value['status'], 'failed')
        self.assertEqual(value['messageCode'], 'update_failed')

    def test_install_timeouts_clamp_to_the_platform_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            node_dir = root / 'nodejs'; node_dir.mkdir()
            (node_dir / 'node.exe').write_text('fixture')
            cli = node_dir / 'node_modules/npm/bin/npm-cli.js'
            cli.parent.mkdir(parents=True); cli.write_text('fixture')
            install = common.Install('codex-cli', 'windows', root / 'npm/codex.cmd', '0.153.4', 'npm', package_root=root / 'npm/node_modules/@openai/codex')
            with mock.patch.object(updater.shutil, 'which', side_effect=lambda name: str(node_dir / (name or ''))), mock.patch.object(updater, 'command') as run:
                updater.perform_cli_update(install, time.monotonic() + 40000)
                self.assertEqual(run.call_args.kwargs['timeout'], 300)
                updater.perform_cli_update(install, time.monotonic() + 40)
                self.assertLessEqual(run.call_args.kwargs['timeout'], 40)
                self.assertGreaterEqual(run.call_args.kwargs['timeout'], 30)

    def test_download_maps_forbidden_to_blocked(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / 'update.dmg'
            with contextlib.closing(urllib.error.HTTPError('https://fixture.test/app.dmg', 403, 'Forbidden', {}, io.BytesIO())) as failure, \
                    mock.patch.object(common.urllib.request, 'urlopen', side_effect=failure):
                with self.assertRaises(common.UpdateFailure) as raised:
                    common.download('https://fixture.test/app.dmg', target)
            self.assertEqual(raised.exception.code, 'download_blocked')

    def test_download_maps_challenge_page_to_blocked(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / 'update.dmg'
            response = mock.MagicMock()
            response.geturl.return_value = 'https://fixture.test/app.dmg'
            response.headers.get.return_value = None
            response.read.side_effect = [b'<!DOCTYPE html><html><title>Just a moment...</title>', b'']
            response.__enter__.return_value = response
            with mock.patch.object(common.urllib.request, 'urlopen', return_value=response):
                with self.assertRaises(common.UpdateFailure) as raised:
                    common.download('https://fixture.test/app.dmg', target)
            self.assertEqual(raised.exception.code, 'download_blocked')
            self.assertFalse(target.exists())

    def test_download_honours_explicit_desktop_maximum(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / 'update.msix'
            response = mock.MagicMock()
            response.geturl.return_value = 'https://fixture.test/app.msix'
            response.headers.get.side_effect = lambda name: '912020731' if name == 'Content-Length' else None
            response.read.return_value = b''
            response.__enter__.return_value = response
            with mock.patch.object(common.urllib.request, 'urlopen', return_value=response):
                with self.assertRaises(common.UpdateFailure):
                    common.download('https://fixture.test/app.msix', target)
                common.download('https://fixture.test/app.msix', target, maximum=desktop.DESKTOP_DOWNLOAD_MAXIMUM)
            self.assertTrue(target.exists())

    def _fake_mac_bundle(self, root, name, identity, version):
        app = root / name
        info = app / 'Contents/Info.plist'
        info.parent.mkdir(parents=True)
        import plistlib
        with info.open('wb') as handle:
            plistlib.dump({'CFBundleIdentifier': identity, 'CFBundleShortVersionString': version}, handle)
        return app

    def test_mac_refused_quit_reports_quit_first_without_swapping(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            installed = self._fake_mac_bundle(root / 'Applications', 'ChatGPT.app', 'com.openai.codex', '26.928.31416')
            install = common.Install('codex-desktop', 'mac', installed, '26.928.31416', 'official-download', 'com.openai.codex', package_root=installed)
            candidate = self._fake_mac_bundle(root / 'mnt', 'ChatGPT.app', 'com.openai.codex', '26.930.41038')
            (candidate / 'Contents' / 'staged-marker.txt').write_text('staged')
            refused = processes.Process(21, 1, 1, str(installed / 'Contents/MacOS/ChatGPT'), '21', [str(installed / 'Contents/MacOS/ChatGPT')])
            import plistlib as plist
            attached = plist.dumps({'system-entities': [{'mount-point': str(root / 'mnt'), 'dev-entry': '/dev/disk9'}]}).decode('utf-8')
            def commands(argv, **kwargs):
                if argv[:2] == ['/usr/bin/hdiutil', 'attach']:
                    return attached
                return ''
            with mock.patch.object(desktop, 'download'), mock.patch.object(desktop, 'command', side_effect=commands), \
                    mock.patch.object(desktop, 'verify_mac', return_value={'CFBundleIdentifier': 'com.openai.codex', 'CFBundleShortVersionString': '26.930.41038'}), \
                    mock.patch.object(desktop, 'scan', return_value=[]), \
                    mock.patch.object(desktop, 'main_contexts', return_value=[refused]), \
                    mock.patch.object(desktop, 'request_desktop_quit', return_value=([], [refused])), \
                    mock.patch.object(desktop, 'restart_desktops') as relaunch, \
                    mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(root / 'stage-tmp')):
                (root / 'stage-tmp').mkdir(exist_ok=True)
                value = desktop.update_mac(install)
            relaunch.assert_not_called()
            self.assertEqual(value['status'], 'action_required')
            self.assertEqual(value['messageCode'], 'quit_first')
            self.assertFalse(value['updateAttempted'])
            self.assertEqual(value['version'], '26.928.31416')
            self.assertFalse((installed / 'Contents' / 'staged-marker.txt').exists())
            self.assertEqual([path.name for path in (root / 'Applications').iterdir()], ['ChatGPT.app'])

    def test_mac_download_timeout_keeps_its_code(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            installed = self._fake_mac_bundle(root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
            install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
            with mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure('timeout')), \
                    mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(root)):
                value = desktop.update_mac(install)
            self.assertEqual(value['status'], 'failed')
            self.assertEqual(value['messageCode'], 'timeout')

    def test_desktop_retry_refused_reports_quit_first(self):
        install = common.Install('codex-desktop', 'mac', pathlib.Path('/fixture/ChatGPT.app'), '26.930.41038', 'official-download', 'com.openai.codex', package_root=pathlib.Path('/fixture/ChatGPT.app'))
        running = processes.Process(51, 1, 1, '/fixture/ChatGPT.app/Contents/MacOS/ChatGPT', '51', ['/fixture/ChatGPT.app/Contents/MacOS/ChatGPT'])
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(desktop, 'scan', return_value=[]), \
                mock.patch.object(desktop, 'main_contexts', return_value=[running]), \
                mock.patch.object(desktop, 'request_desktop_quit', return_value=([], [running])), \
                mock.patch.object(desktop, 'restart_desktops') as relaunch:
            marker = pathlib.Path(directory) / '.ccs/app-updates/codex-desktop-pending-restart.json'
            common.write_private_json(marker, {'version': '26.930.41038'})
            value = desktop.update_desktop(install)
            self.assertTrue(marker.exists())
        relaunch.assert_not_called()
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'quit_first')
        self.assertFalse(value['updateAttempted'])

    def test_mac_blocked_download_reports_check_in_app(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            installed = self._fake_mac_bundle(root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
            install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
            with mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure('download_blocked')), \
                    mock.patch.object(desktop.time, 'sleep'), \
                    mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(root)):
                value = desktop.update_mac(install)
            self.assertEqual(value['status'], 'action_required')
            self.assertEqual(value['messageCode'], 'check_in_app')
            self.assertFalse(value['updateAttempted'])

    def test_windows_survivors_report_quit_first_without_installing(self):
        install = common.Install('codex-desktop', 'windows', pathlib.Path('/fixture/ChatGPT.exe'), '26.930.3748.0', 'msix', 'OpenAI.Codex', 'CN=fixture', pathlib.Path('/fixture'))
        running = processes.Process(31, 1, 1, '/fixture/ChatGPT.exe', '31', ['/fixture/ChatGPT.exe'])
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.4958.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'scan', return_value=[]), \
                mock.patch.object(desktop, 'main_contexts', return_value=[running]), \
                mock.patch.object(desktop, 'request_desktop_quit', return_value=([], [running])), \
                mock.patch.object(desktop, 'add_appx_package') as deploy, \
                mock.patch.object(desktop, 'restart_desktops') as relaunch, \
                mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(pathlib.Path(directory))):
            value = desktop.update_windows(install)
        deploy.assert_not_called()
        relaunch.assert_not_called()
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'quit_first')
        self.assertFalse(value['updateAttempted'])

    def test_windows_deploy_needing_close_reports_quit_first(self):
        install = common.Install('codex-desktop', 'windows', pathlib.Path('/fixture/ChatGPT.exe'), '26.930.3748.0', 'msix', 'OpenAI.Codex', 'CN=fixture', pathlib.Path('/fixture'))
        closed = processes.Process(32, 1, 1, '/fixture/ChatGPT.exe', '32', ['/fixture/ChatGPT.exe'])
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.4958.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'scan', return_value=[]), \
                mock.patch.object(desktop, 'main_contexts', return_value=[closed]), \
                mock.patch.object(desktop, 'request_desktop_quit', return_value=([closed], [])), \
                mock.patch.object(desktop, 'add_appx_package', side_effect=common.UpdateFailure('quit_first')), \
                mock.patch.object(desktop, 'restart_desktops', return_value=1) as relaunch, \
                mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(pathlib.Path(directory))):
            value = desktop.update_windows(install)
        relaunch.assert_called_once()
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'quit_first')
        self.assertFalse(value['updateAttempted'])

    def test_request_quit_collects_refusals_without_forcing(self):
        first = processes.Process(41, 1, 1, '/fixture/app', '41', ['/fixture/app'])
        second = processes.Process(42, 1, 1, '/fixture/app', '42', ['/fixture/app'])
        install = common.Install('codex-desktop', 'mac', pathlib.Path('/fixture/ChatGPT.app'), '1.0', 'official-download')
        with mock.patch.object(processes, 'mac_quit_request', side_effect=[True, False]) as quit, \
                mock.patch.object(processes, 'live_contexts', side_effect=[[first, second], [], [second]]), \
                mock.patch.object(processes.time, 'sleep'):
            exited, refused = processes.request_desktop_quit(install, [first, second], grace=0)
        self.assertEqual([item.pid for item in exited], [41])
        self.assertEqual([item.pid for item in refused], [42])
        self.assertEqual(quit.call_count, 2)

    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('gcc') and shutil.which('tmux'), 'native fixture compiler and tmux required')
    def test_real_standin_cli_restarts_in_new_pty_without_original_prompt(self):
        source = r'''#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>
int main(int argc,char **argv){
 if(argc>1 && !strcmp(argv[1],"--version")){puts(VERSION);return 0;}
 FILE *out=fopen(getenv("FIXTURE_LOG"),"a");
 fprintf(out,"%d %s %d %d %d\n",getpid(),VERSION,argc,isatty(0),getenv("FIXTURE_PRIVATE") && !strcmp(getenv("FIXTURE_PRIVATE"),"private-fixture"));fclose(out);
 while(1)sleep(1);return 0;
}'''
        with tempfile.TemporaryDirectory(prefix='ccs-update-fixture-') as directory:
            root = pathlib.Path(directory); code = root / 'fixture.c'; code.write_text(source)
            binary, stage, unrelated = root / 'agy', root / 'agy-next', root / 'unrelated'
            for target, version in [(binary, '1.0.0'), (stage, '2.0.0'), (unrelated, '8.0.0')]:
                subprocess.run(['gcc', '-DVERSION="'+version+'"', str(code), '-o', str(target)], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            log = root / 'runs'; env = {**os.environ, 'FIXTURE_LOG': str(log), 'FIXTURE_PRIVATE': 'private-fixture'}
            master, slave = pty.openpty()
            old = subprocess.Popen([str(binary), '--original-prompt', 'DO_NOT_REPLAY'], stdin=slave, stdout=slave, stderr=slave, env=env, cwd=root)
            other = subprocess.Popen([str(unrelated)], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env, cwd=root)
            os.close(slave); value = None
            try:
                deadline = time.monotonic()+5
                while (not log.exists() or len(log.read_text().splitlines()) < 2) and time.monotonic()<deadline: time.sleep(.05)
                install = common.Install('antigravity-cli', 'ubuntu', binary, '1.0.0')
                replacement = common.Install('antigravity-cli', 'ubuntu', binary, '2.0.0')
                with mock.patch.object(pathlib.Path, 'home', return_value=root), mock.patch.object(updater, 'perform_cli_update', side_effect=lambda item, deadline=None: os.replace(stage, binary)), mock.patch.object(updater, 'detect_cli', return_value=replacement):
                    value = updater.update_cli(install, time.monotonic()+60)
                self.assertEqual(value['status'], 'updated', value)
                self.assertEqual(value['restartedProcesses'], 1)
                old.wait(timeout=3)
                self.assertIsNone(other.poll(), 'unrelated process must remain running')
                runs = [line.split() for line in log.read_text().splitlines()]
                fresh = next(row for row in runs if row[1] == '2.0.0')
                self.assertNotEqual(int(fresh[0]), old.pid)
                self.assertEqual(fresh[2:], ['1','1','1'], 'fresh idle args, real terminal, private env preserved')
                self.assertNotIn('DO_NOT_REPLAY', json.dumps(value))
                self.assertNotIn('private-fixture', json.dumps(value))
            finally:
                if value:
                    for target in value.get('restartTargets', []):
                        subprocess.run(['tmux','-L',target['server'],'kill-server'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                for child in [old, other]:
                    if child.poll() is None: child.terminate()
                    child.wait(timeout=3)
                os.close(master)


if __name__ == '__main__': unittest.main()
