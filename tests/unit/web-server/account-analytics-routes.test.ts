import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { createAccountAnalyticsRouter } from '../../../src/web-server/routes/account-analytics-routes';
import { AccountAnalyticsQueryError } from '../../../src/web-server/services/account-analytics-range';
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

describe('account analytics route: contract additions', () => {
  async function withRouter(
    deps: Parameters<typeof createAccountAnalyticsRouter>[0],
    run: (get: (query: string) => Promise<Response>) => Promise<void>
  ): Promise<void> {
    const app = express();
    app.use((req, _res, next) => {
      Object.assign(req, { session: { authenticated: true } });
      next();
    });
    app.use('/api/accounts', createAccountAnalyticsRouter(deps));
    const instance = await new Promise<Server>((resolve) => {
      const listener = app.listen(0, '127.0.0.1', () => resolve(listener));
    });
    try {
      const address = instance.address();
      if (!address || typeof address === 'string') throw new Error('No test listener');
      await run((query) =>
        fetch(`http://127.0.0.1:${address.port}/api/accounts/analytics${query}`)
      );
    } finally {
      await new Promise<void>((resolve) => instance.close(() => resolve()));
    }
  }

  it('passes month, all and custom ranges with a zone through to the service', async () => {
    for (const [query, expected] of [
      ['?range=month&tz=America%2FNew_York', { range: 'month', tz: 'America/New_York' }],
      ['?range=all', { range: 'all' }],
      [
        '?range=custom&from=2026-09-20&to=2026-09-26&tz=Asia%2FKolkata',
        { range: 'custom', from: '2026-09-20', to: '2026-09-26', tz: 'Asia/Kolkata' },
      ],
    ] as const) {
      calls = [];
      expect((await request(query)).status).toBe(200);
      expect(calls).toEqual([{ platform: 'mac', provider: 'all', account: 'all', ...expected }]);
    }
  });

  it.each([
    ['?tz=Mars%2FBase', 'invalid_tz'],
    ['?tz=%2B05%3A00', 'invalid_tz'],
    ['?range=7d&from=2026-09-20', 'invalid_range'],
    ['?range=24h&to=2026-09-20', 'invalid_range'],
    ['?range=custom&from=2026-09-20', 'invalid_range'],
    ['?range=custom&from=2026-09-26&to=2026-09-20', 'invalid_range'],
    ['?range=custom&from=2026-08-01&to=2026-09-20', 'invalid_range'],
    ['?range=custom&from=2026-02-30&to=2026-03-01', 'invalid_range'],
    ['?range=year', 'invalid_range'],
    ['?provider=cliproxy', 'invalid_provider'],
    ['?from[x]=1', 'invalid_query'],
  ])('rejects %s with 400 %s before reading analytics', async (query, code) => {
    const response = await request(query);
    expect(response.status).toBe(400);
    expect(await response.json()).toMatchObject({ code });
    expect(calls).toEqual([]);
  });

  it('validates providers against the server table, not a fixed list', async () => {
    const seen: AccountAnalyticsQuery[] = [];
    await withRouter(
      {
        providerIds: () => ['claude', 'codex', 'antigravity'],
        getAnalytics: async (query) => {
          seen.push(query);
          return { schemaVersion: 1 } as unknown as AccountAnalytics;
        },
      },
      async (get) => {
        const refused = await get('?provider=kimi-code');
        expect(refused.status).toBe(400);
        expect(await refused.json()).toEqual({
          error: 'Select a provider that this server reports.',
          code: 'invalid_provider',
        });
        expect((await get('?provider=antigravity')).status).toBe(200);
        expect((await get('?provider=all')).status).toBe(200);
        // CLI usage on a route no provider claims; tool names are never providers
        expect((await get('?provider=other')).status).toBe(200);
        expect((await get('?provider=omp')).status).toBe(400);
        expect((await get('?provider=zcode')).status).toBe(400);
      }
    );
    expect(seen.map((query) => query.provider)).toEqual(['antigravity', 'all', 'other']);
  });

  it('maps service range errors to 400 with their code and keeps fixed error codes', async () => {
    await withRouter(
      {
        getAnalytics: async () => {
          throw new AccountAnalyticsQueryError('invalid_range', 'raw detail sentinel');
        },
      },
      async (get) => {
        const response = await get('?range=custom&from=2026-09-01&to=2026-09-02');
        expect(response.status).toBe(400);
        expect(response.headers.get('cache-control')).toBe('no-store');
        const body = await response.json();
        expect(body.code).toBe('invalid_range');
        expect(JSON.stringify(body)).not.toContain('sentinel');
      }
    );
    expect(await (await request('', {})).json()).toMatchObject({ code: 'auth_required' });
    expect(
      await (
        await request('', { 'x-test-session': 'true', origin: 'https://attacker.example' })
      ).json()
    ).toMatchObject({ code: 'origin_required' });
    failing = true;
    expect(await (await request()).json()).toMatchObject({ code: 'analytics_unavailable' });
  });
});
