// Old generators are the parity oracle only; the Rust commands never execute Node.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash, createPublicKey, verify } from 'node:crypto';
import { writeStatusFixture } from '../../../scripts/status-fixture.mjs';
import { writeScoreboardFixture } from '../../../scripts/scoreboard-fixture.mjs';
const binary = process.argv[2];
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-x4-parity-'));
console.log(`Evidence (lead cleanup): ${dir}`);
const files = (root) =>
  fs
    .readdirSync(root, { recursive: true })
    .filter((p) => fs.statSync(path.join(root, p)).isFile())
    .sort();
let count = 0;
let videoCases = 0;
function run(kind, root, now, flags = []) {
  const proc = spawnSync(
    binary,
    ['fixture', kind, root, '--now', String(now), '--json', ...flags],
    { encoding: 'utf8', env: { ...process.env, HOME: dir, CCS_DIR: '', CCS_HOME: '' } }
  );
  assert.equal(proc.status, 0, proc.stderr);
  return JSON.parse(proc.stdout);
}
for (const now of [Date.parse('2026-11-26T23:59:59.123Z'), Date.parse('2027-01-01T00:00:00Z')]) {
  for (const kind of ['status', 'scoreboard']) {
    for (const flags of kind === 'status'
      ? [
          [],
          ['--no-video'],
          ['--no-video', '--with-kinds'],
          ['--no-video', '--with-problems', '--no-quota'],
          ['--no-video', '--with-kinds', '--with-problems'],
        ]
      : [[]]) {
      const index = count++;
      const js = path.join(dir, `js-${index}`),
        rs = path.join(dir, `rs-${index}`);
      const expected =
        kind === 'status'
          ? writeStatusFixture(js, {
              now,
              withKinds: flags.includes('--with-kinds'),
              withProblems: flags.includes('--with-problems'),
              withQuota: !flags.includes('--no-quota'),
              withVideo: !flags.includes('--no-video'),
            })
          : writeScoreboardFixture(js, { now });
      const result = run(kind, rs, now, flags);
      if (
        kind === 'status' &&
        !flags.includes('--no-video') &&
        fs.existsSync(path.join(rs, 'v1/writers/vm.pm-fo-lead/images/pip-resize.mp4'))
      )
        videoCases++;
      assert.deepEqual(
        JSON.parse(JSON.stringify(result).replaceAll(rs, 'ROOT')),
        JSON.parse(JSON.stringify(expected).replaceAll(js, 'ROOT'))
      );
      assert.deepEqual(files(rs), files(js));
      for (const name of files(js)) {
        const a = fs.readFileSync(path.join(js, name));
        const b = fs.readFileSync(path.join(rs, name));
        // Paths are the sole intentional byte difference.
        assert.deepEqual(
          Buffer.from(b.toString('binary').replaceAll(rs, 'ROOT'), 'binary'),
          Buffer.from(a.toString('binary').replaceAll(js, 'ROOT'), 'binary'),
          name
        );
        if (name.startsWith('boards/'))
          assert.ok(
            Math.abs(
              fs.statSync(path.join(js, name)).mtimeMs - fs.statSync(path.join(rs, name)).mtimeMs
            ) < 1,
            name
          );
      }
    }
  }
}
const canonical = (value) =>
  value === null || typeof value !== 'object'
    ? JSON.stringify(value)
    : Array.isArray(value)
      ? `[${value.map(canonical).join(',')}]`
      : `{${Object.keys(value)
          .sort()
          .map((k) => `${JSON.stringify(k)}:${canonical(value[k])}`)
          .join(',')}}`;
const root = path.join(dir, 'signed'),
  ccs = path.join(dir, 'private');
const now = Date.parse('2026-11-26T12:00:00Z');
const result = run('status', root, now, ['--no-video', '--with-kinds', '--ccs-dir', ccs]);
assert.equal(result.answered.length, 2);
assert.equal(result.noted.length, 1);
const lines = fs
  .readFileSync(path.join(ccs, 'status/answers.jsonl'), 'utf8')
  .trim()
  .split('\n')
  .map(JSON.parse);
assert.equal(lines.length, 9);
for (const name of files(path.join(root, 'v1/answers'))) {
  const answer = JSON.parse(fs.readFileSync(path.join(root, 'v1/answers', name)));
  const { signature, ...unsigned } = answer;
  const key = createPublicKey(
    fs.readFileSync(path.join(root, 'v1/keys', `${signature.keyId}.pub`))
  );
  assert.ok(
    verify(null, Buffer.from(canonical(unsigned)), key, Buffer.from(signature.value, 'base64'))
  );
  const writer = answer.cardId.split('--')[0];
  assert.equal(
    answer.cardSha256,
    createHash('sha256')
      .update(fs.readFileSync(path.join(root, `v1/writers/${writer}/cards/${answer.cardId}.json`)))
      .digest('hex')
  );
  assert.equal(
    signature.keyId,
    createHash('sha256')
      .update(key.export({ type: 'spki', format: 'der' }))
      .digest('hex')
      .slice(0, 16)
  );
}
if (process.platform !== 'win32')
  for (const name of ['answer-key.pem', 'answers.jsonl'])
    assert.equal(fs.statSync(path.join(ccs, 'status', name)).mode & 0o777, 0o600);
// A pre-existing oracle key must be reused and existing delivery history appended.
const publicBefore = fs.readFileSync(path.join(root, 'v1/keys/aac-answers.pub'));
const again = path.join(dir, 'again');
run('status', again, now, ['--no-video', '--ccs-dir', ccs]);
assert.deepEqual(fs.readFileSync(path.join(again, 'v1/keys/aac-answers.pub')), publicBefore);
assert.equal(
  fs.readFileSync(path.join(ccs, 'status/answers.jsonl'), 'utf8').trim().split('\n').length,
  18
);
// Human CLI output stays byte-compatible after replacing the requested output paths.
for (const [kind, withStore] of [
  ['status', false],
  ['status', true],
  ['scoreboard', false],
]) {
  const left = path.join(dir, `stdout-js-${kind}-${withStore}`),
    right = path.join(dir, `stdout-rs-${kind}-${withStore}`);
  const leftStore = path.join(dir, 'stdout-private-js'),
    rightStore = path.join(dir, 'stdout-private-rs');
  const flags = kind === 'status' ? ['--no-video'] : [];
  const env = { ...process.env, HOME: dir, CCS_DIR: '', CCS_HOME: '' };
  const old = spawnSync(
    process.execPath,
    [
      fileURLToPath(new URL(`../../../scripts/${kind}-fixture.mjs`, import.meta.url)),
      left,
      ...flags,
      ...(withStore ? ['--ccs-dir', leftStore] : []),
    ],
    { encoding: 'utf8', env }
  );
  const native = spawnSync(
    binary,
    ['fixture', kind, right, ...flags, ...(withStore ? ['--ccs-dir', rightStore] : [])],
    { encoding: 'utf8', env }
  );
  assert.equal(old.status, 0, old.stderr);
  assert.equal(native.status, 0, native.stderr);
  assert.equal(
    native.stdout.replaceAll(right, 'ROOT').replaceAll(rightStore, 'STORE'),
    old.stdout.replaceAll(left, 'ROOT').replaceAll(leftStore, 'STORE')
  );
}
// The JS kit adapter must preserve its option defaults and ignore inherited store paths.
process.env.AAC_XTASK = binary;
process.env.CCS_DIR = '/do-not-touch-fixture-store';
const adapter = await import('../../../web-dashboard/tests/browser/native-fixtures.mjs');
const adapted = adapter.writeStatusFixture(path.join(dir, 'adapter'), {
  now,
  withKinds: true,
  withProblems: true,
  withQuota: false,
});
assert.equal(adapted.storeDir, null);
assert.equal(adapted.cards.length, 11);
assert.ok(!fs.existsSync(path.join(dir, 'adapter/v1/quota/report.json')));
const adaptedScoreboard = adapter.writeScoreboardFixture(path.join(dir, 'adapter-scoreboard'), {
  now,
});
assert.equal(adaptedScoreboard.runs.length, 92);
// Node must accept the native PEM key and append signed records to the same history.
writeStatusFixture(path.join(dir, 'node-key-reuse'), { now, ccsDir: ccs });
assert.equal(
  fs.readFileSync(path.join(ccs, 'status/answers.jsonl'), 'utf8').trim().split('\n').length,
  27
);
console.log(
  `PASS ${count} byte-parity cases (${videoCases} with video); 3 signed answers; key reuse and append`
);
