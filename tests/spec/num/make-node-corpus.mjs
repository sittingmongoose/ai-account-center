#!/usr/bin/env node
/**
 * L3 numeric corpus from the real JavaScript engine: writes ecma.json, intl.json, dates.json and collation.json
 * beside this script. `crates/aac-num/tests/corpus.rs` checks every row against aac-num. Deterministic (seeded);
 * rerun after changing it, under the Node the dashboard runs (the files record `process.version`):
 *
 *   node tests/spec/num/make-node-corpus.mjs
 *
 * Values: signed zero, NaN, the infinities, the extremes and the subnormals; both neighbouring doubles of powers of
 * ten, of decimal halves at every place, of powers of two and of the reviewers' witnesses; exact ties between two
 * shortest digit strings (an odd multiple of half a decimal unit that is a double); eighth steps near 2^38..2^53;
 * random bit patterns over all of binary64; random human-sized decimals.
 */
import { writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const buffer = new DataView(new ArrayBuffer(8));
const bitsOf = (x) => {
  buffer.setFloat64(0, x);
  return buffer.getBigUint64(0).toString(16).padStart(16, '0');
};
const fromBits = (b) => {
  buffer.setBigUint64(0, BigInt.asUintN(64, b));
  return buffer.getFloat64(0);
};
const step = (x, n) => {
  buffer.setFloat64(0, x);
  return fromBits(buffer.getBigUint64(0) + BigInt(n));
};
let state = 0x13a7c0de;
const random = () => {
  state = (state + 0x6d2b79f5) >>> 0;
  let t = state;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
};
const int = (n) => Math.floor(random() * n);

const values = [];
const seen = new Set();
const add = (x) => {
  for (const v of [x, -x]) {
    const key = bitsOf(v);
    if (!seen.has(key)) {
      seen.add(key);
      values.push(v);
    }
  }
};
const around = (x, width = 1) => {
  if (!Number.isFinite(x)) return add(x);
  for (let k = -width; k <= width; k++) {
    const v = step(Math.abs(x), k);
    if (Number.isFinite(v) && v >= 0) add(v);
  }
};

for (const x of [
  0,
  NaN,
  Infinity,
  Number.MAX_VALUE,
  Number.MIN_VALUE,
  2.2250738585072014e-308,
  2 ** 53,
  2 ** 52,
  2 ** 51,
])
  around(x, 2);
for (let e = -324; e <= 308; e += e >= -30 && e <= 30 ? 1 : 3) around(Number(`1e${e}`));
for (let e = -24; e <= 22; e++) for (let d = 1; d < 10; d++) around(Number(`${d}5e${e - 1}`));
for (let e = -1074; e <= 1023; e += 13) around(2 ** e);
for (const x of [
  100000000000000.12,
  140737488355327.12,
  140737488355327.125,
  2251799813685247.2,
  2251799813685247.25,
  33.333333333333336,
  0.1 + 0.2,
  1.005,
  2.675,
  4.85,
  1.0625,
  1e23,
  9.999999999999999e22,
  99.853515625,
  99.26757812,
  98.29101562,
  0.146484375,
  1e21,
  999999999999999900000,
  1e-7,
  1e-6,
  1e16,
  1e17,
  0.0001,
  0.00001,
])
  around(x, 2);
// Eighth steps near powers of two (the Sol Home matrix).
for (let j = 38; j <= 53; j++)
  for (let k = 1; k <= 9; k++) for (const x of [2 ** j - k / 8, 2 ** j + k / 8]) around(x);
// Exact shortest-digit ties: a double ending in a decimal 5 one place past 17 digits, with both 17-digit neighbours
// reading back (ulp = 2^-1 .. 2^-6 near 2^44 .. 2^52).
for (let j = 44; j <= 52; j++)
  for (let i = 0; i < 10; i++) {
    const base = 2 ** j + int(2 ** (j - 1));
    for (const frac of [0.5, 0.25, 0.75, 0.125, 0.375, 0.625, 0.875, 0.0625, 0.1875, 0.03125])
      around(base + frac, 0);
  }
// Decimal halves at 1..17 significant digits (toFixed, toPrecision and Intl ties) and their neighbours.
for (let i = 0; i < 700; i++) {
  const digits = 1 + int(16);
  const point = int(digits + 1);
  let s = String(1 + int(9));
  for (let k = 1; k < digits; k++) s += String(int(10));
  s += '5';
  around(Number(`${s.slice(0, point) || '0'}.${s.slice(point)}e${int(30) - 15}`));
}
// Random bit patterns over all of binary64, and human-sized decimals.
for (let i = 0; i < 1200; i++) {
  const hi = BigInt(int(2 ** 31)) << 32n;
  const v = fromBits((hi | BigInt(int(2 ** 32))) & 0x7fffffffffffffffn);
  if (Number.isFinite(v)) add(v);
}
for (let i = 0; i < 1500; i++) add(Number((random() * 10 ** int(13)).toFixed(int(7))));
for (let i = 0; i < 500; i++) add(int(1000) / 2 ** int(12));
// Integer ratios as the views make them (percents, shares of a total).
for (let i = 0; i < 500; i++) {
  const limit = 1 + int(5000);
  add((100 * int(limit + 1)) / limit);
}

const FIXED = [0, 1, 2, 6];
const PRECISION = [1, 2, 4, 10, 17];
const rows = values.map((x) => [
  bitsOf(x),
  String(x),
  bitsOf(Math.round(x)),
  FIXED.map((f) => x.toFixed(f)),
  PRECISION.map((p) => x.toPrecision(p)),
]);
// Longer forms and the 100-digit limits on a sample.
const wide = values
  .filter((_, i) => i % 9 === 0)
  .map((x) => [
    bitsOf(x),
    x.toFixed(20),
    x.toFixed(100),
    x.toPrecision(3),
    x.toPrecision(21),
    x.toPrecision(100),
  ]);
const json = values
  .filter((_, i) => i % 11 === 0)
  .concat([NaN, Infinity, -Infinity, -0])
  .map((x) => [bitsOf(x), JSON.stringify(x)]);
// Sums as `reduce((a, b) => a + b, 0)`.
const sums = [];
for (let i = 0; i < 400; i++) {
  const list = Array.from({ length: int(6) }, () =>
    int(4) === 0 ? -0 : values[int(values.length)]
  );
  sums.push([list.map(bitsOf), bitsOf(list.reduce((a, b) => a + b, 0))]);
}
sums.push([[bitsOf(-0), bitsOf(-0)], bitsOf([-0, -0].reduce((a, b) => a + b, 0))]);
const write = (name, body) => {
  writeFileSync(
    path.join(here, name),
    JSON.stringify({ schemaVersion: 1, node: process.version, ...body }) + '\n'
  );
  console.log(
    `${name}: ${Object.entries(body)
      .map(([k, v]) => (Array.isArray(v) ? `${k} ${v.length}` : ''))
      .filter(Boolean)
      .join(', ')}`
  );
};
write('ecma.json', { fixedPlaces: FIXED, precisionDigits: PRECISION, rows, wide, json, sums });

// Intl.NumberFormat en-US as the page builds it (view-model.mjs).
const nf2 = new Intl.NumberFormat('en-US', { maximumFractionDigits: 2 });
const usd = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  maximumFractionDigits: 2,
});
const compact = new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 2 });
const intlValues = values.concat([NaN, Infinity, -Infinity]);
write('intl.json', {
  icu: process.versions.icu,
  rows: intlValues.map((x) => [bitsOf(x), nf2.format(x), usd.format(x), compact.format(x)]),
});

// Dates.
const strings = [];
const pad = (n, w) => String(n).padStart(w, '0');
for (let i = 0; i < 1500; i++) {
  const y = int(10) === 0 ? int(10000) : 1970 + int(330);
  const mo = int(14);
  const d = int(33);
  const h = int(26);
  const mi = int(62);
  const s = int(62);
  const ms = int(1000);
  const zone = ['Z', '+05:30', '-08:00', '+00:00', '-23:59', '+24:00', ''][int(7)];
  const shape = int(6);
  let text = `${pad(y, 4)}-${pad(mo, 2)}-${pad(d, 2)}`;
  if (shape > 0) text += `T${pad(h, 2)}:${pad(mi, 2)}`;
  if (shape > 1) text += `:${pad(s, 2)}`;
  if (shape > 2) text += `.${pad(ms, 3)}`;
  if (shape > 0 || int(2)) text += zone;
  strings.push(text);
}
for (let i = 0; i < 300; i++) strings.push(new Date(int(253402300799999)).toISOString());
strings.push(
  '',
  '2026-01-05',
  '2026-01',
  '2026',
  '2026-01-05T00:07:00',
  '2026-01-05T00:07Z',
  '2026-01-05T24:00:00Z',
  '2026-02-29T00:00:00Z',
  '2024-02-29T12:00:00Z',
  '+010000-01-01T00:00:00.000Z',
  '+002026-01-05T00:07:00Z',
  '-000001-01-01T00:00:00Z',
  '2026-01-05t00:07z',
  '2026-01-05 00:07:00Z',
  '2026-01-05T00:07:00.123456Z',
  '2026-01-05T00:07:00.1Z',
  'Mon, 05 Jan 2026 00:07:00 GMT',
  'never',
  '0000-01-01T00:00:00Z',
  '9999-12-31T23:59:59.999Z'
);
const parses = strings.map((s) => [s, Number.isFinite(Date.parse(s)) ? Date.parse(s) : null]);
const instants = [
  0, -1, 1, 253402300799999, 253402300800000, -62198755200000, 8.64e15, -8.64e15, 1794715260983,
];
for (let i = 0; i < 1500; i++) instants.push(Math.floor((random() * 2 - 1) * 8.64e15));
for (let i = 0; i < 1500; i++) instants.push(int(253402300800000));
const iso = instants.map((ms) => [ms, new Date(ms).toISOString()]);
const civil = [];
for (let i = 0; i < 2000; i++) {
  const days = int(2 * 3_000_000) - 3_000_000;
  const at = new Date(days * 86400000);
  civil.push([days, at.getUTCFullYear(), at.getUTCMonth() + 1, at.getUTCDate(), at.getUTCDay()]);
}
// Fable's fractional board times: status-view-words.ts ageSeconds on fs.Stats.mtimeMs.
const ages = [];
for (let i = 0; i < 1500; i++) {
  const mtime =
    1794715260983 + (random() - 0.5) * 1e7 + int(1000) / 1000 + (int(2) ? random() / 1000 : 0);
  const now = Math.floor(mtime + (random() - 0.2) * 1e6);
  ages.push([now, bitsOf(mtime), bitsOf(Math.max(0, Math.round((now - mtime) / 1000)))]);
}
write('dates.json', { parses, iso, civil, ages });

// localeCompare on printable ASCII: the single-character order with its primary groups, then a pair corpus.
const chars = [];
for (let c = 0x20; c < 0x7f; c++) chars.push(String.fromCharCode(c));
const order = [...chars].sort((a, b) => a.localeCompare(b));
const base = new Intl.Collator(undefined, { sensitivity: 'base' });
const primaryEqual = order.filter((c, i) => i > 0 && base.compare(order[i - 1], c) === 0);
const alphabet = ' _-.,:aAbBzZ09@/~$\'"#';
const pick = () => (int(3) === 0 ? chars[int(chars.length)] : alphabet[int(alphabet.length)]);
const word = () => Array.from({ length: int(6) }, pick).join('');
const pairs = [];
for (const a of chars) for (const b of chars) if (int(4) === 0) pairs.push([a, b]);
for (let i = 0; i < 16000; i++) {
  const a = word();
  let b = int(3) === 0 ? a.slice(0, int(a.length + 1)) + word().slice(0, int(3)) : word();
  if (int(5) === 0) b = a.toUpperCase();
  pairs.push([a, b]);
}
for (let i = 0; i < 2000; i++) pairs.push([`cursor:${word()}`, `cursor:${word()}`]);
write('collation.json', {
  icu: process.versions.icu,
  order: order.join(''),
  primaryEqual: primaryEqual.join(''),
  pairs: pairs.map(([a, b]) => [a, b, Math.sign(a.localeCompare(b))]),
});
