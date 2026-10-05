#!/usr/bin/env python3
"""Explicit Ubuntu runtime setup. --plan is read-only; installation is gated.

This is deliberately not an npm lifecycle action. The original native binary,
shared HOME, credentials, history and original statusLine are retained.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_installation import InstallationError, exact_json


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        while raw := stream.read(1024 * 1024): digest.update(raw)
    return digest.hexdigest()


def verify_public_sources(source):
    source = Path(source).resolve()
    manifest_raw = (source / 'runtime-manifest.json').read_bytes()
    manifest = exact_json(manifest_raw)
    if type(manifest) is not dict or set(manifest) != {'schemaVersion', 'files'} or manifest['schemaVersion'] != 1:
        raise InstallationError('runtime-package-invalid')
    if type(manifest['files']) is not list or not manifest['files']:
        raise InstallationError('runtime-package-invalid')
    names = []
    for row in manifest['files']:
        if type(row) is not dict or set(row) != {'path', 'sha256'}:
            raise InstallationError('runtime-package-invalid')
        name = row['path']
        if type(name) is not str or Path(name).name != name or name in names:
            raise InstallationError('runtime-package-invalid')
        names.append(name)
        path = source / name
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or path.is_symlink() or sha(path) != row['sha256']:
            raise InstallationError('runtime-package-changed')
    release = exact_json((source / 'release.json').read_bytes())
    if type(release) is not dict: raise InstallationError('runtime-package-invalid')
    return manifest, release, hashlib.sha256(manifest_raw).hexdigest()


def private_directory(path):
    path = Path(path)
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = path.lstat()
    if (not stat.S_ISDIR(info.st_mode) or path.resolve() != path or
            info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700):
        raise InstallationError('runtime-private-directory-required')


def write_exclusive(path, raw, mode=0o600):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, mode)
    with os.fdopen(descriptor, 'wb') as stream:
        stream.write(raw); stream.flush(); os.fsync(stream.fileno())


def prepare_parser(bundle, library, runner):
    """Create the bundle venv and install the pinned parser into bundle/parser.

    The venv supplies the launcher/helper interpreter path. pyte is pure Python
    and wcwidth is a CPython stable-ABI (abi3, 3.10+) wheel, so both load under
    any later CPython 3 minor version. They go into a version-neutral directory
    rather than the venv's lib/pythonX.Y/site-packages: a later system Python
    upgrade cannot orphan them. The final check runs the service interpreter
    (/usr/bin/python3 -I -B) exactly as the resident service will load them.
    """
    bundle, library = Path(bundle), Path(library)
    environment = dict(os.environ)
    for key in ('PYTHONPATH', 'PYTHONHOME', 'PIP_INDEX_URL', 'PIP_EXTRA_INDEX_URL', 'PIP_TRUSTED_HOST'):
        environment.pop(key, None)
    environment.update(PIP_CONFIG_FILE='/dev/null', PIP_DISABLE_PIP_VERSION_CHECK='1')
    runner(['/usr/bin/python3', '-I', '-m', 'venv', str(bundle / 'venv')], env=environment, check=True)
    python = bundle / 'venv/bin/python3'
    runner([str(python), '-I', '-m', 'pip', 'install', '--require-hashes', '--no-deps',
            '--only-binary=:all:', '--index-url', 'https://pypi.org/simple',
            '--target', str(bundle / 'parser'),
            '-r', str(library / 'requirements.txt')], env=environment, check=True)
    runner(['/usr/bin/python3', '-I', '-B', str(Path(__file__).with_name('runtime_health.py')),
            '--require-ok', str(bundle)], env=environment, check=True)
    return python


def setup_plan(home, source):
    home = Path(home).resolve()
    manifest, release, digest = verify_public_sources(source)
    binary = home / '.local/bin/agy'
    info = binary.lstat()
    if not stat.S_ISREG(info.st_mode) or binary.is_symlink() or not os.access(binary, os.X_OK):
        raise InstallationError('runtime-native-binary-unsafe')
    fingerprint = sha(binary)
    if release.get('nativeSha256') != fingerprint:
        raise InstallationError('runtime-native-binary-changed')
    root = home / '.local/share/ai-account-center/antigravity-runtime'
    return {
        'schemaVersion': 1,
        'bundleDirectory': str(root / 'bundles' / digest),
        'nativeBinary': str(binary), 'nativeSha256': fingerprint,
        'socketPath': str(home / '.ccs/antigravity-runtime/control.sock'),
    }, manifest, release


def install(home, source, *, runner=subprocess.run):
    home, source = Path(home).resolve(), Path(source).resolve()
    planned, manifest, release = setup_plan(home, source)
    # Changing nativeActivationReleased requires an independently reviewed
    # actual native hook/identity/session/restart gate, not a UI setting.
    proof = release.get('nativeProofReceiptSha256')
    if release.get('nativeActivationReleased') is not True or type(proof) is not str or len(proof) != 64:
        raise InstallationError('runtime-native-gate-pending')
    bundle = Path(planned['bundleDirectory'])
    root = bundle.parent.parent
    state = home / '.ccs/antigravity-switching'
    endpoint = home / '.ccs/antigravity-runtime'
    for directory in (root, bundle.parent, state, endpoint): private_directory(directory)
    preparation = state / 'runtime-preparation.json'
    descriptor = state / 'runtime-installation.json'
    shim_path = root / 'bin/agy'
    if preparation.exists() or preparation.is_symlink():
        raise InstallationError('runtime-preparation-recovery-required')
    if descriptor.exists() or descriptor.is_symlink() or shim_path.exists() or shim_path.is_symlink():
        raise InstallationError('runtime-installation-already-present')
    # Existing installations require a separate reviewed upgrade, never an
    # overwrite of live source, resident PTYs, saved settings or rollback data.
    bundle.mkdir(mode=0o700)
    owned = bundle.lstat()
    write_exclusive(preparation, (json.dumps({'schemaVersion': 1,
        'bundleDirectory': str(bundle), 'device': str(owned.st_dev), 'inode': str(owned.st_ino),
        'phase': 'preparing'}) + '\n').encode())
    library = bundle / 'lib'; library.mkdir(mode=0o700)
    for row in manifest['files']:
        shutil.copyfile(source / row['path'], library / row['path'])
    shutil.copyfile(source / 'runtime-manifest.json', library / 'runtime-manifest.json')
    # The launcher/discovery modules are copied from this package, never staging.
    for name in ('managed_launcher.py', 'runtime_installation.py'):
        shutil.copyfile(Path(__file__).with_name(name), bundle / name)
    python = prepare_parser(bundle, library, runner)
    # Runtime source remains versioned for resident processes and rollback even
    # if a later npm install replaces the global package directory.
    shim_dir = root / 'bin'; private_directory(shim_dir)
    shim = '#!/bin/sh\nexec ' + shlex.join([str(python), '-I', str(bundle / 'managed_launcher.py')]) + ' "$@"\n'
    write_exclusive(shim_dir / 'agy', shim.encode(), 0o700)
    write_exclusive(state / 'runtime-installation.json', (json.dumps(planned) + '\n').encode())
    # A descriptor is the commit boundary. Preserve the immutable bundle and
    # remove only its preparation marker after both launch inputs are published.
    preparation.unlink()
    return planned


def recover_preparation(home):
    """Explicit recovery of an incomplete, unadopted public runtime bundle.

    A final descriptor/shim or a foreign bundle inode requires review. This
    command never removes accounts, native credentials, history or native agy.
    """
    home = Path(home).resolve()
    state = home / '.ccs/antigravity-switching'
    marker = state / 'runtime-preparation.json'
    private_directory(state)
    info = marker.lstat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or
            stat.S_IMODE(info.st_mode) != 0o600 or info.st_nlink != 1 or info.st_size > 8192):
        raise InstallationError('runtime-preparation-invalid')
    raw = marker.read_bytes()
    receipt = exact_json(raw)
    if (type(receipt) is not dict or set(receipt) !=
            {'schemaVersion', 'bundleDirectory', 'device', 'inode', 'phase'} or
            receipt.get('schemaVersion') != 1 or receipt.get('phase') != 'preparing'):
        raise InstallationError('runtime-preparation-invalid')
    bundle = Path(receipt['bundleDirectory'])
    root = home / '.local/share/ai-account-center/antigravity-runtime'
    if (bundle.parent != root / 'bundles' or len(bundle.name) != 64 or
            any(char not in '0123456789abcdef' for char in bundle.name) or
            (state / 'runtime-installation.json').exists() or
            (state / 'runtime-installation.json').is_symlink() or
            (root / 'bin/agy').exists() or (root / 'bin/agy').is_symlink()):
        raise InstallationError('runtime-preparation-recovery-refused')
    try: owned = bundle.lstat()
    except FileNotFoundError:
        after = marker.lstat()
        if (after.st_dev, after.st_ino, after.st_mtime_ns) != (info.st_dev, info.st_ino, info.st_mtime_ns) or marker.read_bytes() != raw:
            raise InstallationError('runtime-preparation-recovery-refused')
        marker.unlink()  # Resume cleanup after an owned bundle was already removed.
        return
    if (not stat.S_ISDIR(owned.st_mode) or owned.st_uid != os.getuid() or
            stat.S_IMODE(owned.st_mode) != 0o700 or bundle.resolve() != bundle or
            str(owned.st_dev) != receipt['device'] or str(owned.st_ino) != receipt['inode']):
        raise InstallationError('runtime-preparation-recovery-refused')
    # rmtree does not follow child symlinks; permit venv's Python links but no
    # foreign-owned entries. The bundle is private and never contains auth data.
    for directory, subdirectories, filenames in os.walk(bundle, followlinks=False):
        for name in subdirectories + filenames:
            if (Path(directory) / name).lstat().st_uid != os.getuid():
                raise InstallationError('runtime-preparation-recovery-refused')
    after = marker.lstat()
    if (after.st_dev, after.st_ino, after.st_mtime_ns) != (info.st_dev, info.st_ino, info.st_mtime_ns) or marker.read_bytes() != raw:
        raise InstallationError('runtime-preparation-recovery-refused')
    shutil.rmtree(bundle)
    after = marker.lstat()
    if (after.st_dev, after.st_ino, after.st_mtime_ns) != (info.st_dev, info.st_ino, info.st_mtime_ns) or marker.read_bytes() != raw:
        raise InstallationError('runtime-preparation-recovery-refused')
    marker.unlink()


def shell_path_proposal(original, directory):
    """A reviewable reversible proposal; this function never edits shell files."""
    begin, end = '# AI Account Center Antigravity PATH begin', '# AI Account Center Antigravity PATH end'
    if type(original) is not bytes or begin.encode() in original or end.encode() in original:
        raise InstallationError('runtime-shell-profile-conflict')
    block = (begin + '\nexport PATH=' + shlex.quote(str(directory)) + ':"$PATH"\n' + end + '\n').encode()
    # Stock Ubuntu profiles prepend ~/.local/bin to PATH inside the original
    # bytes; append this managed block after those unchanged original bytes so
    # a new login shell resolves agy to the managed launcher first.
    if not original:
        return block
    if original.endswith(b'\n'):
        return original + block
    return original + b'\n' + block


def main():
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--apply', action='store_true')
    mode.add_argument('--plan', action='store_true')
    mode.add_argument('--recover-preparation', action='store_true')
    args = parser.parse_args()
    try:
        if sys.platform != 'linux': raise InstallationError('runtime-ubuntu-only')
        source = Path(__file__).with_name('runtime')
        if args.recover_preparation:
            recover_preparation(Path.home())
            print('Incomplete Antigravity runtime preparation removed; setup can be retried.')
        elif args.apply:
            install(Path.home(), source)
            # PATH, statusLine multiplexing and the user resident service are a
            # separate transactional adoption step after their concrete review.
            print('Antigravity runtime prepared; launch adoption still requires verification.')
        else:
            _, _, release = setup_plan(Path.home(), source)
            print(json.dumps({'ubuntuOnly': True, 'nativeActivationReleased': release.get('nativeActivationReleased') is True,
                              'originalBinaryPreserved': True, 'installed': False}))
        return 0
    except (InstallationError, OSError, ValueError, subprocess.SubprocessError):
        sys.stderr.write('Antigravity runtime setup could not complete its required checks.\n')
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
