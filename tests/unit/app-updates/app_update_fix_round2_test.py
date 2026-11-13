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


class Round2(Home):
    consumers = fixtures.Adapters.consumers
    npm_prefix = fixtures.Adapters.npm_prefix
    npm_writes = fixtures.Adapters.npm_writes
    update = fixtures.Adapters.update

    def test_c1_real_command_drops_carriers_and_continuations_before_truncation(self):
        samples = ['{"//registry.example.test/:_auth":"dXNlcjpmaXh0dXJl"}',
                   'Cookie: SID=fixtureCookieOne;\n\tHSID=fixtureCookieTwo',
                   'installer --password "first second third"',
                   'token=' + 'x' * 70000 + '\n    foldedWitness',
                   'x' * 700 + ' api_key=lateMarkerWitness',
                   'aUtH: value\n foldedWitness\n\tlastWitness',
                   'Cookie:\nfoldedWitness;\nlastWitness']
        for sample in samples:
            with self.subTest(sample=sample[:60]), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(sample) + '); sys.exit(1)'],
                                   errors=True, label='fixture updater')
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                tail = row['diagnostics']['tail']
                # A credential flag, or a quote after a marker, ends the kept output; other markers
                # drop their line and its continuations.
                self.assertEqual(tail, REST if sample.startswith(('{', 'installer')) else REMOVED)
        self.assertEqual(common.scrub('ordinary ENOSPC diagnostic'), 'ordinary ENOSPC diagnostic')

    H1_FLAGS = ('--password', '--passwd', '--pass', '--token', '--api-key', '--apikey', '--auth', '--auth-token',
                '--authtoken', '--secret', '--access-token', '--accesstoken', '--otp')
    H1_WITNESSES = ('horse', 'battery', 'staple', 'correctHorseWitness', 'continuationWitness')

    @classmethod
    def h1_samples(cls):
        """Every flag the token pass redacts, both dash forms, quoted and unquoted, '=' and space."""
        samples = []
        for flag in cls.H1_FLAGS + tuple(flag[1:] for flag in cls.H1_FLAGS):
            for value in ('"correct horse battery staple"', "'correct horse battery staple'", 'correctHorseWitness'):
                samples += ['installer %s %s' % (flag, value), 'installer %s=%s' % (flag, value)]
        return samples + ['installer -p "correct horse battery staple"', 'installer -p="correct horse battery staple"',
                          "installer -p'correct horse battery staple'", 'installer --pass \\\n  continuationWitness']

    def test_h1_every_recognised_flag_drops_its_whole_line_through_command_and_export(self):
        for sample in self.h1_samples():
            with self.subTest(sample=sample), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(sample) + '); sys.exit(1)'],
                                   errors=True, label='fixture updater')
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                self.assertEqual(row['diagnostics']['tail'], REST)
                self.assertEqual(common.scrub(sample), REST)
                for witness in self.H1_WITNESSES:
                    self.assertNotIn(witness, json.dumps(row))

    def test_h1_every_flag_the_token_pass_redacts_is_a_whole_line_marker(self):
        for flag in self.H1_FLAGS:
            self.assertRegex(flag, '^' + common.CREDENTIAL_FLAG + '$')
            self.assertRegex(flag, common._CREDENTIAL_MARKER)
        self.assertIn(common.CREDENTIAL_FLAG, common.CREDENTIAL_MARKERS)

    AFTER = 'update step finished'
    # Round-1 witnesses: each carries a credential flag, so the line and the rest of the output go.
    H1_MULTILINE = (
        ('installer --pass "correct\nhorse battery staple"\n' + AFTER, REST),
        ("installer --pass 'correct\nhorse battery staple'\n" + AFTER, REST),
        ("installer -OTP 'correct\nhorse battery\nstaple'\n" + AFTER, REST),
        ('installer -OTP "correct\nhorse battery staple"\n' + AFTER, REST),
        ('installer --password "correct\nhorse battery staple"\n' + AFTER, REST),
        ("installer --password 'correct\nhorse battery staple'\n" + AFTER, REST),
        ('installer --pass "correct\\"\nhorse battery staple"\n' + AFTER, REST),
        ('installer --pass "correct\nhorse battery staple\n' + AFTER, REST),
        ("installer -OTP 'correct\nhorse\nbattery staple\n" + AFTER, REST),
        ('installer --password "correct\nhorse battery staple', REST),
    )

    def test_h1_a_multiline_value_after_a_flag_never_keeps_a_tail(self):
        for sample, expected in self.H1_MULTILINE:
            with self.subTest(sample=sample), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(sample) + '); sys.exit(1)'],
                                   errors=True, label='fixture updater')
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                self.assertEqual(row['diagnostics']['tail'], expected)
                self.assertEqual(common.scrub(sample), expected)
                for witness in self.H1_WITNESSES:
                    self.assertNotIn(witness, json.dumps(row))

    # RECHECK-H1-2 probe: 24 output carriers. Each holds a credential flag, or a quote after a marker,
    # so its line and the rest of the output go, with no quote parsing.
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

    def test_h1_recheck2_output_carriers_end_the_kept_output(self):
        for name, sample in self.H1_PROBE:
            with self.subTest(name=name), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(sample) + '); sys.exit(1)'],
                                   errors=True, label='fixture updater')
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                self.assertEqual(row['diagnostics']['tail'], REST)
                self.assertEqual(common.scrub(sample), REST)
                for witness in self.H1_WITNESSES:
                    self.assertNotIn(witness, json.dumps(row))

    def test_h1_recheck2_a_line_break_in_an_argument_never_leaves_a_tail_in_the_command(self):
        child = self.root / 'fail.py'
        child.write_text('import sys; sys.exit(1)')
        for value in ('correct\nhorse battery staple', 'correct\r\nhorse battery staple', 'zebraWitness'):
            with self.subTest(value=value), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, str(child), '--pass', value], errors=True)
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                self.assertEqual(row['diagnostics']['command'], REST)
                for witness in self.H1_WITNESSES + ('zebraWitness',):
                    self.assertNotIn(witness, json.dumps(row))
        self.assertEqual(common.command_display(['installer', 'line one\nline two']), 'installer line one line two')

    def test_h1_recheck3_a_long_slash_equals_argument_never_leaves_fragments_in_the_command(self):
        # Path shortening keeps an argument's last characters; the flag must be seen on the
        # complete argument before that shortening can remove it.
        child = self.root / 'fail.py'
        child.write_text('import sys; sys.exit(1)')
        for arg in ('--pass=/' + 'horse battery staple/' * 12,
                    '--password=/' + 'horse battery staple/' * 12):
            with self.subTest(arg=arg[:20]), scope() as record:
                with self.assertRaises(common.UpdateFailure):
                    common.command([sys.executable, str(child), arg], errors=True)
                row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
                self.assertEqual(row['diagnostics']['command'], REST)
                self.assertEqual(common.command_display(['installer', arg]), REST)
                self.assertEqual(common.display_path(arg), REST)
                for witness in self.H1_WITNESSES:
                    self.assertNotIn(witness, json.dumps(row))
        # Short and ordinary commands still display normally.
        self.assertEqual(common.command_display(['installer', '--verbose']), 'installer --verbose')
        self.assertEqual(common.command_display(['installer', 'share/file.txt']), 'installer share/file.txt')
        self.assertEqual(common.display_path('share/file.txt'), 'share/file.txt')

    def test_h1_an_exit_code_keeps_its_failure_line(self):
        line = 'installer failed: exit code=1; ENOSPC: no space left on device'
        self.assertEqual(common.scrub(line), line)
        self.assertEqual(common.scrub('callback code=abc123def'), REMOVED)
        self.assertEqual(common.scrub('verification code=482913'), REMOVED)

    def test_h1_ordinary_installer_output_keeps_its_text(self):
        control = 'installer: unpacking zcode-acp-server 0.84.3 (mkdir -p prefix; lsof -a -p 4242 found 0 holders)'
        with scope() as record:
            with self.assertRaises(common.UpdateFailure):
                common.command([sys.executable, '-c', 'import sys; sys.stderr.write(' + repr(control) + '); sys.exit(1)'],
                               errors=True, label='fixture updater')
            row = attach(common.result('t3-acp-adapters', 'mac', 'failed', code='update_failed'), record)
        self.assertEqual(row['diagnostics']['tail'], control)

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
