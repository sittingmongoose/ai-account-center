"""Uninstalled foreground wrapper candidate for normal `cd <project> && agy`.

The user-invoked wrapper sends original cwd/argv/env only to its same-user private
resident broker. It forwards terminal bytes once, restores tty settings, never
records/replays input, never mutates auth and never replaces the native binary.
"""
import base64
import json
import os
from pathlib import Path
import selectors
import signal
import socket
import stat
import struct
import sys
import termios
import tty
import uuid
from collections import deque
sys.path.insert(0, str(Path(__file__).resolve().parent))

from resident_broker import encode_frame, extract_frame, ContinuityError, MAX_FRAME


def validate_endpoint(path):
    location = Path(path)
    directory = location.parent.lstat(); info = location.lstat()
    if (not stat.S_ISDIR(directory.st_mode) or directory.st_uid != os.getuid() or
            stat.S_IMODE(directory.st_mode) != 0o700 or not stat.S_ISSOCK(info.st_mode) or
            info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600):
        raise ContinuityError('ipc-endpoint-unsafe')


def main(socket_path, args):
    validate_endpoint(socket_path)
    if not os.isatty(0) or not os.isatty(1):raise ContinuityError('foreground-terminal-required')
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.connect(socket_path)
    _, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    if uid != os.getuid():raise ContinuityError('ipc-peer-unsafe')
    connection.sendall(encode_frame({'requestId': str(uuid.uuid4()), 'method': 'launch',
                                    'args': args, 'cwd': os.getcwd(),
                                    'environment': dict(os.environ)}))
    settings = termios.tcgetattr(0)
    selector = selectors.DefaultSelector(); pending = bytearray(); outgoing = deque()
    sending = b''; sent = 0
    try:
        tty.setraw(0)
        connection.setblocking(False)
        selector.register(connection, selectors.EVENT_READ, 'socket')
        selector.register(0, selectors.EVENT_READ, 'stdin')
        started = False
        while True:
            for key, events in selector.select():
                if key.data == 'stdin':
                    raw = os.read(0, 16384)
                    if not raw:return 0
                    if not started:raise ContinuityError('foreground-not-ready')
                    outgoing.append(encode_frame({'requestId': str(uuid.uuid4()), 'method': 'input',
                                                  'bytes': base64.b64encode(raw).decode()}))
                    if sum(map(len, outgoing)) + len(sending) - sent > MAX_FRAME:raise ContinuityError('foreground-input-budget')
                    selector.modify(connection, selectors.EVENT_READ | selectors.EVENT_WRITE, 'socket')
                else:
                    if events & selectors.EVENT_WRITE:
                        if not sending and outgoing:sending = outgoing.popleft(); sent = 0
                        if sending:
                            sent += connection.send(sending[sent:])
                            if sent == len(sending):sending = b''; sent = 0
                        if not outgoing and not sending:selector.modify(connection, selectors.EVENT_READ, 'socket')
                    if events & selectors.EVENT_READ:
                        raw = connection.recv(65536)
                        if not raw:raise ContinuityError('resident-broker-disconnected')
                        pending.extend(raw)
                        while (message := extract_frame(pending)) is not None:
                            if message.get('error'):raise ContinuityError('managed-runtime-request-failed')
                            if message.get('switchState') == 'stopping':
                                # Drop pending terminal input throughout the
                                # transaction; never replay into another account.
                                outgoing.clear()
                                # An already-partially-sent framed input must
                                # finish once to preserve IPC framing. The
                                # broker blocks it and invalidates the receipt.
                                if sent == 0:sending = b''
                                try:selector.unregister(0)
                                except KeyError:pass
                            elif message.get('switchState') == 'resumed':
                                termios.tcflush(0, termios.TCIFLUSH)
                                try:selector.register(0, selectors.EVENT_READ, 'stdin')
                                except KeyError:pass
                            if message.get('result', {}).get('inputBlocked'):
                                os.write(1, b'\x07')  # No dropped input is replayed.
                            if message.get('result', {}).get('sessionToken'):
                                started = True
                                rows, columns = os.get_terminal_size(0).lines, os.get_terminal_size(0).columns
                                outgoing.append(encode_frame({'requestId': str(uuid.uuid4()), 'method': 'resize',
                                                              'rows': rows, 'columns': columns}))
                                selector.modify(connection, selectors.EVENT_READ | selectors.EVENT_WRITE, 'socket')
                            if 'terminalOutput' in message:
                                output = base64.b64decode(message['terminalOutput'], validate=True)
                                view = memoryview(output)
                                while view:
                                    written = os.write(1, view); view = view[written:]
                            if 'nativeExit' in message:return int(message['nativeExit'])
    finally:
        termios.tcsetattr(0, termios.TCSADRAIN, settings)
        selector.close(); connection.close()


if __name__ == '__main__':
    # Explicit staging invocation only. No PATH/install/systemd modifications.
    if len(sys.argv) < 3 or sys.argv[1] != '--socket':
        print('Usage: foreground_launcher.py --socket PRIVATE_SOCKET [native arguments]', file=sys.stderr)
        raise SystemExit(2)
    try:raise SystemExit(main(sys.argv[2], sys.argv[3:]))
    except (ContinuityError, OSError, ValueError):
        # Do not print exception paths, terminal bytes or private argument values.
        print('Managed Antigravity launcher could not preserve this session.', file=sys.stderr)
        raise SystemExit(1)
