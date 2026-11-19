// Run on Unix with the parity binaries in an isolated provider home and network namespace.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createEmptyUnifiedConfig } from '../../src/config/unified-config-types';

const launcher = process.env.AAC_M6_TARGET_CLI;
const serviceBinary = process.env.AAC_RUST_SERVICE_BINARY;
assert(launcher && path.isAbsolute(launcher), 'set AAC_M6_TARGET_CLI to a native build');
assert(serviceBinary && path.isAbsolute(serviceBinary), 'set AAC_RUST_SERVICE_BINARY');

const base = await mkdtemp(path.join(os.tmpdir(), 'aac-adopted-'));
// `codex_local` only reads a rollout under `$HOME/.ccs`; make that the fixture root.
const root = path.join(base, '.ccs');
await mkdir(root, { recursive: true, mode: 0o700 });
const children = [];
const accountId = 'zai:acct:0123abcd';
const accountPath = '/api/accounts/' + encodeURIComponent(accountId);
const dashboardPath = '/api/accounts/dashboard?platform=mac';
const credentials = { username: 'fixture-owner', password: 'fixture-password-only' };
const env = {
  ...process.env,
  CCS_DIR: root,
  CCS_HOME: base,
  HOME: base,
  CODEX_HOME: path.join(root, '.codex'),
  AAC_TS_SIDECAR: 'off',
  AAC_AGENT_VERBS: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_RUST_STORE: 'off',
  AAC_RUST_GROUP_COLLECTION: 'on',
  AAC_RUST_GROUP_APP_UPDATES: 'off',
};

async function ready(child, probe) {
  for (let i = 0; i < 200; i++) {
    assert.equal(child.exitCode, null, 'the fixture listener exited before readiness');
    assert.equal(child.signalCode, null, 'the fixture listener was stopped before readiness');
    if (await probe()) return;
    await delay(20);
  }
  throw new Error('the fixture listener did not become ready');
}

async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const done = once(child, 'exit');
  child.kill('SIGTERM');
  await Promise.race([done, delay(1500, undefined, { ref: false })]);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  await done;
}

async function writeSource(label) {
  await writeFile(
    path.join(root, 'account-usage-accounts.json'),
    JSON.stringify({
      version: 2,
      accounts: [
        {
          id: accountId,
          provider: 'zai',
          platform: 'ubuntu',
          sshHost: null,
          label,
          credential: { kind: 'aac-key', keyId: '0123abcd' },
          createdAt: null,
          createdBy: null,
        },
      ],
    }),
    { mode: 0o600 }
  );
}

function label(dashboard) {
  return dashboard.accounts.find((row) => row.id === accountId)?.label;
}

try {
  const config = createEmptyUnifiedConfig();
  await writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      ...config,
      dashboard_auth: {
        enabled: true,
        username: credentials.username,
        password_hash: await Bun.password.hash(credentials.password, {
          algorithm: 'bcrypt',
          cost: 4,
        }),
        session_timeout_hours: 24,
      },
    }),
    { mode: 0o600 }
  );
  await writeSource('Before');
  await writeFile(
    path.join(root, 'codex-profiles.yaml'),
    "version: '1.0'\ndefault: work\nprofiles:\n  work:\n    type: codex\n    created: '2026-10-01T00:00:00Z'\n    last_used: null\n",
    { mode: 0o600 }
  );
  const b64 = (value) => Buffer.from(JSON.stringify(value)).toString('base64url');
  const idToken = [
    b64({ alg: 'none', typ: 'JWT' }),
    b64({
      email: 'fixture@example.com',
      exp: 1900000000,
      'https://api.openai.com/auth': { chatgpt_account_id: 'acct-fixture' },
    }),
    Buffer.from('fixture-signature').toString('base64url'),
  ].join('.');
  await mkdir(path.join(root, 'codex-instances/work'), { recursive: true, mode: 0o700 });
  await writeFile(
    path.join(root, 'codex-instances/work/auth.json'),
    JSON.stringify({ tokens: { id_token: idToken, account_id: null } }),
    { mode: 0o600 }
  );
  await mkdir(path.join(root, '.codex/sessions/2026/11/19'), { recursive: true, mode: 0o700 });
  await writeFile(
    path.join(root, '.codex/sessions/2026/11/19/rollout-fixture.jsonl'),
    JSON.stringify({
      payload: {
        rate_limits: {
          primary: { used_percent: 24, resets_at: 1795090800, window_minutes: 300 },
        },
      },
    }) + '\n',
    { mode: 0o600 }
  );
  const registry = JSON.parse(
    await readFile(new URL('../../config/private-files.json', import.meta.url), 'utf8')
  );
  const files = Object.fromEntries(
    registry.rows.map((row) => [
      row.id,
      ['collection', 'writes', 'lifecycle'].includes(row.movesWith) ? 'rust-service' : row.owner,
    ])
  );
  const bootId = randomBytes(12).toString('hex');
  const run = path.join(root, 'run');
  const folder = path.join(run, 'aac-http-' + bootId);
  await mkdir(run, { mode: 0o700 });
  await mkdir(folder, { mode: 0o700 });
  const boot = {
    ccsDir: root,
    bootId,
    secret: randomBytes(32).toString('hex'),
    socketPath: path.join(folder, 'http.sock'),
    files,
    nativeLifecycle: true,
  };
  const service = spawn(serviceBinary, ['--native-collection'], {
    env,
    stdio: ['pipe', 'ignore', 'inherit'],
  });
  children.push(service);
  service.stdin.write(JSON.stringify(boot) + '\n');
  await ready(service, async () => (await stat(boot.socketPath).catch(() => null))?.isSocket());
  await writeFile(path.join(run, 'aac-http.json'), JSON.stringify(boot), { mode: 0o600 });

  const reservation = createServer();
  reservation.listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const { port } = reservation.address();
  await new Promise((resolve) => reservation.close(resolve));
  const front = spawn(
    launcher,
    ['dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
    { env, stdio: ['ignore', 'ignore', 'inherit'] }
  );
  children.push(front);
  const origin = 'http://127.0.0.1:' + port;
  const send = (uri, options = {}) =>
    fetch(origin + uri, { ...options, signal: AbortSignal.timeout(15000) });
  const post = (uri, body, headers) =>
    send(uri, {
      method: 'POST',
      body: JSON.stringify(body),
      headers: { 'content-type': 'application/json', ...headers },
    });
  await ready(front, async () => (await send('/api/health').catch(() => null))?.ok);
  const login = await post('/api/auth/login', credentials, { origin });
  assert.equal(login.status, 200);
  const cookie = login.headers.get('set-cookie').split(';')[0];

  const initial = await send(dashboardPath, { headers: { cookie } });
  const noOrigin = await post(accountPath + '/recheck', {}, { cookie });
  assert.deepEqual(
    [initial.status, noOrigin.status],
    [200, 403],
    'the adopted service supplies the public dashboard and existing Re-check admission'
  );
  assert.equal(label(await initial.json()), 'Before');
  const quotas = await send('/api/codex/profiles/quotas', { headers: { cookie } });
  assert.equal(quotas.status, 200);
  const quotaBody = await quotas.json();
  assert.equal(quotaBody.profiles.length, 1, 'the saved profile is listed');
  assert.equal(quotaBody.profiles[0].profileName, 'work');
  assert.equal((await noOrigin.json()).code, 'origin_required');
  const tag = initial.headers.get('etag');
  assert(tag);
  const head = await send(dashboardPath, { method: 'HEAD', headers: { cookie } });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), '');
  const conditional = await send(dashboardPath, { headers: { cookie, 'if-none-match': '*' } });
  assert.equal(conditional.status, 304);
  const bypass = await send(dashboardPath, {
    headers: { cookie, 'if-none-match': '*', 'cache-control': 'no-cache' },
  });
  assert.equal(bypass.status, 200);

  await writeSource('After');
  const held = await send(dashboardPath, { headers: { cookie } });
  assert.equal(label(await held.json()), 'Before', 'the first generation is still held');
  const checked = await post(accountPath + '/recheck', {}, { cookie, origin });
  assert.equal(checked.status, 200);
  assert.equal((await checked.json()).account.label, 'After');
  const next = await send(dashboardPath, { headers: { cookie, 'if-none-match': tag } });
  assert.equal(next.status, 200);
  assert.notEqual(next.headers.get('etag'), tag);
  assert.equal(
    label(await next.json()),
    'After',
    'Re-check updates the dashboard generation before it returns'
  );
  const repeated = await post(accountPath + '/recheck', {}, { cookie, origin });
  assert.equal(repeated.status, 429);
  assert.equal((await repeated.json()).code, 'rate_limited');
  assert(Number(repeated.headers.get('retry-after')) > 0);

  const pairing = await post(
    '/api/auth/devices/pair',
    {
      ...credentials,
      deviceName: 'Fixture device',
      platform: 'mac',
      installId: '00000000-0000-4000-8000-000000000001',
      appVersion: '0.0.0',
    },
    { origin }
  );
  assert.equal(pairing.status, 201);
  const { token } = await pairing.json();
  const authorization = 'Bearer ' + token;
  const device = await send(dashboardPath, { headers: { authorization } });
  assert.equal(device.status, 200);
  assert.equal(label(await device.json()), 'After');
  const deviceRecheck = await post(accountPath + '/recheck', {}, { authorization, origin });
  assert.equal(deviceRecheck.status, 403);
  assert.equal((await deviceRecheck.json()).code, 'device_scope');

  await stop(service);
  for (const response of await Promise.all([
    send(dashboardPath, { headers: { cookie } }),
    post(accountPath + '/recheck', {}, { cookie, origin }),
  ])) {
    assert.equal(response.status, 503);
    assert.equal((await response.json()).code, 'route_unavailable');
  }
  console.log(
    'Adopted collection regression: 1 scenario passed (dashboard, Re-check, device scope and owner loss).'
  );
} finally {
  for (const child of children.reverse()) await stop(child);
  await rm(base, { recursive: true, force: true });
}
