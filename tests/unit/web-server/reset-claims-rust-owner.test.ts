/** M5 lane K: one writer per claims file. While the registry assigns the `reset-claims` row to the
 * Rust service, an unavailable service fails closed: the route never opens the file in process,
 * never reads the source and never sends. With the native module present (AAC_STATUS_MODULE), an
 * in-process fallback would answer GET with 200; the route must answer its unavailable replies. */
import { afterAll, afterEach, expect, test } from 'bun:test';
import express from 'express';
import { mkdtempSync, rmSync, existsSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import type { Server } from 'http';
import { settlePrivateFileOwners, tsWritesRow } from '../../../src/shared/private-files';
import { createResetClaimRouter } from '../../../src/web-server/routes/reset-claim-routes';
import { rustResetClaimsFile } from '../../../src/web-server/rust-service';

const servers: Server[] = [];
const dirs: string[] = [];
afterEach(async () => {
  settlePrivateFileOwners(() => false);
  for (const server of servers.splice(0)) await new Promise<void>((r) => server.close(() => r()));
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});
afterAll(() => settlePrivateFileOwners(() => false));

test('the writer follows the registry row', () => {
  settlePrivateFileOwners(() => false);
  expect(tsWritesRow('reset-claims')).toBe(true);
  expect(rustResetClaimsFile({ locals: {} } as any)).toBeNull();
  settlePrivateFileOwners((group) => group === 'writes');
  expect(tsWritesRow('reset-claims')).toBe(false);
  expect(() => rustResetClaimsFile({ locals: {} } as any)).toThrow();
  expect(() => rustResetClaimsFile({ locals: { rustService: {} } } as any)).toThrow();
});

test('a Rust-owned claims file with no service fails closed without any source call', async () => {
  settlePrivateFileOwners((group) => group === 'writes');
  const ccsDir = mkdtempSync(join(tmpdir(), 'aac-reset-owner-'));
  dirs.push(ccsDir);
  let reads = 0;
  let sends = 0;
  const app = express();
  app.use(express.json());
  app.use((req: any, _res, next) => {
    req.session = { authenticated: true };
    req.sessionID = 'synthetic-session';
    next();
  });
  app.use(
    '/accounts',
    createResetClaimRouter({
      ccsDir,
      source: {
        async read() {
          reads++;
          return {};
        },
        async send() {
          sends++;
          return { outcome: 'reset' };
        },
      },
    })
  );
  const server = app.listen(0, '127.0.0.1');
  servers.push(server);
  await new Promise<void>((r) => server.on('listening', r));
  const url = `http://127.0.0.1:${(server.address() as any).port}/accounts/codex%3Asynthetic/reset-claims`;
  const origin = new URL(url).origin;
  const get = await fetch(url);
  expect(get.status).toBe(503);
  expect(await get.json()).toEqual({ status: 'unavailable', code: 'reset-inventory-unavailable' });
  for (const [suffix, body] of [
    ['/prepare', { grantKey: 'b'.repeat(64) }],
    ['', { grantKey: 'b'.repeat(64), confirmationToken: 'c'.repeat(64) }],
    ['/check', { attemptKey: 'd'.repeat(64) }],
  ] as const) {
    const res = await fetch(url + suffix, {
      method: 'POST',
      headers: { origin, 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    expect(res.status).toBe(409);
    expect(await res.json()).toEqual({
      code: 'reset-state-unavailable',
      action: 'check-again',
      reason: null,
      sent: false,
      evidence: null,
    });
  }
  expect(reads).toBe(0);
  expect(sends).toBe(0);
  expect(existsSync(join(ccsDir, 'account-usage', 'reset-claims', 'claims.sqlite'))).toBe(false);
});
