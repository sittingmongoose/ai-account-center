/**
 * CONTRACT-auth-devices section 2a, rule 4 (amended 2026-10-02): the trusted
 * local network through the real middleware order. The switch is off by
 * default, a private peer is secure only while it is on, it turns on only
 * from the dashboard computer and off from any session, and it is saved in
 * config.yaml without touching anything else. Temporary CCS_HOME, bcrypt cost 4.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import yaml from 'js-yaml';

import { loadOrCreateUnifiedConfig } from '../../../src/config/config-loader-facade';
import { prepareFirstRunSetupCode } from '../../../src/web-server/dashboard-auth-runtime';
import { DEFAULT_TRUSTED_NETWORKS } from '../../../src/web-server/middleware/trusted-networks';
import {
  Client,
  configYaml,
  pairTray,
  PASSWORD,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const PRIVATE_PEER = '192.168.50.20';
const PRIVATE_HOST = '192.168.50.10:3000';
const PUBLIC_PEER = '203.0.113.9';
const NEW_PASSWORD = 'second-fixture-password';
const ON = { trust_local_network: true };

let harness: Harness | null = null;

afterEach(async () => {
  await harness?.close();
  harness = null;
});

function atPeer(h: Harness, address: string, host: string | null = PRIVATE_HOST): void {
  h.peer.address = address;
  h.peer.host = host;
}

async function signedIn(h: Harness): Promise<Client> {
  const client = new Client(h);
  const response = await client.login();
  expect(response.status).toBe(200);
  return client;
}

function storedNetwork(h: Harness): unknown {
  const parsed = yaml.load(fs.readFileSync(configYaml(h), 'utf8')) as Record<string, unknown>;
  return parsed.dashboard_network;
}

/** Pair, rotate and a password change (last, so pairing still uses the first password). */
async function sensitiveAuthRoutes(h: Harness, browser: Client, token: string) {
  const pair = await pairTray(h, { deviceName: 'fixture-windows', platform: 'windows' });
  const rotate = await new Client(h, token).send('POST', '/api/auth/devices/me/rotate');
  const change = await browser.send('POST', '/api/auth/password', {
    currentPassword: PASSWORD,
    newPassword: NEW_PASSWORD,
  });
  return { change, pair, rotate };
}

describe('GET /api/auth/check and /api/auth/setup', () => {
  it('report the switch off by default, and a private connection as not trusted', async () => {
    harness = await startAuthHarness();
    atPeer(harness, `::ffff:${PRIVATE_PEER}`);
    for (const route of ['/api/auth/check', '/api/auth/setup']) {
      const response = await new Client(harness).send('GET', route);
      expect(response.status).toBe(200);
      expect(response.body).toMatchObject({
        secureTransport: false,
        trustedLocalNetwork: false,
        connection: { peer: PRIVATE_PEER, trusted: false },
      });
    }
  });

  it('report a private connection as trusted once it is on, and a public one as not', async () => {
    harness = await startAuthHarness({ dashboardNetwork: ON });
    atPeer(harness, `::ffff:${PRIVATE_PEER}`);
    for (const route of ['/api/auth/check', '/api/auth/setup']) {
      const response = await new Client(harness).send('GET', route);
      expect(response.body).toMatchObject({
        secureTransport: true,
        trustedLocalNetwork: true,
        connection: { peer: PRIVATE_PEER, trusted: true },
      });
    }
    atPeer(harness, PUBLIC_PEER);
    const response = await new Client(harness).send('GET', '/api/auth/check');
    expect(response.body).toMatchObject({
      secureTransport: false,
      trustedLocalNetwork: true,
      connection: { peer: PUBLIC_PEER, trusted: false },
    });
  });
});

describe('sensitive auth routes from a private peer', () => {
  it('are refused while the switch is off, before any password is compared', async () => {
    harness = await startAuthHarness();
    const token = String((await pairTray(harness)).body.token);
    const hashBefore = loadOrCreateUnifiedConfig().dashboard_auth?.password_hash;
    atPeer(harness, PRIVATE_PEER);
    const browser = await signedIn(harness);
    const { change, pair, rotate } = await sensitiveAuthRoutes(harness, browser, token);
    for (const response of [change, pair, rotate]) {
      expect([response.status, response.body.code]).toEqual([403, 'secure_transport_required']);
    }
    expect(loadOrCreateUnifiedConfig().dashboard_auth?.password_hash).toBe(hashBefore);
    expect((await new Client(harness).login('wrong-password')).body.triesLeft).toBe(4);
  });

  it('are accepted while it is on, and still refused from a public peer', async () => {
    harness = await startAuthHarness({ dashboardNetwork: ON });
    const token = String((await pairTray(harness)).body.token);
    atPeer(harness, PUBLIC_PEER);
    const outsider = await signedIn(harness);
    const refused = await sensitiveAuthRoutes(harness, outsider, token);
    for (const response of [refused.change, refused.pair, refused.rotate]) {
      expect([response.status, response.body.code]).toEqual([403, 'secure_transport_required']);
    }
    atPeer(harness, PRIVATE_PEER);
    const browser = await signedIn(harness);
    const { change, pair, rotate } = await sensitiveAuthRoutes(harness, browser, token);
    expect(change.status).toBe(200);
    expect(change.body).toMatchObject({ ok: true });
    expect(pair.status).toBe(201);
    expect(String(pair.body.token)).toMatch(/^aacd_/);
    expect(rotate.status).toBe(200);
    expect(String(rotate.body.token)).toMatch(/^aacd_/);
  });

  it('LAN first-run setup still needs the one-time code, and is refused while off', async () => {
    harness = await startAuthHarness({ mode: 'setup' });
    const code = await prepareFirstRunSetupCode(() => undefined);
    atPeer(harness, PRIVATE_PEER);
    const body = { username: 'jared-admin', password: NEW_PASSWORD };
    const off = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      setupCode: code,
    });
    expect([off.status, off.body.code]).toEqual([403, 'secure_transport_required']);
    await harness.close();

    harness = await startAuthHarness({ mode: 'setup', dashboardNetwork: ON });
    const second = await prepareFirstRunSetupCode(() => undefined);
    atPeer(harness, PRIVATE_PEER);
    const missing = await new Client(harness).send('POST', '/api/auth/setup', body);
    expect([missing.status, missing.body.code]).toEqual([403, 'setup_code_required']);
    const right = await new Client(harness).send('POST', '/api/auth/setup', {
      ...body,
      setupCode: second,
    });
    expect(right.status).toBe(201);
  });
});

describe('GET and PUT /api/auth/network', () => {
  it('turns the trust on only from the dashboard computer, and off from any session', async () => {
    harness = await startAuthHarness();
    atPeer(harness, PRIVATE_PEER);
    const lan = await signedIn(harness);
    expect((await lan.send('GET', '/api/auth/network')).body).toEqual({
      trustLocalNetwork: false,
      trustedNetworks: [...DEFAULT_TRUSTED_NETWORKS],
      connection: { peer: PRIVATE_PEER, trusted: false },
      canTurnOn: false,
    });
    const refused = await lan.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect([refused.status, refused.body.code]).toEqual([403, 'loopback_required']);
    expect(storedNetwork(harness)).toBeUndefined();

    atPeer(harness, '127.0.0.1', null);
    const local = await signedIn(harness);
    const on = await local.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect(on.status).toBe(200);
    expect(on.headers.get('cache-control')).toBe('no-store');
    expect(on.body).toEqual({
      trustLocalNetwork: true,
      trustedNetworks: [...DEFAULT_TRUSTED_NETWORKS],
      connection: { peer: '127.0.0.1', trusted: true },
      canTurnOn: true,
    });
    expect(storedNetwork(harness)).toEqual({ trust_local_network: true });

    atPeer(harness, PRIVATE_PEER);
    expect((await lan.send('GET', '/api/auth/check')).body).toMatchObject({
      secureTransport: true,
      trustedLocalNetwork: true,
      connection: { peer: PRIVATE_PEER, trusted: true },
    });
    const off = await lan.send('PUT', '/api/auth/network', { trustLocalNetwork: false });
    expect(off.status).toBe(200);
    expect(off.body).toMatchObject({
      trustLocalNetwork: false,
      connection: { peer: PRIVATE_PEER, trusted: false },
      canTurnOn: false,
    });
    expect(storedNetwork(harness)).toEqual({ trust_local_network: false });
    const change = await lan.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect([change.status, change.body.code]).toEqual([403, 'secure_transport_required']);
    // Turning it off again changes nothing and still answers.
    const again = await lan.send('PUT', '/api/auth/network', { trustLocalNetwork: false });
    expect(again.status).toBe(200);
  });

  it('does not let a local proxy on loopback turn it on for someone else', async () => {
    harness = await startAuthHarness();
    atPeer(harness, '127.0.0.1', null);
    const local = await signedIn(harness);
    harness.peer.headers = { 'x-forwarded-for': PUBLIC_PEER };
    const proxied = await local.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect([proxied.status, proxied.body.code]).toEqual([403, 'loopback_required']);
    expect(storedNetwork(harness)).toBeUndefined();
  });

  it('needs a browser session, the dashboard origin, JSON and exactly a boolean', async () => {
    harness = await startAuthHarness();
    const anonymous = new Client(harness);
    const read = await anonymous.send('GET', '/api/auth/network');
    expect([read.status, read.body.code]).toEqual([401, 'auth_required']);
    const write = await anonymous.send('PUT', '/api/auth/network', { trustLocalNetwork: false });
    expect([write.status, write.body.code]).toEqual([401, 'auth_required']);

    const token = String((await pairTray(harness)).body.token);
    const tray = new Client(harness, token);
    for (const response of [
      await tray.send('GET', '/api/auth/network'),
      await tray.send('PUT', '/api/auth/network', { trustLocalNetwork: false }),
    ]) {
      expect([response.status, response.body.code]).toEqual([403, 'device_scope']);
    }

    const browser = await signedIn(harness);
    const cases: Array<[unknown, Record<string, string>, number, string]> = [
      [{ trustLocalNetwork: true }, { origin: '' }, 403, 'origin_required'],
      [{ trustLocalNetwork: true }, { origin: 'http://evil.example' }, 403, 'origin_required'],
      ['trustLocalNetwork=true', { 'content-type': 'text/plain' }, 415, 'json_required'],
      [{ trustLocalNetwork: 'true' }, {}, 400, 'invalid_body'],
      [{ trustLocalNetwork: true, trustedNetworks: ['0.0.0.0/0'] }, {}, 400, 'invalid_body'],
      [{}, {}, 400, 'invalid_body'],
    ];
    for (const [body, headers, status, code] of cases) {
      const response = await browser.send('PUT', '/api/auth/network', body, headers);
      expect([status, code, response.status, response.body.code]).toEqual([
        status,
        code,
        status,
        code,
      ]);
    }
    const query = await browser.send('PUT', '/api/auth/network?on=1', { trustLocalNetwork: true });
    expect([query.status, query.body.code]).toEqual([400, 'unexpected_query']);
    const readQuery = await browser.send('GET', '/api/auth/network?x=1');
    expect([readQuery.status, readQuery.body.code]).toEqual([400, 'unexpected_query']);
    expect(storedNetwork(harness)).toBeUndefined();
  });

  it('answers 409 auth_not_configured while dashboard sign-in is off', async () => {
    harness = await startAuthHarness({ mode: 'off' });
    const local = new Client(harness);
    const read = await local.send('GET', '/api/auth/network');
    expect([read.status, read.body.code]).toEqual([409, 'auth_not_configured']);
    const write = await local.send('PUT', '/api/auth/network', { trustLocalNetwork: true });
    expect([write.status, write.body.code]).toEqual([409, 'auth_not_configured']);
  });

  it('keeps trusted_networks and the rest of config.yaml, and a password change keeps the block', async () => {
    const ranges = ['10.6.0.0/24', '100.64.0.0/10'];
    harness = await startAuthHarness({
      dashboardNetwork: { trust_local_network: true, trusted_networks: ranges },
    });
    atPeer(harness, '100.100.1.2');
    expect((await new Client(harness).send('GET', '/api/auth/check')).body).toMatchObject({
      trustedLocalNetwork: true,
      connection: { peer: '100.100.1.2', trusted: true },
    });
    atPeer(harness, PRIVATE_PEER);
    expect((await new Client(harness).send('GET', '/api/auth/check')).body).toMatchObject({
      connection: { peer: PRIVATE_PEER, trusted: false },
    });

    atPeer(harness, '100.100.1.2');
    const vpn = await signedIn(harness);
    expect((await vpn.send('GET', '/api/auth/network')).body).toMatchObject({
      trustedNetworks: ranges,
    });
    const off = await vpn.send('PUT', '/api/auth/network', { trustLocalNetwork: false });
    expect(off.status).toBe(200);
    expect(storedNetwork(harness)).toEqual({
      trust_local_network: false,
      trusted_networks: ranges,
    });
    const authBefore = loadOrCreateUnifiedConfig().dashboard_auth;
    expect(authBefore?.username).toBeTruthy();

    atPeer(harness, '127.0.0.1', null);
    const local = await signedIn(harness);
    const change = await local.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(change.status).toBe(200);
    expect(storedNetwork(harness)).toEqual({
      trust_local_network: false,
      trusted_networks: ranges,
    });
    expect(loadOrCreateUnifiedConfig().dashboard_auth?.username).toBe(authBefore?.username);
  });
});
