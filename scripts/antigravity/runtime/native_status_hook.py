"""Uninstalled statusLine multiplexer. No import side effects or raw logging."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

from native_status_attestor import (MAX_NATIVE_STDIN, StatusError,
    project_native_status, send_projection, bounded_json, fail)


def read_original_command(path):
    try:
        leaf = Path(path).lstat()
        if (not stat.S_ISREG(leaf.st_mode) or leaf.st_uid != os.getuid() or
                leaf.st_nlink != 1 or stat.S_IMODE(leaf.st_mode) != 0o600):
            fail('status-original-command-unsafe')
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor, 'rb') as stream:
            if os.fstat(stream.fileno()).st_ino != leaf.st_ino: fail('status-original-command-changed')
            value = bounded_json(stream.read(8193), 8192)
        if set(value) != {'command'}: fail('status-original-command-invalid')
        command = value['command']
        if command is not None and (type(command) is not str or not command or
                len(command) > 4096 or '\x00' in command):
            fail('status-original-command-invalid')
        return command
    except StatusError: raise
    except OSError: fail('status-original-command-unavailable')


def forward_native_status(raw, socket_path, original_command, *, sender=send_projection,
                          runner=subprocess.run):
    # Native JSON is kept transiently so an existing user statusLine script sees
    # exactly the original stdin. Only the whitelist crosses the private socket.
    if type(raw) is not bytes or len(raw) > MAX_NATIVE_STDIN:
        fail('status-input-size')
    accepted = False
    try:
        projection = project_native_status(raw)
        sender(Path(socket_path), projection)
        accepted = True
    except StatusError:
        pass  # Status failure must not replace or print private native content.
    if original_command is not None:
        # Preserve the existing command's original stdin/stdout/stderr. Do not
        # impose a new timeout or terminate its process independently. A real
        # native probe must review the existing command before enabling this
        # multiplexing candidate; owned probe teardown bounds its whole family.
        try:
            result = runner(original_command, shell=True, input=raw, stdout=sys.stdout.buffer,
                            stderr=sys.stderr.buffer, check=False)
            return result.returncode, accepted
        except OSError:
            return 1, accepted
    return (0 if accepted else 1), accepted


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--socket', required=True)
    parser.add_argument('--original-command-file', required=True)
    args = parser.parse_args()
    try:
        original = read_original_command(args.original_command_file)
        raw = sys.stdin.buffer.read(MAX_NATIVE_STDIN + 1)
        code, _ = forward_native_status(raw, args.socket, original)
        return code
    except StatusError:
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
