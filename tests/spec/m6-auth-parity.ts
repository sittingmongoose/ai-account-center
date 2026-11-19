/**
 * M6 auth over HTTP against the real TS routers and opt-in Rust TCP binary.
 * Bun fixtures use distinct temporary roots and never start the provider collectors.
 * Run with: bun tests/spec/m6-auth-parity.ts <absolute-path-to-aac-http>
 */
import assert from 'node:assert/strict';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import {
  PASSWORD,
  USERNAME,
  startAuthHarness,
  type AuthMode,
} from '../unit/web-server/dashboard-auth-harness';

const binary = path.resolve(process.argv[2] ?? '');
const auth = '/api/auth';
const pairInput = {
  username: USERNAME,
  password: PASSWORD,
  deviceName: 'Desk',
  platform: 'windows',
};
const installId = '76A8CFC9-6C80-404D-BDA6-E5274C4EA0CB';
const bodyHeaders = [
  'cache-control',
  'content-type',
  'ratelimit-limit',
  'ratelimit-policy',
  'ratelimit-remaining',
] as const;

type Actor = 'guest' | 'browser' | 'other' | 'device' | 'old-device';
type Call = {
  method: string;
  route: string;
  actor?: Actor;
  input?: unknown;
  origin?: 'omit' | 'bad' | 'alias';
  bearer?: string;
  content?: string;
  nonce?: string;
  update?: 'device' | 'rotate';
};

class Target {
  cookies = new Map<Actor, string>();
  tokens = new Map<Actor, string>();
  ids = new Map<string, string>();
  constructor(
    readonly url: string,
    readonly ccs: string
  ) {}

  async request(call: Call) {
    const actor = call.actor ?? 'guest';
    const origin =
      call.origin === 'bad'
        ? 'http://foreign.example'
        : call.origin === 'alias'
          ? this.url.replace('127.0.0.1', 'localhost')
          : this.url;
    const headers = new Headers();
    const cookie = this.cookies.get(actor);
    const bearer = call.bearer ?? this.tokens.get(actor);
    if (cookie) headers.set('cookie', cookie);
    if (bearer !== undefined) headers.set('authorization', 'Bearer ' + bearer);
    if (call.origin !== 'omit' && (call.method !== 'GET' || call.origin))
      headers.set('origin', origin);
    if (call.input !== undefined) headers.set('content-type', call.content ?? 'application/json');
    if (call.nonce) headers.set('x-ccs-bar-nonce', call.nonce);
    const reply = await fetch(
      this.url + call.route.replace('@device', this.ids.get('device') ?? ''),
      {
        method: call.method,
        headers,
        body:
          call.input === undefined
            ? undefined
            : typeof call.input === 'string'
              ? call.input
              : JSON.stringify(call.input),
        signal: AbortSignal.timeout(10_000),
      }
    );
    const setCookie = reply.headers.get('set-cookie');
    if (setCookie) {
      const first = setCookie.split(';')[0];
      if (first === 'connect.sid=') this.cookies.delete(actor);
      else this.cookies.set(actor, first);
    }
    const text = await reply.text();
    if (text && call.update && reply.ok) {
      const value = JSON.parse(text);
      if (call.update === 'device') this.ids.set('device', value.deviceId);
      else this.tokens.set('old-device', this.tokens.get('device') ?? '');
      this.tokens.set('device', value.token);
      assert.match(value.token, /^aacd_[A-Za-z0-9_-]{43}$/);
    }
    return { status: reply.status, headers: reply.headers, text, setCookie };
  }
}

async function startRust(
  root: string,
  env: NodeJS.ProcessEnv
): Promise<{ target: Target; audit: string[]; stop(): Promise<void> }> {
  const child = spawn(binary, ['--public'], {
    env: { ...env, CCS_DIR: root, AAC_RUST_PUBLIC_LISTEN: '127.0.0.1:0' },
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const audit: string[] = [];
  child.stderr?.on('data', (chunk: Buffer) => {
    if (audit.length < 1024) audit.push(chunk.toString());
  });
  try {
    const url = await ready(child);
    return { target: new Target(url, root), audit, stop: () => stop(child) };
  } catch (error) {
    await stop(child);
    throw error;
  }
}

async function ready(child: ChildProcess): Promise<string> {
  assert(child.stdout && child.stderr);
  const output = createInterface({ input: child.stdout });
  child.stderr.resume();
  try {
    return await new Promise((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error('Rust TCP listener did not start')),
        10_000
      );
      child.once('error', reject);
      child.once('exit', (status) => reject(new Error(`Rust TCP listener exited with ${status}`)));
      output.on('line', (line) => {
        const address = /^aac-http public listening on (127\.0\.0\.1:\d+)$/.exec(line)?.[1];
        if (address) resolve('http://' + address);
      });
      // The listener still needs a timeout if it prints no readiness line.
      output.once('close', () => clearTimeout(timeout));
    });
  } finally {
    output.close();
  }
}

async function stop(child: ChildProcess): Promise<void> {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit');
  child.kill();
  await exited;
}

function normalized(text: string): string {
  return text
    .replace(/"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z"/g, '"<time>"')
    .replace(/"dev_[0-9a-f]{16}"/g, '"<device>"')
    .replace(/("token":)"aacd_[A-Za-z0-9_-]{43}"/g, '$1"<token>"')
    .replace(/("retryAfterSeconds":)([0-9]+)/g, (value, field, seconds) =>
      +seconds > 0 && +seconds <= 3600 ? field + '"<retry>"' : value
    );
}

function cookie(value: string | null): string | null {
  return (
    value
      ?.replace(/connect\.sid=[^;]+/, 'connect.sid=<cookie>')
      .replace(/Expires=[^;]+/, 'Expires=<date>') ?? null
  );
}

let checked = 0;
async function verify(reference: Target, rust: Target, call: Call, status: number, code?: string) {
  const label = `${call.actor ?? 'guest'} ${call.method} ${call.route} (#${checked + 1})`;
  const ts = await reference.request(call);
  const rs = await rust.request(call);
  assert.equal(ts.status, status, label + ' TS status');
  assert.equal(rs.status, status, label + ' Rust status');
  if (code) {
    assert.equal(JSON.parse(ts.text).code, code, label + ' TS code');
    assert.equal(JSON.parse(rs.text).code, code, label + ' Rust code');
  }
  assert.equal(normalized(rs.text), normalized(ts.text), label + ' body bytes');
  for (const name of bodyHeaders)
    assert.equal(rs.headers.get(name), ts.headers.get(name), label + ' ' + name);
  assert.equal(cookie(rs.setCookie), cookie(ts.setCookie), label + ' cookie attributes');
  if (call.nonce) {
    const { createHmac } = await import('node:crypto');
    for (const [target, reply] of [
      [reference, ts],
      [rust, rs],
    ] as const) {
      const proof = reply.headers.get('x-ccs-bar-token');
      if (status !== 200 || call.method !== 'GET' || call.nonce.length < 16) {
        assert.equal(proof, null, label + ' has no proof');
      } else {
        const token = (
          await fs.readFile(path.join(target.ccs, 'bar', '.auth-token'), 'utf8')
        ).trim();
        assert.equal(proof, createHmac('sha256', token).update(call.nonce).digest('hex'), label);
      }
    }
  }
  checked++;
}

async function pairServers(mode: AuthMode, run: (ts: Target, rs: Target) => Promise<void>) {
  const ts = await startAuthHarness({ mode });
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-auth-rust-'));
  try {
    await fs.copyFile(path.join(ts.ccsDir, 'config.yaml'), path.join(root, 'config.yaml'));
    const rs = await startRust(root, process.env);
    try {
      await run(new Target(ts.baseUrl, ts.ccsDir), rs.target);
    } finally {
      await rs.stop();
    }
  } finally {
    await ts.close();
    await fs.rm(root, { force: true, recursive: true });
  }
}

await pairServers('config', async (ts, rs) => {
  const step = (call: Call, status: number, code?: string) => verify(ts, rs, call, status, code);
  const get = (route: string, actor?: Actor, extra?: Partial<Call>) => ({
    method: 'GET',
    route,
    actor,
    ...extra,
  });
  const post = (route: string, input: unknown, actor?: Actor, extra?: Partial<Call>) => ({
    method: 'POST',
    route,
    input,
    actor,
    ...extra,
  });
  const put = (route: string, input: unknown, actor: Actor, extra?: Partial<Call>) => ({
    method: 'PUT',
    route,
    input,
    actor,
    ...extra,
  });
  const del = (route: string, actor: Actor, extra?: Partial<Call>) => ({
    method: 'DELETE',
    route,
    actor,
    ...extra,
  });
  const credentials = { username: USERNAME, password: PASSWORD };

  await step(get(auth + '/check'), 200);
  await step(get(auth + '/setup'), 200);
  await step(get(auth + '/session'), 401, 'auth_required');
  await step(get(auth + '/devices'), 401, 'auth_required');
  await step(get(auth + '/network'), 401, 'auth_required');
  await step(get(auth + '/session-lifetime'), 401, 'auth_required');
  await step(get('/api/bar/auth'), 401);
  await step(get(auth + '/check?credential=aacd_aaaaaaaaaaaaaaaa'), 400, 'token_in_query');
  await step(get(auth + '/check?credential=%61acd_aaaaaaaaaaaaaaaa'), 400, 'token_in_query');
  await step(
    post(auth + '/devices/pair', { key: ['aacd_' + 'a'.repeat(43)] }),
    400,
    'token_in_query'
  );
  await step(post(auth + '/login', {}), 400);
  await step(post(auth + '/login', { ...credentials, rememberMe: 'yes' }), 400);
  await step(
    post(auth + '/login', { ...credentials, password: 'wrong' }),
    401,
    'invalid_credentials'
  );
  await step(post(auth + '/setup', credentials), 409, 'already_configured');
  await step(post(auth + '/login', credentials, 'browser'), 200);
  await step(post(auth + '/login', { ...credentials, rememberMe: false }, 'other'), 200);
  await step(get(auth + '/check', 'browser'), 200);
  await step(get(auth + '/session', 'browser'), 200);
  await step(get(auth + '/session', 'other'), 200);
  await step(get(auth + '/session?', 'browser'), 400, 'unexpected_query');
  await step(get(auth + '/network', 'browser'), 200);
  await step(get(auth + '/session-lifetime', 'browser'), 200);
  await step(put(auth + '/session-lifetime', { days: 14 }, 'browser'), 400, 'invalid_body');
  await step(
    put(auth + '/session-lifetime', { days: 30 }, 'browser', { origin: 'bad' }),
    403,
    'origin_required'
  );
  await step(
    put(auth + '/session-lifetime', { days: 30 }, 'browser', { origin: 'omit' }),
    403,
    'origin_required'
  );
  await step(put(auth + '/session-lifetime', { days: 30 }, 'browser', { origin: 'alias' }), 200);
  await step(get(auth + '/session-lifetime?', 'browser'), 400, 'unexpected_query');
  await step(put(auth + '/network', { trustLocalNetwork: true }, 'browser'), 200);
  await step(put(auth + '/network', { trustLocalNetwork: false }, 'browser'), 200);
  await step(
    put(auth + '/network', { trustLocalNetwork: false }, 'browser', { origin: 'omit' }),
    403,
    'origin_required'
  );
  await step(get(auth + '/network?unused=1', 'browser'), 400, 'unexpected_query');
  await step(
    post(auth + '/password', { currentPassword: PASSWORD, newPassword: 'short' }, 'browser'),
    400,
    'weak_password'
  );
  await step(
    post(auth + '/password', { currentPassword: PASSWORD, newPassword: PASSWORD }, 'browser'),
    400,
    'same_password'
  );
  await step(
    post(
      auth + '/password',
      { currentPassword: 'wrong', newPassword: 'new-fixture-password' },
      'browser'
    ),
    401,
    'wrong_password'
  );
  await step(get('/api/bar/auth', 'browser', { nonce: 'abcdef0123456789' }), 200);
  await step(get('/api/bar/auth', 'browser', { nonce: 'short' }), 200);
  await step(get('/api/bar/auth', 'browser', { nonce: 'abcdef0123456789', origin: 'bad' }), 403);
  await step(get(auth + '/session', 'browser', { bearer: 'invalid' }), 401, 'invalid_token');
  await step(
    post(auth + '/devices/pair', pairInput, 'guest', { origin: 'bad' }),
    403,
    'origin_required'
  );
  await step(
    post(auth + '/devices/pair', { ...pairInput, platform: 'linux' }),
    400,
    'invalid_body'
  );
  await step(
    post(auth + '/devices/pair', { ...pairInput, installId, appVersion: '1.2.3' }, 'guest', {
      origin: 'omit',
      update: 'device',
    }),
    201
  );
  await step(get(auth + '/devices', 'browser'), 200);
  await step(get(auth + '/devices', 'device'), 403, 'device_scope');
  await step(get(auth + '/devices/me', 'browser'), 403, 'device_required');
  await step(get(auth + '/devices/me', 'device'), 200);
  await step(get(auth + '/devices/me?unused=1', 'device'), 400, 'unexpected_query');
  await step(get('/api/bar/auth', 'device', { nonce: 'abcdef0123456789' }), 200);
  await step(post(auth + '/devices/me/rotate', {}, 'device', { update: 'rotate' }), 200);
  await step(get(auth + '/devices/me', 'old-device'), 200);
  await step(get(auth + '/devices/me', 'device'), 200);
  await step(get(auth + '/devices/me', 'old-device'), 401, 'invalid_token');
  await step(del(auth + '/devices/@device', 'browser', { origin: 'omit' }), 403, 'origin_required');
  await step(del(auth + '/devices/@device', 'browser'), 204);
  await step(get(auth + '/devices/me', 'device'), 401, 'device_revoked');
  await step(post(auth + '/sessions/revoke-others', {}, 'browser'), 200);
  await step(get(auth + '/session', 'other'), 401, 'session_revoked');
  await step(get(auth + '/check', 'other'), 200);
  await step(post(auth + '/devices/pair', pairInput, 'guest', { update: 'device' }), 201);
  await step(post(auth + '/devices/revoke-all', { signOutOtherBrowsers: false }, 'browser'), 200);
  await step(get(auth + '/devices/me', 'device'), 401, 'device_revoked');
  await step(post(auth + '/logout', undefined, 'browser'), 200);
  await step(get(auth + '/check', 'browser'), 200);
});

await pairServers('env', async (ts, rs) => {
  for (const [call, status, code] of [
    [{ method: 'POST', route: auth + '/setup', input: pairInput }, 409, 'managed_by_env'],
    [
      { method: 'POST', route: auth + '/login', input: pairInput, actor: 'browser' },
      200,
      undefined,
    ],
    [
      { method: 'POST', route: auth + '/password', input: {}, actor: 'browser' },
      409,
      'managed_by_env',
    ],
    [{ method: 'GET', route: auth + '/setup' }, 200, undefined],
  ] as const)
    await verify(ts, rs, call, status, code);
});

await pairServers('setup', async (ts, rs) => {
  for (const [call, status, code] of [
    [{ method: 'GET', route: auth + '/check' }, 200, undefined],
    [{ method: 'GET', route: auth + '/setup' }, 200, undefined],
    [
      { method: 'POST', route: auth + '/setup', input: { username: '!!', password: PASSWORD } },
      400,
      'invalid_username',
    ],
    [
      { method: 'POST', route: auth + '/setup', input: { username: USERNAME, password: 'short' } },
      400,
      'weak_password',
    ],
    [
      {
        method: 'POST',
        route: auth + '/setup',
        input: { username: USERNAME, password: PASSWORD },
        actor: 'browser',
      },
      201,
      undefined,
    ],
    [{ method: 'GET', route: auth + '/session', actor: 'browser' }, 200, undefined],
  ] as const)
    await verify(ts, rs, call, status, code);
});

await pairServers('off', async (ts, rs) => {
  await verify(ts, rs, { method: 'GET', route: auth + '/check' }, 200);
  await verify(ts, rs, { method: 'GET', route: auth + '/session' }, 409, 'auth_not_configured');
  await verify(ts, rs, { method: 'GET', route: '/api/bar/auth', nonce: 'abcdef0123456789' }, 200);
  await verify(ts, rs, { method: 'GET', route: '/api/bar/auth', origin: 'bad' }, 403);
});

await pairServers('config', async (ts, rs) => {
  const login = (actor: Actor, password = PASSWORD): Call => ({
    method: 'POST',
    route: auth + '/login',
    input: { username: USERNAME, password },
    actor,
  });
  await verify(ts, rs, login('browser'), 200);
  await verify(ts, rs, login('other'), 200);
  const newPassword = 'changed-fixture-password';
  await verify(
    ts,
    rs,
    {
      method: 'POST',
      route: auth + '/password',
      actor: 'browser',
      input: { currentPassword: PASSWORD, newPassword, signOutOtherBrowsers: false },
    },
    200
  );
  await verify(ts, rs, { method: 'GET', route: auth + '/session', actor: 'other' }, 200);
  await verify(ts, rs, login('guest', newPassword), 200);

  for (let index = 0; index < 5; index++) {
    await verify(ts, rs, login('guest', 'incorrect-password'), 401, 'invalid_credentials');
  }
  await verify(ts, rs, login('guest', newPassword), 429, 'rate_limited');
  await verify(
    ts,
    rs,
    {
      method: 'POST',
      route: auth + '/devices/pair',
      input: { ...pairInput, password: newPassword },
      origin: 'omit',
    },
    429,
    'rate_limited'
  );
});

async function verifyHandoff() {
  const ts = await startAuthHarness();
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-auth-handoff-'));
  try {
    const reference = new Target(ts.baseUrl, ts.ccsDir);
    const logged = await reference.request({
      method: 'POST',
      route: auth + '/login',
      actor: 'browser',
      input: { username: USERNAME, password: PASSWORD },
    });
    assert.equal(logged.status, 200);
    assert.equal(
      (
        await reference.request({
          method: 'POST',
          route: auth + '/devices/pair',
          input: pairInput,
          update: 'device',
        })
      ).status,
      201
    );
    await fs.copyFile(path.join(ts.ccsDir, 'config.yaml'), path.join(root, 'config.yaml'));
    await fs.mkdir(path.join(root, 'auth'), { mode: 0o700 });
    for (const file of ['sessions.json', 'devices.json', 'credential.json'])
      await fs.copyFile(path.join(ts.ccsDir, 'auth', file), path.join(root, 'auth', file));

    const env = { ...process.env };
    const live = await startRust(root, env);
    live.target.cookies = new Map(reference.cookies);
    live.target.tokens = new Map(reference.tokens);
    try {
      assert.equal(
        (await live.target.request({ method: 'GET', route: auth + '/session', actor: 'browser' }))
          .status,
        200
      );
      assert.equal(
        (await live.target.request({ method: 'GET', route: auth + '/devices/me', actor: 'device' }))
          .status,
        200
      );
      assert.equal(
        (
          await live.target.request({
            method: 'POST',
            route: auth + '/login',
            actor: 'other',
            input: { username: USERNAME, password: PASSWORD },
          })
        ).status,
        200
      );
    } finally {
      await live.stop();
    }
    const restarted = await startRust(root, env);
    try {
      restarted.target.cookies = new Map(live.target.cookies);
      restarted.target.tokens = new Map(live.target.tokens);
      assert.equal(
        (
          await restarted.target.request({
            method: 'GET',
            route: auth + '/session',
            actor: 'other',
          })
        ).status,
        200
      );
      assert.equal(
        (
          await restarted.target.request({
            method: 'GET',
            route: auth + '/devices/me',
            actor: 'device',
          })
        ).status,
        200
      );
      const { hashPassword } = await import('../unit/web-server/dashboard-auth-harness');
      const { default: yaml } = await import('js-yaml');
      const yamlPath = path.join(root, 'config.yaml');
      const config = yaml.load(await fs.readFile(yamlPath, 'utf8')) as {
        dashboard_auth: { password_hash: string };
      };
      config.dashboard_auth.password_hash = await hashPassword('outside-fixture-password');
      await fs.writeFile(yamlPath, yaml.dump(config), { mode: 0o600 });
      const revoked = await restarted.target.request({
        method: 'GET',
        route: auth + '/session',
        actor: 'other',
      });
      assert.equal(revoked.status, 401);
      assert.equal(JSON.parse(revoked.text).code, 'session_revoked');
      assert.equal(
        (
          await restarted.target.request({
            method: 'GET',
            route: auth + '/devices/me',
            actor: 'device',
          })
        ).status,
        200
      );
    } finally {
      await restarted.stop();
    }
  } finally {
    await ts.close();
    await fs.rm(root, { force: true, recursive: true });
  }
}

await verifyHandoff();
console.log(
  `M6 auth: ${checked} HTTP responses matched TypeScript. TS-to-Rust sessions and devices, Rust restart, and external credential revocation passed.`
);

async function verifyNativeTls(openssl: string) {
  const { createServer } = await import('node:net');
  const { request: httpsRequest } = await import('node:https');
  const { promisify } = await import('node:util');
  const { execFile } = await import('node:child_process');
  const probe = createServer().listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const address = probe.address();
  assert(address && typeof address === 'object');
  await new Promise<void>((resolve) => probe.close(() => resolve()));
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'aac-m6-auth-tls-'));
  try {
    const cert_path = path.join(root, 'cert.pem');
    const key_path = path.join(root, 'key.pem');
    await promisify(execFile)(
      openssl,
      [
        'req',
        '-x509',
        '-newkey',
        'rsa:2048',
        '-nodes',
        '-days',
        '1',
        '-subj',
        '/CN=dashboard.example.test',
        '-keyout',
        key_path,
        '-out',
        cert_path,
      ],
      { windowsHide: true }
    );
    await fs.chmod(key_path, 0o600);
    const { default: yaml } = await import('js-yaml');
    await fs.writeFile(
      path.join(root, 'config.yaml'),
      yaml.dump({
        dashboard_auth: { enabled: true },
        dashboard_tls: {
          https_listener: { enabled: true, port: address.port, cert_path, key_path },
          public_origin: 'https://dashboard.example.test',
        },
      }),
      { mode: 0o600 }
    );
    const environment = Object.fromEntries(
      Object.entries(process.env).filter(([name]) => !name.startsWith('CCS_'))
    );
    const side = await startRust(root, environment);
    try {
      const setupFile = path.join(root, 'auth', 'setup-code');
      const setupCode = (await fs.readFile(setupFile, 'utf8')).trim();
      const plain = await fetch(side.target.url + auth + '/check', {
        headers: { host: 'dashboard.example.test' },
        signal: AbortSignal.timeout(4_000),
      });
      assert.equal((await plain.json()).secureTransport, false);
      const body = JSON.stringify({ username: USERNAME, password: PASSWORD, setupCode });
      const reply = await new Promise<{ status?: number; cookie?: string; text: string }>(
        (resolve, reject) => {
          const request = httpsRequest(
            {
              host: '127.0.0.1',
              port: address.port,
              rejectUnauthorized: false,
              timeout: 4_000,
              path: auth + '/setup',
              method: 'POST',
              headers: {
                host: 'dashboard.example.test',
                origin: 'https://dashboard.example.test',
                'content-type': 'application/json',
                'content-length': Buffer.byteLength(body),
              },
            },
            (response) => {
              let text = '';
              response.setEncoding('utf8');
              response.on('data', (chunk) => {
                text += chunk;
              });
              response.on('error', reject);
              response.on('end', () =>
                resolve({
                  status: response.statusCode,
                  cookie: response.headers['set-cookie']?.[0],
                  text,
                })
              );
            }
          );
          request.on('error', reject);
          request.on('timeout', () => request.destroy(new Error('Rust TLS request timed out')));
          request.end(body);
        }
      );
      assert.equal(reply.status, 201);
      assert.match(reply.cookie ?? '', /; Secure; SameSite=Strict$/);
      assert.equal(JSON.parse(reply.text).username, USERNAME);
      await assert.rejects(fs.stat(setupFile), { code: 'ENOENT' });
      const logs = side.audit.join('');
      assert(logs.includes('auth.setup.completed'));
      assert(logs.includes('request.completed'));
      for (const secret of [
        setupCode,
        setupCode.replace('-', ''),
        PASSWORD,
        reply.cookie!.split(';')[0],
      ]) {
        assert(!logs.includes(secret), 'fixture credential leaked to Rust audit output');
      }
    } finally {
      await side.stop();
    }
  } finally {
    await fs.rm(root, { force: true, recursive: true });
  }
}

if (process.env.AAC_M6_OPENSSL) {
  await verifyNativeTls(process.env.AAC_M6_OPENSSL);
  console.log(
    'M6 native TLS: setup code, Secure cookie, file consumption, and audit redaction passed.'
  );
}
