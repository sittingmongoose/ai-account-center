#!/usr/bin/env python3
"""Reviewed runtime bundle rebuild after a native auto-update.

No action occurs on import. --plan is read-only. --apply builds a new
immutable bundle from the current packaged runtime sources when the installed
CLI moved to another reviewed build, then atomically repoints the launcher
shim, the user service unit and the installation descriptor at it. Anything
unreviewed, foreign, mid-switch or unexpected refuses before any owned write,
and every cutover failure restores the previous owned bytes.
"""
from __future__ import annotations
import argparse
import base64
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_installation import InstallationError, exact_json, load_installation
from install_runtime import private_directory, setup_plan, sha, write_exclusive
from adopt_runtime import UNIT, publish, restore, run_user_service, same, snapshot, unit_bytes
from runtime_control import service_readiness

VERSION_RE = re.compile(r'\d+\.\d+\.\d+(?:-[\w.-]+)?$')
SHA_RE = re.compile(r'[a-f0-9]{64}$')
JOURNAL_LIMIT = 4 * 1024 * 1024


def expected_shim(bundle):
    python = Path(bundle) / 'venv/bin/python3'
    return ('#!/bin/sh\nexec ' + shlex.join([str(python), '-I', str(Path(bundle) / 'managed_launcher.py')]) +
            ' "$@"\n').encode()


def bundle_pin(bundle):
    bundle = Path(bundle)
    try:
        release = exact_json((bundle / 'lib/release.json').read_bytes())
    except (OSError, ValueError, InstallationError):
        raise InstallationError('runtime-bundle-invalid')
    pin = release.get('nativeSha256') if type(release) is dict else None
    if type(pin) is not str or not SHA_RE.fullmatch(pin):
        raise InstallationError('runtime-bundle-invalid')
    version = release.get('nativeVersion')
    return pin, version if type(version) is str else 'unknown'


def current_transaction(home):
    directory = Path(home) / '.ccs/antigravity-profiles'
    try:
        names = sorted(name for name in os.listdir(directory)
                       if name.startswith('registry-') and name.endswith('.json'))
    except FileNotFoundError:
        return None
    except (OSError, ValueError):
        raise InstallationError('runtime-registry-unreadable')
    if not names:
        return None
    try:
        state = exact_json((directory / names[-1]).read_bytes())
    except (OSError, ValueError, InstallationError):
        raise InstallationError('runtime-registry-unreadable')
    if type(state) is not dict or 'transaction' not in state:
        raise InstallationError('runtime-registry-unreadable')
    return state['transaction']


def load_checkpoint(home):
    path = Path(home) / '.ccs/antigravity-switching/runtime-adoption.json'
    try:
        raw = base64.b64decode(snapshot(path, limit=JOURNAL_LIMIT)['rawBase64'])
    except FileNotFoundError:
        raise InstallationError('runtime-not-adopted')
    receipt = exact_json(raw)
    if type(receipt) is not dict or type(receipt.get('changes')) is not list:
        raise InstallationError('runtime-adoption-checkpoint-invalid')
    return receipt


def change_for(receipt, path):
    for change in receipt['changes']:
        if type(change) is dict and type(change.get('before')) is dict and change['before'].get('path') == path:
            return change
    return None


def plan(home, source):
    home, source = Path(home).resolve(), Path(source).resolve()
    try:
        newplan, manifest, release = setup_plan(home, source)
    except InstallationError as error:
        if str(error) == 'runtime-native-binary-changed':
            raise InstallationError('runtime-native-unreviewed')
        raise
    version = release.get('nativeVersion')
    if type(version) is not str or not VERSION_RE.fullmatch(version) or len(version) > 128:
        raise InstallationError('runtime-package-invalid')
    try:
        installed = load_installation(home)
    except InstallationError as error:
        if str(error) == 'runtime-native-binary-changed':
            raise InstallationError('runtime-descriptor-stale')
        raise
    binary_fp = sha(Path(installed['nativeBinary']))
    old_bundle = Path(installed['bundleDirectory'])
    try:
        owned = old_bundle.lstat()
    except FileNotFoundError:
        raise InstallationError('runtime-bundle-unsafe')
    if (not stat.S_ISDIR(owned.st_mode) or owned.st_uid != os.getuid() or
            old_bundle.resolve() != old_bundle):
        raise InstallationError('runtime-bundle-unsafe')
    old_pin, old_version = bundle_pin(old_bundle)
    if current_transaction(home) is not None:
        raise InstallationError('runtime-transaction-active')
    journal = home / '.ccs/antigravity-switching/runtime-rebuild.json'
    if journal.exists() or journal.is_symlink():
        try:
            finished = exact_json(journal.read_bytes()).get('phase') == 'rebuilt'
        except (OSError, ValueError, InstallationError):
            finished = False
        if not finished:
            raise InstallationError('runtime-rebuild-recovery-required')
    root = old_bundle.parent.parent
    shim, unit = root / 'bin/agy', home / '.config/systemd/user' / UNIT
    try:
        shim_snapped = snapshot(shim)
    except FileNotFoundError:
        raise InstallationError('runtime-installation-invalid')
    try:
        unit_snapped = snapshot(unit)
    except FileNotFoundError:
        raise InstallationError('runtime-not-adopted')
    if base64.b64decode(shim_snapped['rawBase64']) != expected_shim(old_bundle):
        raise InstallationError('runtime-installation-foreign')
    if base64.b64decode(unit_snapped['rawBase64']) != unit_bytes(installed):
        raise InstallationError('runtime-installation-foreign')
    receipt = load_checkpoint(home)
    unit_change = change_for(receipt, str(unit))
    recorded = unit_change.get('installed') if unit_change is not None else None
    if type(recorded) is not dict or not same(unit_snapped, recorded):
        raise InstallationError('runtime-installation-foreign')
    new_bundle = Path(newplan['bundleDirectory'])
    if new_bundle == old_bundle:
        if old_pin != binary_fp:
            raise InstallationError('runtime-bundle-changed')
        return {'stale': False, 'version': version, 'oldVersion': old_version,
                'bundleDirectory': str(old_bundle), 'nativeSha256': binary_fp}
    if old_pin == binary_fp:
        return {'stale': False, 'version': version, 'oldVersion': old_version,
                'bundleDirectory': str(old_bundle), 'nativeSha256': binary_fp}
    if new_bundle.exists() or new_bundle.is_symlink():
        raise InstallationError('runtime-bundle-already-present')
    return {'stale': True, 'version': version, 'oldVersion': old_version,
            'oldBundle': str(old_bundle), 'newBundle': str(new_bundle),
            'oldPin': old_pin, 'newPin': binary_fp, 'manifest': manifest,
            'shim': str(shim), 'unit': str(unit)}


def build_bundle(new_bundle, source, manifest, *, runner):
    new_bundle = Path(new_bundle)
    new_bundle.mkdir(mode=0o700)
    library = new_bundle / 'lib'
    library.mkdir(mode=0o700)
    for row in manifest['files']:
        shutil.copyfile(Path(source) / row['path'], library / row['path'])
    shutil.copyfile(Path(source) / 'runtime-manifest.json', library / 'runtime-manifest.json')
    for name in ('managed_launcher.py', 'runtime_installation.py'):
        shutil.copyfile(Path(__file__).with_name(name), new_bundle / name)
    environment = dict(os.environ)
    for key in ('PYTHONPATH', 'PYTHONHOME', 'PIP_INDEX_URL', 'PIP_EXTRA_INDEX_URL', 'PIP_TRUSTED_HOST'):
        environment.pop(key, None)
    environment.update(PIP_CONFIG_FILE='/dev/null', PIP_DISABLE_PIP_VERSION_CHECK='1')
    runner(['/usr/bin/python3', '-I', '-m', 'venv', str(new_bundle / 'venv')], env=environment, check=True)
    python = new_bundle / 'venv/bin/python3'
    runner([str(python), '-I', '-m', 'pip', 'install', '--require-hashes', '--no-deps',
            '--only-binary=:all:', '--index-url', 'https://pypi.org/simple',
            '-r', str(library / 'requirements.txt')], env=environment, check=True)
    runner([str(python), '-I', '-c',
            "import importlib.metadata as m; assert m.version('pyte')=='0.8.2'; assert m.version('wcwidth')=='0.9.1'"],
           env=environment, check=True)


def apply(home, source, *, runner=subprocess.run, readiness=service_readiness):
    home = Path(home).resolve()
    info = plan(home, source)
    if not info['stale']:
        return {'status': 'current', 'version': info['version']}
    if readiness is None:
        raise InstallationError('runtime-readiness-provider-unavailable')
    state = home / '.ccs/antigravity-switching'
    journal_path = state / 'runtime-rebuild.json'
    old_bundle, new_bundle = Path(info['oldBundle']), Path(info['newBundle'])
    shim, unit = Path(info['shim']), Path(info['unit'])
    descriptor_path = state / 'runtime-installation.json'
    checkpoint_path = state / 'runtime-adoption.json'
    before = {'shim': snapshot(shim), 'unit': snapshot(unit),
              'descriptor': snapshot(descriptor_path), 'checkpoint': snapshot(checkpoint_path, limit=JOURNAL_LIMIT)}
    if journal_path.exists() or journal_path.is_symlink():
        try:
            finished = exact_json(journal_path.read_bytes()).get('phase') == 'rebuilt'
        except (OSError, ValueError, InstallationError):
            finished = False
        if not finished:
            raise InstallationError('runtime-rebuild-recovery-required')
        journal_path.unlink()
    journal = {'schemaVersion': 1, 'phase': 'building', 'oldBundle': str(old_bundle),
               'newBundle': str(new_bundle), 'oldPin': info['oldPin'], 'newPin': info['newPin'],
               'version': info['version'], 'before': before, 'changes': []}
    write_exclusive(journal_path, (json.dumps(journal) + '\n').encode())
    journal_snapshot = snapshot(journal_path, limit=JOURNAL_LIMIT)
    stopped = False

    def persist():
        nonlocal journal_snapshot
        raw = (json.dumps(journal) + '\n').encode()
        if len(raw) > JOURNAL_LIMIT:
            raise InstallationError('runtime-rebuild-journal-size')

        def committed(known):
            nonlocal journal_snapshot
            journal_snapshot = known
        publish(journal_snapshot, raw, on_committed=committed)

    def apply_change(original, raw, mode=None):
        def committed(known):
            journal['changes'].append({'before': original, 'installed': known})
            persist()
        return publish(original, raw, mode, on_committed=committed)

    try:
        private_directory(new_bundle.parent)
        build_bundle(new_bundle, Path(source).resolve(), info['manifest'], runner=runner)
        journal['phase'] = 'cutover'
        persist()
        run_user_service(runner, 'stop', UNIT)
        stopped = True
        installed_unit = apply_change(before['unit'], unit_bytes({
            'bundleDirectory': str(new_bundle), 'nativeBinary': str(home / '.local/bin/agy'),
            'socketPath': str(home / '.ccs/antigravity-runtime/control.sock')}))
        apply_change(before['shim'], expected_shim(new_bundle))
        previous_raw = base64.b64decode(before['descriptor']['rawBase64'])
        backup_parent = state / 'descriptor-backups'
        private_directory(backup_parent)
        backup = backup_parent / (info['oldPin'] + '.json')
        if backup.exists() or backup.is_symlink():
            if backup.read_bytes() != previous_raw:
                raise InstallationError('runtime-backup-divergent')
        else:
            write_exclusive(backup, previous_raw)
        renewed = dict(exact_json(previous_raw))
        renewed['bundleDirectory'] = str(new_bundle)
        renewed['nativeSha256'] = info['newPin']
        apply_change(before['descriptor'], (json.dumps(renewed) + '\n').encode())
        receipt = exact_json(base64.b64decode(before['checkpoint']['rawBase64']))
        recorded = change_for(receipt, str(unit))
        if recorded is None:
            raise InstallationError('runtime-adoption-checkpoint-invalid')
        recorded['installed'] = installed_unit
        apply_change(before['checkpoint'], (json.dumps(receipt) + '\n').encode())
        run_user_service(runner, 'daemon-reload')
        run_user_service(runner, 'start', UNIT)
        ready = readiness({'bundleDirectory': str(new_bundle),
                           'nativeBinary': str(home / '.local/bin/agy'),
                           'nativeSha256': info['newPin'],
                           'socketPath': str(home / '.ccs/antigravity-runtime/control.sock')})
        if (type(ready) is not dict or ready.get('serviceReady') is not True or
                type(ready.get('nativeCapability')) is not bool):
            raise InstallationError('runtime-service-readiness-failed')
        journal['phase'] = 'rebuilt'
        persist()
        return {'status': 'rebuilt', 'version': info['version']}
    except BaseException:
        try:
            for change in reversed(journal['changes']):
                restore(change)
            if stopped or journal['changes']:
                run_user_service(runner, 'daemon-reload')
                run_user_service(runner, 'start', UNIT)
            journal['phase'] = 'failed'
            persist()
        except BaseException:
            pass
        raise


def recover_rebuild(home):
    home = Path(home).resolve()
    state = home / '.ccs/antigravity-switching'
    journal_path = state / 'runtime-rebuild.json'
    try:
        owner = snapshot(journal_path, limit=JOURNAL_LIMIT)
    except FileNotFoundError:
        raise InstallationError('runtime-rebuild-nothing-to-recover')
    journal = exact_json(base64.b64decode(owner['rawBase64']))
    if (type(journal) is not dict or journal.get('schemaVersion') != 1 or
            journal.get('phase') not in {'building', 'cutover', 'failed'} or
            type(journal.get('before')) is not dict or
            set(journal['before']) != {'shim', 'unit', 'descriptor', 'checkpoint'}):
        raise InstallationError('runtime-rebuild-journal-invalid')
    root = home / '.local/share/ai-account-center/antigravity-runtime/bundles'
    new_bundle = Path(journal.get('newBundle', ''))
    if (new_bundle.parent != root or not SHA_RE.fullmatch(new_bundle.name) or
            new_bundle.resolve() != new_bundle):
        raise InstallationError('runtime-rebuild-journal-invalid')
    for key in ('shim', 'unit', 'descriptor', 'checkpoint'):
        current = snapshot(Path(journal['before'][key]['path']), missing=True, limit=JOURNAL_LIMIT)
        if not same(current, journal['before'][key]):
            raise InstallationError('runtime-rebuild-diverged')
    if new_bundle.exists() or new_bundle.is_symlink():
        if not new_bundle.is_dir() or new_bundle.resolve() != new_bundle:
            raise InstallationError('runtime-rebuild-diverged')
        shutil.rmtree(new_bundle)
    if not same(snapshot(journal_path, limit=JOURNAL_LIMIT), owner):
        raise InstallationError('runtime-rebuild-diverged')
    journal_path.unlink()
    return {'status': 'recovered'}


HINTS = {
    'runtime-native-unreviewed': 'installed CLI is not the reviewed build',
    'runtime-descriptor-stale': 'run: ai-account-center antigravity runtime refresh',
    'runtime-transaction-active': 'finish or recover the switch first: ai-account-center antigravity recover',
    'runtime-rebuild-recovery-required': 'review, then: rebuild_bundle.py --recover-rebuild',
    'runtime-bundle-already-present': 'a new bundle already exists; review before retrying',
    'runtime-not-adopted': 'adopt the runtime first',
    'runtime-installation-foreign': 'owned files changed; review before retrying',
    'runtime-backup-divergent': 'a divergent descriptor backup exists; review before retrying',
    'runtime-service-readiness-failed': 'previous owned bytes restored; review the service',
}


def main():
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--plan', action='store_true')
    mode.add_argument('--apply', action='store_true')
    mode.add_argument('--recover-rebuild', action='store_true')
    args = parser.parse_args()
    try:
        if sys.platform != 'linux':
            raise InstallationError('runtime-ubuntu-only')
        home = Path.home()
        source = Path(__file__).with_name('runtime')
        if args.recover_rebuild:
            recover_rebuild(home)
            print('Incomplete Antigravity bundle rebuild removed; rebuild can be retried.')
            return 0
        if args.apply:
            result = apply(home, source)
        else:
            info = plan(home, source)
            result = {'status': 'current' if not info['stale'] else 'stale', 'version': info['version'],
                      'oldVersion': info.get('oldVersion')}
        if result['status'] == 'current':
            print('Antigravity runtime bundle is current (reviewed %s build).' % result['version'])
        elif result['status'] == 'stale':
            print('Antigravity runtime bundle is stale (%s -> %s); --apply rebuilds it.' % (
                result.get('oldVersion'), result['version']))
        else:
            print('Antigravity runtime bundle rebuilt to the reviewed %s build.' % result['version'])
        return 0
    except (InstallationError, OSError, ValueError, subprocess.SubprocessError) as error:
        code = str(error) if isinstance(error, InstallationError) else error.__class__.__name__
        hint = HINTS.get(code)
        sys.stderr.write('Antigravity bundle rebuild could not complete (%s)%s.\n' % (
            code, ': ' + hint if hint else ''))
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
