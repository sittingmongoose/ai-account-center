import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { isDeviceScopeRoute } from '../../../src/web-server/middleware/api-request-guard';
import { createStoreRouter, createUsageRouter } from '../../../src/web-server/routes/store-routes';
import type { AacStore } from '../../../src/store/store-service';

describe('device scope for the history store (approved L6 change)', () => {
  it('admits exactly the read-only store routes', () => {
    for (const route of [
      '/store/status',
      '/usage/history',
      '/usage/at',
      '/usage/burn',
      '/cost/models',
      '/cost/tasks',
      '/usage/history/',
    ])
      expect(isDeviceScopeRoute('GET', route)).toBe(true);
    expect(isDeviceScopeRoute('HEAD', '/usage/history')).toBe(true);
  });
  it('refuses every write and every other route', () => {
    for (const method of ['POST', 'PUT', 'DELETE', 'PATCH'])
      for (const route of ['/store/status', '/usage/history', '/cost/models'])
        expect(isDeviceScopeRoute(method, route)).toBe(false);
    for (const route of [
      '/store/backup',
      '/store',
      '/usage',
      '/usage/',
      '/usage/history/extra',
      '/Usage/history',
      '/accounts/analytics',
      '/status/answers',
      '/auth/devices',
      '/usage/../accounts/analytics',
    ])
      expect(isDeviceScopeRoute('GET', route)).toBe(false);
  });
  it('keeps the tray routes', () => {
    expect(isDeviceScopeRoute('GET', '/accounts/dashboard')).toBe(true);
    expect(isDeviceScopeRoute('POST', '/codex/profiles/a/activate')).toBe(true);
  });
});

let server: Server;
let baseUrl: string;
let calls: Array<[string, unknown]>;
let available = true;

function fakeStore(): AacStore {
  const record = (name: string) => async (arg: unknown) => {
    calls.push([name, arg]);
    if (name === 'seriesPoints')
      return {
        resolution: 'hour',
        columns: [],
        truncated: false,
        series: [
          {
            identity: 'i'.repeat(64),
            accountId: 'codex:first',
            provider: 'codex',
            plan: null,
            window: {
              identity: '["five_hour",null,null]',
              key: 'five_hour',
              label: '5 hour',
              kind: null,
              unit: null,
              poolId: null,
              poolLabel: null,
            },
            points: [[0, 1, 2, 3, 60, 1, 0, 0, 0]],
          },
        ],
      };
    if (name === 'seriesList')
      return [
        {
          identity: 'i'.repeat(64),
          accountId: 'codex:first',
          provider: 'codex',
          plan: null,
          window: {
            identity: '["five_hour",null,null]',
            key: 'five_hour',
            label: '5 hour',
            kind: null,
            unit: null,
            poolId: null,
            poolLabel: null,
          },
        },
      ];
    if (name === 'burnLong')
      return [
        {
          identity: 'i',
          window: 'w',
          spans: [{ spanMs: 1, consumed: 2, rate: 0.5, coverage: 0.3 }],
        },
      ];
    if (name === 'periods') return [{ identity: 'i', window: 'w', periods: [] }];
    return { ok: name };
  };
  return {
    status: record('status'),
    seriesPoints: record('seriesPoints'),
    seriesList: record('seriesList'),
    usageAt: record('usageAt'),
    burnLong: (series: unknown, spans: unknown) => record('burnLong')({ series, spans }),
    periods: (series: unknown) => record('periods')({ series }),
  } as unknown as AacStore;
}

beforeEach(async () => {
  calls = [];
  available = true;
  const app = express();
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    if (req.headers['x-test-device'] === 'true') Object.assign(req, { auth: { kind: 'device' } });
    next();
  });
  const deps = {
    store: () => (available ? fakeStore() : null),
    now: () => Date.parse('2026-11-06T12:00:00Z'),
  };
  app.use('/api/store', createStoreRouter(deps));
  app.use('/api/usage', createUsageRouter(deps));
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test listener');
  baseUrl = `http://127.0.0.1:${address.port}`;
});
afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

const device = { 'x-test-device': 'true' };

describe('history store routes', () => {
  it('need a session or a device token', async () => {
    for (const route of [
      '/api/store/status',
      '/api/usage/history',
      '/api/usage/at',
      '/api/usage/burn',
    ]) {
      const response = await fetch(`${baseUrl}${route}`);
      expect(response.status).toBe(401);
      expect(response.headers.get('cache-control')).toBe('no-store');
    }
    expect(calls).toEqual([]);
  });

  it('a session from another origin is refused, a device token is not origin-bound', async () => {
    const cross = await fetch(`${baseUrl}/api/store/status`, {
      headers: { 'x-test-session': 'true', origin: 'https://elsewhere.example' },
    });
    expect(cross.status).toBe(403);
    const ok = await fetch(`${baseUrl}/api/store/status`, { headers: device });
    expect(ok.status).toBe(200);
  });

  it('validates queries with fixed errors that never echo input', async () => {
    for (const query of [
      '?resolution=minute',
      '?from=yesterday',
      '?account=%3Cscript%3E',
      '?bogus=1',
      '?from=2026-11-06T00:00:00Z&to=2026-11-01T00:00:00Z',
    ]) {
      const response = await fetch(`${baseUrl}/api/usage/history${query}`, { headers: device });
      expect(response.status).toBe(400);
      const body = await response.json();
      expect(body.code).toBe('invalid_query');
      expect(JSON.stringify(body)).not.toContain('script');
    }
    const span = await fetch(`${baseUrl}/api/usage/burn?span=2y`, { headers: device });
    expect(span.status).toBe(400);
  });

  it('passes the filters to the store, and the burn route applies the coverage rule', async () => {
    const history = await fetch(
      `${baseUrl}/api/usage/history?account=codex:first&window=five_hour&resolution=raw&from=2026-11-01T00:00:00Z`,
      { headers: device }
    );
    expect(history.status).toBe(200);
    expect(calls[0]).toEqual([
      'seriesPoints',
      {
        fromMs: Date.parse('2026-11-01T00:00:00Z'),
        toMs: Date.parse('2026-11-06T12:00:00Z'),
        resolution: 'raw',
        accountIds: ['codex:first'],
        windowKey: 'five_hour',
      },
    ]);
    const burn = await (
      await fetch(`${baseUrl}/api/usage/burn?span=7d`, { headers: device })
    ).json();
    // Coverage 0.3 is under half the span: the rate is withheld, the coverage shown.
    expect(burn.series[0].spans[0]).toEqual({ span: '7d', consumed: 2, rate: null, coverage: 0.3 });
    // Series come from metadata, never from a capped point query.
    const burnCalls = calls.slice(1).map(([name]) => name);
    expect(burnCalls).toContain('seriesList');
    expect(burnCalls).not.toContain('seriesPoints');
  });

  it('says plainly when the store is not available', async () => {
    available = false;
    const history = await fetch(`${baseUrl}/api/usage/history`, { headers: device });
    expect(history.status).toBe(503);
    expect((await history.json()).code).toBe('store_unavailable');
    const status = await (await fetch(`${baseUrl}/api/store/status`, { headers: device })).json();
    expect(status.state).toBe('unavailable');
  });
});
