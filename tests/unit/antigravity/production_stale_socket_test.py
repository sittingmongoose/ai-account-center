"""Stale socket recovery for the resident control and native status sockets.

Only disposable sockets under temporary directories are bound. The startup
subprocesses run a copied runtime bundle with a fake parser and a temporary
HOME; they never read the real ~/.ccs, ~/.gemini or an installed agy.
"""
import json
import os
from pathlib import Path
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

PUBLIC_RUNTIME = Path(__file__).resolve().parents[3] / 'scripts/antigravity/runtime'
sys.path.insert(0, str(PUBLIC_RUNTIME))
from native_status_attestor import NativeStatusSocket, StatusError, clear_stale_socket
from resident_broker import ResidentBroker
from runtime_continuity import ContinuityError


def bind_socket(path):
    """Bind a listening socket the way the brokers do: umask 0o177, so mode 0600."""
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    old = os.umask(0o177)
    try: listener.bind(str(path))
    finally: os.umask(old)
    listener.listen(4)
    return listener


def leftover_socket(path):
    """A socket file with nothing listening, as a crash leaves behind."""
    bind_socket(path).close()


def accepts(path):
    """True when a fresh client can connect; the probe is closed again at once."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(1.0)
        try: client.connect(str(path))
        except OSError: return False
    return True


class StaleSocketScenarios:
    """Shared cases mixed into one TestCase per socket; not collected on its own."""
    label = None

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='aac-stale-socket-')
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.directory.chmod(0o700)
        self.path = self.directory / ('control.sock' if self.label == 'control' else 'status.sock')

    def start(self):
        """Return the real listen() for this socket and register its cleanup."""
        if self.label == 'control':
            broker = ResidentBroker(SimpleNamespace(sessions={}), self.path)
            self.addCleanup(self.close_broker, broker)
            return broker.listen
        receiver = NativeStatusSocket(self.path, Mock())
        self.addCleanup(receiver.close)
        return receiver.listen

    @staticmethod
    def close_broker(broker):
        if broker.listener is not None: broker.listener.close()
        broker.selector.close()

    def refusal(self):
        if self.label == 'control': return ContinuityError, 'ipc-already-exists'
        return StatusError, 'status-socket-already-exists'

    def test_stale_leftover_is_removed_and_listen_binds_again(self):
        leftover_socket(self.path)
        listen = self.start()
        listen()
        self.assertTrue(stat.S_ISSOCK(self.path.lstat().st_mode))
        self.assertEqual(stat.S_IMODE(self.path.lstat().st_mode), 0o600)
        self.assertTrue(accepts(self.path))

    def test_live_listener_is_refused_and_its_file_is_kept(self):
        live = bind_socket(self.path)
        self.addCleanup(live.close)
        before = self.path.lstat()
        error, code = self.refusal()
        listen = self.start()
        with self.assertRaises(error) as caught: listen()
        self.assertEqual(str(caught.exception), code)
        after = self.path.lstat()
        self.assertEqual((after.st_dev, after.st_ino), (before.st_dev, before.st_ino))
        self.assertTrue(accepts(self.path))

    def test_regular_file_is_refused_and_left_untouched(self):
        self.path.write_bytes(b'not a socket')
        error, code = self.refusal()
        listen = self.start()
        with self.assertRaises(error) as caught: listen()
        self.assertEqual(str(caught.exception), code)
        self.assertEqual(self.path.read_bytes(), b'not a socket')

    def test_symlink_is_refused_and_its_target_is_untouched(self):
        target = self.directory / 'target.sock'
        leftover_socket(target)
        os.symlink(target, self.path)
        error, code = self.refusal()
        listen = self.start()
        with self.assertRaises(error) as caught: listen()
        self.assertEqual(str(caught.exception), code)
        self.assertTrue(self.path.is_symlink())
        self.assertEqual(os.readlink(self.path), str(target))
        self.assertTrue(stat.S_ISSOCK(target.lstat().st_mode))


class ControlSocketStaleTests(StaleSocketScenarios, unittest.TestCase):
    label = 'control'


class StatusSocketStaleTests(StaleSocketScenarios, unittest.TestCase):
    label = 'status'


class ClearStaleSocketFixtures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='aac-clear-socket-')
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.directory.chmod(0o700)
        self.path = self.directory / 'control.sock'

    def test_absent_path_is_clear_for_the_caller_to_bind(self):
        self.assertTrue(clear_stale_socket(self.path))
        self.assertFalse(self.path.exists())

    def test_foreign_owned_socket_is_refused_and_kept(self):
        leftover_socket(self.path)
        with patch('native_status_attestor.os.getuid', return_value=os.getuid() + 1):
            self.assertFalse(clear_stale_socket(self.path))
        self.assertTrue(stat.S_ISSOCK(self.path.lstat().st_mode))

    def test_probe_timeout_is_refused_and_kept(self):
        leftover_socket(self.path)
        with patch.object(socket.socket, 'connect', side_effect=socket.timeout('fixture timeout')):
            self.assertFalse(clear_stale_socket(self.path))
        self.assertTrue(stat.S_ISSOCK(self.path.lstat().st_mode))

    def test_inode_replaced_during_probe_is_not_unlinked(self):
        leftover_socket(self.path)
        keep = self.directory / 'keep.sock'
        try: os.link(self.path, keep)
        except OSError: self.skipTest('hard links to sockets are unsupported here')
        self.addCleanup(lambda: keep.unlink(missing_ok=True))
        def replace_then_refuse(*args, **kwargs):
            self.path.unlink()
            leftover_socket(self.path)
            raise ConnectionRefusedError(111, 'fixture refused')
        with patch.object(socket.socket, 'connect', side_effect=replace_then_refuse):
            self.assertFalse(clear_stale_socket(self.path))
        self.assertTrue(stat.S_ISSOCK(self.path.lstat().st_mode))


class StartupMessageFixtures(unittest.TestCase):
    """Runs the real resident_main entrypoint from a copied bundle, as a subprocess."""

    def run_entrypoint(self, socket_path_for):
        temp = tempfile.TemporaryDirectory(prefix='aac-startup-message-')
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        root.chmod(0o700)
        library = root / 'bundle' / 'library'
        shutil.copytree(PUBLIC_RUNTIME, library, ignore=shutil.ignore_patterns('__pycache__'))
        release = json.loads((library / 'release.json').read_text(encoding='utf-8'))
        release['nativeActivationReleased'] = False
        (library / 'release.json').write_text(json.dumps(release), encoding='utf-8')
        parser = root / 'bundle' / 'parser'
        for name, version in (('pyte', '0.8.2'), ('wcwidth', '0.9.1')):
            (parser / name).mkdir(parents=True)
            (parser / name / '__init__.py').write_text('', encoding='utf-8')
            dist = parser / f'{name}-{version}.dist-info'
            dist.mkdir()
            (dist / 'METADATA').write_text(f'Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n', encoding='utf-8')
        home = root / 'home'
        home.mkdir(mode=0o700)
        socket_path = socket_path_for(root)
        command = [sys.executable, '-I', str(library / 'resident_main.py'), '--binary', str(root / 'agy'),
                   '--database', str(root / 'conversations.db'), '--socket', str(socket_path)]
        result = subprocess.run(command, env={'HOME': str(home), 'PATH': os.environ.get('PATH', '/usr/bin:/bin')},
                                cwd=str(root), capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn('Traceback', result.stderr)
        return result.stderr.strip().splitlines()[-1], result.stderr

    def test_live_control_socket_names_the_continuity_code(self):
        run = {}
        def live_socket(root):
            run['dir'] = root / 'run'
            run['dir'].mkdir(mode=0o700)
            run['live'] = bind_socket(run['dir'] / 'control.sock')
            self.addCleanup(run['live'].close)
            return run['dir'] / 'control.sock'
        last, stderr = self.run_entrypoint(live_socket)
        self.assertEqual(last, 'Managed Antigravity runtime is unavailable (ipc-already-exists).', stderr)
        self.assertTrue(accepts(run['dir'] / 'control.sock'))

    def test_os_error_keeps_the_generic_message_without_paths(self):
        def blocked_parent(root):
            blocker = root / 'run-file'
            blocker.write_bytes(b'fixture')
            return blocker / 'control.sock'
        last, stderr = self.run_entrypoint(blocked_parent)
        self.assertEqual(last, 'Managed Antigravity runtime is unavailable.', stderr)
        self.assertNotIn('run-file', stderr)


if __name__ == '__main__':
    unittest.main()
