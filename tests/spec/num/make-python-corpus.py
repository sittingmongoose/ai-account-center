#!/usr/bin/env python3
"""L3 numeric corpus from the real CPython: writes python.json beside this script, checked against aac-num's `py`
module by crates/aac-num/tests/corpus.rs. Deterministic (seeded); rerun after changing it, with the Python AAC's
helpers run under (the file records the version):

    python3 tests/spec/num/make-python-corpus.py

Rows: round(x, n) on exact ties (odd multiples of 2^-k, the Kimi and Z.ai witnesses of the big review), both
neighbouring doubles of each, powers of two and ten, signed zero, subnormals, huge values and random bit patterns;
repr and json.dumps text; datetime.fromtimestamp(ms / 1000, utc) milliseconds (signin_expiry.iso), including the
final Sol max witnesses 8796093022210 and 9999999999001 and their neighbours; plan_common.reset_at's seconds forms.
"""
import datetime as dt
import json
import math
import random
import struct
import sys
from pathlib import Path

rng = random.Random(0x13A7)


def bits(x):
    return struct.pack('>d', x).hex()


def from_bits(b):
    return struct.unpack('>d', b.to_bytes(8, 'big'))[0]


values = []
seen = set()


def add(x):
    for v in (x, -x):
        key = bits(v)
        if key not in seen:
            seen.add(key)
            values.append(v)


def around(x, width=1):
    if not math.isfinite(x):
        add(x)
        return
    v = abs(x)
    add(v)
    lo = hi = v
    for _ in range(width):
        lo, hi = math.nextafter(lo, -math.inf), math.nextafter(hi, math.inf)
        if lo >= 0:
            add(lo)
        if math.isfinite(hi):
            add(hi)


for x in [0.0, math.nan, math.inf, sys.float_info.max, 5e-324, sys.float_info.min, 2.0 ** 53, 2.0 ** 52]:
    around(x, 2)
for e in range(-324, 309, 3):
    around(float(f'1e{e}'))
for e in range(-1074, 1024, 17):
    around(2.0 ** e)
# Exact ties at 0..17 places: odd multiples of 2^-k (k <= 60) are exact decimal halves at k places.
for k in range(1, 61):
    for _ in range(12):
        around((2 * rng.randrange(1, 1 << min(k, 50)) + 1) / 2 ** (k + 1))
# The quota collector's remainingPercent: 100 - 100 * used / limit with power-of-two limits (Kimi 2048, Z.ai 2^20).
for used, limit in [(3, 2048), (7, 2048), (11, 2048), (1, 2048), (5, 2048), (7680, 1 << 20), (17920, 1 << 20),
                    (1, 3), (2, 3), (12345, 99991)]:
    around(max(0, 100 - 100 * used / limit), 2)
for _ in range(400):
    limit = 1 << rng.randrange(1, 31)
    around(max(0, 100 - 100 * rng.randrange(0, limit + 1) / limit))
for x in [99.853515625, 99.26757812, 98.29101562, 2.675, 1.005, 0.125, 0.375, 2.5, 1e16, 1e17, 0.0001, 0.00001,
          2251799813685247.25, 140737488355327.125, 123456789012345678.0]:
    around(x, 2)
for _ in range(1200):
    v = from_bits(rng.getrandbits(63))
    if math.isfinite(v):
        add(v)
for _ in range(800):
    add(round(rng.uniform(0, 10 ** rng.randrange(0, 13)), rng.randrange(0, 9)))

PLACES = [0, 1, 2, 3, 8, 17, 18, 22, 30]
WIDE = [-1, -2, -5, -308, -309, 300, 323, 324]


def rounded(x, n):
    try:
        return repr(round(x, n))
    except OverflowError:
        return None


rows = [[bits(x), repr(x), json.dumps(x), [rounded(x, n) for n in PLACES]] for x in values]
wide = [[bits(x), [rounded(x, n) for n in WIDE]] for i, x in enumerate(values) if i % 7 == 0]


def iso(ms):
    try:
        return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
    except (ValueError, OverflowError, OSError):
        return None


millis = set([0, -1, 1, 1767225600000, 8796093022210, 9999999999001, 10_000_000_000_000, 1_000_000_000_000,
              253402300799999, 253402300800000, -62135596800000, -62135596800001])
for w in [8796093022210, 9999999999001]:
    millis.update(range(w - 40, w + 41))
for _ in range(3000):
    millis.add(rng.randrange(1_000_000_000_000, 10_000_000_000_001))
for _ in range(1500):
    millis.add(rng.randrange(-62135596800000, 253402300800000))
millis_rows = [[ms, iso(ms)] for ms in sorted(millis)]


def seconds(t):
    try:
        return dt.datetime.fromtimestamp(t, dt.timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z')
    except (ValueError, OverflowError, OSError):
        return None


stamps = []
for _ in range(1500):
    whole = rng.randrange(946684800, 7258118400)
    stamps.append(whole + rng.choice([0.0, 0.5, 0.9999995, 0.9999994999, 0.99999950001, 0.0000005, rng.random()]))
for _ in range(800):
    stamps.append(rng.randrange(946684800000, 7258118400000) / 1000 + rng.choice([0.0, 0.0000005, 0.0009999995]))
seconds_rows = [[repr(t), seconds(t)] for t in stamps]


def reset_iso(text):
    try:
        instant = dt.datetime.fromisoformat(text.strip().replace('Z', '+00:00'))
    except ValueError:
        return None
    return instant.astimezone(dt.timezone.utc).isoformat(timespec='seconds').replace('+00:00', 'Z')


texts = ['2026-01-01T00:00:00Z', '2026-02-29T00:00:00Z', '2024-02-29T23:59:59Z', '0001-01-01T00:00:00Z',
         '9999-12-31T23:59:59Z', '2026-13-01T00:00:00Z', '2026-01-32T00:00:00Z', '2026-01-01T24:00:00Z',
         '2026-01-01T23:60:00Z', '2026-01-01T23:59:59Z']
for _ in range(600):
    texts.append(f'{rng.randrange(1, 10000):04d}-{rng.randrange(0, 14):02d}-{rng.randrange(0, 33):02d}T'
                 f'{rng.randrange(0, 25):02d}:{rng.randrange(0, 61):02d}:{rng.randrange(0, 60):02d}Z')
reset_rows = [[t, reset_iso(t)] for t in texts]

out = Path(__file__).with_name('python.json')
out.write_text(json.dumps({
    'schemaVersion': 1, 'python': sys.version.split()[0], 'places': PLACES, 'widePlaces': WIDE,
    'rows': rows, 'wide': wide, 'millis': millis_rows, 'seconds': seconds_rows, 'resets': reset_rows,
}, separators=(',', ':')) + '\n')
print(f'{len(rows)} values, {len(wide)} wide, {len(millis_rows)} millis, {len(seconds_rows)} seconds, '
      f'{len(reset_rows)} reset strings -> {out}')
