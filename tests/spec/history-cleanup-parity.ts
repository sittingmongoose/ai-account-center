/** Snapshot cleanup HTTP admission and byte spelling from settings-review-r2. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import type { Server } from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { spyOn } from 'bun:test';
import express from 'express';

const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cleanup-http-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
fs.writeFileSync(path.join(ccs, 'config.yaml'), 'version: 1\ndashboard_auth:\n  enabled: true\n');
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_AGENT_VERBS: 'off',
  AAC_RUST_GROUP_WRITES: 'on',
});
const { RustService } = await import('../../src/web-server/rust-service');
const history = await import('../../src/web-server/services/claude-history-sync-service');
const { default: router } = await import('../../src/web-server/routes/claude-desktop-routes');
const output = { profiles: 2, targets: 3, kept: 4, deleted: 1, skipped: 2, failed: 1 };
const cleanup = spyOn(history, 'cleanupAllHistorySnapshots');
const tsOnly = process.argv.includes('--ts-only');

async function test(mode: 'off' | 'shadow' | 'on'): Promise<void> {
  const service = new RustService(mode);
  let server: Server | undefined;
  cleanup.mockClear();
  cleanup.mockResolvedValue(output);
  try {
    const app = express();
    app.use(express.json());
    app.use((req, res, next) => {
      res.locals.rustService = service;
      if (req.get('x-fixture-session') === 'yes')
        Object.assign(req, { sessionID: 'fixture-history', session: { authenticated: true } });
      next();
    });
    app.use('/api/claude', router);
    server = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    const address = server.address();
    assert.ok(address && typeof address !== 'string');
    const origin = 'http://127.0.0.1:' + address.port;
    const url = origin + '/api/claude/history-snapshots/cleanup';
    const post = (body: string, headers: Record<string, string> = {}) =>
      fetch(url, {
        method: 'POST',
        headers: {
          origin,
          'x-fixture-session': 'yes',
          'content-type': 'application/json',
          ...headers,
        },
        body,
      });
    for (const [request, status] of [
      [() => post('{}', { 'x-fixture-session': 'no' }), 401],
      [() => post('{}', { origin: 'https://foreign.test' }), 403],
      [() => post('{}', { 'content-type': 'text/plain' }), 415],
      [() => post('{"profile":"outside"}'), 400],
      [() => post('[]'), 400],
    ] as const) {
      const response = await request();
      assert.equal(response.status, status);
      await response.arrayBuffer();
    }
    assert.equal(cleanup.mock.calls.length, 0);
    const response = await post('{}');
    assert.equal(response.status, 200);
    assert.equal(await response.text(), JSON.stringify(output));
    cleanup.mockRejectedValueOnce(new Error('listing failed'));
    const failed = await post('{}');
    assert.equal(failed.status, 500);
    assert.equal(await failed.text(), '{"error":"History snapshots could not be cleaned safely."}');
    assert.equal(cleanup.mock.calls.length, 2);
  } finally {
    service.stop();
    if (server) await new Promise<void>((resolve) => server!.close(() => resolve()));
  }
}

try {
  await test('off');
  await test('shadow');
  if (!tsOnly) await test('on');
  console.log('snapshot cleanup HTTP: seven guard/outcome cases per mode with fixture jobs');
} finally {
  cleanup.mockRestore();
  fs.rmSync(home, { recursive: true, force: true });
}
