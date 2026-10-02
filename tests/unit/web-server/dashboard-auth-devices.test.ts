/**
 * CONTRACT-auth-devices section 12, tests 6-9, 14 and 15: pairing, the bearer
 * scope, device management, rotation and the 90-day idle expiry, plus the
 * Antigravity confirmation bound to the caller. Every test runs in a temporary
 * CCS_HOME with bcrypt at cost 4 and an injected clock where time matters; the
 * services behind the tray routes are stubbed.
 */
import { afterEach, beforeEach, describe, expect, it, spyOn } from 'bun:test';
import express from 'express';
import * as fs from 'fs';
import * as path from 'path';

import { ANTIGRAVITY_AUTO_SWITCH_MESSAGES } from '../../../src/antigravity/auto-switch/monitor';
import type { ActivationResult } from '../../../src/antigravity/types';
import * as codexActivation from '../../../src/codex-auth/activate-codex-profile';
import { isDashboardWebSocketOriginAllowed } from '../../../src/web-server/middleware/auth-middleware';
import { createAntigravityRouter } from '../../../src/web-server/routes/antigravity-routes';
import * as accountDashboard from '../../../src/web-server/services/account-dashboard-service';
import * as claudeOpen from '../../../src/web-server/services/claude-desktop-open-service';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import {
  setAuthClockForTests,
  settleAuthWrites,
} from '../../../src/web-server/services/dashboard-auth-files';
import {
  DAY_MS,
  hashDeviceToken,
  resetDeviceStoreForTests,
} from '../../../src/web-server/services/dashboard-device-store';
import {
  Client,
  pairTray,
  PASSWORD,
  readDevicesFile,
  startAuthHarness,
  type Harness,
} from './dashboard-auth-harness';

const AG_TOKEN = 'FixtureConfirmation_1234567890';
const SAMPLE_TIME = '2026-10-01T17:00:00.000Z';
let harness: Harness | null = null;
let activationResult: ActivationResult;
let stubs: Array<{ mockRestore: () => void }> = [];

function antigravityRouter(app: express.Express): void {
  app.use(
    '/api/antigravity',
    createAntigravityRouter(
      {
        getInventory: async () => ({ schemaVersion: 1, hostId: 'ubuntu', profiles: [] }),
        getAccounts: async () => [],
        activate: async () => activationResult,
        getAutoSwitchStatus: () => ({
          enabled: false,
          thresholdUsedPercent: 95,
          pollIntervalSeconds: 60,
          maxQuotaAgeSeconds: 300,
          cooldownSeconds: 300,
          selectedHostIds: ['ubuntu'],
          requestedPoolId: null,
          outcome: 'disabled',
          message: ANTIGRAVITY_AUTO_SWITCH_MESSAGES.disabled,
          activationInProgress: false,
          lastCheckedAt: SAMPLE_TIME,
          lastSwitchedAt: null,
          lastProfileId: null,
          lastHostId: null,
        }),
        updateAutoSwitchSettings: () =>
          ({
            enabled: true,
            thresholdUsedPercent: 95,
            pollIntervalSeconds: 60,
            maxQuotaAgeSeconds: 300,
            cooldownSeconds: 300,
            selectedHostIds: ['ubuntu'],
            requestedPoolId: null,
            outcome: 'disabled',
            activationInProgress: false,
            lastCheckedAt: SAMPLE_TIME,
            lastSwitchedAt: null,
            lastProfileId: null,
            lastHostId: null,
          }) as never,
      },
      isDashboardWebSocketOriginAllowed
    )
  );
}

function busy(): ActivationResult {
  return {
    status: 'confirmation-required',
    profileId: 'party',
    hostId: 'ubuntu',
    email: 'party@example.com',
    reason: 'running-processes',
    confirmation: {
      token: AG_TOKEN,
      expiresAt: '2099-10-01T13:00:00-04:00',
      profileId: 'party',
      hostId: 'ubuntu',
      email: 'party@example.com',
      warning: 'fixture',
      processes: [{ pid: 12, role: 'cli', label: 'Antigravity CLI' }],
    },
  } as ActivationResult;
}

beforeEach(() => {
  activationResult = {
    status: 'active',
    profileId: 'party',
    hostId: 'ubuntu',
    email: 'party@example.com',
  } as ActivationResult;
  stubs = [
    spyOn(accountDashboard, 'getAccountDashboard').mockResolvedValue({ stub: true } as never),
    spyOn(claudeProfiles, 'listClaudeDesktopProfileMetadata').mockResolvedValue([] as never),
    spyOn(claudeOpen, 'claudeOpenUsesManagedHistory').mockResolvedValue(false as never),
    spyOn(claudeOpen, 'openClaudeDesktopProfile').mockResolvedValue(undefined as never),
    spyOn(codexActivation, 'activateCodexProfile').mockResolvedValue({
      name: 'work',
      email: 'work@example.test',
      plan: 'plus',
      codexHome: '/fixture/.codex',
      previousEmail: null,
    } as never),
    spyOn(CodexAutoSwitchService.prototype, 'updateSettings').mockReturnValue({
      stub: true,
    } as never),
  ];
});

afterEach(async () => {
  for (const stub of stubs) stub.mockRestore();
  stubs = [];
  await harness?.close();
  harness = null;
});

async function signedIn(h: Harness): Promise<Client> {
  const client = new Client(h);
  expect((await client.login()).status).toBe(200);
  return client;
}

async function pairedTray(
  h: Harness,
  overrides: Record<string, unknown> = {}
): Promise<{ client: Client; token: string; id: string }> {
  const response = await pairTray(h, overrides);
  expect(response.status).toBe(201);
  return {
    client: new Client(h, String(response.body.token)),
    token: String(response.body.token),
    id: String(response.body.deviceId),
  };
}

describe('POST /api/auth/devices/pair', () => {
  it('returns the token once and stores only its SHA-256 in a 0600 file', async () => {
    harness = await startAuthHarness();
    const response = await pairTray(harness, {
      installId: '6f1c0f0e-2d4b-4c43-9a8e-1f2a3b4c5d6e',
      appVersion: '2.4.2',
    });
    expect(response.status).toBe(201);
    const token = String(response.body.token);
    expect(token).toMatch(/^aacd_[A-Za-z0-9_-]{43}$/);
    expect(token.length).toBe(48);
    expect(response.body).toMatchObject({
      deviceId: expect.stringMatching(/^dev_[0-9a-f]{16}$/),
      name: 'fixture-mac',
      platform: 'mac',
      pairedAt: expect.any(String),
      rotateAfter: expect.any(String),
    });
    const file = readDevicesFile(harness);
    expect(file.mode).toBe(0o600);
    expect(fs.statSync(path.join(harness.ccsDir, 'auth')).mode & 0o777).toBe(0o700);
    expect(file.text).toContain(hashDeviceToken(token));
    expect(file.text).not.toContain(token);
    expect(file.text).not.toContain(PASSWORD);
    // Shown once: nothing else ever returns it.
    const browser = await signedIn(harness);
    const list = await browser.send('GET', '/api/auth/devices');
    expect(list.text).not.toContain(token);
  });

  it('spends the login budget on wrong passwords; the sixth try is 429', async () => {
    harness = await startAuthHarness();
    const left: unknown[] = [];
    for (let attempt = 0; attempt < 5; attempt += 1) {
      const response = await pairTray(harness, { password: `wrong-${attempt}` });
      expect([response.status, response.body.code]).toEqual([401, 'invalid_credentials']);
      left.push(response.body.triesLeft);
    }
    expect(left).toEqual([4, 3, 2, 1, 0]);
    const sixth = await pairTray(harness);
    expect(sixth.status).toBe(429);
    expect(sixth.body.code).toBe('rate_limited');
    // Pairing and login share the key and the budget.
    expect((await new Client(harness).login()).status).toBe(429);
  });

  it('pairing again with the same installId revokes the old token', async () => {
    harness = await startAuthHarness();
    const installId = '6f1c0f0e-2d4b-4c43-9a8e-1f2a3b4c5d6e';
    const first = await pairedTray(harness, { installId });
    const second = await pairedTray(harness, { installId });
    expect(second.id).not.toBe(first.id);
    const old = await first.client.send('GET', '/api/auth/devices/me');
    expect([old.status, old.body.code]).toEqual([401, 'device_revoked']);
    expect((await second.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
    const browser = await signedIn(harness);
    const list = await browser.send('GET', '/api/auth/devices');
    expect((list.body.devices as unknown[]).length).toBe(1);
  });

  it('caps active devices at 20', async () => {
    harness = await startAuthHarness();
    for (let index = 0; index < 20; index += 1) {
      expect((await pairTray(harness, { deviceName: `tray-${index}` })).status).toBe(201);
    }
    const extra = await pairTray(harness, { deviceName: 'tray-21' });
    expect([extra.status, extra.body.code]).toEqual([409, 'too_many_devices']);
  });

  it('validates the body strictly and accepts a native client without Origin', async () => {
    harness = await startAuthHarness();
    for (const overrides of [
      { platform: 'linux' },
      { deviceName: '' },
      { deviceName: 'x'.repeat(65) },
      { deviceName: 'bad\u0007name' },
      { installId: 'not-a-uuid' },
      { appVersion: 'x'.repeat(33) },
      { extra: true },
    ]) {
      const response = await pairTray(harness, overrides);
      expect([JSON.stringify(overrides), response.status, response.body.code]).toEqual([
        JSON.stringify(overrides),
        400,
        'invalid_body',
      ]);
    }
    const foreign = await new Client(harness).send(
      'POST',
      '/api/auth/devices/pair',
      { username: 'aac-test-admin', password: PASSWORD, deviceName: 'm', platform: 'mac' },
      { origin: 'https://evil.example' }
    );
    expect([foreign.status, foreign.body.code]).toEqual([403, 'origin_required']);
    const trimmed = await pairTray(harness, { deviceName: '  Jared’s Mac  ' });
    expect(trimmed.body.name).toBe('Jared’s Mac');
  });
});

describe('Bearer device tokens', () => {
  it('reach every tray route on the allowlist', async () => {
    harness = await startAuthHarness({ before: antigravityRouter });
    const { client } = await pairedTray(harness);
    const calls: Array<[string, string, unknown?]> = [
      ['GET', '/api/accounts/dashboard?platform=mac&refresh=false'],
      ['POST', '/api/codex/profiles/work/activate', {}],
      ['PUT', '/api/codex/profiles/auto-switch', { enabled: false }],
      ['POST', '/api/claude/desktop-profiles/work/open', { platform: 'mac' }],
      ['GET', '/api/claude/desktop-profiles'],
      ['GET', '/api/antigravity/profiles'],
      ['GET', '/api/antigravity/auto-switch'],
      ['PUT', '/api/antigravity/auto-switch', { enabled: true }],
      ['POST', '/api/antigravity/profiles/party/activate', { hostId: 'ubuntu' }],
      [
        'POST',
        '/api/antigravity/profiles/party/confirm',
        { hostId: 'ubuntu', confirmationToken: AG_TOKEN },
      ],
      ['GET', '/api/bar/auth'],
      ['GET', '/api/auth/devices/me'],
      ['POST', '/api/auth/devices/me/rotate'],
    ];
    for (const [method, route, body] of calls) {
      const response = await client.send(method, route, body);
      expect([method, route, response.status >= 200 && response.status < 300]).toEqual([
        method,
        route,
        true,
      ]);
    }
    const rotated = await client.send('POST', '/api/auth/devices/me/rotate');
    const fresh = new Client(harness, String(rotated.body.token));
    expect((await fresh.send('DELETE', '/api/auth/devices/me')).status).toBe(204);
  });

  it('get 403 device_scope everywhere else', async () => {
    harness = await startAuthHarness();
    const { client } = await pairedTray(harness);
    for (const [method, route, body] of [
      ['POST', '/api/auth/password', { currentPassword: PASSWORD, newPassword: 'another-one-1' }],
      ['GET', '/api/auth/devices'],
      ['GET', '/api/auth/session'],
      ['POST', '/api/auth/devices/revoke-all', {}],
      ['PUT', '/api/accounts/settings', { refreshIntervalSeconds: 60 }],
      ['GET', '/api/accounts/settings'],
      ['GET', '/api/app-updates/status'],
      ['GET', '/api/accounts/analytics'],
      ['GET', '/api/accounts/registry'],
      ['GET', '/api/codex/profiles'],
      ['GET', '/api/codex/profiles/auto-switch'],
      ['GET', '/API/accounts/dashboard'],
    ] as Array<[string, string, unknown?]>) {
      const response = await client.send(method, route, body);
      expect([method, route, response.status, response.body.code]).toEqual([
        method,
        route,
        403,
        'device_scope',
      ]);
    }
  });

  it('refuse malformed headers, tokens in the URL, and never fall back to the cookie', async () => {
    harness = await startAuthHarness();
    const { token } = await pairedTray(harness);
    const browser = await signedIn(harness);
    for (const header of ['Bearer', 'Bearer not-a-token', `Bearer ${token}x`, 'bearer  ']) {
      const response = await browser.send('GET', '/api/accounts/dashboard', undefined, {
        authorization: header,
      });
      expect([header, response.status, response.body.code]).toEqual([header, 401, 'invalid_token']);
    }
    const query = await new Client(harness).send(
      'GET',
      `/api/accounts/dashboard?platform=mac&token=${token}`
    );
    expect([query.status, query.body.code]).toEqual([400, 'token_in_query']);
    const inBody = await browser.send('PUT', '/api/accounts/settings', { deviceToken: token });
    expect([inBody.status, inBody.body.code]).toEqual([400, 'token_in_query']);
    // The cookie alone still works.
    expect((await browser.send('GET', '/api/accounts/dashboard')).status).toBe(200);
  });

  it('are ignored when dashboard auth is off', async () => {
    harness = await startAuthHarness({ mode: 'off' });
    const bogus = new Client(harness, 'aacd_' + 'A'.repeat(43));
    // Localhost reads work as today; the header changes nothing.
    expect((await bogus.send('GET', '/api/bar/auth')).status).toBe(200);
    const withHeader = await bogus.send('GET', '/api/accounts/dashboard');
    const without = await new Client(harness).send('GET', '/api/accounts/dashboard');
    expect([withHeader.status, withHeader.body]).toEqual([without.status, without.body]);
    const pair = await pairTray(harness);
    expect([pair.status, pair.body.code]).toEqual([409, 'auth_not_configured']);
  });
});

describe('device management', () => {
  it('lists devices without hashes and revokes one at once', async () => {
    harness = await startAuthHarness();
    const mac = await pairedTray(harness);
    const windows = await pairedTray(harness, { deviceName: 'fixture-pc', platform: 'windows' });
    const browser = await signedIn(harness);
    const list = await browser.send('GET', '/api/auth/devices');
    expect(list.status).toBe(200);
    expect(list.text).not.toContain('tokenSha256');
    expect(list.text).not.toContain(hashDeviceToken(mac.token));
    expect(list.body.devices).toEqual([
      {
        id: mac.id,
        name: 'fixture-mac',
        platform: 'mac',
        appVersion: null,
        pairedAt: expect.any(String),
        lastSeenAt: null,
        lastSeenAddress: '127.0.0.1',
        rotatedAt: null,
        idleExpiresAt: expect.any(String),
      },
      expect.objectContaining({ id: windows.id, platform: 'windows' }),
    ]);
    const unknown = await browser.send('DELETE', '/api/auth/devices/dev_0000000000000000');
    expect([unknown.status, unknown.body.code]).toEqual([404, 'unknown_device']);
    const noOrigin = await browser.send('DELETE', `/api/auth/devices/${mac.id}`, undefined, {
      origin: '',
    });
    expect(noOrigin.status).toBe(403);
    expect((await browser.send('DELETE', `/api/auth/devices/${mac.id}`)).status).toBe(204);
    const revoked = await mac.client.send('GET', '/api/accounts/dashboard');
    expect([revoked.status, revoked.body.code]).toEqual([401, 'device_revoked']);
    expect((await windows.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
  });

  it('revoke-all revokes every device and keeps this browser signed in', async () => {
    harness = await startAuthHarness();
    const mac = await pairedTray(harness);
    const windows = await pairedTray(harness, { deviceName: 'pc', platform: 'windows' });
    const browser = await signedIn(harness);
    const other = await signedIn(harness);
    const response = await browser.send('POST', '/api/auth/devices/revoke-all', {});
    expect(response.status).toBe(200);
    expect(response.body).toEqual({ revokedDevices: 2, signedOutBrowsers: 1 });
    for (const tray of [mac, windows]) {
      expect((await tray.client.send('GET', '/api/auth/devices/me')).body.code).toBe(
        'device_revoked'
      );
    }
    expect((await browser.send('GET', '/api/auth/devices')).body).toEqual({ devices: [] });
    expect((await other.send('GET', '/api/auth/session')).body.code).toBe('session_revoked');
  });

  it('revoke-all with signOutOtherBrowsers false keeps other browsers', async () => {
    harness = await startAuthHarness();
    await pairedTray(harness);
    const browser = await signedIn(harness);
    const other = await signedIn(harness);
    const response = await browser.send('POST', '/api/auth/devices/revoke-all', {
      signOutOtherBrowsers: false,
    });
    expect(response.body).toEqual({ revokedDevices: 1, signedOutBrowsers: 0 });
    expect((await other.send('GET', '/api/auth/session')).status).toBe(200);
  });

  it('DELETE /me disconnects the tray itself; a browser cannot call it', async () => {
    harness = await startAuthHarness();
    const tray = await pairedTray(harness);
    const browser = await signedIn(harness);
    const fromBrowser = await browser.send('DELETE', '/api/auth/devices/me');
    expect([fromBrowser.status, fromBrowser.body.code]).toEqual([403, 'device_required']);
    expect((await tray.client.send('DELETE', '/api/auth/devices/me')).status).toBe(204);
    expect((await tray.client.send('GET', '/api/auth/devices/me')).body.code).toBe(
      'device_revoked'
    );
  });
});

describe('rotation and idle expiry', () => {
  it('keeps the old token until the new one is used', async () => {
    harness = await startAuthHarness();
    const tray = await pairedTray(harness);
    const rotated = await tray.client.send('POST', '/api/auth/devices/me/rotate');
    expect(rotated.status).toBe(200);
    expect(rotated.body).toEqual({ token: expect.any(String), rotateAfter: expect.any(String) });
    const fresh = new Client(harness, String(rotated.body.token));
    // The old token still works and is told to rotate now.
    const old = await tray.client.send('GET', '/api/auth/devices/me');
    expect(old.status).toBe(200);
    expect(Date.parse(String(old.body.rotateAfter))).toBeLessThanOrEqual(Date.now());
    expect((await fresh.send('GET', '/api/auth/devices/me')).status).toBe(200);
    const after = await tray.client.send('GET', '/api/auth/devices/me');
    expect([after.status, after.body.code]).toEqual([401, 'invalid_token']);
  });

  it('ends the old token 24 hours later if the new one is never used', async () => {
    let now = Date.parse('2026-10-02T12:00:00.000Z');
    harness = await startAuthHarness();
    setAuthClockForTests(() => now);
    const tray = await pairedTray(harness);
    const rotated = await tray.client.send('POST', '/api/auth/devices/me/rotate');
    expect(rotated.status).toBe(200);
    now += DAY_MS - 60_000;
    expect((await tray.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
    now += 120_000;
    expect((await tray.client.send('GET', '/api/auth/devices/me')).body.code).toBe('invalid_token');
    expect(
      (await new Client(harness, String(rotated.body.token)).send('GET', '/api/auth/devices/me'))
        .status
    ).toBe(200);
  });

  it('expires a token unused for 90 days, to the minute', async () => {
    let now = Date.parse('2026-10-02T12:00:00.000Z');
    harness = await startAuthHarness();
    setAuthClockForTests(() => now);
    const fresh = await pairedTray(harness);
    const stale = await pairedTray(harness, { deviceName: 'stale' });
    const neverUsed = await pairedTray(harness, { deviceName: 'never-used' });
    expect((await fresh.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
    expect((await stale.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
    const me = await fresh.client.send('GET', '/api/auth/devices/me');
    expect(me.body.idleExpiresAt).toBe(new Date(now + 90 * DAY_MS).toISOString());
    now += 90 * DAY_MS - 60_000;
    expect((await fresh.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
    now += 120_000;
    const expired = await stale.client.send('GET', '/api/auth/devices/me');
    expect([expired.status, expired.body.code]).toEqual([401, 'device_expired']);
    // A token never used counts from pairedAt.
    const never = await neverUsed.client.send('GET', '/api/accounts/dashboard');
    expect([never.status, never.body.code]).toEqual([401, 'device_expired']);
    // Still expired after a restart (the file is re-read once queued writes land).
    await settleAuthWrites();
    resetDeviceStoreForTests();
    expect((await stale.client.send('GET', '/api/auth/devices/me')).body.code).toBe(
      'device_expired'
    );
    expect((await fresh.client.send('GET', '/api/auth/devices/me')).status).toBe(200);
  });

  it('stamps lastSeenAt at most once a minute', async () => {
    let now = Date.parse('2026-10-02T12:00:00.000Z');
    harness = await startAuthHarness();
    setAuthClockForTests(() => now);
    const tray = await pairedTray(harness);
    await tray.client.send('GET', '/api/auth/devices/me');
    const browser = await signedIn(harness);
    const first = await browser.send('GET', '/api/auth/devices');
    const seen = (first.body.devices as Array<{ lastSeenAt: string }>)[0].lastSeenAt;
    expect(seen).toBe(new Date(now).toISOString());
    now += 30_000;
    await tray.client.send('GET', '/api/auth/devices/me');
    const second = await browser.send('GET', '/api/auth/devices');
    expect((second.body.devices as Array<{ lastSeenAt: string }>)[0].lastSeenAt).toBe(seen);
    now += 31_000;
    await tray.client.send('GET', '/api/auth/devices/me');
    const third = await browser.send('GET', '/api/auth/devices');
    expect((third.body.devices as Array<{ lastSeenAt: string }>)[0].lastSeenAt).toBe(
      new Date(now).toISOString()
    );
  });

  it('refuses a devices.json with group or other permission bits', async () => {
    harness = await startAuthHarness();
    const tray = await pairedTray(harness);
    await settleAuthWrites();
    resetDeviceStoreForTests();
    fs.chmodSync(path.join(harness.ccsDir, 'auth', 'devices.json'), 0o644);
    const response = await tray.client.send('GET', '/api/auth/devices/me');
    expect([response.status, response.body.code]).toEqual([503, 'auth_store_unavailable']);
    const pair = await pairTray(harness);
    expect(pair.status).toBe(503);
    expect(fs.statSync(path.join(harness.ccsDir, 'auth', 'devices.json')).mode & 0o777).toBe(0o644);
  });
});

describe('Antigravity confirmations stay with their caller', () => {
  it('refuses a token issued to device A for device B and for a browser', async () => {
    harness = await startAuthHarness({ before: antigravityRouter });
    const deviceA = await pairedTray(harness);
    const deviceB = await pairedTray(harness, { deviceName: 'other' });
    const browser = await signedIn(harness);
    activationResult = busy();
    const offer = await deviceA.client.send('POST', '/api/antigravity/profiles/party/activate', {
      hostId: 'ubuntu',
    });
    expect(offer.status).toBe(409);
    expect((offer.body.confirmation as { token: string }).token).toBe(AG_TOKEN);
    activationResult = {
      status: 'active',
      profileId: 'party',
      hostId: 'ubuntu',
      email: 'party@example.com',
    } as ActivationResult;
    const confirm = { hostId: 'ubuntu', confirmationToken: AG_TOKEN };
    const fromB = await deviceB.client.send(
      'POST',
      '/api/antigravity/profiles/party/confirm',
      confirm
    );
    expect([fromB.status, fromB.body.status]).toEqual([409, 'stale-confirmation']);
    const fromBrowser = await browser.send(
      'POST',
      '/api/antigravity/profiles/party/confirm',
      confirm
    );
    expect([fromBrowser.status, fromBrowser.body.status]).toEqual([409, 'stale-confirmation']);
    const fromA = await deviceA.client.send(
      'POST',
      '/api/antigravity/profiles/party/confirm',
      confirm
    );
    expect([fromA.status, fromA.body.status]).toEqual([200, 'active']);
  });
});
