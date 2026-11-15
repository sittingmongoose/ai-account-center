"""Final Sol max L1 (FINAL-MAX-M1-M3-SOL.md): far-future expiry milliseconds through the complete collector.

signin_expiry.iso divides by 1000 in binary64, and datetime.fromtimestamp rounds that to the microsecond before the
milliseconds are written, so some admitted far-future times print as the millisecond before (8796093022210 is
`2248-09-26T15:10:22.209Z` in Python; the agent's exact-millisecond formatter wrote `.210Z`). The agent must give
the Python helper's exact answer (aac_num::py::iso_millis) or the labelled fallback, never another ordinary answer.

Usage: python3 tests/spec/agent/signin_iso_review.py <aac-agent> [--production]
"""
import json
import pathlib
import random
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from signin_review import compare  # noqa: E402

WITNESSES = [8_796_093_022_210, 9_999_999_999_001]


def cases():
    values = set()
    for witness in WITNESSES:
        values.update(range(witness - 3, witness + 4))
    # The admitted bounds, powers of two where the binary64 quotient's spacing changes, and present-day controls.
    values.update([1_000_000_000_000, 10_000_000_000_000, 1 << 42, (1 << 42) + 1, 1 << 43, (1 << 43) + 1,
                   1_767_225_600_000 + 86_400_000, 1_799_999_999_999])
    rng = random.Random(53)
    values.update(rng.randrange(1 << 43, 10_000_000_000_001) for _ in range(24))
    return sorted(values)


def main(binary):
    binary = str(pathlib.Path(binary).resolve())
    counts = {'equal': 0, 'fallback': 0}
    failures = []
    exercised = 0
    for ms in cases():
        with tempfile.TemporaryDirectory(prefix='aac-signin-iso-') as tmp:
            home = pathlib.Path(tmp)
            folder = home / '.claude'
            folder.mkdir()
            (folder / '.credentials.json').write_text(json.dumps({'claudeAiOauth': {
                'accessToken': 'fixture-access', 'refreshToken': 'fixture-refresh',
                'expiresAt': ms, 'refreshTokenExpiresAt': ms}}))
            outcome, detail = compare(binary, home, f'expiry {ms}')
            python = detail[0] if outcome == 'differs' else detail
            exercised += any(isinstance(p.get('accessExpiresAt'), str) for p in python.get('profiles', []))
            print(f'  {outcome:8} expiry {ms}')
            if outcome == 'differs':
                python, agent = detail
                failures.append(f'expiry {ms}: python {python.get("profiles")}\n    agent  {agent.get("profiles")}')
            else:
                counts[outcome] += 1
    if exercised != len(cases()):
        failures.append(f'only {exercised} of {len(cases())} Python replies carried an expiry word')
    print(f'signin iso review: {counts["equal"]} equal, {counts["fallback"]} fallback, {len(failures)} different')
    if failures:
        raise SystemExit('\n'.join(failures))


if __name__ == '__main__':
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    main(sys.argv[1])
