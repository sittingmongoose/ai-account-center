// Astra M3 regression: an active Claude Open appears on retained Home.
//
// A managed Open returns 202 from the native public front door, the
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

const launcher = process.env.AAC_M6_TARGET_CLI;
assert(launcher && path.isAbsolute(launcher), 'set AAC_M6_TARGET_CLI to a native build');
const fixed = process.env.AAC_TEST_WORKER_SOURCE;
const computers = JSON.parse(
  await readFile(new URL('../../web-dashboard/public/computers.json', import.meta.url), 'utf8')
);
const base = await mkdtemp(path.join(os.tmpdir(), 'aac-native-open-'));
const root = path.join(base, '.ccs');
const log = [];
let front;
const write = (target, value) => writeFile(target, JSON.stringify(value), { mode: 0o600 });
const id = 'zai:acct:0123abcd';
const credentials = { username: 'fixture-owner', password: 'fixture-only-password' };
const readJson = async (target) => JSON.parse(await readFile(target, 'utf8'));
async function until(action) {
  const deadline = Date.now() + 15_000;
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
  const mac = {
    launcherName: 'Claude Fixture',
    launcherPath: '/Applications/Claude Fixture.app',
    profilePath: '/Users/fixture/Library/Application Support/Claude-fixture',
    sshHost: computers.mac.sshAlias,
  };
  const windows = {
    launcherName: 'Claude Fixture',
    profilePath: 'C:/Users/fixture/AppData/Roaming/Claude-fixture',
    sshHost: computers.windows.sshAlias,
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
        AAC_RUST_GROUP_RENEWAL: 'off',
        AAC_RUST_CUT_FIXTURE_STOP_STDIN: '1',
        AAC_AGENT_REPORTS_DIR: path.join(base, 'reports'),
        AAC_PM_MAIL_REGISTRY_DIR: path.join(base, 'registry'),
        AAC_TS_SIDECAR: path.join(base, 'worker-must-not-start.cjs'),
      },
      stdio: [process.platform === 'win32' ? 'pipe' : 'ignore', 'pipe', 'pipe'],
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
    if (process.platform === 'win32') front.stdin.end();
    else front.kill('SIGTERM');
    await exited;
  }
  await rm(base, { recursive: true, force: true, maxRetries: 4 });
}
