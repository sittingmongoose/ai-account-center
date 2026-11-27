/** Linux public regression for persisted Analytics activity through the native dashboard. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { startRustFixture } from './m6-rust-fixture';

const [, , dashboardPath, agentPath, finding] = process.argv;
assert(dashboardPath && agentPath && finding === 'H1');
assert.equal(process.platform, 'linux');
const agent = resolve(agentPath);
const root = await mkdtemp(join(tmpdir(), 'aac-fx4-'));
const home = join(root, 'home');
const ccs = join(home, '.ccs');
const analytics = '/api/accounts/analytics?platform=windows&range=7d&tz=UTC';
const hash = (bytes: string | Buffer) => createHash('sha256').update(bytes).digest('hex');
const username = 'fixture-admin';
const password = 'fx4-fixture-password';

async function put(file: string, body: string) {
  await mkdir(dirname(file), { recursive: true, mode: 0o700 });
  await writeFile(file, body + '\n', { mode: 0o600 });
}


async function until<T>(
  message: string,
  read: () => Promise<T>,
  ready: (value: T) => boolean,
  failed: () => boolean = () => false
) {
  const deadline = Date.now() + 80_000;
  let result: T;
  do {
    result = await read();
    if (ready(result)) return result;
    if (failed()) break;
    await Bun.sleep(500);
  } while (Date.now() < deadline);
  assert.fail(message + ' ' + JSON.stringify(result).slice(0, 1800));
}

try {
  await put(join(ccs, 'config.yaml'), JSON.stringify({
    dashboard_auth: {
      enabled: true,
      username,
      password_hash: Bun.password.hashSync(password, { algorithm: 'bcrypt', cost: 4 }),
    },
    computers: [{ id: 'ubuntu', label: 'Fixture', os: 'linux', transport: { kind: 'local' } }],
  }));
  await put(join(ccs, 'account-refresh-settings.json'), '{"refreshIntervalSeconds":30}');
  await put(join(ccs, 'models-dev-registry-cache.json'), JSON.stringify({
    version: 1,
    fetchedAt: Date.now(),
    providers: { anthropic: { models: { 'claude-sonnet-4-5': { cost: { input: 3, output: 15 } } } } },
  }));
  if (finding === 'H1') {
    const version = Bun.spawnSync([agent, '--version']);
    assert.equal(version.exitCode, 0);
    const { target, version: agentVersion } = JSON.parse(version.stdout.toString());
    await put(join(root, 'agents.json'), JSON.stringify({
      artifacts: [{ target, version: agentVersion, file: agent, sha256: hash(await readFile(agent)) }],
    }));
    await put(join(home, '.claude/projects/fixture/ordinary.jsonl'), JSON.stringify({
      type: 'assistant',
      timestamp: new Date(Date.now() - 60_000).toISOString(),
      sessionId: 'fixture-claude-session',
      requestId: 'fixture-request',
      message: {
        id: 'fixture-message', model: 'claude-sonnet-4-5',
        usage: { input_tokens: 70, output_tokens: 30 },
      },
    }));
  }
  const dashboard = await startRustFixture(resolve(dashboardPath), ccs, {
    PATH: process.env.PATH,
    HOME: home,
    XDG_CONFIG_HOME: join(home, '.config'),
    XDG_CACHE_HOME: join(home, '.cache'),
    CCS_HOME: ccs,
    AAC_AGENT_PACKAGE_ROOT: root,
    AAC_AGENT_CATALOG: join(root, 'agents.json'),
    AAC_AGENT_TRANSPORT: finding === 'H1' ? 'on' : 'off',
    AAC_AGENT_VERBS: 'off',
    AAC_RUST_CUT_NATIVE_ROUTES: 'on',
    AAC_RUST_GROUP_COLLECTION: 'on',
    AAC_RUST_GROUP_HOME: 'on',
    AAC_RUST_GROUP_APP_UPDATES: 'off',
    AAC_STATUS_DOORBELL: 'off',
    AAC_STORE_SHARE_BACKUP_DIR: 'off',
    HTTP_PROXY: 'http://127.0.0.1:1',
    HTTPS_PROXY: 'http://127.0.0.1:1',
    NO_PROXY: '127.0.0.1',
  }, false, 'cut-rust-only');
  try {
    const login = await fetch(dashboard.url + '/api/auth/login', {
      method: 'POST',
      headers: { origin: dashboard.url, 'content-type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });
    assert.equal(login.status, 200);
    const cookie = login.headers.get('set-cookie')?.split(';')[0];
    assert(cookie);
    const get = (path: string) => fetch(dashboard.url + path, { headers: { cookie } });
    const json = async (path: string) => {
      const response = await get(path);
      assert.equal(response.status, 200, path);
      return response.json();
    };
    if (finding === 'H1') {
      const panic = () => dashboard.audit.join('').includes('collect.rs:468');
      const answer = await until(
        'Claude collection did not persist the session',
        () => json(analytics),
        (value) => value.activity.totals?.inputTokens === 70,
        panic
      );
      assert.equal(answer.activity.totals.outputTokens, 30);
      assert.equal(answer.activity.sessions.total, 1);
      assert.equal(answer.activity.sessions.sample.length, 1);
      assert(answer.activity.sessions.sample[0].estimatedCostUsd > 0);
      console.log('H1 passed: public writer persisted one priced 70/30 Claude session.');
    }
  } catch (error) {
    console.error(dashboard.audit.join('').slice(-4000));
    throw error;
  } finally {
    await dashboard.stop();
  }
} finally {
  await rm(root, { recursive: true, force: true });
}
