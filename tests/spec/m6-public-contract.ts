/**
 * Replay the M0 request/role inventory against a Rust TCP front door.
 * --cut uses the shipped CLI with its private worker. --cut-rust-only disables the worker.
 * Unix cut modes require a parity-fixtures binary for the original synthetic app jobs.
 * --require-complete fails for any changed answer or explicit unmounted route.
 * --route-group=<name> selects one worker-removal lane and implies --require-complete.
 * --composed-golden applies the platform's existing recorder overrides.
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
const groups = {
  'status-home': /^\w+ \/api\/(status(?:\/|$)|store\/status$|usage\/[^/]+$)/,
  resets: /^\w+ \/api\/accounts\/[^/]+\/reset-claims(?:\/|$)/,
  'store-usage-home': /^\w+ \/api\/(store\/status|usage\/[^/]+|views\/home)$/,
  status: /^\w+ \/api\/(status(?:\/|$)|scoreboard$)/,
  profiles:
    /^\w+ \/api\/(antigravity\/profiles|codex\/profiles|claude\/desktop-profiles|claude\/history-snapshots\/cleanup)(?:\/|$)/,
  analytics: /^\w+ \/api\/accounts\/analytics(?:\/|$)/,
  settings: /^\w+ \/api\/accounts\/(settings|preferences|visibility)$/,
  lifecycle:
    /^(?:PUT \/api\/accounts\/:id\/key|PATCH \/api\/accounts\/:id|\w+ \/api\/accounts\/(add|registry|signin-command|signin-jobs(?:\/.*)?|trash(?:\/.*)?|:id\/(?:open|remove|signin-again)))$/,
  collection: /^\w+ \/api\/accounts\/(dashboard|:id\/recheck)$/,
  'app-updates': /^\w+ \/api\/app-updates\//,
} satisfies Record<string, RegExp>;
const group =
  process.argv.find((arg) => arg.startsWith('--route-group='))?.split('=', 2)[1] ??
  (process.argv.includes('--group=reset-claims') ? 'resets' : undefined);
const selectedRoutes = [...routes.entries()].filter(([, route]) => {
  if (group === undefined) return true;
  assert.equal(
    mode,
    process.argv.some((arg) => arg.startsWith('--record-native=')) ? 'cut' : 'cut-rust-only',
    'A worker-removal group replays worker-less, or records through the shipped cut.'
  );
  assert(Object.hasOwn(groups, group), `Unknown worker-removal group ${group}.`);
  return groups[group as keyof typeof groups].test(route.method + ' ' + route.path);
});
assert(selectedRoutes.length > 0, 'At least one M0 route is required.');
const composedGolden = process.argv.includes('--composed-golden');
const nativeReset = (method: string, route: string) =>
  mode !== 'fixture' && groups.resets.test(method + ' ' + route);
const overrides: Record<string, Contract> =
  composedGolden || mode !== 'fixture'
    ? JSON.parse(
        await fs.readFile(
          path.join(
            spec,
            'contract',
            `m0-composed-${process.platform === 'win32' ? 'windows' : 'unix'}.json`
          ),
          'utf8'
        )
      )
    : {};
// W-status-home: the group's own composed rows (the native answers of the fixture home).
// They are read only for the group's worker-less run; every other row keeps the shared
// composed contract and the reset rows' own override below.
const nativeGroupRow = (method: string, route: string) =>
  method === 'GET' &&
  (route === '/api/status' ||
    route.startsWith('/api/status/') ||
    route === '/api/store/status' ||
    route.startsWith('/api/usage/') ||
    route === '/api/scoreboard');
const recordNative = process.argv
  .find((arg) => arg.startsWith('--record-native='))
  ?.slice('--record-native='.length);
const recordedNative: Record<string, Contract> = {};
const groupOverrides: Record<string, Contract> = {};
if (group !== undefined && mode === 'cut-rust-only') {
  try {
    Object.assign(
      groupOverrides,
      JSON.parse(
        await fs.readFile(
          path.join(
            spec,
            'contract',
            `m0-composed-${process.platform === 'win32' ? 'windows' : 'unix'}-native.json`
          ),
          'utf8'
        )
      )
    );
  } catch {
    // Not recorded yet: the plain contract answers and the run fails on the difference.
  }
}

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
    // The migration lanes serve Accounts and Re-check natively in both modes, so the native
    // collection runs here too; the worker still owns its remaining routes.
    AAC_RUST_GROUP_COLLECTION: 'on',
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
  // A store boot's own identity (capture.ts nulls the same members when it records the row),
  // the workers' live host readings (capture.ts does the same), and clock numbers.
  if (key === 'openedAt' || key === 'logs' || key === 'meta') return null;
  if ((key === 'at' || key === 'now') && typeof value === 'number') return '<at-ms>';
  if ((key === 'ageSeconds' || key === 'cacheAgeSeconds') && typeof value === 'number')
    return '<elapsed-seconds>';
  if (key === 'cpu' || key === 'memory' || key === 'gpuPct') return null;
  if (key === 'disk' || key === 'warnings') return [];
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
      .replace(/\b\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z\b/g, '<timestamp>')
      .replace(/\b\d{1,2}:\d{2}\s?(?:AM|PM)\b/g, '<clock>')
      .replace(/\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{1,2}\b/g, '<day>');
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
const resets = { match: 0, failed: 0 };
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
  if (recordNative) {
    Object.assign(environment, {
      AAC_RUST_CUT_NATIVE_ROUTES: 'on',
      AAC_RUST_STORE: 'on',
      AAC_RUST_GROUP_STATUS: 'on',
      AAC_RUST_GROUP_SCOREBOARD: 'on',
      AAC_AGENT_REPORTS_DIR: path.join(root, 'reports'),
      AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'registry'),
    });
    await fs.mkdir(path.join(root, 'reports'), { recursive: true });
    await fs.mkdir(path.join(root, 'registry'), { recursive: true });
  }
  if (group === 'status-home' || group === 'status') {
    for (const key of Object.keys(environment))
      if (key.startsWith('AAC_RUST_') || key.startsWith('AAC_STORE')) delete environment[key];
    Object.assign(environment, {
      AAC_RUST_CUT_NATIVE_ROUTES: 'on',
      AAC_AGENT_REPORTS_DIR: path.join(root, 'reports'),
      AAC_PM_MAIL_REGISTRY_DIR: path.join(root, 'registry'),
      AAC_AGENT_VERBS: 'off',
      AAC_AGENT_TRANSPORT: 'off',
    });
    await fs.mkdir(path.join(root, 'reports'), { recursive: true });
    await fs.mkdir(path.join(root, 'registry'), { recursive: true });
  }
  if (group === 'resets') {
    Object.assign(environment, {
      CCS_HOME: root,
      AAC_RUST_GROUP_COLLECTION: 'off',
      AAC_RUST_GROUP_APP_UPDATES: 'off',
      AAC_RUST_CUT_APP_UPDATES_FIXTURE: '0',
      AAC_RUST_STORE: 'off',
      AAC_RUST_GROUP_WRITES: 'off',
      AAC_AGENT_TRANSPORT: 'off',
      AAC_AGENT_VERBS: 'off',
    });
  }
  let rust = await startRustFixture(binary, root, environment, false, mode);
  try {
    for (const [index, route] of selectedRoutes) {
      const name = route.method + ' ' + route.path;
      const ownsReset = nativeReset(route.method, route.path);
      for (const role of route.roles) {
        const fixture = String(index).padStart(3, '0') + '-' + role + '.json';
        const mine = nativeGroupRow(route.method, route.path);
        const contract: Contract =
          (mine
            ? groupOverrides['contract/' + fixture]
            : composedGolden || ownsReset
              ? overrides['contract/' + fixture]
              : undefined) ??
          JSON.parse(await fs.readFile(path.join(spec, 'contract', fixture), 'utf8'));
        const { request, response: expected } = contract;
        const headers = await roleHeaders(rust.url, role);
        let status: number;
        let body: unknown;
        let selected: Record<string, string> | undefined;
        let served = request.path;
        if (request.method === 'UPGRADE') {
          status = await upgrade(rust.url, headers);
        } else {
          let endpoint = request.path
            .replace('/{*splat}', '/')
            .replace(/:[A-Za-z]+/g, 'fixture-missing');
          if (endpoint.includes('/@regex')) endpoint = '/api/status/files/fixture/missing.txt';
          served = endpoint;
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
            if (ownsReset) resets.failed++;
            declined.add(name);
            continue;
          }
          selected = Object.fromEntries(
            expectedHeaders.flatMap((key) => {
              const value = response.headers.get(key);
              return value === null ? [] : [[key, value]];
            })
          );
          if (recordNative && mine) {
            recordedNative['contract/' + fixture] = {
              request: {
                method: request.method === 'ALL' ? 'POST' : request.method,
                path: served,
                role,
                ...(request.body === undefined ? {} : { body: request.body }),
              },
              response: normalize({ status, headers: selected, body }),
            } as Contract;
            counts.match++;
            continue;
          }
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
          if (ownsReset) resets.match++;
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
          if (ownsReset) resets.failed++;
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
if (recordNative) {
  await fs.writeFile(
    path.isAbsolute(recordNative) ? recordNative : path.join(spec, recordNative),
    JSON.stringify(recordedNative, null, 2) + '\n'
  );
  console.log(`Recorded ${Object.keys(recordedNative).length} native rows into ${recordNative}`);
}
console.log(
  `Rust public M0: ${counts.match} matching replies, ${counts.decline} explicit declines across ${declined.size} routes, ${counts.different} differences.`
);
console.log(`M6-owned M0 rows: ${owned.match} matched, ${owned.failed} failed.`);
if (mode !== 'fixture')
  console.log(`Rust reset M0 rows: ${resets.match} matched, ${resets.failed} failed.`);
if (group !== undefined)
  console.log(`M0 worker-removal group ${group}: ${selectedRoutes.length} routes.`);
console.log('M0 declines: ' + [...declined].join(', '));
if (different.length) console.log('M0 differences:\n' + different.join('\n'));
assert.equal(owned.failed, 0, 'M6-owned M0 contract');
assert.equal(resets.failed, 0, 'Rust reset M0 contract');
if (!recordNative && (group !== undefined || process.argv.includes('--require-complete')))
  assert.equal(counts.decline + counts.different, 0, 'Rust M0 cutover gate');
