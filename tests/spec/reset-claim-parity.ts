/**
 * K's whole-claim fixture against the historical Express route and the worker-free
 * public listener. Compare every response after normalizing opaque tokens and clocks.
 * Unix only because the fake Codex app-server uses a shebang. Use --queued-verification
 * to exercise a 4.5-second source queue followed by a 19.8-second provider read.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import express from 'express';
import { startResetFixture } from './reset-http-fixture';

const tsOnly = process.argv.includes('--ts-only');
const queuedVerification = process.argv.includes('--queued-verification');
const repo = path.resolve(import.meta.dir, '../..');
const fixtureCodex = path.join(repo, 'tests/fixtures/reset-source/refusal-codex');
// The native source and the service find `codex` on the process environment's PATH, which a
// `process.env` assignment under Bun does not reach. The parent makes the fake CLI's folder and
// starts this script again with that folder first on PATH from process start; the child refuses
// to run unless the first PATH entry holds exactly the fixture, so a real CLI is never reached.
if (!process.env.AAC_RESET_PARITY_BIN) {
  const parent = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-parity-bin-')));
  fs.chmodSync(parent, 0o700);
  fs.copyFileSync(fixtureCodex, path.join(parent, 'codex'));
  fs.chmodSync(path.join(parent, 'codex'), 0o755);
  const { spawnSync } = await import('node:child_process');
  const child = spawnSync(process.execPath, [import.meta.path, ...process.argv.slice(2)], {
    stdio: 'inherit',
    env: {
      ...process.env,
      PATH: `${parent}${path.delimiter}${process.env.PATH}`,
      AAC_RESET_PARITY_BIN: parent,
    },
  });
  fs.rmSync(parent, { recursive: true, force: true });
  process.exit(child.status ?? 1);
}
const bin = process.env.AAC_RESET_PARITY_BIN;
assert.equal(process.env.PATH?.split(path.delimiter)[0], bin, 'the fake CLI comes first on PATH');
assert.deepEqual(fs.readFileSync(path.join(bin, 'codex')), fs.readFileSync(fixtureCodex));
const scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'aac-reset-parity-')));
fs.chmodSync(scratch, 0o700);
for (const key of Object.keys(process.env)) if (key.startsWith('CCS_')) delete process.env[key];
Object.assign(process.env, {
  CCS_HOME: scratch,
  AAC_RUST_GROUP_WRITES: 'on',
  AAC_STORE: 'off',
  AAC_AGENT_TRANSPORT: 'off',
  AAC_NO_LOCAL_RUST_BUILD: '1',
});
assert.ok(process.env.AAC_STATUS_MODULE, 'AAC_STATUS_MODULE must name the built native module');

const { createResetClaimRouter } = await import('../fixtures/reset-adapter/reset-claim-routes');
const { localUsageHost } = await import('../../src/web-server/services/usage-failover-transport');
const { loadNativeStatus } = await import('../../src/status-native/native-loader');
import type { ResetSource } from '../fixtures/reset-adapter/reset-claim-routes';

const ROUTE = 'codex:fixture';

interface Reply {
  status: number;
  body: unknown;
}

async function startReference(source: ResetSource) {
  const app = express();
  app.use(express.json());
  app.use((req: any, _res, next) => {
    req.session = { authenticated: true };
    req.sessionID = 'fixture-browser';
    next();
  });
  app.use('/api/accounts', createResetClaimRouter({ source }));
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

/** One whole claim on a fresh root. */
async function scenario(root: string, rust: boolean) {
  process.env.CCS_DIR = root;
  const home = path.join(root, 'codex-instances', 'fixture');
  fs.mkdirSync(home, { mode: 0o700, recursive: true });
  fs.writeFileSync(
    path.join(root, 'codex-profiles.yaml'),
    "version: '1'\ndefault: fixture\nprofiles:\n  fixture:\n    type: codex\n    created: '2026-11-01T00:00:00Z'\n    last_used: null\n"
  );
  if (rust) fs.writeFileSync(path.join(home, 'allow-fixture-send'), '1');
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
      // As the production adapter does before a ticketed read.
      const dir = path.join(root, 'account-usage', 'reset-claims');
      fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
      return JSON.parse(
        await claimsFile.call(
          'resetCodexSource',
          JSON.stringify({
            home,
            expectedAccount: 'account-x',
            profile: 'fixture',
            host: localUsageHost().host,
            write: null,
            dbPath: path.join(dir, 'claims.sqlite'),
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
  const fixture = rust ? await startResetFixture(repo, root) : await startReference(source);
  const origin = fixture.url;
  const url = `${origin}/api/accounts/${encodeURIComponent(ROUTE)}/reset-claims`;
  const send = async (suffix: string, body?: unknown, from = origin): Promise<Reply> => {
    const response = await fetch(url + suffix, {
      method: body === undefined ? 'GET' : 'POST',
      headers:
        body === undefined
          ? { cookie: fixture.cookie }
          : { cookie: fixture.cookie, origin: from, 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(response.headers.get('content-type'), 'application/json; charset=utf-8');
    const text = await response.text();
    const parsed = JSON.parse(text);
    assert.equal(text, JSON.stringify(parsed), 'exact Express JSON encoding');
    return { status: response.status, body: parsed };
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
    // A wrong token leaves the outstanding one in place: a second prepare is refused.
    const token = (steps.prepare.body as any).confirmationToken as string;
    steps.claim = await send('', { grantKey: key, confirmationToken: token });
    steps.after = await send('');
    steps.check = await send('/check', { attemptKey: (steps.claim.body as any).attemptKey });
    steps.reused = await send('', { grantKey: key, confirmationToken: token });
    steps.final = await send('');
  } finally {
    await fixture.stop();
  }
  if (rust)
    sends = fs.readFileSync(path.join(home, 'fixture-sends'), 'utf8').trim().split('\n').length;
  assert.equal(sends, 1, 'exactly one synthetic provider send');
  assert.equal(fs.existsSync(path.join(home, 'unexpected-send')), false);
  return steps;
}

/** Hold the four source permits as the claim moves from send to verification. */
async function queuedClaim(root: string) {
  fs.mkdirSync(root, { mode: 0o700 });
  const barrier = path.join(root, 'queue-fixture');
  fs.mkdirSync(barrier, { mode: 0o700 });
  const holders = Array.from({ length: 4 }, (_, index) => `holder-${index}`);
  const profiles = ['fixture', ...holders];
  const state = { primary: 100, secondary: 20, spentA: false };
  fs.writeFileSync(
    path.join(root, 'codex-profiles.yaml'),
    "version: '1'\nprofiles:\n" +
      profiles
        .map((profile) => `  ${profile}: {type: codex, created: '2026-11-01', last_used: null}\n`)
        .join('')
  );
  for (const profile of profiles) {
    const home = path.join(root, 'codex-instances', profile);
    fs.mkdirSync(home, { recursive: true, mode: 0o700 });
    const claims = Buffer.from(
      JSON.stringify({
        email: `fixture-${profile}`,
        'https://api.openai.com/auth': { chatgpt_account_id: profile },
      })
    ).toString('base64url');
    fs.writeFileSync(
      path.join(home, 'auth.json'),
      JSON.stringify({
        tokens: { account_id: profile, id_token: `e30.${claims}.fixture` },
      })
    );
    fs.writeFileSync(path.join(home, 'fixture-state.json'), JSON.stringify(state));
    if (profile === 'fixture') fs.writeFileSync(path.join(home, 'allow-fixture-send'), '1');
  }
  const waitFor = async (name: string) => {
    const marker = path.join(barrier, name);
    const deadline = Date.now() + 5_000;
    while (!fs.existsSync(marker) || fs.statSync(marker).size === 0) {
      assert.ok(Date.now() < deadline, `fixture did not reach ${name}`);
      await Bun.sleep(10);
    }
    return Number(fs.readFileSync(marker, 'utf8'));
  };
  const fixture = await startResetFixture(repo, root);
  const send = async (account: string, suffix = '', body?: unknown) => {
    const response = await fetch(
      `${fixture.url}/api/accounts/${encodeURIComponent(account)}/reset-claims${suffix}`,
      {
        method: body === undefined ? 'GET' : 'POST',
        headers: {
          cookie: fixture.cookie,
          origin: fixture.url,
          'content-type': 'application/json',
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(120_000),
      }
    );
    const result = await response.json();
    assert.equal(response.status, 200, JSON.stringify(result));
    return result;
  };
  const pending: Promise<unknown>[] = [];
  try {
    const inventory = await send(ROUTE);
    assert.equal(inventory.status, 'ok', JSON.stringify(inventory));
    assert.equal(inventory.grants.length, 2);
    const grantKey = inventory.grants[1].grantKey;
    const { confirmationToken } = await send(ROUTE, '/prepare', { grantKey });
    const claim = send(ROUTE, '', { grantKey, confirmationToken });
    pending.push(claim);
    await waitFor('send-waiting');
    for (const profile of holders.slice(0, 3)) pending.push(send(`codex:${profile}`));
    await Promise.all(holders.slice(0, 3).map(waitFor));
    pending.push(send(`codex:${holders[3]}`));
    // The fourth reader queues behind the sender. When the send releases its permit,
    // the fourth reader enters first and the verification has to wait.
    await Bun.sleep(500);
    fs.writeFileSync(path.join(barrier, 'release-send'), '1');
    const sendEnd = await waitFor('send-end');
    await waitFor(holders[3]);
    fs.writeFileSync(path.join(barrier, 'release-holders'), String(sendEnd + 4.5));
    const receipt = await claim;
    await Promise.all(pending);
    assert.equal(receipt.outcome, 'claimed', JSON.stringify(receipt));
    assert.equal(receipt.providerSuccess, true);
    const queue = (await waitFor('verify-start')) - sendEnd;
    const read = (await waitFor('verify-end')) - (await waitFor('verify-start'));
    assert.ok(queue >= 4.45 && queue < 5, `verification queued for ${queue}s`);
    assert.ok(read >= 19.8 && read < 20, `verification read took ${read}s`);
    const sends = fs.readFileSync(path.join(root, 'codex-instances/fixture/fixture-sends'), 'utf8');
    assert.equal(sends, 'sent\n');
    console.log(
      `Reset queued verification confirmed after ${queue.toFixed(2)}s queue and ${read.toFixed(2)}s provider read, one send.`
    );
  } finally {
    await fixture.stop();
    await Promise.allSettled(pending);
  }
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

try {
  const tsRoot = path.join(scratch, 'ts');
  fs.mkdirSync(tsRoot, { mode: 0o700 });
  const ts = await scenario(tsRoot, false);
  assert.equal((ts.claim.body as any).outcome, 'claimed', JSON.stringify(ts.claim));
  assert.equal(ts.wrongToken.status, 409);
  assert.equal((ts.prepareAgain.body as any).reason, 'claim-token-outstanding');
  assert.equal(ts.check.status, 200, JSON.stringify(ts.check));
  assert.equal(ts.reused.status, 409);
  if (!tsOnly) {
    const rustRoot = path.join(scratch, 'rust');
    fs.mkdirSync(rustRoot, { mode: 0o700 });
    const rust = await scenario(rustRoot, true);
    assert.ok(
      fs.existsSync(path.join(rustRoot, 'account-usage', 'reset-claims', 'claims.sqlite.lock'))
    );
    assert.deepEqual(comparable(rust), comparable(ts));
    // Byte order too: the route's JSON text is the same after normalization.
    assert.equal(JSON.stringify(comparable(rust)), JSON.stringify(comparable(ts)));
  }
  if (queuedVerification) {
    assert.equal(tsOnly, false, 'queued verification requires the Rust listener');
    await queuedClaim(path.join(scratch, 'queued'));
  }
  console.log(
    `reset claim parity: ${Object.keys(ts).length} replies over one whole claim (${tsOnly ? 'TS' : 'TS/Rust'})`
  );
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
