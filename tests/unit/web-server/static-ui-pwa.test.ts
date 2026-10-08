/**
 * Serving the installable PWA shell through the real startServer() stack with
 * dashboard auth enabled: the manifest, the stamped worker and its routing
 * module, and every icon the manifest names answer 200 with their own content
 * types while signed out (no session cookie), while /api stays behind the
 * auth wall. The UI is produced by the real scripts/build-ui.js with a fake
 * wasm-pack, in a temporary directory; CCS_HOME is temporary too.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import bcrypt from 'bcrypt';
import fs from 'fs';
import http from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';

import { startServer } from '../../../src/web-server';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';

const { buildUi } = require('../../../scripts/build-ui.js');

const REPO_ROOT = path.resolve(__dirname, '..', '..', '..');
const REAL_PUBLIC = path.join(REPO_ROOT, 'web-dashboard', 'public');
const WASM = Buffer.concat([
  Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]),
  Buffer.from('slint-dashboard-pwa-fixture-'.repeat(512)),
]);
const INDEX = `<!doctype html><canvas id="slint-dashboard"></canvas>${'<!-- page -->'.repeat(120)}<script type="module" src="/bridge.js"></script>`;
const MANIFEST = JSON.parse(
  fs.readFileSync(path.join(REAL_PUBLIC, 'manifest.webmanifest'), 'utf8')
);
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

let tempRoot = '';
let staticDir = '';
let manifest: { buildId: string };
let instance: Awaited<ReturnType<typeof startServer>> | undefined;
let port = 0;
let originalEnvironment: Record<string, string | undefined> = {};

function buildFixtureUi(root: string): typeof manifest {
  const crate = path.join(root, 'web-dashboard');
  for (const directory of ['public', 'pkg', 'tests'])
    fs.mkdirSync(path.join(crate, directory), { recursive: true });
  fs.writeFileSync(
    path.join(crate, 'Cargo.toml'),
    '[dependencies]\nslint = { version = "=1.18.1" }\n[build-dependencies]\nslint-build = "=1.18.1"\n'
  );
  fs.writeFileSync(
    path.join(crate, 'Cargo.lock'),
    '[[package]]\nname = "slint"\nversion = "1.18.1"\n[[package]]\nname = "slint-build"\nversion = "1.18.1"\n'
  );
  fs.writeFileSync(path.join(crate, 'public', 'index.html'), INDEX);
  fs.writeFileSync(
    path.join(crate, 'public', 'bridge.js'),
    "import init from './pkg/ccs_account_dashboard.js';\ninit();\n"
  );
  // The real PWA shell files, so the real manifest, worker and icons are served.
  for (const file of ['manifest.webmanifest', 'sw.js', 'sw-route.js']) {
    fs.copyFileSync(path.join(REAL_PUBLIC, file), path.join(crate, 'public', file));
  }
  fs.cpSync(path.join(REAL_PUBLIC, 'icons'), path.join(crate, 'public', 'icons'), {
    recursive: true,
  });
  fs.mkdirSync(path.join(crate, 'public', 'assets'), { recursive: true });
  fs.copyFileSync(
    path.join(REAL_PUBLIC, 'assets', 'favicon.svg'),
    path.join(crate, 'public', 'assets', 'favicon.svg')
  );
  fs.writeFileSync(
    path.join(crate, 'pkg', 'ccs_account_dashboard.js'),
    `export default async function init() {}\n${'// glue\n'.repeat(200)}`
  );
  fs.writeFileSync(path.join(crate, 'pkg', 'ccs_account_dashboard_bg.wasm'), WASM);
  return buildUi({
    repoRoot: root,
    run: (command: string) =>
      command.includes('rustc')
        ? 'rustc 1.98.0 (fixture)'
        : command.includes('rustup')
          ? 'wasm32-unknown-unknown\n'
          : '',
    readCommit: () => '8fb5e3de',
  });
}

async function start(): Promise<void> {
  instance = await startServer({ port: 0, host: '127.0.0.1', staticDir });
  port = (instance.server.address() as AddressInfo).port;
}

async function stop(): Promise<void> {
  if (!instance) return;
  instance.cleanup();
  const server = instance.server;
  instance = undefined;
  await new Promise<void>((resolve) => server.close(() => resolve()));
}

interface RawResponse {
  status: number;
  headers: http.IncomingHttpHeaders;
  body: Buffer;
}

/** A raw request: no cookies, no automatic decompression, no path normalisation. */
function raw(
  method: string,
  route: string,
  headers: Record<string, string> = {},
  body?: string | Buffer
): Promise<RawResponse> {
  return new Promise((resolve, reject) => {
    const request = http.request(
      { host: '127.0.0.1', port, method, path: route, headers, agent: false },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.on('end', () =>
          resolve({
            status: response.statusCode ?? 0,
            headers: response.headers,
            body: Buffer.concat(chunks),
          })
        );
        response.on('error', reject);
      }
    );
    request.on('error', reject);
    if (body !== undefined) request.write(body);
    request.end();
  });
}

beforeEach(async () => {
  originalEnvironment = Object.fromEntries(
    FIXTURE_ENVIRONMENT.map((name) => [name, process.env[name]])
  );
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-static-ui-pwa-'));
  process.env.CCS_HOME = path.join(tempRoot, 'home');
  process.env.CCS_DIR = path.join(tempRoot, 'home', '.ccs');
  process.env.CODEX_HOME = path.join(tempRoot, 'home', '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(tempRoot, 'home', '.claude');
  // Dashboard auth on, but no request below carries a session cookie.
  process.env.CCS_DASHBOARD_AUTH_ENABLED = 'true';
  process.env.CCS_DASHBOARD_USERNAME = 'aac-test-admin';
  process.env.CCS_DASHBOARD_PASSWORD_HASH = await bcrypt.hash('pwa-fixture-password', 4);
  delete process.env.CCS_SESSION_SECRET;
  spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {});
  spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {});
  manifest = buildFixtureUi(tempRoot);
  staticDir = path.join(tempRoot, 'dist', 'ui');
  await start();
});

afterEach(async () => {
  try {
    await stop();
  } finally {
    mock.restore();
    for (const name of FIXTURE_ENVIRONMENT) {
      const value = originalEnvironment[name];
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
});

describe('PWA shell serving while signed out', () => {
  it('serves the manifest with the manifest content type', async () => {
    const response = await raw('GET', '/manifest.webmanifest');
    expect(response.status).toBe(200);
    expect(response.headers['content-type']).toBe('application/manifest+json');
    expect(response.headers['cache-control']).toBe('no-cache');
    const served = JSON.parse(response.body.toString('utf8'));
    expect(served).toEqual(MANIFEST);
    expect(served.name).toBe('AI Account Center');
    expect(served.short_name).toBe('AAC');
    expect(served.start_url).toBe('/');
    expect(served.display).toBe('standalone');
  });

  it('serves the stamped worker and its routing module as JavaScript', async () => {
    const worker = await raw('GET', '/sw.js');
    expect(worker.status).toBe(200);
    expect(worker.headers['content-type']).toBe('text/javascript; charset=utf-8');
    expect(worker.headers['cache-control']).toBe('no-cache');
    expect(worker.body.toString('utf8')).toContain(`'${manifest.buildId}'`);
    expect(worker.body.toString('utf8')).not.toContain('__AAC_BUILD_ID__');
    const routing = await raw('GET', '/sw-route.js');
    expect(routing.status).toBe(200);
    expect(routing.headers['content-type']).toBe('text/javascript; charset=utf-8');
  });

  it('serves every icon the manifest names', async () => {
    expect(Array.isArray(MANIFEST.icons)).toBe(true);
    expect(MANIFEST.icons.length).toBeGreaterThan(0);
    for (const icon of MANIFEST.icons) {
      const response = await raw('GET', icon.src);
      expect([icon.src, response.status]).toEqual([icon.src, 200]);
      expect([icon.src, response.headers['content-type']]).toEqual([icon.src, icon.type]);
      expect(response.body.length).toBeGreaterThan(0);
    }
  });

  it('serves root icon aliases without a session and preserves redirects for other routes', async () => {
    for (const route of [
      '/apple-touch-icon.png',
      '/apple-touch-icon-precomposed.png',
      '/apple-touch-icon-180x180.png',
      '/apple-touch-icon-180x180-precomposed.png',
      '/apple-touch-icon-120x120.png',
      '/apple-touch-icon-120x120-precomposed.png',
      '/apple-touch-icon-152x152.png',
      '/apple-touch-icon-152x152-precomposed.png',
      '/apple-touch-icon-167x167.png',
      '/apple-touch-icon-167x167-precomposed.png',
      '/apple-touch-icon-180.png',
      '/favicon.ico',
    ]) {
      const response = await raw('GET', route);
      expect([route, response.status]).toEqual([route, 200]);
      expect(response.headers['content-type']).toBe('image/png');
      expect(response.headers['cache-control']).toBe('no-cache');
      expect(response.body.length).toBeGreaterThan(0);

      const head = await raw('HEAD', route);
      expect([route, head.status]).toEqual([route, 200]);
      expect(head.body.length).toBe(0);
    }

    const other = await raw('GET', '/unknown-icon.png');
    expect(other.status).toBe(302);
    expect(other.headers.location).toBe('/');
  });

  it('keeps /api behind the auth wall while the shell stays public', async () => {
    const api = await raw('GET', '/api/accounts/registry');
    expect(api.status).toBe(401);
    expect(api.headers['content-type']).toContain('application/json');
  });
});
