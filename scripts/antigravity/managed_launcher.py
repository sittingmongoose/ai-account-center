#!/usr/bin/env python3
"""User-invoked ordinary agy launcher; no account switching or prompt replay."""
import hashlib
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_installation import InstallationError, launcher_argv, load_installation


def main(args):
    try:
        installation = load_installation(require_native_pin=False)
        # A new official binary remains usable. Only managed activation needs
        # a new independently verified native version/session gate.
        digest = hashlib.sha256()
        with Path(installation['nativeBinary']).open('rb') as stream:
            while raw := stream.read(1024 * 1024): digest.update(raw)
        command = launcher_argv(installation, args,
                                native_pin_matches=digest.hexdigest() == installation['nativeSha256'])
        # Keep HOME, cwd, environment and native command arguments exactly.
        os.execv(command[0], command)
    except (InstallationError, OSError, ValueError, TypeError, KeyError):
        sys.stderr.write('The Antigravity account switching helper needs to be set up again.\n')
        return 1


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
