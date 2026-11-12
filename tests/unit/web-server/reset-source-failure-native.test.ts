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
for (const changed of [true, false]) {
  nativeTest(
    `captured identity survives quota failure: replacement=${changed}`,
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

      let sends = 0;
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
            read: () => actualRead(),
            send: async () => {
              sends++;
              throw new Error('No provider sends');
            },
          },
        })
      );
      const server = app.listen(0, '127.0.0.1');
      await new Promise<void>((resolve) => server.once('listening', resolve));
      const origin = `http://127.0.0.1:${(server.address() as any).port}`;
      const get = async () => {
        const r = await fetch(`${origin}/api/accounts/${encodeURIComponent(route)}/reset-claims`);
        return { status: r.status, body: (await r.json()) as any };
      };
      try {
        login('account-x');
        const xAttempt = await seed(route, 'reused', true);
        const prior = await get();
        expect(prior.body.evidence.latestAttempt.outcome).toBe('claimed');
        let yAttempt: string | undefined;
        if (changed) {
          login('account-y');
          yAttempt = await seed(sibling, 'y-sibling');
        }
        writeFileSync(join(home, 'fail-usage'), '');
        const unavailable = await get();
        expect(unavailable.status).toBe(200);
        expect(unavailable.body.status).toBe('unavailable');
        const saved = JSON.parse(readFileSync(join(home, 'auth.json'), 'utf8')).tokens.account_id;
        const siblingPending = await call('pending', sibling);
        expect(saved).toBe(changed ? 'account-y' : 'account-x');
        if (changed) expect(siblingPending.pendingAttempt).toBe(yAttempt);
        expect(readFileSync(join(home, 'fixture-reader-started'), 'utf8')).toContain(saved);
        expect(readFileSync(join(home, 'fixture-usage-failed'), 'utf8')).toBe(saved);
        if (changed) {
          expect(unavailable.body.canonicalAccountKey).not.toBe(prior.body.canonicalAccountKey);
          expect(unavailable.body.canonicalAccountKey).toBe(siblingPending.canonicalAccountKey);
          expect(unavailable.body.pendingAttempt).toBe(yAttempt);
          expect(unavailable.body.evidence.latestAttempt.attemptKey).toBe(yAttempt);
          expect(unavailable.body.history.some((event: any) => event.outcome === 'claimed')).toBe(
            false
          );
          expect(unavailable.body.reason).toBe('account-changed-usage-unavailable');
        } else {
          expect(unavailable.body.canonicalAccountKey).toBe(prior.body.canonicalAccountKey);
          expect(unavailable.body.evidence.latestAttempt.attemptKey).toBe(xAttempt);
          expect(unavailable.body.evidence.latestAttempt.outcome).toBe('claimed');
          expect(unavailable.body.pendingAttempt).toBeNull();
          expect(unavailable.body.reason).not.toBe('account-changed-usage-unavailable');
        }
        rmSync(join(home, 'fail-usage'));
        const recovered = await get();
        if (changed) expect(recovered.body.pendingAttempt).toBe(yAttempt);
        else expect(recovered.body.evidence.latestAttempt.attemptKey).toBe(xAttempt);
        expect(sends).toBe(0);
        expect(() => readFileSync(join(home, 'unexpected-send'))).toThrow();
        const trace = {
          prior: prior.body,
          unavailable: unavailable.body,
          recovered: recovered.body,
          savedAccount: saved,
          siblingPending,
          providerSends: sends,
        };
        if (process.env.AAC_RESET_CAPTURE_TRACES)
          writeFileSync(
            join(
              process.env.AAC_RESET_CAPTURE_TRACES,
              `reset-source-failure-${changed ? 'replacement' : 'same'}.json`
            ),
            JSON.stringify(trace, null, 2) + '\n'
          );
      } finally {
        server.closeAllConnections();
        await new Promise<void>((resolve) => server.close(() => resolve()));
        rmSync(root, { recursive: true, force: true });
      }
    },
    20000
  );
}
