/** Offline production-router/native regression: a banked Codex credit is usable at any usage (owner, 2026-11-13).
 * Explicit test addon plus a fake Codex app-server; synthetic accounts and temporary SQLite only. The send goes
 * through the real native Codex transport to that fake, never to a provider. */
import { expect, test } from 'bun:test';
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
import { randomBytes } from 'node:crypto';
import express from 'express';
import {
  createResetClaimRouter,
  type ResetWrite,
} from '../../../src/web-server/routes/reset-claim-routes';
const require = createRequire(import.meta.url);

const nativeTest = test.skipIf(!process.env.AAC_RESET_TEST_MODULE);
const cases = [
  {
    name: 'weekly 60% used: the credit is spent and the week refills',
    five: 2,
    week: 60,
    outcome: 'reset',
    refill: { week: 0 },
    result: 'claimed',
    windows: [{ id: 'seven_day', usedPercent: 0 }],
    left: 0,
  },
  {
    name: 'weekly 18% used: the credit is spent before any limit shows a drop',
    five: 18,
    week: 18,
    outcome: 'reset',
    refill: {},
    result: 'claimed',
    windows: [],
    left: 0,
  },
  {
    name: 'nothing used: OpenAI has nothing to reset and nothing is spent',
    five: 0,
    week: 0,
    outcome: 'nothingToReset',
    refill: {},
    result: 'nothing-to-reset',
    windows: [],
    left: 1,
  },
];
/** The real router and native addon over the fake app-server, for one synthetic Codex login. */
async function harness(state: object, opts: { loseAnswer?: boolean } = {}) {
  expect(process.env.PATH?.split(delimiter)[0]).toBe(process.env.AAC_RESET_TEST_CODEX_DIR);
  expect(readFileSync(join(process.env.AAC_RESET_TEST_CODEX_DIR!, 'codex'), 'utf8')).toBe(
    readFileSync(new URL('../../fixtures/reset-source/any-usage-codex', import.meta.url), 'utf8')
  );
  const native = new (require(process.env.AAC_RESET_TEST_MODULE!).NativeStatus)();
  const root = mkdtempSync(join(tmpdir(), 'aac-any-usage-'));
  const home = join(root, 'profile'),
    route = 'codex:fixture';
  mkdirSync(home);
  const dir = join(root, 'account-usage', 'reset-claims');
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const dbPath = join(dir, 'claims.sqlite');
  const claims = Buffer.from(
    JSON.stringify({
      email: 'fixture-account-x',
      'https://api.openai.com/auth': { chatgpt_account_id: 'account-x' },
    })
  ).toString('base64url');
  writeFileSync(
    join(home, 'auth.json'),
    JSON.stringify({ tokens: { account_id: 'account-x', id_token: `e30.${claims}.fixture` } })
  );
  const setState = (next: object) =>
    writeFileSync(join(home, 'fixture-state.json'), JSON.stringify(next));
  setState(state);
  const source = (write: ResetWrite | null, context: object) =>
    native.call(
      'resetCodexSource',
      JSON.stringify({
        home,
        expectedAccount: 'account-x',
        profile: 'fixture',
        host: 'fixture-host',
        write,
        ...context,
      })
    );
  const sent = () =>
    existsSync(join(home, 'sent.jsonl'))
      ? readFileSync(join(home, 'sent.jsonl'), 'utf8').trim().split('\n').map(JSON.parse)
      : [];
  const app = express();
  app.use(express.json());
  app.use((req: any, _res, next) => {
    req.session = { authenticated: true };
    req.sessionID = 'fixture-browser';
    next();
  });
  app.use(
    '/api/accounts',
    createResetClaimRouter({
      native,
      ccsDir: root,
      source: {
        read: async () => JSON.parse(await source(null, { dbPath, routeAccount: route })),
        send: async (write) => {
          const answer = JSON.parse(await source(write, {}));
          // The consume reached the fake, but its answer never comes back: the router sees a lost send.
          if (opts.loseAnswer) throw new Error('synthetic lost answer');
          return answer;
        },
      },
    })
  );
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const origin = `http://127.0.0.1:${(server.address() as any).port}`;
  const url = `${origin}/api/accounts/${encodeURIComponent(route)}/reset-claims`;
  const get = async () => {
    const response = await fetch(url);
    expect(response.status).toBe(200);
    return (await response.json()) as any;
  };
  const post = async (suffix: string, body: any) => {
    const response = await fetch(url + suffix, {
      method: 'POST',
      headers: { origin, 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    return { status: response.status, body: (await response.json()) as any };
  };
  const close = async () => {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    rmSync(root, { recursive: true, force: true });
  };
  return { home, get, post, sent, setState, close };
}

for (const c of cases)
  nativeTest(
    c.name,
    async () => {
      const h = await harness({
        five: c.five,
        week: c.week,
        spent: false,
        outcome: c.outcome,
        refill: c.refill,
      });
      try {
        const before = await h.get();
        expect(before.grants).toHaveLength(1);
        expect(before.grants[0].kind).toBe('banked');
        expect(before.grants[0].claimable).toBe(true);
        expect(before.grants[0].claimBlockedReason).toBeNull();
        const key = before.grants[0].grantKey;
        const prep = await h.post('/prepare', { grantKey: key });
        expect(prep.status).toBe(200);
        // What is used now comes from prepare's own fresh read (display only).
        expect(prep.body.usage).toEqual([
          { id: 'five_hour', usedPercent: c.five },
          { id: 'seven_day', usedPercent: c.week },
        ]);
        expect(h.sent()).toEqual([]);
        // A busy account: usage moves while the dialog is open. That never refuses the claim.
        h.setState({
          five: c.five + 3,
          week: c.week + 2,
          spent: false,
          outcome: c.outcome,
          refill: c.refill,
        });
        const claim = await h.post('', {
          grantKey: key,
          confirmationToken: prep.body.confirmationToken,
        });
        expect(claim.status).toBe(200);
        expect(claim.body.outcome).toBe(c.result);
        expect(claim.body.pending).toBe(false);
        expect(claim.body.verification).toBe(c.result === 'claimed' ? 'confirmed' : 'unconfirmed');
        expect(claim.body.windows).toEqual(c.windows);
        // The result names the usage the claim was sent at, not what the dialog showed.
        expect(claim.body.sentUsage).toEqual([
          { id: 'five_hour', usedPercent: c.five + 3 },
          { id: 'seven_day', usedPercent: c.week + 2 },
        ]);
        // Exactly one consume, for the exact chosen credit and this attempt's key.
        expect(h.sent()).toEqual([{ creditId: 'credit-a', idempotencyKey: claim.body.attemptKey }]);
        const after = await h.get();
        expect(after.pendingAttempt).toBeNull();
        expect(after.grants[0].resetsLeft).toBe(c.left);
        expect(after.grants[0].claimable).toBe(c.left === 1);
        const verification = after.history.filter((e: any) => e.event === 'verification');
        expect(verification.map((e: any) => e.outcome)).toEqual([c.result]);
        expect(verification[0].details.beforeCount).toBe(1);
        expect(verification[0].details.afterCount).toBe(c.left);
        expect(JSON.stringify(after)).not.toContain('credit-a');
        expect(existsSync(join(h.home, 'unexpected-send'))).toBe(false);
      } finally {
        await h.close();
      }
    },
    20000
  );

nativeTest(
  'a lost answer while OpenAI is still redeeming stays unconfirmed and locked until a read says redeemed',
  async () => {
    // The consume reaches OpenAI, which starts redeeming; its answer is lost on the way back.
    const h = await harness(
      {
        five: 2,
        week: 60,
        spent: false,
        outcome: 'reset',
        refill: { status: 'redeeming' },
      },
      { loseAnswer: true }
    );
    try {
      const key = (await h.get()).grants[0].grantKey;
      const prep = await h.post('/prepare', { grantKey: key });
      expect(prep.status).toBe(200);
      const claim = await h.post('', {
        grantKey: key,
        confirmationToken: prep.body.confirmationToken,
      });
      expect(claim.status).toBe(200);
      expect(claim.body.outcome).toBe('claimed-unconfirmed');
      expect(claim.body.verification).toBe('unconfirmed');
      expect(claim.body.pending).toBe(true);
      expect(claim.body.providerSuccess).toBe(false);
      // Locked: the account names the unresolved attempt, and the redeeming credit can't be used.
      const locked = await h.get();
      expect(locked.pendingAttempt).toBe(claim.body.attemptKey);
      expect(locked.grants[0].resetsLeft).toBe(1);
      expect(locked.grants[0].claimable).toBe(false);
      expect(locked.grants[0].claimBlockedReason).toBe('grant-unavailable');
      const again = await h.post('/prepare', { grantKey: locked.grants[0].grantKey });
      expect(again.status).toBe(409);
      expect(again.body.sent).toBe(false);
      // Check again while still redeeming: unchanged, and nothing is sent.
      const still = await h.post('/check', { attemptKey: claim.body.attemptKey });
      expect(still.body.outcome).toBe('claimed-unconfirmed');
      expect(still.body.pending).toBe(true);
      expect(h.sent()).toHaveLength(1);
      // A later read says redeemed: confirmed and unlocked.
      h.setState({ five: 2, week: 0, spent: true, outcome: 'reset' });
      const done = await h.post('/check', { attemptKey: claim.body.attemptKey });
      expect(done.body.outcome).toBe('claimed');
      expect(done.body.verification).toBe('confirmed');
      expect(done.body.pending).toBe(false);
      expect(done.body.windows).toEqual([{ id: 'seven_day', usedPercent: 0 }]);
      expect((await h.get()).pendingAttempt).toBeNull();
      expect(h.sent()).toEqual([{ creditId: 'credit-a', idempotencyKey: claim.body.attemptKey }]);
      expect(existsSync(join(h.home, 'unexpected-send'))).toBe(false);
    } finally {
      await h.close();
    }
  },
  20000
);
