/**
 * Replay the M0 request/role inventory against a Rust TCP front door.
 * --cut uses the shipped CLI with its private worker. --cut-rust-only disables the worker.
 * Unix cut modes require a parity-fixtures binary for the original synthetic app jobs.
 * --require-complete fails for any changed answer or explicit unmounted route.
 */
import assert from 'node:assert/strict';
import { isDeepStrictEqual } from 'node:util';
import { createHash, createHmac } from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import bcrypt from 'bcrypt';
import { WebSocket } from 'ws';
import { createEmptyUnifiedConfig } from '../../src/config/unified-config-types';
import { fixtureEnvironment, startRustFixture } from './m6-rust-fixture';

type Contract = {
  request: { method: string; path: string; role: string; body?: object };
  response: { status: number; body?: unknown; headers?: Record<string, string> };
};

const binary = path.resolve(process.argv[2] ?? '');
const mode = process.argv.includes('--cut-rust-only')
  ? 'cut-rust-only'
  : process.argv.includes('--cut')
    ? 'cut'
    : 'fixture';
const spec = import.meta.dir;
const routes = JSON.parse(await fs.readFile(path.join(spec, 'routes.json'), 'utf8')) as {
  path: string;
  method: string;
  roles: string[];
}[];
const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-contract-'));
const username = 'fixture-admin';
const password = 'fixture-password-only';
const credentials = { username, password };
const nonce = 'f'.repeat(32);
const hub = 'aacu_' + 'f'.repeat(43);
const environment = fixtureEnvironment();
if (mode === 'cut') {
  const preload = path.join(spec, 'm6-cut-worker-fixture.cjs').split(path.sep).join('/');
  Object.assign(environment, {
    NODE_OPTIONS: [environment.NODE_OPTIONS, '--require=' + JSON.stringify(preload)]
      .filter(Boolean)
      .join(' '),
    CODEX_HOME: path.join(root, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(root, '.claude'),
    AAC_AGENT_REPORTS_DIR: path.join(root, 'reports'),
    AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'registry'),
    AAC_RUST_STORE: 'off',
    AAC_RUST_GROUP_HOME: 'off',
    AAC_RUST_HOME_VIEW: 'off',
    AAC_RUST_GROUP_APP_UPDATES: 'off',
    AAC_RUST_GROUP_COLLECTION: 'off',
    AAC_AGENT_VERBS: 'off',
    AAC_AGENT_TRANSPORT: 'off',
    AAC_STATUS_DOORBELL: 'off',
    AAC_STORE_SHARE_BACKUP_DIR: 'off',
  });
}
if (mode !== 'fixture' && process.platform !== 'win32') {
  Object.assign(environment, {
    AAC_RUST_CUT_APP_UPDATES_FIXTURE: '1',
    AAC_RUST_GROUP_APP_UPDATES: 'on',
    AAC_RUST_GROUP_COLLECTION: 'on',
    AAC_AGENT_VERBS: 'off',
    AAC_AGENT_TRANSPORT: 'off',
  });
}

function normalize(value: unknown, key = ''): unknown {
  if (value === undefined) return value;
  if (key === 'etag') return '<etag>';
  if (key === 'x-ccs-bar-token') return '<bar-proof>';
  if (key === 'retry-after' || key === 'retryAfterSeconds') {
    const seconds = Number(value);
    assert(Number.isInteger(seconds) && seconds >= 1 && seconds <= 900);
    return '<retry-after-seconds>';
  }
  if (typeof value === 'string' && key === 'set-cookie') {
    return value
      .replace(/^connect\.sid=[^;]+/, 'connect.sid=<session-cookie>')
      .replace(/Expires=[^;]+/i, 'Expires=<http-date>');
  }
  if (typeof value === 'string') {
    let text = value
      .replaceAll(root, '<fixture-home>')
      .replace(/dev_[0-9a-f]{16}/g, '<device-id>')
      .replace(/aacd_[A-Za-z0-9_-]{43}/g, '<device-token>')
      .replace(/\b\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z\b/g, '<timestamp>');
    if (path.sep === '\\' && text.startsWith('<fixture-home>')) text = text.replaceAll('\\', '/');
    return text;
  }
  if (Array.isArray(value)) return value.map((entry) => normalize(entry));
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value).map(([field, child]) => [field, normalize(child, field)])
    );
  return value;
}

function send(base: string, endpoint: string, args: RequestInit = {}) {
  return fetch(base + endpoint, { ...args, redirect: 'manual', signal: AbortSignal.timeout(5000) });
}

async function roleHeaders(base: string, role: string) {
  const headers = new Headers({ origin: base, 'content-type': 'application/json' });
  if (role === 'session' || role === 'bar') {
    const login = await send(base, '/api/auth/login', {
      method: 'POST',
      headers,
      body: JSON.stringify(credentials),
    });
    assert.equal(login.status, 200, 'fixture browser login');
    await login.arrayBuffer();
    headers.set('cookie', login.headers.get('set-cookie')!.split(';')[0]);
    if (role === 'bar') headers.set('x-ccs-bar-nonce', nonce);
  } else if (role === 'device') {
    const pairing = await send(base, '/api/auth/devices/pair', {
      method: 'POST',
      headers,
      body: JSON.stringify({
        ...credentials,
        deviceName: 'Fixture device',
        platform: 'mac',
        installId: '00000000-0000-4000-8000-000000000001',
        appVersion: '0.0.0',
      }),
    });
    assert.equal(pairing.status, 201, 'fixture tray pairing');
    headers.set('authorization', 'Bearer ' + (await pairing.json()).token);
  } else if (role === 'hub') headers.set('authorization', 'Bearer ' + hub);
  return headers;
}

function upgrade(base: string, headers: Headers): Promise<number> {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(base.replace('http:', 'ws:') + '/ws', {
      headers: Object.fromEntries(headers),
      handshakeTimeout: 5000,
    });
    ws.on('error', (error) => {
      if (!error.message.includes('closed before')) reject(error);
    });
    ws.on('open', () => {
      resolve(101);
      ws.terminate();
    });
    ws.on('unexpected-response', (_request, response) => {
      resolve(response.statusCode!);
      response.resume();
      ws.terminate();
    });
  });
}

const counts = { match: 0, decline: 0, different: 0 };
const owned = { match: 0, failed: 0 };
const owns = (route: string) =>
  route.startsWith('/api/auth/') ||
  route.startsWith('/v0/management/') ||
  ['/{*splat}', '/api/bar/auth', '/api/health', '/ws'].includes(route);
const declined = new Set<string>();
const different: string[] = [];
const expectedHeaders = [
  'content-type',
  'cache-control',
  'location',
  'retry-after',
  'etag',
  'vary',
  'set-cookie',
  'x-ccs-bar-token',
  'content-security-policy',
  'x-content-type-options',
  'referrer-policy',
  'cross-origin-resource-policy',
];

try {
  const ui = path.join(root, 'ui');
  await fs.mkdir(path.join(ui, 'app'), { recursive: true });
  await fs.writeFile(path.join(ui, 'app/index.html'), '<div id="aac-app-fixture"></div>');
  const auth = path.join(root, 'auth');
  await fs.mkdir(auth, { mode: 0o700 });
  await fs.writeFile(
    path.join(auth, 'usage-hub-key.json'),
    JSON.stringify({
      version: 1,
      keySha256: createHash('sha256').update(hub).digest('hex'),
      createdAt: '2026-01-01T00:00:00.000Z',
    }),
    { mode: 0o600 }
  );
  environment.AAC_RUST_UI_DIR = ui;
  delete environment.AAC_RUST_HUB_CACHE;
  await fs.writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      ...createEmptyUnifiedConfig(),
      dashboard_auth: {
        enabled: true,
        username,
        password_hash: await bcrypt.hash(password, 4),
        session_timeout_hours: 24,
      },
    }),
    { mode: 0o600 }
  );
  let rust = await startRustFixture(binary, root, environment, false, mode);
  try {
    for (const [index, route] of routes.entries()) {
      const name = route.method + ' ' + route.path;
      for (const role of route.roles) {
        const fixture = String(index).padStart(3, '0') + '-' + role + '.json';
        const contract: Contract = JSON.parse(
          await fs.readFile(path.join(spec, 'contract', fixture), 'utf8')
        );
        const { request, response: expected } = contract;
        const headers = await roleHeaders(rust.url, role);
        let status: number;
        let body: unknown;
        let selected: Record<string, string> | undefined;
        if (request.method === 'UPGRADE') {
          status = await upgrade(rust.url, headers);
        } else {
          let endpoint = request.path
            .replace('/{*splat}', '/')
            .replace(/:[A-Za-z]+/g, 'fixture-missing');
          if (endpoint.includes('/@regex')) endpoint = '/api/status/files/fixture/missing.txt';
          // The route inventory stores the pattern; capture supplies this query explicitly.
          if (/accounts\/(dashboard|analytics)$/.test(endpoint)) endpoint += '?fixture=invalid';
          const response = await send(rust.url, endpoint, {
            method: request.method === 'ALL' ? 'POST' : request.method,
            headers,
            body: request.body === undefined ? undefined : JSON.stringify(request.body),
          });
          status = response.status;
          const text = await response.text();
          try {
            body = JSON.parse(text);
          } catch {
            body = text;
          }
          if (status === 503 && (body as { code?: string } | null)?.code === 'route_unavailable') {
            counts.decline++;
            if (owns(route.path)) owned.failed++;
            declined.add(name);
            continue;
          }
          selected = Object.fromEntries(
            expectedHeaders.flatMap((key) => {
              const value = response.headers.get(key);
              return value === null ? [] : [[key, value]];
            })
          );
          const proof = selected['x-ccs-bar-token'];
          if (proof) {
            const token = (await fs.readFile(path.join(root, 'bar', '.auth-token'), 'utf8')).trim();
            assert.equal(proof, createHmac('sha256', token).update(nonce).digest('hex'));
          }
        }
        const actual =
          request.method === 'UPGRADE'
            ? { status }
            : normalize({ status, headers: selected, body });
        try {
          assert.deepEqual(actual, expected);
          counts.match++;
          if (owns(route.path)) owned.match++;
        } catch {
          const fields = [
            status !== expected.status ? `status ${status}/${expected.status}` : '',
            isDeepStrictEqual(normalize(body), expected.body) ? '' : 'body',
            ...expectedHeaders.filter(
              (key) =>
                normalize(selected?.[key], key) !== expected.headers?.[key] &&
                (selected?.[key] !== undefined || expected.headers?.[key] !== undefined)
            ),
          ].filter(Boolean);
          different.push(name + ' [' + role + '] ' + fields.join(', '));
          if (process.argv.includes('--show-fixture-diff') && fields.includes('body')) {
            console.log(
              'M0 fixture body ' +
                fixture +
                ' ' +
                JSON.stringify({
                  actual: normalize(body),
                  expected: expected.body,
                })
            );
          }
          counts.different++;
          if (owns(route.path)) owned.failed++;
        }
      }
      // Capture clears the in-memory limiter before each case. Its malformed login cases
      // spend the 5-attempt budget, so restart here with the same persisted fixture state.
      if (name === 'POST /api/auth/login') {
        await rust.stop();
        rust = await startRustFixture(binary, root, environment, false, mode);
      }
    }
  } finally {
    await rust.stop();
  }
} finally {
  await fs.rm(root, { recursive: true, force: true });
}
console.log(
  `Rust public M0: ${counts.match} matching replies, ${counts.decline} explicit declines across ${declined.size} routes, ${counts.different} differences.`
);
console.log(`M6-owned M0 rows: ${owned.match} matched, ${owned.failed} failed.`);
console.log('M0 declines: ' + [...declined].join(', '));
if (different.length) console.log('M0 differences:\n' + different.join('\n'));
assert.equal(owned.failed, 0, 'M6-owned M0 contract');
if (process.argv.includes('--require-complete'))
  assert.equal(counts.decline + counts.different, 0, 'Rust M0 cutover gate');
