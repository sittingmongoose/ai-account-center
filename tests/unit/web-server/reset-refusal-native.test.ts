/** Offline production-router/native regression; synthetic accounts and temporary SQLite only. */
import { expect, test } from 'bun:test';
import { createRequire } from 'node:module';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, delimiter } from 'node:path';
import { randomBytes } from 'node:crypto';
import express from 'express';
import { createResetClaimRouter } from '../../../src/web-server/routes/reset-claim-routes';
const require = createRequire(import.meta.url);

const nativeTest = test.skipIf(!process.env.AAC_RESET_TEST_MODULE);
for (const priorSuccess of [true, false])
  nativeTest(
    `refusal preserves the action answer: prior success=${priorSuccess}`,
    async () => {
      expect(process.env.PATH?.split(delimiter)[0]).toBe(process.env.AAC_RESET_TEST_CODEX_DIR);
      expect(readFileSync(join(process.env.AAC_RESET_TEST_CODEX_DIR!, 'codex'), 'utf8')).toBe(
        readFileSync(new URL('../../fixtures/reset-source/refusal-codex', import.meta.url), 'utf8')
      );
      const native = new (require(process.env.AAC_RESET_TEST_MODULE!).NativeStatus)();
      const root = mkdtempSync(join(tmpdir(), 'aac-refusal-'));
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
      const state = (primary: number, secondary: number, spentA: boolean, spentB = false) =>
        writeFileSync(
          join(home, 'fixture-state.json'),
          JSON.stringify({ primary, secondary, spentA, spentB })
        );
      const call = async (op: string, args: any = {}) =>
        JSON.parse(
          await native.call(
            'resetClaims',
            JSON.stringify({
              dbPath,
              account: route,
              op,
              nowMs: Date.now(),
              entropy: randomBytes(32).toString('hex'),
              ...args,
            })
          )
        );
      const actualRead = async () =>
        JSON.parse(
          await native.call(
            'resetCodexSource',
            JSON.stringify({
              dbPath,
              routeAccount: route,
              home,
              expectedAccount: 'account-x',
              profile: 'fixture',
              host: 'fixture-host',
              write: null,
            })
          )
        );
      let sends = 0;
      let allowRetry = false;
      let duringRetry: unknown;
      let consumedWhilePending: unknown;
      let oldGrant = '',
        oldToken = '';
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
            read: actualRead,
            send: async () => {
              sends++;
              if (!allowRetry) throw new Error('Unexpected synthetic send');
              duringRetry = await get();
              consumedWhilePending = (
                await post('', { grantKey: oldGrant, confirmationToken: oldToken })
              ).body;
              state(0, 21, priorSuccess, true);
              return { outcome: 'reset' };
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
      try {
        let first: any;
        if (priorSuccess) {
          state(100, 20, false);
          const snapshot = await actualRead();
          const inv = await call('inventory', { snapshot });
          const firstKey = inv.grants[0].grantKey;
          const firstPrep = await call('prepare', { key: firstKey, caller: 'local', snapshot });
          first = await call('begin', {
            key: firstKey,
            caller: 'local',
            token: firstPrep.confirmationToken,
            snapshot: await actualRead(),
          });
          state(0, 20, true);
          const settled = await call('finish', {
            key: first.attemptKey,
            outcome: 'reset',
            snapshot: await actualRead(),
          });
          expect(settled.outcome).toBe('claimed');
          expect(settled.windows[0].usedPercent).toBe(0);
        }
        // Normal later usage exhausts primary again; B remains individually available.
        state(100, 20, priorSuccess);
        const before = await get();
        const key = before.grants[1].grantKey;
        expect(before.grants[1].claimable).toBe(true);
        const prep = await post('/prepare', { grantKey: key });
        expect(prep.status).toBe(200);
        const withToken = await get();
        expect(withToken.evidence.claimTokens.tokenKeys).toContain(prep.body.confirmationKey);
        // Another ordinary usage increment between confirmation and execute changes the bound state.
        state(100, 21, priorSuccess);
        const refused = await post('', {
          grantKey: key,
          confirmationToken: prep.body.confirmationToken,
        });
        expect(refused.status).toBe(409);
        expect(refused.body.reason).toBe('grant-changed');
        expect(refused.body.sent).toBe(false);
        const recovered = await get();
        if (priorSuccess) {
          expect(recovered.evidence.latestAttempt.attemptKey).toBe(first.attemptKey);
          expect(recovered.evidence.latestAttempt.outcome).toBe('claimed');
        } else expect(recovered.evidence.latestAttempt).toBeNull();
        expect(recovered.pendingAttempt).toBeNull();
        expect(recovered.evidence.claimTokens.tokenKeys).toEqual([]);
        expect(recovered.history.filter((e: any) => e.event === 'attempt').length).toBe(
          priorSuccess ? 1 : 0
        );
        const fresh = await actualRead();
        expect(fresh.windows.primary).toBe(100);
        expect(fresh.windows.secondary).toBe(21);
        expect(recovered.grants[1].resetsLeft).toBe(1);
        expect(sends).toBe(0);
        expect(existsSync(join(home, 'unexpected-send'))).toBe(false);
        const refusedSendCount = sends;
        oldGrant = key;
        oldToken = prep.body.confirmationToken;
        allowRetry = true;
        const retryPrep = await post('/prepare', { grantKey: recovered.grants[1].grantKey });
        expect(retryPrep.status).toBe(200);
        const retry = await post('', {
          grantKey: recovered.grants[1].grantKey,
          confirmationToken: retryPrep.body.confirmationToken,
        });
        expect(retry.status).toBe(200);
        expect(retry.body.outcome).toBe('claimed');
        expect(retry.body.attemptKey).not.toBe(first?.attemptKey);
        const afterRetry = await get();
        const consumedAfterSuccess = (
          await post('', { grantKey: key, confirmationToken: prep.body.confirmationToken })
        ).body;
        expect(afterRetry.evidence.latestAttempt.attemptKey).toBe(retry.body.attemptKey);
        expect((consumedWhilePending as any).sent).toBe(false);
        expect((consumedWhilePending as any).reason).toBe('check-again-required');
        expect((consumedWhilePending as any).evidence.latestAttempt.pending).toBe(true);
        expect(consumedAfterSuccess.sent).toBe(false);
        expect(consumedAfterSuccess.reason).toBe('confirmation-invalid');
        expect(consumedAfterSuccess.evidence.latestAttempt.attemptKey).toBe(retry.body.attemptKey);
        expect(consumedAfterSuccess.evidence.latestAttempt.outcome).toBe('claimed');
        expect(sends).toBe(1);
        expect(existsSync(join(home, 'unexpected-send'))).toBe(false);
        if (process.env.AAC_RESET_CAPTURE_TRACES)
          writeFileSync(
            join(
              process.env.AAC_RESET_CAPTURE_TRACES,
              `reset-refusal-${priorSuccess ? 'after-success' : 'first-action'}.json`
            ),
            JSON.stringify(
              {
                before,
                prepared: prep.body,
                withToken,
                refused: refused.body,
                recovered,
                actualWindows: fresh.windows,
                providerSends: refusedSendCount,
                retryPrepared: retryPrep.body,
                retry: retry.body,
                afterRetry,
                syntheticRetrySends: sends,
                duringRetry,
                consumedWhilePending,
                consumedAfterSuccess,
              },
              null,
              2
            ) + '\n'
          );
      } finally {
        server.closeAllConnections();
        await new Promise<void>((resolve) => server.close(() => resolve()));
        rmSync(root, { recursive: true, force: true });
      }
    },
    20000
  );
