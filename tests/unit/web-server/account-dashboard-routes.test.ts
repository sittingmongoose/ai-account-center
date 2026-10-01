import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import { createAccountDashboardRouter } from '../../../src/web-server/routes/account-dashboard-routes';
import type { AccountDashboard } from '../../../src/web-server/services/account-dashboard-types';

let server: Server;
let baseUrl: string;
let calls: Array<[string, boolean]>;
let failing = false;

const dashboard: AccountDashboard = {
  schemaVersion: 1,
  updatedAt: '2026-10-01T12:00:00Z',
  accounts: [],
  codexAutoSwitch: {
    enabled: true,
    thresholdPercent: 5,
    pollIntervalSeconds: 60,
    outcome: 'healthy',
    message: 'Healthy',
    activationInProgress: false,
  },
};

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
    createAccountDashboardRouter({
      getDashboard: async (platform, refresh) => {
        calls.push([platform, refresh]);
        if (failing) throw new Error('raw secret upstream body sentinel');
        return dashboard;
      },
    })
  );
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  baseUrl = `http://127.0.0.1:${address.port}`;
});

afterEach(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

function request(query = '', authenticated = true): Promise<Response> {
  return fetch(`${baseUrl}/api/accounts/dashboard${query}`, {
    headers: authenticated ? { 'x-test-session': 'true' } : {},
  });
}

describe('account dashboard route', () => {
  it('requires an authenticated session before reading accounts', async () => {
    expect((await request('', false)).status).toBe(401);
    expect(calls).toEqual([]);
  });

  it('uses Mac and cached refresh defaults and prevents browser caching', async () => {
    const response = await request();
    expect(response.status).toBe(200);
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(await response.json()).toEqual(dashboard);
    expect(calls).toEqual([['mac', false]]);
  });

  it('passes Windows and explicit true refresh to the safe collector', async () => {
    expect((await request('?platform=windows&refresh=true')).status).toBe(200);
    expect(calls).toEqual([['windows', true]]);
  });

  it.each([
    '?platform=ubuntu',
    '?platform=',
    '?platform=MAC',
    '?platform=mac&platform=windows',
    '?platform[host]=evil',
    '?refresh=1',
    '?refresh=',
    '?refresh=True',
    '?refresh=true&refresh=false',
    '?refresh[enabled]=true',
    '?host=evil',
    '?profilePath=/private',
  ])('rejects invalid or unrecognized query %s', async (query) => {
    expect((await request(query)).status).toBe(400);
    expect(calls).toEqual([]);
  });

  it('does not expose raw collector errors', async () => {
    failing = true;
    const response = await request();
    expect(response.status).toBe(500);
    const body = await response.text();
    expect(body).not.toContain('sentinel');
    expect(body).toContain('could not be read safely');
  });
});
