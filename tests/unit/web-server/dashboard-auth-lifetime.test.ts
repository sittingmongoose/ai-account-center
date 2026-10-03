/**
 * The session lifetime setting (Settings): 1, 7, 30 (default), 90 or 365
 * days. The session cookie and the idle expiry follow it; "Remember me" off
 * gives a browser-session cookie. Every test runs in a temporary CCS_HOME
 * with bcrypt at cost 4.
 */
import { afterEach, describe, expect, it } from 'bun:test';
import * as fs from 'fs';
import yaml from 'js-yaml';

import { loadOrCreateUnifiedConfig } from '../../../src/config/config-loader-facade';
import { effectiveSessionLifetimeDays } from '../../../src/config/schemas/auth';
import {
  Client,
  configYaml,
  PASSWORD,
  startAuthHarness,
  USERNAME,
  type Harness,
} from './dashboard-auth-harness';

let harness: Harness | null = null;

afterEach(async () => {
  await harness?.close();
  harness = null;
});

async function signedIn(h: Harness): Promise<Client> {
  const client = new Client(h);
  const response = await client.login();
  expect(response.status).toBe(200);
  return client;
}

function setCookieExpires(headers: Headers): string | null {
  const values =
    typeof headers.getSetCookie === 'function'
      ? headers.getSetCookie()
      : [headers.get('set-cookie') ?? ''];
  const sid = values.find((value) => /(?:^|,\s*)connect\.sid=/.test(value)) ?? '';
  const match = /Expires=([^;]*)/i.exec(sid);
  return match ? match[1] : null;
}

describe('session lifetime', () => {
  it('defaults to 30 days in the session and setup answers', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const session = await browser.send('GET', '/api/auth/session');
    expect(session.status).toBe(200);
    expect(session.body).toMatchObject({ sessionLifetimeDays: 30, sessionTimeoutHours: 720 });
    const setup = await browser.send('GET', '/api/auth/setup');
    expect(setup.status).toBe(200);
    expect(setup.body).toMatchObject({ sessionLifetimeDays: 30, sessionTimeoutHours: 720 });
  });

  it('maps a legacy session_timeout_hours to the nearest offered lifetime', () => {
    expect(effectiveSessionLifetimeDays({ session_timeout_hours: 24 })).toBe(30);
    expect(effectiveSessionLifetimeDays({ session_timeout_hours: 48 })).toBe(1);
    expect(effectiveSessionLifetimeDays({ session_timeout_hours: 168 })).toBe(7);
    expect(effectiveSessionLifetimeDays({ session_timeout_hours: 720 })).toBe(30);
    expect(effectiveSessionLifetimeDays({ session_timeout_hours: 2000 })).toBe(90);
    expect(effectiveSessionLifetimeDays({ session_lifetime_days: 7 })).toBe(7);
    expect(effectiveSessionLifetimeDays({})).toBe(30);
  });

  it('saves the lifetime from any signed-in browser and persists both fields', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const saved = await browser.send('PUT', '/api/auth/session-lifetime', { days: 7 });
    expect(saved.status).toBe(200);
    expect(saved.body).toMatchObject({ days: 7, hours: 168 });
    expect(loadOrCreateUnifiedConfig().dashboard_auth).toMatchObject({
      session_lifetime_days: 7,
      session_timeout_hours: 168,
    });
    const file = yaml.load(fs.readFileSync(configYaml(harness), 'utf8')) as {
      dashboard_auth: Record<string, unknown>;
    };
    expect(file.dashboard_auth).toMatchObject({
      session_lifetime_days: 7,
      session_timeout_hours: 168,
    });
    const read = await browser.send('GET', '/api/auth/session-lifetime');
    expect(read.status).toBe(200);
    expect(read.body).toMatchObject({ days: 7, hours: 168 });
  });

  it('refuses lifetimes outside the offered five', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    for (const days of [0, 2, 31, 3650, '30', null]) {
      const response = await browser.send('PUT', '/api/auth/session-lifetime', { days });
      expect(response.status).toBe(400);
    }
    const file = yaml.load(fs.readFileSync(configYaml(harness), 'utf8')) as {
      dashboard_auth: Record<string, unknown>;
    };
    expect('session_lifetime_days' in file.dashboard_auth).toBe(false);
  });

  it('needs a signed-in browser session', async () => {
    harness = await startAuthHarness();
    const anonymous = new Client(harness);
    expect((await anonymous.send('GET', '/api/auth/session-lifetime')).status).toBe(401);
    expect((await anonymous.send('PUT', '/api/auth/session-lifetime', { days: 7 })).status).toBe(
      401
    );
  });

  it('sets the login cookie from the lifetime, about 30 days out by default', async () => {
    harness = await startAuthHarness();
    const browser = new Client(harness);
    const response = await browser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
    });
    expect(response.status).toBe(200);
    const expires = setCookieExpires(response.headers);
    expect(expires).toBeTruthy();
    const ms = Date.parse(expires ?? '') - Date.now();
    expect(ms).toBeGreaterThan(29 * 24 * 3600 * 1000);
    expect(ms).toBeLessThanOrEqual(30 * 24 * 3600 * 1000);
  });

  it('gives a browser-session cookie when "Remember me" is off', async () => {
    harness = await startAuthHarness();
    const browser = new Client(harness);
    const response = await browser.send('POST', '/api/auth/login', {
      username: USERNAME,
      password: PASSWORD,
      rememberMe: false,
    });
    expect(response.status).toBe(200);
    expect(browser.cookie).toContain('connect.sid=');
    expect(setCookieExpires(response.headers)).toBeNull();
    // The session itself still works.
    const check = await browser.send('GET', '/api/auth/check');
    expect(check.status).toBe(200);
    expect(check.body).toMatchObject({ authenticated: true });
  });

  it('restarts the expiry countdown on use (idle expiry)', async () => {
    harness = await startAuthHarness();
    const browser = await signedIn(harness);
    const first = Date.parse(setCookieExpires((await browser.send('GET', '/api/auth/check')).headers) ?? '');
    expect(Number.isFinite(first)).toBe(true);
    const second = await browser.send('GET', '/api/auth/check');
    expect(setCookieExpires(second.headers)).toBeTruthy();
    expect(Date.parse(setCookieExpires(second.headers) ?? '')).toBeGreaterThanOrEqual(first);
  });
});
