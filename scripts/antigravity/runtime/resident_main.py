"""Explicit uninstalled broker entrypoint. Native switching capability is false.

Running this creates only its private socket; native launch occurs solely through
an ordinary foreground launcher request. Do not run/install without root review.
"""
import argparse
import signal
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from ubuntu_runtime_factory import create_ubuntu_resident_broker
from runtime_continuity import ContinuityError


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    parser.add_argument('--database', required=True)
    parser.add_argument('--socket', required=True)
    parser.add_argument('--allowed-child', action='append', default=[])
    args = parser.parse_args()
    broker = create_ubuntu_resident_broker(binary=args.binary, database=args.database,
                                          socket_path=args.socket, allowed_children=args.allowed_child)
    signal.signal(signal.SIGTERM, lambda *_: setattr(broker, 'running', False))
    broker.serve()


if __name__ == '__main__':
    try:main()
    except (OSError, ContinuityError, ValueError):
        raise SystemExit('Managed Antigravity runtime is unavailable.') from None
