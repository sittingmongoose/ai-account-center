/** Deterministic coverage map. Heuristic kind labels are conservative, not coverage claims. */
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
const root = path.resolve(fileURLToPath(new URL('../..', import.meta.url)));
const tracked = execFileSync(
  'git',
  ['-c', `safe.directory=${root}`, 'ls-files', '--cached', '--others', '--exclude-standard', '-z'],
  { cwd: root, encoding: 'utf8' }
)
  .split('\0')
  .filter(Boolean);
const rows = [];
for (const file of [...new Set(tracked)].sort()) {
  if (file.startsWith('node_modules') || !fs.statSync(path.join(root, file)).isFile()) continue;
  const isTest =
    /(?:\.(?:test|spec)\.[cm]?[jt]s|(?:_test|test_)\w*\.py|Tests?\.swift|Tests?\.cs)$/.test(file) ||
    (/\.rs$/.test(file) &&
      /#\[(?:tokio::)?test\]/.test(fs.readFileSync(path.join(root, file), 'utf8'))) ||
    /^web-dashboard\/tests\/browser\/checks\/.*\.[cm]?[jt]s$/.test(file);
  if (!isTest) continue;
  const source = fs.readFileSync(path.join(root, file), 'utf8');
  const evidence = [];
  if (
    /auth|session|origin|confirm|auto-switch|identity|reset.claim|guard|trusted|rate.limit/i.test(
      file
    )
  )
    evidence.push('safety');
  if (
    /\.listen\(|startServer\(|createServer\(|supertest|fetch\(|browser-check|\.goto\(/.test(source)
  )
    evidence.push('HTTP');
  if (
    /\.py$/.test(file) ||
    /python3|pythonHelper|runPython|Bun\.spawn|spawnSync|execFileSync/.test(source)
  )
    evidence.push('helper');
  if (/\.each\(|testCases|cases\.json|vectors\.json/.test(source)) evidence.push('table');
  const kind = evidence[0] ?? 'internal';
  rows.push({
    file,
    kind,
    evidence,
    ...(kind === 'internal' ? { behaviourTest: 'uncovered' } : {}),
  });
}
const file = path.join(root, 'tests/spec/test-inventory.json');
const bytes =
  JSON.stringify(
    {
      schemaVersion: 1,
      policy:
        'Priority: safety, HTTP, helper, table, internal. Static classification only; internal mappings deliberately say uncovered until proven by a behaviour test.',
      tests: rows,
    },
    null,
    2
  ) + '\n';
if (process.argv.includes('--regenerate')) fs.writeFileSync(file, bytes);
else
  assert.equal(fs.readFileSync(file, 'utf8'), bytes, 'Test inventory drift; regenerate and review');
console.log(`${rows.length} test files inventoried`);
