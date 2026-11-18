// The Leptos fixture server's read contract: every address the frame reads must answer, never 404
// (TOOL-ISSUES duo-Astra-5-L1, Perf G5 row 39, rail3-T1). Starts the real dev server on a free port;
// fixture data only, no browser, no real home.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const servePath = path.join(repo, 'web-dashboard/leptos/dev/serve.mjs');

let child = null;
let base = '';

function freePort() {
  return new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = probe.address().port;
      probe.close(() => resolve(port));
    });
  });
}

async function waitUp() {
  for (let i = 0; i < 100; i += 1) {
    try {
      const r = await fetch(`${base}/api/auth/check`);
      if (r.ok) return;
    } catch {
      // not listening yet
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  throw new Error('fixture server did not start');
}

before(async () => {
  const port = await freePort();
  base = `http://127.0.0.1:${port}`;
  child = spawn(process.execPath, [servePath, String(port)], { cwd: repo, stdio: 'pipe' });
  await waitUp();
});

after(() => {
  if (child) child.kill();
});

test('computers.json is served unchanged (duo-Astra-5-L1)', async () => {
  const r = await fetch(`${base}/computers.json`);
  assert.equal(r.status, 200);
  const expected = fs.readFileSync(path.join(repo, 'web-dashboard/public/computers.json'), 'utf8');
  assert.equal(await r.text(), expected);
});

test('Details reads an honest empty reset inventory, not a 404 (Perf G5 row 39)', async () => {
  const r = await fetch(`${base}/api/accounts/synthetic-account/reset-claims`);
  assert.equal(r.status, 200);
  assert.deepEqual(await r.json(), { status: 'ok', grants: [], history: [], pendingAttempt: null });
});

test('Analytics reads an empty view in the product shape, not a 404 (rail3-T1)', async () => {
  const r = await fetch(
    `${base}/api/accounts/analytics?platform=mac&range=7d&provider=all&account=all`
  );
  assert.equal(r.status, 200);
  const view = await r.json();
  // The frame admits exactly this shape (frame-analytics.mjs): schemaVersion 1 and an accounts array.
  assert.equal(view.schemaVersion, 1);
  assert.deepEqual(view.accounts, []);
  assert.deepEqual(view.providers, []);
  assert.equal(view.range.preset, '7d');
  assert.ok(Number.isFinite(Date.parse(view.range.from)));
  assert.ok(Number.isFinite(Date.parse(view.range.to)));
  assert.equal(view.summary.accountCount, 0);
  assert.equal(view.history.sampleCount, 0);
});
