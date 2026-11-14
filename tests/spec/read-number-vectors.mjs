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
const file = new URL('./read-numbers.json', import.meta.url);
const bytes = JSON.stringify(rows, null, 2) + '\n';
if (process.argv.includes('--regenerate')) fs.writeFileSync(file, bytes);
else assert.equal(fs.readFileSync(file, 'utf8'), bytes, 'numeric reference vectors changed');
console.log(`${rows.length} JS numeric reference forms checked`);
