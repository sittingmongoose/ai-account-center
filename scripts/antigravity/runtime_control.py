"""Bounded same-user private resident broker client. No action on import."""
import json
import os
from pathlib import Path
import socket
import stat
import struct
import time
import uuid

from runtime_installation import InstallationError, exact_json

MAX_FRAME = 262144


def request(installation, method, parameters=None, *, timeout=2):
    if method not in {'capability', 'census', 'inspect', 'stop', 'restart', 'prove', 'stop-owned-restarts',
                      'approve-idle-plan', 'approve-quiesced-stop', 'complete-transaction'}:
        raise InstallationError('runtime-method-invalid')
    path = Path(installation['socketPath'])
    parent, before = path.parent.lstat(), path.lstat()
    if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or
            stat.S_IMODE(parent.st_mode) != 0o700 or path.parent.resolve() != path.parent or
            not stat.S_ISSOCK(before.st_mode) or before.st_uid != os.getuid() or
            stat.S_IMODE(before.st_mode) != 0o600):
        raise InstallationError('runtime-endpoint-unsafe')
    request_id = str(uuid.uuid4())
    packet = {'requestId': request_id, 'method': method}
    packet.update(parameters or {})
    raw = json.dumps(packet, allow_nan=False).encode()
    if len(raw) > MAX_FRAME: raise InstallationError('runtime-request-size')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(path))
        _, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        after = path.lstat()
        if uid != os.getuid() or (after.st_dev, after.st_ino) != (before.st_dev, before.st_ino):
            raise InstallationError('runtime-endpoint-changed')
        connection.sendall(struct.pack('!I', len(raw)) + raw)
        def receive(count):
            result = bytearray()
            while len(result) < count:
                chunk = connection.recv(count - len(result))
                if not chunk: raise InstallationError('runtime-response-incomplete')
                result.extend(chunk)
            return bytes(result)
        size = struct.unpack('!I', receive(4))[0]
        if not 0 < size <= MAX_FRAME: raise InstallationError('runtime-response-size')
        result = exact_json(receive(size))
        if type(result) is not dict or set(result) != {'requestId', 'result'} or result['requestId'] != request_id:
            raise InstallationError('runtime-response-invalid')
        return result['result']


def service_readiness(installation):
    deadline = time.monotonic() + 5
    while True:
        try:
            result = request(installation, 'capability')
            if (type(result) is dict and set(result) == {'canProveRuntimeIdentity'} and
                    type(result['canProveRuntimeIdentity']) is bool):
                return {'serviceReady': True, 'nativeCapability': result['canProveRuntimeIdentity']}
        except (InstallationError, OSError, ValueError):
            pass
        if time.monotonic() >= deadline:
            return {'serviceReady': False, 'nativeCapability': False}
        time.sleep(.1)
