import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { createAppUpdateRouter } from '../../../src/web-server/routes/app-update-routes';
import { AppUpdateService } from '../../../src/web-server/services/app-update-service';
let server: Server;
let base: string;
let calls: number;
beforeEach(async () => {
  calls = 0;
  const app = express();
  app.use(express.json());
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    next();
  });
  const service = new AppUpdateService({
    persist: false,
    runHost: () => {
      calls++;
      return new Promise(() => {});
    },
  });
  app.use('/api/app-updates', createAppUpdateRouter(service));
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  base = `http://127.0.0.1:${address.port}`;
});
afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});
function start(body = '{}', origin = base, authenticated = true, query = '') {
  return fetch(`${base}/api/app-updates/start${query}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      origin,
      ...(authenticated ? { 'x-test-session': 'true' } : {}),
    },
    body,
  });
}
function cancel(body = '{}', origin = base, authenticated = true, query = '') {
  return fetch(`${base}/api/app-updates/cancel${query}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      origin,
      ...(authenticated ? { 'x-test-session': 'true' } : {}),
    },
    body,
  });
}
describe('authenticated app updater action', () => {
  it('reads status without triggering work', async () => {
    const response = await fetch(`${base}/api/app-updates/status`, {
      headers: { 'x-test-session': 'true' },
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(await response.json()).toEqual({ job: null });
    expect(calls).toBe(0);
  });
  it('rejects unauthenticated and cross-origin actions', async () => {
    expect((await start('{}', base, false)).status).toBe(401);
    expect((await start('{}', 'https://external.example')).status).toBe(403);
    expect(calls).toBe(0);
  });
  it('requires an explicit browser Origin before updating', async () => {
    const response = await fetch(`${base}/api/app-updates/start`, {
      method: 'POST',
      headers: { 'x-test-session': 'true', 'content-type': 'application/json' },
      body: '{}',
    });
    expect(response.status).toBe(403);
    expect(calls).toBe(0);
  });
  it.each(['[]', '{"host":"untrusted"}', '{"command":"killall node"}', 'null'])(
    'rejects command/config selectors %s',
    async (body) => {
      expect((await start(body)).status).toBe(400);
      expect(calls).toBe(0);
    }
  );
  it('rejects query selectors and non-JSON content', async () => {
    expect((await start('{}', base, true, '?host=external')).status).toBe(400);
    const response = await fetch(`${base}/api/app-updates/start`, {
      method: 'POST',
      headers: { 'x-test-session': 'true', origin: base, 'content-type': 'text/plain' },
      body: '{}',
    });
    expect(response.status).toBe(415);
    expect(calls).toBe(0);
  });
  it('returns 202 immediately and rejects an active duplicate', async () => {
    const first = await start();
    expect(first.status).toBe(202);
    const job = (await first.json()).job;
    expect(job.state).toBe('running');
    expect(calls).toBe(1);
    const second = await start();
    expect(second.status).toBe(409);
    expect((await second.json()).job.id).toBe(job.id);
    expect(calls).toBe(1);
  });
  it('cancel rejects unauthenticated and cross-origin actions', async () => {
    expect((await cancel('{}', base, false)).status).toBe(401);
    expect((await cancel('{}', 'https://external.example')).status).toBe(403);
  });
  it('cancel requires an explicit browser Origin before acting', async () => {
    const response = await fetch(`${base}/api/app-updates/cancel`, {
      method: 'POST',
      headers: { 'x-test-session': 'true', 'content-type': 'application/json' },
      body: '{}',
    });
    expect(response.status).toBe(403);
  });
  it.each(['[]', '{"host":"untrusted"}', '{"command":"killall node"}', 'null'])(
    'cancel rejects command/config selectors %s',
    async (body) => {
      expect((await cancel(body)).status).toBe(400);
    }
  );
  it('cancel rejects query selectors and non-JSON content', async () => {
    expect((await cancel('{}', base, true, '?host=external')).status).toBe(400);
    const response = await fetch(`${base}/api/app-updates/cancel`, {
      method: 'POST',
      headers: { 'x-test-session': 'true', origin: base, 'content-type': 'text/plain' },
      body: '{}',
    });
    expect(response.status).toBe(415);
  });
  it('cancel acknowledges a running job at once and is idempotent', async () => {
    const started = await start();
    expect(started.status).toBe(202);
    const id = (await started.json()).job.id;
    const first = await cancel();
    expect(first.status).toBe(202);
    const outcome = await first.json();
    expect(outcome.cancelling).toBe(true);
    expect(outcome.job.id).toBe(id);
    expect(outcome.job.state).toBe('running');
    expect(outcome.job.cancelRequested).toBe(true);
    const second = await cancel();
    expect(second.status).toBe(202);
    expect(await second.json()).toEqual(outcome);
    expect(calls).toBe(1);
  });
  it('cancel with no running job is a no-op', async () => {
    const response = await cancel();
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ job: null, cancelling: false });
  });
});
