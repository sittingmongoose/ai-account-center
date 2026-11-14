import fs from 'node:fs';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { parseMachineProbe } from '../../../src/web-server/status/status-collectors';
const cases = JSON.parse(fs.readFileSync('tests/spec/agent/inventory.json', 'utf8')).cases;
for (const row of cases) {
  const expected = parseMachineProbe(row.args.platform, row.args.probe);
  const result = spawnSync(process.argv[2], {
    input:
      JSON.stringify({
        protocol: 1,
        verb: 'inventory',
        id: 'parity',
        deadlineMs: 1000,
        args: row.args,
      }) + '\n',
    encoding: 'utf8',
    timeout: 5000,
  });
  const actual = JSON.parse(result.stdout);
  assert.deepStrictEqual(
    actual.value,
    expected,
    JSON.stringify({ source: row.source, ts: expected, rust: actual })
  );
  assert.deepStrictEqual(expected, row.expected);
}
console.log(`PASS inventory: ${cases.length} TS/golden/Rust cases`);
