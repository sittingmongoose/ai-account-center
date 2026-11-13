"""C1-C8 regression witnesses for max review of e459e64c1. All installs and peers are fixtures."""
import contextlib
import getpass
import http.server
import json
import os
import pathlib
import socket
import sys
import threading
import time
import unittest
import urllib.parse
from unittest import mock

sys.path[:0] = [str(pathlib.Path(__file__).resolve().parents[3] / 'scripts/app-updates'),
                str(pathlib.Path(__file__).resolve().parent)]
import hermetic  # noqa: F401
import app_update_common as common
import app_update_cursor as cursor
import app_update_antigravity as acp
import app_update_t3 as t3
import app_update_t3_rpc as rpc
import app_update_zcode as zcode
import app_updates as updates
import app_update_fix_round1_test as fixtures
from app_update_robustness_test import Home, attach, scope, write_package

REMOVED = '<line removed: may contain a credential>'
REST = '<rest of output removed: may contain a credential>'
# Owner decision U13: a row's diagnostics keep only these; never output or command text.
KEPT_FIELDS = {'cause', 'reason', 'tool', 'exitCode', 'phases', 'owner', 'notes'}
AFTER = 'update step finished'
ESC, BEL = '\x1b', '\x07'


class Round2(Home):
    consumers = fixtures.Adapters.consumers
    npm_prefix = fixtures.Adapters.npm_prefix
    npm_writes = fixtures.Adapters.npm_writes
    update = fixtures.Adapters.update

    def run_failing(self, sample=None, argv=None, label='fixture updater', **kwargs):
        """One real failing command in a row's scope: its exception (raw output, in memory) and exported row."""
        with scope() as record:
            with self.assertRaises(common.UpdateFailure) as caught:
                if argv is None:
                    argv = [sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(sample) + '); sys.exit(1)']
                common.command(argv, errors=True, label=label, **kwargs)
            row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
        return caught.exception, row

    def assert_nothing_kept(self, row, witnesses, tool='unlisted'):
        """No output, tail or command text in the exported row; only the fixed fields remain."""
        diagnostics = row['diagnostics']
        self.assertLessEqual(set(diagnostics), KEPT_FIELDS)
        self.assertEqual((diagnostics['cause'], diagnostics.get('tool'), diagnostics.get('exitCode')),
                         ('installer_failed', tool, 1))
        text = json.dumps(row)
        for witness in witnesses:
            self.assertNotIn(witness, text)

    def test_c1_real_command_keeps_no_output_from_any_carrier(self):
        samples = ['{"//registry.example.test/:_auth":"dXNlcjpmaXh0dXJl"}',
                   'Cookie: SID=fixtureCookieOne;\n\tHSID=fixtureCookieTwo',
                   'installer --password "first second third"',
                   'token=' + 'x' * 70000 + '\n    foldedWitness',
                   'x' * 700 + ' api_key=lateMarkerWitness',
                   'aUtH: value\n foldedWitness\n\tlastWitness',
                   'Cookie:\nfoldedWitness;\nlastWitness']
        witnesses = ('dXNlcjpmaXh0dXJl', 'fixtureCookieOne', 'fixtureCookieTwo', 'second third', 'foldedWitness',
                     'lateMarkerWitness', 'lastWitness', 'xxxxxxxx')
        for sample in samples:
            with self.subTest(sample=sample[:60]):
                error, row = self.run_failing(sample)
                self.assert_nothing_kept(row, witnesses)
                # Classification still reads the output, in memory only.
                self.assertIn(sample[:40], error.output)

    H1_FLAGS = ('--password', '--passwd', '--pass', '--token', '--api-key', '--apikey', '--auth', '--auth-token',
                '--authtoken', '--secret', '--access-token', '--accesstoken', '--otp')
    H1_WITNESSES = ('horse', 'battery', 'staple', 'correctHorseWitness', 'continuationWitness', 'zebraWitness',
                    'aaaaaaaaaa', 'fail.py')

    @classmethod
    def h1_samples(cls):
        """Every flag the token pass redacts, both dash forms, quoted and unquoted, '=' and space."""
        samples = []
        for flag in cls.H1_FLAGS + tuple(flag[1:] for flag in cls.H1_FLAGS):
            for value in ('"correct horse battery staple"', "'correct horse battery staple'", 'correctHorseWitness'):
                samples += ['installer %s %s' % (flag, value), 'installer %s=%s' % (flag, value)]
        return samples + ['installer -p "correct horse battery staple"', 'installer -p="correct horse battery staple"',
                          "installer -p'correct horse battery staple'", 'installer --pass \\\n  continuationWitness']

    # Round 1 (RECHECK-H1): multi-line values after a flag.
    H1_MULTILINE = (
        'installer --pass "correct\nhorse battery staple"\n' + AFTER,
        "installer --pass 'correct\nhorse battery staple'\n" + AFTER,
        "installer -OTP 'correct\nhorse battery\nstaple'\n" + AFTER,
        'installer -OTP "correct\nhorse battery staple"\n' + AFTER,
        'installer --password "correct\nhorse battery staple"\n' + AFTER,
        "installer --password 'correct\nhorse battery staple'\n" + AFTER,
        'installer --pass "correct\\"\nhorse battery staple"\n' + AFTER,
        'installer --pass "correct\nhorse battery staple\n' + AFTER,
        "installer -OTP 'correct\nhorse\nbattery staple\n" + AFTER,
        'installer --password "correct\nhorse battery staple',
    )
    # Round 2 (RECHECK-H1-2): 24 output carriers.
    H1_PROBE = (
        ('R1 dq', 'installer --pass "correct\nhorse battery staple"\n' + AFTER),
        ('R1 sq OTP', "installer -OTP 'correct\nhorse battery\nstaple'\n" + AFTER),
        ('R1 never closed', 'installer --password "correct\nhorse battery staple\n' + AFTER),
        ('R1 escaped dq', 'installer --pass "correct\\"\nhorse battery staple"\n' + AFTER),
        ('CRLF dq', 'installer --pass "correct\r\nhorse battery staple"\r\n' + AFTER + '\r\n'),
        ('CRLF sq eq', "installer --pass='correct\r\nhorse battery staple'\r\n" + AFTER + '\r\n'),
        ('lone CR', 'installer --pass "correct\rhorse battery staple"\r' + AFTER),
        ('mixed apostrophe in dq', 'installer --pass "it\'s correct\nhorse battery staple"\n' + AFTER),
        ('mixed dq in sq', 'installer --pass \'say "hi\nhorse battery staple\'\n' + AFTER),
        ('mixed concat', 'installer --pass "correct"\'x\nhorse battery staple\'\n' + AFTER),
        ('eq dq', 'installer --pass="correct\nhorse battery staple"\n' + AFTER),
        ('space sq', "installer --token 'correct\nhorse battery staple'\n" + AFTER),
        ('close later + more text', 'installer --pass "correct\nhorse battery staple" --verbose ok\n' + AFTER),
        ('close later 3 lines', "installer --secret 'a\nb\nhorse battery staple' then\n" + AFTER),
        ('quote before flag dq', 'installer "--pass=correct\nhorse battery staple"\n' + AFTER),
        ('quote before flag sq', "installer '--password=correct\nhorse battery staple'\n" + AFTER),
        ('argv list', 'Running ["installer", "--pass", "correct\nhorse battery staple"]\n' + AFTER),
        ('earlier quoted marker', 'installer: "auth" step: --pass "correct\nhorse battery staple"\n' + AFTER),
        ('earlier quoted session', 'error: "session expired"; retry --otp "correct\nhorse battery staple"\n' + AFTER),
        ('posix escaped sq outside', "installer --pass it\\'s' correct\nhorse battery staple'\n" + AFTER),
        ('bash sq idiom', "installer --pass 'it'\\''s correct\nhorse battery staple'\n" + AFTER),
        ('powershell backtick', 'installer --pass "correct`" x\nhorse battery staple"\n' + AFTER),
        ('unquoted newline (dash -x)', '+ installer --pass correct\nhorse battery staple\n' + AFTER),
        ('marker password colon', 'password: "correct\nhorse battery staple"\n' + AFTER),
    )
    # Round 4 (RECHECK-H1-4, H1-R6): an ANSI title (OSC) that holds the flag, and a coloured flag.
    H1_OSC = (
        ('osc newline', ESC + ']0;installer --pass "correct\nhorse battery staple"' + BEL + '\n' + AFTER),
        ('osc crlf otp', ESC + ']0;installer --otp "correct\r\nhorse battery staple"' + BEL + '\r\n' + AFTER),
        ('osc over 512', ESC + ']0;installer --pass ' + 'a' * 520 + 'horse battery staple' + BEL + '\n' + AFTER),
        ('sgr coloured flag', 'installer ' + ESC + '[31m--pass' + ESC + '[0m "correct horse battery staple"\n' + AFTER),
    )

    def output_samples(self):
        return ([('alias', sample) for sample in self.h1_samples()] + [('multiline', sample) for sample in self.H1_MULTILINE]
                + list(self.H1_PROBE) + list(self.H1_OSC))

    def test_h1_no_output_from_any_probe_reaches_the_row(self):
        """Every witness of rounds 1-4 (aliases, multi-line, 24 carriers, OSC): classified, then discarded."""
        for name, sample in self.output_samples():
            with self.subTest(name=name, sample=sample[:50]):
                error, row = self.run_failing(sample)
                self.assert_nothing_kept(row, self.H1_WITNESSES)
                # Classification still reads the output, in memory only.
                self.assertIn('staple' if 'staple' in sample else 'Witness', error.output)

    def test_h1_no_argument_reaches_the_row(self):
        """Rounds 2-3: argv values with line breaks, plain values and long slash '=' values. Only the tool word stays."""
        child = self.root / 'fail.py'
        child.write_text('import sys; sys.exit(1)')
        values = [['--pass', 'correct\nhorse battery staple'], ['--pass', 'correct\r\nhorse battery staple'],
                  ['--pass', 'zebraWitness'], ['--pass=/' + 'horse battery staple/' * 12],
                  ['--password=/' + 'horse battery staple/' * 12], ['--verbose', 'correctHorseWitness']]
        for extra in values:
            with self.subTest(extra=extra[0][:20]):
                _, row = self.run_failing(argv=[sys.executable, str(child), *extra], label=None)
                self.assert_nothing_kept(row, self.H1_WITNESSES, tool='python')
        # A label names the tool by its first word only.
        _, row = self.run_failing(argv=[sys.executable, str(child), '--token', 'zebraWitness'],
                                  label='npm install --global zebraWitness')
        self.assert_nothing_kept(row, self.H1_WITNESSES, tool='npm')

    def test_h1_tool_ids_are_fixed_words(self):
        self.assertEqual(common.tool_id(['/usr/bin/systemctl', '--user', 'restart', 'x']), 'systemctl')
        self.assertEqual(common.tool_id(['C:\\Program Files\\nodejs\\node.exe', 'cli.js']), 'node')
        self.assertEqual(common.tool_id(['/usr/bin/python3.14', '-c', 'x']), 'python')
        self.assertEqual(common.tool_id(['x'], 'cursor-agent update (Mac GUI)'), 'cursor-agent')
        self.assertEqual(common.tool_id(['/home/someone/--pass=zebraWitness']), 'unlisted')
        self.assertEqual(common.tool_id(['x'], 'fixture updater'), 'unlisted')
        self.assertEqual(common.tool_id([]), 'unlisted')
        self.assertTrue(all(common.REASON.fullmatch(tool) for tool in common.TOOLS))

    def test_h1_the_recorder_drops_output_handed_to_it_directly(self):
        """The Mac GUI path hands its own output to ran(); it is discarded the same way."""
        with scope() as record:
            record.ran(['/usr/bin/open', '-W', 'x'], 1, 'installer --pass "correct\nhorse battery staple"',
                       label='cursor-agent update (Mac GUI)')
            row = attach(common.result('cursor-cli', 'mac', 'failed', code='update_failed'), record)
        self.assert_nothing_kept(row, self.H1_WITNESSES, tool='cursor-agent')

    def test_h1_scrubbers_stay_as_defence_in_depth(self):
        """No output reaches storage, but the scrubbers still clean any remaining free text (owner paths)."""
        for flag in self.H1_FLAGS:
            self.assertRegex(flag, '^' + common.CREDENTIAL_FLAG + '$')
            self.assertRegex(flag, common._CREDENTIAL_MARKER)
        self.assertIn(common.CREDENTIAL_FLAG, common.CREDENTIAL_MARKERS)
        for name, sample in list(self.H1_PROBE) + list(self.H1_OSC):
            with self.subTest(name=name):
                self.assertEqual(common.scrub(sample), REST)
        for arg in ('--pass=/' + 'horse battery staple/' * 12, ESC + ']0;--pass ' + BEL + 'horse/battery/staple'):
            self.assertEqual(common.display_path(arg), REST)
        self.assertEqual(common.display_path('share/file.txt'), 'share/file.txt')
        line = 'installer failed: exit code=1; ENOSPC: no space left on device'
        self.assertEqual(common.scrub(line), line)
        self.assertEqual(common.scrub('callback code=abc123def'), REMOVED)
        self.assertEqual(common.scrub('verification code=482913'), REMOVED)
        control = 'installer: unpacking zcode-acp-server 0.84.3 (mkdir -p prefix; lsof -a -p 4242 found 0 holders)'
        self.assertEqual(common.scrub(control), control)

    def test_h1_ordinary_output_is_not_kept_either_but_its_cause_and_exit_are(self):
        control = 'installer: unpacking zcode-acp-server 0.84.3\nENOSPC: no space left on device'
        error, row = self.run_failing(control)
        self.assert_nothing_kept(row, ('unpacking', 'ENOSPC', 'zcode-acp-server'))
        self.assertIn('ENOSPC', error.output)

    def test_c2_an_unbound_same_window_log_never_overrides_tls_or_generic_output(self):
        for text in ('Update failed: certificate has expired', 'Update failed: Error'):
            # Real fixture log, deliberately a different PID; no launched PID binding exists.
            helper = fixtures.B3CursorLogs('runTest')
            helper.setUp()
            try:
                row = helper.run_with_log(text, getpass.getuser(), time.time())
                self.assertEqual((row['status'], row['messageCode']), ('failed', 'update_failed'))
                self.assertIn('signin_undetermined', row['diagnostics']['notes'])
            finally:
                helper.doCleanups()
        with scope():
            self.assertEqual(cursor.classify(common.UpdateFailure(output='Update failed: [unauthenticated] Error'), 0).code,
                             'cursor_signin_needed')

    def test_c3_every_owner_and_unplanned_sibling_is_checked_before_target_success(self):
        first = self.npm_prefix('first', '0.84.2')
        second = self.npm_prefix('second', '0.85.0')
        self.consumers(first / 'zcode-acp-server/dist/cli.js', second / 'zcode-acp-server/dist/cli.js')
        install = zcode.detect_adapters('mac')
        write_package(first, 'zcode-acp-server', '0.84.3')
        write_package(second, 'zcode-acp-server', '0.84.3')
        with scope():
            row = zcode.adapters_settle(install, ['zcode-acp-server'], {'zcode-acp-server': '0.84.3'}, {}, set(), True)
        self.assertEqual((row['status'], row['messageCode']), ('failed', 'version_went_down'))
        # The package need not be in behind (a combined updater can touch siblings).
        with scope():
            row = zcode.adapters_settle(install, [], {}, {}, set(), True)
        self.assertEqual((row['status'], row['messageCode']), ('failed', 'version_went_down'))

    def test_c3_unit_with_a_newer_sibling_uses_exact_direct_install(self):
        modules = self.root / '.local/lib/node_modules'
        write_package(modules, '@brokkai/muse-acp', '0.22.0')
        write_package(modules, 'zcode-acp-server', '0.84.2')
        node, cli = self.root / 'node', self.root / 'npm-cli.js'
        def unsafe_unit(deadline):
            write_package(modules, '@brokkai/muse-acp', '0.21.0')
            write_package(modules, 'zcode-acp-server', '0.84.3')
        runs = []
        with mock.patch.object(zcode, 'registry_version', side_effect={'muse-acp':'0.21.0', 'zcode-acp-server':'0.84.3'}.get), \
                mock.patch.object(zcode, 'resolve_npm', return_value=(node, cli)), \
                mock.patch.object(zcode, 'acp_unit_present', return_value=True), \
                mock.patch.object(zcode, 'run_unit', side_effect=unsafe_unit) as unit, \
                mock.patch.object(zcode, 'scan', return_value=[]), \
                mock.patch.object(common, '_run_bounded', side_effect=self.npm_writes(runs=runs)), scope():
            row = zcode.update_adapters(zcode.detect_adapters('ubuntu'), time.monotonic() + 10)
        self.assertEqual(zcode.package_version(modules, 'muse-acp'), '0.22.0')
        self.assertEqual(row['status'], 'updated')
        unit.assert_not_called()
        self.assertEqual(runs[0][-1], 'zcode-acp-server@0.84.3')

    def test_c3_zcode_unit_cannot_downgrade_an_adapter_sibling(self):
        modules = self.root / '.local/lib/node_modules'
        write_package(modules, '@brokkai/muse-acp', '0.22.0')
        install = common.Install('zcode', 'ubuntu', self.root / 'app', '3.14.4')
        with mock.patch.object(zcode, 'latest_zcode', return_value='3.14.5'), \
                mock.patch.object(zcode, 'acp_unit_present', return_value=True), \
                mock.patch.object(zcode, 'scan', return_value=[]), \
                mock.patch.object(zcode, 'registry_version', return_value='0.21.0'), \
                mock.patch.object(zcode, 'run_unit') as unit, scope():
            row = zcode.update_zcode(install, time.monotonic() + 10)
        self.assertEqual(row['status'], 'skipped')
        unit.assert_not_called()

    def test_c3_adapter_unit_also_verifies_the_desktop_it_can_touch(self):
        modules = self.root / '.local/lib/node_modules'
        write_package(modules, '@brokkai/muse-acp', '0.21.0')
        write_package(modules, 'zcode-acp-server', '0.84.2')
        desktop = common.Install('zcode', 'ubuntu', self.root / 'app', '3.14.5')
        with mock.patch.object(zcode, 'registry_version', side_effect={'muse-acp':'0.21.0', 'zcode-acp-server':'0.84.3'}.get), \
                mock.patch.object(zcode, 'acp_unit_present', return_value=True), \
                mock.patch.object(zcode, 'scan', return_value=[]), \
                mock.patch.object(zcode, 'detect_zcode', return_value=desktop), \
                mock.patch.object(zcode, 'installed_version', return_value='3.14.4'), \
                mock.patch.object(zcode, 'run_unit', side_effect=lambda deadline: write_package(modules, 'zcode-acp-server', '0.84.3')), scope():
            row = zcode.update_adapters(zcode.detect_adapters('ubuntu'), time.monotonic() + 10)
        self.assertEqual((row['status'], row['messageCode']), ('failed', 'version_went_down'))

    def test_c3_cursor_requires_a_strict_increase(self):
        install = common.Install('cursor-cli', 'ubuntu', self.root / 'cursor-agent', '2026.11.13-abcdef1')
        with mock.patch.object(cursor, 'run_update'), \
                mock.patch.object(cursor, 'detect_cursor', return_value=common.Install(
                    'cursor-cli', 'ubuntu', install.path, '2026.11.11-abcdef2')), scope():
            row = cursor.update_cursor(install, time.monotonic() + 10)
        self.assertEqual((row['status'], row['messageCode']), ('failed', 'version_went_down'))

    def test_c5_mixed_explicit_and_bare_commands_resolve_both_copies(self):
        first = self.npm_prefix('first', '0.84.2')
        second = self.npm_prefix('second', '0.84.2')
        binary = self.root / 'second/bin/zcode-acp-server'
        binary.parent.mkdir(parents=True)
        binary.symlink_to(second / 'zcode-acp-server/dist/cli.js')
        self.consumers(first / 'zcode-acp-server/dist/cli.js')
        settings = self.root / '.t3/userdata/settings.json'
        config = json.loads(settings.read_text())
        config['providerInstances']['bare'] = {'enabled':True, 'config':{'commandPath':'zcode-acp-server'}}
        settings.write_text(json.dumps(config))
        runs = []
        with mock.patch.dict(os.environ, {'PATH':str(binary.parent)}):
            install, row = self.update('mac', self.npm_writes(runs=runs))
        self.assertEqual((zcode.package_version(first, 'zcode-acp-server'), zcode.package_version(second, 'zcode-acp-server')),
                         ('0.84.3', '0.84.3'))
        self.assertEqual(row['status'], 'updated')
        self.assertEqual(len(runs), 2)

    def test_c5_unresolved_bare_command_skips_instead_of_updating_another_copy(self):
        first = self.npm_prefix('first', '0.84.2')
        self.consumers(first / 'zcode-acp-server/dist/cli.js')
        settings = self.root / '.t3/userdata/settings.json'
        config = json.loads(settings.read_text())
        config['providerInstances']['bare'] = {'enabled':True, 'config':{'commandPath':'zcode-acp-server'}}
        settings.write_text(json.dumps(config))
        runs = []
        install, row = self.update('mac', self.npm_writes(runs=runs))
        self.assertEqual((row['status'], row['messageCode']), ('skipped', 'mixed_ownership'))
        self.assertEqual(runs, [])

    def test_c6_configured_pnpm_store_copy_skips_without_install_or_verification(self):
        modules = self.root / '.local/share/pnpm/global/5/node_modules'
        entry = modules / '.pnpm/zcode-acp-server@0.84.2/node_modules'
        write_package(entry, 'zcode-acp-server', '0.84.2')
        script = entry / 'zcode-acp-server/dist/cli.js'
        script.parent.mkdir(parents=True); script.write_text('fixture')
        (modules / 'zcode-acp-server').symlink_to(entry / 'zcode-acp-server')
        self.consumers(script)
        install = zcode.detect_adapters('ubuntu')
        def pnpm(argv, **kwargs):
            # Honor the actual global destination; the configured old store entry stays put.
            self.assertEqual(pathlib.Path(argv[argv.index('--global-dir') + 1]), modules.parent.parent)
            version = argv[-1].rsplit('@', 1)[1]
            fresh = modules / '.pnpm' / ('zcode-acp-server@' + version) / 'node_modules'
            write_package(fresh, 'zcode-acp-server', version)
            (modules / 'zcode-acp-server').unlink()
            (modules / 'zcode-acp-server').symlink_to(fresh / 'zcode-acp-server')
            return ''
        with mock.patch.object(zcode, 'registry_version', return_value='0.84.3') as registry, \
                mock.patch.object(zcode, 'pnpm_argv', return_value=['fixture-pnpm']), \
                mock.patch.object(zcode, 'acp_unit_present', return_value=False), \
                mock.patch.object(zcode, 'scan', return_value=[]), \
                mock.patch.object(zcode, 'command', side_effect=pnpm) as run, scope():
            row = zcode.update_adapters(install, time.monotonic() + 10)
        self.assertEqual((row['status'], row['messageCode']), ('skipped', 'pnpm_store_copy'))
        registry.assert_not_called(); run.assert_not_called()
        self.assertEqual(zcode.package_version(entry, 'zcode-acp-server'), '0.84.2')

    def test_c7_runtime_only_t3_row_requires_only_an_answer(self):
        row = {'status':'updated', 'version':'0.1.10-nightly.20261113.4616', 'restartTargets':[]}
        self.assertIsNone(acp.replaced_version(row))
        row['restartTargets'] = [{'kind':'desktop'}]
        self.assertEqual(acp.replaced_version(row), row['version'])

    def test_c8_access_denied_without_a_running_adapter_is_not_a_lock(self):
        modules = self.root / 'Program Files/nodejs/node_modules'
        write_package(modules, 'zcode-acp-server', '0.84.2')
        self.consumers(modules / 'zcode-acp-server/package.json')
        for errno in ('EPERM', 'EACCES'):
            for syscall in ('rename', 'unlink', 'mkdir', 'scandir'):
                output = 'npm error code ' + errno + '\nnpm error syscall ' + syscall + '\nnpm error path C:/Program Files/nodejs/node_modules/zcode-acp-server'
                with self.subTest(errno=errno, syscall=syscall):
                    install, row = self.update('windows', lambda *a, **k: (1, output.encode()))
                    self.assertEqual((row['status'], row['messageCode']), ('action_required', 'prefix_not_writable'))
                    self.assertFalse(any(part.get('inUse') for part in row['parts']))


class Deadlines(Home):
    def test_c4_rpc_trickled_body_and_frame_header_obey_both_deadlines(self):
        for operation, settle in ((.10, .25), (.25, .10)):
            for size in (8, 32):
                with self.subTest(operation=operation, settle=settle, size=size):
                    left, right = socket.socketpair()
                    client = rpc.Rpc.__new__(rpc.Rpc)
                    client.socket, client.stream = left, left.makefile('rb')
                    started = time.monotonic()
                    client.deadline, client.settle = started + operation, started + settle
                    def write():
                        with right:
                            try:
                                for _ in range(size):
                                    right.sendall(b'x'); time.sleep(.06)
                            except OSError:
                                pass
                    writer = threading.Thread(target=write); writer.start()
                    try:
                        with self.assertRaises((TimeoutError, common.UpdateFailure)):
                            client.read(size)
                        self.assertLess(time.monotonic() - started, .35)
                    finally:
                        client.close(); writer.join()

    def test_c4_trickled_handshake_has_an_absolute_deadline(self):
        left, right = socket.socketpair()
        def write():
            with right:
                right.recv(4096)
                try:
                    for byte in b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n':
                        right.sendall(bytes([byte])); time.sleep(.025)
                except OSError:
                    pass
        writer = threading.Thread(target=write); writer.start()
        started = time.monotonic()
        try:
            with mock.patch.object(rpc.socket, 'create_connection', return_value=left):
                with self.assertRaises((TimeoutError, common.UpdateFailure)):
                    rpc.Rpc(urllib.parse.urlsplit('http://127.0.0.1:1234'), 'fixture', started + .15)
            self.assertLess(time.monotonic() - started, .40)
        finally:
            left.close(); writer.join()

    def test_c4_all_three_lookups_bound_headers_and_bodies_in_a_killable_child(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                try:
                    if self.server.trickle_headers:
                        for byte in b'HTTP/1.1 200 OK\r\nContent-Length: 300\r\n\r\n':
                            self.connection.sendall(bytes([byte])); time.sleep(.025)
                    else:
                        self.send_response(200); self.send_header('Content-Length', '300'); self.end_headers()
                    for _ in range(300):
                        self.wfile.write(b' '); self.wfile.flush(); time.sleep(.025)
                except OSError:
                    pass
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever); thread.start()
        base = 'http://127.0.0.1:%d/' % server.server_port
        original = zcode.read_bounded
        def short_read(url, prefix, limit, deadline=None):
            return original(url, prefix, limit, deadline=min(deadline or float('inf'), time.monotonic() + .25))
        try:
            for headers in (False, True):
                server.trickle_headers = headers
                for call, module, field in ((t3.latest_release, t3, 'RELEASES'),
                                            (lambda: updates.latest_agy_version('ubuntu'), updates, 'AGY_MANIFEST_BASE'),
                                            (lambda: updates.claude_pointer('latest'), updates, 'CLAUDE_RELEASES')):
                    with self.subTest(headers=headers, field=field), mock.patch.object(module, field, base), \
                            mock.patch.object(zcode, 'read_bounded', side_effect=short_read):
                        started = time.monotonic()
                        try:
                            value = call()
                        except common.UpdateFailure as error:
                            value = None
                            self.assertEqual(error.code, 'timeout')
                        self.assertIsNone(value)
                        self.assertLess(time.monotonic() - started, 1.0)
        finally:
            server.shutdown(); server.server_close(); thread.join()


if __name__ == '__main__':
    unittest.main()
