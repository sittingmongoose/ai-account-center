/**
 * M5 lane K, gap G11: the reset source in the Rust service against the TS reset-source-adapter.
 * The historical router and the worker-free Rust listener run whole claims for a saved Codex profile and a Windows Claude launcher:
 * first with the TS adapter (and the native module as the claims file's writer), then with the
 * Rust service owning the claims file, resolving the accounts and sending. Fixtures only:
 * `codex` is tests/fixtures/reset-source/any-usage-codex, `ssh` is
 * tests/fixtures/reset-source/claude-ssh-host (the packaged claude_usage.py with its provider
 * clients replaced). Never a real host, provider or claim.
 *
 * Equal: every route reply (opaque tokens numbered, clocks zeroed), the ledger's stored
 * identities, every SSH argv and stdin payload, and exactly one send per claim. Also: the
 * explicit-transport SSH policy with an older client, the not-sent and ambiguous send rules.
 * Unix only. `--ts-only` runs the TS adapter alone.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import express from 'express';
import { Database } from 'bun:sqlite';
import { startResetFixture } from './reset-http-fixture';

const tsOnly = process.argv.includes('--ts-only');
const repo = path.resolve(import.meta.dir, '../..');
const fixtures = path.join(repo, 'tests/fixtures/reset-source');
// The native module, the service and the helper find `codex` and `ssh` on the process PATH,
// which a `process.env` assignment under Bun does not reach: start again with the fakes first.
if (!process.env.AAC_RESET_PARITY_BIN) {
  const bin = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-source-bin-')));
  fs.chmodSync(bin, 0o700);
  fs.copyFileSync(path.join(fixtures, 'any-usage-codex'), path.join(bin, 'codex'));
  fs.copyFileSync(path.join(fixtures, 'claude-ssh-host'), path.join(bin, 'ssh'));
  for (const name of ['codex', 'ssh']) fs.chmodSync(path.join(bin, name), 0o755);
  const { spawnSync } = await import('node:child_process');
  const child = spawnSync(process.execPath, [import.meta.path, ...process.argv.slice(2)], {
    stdio: 'inherit',
    env: {
      ...process.env,
      PATH: `${bin}${path.delimiter}${process.env.PATH}`,
      AAC_RESET_PARITY_BIN: bin,
    },
  });
  fs.rmSync(bin, { recursive: true, force: true });
  process.exit(child.status ?? 1);
}
const bin = process.env.AAC_RESET_PARITY_BIN;
assert.equal(process.env.PATH?.split(path.delimiter)[0], bin, 'the fakes come first on PATH');
for (const [name, fixture] of [
  ['codex', 'any-usage-codex'],
  ['ssh', 'claude-ssh-host'],
])
  assert.deepEqual(
    fs.readFileSync(path.join(bin, name)),
    fs.readFileSync(path.join(fixtures, fixture))
  );
const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-source-')));
fs.chmodSync(scratch, 0o700);
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
for (const key of ['AAC_AGENT_IDENTITY_FILE', 'AAC_AGENT_KNOWN_HOSTS_FILE'])
  delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: scratch,
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
assert.ok(process.env.AAC_STATUS_MODULE, 'AAC_STATUS_MODULE must name the built native module');

const { createResetClaimRouter } = await import('../fixtures/reset-adapter/reset-claim-routes');
const { createResetSourceAdapter } = await import('../fixtures/reset-adapter/reset-source-adapter');

const CODEX = 'codex:fixture';
const CLAUDE = 'claude:fixture-claude';
const X = '11111111-1111-1111-1111-111111111111';

/** One root: a saved Codex profile, a Claude launcher on the Windows alias, an optional config. */
function seed(root: string, config?: Record<string, unknown>) {
  fs.mkdirSync(root, { mode: 0o700 });
  fs.writeFileSync(
    path.join(root, 'codex-profiles.yaml'),
    "version: '1'\ndefault: fixture\nprofiles:\n  fixture:\n    type: codex\n    created: '2026-11-01T00:00:00Z'\n    last_used: null\n"
  );
  const home = path.join(root, 'codex-instances', 'fixture');
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  const claims = Buffer.from(
    JSON.stringify({
      email: 'fixture-account-x',
      'https://api.openai.com/auth': { chatgpt_account_id: 'account-x' },
    })
  ).toString('base64url');
  fs.writeFileSync(
    path.join(home, 'auth.json'),
    JSON.stringify({ tokens: { account_id: 'account-x', id_token: `e30.${claims}.fixture` } })
  );
  fs.writeFileSync(
    path.join(home, 'fixture-state.json'),
    JSON.stringify({ five: 100, week: 40, spent: false, outcome: 'reset', refill: { five: 0 } })
  );
  fs.writeFileSync(
    path.join(root, 'claude-desktop-profiles.json'),
    JSON.stringify({
      version: 1,
      profiles: [
        {
          id: 'fixture-claude',
          email: 'fixture@example.com',
          windows: {
            launcherName: 'Claude (fixture)',
            profilePath: 'C:\\Users\\fixture\\AppData\\Roaming\\Claude',
            sshHost: 'fixture-windows',
          },
        },
      ],
    })
  );
  if (config) fs.writeFileSync(path.join(root, 'config.yaml'), JSON.stringify(config));
  // The fake Claude host's state lives beside it; each root starts it fresh.
  for (const name of [
    'ssh-argv.jsonl',
    'ssh-payload.jsonl',
    'claims.jsonl',
    'claimed',
    'claim-mode',
  ])
    fs.rmSync(path.join(bin, name), { force: true });
  fs.writeFileSync(path.join(bin, 'login'), X);
  return home;
}

function lines(file: string): unknown[] {
  return fs.existsSync(file)
    ? fs
        .readFileSync(file, 'utf8')
        .trim()
        .split('\n')
        .filter(Boolean)
        .map((l) => JSON.parse(l))
    : [];
}

interface Reply {
  status: number;
  body: unknown;
}

async function startReference() {
  const app = express();
  app.use(express.json());
  app.use((req: any, _res, next) => {
    req.session = { authenticated: true };
    req.sessionID = 'fixture-browser';
    next();
  });
  app.use('/api/accounts', createResetClaimRouter({ source: createResetSourceAdapter() }));
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  return {
    url: `http://127.0.0.1:${(server.address() as any).port}`,
    cookie: '',
    async stop() {
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}

async function scenario(root: string, rust: boolean, hang = false) {
  process.env.CCS_DIR = root;
  const fixture = rust ? await startResetFixture(repo, root) : await startReference();
  const origin = fixture.url;
  const steps: Record<string, Reply> = {};
  try {
    for (const account of [CODEX, CLAUDE]) {
      const url = `${origin}/api/accounts/${encodeURIComponent(account)}/reset-claims`;
      const send = async (suffix: string, body?: unknown): Promise<Reply> => {
        const response = await fetch(url + suffix, {
          method: body === undefined ? 'GET' : 'POST',
          headers:
            body === undefined
              ? { cookie: fixture.cookie }
              : { cookie: fixture.cookie, origin, 'Content-Type': 'application/json' },
          body: body === undefined ? undefined : JSON.stringify(body),
        });
        assert.equal(response.headers.get('cache-control'), 'no-store');
        assert.equal(response.headers.get('content-type'), 'application/json; charset=utf-8');
        return { status: response.status, body: await response.json() };
      };
      const tag = account.split(':')[0];
      steps[`${tag}.before`] = await send('');
      const grants = (steps[`${tag}.before`].body as any).grants;
      assert.ok(grants?.length >= 1, `${account}: ${JSON.stringify(steps[`${tag}.before`])}`);
      const key = grants[0].grantKey as string;
      steps[`${tag}.prepare`] = await send('/prepare', { grantKey: key });
      const token = (steps[`${tag}.prepare`].body as any).confirmationToken as string;
      if (hang && account === CLAUDE) fs.writeFileSync(path.join(bin, 'claim-mode'), 'hang');
      steps[`${tag}.claim`] = await send('', { grantKey: key, confirmationToken: token });
      steps[`${tag}.after`] = await send('');
      const attempt = (steps[`${tag}.claim`].body as any).attemptKey;
      if (attempt) steps[`${tag}.check`] = await send('/check', { attemptKey: attempt });
    }
  } finally {
    await fixture.stop();
  }
  const db = new Database(path.join(root, 'account-usage', 'reset-claims', 'claims.sqlite'), {
    readonly: true,
  });
  const stored = db
    .query('SELECT account, snapshot FROM targets ORDER BY rowid')
    .all()
    .map((row: any) => ({ account: row.account, identity: JSON.parse(row.snapshot).identity }));
  db.close();
  return {
    steps,
    stored,
    codexSends: lines(path.join(root, 'codex-instances', 'fixture', 'sent.jsonl')).length,
    claudeClaims: lines(path.join(bin, 'claims.jsonl')),
    sshArgv: lines(path.join(bin, 'ssh-argv.jsonl')),
    sshPayload: lines(path.join(bin, 'ssh-payload.jsonl')),
  };
}

/** Opaque 64-hex values become their order of first appearance; clock readings become 0. */
function comparable(value: unknown): unknown {
  const seen = new Map<string, string>();
  const walk = (item: unknown, key = ''): unknown => {
    if (typeof item === 'string' && /^[A-Za-z0-9_-]{32,}$/.test(item) && /[0-9]/.test(item)) {
      if (!seen.has(item)) seen.set(item, `<opaque ${seen.size}>`);
      return seen.get(item);
    }
    if (typeof item === 'number' && /(Ms|At)$/.test(key)) return 0;
    if (typeof item === 'string' && /At$/.test(key) && !Number.isNaN(Date.parse(item)))
      return '<time>';
    if (Array.isArray(item)) return item.map((entry) => walk(entry));
    if (item && typeof item === 'object')
      return Object.fromEntries(Object.entries(item).map(([k, v]) => [k, walk(v, k)]));
    return item;
  };
  return walk(value);
}

async function directSends() {
  const write = (routeAccount: string) => ({
    method: 'x',
    body: {},
    identity: {},
    attemptKey: 'a'.repeat(64),
    sourceBinding: 'b'.repeat(64),
    routeAccount,
  });
  const out: Record<string, unknown> = {};
  for (const account of ['claude:missing', 'codex:missing', 'cursor:any']) {
    try {
      out[account] = await createResetSourceAdapter().send(write(account));
    } catch (error) {
      out[account] = `throws ${String((error as Error).message).split(':', 1)[0]}`;
    }
  }
  return out;
}

const explicit = (root: string) => ({
  version: 10,
  computers: {
    windows: {
      id: 'windows',
      sshAlias: 'fixture-windows',
      transport: { host: '192.0.2.10', user: 'fixture', port: 2222, key: '~/keys/fixture' },
    },
  },
  // One pin for both modes, so the argv strings compare (the roots differ).
  agentTransportSsh: { knownHostsFile: path.join(path.dirname(root), 'known_hosts') },
});

const results: Record<string, any> = {};
try {
  const variants = [
    ['plain', false, undefined, false],
    ['explicit-old-client', false, explicit, true],
    ['claude-hang', true, undefined, false],
  ] as const;
  for (const mode of tsOnly ? ['ts'] : ['ts', 'rust']) {
    for (const [name, hang, configure, oldClient] of variants) {
      const root = path.join(scratch, `${name}-${mode}`);
      seed(root, configure?.(root));
      if (oldClient) fs.writeFileSync(path.join(bin, 'no-knownhostscommand'), '1');
      else fs.rmSync(path.join(bin, 'no-knownhostscommand'), { force: true });
      // Capabilities are cached per executable identity (both sides): a changed client is
      // another file, so the older client is a rewritten copy and is probed again.
      const original = fs.readFileSync(path.join(fixtures, 'claude-ssh-host'));
      fs.writeFileSync(
        path.join(bin, 'ssh'),
        oldClient ? Buffer.concat([original, Buffer.from('# older client\n')]) : original
      );
      const run = await scenario(root, mode === 'rust', hang);
      results[`${name}.${mode}`] = run;
      if (mode === 'rust' && oldClient) {
        // G21: the service's own tracing event (the capability downgrade) is a log entry under
        // its root, in the dashboard log's format.
        const entries = lines(path.join(root, 'logs', 'current.jsonl')) as any[];
        const downgrade = entries.find(
          (e) =>
            e.source === 'rust-service' &&
            e.level === 'warn' &&
            e.context?.option === 'KnownHostsCommand'
        );
        assert.ok(downgrade, 'the Rust service logged the SSH downgrade');
        assert.deepEqual(Object.keys(downgrade), [
          'id',
          'timestamp',
          'level',
          'source',
          'event',
          'message',
          'processId',
          'runId',
          'context',
        ]);
      }
      // Exactly one provider send per claim, in both modes.
      assert.equal(run.codexSends, 1, `${name}.${mode} codex sends`);
      assert.equal(run.claudeClaims.length, 1, `${name}.${mode} claude claims`);
      const claude = (run.steps['claude.claim'].body as any) ?? {};
      if (hang) assert.notEqual(claude.outcome, 'claimed', 'a lost host answer is never a success');
      else assert.equal(claude.outcome, 'claimed', JSON.stringify(run.steps['claude.claim']));
      assert.equal((run.steps['codex.claim'].body as any).outcome, 'claimed');
    }
  }
  for (const [name] of variants) {
    if (!tsOnly) {
      const ts = results[`${name}.ts`];
      const rust = results[`${name}.rust`];
      assert.deepEqual(comparable(rust.steps), comparable(ts.steps), `${name}: route replies`);
      assert.equal(
        JSON.stringify(comparable(rust.steps)),
        JSON.stringify(comparable(ts.steps)),
        `${name}: reply bytes`
      );
      assert.deepEqual(rust.stored, ts.stored, `${name}: ledger identities`);
      assert.deepEqual(rust.sshArgv, ts.sshArgv, `${name}: ssh argv`);
      assert.deepEqual(
        comparable(rust.sshPayload),
        comparable(ts.sshPayload),
        `${name}: ssh payloads`
      );
      assert.deepEqual(
        comparable(rust.claudeClaims),
        comparable(ts.claudeClaims),
        `${name}: claim bodies`
      );
    }
  }
  const tsDirect = await directSends();
  assert.deepEqual(tsDirect, {
    'claude:missing': { outcome: 'not-sent' },
    'codex:missing': 'throws credential-host-unavailable',
    'cursor:any': 'throws provider-read-only',
  });
  const argv = results['explicit-old-client.ts'].sshArgv[0] as string[];
  assert.ok(argv.includes('Hostname=192.0.2.10') && argv.includes('Port=2222'));
  assert.ok(!argv.includes('KnownHostsCommand=none') && argv.includes('SetEnv=AAC_SSH_POLICY=1'));
  console.log(
    `reset source parity: 3 root variants x ${Object.keys(results['plain.ts'].steps).length} replies, ssh argv/payloads, ledger identities, one send per claim, direct send rules (${tsOnly ? 'TS' : 'TS/Rust'})`
  );
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
