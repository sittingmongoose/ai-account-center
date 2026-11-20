/** Reconstruct the documented M6 gate categories from the audit's immutable source tree. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const source = 'df26191524';
const files = execFileSync('git', ['ls-tree', '-r', '--name-only', '-z', source], {
  encoding: 'utf8',
}).split('\0').filter(Boolean);
const testFile = /(?:\.(?:test|spec)\.[cm]?[jt]s|(?:_test|test_)\w*\.py|Tests?\.swift|Tests?\.cs)$/;
const jsTest = /\.(?:test|spec)\.[cm]?[jt]s$/;
const safetyName = /auth|session|origin|confirm|auto-switch|identity|reset.claim|guard|trusted|rate.limit/i;
const serverMarker = /\.listen\(|\bstartServer\(|\bcreateServer\(|\bsupertest\b/;
const browserCheck = /^web-dashboard\/tests\/browser\/checks\/.*\.[cm]?[jt]s$/;
const blob = (file) => execFileSync('git', ['show', source + ':' + file], { encoding: 'utf8' });

const safety = files.filter((file) => safetyName.test(file) && (
  testFile.test(file) ||
  browserCheck.test(file) ||
  // The Status test starts this safety fixture as its own fault-injection process.
  file === 'tests/unit/web-server/status/status-batch-identity-worker.ts' ||
  (file.endsWith('.rs') && /#\[(?:tokio::)?test\]/.test(blob(file)))
));
const http = files.filter((file) => jsTest.test(file) && serverMarker.test(blob(file)));
assert.equal(safety.length, 75);
assert.equal(http.length, 54);

const output = JSON.stringify({
  source: execFileSync('git', ['rev-parse', source], { encoding: 'utf8' }).trim(),
  provenance: 'Reconstructed from AUDIT.md Appendix C and the M0 inventory selectors.',
  safety,
  http,
}, null, 2) + '\n';
const fixture = new URL('./m6-historical-gates.json', import.meta.url);
if (process.argv.includes('--write')) fs.writeFileSync(fixture, output);
else assert.equal(fs.readFileSync(fixture, 'utf8'), output);
console.log('M6 historical gate reconstruction: 75 safety files and 54 HTTP files.');
