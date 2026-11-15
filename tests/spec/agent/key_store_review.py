#!/usr/bin/env python3
"""L2: the agent's key-store writes go through aac-files' one durable replacement.

The real agent binary writes a key under a fixture home through sync-log.c (LD_PRELOAD), which
logs every fsync and can kill the process at one, as a crash would. Python's key_store.put on a
twin home is the reference for the answer and the files. Checked:
  sync          the staged file is fsynced, renamed, then the keys folder is fsynced (new in L2;
                the base wrote no folder sync, so this case fails there);
  crash-file    killed at the data fsync, before the rename: the committed key is whole;
  crash-dir     killed at the folder fsync, after the rename: the new key is whole;
  lease         another writer holds the folder lease: the agent refuses and writes nothing
                (TS then runs the Python helper), where the base wrote anyway;
  parity        answer, file bytes and modes equal Python's, and no lock file joins the folder.
Linux only (it needs cc for the shim). No provider is contacted; fixture data only.

    python3 tests/spec/agent/key_store_review.py <aac-agent binary> [--baseline]

--baseline expects the base's behaviour (no folder sync, no lease) and fails otherwise.
"""
import datetime as dt
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
NOW = dt.datetime(2026, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)
FIRST, SECOND = 'fixture-secret-first', 'fixture-secret-second'


def reference():
    sys.path.insert(0, str(ROOT / 'scripts/account-usage'))
    spec = importlib.util.spec_from_file_location('key_store', ROOT / 'scripts/account-usage/key_store.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def put(binary, home, secret, env):
    args = {'action': 'put', 'provider': 'zai', 'keyId': '12345678', 'secret': secret, 'home': str(home),
            'nowMs': int(NOW.timestamp() * 1000)}
    request = {'protocol': 1, 'verb': 'key-store', 'id': 'l2', 'deadlineMs': 15000, 'args': args}
    process = subprocess.run([binary], input=json.dumps(request) + '\n', text=True, capture_output=True,
                             timeout=30, env=env)
    try:
        event = json.loads(process.stdout)
    except ValueError:
        event = None
    return process.returncode, event


def image(home):
    result = {}
    for root, dirs, files in os.walk(home):
        for name in dirs + files:
            p = Path(root) / name
            meta = p.lstat()
            result[str(p.relative_to(home))] = (stat.S_IMODE(meta.st_mode),
                                                None if p.is_dir() else p.read_bytes())
    return result


def keys(home):
    return home / '.ccs/account-usage/keys'


def main():
    binary = str(Path(sys.argv[1]).resolve())
    baseline = '--baseline' in sys.argv
    if not sys.platform.startswith('linux') or not shutil.which('cc'):
        raise SystemExit('key_store_review.py needs Linux and cc (it preloads a C shim)')
    helper = reference()
    failed = []

    def check(name, ok, detail=''):
        print(f'{"PASS" if ok else "FAIL"}: {name}{(" - " + detail) if detail and not ok else ""}', flush=True)
        if not ok:
            failed.append(name)

    with tempfile.TemporaryDirectory(prefix='aac-l2-keys-') as tmp:
        root = Path(tmp)
        shim = root / 'sync-log.so'
        subprocess.run(['cc', '-shared', '-fPIC', '-O1', '-o', str(shim), str(HERE / 'sync-log.c'), '-ldl'],
                       check=True)

        def env(home, log, kill=None):
            target = keys(home) / 'zai-12345678.json'
            values = {'PATH': os.defpath, 'TOKIO_WORKER_THREADS': '1', 'LD_PRELOAD': str(shim),
                      'AAC_SYNC_LOG': str(log), 'AAC_SYNC_TARGET': str(target)}
            if kill:
                values['AAC_SYNC_KILL'] = kill
            return values

        def expected_size(secret):
            twin = root / f'twin-{secret}'
            helper.put('zai', '12345678', secret, home=str(twin), windows=False, now=NOW)
            return (keys(twin) / 'zai-12345678.json').stat().st_size

        # sync and parity: a fresh home, against Python on a twin home.
        home, twin, log = root / 'sync', root / 'sync-twin', root / 'sync.log'
        home.mkdir(), twin.mkdir()
        size = expected_size(SECOND)
        variables = env(home, log)
        variables['AAC_SYNC_SIZE'] = str(size)
        code, event = put(binary, home, SECOND, variables)
        expected = helper.put('zai', '12345678', SECOND, home=str(twin), windows=False, now=NOW)
        check('parity: answer', code == 0 and event and event.get('value') == expected, f'{code} {event}')
        check('parity: files and modes', image(home) == image(twin))
        check('parity: no lock file in the keys folder',
              sorted(os.listdir(keys(home))) == ['zai-12345678.json'])
        lines = log.read_text().splitlines() if log.exists() else []
        staged = [l for l in lines if l.startswith(f'file {keys(home)}/.zai-12345678.json.') and l.endswith('.tmp')]
        synced = [i for i, l in enumerate(lines) if l == f'dir {keys(home)} after-rename']
        check('sync: the staged file is fsynced', bool(staged), repr(lines))
        check('sync: the keys folder is fsynced after the rename',
              bool(staged) and bool(synced) and synced[-1] > lines.index(staged[0]), repr(lines))

        # crash-file: killed at the data fsync, before the rename.
        home, log = root / 'crash-file', root / 'crash-file.log'
        home.mkdir()
        helper.put('zai', '12345678', FIRST, home=str(home), windows=False, now=NOW)
        committed = (keys(home) / 'zai-12345678.json').read_bytes()
        code, event = put(binary, home, SECOND, env(home, log, 'file'))
        check('crash-file: the process died at the data fsync', code == -9 and 'killed file' in log.read_text(),
              f'{code}')
        check('crash-file: the committed key is whole', (keys(home) / 'zai-12345678.json').read_bytes() == committed)
        leftovers = [n for n in os.listdir(keys(home)) if n != 'zai-12345678.json']
        check('crash-file: only the staged file is left, as by Python',
              len(leftovers) == 1 and leftovers[0].startswith('.zai-12345678.json.') and leftovers[0].endswith('.tmp'),
              repr(leftovers))

        # crash-dir: killed at the folder fsync, after the rename.
        home, log = root / 'crash-dir', root / 'crash-dir.log'
        home.mkdir()
        helper.put('zai', '12345678', FIRST, home=str(home), windows=False, now=NOW)
        twin = root / 'crash-dir-twin'
        helper.put('zai', '12345678', SECOND, home=str(twin), windows=False, now=NOW)
        code, event = put(binary, home, SECOND, env(home, log, 'dir'))
        died = code == -9 and 'killed dir' in (log.read_text() if log.exists() else '')
        check('crash-dir: the process died at the folder fsync', died, f'{code} {event}')
        check('crash-dir: the new key is whole',
              (keys(home) / 'zai-12345678.json').read_bytes() == (keys(twin) / 'zai-12345678.json').read_bytes())

        # lease: another writer holds the keys folder.
        home, log = root / 'lease', root / 'lease.log'
        home.mkdir()
        helper.put('zai', '12345678', FIRST, home=str(home), windows=False, now=NOW)
        before = image(home)
        holder = os.open(keys(home), os.O_RDONLY | os.O_DIRECTORY)
        try:
            fcntl.flock(holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, event = put(binary, home, SECOND, env(home, log))
        finally:
            os.close(holder)
        refused = code == 1 and event and event.get('type') == 'failure'
        check('lease: a held folder lease refuses the write', refused, f'{code} {event}')
        check('lease: nothing was written', image(home) == before)

    if baseline:
        expected_failures = {'sync: the keys folder is fsynced after the rename', 'crash-dir: the process died at the folder fsync',
                             'lease: a held folder lease refuses the write', 'lease: nothing was written'}
        if not expected_failures.issubset(failed):
            raise SystemExit(f'Baseline unexpectedly passed: {sorted(expected_failures - set(failed))}')
        print(f'baseline: {len(failed)} checks fail as expected')
        return
    if failed:
        raise SystemExit(f'{len(failed)} check(s) failed')
    print('key_store_review: every check passed')


if __name__ == '__main__':
    main()
