"""Explicit uninstalled broker entrypoint. Native switching capability is false.

Running this creates only its private socket; native launch occurs solely through
an ordinary foreground launcher request. Do not run/install without root review.
"""
import argparse
import importlib.metadata
import signal
import json
import os
from pathlib import Path
import sys
LIBRARY=Path(__file__).resolve().parent
sys.path.insert(0,str(LIBRARY))
# System interpreter provides dbus. The pinned parser wheels (pure Python and
# CPython stable-ABI) live in a version-neutral bundle directory, not
# venv/lib/pythonX.Y, so a system Python upgrade cannot orphan them.
PARSER=LIBRARY.parent/'parser'
if PARSER.is_dir():sys.path.insert(0,str(PARSER))
from runtime_continuity import ContinuityError


class ParserUnavailable(Exception):
    """A pinned parser module does not import; the message names it."""


def verify_parser_environment():
    try:
        import pyte,wcwidth
    except ImportError as error:
        raise ParserUnavailable('missing Python module %s'%(error.name or 'pyte')) from None
    if (not PARSER.is_dir() or
            importlib.metadata.version('pyte')!='0.8.2' or
            importlib.metadata.version('wcwidth')!='0.9.1' or
            any(not Path(module.__file__).resolve().is_relative_to(PARSER.resolve())
                for module in (pyte,wcwidth))):
        raise ContinuityError('runtime-parser-package-mismatch')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    parser.add_argument('--database', required=True)
    parser.add_argument('--socket', required=True)
    parser.add_argument('--allowed-child', action='append', default=[])
    args = parser.parse_args()
    verify_parser_environment()
    # Imported after the parser check, so a missing module is named plainly
    # (runtime_health.py reports the same cause) instead of a traceback.
    from ubuntu_runtime_factory import create_ubuntu_resident_broker
    from native_status_composition import create_native_status_factory
    release=json.loads((LIBRARY/'release.json').read_bytes())
    home=Path.home()
    status_factory=create_native_status_factory(release=release,binary=args.binary,home=home,
        status_path=home/'.ccs/antigravity-runtime/status.sock',
        python_path=LIBRARY.parent/'venv/bin/python3',helper_path=LIBRARY/'native_status_hook.py',
        original_command_file=home/'.ccs/antigravity-switching/original-status-command.json')
    broker = create_ubuntu_resident_broker(binary=args.binary, database=args.database,
                                          socket_path=args.socket, allowed_children=[*args.allowed_child,
                                              str((LIBRARY.parent/'venv/bin/python3').resolve()),'/bin/sh','/bin/bash',
                                              str(home/'.gemini/antigravity-cli/bin/agentapi'),
                                              str(home/'.gemini/antigravity-cli/bin/webm_encoder')],
                                          capability_validator=lambda:status_factory is not None,
                                          status_service_factory=status_factory)
    signal.signal(signal.SIGTERM, lambda *_: setattr(broker, 'running', False))
    broker.serve()


if __name__ == '__main__':
    try:main()
    except ParserUnavailable as error:
        raise SystemExit('Managed Antigravity runtime failed: %s (runtime-parser-missing).'%error) from None
    except ContinuityError as error:
        raise SystemExit('Managed Antigravity runtime is unavailable (%s).'%error) from None
    except (OSError, ValueError):
        raise SystemExit('Managed Antigravity runtime is unavailable.') from None
