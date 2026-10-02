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

    def test_pending_restart_is_retried_even_when_package_is_current(self):
        install = common.Install('antigravity-cli', 'ubuntu', pathlib.Path('/fixture/agy'), '2.0.0')
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(pathlib.Path, 'home', return_value=pathlib.Path(directory)), mock.patch.object(updater, 'scan', return_value=[]), mock.patch.object(updater, 'perform_cli_update'), mock.patch.object(updater, 'detect_cli', return_value=install), mock.patch.object(updater, 'restart_cli', return_value=[]) as restart:
            marker = pathlib.Path(directory) / '.ccs/app-updates/antigravity-cli-pending-restart.json'
            common.write_private_json(marker, {'version': '2.0.0'})
            value = updater.update_cli(install, time.monotonic() + 60)
            self.assertFalse(marker.exists())
        self.assertEqual(value['status'], 'updated')
        restart.assert_called_once()

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
                with mock.patch.object(pathlib.Path, 'home', return_value=root), mock.patch.object(updater, 'perform_cli_update', side_effect=lambda item: os.replace(stage, binary)), mock.patch.object(updater, 'detect_cli', return_value=replacement):
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
