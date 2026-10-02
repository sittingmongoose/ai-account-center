/**
 * PUT /api/accounts/visibility through the real startServer() stack: session,
 * origin and JSON guards, the private file under the temporary CCS dir, the
 * accounts-changed hint on /ws, and the tray visibility fields in what a
 * device-scope dashboard read receives. Temporary CCS_HOME only; background
 * collectors stubbed.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import fs from 'fs';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import { WebSocket } from 'ws';
import { startServer } from '../../../src/web-server';
import { loginRateLimiter } from '../../../src/web-server/middleware/auth-middleware';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';

const ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_SESSION_SECRET',
] as const;
const USERNAME = 'visibility-test-admin';
const PASSWORD = 'isolated-visibility-test-password';

let passwordHash = '';
let saved: Record<string, string | undefined> = {};
let home = '';
let staticDir = '';
let instance: Awaited<ReturnType<typeof startServer>> | undefined;
let base = '';
const sockets: WebSocket[] = [];

beforeAll(async () => {
  passwordHash = await bcrypt.hash(PASSWORD, 4);
});

beforeEach(async () => {
  saved = Object.fromEntries(ENVIRONMENT.map((name) => [name, process.env[name]]));
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-visibility-server-'));
  staticDir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-visibility-server-ui-'));
  fs.writeFileSync(path.join(staticDir, 'index.html'), '<canvas id="slint-dashboard"></canvas>');
  process.env.CCS_HOME = home;
  process.env.CCS_DIR = path.join(home, '.ccs');
  process.env.CODEX_HOME = path.join(home, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(home, '.claude');
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.CCS_DASHBOARD_USERNAME = USERNAME;
  process.env.CCS_DASHBOARD_PASSWORD_HASH = passwordHash;
  process.env.CCS_SESSION_SECRET = 'isolated-visibility-server-session-secret';
  for (const key of ['127.0.0.1', '::ffff:127.0.0.1', '::1']) loginRateLimiter.resetKey(key);
  spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {});
  spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {});
  instance = await startServer({ port: 0, host: '127.0.0.1', staticDir });
  base = `http://127.0.0.1:${(instance.server.address() as AddressInfo).port}`;
});

afterEach(async () => {
  try {
    for (const socket of sockets.splice(0)) socket.terminate();
    if (instance) {
      for (const client of instance.wss.clients) client.terminate();
      instance.cleanup();
      const server = instance.server;
      const closed = new Promise<void>((resolve) => server.close(() => resolve()));
      server.closeAllConnections();
      // Under bun, a socket that was upgraded to /ws keeps close() pending after
      // both ends terminated; bound the wait instead of failing the hook.
      await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 500))]);
    }
  } finally {
    instance = undefined;
    mock.restore();
    for (const name of ENVIRONMENT) {
      if (saved[name] === undefined) delete process.env[name];
      else process.env[name] = saved[name];
    }
    fs.rmSync(home, { recursive: true, force: true });
    fs.rmSync(staticDir, { recursive: true, force: true });
  }
});

async function signIn(): Promise<string> {
  const response = await fetch(`${base}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USERNAME, password: PASSWORD }),
  });
  expect(response.status).toBe(200);
  return (response.headers.get('set-cookie') as string).split(';')[0];
}

async function openSocket(cookie: string): Promise<WebSocket> {
  const socket = new WebSocket(`${base.replace('http', 'ws')}/ws`, {
    headers: { Cookie: cookie, Origin: base },
  });
  sockets.push(socket);
  await new Promise<void>((resolve, reject) => {
    socket.once('open', () => resolve());
    socket.once('error', reject);
  });
  return socket;
}

describe('account visibility on the real server', () => {
  it('saves the change in the CCS dir and pushes accounts-changed to signed-in /ws clients', async () => {
    const cookie = await signIn();
    const socket = await openSocket(cookie);
    const message = new Promise<string>((resolve) =>
      socket.once('message', (data) => resolve(String(data)))
    );
    const body = { hiddenProviders: ['kimi-code'], hiddenAccountIds: ['codex:lexxmariah'] };
    const saved = { ...body, trayHiddenProviders: [] };
    const response = await fetch(`${base}/api/accounts/visibility`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Origin: base, Cookie: cookie },
      body: JSON.stringify(body),
    });
    expect(response.status).toBe(200);
    // The old two-key body is a partial update: the answer carries the untouched tray list.
    expect(await response.json()).toEqual(saved);
    expect(await message).toBe('{"type":"accounts-changed"}');
    const file = path.join(home, '.ccs', 'account-visibility.json');
    expect(fs.statSync(file).mode & 0o777).toBe(0o600);
    const read = await fetch(`${base}/api/accounts/visibility`, { headers: { Cookie: cookie } });
    expect(await read.json()).toEqual(saved);
  });

  it('gives a device-scope dashboard read the tray visibility fields', async () => {
    const cookie = await signIn();
    // Save the tray list alone; the other lists stay as they are.
    const put = await fetch(`${base}/api/accounts/visibility`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Origin: base, Cookie: cookie },
      body: JSON.stringify({ trayHiddenProviders: ['qwen'] }),
    });
    expect(put.status).toBe(200);
    expect(await put.json()).toEqual({
      hiddenProviders: [],
      hiddenAccountIds: [],
      trayHiddenProviders: ['qwen'],
    });
    // Pair a tray and read the dashboard DTO with its device token.
    const pair = await fetch(`${base}/api/auth/devices/pair`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        username: USERNAME,
        password: PASSWORD,
        deviceName: 'fixture-tray',
        platform: 'mac',
      }),
    });
    expect(pair.status).toBe(201);
    const paired = (await pair.json()) as { token: string };
    const dashboard = await fetch(`${base}/api/accounts/dashboard`, {
      headers: { Authorization: `Bearer ${paired.token}` },
    });
    expect(dashboard.status).toBe(200);
    const dto = (await dashboard.json()) as {
      settings?: { hiddenProviders?: string[]; trayHiddenProviders?: string[] };
      providers?: Array<{ id: string; visible: boolean; trayVisible: boolean }>;
    };
    expect(dto.settings?.trayHiddenProviders).toEqual(['qwen']);
    expect(dto.settings?.hiddenProviders).toEqual([]);
    const byId = new Map((dto.providers ?? []).map((entry) => [entry.id, entry]));
    expect(byId.get('qwen')).toMatchObject({ visible: true, trayVisible: false });
    expect(byId.get('zai')).toMatchObject({ visible: true, trayVisible: true });
    expect(
      (dto.providers ?? []).every(
        (entry) => typeof entry.visible === 'boolean' && typeof entry.trayVisible === 'boolean'
      )
    ).toBe(true);
  }, 20_000);

  // The 415 case is in account-visibility.test.ts: under bun, an unread text/plain
  // body on this stack keeps the test server from closing.
  it('keeps the guards in front of the store', async () => {
    const cookie = await signIn();
    const valid = JSON.stringify({ hiddenProviders: [], hiddenAccountIds: [] });
    const put = (headers: Record<string, string>, route = '/api/accounts/visibility') =>
      fetch(`${base}${route}`, { method: 'PUT', headers, body: valid });
    const cases: Array<[Response, number, string]> = [
      [await put({ 'Content-Type': 'application/json', Origin: base }), 401, 'auth_required'],
      // With dashboard auth on, the global guard answers first; it carries the same code.
      [await fetch(`${base}/api/accounts/visibility`), 401, 'auth_required'],
      [
        await put({
          'Content-Type': 'application/json',
          Origin: 'https://other.example',
          Cookie: cookie,
        }),
        403,
        'origin_required',
      ],
      [
        await put(
          { 'Content-Type': 'application/json', Origin: base, Cookie: cookie },
          '/api/accounts/visibility?x=1'
        ),
        400,
        'unexpected_query',
      ],
      [
        await put({ 'Content-Type': 'application/json', Origin: base }, '/API/accounts/visibility'),
        401,
        'auth_required',
      ],
    ];
    for (const [response, status, code] of cases) {
      expect(response.status).toBe(status);
      expect((await response.json()).code).toBe(code);
      expect(response.headers.get('cache-control')).toBe('no-store');
    }
    // Letter-case variants never reach the handler, even with a session.
    for (const route of [
      '/API/accounts/visibility',
      '/api/Accounts/visibility',
      '/api/accounts/Visibility',
    ]) {
      const response = await put(
        { 'Content-Type': 'application/json', Origin: base, Cookie: cookie },
        route
      );
      expect(response.status).toBe(404);
      expect(await response.json()).toEqual({ error: 'API endpoint was not found.' });
    }
    expect(fs.existsSync(path.join(home, '.ccs', 'account-visibility.json'))).toBe(false);
  });
});
