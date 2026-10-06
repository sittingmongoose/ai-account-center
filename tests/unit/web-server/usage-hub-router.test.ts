import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import express from 'express';
import type { Server } from 'http';
import type { IncomingMessage } from 'http';
import {
  CLAUDE_USAGE_URL,
  CODEX_CREDITS_URL,
  CODEX_USAGE_URL,
} from '../../../src/web-server/usage-hub/usage-hub-contract';
import {
  hashUsageHubKey,
  type UsageHubKeyState,
} from '../../../src/web-server/usage-hub/usage-hub-key-store';
import {
  createUsageHubRouter,
  presentedManagementKey,
  type UsageHubRouterDeps,
} from '../../../src/web-server/usage-hub/usage-hub-router';
import {
  usageHubAuthIndex,
  type UsageHubAccount,
} from '../../../src/web-server/usage-hub/usage-hub-projection';
import { isSecureTransport } from '../../../src/web-server/middleware/secure-transport';
import { parseTrustedNetworks } from '../../../src/web-server/middleware/trusted-networks';

const KEY = `aacu_${'A'.repeat(43)}`;
const ON: UsageHubKeyState = {
  state: 'on',
  record: { version: 1, keySha256: hashUsageHubKey(KEY), createdAt: '2026-10-06T12:00:00Z' },
};

const codexAccount: UsageHubAccount = {
  authFile: {
    id: 'codex:alpha',
    auth_index: usageHubAuthIndex('codex:alpha'),
    provider: 'codex',
    type: 'codex',
    label: 'alpha',
    email: 'alpha@example.com',
    disabled: false,
    status: 'active',
    status_message: 'Cached AI Account Center reading.',
    sampled_at: '2026-10-06T12:00:00.000Z',
  },
  usage: {
    plan_type: 'pro',
    rate_limit: {
      primary_window: { used_percent: 30, reset_at: 1791298800, limit_window_seconds: 18000 },
      secondary_window: { used_percent: 78, reset_at: 1791581532, limit_window_seconds: 604800 },
    },
  },
};
const claudeAccount: UsageHubAccount = {
  authFile: {
    ...codexAccount.authFile,
    id: 'claude:beta',
    auth_index: usageHubAuthIndex('claude:beta'),
    provider: 'claude',
    type: 'claude',
    label: 'beta',
    email: 'beta@example.com',
  },
  usage: {
    five_hour: { utilization: 10, resets_at: null },
    seven_day: { utilization: 50, resets_at: '2026-10-10T00:00:00.000Z' },
    limits: [],
  },
};
const emptyAccount: UsageHubAccount = {
  authFile: {
    ...codexAccount.authFile,
    id: 'codex:gamma',
    auth_index: usageHubAuthIndex('codex:gamma'),
    label: 'gamma',
    status: 'error',
    status_message: 'AI Account Center has no current reading for this account.',
    sampled_at: null,
  },
  usage: null,
};

let server: Server;
let baseUrl = '';
let reads = 0;
let keyState: UsageHubKeyState = ON;
let secure = true;
let originalFetch: typeof fetch;
let upstreamCalls: string[] = [];

async function start(overrides: Partial<UsageHubRouterDeps> = {}): Promise<void> {
  const app = express();
  app.use(express.json());
  app.use(
    '/v0/management',
    createUsageHubRouter({
      accounts: {
        read: async () => {
          reads += 1;
          return [codexAccount, claudeAccount, emptyAccount];
        },
      },
      readKeyState: async () => keyState,
      isSecure: () => secure,
      ...overrides,
    })
  );
  server = await new Promise<Server>((resolve) => {
    const instance = app.listen(0, '127.0.0.1', () => resolve(instance));
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('No test port');
  baseUrl = `http://127.0.0.1:${address.port}`;
}

beforeEach(() => {
  reads = 0;
  keyState = ON;
  secure = true;
  upstreamCalls = [];
  originalFetch = globalThis.fetch;
  // Any upstream request from the hub would go through fetch: record and refuse it.
  const testFetch = originalFetch;
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input instanceof Request ? input.url : input);
    if (!url.startsWith(baseUrl)) {
      upstreamCalls.push(url);
      throw new Error('The usage hub must never make upstream requests');
    }
    return testFetch(input, init);
  }) as typeof fetch;
});

afterEach(async () => {
  globalThis.fetch = originalFetch;
  if (server) await new Promise<void>((resolve) => server.close(() => resolve()));
  expect(upstreamCalls).toEqual([]);
});

function get(path: string, headers: Record<string, string> = {}): Promise<Response> {
  return fetch(`${baseUrl}/v0/management${path}`, { headers });
}

function apiCall(body: unknown, headers: Record<string, string> = {}): Promise<Response> {
  return fetch(`${baseUrl}/v0/management/api-call`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${KEY}`, ...headers },
    body: JSON.stringify(body),
  });
}

/** The exact request T3's cliproxyApi.ts sends for a Codex or Claude account. */
function t3Request(account: UsageHubAccount, url: string) {
  return {
    auth_index: account.authFile.auth_index,
    method: 'GET',
    url,
    header:
      account.authFile.provider === 'codex'
        ? {
            Authorization: 'Bearer $TOKEN$',
            'Content-Type': 'application/json',
            'OpenAI-Beta': 'codex-1',
            Originator: 'Codex Desktop',
          }
        : { Authorization: 'Bearer $TOKEN$', 'anthropic-beta': 'oauth-2025-04-20' },
  };
}

describe('usage hub auth', () => {
  it('is off (404) until a key is configured, and reads nothing', async () => {
    keyState = { state: 'off' };
    await start();
    const response = await get('/auth-files', { authorization: `Bearer ${KEY}` });
    expect(response.status).toBe(404);
    expect((await response.json()).code).toBe('usage_hub_off');
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(reads).toBe(0);
  });

  it('answers 503 when the key file cannot be read safely', async () => {
    keyState = { state: 'invalid' };
    await start();
    expect((await get('/auth-files', { authorization: `Bearer ${KEY}` })).status).toBe(503);
  });

  it('refuses an insecure transport before looking at the key', async () => {
    secure = false;
    await start();
    const response = await get('/auth-files', { authorization: `Bearer ${KEY}` });
    expect(response.status).toBe(403);
    expect((await response.json()).code).toBe('secure_transport_required');
    expect(reads).toBe(0);
  });

  it('requires a key (401) and refuses a wrong one (401)', async () => {
    await start();
    const missing = await get('/auth-files');
    expect(missing.status).toBe(401);
    expect((await missing.json()).code).toBe('missing_management_key');
    const wrong = await get('/auth-files', { authorization: `Bearer aacu_${'B'.repeat(43)}` });
    expect(wrong.status).toBe(401);
    expect((await wrong.json()).code).toBe('invalid_management_key');
    expect((await get('/auth-files', { authorization: 'Basic abc' })).status).toBe(401);
    expect(reads).toBe(0);
  });

  it('accepts the right key as a bearer token or as X-Management-Key', async () => {
    await start();
    expect((await get('/auth-files', { authorization: `Bearer ${KEY}` })).status).toBe(200);
    expect((await get('/auth-files', { 'x-management-key': KEY })).status).toBe(200);
  });

  it('refuses a key in the query string without reading it', async () => {
    await start();
    const response = await get(`/auth-files?key=${KEY}`, { authorization: `Bearer ${KEY}` });
    expect(response.status).toBe(400);
    expect((await response.json()).code).toBe('key_in_query');
  });

  it('limits refused keys per client (429)', async () => {
    await start({ failuresPerWindow: 3 });
    for (let index = 0; index < 3; index += 1) {
      expect((await get('/auth-files', { authorization: 'Bearer wrong' })).status).toBe(401);
    }
    const limited = await get('/auth-files', { authorization: `Bearer ${KEY}` });
    expect(limited.status).toBe(429);
    expect((await limited.json()).code).toBe('rate_limited');
    expect(Number(limited.headers.get('retry-after'))).toBeGreaterThan(0);
  });

  it('limits the request rate per client (429)', async () => {
    await start({ requestsPerMinute: 2 });
    expect((await get('/auth-files', { authorization: `Bearer ${KEY}` })).status).toBe(200);
    expect((await get('/auth-files', { authorization: `Bearer ${KEY}` })).status).toBe(200);
    expect((await get('/auth-files', { authorization: `Bearer ${KEY}` })).status).toBe(429);
  });

  it('parses the presented key from either header', () => {
    expect(presentedManagementKey({ headers: { authorization: `Bearer ${KEY}` } })).toBe(KEY);
    expect(presentedManagementKey({ headers: { authorization: `bearer  ${KEY} ` } })).toBe(KEY);
    expect(presentedManagementKey({ headers: { authorization: 'Bearer' } })).toBe('');
    expect(presentedManagementKey({ headers: { 'x-management-key': ` ${KEY} ` } })).toBe(KEY);
    expect(presentedManagementKey({ headers: {} })).toBeNull();
  });
});

describe('usage hub GET /v0/management/auth-files', () => {
  it('lists every Codex and Claude account in the shape T3 decodes', async () => {
    await start();
    const response = await get('/auth-files', { authorization: `Bearer ${KEY}` });
    expect(response.status).toBe(200);
    const body = await response.json();
    expect(body).toEqual({
      files: [codexAccount.authFile, claudeAccount.authFile, emptyAccount.authFile],
    });
    for (const file of body.files) {
      expect(typeof file.id).toBe('string');
      expect(typeof file.auth_index).toBe('string');
      expect(['codex', 'claude']).toContain(file.provider);
      expect(file.disabled).toBe(false);
    }
  });
});

describe('usage hub POST /v0/management/api-call', () => {
  it('answers the Codex usage read from the cache in the wham/usage shape', async () => {
    await start();
    const response = await apiCall(t3Request(codexAccount, CODEX_USAGE_URL));
    expect(response.status).toBe(200);
    const body = await response.json();
    expect(body.status_code).toBe(200);
    expect(typeof body.body).toBe('string');
    expect(JSON.parse(body.body)).toEqual(codexAccount.usage);
  });

  it('answers the Claude usage read from the cache in the oauth/usage shape', async () => {
    await start();
    const body = await (await apiCall(t3Request(claudeAccount, CLAUDE_USAGE_URL))).json();
    expect(body.status_code).toBe(200);
    expect(JSON.parse(body.body)).toEqual(claudeAccount.usage);
  });

  it('reports a missing reading as an upstream 503 so T3 marks only that account', async () => {
    await start();
    const body = await (await apiCall(t3Request(emptyAccount, CODEX_USAGE_URL))).json();
    expect(body.status_code).toBe(503);
    expect(JSON.parse(body.body).error.type).toBe('no_cached_reading');
  });

  it('refuses reset credits, their redemption and reset-quota', async () => {
    await start();
    const credits = await apiCall(t3Request(codexAccount, CODEX_CREDITS_URL));
    expect(credits.status).toBe(403);
    expect((await credits.json()).code).toBe('unsupported_api_call');
    const consume = await apiCall({
      ...t3Request(codexAccount, `${CODEX_CREDITS_URL}/consume`),
      method: 'POST',
      data: '{"credit_id":"c1"}',
    });
    expect(consume.status).toBe(403);
    const reset = await fetch(`${baseUrl}/v0/management/reset-quota`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', authorization: `Bearer ${KEY}` },
      body: JSON.stringify({ auth_index: codexAccount.authFile.auth_index }),
    });
    expect(reset.status).toBe(403);
  });

  it('is not a proxy: any other URL, method or body is refused', async () => {
    await start();
    for (const body of [
      { ...t3Request(codexAccount, 'https://example.com/steal') },
      { ...t3Request(codexAccount, `${CODEX_USAGE_URL}?x=1`) },
      { ...t3Request(codexAccount, 'http://127.0.0.1:3000/api/accounts/dashboard') },
      { ...t3Request(codexAccount, CODEX_USAGE_URL), method: 'POST' },
      { ...t3Request(codexAccount, CODEX_USAGE_URL), method: 'DELETE' },
      { ...t3Request(codexAccount, CODEX_USAGE_URL), data: '{}' },
      { ...t3Request(codexAccount, CLAUDE_USAGE_URL) },
    ]) {
      const response = await apiCall(body);
      expect(response.status).toBe(403);
      expect((await response.json()).code).toBe('unsupported_api_call');
    }
  });

  it('validates the request and the auth_index', async () => {
    await start();
    expect((await apiCall([1, 2])).status).toBe(400);
    expect((await apiCall({ url: CODEX_USAGE_URL })).status).toBe(400);
    expect((await apiCall({ auth_index: 7, url: CODEX_USAGE_URL })).status).toBe(400);
    const unknown = await apiCall({ auth_index: 'nope', method: 'GET', url: CODEX_USAGE_URL });
    expect(unknown.status).toBe(404);
    expect((await unknown.json()).code).toBe('unknown_auth_index');
  });

  it('never echoes the token placeholder or any token-bearing field', async () => {
    await start();
    const texts = [
      await (await get('/auth-files', { authorization: `Bearer ${KEY}` })).text(),
      await (await apiCall(t3Request(codexAccount, CODEX_USAGE_URL))).text(),
      await (await apiCall(t3Request(claudeAccount, CLAUDE_USAGE_URL))).text(),
      await (await apiCall(t3Request(codexAccount, 'https://example.com/x'))).text(),
    ];
    for (const text of texts) {
      expect(text).not.toContain('$TOKEN$');
      expect(text).not.toContain(KEY);
      expect(text).not.toMatch(
        /"[^"]*(token|secret|cookie|password|credential|refresh)[^"]*"\s*:/i
      );
    }
  });

  it('answers 404 for every other management path', async () => {
    await start();
    expect((await get('/config', { authorization: `Bearer ${KEY}` })).status).toBe(404);
    expect((await get('/usage', { authorization: `Bearer ${KEY}` })).status).toBe(404);
  });
});

describe('usage hub transport rule (the dashboard isSecureTransport)', () => {
  function request(remoteAddress: string, headers: Record<string, string> = {}): IncomingMessage {
    return { socket: { remoteAddress }, headers } as unknown as IncomingMessage;
  }
  const lan = { enabled: true, networks: parseTrustedNetworks(undefined).networks };
  const off = { enabled: false, networks: lan.networks };

  it('allows loopback with a loopback Host, and a LAN peer only while local network trust is on', () => {
    expect(
      isSecureTransport(request('127.0.0.1', { host: '127.0.0.1:3000' }), {
        localNetworkTrust: off,
      })
    ).toBe(true);
    expect(
      isSecureTransport(request('192.168.50.20', { host: '192.168.50.179:3000' }), {
        localNetworkTrust: lan,
      })
    ).toBe(true);
    expect(
      isSecureTransport(request('192.168.50.20', { host: '192.168.50.179:3000' }), {
        localNetworkTrust: off,
      })
    ).toBe(false);
  });

  it('refuses a public peer, a proxied LAN peer and a loopback peer with a foreign Host', () => {
    expect(
      isSecureTransport(request('203.0.113.9', { host: 'x:3000' }), { localNetworkTrust: lan })
    ).toBe(false);
    expect(
      isSecureTransport(request('192.168.50.20', { 'x-forwarded-for': '203.0.113.9' }), {
        localNetworkTrust: lan,
      })
    ).toBe(false);
    expect(
      isSecureTransport(request('127.0.0.1', { host: 'evil.example:3000' }), {
        localNetworkTrust: lan,
        trustedProxy: null,
      })
    ).toBe(false);
  });

  it('uses that rule by default: a loopback request with a foreign Host is refused', async () => {
    await start({ isSecure: undefined });
    const ok = await get('/auth-files', { authorization: `Bearer ${KEY}` });
    expect(ok.status).toBe(200);
    // fetch cannot change Host; a raw request can.
    const { request: raw } = await import('http');
    const status = await new Promise<number>((resolve, reject) => {
      const url = new URL(`${baseUrl}/v0/management/auth-files`);
      const req = raw(
        {
          host: url.hostname,
          port: url.port,
          path: url.pathname,
          headers: { host: 'evil.example', authorization: `Bearer ${KEY}` },
        },
        (res) => {
          res.resume();
          resolve(res.statusCode ?? 0);
        }
      );
      req.on('error', reject);
      req.end();
    });
    expect(status).toBe(403);
  });
});
