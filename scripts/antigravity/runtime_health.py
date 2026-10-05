#!/usr/bin/env python3
"""Read-only health of an installed runtime bundle. No action occurs on import.

The resident service runs `/usr/bin/python3 -I <bundle>/lib/resident_main.py`.
This asks that same interpreter, in a fresh process started with `-I -B` (no
user site, no bytecode written into the immutable bundle), whether the bundle's
pinned parser packages load. It prints one JSON line and never reads accounts,
credentials, settings or history.

Bundles built before the version-neutral parser directory kept the parser in the
venv's `lib/pythonX.Y/site-packages`. A system Python upgrade (for example the
Ubuntu 26.04 move from Python 3.13 to 3.14) leaves that directory behind, so the
service then fails with `No module named 'pyte'`. This check names that cause.
"""
from __future__ import annotations
import importlib.metadata
import json
from pathlib import Path
import re
import subprocess
import sys

SERVICE_PYTHON = '/usr/bin/python3'
PARSER_PINS = (('pyte', '0.8.2'), ('wcwidth', '0.9.1'))
# Results that mean the service cannot start from this bundle; a probe that
# could not run at all is not one of them (nothing is known).
BROKEN_PARSER = frozenset({'missing-python-module', 'parser-mismatch', 'parser-import-failed'})
MODULE_RE = re.compile(r'[A-Za-z_][A-Za-z0-9_.]{0,63}$')
MINOR_RE = re.compile(r'\d{1,2}\.\d{1,3}$')


def built_for(bundle):
    """The Python minor version the bundle's venv was created with, or None."""
    try:
        raw = (Path(bundle) / 'venv/pyvenv.cfg').read_bytes()[:4096].decode('utf-8', 'replace')
    except OSError:
        return None
    match = re.search(r'^version(?:_info)?\s*=\s*(\d{1,2})\.(\d{1,3})', raw, re.M)
    return '%s.%s' % match.groups() if match else None


def parser_directory(bundle, version_info=None):
    """Where that bundle's resident_main.py loads its pinned parser from."""
    bundle = Path(bundle)
    neutral = bundle / 'parser'
    if neutral.is_dir():
        return neutral
    major, minor = (version_info or sys.version_info)[:2]
    return bundle / 'venv/lib' / ('python%d.%d' % (major, minor)) / 'site-packages'


def check_current_interpreter(bundle):
    """In-process check. Run only in a fresh interpreter: it edits sys.path."""
    bundle = Path(bundle)
    result = {'ok': False, 'reason': None, 'module': None,
              'python': '%d.%d' % sys.version_info[:2], 'builtFor': built_for(bundle)}
    directory = parser_directory(bundle)
    if directory.is_dir():
        sys.path.insert(0, str(directory))
    for name, version in PARSER_PINS:
        try:
            module = __import__(name)
        except ImportError as error:
            missing = getattr(error, 'name', None)
            result.update(reason='missing-python-module',
                          module=missing if type(missing) is str and MODULE_RE.match(missing) else name)
            return result
        except Exception:
            result.update(reason='parser-import-failed', module=name)
            return result
        try:
            installed = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            installed = None
        location = Path(getattr(module, '__file__', None) or '/').resolve()
        if (installed != version or not directory.is_dir() or
                not location.is_relative_to(directory.resolve())):
            result.update(reason='parser-mismatch', module=name)
            return result
    result['ok'] = True
    return result


def normalize(value):
    """A bounded copy of a probe result; anything unexpected reads as unknown."""
    if type(value) is not dict:
        return {'ok': False, 'reason': 'probe-failed', 'module': None, 'python': None, 'builtFor': None}
    reason = value.get('reason')
    module = value.get('module')
    python, built = value.get('python'), value.get('builtFor')
    ok = value.get('ok') is True
    return {
        'ok': ok,
        'reason': None if ok else reason if reason in BROKEN_PARSER else 'probe-failed',
        'module': module if type(module) is str and MODULE_RE.match(module) else None,
        'python': python if type(python) is str and MINOR_RE.match(python) else None,
        'builtFor': built if type(built) is str and MINOR_RE.match(built) else None,
    }


def probe(bundle, *, python=SERVICE_PYTHON, run=subprocess.run, timeout=15):
    """Ask the service interpreter about a bundle, in a fresh read-only process."""
    try:
        completed = run([python, '-I', '-B', str(Path(__file__).resolve()), str(Path(bundle))],
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                        timeout=timeout, check=False)
        raw = completed.stdout[:8192] if type(completed.stdout) is bytes else b''
        return normalize(json.loads(raw.decode('utf-8')))
    except (OSError, ValueError, subprocess.SubprocessError):
        return normalize(None)


def describe(result):
    """Plain words for a broken result, such as 'missing Python module pyte'."""
    if result.get('ok') is True:
        return 'the parser loads'
    module = result.get('module') or 'unknown'
    if result.get('reason') == 'missing-python-module':
        text = 'missing Python module %s' % module
    elif result.get('reason') == 'parser-mismatch':
        text = 'Python module %s is not the pinned version' % module
    elif result.get('reason') == 'parser-import-failed':
        text = 'Python module %s does not load' % module
    else:
        return 'the parser check could not run'
    python, built = result.get('python'), result.get('builtFor')
    if python and built and python != built:
        text += ' (system Python is %s; the bundle was built for %s)' % (python, built)
    return text


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    require_ok = bool(args) and args[0] == '--require-ok'
    if require_ok:
        args = args[1:]
    if len(args) != 1 or not Path(args[0]).is_absolute():
        sys.stderr.write('Usage: runtime_health.py [--require-ok] <absolute bundle directory>\n')
        return 2
    result = check_current_interpreter(args[0])
    print(json.dumps(result))
    return 1 if require_ok and result['ok'] is not True else 0


if __name__ == '__main__':
    raise SystemExit(main())
