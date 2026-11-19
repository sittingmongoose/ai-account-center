/** Run in its own Bun process: never import this network/I/O sandbox into another suite. */
import { mock, spyOn } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';
const root = path.resolve(fileURLToPath(new URL('../..', import.meta.url)));
const spec = path.join(root, 'tests/spec');
const fixture = JSON.parse(fs.readFileSync(path.join(spec, 'fixture.json'), 'utf8'));
const regenerate = process.argv.includes('--regenerate');
const rustMode = process.env.AAC_RUST_SERVICE ?? 'off';
const rustBinary = path.join(root, 'dist/native/aac-http');
if (rustMode !== 'off')
  assert.ok(fs.existsSync(rustBinary), 'on/shadow contract checks require the Nas1-built binary');
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-contract-'));
const ccs = path.join(home, '.ccs');
fs.mkdirSync(ccs, { recursive: true });
fs.writeFileSync(path.join(ccs, 'fixture.json'), JSON.stringify(fixture));
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: home,
  CCS_DIR: ccs,
  CODEX_HOME: path.join(home, '.codex'),
  CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
  CCS_SESSION_SECRET: 'fixture-session-secret-for-contract-only',
  AAC_NO_LOCAL_RUST_BUILD: '1',
  AAC_RUST_SERVICE_BINARY: rustBinary,
});
spyOn(os, 'homedir').mockReturnValue(home);
// A few legacy readers use process.env.HOME directly. Reject private paths
// outside the fixture even when a reader ignores CCS_HOME and CODEX_HOME.
const fixtureFs = await import('node:fs/promises');
const moduleRoot = fs.realpathSync(path.join(root, 'node_modules'));
const denied = new Set<string>();
function guardFile(input: any) {
  if (typeof input !== 'string' && !(input instanceof URL)) return;
  const target = path.resolve(input instanceof URL ? fileURLToPath(input) : input);
  if ([home, root, moduleRoot].some((base) => target === base || target.startsWith(base + '/')))
    return;
  if (
    target.startsWith('/home/') ||
    target.startsWith('/mnt/Cursor/') ||
    /\/(?:\.ccs|\.claude|\.codex|\.t3)(?:\/|$)/.test(target)
  ) {
    denied.add(target.replace(/^.*\/(\.[^/]+).*$/, '$1'));
    throw Object.assign(new Error('Outside fixture filesystem'), { code: 'EACCES' });
  }
}
for (const [object, names, modules] of [
  [
    fs,
    [
      'readFileSync',
      'writeFileSync',
      'readdirSync',
      'statSync',
      'lstatSync',
      'openSync',
      'existsSync',
      'mkdirSync',
    ],
    ['fs', 'node:fs'],
  ],
  [
    fixtureFs,
    ['readFile', 'writeFile', 'readdir', 'stat', 'lstat', 'open', 'mkdir'],
    ['fs/promises', 'node:fs/promises'],
  ],
] as const) {
  const guarded: any = { ...object };
  for (const name of names) {
    const original = (object as any)[name];
    guarded[name] = (input: any, ...args: any[]) => {
      guardFile(input);
      return original.call(object, input, ...args);
    };
  }
  for (const module of modules) mock.module(module, () => ({ ...guarded, default: guarded }));
}
const fixtureOs = { ...os, homedir: () => home };
for (const module of ['os', 'node:os'])
  mock.module(module, () => ({ ...fixtureOs, default: fixtureOs }));
assert.equal((await import('os')).homedir(), home);
const awaitFsRead = (await import('fs')).readFileSync;
assert.throws(() => awaitFsRead('/home/fixture-forbidden/.codex/auth.json'), /Outside fixture/);
// Fail closed at process boundaries: helpers, SSH and provider programs cannot run.
const cp = await import('node:child_process');
const realSpawn = cp.spawn.bind(cp);
const realSpawnSync = cp.spawnSync.bind(cp);
const fixtureSpawn = (...args: any[]) => {
  if (
    rustMode !== 'off' &&
    args[0] === rustBinary &&
    Array.isArray(args[1]) &&
    args[1].length === 0
  )
    return realSpawn(...(args as Parameters<typeof realSpawn>));
  throw new Error('Fixture process boundary');
};
const rejectSpawn = () => {
  throw new Error('Fixture process boundary');
};
const fixtureSpawnSync = (
  command: string,
  args: string[],
  options: Parameters<typeof cp.spawnSync>[2]
) => {
  // These commands can only flock the inherited descriptor. They cannot open new paths.
  const stdio = options?.stdio;
  if (
    Array.isArray(stdio) &&
    stdio.length === 3 &&
    typeof stdio[0] === 'number' &&
    stdio[1] === 'ignore' &&
    stdio[2] === 'ignore' &&
    Array.isArray(args) &&
    ((command === rustBinary && args.length === 1 && args[0] === '--file-lease') ||
      (process.platform === 'linux' &&
        ['/usr/bin/flock', '/bin/flock'].includes(command) &&
        args.length === 2 &&
        args[0] === '-n' &&
        args[1] === '0'))
  ) {
    if (process.platform === 'linux') {
      const fdPath = fs.readlinkSync('/proc/self/fd/' + stdio[0]);
      assert.ok(fdPath.startsWith(ccs + '/'), 'lease belongs to the fixture');
    }
    return realSpawnSync(command, args, options);
  }
  return rejectSpawn();
};
const allowed = { spawn: fixtureSpawn, spawnSync: fixtureSpawnSync };
const blockedProcesses = { ...cp };
for (const name of [
  'spawn',
  'spawnSync',
  'exec',
  'execSync',
  'execFile',
  'execFileSync',
  'fork',
] as const) {
  const handler = name === 'spawn' || name === 'spawnSync' ? allowed[name] : rejectSpawn;
  spyOn(cp, name).mockImplementation(handler as never);
  (blockedProcesses as any)[name] = handler;
}
for (const module of ['child_process', 'node:child_process'])
  mock.module(module, () => ({ ...blockedProcesses, default: blockedProcesses }));
const tls = await import('node:https');
const noTls = () => {
  throw new Error('External HTTPS forbidden');
};
for (const module of ['https', 'node:https'])
  mock.module(module, () => ({
    ...tls,
    request: noTls,
    get: noTls,
    default: { ...tls, request: noTls, get: noTls },
  }));
const net = await import('node:net');
const socketConnect = net.Socket.prototype.connect;
spyOn(net.Socket.prototype, 'connect').mockImplementation(function (this: any, ...args: any[]) {
  const options = args[0];
  const host =
    typeof options === 'object'
      ? options.host
      : typeof args[1] === 'string'
        ? args[1]
        : 'localhost';
  assert.ok(
    ['127.0.0.1', '::1', 'localhost', undefined].includes(host),
    'External socket forbidden'
  );
  if (typeof options === 'object' && options.path) guardFile(options.path);
  return socketConnect.apply(this, args as never);
} as never);
const realFetch = globalThis.fetch;
globalThis.fetch = ((url: any, options: any) => {
  const parsed = new URL(typeof url === 'string' ? url : (url.url ?? String(url)));
  assert.equal(parsed.hostname, '127.0.0.1', 'External fetch forbidden');
  return realFetch(url, options);
}) as typeof fetch;
async function stub(file: string, replacements: Record<string, unknown>) {
  const module = await import(path.join(root, file));
  for (const [name, value] of Object.entries(replacements))
    spyOn(module, name).mockImplementation(value as never);
}
const noop = () => {};
const stop = () => noop;
await stub('src/utils/managed-process.ts', {
  startProcessBroker: async () => {},
  stopProcessBroker: async () => {},
  runManagedCommand: async () => ({ stdout: '', stderr: '', exitCode: 1, timedOut: false }),
});
await stub('src/web-server/services/account-analytics-service.ts', {
  startAccountAnalyticsSampling: noop,
  stopAccountAnalyticsSampling: noop,
});
await stub('src/web-server/services/account-lifecycle-runtime.ts', {
  startAccountLifecycleMaintenance: noop,
  stopAccountLifecycleMaintenance: noop,
});
await stub('src/web-server/status/status-service.ts', {
  startStatusService: stop,
  getStatusService: () => ({
    view: () => fixture.statusView,
    answers: { sendList: () => [], history: () => [] },
  }),
});
await stub('src/web-server/scoreboard/scoreboard-service.ts', {
  startScoreboardService: noop,
  getScoreboardService: () => ({ view: async () => fixture.scoreboardView }),
});
await stub('src/web-server/services/cgroup-foreign-check.ts', { startCgroupForeignCheck: stop });
await stub('src/web-server/agent-usage/agent-usage-snapshot.ts', { startAgentUsageSnapshot: stop });
await stub('src/antigravity/production-runtime.ts', {
  getInstalledAntigravityRuntimeFactory: () => null,
});
const renewal = await import('../../src/web-server/services/codex-profile-renewal-service');
spyOn(renewal.CodexProfileRenewalService.prototype, 'start').mockImplementation(noop);
await stub('src/store/store-service.ts', {
  openAacStore: async () => ({ store: null, reason: 'disabled' }),
  getAacStore: () => ({
    status: async () => fixture.storeStatus,
    seriesPoints: async () => fixture.seriesPoints,
    seriesList: async () => [],
    usageAt: async () => fixture.usageAt,
    burnLong: async () => [],
    periods: async () => [],
  }),
  aacStoreState: () => ({ state: 'unavailable', reason: 'disabled' }),
});
// Capture actual router registrations, including computed paths and regexes.
const routerModule = await import('../../src/web-server/routes/api-router');
const createRouter = routerModule.createApiRouter;
const records = new Map<any, any[]>();
spyOn(routerModule, 'createApiRouter').mockImplementation(() => {
  const router = createRouter();
  const entries: any[] = [];
  records.set(router, entries);
  for (const method of ['use', 'all', 'get', 'head', 'post', 'put', 'patch', 'delete', 'options']) {
    const original = (router as any)[method];
    (router as any)[method] = (...args: any[]) => {
      const frame = new Error().stack
        ?.split('\n')
        .find((line) => line.includes(`${root}/src/`) && !line.includes('api-router.ts'));
      entries.push({
        method,
        args,
        handlerFile:
          frame?.slice(frame.indexOf(root) + root.length + 1).match(/^(src\/[^():]+\.ts)/)?.[1] ??
          'unknown',
      });
      return original.apply(router, args);
    };
  }
  return router;
});
await stub('src/codex-auth/codex-activation-lock.ts', {
  getActivationCodexHome: () => path.join(home, '.codex'),
});
await stub('src/web-server/services/app-update-service.ts', {
  getAppUpdateService: () => ({
    getStatus: () => ({ job: null }),
    start: () => ({ job: { id: 'fixture-update', state: 'running', hosts: {}, results: [] } }),
    cancel: () => ({ job: null }),
  }),
});
const { createEmptyUnifiedConfig } = await import('../../src/config/unified-config-types');
const { saveUnifiedConfig } = await import('../../src/config/unified-config-loader');
const bcrypt = await import('bcrypt');
const config = createEmptyUnifiedConfig();
config.dashboard_auth = {
  enabled: true,
  username: 'fixture-admin',
  password_hash: bcrypt.hashSync('fixture-password-only', 4),
  session_timeout_hours: 24,
};
saveUnifiedConfig(config);
// Use the owner's writer shape so the Rust subset is exercised, not its TS fallback.
(
  await import('../../src/web-server/services/account-refresh-settings')
).writeAccountRefreshSettings({ refreshIntervalSeconds: 60 }, ccs);
const ui = path.join(home, 'ui');
fs.mkdirSync(ui);
// The Leptos app frame is the dashboard page (static-ui.ts APP_ENTRY); every page route answers with it.
fs.mkdirSync(path.join(ui, 'app'));
fs.writeFileSync(path.join(ui, 'app', 'index.html'), '<div id="aac-app-fixture"></div>');
const { startServer } = await import('../../src/web-server');
const instance = await startServer({
  port: 0,
  host: '127.0.0.1',
  staticDir: ui,
  rustServiceBinary: rustBinary,
});
const base = `http://127.0.0.1:${(instance.server.address() as any).port}`;
const { apiRoutes } = await import('../../src/web-server/routes');
const codexSummary = await import('../../src/codex-auth/codex-auth-dashboard-service');
const { pairDevice } = await import('../../src/web-server/services/dashboard-device-store');

const hubModule = await import('../../src/web-server/usage-hub/usage-hub-key-store');
const hubKey = 'aacu_' + 'f'.repeat(43);
// Seed the on-disk format through a fixture, never mint a real credential.
fs.mkdirSync(path.join(ccs, 'auth'), { recursive: true, mode: 0o700 });
fs.writeFileSync(
  hubModule.usageHubKeyPath(),
  JSON.stringify({
    version: 1,
    keySha256: hubModule.hashUsageHubKey(hubKey),
    createdAt: '2026-01-01T00:00:00.000Z',
  }),
  { mode: 0o600 }
);
const roles = ['signed-out', 'session', 'device', 'bar', 'hub'];
function milestone(route: any) {
  if (/\/auth\b|\/bar\b|^\/v0|^\/ws|splat/.test(route.path)) return 'M6';
  if (route.writes || /app-updates/.test(route.path)) return 'M5';
  return 'M3';
}
const inventory: any[] = [];
function walk(router: any, prefix: string) {
  for (const record of records.get(router) ?? []) {
    const [first, ...rest] = record.args;
    if (record.method === 'use') {
      for (const child of typeof first === 'string' ? rest : record.args)
        if (records.has(child)) walk(child, prefix + (typeof first === 'string' ? first : ''));
      continue;
    }
    if (typeof first !== 'string' && !(first instanceof RegExp))
      throw new Error('Unrecognized route path');
    const routePath =
      first instanceof RegExp
        ? `${prefix}/@regex:${first.source}`
        : prefix + (first === '/' ? '' : first);
    inventory.push({
      method: record.method.toUpperCase(),
      path: routePath,
      roles,
      handlerFile: record.handlerFile,
      writes: !['get', 'head', 'options'].includes(record.method),
    });
  }
}
walk(apiRoutes, '/api');
for (const [router] of records)
  if ((records.get(router) ?? []).some((r) => r.args[0] === '/auth-files'))
    walk(router, '/v0/management');
inventory.push({
  method: 'UPGRADE',
  path: '/ws',
  roles,
  handlerFile: 'src/web-server/index.ts',
  writes: false,
});
inventory.push({
  method: 'GET',
  path: '/{*splat}',
  roles,
  handlerFile: 'src/web-server/index.ts',
  writes: false,
});
// Middleware-only deliverable handler has no Router.get registration.
inventory.push({
  method: 'GET',
  path: '/api/status/deliverable',
  roles,
  handlerFile: 'src/web-server/routes/share-route.ts',
  writes: false,
});
for (const r of inventory) {
  r.milestone = milestone(r);
  if (r.path.startsWith('/v0/management')) r.writes = false;
}
inventory.sort((a, b) => (a.path + a.method).localeCompare(b.path + b.method, 'en'));
function emit(relative: string, data: any) {
  const file = path.join(spec, relative);
  const bytes = JSON.stringify(data, null, 2) + '\n';
  if (regenerate) fs.writeFileSync(file, bytes);
  else
    assert.equal(
      fs.readFileSync(file, 'utf8'),
      bytes,
      `${relative} changed; review and regenerate explicitly`
    );
}
function normalize(value: any, key = ''): any {
  if (typeof value === 'string' && ['buildId', 'buildCommit', 'commit'].includes(key))
    return '<build-id>';
  if (typeof value === 'string' && key === 'x-ccs-bar-token') return '<bar-proof>';
  if (typeof value === 'string' && key === 'etag') return '<etag>';
  // A rate-limit wait counts down from the limiter window's start (900 s), so its whole seconds
  // depend on how long the capture took to reach the request: 900 on a fast machine, 899 on a
  // slow one. Pin its range, not the clock.
  if (key === 'retry-after' || key === 'retryAfterSeconds') {
    const seconds = Number(value);
    assert.ok(
      Number.isInteger(seconds) && seconds >= 1 && seconds <= 900,
      `${key} ${String(value)} is not a whole number of seconds in 1..900`
    );
    return '<retry-after-seconds>';
  }
  if (typeof value === 'string' && key === 'set-cookie')
    return value
      .replace(/^[^=]+=[^;]+/, 'connect.sid=<session-cookie>')
      .replace(/Expires=[^;]+/i, 'Expires=<http-date>');
  if (Array.isArray(value)) return value.map((entry) => normalize(entry));
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b, 'en'))
        .map(([k, v]) => [k, normalize(v, k)])
    );
  if (typeof value === 'string')
    return value
      .replace(/dev_[0-9a-f]{16}/g, '<device-id>')
      .replace(/aacd_[A-Za-z0-9_-]{43}/g, '<device-token>')
      .replaceAll(base, '<origin>')
      .replaceAll(home, '<fixture-home>')
      .replace(/\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z\b/g, '<timestamp>')
      .replace(/\b[0-9a-f]{8}-[0-9a-f-]{27,}\b/g, '<uuid>');
  return value;
}
try {
  // The HTTP listener starts before its private service. Dispatch parity needs a running service.
  if (rustMode !== 'off') {
    const serviceState = () =>
      (instance.rustService.diagnostics().service as { state: string }).state;
    const deadline = Date.now() + 7000;
    while (serviceState() === 'down' && Date.now() < deadline)
      await new Promise((resolve) => setTimeout(resolve, 10));
    assert.equal(serviceState(), 'ready', 'the fixture Rust service must be ready before replay');
  }
  emit('routes.json', inventory);
  // Guards also cover middleware-only routes and top-level static/upgrade registration changes.
  const routeSources = [
    path.join(root, 'src/web-server/index.ts'),
    path.join(root, 'src/web-server/rust-service.ts'),
    path.join(root, 'src/web-server/rust-service-lifecycle.ts'),
    path.join(root, 'src/web-server/rust-service-transport.ts'),
    path.join(root, 'src/web-server/rust-service-supervisor.ts'),
    path.join(root, 'src/web-server/rust-service-health.ts'),
    path.join(root, 'src/web-server/rust-service-routes.json'),
    ...fs
      .readdirSync(path.join(root, 'src/web-server/routes'))
      .filter((f) => f.endsWith('.ts'))
      .map((f) => path.join(root, 'src/web-server/routes', f)),
    path.join(root, 'src/web-server/usage-hub/usage-hub-router.ts'),
  ].sort();
  emit(
    'route-source-hashes.json',
    Object.fromEntries(
      routeSources.map((file) => [
        path.relative(root, file),
        createHash('sha256').update(fs.readFileSync(file)).digest('hex'),
      ])
    )
  );
  for (const [index, route] of inventory.entries())
    for (const role of roles) {
      const auth = await import('../../src/web-server/middleware/auth-middleware');
      for (const ip of ['127.0.0.1', '::ffff:127.0.0.1', '::1']) auth.loginRateLimiter.resetKey(ip);
      await (
        await import('../../src/web-server/routes/auth-rate-limits')
      ).resetAuthRateLimitsForTests();
      const headers: any = { origin: base, 'content-type': 'application/json' };
      if (role === 'session' || role === 'bar') {
        const login = await fetch(base + '/api/auth/login', {
          method: 'POST',
          headers,
          body: JSON.stringify({ username: 'fixture-admin', password: 'fixture-password-only' }),
        });
        assert.equal(login.status, 200, await login.text());
        headers.cookie = login.headers.get('set-cookie')!.split(';')[0];
        if (role === 'bar') headers['x-ccs-bar-nonce'] = 'f'.repeat(32);
      }
      if (role === 'device') {
        const paired = await pairDevice({
          name: 'Fixture device',
          platform: 'mac',
          installId: 'fixture-install',
          appVersion: '0.0.0',
          address: '127.0.0.1',
          transport: 'loopback',
        });
        headers.authorization = `Bearer ${paired.token}`;
      }
      if (role === 'hub') headers.authorization = `Bearer ${hubKey}`;
      let url = route.path
        .replace('/{*splat}', route.method === 'ALL' ? '/fixture-missing' : '/')
        .replace(/:[A-Za-z]+/g, 'fixture-missing');
      if (route.path.includes('/@regex:')) url = '/api/status/files/fixture/missing.txt';
      // Invalid query explicitly exercises validation before costly collectors on read routes.
      if (/accounts\/(dashboard|analytics)$/.test(url)) url += '?fixture=invalid';
      if (route.method === 'UPGRADE') {
        const { default: WebSocket } = await import('ws');
        const status = await new Promise<number>((resolve, reject) => {
          const socket = new WebSocket(base.replace('http:', 'ws:') + '/ws', {
            headers,
            handshakeTimeout: 5000,
          });
          socket.on('open', () => {
            resolve(101);
            socket.terminate();
          });
          socket.on('unexpected-response', (_req, response) => {
            resolve(response.statusCode!);
            response.resume();
            socket.terminate();
          });
          socket.on('error', (error) => {
            if (!String(error).includes('closed before')) reject(error);
          });
        });
        emit(`contract/${String(index).padStart(3, '0')}-${role}.json`, {
          request: { method: 'UPGRADE', path: '/ws', role },
          response: { status },
        });
        continue;
      }
      const method = route.method === 'ALL' ? 'POST' : route.method;
      // The Codex summary's 5-second memo would otherwise answer /api/codex/profiles from the
      // quota route's read, making the Rust reply count depend on timing.
      codexSummary.invalidateCodexAuthProfilesCache();
      const response = await fetch(base + url, {
        method,
        headers,
        ...(!['GET', 'HEAD', 'OPTIONS', 'UPGRADE'].includes(route.method) ? { body: '{}' } : {}),
        redirect: 'manual',
        signal: AbortSignal.timeout(5000),
      });
      const text = await response.text();
      let body: any;
      try {
        body = JSON.parse(text);
      } catch {
        body = text;
      }
      const selected = Object.fromEntries(
        [
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
          'allow',
        ]
          .filter((k) => response.headers.has(k))
          .map((k) => [k, response.headers.get(k)])
      );
      emit(`contract/${String(index).padStart(3, '0')}-${role}.json`, {
        request: {
          method: route.method,
          path: url,
          role,
          ...(!['GET', 'HEAD', 'OPTIONS', 'UPGRADE'].includes(route.method) ? { body: {} } : {}),
        },
        response: normalize({ status: response.status, headers: selected, body }),
      });
    }
  assert.equal(
    // Route/role documents only; named m3-*.json corpora belong to their own parity checks.
    fs.readdirSync(path.join(spec, 'contract')).filter((file) => /^\d{3}-[a-z-]+\.json$/.test(file))
      .length,
    inventory.length * roles.length,
    'Stale or missing golden files'
  );
  if (rustMode === 'on')
    assert.equal(
      instance.rustService.stats.served,
      27,
      'must actually serve health/settings/diag, the session/bar visibility, preferences and registry reads, and the session/bar (and device-scoped) profile reads from Rust after TS role guards'
    );
  if (rustMode === 'shadow') {
    // The three M0 store-status stubs explicitly fall back; Status/Scoreboard stubs do not provide
    // raw reader inputs. The six session/bar account reads (visibility, preferences, registry) are
    // compared; the Analytics stub stops at its query check before any Rust composition.
    // Pair-2 M1 keeps standalone profiles in TS until a minimal shadow DTO exists. These twelve
    // authenticated profile calls must decline before building or sending a private input.
    for (let i = 0; i < 100 && instance.rustService.stats.shadowed < 18; i++)
      await new Promise((r) => setTimeout(r, 10));
    assert.equal(instance.rustService.stats.shadowed, 18);
    assert.equal(instance.rustService.stats.served, 0);
    assert.equal(instance.rustService.stats.differences, 0);
    const counters = instance.rustService.diagnostics().routes as Record<
      string,
      { unavailable?: Record<string, number>; equal?: number; different?: number; served?: number }
    >;
    for (const [route, declined] of [
      ['/api/claude/desktop-profiles', 3],
      ['/api/codex/profiles', 2],
      ['/api/codex/profiles/quotas', 2],
      ['/api/antigravity/profiles', 3],
      ['/api/antigravity/profiles/quotas', 2],
    ] as const) {
      assert.deepEqual(counters[route]?.unavailable, { private_input: declined }, route);
      assert.equal(counters[route]?.equal ?? 0, 0, route);
      assert.equal(counters[route]?.different ?? 0, 0, route);
      assert.equal(counters[route]?.served ?? 0, 0, route);
    }
  }
  if (rustMode === 'off') assert.equal(instance.rustService.stats.served, 0);
  // Caller headers cannot create a principal or bypass the TS session/query/origin guards.
  const forged = {
    'x-aac-principal': JSON.stringify({ kind: 'session', id: 'forged', role: 'admin' }),
    'x-aac-boot-secret': 'f'.repeat(64),
  };
  for (const route of ['/api/accounts/settings', '/api/diag'])
    assert.equal((await fetch(base + route, { headers: forged })).status, 401);
  const auth = await import('../../src/web-server/middleware/auth-middleware');
  for (const ip of ['127.0.0.1', '::ffff:127.0.0.1', '::1']) auth.loginRateLimiter.resetKey(ip);
  await (
    await import('../../src/web-server/routes/auth-rate-limits')
  ).resetAuthRateLimitsForTests();
  const login = await fetch(base + '/api/auth/login', {
    method: 'POST',
    headers: { origin: base, 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'fixture-admin', password: 'fixture-password-only' }),
  });
  assert.equal(login.status, 200);
  const cookie = login.headers.get('set-cookie')!.split(';')[0];
  const headers = { cookie, ...forged };
  assert.equal(
    await (await fetch(base + '/api/health', { headers: forged })).text(),
    '{"status":"ok"}'
  );
  assert.equal(
    await (await fetch(base + '/api/accounts/settings', { headers })).text(),
    '{"refreshIntervalSeconds":60}'
  );
  assert.equal(
    (await fetch(base + '/api/accounts/settings?unexpected=1', { headers })).status,
    400
  );
  assert.equal(
    (
      await fetch(base + '/api/accounts/settings', {
        headers: { ...headers, origin: 'https://fixture-attacker.invalid' },
        method: 'PUT',
        body: '{}',
      })
    ).status,
    403
  );
  assert.equal(
    await (await fetch(base + '/api/accounts/settings/', { headers })).text(),
    '{"refreshIntervalSeconds":60}'
  );
  assert.equal(
    (await fetch(base + '/api/accounts/settings', { headers, method: 'HEAD' })).status,
    200
  );
  if (rustMode !== 'off') {
    const { RustService } = await import('../../src/web-server/rust-service');
    const attached = new RustService('on');
    const descriptor = path.join(ccs, 'run/aac-http.json');
    const before = fs.readFileSync(descriptor, 'utf8');
    await attached.start(rustBinary);
    assert.equal(
      fs.readFileSync(descriptor, 'utf8'),
      before,
      'healthy boot is attached without replacing secret'
    );
    attached.stop();
    assert.equal(
      (await fetch(base + '/api/health')).status,
      200,
      'attachment cleanup leaves owner alive'
    );
    // L1: the connection is kept alive, so a removed socket file no longer breaks it. Kill our
    // own service child instead: a real crash must take the TS fallback until the restart.
    const lease = (instance.rustService as unknown as { lease: { pid: number } | null }).lease;
    assert.ok(lease?.pid, 'the front door owns its fixture boot');
    process.kill(lease.pid, 'SIGKILL');
    for (
      let i = 0;
      i < 100 && (instance.rustService as unknown as { lease: unknown }).lease === lease;
      i++
    )
      await new Promise((r) => setTimeout(r, 10));
    const fallbacks = instance.rustService.stats.fallbacks;
    assert.equal(
      await (await fetch(base + '/api/accounts/settings', { headers })).text(),
      '{"refreshIntervalSeconds":60}'
    );
    for (let i = 0; i < 100 && instance.rustService.stats.fallbacks === fallbacks; i++)
      await new Promise((r) => setTimeout(r, 10));
    assert.ok(
      instance.rustService.stats.fallbacks > fallbacks,
      'unhealthy service logs and serves TS'
    );
  }
  await (await import('./rust-proxy')).checkRustProxyBoundary(home);
  console.log(
    `${inventory.length} routes x ${roles.length} roles captured; rustService=${rustMode}, routing and boundary checks passed`
  );
} finally {
  instance.cleanup();
  instance.wss.close();
  instance.server.closeAllConnections();
  await new Promise<void>((resolve) => instance.server.close(() => resolve()));
  const { settleAuthWrites } = await import('../../src/web-server/services/dashboard-auth-files');
  await settleAuthWrites();
  fs.rmSync(home, { recursive: true, force: true });
}
