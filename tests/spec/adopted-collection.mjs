// Run on Unix with the parity binaries in an isolated provider home and network namespace.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import os from 'node:os';
import { promisify } from 'node:util';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createEmptyUnifiedConfig } from '../../src/config/unified-config-types';

const launcher = process.env.AAC_M6_TARGET_CLI;
const serviceBinary = process.env.AAC_RUST_SERVICE_BINARY;
assert(launcher && path.isAbsolute(launcher), 'set AAC_M6_TARGET_CLI to a native build');
assert(serviceBinary && path.isAbsolute(serviceBinary), 'set AAC_RUST_SERVICE_BINARY');

const root = await mkdtemp(path.join(os.tmpdir(), 'aac-adopted-'));
const children = [];
const accountId = 'zai:acct:0123abcd';
const accountPath = '/api/accounts/' + encodeURIComponent(accountId);
const dashboardPath = '/api/accounts/dashboard?platform=mac';
const credentials = { username: 'fixture-owner', password: 'fixture-password-only' };
const env = {
  ...process.env,
  CCS_DIR: root,
  CCS_HOME: root,
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

async function reservePort() {
  const reservation = createServer();
  reservation.listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const { port } = reservation.address();
  await new Promise((resolve) => reservation.close(resolve));
  return port;
}

const exec = promisify(execFile);
const h2 = process.argv.includes('--http2');
const dead = process.argv.includes('--dead-owner');
const wallet = process.argv.includes('--wallet-binding');
const lease = process.argv.includes('--collection-lease');
const incompatible = process.argv.includes('--incompatible-owner');

try {
  const port = await reservePort();
  const tlsPort = h2 ? await reservePort() : undefined;
  const certificate = path.join(root, 'cert.pem');
  const key = path.join(root, 'key.pem');
  if (h2) {
    await exec('openssl', [
      'req',
      '-x509',
      '-nodes',
      '-newkey',
      'rsa:2048',
      '-days',
      '1',
      '-subj',
      '/CN=127.0.0.1',
      '-addext',
      'subjectAltName=IP:127.0.0.1',
      '-out',
      certificate,
      '-keyout',
      key,
    ]);
  }
  const config = createEmptyUnifiedConfig();
  await writeFile(
    path.join(root, 'config.yaml'),
    JSON.stringify({
      ...config,
      ...(h2 && {
        dashboard_tls: {
          https_listener: {
            enabled: true,
            port: tlsPort,
            cert_path: certificate,
            key_path: key,
          },
        },
      }),
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
  const share = path.join(root, 'usage-share');
  await mkdir(share, { mode: 0o700 });
  await writeFile(
    path.join(root, 'agent-usage-snapshot.json'),
    JSON.stringify({ version: 1, directory: share }),
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
    nativeUsageSnapshot: true,
  };
  let service;
  if (dead) {
    await exec('python3', [
      '-c',
      'import os, socket, sys; s = socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); os.chmod(sys.argv[1], 0o600); s.close()',
      boot.socketPath,
    ]);
    assert((await stat(boot.socketPath)).isSocket(), 'the abandoned socket remains on disk');
  } else {
    service = spawn(serviceBinary, ['--native-collection'], {
      env,
      stdio: ['pipe', 'ignore', 'inherit'],
    });
    children.push(service);
    service.stdin.write(JSON.stringify(boot) + '\n');
    await ready(service, async () => (await stat(boot.socketPath).catch(() => null))?.isSocket());
  }
  await writeFile(path.join(run, 'aac-http.json'), JSON.stringify(boot), { mode: 0o600 });

  if (lease) {
    const secondId = randomBytes(12).toString('hex');
    const secondFolder = path.join(run, 'aac-http-' + secondId);
    await mkdir(secondFolder, { mode: 0o700 });
    const second = { ...boot, bootId: secondId, socketPath: path.join(secondFolder, 'http.sock') };
    const attempt = spawn(serviceBinary, ['--native-collection'], {
      env,
      stdio: ['pipe', 'ignore', 'inherit'],
    });
    children.push(attempt);
    attempt.stdin.write(JSON.stringify(second) + '\n');
    for (let i = 0; i < 200 && attempt.exitCode === null; i++) {
      assert.equal(
        await stat(second.socketPath).catch(() => null),
        null,
        'a second collection owner must never bind'
      );
      await delay(20);
    }
    assert.equal(attempt.exitCode, 1, 'a second collection owner must refuse startup');
  }
  const front = spawn(
    launcher,
    ['dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
    { env, stdio: ['ignore', 'ignore', 'inherit'] }
  );
  children.push(front);
  if (incompatible) {
    for (let i = 0; i < 200 && front.exitCode === null; i++) await delay(20);
    assert.equal(front.exitCode, 1, 'a live incompatible owner must prevent public startup');
    console.log('Incompatible lifecycle owner regression: 1 scenario passed.');
    process.exitCode = 0;
  } else {
    const origin = h2 ? 'https://127.0.0.1:' + tlsPort : 'http://127.0.0.1:' + port;
    const send = async (uri, options = {}) => {
      if (!h2) return fetch(origin + uri, { ...options, signal: AbortSignal.timeout(15000) });
      const args = ['--http2', '--cacert', certificate, '--silent', '--show-error', '--include'];
      if (options.method === 'HEAD') args.push('--head');
      else if (options.method) args.push('--request', options.method);
      for (const [name, value] of Object.entries(options.headers ?? {})) {
        args.push('--header', name + ': ' + value);
      }
      if (options.body !== undefined) args.push('--data-binary', options.body);
      const { stdout } = await exec('curl', [...args, origin + uri], { timeout: 15000 });
      const separator = stdout.indexOf('\r\n\r\n');
      const [statusLine, ...fields] = stdout.slice(0, separator).split('\r\n');
      assert.match(statusLine, /^HTTP\/2 \d{3}/);
      const status = Number(statusLine.split(' ')[1]);
      const headers = new Headers();
      for (const field of fields) {
        const separator = field.indexOf(':');
        headers.append(field.slice(0, separator), field.slice(separator + 1).trim());
      }
      const body = stdout.slice(separator + 4);
      return {
        status,
        headers,
        ok: status >= 200 && status < 300,
        text: async () => body,
        json: async () => JSON.parse(body),
      };
    };
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

    if (wallet) {
      const source = '{"version":1,"platform":"mac","sshHost":"fixture-mac"}';
      const hash = new Bun.CryptoHasher('sha256').update(source).digest('hex');
      const sourcePath = path.join(root, 'opencode-console-wallet-source.json');
      const pointer = path.join(root, 'opencode-console-wallet-account.json');
      const a = 'plan-opencode-go-console-mac-aaaaaaaaaaaa';
      const b = 'plan-opencode-go-console-mac-bbbbbbbbbbbb';
      const point = (id) =>
        writeFile(
          pointer,
          JSON.stringify({
            version: 1,
            binding: hash,
            accountId: id,
          }),
          { mode: 0o600 }
        );
      const remove = (id, body = {}) =>
        post('/api/accounts/' + id + '/remove', body, {
          origin,
          cookie,
        });
      await writeFile(sourcePath, source, { mode: 0o600 });
      await point(a);
      const review = await remove(a);
      assert.equal(review.status, 200);
      const { token } = (await review.json()).confirmation;
      await point(b);
      const stale = await remove(a, { confirmationToken: token });
      assert.equal(stale.status, 409, 'a reviewed wallet cannot remove its successor');
      assert.equal(await readFile(sourcePath, 'utf8'), source);
      assert.equal((await remove(a)).status, 404, 'a stale wallet is unavailable for review');
      const current = await remove(b);
      assert.equal(current.status, 200);
      assert.equal(
        (await remove(b, { confirmationToken: (await current.json()).confirmation.token })).status,
        200
      );
      assert.equal(await stat(sourcePath).catch(() => null), null);
      console.log('OpenCode wallet removal regression: 1 scenario passed.');
    } else {
      const initial = await send(dashboardPath, { headers: { cookie } });
      const noOrigin = await post(accountPath + '/recheck', {}, { cookie });
      assert.deepEqual(
        [initial.status, noOrigin.status],
        [200, 403],
        'the adopted service supplies the public dashboard and existing Re-check admission'
      );
      assert.equal(label(await initial.json()), 'Before');
      assert.equal((await noOrigin.json()).code, 'origin_required');
      if (dead) {
        console.log('Abandoned lifecycle socket regression: 1 scenario passed.');
      } else {
        await ready(service, async () => {
          const text = await readFile(path.join(share, 'usage.json'), 'utf8').catch(() => '');
          return text !== '' && label(JSON.parse(text)) === 'Before';
        });
        const tag = initial.headers.get('etag');
        assert(tag);
        const head = await send(dashboardPath, { method: 'HEAD', headers: { cookie } });
        assert.equal(head.status, 200);
        assert.equal(await head.text(), '');
        const conditional = await send(dashboardPath, {
          headers: { cookie, 'if-none-match': '*' },
        });
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
          'Adopted collection regression: 1 scenario passed (dashboard, Re-check, usage snapshot, device scope and owner loss).'
        );
      }
    }
  }
} finally {
  for (const child of children.reverse()) await stop(child);
  await rm(root, { recursive: true, force: true });
}
