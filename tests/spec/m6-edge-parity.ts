/**
 * M6 edge contracts: the T3 usage hub (`/v0/management/*`), the dashboard's static files and Leptos bundle
 * (frame pages, cache headers, the service worker, the PWA manifest and icons, the JSON 404s), and the CLI entry
 * points (`ai-account-center dashboard`, the `config` alias, the retired-command and retired-bin guidance).
 *
 * Today's TS server records these goldens (`contract/m6-edge.json`). Runtime data (the hub answers, the JSON 404s,
 * the redirects, the empty HEAD/304 bodies, the CLI output) is compared byte for byte. A built asset (frame pages,
 * stylesheet, the pkg files, sw.js, the PWA manifest and icons) is not frozen: the served bytes must equal the
 * packaged file itself under the static root, so a rebuild moves both sides together. Status, the selected headers
 * and the cache policy are always exact.
 *
 * The HTTP cases replay against another front door with `AAC_M6_TARGET_URL` (lane M's Rust service) plus
 * `AAC_M6_STATIC_DIR` pointing at the target's packaged UI, and the CLI cases against another executable with
 * `AAC_M6_TARGET_CLI`. A target must run on the same fixture home with the same synthetic dashboard rows
 * (`m6-dashboard.json`) and the same seeded management key, which is what makes the bodies comparable.
 *
 * Product modules are imported only after the temporary home, environment and dashboard stub exist: their module
 * side effects (config paths, the hub's account source) read the home at load time, so static imports would pin the
 * real one. `--regenerate` refuses to run against another target.
 */
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

import { fakeRun, writeLeptosFixture } from '../shared/fixtures/leptos-ui-fixture';
import type {
  AccountDashboard,
  DashboardAccount,
} from '../../src/web-server/services/account-dashboard-types';
import {
  CLAUDE_USAGE_URL,
  CODEX_CREDITS_URL,
  CODEX_USAGE_URL,
} from '../../src/web-server/usage-hub/usage-hub-contract';
import { usageHubAuthIndex } from '../../src/web-server/usage-hub/usage-hub-projection';

const repo = fileURLToPath(new URL('../..', import.meta.url));
const spec = path.join(repo, 'tests/spec');
const contractFile = path.join(spec, 'contract/m6-edge.json');
const dashboardFile = path.join(spec, 'm6-dashboard.json');
const fixtureFile = path.join(spec, 'fixture.json');
const KEY = 'aacu_' + 'f'.repeat(43);
const KEY_ALT = 'aacu_' + 'b'.repeat(43);
const WORKER = process.argv.indexOf('--worker');
const REGENERATE = process.argv.includes('--regenerate');
const SELECTED_HEADERS = [
  'content-type',
  'cache-control',
  'content-encoding',
  'vary',
  'etag',
  'location',
  'x-content-type-options',
  'content-security-policy',
] as const;

interface HttpCase {
  name: string;
  method: string;
  path: string;
  headers?: Record<string, string>;
  body?: string;
  /** Read the path once, then send its real ETag as If-None-Match. */
  revalidate?: boolean;
  /**
   * A built asset: the served bytes must equal this packaged file under the static root (after decoding), so a
   * rebuild changes both sides together instead of breaking a frozen copy. `<build-id>` is filled in.
   */
  disk?: string;
}

interface CliCase {
  name: string;
  argv: string[];
  /** The legacy bin names only exist in the TS package. */
  tsOnly?: boolean;
}

interface Recorded {
  case: string;
  request: { method: string; path: string; headers: Record<string, string> };
  response: { status: number; headers: Record<string, string>; body: unknown };
}

interface CliRecorded {
  case: string;
  argv: string[];
  status: number | null;
  stdout: string;
  stderr: string;
}

interface Golden {
  version: 1;
  http: Recorded[];
  cli: CliRecorded[];
}

interface NormalizeContext {
  base: string;
  home: string;
  buildId: string;
}

interface CliTarget {
  command: string;
  prefix: string[];
  /** The TS package is the only target that ships the legacy bin names. */
  typescript: boolean;
}

const auth = { authorization: `Bearer ${KEY}` };
const json = { ...auth, 'content-type': 'application/json' };
const codex = usageHubAuthIndex('codex:alpha');
const claude = usageHubAuthIndex('claude:beta');
const gamma = usageHubAuthIndex('codex:gamma');
const usageCall = (authIndex: string, url: string) =>
  JSON.stringify({ auth_index: authIndex, method: 'GET', url });

const HTTP_CASES: HttpCase[] = [
  // The hub guards, in the order usage-hub-router names them.
  { name: 'hub auth-files with a bearer key', method: 'GET', path: '/v0/management/auth-files', headers: auth },
  { name: 'hub auth-files with X-Management-Key', method: 'GET', path: '/v0/management/auth-files', headers: { 'x-management-key': KEY } },
  { name: 'hub api-call: Codex usage', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(codex, CODEX_USAGE_URL) },
  { name: 'hub api-call: Claude usage', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(claude, CLAUDE_USAGE_URL) },
  { name: 'hub api-call: an account with no reading answers 503 upstream', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(gamma, CODEX_USAGE_URL) },
  { name: 'hub api-call: an unknown auth_index', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall('fixture-missing', CODEX_USAGE_URL) },
  { name: 'hub api-call: a provider mismatch', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(codex, CLAUDE_USAGE_URL) },
  { name: 'hub api-call: a foreign URL is not proxied', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(codex, 'https://example.com/usage') },
  { name: 'hub api-call: reset credits are refused', method: 'POST', path: '/v0/management/api-call', headers: json, body: usageCall(codex, CODEX_CREDITS_URL) },
  { name: 'hub api-call: a write is refused', method: 'POST', path: '/v0/management/api-call', headers: json, body: JSON.stringify({ auth_index: codex, method: 'POST', url: CODEX_USAGE_URL, data: {} }) },
  { name: 'hub api-call: invalid JSON', method: 'POST', path: '/v0/management/api-call', headers: json, body: '{' },
  { name: 'hub reset-quota is refused', method: 'POST', path: '/v0/management/reset-quota', headers: json, body: '{}' },
  { name: 'hub: no key', method: 'GET', path: '/v0/management/auth-files' },
  { name: 'hub: a wrong key', method: 'GET', path: '/v0/management/auth-files', headers: { authorization: `Bearer ${KEY_ALT}` } },
  { name: 'hub: a key in the query string is refused', method: 'GET', path: `/v0/management/auth-files?key=${KEY}` },
  { name: 'hub: any other path', method: 'GET', path: '/v0/management/other', headers: auth },
  { name: 'hub: a method it does not serve', method: 'PUT', path: '/v0/management/auth-files', headers: auth, body: '{}' },

  // Static files and the Leptos bundle: pages, cache policy, encodings and the PWA.
  { name: 'page: /', method: 'GET', path: '/', disk: 'app/index.html' },
  { name: 'page: /app/home', method: 'GET', path: '/app/home', disk: 'app/index.html' },
  { name: 'page: /login (bookmark alias)', method: 'GET', path: '/login', disk: 'app/index.html' },
  { name: 'page: HEAD', method: 'HEAD', path: '/app/home' },
  { name: 'frame stylesheet revalidates', method: 'GET', path: '/app/app.css', disk: 'app/app.css' },
  { name: 'classic upgrade module is a script', method: 'GET', path: '/bridge.js', disk: 'bridge.js' },
  { name: 'pkg: identity', method: 'GET', path: '/pkg/<build-id>/aac_web.js', disk: 'pkg/<build-id>/aac_web.js' },
  { name: 'pkg: brotli variant', method: 'GET', path: '/pkg/<build-id>/aac_web.js', headers: { 'accept-encoding': 'br' }, disk: 'pkg/<build-id>/aac_web.js' },
  { name: 'pkg: gzip variant', method: 'GET', path: '/pkg/<build-id>/aac_web.js', headers: { 'accept-encoding': 'gzip' }, disk: 'pkg/<build-id>/aac_web.js' },
  { name: 'pkg: wasm', method: 'GET', path: '/pkg/<build-id>/aac_web_bg.wasm', headers: { 'accept-encoding': 'br' }, disk: 'pkg/<build-id>/aac_web_bg.wasm' },
  { name: 'pkg: a matching ETag revalidates', method: 'GET', path: '/pkg/<build-id>/aac_web.js', headers: { 'accept-encoding': 'gzip' }, revalidate: true },
  { name: 'pkg: a precompressed file is never served directly', method: 'GET', path: '/pkg/<build-id>/aac_web.js.br' },
  { name: 'the build manifest is never served', method: 'GET', path: '/ui-build-manifest.json' },
  { name: 'service worker', method: 'GET', path: '/sw.js', disk: 'sw.js' },
  { name: 'service worker routing module', method: 'GET', path: '/sw-route.js', disk: 'sw-route.js' },
  { name: 'PWA manifest', method: 'GET', path: '/manifest.webmanifest', disk: 'manifest.webmanifest' },
  { name: 'PWA icon', method: 'GET', path: '/icons/aac-192.png', disk: 'icons/aac-192.png' },
  { name: 'root icon alias', method: 'GET', path: '/favicon.ico', disk: 'icons/apple-touch-icon-180-v2.png' },
  { name: 'an unknown API path answers JSON 404', method: 'GET', path: '/api/definitely-not-a-route' },
  { name: 'an unknown page goes back to /', method: 'GET', path: '/definitely-not-a-page' },
  { name: 'an unknown asset goes back to /', method: 'GET', path: '/app/definitely-not-there.js' },
  { name: 'dot segments never escape the static folder', method: 'GET', path: '/app/%2e%2e/%2e%2e/package.json' },
];

const CLI_CASES: CliCase[] = [
  { name: 'cli: help', argv: ['--help'] },
  { name: 'cli: version and paths', argv: ['--version'] },
  { name: 'cli: dashboard help', argv: ['dashboard', '--help'] },
  { name: 'cli: config is the dashboard alias', argv: ['config', '--help'] },
  { name: 'cli: retired self-update', argv: ['update'] },
  { name: 'cli: retired profile workflow', argv: ['launch', 'work'] },
  { name: 'cli: retired legacy bin', argv: [], tsOnly: true },
];

function readDashboard(): DashboardAccount[] {
  // Committed synthetic fixture owned by this suite: the parsed shape is the input, so a direct cast is enough.
  const parsed = JSON.parse(fs.readFileSync(dashboardFile, 'utf8')) as { accounts: DashboardAccount[] };
  return parsed.accounts;
}

function dashboard(accounts: DashboardAccount[]): AccountDashboard {
  return {
    schemaVersion: 1,
    updatedAt: '2026-10-06T12:00:00Z',
    accounts,
    codexAutoSwitch: {
      enabled: false,
      thresholdPercent: 20,
      thresholdUsedPercent: 80,
      pollIntervalSeconds: 60,
      outcome: 'disabled',
      message: '',
      activationInProgress: false,
    },
  };
}

function text(value: string, ctx: NormalizeContext): string {
  let out = value;
  if (ctx.base) out = out.split(ctx.base).join('<origin>');
  if (ctx.home) {
    out = out.split(ctx.home).join('<fixture-home>');
    out = out.split(ctx.home.split(path.sep).join('/')).join('<fixture-home>');
  }
  out = out.split(ctx.buildId).join('<build-id>');
  out = out.split(KEY).join('<management-key>');
  return out.split(repo).join('<repo>');
}

/** Keys sorted and strings normalized the way the M0 contract corpus does it. */
function normalize(value: unknown, ctx: NormalizeContext): unknown {
  if (typeof value === 'string') return text(value, ctx);
  if (Array.isArray(value)) return value.map((entry) => normalize(entry, ctx));
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.entries(value)
        .sort(([a], [b]) => a.localeCompare(b, 'en'))
        .map(([key, entry]) => [key, normalize(entry, ctx)])
    );
  return value;
}

function normalizeHeaders(
  headers: Record<string, string>,
  ctx: NormalizeContext
): Record<string, string> {
  return Object.fromEntries(
    Object.entries(headers)
      .map(([name, value]) => [name, name === 'etag' ? '<etag>' : text(value, ctx)])
      .sort(([a], [b]) => a.localeCompare(b, 'en'))
  );
}

function normalizeRecord(row: Recorded, ctx: NormalizeContext): Recorded {
  // Keys in the corpus order (sorted): headers, method, path / body, headers, status.
  return {
    case: text(row.case, ctx),
    request: {
      headers: normalizeHeaders(row.request.headers, ctx),
      method: row.request.method,
      path: text(row.request.path, ctx),
    },
    response: {
      body: normalize(row.response.body, ctx),
      headers: normalizeHeaders(row.response.headers, ctx),
      status: row.response.status,
    },
  };
}

function selectedHeaders(headers: Headers): Record<string, string> {
  const recorded: Record<string, string> = {};
  for (const name of SELECTED_HEADERS) {
    const value = headers.get(name);
    if (value !== null) recorded[name] = name === 'etag' ? '<etag>' : value;
  }
  return recorded;
}

function bodyOf(bytes: Buffer, contentType: string | null): unknown {
  const type = (contentType ?? '').toLowerCase();
  if (type.startsWith('application/json')) return JSON.parse(bytes.toString('utf8'));
  if (/^(text\/|application\/(manifest\+json|javascript))/.test(type))
    return bytes.toString('utf8');
  return { sha256: createHash('sha256').update(bytes).digest('hex'), bytes: bytes.length };
}

async function runHttp(
  base: string,
  ctx: { home: string; buildId: string; staticDir: string }
): Promise<Recorded[]> {
  const normalizeContext: NormalizeContext = { base, home: ctx.home, buildId: ctx.buildId };
  const out: Recorded[] = [];
  for (const testCase of HTTP_CASES) {
    const route = testCase.path.replaceAll('/<build-id>/', `/${ctx.buildId}/`);
    const headers: Record<string, string> = { ...testCase.headers };
    if (testCase.revalidate) {
      const first = await fetch(base + route, { headers, redirect: 'manual' });
      await first.arrayBuffer();
      const etag = first.headers.get('etag');
      assert.ok(etag, `${testCase.name}: the first answer carried no ETag`);
      headers['if-none-match'] = etag;
    }
    const response = await fetch(base + route, {
      method: testCase.method,
      headers,
      body: testCase.body,
      redirect: 'manual',
    });
    const bytes = Buffer.from(await response.arrayBuffer());
    let body: unknown;
    if (testCase.disk) {
      // A built asset: the answer must be the packaged file itself, decoded. Rebuilds move both sides together.
      const relative = testCase.disk.replaceAll('<build-id>', ctx.buildId);
      const file = path.join(ctx.staticDir, ...relative.split('/'));
      const packaged = fs.readFileSync(file);
      assert.equal(
        bytes.length,
        packaged.length,
        `${testCase.name}: served ${bytes.length} bytes, but ${relative} is ${packaged.length}`
      );
      assert.equal(
        createHash('sha256').update(bytes).digest('hex'),
        createHash('sha256').update(packaged).digest('hex'),
        `${testCase.name}: served bytes differ from ${relative}`
      );
      body = { disk: testCase.disk };
    } else {
      body = bodyOf(bytes, response.headers.get('content-type'));
    }
    out.push(
      normalizeRecord(
        {
          case: testCase.name,
          request: { method: testCase.method, path: testCase.path, headers: testCase.headers ?? {} },
          response: {
            status: response.status,
            headers: selectedHeaders(response.headers),
            body,
          },
        },
        normalizeContext
      )
    );
    const shape = typeof body === 'object' && body !== null && 'bytes' in body ? 'binary' : 'text';
    process.stdout.write(`[m6] ${response.status} ${testCase.method} ${testCase.path} (${shape})\n`);
  }
  return out;
}

function runCli(target: CliTarget, argv: string[], home: string): CliRecorded {
  const env: Record<string, string | undefined> = {
    ...process.env,
    CCS_HOME: home,
    CODEX_HOME: path.join(home, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
    CI: '1',
    NO_COLOR: '1',
  };
  delete env.CCS_DIR;
  delete env.CCS_SESSION_SECRET;
  const result = spawnSync(target.command, [...target.prefix, ...argv], {
    cwd: repo,
    env,
    encoding: 'utf8',
    timeout: 30_000,
  });
  assert.ok(!result.error, `${argv.join(' ')} failed to start: ${result.error?.message}`);
  // Keys in the corpus order (sorted).
  return {
    argv,
    case: '',
    status: result.status,
    stderr: result.stderr ?? '',
    stdout: result.stdout ?? '',
  };
}

/** A legacy bin name is the file it is invoked as: a tiny wrapper makes the guidance name it. */
function compatWrapper(home: string): CliTarget {
  const wrapper = path.join(home, 'ccs-droid.js');
  fs.writeFileSync(
    wrapper,
    `require(${JSON.stringify(path.join(repo, 'dist/bin/compat-cli.js'))});\n`
  );
  return { command: process.execPath, prefix: [wrapper], typescript: true };
}

async function runCliCases(target: CliTarget, home: string): Promise<CliRecorded[]> {
  const ctx: NormalizeContext = { base: '', home, buildId: '' };
  const out: CliRecorded[] = [];
  for (const testCase of CLI_CASES) {
    if (testCase.tsOnly && !target.typescript) continue;
    const recorded = runCli(testCase.tsOnly ? compatWrapper(home) : target, testCase.argv, home);
    recorded.case = testCase.name;
    recorded.stdout = text(recorded.stdout, ctx);
    recorded.stderr = text(recorded.stderr, ctx);
    out.push(recorded);
    process.stdout.write(`[m6] cli ${testCase.name} → ${recorded.status}\n`);
  }
  return out;
}

async function worker(reportPath: string): Promise<void> {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m6-edge-'));
  const ccs = path.join(home, '.ccs');
  fs.mkdirSync(ccs, { recursive: true, mode: 0o700 });
  fs.copyFileSync(fixtureFile, path.join(ccs, 'fixture.json'));
  for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
  Object.assign(process.env, {
    CCS_HOME: home,
    CCS_DIR: ccs,
    CODEX_HOME: path.join(home, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
    AAC_NO_LOCAL_RUST_BUILD: '1',
  });

  const skeleton = path.join(home, 'repo');
  fs.mkdirSync(path.join(skeleton, 'web-dashboard'), { recursive: true });
  fs.cpSync(path.join(repo, 'web-dashboard/public'), path.join(skeleton, 'web-dashboard/public'), {
    recursive: true,
  });
  writeLeptosFixture(skeleton);
  const require = createRequire(import.meta.url);
  const ui = require('../../scripts/build-ui.js') as {
    buildUi: (options: {
      repoRoot: string;
      run: typeof fakeRun;
      readCommit: () => string;
    }) => { buildId: string };
  };
  const built = ui.buildUi({ repoRoot: skeleton, run: fakeRun, readCommit: () => '8fb5e3de' });
  const staticDir = path.join(skeleton, 'dist/ui');

  const keyStore = await import('../../src/web-server/usage-hub/usage-hub-key-store');
  fs.mkdirSync(path.join(ccs, 'auth'), { recursive: true, mode: 0o700 });
  fs.writeFileSync(
    keyStore.usageHubKeyPath(),
    JSON.stringify({
      version: 1,
      keySha256: keyStore.hashUsageHubKey(KEY),
      createdAt: '2026-01-01T00:00:00.000Z',
    }),
    { mode: 0o600 }
  );

  const { spyOn } = await import('bun:test');
  const dashboardService = await import('../../src/web-server/services/account-dashboard-service');
  const mac = dashboard(readDashboard());
  const windows = dashboard([]);
  spyOn(dashboardService, 'peekAccountDashboard').mockImplementation(async (platform: string) =>
    platform === 'windows' ? windows : mac
  );

  const { startServer } = await import('../../src/web-server');
  const instance = await startServer({ port: 0, host: '127.0.0.1', staticDir });
  const address = instance.server.address();
  assert.ok(address && typeof address === 'object', 'the fixture server did not bind');
  const base = `http://127.0.0.1:${address.port}`;
  try {
    const http = await runHttp(base, { home, buildId: built.buildId, staticDir });
    const distCli = path.join(repo, 'dist/ccs.js');
    assert.ok(fs.existsSync(distCli), 'the CLI goldens need the server build: run bun run build:server');
    const cliHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m6-cli-'));
    let cli: CliRecorded[];
    try {
      cli = await runCliCases(
        { command: process.execPath, prefix: [distCli], typescript: true },
        cliHome
      );
    } finally {
      fs.rmSync(cliHome, { recursive: true, force: true });
    }
    fs.writeFileSync(
      reportPath,
      JSON.stringify({ buildId: built.buildId, http, cli }, null, 2) + '\n'
    );
  } finally {
    instance.cleanup();
    await new Promise<void>((resolve) => instance.server.close(() => resolve()));
    fs.rmSync(home, { recursive: true, force: true });
  }
}

function readGolden(): Golden {
  if (!fs.existsSync(contractFile)) return { version: 1, http: [], cli: [] };
  return JSON.parse(fs.readFileSync(contractFile, 'utf8')) as Golden;
}

async function main(): Promise<void> {
  const targetUrl = process.env.AAC_M6_TARGET_URL;
  const targetCli = process.env.AAC_M6_TARGET_CLI;
  const golden = readGolden();
  assert.ok(!(REGENERATE && (targetUrl || targetCli)), '--regenerate records from the TS target only');

  if (targetUrl) {
    const index = await fetch(targetUrl + '/', { redirect: 'manual' });
    const html = await index.text();
    const match = /\/pkg\/([a-f0-9]{12})\//.exec(html);
    assert.ok(match, `${targetUrl}: the frame page names no pkg/<build-id> path`);
    const staticDir = process.env.AAC_M6_STATIC_DIR;
    assert.ok(
      staticDir,
      'AAC_M6_TARGET_URL also needs AAC_M6_STATIC_DIR (the target\'s packaged dist/ui) to compare built assets'
    );
    const rows = await runHttp(targetUrl, { home: '', buildId: match[1], staticDir });
    assert.deepEqual(rows, golden.http, `${targetUrl}: the M6 HTTP contract differs from the goldens`);
    process.stdout.write(`[OK] M6 HTTP contract matches at ${targetUrl} (${rows.length} cases).\n`);
  } else {
    const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m6-report-'));
    try {
      const reportPath = path.join(scratch, 'report.json');
      const result = spawnSync('bun', [fileURLToPath(import.meta.url), '--worker', reportPath], {
        cwd: repo,
        env: { ...process.env, AAC_NO_LOCAL_RUST_BUILD: '1' },
        encoding: 'utf8',
        maxBuffer: 32 * 1024 * 1024,
      });
      if (result.status !== 0)
        throw new Error(result.stderr || result.stdout || 'the M6 fixture worker failed');
      process.stdout.write(result.stdout);
      const report = JSON.parse(fs.readFileSync(reportPath, 'utf8')) as {
        http: Recorded[];
        cli: CliRecorded[];
      };
      if (REGENERATE) {
        fs.writeFileSync(
          contractFile,
          JSON.stringify({ cli: report.cli, http: report.http, version: 1 }, null, 2) + '\n'
        );
        process.stdout.write(`[OK] recorded ${contractFile}\n`);
        return;
      }
      assert.deepEqual(report.http, golden.http, 'M6 HTTP contract changed; review and regenerate explicitly');
      if (targetCli) {
        const cliHome = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-m6-cli-'));
        try {
          const rows = await runCliCases(
            { command: targetCli, prefix: [], typescript: false },
            cliHome
          );
          assert.deepEqual(
            rows,
            golden.cli.filter((row) => !CLI_CASES.find((c) => c.name === row.case)?.tsOnly),
            `${targetCli}: the M6 CLI contract differs from the goldens`
          );
        } finally {
          fs.rmSync(cliHome, { recursive: true, force: true });
        }
      } else {
        assert.deepEqual(report.cli, golden.cli, 'M6 CLI contract changed; review and regenerate explicitly');
      }
      process.stdout.write(
        `[OK] M6 edge contract matches (${report.http.length} HTTP, ${report.cli.length} CLI cases).\n`
      );
    } finally {
      fs.rmSync(scratch, { recursive: true, force: true });
    }
  }
}

if (WORKER >= 0) {
  await worker(process.argv[WORKER + 1]);
} else {
  await main();
}
