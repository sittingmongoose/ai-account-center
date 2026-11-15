#!/usr/bin/env python3
"""Write python-round8.json: CPython's round(x, 8) for the binary64 values the quota collector's
remainingPercent can reach (plan_common.usage_window). Every exact tie at 8 places is an odd
multiple of 1/512; the table holds sampled ties with both neighbouring doubles, the review's
Kimi witnesses, integer-ratio percents and seeded random values. Rerun after changing it:
    python3 tests/spec/agent/make-round-table.py
"""
import json
import math
import random
from pathlib import Path

values = set()
for t in range(1, 51200, 2):
    if t % 97 == 1 or t in (51125, 50925, 51175):
        values.add(t / 512)
for x in list(values):
    values.add(math.nextafter(x, math.inf))
    values.add(math.nextafter(x, -math.inf))
for used, limit in [(3, 2048), (11, 2048), (1, 2048), (5, 2048), (1, 3), (2, 3), (7, 30), (1, 7), (999, 1000), (1, 1 << 20), (12345, 99991)]:
    values.add(max(0, 100 - 100 * used / limit))
rng = random.Random(8)
for _ in range(300):
    values.add(rng.uniform(0, 100))
for _ in range(100):
    values.add(rng.uniform(0, 1e-6))
values.update([0.0, 100.0, 50.0, 5e-324, 2.2250738585072014e-308, 1e-9, 4.9999999e-9, 5e-9, 5.000000000000001e-9, 1.5e-8, 2.5e-8, 99.999999995, 99.999999985])
# repr text: every reader parses it to the same double, whatever its JSON float parser.
rows = [[repr(x), repr(round(x, 8))] for x in sorted(values)]
out = Path(__file__).with_name('python-round8.json')
out.write_text(json.dumps({'schemaVersion': 1, 'places': 8, 'rows': rows}, separators=(',', ':')) + '\n')
print(f'{len(rows)} rows -> {out}')
