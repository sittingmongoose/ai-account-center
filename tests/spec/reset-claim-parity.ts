/**
 * M5 lane K: the reset claim route with each writer of the claims file. The production router (its
 * session, origin, query and body guards, account resolution and the one provider send stay in TS)
 * runs one whole claim against a fixture Codex profile: first with the in-process native module as
 * the file's writer, then with the private Rust service owning the `reset-claims` row. Every reply
 * must be equal after opaque tokens are numbered by first appearance and clock readings zeroed.
 * Synthetic accounts, a fake `codex` and temporary roots only; the send is synthetic.
 * Unix only (the fake CLI and the private socket). `--ts-only` runs the native writer alone.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import express from 'express';

const tsOnly = process.argv.includes('--ts-only');
const repo = path.resolve(import.meta.dir, '../..');
const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-parity-')));
fs.chmodSync(scratch, 0o700);
const bin = path.join(scratch, 'bin');
fs.mkdirSync(bin, { mode: 0o700 });
fs.copyFileSync(
  path.join(repo, 'tests/fixtures/reset-source/refusal-codex'),
  path.join(bin, 'codex')
);
fs.chmodSync(path.join(bin, 'codex'), 0o755);
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: scratch,
  PATH: `${bin}${path.delimiter}${process.env.PATH}`,
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
assert.ok(process.env.AAC_STATUS_MODULE, 'AAC_STATUS_MODULE must name the built native module');

const { createResetClaimRouter } = await import('../../src/web-server/routes/reset-claim-routes');
const { RustService } = await import('../../src/web-server/rust-service');
const { tsWritesRow } = await import('../../src/shared/private-files');
const { loadNativeStatus } = await import('../../src/status-native/native-loader');
import type { ResetSource } from '../../src/web-server/routes/reset-claim-routes';

const ROUTE = 'codex:fixture';

interface Reply {
  status: number;
  body: unknown;
}

/** One whole claim on a fresh root. `rust` puts the service in `res.locals`, as the front door does. */
async function scenario(root: string, rust: InstanceType<typeof RustService> | null) {
  process.env.CCS_DIR = root;
  const home = path.join(root, 'profile');
  fs.mkdirSync(home, { mode: 0o700 });
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
  const state = (primary: number, secondary: number, spentA: boolean, spentB = false) =>
    fs.writeFileSync(
      path.join(home, 'fixture-state.json'),
      JSON.stringify({ primary, secondary, spentA, spentB })
    );
  const native = loadNativeStatus().module;
  assert.ok(native, 'native module');
  let sends = 0;
  const source: ResetSource = {
    // The production adapter's ticketed Codex read, through the route's claims-file writer.
    async read(account, claimsFile) {
      assert.ok(claimsFile, 'the route passes its claims-file writer to the source');
      return JSON.parse(
        await claimsFile.call(
          'resetCodexSource',
          JSON.stringify({
            home,
            expectedAccount: 'account-x',
            profile: 'fixture',
            host: 'fixture-host',
            write: null,
            dbPath: path.join(root, 'account-usage', 'reset-claims', 'claims.sqlite'),
            routeAccount: account,
          })
        )
      );
    },
    async send(write) {
      sends++;
      assert.equal(write.routeAccount, ROUTE);
      state(0, 20, false, true);
      return { outcome: 'reset' };
    },
  };
  const app = express();
  app.use(express.json());
  app.use((req: any, res, next) => {
    req.session = { authenticated: true };
    req.sessionID = 'fixture-browser';
    if (rust) res.locals.rustService = rust;
    next();
  });
  app.use('/api/accounts', createResetClaimRouter({ source }));
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const origin = `http://127.0.0.1:${(server.address() as any).port}`;
  const url = `${origin}/api/accounts/${encodeURIComponent(ROUTE)}/reset-claims`;
  const send = async (suffix: string, body?: unknown, from = origin): Promise<Reply> => {
    const response = await fetch(url + suffix, {
      method: body === undefined ? 'GET' : 'POST',
      headers: body === undefined ? {} : { origin: from, 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { status: response.status, body: await response.json() };
  };
  const steps: Record<string, Reply> = {};
  try {
    state(100, 20, false);
    steps.before = await send('');
    const grants = (steps.before.body as any).grants;
    assert.equal(grants?.length, 2, 'the fixture inventory lists both credits');
    const key = grants[1].grantKey as string;
    // Refused before the claims file: query, origin and body guards.
    steps.query = await send('?unexpected=1');
    steps.origin = await send('/prepare', { grantKey: key }, 'https://wrong.invalid');
    steps.extra = await send('/prepare', { grantKey: key, providerGrantId: 'synthetic' });
    steps.prepare = await send('/prepare', { grantKey: key });
    steps.withToken = await send('');
    steps.wrongToken = await send('', { grantKey: key, confirmationToken: 'c'.repeat(64) });
    steps.afterWrong = await send('');
    steps.prepareAgain = await send('/prepare', { grantKey: key });
    const token = (steps.prepareAgain.body as any).confirmationToken as string;
    steps.claim = await send('', { grantKey: key, confirmationToken: token });
    steps.after = await send('');
    steps.check = await send('/check', { attemptKey: (steps.claim.body as any).attemptKey });
    steps.reused = await send('', { grantKey: key, confirmationToken: token });
    steps.final = await send('');
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
  assert.equal(sends, 1, 'exactly one synthetic provider send');
  assert.equal(fs.existsSync(path.join(home, 'unexpected-send')), false);
  return steps;
}

/** Opaque 64-hex values become their order of first appearance; clock readings become 0. */
function comparable(steps: Record<string, Reply>): unknown {
  const seen = new Map<string, string>();
  const walk = (value: unknown, key = ''): unknown => {
    if (typeof value === 'string' && /^[a-f0-9]{64}$/.test(value)) {
      if (!seen.has(value)) seen.set(value, `<opaque ${seen.size}>`);
      return seen.get(value);
    }
    if (typeof value === 'number' && /(Ms|At)$/.test(key)) return 0;
    if (typeof value === 'string' && /At$/.test(key) && !Number.isNaN(Date.parse(value)))
      return '<time>';
    if (Array.isArray(value)) return value.map((item) => walk(item));
    if (value && typeof value === 'object')
      return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, walk(v, k)]));
    return value;
  };
  return walk(steps);
}

let service: InstanceType<typeof RustService> | null = null;
try {
  assert.equal(tsWritesRow('reset-claims'), true, 'TS owns the row before the service settles it');
  const tsRoot = path.join(scratch, 'ts');
  fs.mkdirSync(tsRoot, { mode: 0o700 });
  const ts = await scenario(tsRoot, null);
  assert.equal((ts.claim.body as any).outcome, 'claimed', JSON.stringify(ts.claim));
  assert.equal(ts.wrongToken.status, 409);
  assert.equal(ts.reused.status, 409);
  if (!tsOnly) {
    const rustRoot = path.join(scratch, 'rust');
    fs.mkdirSync(rustRoot, { mode: 0o700 });
    process.env.CCS_DIR = rustRoot;
    service = new RustService('on');
    assert.equal(tsWritesRow('reset-claims'), false, 'writes on: the service owns the claims file');
    await service.start(
      process.env.AAC_RUST_SERVICE_BINARY ?? path.resolve(repo, 'target/debug/aac-http')
    );
    assert.ok(service.resetClaimsFile(), 'Rust ownership must be active');
    const rust = await scenario(rustRoot, service);
    // The service holds the row's lease; every claims-file call reached it and none fell back.
    assert.ok(
      fs.existsSync(path.join(rustRoot, 'account-usage', 'reset-claims', 'claims.sqlite.lock'))
    );
    assert.ok(service.stats.served >= 20, `served ${service.stats.served}`);
    assert.equal(service.stats.fallbacks, 0);
    assert.deepEqual(comparable(rust), comparable(ts));
    // Byte order too: the route's JSON text is the same after normalization.
    assert.equal(JSON.stringify(comparable(rust)), JSON.stringify(comparable(ts)));
  }
  console.log(
    `reset claim parity: ${Object.keys(ts).length} replies over one whole claim (${tsOnly ? 'TS' : 'TS/Rust'})`
  );
} finally {
  service?.stop();
  fs.rmSync(scratch, { recursive: true, force: true });
}
