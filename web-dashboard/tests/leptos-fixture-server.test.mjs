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

test('computers.json is served unchanged, byte for byte (duo-Astra-5-L1)', async () => {
  const r = await fetch(`${base}/computers.json`);
  assert.equal(r.status, 200);
  // Bytes, not text: Response.text() strips a leading UTF-8 BOM, so a changed first byte would pass.
  const expected = fs.readFileSync(path.join(repo, 'web-dashboard/public/computers.json'));
  assert.equal(Buffer.compare(Buffer.from(await r.arrayBuffer()), expected), 0);
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
  // The kept-view validator requires activity (frame-analytics.mjs); the product's empty one
  // (account-analytics-service.ts) carries every field with nothing in it.
  assert.ok(view.activity && typeof view.activity === 'object');
  assert.equal(view.activity.status, 'unavailable');
  assert.equal(view.activity.scope, 'multi-host-cli');
  assert.equal(view.activity.timezone, 'America/New_York');
  assert.equal(view.activity.accountAttribution, 'unavailable');
  assert.equal(view.activity.costBasis, 'estimated-api-equivalent');
  assert.equal(view.activity.fetchedAt, null);
  assert.equal(view.activity.totals, null);
  assert.deepEqual(view.activity.providers, []);
  assert.deepEqual(view.activity.byDay, []);
  assert.deepEqual(view.activity.byHour, []);
  assert.deepEqual(view.activity.models, []);
  assert.deepEqual(view.activity.byDayModel, []);
  assert.equal(view.activity.sessions, null);
  assert.equal(view.activity.anomalies, null);
  assert.deepEqual(view.activity.sources, []);
});

test('a custom Analytics range is whole local days in the dashboard zone, like the product (lane-b-L3)', async () => {
  // 2026-11-01 is America/New_York's fall-back day: local midnight is 04:00Z and the day ends
  // 25 hours later, at 04:59:59Z the next UTC day.
  const r = await fetch(`${base}/api/accounts/analytics?range=custom&from=2026-11-01&to=2026-11-01`);
  assert.equal(r.status, 200);
  const view = await r.json();
  assert.equal(view.range.preset, 'custom');
  assert.deepEqual(view.range.days, { from: '2026-11-01', to: '2026-11-01' });
  assert.equal(view.range.from, '2026-11-01T04:00:00.000Z');
  assert.equal(view.range.to, '2026-11-02T04:59:59.000Z');
});

test('a bad custom Analytics range answers the product 400, and the server stays up (lane-b-L3)', async () => {
  const bad = [
    'range=custom&from=bad&to=2026-11-01',
    'range=custom&from=2026-11-01',
    'range=custom&from=2026-11-03&to=2026-11-01',
    'range=custom&from=2026-10-01&to=2026-11-01',
    'range=7d&from=2026-11-01',
    'range=yesterday',
  ];
  for (const query of bad) {
    const r = await fetch(`${base}/api/accounts/analytics?${query}`);
    assert.equal(r.status, 400, query);
    const body = await r.json();
    assert.equal(
      body.code,
      'invalid_range',
      `${query}: ${JSON.stringify(body)}`
    );
    assert.equal(
      body.error,
      'Select whole days from the oldest stored day through today, with the start date on or before the end date.'
    );
  }
  const ok = await fetch(`${base}/api/accounts/analytics?range=7d`);
  assert.equal(ok.status, 200);
});
