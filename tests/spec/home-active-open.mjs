// Astra M3 regression: an active Claude Open appears on retained Home.
//
// The real shipping sidecar composition: the compiled launcher is the public front door (it owns
// Profiles::open) and the real TypeScript worker is its sidecar. A managed Open returns 202, the
// public profiles route shows its progress, and retained Home must show the same operation in
// both modes (the native Home group on, and the view switch on with the group off).
//
//   AAC_M6_TARGET_CLI=<aac-http> bun tests/spec/home-active-open.mjs
//   AAC_RECHECK_HOME_GROUP=off ...   (the second mode)
//
// Fixture data only, in a temporary CCS home; no provider is called.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';

const launcher = process.env.AAC_M6_TARGET_CLI;
assert(launcher && path.isAbsolute(launcher), 'set AAC_M6_TARGET_CLI to a native build');
const snapshot = false;
const fixed = process.env.AAC_TEST_WORKER_SOURCE;
if (snapshot) assert.equal(process.platform, 'win32', 'the share writer is retained on Windows');
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
    agentTransportSsh: {
      identityFile: path.join(root, 'fixture-key'),
      knownHostsFile: path.join(root, 'fixture-known-hosts'),
    },
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
  const mac = {
    launcherName: 'Claude Fixture',
    launcherPath: '/Applications/Claude Fixture.app',
    profilePath: '/Users/fixture/Library/Application Support/Claude-fixture',
    sshHost: 'jared-mac',
  };
  const windows = {
    launcherName: 'Claude Fixture',
    profilePath: 'C:/Users/fixture/AppData/Roaming/Claude-fixture',
    sshHost: 'jared-windows',
  };
  const endpoint = (alias) => ({ alias, hostname: 'fixture-host', username: 'fixture', port: 22 });
  await write(path.join(root, 'claude-desktop-profiles.json'), {
    version: 1,
    profiles: [
      {
        id: 'fixture-mac',
        email: 'fixture@example.invalid',
        mac,
        windows,
        historySync: {
          version: 1,
          enabled: true,
          sourcePlatform: 'windows',
          identity: {
            accountUuid: '00000000-0000-4000-8000-000000000421',
            organizationUuid: '00000000-0000-4000-8000-000000000821',
          },
          project: {
            cwd: '/mnt/Cursor/PuppetMaster',
            originCwd: '/mnt/Cursor/PuppetMaster',
            transcriptRoot: '/fixture/.claude/projects',
          },
          ssh: { mac: endpoint(mac.sshHost), windows: endpoint(windows.sshHost) },
        },
      },
    ],
  });
  await writeFile(path.join(root, 'fixture-key'), 'test identity', { mode: 0o600 });
  await writeFile(path.join(root, 'fixture-known-hosts'), 'test pin', { mode: 0o600 });
  await writeFile(
    path.join(root, 'ssh'),
    '#!/bin/sh\nif [ "$1" = "-G" ]; then exit 0; fi\nsleep 4\nexit 0\n',
    { mode: 0o700 }
  );
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
    if (${snapshot}) {
      const read = globalThis.fetch;
      globalThis.fetch = async (url, options) => {
        const response = await read(url, options);
        if (!response.ok || !String(url).endsWith('/sample')) return response;
        const generation = await response.json();
        for (const row of generation.accounts.filter((entry) => entry.id === ${JSON.stringify(id)})) {
          const usedPercent = row.label === 'Before' ? 20 : 35;
          row.status = 'ok';
          row.sampledAt = new Date(generation.sampledAt).toISOString();
          row.windows = [{
            key: 'five-hour', label: '5 hours', kind: 'rate_limit', usedPercent,
            remainingPercent: 100 - usedPercent, windowMinutes: 300, resetAt: null,
            used: null, limit: null, unit: null,
          }];
        }
        return Response.json(generation);
      };
    }
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
        PATH: root + ':' + process.env.PATH,
        AAC_AGENT_PACKAGE_ROOT: fixed,
        CCS_HOME: base,
        CCS_DIR: root,
        HOME: base,
        USERPROFILE: base,
        CLAUDE_CONFIG_DIR: path.join(base, '.claude'),
        CODEX_HOME: path.join(base, '.codex'),
        CCS_CODEX_PATH: path.join(base, 'no-codex'),
        AAC_AGENT_TRANSPORT: 'off',
        AAC_AGENT_VERBS: 'off',
        AAC_RUST_STORE: 'off',
        AAC_RUST_CUT_NATIVE_ROUTES: 'off',
        AAC_RUST_GROUP_HOME: process.env.AAC_RECHECK_HOME_GROUP ?? 'on',
        ...(process.env.AAC_RECHECK_HOME_GROUP === 'off' ? { AAC_RUST_HOME_VIEW: 'on' } : {}),
        AAC_RUST_GROUP_COLLECTION: 'on',
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
  const open = await send('/api/claude/desktop-profiles/fixture-mac/open', {
    method: 'POST',
    headers: { cookie, origin, 'content-type': 'application/json', prefer: 'respond-async' },
    body: '{"platform":"mac"}',
  });
  console.log('Open status:', open.status);
  assert.equal(open.status, 202);
  const headers = { cookie, origin };
  const [profiles, home] = await Promise.all([
    send('/api/claude/desktop-profiles', { headers }),
    send('/api/views/home?platform=mac', { headers }),
  ]);
  assert.equal(profiles.status, 200);
  assert.equal(home.status, 200);
  const direct = (await profiles.json()).profiles.find((row) => row.id === 'fixture-mac');
  const retained = (await home.json()).profiles.find((row) => row.id === 'fixture-mac');
  console.log(
    'Open operation present:',
    Boolean(direct?.openOperation),
    'on retained Home:',
    Boolean(retained?.openOperation)
  );
  assert(direct?.openOperation, 'the public owner must have active progress');
  assert(retained?.openOperation, 'retained Home must show active progress');
} finally {
  if (front && front.exitCode === null && front.signalCode === null) {
    const exited = once(front, 'exit');
    front.kill('SIGTERM');
    await exited;
  }
  await rm(base, { recursive: true, force: true, maxRetries: 4 });
}
