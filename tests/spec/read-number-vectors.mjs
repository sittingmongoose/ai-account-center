/** Exact JS numeric reference forms shared with the Rust read composer. */
import fs from 'node:fs';
import assert from 'node:assert/strict';
const values = [
  0, -0, 1, 1.0625, 1.2345, 4.85, 1.005, 2.675, 0.000000000000000027755575615628914, 1e-22, 1e12,
  -1.0625, -4.85,
];
const buffer = new ArrayBuffer(8);
const view = new DataView(buffer);
for (const x of [1e-22, 1e-9, 0.1, 0.5, 1, 10, 1000, 1e12]) {
  view.setFloat64(0, x);
  const bits = view.getBigUint64(0);
  for (const step of [-1n, 0n, 1n]) {
    view.setBigUint64(0, bits + step);
    values.push(view.getFloat64(0));
  }
}
let seed = 0x514e2;
for (let i = 0; i < 160; i++) {
  seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
  values.push((seed / 2 ** 32) * 10 ** ((i % 35) - 22));
}
// Keep the original forms, then add both adjacent IEEE doubles around every
// terminal x5 digit in each decimal magnitude and across integer-size transitions.
// Parse exact decimal centers once in Node; computing the center with floating
// multiplication/addition could move it one ULP and miss one requested neighbor.
const seen = new Set(values.map(bitsOf));
function bitsOf(value) {
  view.setFloat64(0, value);
  return view.getBigUint64(0).toString(16).padStart(16, '0');
}
function neighbors(value) {
  view.setFloat64(0, value);
  const bits = view.getBigUint64(0);
  return [-1n, 0n, 1n].map((step) => {
    view.setBigUint64(0, bits + step);
    return view.getFloat64(0);
  });
}
function addNeighbors(value) {
  for (const sign of [1, -1])
    for (const point of neighbors(value)) {
      const value = sign * point;
      if (!seen.has(bitsOf(value))) {
        seen.add(bitsOf(value));
        values.push(value);
      }
    }
}
for (let exponent = -23; exponent <= 12; exponent++)
  for (let digit = 0; digit < 10; digit++) addNeighbors(Number(`${digit}5e${exponent - 1}`));
const integerBases = [0, 1, 9, 10, 99, 100, 999, 1000, 1e6, 1e9, 1e12, 2 ** 51, 2 ** 52, 2 ** 53];
for (const base of integerBases)
  for (let digit = 0; digit < 10; digit++) {
    addNeighbors(Number(`${base}.${digit}5`));
    addNeighbors(Number(`${base}.0${digit}5`));
  }
const rows = [];
for (const value of values)
  for (const digits of [1, 2, 3, 4, 10])
    rows.push({
      kind: 'significant',
      value,
      digits,
      admitted: value === 0 || (Math.abs(value) >= 1e-22 && Math.abs(value) <= 1e12),
      expected: Number(value.toPrecision(digits)),
    });
for (const value of values)
  for (const places of [0, 1, 2, 6])
    rows.push({
      kind: 'fixed',
      value,
      places,
      admitted: value === 0 || (Math.abs(value) >= 1e-22 && Math.abs(value) <= 1e12),
      expected: value.toFixed(places),
    });
const operations = {
  integer: { input: (n) => Number(`${n}5e-1`), operand: (x) => x, result: (x) => Math.round(x) },
  tenth: {
    input: (n) => Number(`${n}5e-2`),
    operand: (x) => x * 10,
    result: (x) => Math.round(x * 10) / 10,
  },
  hundredth: {
    input: (n) => Number(`${n}5e-3`),
    operand: (x) => x * 100,
    result: (x) => Math.round(x * 100) / 100,
  },
  percent: {
    input: (n) => Number(`${n}5e-3`),
    operand: (x) => x * 100,
    result: (x) => Math.round(x * 100),
  },
  gib: {
    input: (n) => Number(`${(2n * n + 1n) * 536870912n}e-1`),
    operand: (x) => (x / 1073741824) * 10,
    result: (x) => Math.round((x / 1073741824) * 10) / 10,
  },
  seconds: {
    input: (n) => Number(`${n}5e2`),
    operand: (x) => x / 1000,
    result: (x) => Math.round(x / 1000),
  },
  hours: {
    input: (n) => Number((2n * n + 1n) * 1800000n),
    operand: (x) => x / 3600000,
    result: (x) => Math.round(x / 3600000),
  },
};
const roundSeen = new Set();
for (const [op, expression] of Object.entries(operations)) {
  const add = (value) => {
    const key = `${op}:${bitsOf(value)}`;
    if (!Number.isFinite(value) || roundSeen.has(key)) return;
    roundSeen.add(key);
    rows.push({
      kind: 'math-round',
      op,
      value,
      valueBits: bitsOf(value),
      operandBits: bitsOf(expression.operand(value)),
      expectedBits: bitsOf(expression.result(value)),
    });
  };
  for (const base of integerBases)
    for (let digit = 0; digit < 10; digit++)
      for (const point of neighbors(expression.input(BigInt(base) + BigInt(digit)))) {
        add(point);
        add(-point);
      }
  // Signed zero, subnormals, overflow in scaled operands, and exponent endpoints.
  for (const value of [
    0,
    -0,
    Number.MIN_VALUE,
    -Number.MIN_VALUE,
    1e-22,
    -1e-22,
    Number.MAX_VALUE,
    -Number.MAX_VALUE,
    0.049999999999999996,
  ])
    add(value);
}
// Precision-specific half points: every terminal digit and leading digit at each
// decimal magnitude, plus the carry into the next power. Add only the method's
// own form, so this checks ten-digit halves without a redundant cross-product.
const formatted = new Set(
  rows
    .filter((row) => row.kind !== 'math-round')
    .map((row) => `${row.kind}:${row.digits ?? row.places}:${bitsOf(row.value)}`)
);
function addForm(kind, precision, center) {
  for (const point of neighbors(center))
    for (const sign of [1, -1]) {
      const value = sign * point;
      const key = `${kind}:${precision}:${bitsOf(value)}`;
      if (formatted.has(key)) continue;
      formatted.add(key);
      const admitted = value === 0 || (Math.abs(value) >= 1e-22 && Math.abs(value) <= 1e12);
      rows.push(
        kind === 'significant'
          ? {
              kind,
              value,
              digits: precision,
              admitted,
              expected: Number(value.toPrecision(precision)),
            }
          : { kind, value, places: precision, admitted, expected: value.toFixed(precision) }
      );
    }
}
for (const digits of [1, 2, 3, 4, 10]) {
  const prefixes = new Set([10n ** BigInt(digits) - 1n]);
  for (let leading = 1n; leading <= 9n; leading++)
    for (let terminal = 0n; terminal <= (digits === 1 ? 0n : 9n); terminal++)
      prefixes.add(leading * 10n ** BigInt(digits - 1) + terminal);
  for (let exponent = -23; exponent <= 12; exponent++)
    for (const prefix of prefixes)
      addForm('significant', digits, Number(`${prefix}5e${exponent - digits}`));
}
for (const places of [0, 1, 2, 6])
  for (const base of integerBases) {
    const prefixes = new Set([10 ** places - 1]);
    for (let prefix = 0; prefix < (places === 2 ? 100 : 10); prefix++) prefixes.add(prefix);
    for (const prefix of prefixes) {
      const center =
        places === 0
          ? Number(`${BigInt(base) + BigInt(prefix)}.5`)
          : Number(`${base}.${String(prefix).padStart(places, '0')}5`);
      addForm('fixed', places, center);
    }
  }
const file = new URL('./read-numbers.json', import.meta.url);
const bytes = '[\n' + rows.map((row) => '  ' + JSON.stringify(row)).join(',\n') + '\n]\n';
if (process.argv.includes('--regenerate')) fs.writeFileSync(file, bytes);
else assert.equal(fs.readFileSync(file, 'utf8'), bytes, 'numeric reference vectors changed');
console.log(`${rows.length} JS numeric reference forms checked`);
