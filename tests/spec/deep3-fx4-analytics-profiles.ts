/** Linux public regressions using a pinned native reader and a local quota fixture. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { startRustFixture } from './m6-rust-fixture';

const [, , dashboardPath, agentPath, finding] = process.argv;
assert(dashboardPath && agentPath && (finding === 'H1' || finding === 'M5'));
assert.equal(process.platform, 'linux');
const agent = resolve(agentPath);
const root = await mkdtemp(join(tmpdir(), 'aac-fx4-'));
const home = join(root, 'home');
const ccs = join(home, '.ccs');
const scripts = join(root, 'scripts');
const registry = join(ccs, 'antigravity-profiles/registry-000000000001.json');
const analytics = '/api/accounts/analytics?platform=windows&range=7d&tz=UTC';
const profiles = '/api/antigravity/profiles';
const accounts = '/api/accounts/dashboard?platform=windows';
const hash = (bytes: string | Buffer) => createHash('sha256').update(bytes).digest('hex');
const username = 'fixture-admin';
const password = 'fx4-fixture-password';

async function put(file: string, body: string) {
  await mkdir(dirname(file), { recursive: true, mode: 0o700 });
  await writeFile(file, body + '\n', { mode: 0o600 });
}

async function saveProfile() {
  const credential = 'private fixture bytes\n';
  const fingerprint = hash(credential);
  const email = 'fixture@example.invalid';
  const subject = 'antigravity-fixture-subject';
  const plan = 'Google AI Pro';
  const identity = {
    email, subject, plan, verifiedAt: new Date().toISOString(), source: 'provider-userinfo',
  };
  await put(
    join(ccs, 'antigravity-instances/work/ubuntu', 'credential-' + fingerprint + '.bin'),
    credential.trimEnd()
  );
  await put(registry, JSON.stringify({
    version: 1,
    revision: 1,
    profiles: [{
      id: 'work', email, identityKey: hash(JSON.stringify(['antigravity', email, subject])), plan,
      credential: { fingerprint, format: 'fake-native-json', identity, savedAt: identity.verifiedAt },
    }],
    active: null,
    transaction: null,
  }));
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
  await put(join(scripts, 'antigravity/quota_worker.py'), [
    'import datetime, json, sys',
    'req = json.load(sys.stdin)',
    'assert req["profileId"] == "work"',
    'now = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")',
    'print(json.dumps(dict(profileId="work", identityKey=req["identityKey"],',
    '    credentialRevision=req["credentialRevision"], email="fixture@example.invalid",',
    '    plan="Google AI Pro", status="fresh", fetchedAt=now, sampledAt=now,',
    '    windows=[dict(key="seven_day", label="Weekly", usedPercent=37, windowMinutes=10080)],',
    '    source="native-consumer", identityVerified=True, identityValidation="verified")))',
  ].join('\n'));
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
    AAC_ANTIGRAVITY_SCRIPTS: scripts,
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
      const later = Date.now();
      await saveProfile();
      await until('Accounts did not read the added profile', () => json(accounts),
        (value) => value.accounts.some((row: { provider: string }) => row.provider === 'antigravity'));
      const history = await until(
        'Quota sampler did not persist the profile added after Claude collection',
        () => json('/api/usage/history?provider=antigravity&resolution=raw'),
        (value) => value.series[0]?.points?.some((point: number[]) => point[2] === 37)
      );
      assert(history.series[0].points.some((point: number[]) => point[0] >= later && point[2] === 37));
      console.log('H1 passed: public writer persisted one priced 70/30 Claude session and later sampled 37%.');
    } else {
      assert.deepEqual((await json(profiles)).profiles, []);
      await saveProfile();
      assert.equal((await json(profiles)).profiles[0].id, 'work');
      assert.equal((await json(profiles + '/quotas')).accounts[0].windows[0].usedPercent, 37);
      const homeView = '/api/views/home?platform=windows';
      await until('Accounts collection never became available', () => get(accounts),
        (response) => response.status === 200);
      assert.equal((await json(homeView)).antigravity.profiles[0].id, 'work');
      await chmod(registry, 0o644);
      const responses = await Promise.all([get(profiles), get(profiles + '/quotas'), json(homeView)]);
      assert.deepEqual([responses[0].status, responses[1].status, responses[2].antigravity],
        [500, 500, null], 'private registry refusal must reach Profiles, quotas and Home');
      await chmod(registry, 0o600);
      assert.equal((await json(profiles)).profiles[0].id, 'work');
      console.log('M5 passed: absent, private, unreadable and repaired registry states reached public reads.');
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
