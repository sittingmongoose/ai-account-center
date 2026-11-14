/**
 * DESIGN 17 on the server: the Cursor account reader's states and pacing, only the account's
 * usage outside the scanned computers added to Analytics (never the whole account), the key set
 * through stdin only, and the reader binary used only when its digest matches. Fixtures only: no
 * key exists, and the reader binary is replaced by a fake.
 */
import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { createHash } from 'crypto';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { handleCursorKeyCommand } from '../../../src/commands/cursor-key-command';
import { AccountAnalyticsActivityService } from '../../../src/web-server/services/account-analytics-activity';
import { analyticsRowsResult } from '../../../src/web-server/services/analytics-remote-sources';
import type { AccountAnalyticsQuery } from '../../../src/web-server/services/account-analytics-types';
import {
  CURSOR_ACCOUNT_TTL_MS,
  cursorAccountBinary,
  cursorAccountOutside,
  readCursorAccount,
  type CursorAccountRead,
} from '../../../src/web-server/services/cursor-account-reader';
import type { UsageWorkerResult } from '../../../src/web-server/usage/worker-client';

const NOW = Date.parse('2026-11-13T16:30:00Z');
const SINCE = NOW - 31 * 86_400_000;
const ROW = {
  h: '2026-11-13 15:00',
  m: 'claude-5-sonnet',
  i: 300,
  o: 20,
  cr: 1000,
  cw: 0,
  n: 2,
  cents: 12.5,
  uncosted: 0,
};

let dir: string;
let savedCcs: string | undefined;
beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'aac-cursor-account-'));
  savedCcs = process.env.CCS_HOME;
  process.env.CCS_HOME = path.join(dir, 'ccs');
});
afterEach(() => {
  if (savedCcs === undefined) delete process.env.CCS_HOME;
  else process.env.CCS_HOME = savedCcs;
  fs.rmSync(dir, { recursive: true, force: true });
});

describe('reading the account', () => {
  it('reads at most every 10 minutes, at once on a refresh, and keeps the last good read through a failure', async () => {
    const calls: string[][] = [];
    let answer: unknown = {
      state: 'read',
      readAt: NOW,
      account: 'a'.repeat(16),
      key: { last4: 'WXYZ', fingerprint: 'sha256:0123456789abcdef' },
      events: 2,
      unmeasured: 2,
      rows: [ROW],
    };
    let now = NOW;
    const read = (force = false) =>
      readCursorAccount(SINCE, now, {
        cacheDir: path.join(dir, 'cache'),
        ccsDir: path.join(dir, 'ccs'),
        now: () => now,
        force,
        run: async (args) => {
          calls.push(args);
          return answer;
        },
      });
    const first = await read();
    expect(first).toMatchObject({ state: 'read', events: 2, unmeasured: 2, key: { last4: 'WXYZ' } });
    expect(calls[0]).toEqual(['read', '--ccs-dir', path.join(dir, 'ccs')]);
    now += CURSOR_ACCOUNT_TTL_MS - 1;
    await read();
    expect(calls).toHaveLength(1);
    await read(true);
    expect(calls).toHaveLength(2);
    // A failure keeps the last good rows, with their own time, and says why.
    now += CURSOR_ACCOUNT_TTL_MS;
    answer = { state: 'unavailable', cause: 'timeout' };
    const failed = await read();
    expect(failed).toMatchObject({ state: 'unavailable', cause: 'timeout', events: 2 });
    expect(failed.readAt).toBe(new Date(NOW).toISOString());
    expect(failed.rows).toHaveLength(1);
    // The kept read still says which events were never measured (a cache round trip).
    expect(failed.unmeasured).toBe(2);
    // The saved read holds no key: only its last four and fingerprint.
    const saved = fs.readFileSync(path.join(dir, 'cache', 'cursor-account-v1.json'), 'utf8');
    expect(saved).not.toContain('secret');
    // A rejected key or no key: nothing of the account counts.
    now += CURSOR_ACCOUNT_TTL_MS;
    answer = {
      state: 'key_rejected',
      key: { last4: 'WXYZ', fingerprint: 'sha256:0123456789abcdef' },
    };
    expect(await read()).toMatchObject({ state: 'key_rejected', rows: [], readAt: null });
    now += CURSOR_ACCOUNT_TTL_MS;
    answer = { state: 'no_key' };
    expect(await read()).toMatchObject({ state: 'no_key', rows: [], key: null });
  });

  it('says when no reader is built for this computer', async () => {
    const read = await readCursorAccount(SINCE, NOW, {
      cacheDir: path.join(dir, 'cache'),
      ccsDir: dir,
      now: () => NOW,
      run: async () => {
        throw new Error('no_reader');
      },
    });
    expect(read.state).toBe('no_reader');
  });
});

describe('only the usage outside the scanned computers is added', () => {
  const read: CursorAccountRead = {
    state: 'read',
    checkedAt: new Date(NOW).toISOString(),
    readAt: new Date(NOW).toISOString(),
    account: 'a'.repeat(16),
    key: { last4: 'WXYZ', fingerprint: 'sha256:0123456789abcdef' },
    rows: [ROW, { ...ROW, h: '2026-11-13 14:00', m: 'grok-4.7', uncosted: 1 }],
    events: 4,
    unmeasured: 0,
  };

  it("turns the reader's dedupe into rows: Cursor's cost where every event had one", async () => {
    let asked: unknown;
    const outside = await cursorAccountOutside(
      read,
      [{ h: '2026-11-13 14:00', m: 'grok-4.7', i: 300, o: 20, cr: 1000, cw: 0 }],
      {
        run: async (args, input) => {
          asked = { args, body: JSON.parse(input) };
          return {
            rows: [ROW, { ...ROW, uncosted: 1, cents: 0 }],
            summary: {
              accountTokens: 2640,
              matchedTokens: 1320,
              outsideTokens: 1320,
              localUnmatchedTokens: 0,
            },
          };
        },
      }
    );
    expect(asked).toMatchObject({
      args: ['dedupe'],
      body: { account: read.rows, local: [{ m: 'grok-4.7' }] },
    });
    expect(outside).toMatchObject({
      state: 'read',
      last4: 'WXYZ',
      outsideTokens: 1320,
      matchedTokens: 1320,
    });
    // An hour whose events all had a cost carries Cursor's own cost, reported even at 0; an
    // hour where any event had none is a row of unknown cost, marked partial, never priced.
    expect(outside.rows).toEqual([
      {
        k: 'cursor',
        f: 'cursor-account',
        m: 'claude-5-sonnet',
        h: '2026-11-13 15:00',
        i: 300,
        o: 20,
        cr: 1000,
        cw: 0,
        c: 0.125,
        n: 2,
        costBasis: 'reported',
      },
      {
        k: 'cursor',
        f: 'cursor-account',
        m: 'claude-5-sonnet',
        h: '2026-11-13 15:00',
        i: 300,
        o: 20,
        cr: 1000,
        cw: 0,
        c: 0,
        n: 2,
        costBasis: 'unknown',
      },
    ]);
    expect(outside.costPartial).toBe(true);
  });

  it('a reported zero cost stays zero and a missing cost stays unknown, never list-priced (review M2)', async () => {
    // The review's probe: an outside account row for gpt-4o with 1,000,000 input tokens and no
    // cost became USD 2.50 through the aggregation's list prices, for both a reported zero
    // (`cents=0, uncosted=0`) and a missing cost (`uncosted=1`). Neither may be priced again.
    for (const uncosted of [0, 1]) {
      const row = {
        h: '2026-11-13 15:00',
        m: 'gpt-4o',
        i: 1_000_000,
        o: 0,
        cr: 0,
        cw: 0,
        n: 1,
        cents: 0,
        uncosted,
      };
      const outside = await cursorAccountOutside(
        { ...read, rows: [row] },
        [],
        {
          run: async () => ({
            rows: [row],
            summary: {
              accountTokens: 1_000_000,
              matchedTokens: 0,
              outsideTokens: 1_000_000,
              localUnmatchedTokens: 0,
            },
          }),
        }
      );
      expect(outside.rows[0].c).toBe(0);
      expect(outside.rows[0].costBasis).toBe(uncosted === 0 ? 'reported' : 'unknown');
      expect(outside.costPartial).toBe(uncosted === 0 ? undefined : true);
      const result = analyticsRowsResult(outside.rows, 'cursor');
      expect(result.hourly).toHaveLength(1);
      // Was USD 2.50 at the reviewed head: the list price must never touch these rows.
      expect(result.hourly[0].cost).toBe(0);
      expect(result.hourly[0].costPartial).toBe(uncosted === 0 ? undefined : true);
    }
  });

  it("an all-unmeasured read carries its count, never an absence of usage (recheck R1)", async () => {
    // The recheck's loopback case: one in-range event with tokenUsage {} — the reader answers
    // state read, no rows, unmeasured 1. The server must keep that count, so the page can say
    // the events were not logged instead of "no usage in range".
    const unmeasuredRead: CursorAccountRead = {
      state: 'read',
      checkedAt: new Date(NOW).toISOString(),
      readAt: new Date(NOW).toISOString(),
      account: 'a'.repeat(16),
      key: { last4: 'WXYZ', fingerprint: 'sha256:0123456789abcdef' },
      rows: [],
      events: 0,
      unmeasured: 1,
    };
    const outside = await cursorAccountOutside(unmeasuredRead, [], {
      run: async () => {
        throw new Error('reader_failed');
      },
    });
    expect(outside).toMatchObject({ state: 'read', accountTokens: 0, unmeasured: 1 });
    // And a mixed read: measured tokens beside unmeasured events.
    const outside2 = await cursorAccountOutside(
      { ...read, unmeasured: 2 },
      [],
      {
        run: async () => ({
          rows: read.rows,
          summary: { accountTokens: 2640, matchedTokens: 0, outsideTokens: 2640, localUnmatchedTokens: 0 },
        }),
      }
    );
    expect(outside2).toMatchObject({ state: 'read', accountTokens: 2640, unmeasured: 2 });
  });

  it("adds nothing when the dedupe cannot run: counting T3's runs twice is worse", async () => {
    const outside = await cursorAccountOutside(read, [], {
      run: async () => {
        throw new Error('reader_failed');
      },
    });
    expect(outside).toMatchObject({
      state: 'unavailable',
      cause: 'dedupe_failed',
      rows: [],
      outsideTokens: 0,
    });
  });

  it('the activity totals hold the local runs only; the account is shown beside them, never added (review M1)', async () => {
    const hourly = (model: string, input: number): UsageWorkerResult =>
      ({
        daily: [],
        monthly: [],
        eventCount: 1,
        hourly: [
          {
            hour: '2026-11-13 14:00',
            source: 'native',
            inputTokens: input,
            outputTokens: 0,
            cacheCreationTokens: 0,
            cacheReadTokens: 0,
            cost: 0,
            totalCost: 0,
            modelsUsed: [model],
            requestCount: 1,
            modelBreakdowns: [
              {
                modelName: model,
                inputTokens: input,
                outputTokens: 0,
                cacheCreationTokens: 0,
                cacheReadTokens: 0,
                cost: 0,
              },
            ],
          },
        ],
        session: [],
      }) as unknown as UsageWorkerResult;
    let local: unknown;
    const service = new AccountAnalyticsActivityService({
      scope: () => dir,
      now: () => NOW,
      requests: () => [],
      loadWorker: async () => hourly('unused', 0),
      remote: async () => ({
        fetchedAt: new Date(NOW).toISOString(),
        results: [{ tool: 'cursor', data: hourly('grok-4.7', 1000) }],
        states: [],
      }),
      cursorAccount: {
        read: async () => read,
        outside: async (_read, rows) => {
          local = rows;
          return {
            state: 'read',
            checkedAt: read.checkedAt,
            readAt: read.readAt,
            last4: 'WXYZ',
            accountTokens: 1700,
            matchedTokens: 1000,
            outsideTokens: 700,
            localUnmatchedTokens: 0,
            unmeasured: 2,
            rows: [
              {
                k: 'cursor',
                f: 'cursor-account',
                m: 'claude-5-sonnet',
                h: '2026-11-13 14:00',
                i: 700,
                o: 0,
                cr: 0,
                cw: 0,
                c: 0.25,
                n: 3,
              },
            ],
          };
        },
      },
    });
    const query: AccountAnalyticsQuery = {
      platform: 'mac',
      range: '7d',
      provider: 'all',
      account: 'all',
    };
    try {
      let answer = await service.get(query, NOW - 86_400_000, NOW);
      for (let attempt = 0; attempt < 500 && answer.refreshing !== false; attempt++) {
        await new Promise((resolve) => setTimeout(resolve, 5));
        answer = await service.get(query, NOW - 86_400_000, NOW);
      }
      // The dedupe got the scanned Cursor rows; the totals are the 1,000 local tokens only. The
      // account's 1,700 (authoritative) and its 700 estimated outside part are shown beside the
      // totals, never added: the account's events overlap the local runs and cannot be split.
      expect(local).toEqual([
        { h: '2026-11-13 14:00', m: 'grok-4.7', i: 1000, o: 0, cr: 0, cw: 0 },
      ]);
      expect(answer.totals?.inputTokens).toBe(1000);
      expect(answer.cursorAccount).toMatchObject({
        state: 'read',
        accountTokens: 1700,
        outsideTokens: 700,
        last4: 'WXYZ',
        unmeasured: 2,
      });
      expect(JSON.stringify(answer.cursorAccount)).not.toContain('rows');
    } finally {
      await service.shutdown();
    }
  });
});

describe('the key and the reader binary', () => {
  it('takes the key from stdin only and prints no more than its last four', async () => {
    const seen: Array<{ args: string[]; input: string }> = [];
    const out: string[] = [];
    const io = {
      out: (text: string) => out.push(text),
      err: (text: string) => out.push(text),
      readStdin: async () => 'key_TEST_0123456789abcdef_NOT_REAL\n',
      ccsDir: path.join(dir, 'ccs'),
      run: async (args: string[], input: string) => {
        seen.push({ args, input });
        if (args[1] === 'put')
          return {
            ok: true,
            keyId: 'abcdef01',
            fingerprint: 'sha256:0123456789abcdef',
            last4: 'REAL',
          };
        if (args[1] === 'status') return { present: true, last4: 'REAL' };
        return { ok: true, removed: 1 };
      },
    };
    expect(await handleCursorKeyCommand(['set'], io)).toBe(0);
    expect(await handleCursorKeyCommand(['status'], io)).toBe(0);
    expect(await handleCursorKeyCommand(['remove'], io)).toBe(0);
    expect(seen[0]).toEqual({
      args: ['key', 'put', '--ccs-dir', path.join(dir, 'ccs')],
      input: 'key_TEST_0123456789abcdef_NOT_REAL\n',
    });
    expect(seen.every(({ args }) => !args.some((arg) => arg.includes('key_TEST')))).toBe(true);
    expect(out.join('')).toContain('ending REAL');
    expect(out.join('')).not.toContain('key_TEST');
    // The key as an argument is refused: help, nothing stored.
    expect(await handleCursorKeyCommand(['set', 'key_TEST_0123456789abcdef_NOT_REAL'], io)).toBe(2);
    expect(seen).toHaveLength(3);
    // Ctrl-C at the hidden prompt (the reader rejects with `cancelled`): nothing stored.
    expect(
      await handleCursorKeyCommand(['set'], {
        ...io,
        readStdin: async () => {
          throw new Error('cancelled');
        },
      })
    ).toBe(130);
    expect(out.join('')).toContain('Cancelled');
    expect(seen).toHaveLength(3);
  });

  it('uses the packaged reader only when its digest matches the manifest', () => {
    const pkg = path.join(dir, 'pkg');
    fs.mkdirSync(path.join(pkg, 'x86_64-unknown-linux-gnu'), { recursive: true });
    const body = '#!/bin/sh\necho {}\n';
    const file = path.join(pkg, 'x86_64-unknown-linux-gnu', 'aac-cursor-account');
    fs.writeFileSync(file, body, { mode: 0o755 });
    const manifest = (sha256: string) =>
      fs.writeFileSync(
        path.join(pkg, 'manifest.json'),
        JSON.stringify({
          name: 'aac-cursor-account',
          targets: {
            'x86_64-unknown-linux-gnu': {
              file: 'x86_64-unknown-linux-gnu/aac-cursor-account',
              sha256,
              size: body.length,
            },
          },
        })
      );
    manifest(createHash('sha256').update(body).digest('hex'));
    expect(cursorAccountBinary(pkg, 'x86_64-unknown-linux-gnu')).toBe(file);
    manifest('0'.repeat(64));
    expect(cursorAccountBinary(pkg, 'x86_64-unknown-linux-gnu')).toBeNull();
    expect(cursorAccountBinary(pkg, 'aarch64-apple-darwin')).toBeNull();
  });
});
