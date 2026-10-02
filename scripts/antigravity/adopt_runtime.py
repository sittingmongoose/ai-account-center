#!/usr/bin/env python3
"""Explicit transactional Ubuntu shell/statusLine/user-service adoption.

No action occurs on import. Live use remains blocked by the packaged native
gate. Every rollback compares the exact bytes and inode we published first.
"""
from __future__ import annotations
import argparse
import base64
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_installation import InstallationError, exact_json, load_installation, runtime_release, safe_file
from install_runtime import shell_path_proposal, write_exclusive
from runtime_control import service_readiness

UNIT = 'ai-account-center-antigravity.service'


def snapshot(path, *, missing=False, limit=262144):
    path = Path(path)
    if path.parent.resolve() != path.parent:
        raise InstallationError('runtime-adoption-parent-unsafe')
    try: before = path.lstat()
    except FileNotFoundError:
        if missing: return {'existed': False, 'path': str(path)}
        raise
    if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or
            before.st_nlink != 1 or before.st_mode & 0o022 or before.st_size > limit):
        raise InstallationError('runtime-adoption-file-unsafe')
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as stream:
        opened = os.fstat(stream.fileno())
        raw = stream.read(limit + 1)
    after = path.lstat()
    identity = lambda x: (x.st_dev, x.st_ino, x.st_mtime_ns, x.st_size)
    if identity(before) != identity(opened) or identity(before) != identity(after) or len(raw) > limit:
        raise InstallationError('runtime-adoption-file-changed')
    return {'existed': True, 'path': str(path), 'device': str(before.st_dev),
            'inode': str(before.st_ino), 'mode': stat.S_IMODE(before.st_mode),
            'atimeNs': str(before.st_atime_ns), 'mtimeNs': str(before.st_mtime_ns),
            'rawBase64': base64.b64encode(raw).decode()}


def same(current, expected):
    fields = ('existed', 'path', 'device', 'inode', 'mode', 'mtimeNs', 'rawBase64')
    return all(current.get(key) == expected.get(key) for key in fields)


def publish(original, raw, mode=None, *, on_committed=None, restore_times=None):
    path = Path(original['path'])
    if not same(snapshot(path, missing=True, limit=4 * 1024 * 1024), original):
        raise InstallationError('runtime-adoption-write-conflict')
    descriptor, temporary = tempfile.mkstemp(prefix='.aic-runtime-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            os.fchmod(stream.fileno(), mode if mode is not None else original.get('mode', 0o600))
            stream.write(raw); stream.flush(); os.fsync(stream.fileno())
            if restore_times is not None:
                os.utime(stream.fileno(), ns=restore_times)
                os.fsync(stream.fileno())
            committed = os.fstat(stream.fileno())
        known = {'existed': True, 'path': str(path), 'device': str(committed.st_dev),
                 'inode': str(committed.st_ino), 'mode': stat.S_IMODE(committed.st_mode),
                 'atimeNs': str(committed.st_atime_ns), 'mtimeNs': str(committed.st_mtime_ns),
                 'rawBase64': base64.b64encode(raw).decode()}
        if not same(snapshot(path, missing=True, limit=4 * 1024 * 1024), original):
            raise InstallationError('runtime-adoption-write-conflict')
        os.replace(temporary, path)
        # Retain OUR committed inode/bytes before any post-commit observation
        # can fail or see a foreign replacement. Never adopt that foreign leaf.
        if on_committed is not None: on_committed(known)
        observed = snapshot(path, limit=4 * 1024 * 1024)
        if not same(observed, known): raise InstallationError('runtime-adoption-post-write-conflict')
        return known
    finally:
        try: Path(temporary).unlink()
        except FileNotFoundError: pass


def restore(change):
    before, installed = change['before'], change['installed']
    path = Path(before['path'])
    if not same(snapshot(path, missing=True, limit=4 * 1024 * 1024), installed):
        raise InstallationError('runtime-adoption-restore-conflict')
    if before['existed']:
        restored = publish(installed, base64.b64decode(before['rawBase64'], validate=True), before['mode'],
            restore_times=(int(before['atimeNs']), int(before['mtimeNs'])))
        if not same(snapshot(path, limit=4 * 1024 * 1024), restored):
            raise InstallationError('runtime-adoption-restore-conflict')
    else:
        path.unlink()


def unit_bytes(installation):
    bundle = Path(installation['bundleDirectory'])
    home = Path(installation['nativeBinary']).parents[2]
    def quote(value):
        value = str(value)
        if any(ord(char) < 32 for char in value): raise InstallationError('runtime-unit-path-invalid')
        return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
    command = ['/usr/bin/python3', '-I', bundle / 'lib/resident_main.py',
               '--binary', installation['nativeBinary'], '--database',
               home / '.gemini/antigravity-cli/conversation_summaries.db',
               '--socket', installation['socketPath']]
    return ('[Unit]\nDescription=AI Account Center Antigravity runtime\n'
            '[Service]\nType=simple\nExecStart=' + ' '.join(quote(item) for item in command) +
            '\nRestart=no\n[Install]\nWantedBy=default.target\n').encode()


def run_user_service(runner, *args):
    runner(['systemctl', '--user', *args], check=True, stdin=subprocess.DEVNULL,
           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def adopt(home, *, runner=subprocess.run, readiness=service_readiness):
    home = Path(home).resolve()
    installed = load_installation(home)
    bundle = Path(installed['bundleDirectory'])
    if not runtime_release(bundle): raise InstallationError('runtime-native-gate-pending')
    # Native proof readiness must be injected by the reviewed installed binder.
    # No service state or file fingerprint can substitute for native identity.
    if readiness is None: raise InstallationError('runtime-readiness-provider-unavailable')
    state = home / '.ccs/antigravity-switching'
    checkpoint = state / 'runtime-adoption.json'
    if checkpoint.exists() or checkpoint.is_symlink():
        raise InstallationError('runtime-adoption-already-present')
    runtime_dir = bundle / 'lib'
    sys.path.insert(0, str(runtime_dir))
    from native_status_config import prepare_status_config
    settings = snapshot(home / '.gemini/antigravity-cli/settings.json')
    original_file = state / 'original-status-command.json'
    proposal = prepare_status_config(base64.b64decode(settings['rawBase64']),
        python_path=str(bundle / 'venv/bin/python3'), helper_path=str(runtime_dir / 'native_status_hook.py'),
        socket_path=str(home / '.ccs/antigravity-runtime/status.sock'),
        original_command_file=str(original_file))
    profiles = [snapshot(home / name) for name in ('.bashrc', '.profile')]
    unit = home / '.config/systemd/user' / UNIT
    unit.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    service = snapshot(unit, missing=True)
    if service['existed']: raise InstallationError('runtime-service-already-present')
    changes = []
    service_started = False
    command_created = False
    journal = {'schemaVersion': 1, 'phase': 'preparing', 'installation': installed,
               'snapshots': profiles + [settings, service], 'changes': changes,
               'serviceStarted': False, 'nativeCapability': False}
    write_exclusive(checkpoint, (json.dumps(journal) + '\n').encode())
    journal_snapshot = snapshot(checkpoint, limit=4 * 1024 * 1024)
    def persist():
        nonlocal journal_snapshot
        raw = (json.dumps(journal) + '\n').encode()
        if len(raw) > 4 * 1024 * 1024: raise InstallationError('runtime-adoption-checkpoint-size')
        def committed(known):
            nonlocal journal_snapshot
            journal_snapshot = known
        publish(journal_snapshot, raw, on_committed=committed)
    def apply_change(original, raw, mode=None):
        def committed(known):
            changes.append({'before': original, 'installed': known})
            persist()
        return publish(original, raw, mode, on_committed=committed)
    try:
        write_exclusive(original_file, (json.dumps({'command': proposal.original_command}) + '\n').encode())
        command_created = True
        journal['originalCommand'] = snapshot(original_file)
        persist()
        for original in profiles:
            raw = base64.b64decode(original['rawBase64'])
            patched = shell_path_proposal(raw, bundle.parent.parent / 'bin')
            apply_change(original, patched)
        apply_change(settings, proposal.patched)
        apply_change(service, unit_bytes(installed), 0o600)
        run_user_service(runner, 'daemon-reload')
        # A failed systemctl call may already have enabled/started OUR new unit.
        service_started = True
        journal['serviceStarted'] = True
        persist()
        run_user_service(runner, 'enable', '--now', UNIT)
        # Must check private source/broker wiring and current owned endpoint.
        # Native capability remains a separately reported exact-identity fact.
        ready = readiness(installed)
        if (type(ready) is not dict or ready.get('serviceReady') is not True or
                type(ready.get('nativeCapability')) is not bool):
            raise InstallationError('runtime-service-readiness-failed')
        journal.update(phase='adopted', nativeCapability=ready['nativeCapability'])
        persist()
        return journal
    except BaseException:
        # Keep all rollback material if any foreign edit or unit conflict arises.
        try:
            journal['phase'] = 'recovery-required'; persist()
            if service_started:
                own_unit = next((item for item in changes if item['before']['path'] == str(unit)), None)
                if not own_unit or not same(snapshot(unit, missing=True), own_unit['installed']):
                    raise InstallationError('runtime-service-restore-conflict')
                run_user_service(runner, 'disable', '--now', UNIT)
            # Complete preflight keeps a foreign edit from producing a partial
            # rollback of unrelated original files.
            for change in changes:
                if not same(snapshot(change['before']['path'], missing=True, limit=4 * 1024 * 1024), change['installed']):
                    raise InstallationError('runtime-adoption-restore-conflict')
            for change in reversed(changes): restore(change)
            if command_created:
                if not same(snapshot(original_file), journal['originalCommand']):
                    raise InstallationError('runtime-original-command-changed')
                original_file.unlink()
            run_user_service(runner, 'daemon-reload')
            if not same(snapshot(checkpoint, limit=4 * 1024 * 1024), journal_snapshot):
                raise InstallationError('runtime-adoption-checkpoint-changed')
            checkpoint.unlink()
        except BaseException:
            # Keep the private original snapshots and exact owned-publication
            # identities; never overwrite a foreign edit to claim recovery.
            pass
        raise


def rollback(home, *, runner=subprocess.run):
    home = Path(home).resolve()
    state = home / '.ccs/antigravity-switching'
    checkpoint = state / 'runtime-adoption.json'
    checkpoint_owner = snapshot(checkpoint, limit=4 * 1024 * 1024)
    receipt = exact_json(base64.b64decode(checkpoint_owner['rawBase64'], validate=True))
    expected_paths = {str(home / name) for name in ('.bashrc', '.profile')}
    expected_paths |= {str(home / '.gemini/antigravity-cli/settings.json'),
                       str(home / '.config/systemd/user' / UNIT)}
    if (type(receipt) is not dict or receipt.get('schemaVersion') != 1 or
            type(receipt.get('changes')) is not list or len(receipt['changes']) > 4 or
            type(receipt.get('snapshots')) is not list or len(receipt['snapshots']) != 4 or
            {item['path'] for item in receipt['snapshots']} != expected_paths or
            len({change['before']['path'] for change in receipt['changes']}) != len(receipt['changes']) or
            not {change['before']['path'] for change in receipt['changes']}.issubset(expected_paths) or
            receipt.get('phase') not in {'adopted', 'preparing', 'recovery-required'} or
            ('originalCommand' in receipt and receipt['originalCommand'].get('path') !=
             str(state / 'original-status-command.json'))):
        raise InstallationError('runtime-adoption-checkpoint-invalid')
    # Native may rewrite JSON formatting/inode around our exact owned hook.
    # Only settings get the already-reviewed lexical reversal; other edits hold.
    lexical={}
    # All conflicts are checked before stopping any service or reverting a file.
    for change in receipt['changes']:
        if not same(snapshot(change['before']['path'], missing=True, limit=4 * 1024 * 1024), change['installed']):
            if change['before']['path']!=str(home/'.gemini/antigravity-cli/settings.json'):
                raise InstallationError('runtime-adoption-restore-conflict')
            installed=load_installation(home)
            sys.path.insert(0,str(Path(installed['bundleDirectory'])/'lib'))
            from native_status_hook_cleanup import guarded_read,prepare_hook_reversal,apply_removal
            pin,current=guarded_read(change['before']['path'])
            lexical[change['before']['path']]=prepare_hook_reversal(
                (base64.b64decode(change['before']['rawBase64'],validate=True) if change['before']['existed'] else b'{}'),
                base64.b64decode(change['installed']['rawBase64'],validate=True),current,pin)
    changed = {item['before']['path'] for item in receipt['changes']}
    for original in receipt['snapshots']:
        if original['path'] not in changed and not same(snapshot(original['path'], missing=True), original):
            raise InstallationError('runtime-adoption-restore-conflict')
    if ('originalCommand' in receipt and not same(snapshot(state / 'original-status-command.json'), receipt['originalCommand'])):
        raise InstallationError('runtime-original-command-changed')
    if receipt.get('serviceStarted') is True:
        run_user_service(runner, 'disable', '--now', UNIT)
    for change in reversed(receipt['changes']):
        if change['before']['path'] in lexical:
            from runtime_continuity import census_cli
            installation=load_installation(home)
            def no_writer():
                if census_cli(installation['nativeBinary'],allowed_children=(
                        str(home/'.gemini/antigravity-cli/bin/agentapi'),
                        str(home/'.gemini/antigravity-cli/bin/webm_encoder'))):
                    raise InstallationError('runtime-native-writer-present')
            apply_removal(change['before']['path'],lexical[change['before']['path']],before_commit=no_writer)
        else:restore(change)
    # Service/restore callbacks may replace either control leaf after preflight.
    # Recheck BOTH before deleting either, retaining foreign recovery material.
    if not same(snapshot(checkpoint, limit=4 * 1024 * 1024), checkpoint_owner):
        raise InstallationError('runtime-adoption-checkpoint-changed')
    if ('originalCommand' in receipt and not same(snapshot(state / 'original-status-command.json'), receipt['originalCommand'])):
        raise InstallationError('runtime-original-command-changed')
    if 'originalCommand' in receipt: (state / 'original-status-command.json').unlink()
    checkpoint.unlink()
    run_user_service(runner, 'daemon-reload')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--rollback', action='store_true')
    args = parser.parse_args()
    try:
        if sys.platform != 'linux': raise InstallationError('runtime-ubuntu-only')
        if args.rollback: rollback(Path.home())
        else: adopt(Path.home())  # No default provider; fail closed before writes.
        return 0
    except (InstallationError, OSError, ValueError, subprocess.SubprocessError):
        sys.stderr.write('Antigravity launch adoption could not complete its required checks.\n')
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
