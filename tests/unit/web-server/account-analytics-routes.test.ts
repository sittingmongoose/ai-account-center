import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { createAccountAnalyticsRouter } from '../../../src/web-server/routes/account-analytics-routes';
import type {
  AccountAnalytics,
  AccountAnalyticsQuery,
} from '../../../src/web-server/services/account-analytics-types';

let server: Server;
let baseUrl: string;
let calls: AccountAnalyticsQuery[];
let failing = false;
beforeEach(async () => {
  calls = [];
  failing = false;
  const app = express();
  app.use((req, _res, next) => {
    if (req.headers['x-test-session'] === 'true')
      Object.assign(req, { session: { authenticated: true } });
    next();
  });
  app.use(
    '/api/accounts',
    createAccountAnalyticsRouter({
      getAnalytics: async (query) => {
        calls.push(query);
        if (failing) throw new Error('raw secret sentinel');
        return { schemaVersion: 1, filters: { ...query } } as unknown as AccountAnalytics;
      },
    })
  );
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
function request(
  query = '',
  headers: Record<string, string> = { 'x-test-session': 'true' }
): Promise<Response> {
  return fetch(`${baseUrl}/api/accounts/analytics${query}`, { headers });
}

describe('account analytics route', () => {
  it('requires an authenticated session before any history read', async () => {
    expect((await request('', {})).status).toBe(401);
    expect((await request('?refresh=true', {})).status).toBe(401);
    expect(calls).toEqual([]);
  });
  it('defaults to Mac/all accounts/seven days and prevents caching', async () => {
    const response = await request();
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(calls).toEqual([{ platform: 'mac', range: '7d', provider: 'all', account: 'all' }]);
  });
  it('accepts exact bounded provider/account filters and same-origin requests', async () => {
    const response = await request(
      '?platform=windows&range=24h&provider=codex&account=codex%3Alexxmariah',
      { 'x-test-session': 'true', origin: baseUrl }
    );
    expect(response.status).toBe(200);
    expect(calls).toEqual([
      { platform: 'windows', range: '24h', provider: 'codex', account: 'codex:lexxmariah' },
    ]);
  });
  it.each(['true', 'false'])(
    'accepts the explicit authenticated refresh=%s flag',
    async (value) => {
      const response = await request(`?refresh=${value}`, {
        'x-test-session': 'true',
        origin: baseUrl,
      });
      expect(response.status).toBe(200);
      expect(calls).toEqual([
        {
          platform: 'mac',
          range: '7d',
          provider: 'all',
          account: 'all',
          refresh: value === 'true',
        },
      ]);
    }
  );
  it.each([
    '?platform=ubuntu',
    '?platform=mac&platform=windows',
    '?range=1y',
    '?range[days]=30',
    '?provider=cliproxy',
    '?provider=codex&provider=claude',
    '?account=../secret',
    '?account[host]=remote',
    '?refresh=',
    '?refresh=1',
    '?refresh=yes',
    '?refresh=TRUE',
    '?refresh=true&refresh=false',
    '?refresh[force]=true',
    '?host=remote',
    '?since=20260101',
    '?account=',
    '?account=' + 'a'.repeat(321),
  ])('rejects invalid or unknown query %s', async (query) => {
    expect((await request(query)).status).toBe(400);
    expect(calls).toEqual([]);
  });
  it('rejects authenticated cross-origin/browser cross-site reads', async () => {
    expect(
      (await request('', { 'x-test-session': 'true', origin: 'https://attacker.example' })).status
    ).toBe(403);
    expect(
      (await request('?refresh=true', { 'x-test-session': 'true', 'sec-fetch-site': 'cross-site' }))
        .status
    ).toBe(403);
    expect(calls).toEqual([]);
  });
  it('redacts collector errors and keeps the failure uncacheable', async () => {
    failing = true;
    const response = await request();
    expect(response.status).toBe(500);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(await response.text()).not.toContain('sentinel');
  });
});
