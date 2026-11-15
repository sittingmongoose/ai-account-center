"""Real-directory witnesses from BIG-REVIEW-M1-M3-ASTRA (MED-1, MED-2) for signin-expiry.

The agent must give the Python helper's exact answer or the labelled fallback, never a
different ordinary answer. Usage: python3 tests/spec/agent/signin_review.py <aac-agent>
(built with --features parity-fixtures).
"""
import json
import os
import pathlib
import subprocess
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from run import normalize, python_reply, terminal  # noqa: E402

NOW = 1767225600000


def args(home):
    return dict(home=str(home), ccsDir=str(home / 'scope'), nowMs=NOW, platform='linux',
                environment={}, includeKeychain=False, keychain={})


def compare(binary, home, label):
    a = args(home)
    previous = os.getcwd()
    os.chdir(home)
    try:
        expected = python_reply(dict(a))
    finally:
        os.chdir(previous)
    request = dict(protocol=1, verb='signin-expiry', id='parity', deadlineMs=15000, args=normalize(a))
    result = subprocess.run([binary], input=json.dumps(request) + '\n', text=True,
                            capture_output=True, timeout=20, cwd=home)
    actual = terminal(result)
    if actual == {'fallback': 'python'}:
        return 'fallback'
    assert actual == expected, label + ': agent answer differs from the Python helper'
    return 'equal'


def main(binary):
    binary = str(pathlib.Path(binary).resolve())
    counts = {'equal': 0, 'fallback': 0}
    # MED-1: more than 64 sibling homes, created in descending order (not alphabetical).
    for count in (63, 64, 65, 66):
        with tempfile.TemporaryDirectory(prefix='aac-signin-review-') as tmp:
            home = pathlib.Path(tmp)
            for i in reversed(range(count)):
                (home / '.claude-t3' / f'slot{i:03}').mkdir(parents=True)
            outcome = compare(binary, home, f'{count} sibling homes')
            assert count <= 64 or outcome == 'fallback', f'{count} sibling homes must fall back'
            counts[outcome] += 1
    # MED-2: configured homes spelled with two (pathlib's own root) or three leading slashes.
    for prefix in ('/', '//'):
        for driver in ('claudeAgent', 'codex'):
            with tempfile.TemporaryDirectory(prefix='aac-signin-review-') as tmp:
                home = pathlib.Path(tmp)
                (home / 'alt').mkdir()
                settings = home / '.t3/userdata/settings.json'
                settings.parent.mkdir(parents=True)
                settings.write_text(json.dumps({'providerInstances': {'one': {
                    'driver': driver, 'config': {'homePath': prefix + str(home / 'alt')}}}}))
                counts[compare(binary, home, f'{len(prefix) + 1} leading slashes, {driver}')] += 1
    print(f'PASS signin-expiry review witnesses: {counts}')


if __name__ == '__main__':
    main(sys.argv[1])
