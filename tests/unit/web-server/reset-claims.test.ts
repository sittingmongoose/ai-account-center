import { afterEach, expect, test } from 'bun:test';
import express from 'express';
import { scrubLoggedUrl } from '../../../src/web-server/middleware/request-logging-middleware';
import { mkdtempSync, rmSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import type { Server } from 'http';
import { WebSocket, WebSocketServer } from 'ws';
import { attachDashboardEventServer } from '../../../src/web-server/dashboard-events';
import {
  createResetClaimRouter,
  type ResetSource,
} from '../../../src/web-server/routes/reset-claim-routes';
const servers: Server[] = [];
const dirs: string[] = [];
const socketClosers: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const close of socketClosers.splice(0)) await close();
  for (const server of servers.splice(0)) await new Promise<void>((r) => server.close(() => r()));
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});
async function serve(
  auth: 'session' | 'device' | 'none',
  source?: ResetSource,
  rejectGate = false,
  reject?: { op: string; message: string },
  pendingAttempt: string | null = null
) {
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (auth === 'session') {
      (req as any).session = { authenticated: true };
      (req as any).sessionID = 'synthetic-session';
    } else if (auth === 'device')
      (req as any).auth = { kind: 'device', deviceId: 'synthetic-device' };
    next();
  });
  const calls: string[] = [];
  const snapshots: Array<Record<string, unknown>> = [];
  const dir = mkdtempSync(join(tmpdir(), 'aac-reset-http-'));
  dirs.push(dir);
  app.use(
    '/accounts',
    createResetClaimRouter({
      ccsDir: dir,
      source,
      native: {
        async call(_method, raw) {
          const a = JSON.parse(raw);
          calls.push(a.op);
          if (a.snapshot) snapshots.push(a.snapshot);
          if (rejectGate && a.op.endsWith('Gate')) throw new Error('synthetic cooldown');
          if (reject && a.op === reject.op) throw new Error(reject.message);
          if (a.op === 'history') return '[]';
          if (a.op === 'pending' || a.op === 'inventory')
            return JSON.stringify({
              ...(a.op === 'inventory'
                ? { status: 'ok', grants: [] }
                : { attemptKey: pendingAttempt }),
              pendingAttempt,
              history: [],
              canonicalAccountKey: 'synthetic-canonical',
              bindingRevision: '17',
              evidence: { canonicalAccountKey: 'synthetic-canonical', revision: '19' },
            });
          if (a.op === 'begin')
            return JSON.stringify({
              attemptKey: 'a'.repeat(64),
              identity: {},
              body: {},
              method: 'fake',
              routeAccount: 'synthetic',
              sourceBinding: 'fake',
            });
          return JSON.stringify({ outcome: 'claimed-unconfirmed' });
        },
      },
    })
  );
  const server = app.listen(0, '127.0.0.1');
  servers.push(server);
  await new Promise<void>((r) => server.on('listening', r));
  const url = `http://127.0.0.1:${(server.address() as any).port}`;
  return { url, calls, server, snapshots };
}
const grantKey = 'b'.repeat(64);
test('paired native devices and anonymous callers cannot prepare or read private history', async () => {
  for (const auth of ['device', 'none'] as const) {
    const { url, calls } = await serve(auth);
    const res = await fetch(`${url}/accounts/synthetic/reset-claims/prepare`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: url },
      body: JSON.stringify({ grantKey }),
    });
    expect(res.status).toBe(403);
    expect(calls).toEqual([]);
  }
});
test('missing executor remains honest unavailable', async () => {
  const { url } = await serve('session');
  const res = await fetch(`${url}/accounts/synthetic/reset-claims`);
  expect((await res.json()).status).toBe('unavailable');
});
test('origin, extra provider fields and queries are rejected before any native action', async () => {
  let reads = 0;
  const source: ResetSource = {
    async read() {
      reads++;
      return {};
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const { url, calls } = await serve('session', source);
  for (const [suffix, body, origin] of [
    ['', { grantKey }, 'https://wrong.invalid'],
    ['', { grantKey, providerGrantId: 'synthetic' }, url],
    ['?unexpected=1', { grantKey }, url],
  ] as const) {
    const res = await fetch(`${url}/accounts/synthetic/reset-claims/prepare${suffix}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: origin },
      body: JSON.stringify(body),
    });
    expect(res.status).toBeGreaterThanOrEqual(400);
  }
  expect(reads).toBe(0);
  expect(calls).toEqual([]);
});
test('check-again reads only and cannot invoke provider send', async () => {
  let sends = 0;
  const source: ResetSource = {
    async read() {
      return {};
    },
    async send() {
      sends++;
      return { outcome: 'reset' };
    },
  };
  const { url, calls } = await serve('session', source);
  const res = await fetch(`${url}/accounts/synthetic/reset-claims/check`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: url },
    body: JSON.stringify({ attemptKey: grantKey }),
  });
  expect(res.status).toBe(200);
  expect(calls).toEqual(['check']);
  expect(sends).toBe(0);
});
test('ambiguous provider send makes one attempt and performs a fresh verification read', async () => {
  let sends = 0,
    reads = 0;
  const source: ResetSource = {
    async read() {
      reads++;
      return {};
    },
    async send() {
      sends++;
      throw new Error('synthetic timeout');
    },
  };
  const { url, calls } = await serve('session', source);
  const res = await fetch(`${url}/accounts/synthetic/reset-claims`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: url },
    body: JSON.stringify({ grantKey, confirmationToken: 'c'.repeat(64) }),
  });
  expect(res.status).toBe(200);
  expect(calls).toEqual(['claimGate', 'begin', 'finish']);
  expect(sends).toBe(1);
  expect(reads).toBe(2);
});

test('reset request logging drops account aliases and all query payloads', () => {
  for (const url of [
    '/api/accounts/private-alias/reset-claims?confirmationToken=private-value',
    '/api/accounts/private-alias/reset-claims/prepare',
    '/api/accounts/private%2falias/reset-claims?unexpected=private-value',
    '/%61pi/accounts/private-alias/reset-claims/check?unexpected=private-value',
  ]) {
    expect(scrubLoggedUrl(url)).toBe('/api/accounts/[account]/reset-claims');
  }
});

test('a persisted native hold refuses the request before any provider read', async () => {
  let reads = 0,
    sends = 0;
  const source: ResetSource = {
    async read() {
      reads++;
      return {};
    },
    async send() {
      sends++;
      return { outcome: 'reset' };
    },
  };
  const { url, calls } = await serve('session', source, true);
  const res = await fetch(`${url}/accounts/synthetic/reset-claims/prepare`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: url },
    body: JSON.stringify({ grantKey }),
  });
  expect(res.status).toBe(409);
  expect(reads).toBe(0);
  expect(sends).toBe(0);
  expect(calls).toEqual(['prepareGate', 'pending']);
});

test('the inventory read names an unresolved attempt so any signed-in session can check it again', async () => {
  const source: ResetSource = {
    async read() {
      throw new Error('synthetic source failure with private words');
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const { url, calls } = await serve('session', source, false, undefined, 'd'.repeat(64));
  const res = await fetch(`${url}/accounts/synthetic/reset-claims`);
  expect(res.status).toBe(200);
  const body = await res.json();
  expect(body).toEqual({
    status: 'unavailable',
    reason: 'reset-inventory-unavailable',
    grants: [],
    history: [],
    attemptKey: 'd'.repeat(64),
    pendingAttempt: 'd'.repeat(64),
    canonicalAccountKey: 'synthetic-canonical',
    bindingRevision: '17',
    evidence: { canonicalAccountKey: 'synthetic-canonical', revision: '19' },
  });
  expect(JSON.stringify(body)).not.toContain('private');
  expect(calls).toEqual(['pending']);
});

test('a refusal passes on only an allowlisted native code, and says whether the write was reached', async () => {
  let sends = 0;
  const source: ResetSource = {
    async read() {
      return {};
    },
    async send() {
      sends++;
      return { outcome: 'reset' };
    },
  };
  const post = (url: string) =>
    fetch(`${url}/accounts/synthetic/reset-claims`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: url },
      body: JSON.stringify({ grantKey, confirmationToken: 'c'.repeat(64) }),
    });
  for (const [reject, reason, sent] of [
    [
      { op: 'begin', message: 'confirmation-invalid: Reset operation refused.' },
      'confirmation-invalid',
      false,
    ],
    [
      { op: 'claimGate', message: 'check-again-required: Reset operation refused.' },
      'check-again-required',
      false,
    ],
    [{ op: 'begin', message: 'private-account-name: leaked words' }, null, false],
    [{ op: 'finish', message: 'storage-unavailable: Reset operation refused.' }, null, true],
  ] as const) {
    const { url } = await serve('session', source, false, reject);
    const res = await post(url);
    expect(res.status).toBe(409);
    const body = await res.json();
    expect(body).toEqual({
      code: 'reset-state-unavailable',
      action: 'check-again',
      reason,
      sent,
      evidence: { canonicalAccountKey: 'synthetic-canonical', revision: '19' },
    });
    expect(JSON.stringify(body)).not.toContain('private');
  }
  expect(sends).toBe(1);
});

test('inventory recovery and revision are returned from one native transaction', async () => {
  const source: ResetSource = {
    async read() {
      return {};
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const { url, calls } = await serve('session', source, false, undefined, 'e'.repeat(64));
  const res = await fetch(`${url}/accounts/alias/reset-claims`);
  expect(res.status).toBe(200);
  const body = await res.json();
  expect(body.pendingAttempt).toBe('e'.repeat(64));
  expect(body.evidence.revision).toBe('19');
  expect(body.bindingRevision).toBe('17');
  expect(calls).toEqual(['inventory']);
});

test('a refusal without recovery storage returns null evidence and never invents a snapshot', async () => {
  let reads = 0;
  const source: ResetSource = {
    async read() {
      reads++;
      throw new Error('must not read');
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const { url, calls } = await serve('session', source, true, {
    op: 'pending',
    message: 'private storage failure',
  });
  const res = await fetch(`${url}/accounts/synthetic/reset-claims/prepare`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: url },
    body: JSON.stringify({ grantKey }),
  });
  expect(res.status).toBe(409);
  const body = await res.json();
  expect(body.evidence).toBeNull();
  expect(body.sent).toBe(false);
  expect(JSON.stringify(body)).not.toContain('private');
  expect(reads).toBe(0);
  expect(calls).toEqual(['prepareGate', 'pending']);
});

test('an outstanding-token refusal is honest and happens before a provider read', async () => {
  let reads = 0;
  const source: ResetSource = {
    async read() {
      reads++;
      return {};
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const { url } = await serve('session', source, false, {
    op: 'prepareGate',
    message: 'claim-token-outstanding: private detail',
  });
  const response = await fetch(`${url}/accounts/synthetic/reset-claims/prepare`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Origin: url },
    body: JSON.stringify({ grantKey }),
  });
  const body = await response.json();
  expect(response.status).toBe(409);
  expect(body.reason).toBe('claim-token-outstanding');
  expect(body.sent).toBe(false);
  expect(reads).toBe(0);
  expect(JSON.stringify(body)).not.toContain('private detail');
});

test('prepare, begin before provider completion, and settlement push to all browser sessions only', async () => {
  let release!: () => void;
  const sendDone = new Promise<void>((resolve) => {
    release = resolve;
  });
  const source: ResetSource = {
    async read() {
      return {};
    },
    async send() {
      await sendDone;
      return { outcome: 'nothingToReset' };
    },
  };
  const { url, server } = await serve('session', source);
  const wss = new WebSocketServer({ server, path: '/ws' });
  const detach = attachDashboardEventServer(wss, {
    authKind: (request) =>
      request.headers['x-test-auth'] === 'session'
        ? 'session'
        : request.headers['x-test-auth'] === 'device'
          ? 'device'
          : null,
  });
  const sockets: WebSocket[] = [];
  socketClosers.push(async () => {
    detach();
    for (const socket of wss.clients) socket.terminate();
    for (const socket of sockets) socket.terminate();
    await new Promise<void>((resolve) => wss.close(() => resolve()));
  });
  const received: string[][] = [];
  for (const kind of ['session', 'session', 'device', 'none']) {
    const socket = new WebSocket(url.replace('http:', 'ws:') + '/ws', {
      headers: { 'x-test-auth': kind },
    });
    const messages: string[] = [];
    received.push(messages);
    sockets.push(socket);
    socket.on('message', (data) => messages.push(String(data)));
    await new Promise<void>((resolve, reject) => {
      socket.once('open', resolve);
      socket.once('error', reject);
    });
  }
  const next = () =>
    Promise.all(
      sockets
        .slice(0, 2)
        .map((socket) => new Promise<void>((resolve) => socket.once('message', () => resolve())))
    );
  const post = (suffix: string, body: unknown) =>
    fetch(`${url}/accounts/synthetic/reset-claims${suffix}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Origin: url },
      body: JSON.stringify(body),
    });
  const prepared = next();
  expect((await post('/prepare', { grantKey })).status).toBe(200);
  await prepared;
  const began = next();
  const claim = post('', { grantKey, confirmationToken: 'c'.repeat(64) });
  await began;
  expect(received[0]).toHaveLength(2); // Begin is announced while the provider is still waiting.
  const settled = next();
  release();
  expect((await claim).status).toBe(200);
  await settled;
  expect(received[0]).toHaveLength(3);
  expect(received[1]).toHaveLength(3);
  expect(received[2]).toEqual([]);
  expect(received[3]).toEqual([]);
  expect(received[0]).toEqual(received[1]);
  expect(new Set(received[0].map((message) => JSON.parse(message).id)).size).toBe(3);
  for (const message of received[0]) {
    const event = JSON.parse(message);
    expect(event.type).toBe('reset-claims-changed');
    expect(event.id).toMatch(/^[a-f0-9]{32}$/);
    expect(Object.keys(event).sort()).toEqual(['id', 'type']);
  }
});

test('router preserves source-owned capture tickets instead of minting request-admission order', async () => {
  let calls: string[] = [];
  const ticket = { captureVersion: 1, routeKey: 'f'.repeat(64), sequence: '9' };
  const source: ResetSource = {
    async read() {
      expect(calls).toEqual([]);
      return { readTicket: ticket };
    },
    async send() {
      throw new Error('must not send');
    },
  };
  const served = await serve('session', source);
  calls = served.calls;
  const response = await fetch(`${served.url}/accounts/synthetic/reset-claims`);
  expect(response.status).toBe(200);
  expect(served.snapshots[0].readTicket).toEqual(ticket);
  expect(calls).toEqual(['inventory']);
});

test('native source capture failure performs no inventory mutation or provider send', async () => {
  let sends = 0;
  const source: ResetSource = {
    async read() {
      throw new Error('credential-host-unavailable: capture failed');
    },
    async send() {
      sends++;
      return { outcome: 'reset' };
    },
  };
  const served = await serve('session', source);
  const response = await fetch(`${served.url}/accounts/synthetic/reset-claims`);
  expect(response.status).toBe(200);
  expect((await response.json()).status).toBe('unavailable');
  expect(sends).toBe(0);
  expect(served.calls).toEqual(['pending']);
});
