/**
 * B4 review fixes for the sign-in limits (CONTRACT-auth-devices sections 3, 5
 * and 10): the password-change limit survives a new session id, a
 * server-wide limit on wrong current passwords, the "sign out other
 * browsers" limit, a server-wide sign-in budget for addresses taken from
 * X-Forwarded-For, and pairing answers after a correct password that spend
 * no login budget. Temporary CCS_HOME, bcrypt cost 4, nothing live.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import * as path from 'path';

import { settleAuthWrites } from '../../../src/web-server/services/dashboard-auth-files';
import { resetDeviceStoreForTests } from '../../../src/web-server/services/dashboard-device-store';
import {
  Client,
  LAN_HOST,
  LAN_PEER,
  pairTray,
  PASSWORD,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const NEW_PASSWORD = 'second-fixture-password';
let harness: Harness | null = null;

afterEach(async () => {
  await harness?.close();
  harness = null;
});

async function signedIn(current: Harness): Promise<Client> {
  const browser = new Client(current);
  expect((await browser.login()).status).toBe(200);
  return browser;
}

function wrongChange(browser: Client, attempt: number) {
  return browser.send('POST', '/api/auth/password', {
    currentPassword: `wrong-guess-${attempt}`,
    newPassword: NEW_PASSWORD,
  });
}

describe('password-change limit', () => {
  it('holds after "sign out other browsers" and "sign out all devices" hand out a new session id', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const tries: unknown[] = [];
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const response = await wrongChange(browser, attempt);
      tries.push([response.status, response.body.code, response.body.triesLeft]);
    }
    expect(tries).toEqual([
      [401, 'wrong_password', 4],
      [401, 'wrong_password', 3],
      [401, 'wrong_password', 2],
      [401, 'wrong_password', 1],
      [401, 'wrong_password', 0],
    ]);
    expect((await wrongChange(browser, 5)).status).toBe(429);

    const before = browser.cookie;
    expect((await browser.send('POST', '/api/auth/sessions/revoke-others', {})).status).toBe(200);
    expect(browser.cookie).not.toBe(before);
    const afterRevoke = await wrongChange(browser, 6);
    expect([afterRevoke.status, afterRevoke.body.code]).toEqual([429, 'rate_limited']);

    const revokeAll = await browser.send('POST', '/api/auth/devices/revoke-all', {
      signOutOtherBrowsers: false,
    });
    expect(revokeAll.status).toBe(200);
    expect((await wrongChange(browser, 7)).status).toBe(429);

    // A second browser from the same address and account shares the budget.
    const second = await signedIn(harness);
    expect((await wrongChange(second, 8)).status).toBe(429);
    // The right password is refused too until the window ends; nothing changed.
    const right = await browser.send('POST', '/api/auth/password', {
      currentPassword: PASSWORD,
      newPassword: NEW_PASSWORD,
    });
    expect(right.status).toBe(429);
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
  });

  it('allows 10 wrong current passwords per hour for the whole server, whatever the address', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    harness.peer.host = LAN_HOST;
    harness.peer.encrypted = true;
    const statuses: number[] = [];
    for (const address of [LAN_PEER, '192.0.2.31']) {
      harness.peer.address = address;
      for (let attempt = 0; attempt < 5; attempt += 1) {
        statuses.push((await wrongChange(browser, attempt)).status);
      }
    }
    expect(statuses).toEqual(Array(10).fill(401));
    harness.peer.address = '192.0.2.32';
    const limited = await wrongChange(browser, 10);
    expect([limited.status, limited.body.code]).toEqual([429, 'rate_limited']);
    const retryAfter = Number(limited.headers.get('retry-after'));
    expect(retryAfter).toBeGreaterThan(15 * 60);
    expect(limited.body.retryAfterSeconds).toBe(retryAfter);
  });
});

describe('"sign out other browsers" limit', () => {
  it('allows 10 per 15 minutes from one address and account, then answers 429', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    for (let index = 0; index < 10; index += 1) {
      expect((await browser.send('POST', '/api/auth/sessions/revoke-others', {})).status).toBe(200);
    }
    const limited = await browser.send('POST', '/api/auth/sessions/revoke-others', {});
    expect([limited.status, limited.body.code]).toEqual([429, 'rate_limited']);
    expect(Number(limited.headers.get('retry-after'))).toBeGreaterThan(0);
    // This browser is still signed in, and a refused request spent nothing.
    expect((await browser.send('GET', '/api/auth/session')).status).toBe(200);
  });
});

describe('server-wide sign-in budget behind a trusted local TLS proxy', () => {
  it('stops a local process that varies X-Forwarded-For, without locking out direct callers', async () => {
    harness = await startAuthHarness({
      dashboardTls: { trusted_proxy: 'tailscale-serve' },
    });
    const statuses: number[] = [];
    for (let index = 0; index < 30; index += 1) {
      harness.peer.headers = { 'x-forwarded-for': `198.51.100.${index + 1}` };
      statuses.push((await new Client(harness).login('wrong-password')).status);
    }
    expect(statuses).toEqual(Array(30).fill(401));

    harness.peer.headers = { 'x-forwarded-for': '198.51.100.200' };
    const limited = await new Client(harness).login(PASSWORD);
    expect([limited.status, limited.body.code]).toEqual([429, 'rate_limited']);
    expect(Number(limited.headers.get('retry-after'))).toBeGreaterThan(15 * 60);
    // Pairing from a forwarded address shares the budget.
    harness.peer.headers = {
      'x-forwarded-for': '198.51.100.201',
      'x-forwarded-proto': 'https',
    };
    const pair = await pairTray(harness);
    expect([pair.status, pair.body.code]).toEqual([429, 'rate_limited']);

    // A caller whose address is its real peer keeps its own budget.
    harness.peer.headers = {};
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
    harness.peer.address = LAN_PEER;
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
  });
});

describe('nobody on the LAN can lock out the person at the VM, LAN HTTPS proxy included', () => {
  const PROXY = '192.168.1.20';
  const LAN_COMPUTER = '192.168.1.21';

  function viaProxy(current: Harness, client: string): void {
    current.peer.address = PROXY;
    current.peer.host = 'aac.example.test';
    current.peer.headers = { 'x-forwarded-for': client, 'x-forwarded-proto': 'https' };
  }

  function direct(current: Harness, address: string): void {
    current.peer.address = address;
    current.peer.host = address === '127.0.0.1' ? null : '192.168.1.10:3000';
    current.peer.headers = {};
  }

  it('keeps loopback and LAN sign-in after the proxy spends its own and the server-wide budgets', async () => {
    harness = await startAuthHarness({
      dashboardTls: {
        trusted_proxy: 'lan-https-proxy',
        trusted_proxy_addresses: [PROXY],
        public_origin: 'https://aac.example.test',
      },
    });
    // Anything on the proxy's computer names the VM's own address and a LAN computer.
    for (const spoofed of ['127.0.0.1', LAN_COMPUTER]) {
      viaProxy(harness, spoofed);
      for (let attempt = 0; attempt < 5; attempt += 1) {
        expect((await new Client(harness).login('wrong-password')).status).toBe(401);
      }
      expect((await new Client(harness).login(PASSWORD)).status).toBe(429);
    }
    // Then varied forwarded clients use up the server-wide budget for proxied sign-ins.
    for (let index = 0; index < 20; index += 1) {
      viaProxy(harness, `198.51.100.${index + 1}`);
      expect((await new Client(harness).login('wrong-password')).status).toBe(401);
    }
    viaProxy(harness, '198.51.100.200');
    const limited = await new Client(harness).login(PASSWORD);
    expect([limited.status, limited.body.code]).toEqual([429, 'rate_limited']);

    // The browser on the VM and a LAN computer keep their full budgets.
    direct(harness, '127.0.0.1');
    const vm = await new Client(harness).login('wrong-password');
    expect([vm.status, vm.body.triesLeft]).toEqual([401, 4]);
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
    direct(harness, LAN_COMPUTER);
    const lan = await new Client(harness).login('wrong-password');
    expect([lan.status, lan.body.triesLeft]).toEqual([401, 4]);
    expect((await new Client(harness).login(PASSWORD)).status).toBe(200);
  });
});

describe('pairing answers after a correct password', () => {
  it('spend no login budget when the device cap is reached', async () => {
    harness = await startAuthHarness();
    for (let index = 0; index < 20; index += 1) {
      expect((await pairTray(harness, { deviceName: `tray-${index}` })).status).toBe(201);
    }
    for (let index = 0; index < 6; index += 1) {
      const extra = await pairTray(harness, { deviceName: `extra-${index}` });
      expect([extra.status, extra.body.code]).toEqual([409, 'too_many_devices']);
    }
    const wrong = await new Client(harness).login('wrong-password');
    expect([wrong.status, wrong.body.triesLeft]).toEqual([401, 4]);
  });

  it('spend no login budget when the device store cannot be trusted', async () => {
    harness = await startAuthHarness();
    expect((await pairTray(harness)).status).toBe(201);
    await settleAuthWrites();
    resetDeviceStoreForTests();
    fs.chmodSync(path.join(harness.ccsDir, 'auth', 'devices.json'), 0o644);
    for (let index = 0; index < 6; index += 1) {
      const pair = await pairTray(harness, { deviceName: `retry-${index}` });
      expect([pair.status, pair.body.code]).toEqual([503, 'auth_store_unavailable']);
    }
    const wrong = await pairTray(harness, { password: 'wrong-password' });
    expect([wrong.status, wrong.body.code, wrong.body.triesLeft]).toEqual([
      401,
      'invalid_credentials',
      4,
    ]);
  });
});
