/**
 * Serving the packaged dashboard through the real startServer() stack:
 * precompressed variants, Cache-Control, page routes, the /api JSON 404 and the
 * JSON error handlers. The UI is produced by the real scripts/build-ui.js with a
 * fake wasm-pack, in a temporary directory; CCS_HOME is temporary too.
 */
import { afterEach, beforeEach, describe, expect, it, mock, spyOn } from 'bun:test';
import crypto from 'crypto';
import fs from 'fs';
import http from 'http';
import type { AddressInfo } from 'net';
import os from 'os';
import path from 'path';
import zlib from 'zlib';

import { requestErrorKind, startServer } from '../../../src/web-server';
import { getRecentLogEntries } from '../../../src/services/logging';
import { CodexAutoSwitchService } from '../../../src/web-server/services/codex-auto-switch-service';
import * as accountAnalytics from '../../../src/web-server/services/account-analytics-service';
import { negotiateEncoding, resolvePageRoute } from '../../../src/web-server/static-ui';

const { buildUi } = require('../../../scripts/build-ui.js');

const WASM = Buffer.concat([
  Buffer.from([0, 97, 115, 109, 1, 0, 0, 0]),
  Buffer.from('slint-dashboard-serving-fixture-'.repeat(512)),
]);
const INDEX = `<!doctype html><canvas id="slint-dashboard"></canvas>${'<!-- page -->'.repeat(120)}<script type="module" src="/bridge.js"></script>`;
const FIXTURE_ENVIRONMENT = [
  'CCS_HOME',
  'CCS_DIR',
  'CODEX_HOME',
  'CLAUDE_CONFIG_DIR',
  'CCS_DASHBOARD_AUTH_ENABLED',
] as const;

let tempRoot = '';
let staticDir = '';
let manifest: {
  buildId: string;
  wasm: { path: string };
  precompressed: Array<{ path: string; encoding: string; file: string; sha256: string }>;
};
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
  fs.writeFileSync(
    path.join(crate, 'public', 'view-model.mjs'),
    'export const rows = [];\n'.repeat(100)
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

/** A raw request: no automatic decompression and no client-side path normalisation. */
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
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'ccs-static-ui-'));
  process.env.CCS_HOME = path.join(tempRoot, 'home');
  process.env.CCS_DIR = path.join(tempRoot, 'home', '.ccs');
  process.env.CODEX_HOME = path.join(tempRoot, 'home', '.codex');
  process.env.CLAUDE_CONFIG_DIR = path.join(tempRoot, 'home', '.claude');
  delete process.env.CCS_DASHBOARD_AUTH_ENABLED;
  spyOn(CodexAutoSwitchService.prototype, 'start').mockImplementation(() => {});
  spyOn(accountAnalytics, 'startAccountAnalyticsSampling').mockImplementation(() => {});
  manifest = buildFixtureUi(tempRoot);
  staticDir = path.join(tempRoot, 'dist', 'ui');
  // A file next to the UI that must never be reachable through a path trick.
  fs.writeFileSync(path.join(tempRoot, 'dist', 'outside-secret.txt'), 'OUTSIDE-SECRET');
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

const wasmUrl = () => `/${manifest.wasm.path}`;
const variant = (encoding: 'br' | 'gzip') =>
  fs.readFileSync(
    path.join(staticDir, `${manifest.wasm.path}${encoding === 'br' ? '.br' : '.gz'}`)
  );
const IMMUTABLE = 'public, max-age=31536000, immutable';

describe('precompressed WebAssembly and immutable pkg caching', () => {
  it('serves Brotli for br, gzip, with the original type and the immutable cache', async () => {
    const response = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br, gzip' });
    expect(response.status).toBe(200);
    expect(response.headers['content-encoding']).toBe('br');
    expect(response.headers['content-type']).toBe('application/wasm');
    expect(response.headers['vary']).toBe('Accept-Encoding');
    expect(response.headers['cache-control']).toBe(IMMUTABLE);
    expect(Number(response.headers['content-length'])).toBe(variant('br').length);
    expect(response.body.equals(variant('br'))).toBe(true);
    expect(zlib.brotliDecompressSync(response.body).equals(WASM)).toBe(true);
    const sha = crypto.createHash('sha256').update(variant('br')).digest('hex');
    expect(response.headers.etag).toBe(`"${sha.slice(0, 16)}-br"`);
  });

  it.each([
    ['gzip', 'gzip'],
    ['br;q=0, gzip', 'gzip'],
    ['gzip;q=1, br;q=0.5', 'gzip'],
    ['*', 'br'],
    ['identity, *;q=0.5', 'br'],
  ])('negotiates %s to %s', async (header, encoding) => {
    const response = await raw('GET', wasmUrl(), { 'Accept-Encoding': header });
    expect(response.headers['content-encoding']).toBe(encoding);
    expect(response.body.equals(variant(encoding as 'br' | 'gzip'))).toBe(true);
    expect(response.headers['content-type']).toBe('application/wasm');
  });

  it.each([undefined, 'identity', 'br;q=0, gzip;q=0', 'deflate', '*;q=0'])(
    'serves identity for Accept-Encoding %s with the same Cache-Control',
    async (header) => {
      const response = await raw('GET', wasmUrl(), header ? { 'Accept-Encoding': header } : {});
      expect(response.status).toBe(200);
      expect(response.headers['content-encoding']).toBeUndefined();
      expect(response.headers['content-type']).toBe('application/wasm');
      expect(response.headers['cache-control']).toBe(IMMUTABLE);
      expect(response.headers['vary']).toBe('Accept-Encoding');
      expect(response.body.equals(WASM)).toBe(true);
    }
  );

  it('answers a matching If-None-Match with 304 and ignores Range on a variant', async () => {
    const first = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br' });
    const etag = String(first.headers.etag);
    const revalidated = await raw('GET', wasmUrl(), {
      'Accept-Encoding': 'br',
      'If-None-Match': etag,
    });
    expect(revalidated.status).toBe(304);
    expect(revalidated.body.length).toBe(0);
    expect(revalidated.headers['cache-control']).toBe(IMMUTABLE);
    const ranged = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br', Range: 'bytes=0-9' });
    expect(ranged.status).toBe(200);
    expect(ranged.body.equals(variant('br'))).toBe(true);
    const head = await raw('HEAD', wasmUrl(), { 'Accept-Encoding': 'gzip' });
    expect(head.status).toBe(200);
    expect(head.headers['content-encoding']).toBe('gzip');
    expect(Number(head.headers['content-length'])).toBe(variant('gzip').length);
    expect(head.body.length).toBe(0);
  });

  it('never serves .br or .gz files directly', async () => {
    for (const route of [
      `${wasmUrl()}.br`,
      `${wasmUrl()}.gz`,
      `${wasmUrl()}.GZ`,
      `${wasmUrl()}%2Ebr`,
      '/view-model.mjs.gz',
    ]) {
      const response = await raw('GET', route, { 'Accept-Encoding': 'br, gzip' });
      expect([route, response.status]).toEqual([route, 404]);
      expect(response.headers['content-encoding']).toBeUndefined();
    }
  });

  it('disables a tampered variant at startup and serves identity instead', async () => {
    await stop();
    const brFile = path.join(staticDir, `${manifest.wasm.path}.br`);
    const tampered = Buffer.from(fs.readFileSync(brFile));
    tampered[tampered.length - 1] ^= 0xff; // same size, different hash
    fs.writeFileSync(brFile, tampered);
    const glue = manifest.precompressed.find(
      (row) => row.path.endsWith('ccs_account_dashboard.js') && row.encoding === 'gzip'
    );
    fs.appendFileSync(path.join(staticDir, `${glue?.file}`), Buffer.from([0])); // size changes
    await start();
    const wasm = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br' });
    expect(wasm.headers['content-encoding']).toBeUndefined();
    expect(wasm.body.equals(WASM)).toBe(true);
    const gzip = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'gzip' });
    expect(gzip.headers['content-encoding']).toBe('gzip');
    const glueGzip = await raw('GET', `/${glue?.path}`, { 'Accept-Encoding': 'gzip' });
    expect(glueGzip.headers['content-encoding']).toBeUndefined();
  });

  it('falls back to identity when a variant vanishes after startup', async () => {
    fs.rmSync(path.join(staticDir, `${manifest.wasm.path}.br`));
    const response = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br' });
    expect(response.status).toBe(200);
    expect(response.headers['content-encoding']).toBeUndefined();
    expect(response.headers['cache-control']).toBe(IMMUTABLE);
    expect(response.headers['content-type']).toBe('application/wasm');
    expect(response.body.equals(WASM)).toBe(true);
  });

  it('keeps no-cache on index.html, bridge.js and modules', async () => {
    for (const route of ['/index.html', '/bridge.js', '/view-model.mjs']) {
      const response = await raw('GET', route, { 'Accept-Encoding': 'br, gzip' });
      expect([route, response.status, response.headers['cache-control']]).toEqual([
        route,
        200,
        'no-cache',
      ]);
    }
    const module = await raw('GET', '/view-model.mjs', { 'Accept-Encoding': 'gzip' });
    expect(module.headers['content-encoding']).toBe('gzip');
    expect(module.headers['content-type']).toBe('text/javascript; charset=utf-8');
    const bridge = await raw('GET', '/bridge.js');
    expect(bridge.body.toString()).toContain(`./pkg/${manifest.buildId}/ccs_account_dashboard.js`);
    expect(bridge.headers['content-type']).toBe('text/javascript; charset=utf-8');
    const index = await raw('GET', '/index.html');
    expect(index.headers['x-content-type-options']).toBe('nosniff');
    expect(index.headers['referrer-policy']).toBe('same-origin');
  });

  it('never serves the build manifest (commit and source hash) to anyone', async () => {
    expect(fs.existsSync(path.join(staticDir, 'ui-build-manifest.json'))).toBe(true);
    for (const [method, route] of [
      ['GET', '/ui-build-manifest.json'],
      ['HEAD', '/ui-build-manifest.json'],
      ['GET', '/UI-Build-Manifest.json'],
      ['GET', '/./ui-build-manifest.json'],
      ['GET', '/%2e/ui-build-manifest.json'],
      ['GET', '//ui-build-manifest.json'],
      ['GET', '/ui-build-manifest%2Ejson'],
    ]) {
      const response = await raw(method, route, { 'Accept-Encoding': 'br, gzip' });
      expect([method, route, response.status]).toEqual([method, route, 404]);
      expect(response.body.toString()).not.toMatch(/8fb5e3de|buildId|sourceFingerprint/);
    }
  });

  it('releases the file and keeps serving when clients go away mid-answer', async () => {
    const countOpenFiles = () =>
      process.platform === 'linux' ? fs.readdirSync('/proc/self/fd').length : 0;
    await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br' });
    const before = countOpenFiles();
    for (let index = 0; index < 30; index += 1) {
      await new Promise<void>((resolve) => {
        const request = http.request({
          host: '127.0.0.1',
          port,
          method: 'GET',
          path: wasmUrl(),
          headers: { 'Accept-Encoding': index % 2 ? 'gzip' : 'br' },
          agent: false,
        });
        request.on('error', () => resolve());
        request.on('response', (response) => {
          response.destroy();
          resolve();
        });
        request.end();
        // Abort as soon as the request is out: before, during or after the file open.
        setImmediate(() => {
          request.destroy();
          resolve();
        });
      });
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
    const after = await raw('GET', wasmUrl(), { 'Accept-Encoding': 'br' });
    expect(after.status).toBe(200);
    expect(after.body.equals(variant('br'))).toBe(true);
    expect(countOpenFiles()).toBeLessThanOrEqual(before + 4);
  });

  it('leaves a small /api JSON answer uncompressed', async () => {
    const response = await raw('GET', '/api/health', { 'Accept-Encoding': 'br, gzip' });
    expect(response.status).toBe(200);
    expect(response.headers['content-encoding']).toBeUndefined();
    expect(response.headers.vary).toBeUndefined();
    expect(JSON.parse(response.body.toString())).toEqual({ status: 'ok' });
  });
});

describe('page routes and deep links', () => {
  it.each(['/', '/login', '/analytics', '/accounts', '/accounts/zai', '/accounts/kimi-code'])(
    'serves index.html for %s with the page headers',
    async (route) => {
      for (const encoding of [undefined, 'br']) {
        const response = await raw('GET', route, encoding ? { 'Accept-Encoding': encoding } : {});
        expect(response.status).toBe(200);
        const body =
          response.headers['content-encoding'] === 'br'
            ? zlib.brotliDecompressSync(response.body)
            : response.body;
        expect(body.toString()).toBe(INDEX);
        expect(response.headers['content-type']).toBe('text/html; charset=utf-8');
        expect(response.headers['cache-control']).toBe('no-cache');
        expect(response.headers['x-content-type-options']).toBe('nosniff');
        expect(response.headers['referrer-policy']).toBe('same-origin');
        expect(response.headers.etag).toBeTruthy();
      }
    }
  );

  it('answers HEAD on a page route without a body and ignores the query', async () => {
    const head = await raw('HEAD', '/analytics');
    expect(head.status).toBe(200);
    expect(head.body.length).toBe(0);
    const query = await raw('GET', '/accounts?provider=%3Cscript%3E&view=<b>x</b>');
    expect(query.status).toBe(200);
    expect(query.body.toString()).toBe(INDEX);
  });

  it.each([
    ['/settings', 301, '/accounts'],
    ['/home', 301, '/'],
    ['/analytics/', 301, '/analytics'],
    ['/accounts/', 301, '/accounts'],
    ['/accounts/zai/', 301, '/accounts/zai'],
    ['/login/', 301, '/login'],
    ['/settings/', 301, '/settings'],
    ['/accounts/nope', 302, '/'],
    ['/accounts/nope/', 302, '/'],
    ['/Analytics', 302, '/'],
    ['/LOGIN', 302, '/'],
    ['/accounts/ZAI', 302, '/'],
    ['/codex/accounts', 302, '/'],
    ['/analytics//', 302, '/'],
    ['//evil.example/', 302, '/'],
  ])('redirects %s with %d to %s', async (route, status, location) => {
    const response = await raw('GET', route);
    expect(response.status).toBe(status);
    expect(response.headers.location).toBe(location);
  });

  it.each(['GET', 'POST', 'PUT', 'PATCH', 'DELETE'])(
    'answers %s for an unknown /api path with a JSON 404',
    async (method) => {
      for (const route of ['/api/nope', '/api/accounts/nope', '/API/health', '/api']) {
        const response = await raw(method, route, {
          'Content-Type': 'application/json',
          Origin: `http://127.0.0.1:${port}`,
        });
        expect([method, route, response.status]).toEqual([method, route, 404]);
        expect(response.headers['content-type']).toContain('application/json');
        expect(JSON.parse(response.body.toString())).toEqual({
          error: 'API endpoint was not found.',
        });
      }
    }
  );

  it('never escapes the static folder through dot segments', async () => {
    for (const route of [
      '/../outside-secret.txt',
      '/%2e%2e/outside-secret.txt',
      '/%2E%2E/outside-secret.txt',
      `/pkg/${manifest.buildId}/../../../outside-secret.txt`,
      `/pkg/%2e%2e/%2e%2e/outside-secret.txt`,
      '/..%2foutside-secret.txt',
      '/%2e%2e%5coutside-secret.txt',
    ]) {
      const response = await raw('GET', route, { 'Accept-Encoding': 'br, gzip' });
      expect([route, response.body.toString().includes('OUTSIDE-SECRET')]).toEqual([route, false]);
      expect(response.status === 200 ? response.body.toString() : '').not.toContain('SECRET');
    }
  });
});

describe('JSON error answers', () => {
  it('answers an oversized body with a JSON 413 and no stack trace', async () => {
    const response = await raw(
      'POST',
      '/api/auth/login',
      { 'Content-Type': 'application/json' },
      JSON.stringify({ username: 'x', password: 'y'.repeat(200 * 1024) })
    );
    expect(response.status).toBe(413);
    expect(response.headers['content-type']).toContain('application/json');
    // Under /api/auth the answer also carries a stable code and no-store (CONTRACT-auth-devices 2).
    expect(JSON.parse(response.body.toString())).toEqual({
      error: 'Request body is too large.',
      code: 'body_too_large',
    });
    expect(response.headers['cache-control']).toBe('no-store');
    expect(response.body.toString()).not.toMatch(/PayloadTooLarge|\bat\s|node_modules/);
  });

  it('answers other body-parser failures with JSON and no stack trace', async () => {
    const response = await raw(
      'POST',
      '/api/auth/login',
      { 'Content-Type': 'application/json; charset=latin1' },
      '{}'
    );
    expect(response.status).toBe(415);
    expect(JSON.parse(response.body.toString())).toEqual({
      error: 'The request could not be processed.',
    });
    expect(response.body.toString()).not.toMatch(/\bat\s|node_modules|charset/);
  });

  it('answers a failed page send with JSON from the final handler', async () => {
    fs.rmSync(path.join(staticDir, 'index.html'));
    fs.rmSync(path.join(staticDir, 'index.html.br'), { force: true });
    fs.rmSync(path.join(staticDir, 'index.html.gz'), { force: true });
    const response = await raw('GET', '/analytics');
    expect(response.status).toBe(404);
    expect(JSON.parse(response.body.toString())).toEqual({
      error: 'The request could not be processed.',
    });
    expect(response.body.toString()).not.toMatch(/ENOENT|\bat\s|index\.html/);
    // The server keeps a trace: status and error class, never the message or a path.
    const failures = getRecentLogEntries().filter((entry) => entry.event === 'web.request.failed');
    const last = failures[failures.length - 1];
    expect(last?.level).toBe('warn');
    expect(last?.context).toEqual({ status: 404, kind: 'ENOENT', headersSent: false });
    expect(JSON.stringify(last)).not.toMatch(/index\.html|ccs-static-ui|no such file/);
  });

  it('names an error by its type, code or class, never by its message', () => {
    expect(requestErrorKind(Object.assign(new Error('x'), { type: 'entity.parse.failed' }))).toBe(
      'entity.parse.failed'
    );
    expect(requestErrorKind(Object.assign(new Error('/home/x/secret'), { code: 'EACCES' }))).toBe(
      'EACCES'
    );
    expect(requestErrorKind(new TypeError('/home/x/secret'))).toBe('TypeError');
    expect(requestErrorKind(Object.assign(new Error('x'), { name: 'has spaces /home/x' }))).toBe(
      'unknown'
    );
    expect(requestErrorKind(null)).toBe('unknown');
    expect(requestErrorKind('/home/x/secret')).toBe('unknown');
  });
});

describe('negotiation and route helpers', () => {
  it('reads q-values, wildcards and exclusions', () => {
    const both = { br: true, gzip: true };
    expect(negotiateEncoding('br, gzip', both)).toBe('br');
    expect(negotiateEncoding('gzip, br', both)).toBe('br');
    expect(negotiateEncoding('gzip;q=0.9, br;q=0.8', both)).toBe('gzip');
    expect(negotiateEncoding('BR;Q=1, GZIP', both)).toBe('br');
    expect(negotiateEncoding('x-gzip', both)).toBe('gzip');
    expect(negotiateEncoding('br;q=0', both)).toBeNull();
    expect(negotiateEncoding('br;q=abc, gzip', both)).toBe('gzip');
    expect(negotiateEncoding('*;q=0, gzip', both)).toBe('gzip');
    expect(negotiateEncoding('br', { gzip: true })).toBeNull();
    expect(negotiateEncoding('', both)).toBeNull();
    expect(negotiateEncoding(undefined, both)).toBeNull();
  });

  it('resolves only exact provider pages from the given table', () => {
    const providers = new Set(['zai']);
    expect(resolvePageRoute('/accounts/zai', providers)).toEqual({ kind: 'page' });
    expect(resolvePageRoute('/accounts/kimi-code', providers)).toEqual({
      kind: 'redirect',
      status: 302,
      location: '/',
    });
  });
});
