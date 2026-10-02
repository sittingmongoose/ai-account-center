"""Product-installed runtime discovery. No reads occur on import."""
from __future__ import annotations
import hashlib
import json
import os
from pathlib import Path
import re
import stat


class InstallationError(Exception):
    pass


def safe_file(path, limit):
    path = Path(path)
    parent, before = path.parent.lstat(), path.lstat()
    if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or
            stat.S_IMODE(parent.st_mode) != 0o700 or path.parent.resolve() != path.parent or
            not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or
            stat.S_IMODE(before.st_mode) != 0o600 or before.st_nlink != 1 or before.st_size > limit):
        raise InstallationError('runtime-installation-unsafe')
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as stream:
        opened = os.fstat(stream.fileno())
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            raise InstallationError('runtime-installation-changed')
        raw = stream.read(limit + 1)
    after = path.lstat()
    if len(raw) > limit or (after.st_dev, after.st_ino, after.st_mtime_ns) != (
            before.st_dev, before.st_ino, before.st_mtime_ns):
        raise InstallationError('runtime-installation-changed')
    return raw


def exact_json(raw):
    def pairs(values):
        result = {}
        for key, value in values:
            if key in result: raise InstallationError('runtime-installation-invalid')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=pairs,
                      parse_constant=lambda _: (_ for _ in ()).throw(InstallationError('runtime-installation-invalid')))


def load_installation(home=None, ccs_directory=None, *, require_native_pin=True):
    home = Path(home or Path.home()).resolve()
    ccs = Path(ccs_directory or home / '.ccs').resolve()
    value = exact_json(safe_file(ccs / 'antigravity-switching/runtime-installation.json', 8192))
    if (type(value) is not dict or set(value) != {
            'schemaVersion', 'bundleDirectory', 'nativeBinary', 'nativeSha256', 'socketPath'} or
            type(value['schemaVersion']) is not int or value['schemaVersion'] != 1 or
            any(type(value[key]) is not str for key in set(value) - {'schemaVersion'})):
        raise InstallationError('runtime-installation-invalid')
    bundle = Path(value['bundleDirectory'])
    root = home / '.local/share/ai-account-center/antigravity-runtime/bundles'
    if (bundle.parent != root or not re.fullmatch('[a-f0-9]{64}', bundle.name) or
            bundle.resolve() != bundle or value['nativeBinary'] != str(home / '.local/bin/agy') or
            not re.fullmatch('[a-f0-9]{64}', value['nativeSha256']) or
            value['socketPath'] != str(ccs / 'antigravity-runtime/control.sock')):
        raise InstallationError('runtime-installation-invalid')
    native = Path(value['nativeBinary'])
    before = native.lstat()
    if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or
            before.st_mode & 0o022 or not before.st_mode & 0o111):
        raise InstallationError('runtime-native-binary-unsafe')
    # Leave the native binary at its original path; update and rollback semantics
    # remain the official executable's own behavior.
    digest = hashlib.sha256()
    with native.open('rb') as stream:
        while raw := stream.read(1024 * 1024): digest.update(raw)
    after = native.lstat()
    if (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size) != (
            before.st_dev, before.st_ino, before.st_mtime_ns, before.st_size) or (
            require_native_pin and digest.hexdigest() != value['nativeSha256']):
        raise InstallationError('runtime-native-binary-changed')
    return value


def runtime_release(bundle):
    value = exact_json((Path(bundle) / 'lib/release.json').read_bytes())
    return (type(value) is dict and value.get('nativeActivationReleased') is True and
            type(value.get('nativeProofReceiptSha256')) is str and
            re.fullmatch('[a-f0-9]{64}', value['nativeProofReceiptSha256']) is not None)


def launcher_argv(installation, args, *, native_pin_matches=True):
    native = installation['nativeBinary']
    controls = {'--version', '-v', '--help', '-h', 'help', 'update', 'login', 'logout', 'auth'}
    if args and args[0] in controls:
        return [native, *args]
    # Preserve native behavior for flags/prompts outside the bounded managed
    # grammar. Such sessions remain unmanaged and cannot be stopped by switching.
    index = 0
    while index < len(args):
        if args[index] not in ('--conversation', '--project') or index + 1 >= len(args):
            return [native, *args]
        index += 2
    bundle = Path(installation['bundleDirectory'])
    if not runtime_release(bundle) or native_pin_matches is not True:
        return [native, *args]
    return [str(bundle / 'venv/bin/python3'), '-I', str(bundle / 'lib/foreground_launcher.py'),
            '--socket', installation['socketPath'], *args]
