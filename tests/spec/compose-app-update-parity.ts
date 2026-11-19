/** The bound H/J job and its WebSocket progress through the public Rust listener. */
import assert from 'node:assert/strict';
import { once } from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { setTimeout } from 'node:timers/promises';
import WebSocket from 'ws';
import type { DashboardEvent } from '../../src/web-server/dashboard-events';

const binary = process.env.AAC_RUST_PUBLIC_BINARY;
assert.ok(binary, 'AAC_RUST_PUBLIC_BINARY is required');
const home = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-compose-app-update-'));
const root = path.join(home, '.ccs');
for (const key of Object.keys(process.env)) {
  if (key.startsWith('AAC_') || key.startsWith('CCS_')) delete process.env[key];
}
for (const key of [
  'HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME',
  'XDG_DATA_HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'CCS_HOME',
]) process.env[key] = home;
Object.assign(process.env, {
  CCS_DIR: root,
  CCS_DASHBOARD_AUTH_ENABLED: 'true',
  AAC_RUST_COMPOSE: '1',
  AAC_RUST_SERVICE: 'on',
  AAC_RUST_GROUP_APP_UPDATES: 'on',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_AGENT_VERBS: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_STORE: 'off',
  AAC_STATUS_DOORBELL: 'off',
  AAC_AGENT_REPORTS_DIR: path.join(home, 'reports'),
  AAC_PM_MAIL_REGISTRY_DIR: path.join(home, 'registry'),
});

const { startComposedServer } = await import('../../src/web-server/compose-server');
let server: Awaited<ReturnType<typeof startComposedServer>> | undefined;

async function waitFor(ready: () => boolean): Promise<void> {
  const stop = AbortSignal.timeout(6000);
  while (!ready()) await setTimeout(25, undefined, { signal: stop });
}

try {
  server = await startComposedServer({ host: '127.0.0.1', port: 0 }, path.resolve(binary));
  const { rustService, publicAddress } = server;
  await waitFor(() => rustService.diagnostics().service.state === 'ready');
  const origin = `http://127.0.0.1:${publicAddress.port}`;
  const json = { origin, 'content-type': 'application/json' };
  const setupCode = (await fs.readFile(path.join(root, 'auth', 'setup-code'), 'utf8')).trim();
  const setup = await fetch(origin + '/api/auth/setup', {
    method: 'POST',
    headers: json,
    body: JSON.stringify({ setupCode, username: 'fixture', password: 'fixture-password-only' }),
  });
  assert.equal(setup.status, 201);
  const cookie = setup.headers.get('set-cookie')?.split(';')[0];
  assert.ok(cookie);

  const before = rustService.stats.served;
  const status = await fetch(origin + '/api/app-updates/status', { headers: { cookie } });
  assert.equal(status.status, 200);
  assert.deepEqual(await status.json(), { job: null });
  const cancel = await fetch(origin + '/api/app-updates/cancel', {
    method: 'POST', headers: { ...json, cookie }, body: '{}',
  });
  assert.equal(cancel.status, 200);
  assert.deepEqual(await cancel.json(), { job: null, cancelling: false });
  assert.equal(rustService.stats.served - before, 2, 'both routes used lane H');

  const events: Extract<DashboardEvent, { type: 'app-update-job' }>[] = [];
  const socket = new WebSocket(`ws://127.0.0.1:${publicAddress.port}/ws`, {
    headers: { cookie, origin },
  });
  try {
    socket.on('message', (bytes) => {
      const event = JSON.parse(bytes.toString()) as DashboardEvent;
      if (event.type === 'app-update-job') events.push(event);
    });
    await once(socket, 'open', { signal: AbortSignal.timeout(4000) });
    const start = await fetch(origin + '/api/app-updates/start', {
      method: 'POST', headers: { ...json, cookie }, body: '{}',
    });
    assert.equal(start.status, 202);
    const { job } = (await start.json()) as { job: { id: string } };
    const jobEvents = () => events.filter((event) => event.job.id === job.id);
    await waitFor(() => jobEvents().some((event) => event.job.state !== 'running'));
    assert.ok(jobEvents().some((event) => event.job.state === 'running'));
    assert.ok(jobEvents().every((event) => !('results' in event.job)));
  } finally {
    socket.terminate();
  }
  console.log('Composed app updates: native status, idle cancellation, start and WebSocket progress passed.');
} finally {
  if (server) {
    const companion = server;
    companion.cleanup();
    companion.wss.close();
    companion.server.closeAllConnections();
    await new Promise<void>((resolve) => companion.server.close(() => resolve()));
  }
  await fs.rm(home, { recursive: true, force: true });
}
