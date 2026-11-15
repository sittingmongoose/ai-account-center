#!/usr/bin/env python3
"""Big review M5: a request's deadlineMs must answer while a store read is stuck.

signin-expiry and quota-read used to do their file work on the dispatch future itself, so a
slow read held the reply past the deadline. Here the real agent binary reads an ordinary fixture
file through slow-read.c (LD_PRELOAD: that file's first read() sleeps three seconds and leaves a
receipt). A 100 ms deadline must end in a timeout failure or the labelled fallback well before
the read returns; the same request with a long deadline is the control that proves the delay
happened. Linux only (it needs a C compiler for the shim). No provider is contacted.

    python3 tests/spec/agent/deadline_review.py <aac-agent binary> [--baseline]

--baseline expects the old behaviour (an ordinary answer after the delay) and fails otherwise.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent


def ask(binary, verb, args, deadline, env):
    request = {'protocol': 1, 'verb': verb, 'id': 'deadline', 'deadlineMs': deadline, 'args': args}
    began = time.monotonic()
    process = subprocess.run([binary], input=json.dumps(request) + '\n', text=True, capture_output=True,
                             timeout=30, env=env)
    elapsed = time.monotonic() - began
    event = json.loads(process.stdout)
    assert event.get('protocol') == 1 and event.get('id') in ('deadline', ''), event
    return process.returncode, event, elapsed


def main():
    binary = str(Path(sys.argv[1]).resolve())
    baseline = '--baseline' in sys.argv
    if not sys.platform.startswith('linux') or not shutil.which('cc'):
        raise SystemExit('deadline_review.py needs Linux and cc (it preloads a C shim)')
    wrong = []
    with tempfile.TemporaryDirectory(prefix='aac-deadline-review-') as tmp:
        root = Path(tmp)
        shim = root / 'slow-read.so'
        subprocess.run(['cc', '-shared', '-fPIC', '-O1', '-o', str(shim), str(HERE / 'slow-read.c'), '-ldl'], check=True)
        home = root / 'home'
        credentials = home / '.claude/.credentials.json'
        credentials.parent.mkdir(parents=True)
        credentials.write_text(json.dumps({'claudeAiOauth': {'expiresAt': 1767225600000 + 3 * 86_400_000}}))
        credentials.chmod(0o600)
        auth = home / '.local/share/opencode/auth.json'
        auth.parent.mkdir(parents=True)
        auth.write_text('{}')
        cases = [
            ('signin-expiry', {'home': str(home), 'ccsDir': str(home / '.ccs'), 'nowMs': 1767225600000}, credentials),
            ('quota-read', {'provider': 'kimi-code', 'platform': 'ubuntu', 'home': str(home)}, auth),
        ]
        for verb, args, slow in cases:
            receipt = root / (verb + '.receipt')
            env = {'PATH': os.defpath, 'TOKIO_WORKER_THREADS': '1', 'LD_PRELOAD': str(shim),
                   'AAC_SLOW_READ_PATH': os.path.realpath(slow), 'AAC_SLOW_READ_RECEIPT': str(receipt)}
            # Control: a long deadline waits for the slow read and answers normally.
            code, event, elapsed = ask(binary, verb, args, 15000, env)
            assert receipt.exists(), f'{verb}: the shim never delayed {slow.name}'
            assert code == 0 and event['type'] == 'result' and elapsed >= 2.9, (verb, code, event, elapsed)
            receipt.unlink()
            code, event, elapsed = ask(binary, verb, args, 100, env)
            assert receipt.exists(), f'{verb}: the shim never delayed {slow.name} under the short deadline'
            declined = event['type'] == 'result' and event['value'] == {'fallback': 'python'}
            timed_out = code == 1 and event['type'] == 'failure' and event['failure'].get('cause') == 'timeout'
            prompt = elapsed < 1.5 and (declined or timed_out)
            outcome = 'timeout' if timed_out else 'fallback' if declined else event['type']
            print(f'{"PASS" if prompt else "FAIL"}: {verb} deadlineMs 100 answered {outcome} after {elapsed:.2f} s', flush=True)
            if not prompt:
                wrong.append(verb)
    if baseline:
        if not wrong:
            raise SystemExit('Baseline unexpectedly answered within the deadline')
        print(f'BASELINE RED as expected: {", ".join(wrong)}')
        return
    if wrong:
        raise SystemExit(f'{len(wrong)} verbs answered past their deadline')
    print('deadline review: 2 verbs answer a 100 ms deadline during a 3 s read')


if __name__ == '__main__':
    main()
