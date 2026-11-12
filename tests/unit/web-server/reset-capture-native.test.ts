/** Offline loopback regression: explicit test addon plus a fake Codex process, never provider I/O. */
import { expect, test } from 'bun:test';
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, renameSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
import { randomBytes } from 'node:crypto';
import express from 'express';
import { createResetClaimRouter } from '../../../src/web-server/routes/reset-claim-routes';
const require = createRequire(import.meta.url);
const nativeTest = test.skipIf(!process.env.AAC_RESET_TEST_MODULE);
const deferred = () => {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
for (const newerFirst of [false, true]) {
  nativeTest(
    `actual capture orders HTTP source responses: newer delivered ${newerFirst ? 'first' : 'last'}`,
    async () => {
      const bin = process.env.AAC_RESET_TEST_CODEX_DIR;
      // Bun's JS environment mutations do not reliably update native child-process lookup.
      // Require an explicit offline executable on the process's initial PATH before any native call.
      expect(bin).toBeTruthy();
      expect(process.env.PATH?.split(delimiter)[0]).toBe(bin);
      expect(readFileSync(join(bin as string, 'codex'), 'utf8')).toBe(
        readFileSync(new URL('../../fixtures/reset-source/codex', import.meta.url), 'utf8')
      );
      const native = new (require(process.env.AAC_RESET_TEST_MODULE as string).NativeStatus)();
      const root = mkdtempSync(join(tmpdir(), 'aac-capture-http-'));
      const home = join(root, 'profile');
      mkdirSync(home);
      const dir = join(root, 'account-usage', 'reset-claims');
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      const dbPath = join(dir, 'claims.sqlite'),
        route = 'codex:reused',
        sibling = 'codex:y-sibling';
      const nonce = () => randomBytes(32).toString('hex');
      async function call(op: string, account: string, args = {}) {
        return JSON.parse(
          await native.call(
            'resetClaims',
            JSON.stringify({ dbPath, op, account, nowMs: Date.now(), entropy: nonce(), ...args })
          )
        );
      }
      function login(who: string) {
        const claims = Buffer.from(
          JSON.stringify({
            email: 'fixture-' + who,
            'https://api.openai.com/auth': { chatgpt_account_id: who },
          })
        ).toString('base64url');
        writeFileSync(
          join(home, 'next'),
          JSON.stringify({ tokens: { account_id: who, id_token: `e30.${claims}.fixture` } })
        );
        renameSync(join(home, 'next'), join(home, 'auth.json'));
      }
      async function actualRead(account = route, profile = 'reused') {
        const expectedAccount = JSON.parse(readFileSync(join(home, 'auth.json'), 'utf8')).tokens
          .account_id;
        return JSON.parse(
          await native.call(
            'resetCodexSource',
            JSON.stringify({
              dbPath,
              routeAccount: account,
              home,
              expectedAccount,
              profile,
              host: 'fixture-host',
              write: null,
            })
          )
        );
      }
      async function seed(account: string, profile: string, settle = false) {
        const snapshot = await actualRead(account, profile);
        const inv = await call('inventory', account, { snapshot });
        const key = inv.grants[0].grantKey;
        const prepared = await call('prepare', account, { key, caller: 'local', snapshot });
        const write = await call('begin', account, {
          key,
          caller: 'local',
          token: prepared.confirmationToken,
          snapshot: await actualRead(account, profile),
        });
        if (settle) {
          writeFileSync(join(home, 'spent-x'), '');
          expect(
            (
              await call('finish', account, {
                key: write.attemptKey,
                outcome: 'reset',
                snapshot: await actualRead(account, profile),
              })
            ).outcome
          ).toBe('claimed');
        }
        return write.attemptKey;
      }
      const aEntered = deferred(),
        aGo = deferred(),
        aCaptured = deferred(),
        aDeliver = deferred(),
        bCaptured = deferred(),
        bDeliver = deferred();
      let reads = 0,
        sends = 0;
      const tickets: Record<number, any> = {};
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
            async read() {
              const ordinal = ++reads;
              if (ordinal === 1) {
                aEntered.resolve();
                await aGo.promise;
              }
              const result = await actualRead();
              tickets[ordinal] = result.readTicket;
              if (ordinal === 1) {
                aCaptured.resolve();
                await aDeliver.promise;
              }
              if (ordinal === 2) {
                bCaptured.resolve();
                await bDeliver.promise;
              }
              return result;
            },
            async send() {
              sends++;
              throw new Error('No sends in capture-order regression');
            },
          },
        })
      );
      const server = app.listen(0, '127.0.0.1');
      await new Promise<void>((resolve) => server.once('listening', resolve));
      const origin = `http://127.0.0.1:${(server.address() as any).port}`;
      async function get() {
        const response = await fetch(
          `${origin}/api/accounts/${encodeURIComponent(route)}/reset-claims`
        );
        return { status: response.status, body: (await response.json()) as any };
      }
      try {
        login('account-x');
        await seed(route, 'reused', true);
        login('account-y');
        const yAttempt = await seed(sibling, 'y-sibling');
        login('account-x');
        const a = get();
        await aEntered.promise;
        const b = get();
        await bCaptured.promise;
        login('account-y');
        aGo.resolve();
        await aCaptured.promise;
        expect(BigInt(tickets[1].sequence)).toBeGreaterThan(BigInt(tickets[2].sequence));
        expect(tickets[1].captureVersion).toBe(1);
        expect(tickets[2].captureVersion).toBe(1);
        let current, older;
        if (newerFirst) {
          aDeliver.resolve();
          current = await a;
          bDeliver.resolve();
          older = await b;
          expect(older.status).toBe(503);
          expect(older.body.status).toBe('unavailable');
        } else {
          bDeliver.resolve();
          older = await b;
          expect(older.status).toBe(503);
          aDeliver.resolve();
          current = await a;
        }
        expect(current.status).toBe(200);
        expect(current.body.pendingAttempt).toBe(yAttempt);
        if (process.env.AAC_RESET_CAPTURE_TRACES) {
          writeFileSync(
            join(
              process.env.AAC_RESET_CAPTURE_TRACES,
              `reset-capture-${newerFirst ? 'newer-first' : 'older-first'}.json`
            ),
            JSON.stringify(
              {
                newer: current.body,
                older: older.body,
                savedAccount: 'account-y',
                providerSends: sends,
              },
              null,
              2
            ) + '\n'
          );
        }
        const recovery = await get();
        expect(recovery.status).toBe(200);
        expect(recovery.body.canonicalAccountKey).toBe(current.body.canonicalAccountKey);
        expect(recovery.body.pendingAttempt).toBe(yAttempt);
        expect((await call('pending', sibling)).pendingAttempt).toBe(yAttempt);
        expect(JSON.parse(readFileSync(join(home, 'auth.json'), 'utf8')).tokens.account_id).toBe(
          'account-y'
        );
        expect(readFileSync(join(home, 'fixture-reader-started'), 'utf8')).toContain('account-y');
        expect(sends).toBe(0);
        expect(readFileSync(join(home, 'auth.json'), 'utf8')).not.toContain('account-x');
        expect(() => readFileSync(join(home, 'unexpected-send'))).toThrow();
      } finally {
        aGo.resolve();
        aDeliver.resolve();
        bDeliver.resolve();
        server.closeAllConnections();
        await new Promise<void>((resolve) => server.close(() => resolve()));
        rmSync(root, { recursive: true, force: true });
      }
    },
    20_000
  );
}
