#!/usr/bin/env python3
"""Bounded private-stdin Antigravity identity/quota worker.

Invoke with /usr/bin/python3 -I <trusted-package-script>. Standard output is a
private child-process protocol, never a log or direct HTTP body. Requests may
contain credentials; argv, environment, stderr and responses never contain them.
"""
from __future__ import annotations
import argparse
import base64
import datetime as dt
import io
import json
import pathlib
import re
import sys

# -I excludes both cwd and the script directory. Add this fixed packaged path,
# rather than accepting PYTHONPATH or a caller-supplied module search directory.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from auth_platforms import AuthError, IdentityMismatch, IdentityUnavailable, NeedsSignIn, credential, parse_credential, refresh_in_memory, verify_identity
from quota_adapter import SavedProfile, collect_snapshot, existing_collector_dependencies

MAX_REQUEST_BYTES = 65536
MAX_RESPONSE_BYTES = 65536


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result: raise AuthError('Invalid private request.')
        result[key] = value
    return result


def decode_request(raw: bytes) -> dict:
    if not isinstance(raw, bytes) or not 0 < len(raw) <= MAX_REQUEST_BYTES:
        raise AuthError('Invalid private request.')
    try:
        request = json.loads(raw, object_pairs_hook=unique_object)
        if not isinstance(request, dict): raise ValueError()
        common = {'operation', 'credentialBase64'}
        allowed = common if request.get('operation') == 'identity' else common | {'profileId', 'email', 'identityKey', 'credentialRevision'}
        if set(request) != allowed or request['operation'] not in ('identity', 'quota'):
            raise ValueError()
        if not isinstance(request['credentialBase64'], str) or len(request['credentialBase64']) > 22000:
            raise ValueError()
        request['_credential'] = credential(base64.b64decode(request['credentialBase64'], validate=True))
        if request['operation'] == 'quota':
            if (not isinstance(request['profileId'], str) or not re.fullmatch(r'[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}', request['profileId'])
                    or not isinstance(request['email'], str) or len(request['email']) > 254
                    or not re.fullmatch(r'[^\s@\x00-\x1f]+@[^\s@\x00-\x1f]+\.[^\s@\x00-\x1f]+', request['email'])
                    or not isinstance(request['identityKey'], str) or not re.fullmatch(r'[0-9a-f]{64}', request['identityKey'])
                    or not isinstance(request['credentialRevision'], str) or not re.fullmatch(r'[0-9a-f]{64}', request['credentialRevision'])):
                raise ValueError()
        return request
    except AuthError:
        raise
    except Exception:
        raise AuthError('Invalid private request.') from None


def process_request(request: dict, dependencies) -> dict:
    value = request['_credential']
    if request['operation'] == 'quota':
        profile = SavedProfile(request['profileId'], request['email'], request['identityKey'], request['credentialRevision'], value)
        return collect_snapshot(profile, dependencies)
    current = value
    now = dependencies.now()
    expiry = dt.datetime.fromisoformat(parse_credential(current.raw)['token']['expiry'].replace('Z', '+00:00'))
    if expiry <= now + dt.timedelta(seconds=30): current = refresh_in_memory(current, dependencies.refresh, now)
    identity = verify_identity(current, dependencies.userinfo, now)
    # This raw authoritative subject is private registry data. The TS caller
    # must discard it from every public response, telemetry and receipt.
    return {'email': identity.email, 'subject': identity.subject, 'plan': None,
            'verifiedAt': identity.verified_at, 'source': identity.source,
            'identityKey': identity.identity_key, 'credentialRevision': value.revision}


def handle_stream(stdin, stdout, stderr, dependencies_factory) -> int:
    try:
        raw = stdin.read(MAX_REQUEST_BYTES + 1)
        request = decode_request(raw)
        result = process_request(request, dependencies_factory())
        output = json.dumps(result, separators=(',', ':'), ensure_ascii=False, allow_nan=False).encode()
        if len(output) > MAX_RESPONSE_BYTES:
            raise AuthError('The normalized response exceeded its safe size limit.')
        stdout.write(output + b'\n')
        return 0
    except (NeedsSignIn, IdentityMismatch):
        stderr.write('Antigravity account identity needs sign-in.\n')
    except IdentityUnavailable:
        stderr.write('Antigravity account identity is temporarily unavailable.\n')
    except Exception:
        stderr.write('Antigravity private account request could not be completed.\n')
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description='Private Antigravity account worker')
    parser.add_argument('--collector-dir', type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent.parent / 'account-usage')
    options = parser.parse_args()
    # This trusted public package location is composed by the backend, never
    # supplied by an HTTP client. Credentials remain on private stdin.
    def dependencies():
        directory = options.collector_dir.resolve(strict=True)
        for name in ('desktop_usage.py', 'desktop_helpers.py'):
            path = directory / name
            if path.is_symlink() or not path.is_file(): raise AuthError('Required package helpers are unavailable.')
        sys.path.insert(0, str(directory))
        import desktop_usage, desktop_helpers
        return existing_collector_dependencies(desktop_usage, desktop_helpers, pathlib.Path.home())
    return handle_stream(sys.stdin.buffer, sys.stdout.buffer, sys.stderr, dependencies)

if __name__ == '__main__':
    raise SystemExit(main())
