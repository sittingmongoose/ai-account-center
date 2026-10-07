/**
 * CONTRACT-auth-devices section 2a, rule 3 (`lan-https-proxy`): an HTTPS
 * reverse proxy on another LAN computer, through the real middleware order. A
 * peer counts as the proxy only when the kind is set and the socket address
 * exactly matches `trusted_proxy_addresses`; its requests are secure only
 * with `X-Forwarded-Proto: https` and an IP address as the rightmost
 * `X-Forwarded-For` entry, are never LAN-trusted, and never count as the
 * dashboard computer. Forwarded clients have their own limiter keys, sessions
 * and tray keys that crossed the LAN in plain text do not work through the
 * proxy, and `/v0/management` answers 404 through it. Temporary CCS_HOME,
 * bcrypt cost 4, placeholder names and addresses only.
 */
import { afterEach, describe, expect, it } from 'bun:test';

import { prepareFirstRunSetupCode } from '../../../src/web-server/dashboard-auth-runtime';
import { createUsageHubRouter } from '../../../src/web-server/usage-hub/usage-hub-router';
import { writeUsageHubKey } from '../../../src/web-server/usage-hub/usage-hub-key-store';
import {
  clearRecentLogEntries,
  getRecentLogEntries,
} from '../../../src/services/logging/log-buffer';
import { invalidateLoggingConfigCache } from '../../../src/services/logging/log-config';
import {
  Client,
  pairTray,
  PASSWORD,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const PROXY = '192.168.1.20';
const HOST = 'aac.example.test';
const ORIGIN = `https://${HOST}`;
const CLIENT_A = '203.0.113.17';
const CLIENT_B = '203.0.113.18';
const OTHER_LAN = '192.168.1.21';
const LAN_HOST = '192.168.1.10:3000';
const NEW_PASSWORD = 'second-fixture-password';
const TLS = {
  trusted_proxy: 'lan-https-proxy',
  trusted_proxy_addresses: [PROXY],
  public_origin: ORIGIN,
};
const NETWORK_ON = { trust_local_network: true };

let harness: Harness | null = null;

afterEach(async () => {
  await harness?.close();
  harness = null;
  clearRecentLogEntries();
  invalidateLoggingConfigCache();
});

/** The peer is the proxy, forwarding `client` over outer https. */
function proxiedAs(h: Harness, client: string, proto = 'https'): void {
  h.peer.address = PROXY;
  h.peer.host = HOST;
  h.peer.headers = { 'x-forwarded-for': client, 'x-forwarded-proto': proto };
}

/** A direct peer with no forwarded headers (loopback uses the real loopback Host). */
function directFrom(h: Harness, address: string): void {
  h.peer.address = address;
  h.peer.host = address === '127.0.0.1' ? null : LAN_HOST;
  h.peer.headers = {};
}

async function signedIn(h: Harness): Promise<Client> {
  const client = new Client(h);
  const response = await client.login();
  expect(response.status).toBe(200);
  return client;
}

function setCookies(headers: Headers): string[] {
  return typeof headers.getSetCookie === 'function'
    ? headers.getSetCookie()
    : [headers.get('set-cookie') ?? ''].filter(Boolean);
}

async function wrongLogins(h: Harness, count: number): Promise<number[]> {
  const statuses: number[] = [];
  for (let attempt = 0; attempt < count; attempt += 1) {
    statuses.push((await new Client(h).login('wrong-password')).status);
  }
  return statuses;
}

describe('GET /api/auth/check through the proxy', () => {
  it('reads encrypted, the real client and the hop, and never the trusted local network', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    proxiedAs(harness, `198.51.100.9, ${CLIENT_A}`);
    const check = await new Client(harness).send('GET', '/api/auth/check');
    expect(check.status).toBe(200);
    expect(check.body).toMatchObject({
      isLocalAccess: false,
      accessMode: 'login',
      setupCodeRequired: false,
      secureTransport: true,
      secureOrigin: ORIGIN,
      connection: { peer: CLIENT_A, trusted: false, proxied: true },
    });
  });

  it('is not secure without headers, with a plain outer proto, or without an IP as the last entry', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    harness.peer.address = PROXY;
    harness.peer.host = HOST;
    for (const [headers, peer] of [
      [{}, 'unknown'],
      [{ 'x-forwarded-for': CLIENT_A, 'x-forwarded-proto': 'http' }, CLIENT_A],
      [{ 'x-forwarded-for': CLIENT_A }, CLIENT_A],
      [{ 'x-forwarded-proto': 'https' }, 'unknown'],
      [{ 'x-forwarded-for': 'not-an-address', 'x-forwarded-proto': 'https' }, 'unknown'],
      [{ 'x-forwarded-for': `${CLIENT_A}, <script>`, 'x-forwarded-proto': 'https' }, 'unknown'],
    ] as const) {
      harness.peer.headers = { ...headers };
      const check = await new Client(harness).send('GET', '/api/auth/check');
      expect([headers, check.body.secureTransport, check.body.connection]).toEqual([
        headers,
        false,
        { peer, trusted: false, proxied: true },
      ]);
    }
  });

  it('ignores the same headers from a peer the list does not name', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    harness.peer.address = OTHER_LAN;
    harness.peer.host = HOST;
    harness.peer.headers = { 'x-forwarded-for': CLIENT_A, 'x-forwarded-proto': 'https' };
    const check = await new Client(harness).send('GET', '/api/auth/check');
    expect(check.body.secureTransport).toBe(false);
    expect(check.body.connection).toEqual({ peer: OTHER_LAN, trusted: false });
  });

  it("does not show an anonymous visitor the owner's settings, but does once signed in", async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    proxiedAs(harness, CLIENT_A);
    const visitor = new Client(harness);
    const check = await visitor.send('GET', '/api/auth/check');
    expect(check.body).not.toHaveProperty('trustedLocalNetwork');
    const setup = await visitor.send('GET', '/api/auth/setup');
    expect(setup.status).toBe(200);
    for (const key of [
      'trustedLocalNetwork',
      'sessionTimeoutHours',
      'sessionLifetimeDays',
      'managedBy',
    ]) {
      expect(setup.body).not.toHaveProperty(key);
    }
    expect(setup.body).toMatchObject({ enabled: true, configured: true, secureTransport: true });
    const browser = await signedIn(harness);
    expect((await browser.send('GET', '/api/auth/check')).body).toMatchObject({
      authenticated: true,
      trustedLocalNetwork: true,
    });
    // A direct visitor on the LAN sees them as before.
    directFrom(harness, OTHER_LAN);
    const lanSetup = (await new Client(harness).send('GET', '/api/auth/setup')).body;
    expect(lanSetup).toMatchObject({ trustedLocalNetwork: true, managedBy: 'config' });
    expect(lanSetup).toHaveProperty('sessionLifetimeDays');
  });
});

describe('sensitive auth routes through the proxy', () => {
  it('accept a password change, pairing and rotation, and refuse them without headers', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    proxiedAs(harness, CLIENT_A);
    const token = String((await pairTray(harness, { deviceName: 'fixture-proxy' })).body.token);
    expect(token).toMatch(/^aacd_/);
    const browser = await signedIn(harness);
    const change = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(200);
    expect(
      (await new Client(harness, token).send('POST', '/api/auth/devices/me/rotate')).status
    ).toBe(200);

    harness.peer.headers = {};
    const refused = await browser.send('POST', '/api/auth/password', {
      currentPassword: NEW_PASSWORD,
      newPassword: PASSWORD,
    });
    expect(refused.status).toBe(401);
  });

  it('takes a password only over the HTTPS side', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_A, 'http');
    const plain = await new Client(harness).login();
    expect([plain.status, plain.body.code, plain.body.secureOrigin]).toEqual([
      403,
      'secure_transport_required',
      ORIGIN,
    ]);
    expect(setCookies(plain.headers)).toEqual([]);
  });

  it('sets a Secure session cookie, while a direct login does not', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_A);
    const login = await new Client(harness).login();
    expect(login.status).toBe(200);
    expect(setCookies(login.headers).some((cookie) => /(^|;\s*)Secure/i.test(cookie))).toBe(true);
    directFrom(harness, '127.0.0.1');
    const direct = await new Client(harness).login();
    expect(direct.status).toBe(200);
    expect(setCookies(direct.headers).some((cookie) => /(^|;\s*)Secure/i.test(cookie))).toBe(false);
  });
});

describe('never the dashboard computer', () => {
  it('refuses a network trust turn-on and reports it cannot turn on', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, '127.0.0.1');
    const browser = await signedIn(harness);
    const network = await browser.send('GET', '/api/auth/network');
    expect(network.body).toMatchObject({
      trustLocalNetwork: false,
      connection: { peer: '127.0.0.1', trusted: false, proxied: true },
      canTurnOn: false,
    });
    const refused = await browser.send(
      'PUT',
      '/api/auth/network',
      { trustLocalNetwork: true },
      { origin: ORIGIN }
    );
    expect([refused.status, refused.body.code]).toEqual([403, 'loopback_required']);
  });

  it('still needs the one-time code for a first-run setup, and accepts it over the proxy', async () => {
    harness = await startAuthHarness({
      mode: 'setup',
      dashboardTls: TLS,
      dashboardNetwork: NETWORK_ON,
    });
    const code = await prepareFirstRunSetupCode(() => undefined);
    proxiedAs(harness, '127.0.0.1');
    const body = { username: 'lan-admin', password: NEW_PASSWORD };
    const missing = await new Client(harness).send('POST', '/api/auth/setup', body, {
      origin: ORIGIN,
    });
    expect([missing.status, missing.body.code]).toEqual([403, 'setup_code_required']);
    const right = await new Client(harness).send(
      'POST',
      '/api/auth/setup',
      { ...body, setupCode: code },
      { origin: ORIGIN }
    );
    expect(right.status).toBe(201);
  });
});

describe('per-client rate limits through the proxy', () => {
  it('locks out one forwarded client without locking out another', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_A);
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const wrong = await new Client(harness).login('wrong-password');
      expect([wrong.status, wrong.body.triesLeft]).toEqual([401, 4 - attempt]);
    }
    expect((await new Client(harness).login(PASSWORD)).status).toBe(429);
    proxiedAs(harness, CLIENT_B);
    const other = await new Client(harness).login('wrong-password');
    expect([other.status, other.body.triesLeft]).toEqual([401, 4]);
    expect((await new Client(harness).login()).status).toBe(200);
  });

  it('never spends the budget of the browser on the dashboard computer or of a LAN computer', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    // Anything on the proxy's computer can claim to forward 127.0.0.1 or a LAN address.
    for (const spoofed of ['127.0.0.1', '::1', OTHER_LAN]) {
      proxiedAs(harness, spoofed);
      expect(await wrongLogins(harness, 6)).toEqual([401, 401, 401, 401, 401, 429]);
    }
    // The mapped spelling is the same forwarded client, still apart from the real loopback.
    proxiedAs(harness, '::ffff:127.0.0.1');
    expect((await new Client(harness).login()).status).toBe(429);
    directFrom(harness, '127.0.0.1');
    const vm = await new Client(harness).login('wrong-password');
    expect([vm.status, vm.body.triesLeft]).toEqual([401, 4]);
    expect((await new Client(harness).login()).status).toBe(200);
    directFrom(harness, OTHER_LAN);
    expect((await new Client(harness).login()).status).toBe(200);
  });

  it('keys a direct LAN spoofer as itself, whatever X-Forwarded-For it sends', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    harness.peer.address = OTHER_LAN;
    harness.peer.host = LAN_HOST;
    harness.peer.headers = { 'x-forwarded-for': '127.0.0.1', 'x-forwarded-proto': 'https' };
    expect(await wrongLogins(harness, 6)).toEqual([401, 401, 401, 401, 401, 429]);
    directFrom(harness, OTHER_LAN);
    expect((await new Client(harness).login()).status).toBe(429);
    directFrom(harness, '127.0.0.1');
    expect((await new Client(harness).login()).status).toBe(200);
  });

  it('puts every forwarded value that is not an IP address in one shared bucket', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    for (const garbage of ['<script>', 'unknown', `${CLIENT_A}, nope`, '[::1]', 'fe80::1%eth0']) {
      proxiedAs(harness, garbage);
      await new Client(harness).login('wrong-password');
    }
    proxiedAs(harness, 'something-else');
    expect((await new Client(harness).login()).status).toBe(429);
    proxiedAs(harness, CLIENT_A);
    expect((await new Client(harness).login()).status).toBe(200);
  });

  it('keys mapped and plain forms of one forwarded client together', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_A);
    await wrongLogins(harness, 3);
    proxiedAs(harness, `::ffff:${CLIENT_A}`);
    const wrong = await new Client(harness).login('wrong-password');
    expect([wrong.status, wrong.body.triesLeft]).toEqual([401, 1]);
    proxiedAs(harness, '2001:db8:aa::7');
    const v6 = await new Client(harness).login('wrong-password');
    expect([v6.status, v6.body.triesLeft]).toEqual([401, 4]);
  });
});

describe('sessions and tray keys that crossed the LAN in plain text', () => {
  it('a session signed in over plain HTTP on the LAN is refused through the proxy, and keeps working on the LAN', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    directFrom(harness, OTHER_LAN);
    const browser = await signedIn(harness);
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);

    // The same cookie, copied, through the proxy.
    proxiedAs(harness, CLIENT_A);
    const copied = new Client(harness);
    copied.cookie = browser.cookie;
    const session = await copied.send('GET', '/api/auth/session');
    expect([session.status, session.body.code]).toEqual([401, 'auth_required']);
    // Nothing about the stored session is re-sent or extended through the proxy.
    expect(setCookies(session.headers)).toEqual([]);
    expect((await copied.send('GET', '/api/auth/check')).body).toMatchObject({
      authenticated: false,
      username: null,
    });
    const logout = await copied.send('POST', '/api/auth/logout', {}, { origin: ORIGIN });
    expect(logout.status).toBe(401);

    // Signing in again through the proxy starts a fresh session and leaves the LAN one alone.
    const again = await copied.login();
    expect(again.status).toBe(200);
    expect(copied.cookie).not.toBe(browser.cookie);
    expect(setCookies(again.headers).some((cookie) => /(^|;\s*)Secure/i.test(cookie))).toBe(true);
    expect((await copied.send('GET', '/api/auth/session')).status).toBe(200);

    // The browser on the LAN is still signed in.
    directFrom(harness, OTHER_LAN);
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
  });

  it('a session signed in through the proxy or on the dashboard computer works through it', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_A);
    const remote = await signedIn(harness);
    expect((await remote.send('GET', '/api/auth/session')).status).toBe(200);
    // Over a plain proxied request even that session is not used.
    proxiedAs(harness, CLIENT_A, 'http');
    expect((await remote.send('GET', '/api/auth/session')).status).toBe(401);

    directFrom(harness, '127.0.0.1');
    const local = await signedIn(harness);
    proxiedAs(harness, CLIENT_A);
    expect((await local.send('GET', '/api/auth/session')).status).toBe(200);
  });

  it('a tray key paired on the trusted LAN works there and is refused through the proxy', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    directFrom(harness, OTHER_LAN);
    const pair = await pairTray(harness, { deviceName: 'fixture-lan' });
    expect(pair.status).toBe(201);
    const tray = new Client(harness, String(pair.body.token));
    expect((await tray.send('GET', '/api/auth/devices/me')).status).toBe(200);

    proxiedAs(harness, CLIENT_A);
    const refused = await tray.send('GET', '/api/auth/devices/me');
    expect([refused.status, refused.body.code]).toEqual([403, 'plain_http_token']);
    directFrom(harness, OTHER_LAN);
    expect((await tray.send('GET', '/api/auth/devices/me')).status).toBe(200);
  });

  it('a tray key paired through the proxy works there until it is used over plain HTTP', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    proxiedAs(harness, CLIENT_A);
    const pair = await pairTray(harness, { deviceName: 'fixture-remote' });
    expect(pair.status).toBe(201);
    const tray = new Client(harness, String(pair.body.token));
    expect((await tray.send('GET', '/api/auth/devices/me')).status).toBe(200);
    const rotated = await tray.send('POST', '/api/auth/devices/me/rotate');
    expect(rotated.status).toBe(200);
    const next = new Client(harness, String(rotated.body.token));
    expect((await next.send('GET', '/api/auth/devices/me')).status).toBe(200);

    // One request over plain HTTP on the LAN, and the key no longer works through the proxy.
    directFrom(harness, OTHER_LAN);
    expect((await next.send('GET', '/api/auth/devices/me')).status).toBe(200);
    proxiedAs(harness, CLIENT_A);
    expect((await next.send('GET', '/api/auth/devices/me')).body.code).toBe('plain_http_token');
  });
});

describe('origin checks through the proxy', () => {
  it('passes a write from the public origin and refuses a mismatched one', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS, dashboardNetwork: NETWORK_ON });
    proxiedAs(harness, CLIENT_A);
    const browser = await signedIn(harness);
    const foreign = await browser.send(
      'PUT',
      '/api/auth/network',
      { trustLocalNetwork: false },
      { origin: 'https://other.example.test' }
    );
    expect([foreign.status, foreign.body.code]).toEqual([403, 'origin_required']);
    const own = await browser.send(
      'PUT',
      '/api/auth/network',
      { trustLocalNetwork: false },
      { origin: ORIGIN }
    );
    expect(own.status).toBe(200);
  });
});

describe('client identity through the proxy', () => {
  it('logs the real client in audit lines and the request log, and the proxy as via', async () => {
    const silenced = { ...console };
    for (const name of ['log', 'info', 'warn', 'error', 'debug'] as const) {
      console[name] = () => undefined;
    }
    try {
      harness = await startAuthHarness({ dashboardTls: TLS, logging: true });
      invalidateLoggingConfigCache();
      clearRecentLogEntries();
      proxiedAs(harness, CLIENT_A);
      expect((await new Client(harness).login('wrong-password')).status).toBe(401);
      const failure = getRecentLogEntries().find((entry) => entry.event === 'auth.login.failed');
      expect(failure?.context).toMatchObject({
        remoteAddress: CLIENT_A,
        reason: 'invalid_credentials',
      });
      const completed = getRecentLogEntries().find(
        (entry) => entry.event === 'request.completed' && entry.context?.path === '/api/auth/login'
      );
      expect(completed?.context).toMatchObject({
        remoteAddress: CLIENT_A,
        via: PROXY,
        statusCode: 401,
      });
      clearRecentLogEntries();
      directFrom(harness, OTHER_LAN);
      await new Client(harness).send('GET', '/api/auth/check');
      const direct = getRecentLogEntries().find((entry) => entry.event === 'request.completed');
      expect(direct?.context?.remoteAddress).toBe(OTHER_LAN);
      expect(direct?.context).not.toHaveProperty('via');
    } finally {
      Object.assign(console, silenced);
    }
  });

  it('stamps the real client on a paired device', async () => {
    harness = await startAuthHarness({ dashboardTls: TLS });
    proxiedAs(harness, CLIENT_B);
    expect((await pairTray(harness, { deviceName: 'fixture-seen' })).status).toBe(201);
    const browser = await signedIn(harness);
    const list = await browser.send('GET', '/api/auth/devices');
    expect(
      (list.body.devices as Array<{ lastSeenAddress: string }>).map(
        (device) => device.lastSeenAddress
      )
    ).toEqual([CLIENT_B]);
  });
});

describe('/v0/management through the proxy', () => {
  it('answers 404, while a direct request reaches the hub guard', async () => {
    harness = await startAuthHarness({
      dashboardTls: TLS,
      before: (app) => {
        app.use('/v0/management', createUsageHubRouter());
      },
    });
    proxiedAs(harness, CLIENT_A);
    for (const [method, route] of [
      ['GET', '/v0/management/auth-files'],
      ['POST', '/v0/management/api-call'],
    ] as const) {
      const response = await new Client(harness).send(
        method,
        route,
        method === 'POST' ? {} : undefined
      );
      expect([method, route, response.status, response.body.code]).toEqual([
        method,
        route,
        404,
        'not_found',
      ]);
      expect(response.headers.get('cache-control')).toBe('no-store');
    }
    // No key is set in the fixture, so a direct request reports the hub is off.
    directFrom(harness, '127.0.0.1');
    const direct = await new Client(harness).send('GET', '/v0/management/auth-files');
    expect([direct.status, direct.body.code]).toEqual([404, 'usage_hub_off']);
  });
});

describe('a proxy list the dashboard refuses', () => {
  it('turns the kind off: the listed peer is an ordinary LAN peer again', async () => {
    harness = await startAuthHarness({
      dashboardTls: { ...TLS, trusted_proxy_addresses: [PROXY, '192.168.1.0/24'] },
      dashboardNetwork: NETWORK_ON,
    });
    directFrom(harness, PROXY);
    const check = await new Client(harness).send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({ secureTransport: true });
    expect(check.body.connection).toEqual({ peer: PROXY, trusted: true });
    // With forwarded headers it is neither secure nor forwarded: req.ip stays the peer.
    proxiedAs(harness, CLIENT_A);
    expect((await new Client(harness).send('GET', '/api/auth/check')).body).toMatchObject({
      secureTransport: false,
      connection: { peer: PROXY, trusted: false },
    });
  });

  it('with loopback listed, keeps this computer and the usage hub on loopback working', async () => {
    harness = await startAuthHarness({
      dashboardTls: { ...TLS, trusted_proxy_addresses: ['127.0.0.1'] },
      before: (app) => {
        app.use('/v0/management', createUsageHubRouter());
      },
    });
    const key = await writeUsageHubKey({ replace: false });
    directFrom(harness, '127.0.0.1');
    const check = await new Client(harness).send('GET', '/api/auth/check');
    expect(check.body).toMatchObject({ isLocalAccess: true, secureTransport: true });
    expect(check.body.connection).toEqual({ peer: '127.0.0.1', trusted: false });
    const hub = await new Client(harness).send('GET', '/v0/management/auth-files', undefined, {
      authorization: `Bearer ${key}`,
    });
    expect(hub.status).toBe(200);
  });
});
