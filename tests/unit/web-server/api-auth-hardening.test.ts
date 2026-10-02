/**
 * Regression tests for the dashboard API auth hardening (review findings F1 and F2).
 *
 * Every request goes through the real startServer() stack: body parsing,
 * session, the global auth guard, the /api router with its own guard, the
 * domain routers, static files and the fallback. Background collectors and the
 * data services behind each route are stubbed, so no provider, host or
 * private file is ever read.
 */

import { afterEach, beforeAll, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import type { AddressInfo } from 'net';

import { startServer } from '../../../src/web-server';
import { loginRateLimiter } from '../../../src/web-server/middleware/auth-middleware';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';
import * as accountDashboard from '../../../src/web-server/services/account-dashboard-service';
import * as claudeOpen from '../../../src/web-server/services/claude-desktop-open-service';
import * as claudeProfiles from '../../../src/web-server/services/claude-desktop-profile-service';
import * as claudeUsage from '../../../src/web-server/services/claude-desktop-usage-service';
import * as codexActivation from '../../../src/codex-auth/activate-codex-profile';
import * as codexDashboard from '../../../src/codex-auth/codex-auth-dashboard-service';
import * as codexQuotas from '../../../src/web-server/services/codex-profile-quota-service';

const FIXTURE_ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
  'CCS_DASHBOARD_USERNAME',
  'CCS_DASHBOARD_PASSWORD_HASH',
  'CCS_SESSION_SECRET',
] as const;

const USERNAME = 'hardening-test-admin';
const PASSWORD = 'isolated-hardening-test-password';

interface ApiRoute {
  family: string;
  method: 'GET' | 'POST';
  path: string;
  query?: string;
  body?: unknown;
  status: number;
}

/** One or more canonical routes for every router mounted under /api. */
const ROUTES: ApiRoute[] = [
  { family: 'accounts', method: 'GET', path: '/api/accounts/dashboard', status: 200 },
  { family: 'accounts', method: 'GET', path: '/api/accounts/settings', status: 200 },
  { family: 'accounts', method: 'GET', path: '/api/accounts/analytics', status: 200 },
  { family: 'app-updates', method: 'GET', path: '/api/app-updates/status', status: 200 },
  { family: 'claude', method: 'GET', path: '/api/claude/desktop-profiles', status: 200 },
  {
    family: 'claude',
    method: 'GET',
    path: '/api/claude/desktop-profiles/usage',
    query: '?platform=mac',
    status: 200,
  },
  {
    family: 'claude',
    method: 'POST',
    path: '/api/claude/desktop-profiles/work/open',
    body: { platform: 'mac' },
    status: 200,
  },
  { family: 'codex', method: 'GET', path: '/api/codex/profiles', status: 200 },
  { family: 'codex', method: 'GET', path: '/api/codex/profiles/quotas', status: 200 },
  { family: 'codex', method: 'GET', path: '/api/codex/profiles/auto-switch', status: 200 },
  {
    family: 'codex',
    method: 'POST',
    path: '/api/codex/profiles/work/activate',
    body: {},
    status: 200,
  },
  { family: 'antigravity', method: 'GET', path: '/api/antigravity/profiles', status: 200 },
  { family: 'antigravity', method: 'GET', path: '/api/antigravity/profiles/quotas', status: 200 },
  { family: 'antigravity', method: 'GET', path: '/api/antigravity/auto-switch', status: 200 },
  { family: 'bar', method: 'GET', path: '/api/bar/auth', status: 200 },
];

function upperFirst(segment: string): string {
  return segment.charAt(0).toUpperCase() + segment.slice(1);
}

/** Letter-case variants of a canonical path: mount, family segment, and whole path. */
function caseVariants(canonical: string): string[] {
  const [, mount, family, ...rest] = canonical.split('/');
  const tail = rest.length ? `/${rest.join('/')}` : '';
  return [
    `/${mount.toUpperCase()}/${family}${tail}`,
    `/${upperFirst(mount)}/${family}${tail}`,
    `/aPi/${family}${tail}`,
    `/${mount}/${family.toUpperCase()}${tail}`,
    `/${mount}/${upperFirst(family)}${tail}`,
    canonical.toUpperCase(),
  ];
}

const VARIANT_CASES = ROUTES.flatMap((route) =>
  caseVariants(route.path).map((variant) => ({ ...route, variant }))
);

let passwordHash = '';
let originalEnvironment: Record<string, string | undefined> = {};
let tempHome = '';
let staticDir = '';
let instance: Awaited<ReturnType<typeof startServer>> | undefined;
let baseUrl = '';
let processFailures: unknown[] = [];
const recordProcessFailure = (reason: unknown) => processFailures.push(reason);
let serviceStubs: Array<ReturnType<typeof spyOn>> = [];

function resetLoginLimiter(): void {
  for (const key of ['127.0.0.1', '::ffff:127.0.0.1', '::1']) loginRateLimiter.resetKey(key);
}

beforeAll(async () => {
  passwordHash = await bcrypt.hash(PASSWORD, 4);
});

beforeEach(async () => {
  originalEnvironment = Object.fromEntries(
    FIXTURE_ENVIRONMENT.map((name) => [name, process.env[name]])
  );
  tempHome = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-auth-hardening-'));
  staticDir = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-api-auth-hardening-ui-'));
  fs.writeFileSync(path.join(staticDir, 'index.html'), '<canvas id="slint-dashboard"></canvas>');
  fs.writeFileSync(path.join(staticDir, 'dashboard.wasm'), Buffer.from([0, 97, 115, 109]));
  process.env.CCS_HOME = tempHome;
  process.env.CCS_DIR = path.join(tempHome, '.ccs');
  process.env.CODEX_HOME = path.join(tempHome, '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(tempHome, '.claude');
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.CCS_DASHBOARD_USERNAME = USERNAME;
  process.env.CCS_DASHBOARD_PASSWORD_HASH = passwordHash;
  process.env.CCS_SESSION_SECRET = 'isolated-api-auth-hardening-session-secret';
  resetLoginLimiter();

  processFailures = [];
  process.on('unhandledRejection', recordProcessFailure);
  process.on('uncaughtException', recordProcessFailure);

  // Background work started by startServer is unrelated to these checks.
  spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {});
  spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {});
  // Data behind every route is stubbed, so a guard failure shows as a stub call.
  serviceStubs = [
    spyOn(accountDashboard, 'getAccountDashboard').mockResolvedValue({ stub: true } as never),
    spyOn(accountAnalytics, 'getAccountAnalytics').mockResolvedValue({ stub: true } as never),
    spyOn(claudeProfiles, 'listClaudeDesktopProfileMetadata').mockResolvedValue([] as never),
    spyOn(claudeUsage, 'getClaudeDesktopUsage').mockResolvedValue({ stub: true } as never),
    spyOn(claudeOpen, 'openClaudeDesktopProfile').mockResolvedValue(undefined as never),
    spyOn(codexDashboard, 'getCodexAuthProfilesSummary').mockResolvedValue({
      stub: true,
    } as never),
    spyOn(codexQuotas, 'getCodexProfileQuotas').mockResolvedValue({ stub: true } as never),
    spyOn(codexActivation, 'activateCodexProfile').mockResolvedValue({
      name: 'work',
      email: 'work@example.test',
      plan: 'plus',
      codexHome: path.join(tempHome, '.codex'),
      previousEmail: null,
    } as never),
  ];

  const realFetch = globalThis.fetch;
  spyOn(globalThis, 'fetch').mockImplementation((input, init) => {
    const target = new URL(
      typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
    );
    if (!baseUrl || target.origin !== baseUrl)
      return Promise.reject(new Error('Only the owned server fixture may receive requests.'));
    return realFetch(input, init);
  });

  instance = await startServer({ port: 0, host: '127.0.0.1', staticDir });
  baseUrl = `http://127.0.0.1:${(instance.server.address() as AddressInfo).port}`;
});

afterEach(async () => {
  try {
    if (instance) {
      instance.cleanup();
      const server = instance.server;
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  } finally {
    instance = undefined;
    baseUrl = '';
    process.off('unhandledRejection', recordProcessFailure);
    process.off('uncaughtException', recordProcessFailure);
    mock.restore();
    resetLoginLimiter();
    for (const name of FIXTURE_ENVIRONMENT) {
      const value = originalEnvironment[name];
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    fs.rmSync(tempHome, { recursive: true, force: true });
    fs.rmSync(staticDir, { recursive: true, force: true });
  }
});

function send(
  method: string,
  route: string,
  options: { body?: unknown; rawBody?: string; cookie?: string; json?: boolean } = {}
): Promise<Response> {
  const headers: Record<string, string> = {};
  if (options.cookie) headers.Cookie = options.cookie;
  const hasBody = options.body !== undefined || options.rawBody !== undefined;
  if (hasBody && options.json !== false) headers['Content-Type'] = 'application/json';
  return fetch(`${baseUrl}${route}`, {
    method,
    headers,
    redirect: 'manual',
    ...(hasBody
      ? { body: options.rawBody !== undefined ? options.rawBody : JSON.stringify(options.body) }
      : {}),
  });
}

function login(body: unknown, rawBody?: string): Promise<Response> {
  resetLoginLimiter();
  return send('POST', '/api/auth/login', rawBody !== undefined ? { rawBody } : { body });
}

async function signIn(): Promise<string> {
  const response = await login({ username: USERNAME, password: PASSWORD });
  expect(response.status).toBe(200);
  const cookie = response.headers.get('set-cookie');
  expect(cookie).toBeTruthy();
  return (cookie as string).split(';')[0];
}

function stubCalls(): number {
  return serviceStubs.reduce((total, stub) => total + stub.mock.calls.length, 0);
}

async function expectServerUp(): Promise<void> {
  const health = await send('GET', '/api/health');
  expect(health.status).toBe(200);
  expect(await health.json()).toEqual({ status: 'ok' });
  expect(processFailures).toEqual([]);
}

describe('F1: API session guard whatever the URL letter case', () => {
  it.each(VARIANT_CASES)(
    'rejects $method $variant without a session',
    async ({ method, variant, query, body }) => {
      const response = await send(method, `${variant}${query ?? ''}`, { body });
      expect(response.status).toBe(401);
      expect(await response.json()).toEqual({ error: 'Authentication required' });
      expect(stubCalls()).toBe(0);
    }
  );

  it.each(ROUTES)('rejects canonical $method $path without a session', async (route) => {
    const response = await send(route.method, `${route.path}${route.query ?? ''}`, {
      body: route.body,
    });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: 'Authentication required' });
    expect(stubCalls()).toBe(0);
  });

  it('serves every canonical route unchanged with a session', async () => {
    const cookie = await signIn();
    for (const route of ROUTES) {
      const response = await send(route.method, `${route.path}${route.query ?? ''}`, {
        body: route.body,
        cookie,
      });
      expect({ path: route.path, status: response.status }).toEqual({
        path: route.path,
        status: route.status,
      });
    }
    expect(serviceStubs.every((stub) => stub.mock.calls.length === 1)).toBe(true);
  });

  it('answers non-canonical casings with 404 and no data, even with a session', async () => {
    const cookie = await signIn();
    for (const { method, variant, query, body } of VARIANT_CASES) {
      const response = await send(method, `${variant}${query ?? ''}`, { body, cookie });
      expect({ variant, status: response.status }).toEqual({ variant, status: 404 });
    }
    expect(stubCalls()).toBe(0);
  });

  it('keeps localhost access without auth on canonical paths only', async () => {
    process.env.CCS_DASHBOARD_AUTH_ENABLED = 'false';
    const canonical = await send('GET', '/api/codex/profiles');
    expect(canonical.status).toBe(200);
    for (const variant of caseVariants('/api/codex/profiles')) {
      const response = await send('GET', variant);
      expect({ variant, status: response.status }).toEqual({ variant, status: 404 });
    }
    expect(stubCalls()).toBe(1);
  });

  it('keeps the public routes public on their canonical paths', async () => {
    expect((await send('GET', '/api/health')).status).toBe(200);
    expect((await send('GET', '/api/auth/check')).status).toBe(200);
    expect((await send('GET', '/api/auth/setup')).status).toBe(200);
    // A public prefix no longer exempts a longer path from the guard.
    expect((await send('GET', '/api/health/details')).status).toBe(401);
    expect((await send('GET', '/api/auth/check-extra')).status).toBe(401);
  });

  it('keeps the static UI paths and legacy redirects', async () => {
    for (const route of ['/', '/login']) {
      const response = await send('GET', route);
      expect(response.status).toBe(200);
      expect(await response.text()).toContain('slint-dashboard');
    }
    for (const route of ['/codex/accounts', '/LOGIN', '/analytics']) {
      const response = await send('GET', route);
      expect(response.status).toBe(302);
      expect(response.headers.get('location')).toBe('/');
    }
    const wasm = await send('GET', '/dashboard.wasm');
    expect(wasm.status).toBe(200);
    expect(wasm.headers.get('content-type')).toBe('application/wasm');
  });
});

describe('F2: malformed login requests stay inside the request', () => {
  it.each([
    { name: 'an empty object', body: {} },
    { name: 'a missing password', body: { username: USERNAME } },
    { name: 'a missing username', body: { password: PASSWORD } },
    { name: 'a numeric username', body: { username: 42, password: PASSWORD } },
    { name: 'a numeric password', body: { username: USERNAME, password: 42 } },
    { name: 'an object password', body: { username: USERNAME, password: { $gt: '' } } },
    { name: 'an array password', body: { username: USERNAME, password: [PASSWORD] } },
    { name: 'a boolean password', body: { username: USERNAME, password: true } },
    { name: 'a null password', body: { username: USERNAME, password: null } },
    { name: 'an empty username', body: { username: '', password: PASSWORD } },
    { name: 'an empty password', body: { username: USERNAME, password: '' } },
    { name: 'an oversized username', body: { username: 'u'.repeat(257), password: PASSWORD } },
    { name: 'an oversized password', body: { username: USERNAME, password: 'p'.repeat(1025) } },
    { name: 'an array body', body: [USERNAME, PASSWORD] },
  ])('returns 400 for $name and keeps serving', async ({ body }) => {
    const response = await login(body);
    expect(response.status).toBe(400);
    expect(await response.json()).toEqual({ error: 'Username and password required' });
    await expectServerUp();
  });

  it.each([
    { name: 'no body', rawBody: undefined },
    { name: 'a JSON string body', rawBody: '"admin"' },
    { name: 'a JSON null body', rawBody: 'null' },
    { name: 'malformed JSON', rawBody: '{"username":' },
  ])('returns 400 for $name and keeps serving', async ({ rawBody }) => {
    const response =
      rawBody === undefined
        ? (resetLoginLimiter(), await send('POST', '/api/auth/login'))
        : await login(undefined, rawBody);
    expect(response.status).toBe(400);
    await expectServerUp();
  });

  it('treats a username with a different UTF-8 byte length as a failed login', async () => {
    // Same number of UTF-16 code units as the real username, more UTF-8 bytes.
    const response = await login({ username: 'é'.repeat(USERNAME.length), password: PASSWORD });
    expect(response.status).toBe(401);
    expect(await response.json()).toEqual({ error: 'Invalid credentials' });
    await expectServerUp();
  });

  it('turns a bcrypt failure into a 401', async () => {
    const compare = spyOn(bcrypt, 'compare').mockImplementation((() =>
      Promise.reject(new Error('PRIVATE_BCRYPT_FAILURE'))) as never);
    const response = await login({ username: USERNAME, password: PASSWORD });
    expect(compare).toHaveBeenCalledTimes(1);
    expect(response.status).toBe(401);
    const text = await response.text();
    expect(JSON.parse(text)).toEqual({ error: 'Invalid credentials' });
    expect(text).not.toContain('PRIVATE_BCRYPT_FAILURE');
    await expectServerUp();
  });

  it('accepts bounded valid credentials and rejects a wrong password', async () => {
    const wrong = await login({ username: USERNAME, password: `${PASSWORD}-wrong` });
    expect(wrong.status).toBe(401);
    const accepted = await login({ username: USERNAME, password: PASSWORD });
    expect(accepted.status).toBe(200);
    expect(await accepted.json()).toEqual({ success: true, username: USERNAME });
    await expectServerUp();
  });

  it('issues an httpOnly, same-site session cookie with the configured lifetime', async () => {
    const accepted = await login({ username: USERNAME, password: PASSWORD });
    const cookie = accepted.headers.get('set-cookie') ?? '';
    expect(cookie).toContain('HttpOnly');
    expect(cookie).toContain('SameSite=Strict');
    const expires = Date.parse(/Expires=([^;]+)/.exec(cookie)?.[1] ?? '');
    const lifetimeHours = (expires - Date.now()) / 3_600_000;
    expect(lifetimeHours).toBeGreaterThan(23.9);
    expect(lifetimeHours).toBeLessThanOrEqual(24);
  });

  it('rate limits the sixth failed login attempt within the window', async () => {
    resetLoginLimiter();
    const statuses: number[] = [];
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const response = await send('POST', '/api/auth/login', {
        body: { username: USERNAME, password: `${PASSWORD}-wrong` },
      });
      statuses.push(response.status);
    }
    expect(statuses).toEqual([401, 401, 401, 401, 401, 429]);
    await expectServerUp();
  });
});
