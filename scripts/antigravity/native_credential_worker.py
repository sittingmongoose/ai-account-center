#!/usr/bin/env python3
"""Bounded private Ubuntu credential-store adapter. No action occurs on import.

Requests and responses are private child-process stdin/stdout, never HTTP/logs.
The switch transaction owns the activation lock and reviewed native stop.
"""
from __future__ import annotations
import argparse
import base64
import json
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from auth_platforms import (AuthError, FileSnapshot, InstallReceipt,
                            UbuntuFileStore, credential)

MAX_PACKET = 65536
MAX_CREDENTIAL = 16384
FORMAT = 'antigravity-consumer-json'


def fail():
    raise ValueError('antigravity-private-store-unavailable')


def bounded_json(raw):
    if type(raw) is not bytes or len(raw) > MAX_PACKET:
        fail()
    def pairs(values):
        result = {}
        for key, value in values:
            if key in result: fail()
            result[key] = value
        return result
    value = json.loads(raw, object_pairs_hook=pairs,
                       parse_constant=lambda _: fail())
    if type(value) is not dict: fail()
    return value


def decode(value):
    if type(value) is not str or len(value) > 22000: fail()
    try: raw = base64.b64decode(value, validate=True)
    except (ValueError, TypeError): fail()
    if not 0 < len(raw) <= MAX_CREDENTIAL: fail()
    return credential(raw)


def fingerprint(value):
    if type(value) is not str or not re.fullmatch('[a-f0-9]{64}', value): fail()
    return value


def snapshot_dto(value):
    # Nanosecond timestamps exceed JavaScript's safe integer range.
    return {'device': str(value.device), 'inode': str(value.inode),
            'modifiedNs': str(value.modified_ns),
            'credentialBase64': base64.b64encode(value.credential.raw).decode()}


def snapshot(value):
    if type(value) is not dict or set(value) != {
            'device', 'inode', 'modifiedNs', 'credentialBase64'}: fail()
    for key in ('device', 'inode', 'modifiedNs'):
        if type(value[key]) is not str or not re.fullmatch('[0-9]{1,24}', value[key]): fail()
    return FileSnapshot(decode(value['credentialBase64']), int(value['device']),
                        int(value['inode']), int(value['modifiedNs']))


def execute(packet, store):
    operation = packet.get('operation')
    if operation == 'read' and set(packet) == {'operation'}:
        value = store.read_current().credential
        return {'format': FORMAT, 'credentialBase64': base64.b64encode(value.raw).decode(),
                'revision': value.revision}
    if operation == 'install' and set(packet) == {
            'operation', 'credentialBase64', 'expectedFingerprint'}:
        expected = fingerprint(packet['expectedFingerprint'])
        replacement = decode(packet['credentialBase64'])
        before = store.read_current()
        if before.credential.revision != expected: fail()
        installed = store.install(replacement, before)
        return {'installedFingerprint': installed.installed.credential.revision,
                'rollbackState': {'before': snapshot_dto(installed.before),
                                  'installed': snapshot_dto(installed.installed)}}
    if operation == 'rollback' and set(packet) == {
            'operation', 'receipt', 'previousCredentialBase64'}:
        value = packet['receipt']
        if type(value) is not dict or set(value) != {'before', 'installed'}: fail()
        before, installed = snapshot(value['before']), snapshot(value['installed'])
        previous = decode(packet['previousCredentialBase64'])
        if previous.revision != before.credential.revision: fail()
        restored = store.rollback(InstallReceipt(before, installed))
        return {'restored': restored.installed.credential.revision == previous.revision}
    fail()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--home', required=True)
    args = parser.parse_args()
    try:
        home = Path(args.home)
        if not home.is_absolute() or sys.platform != 'linux': fail()
        packet = bounded_json(sys.stdin.buffer.read(MAX_PACKET + 1))
        result = execute(packet, UbuntuFileStore(home))
        raw = json.dumps(result, allow_nan=False, separators=(',', ':')).encode()
        if len(raw) > MAX_PACKET: fail()
        sys.stdout.buffer.write(raw)
        return 0
    except (AuthError, OSError, ValueError, TypeError, KeyError):
        # Do not expose private token data, paths or native store errors.
        sys.stderr.write('Antigravity private store request failed safely.\n')
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
