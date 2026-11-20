// The real shipping worker and launcher, with its provider boundaries replaced by fixtures.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';

const launcher = process.env.AAC_M6_TARGET_CLI;
assert(launcher && path.isAbsolute(launcher), 'set AAC_M6_TARGET_CLI to a native build');
const snapshot = process.argv.includes('--snapshot');
const disabled = process.argv.includes('--collection-off');
assert(!(snapshot && disabled), 'select one collection fixture');
const source =
  process.env.AAC_TEST_WORKER_SOURCE ?? fileURLToPath(new URL('../..', import.meta.url));
const base = await mkdtemp(path.join(os.tmpdir(), 'aac-native-worker-'));
const root = path.join(base, '.ccs');
const share = path.join(base, 'usage-share');
const calls = path.join(base, 'collector-calls');
const clock = path.join(base, 'clock-offset');
const log = [];
let front;
const write = (target, value) => writeFile(target, JSON.stringify(value), { mode: 0o600 });
const id = 'zai:acct:0123abcd';
const hubKey = 'aacu_' + 'f'.repeat(43);
const credentials = { username: 'fixture-owner', password: 'fixture-only-password' };
const readJson = async (target) => JSON.parse(await readFile(target, 'utf8'));
async function until(action) {
  const deadline = Date.now() + (snapshot ? 45_000 : 15_000);
  for (;;) {
    if (front.exitCode !== null || front.signalCode !== null) {
      throw new Error('The fixture dashboard stopped. ' + log.join('').slice(-2048));
    }
    const answer = await action().catch(() => undefined);
    if (answer) return answer;
    if (Date.now() >= deadline)
      throw new Error(
        'The fixture did not advance its native collection feed. ' + log.join('').slice(-2048)
      );
    await delay(100);
  }
}

async function registry(label) {
  await write(path.join(root, 'account-usage-accounts.json'), {
    version: 2,
    accounts: [
      {
        id,
        provider: 'zai',
        platform: 'ubuntu',
        sshHost: null,
        label,
        credential: { kind: 'aac-key', keyId: '0123abcd' },
        createdAt: null,
        createdBy: null,
      },
    ],
  });
}

try {
  await mkdir(root, { mode: 0o700 });
  await mkdir(share, { mode: 0o700 });
  await write(path.join(root, 'config.yaml'), {
    version: '1',
    dashboard_auth: {
      enabled: true,
      username: credentials.username,
      password_hash: await Bun.password.hash(credentials.password, {
        algorithm: 'bcrypt',
        cost: 4,
      }),
      session_timeout_hours: 24,
    },
  });
  if (snapshot) {
    await write(path.join(root, 'agent-usage-snapshot.json'), { version: 1, directory: share });
    await write(path.join(root, 'account-refresh-settings.json'), { refreshIntervalSeconds: 30 });
  }
  if (disabled) {
    const auth = path.join(root, 'auth');
    await mkdir(auth, { mode: 0o700 });
    await write(path.join(auth, 'usage-hub-key.json'), {
      version: 1,
      keySha256: createHash('sha256').update(hubKey).digest('hex'),
      createdAt: '2026-01-01T00:00:00.000Z',
    });
  }
  await registry('Before');
  await writeFile(calls, '');
  await writeFile(clock, '0');

  const entry = path.join(base, 'worker.ts');
  const script = path.join(base, 'worker.cjs');
  const module = (file) => pathToFileURL(path.join(source, 'src/web-server', file)).href;
  await writeFile(
    entry,
    `
    import fs from 'node:fs';
    const wallClock = Date.now;
    Date.now = () => wallClock() + Number(fs.readFileSync(${JSON.stringify(clock)}, 'utf8'));
    const { AccountDashboardService } = await import(${JSON.stringify(module('services/account-dashboard-service.ts'))});
    AccountDashboardService.prototype.collect = async function (state) {
      fs.appendFileSync(${JSON.stringify(calls)}, 'collect\\n');
      state.fetchedAt = Date.now();
      return [];
    };
    await import(${JSON.stringify(module('sidecar.ts'))});
  `
  );
  await writeFile(
    script,
    `
    const child = require('node:child_process').spawn(${JSON.stringify(process.execPath)},
      [${JSON.stringify(entry)}], { stdio: 'inherit', windowsHide: true });
    child.once('error', () => process.exit(1));
    child.once('exit', (code) => process.exit(code ?? 1));
  `
  );

  const socket = createServer().listen(0, '127.0.0.1');
  await once(socket, 'listening');
  const { port } = socket.address();
  await new Promise((resolve) => socket.close(resolve));
  front = spawn(
    launcher,
    ['dashboard', '--no-open', '--host', '127.0.0.1', '--port', String(port)],
    {
      env: {
        ...process.env,
        CCS_HOME: base,
        CCS_DIR: root,
        HOME: base,
        USERPROFILE: base,
        CLAUDE_CONFIG_DIR: path.join(base, '.claude'),
        CODEX_HOME: path.join(base, '.codex'),
        CCS_CODEX_PATH: path.join(base, 'no-codex'),
        AAC_AGENT_TRANSPORT: 'off',
        AAC_AGENT_VERBS: 'off',
        AAC_RUST_STORE: snapshot ? 'on' : 'off',
        AAC_RUST_CUT_NATIVE_ROUTES: 'off',
        AAC_RUST_HOME_VIEW: 'off',
        AAC_RUST_GROUP_HOME: 'on',
        AAC_RUST_GROUP_COLLECTION: disabled ? 'off' : 'on',
        AAC_RUST_GROUP_APP_UPDATES: 'off',
        AAC_STATUS_DOORBELL: 'off',
        AAC_STORE_SHARE_BACKUP_DIR: 'off',
        AAC_AGENT_REPORTS_DIR: path.join(base, 'reports'),
        AAC_PM_MAIL_REGISTRY_DIR: path.join(base, 'registry'),
        AAC_TS_SIDECAR: script,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    }
  );
  for (const stream of [front.stdout, front.stderr])
    stream.on('data', (chunk) => {
      log.push(String(chunk));
      if (log.length > 64) log.shift();
    });
  const origin = 'http://127.0.0.1:' + port;
  const send = (uri, options = {}) =>
    fetch(origin + uri, { signal: AbortSignal.timeout(14000), ...options });
  await until(async () => (await send('/api/health')).ok);
  const login = await send('/api/auth/login', {
    method: 'POST',
    headers: { origin, 'content-type': 'application/json' },
    body: JSON.stringify(credentials),
  });
  assert.equal(login.status, 200);
  const cookie = login.headers.get('set-cookie').split(';')[0];
  if (disabled) {
    const dashboardPath = '/api/accounts/dashboard';
    const headers = { cookie, origin, 'content-type': 'application/json' };
    const read = await send(dashboardPath, { headers });
    assert.equal(read.status, 503);
    assert.equal(read.headers.get('cache-control'), 'no-store');
    const unavailable = {
      error: 'Account usage collection is unavailable.',
      code: 'collection_unavailable',
    };
    assert.deepEqual(await read.json(), unavailable);
    const head = await send(dashboardPath, { method: 'HEAD', headers });
    assert.equal(head.status, 503);
    assert.equal(await head.text(), '');
    const badQuery = await send(dashboardPath + '?platform=unknown', { headers });
    assert.equal(badQuery.status, 400);
    const recheck = (body, requestHeaders = headers) =>
      send('/api/accounts/' + id + '/recheck', {
        method: 'POST',
        headers: requestHeaders,
        body,
      });
    const checked = await recheck('{}');
    assert.equal(checked.status, 503);
    assert.equal(checked.headers.get('cache-control'), 'no-store');
    assert.deepEqual(await checked.json(), unavailable);
    assert.equal((await recheck('[]')).status, 400);
    assert.equal((await recheck('{}', { cookie, 'content-type': 'application/json' })).status, 403);
    assert.equal(await readFile(calls, 'utf8'), '');
    const hub = await send('/v0/management/auth-files', {
      headers: { 'x-management-key': hubKey },
    });
    assert.equal(hub.status, 503);
    assert.deepEqual(await hub.json(), unavailable);
    console.log('Disabled collection: 7 HTTP checks passed with the worker present.');
  } else if (snapshot) {
    const file = path.join(share, 'usage.json');
    const native = await send('/api/accounts/dashboard', { headers: { cookie } });
    assert.equal(native.status, 200);
    assert((await native.json()).accounts.some((row) => row.id === id));
    async function sample(label) {
      return until(async () => {
        const usage = await readJson(file);
        return usage.accounts.some((row) => row.label === label) && usage;
      });
    }
    const first = await sample('Before');
    await registry('After');
    const second = await sample('After');
    assert(Date.parse(second.generatedAt) > Date.parse(first.generatedAt));
    for (const generation of [first, second]) {
      const row = generation.accounts.find((account) => account.id === id);
      assert(row, 'the native account identity reaches the share');
      assert(
        !row.windows.some((window) => typeof window.usedPercent === 'number'),
        'a missing credential must never become a usage measurement'
      );
    }
    assert(await readJson(path.join(share, 'history.json')));
    assert.equal(
      await readFile(calls, 'utf8'),
      '',
      'snapshot publication must not collect in the worker'
    );
    console.log('Native usage share: 1 scenario passed (two native generations).');
  } else {
    const routes = [
      '/api/views/home?platform=mac',
      '/api/accounts/analytics?platform=windows&range=7d&tz=UTC',
    ];
    for (const due of [false, true]) {
      if (due) await writeFile(clock, String(2 * 60 * 60 * 1000));
      const results = [];
      for (const route of routes) {
        const reply = await send(route, { headers: { cookie, origin } });
        assert.equal(reply.status, 200, route);
        results.push(await reply.json());
      }
      assert.equal(
        await readFile(calls, 'utf8'),
        '',
        'shipping Home and Analytics must not collect in the worker'
      );
      assert(results[0].dashboard.accounts.some((row) => row.id === id && row.label === 'Before'));
    }
    console.log(
      'Shipping Home and Analytics: 2 scenarios passed (cold and due, zero worker collections).'
    );
  }
} finally {
  if (front && front.exitCode === null && front.signalCode === null) {
    const exited = once(front, 'exit');
    front.kill('SIGTERM');
    await exited;
  }
  await rm(base, { recursive: true, force: true, maxRetries: 4 });
}
