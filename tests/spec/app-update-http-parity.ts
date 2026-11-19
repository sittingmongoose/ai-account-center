/** HTTP outcomes captured from the TS router using fake jobs only. */
import assert from 'node:assert/strict';
import type { Server } from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import express from 'express';
import fixtures from './app-update-http.json';
import type { AppUpdateService } from '../../src/web-server/services/app-update-service';

const withRust = process.argv.includes('--rust-unbound');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-app-update-contract-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { mode: 0o700 });
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_RUST_GROUP_APP_UPDATES: 'on',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
const { RustService } = await import('../../src/web-server/rust-service');
const { createAppUpdateRouter } = await import('../../src/web-server/routes/app-update-routes');
const { AppUpdateBusyError } = await import('../../src/web-server/services/app-update-service');
const bridge = withRust ? new RustService('on') : null;
if (bridge) {
  await bridge.start(process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve('target/debug/aac-http'));
  assert.equal(bridge.stats.fallbacks, 0);
}
let startCalls = 0;
let current = fixtures.cases[0];
const jobs = {
  getStatus: () => ({ job: current.outcome === 'idle' ? null : fixtures.job }),
  start: () => {
    startCalls++;
    if (current.outcome === 'busy') throw new AppUpdateBusyError();
    if (current.outcome === 'failed') throw new Error('fixture');
    return { job: fixtures.job };
  },
  cancel: () => {
    if (current.outcome === 'not_owner')
      return { job: fixtures.job, cancelling: false, notOwner: true };
    if (current.outcome === 'cancelling')
      return { job: { ...fixtures.job, cancelRequested: true }, cancelling: true };
    return { job: null, cancelling: false };
  },
} as unknown as AppUpdateService;
const app = express();
app.use(express.json());
app.use((req, res, next) => {
  if (bridge) res.locals.rustService = bridge;
  if (req.get('x-fixture-session') === 'yes')
    Object.assign(req, { sessionID: 'fixture', session: { authenticated: true } });
  next();
});
app.use('/api/app-updates', createAppUpdateRouter(jobs));
const server = await new Promise<Server>((resolve) => {
  const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
});
try {
  const address = server.address();
  assert.ok(address && typeof address !== 'string');
  const origin = `http://127.0.0.1:${address.port}`;
  if (bridge) {
    const response = await fetch(origin + '/api/app-updates/status', {
      headers: { 'x-fixture-session': 'yes' },
    });
    assert.equal(response.status, 200);
    await response.arrayBuffer();
    assert.equal(bridge.stats.served, 0, 'the Rust binary must have no job owner');
    assert.equal(
      bridge.stats.fallbacks,
      1,
      'the actual Rust route must report an unbound job owner'
    );
  }
  for (const probe of fixtures.cases) {
    current = probe;
    const response = await fetch(`${origin}/api/app-updates/${probe.op}`, {
      headers: { origin, 'x-fixture-session': 'yes', 'content-type': 'application/json' },
      ...(probe.op === 'status' ? {} : { method: 'POST', body: '{}' }),
    });
    assert.equal(response.status, probe.http, `${probe.op}/${probe.outcome}`);
    assert.equal(await response.text(), JSON.stringify(probe.body));
    assert.equal(response.headers.get('cache-control'), 'no-store');
  }
  if (bridge) {
    assert.equal(bridge.stats.fallbacks, fixtures.cases.length + 1);
    assert.equal(bridge.stats.served, 0);
    const headers = { origin, 'x-fixture-session': 'yes', 'content-type': 'application/json' };
    const version = startCalls;
    const invalid = await fetch(origin + '/api/app-updates/start?host=external', {
      method: 'POST',
      headers,
      body: '{}',
    });
    assert.equal(invalid.status, 400);
    assert.equal(bridge.stats.fallbacks, fixtures.cases.length + 1);
    bridge.stop();
    const unavailable = await fetch(origin + '/api/app-updates/start', {
      method: 'POST',
      headers,
      body: '{}',
    });
    assert.equal(unavailable.status, 500);
    assert.equal(startCalls, version, 'a lost transport cannot start the TypeScript updater');
  }
} finally {
  bridge?.stop();
  await new Promise<void>((resolve) => server.close(() => resolve()));
  fs.rmSync(home, { recursive: true, force: true });
}
console.log(
  `app update HTTP contract: ${fixtures.cases.length} TypeScript fixture outcomes${withRust ? ' through the unbound Rust service' : ''}`
);
