/** TS producers drive the opt-in Rust WebSocket through an inherited pipe. */
import assert from 'node:assert/strict';
import { once } from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import bcrypt from 'bcrypt';
import { WebSocket } from 'ws';
import {
  attachDashboardEventFeed,
  broadcastAppUpdateProgress,
  broadcastDashboardEvent,
  type DashboardEvent,
} from '../../src/web-server/dashboard-events';
import { fixtureEnvironment, startRustFixture } from './m6-rust-fixture';

const binary = path.resolve(process.argv[2] ?? '');
const username = 'fixture-admin';
const password = 'fixture-password';
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-events-'));
const fixtureClock = '2026-11-19T00:00:00.000Z';

async function login(base: string, host: string): Promise<string> {
  const response = await fetch(base + '/api/auth/login', {
    method: 'POST',
    headers: { host, 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
    signal: AbortSignal.timeout(5000),
  });
  assert.equal(response.status, 200);
  await response.arrayBuffer();
  return response.headers.get('set-cookie')!.split(';')[0];
}

async function connect(base: string, host: string, cookie?: string) {
  const client = new WebSocket(base.replace('http:', 'ws:') + '/ws', {
    headers: { host, origin: 'http://' + host, ...(cookie ? { cookie } : {}) },
    handshakeTimeout: 5000,
  });
  const events: DashboardEvent[] = [];
  client.on('message', (message) => events.push(JSON.parse(String(message))));
  try {
    await once(client, 'open');
  } catch (error) {
    client.terminate();
    throw error;
  }
  return { client, events };
}

async function until(condition: () => boolean, label: string) {
  const end = Date.now() + 5_000;
  while (!condition()) {
    assert(Date.now() < end, label);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

const job = {
  id: 'job_0123456789abcdef',
  provider: 'codex',
  kind: 'device-code',
  mode: 'add',
  accountId: null,
  profileName: 'test-profile',
  platform: 'ubuntu',
  state: 'waiting',
  verification: { url: 'https://example.test/verify', userCode: 'fixture-code', expiresAt: null },
  result: null,
  error: null,
  startedAt: fixtureClock,
  updatedAt: fixtureClock,
  expiresAt: fixtureClock,
} as const;

try {
  const yamlPath = path.join(root, 'config.yaml');
  await fs.writeFile(
    yamlPath,
    JSON.stringify({
      dashboard_auth: { enabled: true, username, password_hash: await bcrypt.hash(password, 4) },
    }),
    { mode: 0o600 }
  );
  const rust = await startRustFixture(binary, root, fixtureEnvironment(), true);
  assert(rust.pipe);
  const detach = attachDashboardEventFeed(rust.pipe);
  const sockets: WebSocket[] = [];
  try {
    const localHost = new URL(rust.url).host;
    const plainHost = 'dashboard.example.test';
    const secure = await connect(rust.url, localHost, await login(rust.url, localHost));
    sockets.push(secure.client);
    const plain = await connect(rust.url, plainHost, await login(rust.url, plainHost));
    sockets.push(plain.client);
    broadcastDashboardEvent({ type: 'accounts-changed' });
    broadcastDashboardEvent({ type: 'status-changed' });
    broadcastDashboardEvent({ type: 'analytics-changed' });
    broadcastDashboardEvent({ type: 'reset-claims-changed', id: 'a'.repeat(32) });
    broadcastDashboardEvent({ type: 'signin-job', job });
    const done = {
      ...job,
      state: 'succeeded',
      verification: null,
      result: { accountId: 'fixture', plan: 'pro', email: 'user@example.test' },
    } as const;
    broadcastDashboardEvent({ type: 'signin-job', job: done });
    // H/J run in the native owner now; the event contract is the same progress frame.
    const update = broadcastAppUpdateProgress({
      id: 'fixture-update',
      state: 'running',
      cancelRequested: false,
      expectedResults: 1,
      results: [],
      hosts: {
        ubuntu: { state: 'running', currentApp: 'codex-cli', phase: 'downloading' },
      },
    });
    await until(
      () =>
        [secure, plain].every(({ events }) =>
          events.some(
            (event) =>
              event.type === 'app-update-job' &&
              event.job.hosts.some((host) => host.phase === 'downloading')
          )
        ),
      'Rust app-progress frame'
    );
    for (const peer of [secure, plain]) {
      assert.deepEqual(
        peer.events.slice(0, 6).map((event) => event.type),
        [
          'accounts-changed',
          'status-changed',
          'analytics-changed',
          'reset-claims-changed',
          'signin-job',
          'signin-job',
        ]
      );
      const reset = peer.events[3];
      assert(reset.type === 'reset-claims-changed' && /^[0-9a-f]{32}$/.test(reset.id));
      const progress = peer.events[6];
      assert(progress.type === 'app-update-job');
      assert.equal(progress.job.id, 'fixture-update');
      assert.equal(progress.job.state, 'running');
      assert.deepEqual(Object.keys(progress.job), [
        'id',
        'state',
        'cancelRequested',
        'completedResults',
        'expectedResults',
        'hosts',
      ]);
    }
    assert.deepEqual(secure.events[4], { type: 'signin-job', job });
    assert.deepEqual(secure.events[5], { type: 'signin-job', job: done });
    assert.deepEqual(plain.events[4], { type: 'signin-job', job: { ...job, verification: null } });
    assert.deepEqual(plain.events[5], {
      type: 'signin-job',
      job: { ...done, result: { ...done.result, email: null } },
    });
    assert.deepEqual(secure.events[3], plain.events[3]);
    // H/J run in the native owner; the cancel and terminal progress frames are the contract.
    broadcastAppUpdateProgress({
      id: 'fixture-update',
      state: 'running',
      cancelRequested: true,
      expectedResults: 1,
      results: [],
      hosts: { ubuntu: { state: 'running', currentApp: 'codex-cli', phase: 'downloading' } },
    });
    broadcastAppUpdateProgress({
      id: 'fixture-update',
      state: 'cancelled',
      cancelRequested: true,
      expectedResults: 1,
      results: [{}],
      hosts: { ubuntu: { state: 'done' } },
    });
    await until(
      () =>
        secure.events.some(
          (event) => event.type === 'app-update-job' && event.job.state !== 'running'
        ),
      'Rust terminal update event'
    );
    assert(
      secure.events.some((event) => event.type === 'app-update-job' && event.job.cancelRequested)
    );
    // Even a signed-in account cannot publish to the process-private feed over HTTP.
    const noEndpoint = await fetch(rust.url + '/internal/dashboard-events', {
      method: 'POST',
      body: '{"type":"status-changed"}',
      headers: { 'content-type': 'application/json', cookie: await login(rust.url, localHost) },
      signal: AbortSignal.timeout(5000),
    });
    assert.equal(noEndpoint.status, 404);
    await noEndpoint.arrayBuffer();
  } finally {
    detach();
    for (const socket of sockets) socket.terminate();
    await rust.stop();
  }
} finally {
  await fs.rm(root, { recursive: true, force: true });
}
console.log(
  'M6 events: six TS event types reached Rust WebSockets; roles, redaction, progress, and pipe isolation passed.'
);
