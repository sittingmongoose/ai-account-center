"""Real-directory witnesses for signin-expiry from the M1-M3 big reviews.

BIG-REVIEW-M1-M3-ASTRA MED-1 and MED-2 (sibling bound, "//" homes) and BIG-REVIEW-M1-M3-WONDER M1
(T3 settings and discovered homes reached through links). The agent must give the Python helper's
exact answer or the labelled fallback, never a different ordinary answer.

Usage: python3 tests/spec/agent/signin_review.py <aac-agent> [--production] [--foreign-root DIR]
The default binary is built with --features parity-fixtures. --production sends only the
arguments a production agent accepts (home, ccsDir, nowMs), so the plain release build runs the
same witnesses through its own filesystem path. The ambient-selector witnesses (Wonder L8) send
no home at all: the agent reads HOME and the selectors from its own environment, as in
production. --foreign-root names a folder whose new files the kernel reports with another owner
(an NFS export that maps every file to one user does this); without it the foreign-owner witness
is skipped and says so.
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
DAY = 86_400_000
PRODUCTION = '--production' in sys.argv


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
    sent = {k: a[k] for k in ('home', 'ccsDir', 'nowMs')} if PRODUCTION else a
    request = dict(protocol=1, verb='signin-expiry', id='parity', deadlineMs=15000, args=normalize(sent))
    result = subprocess.run([binary], input=json.dumps(request) + '\n', text=True,
                            capture_output=True, timeout=20, cwd=home)
    actual = terminal(result)
    if actual == {'fallback': 'python'}:
        return 'fallback', expected
    if actual != expected:
        return 'differs', (expected, actual)
    return 'equal', expected


def ambient(binary, home, selector):
    """The production request shape: no home argument, so HOME and CLAUDE_CONFIG_DIR come from
    the agent's own environment (bytes, so a value that is not UTF-8 reaches it unchanged)."""
    environment = {'CLAUDE_CONFIG_DIR': os.fsdecode(selector)}
    a = dict(home=str(home), ccsDir=str(home / '.ccs'), nowMs=NOW, platform='linux',
             environment=environment, includeKeychain=True, keychain={})
    previous = os.getcwd()
    os.chdir(home)
    try:
        expected = normalize(python_reply(dict(a)))
    finally:
        os.chdir(previous)
    env = {b'PATH': os.environb.get(b'PATH', b'/usr/bin:/bin'), b'HOME': os.fsencode(home),
           b'CLAUDE_CONFIG_DIR': selector, b'LANG': b'C.UTF-8'}
    request = dict(protocol=1, verb='signin-expiry', id='parity', deadlineMs=15000, args={'nowMs': NOW})
    result = subprocess.run([binary], input=(json.dumps(request) + '\n').encode(), env=env,
                            capture_output=True, timeout=20, cwd=home)
    actual = terminal(subprocess.CompletedProcess(result.args, result.returncode,
                                                  result.stdout.decode(), result.stderr.decode()))
    if actual == {'fallback': 'python'}:
        return 'fallback', expected
    if actual != expected:
        return 'differs', (expected, actual)
    return 'equal', expected


def rows(answer):
    return [(p['name'], p['state']) for p in answer['profiles']]


def login(folder):
    folder.mkdir(parents=True, exist_ok=True)
    (folder / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
        'accessToken': 'fixture-access', 'refreshToken': 'fixture-refresh',
        'expiresAt': NOW + 30 * DAY, 'refreshTokenExpiresAt': NOW + 60 * DAY}}))


def settings_text(*homes):
    return json.dumps({'providerInstances': {f'i{n}': {'driver': 'claudeAgent', 'config': {'homePath': str(h)}}
                                             for n, h in enumerate(homes)}})


def link(path, target):
    path.parent.mkdir(parents=True, exist_ok=True)
    os.symlink(target, path)


# Wonder M1 and its neighbours. Each builder gets (home, outside) and lays out one shape; the
# home always holds a valid default login and a valid `custom` login that only settings names.
def settings_file(home, _outside):
    settings = home / '.t3/userdata/settings.json'
    settings.parent.mkdir(parents=True)
    settings.write_text(settings_text(home / 'custom'))


def t3_link_out(home, outside):
    (outside / 't3/userdata').mkdir(parents=True)
    (outside / 't3/userdata/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3', outside / 't3')


def userdata_link_out(home, outside):
    (outside / 'userdata').mkdir(parents=True)
    (outside / 'userdata/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3/userdata', outside / 'userdata')


def settings_link_out(home, outside):
    (outside / 'settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3/userdata/settings.json', outside / 'settings.json')


def settings_link_in(home, _outside):
    (home / 'elsewhere').mkdir()
    (home / 'elsewhere/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3/userdata/settings.json', home / 'elsewhere/settings.json')


def settings_link_relative(home, _outside):
    (home / 'elsewhere').mkdir()
    (home / 'elsewhere/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3/userdata/settings.json', '../../elsewhere/settings.json')


def t3_link_in(home, _outside):
    (home / 'sync/t3/userdata').mkdir(parents=True)
    (home / 'sync/t3/userdata/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3', 'sync/t3')


def userdata_link_in(home, _outside):
    (home / 'sync/userdata').mkdir(parents=True)
    (home / 'sync/userdata/settings.json').write_text(settings_text(home / 'custom'))
    link(home / '.t3/userdata', home / 'sync/userdata')


def t3_link_out_and_back(home, outside):
    (home / 'sync/t3/userdata').mkdir(parents=True)
    (home / 'sync/t3/userdata/settings.json').write_text(settings_text(home / 'custom'))
    link(outside / 'hop', home / 'sync/t3')
    link(home / '.t3', outside / 'hop')


def settings_chain_in(home, _outside):
    (home / 'elsewhere').mkdir()
    (home / 'elsewhere/settings.json').write_text(settings_text(home / 'custom'))
    link(home / 'elsewhere/hop.json', 'settings.json')
    link(home / '.t3/userdata/settings.json', home / 'elsewhere/hop.json')


def settings_dangling(home, _outside):
    link(home / '.t3/userdata/settings.json', home / 'gone.json')


def t3_dangling(home, outside):
    link(home / '.t3', outside / 'gone')


def settings_loop(home, _outside):
    link(home / '.t3/userdata/settings.json', 'loop.json')
    link(home / '.t3/userdata/loop.json', 'settings.json')


def settings_folder(home, _outside):
    (home / '.t3/userdata/settings.json').mkdir(parents=True)


def homepath_link_out(home, outside):
    login(outside / 'profile')
    link(home / 'linked', outside / 'profile')
    settings = home / '.t3/userdata/settings.json'
    settings.parent.mkdir(parents=True)
    settings.write_text(settings_text(home / 'linked'))


def homepath_below_link_out(home, outside):
    login(outside / 'tree/profile')
    link(home / 'mount', outside / 'tree')
    settings = home / '.t3/userdata/settings.json'
    settings.parent.mkdir(parents=True)
    settings.write_text(settings_text(home / 'mount/profile'))


def homepath_link_in(home, _outside):
    login(home / 'real-profile')
    link(home / 'linked', 'real-profile')
    settings = home / '.t3/userdata/settings.json'
    settings.parent.mkdir(parents=True)
    settings.write_text(settings_text(home / 'linked', home / 'custom'))


def sibling_link_out(home, outside):
    login(outside / 'a')
    login(home / '.claude-t3/b')
    link(home / '.claude-t3/a', outside / 'a')


def sibling_link_in(home, _outside):
    login(home / 'real-a')
    login(home / '.claude-t3/b')
    link(home / '.claude-t3/a', home / 'real-a')


def sibling_link_relative(home, _outside):
    login(home / 'real-a')
    login(home / '.claude-t3/b')
    link(home / '.claude-t3/a', '../real-a')


def sibling_link_network(home, outside):
    # Python refuses a "//" target outright; the OS reads it as "/" and finds the folder.
    login(outside / 'a')
    login(home / '.claude-t3/b')
    link(home / '.claude-t3/a', '/' + str(outside / 'a'))


def sibling_link_chain(home, _outside):
    login(home / 'real-a')
    login(home / '.claude-t3/b')
    link(home / 'hop', 'real-a')
    link(home / '.claude-t3/a', home / 'hop')


def sibling_base_link_out(home, outside):
    login(outside / 'claude-t3/a')
    link(home / '.claude-t3', outside / 'claude-t3')
    settings_file(home, outside)


def sibling_base_link_in(home, outside):
    login(home / 'sync/claude-t3/a')
    link(home / '.claude-t3', 'sync/claude-t3')
    settings_file(home, outside)


def instances_link_out(home, outside):
    login(outside / 'saved')
    login(home / 'scope/instances/work')
    link(home / 'scope/instances/alpha', outside / 'saved')


def sibling_entry_dangling(home, _outside):
    login(home / '.claude-t3/b')
    link(home / '.claude-t3/gone', home / 'missing')


def sibling_entry_file(home, _outside):
    login(home / '.claude-t3/b')
    (home / 'note.txt').write_text('not a home')
    link(home / '.claude-t3/note', home / 'note.txt')


def sibling_base_dangling(home, outside):
    link(home / '.claude-t3', outside / 'missing')
    settings_file(home, outside)


def sibling_base_file(home, outside):
    (home / 'note.txt').write_text('not a home')
    link(home / '.claude-t3', 'note.txt')
    settings_file(home, outside)


def sibling_names(home, outside):
    for name in ('_under', '-dash', 'y' * 65, 'plain'):
        login(home / '.claude-t3' / name)
    settings_file(home, outside)


SHAPES = [
    # (label, builder, the outcome the agent must give: Python's answer, or the labelled fallback)
    ('settings: ordinary file (control)', settings_file, 'equal'),
    ('settings: .t3 links out of the home', t3_link_out, 'fallback'),
    ('settings: .t3/userdata links out of the home', userdata_link_out, 'fallback'),
    ('settings: settings.json links out of the home', settings_link_out, 'fallback'),
    ('settings: settings.json links inside the home', settings_link_in, 'fallback'),
    ('settings: settings.json relative link inside the home', settings_link_relative, 'fallback'),
    ('settings: .t3 relative link inside the home', t3_link_in, 'fallback'),
    ('settings: .t3/userdata links inside the home', userdata_link_in, 'fallback'),
    ('settings: .t3 links out, the outside link points back in', t3_link_out_and_back, 'fallback'),
    ('settings: settings.json through a chain of links', settings_chain_in, 'fallback'),
    ('settings: settings.json dangling link', settings_dangling, 'fallback'),
    ('settings: .t3 dangling link', t3_dangling, 'fallback'),
    ('settings: settings.json link loop', settings_loop, 'fallback'),
    ('settings: settings.json is a folder (control)', settings_folder, 'equal'),
    ('homePath: in-home link to a profile outside the home', homepath_link_out, 'equal'),
    ('homePath: below an in-home link that leaves the home', homepath_below_link_out, 'equal'),
    ('homePath: in-home link to an in-home profile', homepath_link_in, 'equal'),
    ('.claude-t3 entry links out of the home', sibling_link_out, 'equal'),
    ('.claude-t3 entry links inside the home', sibling_link_in, 'equal'),
    ('.claude-t3 entry links inside the home, relative target', sibling_link_relative, 'equal'),
    ('.claude-t3 entry links to a "//" target', sibling_link_network, 'equal'),
    ('.claude-t3 entry links through a second link', sibling_link_chain, 'fallback'),
    ('.claude-t3 links out of the home', sibling_base_link_out, 'fallback'),
    ('.claude-t3 links inside the home', sibling_base_link_in, 'fallback'),
    ('saved instance links out of the home', instances_link_out, 'equal'),
    # A link that is no folder adds no home in either scan: these stay ordinary answers.
    ('.claude-t3 entry is a dangling link', sibling_entry_dangling, 'equal'),
    ('.claude-t3 entry links to a file', sibling_entry_file, 'equal'),
    ('.claude-t3 is a dangling link', sibling_base_dangling, 'equal'),
    ('.claude-t3 links to a file', sibling_base_file, 'equal'),
    ('.claude-t3 names the sibling filter refuses', sibling_names, 'equal'),
]

# Wonder's five witnesses: Python's rows, which the agent must equal unless it falls back.
WONDER = {
    'settings: .t3 links out of the home': [('default', 'ok')],
    'settings: .t3/userdata links out of the home': [('default', 'ok')],
    'settings: settings.json links out of the home': [('default', 'ok')],
    'settings: settings.json links inside the home': [('default', 'ok'), ('custom', 'ok')],
    'homePath: in-home link to a profile outside the home': [('default', 'ok'), ('linked', 'unavailable')],
}


def main(binary):
    binary = str(pathlib.Path(binary).resolve())
    counts = {'equal': 0, 'fallback': 0}
    failures = []

    def record(label, outcome, detail, allowed=None):
        print(f'  {outcome:8} {label}')
        if outcome == 'differs':
            expected, actual = detail
            failures.append(f'{label}: agent answer differs from the Python helper\n'
                            f'    python {rows(expected)}\n    agent  {actual.get("profiles") and rows(actual)}')
            return
        if allowed and outcome != allowed:
            failures.append(f'{label}: expected {allowed}, got {outcome}')
            return
        counts[outcome] += 1

    # MED-1: more than 64 sibling homes, created in descending order (not alphabetical).
    for count in (63, 64, 65, 66):
        with tempfile.TemporaryDirectory(prefix='aac-signin-review-') as tmp:
            home = pathlib.Path(tmp)
            for i in reversed(range(count)):
                (home / '.claude-t3' / f'slot{i:03}').mkdir(parents=True)
            outcome, detail = compare(binary, home, f'{count} sibling homes')
            record(f'{count} sibling homes', outcome, detail, 'fallback' if count > 64 else None)
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
                label = f'{len(prefix) + 1} leading slashes, {driver}'
                record(label, *compare(binary, home, label))
    # Wonder M1: link shapes on the settings path, configured homes and sibling homes.
    for label, build, allowed in SHAPES:
        with tempfile.TemporaryDirectory(prefix='aac-signin-review-') as tmp:
            home, outside = pathlib.Path(tmp) / 'home', pathlib.Path(tmp) / 'outside'
            home.mkdir()
            outside.mkdir()
            login(home / '.claude')
            login(home / 'custom')
            build(home, outside)
            outcome, detail = compare(binary, home, label)
            if label in WONDER and outcome != 'differs':
                python_rows = rows(detail)
                assert python_rows == WONDER[label], f'{label}: Python reference changed: {python_rows}'
            record(label, outcome, detail, allowed)
    # Wonder L8: an inherited selector that is not UTF-8. Python keeps it as an identity (its row
    # is published); the agent must not read it as unset. The UTF-8 spelling is the control.
    for label, name, allowed in (('ambient selector: UTF-8 (control)', b'profile', 'equal'),
                                 ('ambient selector: not UTF-8', b'prof\xff', 'fallback')):
        with tempfile.TemporaryDirectory(prefix='aac-signin-review-') as tmp:
            home = pathlib.Path(tmp)
            login(home / '.claude')
            login(pathlib.Path(os.fsdecode(os.fsencode(home) + b'/' + name)))
            outcome, detail = ambient(binary, home, os.fsencode(home) + b'/' + name)
            python_rows = rows(detail if outcome != 'differs' else detail[0])
            assert len(python_rows) == 2 and python_rows[1][1] == 'ok', f'{label}: Python reference changed: {python_rows}'
            record(label, outcome, detail, allowed)
    # The owner rule of `_read_present`: a settings file another user owns is not read, and
    # Python's read_failed() marks discovery incomplete.
    if '--foreign-root' in sys.argv:
        root = pathlib.Path(sys.argv[sys.argv.index('--foreign-root') + 1])
        with tempfile.TemporaryDirectory(prefix='aac-signin-review-', dir=root) as tmp:
            home = pathlib.Path(tmp)
            login(home / '.claude')
            login(home / 'custom')
            settings_file(home, None)
            owner = (home / '.t3/userdata/settings.json').lstat().st_uid
            assert owner != os.getuid(), f'{root} gives new files this user ({owner}); pick another --foreign-root'
            label = 'settings: owned by another user'
            outcome, detail = compare(binary, home, label)
            python_rows = rows(detail if outcome != 'differs' else detail[0])
            assert python_rows == [('default', 'ok'), ('saved-profiles', 'unavailable')], f'{label}: Python reference changed: {python_rows}'
            record(label, outcome, detail)
    else:
        print('SKIP settings owned by another user: no --foreign-root given')
    assert not failures, '\n'.join(failures)
    print(f'PASS signin-expiry review witnesses: {counts}')


if __name__ == '__main__':
    main(sys.argv[1])
