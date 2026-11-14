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
    expect(first).toMatchObject({ state: 'read', events: 2, key: { last4: 'WXYZ' } });
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
            rows: [ROW],
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
      },
    ]);
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

  it('the activity totals hold the local runs plus the outside part, and the answer names the account', async () => {
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
      // The dedupe got the scanned Cursor rows; the totals are 1,000 local + 700 outside, not 1,700 + 1,000.
      expect(local).toEqual([
        { h: '2026-11-13 14:00', m: 'grok-4.7', i: 1000, o: 0, cr: 0, cw: 0 },
      ]);
      expect(answer.totals?.inputTokens).toBe(1700);
      expect(answer.cursorAccount).toMatchObject({
        state: 'read',
        outsideTokens: 700,
        last4: 'WXYZ',
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
