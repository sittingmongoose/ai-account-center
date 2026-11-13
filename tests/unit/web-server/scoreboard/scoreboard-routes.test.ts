import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import type { AddressInfo } from 'net';
import path from 'path';
import { createScoreboardRouter } from '../../../../src/web-server/routes/scoreboard-routes';
import { ScoreboardService } from '../../../../src/web-server/scoreboard/scoreboard-service';
import { NOW, tempDirs } from '../status/status-test-helpers';

const dirs = tempDirs();
let server: Server | undefined;
// The 401 path exists only with dashboard sign-in on; the sign-in-off answer has its own test.
const authEnv = process.env.CCS_DASHBOARD_AUTH_ENABLED;
beforeEach(() => {
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
});
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server?.close(() => resolve()));
  server = undefined;
  dirs.cleanup();
  if (authEnv === undefined) delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  else process.env.CCS_DASHBOARD_AUTH_ENABLED = authEnv;
});

async function start(service: ScoreboardService) {
  const app = express();
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    if (req.headers['x-test-device'] === 'true') Object.assign(req, { auth: { kind: 'device' } });
    next();
  });
  app.use(
    '/api/scoreboard',
    createScoreboardRouter(() => service)
  );
  server = await new Promise<Server>((resolve) => {
    const listening = app.listen(0, '127.0.0.1', () => resolve(listening));
  });
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}/api/scoreboard`;
}

function service() {
  return new ScoreboardService({
    root: () => path.join(dirs.make(), 'none'),
    now: () => NOW,
    timeZone: () => 'UTC',
  });
}

const session = { 'x-test-session': 'true' };

describe('scoreboard route', () => {
  it('serves the view to a browser session only, never cached', async () => {
    const url = await start(service());
    const anonymous = await fetch(url);
    expect(anonymous.status).toBe(401);
    expect(anonymous.headers.get('cache-control')).toBe('no-store');
    expect(await anonymous.json()).toEqual({
      error: 'Authentication required',
      code: 'auth_required',
    });
    const device = await fetch(url, { headers: { 'x-test-device': 'true', ...session } });
    expect(device.status).toBe(403);
    expect((await device.json()).code).toBe('device_scope');
    const ok = await fetch(url, { headers: session });
    expect(ok.status).toBe(200);
    expect(ok.headers.get('cache-control')).toBe('private, no-cache');
    const etag = ok.headers.get('etag');
    expect(etag).toBeTruthy();
    const unchanged = await fetch(url, { headers: { ...session, 'If-None-Match': etag! } });
    expect(unchanged.status).toBe(304);
    expect(await unchanged.text()).toBe('');
    const view = await ok.json();
    expect(view.version).toBe(1);
    expect(view.refresh).toEqual({ pollSeconds: 60 });
    expect(view.sources.map((source: { id: string }) => source.id)).toEqual([
      'scoreboard-runs',
      'scoreboard-catalog',
    ]);
  });

  it('with dashboard sign-in off, says these tabs need it, not a bare 401', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    const url = await start(service());
    const anonymous = await fetch(url);
    expect(anonymous.status).toBe(403);
    expect(await anonymous.json()).toEqual({
      error: 'These tabs need dashboard sign-in turned on.',
      code: 'signin_required',
    });
    // A kept session still works.
    expect((await fetch(url, { headers: session })).status).toBe(200);
  });

  it('takes no query string', async () => {
    const url = await start(service());
    const response = await fetch(`${url}?x=1`, { headers: session });
    expect(response.status).toBe(400);
    expect((await response.json()).code).toBe('unexpected_query');
  });

  it('takes only the four filters, each once, with fixed refusals', async () => {
    const seen: unknown[] = [];
    const recording = service();
    recording.view = async (filters) => {
      seen.push(filters);
      return { version: 1 } as never;
    };
    const url = await start(recording);
    const ok = await fetch(`${url}?from=2026-11-01&to=2026-11-13&source=real&taskType=build`, {
      headers: session,
    });
    expect(ok.status).toBe(200);
    expect(seen).toEqual([
      { from: '2026-11-01', to: '2026-11-13', source: 'real', taskType: 'build' },
    ]);
    await fetch(url, { headers: session });
    expect(seen[1]).toEqual({ from: null, to: null, source: 'all', taskType: null });
    for (const [query, code, error] of [
      ['?from=2026-13-01', 'invalid_filter', 'Dates are days written YYYY-MM-DD.'],
      ['?from=2026-11-13&to=2026-11-01', 'invalid_filter', 'The first day is after the last day.'],
      ['?source=everything', 'invalid_filter', 'Source is all, real or benchmark.'],
      ['?taskType=../x', 'invalid_filter', 'The task type is not a task type id.'],
      ['?source=real&source=all', 'invalid_filter', 'Each filter may be given once.'],
      [
        '?family=openai',
        'unexpected_query',
        'This request takes only from, to, source and taskType.',
      ],
    ]) {
      const refused = await fetch(`${url}${query}`, { headers: session });
      expect(refused.status).toBe(400);
      expect(await refused.json()).toEqual({ error, code });
    }
    expect(seen).toHaveLength(2);
  });

  it('answers a failure with fixed words', async () => {
    const broken = service();
    broken.view = async () => {
      throw new Error('private detail');
    };
    const url = await start(broken);
    const response = await fetch(url, { headers: session });
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({
      error: 'The scoreboard could not be built.',
      code: 'scoreboard_failed',
    });
  });
});
