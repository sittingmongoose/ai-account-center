import { afterEach, beforeEach, expect, it } from 'bun:test';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { registerRustAgentConnection, rustAgent } from '../../../src/web-server/rust-service-transport';
import { runAgentVerb } from '../../../src/web-server/services/agent-verbs';

let root = '';
let server: http.Server | undefined;
let disconnect: (() => void) | undefined;
let pool: http.Agent | undefined;
const saved = { home: process.env.CCS_HOME, dir: process.env.CCS_DIR };
const host = { platform: 'linux' as const, sshHost: 'fixture' };
const artifact = { target: 'x86_64-unknown-linux-musl', version: '0.1.0', sha256: 'a'.repeat(64), file: 'fixture' };
const options = { mode: 'on' as const, transport: 'on' as const, deadlineMs: 1000, maxBytes: 65536 };
const result = (value: unknown) => ({ stdout: JSON.stringify(value), code: 0, failed: false, missing: false, timedOut: false });

beforeEach(() => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-l5-ts-'));
  process.env.CCS_HOME = root;
  process.env.CCS_DIR = path.join(root, '.ccs');
  fs.mkdirSync(process.env.CCS_DIR);
});
afterEach(async () => {
  disconnect?.(); disconnect = undefined;
  pool?.destroy(); pool = undefined;
  if (server) { server.closeAllConnections(); await new Promise<void>((r) => server!.close(() => r())); server = undefined; }
  if (saved.home === undefined) delete process.env.CCS_HOME; else process.env.CCS_HOME = saved.home;
  if (saved.dir === undefined) delete process.env.CCS_DIR; else process.env.CCS_DIR = saved.dir;
  fs.rmSync(root, { recursive: true, force: true });
});
async function service(handler: (body: Record<string, any>, res: http.ServerResponse) => void) {
  server = http.createServer((req, res) => {
    expect(req.method).toBe('POST');
    expect(req.url).toBe('/internal/agent/signin-expiry');
    expect(JSON.parse(String(req.headers['x-aac-principal']))).toEqual({ kind: 'session', id: 'front-door', role: 'service' });
    expect(req.headers['x-aac-boot-secret']).toBe('private-fixture');
    res.setHeader('x-aac-service-boot', 'fixture');
    res.setHeader('content-type', 'application/x-ndjson');
    let text = ''; req.on('data', (chunk) => text += chunk); req.on('end', () => handler(JSON.parse(text), res));
  });
  const socketPath = path.join(root, 'http.sock');
  await new Promise<void>((resolve) => server!.listen(socketPath, resolve));
  pool = rustAgent();
  disconnect = registerRustAgentConnection(process.env.CCS_DIR!, { socketPath, secret: 'private-fixture', bootId: 'fixture', agent: pool }, new Set());
}

it('off does no service request and keeps the original null result', async () => {
  let calls = 0;
  await service(() => calls++);
  expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, mode: 'off' })).toBeNull();
  expect(calls).toBe(0);
});
it('the actual caller uses service NDJSON with no TS artifact hash or launcher', async () => {
  const progress: unknown[] = [];
  await service((body, res) => {
    expect(Object.keys(body).sort()).toEqual(['args', 'callId', 'computer', 'deadlineMs', 'maxBytes', 'timeoutMs']);
    expect(body.computer).toEqual({ platform: 'linux', sshHost: 'fixture' });
    res.write(JSON.stringify({ type: 'progress', protocol: 1, id: body.callId, data: { ready: true } }) + '\n');
    res.end(JSON.stringify({ type: 'result', protocol: 1, id: body.callId, value: { schemaVersion: 1, profiles: [] } }) + '\n');
  });
  expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, onProgress: (p) => progress.push(p), run: async () => { throw new Error('TS must not launch'); } })).toBe('{"schemaVersion":1,"profiles":[]}');
  expect(progress).toEqual([{ ready: true }]);
});
it('service refusals, malformed streams and failures go to the helper without another agent attempt', async () => {
  let outcome = 'fallback';
  await service((body, res) => {
    if (outcome === 'malformed') res.end('broken\n');
    else if (outcome === 'duplicate') {
      const event = JSON.stringify({ type: 'result', protocol: 1, id: body.callId, value: { ok: true } });
      res.end(event + '\n' + event + '\n');
    } else res.end(JSON.stringify({ type: outcome, protocol: 1, id: body.callId, fallback: 'python' }) + '\n');
  });
  for (outcome of ['fallback', 'failure', 'malformed', 'duplicate']) {
    expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, artifacts: [artifact], run: async () => { throw new Error('no duplicate launch'); } })).toBeNull();
  }
});
it('missing and old services retain the TS fallback exactly once', async () => {
  let launches = 0;
  const run = async (_file: string, _args: string[], opts: { input?: string }) => {
    launches++;
    return result({ type: 'result', protocol: 1, id: JSON.parse(opts.input!).id, value: { fallback: 'python' } });
  };
  expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, artifacts: [artifact], run })).toBeNull();
  await service((_body, res) => { res.statusCode = 404; res.end('{}'); });
  expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, artifacts: [artifact], run })).toBeNull();
  expect(launches).toBe(2);
});
it('a stalled service respects the caller budget and does not consume the Python reserve', async () => {
  await service(() => {});
  const started = Date.now();
  expect(await runAgentVerb(host, 'signin-expiry', {}, { ...options, timeoutMs: 50 })).toBeNull();
  expect(Date.now() - started).toBeLessThan(500);
});
