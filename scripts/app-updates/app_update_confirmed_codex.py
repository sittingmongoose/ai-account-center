#!/usr/bin/env python3
"""Linux-only fresh idle Codex terminal relaunch; no package/account mutation.

A trusted local runtime supplies one context over stdin. Credentials remain in
memory. Only fixed installed standalone or official npm Codex native binaries can be launched.
"""
import json
import os
import pathlib
import re
import sys
from app_update_common import Install
from app_update_processes import Process
from app_update_terminal import check_terminal, restart_cli


def installed_codex(candidate):
    """Only fixed standalone or official installed npm native binary layouts."""
    candidate = candidate.resolve()
    if candidate.name != 'codex' or not candidate.is_file() or candidate.stat().st_uid not in (0, os.getuid()):
        raise ValueError('Unsupported executable.')
    releases = pathlib.Path.home() / '.codex/packages/standalone/releases'
    try:
        relative = candidate.relative_to(releases.resolve()).parts
        if len(relative) == 3 and relative[1:] == ('bin', 'codex') and re.fullmatch(r'[0-9][0-9A-Za-z._-]{0,127}', relative[0]):
            return candidate
    except ValueError:
        pass
    roots = [pathlib.Path.home() / '.local/lib/node_modules/@openai/codex', pathlib.Path('/usr/local/lib/node_modules/@openai/codex'), pathlib.Path('/usr/lib/node_modules/@openai/codex')]
    for root in roots:
        try:
            relative = candidate.relative_to(root.resolve()).parts
            info_path = root / 'package.json'
            if info_path.stat().st_size > 65536 or json.loads(info_path.read_text()).get('name') != '@openai/codex':
                continue
            architectures = {'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl'}
            nested = len(relative) == 7 and relative[:2] == ('node_modules', '@openai') and relative[2] in {'codex-linux-x64', 'codex-linux-arm64'} and relative[3] == 'vendor' and relative[4] in architectures and relative[5:] == ('bin', 'codex')
            bundled = len(relative) == 4 and relative[0] == 'vendor' and relative[1] in architectures and relative[2:] in {('bin', 'codex'), ('codex', 'codex')}
            if nested or bundled:
                return candidate
        except (OSError, ValueError, TypeError):
            continue
    raise ValueError('Unsupported executable.')


def restart(value, check=False):
    if not sys.platform.startswith('linux') or not isinstance(value, dict) or set(value) not in ({'exe', 'cwd', 'env'}, {'exe', 'cwd', 'env', 'args'}):
        raise ValueError('Unsupported context.')
    candidate = pathlib.Path(value['exe']) if isinstance(value['exe'], str) else None
    if candidate is None:
        raise ValueError('Unsupported executable.')
    active = installed_codex(candidate)
    cwd, env, args = value['cwd'], value['env'], value.get('args', [])
    if not isinstance(cwd, str) or not pathlib.Path(cwd).is_absolute() or not pathlib.Path(cwd).is_dir() or args != [] or not isinstance(env, dict) or len(env) > 4096:
        raise ValueError('Unsupported context.')
    for key, content in env.items():
        if not isinstance(key, str) or not key or '=' in key or '\0' in key or not isinstance(content, str) or '\0' in content:
            raise ValueError('Unsupported environment.')
    codex_home = env.get('CODEX_HOME') or str(pathlib.Path(env.get('HOME', str(pathlib.Path.home()))) / '.codex')
    if pathlib.Path(codex_home).resolve() != (pathlib.Path.home() / '.codex').resolve():
        raise ValueError('Unsupported Codex home.')
    context = Process(0, 0, os.getuid(), str(active), 'new', args=[str(active)], cwd=cwd, env=env)
    check_terminal('ubuntu', [context])
    if check:
        return {'success': True}
    return {'success': True, 'restartTargets': restart_cli(Install('codex-cli', 'ubuntu', active), [context])}


def main():
    try:
        raw = sys.stdin.buffer.read(2 * 1024 * 1024 + 1)
        if len(raw) > 2 * 1024 * 1024:
            raise ValueError('Oversized context.')
        if sys.argv[1:] not in ([], ['--check']):
            raise ValueError('Unsupported invocation.')
        result = restart(json.loads(raw), check=sys.argv[1:] == ['--check'])
    except Exception:
        result = {'success': False, 'message': 'The confirmed Codex CLI could not reopen in a fresh terminal.'}
    print(json.dumps(result, ensure_ascii=True, separators=(',', ':')))
    return 0 if result['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
