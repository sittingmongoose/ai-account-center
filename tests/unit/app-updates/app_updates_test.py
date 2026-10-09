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
        with mock.patch.object(common, '_run_bounded', return_value=(0, b'x' * 112071)):
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
            with mock.patch.object(desktop, 'private_temporary', return_value=contextlib.nullcontext(root)), mock.patch.object(desktop, 'download'), \
                    mock.patch.object(desktop, 'scan', return_value=[]), mock.patch.object(desktop, 'remote_msix_identity', return_value=None), \
                    mock.patch.object(desktop, 'codex_store_version', return_value=None), mock.patch.object(desktop, 'terminate_desktops') as stop:
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
        self.assertEqual(len(value['results']), len(common.APP_LABELS))
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
        def cli(install, deadline, phase=None):
            return common.result(install.app_id, 'ubuntu', 'current', '1.0.0', '1.0.0', 'native', attempted=True)
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'detect', return_value=installations), mock.patch.object(updater, 'check_readiness', side_effect=gate), mock.patch.object(updater, 'update_cli', side_effect=cli) as update, mock.patch.object(updater, 'update_desktop', side_effect=cli) as desktop_update, mock.patch.object(updater, 'update_t3', side_effect=cli) as t3_update, mock.patch.object(updater, 'antigravity_hold', return_value=None):
            value = updater.run_apply('ubuntu')
        rows = {item['appId']: item for item in value['results']}
        self.assertEqual(len(value['results']), len(common.APP_LABELS))
        self.assertEqual(rows['muse-code']['status'], 'failed')
        self.assertEqual(rows['muse-code']['messageCode'], 'unsupported')
        self.assertFalse(rows['muse-code']['updateAttempted'])
        attempted = {call.args[0].app_id for call in update.call_args_list} | {call.args[0].app_id for call in desktop_update.call_args_list} | {call.args[0].app_id for call in t3_update.call_args_list}
        self.assertNotIn('muse-code', attempted)
        self.assertEqual(len(attempted), len(common.APP_LABELS) - 1)

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

    @unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('gcc') and shutil.which('tmux'), 'native fixture compiler and tmux required')
    def test_ubuntu_tmux_restart_escapes_service_cgroup(self):
        install = common.Install('codex-cli', 'ubuntu', pathlib.Path('/fixture/codex'), '0.160.0')
        context = mock.Mock(cwd='/tmp', args=[], env={'HOME': '/tmp', 'PATH': '/usr/bin'})
        alive = [mock.Mock(pid=4242, ppid=1)]
        which = lambda name: {'tmux': '/usr/bin/tmux', 'systemd-run': '/usr/bin/systemd-run'}.get(name)
        with mock.patch.object(terminal.shutil, 'which', side_effect=which), \
             mock.patch.object(terminal, 'command') as run, \
             mock.patch.object(terminal, 'family', return_value=alive), \
             mock.patch.object(terminal, 'scan', return_value=[]):
            sessions = terminal.restart_cli(install, [context])
        self.assertEqual(sessions[0]['kind'], 'tmux')
        argv = run.call_args.args[0]
        self.assertTrue(argv[0].endswith('systemd-run'), argv)
        self.assertIn('--user', argv)
        self.assertIn('--collect', argv)
        self.assertIn('--service-type=forking', argv)
        self.assertIn('--working-directory=/tmp', argv)
        self.assertIn('--setenv=HOME=/tmp', argv)
        self.assertNotIn('--scope', argv)
        unit = next(item for item in argv if item.startswith('--unit='))
        self.assertRegex(unit, r'^--unit=aac-launch-codex-cli-[0-9a-f]{12}$')
        separator = argv.index('--')
        self.assertTrue(argv[separator + 1].endswith('tmux'), argv)
        self.assertIn('new-session', argv[separator:])

    def test_ubuntu_restart_without_systemd_run_fails_closed(self):
        with mock.patch.object(terminal.shutil, 'which', return_value=None):
            with self.assertRaises(common.UpdateFailure) as raised:
                terminal.systemd_user_service_argv('aac-launch-x', '/tmp', {}, ['tmux'])
            self.assertEqual(raised.exception.code, 'restart_context')
            with self.assertRaises(common.UpdateFailure):
                terminal.check_terminal('ubuntu', [mock.Mock()])

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


def _fixture_file(path, text='fixture'):
    """A fixture file in a temporary folder (never a real install)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


@contextlib.contextmanager
def windows_case_folding():
    """os.path.normcase folds case only on Windows; stand in for that on POSIX."""
    with mock.patch.object(os.path, 'normcase', side_effect=lambda value: str(value).lower()):
        yield


class WindowsCaseFoldingTests(unittest.TestCase):
    """Windows names ignore case, but PATHEXT, resolve() and process paths can spell them in any case."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix='ccs-update-case-')
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name).resolve()
        self.home, self.local = self.root / 'Home', self.root / 'Local'
        for patcher in (mock.patch.object(pathlib.Path, 'home', return_value=self.home), mock.patch.dict(os.environ, {'LOCALAPPDATA': str(self.local)})):
            patcher.start()
            self.addCleanup(patcher.stop)

    def detect_muse(self, shim, launcher=True):
        """Meta's Windows install with shutil.which finding `shim`; returns (install, PowerShell probe)."""
        _fixture_file(shim)
        launcher_file = self.local / 'Programs/muse/.muse-launcher.ps1'
        launcher_file.unlink(missing_ok=True)
        if launcher:
            _fixture_file(launcher_file)
        with mock.patch.object(updater.shutil, 'which', return_value=str(shim)), mock.patch.object(updater, 'command', return_value='Muse Code (1.4.2)\n') as probe:
            return updater.detect_cli('muse-code', 'windows'), probe

    def test_windows_muse_cmd_from_path_ext_is_native_with_its_launcher(self):
        install, probe = self.detect_muse(self.local / 'Programs/muse/muse.CMD')
        self.assertEqual((install.manager, install.path, install.version, install.probe), ('native', self.local / 'Programs/muse/muse.cmd', '1.4.2', None))
        self.assertIn(str(self.local / 'Programs/muse/.muse-launcher.ps1'), [str(arg) for arg in probe.call_args.args[0]])

    def test_windows_muse_launcher_name_case_is_ignored(self):
        for name in ('muse', 'muse.cmd', 'MUSE.CMD', 'Muse.Cmd'):
            with self.subTest(name=name):
                install, _ = self.detect_muse(self.local / 'Programs/muse' / name)
                self.assertEqual((install.manager, install.path), ('native', self.local / 'Programs/muse/muse.cmd'))

    def test_windows_muse_outside_its_folder_or_without_launcher_stays_unsupported(self):
        for label, shim, launcher in (
            ('other folder', self.root / 'Other/muse.CMD', True),
            ('no launcher', self.local / 'Programs/muse/muse.CMD', False),
            ('other name', self.local / 'Programs/muse/MUSE.EXE', True),
        ):
            with self.subTest(label):
                install, probe = self.detect_muse(shim, launcher)
                self.assertEqual(install.manager, 'unsupported')
                probe.assert_not_called()

    def test_muse_binary_prefix_matches_whatever_case_windows_reports(self):
        install = common.Install('muse-code', 'windows', self.local / 'Programs/muse/muse.cmd')
        rows = [processes.Process(7, 1, 1, str(self.local / 'Programs/muse/MUSE-BIN-1.4.2.exe'), '7'),
                processes.Process(8, 1, 1, str(self.local / 'Other/MUSE-BIN-1.4.2.exe'), '8')]
        with windows_case_folding():
            self.assertEqual([item.pid for item in processes.family(install, rows)], [7])

    def test_resolved_package_roots_fold_case_like_windows(self):
        # resolve() reports the on-disk spelling, which need not match the folders CCS names.
        for app_id, folder, on_disk in (
            ('claude-code', '.local/share/claude/versions', '.local/share/Claude/Versions'),
            ('codex-cli', '.codex/packages/standalone/releases', '.codex/Packages/Standalone/Releases'),
        ):
            with self.subTest(app_id=app_id):
                name = updater.CLI_NAMES[app_id] + '.exe'
                binary = _fixture_file(self.home / on_disk / '1.0.0' / name)
                link = self.home / '.local/bin' / name
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to(binary)
                with windows_case_folding(), mock.patch.object(updater, '_candidates', return_value=[link]), mock.patch.object(updater, 'cli_probe', return_value=('1.0.0', None)):
                    install = updater.detect_cli(app_id, 'windows')
                self.assertEqual(install.package_root, self.home / folder)


class WindowsMuseUpdateTests(unittest.TestCase):
    """A live launcher lock reports busy, and AAC never stops or restarts Windows Muse."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix='ccs-update-muse-')
        self.addCleanup(directory.cleanup)
        self.root = pathlib.Path(directory.name).resolve()
        self.folder, self.lock = self.root / 'Programs/muse', self.root / 'Programs/muse/.muse-update-lock'
        patcher = mock.patch.object(pathlib.Path, 'home', return_value=self.root / 'Home')
        patcher.start()
        self.addCleanup(patcher.stop)
        self.install = common.Install('muse-code', 'windows', self.folder / 'muse.cmd', '1.4.2', 'native')
        # T3's muse-acp adapter runs `muse serve` from this folder as muse-bin-<version>.exe.
        self.host = processes.Process(77, 1, 1, str(self.folder / 'muse-bin-1.4.2.exe'), '77', ['muse-bin-1.4.2.exe', 'serve'], session=1)

    @staticmethod
    def refuse_running_instances(platform, contexts):
        """Stands in for check_terminal, which refuses any running instance it could not restart."""
        if contexts:
            raise common.UpdateFailure('restart_context')

    def test_muse_update_busy_is_false_without_a_lock(self):
        with mock.patch.object(updater, '_pid_alive', return_value=True) as alive:
            self.assertFalse(updater.muse_update_busy(self.folder))
        alive.assert_not_called()

    def test_muse_update_busy_is_false_for_a_dead_holder(self):
        _fixture_file(self.lock / 'pid', '4242')
        with mock.patch.object(updater, '_pid_alive', return_value=False) as alive:
            self.assertFalse(updater.muse_update_busy(self.folder))
        alive.assert_called_once_with(4242)

    def test_muse_update_busy_rejects_garbage_and_oversized_pid_files(self):
        pid = self.lock / 'pid'
        pid.parent.mkdir(parents=True, exist_ok=True)
        for raw in (b'', b'abc', b'0', b'-7', b'12.5', b'4242 4243', b'9' * 10, bytes([195, 169]), b'1' * 100000):
            with self.subTest(raw=raw[:12]):
                pid.write_bytes(raw)
                with mock.patch.object(updater, '_pid_alive', return_value=True) as alive:
                    self.assertFalse(updater.muse_update_busy(self.folder))
                alive.assert_not_called()

    def test_muse_update_busy_is_true_for_a_live_holder(self):
        _fixture_file(self.lock / 'pid', '4242\r\n')
        with mock.patch.object(updater, '_pid_alive', return_value=True) as alive:
            self.assertTrue(updater.muse_update_busy(self.folder))
        alive.assert_called_once_with(4242)

    def test_windows_muse_same_version_while_its_launcher_updates_reports_busy(self):
        # Meta's installer exits 0 without updating while the launcher's live updater holds the lock.
        _fixture_file(self.lock / 'pid', '4242')
        with mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'perform_cli_update') as installer, mock.patch.object(updater, 'detect_cli', return_value=self.install), \
                mock.patch.object(updater, '_pid_alive', return_value=True) as alive:
            value = updater.update_cli(self.install, time.monotonic() + 60)
        installer.assert_called_once()
        alive.assert_called_once_with(4242)
        self.assertEqual((value['status'], value['messageCode'], value['updateAttempted']), ('failed', 'busy', True))
        self.assertTrue((self.lock / 'pid').is_file())  # the launcher owns the lock; AAC never removes it

    def test_windows_muse_same_version_without_a_lock_stays_current(self):
        with mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'cli_contexts', return_value=([], [])), mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'perform_cli_update'), mock.patch.object(updater, 'detect_cli', return_value=self.install), \
                mock.patch.object(updater, '_pid_alive', return_value=True) as alive:
            value = updater.update_cli(self.install, time.monotonic() + 60)
        alive.assert_not_called()
        self.assertEqual((value['status'], value['messageCode'], value['updateAttempted']), ('current', 'current', True))

    def test_windows_muse_update_never_stops_or_restarts_its_running_binary(self):
        # The installer writes the new muse-bin beside the running one; its host keeps the old version.
        refreshed = common.Install('muse-code', 'windows', self.folder / 'muse.cmd', '1.5.0', 'native')
        with mock.patch.object(updater, 'scan', return_value=[self.host]), mock.patch.object(updater, 'cli_contexts', return_value=([self.host], [self.host])), \
                mock.patch.object(updater, 'check_terminal', side_effect=self.refuse_running_instances), mock.patch.object(updater, 'perform_cli_update') as installer, \
                mock.patch.object(updater, 'detect_cli', return_value=refreshed), mock.patch.object(updater, 'terminate_cli') as stop, \
                mock.patch.object(updater, 'restart_cli', return_value=[]) as restart:
            value = updater.update_cli(self.install, time.monotonic() + 60)
        installer.assert_called_once()
        stop.assert_not_called()
        restart.assert_called_once_with(refreshed, [])  # restart_cli is only ever handed no processes
        self.assertEqual((value['status'], value['version'], value['restartedProcesses']), ('updated', '1.5.0', 0))

    def test_windows_muse_readiness_never_judges_its_running_binary(self):
        with mock.patch.object(updater, 'scan', return_value=[self.host]), mock.patch.object(updater, 'cli_contexts', return_value=([self.host], [self.host])), \
                mock.patch.object(updater, 'check_terminal', side_effect=self.refuse_running_instances):
            self.assertIsNone(updater.check_readiness(self.install))


def _script(path, body):
    """A fixture executable (never a real app)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('#!/bin/sh\n' + body + '\n')
    path.chmod(0o755)
    return path


class BoundedUpdateTests(unittest.TestCase):
    """Update all can never hang: every probe and step ends at its timeout."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='ccs-update-bounded-')
        self.root = pathlib.Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def test_command_kills_a_hanging_child_and_its_helpers_at_the_timeout(self):
        marker = self.root / 'grandchild.pid'
        hang = ('import subprocess, sys, time\n'
                'child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])\n'
                'open(sys.argv[1], "w").write(str(child.pid))\n'
                'time.sleep(60)\n')
        started = time.monotonic()
        with self.assertRaises(common.UpdateFailure) as raised:
            common.command([sys.executable, '-c', hang, marker], timeout=1, capture=True)
        self.assertEqual(raised.exception.code, 'timeout')
        self.assertLess(time.monotonic() - started, 8)
        grandchild = int(marker.read_text())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                os.kill(grandchild, 0)
            except ProcessLookupError:
                break
            # A killed child stays a zombie until its (also killed) parent is reaped.
            try:
                if pathlib.Path('/proc/%d/stat' % grandchild).read_text().split(')')[-1].split()[0] == 'Z':
                    break
            except OSError:
                break
            time.sleep(.05)
        else:
            self.fail('the timed-out command left its helper process running')

    def test_short_timeouts_report_timeout_not_a_generic_failure(self):
        with self.assertRaises(common.UpdateFailure) as raised:
            common.command([sys.executable, '-c', 'import time; time.sleep(30)'], timeout=0.5)
        self.assertEqual(raised.exception.code, 'timeout')

    def test_commands_get_no_terminal_and_empty_stdin(self):
        probe = ('import sys\n'
                 'try:\n    open("/dev/tty").close(); tty = "tty"\n'
                 'except OSError:\n    tty = "no-tty"\n'
                 'print(tty, "eof" if sys.stdin.read() == "" else "input")\n')
        started = time.monotonic()
        text = common.command([sys.executable, '-c', probe], timeout=10, capture=True)
        self.assertEqual(text.split(), ['no-tty', 'eof'])
        self.assertLess(time.monotonic() - started, 5)

    def test_hanging_version_probe_reports_check_timed_out_without_updating(self):
        home = self.root
        _script(home / '.local/bin/omp', 'sleep 60')
        _script(home / '.local/bin/claude', 'echo 2.1.0')
        events = []
        def fake_update(install, deadline):
            return common.result(install.app_id, 'ubuntu', 'current', install.version, install.version, 'native', attempted=True)
        # Only the fixture home is searched: a real CLI on this machine is never probed or updated.
        with mock.patch.object(pathlib.Path, 'home', return_value=home), \
                mock.patch.object(updater, '_candidates', side_effect=lambda name, platform: [home / '.local/bin' / name]), \
                mock.patch.object(common, 'VERSION_PROBE_TIMEOUT', 1), \
                mock.patch.object(updater, 'detect_desktop', return_value=None), \
                mock.patch.object(updater, 'check_readiness', return_value=None), \
                mock.patch.object(updater, 'update_desktop') as desktop_update, \
                mock.patch.object(updater, 'update_cli', side_effect=fake_update) as update:
            started = time.monotonic()
            value = updater.run_apply('ubuntu', emit=events.append)
            elapsed = time.monotonic() - started
        rows = {row['appId']: row for row in value['results']}
        self.assertEqual(rows['omp']['status'], 'unknown')
        self.assertEqual(rows['omp']['messageCode'], 'check_timeout')
        self.assertFalse(rows['omp']['updateAttempted'])
        self.assertEqual(rows['claude-code']['status'], 'current')
        self.assertEqual([call.args[0].app_id for call in update.call_args_list], ['claude-code'])
        desktop_update.assert_not_called()
        self.assertLess(elapsed, 8)
        streamed = [event['result']['appId'] for event in events if event['event'] == 'result']
        self.assertEqual(sorted(streamed), sorted(common.APP_LABELS))

    def test_version_probes_run_side_by_side(self):
        home = self.root
        for name in ('agy', 'muse', 'omp', 'codex', 'claude'):
            _script(home / '.local/bin' / name, 'sleep 1; echo 1.0.0')
        with mock.patch.object(pathlib.Path, 'home', return_value=home), \
                mock.patch.object(updater, '_candidates', side_effect=lambda name, platform: [home / '.local/bin' / name]), \
                mock.patch.object(updater, 'detect_desktop', return_value=None):
            started = time.monotonic()
            found = updater.detect('ubuntu')
            elapsed = time.monotonic() - started
        self.assertEqual({key: value.version for key, value in found.items() if value}, {
            'antigravity-cli': '1.0.0', 'muse-code': '1.0.0', 'omp': '1.0.0', 'codex-cli': '1.0.0', 'claude-code': '1.0.0'})
        self.assertLess(elapsed, 3.5, 'five 1 s probes in series would take 5 s')

    def test_windows_package_query_timeout_is_not_not_installed(self):
        with mock.patch.object(desktop, 'powershell', side_effect=common.UpdateFailure('timeout')):
            install = desktop.windows_package('codex-desktop')
        self.assertIsNotNone(install)
        self.assertEqual(install.probe, 'timeout')
        with mock.patch.object(desktop, 'powershell', side_effect=common.UpdateFailure('update_failed')):
            self.assertIsNone(desktop.windows_package('codex-desktop'))

    def test_cancel_skips_every_app_not_yet_started(self):
        installations = {key: common.Install(key, 'ubuntu', pathlib.Path('/fixture/app'), '1.0.0') for key in common.APP_LABELS}
        state = {'cancelled': False}
        def cli(install, deadline):
            state['cancelled'] = True  # the cancel lands while this first app runs
            return common.result(install.app_id, 'ubuntu', 'current', '1.0.0', '1.0.0', 'native', attempted=True)
        with mock.patch.object(pathlib.Path, 'home', return_value=self.root), \
                mock.patch.object(updater, 'detect', return_value=installations), \
                mock.patch.object(updater, 'check_readiness', return_value=None), \
                mock.patch.object(updater, 'update_desktop') as desktop_update, \
                mock.patch.object(updater, 'update_cli', side_effect=cli) as update, \
                mock.patch.object(updater, 'antigravity_hold', return_value=None):
            value = updater.run_apply('ubuntu', cancelled=lambda: state['cancelled'])
        self.assertEqual(update.call_count, 1)
        desktop_update.assert_not_called()
        rows = value['results']
        self.assertEqual(rows[0]['status'], 'current')
        self.assertEqual({row['status'] for row in rows[1:]}, {'skipped'})
        self.assertEqual({row['messageCode'] for row in rows[1:]}, {'skipped_cancelled'})
        self.assertEqual(len(rows), len(common.APP_LABELS))

    def test_progress_streams_json_lines_and_hears_cancel_on_stdin(self):
        output = io.StringIO()
        read_end, write_end = os.pipe()
        with open(read_end, 'rb', buffering=0) as stdin, mock.patch.object(sys, 'stdin', stdin), contextlib.redirect_stdout(output):
            emit, cancelled = updater.stream_progress()
            emit({'event': 'app', 'appId': 'omp', 'phase': 'checking'})
            os.write(write_end, b'noise\n')
            time.sleep(.1)
            self.assertFalse(cancelled())
            os.write(write_end, b'cancel\n')
            deadline = time.monotonic() + 2
            while not cancelled() and time.monotonic() < deadline:
                time.sleep(.01)
            os.close(write_end)
        self.assertTrue(cancelled())
        self.assertEqual(json.loads(output.getvalue().splitlines()[0]), {'event': 'app', 'appId': 'omp', 'phase': 'checking'})

    def test_progress_is_opt_in_so_older_dashboards_get_one_document(self):
        installations = {key: None for key in common.APP_LABELS}
        output = io.StringIO()
        environment = {key: value for key, value in os.environ.items() if key != 'AAC_UPDATE_PROGRESS'}
        with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(pathlib.Path, 'home', return_value=self.root), \
                mock.patch.object(updater, 'detect', return_value=installations), \
                mock.patch.object(sys, 'argv', ['helper', '--apply', '--platform', 'ubuntu']), contextlib.redirect_stdout(output):
            updater.main()
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertEqual(len(json.loads(lines[0])['results']), len(common.APP_LABELS))
        output = io.StringIO()
        devnull = open(os.devnull, 'rb')
        self.addCleanup(devnull.close)
        with mock.patch.dict(os.environ, {'AAC_UPDATE_PROGRESS': '1'}), mock.patch.object(pathlib.Path, 'home', return_value=self.root), \
                mock.patch.object(updater, 'detect', return_value=installations), mock.patch.object(sys, 'stdin', devnull), \
                mock.patch.object(sys, 'argv', ['helper', '--apply', '--platform', 'ubuntu']), contextlib.redirect_stdout(output):
            updater.main()
        lines = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual([line.get('event') for line in lines[:-1]], ['app'] + ['result'] * len(common.APP_LABELS))
        self.assertEqual(len(lines[-1]['results']), len(common.APP_LABELS))

    def test_windows_coordinator_relays_task_progress_and_forwards_cancel(self):
        import threading
        app_root = self.root / '.ccs/app-updates'
        app_root.mkdir(parents=True)
        common.write_private_json(app_root / 'windows-task-progress.json', {'nonce': 'stale', 'events': [{'event': 'app', 'appId': 'omp', 'phase': 'checking'}]})
        events, state = [], {'cancel': False}
        def task_child():
            deadline = time.monotonic() + 10
            nonce = None
            while nonce is None and time.monotonic() < deadline:
                try: nonce = json.loads((app_root / 'windows-task-request.json').read_text())['nonce']
                except (OSError, ValueError): time.sleep(.05)
            emit, cancelled = updater.task_child_progress(nonce)
            emit({'event': 'app', 'appId': 'codex-cli', 'phase': 'updating'})
            emit({'event': 'result', 'result': {'appId': 'codex-cli', 'status': 'current', 'messageCode': 'current'}})
            state['cancel'] = True
            while not cancelled() and time.monotonic() < deadline:
                time.sleep(.05)
            common.write_private_json(app_root / 'windows-task-result.json', {'nonce': nonce, 'results': [{'appId': 'codex-cli', 'status': 'current'}, {'appId': 'omp', 'status': 'skipped'}]})
        with mock.patch.object(pathlib.Path, 'home', return_value=self.root), mock.patch.object(updater, 'powershell', return_value='') as shell:
            worker = threading.Thread(target=task_child)
            worker.start()
            value = updater.windows_interactive_apply(events.append, lambda: state['cancel'])
            worker.join(5)
        self.assertEqual(shell.call_count, 2)
        self.assertEqual([event['event'] for event in events], ['app', 'result'])
        self.assertEqual(events[0]['appId'], 'codex-cli', 'stale progress from an earlier nonce is never relayed')
        self.assertEqual(value['results'][1]['status'], 'skipped')

    def test_ubuntu_codex_busy_row_passes_through_without_stopping_anything(self):
        install = common.Install('codex-cli', 'ubuntu', pathlib.Path('/fixture/codex'), '1.0.0')
        daemon = processes.Process(10, 1, 1, '/fixture/codex', '10', ['codex', 'app-server', '--listen', 'unix://'])
        busy = {'appId': 'codex-cli', 'platform': 'ubuntu', 'status': 'action_required', 'previousVersion': '1.0.0',
                'version': '1.1.0', 'manager': 'native', 'messageCode': 'codex_busy', 'updateAttempted': True,
                'restartedProcesses': 0, 'forcedStops': 0}
        with mock.patch.object(pathlib.Path, 'home', return_value=self.root), \
                mock.patch.object(updater, 'scan', return_value=[daemon]), \
                mock.patch.object(updater, 'family', return_value=[daemon]), \
                mock.patch.object(updater, 'cli_contexts', return_value=([], [])), \
                mock.patch.object(updater, 'check_terminal'), \
                mock.patch.object(updater, 'command', return_value=json.dumps(busy)) as run, \
                mock.patch.object(updater, 'terminate_cli') as stop, mock.patch.object(updater, 'restart_cli') as restart:
            value = updater.update_cli(install, time.monotonic() + 15 * 60)
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'codex_busy')
        stop.assert_not_called(); restart.assert_not_called()
        # The bridge gets a bounded budget, never the old 15-minute idle wait.
        argv, timeout = run.call_args.args[0], run.call_args.kwargs['timeout']
        self.assertLessEqual(int(argv[argv.index('--timeout-seconds') + 1]), updater.CODEX_BRIDGE_SECONDS)
        self.assertLessEqual(timeout, updater.CODEX_BRIDGE_SECONDS + 15)


class WindowsConsoleTests(unittest.TestCase):
    """pythonw has no console, so each console child needs CREATE_NO_WINDOW or it opens its own window."""

    @staticmethod
    def popen_options(os_name, launch):
        """The keyword options Popen received for the child that launch() starts."""
        child = mock.Mock(returncode=0, pid=1)
        child.communicate.return_value = (b'', None)
        with mock.patch.object(common.os, 'name', os_name), mock.patch.object(common.subprocess, 'Popen', return_value=child) as popen:
            launch()
        return popen.call_args.kwargs

    def test_windows_children_start_without_a_console_window(self):
        options = self.popen_options('nt', lambda: common.command(['codex.exe', '--version'], capture=True))
        self.assertEqual(options['creationflags'], 0x08000000)  # CREATE_NO_WINDOW
        self.assertNotIn('startupinfo', options)  # never SW_HIDE: relaunched GUI apps must still show
        options = self.popen_options('nt', lambda: common.powershell('Get-Date'))
        self.assertEqual(options['creationflags'], 0x08000000)

    def test_visible_launch_keeps_the_default_console(self):
        options = self.popen_options('nt', lambda: common.command(['wt.exe', '-w', '0'], timeout=15, visible=True))
        self.assertNotIn('creationflags', options)
        self.assertNotIn('startupinfo', options)

    def test_posix_children_keep_their_own_session_and_no_console_flags(self):
        options = self.popen_options('posix', lambda: common.command(['tool', '--version']))
        self.assertTrue(options['start_new_session'])
        self.assertNotIn('creationflags', options)
        options = self.popen_options('posix', lambda: common.command(['tool'], visible=True))
        self.assertTrue(options['start_new_session'])
        self.assertNotIn('creationflags', options)

    def test_windows_terminal_tab_for_a_relaunched_cli_is_visible(self):
        install = common.Install('omp', 'windows', pathlib.Path('C:/omp/omp.exe'))
        context = processes.Process(1, 0, 1, 'C:/omp/omp.exe', '1', cwd='C:/work', env={'CCS_TEST': '1'})
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), \
                mock.patch.object(pathlib.Path, 'is_file', return_value=True), \
                mock.patch.object(terminal.shutil, 'which', return_value='C:/wt.exe'), \
                mock.patch.object(pipes, 'PrivatePipe') as pipe, \
                mock.patch.object(terminal, 'command') as launch:
            pipe.return_value.endpoint = 'fixture-endpoint'
            terminal._broker_terminal(install, context, [])
        self.assertEqual(launch.call_args.args[0][:4], ['C:/wt.exe', '-w', '0', 'new-tab'])
        self.assertIs(launch.call_args.kwargs['visible'], True)


class _FakeRangeServer:
    """urlopen stand-in that serves one in-memory file by HTTP byte range (or ignores ranges)."""

    def __init__(self, payload, honour_ranges=True):
        self.payload, self.honour_ranges, self.requests, self.bytes = payload, honour_ranges, 0, 0

    def __call__(self, request, timeout=None):
        self.requests += 1
        value = request.get_header('Range')
        size = len(self.payload)
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.geturl.return_value = 'https://fixture.test/final.msix'
        if not self.honour_ranges or not value:
            response.status = 200
            response.headers = {'Content-Length': str(size)}
            response.read.side_effect = lambda limit=-1: self.payload[:limit]
            return response
        spec = value.split('=', 1)[1]
        if spec.startswith('-'):
            first, last = max(0, size - int(spec[1:])), size - 1
        else:
            first, last = (int(part) for part in spec.split('-'))
            last = min(last, size - 1)
        chunk = self.payload[first:last + 1]
        self.bytes += len(chunk)
        response.status = 206
        response.headers = {'Content-Range': 'bytes %d-%d/%d' % (first, last, size)}
        response.read.side_effect = lambda limit=-1: chunk[:limit]
        return response


def _fake_msix(version, name='OpenAI.Codex', publisher='CN=fixture', filler=3 * 1024 * 1024):
    """A fixture MSIX: a manifest plus incompressible bulk, so a full download is clearly avoidable."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w', zipfile.ZIP_DEFLATED) as value:
        value.writestr('app/resources/app.asar', os.urandom(filler))
        value.writestr('AppxManifest.xml', '<Package><Identity Name="%s" Publisher="%s" Version="%s" ProcessorArchitecture="x64"/></Package>' % (name, publisher, version))
    return buffer.getvalue()


REAL_REMOTE_MSIX_IDENTITY = desktop.remote_msix_identity
REAL_CODEX_STORE_VERSION = desktop.codex_store_version


class DesktopWaitTests(unittest.TestCase):
    """A running Codex or Claude desktop app is never quit, closed, restarted or killed.

    Its update reports "quit first" within seconds instead of after a full
    package download plus a close request (the 2026-10-06 five-minute wait).
    Every process scan, network call and installer here is a fixture.
    """

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='ccs-desktop-wait-')
        self.root = pathlib.Path(self.directory.name)
        patches = [
            mock.patch.object(pathlib.Path, 'home', return_value=self.root),
            mock.patch.object(desktop, 'scan', return_value=[]),
            mock.patch.object(desktop, 'remote_fingerprint', return_value=None),
            mock.patch.object(desktop, 'remote_msix_identity', return_value=None),
            mock.patch.object(desktop, 'codex_store_version', return_value=None),
            # Any attempt to stop, close or relaunch a desktop app fails the test.
            mock.patch.object(desktop, 'terminate_desktops', side_effect=AssertionError('desktop app stopped')),
            mock.patch.object(desktop, 'restart_desktops', side_effect=AssertionError('desktop app relaunched')),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        self.addCleanup(self.directory.cleanup)

    def _fake_mac_bundle(self, root, name, identity, version):
        app = root / name
        info = app / 'Contents/Info.plist'
        info.parent.mkdir(parents=True)
        import plistlib
        with info.open('wb') as handle:
            plistlib.dump({'CFBundleIdentifier': identity, 'CFBundleShortVersionString': version}, handle)
        return app

    def _windows_codex(self):
        return common.Install('codex-desktop', 'windows', pathlib.Path('/fixture/ChatGPT.exe'), '26.930.3748.0', 'msix', 'OpenAI.Codex', 'CN=fixture', pathlib.Path('/fixture'))

    def _assert_quit_first(self, value, version):
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'quit_first')
        self.assertFalse(value['updateAttempted'])
        self.assertEqual(value['version'], version)

    # ---------------------------------------------------------------- Windows Codex via the Microsoft Store
    def test_codex_store_feed_is_read_only_for_its_own_package(self):
        install = self._windows_codex()
        def feed(body):
            def write(url, target, **_kwargs):
                self.assertEqual(url, desktop.CODEX_STORE_FEED)
                pathlib.Path(target).write_text(body, encoding='utf-8')
            return write
        good = '{"schemaVersion":1,"buildVersion":"26.1002.7124.0","storeProductId":"9PLM9XGG6VKS","packageIdentity":"OpenAI.Codex"}'
        foreign = '{"buildVersion":"99.0.0.0","storeProductId":"9PLM9XGG6VKS","packageIdentity":"Impostor"}'
        with mock.patch.object(desktop, 'download', side_effect=feed(good)):
            self.assertEqual(REAL_CODEX_STORE_VERSION(install), '26.1002.7124.0')
        with mock.patch.object(desktop, 'download', side_effect=feed(foreign)):
            self.assertIsNone(REAL_CODEX_STORE_VERSION(install))
        with mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure()):
            self.assertIsNone(REAL_CODEX_STORE_VERSION(install))

    def test_windows_running_codex_with_newer_store_build_reports_quit_first(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'codex_store_version', return_value='26.1002.7124.0'), \
                mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'command', side_effect=AssertionError('winget ran')):
            self._assert_quit_first(desktop.update_windows(install), '26.930.3748.0')

    def test_windows_codex_store_build_installs_through_winget_without_closing(self):
        install = self._windows_codex()
        refreshed = common.Install('codex-desktop', 'windows', pathlib.Path('/fixture/ChatGPT.exe'), '26.1002.7124.0', 'msix', 'OpenAI.Codex', 'CN=fixture', pathlib.Path('/fixture'))
        with mock.patch.object(desktop, 'codex_store_version', return_value='26.1002.7124.0'), \
                mock.patch.object(desktop, 'desktop_running', return_value=False), \
                mock.patch.object(desktop.shutil, 'which', return_value='C:/winget.exe'), \
                mock.patch.object(desktop, 'command') as run, mock.patch.object(desktop, 'windows_package', return_value=refreshed), \
                mock.patch.object(desktop, 'download_desktop', side_effect=AssertionError('stale MSIX downloaded')):
            value = desktop.update_windows(install)
        self.assertEqual((value['status'], value['version'], value['forcedStops']), ('updated', '26.1002.7124.0', 0))
        argv = run.call_args.args[0]
        self.assertEqual(argv[:6], ['C:/winget.exe', 'install', '--id', '9PLM9XGG6VKS', '--source', 'msstore'])

    def test_windows_codex_store_install_that_changes_nothing_is_not_updated(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'codex_store_version', return_value='26.1002.7124.0'), \
                mock.patch.object(desktop, 'desktop_running', return_value=False), \
                mock.patch.object(desktop.shutil, 'which', return_value='C:/winget.exe'), \
                mock.patch.object(desktop, 'command'), mock.patch.object(desktop, 'windows_package', return_value=install):
            value = desktop.update_windows(install)
        self.assertEqual((value['status'], value['messageCode']), ('failed', 'version_unknown'))

    # ---------------------------------------------------------------- the quit request is gone
    def test_updater_has_no_way_to_ask_a_desktop_app_to_quit(self):
        for name in ('request_desktop_quit', 'mac_quit_request', 'windows_close_broadcast'):
            self.assertFalse(hasattr(processes, name), name)
            self.assertFalse(hasattr(desktop, name), name)

    def test_running_is_any_process_of_the_app_family(self):
        install = self._windows_codex()
        running = processes.Process(31, 1, 1, '/fixture/ChatGPT.exe', '31', ['/fixture/ChatGPT.exe'])
        with mock.patch.object(desktop, 'scan', return_value=[running]):
            self.assertTrue(desktop.desktop_running(install))
        self.assertFalse(desktop.desktop_running(install))

    # ---------------------------------------------------------------- Windows (MSIX)
    def test_windows_running_codex_with_newer_manifest_reports_quit_first_at_once(self):
        install = self._windows_codex()
        events = []
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'remote_msix_identity', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'download') as fetch, \
                mock.patch.object(desktop, 'add_appx_package') as deploy:
            started = time.monotonic()
            value = desktop.update_desktop(install, phase=events.append)
            elapsed = time.monotonic() - started
        self._assert_quit_first(value, '26.930.3748.0')
        fetch.assert_not_called()
        deploy.assert_not_called()
        self.assertEqual(events, [], 'no download phase: the answer came from the manifest alone')
        self.assertLess(elapsed, 2)

    def test_windows_current_manifest_skips_the_package_download(self):
        install = common.Install('claude-desktop', 'windows', pathlib.Path('/fixture/Claude.exe'), '2.19675.1.0', 'msix', 'Claude', 'CN=fixture', pathlib.Path('/fixture'))
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'remote_msix_identity', return_value={'Name': 'Claude', 'Publisher': 'CN=fixture', 'Version': '2.19675.1.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'download') as fetch:
            value = desktop.update_desktop(install)
        self.assertEqual(value['status'], 'current')
        fetch.assert_not_called()

    def test_windows_unknown_manifest_downloads_live_then_reports_quit_first(self):
        install = self._windows_codex()
        events = []
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'download') as fetch, \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'add_appx_package') as deploy:
            value = desktop.update_desktop(install, phase=events.append)
        self._assert_quit_first(value, '26.930.3748.0')
        fetch.assert_called_once()
        deploy.assert_not_called()
        self.assertEqual(events, ['downloading', 'updating'])

    def test_windows_app_opened_during_the_download_is_left_alone(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'desktop_running', side_effect=[False, True]), \
                mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'add_appx_package') as deploy:
            value = desktop.update_desktop(install)
        self._assert_quit_first(value, '26.930.3748.0')
        deploy.assert_not_called()

    def test_windows_deploy_needing_close_reports_quit_first(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'add_appx_package', side_effect=common.UpdateFailure('quit_first')):
            value = desktop.update_desktop(install)
        self._assert_quit_first(value, '26.930.3748.0')

    def test_windows_not_running_installs_without_relaunching_anything(self):
        install = self._windows_codex()
        refreshed = common.Install('codex-desktop', 'windows', pathlib.Path('/fixture/ChatGPT.exe'), '26.930.7945.0', 'msix', 'OpenAI.Codex', 'CN=fixture', pathlib.Path('/fixture'))
        with mock.patch.object(desktop, 'remote_msix_identity', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'OpenAI.Codex', 'Publisher': 'CN=fixture', 'Version': '26.930.7945.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'add_appx_package') as deploy, \
                mock.patch.object(desktop, 'windows_package', return_value=refreshed):
            value = desktop.update_desktop(install)
        deploy.assert_called_once()
        self.assertEqual(value['status'], 'updated')
        self.assertEqual(value['version'], '26.930.7945.0')
        self.assertEqual(value['restartedProcesses'], 0)
        self.assertEqual(value['forcedStops'], 0)

    def test_windows_identity_mismatch_never_installs(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'download'), \
                mock.patch.object(desktop, 'msix_info', return_value={'Name': 'Impostor', 'Publisher': 'Other', 'Version': '99.0.0.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'add_appx_package') as deploy:
            value = desktop.update_desktop(install)
        self.assertEqual(value['messageCode'], 'signature_failed')
        deploy.assert_not_called()

    def test_windows_impostor_manifest_does_not_decide_current(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'remote_msix_identity', return_value={'Name': 'Impostor', 'Publisher': 'Other', 'Version': '1.0.0.0', 'ProcessorArchitecture': 'x64'}), \
                mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure('timeout')) as fetch:
            value = desktop.update_desktop(install)
        fetch.assert_called_once()
        self.assertEqual(value['messageCode'], 'timeout')

    def test_windows_process_scan_timeout_reports_its_code_before_any_download(self):
        install = self._windows_codex()
        with mock.patch.object(desktop, 'desktop_running', side_effect=common.UpdateFailure('timeout')), \
                mock.patch.object(desktop, 'download') as fetch:
            value = desktop.update_desktop(install)
        fetch.assert_not_called()
        self.assertEqual(value['status'], 'failed')
        self.assertEqual(value['messageCode'], 'timeout')

    # ---------------------------------------------------------------- reading only the manifest
    def test_remote_manifest_reads_a_sliver_of_the_package(self):
        payload = _fake_msix('26.930.7945.0')
        server = _FakeRangeServer(payload)
        with mock.patch.object(common.urllib.request, 'urlopen', side_effect=server):
            info = REAL_REMOTE_MSIX_IDENTITY('https://fixture.test/app.msix')
        self.assertEqual(info['Version'], '26.930.7945.0')
        self.assertEqual(info['Name'], 'OpenAI.Codex')
        self.assertLessEqual(server.requests, 4)
        self.assertLess(server.bytes, len(payload) // 4)

    def test_remote_manifest_unavailable_is_none_not_an_error(self):
        server = _FakeRangeServer(_fake_msix('26.930.7945.0', filler=1024), honour_ranges=False)
        with mock.patch.object(common.urllib.request, 'urlopen', side_effect=server):
            self.assertIsNone(REAL_REMOTE_MSIX_IDENTITY('https://fixture.test/app.msix'))

    def test_server_ignoring_ranges_falls_back_to_the_full_check(self):
        server = _FakeRangeServer(_fake_msix('26.930.7945.0', filler=1024), honour_ranges=False)
        with mock.patch.object(common.urllib.request, 'urlopen', side_effect=server):
            with self.assertRaises(common.UpdateFailure):
                common.RangeReader('https://fixture.test/app.msix')

    def test_range_reader_stops_at_its_byte_budget(self):
        server = _FakeRangeServer(_fake_msix('1.0.0.0', filler=4 * 1024 * 1024))
        with mock.patch.object(common.urllib.request, 'urlopen', side_effect=server):
            reader = common.RangeReader('https://fixture.test/app.msix', maximum_bytes=128 * 1024)
            reader.seek(0)
            with self.assertRaises(common.UpdateFailure):
                reader.read(1024 * 1024)

    # ---------------------------------------------------------------- Mac
    def test_mac_running_claude_with_newer_feed_reports_quit_first_without_package_download(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        feed = {'currentRelease': '2.19700.0', 'releases': [{'updateTo': {'version': '2.19700.0', 'url': desktop.CLAUDE_DARWIN_PREFIX + '2.19700.0/Claude.zip'}}]}
        fetched = []
        def download(url, destination, **kwargs):
            fetched.append(url)
            pathlib.Path(destination).write_text(json.dumps(feed))
        events = []
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'download', side_effect=download), \
                mock.patch.object(desktop, 'command') as run:
            started = time.monotonic()
            value = desktop.update_desktop(install, phase=events.append)
            elapsed = time.monotonic() - started
        self._assert_quit_first(value, '2.19675.0')
        self.assertEqual(fetched, [desktop.CLAUDE_DARWIN_FEED])
        run.assert_not_called()
        self.assertEqual(events, [])
        self.assertLess(elapsed, 2)

    def test_mac_running_codex_with_remembered_newer_dmg_reports_quit_first_without_download(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'ChatGPT.app', 'com.openai.codex', '26.930.41038')
        install = common.Install('codex-desktop', 'mac', installed, '26.930.41038', 'official-download', 'com.openai.codex', package_root=installed)
        fingerprint = {'url': desktop.MAC['codex-desktop'][3], 'size': 750982223, 'validator': '"0xFIXTURE"'}
        common.write_private_json(desktop.package_memory(install), {**fingerprint, 'version': '26.930.61225'})
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'remote_fingerprint', return_value=fingerprint), \
                mock.patch.object(desktop, 'dmg_candidates') as fetch:
            value = desktop.update_desktop(install)
        self._assert_quit_first(value, '26.930.41038')
        fetch.assert_not_called()

    def test_mac_codex_remembered_current_dmg_skips_download(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'ChatGPT.app', 'com.openai.codex', '26.930.61225')
        install = common.Install('codex-desktop', 'mac', installed, '26.930.61225', 'official-download', 'com.openai.codex', package_root=installed)
        fingerprint = {'url': desktop.MAC['codex-desktop'][3], 'size': 750982223, 'validator': '"0xFIXTURE"'}
        common.write_private_json(desktop.package_memory(install), {**fingerprint, 'version': '26.930.61225'})
        with mock.patch.object(desktop, 'remote_fingerprint', return_value=fingerprint), \
                mock.patch.object(desktop, 'dmg_candidates') as fetch:
            value = desktop.update_desktop(install)
        self.assertEqual(value['status'], 'current')
        fetch.assert_not_called()

    def test_mac_running_codex_with_unknown_dmg_downloads_live_then_reports_quit_first_without_swapping(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'ChatGPT.app', 'com.openai.codex', '26.928.31416')
        install = common.Install('codex-desktop', 'mac', installed, '26.928.31416', 'official-download', 'com.openai.codex', package_root=installed)
        candidate = self._fake_mac_bundle(self.root / 'mnt', 'ChatGPT.app', 'com.openai.codex', '26.930.41038')
        (candidate / 'Contents' / 'staged-marker.txt').write_text('staged')
        fingerprint = {'url': desktop.MAC['codex-desktop'][3], 'size': 1, 'validator': '"0xNEW"'}
        events = []
        with mock.patch.object(desktop, 'desktop_running', return_value=True), \
                mock.patch.object(desktop, 'remote_fingerprint', return_value=fingerprint), \
                mock.patch.object(desktop, 'dmg_candidates', return_value=([candidate], None)), \
                mock.patch.object(desktop, 'verify_mac', return_value={'CFBundleIdentifier': 'com.openai.codex', 'CFBundleShortVersionString': '26.930.41038'}):
            value = desktop.update_desktop(install, phase=events.append)
        self._assert_quit_first(value, '26.928.31416')
        self.assertEqual(events, ['downloading', 'updating'])
        self.assertFalse((installed / 'Contents' / 'staged-marker.txt').exists())
        self.assertEqual([path.name for path in (self.root / 'Applications').iterdir()], ['ChatGPT.app'])
        # The next click answers in seconds: the verified version is remembered.
        self.assertEqual(desktop.remembered_dmg_version(install, fingerprint), '26.930.41038')

    def test_mac_app_opened_while_staging_is_never_swapped(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.0.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.0.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        candidate = self._fake_mac_bundle(self.root / 'extracted', 'Claude.app', 'com.anthropic.claudefordesktop', '3.0.0')
        with mock.patch.object(desktop, 'desktop_running', side_effect=[False, False, True]), \
                mock.patch.object(desktop, 'claude_release_url', return_value=desktop.CLAUDE_DARWIN_PREFIX + '3.0.0/Claude.zip'), \
                mock.patch.object(desktop, 'claude_zip_candidates', return_value=[candidate]), \
                mock.patch.object(desktop, 'verify_mac', side_effect=lambda app, app_id: desktop.bundle_info(app)):
            value = desktop.update_desktop(install)
        self._assert_quit_first(value, '2.0.0')
        self.assertEqual(desktop.bundle_info(installed)['CFBundleShortVersionString'], '2.0.0')
        self.assertEqual([path.name for path in (self.root / 'Applications').iterdir()], ['Claude.app'])

    def test_legacy_restart_marker_is_dropped_and_nothing_is_relaunched(self):
        install = common.Install('codex-desktop', 'mac', pathlib.Path('/fixture/ChatGPT.app'), '26.930.41038', 'official-download', 'com.openai.codex', package_root=pathlib.Path('/fixture/ChatGPT.app'))
        marker = desktop.restart_marker(install)
        common.write_private_json(marker, {'version': '26.930.41038'})
        with mock.patch.object(desktop, 'update_mac', return_value={'status': 'current'}) as flow:
            value = desktop.update_desktop(install)
        self.assertFalse(marker.exists())
        flow.assert_called_once_with(install, None)
        self.assertEqual(value, {'status': 'current'})

    def test_mac_download_timeout_keeps_its_code(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        with mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure('timeout')):
            value = desktop.update_mac(install)
        self.assertEqual(value['status'], 'failed')
        self.assertEqual(value['messageCode'], 'timeout')

    def test_dmg_plist_garbage_reports_update_failed(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'ChatGPT.app', 'com.openai.codex', '26.928.31416')
        install = common.Install('codex-desktop', 'mac', installed, '26.928.31416', 'official-download', 'com.openai.codex', package_root=installed)
        def commands(argv, **kwargs):
            if argv[:2] == ['/usr/bin/hdiutil', 'attach']:
                return 'hdiutil: this is not a plist'
            return ''
        with mock.patch.object(desktop, 'download'), mock.patch.object(desktop, 'command', side_effect=commands):
            value = desktop.update_mac(install)
        self.assertEqual(value['status'], 'failed')
        self.assertEqual(value['messageCode'], 'update_failed')

    def test_mac_blocked_download_reports_check_in_app(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        with mock.patch.object(desktop, 'download', side_effect=common.UpdateFailure('download_blocked')), \
                mock.patch.object(desktop.time, 'sleep'):
            value = desktop.update_mac(install)
        self.assertEqual(value['status'], 'action_required')
        self.assertEqual(value['messageCode'], 'check_in_app')
        self.assertFalse(value['updateAttempted'])

    def test_claude_feed_current_skips_package_download(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.19675.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.19675.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        feed = {'currentRelease': '2.19675.0', 'releases': [{'version': '2.19675.0', 'updateTo': {'version': '2.19675.0', 'url': desktop.CLAUDE_DARWIN_PREFIX + '2.19675.0/Claude-x.zip'}}]}
        fetched = []
        def download(url, destination, **kwargs):
            fetched.append((url, kwargs))
            pathlib.Path(destination).write_text(json.dumps(feed))
        with mock.patch.object(desktop, 'download', side_effect=download), mock.patch.object(desktop, 'command') as run:
            value = desktop.update_mac(install)
        self.assertEqual(value['status'], 'current')
        self.assertEqual(fetched, [(desktop.CLAUDE_DARWIN_FEED, {'maximum': 256 * 1024, 'timeout': 60})])
        run.assert_not_called()

    def test_claude_feed_zip_installs_through_ditto_when_not_running(self):
        installed = self._fake_mac_bundle(self.root / 'Applications', 'Claude.app', 'com.anthropic.claudefordesktop', '2.0.0')
        install = common.Install('claude-desktop', 'mac', installed, '2.0.0', 'official-download', 'com.anthropic.claudefordesktop', package_root=installed)
        feed = {'currentRelease': '3.0.0', 'releases': [{'version': '3.0.0', 'updateTo': {'version': '3.0.0', 'url': desktop.CLAUDE_DARWIN_PREFIX + '3.0.0/Claude-y.zip'}}]}
        fetched = []
        def download(url, destination, **kwargs):
            fetched.append((url, kwargs))
            if url == desktop.CLAUDE_DARWIN_FEED:
                pathlib.Path(destination).write_text(json.dumps(feed))
        def commands(argv, **kwargs):
            if argv[0] == '/usr/bin/ditto':
                self._fake_mac_bundle(pathlib.Path(argv[4]), 'Claude.app', 'com.anthropic.claudefordesktop', '3.0.0')
            return ''
        with mock.patch.object(desktop, 'download', side_effect=download), \
                mock.patch.object(desktop, 'command', side_effect=commands), \
                mock.patch.object(desktop, 'verify_mac', side_effect=lambda app, app_id: desktop.bundle_info(app)):
            value = desktop.update_mac(install)
        self.assertEqual(value['status'], 'updated')
        self.assertEqual(value['version'], '3.0.0')
        self.assertEqual(value['restartedProcesses'], 0)
        self.assertEqual(fetched[0], (desktop.CLAUDE_DARWIN_FEED, {'maximum': 256 * 1024, 'timeout': 60}))
        self.assertEqual(fetched[1], (desktop.CLAUDE_DARWIN_PREFIX + '3.0.0/Claude-y.zip', {'maximum': desktop.DESKTOP_DOWNLOAD_MAXIMUM, 'timeout': desktop.DESKTOP_DOWNLOAD_TIMEOUT}))
        self.assertEqual(desktop.bundle_info(installed)['CFBundleShortVersionString'], '3.0.0')
        self.assertEqual([path.name for path in (self.root / 'Applications').iterdir()], ['Claude.app'])

    # ---------------------------------------------------------------- the apply loop
    def test_run_apply_streams_desktop_download_phase(self):
        install = self._windows_codex()
        installations = {key: None for key in common.APP_LABELS}
        installations['codex-desktop'] = install
        events = []
        def flow(value, deadline, phase):
            phase('downloading')
            return common.result(value.app_id, 'windows', 'action_required', value.version, value.version, 'msix', 'quit_first')
        with mock.patch.object(updater, 'detect', return_value=installations), \
                mock.patch.object(updater, 'execution_lock', return_value=contextlib.nullcontext()), \
                mock.patch.object(updater, 'update_desktop', side_effect=flow):
            updater.run_apply('windows', emit=events.append)
        phases = [event['phase'] for event in events if event.get('event') == 'app' and event.get('appId') == 'codex-desktop']
        self.assertEqual(phases, ['checking', 'updating', 'downloading'])


class _Manifest:
    """A fixture HTTP response for the official Antigravity manifest."""
    def __init__(self, body, url=None):
        self.body, self.url = body, url or updater.AGY_MANIFEST_BASE + 'linux_amd64.json'
    def __enter__(self): return self
    def __exit__(self, *args): return False
    def geturl(self): return self.url
    def read(self, limit=-1): return self.body[:limit] if limit >= 0 else self.body


class AntigravityReviewHoldTests(unittest.TestCase):
    """Update all never installs an Antigravity CLI build without a switching review."""

    REVIEWED = frozenset({'1.2.14', '1.2.16'})

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='ccs-agy-hold-')
        self.root = pathlib.Path(self.directory.name)

    def tearDown(self):
        self.directory.cleanup()

    def install(self, version='1.2.16', platform='ubuntu'):
        return common.Install('antigravity-cli', platform, pathlib.Path('/fixture/agy'), version)

    def test_reviewed_list_is_strict(self):
        self.assertEqual(updater.parse_reviewed_versions('1.2.14,1.2.16,1.3.0'), frozenset({'1.2.14', '1.2.16', '1.3.0'}))
        self.assertEqual(updater.parse_reviewed_versions('1.3.0-rc.1'), frozenset({'1.3.0-rc.1'}))
        for hostile in ('', '1.2', '1.2.16,', '1.2.16;id', "1.2.16' ; rm -rf /", '1.2.16,1.2.16', ','.join('1.0.%d' % n for n in range(17)), 'v1.2.16', None):
            self.assertIsNone(updater.parse_reviewed_versions(hostile), hostile)

    def test_release_file_is_read_like_the_dashboard_reads_it(self):
        self.assertIn('1.3.0', updater.read_release_versions())
        release = self.root / 'release.json'
        entry = lambda version, digit: {'nativeVersion': version, 'nativeSha256': digit * 64}
        release.write_text(json.dumps({'reviewedNatives': [entry('1.2.14', 'a'), entry('1.2.16', 'b')]}))
        self.assertEqual(updater.read_release_versions(release), self.REVIEWED)
        release.write_text(json.dumps({'nativeVersion': '1.2.16', 'nativeSha256': 'b' * 64}))
        self.assertEqual(updater.read_release_versions(release), frozenset({'1.2.16'}))
        for broken in ({'reviewedNatives': [entry('1.2.16', 'b'), entry('1.2.16', 'c')]},
                       {'reviewedNatives': [entry('1.2.14', 'b'), entry('1.2.16', 'b')]},
                       {'reviewedNatives': [{**entry('1.2.16', 'b'), 'extra': 1}]},
                       {'reviewedNatives': []}, {'reviewedNatives': 'x'}, [], {'nativeVersion': '1.2'}):
            release.write_text(json.dumps(broken))
            self.assertIsNone(updater.read_release_versions(release), broken)
        self.assertIsNone(updater.read_release_versions(self.root / 'missing.json'))

    def test_manifest_names_match_the_official_installers(self):
        self.assertEqual(updater.agy_manifest_key('ubuntu', 'x86_64'), 'linux_amd64')
        self.assertEqual(updater.agy_manifest_key('ubuntu', 'aarch64'), 'linux_arm64')
        self.assertEqual(updater.agy_manifest_key('mac', 'arm64'), 'darwin_arm64')
        self.assertEqual(updater.agy_manifest_key('mac', 'x86_64'), 'darwin_amd64')
        self.assertEqual(updater.agy_manifest_key('windows', 'AMD64'), 'windows_amd64')
        self.assertIsNone(updater.agy_manifest_key('windows', 'i686'))

    def test_latest_version_reads_only_a_bounded_official_manifest(self):
        body = json.dumps({'version': '1.3.0', 'url': 'https://fixture.invalid/agy.tgz', 'sha512': 'f' * 128}).encode()
        with mock.patch('urllib.request.urlopen', return_value=_Manifest(body)) as opened, mock.patch('platform.machine', return_value='x86_64'):
            self.assertEqual(updater.latest_agy_version('ubuntu'), '1.3.0')
        self.assertEqual(opened.call_args.args[0].full_url, updater.AGY_MANIFEST_BASE + 'linux_amd64.json')
        for response in (_Manifest(body, 'https://elsewhere.invalid/manifests/linux_amd64.json'),
                         _Manifest(b'{"version": "1.3.0", "pad": "' + b'x' * 20000 + b'"}'),
                         _Manifest(b'{"version": "1.3.0; id"}'), _Manifest(b'not json'), _Manifest(b'[]')):
            with mock.patch('urllib.request.urlopen', return_value=response), mock.patch('platform.machine', return_value='x86_64'):
                self.assertIsNone(updater.latest_agy_version('ubuntu'))
        with mock.patch('urllib.request.urlopen', side_effect=urllib.error.URLError('offline')):
            self.assertIsNone(updater.latest_agy_version('mac'))

    def test_unreviewed_newest_build_is_held_and_named(self):
        row = updater.antigravity_hold(self.install(), self.REVIEWED, latest=lambda platform: '1.3.0')
        self.assertEqual((row['status'], row['messageCode'], row['heldVersion']), ('held', 'held_for_review', '1.3.0'))
        self.assertEqual((row['previousVersion'], row['version'], row['updateAttempted']), ('1.2.16', '1.2.16', False))

    def test_reviewed_newest_build_may_update_and_current_never_runs_the_updater(self):
        self.assertIsNone(updater.antigravity_hold(self.install('1.2.14'), self.REVIEWED, latest=lambda platform: '1.2.16'))
        row = updater.antigravity_hold(self.install('1.3.0'), self.REVIEWED, latest=lambda platform: '1.3.0')
        self.assertEqual((row['status'], row['messageCode'], row['updateAttempted']), ('current', 'current', False))

    def test_unknown_review_or_manifest_holds_without_installing(self):
        probe = mock.Mock(return_value='1.2.16')
        row = updater.antigravity_hold(self.install(), None, latest=probe)
        self.assertEqual((row['status'], row['messageCode'], row['heldVersion']), ('held', 'held_unchecked', None))
        probe.assert_not_called()
        row = updater.antigravity_hold(self.install(), self.REVIEWED, latest=lambda platform: None)
        self.assertEqual((row['status'], row['messageCode']), ('held', 'held_unchecked'))
        self.assertIsNone(updater.antigravity_hold(self.install(None), self.REVIEWED, latest=probe))

    def run_apply(self, install, reviewed, newest, update=None):
        installs = {key: None for key in common.APP_LABELS}
        installs['antigravity-cli'] = install
        installs['omp'] = common.Install('omp', install.platform, pathlib.Path('/fixture/omp'), '1.0.0')
        update = update or (lambda item, deadline: common.result(item.app_id, item.platform, 'current', item.version, item.version, 'native', attempted=True))
        with mock.patch.object(pathlib.Path, 'home', return_value=self.root), \
                mock.patch.object(updater, 'detect', return_value=installs), \
                mock.patch.object(updater, 'latest_agy_version', side_effect=lambda platform: newest), \
                mock.patch.object(updater, 'check_readiness', return_value=None) as ready, \
                mock.patch.object(updater, 'update_cli', side_effect=update) as run:
            value = updater.run_apply(install.platform, agy_reviewed=reviewed)
        return {row['appId']: row for row in value['results']}, ready, run

    def test_run_apply_holds_antigravity_on_every_host_and_still_updates_the_rest(self):
        for platform in ('ubuntu', 'mac', 'windows'):
            rows, ready, run = self.run_apply(self.install(platform=platform), self.REVIEWED, '1.3.0')
            self.assertEqual(rows['antigravity-cli']['status'], 'held', platform)
            self.assertEqual(rows['antigravity-cli']['heldVersion'], '1.3.0')
            self.assertEqual([call.args[0].app_id for call in run.call_args_list], ['omp'], 'agy update never ran on ' + platform)
            self.assertEqual([call.args[0].app_id for call in ready.call_args_list], ['omp'])
            self.assertEqual(rows['omp']['status'], 'current')

    def test_run_apply_reports_an_update_that_landed_outside_the_review(self):
        moved = lambda item, deadline: common.result(item.app_id, item.platform, 'updated', item.version, '1.3.1', 'native', attempted=True)
        rows, _, _ = self.run_apply(self.install('1.2.14'), self.REVIEWED, '1.2.16', update=moved)
        self.assertEqual((rows['antigravity-cli']['status'], rows['antigravity-cli']['messageCode']), ('updated', 'updated_unreviewed'))
        reviewed = lambda item, deadline: common.result(item.app_id, item.platform, 'updated', item.version, '1.2.16', 'native', attempted=True)
        rows, _, _ = self.run_apply(self.install('1.2.14'), self.REVIEWED, '1.2.16', update=reviewed)
        self.assertEqual(rows['antigravity-cli']['messageCode'], 'updated')

    def test_fake_cli_is_never_asked_to_update_past_the_review(self):
        # A fixture `agy` on a fixture HOME: --version prints its version file,
        # `update` records the call and moves to the version the manifest named.
        home = self.root / 'home'
        state = self.root / 'agy-version'
        calls = self.root / 'agy-calls'
        state.write_text('1.2.16')
        _script(home / '.local/bin/agy', 'echo "$*" >> "%s"\nif [ "$1" = update ]; then cat "%s.next" > "%s"; exit 0; fi\ncat "%s"' % (calls, state, state, state))
        installs = {key: None for key in common.APP_LABELS}
        env = {'PATH': str(home / '.local/bin') + os.pathsep + '/usr/bin:/bin'}

        def run(newest, reviewed):
            pathlib.Path(str(state) + '.next').write_text(newest)
            with mock.patch.object(pathlib.Path, 'home', return_value=home), mock.patch.dict(os.environ, env), \
                    mock.patch.object(updater, 'detect', side_effect=lambda platform: {**installs, 'antigravity-cli': updater.detect_cli('antigravity-cli', platform)}), \
                    mock.patch.object(updater, 'latest_agy_version', return_value=newest), \
                    mock.patch.object(updater, 'scan', return_value=[]):
                return updater.run_apply('ubuntu', agy_reviewed=reviewed)['results'][0]

        held = run('1.3.0', self.REVIEWED)
        self.assertEqual((held['status'], held['heldVersion'], held['version']), ('held', '1.3.0', '1.2.16'))
        self.assertNotIn('update', calls.read_text().split())
        self.assertEqual(state.read_text(), '1.2.16')
        updated = run('1.3.0', self.REVIEWED | {'1.3.0'})
        self.assertEqual((updated['status'], updated['version'], updated['messageCode']), ('updated', '1.3.0', 'updated'))
        self.assertIn('update', calls.read_text().split())

    def test_main_passes_the_dashboard_list_and_falls_back_to_the_release_file(self):
        seen = []
        with mock.patch.object(updater, 'run_apply', side_effect=lambda platform, emit, cancelled, reviewed: seen.append(reviewed) or {'results': []}), \
                contextlib.redirect_stdout(io.StringIO()):
            with mock.patch.object(sys, 'argv', ['helper', '--apply', '--platform', 'ubuntu', '--agy-reviewed', '1.2.16,1.3.0']):
                updater.main()
            with mock.patch.object(sys, 'argv', ['helper', '--apply', '--platform', 'ubuntu', '--agy-reviewed', '1.2.16;id']):
                updater.main()
            with mock.patch.object(sys, 'argv', ['helper', '--apply', '--platform', 'ubuntu']):
                updater.main()
        self.assertEqual(seen[0], frozenset({'1.2.16', '1.3.0'}))
        self.assertIsNone(seen[1])
        self.assertIn('1.3.0', seen[2])

    def test_windows_task_child_receives_the_list_through_its_request_file(self):
        app_root = self.root / '.ccs/app-updates'
        seen = {}

        def shell(script, timeout=30):
            if 'Start-ScheduledTask' in script:
                seen['request'] = json.loads((app_root / 'windows-task-request.json').read_text())
                common.write_private_json(app_root / 'windows-task-result.json', {'nonce': seen['request']['nonce'], 'results': []})
            return ''
        with mock.patch.object(pathlib.Path, 'home', return_value=self.root), mock.patch.object(updater, 'powershell', side_effect=shell):
            updater.windows_interactive_apply(agy_reviewed=frozenset({'1.3.0', '1.2.16'}))
        self.assertEqual(seen['request']['agyReviewed'], ['1.2.16', '1.3.0'])
        request = app_root / 'windows-task-request.json'
        self.assertEqual(updater.read_task_request(request), (seen['request']['nonce'], frozenset({'1.2.16', '1.3.0'})))
        nonce = 'a' * 32
        for body, expected in (({'nonce': nonce}, (nonce, None)), ({'nonce': nonce, 'agyReviewed': ['1.3.0;id']}, (nonce, None)),
                               ({'nonce': nonce, 'agyReviewed': '1.3.0'}, (nonce, None)), ({'nonce': 'x', 'agyReviewed': ['1.3.0']}, (None, None)),
                               ([], (None, None))):
            request.write_text(json.dumps(body))
            self.assertEqual(updater.read_task_request(request), expected, body)
        self.assertEqual(updater.read_task_request(self.root / 'missing.json'), (None, None))

if __name__ == '__main__': unittest.main()
